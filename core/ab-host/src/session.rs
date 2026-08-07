//! 会话与监督树（host-runtime.md §3/§5.4；protocol.md §5.1）。
//!
//! 每个「插件 × 会话」实例持有一个状态机；`PluginRuntime` 负责按发现结果
//! 拉起（spawn → 250ms 快速退出检测 → initialize 握手 → Ready）并登记孤儿兜底。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc, Notify};

use ab_protocol::errors::ERR_PLUGIN_BUSY;
use ab_protocol::types::{
    AnnotateParams, AnnotateResult, CanHandleParams, CanHandleResult, CancelParseParams,
    FileSummary, InitializeParams, InitializeResult, KeyValuesParams, KeyValuesResult,
    LoadFileParams, ParseParams, ParseResult, ProgressParams, RecordBatch, SchemaResult,
    UnloadFileParams,
};

use crate::discovery::DiscoveredPlugin;
use crate::rpc::{
    run_read_loop, FrameDisposition, FrameError, NotificationHandler, PluginNotification,
    ReadLoopError, RpcChannel, RpcOutcome, SeqValidator,
};
use crate::spawner::PluginSpawner;
use crate::{HostError, HostEvent};

// ---------------------------------------------------------------------------
// 状态机（§3.2，映射 protocol.md §5.1）
// ---------------------------------------------------------------------------

/// 插件进程状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginProcessState {
    Discovered,
    Spawning,
    Initializing,
    Ready,
    Loading,
    Parsing,
    Draining,
    Shutdown,
    Crashed,
    Timeout,
}

impl PluginProcessState {
    /// 吸收态：实例只读，重试 = 新实例（protocol.md §5.2）。
    pub fn is_absorbing(self) -> bool {
        matches!(
            self,
            PluginProcessState::Shutdown
                | PluginProcessState::Crashed
                | PluginProcessState::Timeout
        )
    }
}

/// 状态机事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmEvent {
    SpawnRequested,
    SpawnFailed,
    Initialized,
    LoadStarted,
    LoadFinished,
    ParseStarted,
    ParseFinished,
    ShutdownRequested,
    ExitConfirmed,
    HeartbeatMissed,
    ProtocolFatalError,
}

/// 状态机：查表转移，非法转移返回 `None` 且不改状态（§3.2）。
#[derive(Debug, Clone, Copy)]
pub struct StateMachine {
    state: PluginProcessState,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            state: PluginProcessState::Discovered,
        }
    }

    pub fn state(&self) -> PluginProcessState {
        self.state
    }

    /// 转移表（host-runtime.md §3.2，与 protocol.md §5.1 状态图逐条对应）。
    pub fn apply(&mut self, ev: SmEvent) -> Option<PluginProcessState> {
        let next = match (self.state, ev) {
            (PluginProcessState::Discovered, SmEvent::SpawnRequested) => {
                PluginProcessState::Spawning
            }
            (PluginProcessState::Discovered, SmEvent::ShutdownRequested) => {
                PluginProcessState::Shutdown
            }

            (PluginProcessState::Spawning, SmEvent::SpawnFailed) => PluginProcessState::Crashed,
            (PluginProcessState::Spawning, SmEvent::Initialized) => {
                PluginProcessState::Initializing
            }
            (PluginProcessState::Spawning, SmEvent::ShutdownRequested) => {
                PluginProcessState::Draining
            }
            (PluginProcessState::Spawning, SmEvent::ExitConfirmed) => PluginProcessState::Crashed,

            (PluginProcessState::Initializing, SmEvent::Initialized) => PluginProcessState::Ready,
            (PluginProcessState::Initializing, SmEvent::ShutdownRequested) => {
                PluginProcessState::Draining
            }
            (PluginProcessState::Initializing, SmEvent::ExitConfirmed) => {
                PluginProcessState::Crashed
            }
            (PluginProcessState::Initializing, SmEvent::HeartbeatMissed) => {
                PluginProcessState::Timeout
            }
            (PluginProcessState::Initializing, SmEvent::ProtocolFatalError) => {
                PluginProcessState::Crashed
            }

            (PluginProcessState::Ready, SmEvent::LoadStarted) => PluginProcessState::Loading,
            (PluginProcessState::Ready, SmEvent::ParseStarted) => PluginProcessState::Parsing,
            (PluginProcessState::Ready, SmEvent::ShutdownRequested) => PluginProcessState::Draining,
            (PluginProcessState::Ready, SmEvent::ExitConfirmed) => PluginProcessState::Crashed,
            (PluginProcessState::Ready, SmEvent::ProtocolFatalError) => PluginProcessState::Crashed,

            (PluginProcessState::Loading, SmEvent::LoadFinished) => PluginProcessState::Ready,
            (PluginProcessState::Loading, SmEvent::ShutdownRequested) => {
                PluginProcessState::Draining
            }
            (PluginProcessState::Loading, SmEvent::ExitConfirmed) => PluginProcessState::Crashed,
            (PluginProcessState::Loading, SmEvent::HeartbeatMissed) => PluginProcessState::Timeout,
            (PluginProcessState::Loading, SmEvent::ProtocolFatalError) => {
                PluginProcessState::Crashed
            }

            (PluginProcessState::Parsing, SmEvent::ParseFinished) => PluginProcessState::Ready,
            (PluginProcessState::Parsing, SmEvent::ShutdownRequested) => {
                PluginProcessState::Draining
            }
            (PluginProcessState::Parsing, SmEvent::ExitConfirmed) => PluginProcessState::Crashed,
            (PluginProcessState::Parsing, SmEvent::HeartbeatMissed) => PluginProcessState::Timeout,
            (PluginProcessState::Parsing, SmEvent::ProtocolFatalError) => {
                PluginProcessState::Crashed
            }

            (PluginProcessState::Draining, SmEvent::ExitConfirmed) => PluginProcessState::Shutdown,
            (PluginProcessState::Draining, SmEvent::ProtocolFatalError) => {
                PluginProcessState::Shutdown
            }

            // 吸收态：全 ✗。
            _ => return None,
        };
        self.state = next;
        Some(next)
    }
}

