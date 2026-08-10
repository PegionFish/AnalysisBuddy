// D2-01 DoD tests: NdjsonTransport frame rules — 8MB pre-validation, stray '\r'
// rejection, frame boundary handling, EOF.

using System.Text;
using AnalysisBuddy.Sdk;
using Xunit;

namespace AnalysisBuddy.Sdk.Tests;

public class NdjsonTransportTests
{
    private static (NdjsonTransport Transport, MemoryStream In, MemoryStream Out) CreatePair()
    {
        var stdin = new MemoryStream();
        var stdout = new MemoryStream();
        return (new NdjsonTransport(stdin, stdout), stdin, stdout);
    }

    private static void Feed(MemoryStream stdin, params string[] lines)
    {
        var text = string.Join('\n', lines) + "\n";
        var bytes = Encoding.UTF8.GetBytes(text);
        stdin.Write(bytes, 0, bytes.Length);
        stdin.Position = 0;
    }

    [Fact]
    public async Task ReadsFramesAtBoundaries()
    {
        var (t, stdin, _) = CreatePair();
        Feed(stdin, "{\"a\":1}", "{\"b\":2}");

        Assert.Equal("{\"a\":1}", await t.ReadLineAsync());
        Assert.Equal("{\"b\":2}", await t.ReadLineAsync());
        Assert.Null(await t.ReadLineAsync()); // EOF
    }

    [Fact]
    public async Task LineJustUnderLimit_Accepted()
    {
        var (t, stdin, _) = CreatePair();
        var line = new string('x', (int)NdjsonTransport.MaxLineBytes - 2) + "{}";
        var bytes = Encoding.UTF8.GetBytes(line + "\n");
        stdin.Write(bytes, 0, bytes.Length);
        stdin.Position = 0;

        var read = await t.ReadLineAsync();
        Assert.NotNull(read);
    }

    [Fact]
    public async Task LineOverLimit_ThrowsProtocolViolation()
    {
        var (t, stdin, _) = CreatePair();
        var line = new string('x', (int)NdjsonTransport.MaxLineBytes + 1);
        var bytes = Encoding.UTF8.GetBytes(line + "\n");
        stdin.Write(bytes, 0, bytes.Length);
        stdin.Position = 0;

        await Assert.ThrowsAsync<ProtocolViolationException>(() => t.ReadLineAsync());
    }

    [Fact]
    public async Task StrayCarriageReturn_ThrowsProtocolViolation()
    {
        var (t, stdin, _) = CreatePair();
        Feed(stdin, "{\"a\":1}\r");

        await Assert.ThrowsAsync<ProtocolViolationException>(() => t.ReadLineAsync());
    }

    [Fact]
    public async Task EofInMiddleOfFrame_ThrowsProtocolViolation()
    {
        var (t, stdin, _) = CreatePair();
        var bytes = Encoding.UTF8.GetBytes("{\"partial\"");
        stdin.Write(bytes, 0, bytes.Length);
        stdin.Position = 0;

        await Assert.ThrowsAsync<ProtocolViolationException>(() => t.ReadLineAsync());
    }

    [Fact]
    public async Task WriteFrame_WritesSingleLineWithTrailingLf()
    {
        var (t, _, stdout) = CreatePair();
        await t.WriteFrameAsync(new RpcNotification("RecordBatch", new RecordBatchParams("f1", 0, new List<Record>(), true)));

        var text = Encoding.UTF8.GetString(stdout.ToArray());
        Assert.EndsWith("\n", text);
        Assert.Equal(
            "{\"jsonrpc\":\"2.0\",\"method\":\"RecordBatch\",\"params\":{\"file_id\":\"f1\",\"seq\":0,\"records\":[],\"done\":true}}\n",
            text);
    }

    [Fact]
    public async Task ConcurrentWrites_DoNotInterleave()
    {
        var (t, _, stdout) = CreatePair();
        var tasks = Enumerable.Range(0, 50).Select(i => t.WriteFrameAsync(new RpcNotification("p", new ProgressParams("f1", null, (ulong)i, null))));
        await Task.WhenAll(tasks);

        var text = Encoding.UTF8.GetString(stdout.ToArray());
        var lines = text.Split('\n', StringSplitOptions.RemoveEmptyEntries);
        Assert.Equal(50, lines.Length);
        // Each line is complete, well-formed JSON — no interleaving.
        foreach (var line in lines)
        {
            using var doc = System.Text.Json.JsonDocument.Parse(line);
            Assert.Equal("2.0", doc.RootElement.GetProperty("jsonrpc").GetString());
            Assert.Equal("p", doc.RootElement.GetProperty("method").GetString());
        }
    }
}
