// AnalysisBuddy.Sdk — JSON-RPC 2.0 server main loop (sdk-plugins.md §2.3,
// protocol-v1.md §1/§2/§3.4). Behavior contract, aligned with the Python SDK
// serve():
// - stdin is read line by line with 8 MiB pre-validation (NdjsonTransport);
// - stdout frames are written atomically under a send lock;
// - logs go to stderr via PluginLog (INFO/WARN/ERROR prefixes);
// - stdin EOF → flush → normal return (exit code 0);
// - shutdown and cancel_parse are answered automatically;
// - a second parse on the same file_id is answered with -32001;
// - unknown methods are answered with -32601; malformed requests with -32600;
// - all ten methods are routed.

using System.Collections.Concurrent;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace AnalysisBuddy.Sdk;

/// <summary>Main plugin entry point: runs the plugin as a JSON-RPC server over stdio.</summary>
public static class PluginHost
{
    /// <summary>Heartbeat interval; internal so tests can shrink it.</summary>
    internal static TimeSpan HeartbeatInterval { get; set; } = TimeSpan.FromSeconds(2);

    /// <summary>Runs the handler over the process stdin/stdout (exit code 0 on EOF/shutdown).</summary>
    public static Task RunAsync(IPluginHandler handler, CancellationToken ct = default)
        => RunAsync(
            handler,
            Console.OpenStandardInput(),
            Console.OpenStandardOutput(),
            Console.Error,
            ct);

    /// <summary>Runs the handler over explicit streams (testable overload).</summary>
    public static Task RunAsync(
        IPluginHandler handler,
        Stream stdin,
        Stream stdout,
        TextWriter? stderrLog = null,
        CancellationToken ct = default)
    {
        ArgumentNullException.ThrowIfNull(handler);
        PluginLog.Configure(stderrLog ?? Console.Error);
        var session = new PluginHostSession(handler, new NdjsonTransport(stdin, stdout), ct);
        return session.RunAsync();
    }

    /// <summary>True when the handler provides a real annotate implementation (capabilities.annotate).</summary>
    internal static bool SupportsAnnotate(IPluginHandler handler)
    {
        if (handler is not PluginHandlerBase)
        {
            // Direct interface implementers must provide a body.
            return true;
        }

        var method = handler.GetType().GetMethod(nameof(IPluginHandler.AnnotateAsync));
        return method is not null && method.DeclaringType != typeof(PluginHandlerBase);
    }
}

/// <summary>JSON-RPC request envelope received from the host.</summary>
internal sealed class RpcRequest
{
    [JsonPropertyName("jsonrpc")]
    public string? JsonRpc { get; init; }

    [JsonPropertyName("id")]
    public ulong? Id { get; init; }

    [JsonPropertyName("method")]
    public string? Method { get; init; }

    [JsonPropertyName("params")]
    public JsonElement? Params { get; init; }
}

/// <summary>JSON-RPC response envelope (success or error).</summary>
internal sealed class RpcResponse
{
    [JsonPropertyName("jsonrpc")]
    public string JsonRpc => "2.0";

    [JsonPropertyName("id")]
    public ulong? Id { get; init; }

    [JsonPropertyName("result")]
    public object? Result { get; init; }

    [JsonPropertyName("error")]
    public RpcError? Error { get; init; }
}

internal sealed class PluginHostSession
{
    private readonly IPluginHandler _handler;
    private readonly NdjsonTransport _transport;
    private readonly CancellationToken _ct;
    private readonly ConcurrentDictionary<string, CancellationTokenSource> _activeParses = new();
    private readonly ConcurrentDictionary<string, Task> _parseTasks = new();
    private volatile bool _shutdown;

    public PluginHostSession(IPluginHandler handler, NdjsonTransport transport, CancellationToken ct)
    {
        _handler = handler;
        _transport = transport;
        _ct = ct;
    }

    public async Task RunAsync()
    {
        try
        {
            while (!_shutdown)
            {
                string? line = await _transport.ReadLineAsync(_ct).ConfigureAwait(false);
                if (line is null)
                {
                    // stdin EOF → flush → exit code 0 (protocol-v1.md §9 #5).
                    break;
                }

                await DispatchAsync(line).ConfigureAwait(false);
            }
        }
        catch (ProtocolViolationException ex)
        {
            // §1.2/§1.3: the host judges protocol errors and will kill us; log and exit non-zero.
            PluginLog.Error($"protocol violation: {ex.Message}");
            throw;
        }
        finally
        {
            foreach (var cts in _activeParses.Values)
            {
                cts.Cancel();
            }

            // Await in-flight parses (bounded) so their final frames land before we exit.
            if (_parseTasks.Values.Count > 0)
            {
                try
                {
                    await Task.WhenAll(_parseTasks.Values).WaitAsync(TimeSpan.FromSeconds(5)).ConfigureAwait(false);
                }
                catch (Exception)
                {
                    // ignored: process is exiting anyway
                }
            }

            await _transport.FlushAsync(_ct).ConfigureAwait(false);
        }
    }

