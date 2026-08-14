//! 用户预设 CRUD 集成测试（Wave 2 C5）：注入临时目录直测 `*_logic` 逻辑体，
//! 覆盖落盘形状（UTF-8 无 BOM）、冲突/参数错误码、损坏回落、往返一致与
//! id 生成（slug 化）行为。

mod common;

use std::collections::HashMap;
use std::fs;

use ab_app::commands::presets::{
    delete_user_preset_logic, list_user_presets_logic, save_user_preset_logic, UserPresetDto,
};
use ab_protocol::manifest::LocalizedName;
use common::TempDir;

fn name(zh: &str, en: &str) -> LocalizedName {
    LocalizedName {
        zh: zh.to_string(),
        en: en.to_string(),
    }
}

fn entries_of(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(plugin_id, metrics)| {
            (
                (*plugin_id).to_string(),
                metrics.iter().map(|m| (*m).to_string()).collect(),
            )
        })
        .collect()
}

/// 空目录 list → 空数组。
#[test]
fn empty_dir_lists_empty() {
    let tmp = TempDir::new("presets-empty");
    assert!(
        list_user_presets_logic(tmp.path()).is_empty(),
        "空目录（目录缺失）必须回落空数组"
    );
}

/// save → 文件落盘（UTF-8 无 BOM：读回字节前 3 个不得是 EF BB BF）→
/// list 读到同形。
#[test]
fn save_persists_utf8_no_bom_and_lists_back() {
    let tmp = TempDir::new("presets-save");
    let entries = entries_of(&[("demo-tool", &["fps", "frame_ms"]), ("gpu-tool", &[])]);
    let saved = save_user_preset_logic(tmp.path(), name("My Preset!", "My Preset!"), entries.clone())
        .expect("save");
    assert_eq!(saved.id, "my-preset");
    let raw = fs::read(tmp.path().join("my-preset.abpreset.json")).expect("read file");
    assert!(
        raw.len() < 3 || raw[0] != 0xEF || raw[1] != 0xBB || raw[2] != 0xBF,
        "落盘文件不得以 UTF-8 BOM 开头"
    );
    let listed = list_user_presets_logic(tmp.path());
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], saved, "list 读回与 save 返回同形");
    assert_eq!(listed[0].entries, entries, "entries 逐键相等");
}

/// save 重名（同 id 已有文件）→ Err(preset_conflict)，message 带 id。
#[test]
fn save_conflict_rejects_preset_conflict() {
    let tmp = TempDir::new("presets-conflict");
    save_user_preset_logic(tmp.path(), name("My Preset!", "My Preset!"), HashMap::new())
        .expect("first save");
    let err = save_user_preset_logic(tmp.path(), name("My Preset!", "My Preset!"), HashMap::new())
        .expect_err("重名必须 reject");
    assert_eq!(err.code, "preset_conflict");
    assert!(
        err.message.contains("my-preset"),
        "preset_conflict message 必须带 id：{err:?}"
    );
    assert!(err.data.is_none(), "preset_conflict 不携带 data");
}

/// save 空双语名（zh/en trim 后任一为空）→ Err(invalid_arg)，且不落盘。
#[test]
fn save_blank_bilingual_name_rejects_invalid_arg() {
    let tmp = TempDir::new("presets-blank-name");
    for n in [
        name("  ", "ok-en"),
        name("ok-zh", "  "),
        name("", "ok-en"),
        name("ok-zh", ""),
    ] {
        let err = save_user_preset_logic(tmp.path(), n.clone(), HashMap::new())
            .expect_err("空名必须 reject");
        assert_eq!(err.code, "invalid_arg", "name={n:?}");
    }
    assert!(
        list_user_presets_logic(tmp.path()).is_empty(),
        "失败的 save 不得落盘任何文件"
    );
}

/// delete 存在 → Ok + 文件消失。
#[test]
fn delete_existing_removes_file() {
    let tmp = TempDir::new("presets-delete");
    save_user_preset_logic(tmp.path(), name("My Preset!", "My Preset!"), HashMap::new())
        .expect("save");
    let path = tmp.path().join("my-preset.abpreset.json");
    assert!(path.exists());
    delete_user_preset_logic(tmp.path(), "my-preset").expect("delete");
    assert!(!path.exists(), "删除后文件必须消失");
    assert!(list_user_presets_logic(tmp.path()).is_empty());
}

