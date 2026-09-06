//! 插件管理类 Tauri command（ipc-ui.md §1.1 / §2.2 / §4.6）：
//! `list_plugins`（8 命令之一）、辅助命令 `get_plugin_log`（stderr 环形缓冲
//! 尾部补发）与 `reload_plugin`（停机后重建实例，返回新 `PluginInfo`）。
//! 逻辑体在 `ab_engine::commands::plugin`（M1）。

use std::sync::Arc;

use ab_engine::commands::{IpcError, PluginInfoDto};
use ab_engine::events::{PluginLogBuffer, PluginMeta};
use ab_engine::pipeline_bridge::ImportCoordinator;
use ab_host::PluginRegistry;

pub use ab_engine::commands::plugin::{
    get_plugin_log_logic, list_plugins_logic, reload_plugin_logic,
};

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

/// `get_plugin_log`（ipc-ui.md §2.2）：环形缓冲尾部补发，默认 200 条。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_plugin_log(
    buffer: tauri::State<'_, Arc<PluginLogBuffer>>,
    plugin_id: String,
    limit: Option<usize>,
) -> Result<Vec<ab_engine::events::PluginLogPayload>, IpcError> {
    get_plugin_log_logic(buffer.inner(), &plugin_id, limit)
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
