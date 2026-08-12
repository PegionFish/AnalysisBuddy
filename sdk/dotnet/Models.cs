// AnalysisBuddy.Sdk — protocol POCOs and serialization configuration.
//
// All JSON field names follow the protocol snake_case convention and are
// declared explicitly via [JsonPropertyName]. Optional fields carry the
// [SkipIfEmpty] attribute so that null / empty string / empty collection
// values are omitted from serialized output (protocol-v1.md §3.1
// "skip if empty"); required fields (e.g. RecordBatch.records) always
// serialize even when empty.

using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

namespace AnalysisBuddy.Sdk;

/// <summary>Marks an optional protocol field: omitted when null / empty string / empty collection.</summary>
[AttributeUsage(AttributeTargets.Property)]
internal sealed class SkipIfEmptyAttribute : Attribute;

/// <summary>Shared JSON serializer configuration for all protocol frames.</summary>
internal static class Json
{
    public static readonly JsonSerializerOptions Options = CreateOptions();

    private static JsonSerializerOptions CreateOptions()
    {
        var options = new JsonSerializerOptions
        {
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
            // Non-ASCII (e.g. Chinese display names) is emitted as raw UTF-8, matching
            // the serde_json/Python serializer behavior of the other SDKs.
            Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
            TypeInfoResolver = new DefaultJsonTypeInfoResolver { Modifiers = { ApplySkipIfEmpty } },
        };
        return options;
    }

    private static void ApplySkipIfEmpty(JsonTypeInfo typeInfo)
    {
        if (typeInfo.Kind != JsonTypeInfoKind.Object)
        {
            return;
        }

        foreach (var prop in typeInfo.Properties)
        {
            if (prop.Get is null || prop.AttributeProvider is null)
            {
                continue;
            }

            if (prop.AttributeProvider.IsDefined(typeof(SkipIfEmptyAttribute), false))
            {
                var getter = prop.Get;
                prop.ShouldSerialize = (obj, _) =>
                {
                    var value = getter(obj);
                    return value switch
                    {
                        null => false,
                        string s => s.Length > 0,
                        System.Collections.IEnumerable e => e.GetEnumerator().MoveNext(),
                        _ => true,
                    };
                };
            }
        }
    }
}

/// <summary>Plugin identity metadata (id / display name / semver version).</summary>
public sealed record PluginInfo(string Id, string Name, string Version);

/// <summary>Plugin capabilities, advertised in the initialize response (protocol-v1.md §2.1).</summary>
public sealed record Capabilities(
    [property: JsonPropertyName("annotate")] bool Annotate,
    [property: JsonPropertyName("subscribe")] bool Subscribe,
    [property: JsonPropertyName("binary_sidecar")] bool BinarySidecar)
{
    /// <summary>Default capability set (annotate = whether the handler implements it).</summary>
    public static Capabilities Default(bool annotate) => new(annotate, false, false);
}

/// <summary>initialize result (protocol-v1.md §2.1).</summary>
public sealed record InitializeResult(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("version")] string Version,
    [property: JsonPropertyName("capabilities")] Capabilities Capabilities);

/// <summary>can_handle params (protocol-v1.md §2.2).</summary>
public sealed record CanHandleParams(
    [property: JsonPropertyName("path")] string Path,
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("ext")] string Ext,
    [property: JsonPropertyName("size_bytes")] ulong SizeBytes,
    [property: JsonPropertyName("head_sample")] string HeadSample);

/// <summary>can_handle result (protocol-v1.md §2.2).</summary>
public sealed record CanHandleResult(
    [property: JsonPropertyName("can_handle")] bool CanHandle,
    [property: JsonPropertyName("confidence")] double Confidence,
    [property: JsonPropertyName("reason")][property: SkipIfEmpty] string? Reason);

/// <summary>load_file params (protocol-v1.md §2.3).</summary>
public sealed record LoadFileParams(
    [property: JsonPropertyName("file_id")] string FileId,
    [property: JsonPropertyName("path")] string Path);

/// <summary>Closed time range in UTC milliseconds (protocol-v1.md §2.3 / §2.7).</summary>
public sealed record TimeRange(
    [property: JsonPropertyName("start_ms")] long StartMs,
    [property: JsonPropertyName("end_ms")] long EndMs);

