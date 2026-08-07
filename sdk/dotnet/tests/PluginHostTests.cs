// D2-01 DoD tests: PluginHost.RunAsync 10-method routing, concurrent-parse
// interception (-32001), stdin EOF exit, unknown method (-32601), malformed
// request (-32600), host-driven heartbeat, and exception mapping.

using System.Text;
using System.Text.Json;
using AnalysisBuddy.Sdk;
using AnalysisBuddy.Sdk.Errors;
using Xunit;

namespace AnalysisBuddy.Sdk.Tests;

public class PluginHostTests
{
    /// <summary>Handler whose behavior is scripted per test.</summary>
    private sealed class FakeHandler : PluginHandlerBase
    {
        public override PluginInfo Info => new("test-handler", "Test Handler", "0.1.0");

        public Func<CanHandleParams, Task<CanHandleResult>>? OnCanHandle { get; set; }
        public Func<LoadFileParams, Task<FileSummary?>>? OnLoadFile { get; set; }
        public Func<string, JsonElement?, RecordBatchWriter, CancellationToken, Task<ulong>>? OnParse { get; set; }
        public Func<Task<SchemaResult>>? OnSchema { get; set; }
        public Func<string, long, Task<KeyValuesResult>>? OnKeyValues { get; set; }
        public Func<string, TimeRange, Task<AnnotateResult>>? OnAnnotate { get; set; }
        public Func<string, Task>? OnUnload { get; set; }

        public override Task<CanHandleResult> CanHandleAsync(CanHandleParams p, CancellationToken ct)
            => OnCanHandle?.Invoke(p) ?? Task.FromResult(new CanHandleResult(true, 0.9, "test fixture"));

        public override Task<FileSummary?> LoadFileAsync(LoadFileParams p, CancellationToken ct)
            => OnLoadFile?.Invoke(p) ?? Task.FromResult<FileSummary?>(FileSummary.Empty);

        public override Task<ulong> ParseAsync(string fileId, JsonElement? options, RecordBatchWriter writer, CancellationToken ct)
            => OnParse is not null ? OnParse(fileId, options, writer, ct) : Task.FromResult(0UL);

        public override Task<SchemaResult> SchemaAsync(CancellationToken ct)
            => OnSchema?.Invoke() ?? Task.FromResult(new SchemaResult(new List<MetricDef> { new("fps", "fps", "fps", null, Aggregation.Last) }));

        public override Task<KeyValuesResult> KeyValuesAsync(string fileId, long timestampMs, CancellationToken ct)
            => OnKeyValues?.Invoke(fileId, timestampMs) ?? Task.FromResult(new KeyValuesResult(new List<KeyValueEntry> { new("scene", "menu") }));

        public override Task<AnnotateResult> AnnotateAsync(string fileId, TimeRange range, CancellationToken ct)
            => OnAnnotate?.Invoke(fileId, range) ?? Task.FromResult(new AnnotateResult(new List<AnnotateEvent>()));

        public override Task UnloadFileAsync(string fileId, CancellationToken ct)
        {
            OnUnload?.Invoke(fileId);
            return Task.CompletedTask;
        }
    }

    /// <summary>stdin stream that yields the initial text, then blocks until more text or EOF
    /// is fed by the test. Makes parse timing deterministic.</summary>
    private sealed class GateInput : Stream
    {
        private readonly List<byte> _buf = new();
        private int _pos;
        private readonly SemaphoreSlim _gate = new(0);
        private bool _eof;

        public GateInput(string initialText)
        {
            _buf.AddRange(Encoding.UTF8.GetBytes(initialText));
        }

        public void Feed(string text)
        {
            _buf.AddRange(Encoding.UTF8.GetBytes(text));
            _gate.Release();
        }

        public void Finish()
        {
            _eof = true;
            _gate.Release();
        }

        public override async Task<int> ReadAsync(byte[] buffer, int offset, int count, CancellationToken ct)
        {
            while (_pos >= _buf.Count)
            {
                if (_eof)
                {
                    return 0;
                }

                await _gate.WaitAsync(ct).ConfigureAwait(false);
            }

            int n = Math.Min(count, _buf.Count - _pos);
            for (int i = 0; i < n; i++)
            {
                buffer[offset + i] = _buf[_pos + i];
            }

            _pos += n;
            return n;
        }

        public override int Read(byte[] buffer, int offset, int count)
            => ReadAsync(buffer, offset, count, CancellationToken.None).GetAwaiter().GetResult();

        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => _buf.Count;
        public override long Position { get => _pos; set => throw new NotSupportedException(); }
        public override void Flush() { }
        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    }

