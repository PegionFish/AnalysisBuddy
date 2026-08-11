//! 行为类规则 BEH-01 ~ BEH-12 规则级测试（`--behavior`；每条规则一正一反 fixture）。
//!
//! fixture 插件为 `python` 解释器型入口（tests/fixtures/*/plugin.py），实现
//! protocol-v1.md 最小协议子集并在各自文件中制造目标违规。

mod common;

use common::{fixture, has_rule, rules_len, run_json, summary};

fn beh(name: &str) -> (i32, serde_json::Value) {
    run_json(&["--behavior", fixture(name).to_str().unwrap()])
}

/// 正例：good-plugin 全量行为回放必须退出码 0、零规则。
#[test]
fn beh_00_good_plugin_full_pass() {
    let (code, json) = beh("good-plugin");
    assert_eq!(code, 0, "合规插件 --behavior 必须退出码 0");
    assert_eq!(rules_len(&json), 0);
    assert_eq!(summary(&json)["phase2"], "pass");
}

// ---------------------------------------------------------------------------
// BEH-01 initialize 元数据 + id 一致（含 can_handle confidence 越界）
// ---------------------------------------------------------------------------

#[test]
fn beh_01_negative_initialize_id_mismatch() {
    let (code, json) = beh("bad-beh-01-init");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "BEH-01"));
}

#[test]
fn beh_01_negative_confidence_out_of_range() {
    let (code, json) = beh("bad-beh-01-confidence");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "BEH-01"),
        "can_handle confidence 越界按 BEH-01 同类处理（docs-validator.md §3.5）"
    );
}

// ---------------------------------------------------------------------------
// BEH-02 响应 id 匹配
// ---------------------------------------------------------------------------

#[test]
fn beh_02_negative_response_id_mismatch() {
    let (code, json) = beh("bad-beh-02-id");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "BEH-02"));
}

// ---------------------------------------------------------------------------
// BEH-03 必选方法 -32601 / 非标准错误码
// ---------------------------------------------------------------------------

#[test]
fn beh_03_negative_32601_on_mandatory_method() {
    let (code, json) = beh("bad-beh-03-32601");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "BEH-03"));
}

#[test]
fn beh_03_negative_nonstandard_error_code() {
    let (code, json) = beh("bad-beh-03-badcode");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "BEH-03"),
        "集合外自定义错误码必须触发 BEH-03"
    );
}

#[test]
fn beh_03_negative_parse_unsupported_in_v1() {
    let (code, json) = beh("bad-beh-03-no-parse");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "BEH-03"),
        "缺失必选 parse 的插件（SDK 缺省回 -32005 unsupported_in_v1）必须触发 BEH-03"
    );
}

// ---------------------------------------------------------------------------
// BEH-04 parse 心跳
// ---------------------------------------------------------------------------

#[test]
fn beh_04_negative_no_progress_heartbeat() {
    let (code, json) = beh("bad-beh-04-no-progress");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "BEH-04"),
        "parse 全程无 progress 心跳必须触发 BEH-04"
    );
}

// ---------------------------------------------------------------------------
// BEH-05 Record 三必填 + metric ∈ schema + NaN/Infinity
// ---------------------------------------------------------------------------

#[test]
fn beh_05_negative_undeclared_metric() {
    let (code, json) = beh("bad-beh-05-metric");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "BEH-05"));
}

#[test]
fn beh_05_negative_nan_literal() {
    let (code, json) = beh("bad-beh-05-nan");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "BEH-05"), "NaN 字面量必须触发 BEH-05");
}

// ---------------------------------------------------------------------------
// BEH-06 seq 缺号/重复 + records_total 核对
// ---------------------------------------------------------------------------

#[test]
fn beh_06_negative_seq_gap() {
    let (code, json) = beh("bad-beh-06-seq-gap");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "BEH-06"));
}

#[test]
fn beh_06_negative_records_total_mismatch() {
    let (code, json) = beh("bad-beh-06-total");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "BEH-06"),
        "records_total ≠ 各批之和必须触发 BEH-06"
    );
}