/// <summary>load_file result; all fields optional (protocol-v1.md §2.3).</summary>
public sealed record FileSummary(
    [property: JsonPropertyName("record_count_hint")][property: SkipIfEmpty] ulong? RecordCountHint,
    [property: JsonPropertyName("time_range")] TimeRange? TimeRange,
    [property: JsonPropertyName("note")][property: SkipIfEmpty] string? Note)
{
    /// <summary>Empty summary instance (no record_count_hint / time_range / note).</summary>
    public static FileSummary Empty { get; } = new(null, null, null);
}

/// <summary>parse params (protocol-v1.md §2.4).</summary>
public sealed record ParseParams(
    [property: JsonPropertyName("file_id")] string FileId,
    [property: JsonPropertyName("options")] JsonElement? Options);

/// <summary>parse result (protocol-v1.md §2.4).</summary>
public sealed record ParseResult(
    [property: JsonPropertyName("records_total")] ulong RecordsTotal);

/// <summary>Metric aggregation enum values (protocol-v1.md §2.5).</summary>
public static class Aggregation
{
    /// <summary>"last" — the most recent value.</summary>
    public const string Last = "last";
    /// <summary>"sum" — sum over the range.</summary>
    public const string Sum = "sum";
    /// <summary>"avg" — average over the range.</summary>
    public const string Avg = "avg";
    /// <summary>"min" — minimum over the range.</summary>
    public const string Min = "min";
    /// <summary>"max" — maximum over the range.</summary>
    public const string Max = "max";
}

/// <summary>Metric definition (protocol-v1.md §2.5).</summary>
public sealed record MetricDef(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("unit")][property: SkipIfEmpty] string? Unit = null,
    [property: JsonPropertyName("description")][property: SkipIfEmpty] string? Description = null,
    [property: JsonPropertyName("aggregation")] string Aggregation = Aggregation.Avg);

/// <summary>schema result (protocol-v1.md §2.5); metrics is a required field.</summary>
public sealed record SchemaResult(
    [property: JsonPropertyName("metrics")] IReadOnlyList<MetricDef> Metrics);

/// <summary>key_values params (protocol-v1.md §2.6).</summary>
public sealed record KeyValuesParams(
    [property: JsonPropertyName("file_id")] string FileId,
    [property: JsonPropertyName("timestamp_ms")] long TimestampMs);

/// <summary>Single key/value state entry; value is string | number | boolean (protocol-v1.md §2.6).</summary>
public sealed record KeyValueEntry(
    [property: JsonPropertyName("key")] string Key,
    [property: JsonPropertyName("value")][property: JsonConverter(typeof(KeyValueValueConverter))] object Value,
    [property: JsonPropertyName("unit")][property: SkipIfEmpty] string? Unit = null);

/// <summary>key_values result (protocol-v1.md §2.6); entries is a required field.</summary>
public sealed record KeyValuesResult(
    [property: JsonPropertyName("entries")] IReadOnlyList<KeyValueEntry> Entries);

/// <summary>annotate params (protocol-v1.md §2.7).</summary>
public sealed record AnnotateParams(
    [property: JsonPropertyName("file_id")] string FileId,
    [property: JsonPropertyName("range")] TimeRange Range);

/// <summary>Annotation event (protocol-v1.md §2.7).</summary>
public sealed record AnnotateEvent(
    [property: JsonPropertyName("timestamp_ms")] long TimestampMs,
    [property: JsonPropertyName("label")] string Label,
    [property: JsonPropertyName("level")][property: SkipIfEmpty] string? Level = null);

/// <summary>annotate result (protocol-v1.md §2.7); events is a required field.</summary>
public sealed record AnnotateResult(
    [property: JsonPropertyName("events")] IReadOnlyList<AnnotateEvent> Events);

/// <summary>unload_file / cancel_parse params (protocol-v1.md §2.8 / §2.10).</summary>
public sealed record FileIdParams(
    [property: JsonPropertyName("file_id")] string FileId);

