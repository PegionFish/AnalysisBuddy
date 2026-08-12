//! C2.2 取消竞争矩阵 + C2.5 重试语义集成测试：`GateSession`（可阻塞/可注入
//! 失败的 `PluginSession` mock，确定性优于 mock-plugin 的流式剧本——取消
//! 与卸载并发、parse 中取消等时序由闸门精确控制）驱动 `ImportCoordinator`。
//!
//! 覆盖（C2.6 验收清单）：
//! - parse 进行中取消：task 静默退出（不发终态事件、不写 frozen）、
//!   半成品唯一一方清理、ParseCancelled 恰一次、幂等重取消无新事件；
//! - load 阶段取消：同语义（丢弃半成品，不落到 schema/parse 终态）；
//! - 取消 × 卸载并发：状态由 job 所有权串行化，无 ParseCompleted/ParseFailed
//!   倒退，最终无残留状态；
//! - 终态后取消：Ok(()) 幂等，已就绪文件不受影响；
//! - 重试（C2.5）：总尝试 3 次、退避序列 1s/3s（注入短值实测时序）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ab_host::{PluginRegistry, PluginRuntime};
use ab_pipeline::{ParseEvent, PipelineEvent, PluginSession, SessionError, SessionRegistry, Store};
use ab_protocol::types::{
    CanHandleParams, CanHandleResult, CancelParseParams, FileSummary, KeyValuesParams,
    KeyValuesResult, LoadFileParams, MetricDef, ParseParams, Record, RecordBatch, SchemaResult,
    UnloadFileParams,
};
use tokio::sync::Notify;

use ab_app::pipeline_bridge::{ImportCoordinator, ImportStatus, PipelineConfig};

const PLUGIN_ID: &str = "gate-mock";
const FILE_ID: &str = "g1c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

/// 调用日志（GateSession 共享状态）。
#[derive(Default, Clone)]
struct CallLog {
    load_calls: u64,
    load_times: Vec<Instant>,
    parse_calls: u64,
    cancel_parse_calls: u64,
    unload_file_calls: u64,
}

/// 临时 CSV（导入编排读元信息，路径必须存在；用后自删）。
struct TempCsv(PathBuf);

impl TempCsv {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ab-cancel-mock-{}-{}.csv",
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

/// 可阻塞的 `PluginSession` mock：load/parse 可经闸门（started 标志 +
/// release Notify，notify_one 的 permit 语义对晚到订阅者无漏唤醒）停在
/// 指定点，供取消/卸载并发时序确定性控制。
struct GateSession {
    plugin_id: &'static str,
    schema_metrics: Vec<MetricDef>,
    /// 前 `remaining_load_failures` 次 load_file 返回脚本错误（重试测试）。
    remaining_load_failures: AtomicU64,
    load_blocked: AtomicBool,
    parse_blocked: AtomicBool,
    load_gate: Option<Arc<Notify>>,
    parse_gate: Option<Arc<Notify>>,
    records: Vec<Record>,
    log: Mutex<CallLog>,
}

impl GateSession {
    /// 无闸门（立即可完成），records 条记录。
    fn new(records: u64) -> Arc<Self> {
        Self::with_gates(records, None, None)
    }

    /// parse 阶段阻塞（取消窗口内 parse task 停在 release 前）。
    fn with_parse_gate(records: u64) -> Arc<Self> {
        Self::with_gates(records, None, Some(Arc::new(Notify::new())))
    }

    /// load 阶段阻塞（取消窗口内 load_file 停在 release 前）。
    fn with_load_gate(records: u64) -> Arc<Self> {
        Self::with_gates(records, Some(Arc::new(Notify::new())), None)
    }

