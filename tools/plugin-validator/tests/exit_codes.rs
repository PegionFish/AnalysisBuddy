//! 五档退出码语义（docs-validator.md §1.3）与 `--json` 冻结字段（§1.4）。

mod common;

use common::{fixture, has_rule, rules_len, run_json, summary};

/// 0 EXIT_PASS：合规插件结构校验通过。
#[test]
fn exit_0_good_structure() {
    let (code, json) = run_json(&[fixture("good-plugin").to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(rules_len(&json), 0);
    assert_eq!(summary(&json)["phase1"], "pass");
    assert_eq!(summary(&json)["phase2"], "skipped");
}

/// 1 EXIT_WARN：仅 warning 无 error。
#[test]
fn exit_1_only_warning() {
    let (code, json) = run_json(&[fixture("bad-man-06-empty-match").to_str().unwrap()]);
    assert_eq!(code, 1, "仅 warning 必须退出码 1");
    assert!(has_rule(&json, "MAN-06"));
    assert_eq!(summary(&json)["errors"], 0);
    assert!(summary(&json)["warnings"].as_u64().unwrap() >= 1);
}

/// 2 EXIT_ERROR：目录无 plugin.json → MAN-08 诊断，而非用法错误 3。
#[test]
fn exit_2_no_plugin_json_is_man08_not_usage() {
    let (code, json) = run_json(&[fixture("bad-man-08-none").to_str().unwrap()]);
    assert_eq!(code, 2, "目录无 plugin.json 必须退出码 2（MAN-08），而非 3");
    assert!(has_rule(&json, "MAN-08"));
}

/// 2 EXIT_ERROR：结构不合规。
#[test]
fn exit_2_manifest_error() {
    let (code, json) = run_json(&[fixture("bad-man-01-missing-field").to_str().unwrap()]);
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-01"));
}

/// 3 EXIT_USAGE：缺参数 / 目录不存在 / 未知参数。
#[test]
fn exit_3_usage_errors() {
    // 用法错误输出到 stderr、无 JSON，仅断言退出码
    let out = std::process::Command::new(common::bin())
        .arg("--json")
        .output()
        .expect("plugin-check 应能启动");
    assert_eq!(out.status.code(), Some(3), "缺少 <plugin_dir> 必须退出码 3");

    let out = std::process::Command::new(common::bin())
        .args(["--json", "C:\\no\\such\\plugin\\dir"])
        .output()
        .expect("plugin-check 应能启动");
    assert_eq!(out.status.code(), Some(3), "目录不存在必须退出码 3");

    let out = std::process::Command::new(common::bin())
        .args([
            "--json",
            "--bogus-flag",
            fixture("good-plugin").to_str().unwrap(),
        ])
        .output()
        .expect("plugin-check 应能启动");
    assert_eq!(out.status.code(), Some(3), "未知参数必须退出码 3");
}

/// 4 EXIT_INTERNAL：Schema 文件缺失（--schema-dir 指向空目录）。
#[test]
fn exit_4_missing_schema_files() {
    let empty = std::env::temp_dir().join(format!(
        "pcheck-empty-schema-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&empty).unwrap();
    let out = std::process::Command::new(common::bin())
        .args([
            "--json",
            "--schema-dir",
            empty.to_str().unwrap(),
            fixture("good-plugin").to_str().unwrap(),
        ])
        .output()
        .expect("plugin-check 应能启动");
    assert_eq!(
        out.status.code(),
        Some(4),
        "Schema 文件缺失必须退出码 4（EXIT_INTERNAL）"
    );
    let _ = std::fs::remove_dir_all(&empty);
}

/// `--json` 顶层字段冻结：plugin_dir / rules / summary / exit_code。
#[test]
fn json_top_level_fields_frozen() {
    let (_, json) = run_json(&[fixture("good-plugin").to_str().unwrap()]);
    let obj = json.as_object().expect("顶层为对象");
    let keys: Vec<&String> = obj.keys().collect();
    assert_eq!(
        keys.len(),
        4,
        "顶层字段必须恰为 4 个（冻结形态），实际：{keys:?}"
    );
    for key in ["plugin_dir", "rules", "summary", "exit_code"] {
        assert!(obj.contains_key(key), "缺少冻结字段 `{key}`");
    }
    // rules[] 条目字段冻结
    let (_, bad) = run_json(&[fixture("bad-man-01-missing-field").to_str().unwrap()]);
    let rule = &bad["rules"][0];
    assert_eq!(
        rule.as_object().unwrap().keys().len(),
        4,
        "rules[] 字段必须恰为 id/level/message/location"
    );
    for key in ["id", "level", "message", "location"] {
        assert!(rule.as_object().unwrap().contains_key(key));
    }
    assert_eq!(rule["id"], "MAN-01");
    assert_eq!(rule["level"], "error");
}
