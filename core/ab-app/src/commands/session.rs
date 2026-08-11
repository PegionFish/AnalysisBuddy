//! 会话类 Tauri command（ipc-ui.md §1.7/§1.8）：`save_session`（`path` 省略时
//! 调系统另存为对话框，取消 → reject `cancelled`）、`load_session`（校验
//! missing/hash_mismatch，通过者按记录 plugin_id 重走导入管线，pipeline.md §5.3）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ab_pipeline::save_session as write_session_file;
use ab_pipeline::{
    open_session, sha256_of_file, ChartViewState, FileVerifyStatus, SessionFile, SessionFileEntry,
    YAxisScale,
};

use crate::commands::{
    FileTimeRangeDto, IpcError, LoadResultDto, MissingFileEntryDto, SessionMetaDto,
};
use crate::pipeline_bridge::{ImportCoordinator, ImportStatus};

/// `save_session`（ipc-ui.md §1.7）：`path` 省略 → 系统另存为对话框
/// （取消 → reject `cancelled`）；落盘失败 reject `session_io`。
#[tauri::command(rename_all = "snake_case")]
pub async fn save_session(
    app: tauri::AppHandle,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    path: Option<String>,
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
    save_session_logic(coordinator.inner(), &path)
}

/// `save_session` 逻辑体（handler 薄包装；测试注入显式 path）。
pub fn save_session_logic(
    coordinator: &ImportCoordinator,
    path: &std::path::Path,
) -> Result<SessionMetaDto, IpcError> {
    let session = collect_session_file(coordinator);
    write_session_file(&session, path)
        .map_err(|e| io_error("session_io", format!("cannot write session file: {e}")))?;
    Ok(meta_of(&session, path))
}

/// `load_session`（ipc-ui.md §1.8）：文件损坏 → `session_io`；路径不存在 →
/// `file_not_found`；missing/modified 逐项标记，通过者重新进入导入管线。
#[tauri::command(rename_all = "snake_case")]
pub async fn load_session(
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    path: String,
) -> Result<LoadResultDto, IpcError> {
    if path.trim().is_empty() {
        return Err(IpcError::invalid_arg("path must not be empty"));
    }
    load_session_logic(coordinator.inner(), &PathBuf::from(&path)).await
}

/// `load_session` 逻辑体（handler 薄包装）。
pub async fn load_session_logic(
    coordinator: &ImportCoordinator,
    path: &std::path::Path,
) -> Result<LoadResultDto, IpcError> {
    let session = open_session(path).map_err(|e| match e {
        ab_pipeline::SessionFileError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            IpcError {
                code: "file_not_found".to_string(),
                message: format!("session file not found: {}", path.display()),
                data: None,
            }
        }
        other => io_error("session_io", format!("cannot open session file: {other}")),
    })?;

    // pipeline.md §5.3 步骤 1-2：逐文件三态校验。
    let verified = ab_pipeline::verify_files(&session);
    let mut missing = Vec::new();
    let mut to_reimport = Vec::new();
    for entry in &session.files {
        match verified.get(&entry.path) {
            Some(FileVerifyStatus::Ok) | None => to_reimport.push(entry),
            Some(FileVerifyStatus::Missing) => missing.push(MissingFileEntryDto {
                path: entry.path.clone(),
                reason: "not_found",
            }),
            Some(FileVerifyStatus::Modified) => missing.push(MissingFileEntryDto {
                path: entry.path.clone(),
                reason: "hash_mismatch",
            }),
        }
    }

    // 步骤 3：通过者按记录 plugin_id 重走导入管线（跳过自动匹配）。
    let mut loaded_file_ids = Vec::new();
    // 任务 19：重开成功文件透传实际数据时间域，供前端视口自动适配。
    let mut time_ranges = Vec::new();
    // 重解析失败（未达 Ready）逐项上报 UI（§1.8 扩展：此前无失败通道）。
    let mut reopen_failed = Vec::new();
    for entry in to_reimport {
        let outcome = coordinator
            .reopen_file(PathBuf::from(&entry.path), &entry.plugin_id)
            .await;
        match outcome.status {
            ImportStatus::Ready => {
                if let Some(file_id) = outcome.file_id {
                    if let Some(range) = coordinator.store().time_range(&file_id) {
                        time_ranges.push(FileTimeRangeDto {
                            file_id: file_id.clone(),
                            start_ms: range.start_ms,
                            end_ms: range.end_ms,
                        });
                    }
                    loaded_file_ids.push(file_id);
                }
            }
            other => {
                // 重解析失败（如插件崩溃/忙碌）：宿主日志记录 + 逐项上报
                // `reopen_failed`，UI 侧提示未达 Ready 的文件（§5.3 步骤 5）。
                eprintln!(
                    "load_session: reopen failed for {}: status {other:?} error {:?}",
                    entry.path, outcome.error
                );
                reopen_failed.push(MissingFileEntryDto {
                    path: entry.path.clone(),
                    reason: "reopen_failed",
                });
            }
        }
    }

    Ok(LoadResultDto {
        session: meta_of(&session, path),
        loaded_file_ids,
        missing,
        reopen_failed,
        time_ranges,
    })
}