    fn with_gates(
        records: u64,
        load_gate: Option<Arc<Notify>>,
        parse_gate: Option<Arc<Notify>>,
    ) -> Arc<Self> {
        Arc::new(GateSession {
            plugin_id: PLUGIN_ID,
            schema_metrics: vec![MetricDef {
                id: "fps".to_string(),
                name: "FPS".to_string(),
                unit: None,
                description: None,
                aggregation: ab_protocol::types::Aggregation::Last,
            }],
            remaining_load_failures: AtomicU64::new(0),
            load_blocked: AtomicBool::new(false),
            parse_blocked: AtomicBool::new(false),
            load_gate,
            parse_gate,
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
            log: Mutex::new(CallLog::default()),
        })
    }

    /// 脚本化前 N 次 load_file 失败（第 N+1 次起成功）。
    fn set_load_failures(&self, n: u64) {
        self.remaining_load_failures.store(n, Ordering::SeqCst);
    }

    fn log(&self) -> CallLog {
        self.log.lock().unwrap().clone()
    }

    fn cancel_called(&self) -> u64 {
        self.log.lock().unwrap().cancel_parse_calls
    }

    fn unload_called(&self) -> u64 {
        self.log.lock().unwrap().unload_file_calls
    }

    /// 轮询等待布尔条件（10ms 步进；5s 超时 panic——防悬挂的最终防线）。
    async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(
                Instant::now() < deadline,
                "等待 {what} 超时（5s）——时序失控，测试悬挂防线"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_parse_blocked(&self) {
        Self::wait_until("parse 进入阻塞点", || self.parse_blocked.load(Ordering::SeqCst)).await;
    }

    async fn wait_load_blocked(&self) {
        Self::wait_until("load 进入阻塞点", || self.load_blocked.load(Ordering::SeqCst)).await;
    }

    async fn wait_cancel_called(&self) {
        Self::wait_until("cancel_parse 被调用", || self.cancel_called() > 0).await;
    }

    async fn wait_unload_called(&self) {
        Self::wait_until("unload_file 被调用", || self.unload_called() > 0).await;
    }

    fn release_parse(&self) {
        if let Some(gate) = &self.parse_gate {
            gate.notify_one();
        }
    }

    fn release_load(&self) {
        if let Some(gate) = &self.load_gate {
            gate.notify_one();
        }
    }
}

#[async_trait::async_trait]
impl PluginSession for GateSession {
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
        {
            let mut log = self.log.lock().unwrap();
            log.load_calls += 1;
            log.load_times.push(Instant::now());
        }
        if let Some(gate) = &self.load_gate {
            self.load_blocked.store(true, Ordering::SeqCst);
            gate.notified().await;
            self.load_blocked.store(false, Ordering::SeqCst);
        }
        let remaining = self.remaining_load_failures.load(Ordering::SeqCst);
        if remaining > 0 {
            self.remaining_load_failures
                .store(remaining - 1, Ordering::SeqCst);
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
        self.log.lock().unwrap().parse_calls += 1;
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
        if let Some(gate) = &self.parse_gate {
            self.parse_blocked.store(true, Ordering::SeqCst);
            gate.notified().await;
            self.parse_blocked.store(false, Ordering::SeqCst);
        }
        Ok(self.records.len() as u64)
    }

    async fn cancel_parse(&self, _p: CancelParseParams) -> Result<(), SessionError> {
        self.log.lock().unwrap().cancel_parse_calls += 1;
        Ok(())
    }

    async fn key_values(&self, _p: KeyValuesParams) -> Result<KeyValuesResult, SessionError> {
        Ok(KeyValuesResult { entries: vec![] })
    }

