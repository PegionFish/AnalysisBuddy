//! 会话重开接线（pipeline.md §5.3）：按记录 `plugin_id` 直连会话重解析，
//! 跳过 can_handle 自动匹配。
//!
//! 完整导入编排（`ImportCoordinator` / `matcher`，pipeline.md §1/§6）由
//! P3-02 合并卡在 ab-app 侧实现；本模块提供 B 路范围内的 `SessionRegistry`
//! 与会话事件全集，供重开流程与合并接线复用。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use ab_protocol::types::{FileSummary, LoadFileParams, ParseParams};
use tokio::sync::mpsc;

use crate::session_file::{self, FileVerifyStatus, SessionFile, SessionFileEntry};
use crate::store::{ParseWarnings, Store};
use crate::{ParseEvent, PluginSession};

/// 插件匹配候选（pipeline.md §6 matcher.rs 类型；matcher 本体由 P3-02 实现）。
#[derive(Debug, Clone, PartialEq)]
pub struct MatchCandidate {
    pub plugin_id: String,
    /// 置信度 `[0, 1]`。
    pub confidence: f64,
    /// 来自 `CanHandleResult.reason`。
    pub reason: Option<String>,
}

/// 管线事件全集（pipeline.md §6；ab-app 经 Tauri 事件流转给前端）。
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    ImportStarted {
        path: String,
    },
    ImportFailed {
        path: String,
        reason: String,
    },
    MatchCandidates {
        path: String,
        candidates: Vec<MatchCandidate>,
        needs_user_choice: bool,
    },
    PluginSelected {
        path: String,
        plugin_id: String,
        by: &'static str,
    },
    FileLoaded {
        file_id: String,
        summary: Option<FileSummary>,
    },
    FileLoadFailed {
        file_id: String,
        message: String,
    },
    ParseProgress {
        file_id: String,
        percent: Option<f64>,
        records_so_far: u64,
    },
    ParseCompleted {
        file_id: String,
        records_total: u64,
        warnings: ParseWarnings,
    },
    ParseFailed {
        file_id: String,
        reason: String,
        detail: Option<String>,
    },
    ParseCancelled {
        file_id: String,
    },
    QueryReady {
        file_id: String,
    },
    FileUnloaded {
        file_id: String,
    },
}

/// 插件会话注册表：`plugin_id → Arc<dyn PluginSession>`（pipeline.md §4.2）。
#[derive(Default)]
pub struct SessionRegistry {
    inner: RwLock<HashMap<String, Arc<dyn PluginSession>>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        SessionRegistry::default()
    }

    pub fn register(&self, session: Arc<dyn PluginSession>) {
        self.inner
            .write()
            .unwrap()
            .insert(session.plugin_id().to_string(), session);
    }

    pub fn get(&self, plugin_id: &str) -> Option<Arc<dyn PluginSession>> {
        self.inner.read().unwrap().get(plugin_id).cloned()
    }
}

/// 单文件重开结果（pipeline.md §5.3：missing/modified 标记，通过者重解析）。
#[derive(Debug, Clone)]
pub struct ReopenOutcome {
    /// 文件路径（会话条目原文）。
    pub path: String,
    /// 哈希校验三态。
    pub verify: FileVerifyStatus,
    /// 校验通过并进入重解析的文件的新 file_id。
    pub file_id: Option<String>,
    /// 重解析失败原因（`None` = 成功或未进入解析）。
    pub error: Option<String>,
}

/// 会话重开：逐文件校验哈希，通过者按记录 `plugin_id` 直连会话
/// （不触发 `can_handle`）走 load_file → schema → parse → freeze 全流程
/// （pipeline.md §5.3 步骤 3）；`missing`/`modified` 不阻塞其余文件。
pub async fn reopen_files(
    store: Arc<Store>,
    registry: Arc<SessionRegistry>,
    session: &SessionFile,
    events: &mpsc::UnboundedSender<PipelineEvent>,
) -> Vec<ReopenOutcome> {
    let mut outcomes = Vec::with_capacity(session.files.len());
    for (index, entry) in session.files.iter().enumerate() {
        let verify = match session_file::sha256_of_file(&PathBuf::from(&entry.path)) {
            Ok(digest) if digest == entry.sha256 => FileVerifyStatus::Ok,
            Ok(_) => FileVerifyStatus::Modified,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileVerifyStatus::Missing,
            Err(_) => FileVerifyStatus::Modified,
        };
        if verify != FileVerifyStatus::Ok {
            outcomes.push(ReopenOutcome {
                path: entry.path.clone(),
                verify,
                file_id: None,
                error: None,
            });
            continue;
        }
        let Some(sess) = registry.get(&entry.plugin_id) else {
            outcomes.push(ReopenOutcome {
                path: entry.path.clone(),
                verify,
                file_id: None,
                error: Some(format!("plugin '{}' not registered", entry.plugin_id)),
            });
            continue;
        };
        // 重开分配全新 file_id（原会话 file_id 已失效）
        let file_id = format!("reopen-{index}");
        match reopen_one_file(&store, sess.as_ref(), &file_id, entry, events).await {
            Ok(()) => outcomes.push(ReopenOutcome {
                path: entry.path.clone(),
                verify,
                file_id: Some(file_id),
                error: None,
            }),
            Err(error) => outcomes.push(ReopenOutcome {
                path: entry.path.clone(),
                verify,
                file_id: Some(file_id),
                error: Some(error),
            }),
        }
    }
    outcomes
}

