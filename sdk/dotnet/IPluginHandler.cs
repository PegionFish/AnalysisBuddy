// AnalysisBuddy.Sdk — plugin handler contract (sdk-plugins.md §2.2).
//
// A plugin author implements IPluginHandler (or subclasses PluginHandlerBase
// and overrides only the methods that need behavior). PluginHost drives the
// handler over stdio as a JSON-RPC 2.0 server.

using System.Text.Json;

namespace AnalysisBuddy.Sdk;

/// <summary>Plugin handler contract: one method per protocol request (protocol-v1.md §2).</summary>
public interface IPluginHandler
{
    /// <summary>Plugin identity; feeds the initialize response (protocol-v1.md §2.1).</summary>
    PluginInfo Info { get; }

    /// <summary>File-match probe (protocol-v1.md §2.2).</summary>
    Task<CanHandleResult> CanHandleAsync(CanHandleParams p, CancellationToken ct);

    /// <summary>Load a file and retain raw data (protocol-v1.md §2.3). Return null for an empty summary.</summary>
    Task<FileSummary?> LoadFileAsync(LoadFileParams p, CancellationToken ct);

    /// <summary>Streaming parse; emits records/progress through <paramref name="writer"/>.
    /// Returns records_total (protocol-v1.md §2.4).</summary>
    Task<ulong> ParseAsync(string fileId, JsonElement? options, RecordBatchWriter writer, CancellationToken ct);

    /// <summary>Metric list declaration (protocol-v1.md §2.5).</summary>
    Task<SchemaResult> SchemaAsync(CancellationToken ct);

    /// <summary>Key state values at time T (protocol-v1.md §2.6).</summary>
    Task<KeyValuesResult> KeyValuesAsync(string fileId, long timestampMs, CancellationToken ct);

    /// <summary>Event annotations over a closed time range (protocol-v1.md §2.7; optional capability).</summary>
    Task<AnnotateResult> AnnotateAsync(string fileId, TimeRange range, CancellationToken ct);

    /// <summary>Unload a file and release retained memory (protocol-v1.md §2.8; idempotent).</summary>
    Task UnloadFileAsync(string fileId, CancellationToken ct);
}

/// <summary>Convenience base class with safe defaults (sdk-plugins.md §2.2):
/// CanHandleAsync abstains, AnnotateAsync throws -32005, UnloadFileAsync is a no-op.</summary>
public abstract class PluginHandlerBase : IPluginHandler
{
    /// <summary>Plugin identity; feeds the initialize response (protocol-v1.md §2.1).</summary>
    public abstract PluginInfo Info { get; }

    /// <summary>Default abstain (can_handle=false, confidence=0).</summary>
    public virtual Task<CanHandleResult> CanHandleAsync(CanHandleParams p, CancellationToken ct)
        => Task.FromResult(new CanHandleResult(false, 0, null));

    /// <summary>Loads a file; return null for an empty summary (protocol-v1.md §2.3).</summary>
    public abstract Task<FileSummary?> LoadFileAsync(LoadFileParams p, CancellationToken ct);

    /// <summary>Streaming parse (protocol-v1.md §2.4); returns records_total.</summary>
    public abstract Task<ulong> ParseAsync(string fileId, JsonElement? options, RecordBatchWriter writer, CancellationToken ct);

    /// <summary>Metric list declaration (protocol-v1.md §2.5).</summary>
    public abstract Task<SchemaResult> SchemaAsync(CancellationToken ct);

    /// <summary>Key state values at time T (protocol-v1.md §2.6).</summary>
    public abstract Task<KeyValuesResult> KeyValuesAsync(string fileId, long timestampMs, CancellationToken ct);

    /// <summary>Default -32005: annotate is an optional capability.</summary>
    public virtual Task<AnnotateResult> AnnotateAsync(string fileId, TimeRange range, CancellationToken ct)
        => throw new Errors.UnsupportedInV1Exception("annotate capability is not implemented");

    /// <summary>Default no-op (protocol-v1.md §2.8; idempotent).</summary>
    public virtual Task UnloadFileAsync(string fileId, CancellationToken ct) => Task.CompletedTask;
}

/// <summary>Stderr logger (protocol-v1.md §1.1: stderr is the plugin log channel; stdout is
/// protocol-only). Writes level-prefixed lines, e.g. <c>INFO|... message</c>.</summary>
public static class PluginLog
{
    private static readonly object Gate = new();
    private static TextWriter? _sink;

    /// <summary>Points the logger at a writer (the host captures stderr); thread-safe.</summary>
    public static void Configure(TextWriter? sink) => _sink = sink;

    /// <summary>Logs at INFO level.</summary>
    public static void Info(string message) => Write("INFO", message);

    /// <summary>Logs at WARN level.</summary>
    public static void Warn(string message) => Write("WARN", message);

    /// <summary>Logs at ERROR level.</summary>
    public static void Error(string message) => Write("ERROR", message);

    private static void Write(string level, string message)
    {
        var sink = _sink ?? Console.Error;
        lock (Gate)
        {
            sink.WriteLine($"{level}|{message}");
        }
    }
}
