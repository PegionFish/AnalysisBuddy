//! 结构类规则 MAN-01 ~ MAN-13 规则级测试（每条规则至少一正一反 fixture 目录）。

mod common;

use std::path::PathBuf;

use common::{fixture, has_rule, rules_len, run_json};

fn check(name: &str) -> (i32, serde_json::Value) {
    run_json(&[fixture(name).to_str().unwrap()])
}

/// 临时目录（测试用，不引入第三方依赖）。
/// 注意：插件目录名必须 == manifest id（MAN-02），故 `new(name)` 创建
/// `<temp>/<唯一>/<name>` 三层结构，`path()` 返回名为 `name` 的内层目录。
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let root = std::env::temp_dir().join(format!(
            "pcheck-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
    fn path(&self) -> &PathBuf {
        &self.0
    }
    fn write(&self, rel: &str, content: &str) {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Some(root) = self.0.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn good_manifest(dir: &str) -> String {
    format!(
        r#"{{
  "id": "{dir}",
  "display_name": "Good {dir}",
  "version": "0.1.0",
  "entry": {{ "command": "python", "args": ["plugin.py"] }},
  "match": {{ "extensions": ["csv"] }},
  "min_protocol_version": 1
}}"#
    )
}

// ---------------------------------------------------------------------------
// MAN-01 必填字段/类型（JSON Schema 判据）
// ---------------------------------------------------------------------------

#[test]
fn man_01_positive_good_manifest() {
    let (code, json) = check("good-plugin");
    assert_eq!(code, 0);
    assert!(!has_rule(&json, "MAN-01"));
}

#[test]
fn man_01_negative_missing_field() {
    let (code, json) = check("bad-man-01-missing-field");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-01"),
        "缺 entry 必须触发 MAN-01（Schema required）"
    );
    assert!(
        json["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "MAN-01" && r["location"].as_str().unwrap().contains("plugin.json")),
        "MAN-01 定位必须带 plugin.json 前缀"
    );
}

#[test]
fn man_01_negative_invalid_json() {
    let (code, json) = check("bad-man-01-invalid-json");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-01"));
}

// ---------------------------------------------------------------------------
// MAN-02 id 与目录名冲突 / 重复
// ---------------------------------------------------------------------------

#[test]
fn man_02_negative_id_dir_mismatch() {
    let (code, json) = check("bad-man-02-id-mismatch");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-02"));
}

#[test]
fn man_02_negative_duplicate_id_in_tree() {
    let (code, json) = check("bad-man-02-duplicate");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-02"), "目录树内重复 id 必须触发 MAN-02");
    // 嵌套 plugin.json 同时触发 MAN-08（两者独立判定）
    assert!(has_rule(&json, "MAN-08"));
}

// ---------------------------------------------------------------------------
// MAN-03 entry.command / working_dir 存在性
// ---------------------------------------------------------------------------

#[test]
fn man_03_positive_good_plugin() {
    let (code, json) = check("good-plugin");
    assert_eq!(code, 0);
    assert!(!has_rule(&json, "MAN-03"));
}

#[test]
fn man_03_negative_entry_missing() {
    let (code, json) = check("bad-man-03-entry-missing");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-03"));
}

#[test]
fn man_03_negative_working_dir_missing() {
    let (code, json) = check("bad-man-03-working-dir");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-03"));
}

// ---------------------------------------------------------------------------
// MAN-04 绝对路径 warning（运行时物化：绝对路径指向真实存在的文件）
// ---------------------------------------------------------------------------

#[test]
fn man_04_negative_absolute_command_warns() {
    let dir = TempDir::new("good-plugin"); // 目录名必须 == manifest id（MAN-02）
    let abs_command = fixture("good-plugin").join("plugin.py");
    let manifest = serde_json::json!({
        "id": "good-plugin",
        "display_name": "abs path",
        "version": "0.1.0",
        "entry": { "command": abs_command.display().to_string(), "args": [] },
        "match": { "extensions": ["csv"] },
        "min_protocol_version": 1,
    });
    dir.write("plugin.json", &manifest.to_string());
    let (code, json) = run_json(&[dir.path().to_str().unwrap()]);
    assert_eq!(code, 1, "绝对路径仅 warning -> 退出码 1");
    assert!(has_rule(&json, "MAN-04"));
}

// ---------------------------------------------------------------------------
// MAN-05 min_protocol_version 超限
// ---------------------------------------------------------------------------

#[test]
fn man_05_negative_min_protocol_too_high() {
    let (code, json) = check("bad-man-05-min-protocol");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-05"));
}

#[test]
fn man_05_positive_with_host_version_override() {
    let (code, json) = run_json(&[
        "--host-version",
        "2",
        fixture("bad-man-05-min-protocol").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "--host-version 2 时 min_protocol_version=2 应通过");
    assert!(!has_rule(&json, "MAN-05"));
}

// ---------------------------------------------------------------------------
// MAN-06 match 双空 warning
// ---------------------------------------------------------------------------

#[test]
fn man_06_negative_empty_match_warns() {
    let (code, json) = check("bad-man-06-empty-match");
    assert_eq!(code, 1);
    assert!(has_rule(&json, "MAN-06"));
}

// ---------------------------------------------------------------------------
// MAN-07 version 非 semver（warning）
// ---------------------------------------------------------------------------

#[test]
fn man_07_negative_non_semver_version() {
    let (code, json) = check("bad-man-07-version");
    assert_eq!(code, 2);
    // Schema 严格 pattern 先按 MAN-01 判 error（单源），MAN-07 宽松层同时给 warning
    assert!(has_rule(&json, "MAN-01"));
    assert!(
        has_rule(&json, "MAN-07"),
        "version `1.0` 必须触发 MAN-07 警告"
    );
}

#[test]
fn man_07_positive_good_version() {
    let (code, json) = check("good-plugin");
    assert_eq!(code, 0);
    assert!(!has_rule(&json, "MAN-07"));
}

// ---------------------------------------------------------------------------
// MAN-08 plugin.json 位置
// ---------------------------------------------------------------------------

#[test]
fn man_08_negative_nested_manifest() {
    let (code, json) = check("bad-man-08-nested");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-08"));
}

#[test]
fn man_08_negative_multiple_manifests() {
    let (code, json) = check("bad-man-08-multiple");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-08"),
        "根级 + 子目录双重清单必须触发 MAN-08"
    );
}

// ---------------------------------------------------------------------------
// MAN-09 无关文件容忍（反向验收：带 .git/ 与杂项文件零告警）
// ---------------------------------------------------------------------------

#[test]
fn man_09_ignores_unrelated_files() {
    // 已提交 fixture：good-plugin 内含 README.md / src/notes.txt
    let (code, json) = check("good-plugin");
    assert_eq!(code, 0);
    assert_eq!(rules_len(&json), 0);

    // 运行时物化：目录内含 .git/（protocol-v1.md §7.1 第 3 条明确允许）
    let dir = TempDir::new("good-plugin"); // 目录名必须 == manifest id（MAN-02）
    dir.write("plugin.json", &good_manifest("good-plugin"));
    dir.write(".git/config", "[core]\n");
    dir.write("target/release/builtin.exe", "junk");
    dir.write("src/main.rs", "fn main() {}");
    let (code, json) = run_json(&[dir.path().to_str().unwrap()]);
    assert_eq!(code, 0, "带 .git/ 的插件目录必须零告警");
    assert_eq!(rules_len(&json), 0);
}

// ---------------------------------------------------------------------------
// MAN-10 author/repository 格式（模块管理器设计 §3.1/§8）
// ---------------------------------------------------------------------------

#[test]
fn man_10_positive_good_meta() {
    let (code, json) = check("good-man-meta");
    assert_eq!(code, 0);
    for id in ["MAN-10", "MAN-11", "MAN-12", "MAN-13"] {
        assert!(!has_rule(&json, id), "合规元信息 fixture 不得触发 {id}");
    }
}

#[test]
fn man_10_negative_empty_author() {
    let (code, json) = check("bad-man-10-author-empty");
    assert_eq!(code, 2);
    assert!(has_rule(&json, "MAN-10"), "author 空字符串必须触发 MAN-10");
    assert!(
        json["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "MAN-10" && r["location"].as_str().unwrap().contains("#/author")),
        "MAN-10 定位必须对齐 plugin.json#/author"
    );
}

#[test]
fn man_10_negative_invalid_repository() {
    let (code, json) = check("bad-man-10-repository-http");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-10"),
        "repository 非 https URL 必须触发 MAN-10（TLS 强制）"
    );
}

// ---------------------------------------------------------------------------
// MAN-11 tools 约束语法（`{tool} {VersionReq}`）
// ---------------------------------------------------------------------------

#[test]
fn man_11_negative_bad_tools_syntax() {
    let (code, json) = check("bad-man-11-tools");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-11"),
        "tools 项缺 VersionReq 必须触发 MAN-11"
    );
}

