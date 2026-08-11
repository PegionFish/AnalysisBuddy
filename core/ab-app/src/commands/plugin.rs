//! 插件管理类 Tauri command（ipc-ui.md §1.1 / §2.2 / §4.6）：
//! `list_plugins`（8 命令之一）、辅助命令 `get_plugin_log`（stderr 环形缓冲
//! 尾部补发）与 `reload_plugin`（停机后重建实例，返回新 `PluginInfo`）。

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use ab_host::{DiscoveredPlugin, PluginRegistry};
use ab_pipeline::SessionError;

use crate::commands::{IpcError, PluginInfoDto};
use crate::events::{PluginLogBuffer, PluginMeta, LOG_TAIL_DEFAULT};
use crate::pipeline_bridge::ImportCoordinator;

/// `list_plugins`（ipc-ui.md §1.1）：返回全部已发现插件（未拉起 → `discovered`）。
///
/// 注意：state 类型必须与 lib.rs `app.manage(...)` 注入的类型逐字一致
/// （`Arc<PluginMeta>`/`Arc<PluginLogBuffer>`）——Tauri `State<T>` 按 TypeId
/// 查找，`State<PluginMeta>` 取不到 `manage(Arc<PluginMeta>)` 的值，会以
/// "state not managed" 拒绝（任务 15 缺陷 1 的第二层根因，acl_runtime_test 固化）。
///
/// 全部命令统一 `rename_all = "snake_case"`（任务 21：tauri-macros 默认
/// camelCase，与前端 snake_case 契约不符时参数静默失配）。
#[tauri::command(rename_all = "snake_case")]
pub async fn list_plugins(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    meta: tauri::State<'_, Arc<PluginMeta>>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
) -> Result<Vec<PluginInfoDto>, IpcError> {
    Ok(list_plugins_logic(
        discovery.inner(),
        meta.inner(),
        coordinator.inner(),
        &crate::commands::plugin_manager::default_plugins_dir(),
    ))
}