// ---------------------------------------------------------------------------
// 孤儿进程兜底注册表（§3.4 第 3 层）
// ---------------------------------------------------------------------------

/// 全部成功 spawn 的进程句柄统一登记；宿主析构时 sweep（§9 第 5 条）。
#[derive(Debug, Default)]
pub struct ChildProcessRegistry {
    children: Mutex<HashMap<u32, mpsc::Sender<()>>>,
}

impl ChildProcessRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, pid: u32, kill: mpsc::Sender<()>) {
        self.children
            .lock()
            .expect("children lock poisoned")
            .insert(pid, kill);
    }

    pub fn unregister(&self, pid: u32) {
        self.children
            .lock()
            .expect("children lock poisoned")
            .remove(&pid);
    }

    pub fn len(&self) -> usize {
        self.children.lock().expect("children lock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.children
            .lock()
            .expect("children lock poisoned")
            .is_empty()
    }

    /// 兜底 sweep：对仍登记的子进程直接 kill（宿主析构 / panic 钩子路径）。
    pub fn sweep_orphans(&self) {
        let children: Vec<mpsc::Sender<()>> = self
            .children
            .lock()
            .expect("children lock poisoned")
            .values()
            .cloned()
            .collect();
        for kill in children {
            let _ = kill.try_send(());
        }
    }
}

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

/// 一个「插件 × 已加载文件」常驻会话（§3.3）。
#[derive(Clone)]
pub struct PluginSession {
    inner: Arc<SessionInner>,
}

impl std::fmt::Debug for PluginSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PluginSession {{ plugin_id: {:?}, state: {:?} }}",
            self.inner.plugin_id,
            self.state()
        )
    }
}

struct SessionInner {
    plugin_id: String,
    /// 会话实例序号（与 `(plugin_id, session_seq)` 隔离 stderr 缓冲用，A-03 落地）。
    session_seq: u64,
    state: Mutex<StateMachine>,
    channel: RpcChannel,
    events: broadcast::Sender<HostEvent>,
    sessions: Arc<Mutex<HashMap<String, Arc<PluginSession>>>>,
    children: Arc<ChildProcessRegistry>,
    pid: u32,
    loaded_files: Mutex<HashSet<String>>,
    seq: Mutex<SeqValidator>,
    invalid_json: AtomicU64,
    terminated: AtomicBool,
    kill_tx: Mutex<Option<mpsc::Sender<()>>>,
    exit_notify: Notify,
}