    async fn unload_file(&self, _p: UnloadFileParams) -> Result<(), SessionError> {
        self.log.lock().unwrap().unload_file_calls += 1;
        Ok(())
    }
}

/// 预注册 GateSession + 固定 file_id 的协调器。
fn coordinator_with(session: Arc<GateSession>) -> (ImportCoordinator, tokio::sync::mpsc::UnboundedReceiver<PipelineEvent>) {
    let registry = Arc::new(SessionRegistry::new());
    registry.register(session);
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let discovery = Arc::new(PluginRegistry::new());
    let coordinator = ImportCoordinator::with_config(
        Arc::new(Store::new()),
        registry,
        events_tx,
        Arc::new(PluginRuntime::new(discovery.clone())),
        discovery,
        PipelineConfig {
            file_id_fn: Some(Arc::new(|_| FILE_ID.to_string())),
            ..PipelineConfig::default()
        },
    );
    (coordinator, events_rx)
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<PipelineEvent>) -> Vec<PipelineEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn count(events: &[PipelineEvent], f: impl Fn(&PipelineEvent) -> bool) -> usize {
    events.iter().filter(|e| f(e)).count()
}

/// C2.2 主场景：parse 进行中取消 → task 静默退出（不发终态事件、不写
/// frozen）、半成品唯一一方清理、ParseCancelled 恰一次、诊断计数齐备、
/// 重取消幂等。
#[tokio::test]
async fn cancel_during_parse_stops_task_and_cleans_up_once() {
    let session = GateSession::with_parse_gate(2);
    let (coordinator, mut events_rx) = coordinator_with(session.clone());
    let csv = TempCsv::new();

    let import_task = tokio::spawn({
        let coordinator = coordinator.clone();
        let path = csv.path().to_path_buf();
        async move {
            coordinator
                .import_with_plugin(path, PLUGIN_ID)
                .await
        }
    });
    session.wait_parse_blocked().await;

    let cancel_task = tokio::spawn({
        let coordinator = coordinator.clone();
        async move { coordinator.cancel_parse(FILE_ID).await }
    });
    // cancel 已置 cancelled 并调过插件（确定性先于放行）。
    session.wait_cancel_called().await;

    session.release_parse();
    let outcome = import_task.await.expect("import task");
    cancel_task.await.expect("cancel task");

    // 结果：cancelled 错误码，不伪装成终态。
    assert_eq!(outcome.status, ImportStatus::Error, "取消后 outcome: {outcome:?}");
    let error = outcome.error.as_ref().expect("cancelled 错误");
    assert_eq!(error.code, "cancelled");

    // 状态：无 frozen、无 file_index。
    assert!(coordinator.list_frozen().is_empty(), "取消后不得残留 frozen");
    assert!(
        coordinator.file_index().get(FILE_ID).is_none(),
        "取消后不得残留 file_index"
    );

    // 事件：ParseCancelled 恰一次；无 ParseCompleted/ParseFailed/QueryReady。
    let events = drain(&mut events_rx);
    assert_eq!(
        count(&events, |e| matches!(e, PipelineEvent::ParseCancelled { .. })),
        1,
        "ParseCancelled 恰一次"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::FileLoaded { .. })),
        "load 完成于取消前 → FileLoaded 仍在"
    );
    assert!(
        !events.iter().any(|e| matches!(e, PipelineEvent::ParseCompleted { .. })),
        "取消后不得发 ParseCompleted"
    );
    assert!(
        !events.iter().any(|e| matches!(e, PipelineEvent::QueryReady { .. })),
        "取消后不得发 QueryReady"
    );
    assert!(
        !events.iter().any(|e| matches!(e, PipelineEvent::ParseFailed { .. })),
        "取消后不得发 ParseFailed"
    );

    // 诊断快照：received == records_total，dropped == 0（C2.4）。
    let diag = coordinator
        .job_diagnostics(FILE_ID)
        .expect("job 诊断快照（终态后仍可读）");
    assert_eq!(diag.received_records, 2);
    assert_eq!(diag.received_batches, 1);
    assert_eq!(diag.dropped_batches, 0);

    // 幂等：终态后再次取消 → 无新事件、无状态变化。
    coordinator.cancel_parse(FILE_ID).await;
    let after = drain(&mut events_rx);
    assert!(
        after
            .iter()
            .all(|e| !matches!(e, PipelineEvent::ParseCancelled { .. })),
        "重取消不得再发 ParseCancelled（C2.1 幂等）"
    );
    assert!(coordinator.list_frozen().is_empty());
}

