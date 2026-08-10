//! 插件管理类 Tauri command（ipc-ui.md §1.1 / §2.2 / §4.6）：
//! `list_plugins`（8 命令之一）、辅助命令 `get_plugin_log`（stderr 环形缓冲
//! 尾部补发）与 `reload_plugin`（停机后重建实例，返回新 `PluginInfo`）。

use std::sync::Arc;

use ab_host::{DiscoveredPlugin, PluginRegistry};
use ab_pipeline::SessionError;

use crate::commands::{IpcError, PluginInfoDto};
use crate::events::{PluginLogBuffer, PluginMeta, LOG_TAIL_DEFAULT};
use crate::pipeline_bridge::ImportCoordinator;

/// `list_plugins`（ipc-ui.md §1.1）：返回全部已发现插件（未拉起 → `discovered`）。
#[tauri::command]
pub async fn list_plugins(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    meta: tauri::State<'_, PluginMeta>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
) -> Result<Vec<PluginInfoDto>, IpcError> {
    Ok(list_plugins_logic(
        discovery.inner(),
        &meta,
        coordinator.inner(),
    ))
}

/// `list_plugins` 逻辑体（handler 薄包装）。
pub fn list_plugins_logic(
    discovery: &PluginRegistry,
    meta: &PluginMeta,
    coordinator: &ImportCoordinator,
) -> Vec<PluginInfoDto> {
    discovery
        .list()
        .iter()
        .map(|plugin| to_plugin_info(plugin, meta, coordinator))
        .collect()
}

/// `get_plugin_log`（ipc-ui.md §2.2）：环形缓冲尾部补发，默认 200 条。
#[tauri::command]
pub async fn get_plugin_log(
    buffer: tauri::State<'_, PluginLogBuffer>,
    plugin_id: String,
    limit: Option<usize>,
) -> Result<Vec<crate::events::PluginLogPayload>, IpcError> {
    get_plugin_log_logic(buffer.inner(), &plugin_id, limit)
}

/// `get_plugin_log` 逻辑体（handler 薄包装）。
pub fn get_plugin_log_logic(
    buffer: &PluginLogBuffer,
    plugin_id: &str,
    limit: Option<usize>,
) -> Result<Vec<crate::events::PluginLogPayload>, IpcError> {
    if plugin_id.trim().is_empty() {
        return Err(IpcError::invalid_arg("plugin_id must not be empty"));
    }
    // §2.2 默认 200 条；上限 10_000 条（防越界滥用）。
    let limit = limit.unwrap_or(LOG_TAIL_DEFAULT).clamp(1, 10_000);
    Ok(buffer.tail(plugin_id, limit))
}

/// `reload_plugin`（ipc-ui.md §4.6）：shutdown 旧实例 → 重建（§5.2），
/// 返回新 `PluginInfo`；未知插件 reject `internal`。
#[tauri::command]
pub async fn reload_plugin(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    meta: tauri::State<'_, PluginMeta>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    plugin_id: String,
) -> Result<PluginInfoDto, IpcError> {
    reload_plugin_logic(discovery.inner(), &meta, coordinator.inner(), &plugin_id).await
}

/// `reload_plugin` 逻辑体（handler 薄包装）。
pub async fn reload_plugin_logic(
    discovery: &PluginRegistry,
    meta: &PluginMeta,
    coordinator: &ImportCoordinator,
    plugin_id: &str,
) -> Result<PluginInfoDto, IpcError> {
    if plugin_id.trim().is_empty() {
        return Err(IpcError::invalid_arg("plugin_id must not be empty"));
    }
    let Some(plugin) = discovery.get(plugin_id) else {
        return Err(IpcError {
            code: "internal".to_string(),
            message: format!("plugin not found: {plugin_id}"),
            data: None,
        });
    };
    coordinator
        .reload_session(plugin_id)
        .await
        .map_err(|e| map_reload_error(&e))?;
    Ok(to_plugin_info(&plugin, meta, coordinator))
}

/// 重建失败映射（§1.10：会话拉起失败按 `SessionError` 表；`SessionGone`
/// 与帧错终止均为崩溃语义 → `plugin_crashed`）。
fn map_reload_error(error: &SessionError) -> IpcError {
    crate::ipc_errors::map_session_error(error.clone(), true)
}

/// 组装 `PluginInfoDto`：状态取事件流事实（未发生事件 → `discovered`）；
/// 驻留文件取宿主文件索引（file_id → plugin_id 反查）；失败摘要取事件流。
fn to_plugin_info(
    plugin: &DiscoveredPlugin,
    meta: &PluginMeta,
    coordinator: &ImportCoordinator,
) -> PluginInfoDto {
    PluginInfoDto::from_parts(
        plugin.manifest.id.clone(),
        plugin.manifest.display_name.clone(),
        plugin.manifest.version.clone(),
        meta.state_of(&plugin.manifest.id)
            .unwrap_or_else(|| "discovered".to_string()),
        coordinator.file_index().files_of(&plugin.manifest.id),
        meta.last_error_of(&plugin.manifest.id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};

    fn sample_plugin(id: &str) -> DiscoveredPlugin {
        let manifest = Manifest {
            id: id.to_string(),
            display_name: format!("Mock {id}"),
            version: "0.1.0".to_string(),
            entry: PluginEntry {
                command: "mock".to_string(),
                args: vec![],
                working_dir: None,
            },
            r#match: MatchRules {
                extensions: vec!["csv".to_string()],
                header_fingerprints: None,
            },
            min_protocol_version: 1,
        };
        DiscoveredPlugin {
            manifest,
            plugin_dir: std::path::PathBuf::from("."),
            source: ab_host::PluginSource::Portable,
            resolved: ab_host::ResolvedEntry {
                program: std::path::PathBuf::from("mock"),
                args: vec![],
                working_dir: std::path::PathBuf::from("."),
            },
        }
    }

    #[test]
    fn plugin_info_dto_shape_matches_ipc_ui_section1() {
        let plugin = sample_plugin("mock");
        let meta = PluginMeta::new();
        let coordinator = ImportCoordinator::new(
            Arc::new(ab_pipeline::Store::new()),
            Arc::new(ab_pipeline::SessionRegistry::new()),
            tokio::sync::mpsc::unbounded_channel().0,
            Arc::new(ab_host::PluginRuntime::new(Arc::new(PluginRegistry::new()))),
            Arc::new(PluginRegistry::new()),
        );
        let dto = to_plugin_info(&plugin, &meta, &coordinator);
        assert_eq!(dto.id, "mock");
        assert_eq!(dto.state, "discovered", "未发生事件 → discovered");
        assert_eq!(dto.last_error, None, "last_error 序列化为 null 而非省略键");
        let value = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(value["last_error"], serde_json::Value::Null);
        assert_eq!(
            value["capabilities"],
            serde_json::json!({
                "annotate": false,
                "subscribe": false,
                "binary_sidecar": false,
            }),
            "§1.0 capabilities 形状"
        );
        assert_eq!(value["loaded_file_ids"], serde_json::json!([]));
    }

    #[test]
    fn reload_error_maps_via_section1_10() {
        let e = map_reload_error(&SessionError::Plugin {
            code: ab_protocol::errors::ERR_PLUGIN_BUSY,
            message: "busy".to_string(),
        });
        assert_eq!(e.code, "plugin_busy");
        let e = map_reload_error(&SessionError::SessionGone);
        assert_eq!(e.code, "plugin_crashed");
    }
}