/// 拉起后「立即退出」判定窗口（§3.1）。
const QUICK_EXIT_WINDOW: Duration = Duration::from_millis(250);

impl PluginSession {
    fn new(
        plugin: &DiscoveredPlugin,
        events: broadcast::Sender<HostEvent>,
        sessions: Arc<Mutex<HashMap<String, Arc<PluginSession>>>>,
        children: Arc<ChildProcessRegistry>,
        pid: u32,
        session_seq: u64,
        channel: RpcChannel,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(SessionInner {
                plugin_id: plugin.manifest.id.clone(),
                session_seq,
                state: Mutex::new(StateMachine::new()),
                channel,
                events,
                sessions,
                children,
                pid,
                loaded_files: Mutex::new(HashSet::new()),
                seq: Mutex::new(SeqValidator::new()),
                invalid_json: AtomicU64::new(0),
                terminated: AtomicBool::new(false),
                kill_tx: Mutex::new(None),
                exit_notify: Notify::new(),
            }),
        })
    }

    pub fn state(&self) -> PluginProcessState {
        self.inner
            .state
            .lock()
            .expect("state lock poisoned")
            .state()
    }

    pub fn plugin_id(&self) -> &str {
        &self.inner.plugin_id
    }

    /// 会话实例序号（宿主本地，stderr 缓冲隔离键的组成部分）。
    pub fn session_seq(&self) -> u64 {
        self.inner.session_seq
    }

    pub fn is_live(&self) -> bool {
        !self.state().is_absorbing()
    }

    /// 已加载文件集合（§2.3 会话模型）。
    pub fn loaded_files(&self) -> Vec<String> {
        let mut files: Vec<String> = self
            .inner
            .loaded_files
            .lock()
            .expect("loaded lock poisoned")
            .iter()
            .cloned()
            .collect();
        files.sort();
        files
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<HostEvent> {
        self.inner.events.subscribe()
    }

    pub fn subscribe_notifications(&self) -> mpsc::Receiver<PluginNotification> {
        self.inner.channel.subscribe_notifications()
    }

    // -----------------------------------------------------------------------
    // RPC 方法族（§2 签名，宿主封装）
    // -----------------------------------------------------------------------

    /// §2.1 initialize（握手在 `PluginRuntime::spawn` 内封装，此处供显式调用）。
    pub async fn initialize(
        &self,
        params: InitializeParams,
    ) -> Result<InitializeResult, HostError> {
        self.inner
            .channel
            .call_typed("initialize", params, Duration::from_secs(5))
            .await
    }

    /// §2.2 can_handle（超时按弃权处理属 A-03 超时动作表）。
    pub async fn can_handle(&self, params: CanHandleParams) -> Result<CanHandleResult, HostError> {
        self.inner
            .channel
            .call_typed("can_handle", params, Duration::from_secs(3))
            .await
    }

    /// §2.3 load_file（Ready → Loading → Ready）。
    pub async fn load_file(&self, params: LoadFileParams) -> Result<FileSummary, HostError> {
        self.reject_busy("load_file")?;
        self.apply_ev(SmEvent::LoadStarted);
        let file_id = params.file_id.clone();
        let r = self
            .inner
            .channel
            .call_typed("load_file", params, Duration::from_secs(10))
            .await;
        self.apply_ev(SmEvent::LoadFinished);
        if r.is_ok() {
            self.inner
                .loaded_files
                .lock()
                .expect("loaded lock poisoned")
                .insert(file_id);
        }
        r
    }

    /// §2.4 parse（Ready → Parsing → Ready；数据走 §4.4 通知流）。
    /// A-03 引入心跳看门狗（`timeout_for("parse")` 窗口 + progress/RecordBatch 续期）。
    pub async fn parse(&self, params: ParseParams) -> Result<ParseResult, HostError> {
        self.reject_busy("parse")?;
        self.inner
            .seq
            .lock()
            .expect("seq lock poisoned")
            .reset(&params.file_id);
        self.apply_ev(SmEvent::ParseStarted);
        let r = self
            .inner
            .channel
            .call_typed("parse", params, Duration::from_secs(600))
            .await;
        self.apply_ev(SmEvent::ParseFinished);
        r
    }

    /// §2.5 schema。
    pub async fn schema(&self) -> Result<SchemaResult, HostError> {
        self.inner
            .channel
            .call_typed(
                "schema",
                serde_json::Value::Object(Default::default()),
                Duration::from_secs(3),
            )
            .await
    }

    /// §2.6 key_values。
    pub async fn key_values(&self, params: KeyValuesParams) -> Result<KeyValuesResult, HostError> {
        self.inner
            .channel
            .call_typed("key_values", params, Duration::from_secs(10))
            .await
    }

    /// §2.7 annotate（可选能力）。
    pub async fn annotate(&self, params: AnnotateParams) -> Result<AnnotateResult, HostError> {
        self.inner
            .channel
            .call_typed("annotate", params, Duration::from_secs(10))
            .await
    }

    /// §2.8 unload_file（幂等）。
    pub async fn unload_file(&self, params: UnloadFileParams) -> Result<(), HostError> {
        let file_id = params.file_id.clone();
        let r = self
            .inner
            .channel
            .call_typed::<_, serde_json::Value>("unload_file", params, Duration::from_secs(3))
            .await
            .map(|_| ());
        if r.is_ok() {
            self.inner
                .loaded_files
                .lock()
                .expect("loaded lock poisoned")
                .remove(&file_id);
        }
        r
    }

    /// §3.4 cancel_parse（对未在解析的 file_id 同样回 `{}`）。
    pub async fn cancel_parse(&self, params: CancelParseParams) -> Result<(), HostError> {
        self.inner
            .channel
            .call_typed::<_, serde_json::Value>("cancel_parse", params, Duration::from_secs(10))
            .await
            .map(|_| ())
    }

    /// §2.9 优雅停机（§3.5）：Draining → 发 `shutdown`（3s 预算，以先到为准）
    /// → drop stdin（EOF）→ 若仍活 kill → 等 exit → Shutdown。
    pub async fn shutdown(&self) -> Result<(), HostError> {
        if self.state().is_absorbing() {
            return Ok(());
        }
        self.apply_ev(SmEvent::ShutdownRequested);

        let outcome = self
            .inner
            .channel
            .call("shutdown", serde_json::json!({}), Duration::from_secs(3))
            .await;
        self.inner.channel.close_stdin();

        if !self.wait_terminated(Duration::from_secs(3)).await {
            self.request_kill();
            self.wait_terminated(Duration::from_secs(2)).await;
        }
        match outcome {
            Ok(RpcOutcome::Result(_)) | Ok(RpcOutcome::Error { .. }) => Ok(()),
            Ok(RpcOutcome::TransportError(e)) | Err(e) => Err(e),
        }
    }

    /// 等待进程退出（`terminated` 标志 + notify），超时返回 `false`。
    async fn wait_terminated(&self, timeout: Duration) -> bool {
        loop {
            if self.inner.terminated.load(Ordering::Acquire) {
                return true;
            }
            let notified = self.inner.exit_notify.notified();
            if tokio::time::timeout(timeout, notified).await.is_err() {
                return false;
            }
        }
    }

    // -----------------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------------

    fn reject_busy(&self, method: &str) -> Result<(), HostError> {
        if matches!(
            self.state(),
            PluginProcessState::Parsing | PluginProcessState::Loading
        ) {
            return Err(HostError::Protocol {
                code: ERR_PLUGIN_BUSY,
                message: "plugin_busy".to_string(),
                data: Some(serde_json::json!({ "method": method })),
            });
        }
        Ok(())
    }

    /// 查表转移；成功时发 `HostEvent::StateChanged`（§3.2 实现要求）。
    fn apply_ev(&self, ev: SmEvent) {
        let mut sm = self.inner.state.lock().expect("state lock poisoned");
        let from = sm.state();
        if let Some(to) = sm.apply(ev) {
            let _ = self.inner.events.send(HostEvent::StateChanged {
                plugin_id: self.inner.plugin_id.clone(),
                from,
                to,
            });
        }
    }

    /// 统一终止出口（§5.4）：kill（若仍活）→ 吸收态 → 清空 pending → 事件 → 清理。
    fn terminate_from(
        self: &Arc<Self>,
        ev: SmEvent,
        exit_code: Option<i32>,
        pending_error: HostError,
    ) {
        if self
            .inner
            .terminated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.apply_ev(ev);
        self.inner.channel.drain_pending(pending_error);
        self.request_kill();
        let _ = self.inner.events.send(HostEvent::SessionTerminated {
            plugin_id: self.inner.plugin_id.clone(),
            exit_code,
            summary: String::new(), // A-03：附 stderr tail_summary
        });
        self.cleanup();
        self.inner.exit_notify.notify_waiters();
    }

    fn cleanup(self: &Arc<Self>) {
        self.inner
            .loaded_files
            .lock()
            .expect("loaded lock poisoned")
            .clear();
        self.inner
            .seq
            .lock()
            .expect("seq lock poisoned")
            .reset_all();
        self.inner.children.unregister(self.inner.pid);
        // 自移除（若映射里仍指向本实例，避免误删新会话）。
        let Ok(mut map) = self.inner.sessions.lock() else {
            return;
        };
        let plugin_id = self.inner.plugin_id.clone();
        if map
            .get(&plugin_id)
            .map(|s| Arc::ptr_eq(s, self))
            .unwrap_or(false)
        {
            map.remove(&plugin_id);
        }
    }

    fn request_kill(&self) {
        if let Some(tx) = self
            .inner
            .kill_tx
            .lock()
            .expect("kill lock poisoned")
            .as_ref()
        {
            let _ = tx.try_send(());
        }
    }

    /// 读泵结束处理（§4.2 帧错误处置表 / §5.4 Eof）。
    fn on_read_loop_error(self: &Arc<Self>, err: ReadLoopError) {
        match err {
            ReadLoopError::Frame(FrameError::Eof) => {
                // Eof 与 wait() 互为印证，任一先触发即认定（§5.4）。
                self.terminate_from(SmEvent::ExitConfirmed, None, HostError::process_exited());
            }
            ReadLoopError::Frame(FrameError::LineTooLong) => {
                let err = HostError::frame_error("line exceeds 8MB limit");
                self.terminate_from(SmEvent::ProtocolFatalError, None, err);
            }
            ReadLoopError::Frame(FrameError::MalformedLine) => {
                let err = HostError::frame_error("malformed protocol line");
                self.terminate_from(SmEvent::ProtocolFatalError, None, err);
            }
            // InvalidJson 已由 SessionHandler::on_frame_error 计数：重复出现才停。
            ReadLoopError::Frame(FrameError::InvalidJson) => {
                let err = HostError::frame_error("invalid JSON on stdout");
                self.terminate_from(SmEvent::ProtocolFatalError, None, err);
            }
            ReadLoopError::Fatal(reason) => {
                eprintln!(
                    "ERROR ab-host: plugin {} fatal: {reason}",
                    self.inner.plugin_id
                );
                self.terminate_from(
                    SmEvent::ProtocolFatalError,
                    None,
                    HostError::process_exited(),
                );
            }
        }
    }

    /// 写侧 BrokenPipe → 等价进程退出（§5.4）。
    fn on_pipe_broken(self: &Arc<Self>, err: HostError) {
        eprintln!(
            "ERROR ab-host: plugin {} write side: {err}",
            self.inner.plugin_id
        );
        self.terminate_from(SmEvent::ExitConfirmed, None, HostError::process_exited());
    }
}