// ---------------------------------------------------------------------------
// BEH-07 key_values 看门狗 / 结构
// ---------------------------------------------------------------------------

#[test]
fn beh_07_negative_result_shape() {
    let (code, json) = beh("bad-beh-07-shape");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "BEH-07"),
        "KeyValuesResult 结构不符必须触发 BEH-07"
    );
}

// ---------------------------------------------------------------------------
// BEH-08 单行 > 8MB
// ---------------------------------------------------------------------------

#[test]
fn beh_08_negative_oversized_line() {
    let (code, json) = beh("bad-beh-08-line");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "BEH-08"));
}

// ---------------------------------------------------------------------------
// BEH-09 stdout 混入非 NDJSON
// ---------------------------------------------------------------------------

#[test]
fn beh_09_negative_stdout_banner() {
    let (code, json) = beh("bad-beh-09-banner");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "BEH-09"));
}

// ---------------------------------------------------------------------------
// BEH-10 shutdown 后未及时退出（warning）
// ---------------------------------------------------------------------------

#[test]
fn beh_10_negative_slow_exit_warns() {
    let (code, json) = beh("bad-beh-10-slow-exit");
    assert_eq!(code, 1, "BEH-10 为 warning -> 退出码 1");
    assert!(has_rule(&json, "BEH-10"));
}

// ---------------------------------------------------------------------------
// BEH-11 load_file 幂等重入（warning）
// ---------------------------------------------------------------------------

#[test]
fn beh_11_negative_reload_fails_warns() {
    let (code, json) = beh("bad-beh-11-reload");
    assert_eq!(code, 1, "BEH-11 为 warning -> 退出码 1");
    assert!(has_rule(&json, "BEH-11"));
}

// ---------------------------------------------------------------------------
// BEH-12 stdin EOF 未自退（warning）
// ---------------------------------------------------------------------------

#[test]
fn beh_12_negative_no_eof_exit_warns() {
    let (code, json) = beh("bad-beh-12-no-eof");
    assert_eq!(code, 1, "BEH-12 为 warning -> 退出码 1");
    assert!(has_rule(&json, "BEH-12"));
    assert!(
        has_rule(&json, "BEH-10"),
        "该 fixture 连带触发 BEH-10（shutdown 后未退出）"
    );
}

// ---------------------------------------------------------------------------
// can_handle 不认领 → 跳过 ⑤~⑥ 并提示换 --fixture（无 error）
// ---------------------------------------------------------------------------

#[test]
fn beh_abstention_skips_load_parse_with_note() {
    let (code, json) = beh("abstain-plugin");
    assert_eq!(code, 0, "弃权不产生 error -> 退出码 0");
    let notes = summary(&json)["notes"].as_array().unwrap();
    assert!(
        notes.iter().any(|n| n.as_str().unwrap().contains("未认领")),
        "弃权提示必须出现在 summary.notes"
    );
    assert!(!has_rule(&json, "BEH-05"), "跳过 parse 后不得评估 BEH-05");
}

// ---------------------------------------------------------------------------
// --timeout-scale 对全部看门狗生效（慢机 CI 无误报）
// ---------------------------------------------------------------------------

#[test]
fn timeout_scale_does_not_break_conforming_plugin() {
    let (code, json) = run_json(&[
        "--behavior",
        "--timeout-scale",
        "2.0",
        fixture("good-plugin").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "2 倍缩放下合规插件必须仍然通过");
    assert_eq!(rules_len(&json), 0);
}

/// --fixture 显式指定时使用该文件。
#[test]
fn explicit_fixture_override() {
    let (code, json) = run_json(&[
        "--behavior",
        "--fixture",
        "fixtures/small_with_header.csv",
        fixture("good-plugin").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "显式 --fixture（与内置同源的 CSV）应通过");
    assert_eq!(rules_len(&json), 0);
}