/// C2.2 规则 3：load 阶段取消 → 半成品（未落地）丢弃，不再落到 schema/parse。
#[tokio::test]
async fn cancel_during_load_discards_half_loaded_file() {
    let session = GateSession::with_load_gate(0);
    let (coordinator, mut events_rx) = coordinator_with(session.clone());

    let import_task = tokio::spawn({
        let coordinator = coordinator.clone();
        let csv = TempCsv::new();
        async move {
            coordinator
                .import_with_plugin(csv.path().to_path_buf(), PLUGIN_ID)
                .await
        }
    });
    session.wait_load_blocked().await;

    let cancel_task = tokio::spawn({
        let coordinator = coordinator.clone();
        async move { coordinator.cancel_parse(FILE_ID).await }
    });
    session.wait_cancel_called().await;

    session.release_load();
    let outcome = import_task.await.expect("import task");
    cancel_task.await.expect("cancel task");

    assert_eq!(outcome.status, ImportStatus::Error);
    assert_eq!(outcome.error.as_ref().expect("error").code, "cancelled");
    assert!(coordinator.list_frozen().is_empty());
    assert!(coordinator.file_index().get(FILE_ID).is_none());

    let events = drain(&mut events_rx);
    assert_eq!(
        count(&events, |e| matches!(e, PipelineEvent::ParseCancelled { .. })),
        1
    );
    assert!(
        !events.iter().any(|e| matches!(e, PipelineEvent::ParseCompleted { .. })),
        "load 阶段取消不得发 ParseCompleted"
    );
    assert!(
        !events.iter().any(|e| matches!(e, PipelineEvent::ParseFailed { .. })),
        "load 阶段取消不得发 ParseFailed"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PipelineEvent::FileLoadFailed { .. })),
        "load 阶段取消不得发 FileLoadFailed"
    );
}

/// C2.2 规则 5：取消 × 卸载并发 → 状态由 job 所有权串行化，无
/// ParseCompleted/ParseFailed 倒退，最终无残留状态，两事件各恰一次。
#[tokio::test]
async fn cancel_and_unload_race_serializes_via_job_ownership() {
    let session = GateSession::with_parse_gate(2);
    let (coordinator, mut events_rx) = coordinator_with(session.clone());

    let import_task = tokio::spawn({
        let coordinator = coordinator.clone();
        let csv = TempCsv::new();
        async move {
            coordinator
                .import_with_plugin(csv.path().to_path_buf(), PLUGIN_ID)
                .await
        }
    });
    session.wait_parse_blocked().await;

    // 先 cancel（已持有 job）再 unload 并发入场；二者都阻塞在插件调用上。
    let cancel_task = tokio::spawn({
        let coordinator = coordinator.clone();
        async move { coordinator.cancel_parse(FILE_ID).await }
    });
    session.wait_cancel_called().await;
    let unload_task = tokio::spawn({
        let coordinator = coordinator.clone();
        async move { coordinator.unload_file(FILE_ID).await }
    });
    session.wait_unload_called().await;

    session.release_parse();
    let outcome = import_task.await.expect("import task");
    cancel_task.await.expect("cancel task");
    unload_task.await.expect("unload task");

    assert_eq!(outcome.status, ImportStatus::Error, "并发下 parse 不得复活为 Ready");
    assert!(coordinator.list_frozen().is_empty(), "卸载+取消后无 frozen 残留");
    assert!(coordinator.file_index().get(FILE_ID).is_none());

    let events = drain(&mut events_rx);
    assert!(
        !events.iter().any(|e| matches!(e, PipelineEvent::ParseCompleted { .. })),
        "旧 task 不得把状态改回 Ready（倒退回归）"
    );
    assert!(
        !events.iter().any(|e| matches!(e, PipelineEvent::ParseFailed { .. })),
        "旧 task 不得把状态改回 ParseFailed（倒退回归）"
    );
    assert_eq!(
        count(&events, |e| matches!(e, PipelineEvent::ParseCancelled { .. })),
        1,
        "ParseCancelled 恰一次"
    );
    assert_eq!(
        count(&events, |e| matches!(e, PipelineEvent::FileUnloaded { .. })),
        1,
        "FileUnloaded 恰一次"
    );
}