/// 读泵 notification 分流（§4.4）：progress/RecordBatch 各自重置看门狗（A-03）
/// 并转发订阅者；RecordBatch 做 seq 连续性校验；未知 method 记日志忽略。
struct SessionHandler {
    inner: Arc<SessionInner>,
}

impl NotificationHandler for SessionHandler {
    fn on_notification(&mut self, method: &str, params: &serde_json::Value) -> Result<(), String> {
        match method {
            "progress" => {
                let progress: ProgressParams = serde_json::from_value(params.clone())
                    .map_err(|e| format!("invalid progress notification: {e}"))?;
                // A-03：parse 看门狗续期。
                let _ = self
                    .inner
                    .events
                    .send(HostEvent::Progress(progress.clone()));
                self.inner
                    .channel
                    .fan_out(PluginNotification::Progress(progress));
                Ok(())
            }
            "RecordBatch" => {
                let batch: RecordBatch = serde_json::from_value(params.clone())
                    .map_err(|e| format!("invalid RecordBatch notification: {e}"))?;
                // 缺号 / 重复 seq → 协议致命错（protocol.md §3.2）。
                self.inner
                    .seq
                    .lock()
                    .expect("seq lock poisoned")
                    .accept(&batch)?;
                // A-03：parse 看门狗续期。
                self.inner
                    .channel
                    .fan_out(PluginNotification::RecordBatch(batch));
                Ok(())
            }
            other => {
                eprintln!(
                    "WARN ab-host: plugin {} sent unknown notification `{other}`, ignored",
                    self.inner.plugin_id
                );
                Ok(())
            }
        }
    }