/// 收集会话文件（pipeline.md §5.3 保存）：活跃（Frozen 可查）文件清单，
/// path + 实时计算 sha256 + plugin_id；文件不可读（已被删除等）跳过并告警。
fn collect_session_file(coordinator: &ImportCoordinator) -> SessionFile {
    let mut files = Vec::new();
    for file_id in coordinator.list_frozen() {
        let Some(path) = coordinator.path_of(&file_id) else {
            continue;
        };
        let Some(plugin_id) = coordinator.file_index().get(&file_id) else {
            continue;
        };
        let Ok(sha256) = sha256_of_file(&PathBuf::from(&path)) else {
            eprintln!("save_session: skip unreadable file `{path}`");
            continue;
        };
        files.push(SessionFileEntry {
            path,
            sha256,
            plugin_id,
        });
    }
    SessionFile {
        version: ab_pipeline::SESSION_FILE_VERSION,
        files,
        // UI 零改动约束下 `save_session` 仅收 path（ipc-ui.md §1.7 签名）；
        // 已选指标/图表视图/游标在 UI reducer 侧，宿主不可达 → 空/None
        // 落盘（schema v1 合法形状；视图状态恢复待 C 路扩展卡，见报告）。
        selected_metrics: HashMap::new(),
        chart_view_state: ChartViewState {
            time_range: None,
            legend_disabled: Vec::new(),
            y_axis_scale: YAxisScale::Shared,
        },
        cursor_ms: None,
    }
}

/// `SessionMeta` 组装（saved_at_ms = 落盘时刻，UTC 毫秒）。
fn meta_of(session: &SessionFile, path: &std::path::Path) -> SessionMetaDto {
    SessionMetaDto {
        path: path.display().to_string(),
        saved_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        file_count: session.files.len(),
        selected_metric_count: session.selected_metrics.values().map(Vec::len).sum(),
    }
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

fn io_error(code: &str, message: String) -> IpcError {
    IpcError {
        code: code.to_string(),
        message,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 会话 JSON 可被 ab-pipeline 读回（schema v1 往返），路径/哈希/插件 id 齐备。
    #[test]
    fn collect_session_file_roundtrips_via_session_file_module() {
        let tmp = std::env::temp_dir().join(format!("ab-app-session-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).expect("mkdir");
        let csv = tmp.join("a.csv");
        fs::write(&csv, "timestamp,fps\n1,60.0\n").expect("write csv");

        // 未导入任何文件 → 空会话文件（合法形状）。
        let coordinator = ImportCoordinator::new(
            Arc::new(ab_pipeline::Store::new()),
            Arc::new(ab_pipeline::SessionRegistry::new()),
            tokio::sync::mpsc::unbounded_channel().0,
            Arc::new(ab_host::PluginRuntime::new(Arc::new(
                ab_host::PluginRegistry::new(),
            ))),
            Arc::new(ab_host::PluginRegistry::new()),
        );
        let session = collect_session_file(&coordinator);
        assert!(session.files.is_empty());
        assert_eq!(session.version, ab_pipeline::SESSION_FILE_VERSION);

        let out = tmp.join("empty.absession");
        write_session_file(&session, &out).expect("save empty session");
        let back = open_session(&out).expect("open back");
        assert_eq!(back, session, "schema v1 往返一致");

        let _ = fs::remove_dir_all(&tmp);
    }
}
