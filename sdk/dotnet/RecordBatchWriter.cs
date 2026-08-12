// AnalysisBuddy.Sdk — batching writer + heartbeat for streaming parse
// (sdk-plugins.md §2.4, protocol-v1.md §3.2/§3.3).
//
// Behavior contract (aligned with the Python SDK EmitContext):
// - records are buffered and flushed as RecordBatch notifications with seq
//   starting at 0 and no gaps;
// - batchSize must be within [1000, 8000] (default 4000), enforced at
//   construction with ArgumentOutOfRangeException;
// - a batch whose approximate serialized size approaches the 1 MB send-side
//   recommendation is flushed early at 900 KB, leaving headroom for the frame
//   envelope (send-side recommendation, protocol-v1.md §1.3);
// - a heartbeat loop (PeriodicTimer, 2s) sends a progress notification when
//   parsing is quiet for >= 2s (heartbeat obligation, protocol-v1.md §3.3);
// - FlushAsync emits the residual buffer as the final done:true batch (records
//   may be an empty array);
// - records with NaN / ±Infinity values are dropped and counted on stderr;
// - ThrowIfCancelled throws CancelledException once cancel_parse has been
//   delivered by the host.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace AnalysisBuddy.Sdk;

/// <summary>Streaming record batch writer created by PluginHost for each parse request.</summary>
public sealed class RecordBatchWriter : IAsyncDisposable
{
    /// <summary>Protocol batch-size band (protocol-v1.md §3.2).</summary>
    public const int MinBatchSize = 1000;
    /// <summary>Protocol batch-size upper bound (protocol-v1.md §3.2).</summary>
    public const int MaxBatchSize = 8000;

    /// <summary>Early-flush threshold: 900 KB of accumulated serialized records, leaving
    /// headroom under the ~1 MB frame recommendation (unified with the Python SDK's
    /// EARLY_FLUSH_BYTES = 900_000).</summary>
    private const long EarlyFlushBytes = 900_000;

    private readonly string _fileId;
    private readonly int _batchSize;
    private readonly Func<object, Task> _sendFrame;
    private readonly object _gate = new();
    private readonly List<Record> _buffer = new();
    private long _bufferedBytes;
    private ulong _seq;
    private ulong _recordsEmitted;
    private long _droppedNonFinite;
    private DateTime _lastSendUtc = DateTime.UtcNow;
    private CancellationToken _cancelToken;
    private TimeSpan _heartbeatInterval = TimeSpan.FromSeconds(2);

    /// <summary>Created by PluginHost; the send function is host-injected.</summary>
    internal RecordBatchWriter(string fileId, Func<object, Task> sendFrame, int batchSize = 4000)
        : this(fileId, sendFrame, batchSize, TimeSpan.FromSeconds(2))
    {
    }

    internal RecordBatchWriter(string fileId, Func<object, Task> sendFrame, int batchSize, TimeSpan heartbeatInterval)
    {
        if (batchSize is < MinBatchSize or > MaxBatchSize)
        {
            throw new ArgumentOutOfRangeException(
                nameof(batchSize), batchSize,
                $"batchSize must be within [{MinBatchSize}, {MaxBatchSize}]");
        }

        _fileId = fileId;
        _sendFrame = sendFrame;
        _batchSize = batchSize;
        _heartbeatInterval = heartbeatInterval;
    }

    /// <summary>Public surface per sdk-plugins.md §2.4. A writer constructed this way has no
    /// host-injected sender; the Host constructs the writer it passes to
    /// <see cref="IPluginHandler.ParseAsync"/> via its internal factory.</summary>
    public RecordBatchWriter(string fileId, int batchSize = 4000)
        : this(fileId, _ => throw new InvalidOperationException(
            "RecordBatchWriter is not attached to a PluginHost"), batchSize)
    {
    }

    /// <summary>Number of records emitted into batches so far (excludes dropped non-finite values).</summary>
    public ulong RecordsEmitted
    {
        get { lock (_gate) { return _recordsEmitted; } }
    }

    /// <summary>Number of records dropped because their value was NaN / ±Infinity.</summary>
    public long DroppedNonFinite
    {
        get { lock (_gate) { return _droppedNonFinite; } }
    }

    /// <summary>Binds the cancellation token delivered by cancel_parse.</summary>
    internal void AttachCancellation(CancellationToken token) => _cancelToken = token;