    /// 帧错误处置（§4.2）：InvalidJson 首次告警续读，重复出现终止；其余致命。
    fn on_frame_error(&mut self, err: FrameError) -> FrameDisposition {
        if let FrameError::InvalidJson = err {
            let n = self.inner.invalid_json.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 1 {
                eprintln!(
                    "WARN ab-host: plugin {} emitted invalid JSON (first occurrence, session continues)",
                    self.inner.plugin_id
                );
                return FrameDisposition::Continue;
            }
            eprintln!(
                "ERROR ab-host: plugin {} repeated invalid JSON, terminating session",
                self.inner.plugin_id
            );
        }
        FrameDisposition::Stop
    }
}

// ---------------------------------------------------------------------------
// 运行时
// ---------------------------------------------------------------------------

/// 插件运行时聚合根：拉起 / 复用常驻进程 / 全量停机。
pub struct PluginRuntime {
    registry: Arc<crate::discovery::PluginRegistry>,
    spawner: PluginSpawner,
    sessions: Arc<Mutex<HashMap<String, Arc<PluginSession>>>>,
    spawn_lock: tokio::sync::Mutex<()>,
    events: broadcast::Sender<HostEvent>,
    children: Arc<ChildProcessRegistry>,
    next_session_seq: AtomicU64,
}