    private async Task DispatchAsync(string line)
    {
        RpcRequest? request;
        try
        {
            request = JsonSerializer.Deserialize<RpcRequest>(line, Json.Options);
        }
        catch (JsonException ex)
        {
            PluginLog.Warn($"malformed request frame: {ex.Message}");
            // 结构非法请求回 -32600（与 Python SDK serve() 对齐）
            await RespondErrorAsync(null, new RpcError { Code = -32600, Message = "Invalid Request" }).ConfigureAwait(false);
            return;
        }

        if (request is null || request.Id is null || request.Method is null)
        {
            await RespondErrorAsync(null, new RpcError { Code = -32600, Message = "Invalid Request" }).ConfigureAwait(false);
            return;
        }

        try
        {
            await RouteAsync(request).ConfigureAwait(false);
        }
        catch (Errors.PluginException pe)
        {
            await RespondErrorAsync(request.Id, ToRpcError(pe)).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            // swallow: session is shutting down
        }
        catch (Exception ex)
        {
            PluginLog.Error($"unhandled exception while dispatching {request.Method}: {ex}");
            await RespondErrorAsync(request.Id, new RpcError { Code = -32603, Message = "Internal error" }).ConfigureAwait(false);
        }
    }

    private async Task RouteAsync(RpcRequest request)
    {
        ulong id = request.Id!.Value;
        JsonElement? prm = request.Params;

        switch (request.Method)
        {
            case "initialize":
                await RespondResultAsync(id, new InitializeResult(
                    _handler.Info.Id, _handler.Info.Name, _handler.Info.Version,
                    Capabilities.Default(PluginHost.SupportsAnnotate(_handler)))).ConfigureAwait(false);
                return;

            case "can_handle":
            {
                var p = DeserializeParams<CanHandleParams>(prm);
                await RespondResultAsync(id, await _handler.CanHandleAsync(p, _ct).ConfigureAwait(false)).ConfigureAwait(false);
                return;
            }

            case "load_file":
            {
                var p = DeserializeParams<LoadFileParams>(prm);
                var summary = await _handler.LoadFileAsync(p, _ct).ConfigureAwait(false);
                await RespondResultAsync(id, summary ?? (object)new EmptyResult()).ConfigureAwait(false);
                return;
            }

            case "parse":
            {
                var p = DeserializeParams<ParseParams>(prm);
                var cts = new CancellationTokenSource();
                if (!_activeParses.TryAdd(p.FileId, cts))
                {
                    // Concurrent parse on the same file_id → -32001 (protocol-v1.md §2.4).
                    cts.Dispose();
                    await RespondErrorAsync(id, new RpcError { Code = -32001, Message = "plugin busy" }).ConfigureAwait(false);
                    return;
                }

                var parseTask = Task.Run(() => RunParseAsync(id, p, cts), _ct);
                _parseTasks[p.FileId] = parseTask;
                return;
            }

            case "schema":
                await RespondResultAsync(id, await _handler.SchemaAsync(_ct).ConfigureAwait(false)).ConfigureAwait(false);
                return;

            case "key_values":
            {
                var p = DeserializeParams<KeyValuesParams>(prm);
                await RespondResultAsync(id, await _handler.KeyValuesAsync(p.FileId, p.TimestampMs, _ct).ConfigureAwait(false)).ConfigureAwait(false);
                return;
            }

            case "annotate":
            {
                var p = DeserializeParams<AnnotateParams>(prm);
                await RespondResultAsync(id, await _handler.AnnotateAsync(p.FileId, p.Range, _ct).ConfigureAwait(false)).ConfigureAwait(false);
                return;
            }

            case "unload_file":
            {
                var p = DeserializeParams<FileIdParams>(prm);
                await _handler.UnloadFileAsync(p.FileId, _ct).ConfigureAwait(false);
                await RespondResultAsync(id, new EmptyResult()).ConfigureAwait(false);
                return;
            }

            case "shutdown":
                // Auto-answer {} → flush → exit code 0 (protocol-v1.md §2.9).
                await RespondResultAsync(id, new EmptyResult()).ConfigureAwait(false);
                await _transport.FlushAsync(_ct).ConfigureAwait(false);
                Environment.ExitCode = 0;
                _shutdown = true;
                return;

            case "cancel_parse":
            {
                var p = DeserializeParams<FileIdParams>(prm);
                if (_activeParses.TryGetValue(p.FileId, out var cts))
                {
                    cts.Cancel(); // author loop's ThrowIfCancelled() → -32004 for the parse request
                }

                await RespondResultAsync(id, new EmptyResult()).ConfigureAwait(false);
                return;
            }

            default:
                await RespondErrorAsync(id, new RpcError { Code = -32601, Message = "Method not found" }).ConfigureAwait(false);
                return;
        }
    }

