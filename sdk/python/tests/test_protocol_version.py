"""协议版本兼容断言（契约 C7 / P2-04）：SDK 固化的协议版本常量与协议正本一致。

单源纪律：协议版本唯一事实来源是 core/ab-protocol/src/lib.rs 的
`PROTOCOL_VERSION`（= 1）；docs/spec/plugin-manifest.schema.json 对
`min_protocol_version` 设 `minimum: 1`。SDK 零第三方依赖、不可引用
ab-protocol crate，故在本包固化常量，由本测试断言其与正本一致、防漂移。
"""

import json
from pathlib import Path

from analysisbuddy import MIN_PROTOCOL_VERSION, PROTOCOL_VERSION

SDK_DIR = Path(__file__).resolve().parents[1]
SAMPLE_MANIFEST = SDK_DIR / "examples" / "sample-plugin" / "plugin.json"


def test_protocol_version_matches_ab_protocol_contract():
    # core/ab-protocol/src/lib.rs: pub const PROTOCOL_VERSION: u32 = 1
    assert isinstance(PROTOCOL_VERSION, int)
    assert PROTOCOL_VERSION == 1


def test_min_protocol_version_matches_schema_minimum():
    # docs/spec/plugin-manifest.schema.json: "min_protocol_version": { "minimum": 1 }
    assert MIN_PROTOCOL_VERSION == 1


def test_min_max_ordering_sane():
    assert PROTOCOL_VERSION >= MIN_PROTOCOL_VERSION


def test_sample_manifest_declares_supported_version():
    manifest = json.loads(SAMPLE_MANIFEST.read_text(encoding="utf-8"))
    assert manifest["min_protocol_version"] == MIN_PROTOCOL_VERSION
    assert manifest["min_protocol_version"] <= PROTOCOL_VERSION
