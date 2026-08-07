//! 序列化契约测试（DoD：§3.5 四段示例往返 / skip-if-empty / NaN 报错 / 错误码比对）。
//!
//! 往返测试的 JSON 载荷逐字取自 `docs/spec/examples/frame-ok-*.json`
//! （protocol.md §3.5 四段完整示例的机器可校验副本），与原文档逐字节一致。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::errors;
use crate::manifest::Manifest;
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
        r#"{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","percent":0.8,"records_so_far":1000,"bytes_read":819200}"#,
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
