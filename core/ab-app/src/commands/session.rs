//! 会话类 Tauri command（ipc-ui.md §1.7/§1.8）：`save_session`（`path` 省略时
//! 调系统另存为对话框，取消 → reject `cancelled`）、`load_session`（校验
//! missing/hash_mismatch，通过者按记录 plugin_id 重走导入管线，pipeline.md
//! §5.3）。逻辑体在 `ab_engine::commands::session`（M1，显式 path 注入）。

use std::path::PathBuf;
use std::sync::Arc;

use ab_engine::commands::{IpcError, SessionMetaDto, SessionSnapshotDto};
use ab_engine::pipeline_bridge::ImportCoordinator;

pub use ab_engine::commands::session::{load_session_logic, save_session_logic};

/// `save_session`（ipc-ui.md §1.7）：`path` 省略 → 系统另存为对话框
/// （取消 → reject `cancelled`）；落盘失败 reject `session_io`。
/// `snapshot` 为前端提交的会话快照（契约 C1；省略/空 → 回落空字段）。
#[tauri::command(rename_all = "snake_case")]
pub async fn save_session(
    app: tauri::AppHandle,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    path: Option<String>,
    snapshot: Option<SessionSnapshotDto>,
) -> Result<SessionMetaDto, IpcError> {
    let path = match path {
        Some(path) if !path.trim().is_empty() => PathBuf::from(path),
        _ => match pick_save_path(&app).await {
            Some(path) => path,
            None => {
                return Err(IpcError {
                    code: "cancelled".to_string(),
                    message: "save dialog cancelled".to_string(),
                    data: None,
                })
            }
        },
    };
    save_session_logic(coordinator.inner(), &path, snapshot)
}

/// `load_session`（ipc-ui.md §1.8）：文件损坏 → `session_io`；路径不存在 →
/// `file_not_found`；missing/modified 逐项标记，通过者重新进入导入管线。
#[tauri::command(rename_all = "snake_case")]
pub async fn load_session(
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    path: String,
) -> Result<ab_engine::commands::LoadResultDto, IpcError> {
    if path.trim().is_empty() {
        return Err(IpcError::invalid_arg("path must not be empty"));
    }
    load_session_logic(coordinator.inner(), &PathBuf::from(&path)).await
}

/// 系统另存为对话框（ipc-ui.md §1.7：取消 → `None`）。
/// 任务 17 兜底：oneshot await 增加超时——若原生回调因环境异常永不触发，
/// 不能把 invoke 永久挂起（前端侧已改为前端对话框发起，此处为残留防线）。
async fn pick_save_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<PathBuf>>();
    app.dialog()
        .file()
        .add_filter("AnalysisBuddy Session", &["absession"])
        .set_file_name("session.absession")
        .set_title("Save AnalysisBuddy Session")
        .save_file(move |path| {
            let _ = tx.send(path.and_then(|p| p.as_path().map(|p| p.to_path_buf())));
        });
    match tokio::time::timeout(std::time::Duration::from_secs(600), rx).await {
        Ok(received) => received.ok().flatten(),
        Err(_) => {
            eprintln!("save_session: save dialog timed out after 600s, treating as cancelled");
            None
        }
    }
}
