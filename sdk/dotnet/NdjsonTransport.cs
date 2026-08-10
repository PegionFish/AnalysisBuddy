// AnalysisBuddy.Sdk — NDJSON transport over stdio (protocol-v1.md §1).
//
// Frame rules implemented here (matching the Python SDK / ab-protocol):
// - lines are read incrementally by byte with length validated before content;
//   a line exceeding 8 MiB is a protocol error and is aborted without retaining
//   more than the limit in memory;
// - a stray '\r' anywhere in a frame is a protocol error;
// - writes serialize a complete JSON-RPC message then emit it as one atomic
//   whole-line write (buffer + single Write + flush), guarded by a send lock so
//   concurrent tasks never interleave half lines.

using System.Text;
using System.Text.Json;

namespace AnalysisBuddy.Sdk;

/// <summary>Raised when the peer violates the NDJSON framing rules (protocol-v1.md §1.2/§1.3).</summary>
public sealed class ProtocolViolationException : Exception
{
    public ProtocolViolationException(string message)
        : base(message)
    {
    }
}

/// <summary>Whole-line NDJSON reader/writer over two byte streams.</summary>
public sealed class NdjsonTransport : IDisposable
{
    /// <summary>8 MiB single-line ceiling (8 × 1024 × 1024 bytes, protocol-v1.md §1.3).</summary>
    public const long MaxLineBytes = 8 * 1024 * 1024;

    private readonly Stream _stdin;
    private readonly Stream _stdout;
    private readonly SemaphoreSlim _writeLock = new(1, 1);
    private readonly byte[] _chunk = new byte[16 * 1024];
    private int _chunkPos;
    private int _chunkLen;
    private readonly List<byte[]> _segments = new();
    private long _lineBytes;

    /// <summary>Creates a transport reading from <paramref name="stdin"/> and writing to <paramref name="stdout"/>.</summary>
    public NdjsonTransport(Stream stdin, Stream stdout)
    {
        _stdin = stdin;
        _stdout = stdout;
    }

    /// <summary>Reads one line; returns null on clean EOF. Throws <see cref="ProtocolViolationException"/>
    /// on oversized lines or stray '\r'.</summary>
    public async Task<string?> ReadLineAsync(CancellationToken ct = default)
    {
        _segments.Clear();
        _lineBytes = 0;

        while (true)
        {
            if (_chunkPos >= _chunkLen)
            {
                _chunkLen = await _stdin.ReadAsync(_chunk, ct).ConfigureAwait(false);
                _chunkPos = 0;
                if (_chunkLen == 0)
                {
                    // EOF: a partial line without a trailing LF is a protocol error.
                    if (_lineBytes == 0)
                    {
                        return null;
                    }

                    throw new ProtocolViolationException("EOF in the middle of a frame");
                }
            }

            int nl = Array.IndexOf(_chunk, (byte)'\n', _chunkPos, _chunkLen - _chunkPos);
            if (nl >= 0)
            {
                TakeSegment(_chunkPos, nl - _chunkPos);
                _chunkPos = nl + 1;
                return DecodeLine();
            }

            TakeSegment(_chunkPos, _chunkLen - _chunkPos);
            _chunkPos = _chunkLen;
        }
    }

    /// <summary>Serializes <paramref name="payload"/> and writes it as one atomic NDJSON line.</summary>
    public async Task WriteFrameAsync(object payload, CancellationToken ct = default)
    {
        byte[] json = JsonSerializer.SerializeToUtf8Bytes(payload, Json.Options);
        byte[] frame = new byte[json.Length + 1];
        Buffer.BlockCopy(json, 0, frame, 0, json.Length);
        frame[^1] = (byte)'\n';

        await _writeLock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            await _stdout.WriteAsync(frame, ct).ConfigureAwait(false);
            await _stdout.FlushAsync(ct).ConfigureAwait(false);
        }
        finally
        {
            _writeLock.Release();
        }
    }

    /// <summary>Flushes the output stream.</summary>
    public Task FlushAsync(CancellationToken ct = default) => _stdout.FlushAsync(ct);

    public void Dispose()
    {
        _writeLock.Dispose();
    }

    private void TakeSegment(int start, int count)
    {
        if (count == 0)
        {
            return;
        }

        // Stray '\r' anywhere in a frame is a protocol error (protocol-v1.md §1.2).
        if (Array.IndexOf(_chunk, (byte)'\r', start, count) >= 0)
        {
            throw new ProtocolViolationException("stray '\\r' in frame");
        }

        // Validate length before materializing content: abort once the line
        // would exceed the 8 MiB ceiling, without reading the remainder.
        if (_lineBytes + count > MaxLineBytes)
        {
            throw new ProtocolViolationException($"line exceeds 8MB limit ({MaxLineBytes} bytes)");
        }

        _lineBytes += count;
        var segment = new byte[count];
        Array.Copy(_chunk, start, segment, 0, count);
        _segments.Add(segment);
    }

    private string DecodeLine()
    {
        var buffer = new byte[_lineBytes];
        int offset = 0;
        foreach (var segment in _segments)
        {
            Buffer.BlockCopy(segment, 0, buffer, offset, segment.Length);
            offset += segment.Length;
        }

        return Encoding.UTF8.GetString(buffer);
    }
}
