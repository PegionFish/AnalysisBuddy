// D2-02 DoD smoke tests: launch the `dotnet publish` artifact and drive a
// minimal protocol sequence, asserting:
//  - shutdown 后 3s 内进程退出码 0（BEH-10）
//  - stdin 关闭后进程自行退出（BEH-12）
//  - initialize 响应 id 与 manifest id 一致（BEH-01）
//  - Record.metric 全部在 schema 声明集内（BEH-05 前置）
//  - RecordBatch seq 无缺号、records_total 等于各批之和（BEH-06）

using System.Diagnostics;
using System.Text;
using System.Text.Json;
using Xunit;

namespace SamplePluginCSharp.Tests;

public class SamplePluginSmokeTests : IDisposable
{
    private readonly string _exe;
    private readonly Process _proc;
    private readonly StreamWriter _stdin;
    private readonly StreamReader _stdout;
    private readonly StreamReader _stderr;

    public SamplePluginSmokeTests()
    {
        _exe = FindPublishArtifact();
        _proc = new Process
        {
            StartInfo = new ProcessStartInfo
            {
                FileName = _exe,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                WorkingDirectory = Path.GetDirectoryName(_exe)!,
            },
        };
        Assert.True(_proc.Start(), "plugin process should start");
        _stdin = _proc.StandardInput;
        _stdin.NewLine = "\n"; // NDJSON frames end with LF only (protocol-v1.md §1.2)
        _stdout = _proc.StandardOutput;
        _stderr = _proc.StandardError;
    }

    public void Dispose()
    {
        try
        {
            if (!_proc.HasExited)
            {
                _proc.Kill();
                _proc.WaitForExit();
            }
        }
        finally
        {
            _proc.Dispose();
        }
    }

    /// <summary>Locates the published plugin artifact. The card's verification flow runs
    /// `dotnet publish -c Release -o publish` before `dotnet test`, so the artifact is
    /// expected to exist; we never publish from within the test to avoid racing the
    /// solution build on the shared obj directory.</summary>
    private static string FindPublishArtifact()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !File.Exists(Path.Combine(dir.FullName, "examples", "sample-plugin-csharp", "SamplePluginCSharp.csproj")))
        {
            dir = dir.Parent;
        }

        Assert.NotNull(dir);

        var repo = Path.Combine(dir.FullName, "examples", "sample-plugin-csharp");
        var exe = Path.Combine(repo, "publish", "SamplePluginCSharp.exe");
        Assert.True(
            File.Exists(exe),
            $"publish artifact missing: {exe}. Run `dotnet publish -c Release -o publish` in sdk/dotnet/examples/sample-plugin-csharp first.");
        return exe;
    }

    private void WriteRequest(long id, string method, string? paramsJson = null)
    {
        var prm = paramsJson is null ? string.Empty : $",\"params\":{paramsJson}";
        _stdin.WriteLine($"{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\"{prm}}}");
        _stdin.Flush();
    }

    private JsonElement ReadResponse(int timeoutMs = 5000)
    {
        var task = _stdout.ReadLineAsync();
        Assert.True(task.Wait(timeoutMs), "plugin should answer within timeout");
        var line = task.Result;
        Assert.False(string.IsNullOrEmpty(line), "stdout closed unexpectedly");
        return JsonDocument.Parse(line).RootElement.Clone();
    }

    [Fact]
    public async Task MinimalSequence_AllResponsesWellFormed()
    {
        var fixture = Path.Combine(
            Directory.GetParent(_exe)!.Parent!.FullName, "sample.csv");

        WriteRequest(1, "initialize");
        var init = ReadResponse();
        Assert.Equal(1L, init.GetProperty("id").GetInt64());
        var result = init.GetProperty("result");
        Assert.Equal("sample-plugin-csharp", result.GetProperty("id").GetString()); // BEH-01
        Assert.False(result.GetProperty("capabilities").GetProperty("annotate").GetBoolean());

        WriteRequest(2, "schema");
        var schema = ReadResponse();
        var metrics = schema.GetProperty("result").GetProperty("metrics");
        var ids = metrics.EnumerateArray().Select(m => m.GetProperty("id").GetString()!).ToHashSet();
        Assert.Contains("fps", ids);
        Assert.Contains("frame_ms", ids);

        WriteRequest(3, "can_handle",
            $"{{\"path\":\"{fixture.Replace("\\", "\\\\")}\",\"name\":\"sample.csv\",\"ext\":\"csv\",\"size_bytes\":{new FileInfo(fixture).Length},\"head_sample\":\"timestamp,\"}}");
        var can = ReadResponse();
        Assert.True(can.GetProperty("result").GetProperty("can_handle").GetBoolean());

        WriteRequest(4, "load_file",
            $"{{\"file_id\":\"f-0001\",\"path\":\"{fixture.Replace("\\", "\\\\")}\"}}");
        Assert.Equal(4L, ReadResponse().GetProperty("id").GetInt64());

        WriteRequest(5, "parse", "{\"file_id\":\"f-0001\"}");

        // Collect frames until the parse response: RecordBatch notifications + final response.
        ulong total = 0;
        long expectedSeq = 0;
        JsonElement? parseResp = null;
        while (parseResp is null)
        {
            var frame = ReadResponse();
            if (frame.TryGetProperty("method", out var method) && method.GetString() == "RecordBatch")
            {
                var batch = frame.GetProperty("params");
                Assert.Equal((long)expectedSeq, batch.GetProperty("seq").GetInt64()); // BEH-06 seq
                expectedSeq++;
                foreach (var rec in batch.GetProperty("records").EnumerateArray())
                {
                    Assert.True(rec.TryGetProperty("timestamp", out _));
                    var metric = rec.GetProperty("metric").GetString()!;
                    Assert.Contains(metric, ids); // BEH-05 metric ∈ schema
                    total++;
                }
            }
            else if (frame.TryGetProperty("result", out _))
            {
                parseResp = frame;
            }
        }

        Assert.Equal(5L, parseResp!.Value.GetProperty("id").GetInt64());
        Assert.Equal(total, parseResp.Value.GetProperty("result").GetProperty("records_total").GetUInt64()); // BEH-06

        WriteRequest(6, "key_values", "{\"file_id\":\"f-0001\",\"timestamp_ms\":1785600002000}");
        Assert.Equal(6L, ReadResponse().GetProperty("id").GetInt64());

        WriteRequest(7, "unload_file", "{\"file_id\":\"f-0001\"}");
        Assert.Equal(7L, ReadResponse().GetProperty("id").GetInt64());

        WriteRequest(8, "shutdown");
        Assert.Equal(8L, ReadResponse().GetProperty("id").GetInt64());

        // BEH-10: shutdown 后 3s 内进程退出码 0
        Assert.True(_proc.WaitForExit(3000), "plugin must exit within 3s of shutdown");
        Assert.Equal(0, _proc.ExitCode);
    }

    [Fact]
    public async Task StdinEof_ProcessExitsOnItsOwn()
    {
        WriteRequest(1, "initialize");
        Assert.Equal(1L, ReadResponse().GetProperty("id").GetInt64());
        _stdin.Close(); // stdin EOF，无 shutdown

        // BEH-12: stdin 关闭后插件应自行退出（禁止孤儿进程），退出码 0
        Assert.True(_proc.WaitForExit(5000), "plugin must exit on its own after stdin EOF");
        Assert.Equal(0, _proc.ExitCode);
    }
}