/// `list_plugins` 逻辑体（handler 薄包装）。
///
/// `plugins_dir` = portable 插件目录（`default_plugins_dir()`）：禁用合并展示
/// 需要按 id 读 `<plugins_dir>/<id>/plugin.json`（禁用模块不在发现列表）。
///
/// 合并展示（用户裁定）：发现过滤禁用（不可 spawn），但命令层把禁用模块
/// 一并列出（disabled=true），UI 才能提供「启用」入口。合并行数据源 =
/// 状态文件禁用集合 + 插件目录 manifest（manifest 不可读 → `invalid` 保留值）。
/// 返回值按 id 排序（发现列表本身即 BTreeMap 序，合并行并入后仍稳定）。
pub fn list_plugins_logic(
    discovery: &PluginRegistry,
    meta: &PluginMeta,
    coordinator: &ImportCoordinator,
    plugins_dir: &Path,
) -> Vec<PluginInfoDto> {
    let discovered = discovery.list();
    let discovered_ids: HashSet<&str> = discovered.iter().map(|p| p.manifest.id.as_str()).collect();
    let mut plugins: Vec<PluginInfoDto> = discovered
        .iter()
        .map(|plugin| to_plugin_info(discovery, plugin, meta, coordinator))
        .collect();
    for disabled_id in crate::commands::plugin_manager::load_disabled_ids(plugins_dir) {
        if discovered_ids.contains(disabled_id.as_str()) {
            continue;
        }
        plugins.push(disabled_plugin_info(disabled_id, plugins_dir, meta));
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    plugins
}

/// 禁用模块的合并展示行：读 `<plugins_dir>/<id>/plugin.json` 解析 manifest，
/// 元信息（display_name/version/update_url/author/repository/tools/changelog）
/// 透传；manifest 不可读 → `invalid` 保留值（display_name 回落 id、版本为空）。
fn disabled_plugin_info(id: String, plugins_dir: &Path, meta: &PluginMeta) -> PluginInfoDto {
    let state = meta
        .state_of(&id)
        .unwrap_or_else(|| "discovered".to_string());
    let manifest = fs::read_to_string(plugins_dir.join(&id).join("plugin.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<ab_protocol::manifest::Manifest>(&raw).ok());
    match manifest {
        Some(m) => PluginInfoDto::from_parts(
            id.clone(),
            m.display_name,
            m.version,
            state,
            Vec::new(),
            None,
            "portable",
            crate::BUILTIN_PLUGIN_IDS.contains(&id.as_str()),
            true,
            m.update_url,
            m.author,
            m.repository,
            m.tools,
            m.changelog,
        ),
        None => {
            let builtin = crate::BUILTIN_PLUGIN_IDS.contains(&id.as_str());
            PluginInfoDto::from_parts(
                id.clone(),
                id,
                String::new(),
                state,
                Vec::new(),
                None,
                "invalid",
                builtin,
                true,
                None,
                None,
                None,
                None,
                None,
            )
        }
    }
}

/// `get_plugin_log`（ipc-ui.md §2.2）：环形缓冲尾部补发，默认 200 条。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_plugin_log(
    buffer: tauri::State<'_, Arc<PluginLogBuffer>>,
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
#[tauri::command(rename_all = "snake_case")]
pub async fn reload_plugin(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    meta: tauri::State<'_, Arc<PluginMeta>>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    plugin_id: String,
) -> Result<PluginInfoDto, IpcError> {
    reload_plugin_logic(
        discovery.inner(),
        meta.inner(),
        coordinator.inner(),
        &plugin_id,
    )
    .await
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
    // 禁用模块拒绝重建（spec §4.4「不可 spawn」）：禁用 id 从发现列表
    // 消失后 get() 会失败，此处提前给出明确错误码。错误码复用 §5.1
    // module_not_found（目标插件不可用），不新增 module_disabled 码位。
    if discovery.is_disabled(plugin_id) {
        return Err(IpcError {
            code: "module_not_found".to_string(),
            message: format!("plugin `{plugin_id}` is disabled"),
            data: None,
        });
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
    Ok(to_plugin_info(discovery, &plugin, meta, coordinator))
}

/// 重建失败映射（§1.10：会话拉起失败按 `SessionError` 表；`SessionGone`
/// 与帧错终止均为崩溃语义 → `plugin_crashed`）。
fn map_reload_error(error: &SessionError) -> IpcError {
    crate::ipc_errors::map_session_error(error.clone(), true)
}

/// 组装 `PluginInfoDto`：状态取事件流事实（未发生事件 → `discovered`）；
/// 驻留文件取宿主文件索引（file_id → plugin_id 反查）；失败摘要取事件流；
/// 任务 5 起补充来源/内建/禁用/更新源（spec §6.3）。
fn to_plugin_info(
    discovery: &PluginRegistry,
    plugin: &DiscoveredPlugin,
    meta: &PluginMeta,
    coordinator: &ImportCoordinator,
) -> PluginInfoDto {
    let id = plugin.manifest.id.clone();
    PluginInfoDto::from_parts(
        id.clone(),
        plugin.manifest.display_name.clone(),
        plugin.manifest.version.clone(),
        meta.state_of(&id)
            .unwrap_or_else(|| "discovered".to_string()),
        coordinator.file_index().files_of(&id),
        meta.last_error_of(&id),
        crate::commands::plugin_source_name(plugin.source),
        crate::BUILTIN_PLUGIN_IDS.contains(&id.as_str()),
        discovery.is_disabled(&id),
        plugin.manifest.update_url.clone(),
        plugin.manifest.author.clone(),
        plugin.manifest.repository.clone(),
        plugin.manifest.tools.clone(),
        plugin.manifest.changelog.clone(),
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
            author: Some("PegionFish".to_string()),
            repository: Some("https://github.com/owner/repo".to_string()),
            tools: Some(vec!["AnalysisBuddy >= 0.1.0".to_string()]),
            changelog: Some(vec![ab_protocol::manifest::ChangelogEntry {
                version: "0.1.0".to_string(),
                date: "2026-08-01".to_string(),
                notes: vec!["初始".to_string()],
            }]),
            ..Default::default()
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
        let registry = Arc::new(PluginRegistry::new());
        let coordinator = ImportCoordinator::new(
            Arc::new(ab_pipeline::Store::new()),
            Arc::new(ab_pipeline::SessionRegistry::new()),
            tokio::sync::mpsc::unbounded_channel().0,
            Arc::new(ab_host::PluginRuntime::new(registry.clone())),
            registry.clone(),
        );
        let dto = to_plugin_info(&registry, &plugin, &meta, &coordinator);
        assert_eq!(dto.id, "mock");
        assert_eq!(dto.state, "discovered", "未发生事件 → discovered");
        assert_eq!(dto.last_error, None, "last_error 序列化为 null 而非省略键");
        assert!(!dto.builtin, "非内建模块");
        assert_eq!(dto.source, "portable");
        assert!(!dto.disabled);
        assert_eq!(dto.update_url, None);
        assert_eq!(
            dto.author.as_deref(),
            Some("PegionFish"),
            "manifest.author 透传 DTO"
        );
        assert_eq!(
            dto.repository.as_deref(),
            Some("https://github.com/owner/repo"),
            "manifest.repository 透传 DTO"
        );
        assert_eq!(
            dto.tools.as_deref(),
            Some(&["AnalysisBuddy >= 0.1.0".to_string()][..]),
            "manifest.tools 透传 DTO"
        );
        assert_eq!(
            dto.changelog.as_ref().map(|c| c.len()),
            Some(1),
            "manifest.changelog 透传 DTO"
        );
        let value = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(value["last_error"], serde_json::Value::Null);
        assert_eq!(value["source"], "portable");
        assert_eq!(value["builtin"], serde_json::Value::Bool(false));
        assert_eq!(value["disabled"], serde_json::Value::Bool(false));
        assert_eq!(
            value["author"],
            serde_json::Value::String("PegionFish".to_string())
        );
        assert_eq!(value["changelog"][0]["version"], "0.1.0");
        assert!(
            value.get("update_url").is_none(),
            "update_url 缺省时省略键（§1.0 skip-if-none 约定）"
        );
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