/// C2.1：终态（Ready）后取消 → 幂等 Ok(())，已就绪文件不受影响。
#[tokio::test]
async fn cancel_after_completion_is_idempotent_noop() {
    let session = GateSession::new(2);
    let (coordinator, mut events_rx) = coordinator_with(session.clone());
    let csv = TempCsv::new();

    let outcome = coordinator
        .import_with_plugin(csv.path().to_path_buf(), PLUGIN_ID)
        .await;
    assert_eq!(outcome.status, ImportStatus::Ready, "无取消时正常 Ready");
    let events = drain(&mut events_rx);
    assert!(
        events.iter().any(|e| matches!(e, PipelineEvent::ParseCompleted { .. })),
        "正常完成发 ParseCompleted"
    );
    assert!(!coordinator.list_frozen().is_empty());

    coordinator.cancel_parse(FILE_ID).await;
    let after = drain(&mut events_rx);
    assert!(
        after
            .iter()
            .all(|e| !matches!(e, PipelineEvent::ParseCancelled { .. })),
        "终态后取消不得发 ParseCancelled（幂等）"
    );
    assert!(
        !coordinator.list_frozen().is_empty(),
        "终态文件不被取消影响"
    );
    assert_eq!(session.cancel_called(), 0, "终态后取消不触达插件");
}

/// C2.5：load_file 重试锁定——总尝试 3 次（初始 + 重试 2 次），退避序列
/// 1s/3s（注入 80ms/160ms 实测时序：第 1、2 次失败后各按序等待），
/// 第 3 次成功 → Ready 且 FileLoaded 恰一次。
#[tokio::test]
async fn load_file_retries_exactly_three_times_with_backoff_sequence() {
    let session = GateSession::new(2);
    session.set_load_failures(2); // 第 1、2 次失败，第 3 次成功
    let registry = Arc::new(SessionRegistry::new());
    registry.register(session.clone());
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let discovery = Arc::new(PluginRegistry::new());
    let coordinator = ImportCoordinator::with_config(
        Arc::new(Store::new()),
        registry,
        events_tx,
        Arc::new(PluginRuntime::new(discovery.clone())),
        discovery,
        PipelineConfig {
            file_id_fn: Some(Arc::new(|_| FILE_ID.to_string())),
            load_retry_backoffs: vec![
                Duration::from_millis(80),
                Duration::from_millis(160),
            ],
            ..PipelineConfig::default()
        },
    );

    let csv = TempCsv::new();
    let outcome = coordinator
        .import_with_plugin(csv.path().to_path_buf(), PLUGIN_ID)
        .await;
    assert_eq!(
        outcome.status,
        ImportStatus::Ready,
        "第 3 次尝试成功后必须 Ready（总尝试 3 次）"
    );

    let log = session.log();
    assert_eq!(log.load_calls, 3, "总尝试 3 次（初始 + 重试 2 次，P2-02）");
    // 退避断言取宽松下界（Windows 计时器 ~15.6ms 粒度，80/160ms 注入足够
    // 远离阈值，防抖动）。
    let d1 = log.load_times[1] - log.load_times[0];
    let d2 = log.load_times[2] - log.load_times[1];
    assert!(
        d1 >= Duration::from_millis(60),
        "第 1 次失败后退避 ~80ms，实测 {d1:?}"
    );
    assert!(
        d2 >= Duration::from_millis(130),
        "第 2 次失败后退避 ~160ms，实测 {d2:?}"
    );

    let events = drain(&mut events_rx);
    assert_eq!(
        count(&events, |e| matches!(e, PipelineEvent::FileLoaded { .. })),
        1,
        "只有最后一次尝试成功 → FileLoaded 恰一次"
    );
}