/// delete 不存在 → 幂等 Ok。
#[test]
fn delete_missing_is_idempotent_ok() {
    let tmp = TempDir::new("presets-delete-missing");
    assert!(
        delete_user_preset_logic(tmp.path(), "ghost").is_ok(),
        "文件不存在 → 幂等 Ok"
    );
}

/// delete 非法 id（含 `/`、`\`、`..` 任意形态、空串、大写）→ Err(invalid_arg)。
#[test]
fn delete_invalid_id_rejects_invalid_arg() {
    let tmp = TempDir::new("presets-delete-invalid");
    for bad in ["../evil", "a/b", "", "ABC", "a\\b", "..", "a.b"] {
        let err = delete_user_preset_logic(tmp.path(), bad).expect_err("非法 id 必须 reject");
        assert_eq!(err.code, "invalid_arg", "id={bad:?}");
    }
}

/// 损坏文件（非法 JSON 的 `<id>.abpreset.json`）与文件内 id 不符的文件 →
/// list 跳过 + 不崩溃；完好文件照常读出。
#[test]
fn corrupted_file_is_skipped_without_crash() {
    let tmp = TempDir::new("presets-corrupt");
    save_user_preset_logic(tmp.path(), name("Good Preset", "Good Preset"), HashMap::new())
        .expect("save");
    fs::write(tmp.path().join("broken.abpreset.json"), "not json{{{").expect("write corrupt");
    fs::write(
        tmp.path().join("wrong-id.abpreset.json"),
        serde_json::to_string(&UserPresetDto {
            id: "other".to_string(),
            name: name("Wrong", "Wrong"),
            description: None,
            entries: HashMap::new(),
        })
        .expect("json"),
    )
    .expect("write mismatch");
    let listed = list_user_presets_logic(tmp.path());
    assert_eq!(listed.len(), 1, "损坏/不符文件跳过，不崩溃");
    assert_eq!(listed[0].id, "good-preset");
}

/// 往返：save 的 entries HashMap 经磁盘再 list 回，逐键相等。
#[test]
fn entries_roundtrip_equal_per_key() {
    let tmp = TempDir::new("presets-roundtrip");
    let entries = entries_of(&[
        ("cpu-tool", &["core0", "core1"]),
        ("gpu-tool", &["gpu-clock"]),
        ("empty-tool", &[]),
    ]);
    save_user_preset_logic(tmp.path(), name("Round Trip", "Round Trip"), entries.clone())
        .expect("save");
    let listed = list_user_presets_logic(tmp.path());
    assert_eq!(listed.len(), 1);
    let back = &listed[0].entries;
    assert_eq!(back.len(), entries.len());
    for (plugin_id, metrics) in &entries {
        assert_eq!(back.get(plugin_id), Some(metrics), "plugin_id={plugin_id}");
    }
}

/// id 生成：name "My Preset!" → id "my-preset"；name "测试"（无 ASCII）→
/// 逐字符转 '-' 折叠后 trim 为空 → "preset"。
#[test]
fn id_generation_slugs_name() {
    let tmp = TempDir::new("presets-idgen");
    let a = save_user_preset_logic(tmp.path(), name("My Preset!", "My Preset!"), HashMap::new())
        .expect("save");
    assert_eq!(a.id, "my-preset");
    let b = save_user_preset_logic(tmp.path(), name("测试", "Chinese"), HashMap::new())
        .expect("save");
    assert_eq!(b.id, "preset", "全非 ASCII → 折叠后为空 → preset");
    assert!(tmp.path().join("my-preset.abpreset.json").exists());
    assert!(tmp.path().join("preset.abpreset.json").exists());
    let listed = list_user_presets_logic(tmp.path());
    let ids: Vec<&str> = listed.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, vec!["my-preset", "preset"], "按 id 排序返回");
}