    private static string MakeRequest(int id, string method, string? paramsJson = null)
    {
        var prm = paramsJson is null ? "" : $",\"params\":{paramsJson}";
        return $"{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\"{prm}}}";
    }

    /// <summary>Runs the host with a fixed request script and a closed stdin (EOF).</summary>
    private static async Task<SessionResult> RunScriptedAsync(
        string[] requests, IPluginHandler handler, TimeSpan? heartbeatInterval = null)
    {
        var stdin = new MemoryStream();
        var stdout = new MemoryStream();
        var stderr = new StringWriter();
        var bytes = Encoding.UTF8.GetBytes(string.Join('\n', requests) + "\n");
        stdin.Write(bytes, 0, bytes.Length);
        stdin.Position = 0;
        await RunSessionAsync(handler, stdin, stdout, stderr, heartbeatInterval);
        return ReadSession(stdout, stderr);
    }

    /// <summary>Runs the host until the session ends; returns stdout/stderr snapshots.</summary>
    private static async Task RunSessionAsync(
        IPluginHandler handler, Stream stdin, Stream stdout, TextWriter stderr, TimeSpan? heartbeatInterval = null)
    {
        var saved = PluginHost.HeartbeatInterval;
        try
        {
            if (heartbeatInterval is { } hb)
            {
                PluginHost.HeartbeatInterval = hb;
            }

            await PluginHost.RunAsync(handler, stdin, stdout, stderr);
        }
        finally
        {
            PluginHost.HeartbeatInterval = saved;
        }
    }

    private static SessionResult ReadSession(Stream stdout, TextWriter stderr)
    {
        var text = Encoding.UTF8.GetString(((MemoryStream)stdout).ToArray());
        return new SessionResult(text.Split('\n', StringSplitOptions.RemoveEmptyEntries), stderr.ToString());
    }

    private sealed record SessionResult(IReadOnlyList<string> Lines, string StdErr);

    private static JsonElement ParseLine(string line) => JsonDocument.Parse(line).RootElement.Clone();

    /// <summary>Polls the live stdout stream until it contains <paramref name="expectedLineCount"/>
    /// complete frames or the timeout elapses; then returns the snapshot.</summary>
    private static async Task<SessionResult> PollOutputAsync(
        MemoryStream stdout, TextWriter stderr, int expectedLineCount, int timeoutMs = 5000)
    {
        var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
        while (DateTime.UtcNow < deadline)
        {
            var text = Encoding.UTF8.GetString(stdout.ToArray());
            var lines = text.Split('\n', StringSplitOptions.RemoveEmptyEntries);
            if (lines.Length >= expectedLineCount)
            {
                return new SessionResult(lines, stderr.ToString());
            }

            await Task.Delay(20);
        }

        var snapshot = Encoding.UTF8.GetString(stdout.ToArray());
        throw new TimeoutException($"expected {expectedLineCount} frames, got: {snapshot}");
    }

