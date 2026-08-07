//! 命令层 DTO 与 `IpcError`（ipc-ui.md §1.0 错误形状）。
//!
//! `IpcError` 为全部 command 的 reject 值形状；`key_values_at` 除外——它走
//! 部分失败协议（§1.6），逐文件错误进入结果项，整体永不 reject。

pub mod import;
pub mod query;

use serde::Serialize;

/// 统一错误形状（ipc-ui.md §1.0 `IpcError`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl IpcError {
    pub fn invalid_arg(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_arg".to_string(),
            message: message.into(),
            data: None,
        }
    }
}

/// 手选覆盖入参（ipc-ui.md §1.2 `overrides`）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ImportOverride {
    pub plugin_id: String,
}

/// 匹配候选（§1.0 `PluginMatch`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginMatchDto {
    pub plugin_id: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 单文件导入结果（§1.0 `ImportResult`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportResultDto {
    pub file_id: String,
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_plugin: Option<PluginMatchDto>,
    pub candidate_plugins: Vec<PluginMatchDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_user_choice: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
}
