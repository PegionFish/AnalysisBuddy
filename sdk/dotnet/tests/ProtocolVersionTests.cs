// Protocol-version compatibility assertions (contract C7 / P2-04): the SDK's
// fixed protocol constants must match the contract's single source of truth
// (core/ab-protocol/src/lib.rs PROTOCOL_VERSION = 1 and
// docs/spec/plugin-manifest.schema.json minimum: 1). The SDK is BCL-only and
// cannot reference the ab-protocol crate, so the constants are fixed here and
// this test guards against drift.

using System.Text.Json;
using AnalysisBuddy.Sdk;
using Xunit;

namespace AnalysisBuddy.Sdk.Tests;

public class ProtocolVersionTests
{
    private static string FindSampleManifest()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            var candidate = Path.Combine(dir.FullName, "examples", "sample-plugin-csharp", "plugin.json");
            if (File.Exists(candidate))
            {
                return candidate;
            }

            dir = dir.Parent;
        }

        throw new FileNotFoundException("examples/sample-plugin-csharp/plugin.json not found");
    }

    // G4（卷三主题 5 / A5-P1-4）：防漂移测试解析协议正本
    // core/ab-protocol/src/lib.rs 的 PROTOCOL_VERSION 常量，不再断言字面量。
    private static int ContractProtocolVersion()
    {
        var candidates = new[]
        {
            Path.Combine(Directory.GetCurrentDirectory(), "..", "..", "..", "..", "..", "core", "ab-protocol", "src", "lib.rs"),
        };
        foreach (var path in candidates)
        {
            if (!File.Exists(path)) continue;
            var text = File.ReadAllText(path);
            var m = System.Text.RegularExpressions.Regex.Match(
                text, @"pub const PROTOCOL_VERSION:\s*u32\s*=\s*(\d+)");
            Assert.True(m.Success, $"PROTOCOL_VERSION constant missing in {path}");
            return int.Parse(m.Groups[1].Value);
        }
        throw new FileNotFoundException("core/ab-protocol/src/lib.rs not found (run from repo checkout)");
    }

    [Fact]
    public void Current_MatchesAbProtocolContract()
    {
        Assert.Equal(ContractProtocolVersion(), ProtocolVersion.Current);
    }

    [Fact]
    public void Minimum_MatchesManifestSchema()
    {
        // docs/spec/plugin-manifest.schema.json: "min_protocol_version": { "minimum": 1 }
        Assert.Equal(1, ProtocolVersion.Minimum);
    }

    [Fact]
    public void Current_IsAtLeastMinimum()
    {
        Assert.True(ProtocolVersion.Current >= ProtocolVersion.Minimum);
    }

    [Fact]
    public void SamplePluginManifest_DeclaresSupportedVersion()
    {
        using var doc = JsonDocument.Parse(File.ReadAllText(FindSampleManifest()));
        int min = doc.RootElement.GetProperty("min_protocol_version").GetInt32();
        Assert.Equal(ProtocolVersion.Minimum, min);
        Assert.True(min <= ProtocolVersion.Current);
    }
}
