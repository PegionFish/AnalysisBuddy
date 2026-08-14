//! 序列化契约测试（DoD：§3.5 四段示例往返 / skip-if-empty / NaN 报错 / 错误码比对）。
//!
//! 往返测试的 JSON 载荷逐字取自 `docs/spec/examples/frame-ok-*.json`
//! （protocol.md §3.5 四段完整示例的机器可校验副本），与原文档逐字节一致。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::errors;
use crate::manifest::{LocalizedName, Manifest, PresetDef, PresetEntry, PresetGroup};
use crate::types::{self, Record};

/// `from_str → to_string → 语义相等`：载荷反序列化后重新序列化，
/// 与原始 JSON 的语义（对象键序无关）完全一致。
fn assert_payload_roundtrip<T>(payload: &str)
where
    T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug + PartialEq,
{
    let parsed: T = serde_json::from_str(payload).expect("payload 必须可反序列化");
    let reencoded = serde_json::to_string(&parsed).expect("payload 必须可序列化");
    let original: serde_json::Value = serde_json::from_str(payload).unwrap();
    let roundtrip: serde_json::Value = serde_json::from_str(&reencoded).unwrap();
    assert_eq!(original, roundtrip, "载荷必须往返语义相等");
}

/// §3.5 ① initialize 握手（请求 + 响应两帧）。
#[test]
fn roundtrip_initialize_exchange() {
    assert_payload_roundtrip::<types::InitializeParams>(
        r#"{"protocol_version":1,"host_info":{"name":"AnalysisBuddy","version":"0.1.0"}}"#,
    );
    assert_payload_roundtrip::<types::InitializeResult>(
        r#"{"id":"builtin-csv","name":"CSV Universal Parser","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}"#,
    );

    let params: types::InitializeParams = serde_json::from_str(
        r#"{"protocol_version":1,"host_info":{"name":"AnalysisBuddy","version":"0.1.0"}}"#,
    )
    .unwrap();
    assert_eq!(params.protocol_version, crate::PROTOCOL_VERSION);
}

/// §3.5 ② load_file（请求 + 响应两帧）。
#[test]
fn roundtrip_load_file_exchange() {
    assert_payload_roundtrip::<types::LoadFileParams>(
        r#"{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","path":"C:\\logs\\match_20260807.csv"}"#,
    );
    assert_payload_roundtrip::<types::FileSummary>(
        r#"{"record_count_hint":128000,"time_range":{"start_ms":1785600000000,"end_ms":1785603600000},"note":"comma-separated, header row detected"}"#,
    );
}

