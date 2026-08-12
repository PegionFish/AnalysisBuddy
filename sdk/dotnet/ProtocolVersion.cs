// AnalysisBuddy.Sdk — protocol version constants (contract C7 / P2-04).
// Single source of truth: core/ab-protocol/src/lib.rs PROTOCOL_VERSION and
// docs/spec/plugin-manifest.schema.json minimum: 1. The SDK is BCL-only and
// cannot reference the ab-protocol crate, so the constants are fixed here;
// tests/ProtocolVersionTests.cs asserts them against the contract to prevent
// drift.

namespace AnalysisBuddy.Sdk;

/// <summary>Protocol version constants (protocol-v1.md §7.2).</summary>
public static class ProtocolVersion
{
    /// <summary>Protocol version this SDK implements; equals ab_protocol::PROTOCOL_VERSION (= 1).</summary>
    public const int Current = 1;

    /// <summary>Minimum min_protocol_version a manifest may declare (schema minimum; = 1).</summary>
    public const int Minimum = 1;
}