    private async Task RunParseAsync(ulong id, ParseParams p, CancellationTokenSource parseCts)
    {
        var writer = new RecordBatchWriter(
            p.FileId,
            frame => _transport.WriteFrameAsync(frame, _ct),
            batchSize: 4000,
            heartbeatInterval: PluginHost.HeartbeatInterval);
        writer.AttachCancellation(parseCts.Token);

        var heartbeatTask = Task.Run(async () =>
        {
            try
            {
                await writer.HeartbeatLoopAsync(parseCts.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                // parse finished or cancelled; heartbeat stops
            }
        }, _ct);

        try
        {
            ulong total;
            try
            {
                total = await _handler.ParseAsync(p.FileId, p.Options, writer, parseCts.Token).ConfigureAwait(false);
                if (total != writer.RecordsEmitted)
                {
                    PluginLog.Warn(
                        $"parse records_total mismatch: handler reported {total}, writer emitted {writer.RecordsEmitted}");
                }

                // 末批：flush 残余 + done:true，随后才发最终 response（§3.2）。
                await writer.FlushAsync(_ct).ConfigureAwait(false);
                await RespondResultAsync(id, new ParseResult(writer.RecordsEmitted)).ConfigureAwait(false);
            }
            catch (Errors.CancelledException)
            {
                // 取消语义（§3.4）：可再发一批已就绪数据或直接 done:true，然后必须回 -32004。
                await writer.FlushAsync(_ct).ConfigureAwait(false);
                await RespondErrorAsync(id, new RpcError { Code = -32004, Message = "cancelled" }).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                await writer.FlushAsync(_ct).ConfigureAwait(false);
                await RespondErrorAsync(id, new RpcError { Code = -32004, Message = "cancelled" }).ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                PluginLog.Error($"parse failed: {ex}");
                await RespondErrorAsync(id, ToRpcError(ex)).ConfigureAwait(false);
            }
        }
        finally
        {
            parseCts.Cancel();
            try
            {
                await heartbeatTask.ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
            }

            _activeParses.TryRemove(p.FileId, out _);
            _parseTasks.TryRemove(p.FileId, out _);
            parseCts.Dispose();
            await writer.DisposeAsync().ConfigureAwait(false);
        }
    }

    private static T DeserializeParams<T>(JsonElement? prm)
    {
        try
        {
            if (prm is null || prm.Value.ValueKind != JsonValueKind.Object)
            {
                throw new Errors.InvalidParamsException("missing params");
            }

            return prm.Value.Deserialize<T>(Json.Options)
                   ?? throw new Errors.InvalidParamsException("params could not be deserialized");
        }
        catch (JsonException ex)
        {
            throw new Errors.InvalidParamsException($"invalid params: {ex.Message}", ex);
        }
    }

    /// <summary>Exception → error code mapping (sdk-plugins.md §2.5, protocol-v1.md §4).</summary>
    private static RpcError ToRpcError(Exception ex)
    {
        if (ex is Errors.PluginException pluginEx)
        {
            return new RpcError
            {
                Code = pluginEx.ErrorCode,
                Message = pluginEx.ErrorMessage,
                Data = pluginEx is Errors.ParseFailedException parseEx ? parseEx.ErrorData : null,
            };
        }

        PluginLog.Error($"unhandled exception mapped to -32603: {ex}");
        return new RpcError { Code = -32603, Message = "Internal error" };
    }

    private Task RespondResultAsync(ulong? id, object result)
        => _transport.WriteFrameAsync(new RpcResponse { Id = id, Result = result }, _ct);

    private Task RespondErrorAsync(ulong? id, RpcError error)
        => _transport.WriteFrameAsync(new RpcResponse { Id = id, Error = error }, _ct);
}
