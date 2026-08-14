//! 用户预设存储命令（Wave 2 C5）：`list_user_presets` / `save_user_preset` /
//! `delete_user_preset`，文件 `%APPDATA%\AnalysisBuddy\presets\<id>.abpreset.json`
//! （与插件预设同构，见 `ab_protocol::manifest::PresetDef`——id/name/description
//! 形状一致，entries 按 plugin_id 分键）。
//!
//! 存储模式照抄 `plugin_manager.rs` 状态文件：只读解析 + 损坏回落（list 跳过
//! 损坏/不可读文件并 stderr 告警）、tmp + rename 原子写、失败清理 tmp、
//! `serde_json::to_vec_pretty`（UTF-8 无 BOM）。
//! 错误码 `preset_conflict` 为命令层自有码（`module_error` 先例，§1.10 表外），
//! message 带 id。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ab_protocol::manifest::LocalizedName;

use crate::commands::IpcError;
use crate::ipc_errors::module_error;

/// 用户预设文件名后缀（`<id>.abpreset.json`）。
const PRESET_FILE_SUFFIX: &str = ".abpreset.json";
/// id 长度上限（正则 `{0,63}` 的首字符外余长；正则本身即 ≤64 全长）。
const ID_MAX_LEN: usize = 64;

/// 用户预设（文件 `%APPDATA%\AnalysisBuddy\presets\<id>.abpreset.json`；
/// 与插件预设同构——id/name/description 形状一致，entries 按 plugin_id 分键）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserPresetDto {
    /// `^[a-z0-9][a-z0-9-_]{0,63}$`；文件名 = `<id>.abpreset.json`。
    pub id: String,
    /// 强制双语名。
    pub name: ab_protocol::manifest::LocalizedName,
    /// 可选双语描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<ab_protocol::manifest::LocalizedName>,
    /// plugin_id → 该插件 metric 候选列表（用户保存天然精确，无模糊项）。
    #[serde(default)]
    pub entries: std::collections::HashMap<String, Vec<String>>,
}

/// 用户预设目录：照抄 `ab-host::discovery.rs` 的 APPDATA 公式 +
/// `.join("presets")`（`%APPDATA%\AnalysisBuddy\presets`）。
pub fn presets_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("AnalysisBuddy")
        .join("presets")
}

/// id 合法性（`^[a-z0-9][a-z0-9-_]{0,63}$` 手工实现，无 regex 依赖）：
/// 首字符 [a-z0-9]，余字符 [a-z0-9-_]，全长 ≤64。`/`、`\`、`.`（含 `..`
/// 任意形态、空串、大写）天然不命中。
fn valid_preset_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    let mut len = 1usize;
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return false;
        }
        len += 1;
        if len > ID_MAX_LEN {
            return false;
        }
    }
    true
}

/// id 由名称生成（slug 化）：小写、非 [a-z0-9-] 转 '-'、连续 '-' 折叠、
/// 去首尾 '-'（pending_dash 只在后随字母/数字时落笔）；空 → "preset"；
/// 截断 64。源取 `name.zh`（保存侧已校验 trim 后非空）。
fn slugify_id(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in name.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            // 首字符前的 '-' 不落笔（去首 '-'）；连续 '-' 经 pending 折叠。
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(lower);
        } else {
            pending_dash = true;
        }
    }
    // 末尾残留 pending '-' 不落笔（去尾 '-'）。
    if out.is_empty() {
        out.push_str("preset");
    }
    out.truncate(ID_MAX_LEN);
    out
}

/// `list_user_presets` 逻辑体（dir 注入供测试）：扫描 `<dir>/*.abpreset.json`，
/// 损坏/不可读/文件名 id 非法或与文件内 id 不符 → 跳过 + stderr 告警
/// （回落空集不崩溃）；目录缺失 → 空集。按 id 排序返回。
pub fn list_user_presets_logic(dir: &Path) -> Vec<UserPresetDto> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!(
                "WARN ab-app: 用户预设目录不可读（回退空集）：{e}：{}",
                dir.display()
            );
            return Vec::new();
        }
    };
    let mut presets: Vec<UserPresetDto> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(PRESET_FILE_SUFFIX).map(String::from))
        else {
            continue;
        };
        if !valid_preset_id(&id) {
            continue;
        }
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("WARN ab-app: 用户预设不可读（跳过）：{e}：{}", path.display());
                continue;
            }
        };
        match serde_json::from_slice::<UserPresetDto>(&raw) {
            Ok(preset) if preset.id == id => presets.push(preset),
            Ok(_) => eprintln!(
                "WARN ab-app: 用户预设文件内 id 与文件名不符（跳过）：{}",
                path.display()
            ),
            Err(e) => eprintln!("WARN ab-app: 用户预设损坏（跳过）：{e}：{}", path.display()),
        }
    }
    presets.sort_by(|a, b| a.id.cmp(&b.id));
    presets
}

