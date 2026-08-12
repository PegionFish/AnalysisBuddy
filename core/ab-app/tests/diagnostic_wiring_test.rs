//! C8.2 诊断接线集成测试：`ScriptSession`（可注入失败/可阻塞的
//! `PluginSession` mock，风格同 tests/cancel_import_test.rs 的 GateSession）
//! 驱动 `ImportCoordinator`，验证导入完成/失败/取消三条路径各写入
//! 一条正确 kind 与字段的 `DiagnosticEntry`（经
//! `ImportCoordinator::diagnostics()` 可读 API 断言）。
//!
//! 覆盖（C8.3 验收）：
//! - 完成 → `ImportDone`：records_total/received_batches/dropped_batches
//!   终态快照、file_path、plugin_id、host_version、duration_ms > 0；
//! - 失败（load 全失败）→ `ImportFailed`：error_code/message 齐备；
//! - 取消 → `ImportCancelled`：取消前的局部接收进度 + 无 error_code。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ab_host::{PluginRegistry, PluginRuntime};
use ab_pipeline::{ParseEvent, PluginSession, SessionError, SessionRegistry, Store};
use ab_protocol::types::{
    CanHandleParams, CanHandleResult, CancelParseParams, FileSummary, KeyValuesParams,
    KeyValuesResult, LoadFileParams, MetricDef, ParseParams, Record, RecordBatch, SchemaResult,
    UnloadFileParams,
};
use tokio::sync::Notify;

use ab_app::events::{DiagnosticEntry, DiagnosticKind};
use ab_app::pipeline_bridge::{ImportCoordinator, ImportStatus, PipelineConfig};

const PLUGIN_ID: &str = "diag-mock";
const FILE_ID: &str = "d1a2b3c4-5e6f-4a7b-8c9d-0e1f2a3b4c5d";

/// 临时 CSV（导入编排读元信息，路径必须存在；用后自删）。
struct TempCsv(PathBuf);

impl TempCsv {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ab-diag-mock-{}-{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "a,b\n1,2\n").expect("write temp csv");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempCsv {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 可注入失败/可阻塞的 `PluginSession` mock：load 可脚本化失败（全失败 →
/// 导入走 ImportFailed）；parse 可延迟（duration_ms 断言）或阻塞在闸门上
/// （取消窗口内 parse task 停在 release 前）。
struct ScriptSession {
    plugin_id: &'static str,
    schema_metrics: Vec<MetricDef>,
    fail_load: bool,
    parse_delay: Duration,
    parse_gate: Option<Arc<Notify>>,
    parse_blocked: AtomicBool,
    records: Vec<Record>,
}

impl ScriptSession {
    /// 立即可完成（records 条记录；可选 parse 延迟）。
    fn new(records: u64, parse_delay: Duration) -> Arc<Self> {
        Self::with_gates(records, parse_delay, None)
    }

    /// parse 阶段阻塞（取消窗口内 parse task 停在 release 前）。
    fn with_parse_gate(records: u64) -> Arc<Self> {
        Self::with_gates(records, Duration::ZERO, Some(Arc::new(Notify::new())))
    }

    fn with_gates(
        records: u64,
        parse_delay: Duration,
        parse_gate: Option<Arc<Notify>>,
    ) -> Arc<Self> {
        Arc::new(ScriptSession {
            plugin_id: PLUGIN_ID,
            schema_metrics: vec![MetricDef {
                id: "fps".to_string(),
                name: "FPS".to_string(),
                unit: None,
                description: None,
                aggregation: ab_protocol::types::Aggregation::Last,
            }],
            fail_load: false,
            parse_delay,
            parse_gate,
            parse_blocked: AtomicBool::new(false),
            records: (0..records)
                .map(|i| Record {
                    timestamp: i as i64,
                    metric: "fps".to_string(),
                    value: 1.0,
                    level: None,
                    tags: None,
                    raw_line: None,
                })
                .collect(),
        })
    }

    fn fail_load(mut self: Arc<Self>) -> Arc<Self> {
        Arc::get_mut(&mut self).expect("sole owner").fail_load = true;
        self
    }

    async fn wait_parse_blocked(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.parse_blocked.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "等待 parse 进入阻塞点超时（5s）");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn release_parse(&self) {
        if let Some(gate) = &self.parse_gate {
            gate.notify_one();
        }
    }
}

#[async_trait::async_trait]
impl PluginSession for ScriptSession {
    fn plugin_id(&self) -> &str {
        self.plugin_id
    }

    async fn schema(&self) -> Result<SchemaResult, SessionError> {
        Ok(SchemaResult {
            metrics: self.schema_metrics.clone(),
        })
    }