/// §3.5 ③ parse 请求 + 首批/末批回传 + 最终响应（共五帧）。
#[test]
fn roundtrip_parse_exchange() {
    assert_payload_roundtrip::<types::ParseParams>(
        r#"{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c"}"#,
    );
    assert_payload_roundtrip::<types::ProgressParams>(
        r#"{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","percent":80.5,"records_so_far":1000,"bytes_read":819200}"#,
    );
    assert_payload_roundtrip::<types::RecordBatch>(
        r#"{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","seq":0,"records":[{"timestamp":1785600000123,"metric":"fps","value":59.8},{"timestamp":1785600000123,"metric":"frame_ms","value":16.7,"level":"info","raw_line":"2026-08-01T00:00:00.123Z,fps,59.8"}],"done":false}"#,
    );
    assert_payload_roundtrip::<types::RecordBatch>(
        r#"{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","seq":16,"records":[{"timestamp":1785603599870,"metric":"fps","value":31.2,"tags":{"scene":"boss"}}],"done":true}"#,
    );
    assert_payload_roundtrip::<types::ParseResult>(r#"{"records_total":128000}"#);
}

/// §3.5 ④ key_values（请求 + 响应两帧）。
#[test]
fn roundtrip_key_values_exchange() {
    assert_payload_roundtrip::<types::KeyValuesParams>(
        r#"{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","timestamp_ms":1785601234567}"#,
    );
    assert_payload_roundtrip::<types::KeyValuesResult>(
        r#"{"entries":[{"key":"scene","value":"boss"},{"key":"player_hp","value":73,"unit":"%"},{"key":"paused","value":false}]}"#,
    );
}

/// §7.4 manifest 完整示例（`rename = "match"` 双向成立）。
#[test]
fn roundtrip_manifest_example() {
    assert_payload_roundtrip::<Manifest>(
        r#"{"id":"builtin-csv","display_name":"CSV Universal Parser","version":"0.1.0","entry":{"command":"target/release/builtin-csv.exe","args":["--stdio"]},"match":{"extensions":["csv","tsv","txt"],"header_fingerprints":["timestamp,","time,"]},"min_protocol_version":1}"#,
    );
}

/// §3.1 skip-if-empty：空 `level` / `tags` / `raw_line` 输出不含该键且不含 `null`。
#[test]
fn record_skips_empty_optionals() {
    let record = Record {
        timestamp: 1_785_600_000_123,
        metric: "fps".to_string(),
        value: 59.8,
        level: None,
        tags: None,
        raw_line: None,
    };
    let s = serde_json::to_string(&record).unwrap();
    let obj = serde_json::from_str::<serde_json::Value>(&s)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    assert!(obj.get("level").is_none(), "空 level 必须省略该键");
    assert!(obj.get("tags").is_none(), "空 tags 必须省略该键");
    assert!(obj.get("raw_line").is_none(), "空 raw_line 必须省略该键");
    assert!(!s.contains("null"), "输出不得包含 null");
    assert_eq!(obj.len(), 3, "仅保留 timestamp/metric/value 三键");
}

/// §3.1 skip-if-empty 另一面：`Some("")` 空串与 `Some(空 map)` 同样省略。
#[test]
fn record_skips_empty_string_and_empty_map() {
    let record = Record {
        timestamp: 1_785_600_000_123,
        metric: "fps".to_string(),
        value: 59.8,
        level: Some(String::new()),
        tags: Some(BTreeMap::new()),
        raw_line: Some(String::new()),
    };
    let s = serde_json::to_string(&record).unwrap();
    let obj = serde_json::from_str::<serde_json::Value>(&s)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    assert!(obj.get("level").is_none());
    assert!(obj.get("tags").is_none());
    assert!(obj.get("raw_line").is_none());
    assert!(!s.contains("null"));
    assert_eq!(obj.len(), 3);
}

/// §3.1 `value: f64` 非有限数：serde_json 序列化报错（SDK 据此丢弃/置 0）。
#[test]
fn record_non_finite_value_serialization_errors() {
    let base = Record {
        timestamp: 1_785_600_000_123,
        metric: "fps".to_string(),
        value: 0.0,
        level: None,
        tags: None,
        raw_line: None,
    };
    assert!(serde_json::to_string(&Record {
        value: f64::NAN,
        ..base.clone()
    })
    .is_err());
    assert!(serde_json::to_string(&Record {
        value: f64::INFINITY,
        ..base
    })
    .is_err());
}

/// 错误码常量与 protocol.md §4 错误码表逐一相等。
#[test]
fn error_codes_match_doc() {
    assert_eq!(errors::ERR_PARSE_ERROR, -32_700);
    assert_eq!(errors::ERR_INVALID_REQUEST, -32_600);
    assert_eq!(errors::ERR_METHOD_NOT_FOUND, -32_601);
    assert_eq!(errors::ERR_INVALID_PARAMS, -32_602);
    assert_eq!(errors::ERR_INTERNAL_ERROR, -32_603);
    assert_eq!(errors::ERR_PLUGIN_BUSY, -32_001);
    assert_eq!(errors::ERR_FILE_LOAD_FAILED, -32_002);
    assert_eq!(errors::ERR_PARSE_FAILED, -32_003);
    assert_eq!(errors::ERR_CANCELLED, -32_004);
    assert_eq!(errors::ERR_UNSUPPORTED_IN_V1, -32_005);
}

/// §7.2 场景预设：全字段 PresetDef 往返（serialize → deserialize → 相等）。
#[test]
fn preset_def_full_roundtrip() {
    let def = PresetDef {
        id: "perf-scene".to_string(),
        name: LocalizedName {
            zh: "性能场景".to_string(),
            en: "Performance Scene".to_string(),
        },
        description: Some(LocalizedName {
            zh: "帧率/内存常规采集".to_string(),
            en: "Routine frame/memory capture".to_string(),
        }),
        entries: vec![
            PresetEntry {
                want: Some("frame_rate".to_string()),
                names: vec!["fps".to_string(), "frame_ms".to_string()],
            },
            PresetEntry {
                want: None,
                names: vec!["mem_used".to_string()],
            },
        ],
        groups: vec![
            PresetGroup {
                id: "platform".to_string(),
                name: LocalizedName {
                    zh: "平台".to_string(),
                    en: "Platform".to_string(),
                },
                entries: vec![PresetEntry {
                    want: Some("gpu_util".to_string()),
                    names: vec!["gpu_utilization".to_string()],
                }],
            },
            PresetGroup {
                id: "vendor".to_string(),
                name: LocalizedName {
                    zh: "供应商".to_string(),
                    en: "Vendor".to_string(),
                },
                entries: Vec::new(),
            },
        ],
        keywords: vec!["fps".to_string(), "memory".to_string()],
    };

    let json = serde_json::to_string(&def).unwrap();
    let back: PresetDef = serde_json::from_str(&json).unwrap();
    assert_eq!(def, back, "全字段 PresetDef 必须往返相等");
}

/// §3.1 skip-if-empty：空 entries/groups/keywords/description 序列化时省略键。
#[test]
fn preset_def_skips_empty_optionals() {
    let def = PresetDef {
        id: "empty-scene".to_string(),
        name: LocalizedName {
            zh: "空场景".to_string(),
            en: "Empty Scene".to_string(),
        },
        description: None,
        entries: Vec::new(),
        groups: Vec::new(),
        keywords: Vec::new(),
    };
    let s = serde_json::to_string(&def).unwrap();
    let obj = serde_json::from_str::<serde_json::Value>(&s)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    assert!(obj.get("description").is_none(), "空 description 必须省略该键");
    assert!(obj.get("entries").is_none(), "空 entries 必须省略该键");
    assert!(obj.get("groups").is_none(), "空 groups 必须省略该键");
    assert!(obj.get("keywords").is_none(), "空 keywords 必须省略该键");
    assert!(!s.contains("null"), "输出不得包含 null");
    assert_eq!(obj.len(), 2, "仅保留 id/name 两键");
    assert_eq!(
        s,
        r#"{"id":"empty-scene","name":{"zh":"空场景","en":"Empty Scene"}}"#,
        "仅剩必填键时的输出必须逐字节一致"
    );
}

/// §7.2 预设条目 skip-if-empty：空 want 省略该键。
#[test]
fn preset_entry_skips_empty_want() {
    let entry = PresetEntry {
        want: None,
        names: vec!["mem_used".to_string()],
    };
    let s = serde_json::to_string(&entry).unwrap();
    assert_eq!(s, r#"{"names":["mem_used"]}"#, "空 want 必须省略该键");
}

/// 兼容性：不含 presets 的旧 manifest JSON 反序列化成功且 presets 为 None。
#[test]
fn manifest_without_presets_still_deserializes() {
    let m: Manifest = serde_json::from_str(
        r#"{"id":"builtin-csv","display_name":"CSV Universal Parser","version":"0.1.0","entry":{"command":"target/release/builtin-csv.exe","args":["--stdio"]},"match":{"extensions":["csv","tsv","txt"],"header_fingerprints":["timestamp,","time,"]},"min_protocol_version":1,"author":"AnalysisBuddy Team","changelog":[{"version":"0.1.0","date":"2026-08-01","notes":["initial release"]}]}"#,
    )
    .unwrap();
    assert!(m.presets.is_none(), "旧 manifest 无 presets 键时必为 None");
    assert_eq!(m.author.as_deref(), Some("AnalysisBuddy Team"));
    assert_eq!(m.changelog.as_ref().map(Vec::len), Some(1));
}

/// 兼容性：带 presets 的 manifest 往返成功且语义相等。
#[test]
fn manifest_with_presets_roundtrips() {
    let m = Manifest {
        id: "builtin-csv".to_string(),
        display_name: "CSV Universal Parser".to_string(),
        version: "0.1.0".to_string(),
        entry: crate::manifest::PluginEntry {
            command: "target/release/builtin-csv.exe".to_string(),
            args: vec!["--stdio".to_string()],
            working_dir: None,
        },
        r#match: crate::manifest::MatchRules {
            extensions: vec!["csv".to_string()],
            header_fingerprints: None,
        },
        min_protocol_version: 1,
        author: None,
        repository: None,
        tools: None,
        update_url: None,
        changelog: None,
        presets: Some(vec![PresetDef {
            id: "perf-scene".to_string(),
            name: LocalizedName {
                zh: "性能场景".to_string(),
                en: "Performance Scene".to_string(),
            },
            description: None,
            entries: vec![PresetEntry {
                want: Some("frame_rate".to_string()),
                names: vec!["fps".to_string()],
            }],
            groups: Vec::new(),
            keywords: Vec::new(),
        }]),
    };
    let s = serde_json::to_string(&m).unwrap();
    let back: Manifest = serde_json::from_str(&s).unwrap();
    assert_eq!(m, back, "带 presets 的 manifest 必须往返相等");
    let obj = serde_json::from_str::<serde_json::Value>(&s).unwrap();
    assert_eq!(
        obj["presets"][0]["id"],
        "perf-scene",
        "presets 段必须序列化输出"
    );
}

/// 容忍性：presets 中缺省字段（条目无 want、组无 entries）反序列化取默认值。
#[test]
fn preset_sparse_fields_default() {
    let m: Manifest = serde_json::from_str(
        r#"{"id":"builtin-csv","display_name":"CSV Universal Parser","version":"0.1.0","entry":{"command":"c","args":[]},"match":{"extensions":[]},"min_protocol_version":1,"presets":[{"id":"sparse","name":{"zh":"稀疏","en":"Sparse"},"entries":[{"names":["fps"]}],"groups":[{"id":"platform","name":{"zh":"平台","en":"Platform"}}]}]}"#,
    )
    .unwrap();
    let presets = m.presets.expect("presets 必须存在");
    assert_eq!(presets.len(), 1);
    let def = &presets[0];
    assert!(def.description.is_none(), "缺 description 必为 None");
    assert_eq!(def.keywords.len(), 0, "缺 keywords 必为空数组");
    assert_eq!(def.entries.len(), 1);
    assert!(def.entries[0].want.is_none(), "条目缺 want 必为 None");
    assert_eq!(def.groups.len(), 1);
    assert!(
        def.groups[0].entries.is_empty(),
        "组缺 entries 必为空数组"
    );
}

/// 必填字段校验：LocalizedName 缺键（如只给 zh）应反序列化失败。
#[test]
fn localized_name_missing_key_fails() {
    assert!(
        serde_json::from_str::<LocalizedName>(r#"{"zh":"性能场景"}"#).is_err(),
        "缺 en 必须反序列化失败"
    );
    assert!(
        serde_json::from_str::<LocalizedName>(r#"{"en":"Performance Scene"}"#).is_err(),
        "缺 zh 必须反序列化失败"
    );
}

/// 必填字段校验：PresetDef 缺 id/name 应反序列化失败。
#[test]
fn preset_def_missing_required_fails() {
    assert!(
        serde_json::from_str::<PresetDef>(
            r#"{"name":{"zh":"性能场景","en":"Performance Scene"}}"#
        )
        .is_err(),
        "缺 id 必须反序列化失败"
    );
    assert!(
        serde_json::from_str::<PresetDef>(r#"{"id":"perf-scene"}"#).is_err(),
        "缺 name 必须反序列化失败"
    );
}
