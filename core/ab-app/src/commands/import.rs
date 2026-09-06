//! 导入相关 Tauri command（ipc-ui.md §1.2/§1.3）：`import_files`（单路径失败
//! 不影响其余，整体仅在全部路径非法时 reject）、`unload_file`（幂等）、
//! `cancel_parse`（P0-02）。逻辑体在 `ab_engine::commands::import`（M1）。

use std::collections::HashMap;
use std::sync::Arc;

use ab_engine::commands::{ImportOverride, ImportResultDto, IpcError};
use ab_engine::pipeline_bridge::ImportCoordinator;

pub use ab_engine::commands::import::{cancel_parse_logic, import_files_logic, unload_file_logic};

/// `import_files`（ipc-ui.md §1.2）：与入参同序返回；单路径失败置该路径
/// `status:"error"`，其余照常；全部路径为空串才整体 reject `invalid_arg`。
///
/// 全部命令统一 `rename_all = "snake_case"`（任务 21：tauri-macros 默认
/// camelCase，与前端 snake_case 契约不符时参数静默失配）。
#[tauri::command(rename_all = "snake_case")]
pub async fn import_files(
    state: tauri::State<'_, Arc<ImportCoordinator>>,
    paths: Vec<String>,
    overrides: Option<HashMap<String, ImportOverride>>,
) -> Result<Vec<ImportResultDto>, IpcError> {
    import_files_logic(state.inner(), paths, overrides).await
}

/// `unload_file`（ipc-ui.md §1.3）：幂等；未知 file_id 视为成功。
#[tauri::command(rename_all = "snake_case")]
pub async fn unload_file(
    state: tauri::State<'_, Arc<ImportCoordinator>>,
    file_id: String,
) -> Result<(), IpcError> {
    unload_file_logic(state.inner(), file_id).await
}

/// `cancel_parse`（P0-02，C2.1）：取消进行中的 parse。幂等：空 file_id →
/// `invalid_arg`；未知 file_id（无活跃 import job）或已终态 → `Ok(())`。
/// 实际取消语义（置 cancelled → 插件 cancel_parse → 等待 parse task 结束 →
/// 唯一一方丢弃半成品 → 发 ParseCancelled）在 coordinator 内实现（C2.2）。
#[tauri::command(rename_all = "snake_case")]
pub async fn cancel_parse(
    state: tauri::State<'_, Arc<ImportCoordinator>>,
    file_id: String,
) -> Result<(), IpcError> {
    cancel_parse_logic(state.inner(), file_id).await
}