/// 单文件重解析接线（pipeline.md §1.1 时序的 store 侧子集）。
async fn reopen_one_file(
    store: &Arc<Store>,
    sess: &dyn PluginSession,
    file_id: &str,
    entry: &SessionFileEntry,
    events: &mpsc::UnboundedSender<PipelineEvent>,
) -> Result<(), String> {
    let summary = sess
        .load_file(LoadFileParams {
            file_id: file_id.to_string(),
            path: entry.path.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;
    let _ = events.send(PipelineEvent::FileLoaded {
        file_id: file_id.to_string(),
        summary: Some(summary.clone()),
    });
    let schema = sess.schema().await.map_err(|e| e.to_string())?;
    let whitelist: Vec<String> = schema.metrics.iter().map(|m| m.id.clone()).collect();
    store
        .register(file_id, Some(summary), &whitelist)
        .map_err(|e| e.to_string())?;

    let (tx, mut rx) = mpsc::channel::<ParseEvent>(256);
    let sink_store = store.clone();
    let sink_file_id = file_id.to_string();
    let sink_events = events.clone();
    let sink_task = tokio::spawn(async move {
        let mut append_error: Option<String> = None;
        while let Some(event) = rx.recv().await {
            match event {
                ParseEvent::Batch(batch) => {
                    if append_error.is_none() {
                        if let Err(e) = sink_store.append_batch(&sink_file_id, batch) {
                            append_error = Some(e.to_string());
                        }
                    }
                }
                ParseEvent::Progress(p) => {
                    let _ = sink_events.send(PipelineEvent::ParseProgress {
                        file_id: sink_file_id.clone(),
                        percent: p.percent,
                        records_so_far: p.records_so_far,
                    });
                }
            }
        }
        append_error
    });
    let parse_result = sess
        .parse_stream(
            ParseParams {
                file_id: file_id.to_string(),
                options: None,
            },
            tx,
        )
        .await;
    let append_error = sink_task
        .await
        .map_err(|_| "parse sink task panicked".to_string())?;

    let records_total = match parse_result {
        Ok(total) => total,
        Err(e) => {
            store.unload(file_id);
            let _ = events.send(PipelineEvent::ParseFailed {
                file_id: file_id.to_string(),
                reason: "plugin_error".to_string(),
                detail: Some(e.to_string()),
            });
            return Err(e.to_string());
        }
    };
    if let Some(err) = append_error {
        store.unload(file_id);
        let _ = events.send(PipelineEvent::ParseFailed {
            file_id: file_id.to_string(),
            reason: "protocol_error".to_string(),
            detail: Some(err.clone()),
        });
        return Err(err);
    }
    if let Err(e) = store.freeze(file_id, records_total) {
        store.unload(file_id);
        let _ = events.send(PipelineEvent::ParseFailed {
            file_id: file_id.to_string(),
            reason: "count_mismatch".to_string(),
            detail: Some(e.to_string()),
        });
        return Err(e.to_string());
    }
    // 告警取 store 累计值（append_batch 返回的是文件累计计数，逐批累加会双计；
    // dropped_tags 亦由 store 记录，pipeline.md §6）
    let warnings = store.warnings(file_id).unwrap_or_default();
    let _ = events.send(PipelineEvent::ParseCompleted {
        file_id: file_id.to_string(),
        records_total,
        warnings,
    });
    let _ = events.send(PipelineEvent::QueryReady {
        file_id: file_id.to_string(),
    });
    Ok(())
}