// ---------------------------------------------------------------------------
// MAN-12 changelog 结构（version semver / date YYYY-MM-DD / notes 数组）
// ---------------------------------------------------------------------------

#[test]
fn man_12_negative_bad_changelog_date() {
    let (code, json) = check("bad-man-12-changelog-date");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-12"),
        "changelog 日期非 YYYY-MM-DD 必须触发 MAN-12"
    );
}

#[test]
fn man_12_negative_bad_changelog_version() {
    let (code, json) = check("bad-man-12-changelog-version");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-12"),
        "changelog 版本非 semver 必须触发 MAN-12"
    );
}

#[test]
fn man_12_negative_changelog_missing_notes() {
    let (code, json) = check("bad-man-12-changelog-notes");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-12"),
        "changelog 条目缺 notes 数组必须触发 MAN-12"
    );
}

// ---------------------------------------------------------------------------
// MAN-13 changelog 一致性（非空时版本降序 + 当前版本在列）
// ---------------------------------------------------------------------------

#[test]
fn man_13_negative_changelog_not_descending() {
    let (code, json) = check("bad-man-13-changelog-order");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-13"),
        "changelog 版本非严格降序必须触发 MAN-13"
    );
}

#[test]
fn man_13_negative_missing_current_version() {
    let (code, json) = check("bad-man-13-missing-version");
    assert_eq!(code, 2);
    assert!(
        has_rule(&json, "MAN-13"),
        "changelog 不含当前 version 必须触发 MAN-13"
    );
}
