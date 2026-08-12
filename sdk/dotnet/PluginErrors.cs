// AnalysisBuddy.Sdk — exception-to-error-code mapping (sdk-plugins.md §2.5,
// protocol-v1.md §4). Exceptions thrown by plugin handlers are translated by
// PluginHost into JSON-RPC error responses. Custom codes outside the standard
// set ∪ {-32001…-32005} are forbidden; serialization of an error object is
// hard-validated against the allowed code set.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace AnalysisBuddy.Sdk.Errors;

/// <summary>Base class of all plugin-protocol exceptions with a fixed error code.</summary>
public abstract class PluginException : Exception
{
    /// <summary>JSON-RPC error code of this exception (protocol-v1.md §4).</summary>
    public abstract int ErrorCode { get; }

    /// <summary>Short English message carried in the error object.</summary>
    public abstract string ErrorMessage { get; }

    /// <summary>Base constructor.</summary>
    protected PluginException(string? message, Exception? innerException)
        : base(message, innerException)
    {
    }
}

/// <summary>Plugin busy (concurrent parse on the same file_id, load limit, ...). code=-32001.</summary>
public sealed class PluginBusyException : PluginException
{
    /// <summary>-32001.</summary>
    public override int ErrorCode => -32001;
    /// <summary>"plugin busy".</summary>
    public override string ErrorMessage => "plugin busy";

    /// <summary>Creates a plugin-busy error.</summary>
    public PluginBusyException(string? message = null, Exception? innerException = null)
        : base(message, innerException)
    {
    }
}

/// <summary>File could not be loaded (missing / unreadable / format mismatch). code=-32002.</summary>
public sealed class FileLoadFailedException : PluginException
{
    /// <summary>-32002.</summary>
    public override int ErrorCode => -32002;
    /// <summary>"file load failed".</summary>
    public override string ErrorMessage => "file load failed";

    /// <summary>Creates a file-load-failed error.</summary>
    public FileLoadFailedException(string? message = null, Exception? innerException = null)
        : base(message, innerException)
    {
    }
}

/// <summary>Mid-parse failure; optional <see cref="ErrorData"/> becomes error.data. code=-32003.</summary>
public sealed class ParseFailedException : PluginException
{
    /// <summary>-32003.</summary>
    public override int ErrorCode => -32003;
    /// <summary>"parse failed".</summary>
    public override string ErrorMessage => "parse failed";

    /// <summary>Optional detail payload carried into error.data (e.g. {"line": 42}).</summary>
    public IReadOnlyDictionary<string, object>? ErrorData { get; }

    /// <summary>Creates a parse-failed error with optional structured detail.</summary>
    public ParseFailedException(
        string? message = null,
        Exception? innerException = null,
        IReadOnlyDictionary<string, object>? errorData = null)
        : base(message, innerException)
    {
        ErrorData = errorData;
    }
}

/// <summary>Parse cancelled by cancel_parse; the SDK throws this from
/// <see cref="RecordBatchWriter.ThrowIfCancelled"/>. code=-32004.</summary>
public sealed class CancelledException : PluginException
{
    /// <summary>-32004.</summary>
    public override int ErrorCode => -32004;
    /// <summary>"cancelled".</summary>
    public override string ErrorMessage => "cancelled";

    /// <summary>Creates a cancelled-parse error.</summary>
    public CancelledException(string? message = null, Exception? innerException = null)
        : base(message, innerException)
    {
    }
}

/// <summary>A capability not implemented in v1 was invoked (e.g. annotate when disabled). code=-32005.</summary>
public sealed class UnsupportedInV1Exception : PluginException
{
    /// <summary>-32005.</summary>
    public override int ErrorCode => -32005;
    /// <summary>"unsupported in v1".</summary>
    public override string ErrorMessage => "unsupported in v1";

    /// <summary>Creates an unsupported-in-v1 error.</summary>
    public UnsupportedInV1Exception(string? message = null, Exception? innerException = null)
        : base(message, innerException)
    {
    }
}

/// <summary>Request params judged invalid (file_id not loaded, malformed params, ...). code=-32602.</summary>
public sealed class InvalidParamsException : PluginException
{
    /// <summary>-32602.</summary>
    public override int ErrorCode => -32602;
    /// <summary>"Invalid params".</summary>
    public override string ErrorMessage => "Invalid params";

    /// <summary>Creates an invalid-params error.</summary>
    public InvalidParamsException(string? message = null, Exception? innerException = null)
        : base(message, innerException)
    {
    }
}

/// <summary>JSON-RPC error serialization with hard code validation: a code outside the
/// allowed set throws at serialization time (protocol-v1.md §4.2: plugins MUST NOT
/// use custom codes outside -32001…-32005).</summary>
public sealed class RpcErrorConverter : JsonConverter<RpcError>
{
    /// <summary>Default constructor (required by System.Text.Json).</summary>
    public RpcErrorConverter()
    {
    }

    /// <summary>Not supported: errors are write-only on the plugin side.</summary>
    public override RpcError? Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
        => throw new NotSupportedException("RpcError deserialization is not supported");

    /// <summary>Writes the error object, hard-validating the code against <see cref="RpcError.AllowedCodes"/>.</summary>
    public override void Write(Utf8JsonWriter writer, RpcError value, JsonSerializerOptions options)
    {
        if (!RpcError.AllowedCodes.Contains(value.Code))
        {
            throw new InvalidOperationException(
                $"illegal error code {value.Code}: plugins must only use standard codes and -32001..-32005");
        }

        writer.WriteStartObject();
        writer.WriteNumber("code", value.Code);
        writer.WriteString("message", value.Message);
        if (value.Data is not null)
        {
            writer.WritePropertyName("data");
            JsonSerializer.Serialize(writer, value.Data, value.Data.GetType(), options);
        }

        writer.WriteEndObject();
    }
}