    /// <summary>Buffers a single record; flushes a batch when the batch size or 900 KB size
    /// threshold is reached.</summary>
    public async Task EmitAsync(Record record, CancellationToken ct = default)
    {
        if (double.IsNaN(record.Value) || double.IsInfinity(record.Value))
        {
            lock (_gate)
            {
                _droppedNonFinite++;
            }

            PluginLog.Warn(
                $"dropped record with non-finite value {record.Value} (metric={record.Metric}, ts={record.Timestamp}); " +
                $"total dropped={DroppedNonFinite}");
            return;
        }

        bool flushNow;
        lock (_gate)
        {
            _buffer.Add(record);
            _bufferedBytes += EstimateBytes(record);
            _recordsEmitted++;
            flushNow = _buffer.Count >= _batchSize || _bufferedBytes >= EarlyFlushBytes;
        }

        if (flushNow)
        {
            await FlushBatchAsync(done: false, ct).ConfigureAwait(false);
        }
    }

    /// <summary>Buffers a collection of records (see <see cref="EmitAsync(Record, CancellationToken)"/>).</summary>
    public async Task EmitAsync(IEnumerable<Record> records, CancellationToken ct = default)
    {
        foreach (var record in records)
        {
            await EmitAsync(record, ct).ConfigureAwait(false);
        }
    }

    /// <summary>Sends a progress notification immediately (protocol-v1.md §3.3).</summary>
    public async Task ProgressAsync(double? percent = null, ulong? bytesRead = null, CancellationToken ct = default)
    {
        lock (_gate)
        {
            _lastSendUtc = DateTime.UtcNow;
        }

        var frame = new RpcNotification(
            "progress",
            new ProgressParams(_fileId, percent, RecordsEmitted, bytesRead));
        await _sendFrame(frame).ConfigureAwait(false);
    }

    /// <summary>Throws <see cref="Errors.CancelledException"/> if cancel_parse has been delivered.</summary>
    public void ThrowIfCancelled()
    {
        if (_cancelToken.IsCancellationRequested)
        {
            throw new Errors.CancelledException("parse cancelled by host");
        }
    }

    /// <summary>Flushes the residual buffer as the final done:true batch (records may be empty).</summary>
    public Task FlushAsync(CancellationToken ct = default) => FlushBatchAsync(done: true, ct);

    /// <summary>Heartbeat loop: while parsing, sends progress when the last frame is >= interval old
    /// (protocol-v1.md §3.3 heartbeat obligation). Host-owned.</summary>
    internal async Task HeartbeatLoopAsync(CancellationToken ct)
    {
        using var timer = new PeriodicTimer(_heartbeatInterval);
        while (await timer.WaitForNextTickAsync(ct).ConfigureAwait(false))
        {
            DateTime lastSend;
            lock (_gate)
            {
                lastSend = _lastSendUtc;
            }

            if (DateTime.UtcNow - lastSend >= _heartbeatInterval)
            {
                await ProgressAsync(percent: null, bytesRead: null, ct).ConfigureAwait(false);
            }
        }
    }

    /// <summary>No-op disposal (no unmanaged resources).</summary>
    public ValueTask DisposeAsync() => ValueTask.CompletedTask;

    private async Task FlushBatchAsync(bool done, CancellationToken ct)
    {
        Record[] records;
        ulong seq;
        lock (_gate)
        {
            records = _buffer.ToArray();
            _buffer.Clear();
            _bufferedBytes = 0;
            seq = _seq++;
            _lastSendUtc = DateTime.UtcNow;
        }

        var frame = new RpcNotification(
            "RecordBatch",
            new RecordBatchParams(_fileId, seq, records, done));
        await _sendFrame(frame).ConfigureAwait(false);
    }

    private static long EstimateBytes(Record record)
    {
        // Rough serialized-size estimate for the 900 KB early-flush threshold:
        // timestamp (13 digits) + metric + value + optional fields, with JSON
        // overhead. Only used for thresholding, precision is not required.
        long size = 32;
        if (record.Metric is not null)
        {
            size += record.Metric.Length;
        }

        if (record.Level is not null)
        {
            size += record.Level.Length;
        }

        if (record.RawLine is not null)
        {
            size += record.RawLine.Length;
        }

        if (record.Tags is not null)
        {
            foreach (var (k, v) in record.Tags)
            {
                size += k.Length + v.Length + 8;
            }
        }

        return size;
    }
}

/// <summary>JSON-RPC notification envelope (plugin → host push, protocol-v1.md §1.1).
/// Property order matters: jsonrpc, method, params (matches the protocol examples).</summary>
public sealed class RpcNotification
{
    /// <summary>Creates a notification for the given method and params payload.</summary>
    public RpcNotification(string method, object? paramsPayload)
    {
        Method = method;
        Params = paramsPayload;
    }

    /// <summary>Protocol version marker, always "2.0".</summary>
    [JsonPropertyName("jsonrpc")]
    public string JsonRpc => "2.0";

    /// <summary>Notification method name (e.g. "RecordBatch", "progress").</summary>
    [JsonPropertyName("method")]
    public string Method { get; }

    /// <summary>Notification payload; omitted when null (skip-if-empty).</summary>
    [JsonPropertyName("params")]
    public object? Params { get; }
}