    async fn can_handle(&self, _p: CanHandleParams) -> Result<CanHandleResult, SessionError> {
        Ok(CanHandleResult {
            can_handle: true,
            confidence: 1.0,
            reason: None,
        })
    }

    async fn load_file(&self, _p: LoadFileParams) -> Result<FileSummary, SessionError> {
        if self.fail_load {
            return Err(SessionError::Plugin {
                code: ab_protocol::errors::ERR_FILE_LOAD_FAILED,
                message: "scripted load failure".to_string(),
            });
        }
        Ok(FileSummary {
            record_count_hint: None,
            time_range: None,
            note: None,
        })
    }

    async fn parse_stream(
        &self,
        p: ParseParams,
        sink: tokio::sync::mpsc::Sender<ParseEvent>,
    ) -> Result<u64, SessionError> {
        if !self.records.is_empty() {
            let batch = RecordBatch {
                file_id: p.file_id.clone(),
                seq: 0,
                records: self.records.clone(),
                done: true,
            };
            if sink.send(ParseEvent::Batch(batch)).await.is_err() {
                return Err(SessionError::SessionGone);
            }
        }
        if !self.parse_delay.is_zero() {
            tokio::time::sleep(self.parse_delay).await;
        }
        if let Some(gate) = &self.parse_gate {
            self.parse_blocked.store(true, Ordering::SeqCst);
            gate.notified().await;
            self.parse_blocked.store(false, Ordering::SeqCst);
        }
        Ok(self.records.len() as u64)
    }

    async fn cancel_parse(&self, _p: CancelParseParams) -> Result<(), SessionError> {
        Ok(())
    }

    async fn key_values(&self, _p: KeyValuesParams) -> Result<KeyValuesResult, SessionError> {
        Ok(KeyValuesResult { entries: vec![] })
    }

