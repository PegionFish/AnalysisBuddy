// SamplePluginCSharp — compliant example plugin built on AnalysisBuddy.Sdk.
//
// Mirrors the Python sample plugin (sdk/python/examples/sample-plugin) in
// behavior so that both SDKs produce identical verdicts under the E-track
// `plugin check` protocol replay (cross-SDK consistency, sdk-plugins.md
// appendix A). Parses a minimal CSV: header `timestamp,fps,frame_ms`, one
// record per metric per row.
//
// Top-level statement entry: `await PluginHost.RunAsync(new SampleHandler())`.

using System.Text.Json;
using AnalysisBuddy.Sdk;
using AnalysisBuddy.Sdk.Errors;

// Top-level statement entry (sdk-plugins.md §2.6): the plugin process starts here.
await PluginHost.RunAsync(new SampleHandler());

sealed class SampleHandler : PluginHandlerBase
{
    private readonly Dictionary<string, string> _paths = new();

    public override PluginInfo Info => new("sample-plugin-csharp", "Sample Plugin (C#)", "0.1.0");

    public override Task<CanHandleResult> CanHandleAsync(CanHandleParams p, CancellationToken ct)
        => Task.FromResult(new CanHandleResult(p.Ext == "csv", 0.9, null));

    public override Task<FileSummary?> LoadFileAsync(LoadFileParams p, CancellationToken ct)
    {
        if (!File.Exists(p.Path))
        {
            throw new FileLoadFailedException($"file not found: {p.Path}");
        }

        _paths[p.FileId] = p.Path;
        return Task.FromResult<FileSummary?>(new FileSummary(RecordCountHint: 4, TimeRange: null, Note: "sample csv"));
    }

    public override async Task<ulong> ParseAsync(
        string fileId, JsonElement? options, RecordBatchWriter writer, CancellationToken ct)
    {
        string path = _paths[fileId];
        long fileBytes = new FileInfo(path).Length;
        ulong total = 0;
        long bytesRead = 0;
        long lineNo = 0;
        foreach (var line in File.ReadLines(path))
        {
            writer.ThrowIfCancelled();
            lineNo++;
            bytesRead += System.Text.Encoding.UTF8.GetByteCount(line) + 1;
            if (lineNo == 1)
            {
                continue; // header
            }

            var parts = line.Split(',');
            if (parts.Length < 3 ||
                !long.TryParse(parts[0], out var ts) ||
                !double.TryParse(parts[1], out var fps) ||
                !double.TryParse(parts[2], out var frameMs))
            {
                throw new ParseFailedException(
                    $"malformed row at line {lineNo}",
                    errorData: new Dictionary<string, object> { ["line"] = lineNo });
            }

            await writer.EmitAsync(new Record(ts, "fps", fps), ct);
            await writer.EmitAsync(new Record(ts, "frame_ms", frameMs), ct);
            total += 2;
            await writer.ProgressAsync(
                percent: fileBytes > 0 ? Math.Min(100, bytesRead * 100.0 / fileBytes) : null,
                bytesRead: (ulong)bytesRead, ct);
        }

        return total;
    }

    public override Task<SchemaResult> SchemaAsync(CancellationToken ct)
        => Task.FromResult(new SchemaResult(new List<MetricDef>
        {
            new("fps", "FPS", "fps", "frames per second", Aggregation.Last),
            new("frame_ms", "Frame time", "ms", "frame duration", Aggregation.Avg),
        }));

    public override Task<KeyValuesResult> KeyValuesAsync(string fileId, long timestampMs, CancellationToken ct)
        => Task.FromResult(new KeyValuesResult(new List<KeyValueEntry>
        {
            new("scene", "menu"),
            new("state", "idle"),
        }));

    public override async Task UnloadFileAsync(string fileId, CancellationToken ct)
    {
        _paths.Remove(fileId);
        await Task.CompletedTask;
    }
}