    [Fact]
    public async Task RoutesAllTenMethods()
    {
        var handler = new FakeHandler
        {
            OnCanHandle = p => Task.FromResult(new CanHandleResult(p.Ext == "log", 0.9, "ext=log")),
            OnLoadFile = p => Task.FromResult<FileSummary?>(new FileSummary(128, new TimeRange(1, 2), "note")),
            OnParse = async (fileId, options, writer, ct) =>
            {
                await writer.EmitAsync(new Record(1, "fps", 60.0));
                await writer.EmitAsync(new Record(2, "fps", 61.0));
                return 2;
            },
            OnKeyValues = (fileId, ts) => Task.FromResult(new KeyValuesResult(new List<KeyValueEntry> { new("scene", "boss", null) })),
            OnAnnotate = (fileId, range) => Task.FromResult(new AnnotateResult(new List<AnnotateEvent> { new(5, "crash", "error") })),
            OnSchema = () => Task.FromResult(new SchemaResult(new List<MetricDef>
            {
                new("fps", "fps", "fps", null, Aggregation.Last),
                new("frame_ms", "frame_ms", "ms", null, Aggregation.Avg),
            })),
        };

        var requests = new[]
        {
            MakeRequest(1, "initialize"),
            MakeRequest(2, "can_handle", "{\"path\":\"C:\\\\a.log\",\"name\":\"a.log\",\"ext\":\"log\",\"size_bytes\":10,\"head_sample\":\"line\"}"),
            MakeRequest(3, "load_file", "{\"file_id\":\"f1\",\"path\":\"C:\\\\a.log\"}"),
            MakeRequest(4, "parse", "{\"file_id\":\"f1\"}"),
            MakeRequest(5, "schema"),
            MakeRequest(6, "key_values", "{\"file_id\":\"f1\",\"timestamp_ms\":42}"),
            MakeRequest(7, "annotate", "{\"file_id\":\"f1\",\"range\":{\"start_ms\":0,\"end_ms\":100}}"),
            MakeRequest(8, "unload_file", "{\"file_id\":\"f1\"}"),
            MakeRequest(9, "cancel_parse", "{\"file_id\":\"f1\"}"),
            MakeRequest(10, "shutdown"),
        };

        var session = await RunScriptedAsync(requests, handler);

        // 10 responses, id-mapped (RecordBatch/progress notifications have no id).
        var byId = session.Lines
            .Where(l => l.Contains("\"id\""))
            .Select(l => (Line: l, Doc: ParseLine(l)))
            .ToDictionary(x => x.Doc.GetProperty("id").GetInt64(), x => x.Doc);

        Assert.Equal(10, byId.Count);

        Assert.Equal("test-handler", byId[1].GetProperty("result").GetProperty("id").GetString());
        Assert.True(byId[1].GetProperty("result").GetProperty("capabilities").GetProperty("annotate").GetBoolean());

        Assert.True(byId[2].GetProperty("result").GetProperty("can_handle").GetBoolean());
        Assert.Equal(128, byId[3].GetProperty("result").GetProperty("record_count_hint").GetInt64());
        Assert.Equal(2UL, byId[4].GetProperty("result").GetProperty("records_total").GetUInt64());
        Assert.Equal(2, byId[5].GetProperty("result").GetProperty("metrics").GetArrayLength());
        Assert.Equal("boss", byId[6].GetProperty("result").GetProperty("entries")[0].GetProperty("value").GetString());
        Assert.Equal("crash", byId[7].GetProperty("result").GetProperty("events")[0].GetProperty("label").GetString());
        Assert.Equal(0, byId[8].GetProperty("result").EnumerateObject().Count());
        Assert.Equal(0, byId[9].GetProperty("result").EnumerateObject().Count());
        Assert.Equal(0, byId[10].GetProperty("result").EnumerateObject().Count());
    }

    [Fact]
    public async Task ConcurrentParse_SameFile_ReturnsBusy()
    {
        using var gate = new SemaphoreSlim(0);
        var handler = new FakeHandler
        {
            OnParse = async (fileId, options, writer, ct) =>
            {
                await gate.WaitAsync(ct); // first parse blocks until released/cancelled
                return 1;
            },
        };

        var stdin = new GateInput(MakeRequest(1, "parse", "{\"file_id\":\"f1\"}") + "\n" + MakeRequest(2, "parse", "{\"file_id\":\"f1\"}") + "\n");
        var stdout = new MemoryStream();
        var stderr = new StringWriter();

        var run = PluginHost.RunAsync(handler, stdin, stdout, stderr);
        await Task.Delay(150); // let the first parse go in-flight
        gate.Release();
        stdin.Finish(); // EOF
        await run;

        var session = ReadSession(stdout, stderr);
        var responses = session.Lines.Where(l => l.Contains("\"error\"")).Select(ParseLine).ToList();
        // id 2 (the concurrent parse) must be answered -32001; id 1 may additionally
        // report -32004 if cancellation raced the gate release.
        var busy = Assert.Single(responses.Where(r => r.GetProperty("id").GetInt64() == 2));
        Assert.Equal(-32001, busy.GetProperty("error").GetProperty("code").GetInt32());
    }

    [Fact]
    public async Task EOF_ExitsCleanly()
    {
        var handler = new FakeHandler();
        var session = await RunScriptedAsync(new[] { MakeRequest(1, "initialize") }, handler);
        Assert.Single(session.Lines);
        Assert.Contains("test-handler", session.Lines[0]);
    }

    [Fact]
    public async Task UnknownMethod_ReturnsMethodNotFound()
    {
        var handler = new FakeHandler();
        var session = await RunScriptedAsync(new[] { MakeRequest(1, "bogus_method") }, handler);
        var doc = ParseLine(session.Lines.Single());
        Assert.Equal(-32601, doc.GetProperty("error").GetProperty("code").GetInt32());
    }

