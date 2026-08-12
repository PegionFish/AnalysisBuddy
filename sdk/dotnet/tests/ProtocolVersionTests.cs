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

    [Fact]
    public void Current_MatchesAbProtocolContract()
    {
        // core/ab-protocol/src/lib.rs: pub const PROTOCOL_VERSION: u32 = 1
        Assert.Equal(1, ProtocolVersion.Current);
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