    async fn unload_file(&self, _p: UnloadFileParams) -> Result<(), SessionError> {
        Ok(())
    }
}

/// 预注册 mock 会话 + 固定 file_id + 短重试退避的协调器。
fn coordinator_with(session: Arc<ScriptSession>) -> ImportCoordinator {
    let registry = Arc::new(SessionRegistry::new());
    registry.register(session);
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let discovery = Arc::new(PluginRegistry::new());
    ImportCoordinator::with_config(
        Arc::new(Store::new()),
        registry,
        events_tx,
        Arc::new(PluginRuntime::new(discovery.clone())),
        discovery,
        PipelineConfig {
            file_id_fn: Some(Arc::new(|_| FILE_ID.to_string())),
            // load 全失败路径不空耗默认 1s/3s 退避。
            load_retry_backoffs: vec![Duration::from_millis(10), Duration::from_millis(10)],
            ..PipelineConfig::default()
        },
    )
}

/// C8.2：完成路径 → `ImportDone`（records_total=2、received_batches=1、
/// dropped_batches=0、file_path、plugin_id、host_version、duration_ms>0、
/// 缓冲盖章的 session_id/ts_ms）。
#[tokio::test]
async fn import_success_records_import_done_diagnostic() {
    let session = ScriptSession::new(2, Duration::from_millis(10));
    let coordinator = coordinator_with(session);
    let csv = TempCsv::new();

    let outcome = coordinator
        .import_with_plugin(csv.path().to_path_buf(), PLUGIN_ID)
        .await;
    assert_eq!(outcome.status, ImportStatus::Ready, "正常完成 → Ready");

    let entries = coordinator.diagnostics().recent(10);
    assert_eq!(entries.len(), 1, "恰一条诊断");
    let entry = &entries[0];
    assert_eq!(entry.kind, DiagnosticKind::ImportDone);
    assert_eq!(
        entry.file_path.as_deref(),
        Some(csv.path().display().to_string().as_str())
    );
    assert_eq!(entry.plugin_id.as_deref(), Some(PLUGIN_ID));
    assert_eq!(entry.records_total, 2);
    assert_eq!(entry.received_batches, 1);
    assert_eq!(entry.dropped_batches, 0);
    assert_eq!(entry.error_code, None);
    assert!(
        entry.duration_ms >= 1,
        "parse 延迟 10ms → duration_ms 实测 {}",
        entry.duration_ms
    );
    assert_eq!(
        entry.host_version,
        env!("CARGO_PKG_VERSION"),
        "host_version = 编译期 crate 版本"
    );
    assert_eq!(entry.session_id, coordinator.diagnostics().session_id());
    assert!(entry.ts_ms > 0, "缓冲统一盖章时刻");
    // 未纳入发现的 mock 会话：plugin_version/source 不可获取 → None。
    assert_eq!(entry.plugin_version, None);
    assert_eq!(entry.plugin_source, None);
}

/// C8.2：失败路径（load 全失败 → 重试 3 次耗尽）→ `ImportFailed`，
/// error_code/message 齐备、计数为 0（未进入 parse）。
#[tokio::test]
async fn import_failure_records_import_failed_diagnostic() {
    let coordinator = coordinator_with(ScriptSession::new(2, Duration::ZERO).fail_load());
    let csv = TempCsv::new();

    let outcome = coordinator
        .import_with_plugin(csv.path().to_path_buf(), PLUGIN_ID)
        .await;
    assert_eq!(outcome.status, ImportStatus::Error, "load 全失败 → Error");
    assert_eq!(
        outcome.error.as_ref().expect("error").code,
        "file_load_failed"
    );

    let entries = coordinator.diagnostics().recent(10);
    assert_eq!(entries.len(), 1, "恰一条诊断");
    let entry = &entries[0];
    assert_eq!(entry.kind, DiagnosticKind::ImportFailed);
    assert_eq!(entry.error_code.as_deref(), Some("file_load_failed"));
    let message = entry.message.as_deref().expect("失败 message 存在");
    assert!(
        message.contains("scripted load failure"),
        "message 透传插件错误，实测 {message}"
    );
    assert_eq!(entry.plugin_id.as_deref(), Some(PLUGIN_ID));
    assert_eq!(
        entry.file_path.as_deref(),
        Some(csv.path().display().to_string().as_str())
    );
    assert_eq!(entry.records_total, 0);
    assert_eq!(entry.received_batches, 0);
    assert_eq!(entry.dropped_batches, 0);
}

/// C8.2：取消路径（parse 闸门阻塞中取消）→ `ImportCancelled`：无
/// error_code、计数为取消前的局部接收进度（1 批 / 2 条已入 sink）、
/// records_total 为 0（parse 未完成）。
#[tokio::test]
async fn cancel_records_import_cancelled_diagnostic() {
    let session = ScriptSession::with_parse_gate(2);
    let coordinator = coordinator_with(session.clone());
    let csv = TempCsv::new();

    let import_task = tokio::spawn({
        let coordinator = coordinator.clone();
        let path = csv.path().to_path_buf();
        async move { coordinator.import_with_plugin(path, PLUGIN_ID).await }
    });
    session.wait_parse_blocked().await;

    let cancel_task = tokio::spawn({
        let coordinator = coordinator.clone();
        async move { coordinator.cancel_parse(FILE_ID).await }
    });
    session.release_parse();
    let outcome = import_task.await.expect("import task");
    cancel_task.await.expect("cancel task");
    assert_eq!(outcome.status, ImportStatus::Error);
    assert_eq!(outcome.error.as_ref().expect("error").code, "cancelled");

    let entries = coordinator.diagnostics().recent(10);
    assert_eq!(entries.len(), 1, "恰一条诊断");
    let entry = &entries[0];
    assert_eq!(entry.kind, DiagnosticKind::ImportCancelled);
    assert_eq!(entry.error_code, None, "取消不是失败");
    assert_eq!(entry.records_total, 0, "parse 未完成 → 无终态总数");
    assert_eq!(entry.received_batches, 1, "取消前已入 sink 1 批");
    assert_eq!(entry.dropped_batches, 0);
    assert_eq!(entry.plugin_id.as_deref(), Some(PLUGIN_ID));
    assert_eq!(
        entry.file_path.as_deref(),
        Some(csv.path().display().to_string().as_str())
    );
}

/// C8.2：多条导入在缓冲内按时间序累积（recent 尾部语义）。
/// 用默认随机 file_id（固定 id 重导入会被 store.register 拒绝）。
#[tokio::test]
async fn diagnostics_accumulate_across_imports_in_order() {
    let session = ScriptSession::new(1, Duration::ZERO);
    let registry = Arc::new(SessionRegistry::new());
    registry.register(session);
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let discovery = Arc::new(PluginRegistry::new());
    let coordinator = ImportCoordinator::with_config(
        Arc::new(Store::new()),
        registry,
        events_tx,
        Arc::new(PluginRuntime::new(discovery.clone())),
        discovery,
        PipelineConfig::default(),
    );
    let csv = TempCsv::new();

    for _ in 0..3 {
        let outcome = coordinator
            .import_with_plugin(csv.path().to_path_buf(), PLUGIN_ID)
            .await;
        assert_eq!(outcome.status, ImportStatus::Ready);
    }

    let entries: Vec<DiagnosticEntry> = coordinator.diagnostics().recent(10);
    assert_eq!(entries.len(), 3, "三条导入全部入缓冲");
    assert!(
        entries.iter().all(|e| e.kind == DiagnosticKind::ImportDone),
        "全部 ImportDone"
    );
    assert_eq!(
        entries.windows(2).all(|w| w[0].ts_ms <= w[1].ts_ms),
        true,
        "时间序保持"
    );
}
