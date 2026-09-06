//! 模块管理 Tauri command（spec §4.1/§4.2/§4.4/§5.1/§5.2）：`install_plugin_zip`、
//! `uninstall_plugin`、`set_plugin_enabled`（T6 追加 check/update）。
//! 逻辑体在 `ab_engine::commands::plugin_manager`（M1）；本模块保留 Tauri
//! 薄包装 + 生产 `default_plugins_dir()` 路径公式（exe 同目录 `plugins`，
//! 原公式原样保留）+ 公共项再导出。
//!
//! 全部命令统一 `rename_all = "snake_case"`（任务 21：tauri-macros 默认
//! camelCase，与前端 snake_case 契约不符时参数静默失配）。

use std::path::PathBuf;
use std::sync::Arc;

use ab_engine::commands::{IpcError, PluginInfoDto};
use ab_engine::pipeline_bridge::ImportCoordinator;
use ab_host::PluginRegistry;

pub use ab_engine::commands::plugin_manager::{
    check_plugin_update_logic, extract_plugin_zip, install_plugin_zip_logic, load_disabled_ids,
    load_module_state, save_module_state, set_plugin_enabled_logic, uninstall_plugin_logic,
    update_plugin_logic, PluginManagerState, UpdateInfoDto, ZipError,
};

/// 生产 plugins 目录：与 `PluginRegistry::new()` 的 Portable 源同一公式
/// （宿主 exe 所在目录 / plugins；ZIP 布局下 InstallDir 同路径）。
/// 逻辑体显式接收该路径，测试注入临时目录。
pub fn default_plugins_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default()
        .join("plugins")
}

/// `install_plugin_zip`（spec §4.1/§4.2）：安装本地 ZIP；`overwrite=true`
/// 覆盖不同版本（同版本恒「已安装」冲突）。返回新模块 `PluginInfoDto`。
#[tauri::command(rename_all = "snake_case")]
pub async fn install_plugin_zip(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    path: String,
    overwrite: bool,
) -> Result<PluginInfoDto, IpcError> {
    install_plugin_zip_logic(
        coordinator.inner(),
        discovery.inner(),
        &default_plugins_dir(),
        &path,
        overwrite,
    )
    .await
}

/// `uninstall_plugin`（spec §4.4）：关闭该插件全部文件会话 → 终止插件进程
/// （shutdown_plugin_sessions，live 进程 CWD 句柄会阻塞删目录）→ 删目录 →
/// reload；内建拒绝（`module_protected`）、目录不存在 → `module_not_found`、
/// 清理失败 → `module_in_use`。
#[tauri::command(rename_all = "snake_case")]
pub async fn uninstall_plugin(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    plugin_id: String,
) -> Result<(), IpcError> {
    uninstall_plugin_logic(
        coordinator.inner(),
        discovery.inner(),
        &default_plugins_dir(),
        &plugin_id,
    )
    .await
}

/// `set_plugin_enabled`（spec §4.4/§3.2）：写状态文件 → `set_disabled`（触发
/// reload + PluginsReloaded）。内建模块仅可禁用不可卸载，故无保护限制。
/// 卸载不清理状态文件（重装后保持禁用意愿，语义幂等）。
#[tauri::command(rename_all = "snake_case")]
pub async fn set_plugin_enabled(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    plugin_id: String,
    enabled: bool,
) -> Result<(), IpcError> {
    set_plugin_enabled_logic(
        discovery.inner(),
        &default_plugins_dir(),
        &plugin_id,
        enabled,
    )
    .await
}

/// `check_plugin_update`（spec §4.3 / docs 09）：解析 `update_url` →
/// GitHub Releases 最新发行版 → 恰好一个 zip 资产 → semver 比较，返回
/// `UpdateInfoDto`（latest_version / asset_name / is_newer）。
#[tauri::command(rename_all = "snake_case")]
pub async fn check_plugin_update(
    state: tauri::State<'_, PluginManagerState>,
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    plugin_id: String,
) -> Result<UpdateInfoDto, IpcError> {
    check_plugin_update_logic(
        state.inner().fetcher.as_ref(),
        discovery.inner(),
        &default_plugins_dir(),
        &plugin_id,
    )
    .await
}

/// `update_plugin`（spec §4.3 / docs 09）：下载最新发行版 zip → 走安装管线
/// → 关键校验（ZIP 内 id == 被更新模块 id、版本 > 当前）→ 关闭运行中会话 →
/// 覆盖 → 重扫 → 自动重开该模块驻留文件（会话自动重启），返回新
/// `PluginInfoDto`。
#[tauri::command(rename_all = "snake_case")]
pub async fn update_plugin(
    state: tauri::State<'_, PluginManagerState>,
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    plugin_id: String,
) -> Result<PluginInfoDto, IpcError> {
    update_plugin_logic(
        state.inner().fetcher.as_ref(),
        coordinator.inner(),
        discovery.inner(),
        &default_plugins_dir(),
        &plugin_id,
    )
    .await
}