/// `save_user_preset` 逻辑体（dir 注入供测试）：
/// ① `name.zh`/`name.en` trim 后须非空（否则 reject `invalid_arg`）；
/// ② id 由 `name.zh` slug 化（[`slugify_id`]）；
/// ③ 同 id 文件已存在 → reject `preset_conflict`（message 带 id）；
/// ④ 目录不存在则创建；tmp + rename 原子写（UTF-8 无 BOM），失败清理 tmp。
pub fn save_user_preset_logic(
    dir: &Path,
    name: LocalizedName,
    entries: HashMap<String, Vec<String>>,
) -> Result<UserPresetDto, IpcError> {
    let zh = name.zh.trim();
    let en = name.en.trim();
    if zh.is_empty() || en.is_empty() {
        return Err(IpcError::invalid_arg(
            "name.zh and name.en must be non-empty after trimming",
        ));
    }
    let id = slugify_id(zh);
    let path = dir.join(format!("{id}{PRESET_FILE_SUFFIX}"));
    let tmp = dir.join(format!("{id}{PRESET_FILE_SUFFIX}.tmp"));
    if path.exists() {
        return Err(module_error(
            "preset_conflict",
            format!("user preset `{id}` already exists"),
        ));
    }
    let preset = UserPresetDto {
        id,
        name,
        description: None,
        entries,
    };
    fs::create_dir_all(dir)
        .map_err(|e| module_error("state_io", format!("cannot create user preset dir: {e}")))?;
    let json = serde_json::to_vec_pretty(&preset)
        .map_err(|e| module_error("internal", format!("cannot serialize user preset: {e}")))?;
    let write = (|| -> io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(&json)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write {
        let _ = fs::remove_file(&tmp);
        return Err(module_error(
            "state_io",
            format!("cannot write user preset file: {e}"),
        ));
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(module_error(
            "state_io",
            format!("cannot move user preset into place: {e}"),
        ));
    }
    Ok(preset)
}

/// `delete_user_preset` 逻辑体（dir 注入供测试）：id 不匹配
/// `^[a-z0-9][a-z0-9-_]{0,63}$`（含 `/`、`\`、`..` 任意形态、空串、大写）→
/// reject `invalid_arg`；文件不存在 → 幂等 `Ok`；删除失败 → `state_io`。
pub fn delete_user_preset_logic(dir: &Path, id: &str) -> Result<(), IpcError> {
    if !valid_preset_id(id) {
        return Err(IpcError::invalid_arg(format!(
            "invalid user preset id `{id}` (must match ^[a-z0-9][a-z0-9-_]{{0,63}}$)"
        )));
    }
    let path = dir.join(format!("{id}{PRESET_FILE_SUFFIX}"));
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(module_error(
            "state_io",
            format!("cannot delete user preset `{id}`: {e}"),
        )),
    }
}

/// 列出 `%APPDATA%\AnalysisBuddy\presets\*.abpreset.json`；损坏/不可读文件
/// 跳过 + stderr 诊断（回落空集）；按 id 排序返回。
#[tauri::command(rename_all = "snake_case")]
pub async fn list_user_presets() -> Result<Vec<UserPresetDto>, IpcError> {
    Ok(list_user_presets_logic(&presets_dir()))
}

/// 保存用户预设：id 由 name 生成（slug 化）；重名（同 id 已有文件）→
/// reject `preset_conflict`；目录不存在则创建；tmp + rename 原子写。
#[tauri::command(rename_all = "snake_case")]
pub async fn save_user_preset(
    name: LocalizedName,
    entries: HashMap<String, Vec<String>>,
) -> Result<UserPresetDto, IpcError> {
    save_user_preset_logic(&presets_dir(), name, entries)
}

/// 删除 `<id>.abpreset.json`；文件不存在 → 幂等 `Ok`；id 非法（不匹配
/// `^[a-z0-9][a-z0-9-_]{0,63}$`，含 `/`、`\`、`..` 任意形态）→ reject
/// `invalid_arg`。
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_user_preset(id: String) -> Result<(), IpcError> {
    delete_user_preset_logic(&presets_dir(), &id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_rules() {
        assert_eq!(slugify_id("My Preset!"), "my-preset");
        assert_eq!(slugify_id("测试"), "preset", "全非 ASCII → 折叠后为空 → preset");
        assert_eq!(slugify_id("A  B"), "a-b", "连续空白折叠为单 '-'");
        assert_eq!(slugify_id("   "), "preset", "纯空白 → 空 → preset");
        assert_eq!(slugify_id("-lead-t"), "lead-t", "去首 '-'");
        assert_eq!(slugify_id("trail-"), "trail", "去尾 '-'");
        let long = format!("{}-{}", "a".repeat(40), "b".repeat(40));
        let slug = slugify_id(&long);
        assert_eq!(slug.len(), 64, "截断 64");
        assert!(valid_preset_id(&slug), "截断产物仍是合法 id");
    }

    #[test]
    fn preset_id_validation() {
        for good in ["a", "a-b_c", "0abc", "a".repeat(64).as_str()] {
            assert!(valid_preset_id(good), "{good:?} 应合法");
        }
        for bad in [
            "",
            "A",
            "-abc",
            "_abc",
            "a/b",
            "a\\b",
            "a.b",
            "..",
            "a ".repeat(22).trim_end(),
            "a".repeat(65).as_str(),
        ] {
            assert!(!valid_preset_id(bad), "{bad:?} 应非法");
        }
    }
}