/// <summary>Normalized record (protocol-v1.md §3.1). Optional fields skip when empty.</summary>
public sealed record Record(
    [property: JsonPropertyName("timestamp")] long Timestamp,
    [property: JsonPropertyName("metric")] string Metric,
    [property: JsonPropertyName("value")] double Value,
    [property: JsonPropertyName("level")][property: SkipIfEmpty] string? Level = null,
    [property: JsonPropertyName("tags")][property: SkipIfEmpty] IReadOnlyDictionary<string, string>? Tags = null,
    [property: JsonPropertyName("raw_line")][property: SkipIfEmpty] string? RawLine = null)
{
    /// <summary>Creates a record with only the required fields.</summary>
    public Record(long timestamp, string metric, double value)
        : this(timestamp, metric, value, null, null, null)
    {
    }

    /// <summary>Returns a copy with the level field replaced.</summary>
    public Record WithLevel(string? level) => this with { Level = level };
    /// <summary>Returns a copy with the tags field replaced.</summary>
    public Record WithTags(IReadOnlyDictionary<string, string>? tags) => this with { Tags = tags };
    /// <summary>Returns a copy with the raw_line field replaced.</summary>
    public Record WithRawLine(string? rawLine) => this with { RawLine = rawLine };
}

/// <summary>RecordBatch notification params (protocol-v1.md §3.2); records is a required field.</summary>
public sealed record RecordBatchParams(
    [property: JsonPropertyName("file_id")] string FileId,
    [property: JsonPropertyName("seq")] ulong Seq,
    [property: JsonPropertyName("records")] IReadOnlyList<Record> Records,
    [property: JsonPropertyName("done")] bool Done);

/// <summary>progress notification params (protocol-v1.md §3.3); percent/bytes_read optional.</summary>
public sealed record ProgressParams(
    [property: JsonPropertyName("file_id")] string FileId,
    [property: JsonPropertyName("percent")] double? Percent,
    [property: JsonPropertyName("records_so_far")] ulong RecordsSoFar,
    [property: JsonPropertyName("bytes_read")] ulong? BytesRead);

/// <summary>Converts KeyValueEntry.Value between the wire form (string|number|boolean) and CLR scalars.</summary>
internal sealed class KeyValueValueConverter : JsonConverter<object>
{
    public override object? Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        return reader.TokenType switch
        {
            JsonTokenType.String => reader.GetString(),
            JsonTokenType.Number when reader.TryGetInt64(out var l) => l,
            JsonTokenType.Number => reader.GetDouble(),
            JsonTokenType.True => true,
            JsonTokenType.False => false,
            _ => throw new JsonException("KeyValueEntry.value must be a string, number, or boolean"),
        };
    }

    public override void Write(Utf8JsonWriter writer, object value, JsonSerializerOptions options)
    {
        switch (value)
        {
            case string s:
                writer.WriteStringValue(s);
                break;
            case bool b:
                writer.WriteBooleanValue(b);
                break;
            case byte or sbyte or short or ushort or int or uint or long or ulong:
                writer.WriteNumberValue(Convert.ToInt64(value));
                break;
            case float f:
                writer.WriteNumberValue(f);
                break;
            case double d:
                writer.WriteNumberValue(d);
                break;
            case decimal m:
                writer.WriteNumberValue(m);
                break;
            default:
                throw new JsonException($"KeyValueEntry.value of type {value.GetType().Name} is not serializable");
        }
    }
}

/// <summary>Serializes as an empty object <c>{}</c> (e.g. load_file with no summary).</summary>
internal sealed record EmptyResult;

/// <summary>JSON-RPC 2.0 error object (protocol-v1.md §4). Code is hard-validated on serialization:
/// serializing an error with a code outside the allowed set throws (sdk-plugins.md §2.5).</summary>
[JsonConverter(typeof(AnalysisBuddy.Sdk.Errors.RpcErrorConverter))]
public sealed record RpcError
{
    /// <summary>JSON-RPC error code.</summary>
    [JsonPropertyName("code")]
    public int Code { get; init; }

    /// <summary>Short English message carried in the error object.</summary>
    [JsonPropertyName("message")]
    public string Message { get; init; } = string.Empty;

    /// <summary>Optional structured detail (protocol-v1.md §4.1).</summary>
    [JsonPropertyName("data")]
    public object? Data { get; init; }

    /// <summary>The only codes a plugin may emit (protocol-v1.md §4.1 + §4.2, rpc-messages.schema.json).</summary>
    public static readonly int[] AllowedCodes =
    [
        -32700, -32600, -32601, -32602, -32603,
        -32001, -32002, -32003, -32004, -32005,
    ];
}
