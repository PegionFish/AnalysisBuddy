//! 导入相关 Tauri command（ipc-ui.md §1.2/§1.3）：`import_files`（单路径失败
//! 不影响其余，整体仅在全部路径非法时 reject）、`unload_file`（幂等）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ab_pipeline::import::MatchCandidate;

use crate::commands::{ImportOverride, ImportResultDto, IpcError, PluginMatchDto, TimeRangeDto};
use crate::pipeline_bridge::{ImportCoordinator, ImportOutcome, ImportStatus};

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

/// `import_files` 逻辑体（handler 薄包装，便于 command 级集成测试）。
pub async fn import_files_logic(
    coordinator: &ImportCoordinator,
    paths: Vec<String>,
    overrides: Option<HashMap<String, ImportOverride>>,
) -> Result<Vec<ImportResultDto>, IpcError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if paths.iter().all(|p| p.trim().is_empty()) {
        return Err(IpcError::invalid_arg("all paths are empty"));
    }
    let overrides = overrides.unwrap_or_default();

    enum Pending {
        Task(tokio::task::JoinHandle<ImportOutcome>),
        Outcome(ImportOutcome),
    }

    let mut pending = Vec::with_capacity(paths.len());
    for path in paths {
        let trimmed = path.trim().to_string();
        if trimmed.is_empty() {
            pending.push(Pending::Outcome(ImportOutcome::failed(
                &trimmed,
                0,
                "invalid_arg",
                "path must not be empty".to_string(),
            )));
            continue;
        }
        let me = coordinator.clone();
        if let Some(override_entry) = overrides.get(&trimmed) {
            let plugin_id = override_entry.plugin_id.clone();
            pending.push(Pending::Task(tokio::spawn(async move {
                me.import_with_plugin(PathBuf::from(&trimmed), &plugin_id)
                    .await
            })));
        } else {
            pending.push(Pending::Task(tokio::spawn(async move {
                let mut outcomes = me.import_files(&[PathBuf::from(&trimmed)]).await;
                outcomes
                    .pop()
                    .expect("single-path import yields one outcome")
            })));
        }
    }

    let mut results = Vec::with_capacity(pending.len());
    for item in pending {
        let outcome = match item {
            Pending::Outcome(outcome) => outcome,
            Pending::Task(task) => match task.await {
                Ok(outcome) => outcome,
                Err(e) => {
                    ImportOutcome::failed("", 0, "internal", format!("import task panicked: {e}"))
                }
            },
        };
        results.push(to_dto(coordinator, outcome));
    }
    Ok(results)
}

/// `unload_file`（ipc-ui.md §1.3）：幂等；未知 file_id 视为成功。
#[tauri::command(rename_all = "snake_case")]
pub async fn unload_file(
    state: tauri::State<'_, Arc<ImportCoordinator>>,
    file_id: String,
) -> Result<(), IpcError> {
    unload_file_logic(state.inner(), file_id).await
}

/// `unload_file` 逻辑体（handler 薄包装）。
pub async fn unload_file_logic(
    coordinator: &ImportCoordinator,
    file_id: String,
) -> Result<(), IpcError> {
    if file_id.trim().is_empty() {
        return Err(IpcError::invalid_arg("file_id must not be empty"));
    }
    coordinator.unload_file(&file_id).await;
    Ok(())
}

fn to_dto(coordinator: &ImportCoordinator, outcome: ImportOutcome) -> ImportResultDto {
    let status = match outcome.status {
        ImportStatus::Matched => "matched",
        ImportStatus::Parsing => "parsing",
        ImportStatus::Ready => "ready",
        ImportStatus::Error => "error",
    };
    let file_id = outcome.file_id.unwrap_or_default();
    // 任务 19：ready 文件透传实际数据时间域（Frozen 文件取数据 min/max），
    // 供前端视口自动适配；仅 DTO 透传，不改命令签名/契约。
    let time_range = outcome
        .status
        .eq(&ImportStatus::Ready)
        .then(|| {
            coordinator
                .store()
                .time_range(&file_id)
                .map(|r| TimeRangeDto {
                    start_ms: r.start_ms,
                    end_ms: r.end_ms,
                })
        })
        .flatten();
    ImportResultDto {
        file_id,
        path: outcome.path,
        name: outcome.name,
        size_bytes: outcome.size_bytes,
        status,
        matched_plugin: outcome.matched_plugin.as_ref().map(to_plugin_match),
        candidate_plugins: outcome
            .candidate_plugins
            .iter()
            .map(to_plugin_match)
            .collect(),
        needs_user_choice: outcome.needs_user_choice.then_some(true),
        error: outcome.error.map(|e| IpcError {
            code: e.code.to_string(),
            message: e.message,
            data: None,
        }),
        time_range,
    }
}

/// 测试用最小 coordinator（无插件、空 store；time_range 恒 None）。
#[cfg(test)]
fn test_coordinator() -> ImportCoordinator {
    ImportCoordinator::new(
        Arc::new(ab_pipeline::Store::new()),
        Arc::new(ab_pipeline::SessionRegistry::new()),
        tokio::sync::mpsc::unbounded_channel().0,
        Arc::new(ab_host::PluginRuntime::new(Arc::new(
            ab_host::PluginRegistry::new(),
        ))),
        Arc::new(ab_host::PluginRegistry::new()),
    )
}

fn to_plugin_match(candidate: &MatchCandidate) -> PluginMatchDto {
    PluginMatchDto {
        plugin_id: candidate.plugin_id.clone(),
        confidence: candidate.confidence,
        reason: candidate.reason.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(path: &str, status: ImportStatus) -> ImportOutcome {
        ImportOutcome {
            path: path.to_string(),
            name: "x.csv".to_string(),
            size_bytes: 10,
            file_id: status.eq(&ImportStatus::Ready).then(|| "f1".to_string()),
            status,
            matched_plugin: None,
            candidate_plugins: Vec::new(),
            needs_user_choice: status.eq(&ImportStatus::Matched),
            error: status
                .eq(&ImportStatus::Error)
                .then(|| crate::pipeline_bridge::ImportError {
                    code: "file_not_found",
                    message: "no such file".to_string(),
                }),
        }
    }

    #[test]
    fn dto_status_and_error_shape_match_ipc_ui_section1() {
        let coordinator = test_coordinator();
        let dto = to_dto(&coordinator, outcome("C:\\logs\\a.csv", ImportStatus::Error));
        assert_eq!(dto.status, "error");
        assert_eq!(dto.file_id, "");
        let error = dto.error.expect("error present");
        assert_eq!(error.code, "file_not_found");

        let dto = to_dto(&coordinator, outcome("C:\\logs\\b.csv", ImportStatus::Matched));
        assert_eq!(dto.status, "matched");
        assert_eq!(dto.needs_user_choice, Some(true));
        assert!(dto.error.is_none());
        assert!(dto.matched_plugin.is_none());

        let dto = to_dto(&coordinator, outcome("C:\\logs\\c.csv", ImportStatus::Ready));
        assert_eq!(dto.status, "ready");
        assert_eq!(dto.file_id, "f1");
        // 序列化形状：可选字段省略键（§1.0 skip-if-empty 约定）。
        let value = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(value["status"], "ready");
        assert!(value.get("error").is_none());
        assert!(value.get("needs_user_choice").is_none());
        // 任务 19：空 store 无该文件 → time_range 省略键（skip-if-none）。
        assert!(dto.time_range.is_none());
        assert!(value.get("time_range").is_none());
    }
}