impl PluginRuntime {
    pub fn new(registry: Arc<crate::discovery::PluginRegistry>) -> Self {
        Self {
            registry,
            spawner: PluginSpawner,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            spawn_lock: tokio::sync::Mutex::new(()),
            events: broadcast::channel(1024).0,
            children: Arc::new(ChildProcessRegistry::new()),
            next_session_seq: AtomicU64::new(0),
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<HostEvent> {
        self.events.subscribe()
    }

    /// §7.4 spawn：拉起 → 250ms 快速退出检测 → initialize 握手 → Ready。
    ///
    /// 注：入参为 [`DiscoveredPlugin`]（含解析后的 entry），因为握手与拉起需要
    /// `resolved` 与 `plugin_dir`，`Manifest` 自身不携带。
    pub async fn spawn(&self, plugin: &DiscoveredPlugin) -> Result<Arc<PluginSession>, HostError> {
        let spawned = self.spawner.spawn(&plugin.resolved).inspect_err(|_| {
            let _ = self.events.send(HostEvent::StateChanged {
                plugin_id: plugin.manifest.id.clone(),
                from: PluginProcessState::Spawning,
                to: PluginProcessState::Crashed,
            });
        })?;
        let pid = spawned
            .child
            .id()
            .ok_or_else(|| HostError::Transport("plugin child has no pid".to_string()))?;

        // 会话对象先行创建（后台任务需要 Arc<inner>）。
        let (writer_tx, writer_rx) = mpsc::channel::<String>(64);
        let channel = RpcChannel::new(writer_tx);
        let session = PluginSession::new(
            plugin,
            self.events.clone(),
            self.sessions.clone(),
            self.children.clone(),
            pid,
            self.next_session_seq.fetch_add(1, Ordering::Relaxed) + 1,
            channel,
        );
        session.apply_ev(SmEvent::SpawnRequested);

        // 250ms 快速退出检测（§3.1）：拿到退出状态 → SpawnFailed → Crashed。
        let mut child = spawned.child;
        match tokio::time::timeout(QUICK_EXIT_WINDOW, child.wait()).await {
            Ok(Ok(status)) => {
                let code = status.code();
                session.terminate_from(SmEvent::SpawnFailed, code, HostError::process_exited());
                return Err(HostError::Transport(format!(
                    "plugin exited immediately after spawn (code {code:?})"
                )));
            }
            Ok(Err(e)) => {
                session.terminate_from(SmEvent::SpawnFailed, None, HostError::process_exited());
                return Err(HostError::Transport(format!("child wait failed: {e}")));
            }
            Err(_) => {} // 存活，继续。
        }

        // 后台任务：写者（stdin 序列化出口）、读泵（stdout）、等待（exit/kill）、stderr。
        session.start_writer_task(spawned.stdin, writer_rx);
        session.start_read_pump(spawned.stdout);
        session.start_wait_task(child);
        session.start_stderr_pump(spawned.stderr);

        // Spawning → Initializing（§3.2）。
        session.apply_ev(SmEvent::Initialized);

        // initialize 握手（§2.1）。
        let init = InitializeParams {
            protocol_version: ab_protocol::PROTOCOL_VERSION,
            host_info: ab_protocol::types::HostInfo {
                name: "AnalysisBuddy".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };
        let handshake = session.initialize(init).await;
        let result = match handshake {
            Ok(result) => result,
            Err(e) => {
                let timed_out = matches!(&e, HostError::Transport(m) if m.contains("timed out"));
                if timed_out {
                    session.terminate_from(
                        SmEvent::HeartbeatMissed,
                        None,
                        HostError::process_exited(),
                    );
                } else {
                    session.terminate_from(
                        SmEvent::ProtocolFatalError,
                        None,
                        HostError::process_exited(),
                    );
                }
                return Err(e);
            }
        };
        if result.id != plugin.manifest.id {
            session.terminate_from(
                SmEvent::ProtocolFatalError,
                None,
                HostError::process_exited(),
            );
            return Err(HostError::Transport(format!(
                "initialize result id `{}` does not match manifest id `{}`",
                result.id, plugin.manifest.id
            )));
        }

        // Initializing → Ready。
        session.apply_ev(SmEvent::Initialized);
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .insert(plugin.manifest.id.clone(), session.clone());
        Ok(session)
    }

    /// 拉起或复用常驻进程（PLAN.md §3.3）。
    pub async fn get_or_spawn(&self, plugin_id: &str) -> Result<Arc<PluginSession>, HostError> {
        let _guard = self.spawn_lock.lock().await;
        if let Some(session) = self
            .sessions
            .lock()
            .expect("sessions lock poisoned")
            .get(plugin_id)
        {
            if session.is_live() {
                return Ok(session.clone());
            }
        }
        let plugin = self
            .registry
            .get(plugin_id)
            .ok_or_else(|| HostError::Transport(format!("plugin `{plugin_id}` not found")))?;
        self.spawn(&plugin).await
    }

    /// 全量停机（§5.2 会话关闭 + §9 第 5 条）。
    pub async fn shutdown_all(&self) {
        let sessions: Vec<Arc<PluginSession>> = self
            .sessions
            .lock()
            .expect("sessions lock poisoned")
            .values()
            .cloned()
            .collect();
        for session in sessions {
            let _ = session.shutdown().await;
        }
        self.children.sweep_orphans();
    }

    /// 孤儿兜底注册表（宿主析构时 sweep，§3.4 第 3 层）。
    pub fn children(&self) -> &ChildProcessRegistry {
        &self.children
    }
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        self.children.sweep_orphans();
    }
}

// 后台任务实现（spawn 后由会话启动；任务持有 Arc<PluginSession> 以调用终止流程）。
impl PluginSession {
    /// 写者任务：独占 stdin 整行原子写出（§9 第 4 条）；写失败 = BrokenPipe = 进程退出。
    fn start_writer_task(
        self: &Arc<Self>,
        mut stdin: tokio::process::ChildStdin,
        mut rx: mpsc::Receiver<String>,
    ) {
        let session = self.clone();
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if let Err(e) = stdin.write_all(frame.as_bytes()).await {
                    session.on_pipe_broken(HostError::Transport(format!(
                        "plugin stdin write failed: {e}"
                    )));
                    break;
                }
            }
            // rx 关闭（close_stdin / 会话丢弃）→ drop stdin → 子进程 EOF（§9 第 5 条）。
        });
    }

    /// 读泵任务：stdout 帧 → 路由/分流；结束（错误/EOF）通知会话。
    fn start_read_pump(self: &Arc<Self>, stdout: tokio::process::ChildStdout) {
        let session = self.clone();
        tokio::spawn(async move {
            let handler = SessionHandler {
                inner: session.inner.clone(),
            };
            if let Err(err) = run_read_loop(&session.inner.channel, stdout, handler).await {
                session.on_read_loop_error(err);
            }
        });
    }

    /// 等待任务：child.wait() 与 kill 通道二选一（§5.4 / §3.5 kill 兜底）。
    fn start_wait_task(self: &Arc<Self>, mut child: Child) {
        let (kill_tx, mut kill_rx) = mpsc::channel::<()>(1);
        let session = self.clone();
        *self.inner.kill_tx.lock().expect("kill lock poisoned") = Some(kill_tx.clone());
        self.inner.children.register(self.inner.pid, kill_tx);
        tokio::spawn(async move {
            let status = tokio::select! {
                s = child.wait() => s,
                _ = kill_rx.recv() => {
                    let _ = child.kill().await;
                    child.wait().await
                }
            };
            match status {
                Ok(status) => session.on_process_exit(Some(status)),
                Err(e) => {
                    eprintln!(
                        "ERROR ab-host: plugin {} wait failed: {e}",
                        session.plugin_id()
                    );
                    session.on_process_exit(None);
                }
            }
        });
    }

    /// stderr 泵（§6）：A-02 阶段仅转储；A-03 换成 `StderrSink` 环形缓冲 + 事件流。
    fn start_stderr_pump(self: &Arc<Self>, stderr: tokio::process::ChildStderr) {
        let session = self.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = line.trim_end_matches(['\r', '\n']);
                        eprintln!("[{}] {}", session.inner.plugin_id, line);
                    }
                }
            }
        });
    }

    /// 进程退出统一入口（§5.4）：读侧 Eof 与 wait() 互为印证，任一先触发即认定。
    /// 退出码 0 且处于 Draining = 正常 Shutdown，其余一律 Crashed ——
    /// 由状态机 `ExitConfirmed` 转移表裁决。
    fn on_process_exit(self: &Arc<Self>, status: Option<std::process::ExitStatus>) {
        let exit_code = status.as_ref().and_then(|s| s.code());
        self.terminate_from(
            SmEvent::ExitConfirmed,
            exit_code,
            HostError::process_exited(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_basic_transitions() {
        let mut sm = StateMachine::new();
        assert_eq!(sm.state(), PluginProcessState::Discovered);

        let seq: Vec<PluginProcessState> = [
            SmEvent::SpawnRequested,
            SmEvent::Initialized,
            SmEvent::Initialized,
            SmEvent::LoadStarted,
            SmEvent::LoadFinished,
            SmEvent::ParseStarted,
            SmEvent::ParseFinished,
            SmEvent::ShutdownRequested,
            SmEvent::ExitConfirmed,
        ]
        .iter()
        .filter_map(|ev| sm.apply(*ev))
        .collect();
        assert_eq!(
            seq,
            [
                PluginProcessState::Spawning,
                PluginProcessState::Initializing,
                PluginProcessState::Ready,
                PluginProcessState::Loading,
                PluginProcessState::Ready,
                PluginProcessState::Parsing,
                PluginProcessState::Ready,
                PluginProcessState::Draining,
                PluginProcessState::Shutdown,
            ]
        );

        // 吸收态：任何事件都返回 None。
        assert!(sm.apply(SmEvent::SpawnRequested).is_none());
        assert!(sm.apply(SmEvent::ExitConfirmed).is_none());
        assert_eq!(sm.state(), PluginProcessState::Shutdown);
    }

    #[test]
    fn illegal_transition_returns_none_and_keeps_state() {
        let mut sm = StateMachine::new();
        assert_eq!(
            sm.apply(SmEvent::Initialized),
            None,
            "Discovered+Initialized is illegal"
        );
        assert_eq!(sm.state(), PluginProcessState::Discovered);
        assert_eq!(
            sm.apply(SmEvent::ExitConfirmed),
            None,
            "Discovered+ExitConfirmed is illegal"
        );
        assert_eq!(sm.state(), PluginProcessState::Discovered);
    }
}
