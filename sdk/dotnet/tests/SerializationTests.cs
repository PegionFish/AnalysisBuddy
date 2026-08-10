// D2-01 DoD tests: snake_case serialization, skip-if-empty, byte-equivalence
// with protocol-v1.md §3.5 examples, and hard error-code validation.

using System.Text.Json;
using AnalysisBuddy.Sdk;
using Xunit;

namespace AnalysisBuddy.Sdk.Tests;

public class SerializationTests
{
    private static string ToJson(object value) =>
        JsonSerializer.Serialize(value, Json.Options);

    [Fact]
    public void Record_SerializesSnakeCaseWithRequiredFieldsOnly()
    {
        var record = new Record(1785600000123, "fps", 59.8);
        Assert.Equal("{\"timestamp\":1785600000123,\"metric\":\"fps\",\"value\":59.8}", ToJson(record));
    }

    [Fact]
    public void Record_WithOptionalFields_ByteEquivalentToProtocolExample()
    {
        var record = new Record(1785600000123, "frame_ms", 16.7, "info", null, "2026-08-01T00:00:00.123Z,fps,59.8");
        Assert.Equal(
            "{\"timestamp\":1785600000123,\"metric\":\"frame_ms\",\"value\":16.7,\"level\":\"info\",\"raw_line\":\"2026-08-01T00:00:00.123Z,fps,59.8\"}",
            ToJson(record));
    }

    [Fact]
    public void Record_WithTags_MatchesProtocolExample()
    {
        var record = new Record(1785603599870, "fps", 31.2).WithTags(new Dictionary<string, string> { ["scene"] = "boss" });
        Assert.Equal(
            "{\"timestamp\":1785603599870,\"metric\":\"fps\",\"value\":31.2,\"tags\":{\"scene\":\"boss\"}}",
            ToJson(record));
    }

    [Fact]
    public void SkipIfEmpty_OmitsEmptyStringAndEmptyCollectionKeys()
    {
        var record = new Record(1, "m", 1.0, "", new Dictionary<string, string>(), "");
        var json = ToJson(record);
        Assert.Equal("{\"timestamp\":1,\"metric\":\"m\",\"value\":1}", json);
        Assert.DoesNotContain("\"level\"", json);
        Assert.DoesNotContain("\"tags\"", json);
        Assert.DoesNotContain("\"raw_line\"", json);
    }

    [Fact]
    public void SkipIfEmpty_NeverEmitsNullOrEmptyContainers()
    {
        var summary = FileSummary.Empty;
        Assert.Equal("{}", ToJson(summary));

        var result = new SchemaResult(new List<MetricDef>());
        // metrics is required → still serialized even when empty
        Assert.Equal("{\"metrics\":[]}", ToJson(result));
    }

    [Fact]
    public void MetricDef_OmitsEmptyUnitAndDescription()
    {
        var metric = new MetricDef("fps", "帧率", "", "", Aggregation.Last);
        var json = ToJson(metric);
        Assert.Equal("{\"id\":\"fps\",\"name\":\"帧率\",\"aggregation\":\"last\"}", json);
        Assert.DoesNotContain("\"unit\"", json);
        Assert.DoesNotContain("\"description\"", json);
    }

    [Fact]
    public void InitializeResult_MatchesProtocolExample()
    {
        var result = new InitializeResult(
            "builtin-csv", "CSV Universal Parser", "0.1.0",
            new Capabilities(false, false, false));
        Assert.Equal(
            "{\"id\":\"builtin-csv\",\"name\":\"CSV Universal Parser\",\"version\":\"0.1.0\"," +
            "\"capabilities\":{\"annotate\":false,\"subscribe\":false,\"binary_sidecar\":false}}",
            ToJson(result));
    }

    [Fact]
    public void KeyValuesResult_MatchesProtocolExample()
    {
        var result = new KeyValuesResult(new List<KeyValueEntry>
        {
            new("scene", "boss"),
            new("player_hp", 73, "%"),
            new("paused", false),
        });
        Assert.Equal(
            "{\"entries\":[{\"key\":\"scene\",\"value\":\"boss\"}," +
            "{\"key\":\"player_hp\",\"value\":73,\"unit\":\"%\"}," +
            "{\"key\":\"paused\",\"value\":false}]}",
            ToJson(result));
    }

    [Fact]
    public void KeyValueEntry_RejectsUnsupportedValueTypes()
    {
        Assert.ThrowsAny<JsonException>(() => ToJson(new KeyValueEntry("k", new object())));
    }

    [Fact]
    public void Error_WithIllegalCode_ThrowsOnSerialization()
    {
        var error = new RpcError { Code = -12345, Message = "custom" };
        Assert.ThrowsAny<Exception>(() => ToJson(error));
    }

    [Theory]
    [InlineData(-32700)]
    [InlineData(-32600)]
    [InlineData(-32601)]
    [InlineData(-32602)]
    [InlineData(-32603)]
    [InlineData(-32001)]
    [InlineData(-32002)]
    [InlineData(-32003)]
    [InlineData(-32004)]
    [InlineData(-32005)]
    public void Error_WithAllowedCode_Serializes(int code)
    {
        var error = new RpcError { Code = code, Message = "msg" };
        var json = ToJson(error);
        Assert.Contains($"\"code\":{code}", json);
        Assert.Contains("\"message\":\"msg\"", json);
    }

    [Fact]
    public void Error_OmitsDataWhenNull()
    {
        var error = new RpcError { Code = -32001, Message = "plugin busy" };
        Assert.Equal("{\"code\":-32001,\"message\":\"plugin busy\"}", ToJson(error));
    }

    [Fact]
    public void Error_WithData_SerializesData()
    {
        var error = new RpcError { Code = -32003, Message = "parse failed", Data = new Dictionary<string, object> { ["line"] = 42 } };
        Assert.Equal("{\"code\":-32003,\"message\":\"parse failed\",\"data\":{\"line\":42}}", ToJson(error));
    }

    [Fact]
    public void RecordBatchNotification_MatchesProtocolExampleShape()
    {
        var notification = new RpcNotification(
            "RecordBatch",
            new RecordBatchParams(
                "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c", 0,
                new List<Record>
                {
                    new(1785600000123, "fps", 59.8),
                    new(1785600000123, "frame_ms", 16.7, "info", null, "2026-08-01T00:00:00.123Z,fps,59.8"),
                },
                false));
        Assert.Equal(
            "{\"jsonrpc\":\"2.0\",\"method\":\"RecordBatch\",\"params\":{\"file_id\":\"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c\",\"seq\":0," +
            "\"records\":[{\"timestamp\":1785600000123,\"metric\":\"fps\",\"value\":59.8}," +
            "{\"timestamp\":1785600000123,\"metric\":\"frame_ms\",\"value\":16.7,\"level\":\"info\",\"raw_line\":\"2026-08-01T00:00:00.123Z,fps,59.8\"}],\"done\":false}}",
            ToJson(notification));
    }

    [Fact]
    public void ProgressNotification_MatchesProtocolExample()
    {
        var notification = new RpcNotification(
            "progress",
            new ProgressParams("f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c", 80.5, 1000, 819200));
        Assert.Equal(
            "{\"jsonrpc\":\"2.0\",\"method\":\"progress\",\"params\":{\"file_id\":\"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c\",\"percent\":80.5,\"records_so_far\":1000,\"bytes_read\":819200}}",
            ToJson(notification));
    }
}