    [Fact]
    public async Task MalformedRequest_ReturnsInvalidRequest()
    {
        var handler = new FakeHandler();
        var stdin = new GateInput("{\"jsonrpc\":\"2.0\",\"id\":1}\n"); // no method
        var stdout = new MemoryStream();
        var stderr = new StringWriter();
        var run = PluginHost.RunAsync(handler, stdin, stdout, stderr);
        stdin.Finish();
        await run;

        var session = ReadSession(stdout, stderr);
        Assert.Equal(-32600, ParseLine(session.Lines.Single()).GetProperty("error").GetProperty("code").GetInt32());
    }

    [Fact]
    public async Task Heartbeat_EmitsProgressDuringQuietParse()
    {
        var parseStarted = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var handler = new FakeHandler
        {
            OnParse = async (fileId, options, writer, ct) =>
            {
                await writer.EmitAsync(new Record(1, "fps", 60.0));
                parseStarted.SetResult();
                await Task.Delay(400, ct); // quiet period → host heartbeat fires
                return 1;
            },
        };

        var stdin = new GateInput(MakeRequest(1, "parse", "{\"file_id\":\"f1\"}") + "\n");
        var stdout = new MemoryStream();
        var stderr = new StringWriter();

        var saved = PluginHost.HeartbeatInterval;
        PluginHost.HeartbeatInterval = TimeSpan.FromMilliseconds(50);
        try
        {
            var run = PluginHost.RunAsync(handler, stdin, stdout, stderr);
            await parseStarted.Task.WaitAsync(TimeSpan.FromSeconds(5));
            await Task.Delay(250); // several heartbeat ticks during the quiet parse
            stdin.Feed(MakeRequest(2, "shutdown") + "\n");
            stdin.Finish();
            await run.WaitAsync(TimeSpan.FromSeconds(5));
        }
        finally
        {
            PluginHost.HeartbeatInterval = saved;
        }

        var session = ReadSession(stdout, stderr);
        var progresses = session.Lines
            .Where(l => l.Contains("\"method\":\"progress\""))
            .Select(ParseLine)
            .ToList();

        Assert.NotEmpty(progresses);
        Assert.Equal(1UL, progresses[0].GetProperty("params").GetProperty("records_so_far").GetUInt64());
        Assert.False(progresses[0].GetProperty("params").TryGetProperty("percent", out _));
    }

    [Fact]
    public async Task Annotate_NotOverridden_ReturnsUnsupported()
    {
        // A handler that does NOT override AnnotateAsync (base → UnsupportedInV1Exception).
        var handler = new NoAnnotateHandler();
        var session = await RunScriptedAsync(new[] { MakeRequest(1, "annotate", "{\"file_id\":\"f1\",\"range\":{\"start_ms\":0,\"end_ms\":1}}") }, handler);
        var doc = ParseLine(session.Lines.Single());
        Assert.Equal(-32005, doc.GetProperty("error").GetProperty("code").GetInt32());
    }

    private sealed class NoAnnotateHandler : PluginHandlerBase
    {
        public override PluginInfo Info => new("no-annotate", "No Annotate", "0.1.0");

        public override Task<FileSummary?> LoadFileAsync(LoadFileParams p, CancellationToken ct)
            => Task.FromResult<FileSummary?>(FileSummary.Empty);

        public override Task<ulong> ParseAsync(string fileId, JsonElement? options, RecordBatchWriter writer, CancellationToken ct)
            => Task.FromResult(0UL);

        public override Task<SchemaResult> SchemaAsync(CancellationToken ct)
            => Task.FromResult(new SchemaResult(new List<MetricDef> { new("fps", "fps", "fps", null, Aggregation.Last) }));

        public override Task<KeyValuesResult> KeyValuesAsync(string fileId, long timestampMs, CancellationToken ct)
            => Task.FromResult(new KeyValuesResult(new List<KeyValueEntry>()));
    }

    [Fact]
    public async Task ParseFailure_ReturnsParseFailedWithData()
    {
        var handler = new FakeHandler
        {
            OnParse = (fileId, options, writer, ct) =>
                throw new ParseFailedException("boom", errorData: new Dictionary<string, object> { ["line"] = 12 }),
        };
        var session = await RunScriptedAsync(new[] { MakeRequest(1, "parse", "{\"file_id\":\"f1\"}") }, handler);
        var doc = ParseLine(session.Lines.Single());
        Assert.Equal(-32003, doc.GetProperty("error").GetProperty("code").GetInt32());
        Assert.Equal(12, doc.GetProperty("error").GetProperty("data").GetProperty("line").GetInt32());
    }
}
