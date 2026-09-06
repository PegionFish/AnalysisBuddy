//! 用户预设存储 Tauri command（Wave 2 C5）：`list_user_presets` /
//! `save_user_preset` / `delete_user_preset`。逻辑体在
//! `ab_engine::commands::presets`（M1，预设目录由调用方注入）；本模块保留
//! Tauri 薄包装 + 生产 `presets_dir()` 路径公式（原 APPDATA 公式原样保留）。

use std::collections::HashMap;
use std::path::PathBuf;

use ab_engine::commands::IpcError;
use ab_protocol::manifest::LocalizedName;

pub use ab_engine::commands::presets::{
    delete_user_preset_locked, delete_user_preset_logic, list_user_presets_logic,
    save_user_preset_locked, save_user_preset_logic, UserPresetDto,
};

/// 用户预设目录：照抄 `ab-host::discovery.rs` 的 APPDATA 公式 +
/// `.join("presets")`（`%APPDATA%\AnalysisBuddy\presets`）。
pub fn presets_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("AnalysisBuddy")
        .join("presets")
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
    save_user_preset_locked(&presets_dir(), name, entries).await
}

/// 删除 `<id>.abpreset.json`；文件不存在 → 幂等 `Ok`；id 非法（不匹配
/// `^[a-z0-9][a-z0-9-_]{0,63}$`，含 `/`、`\`、`..` 任意形态）→ reject
/// `invalid_arg`。
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_user_preset(id: String) -> Result<(), IpcError> {
    delete_user_preset_locked(&presets_dir(), &id).await
}
