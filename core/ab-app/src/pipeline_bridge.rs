//! P3-02 管线桥（pipeline.md §1/§4.2/§6 的 ab-app 组装点）。
//!
//! - [`ImportCoordinator`]：导入编排——manifest 预筛 → `can_handle` 扇出（3s
//!   弃权超时）→ 裁定 → `load_file`（自动重试）→ `schema` 缓存 → `parse_stream`
//!   → `freeze`，事件走 `ab_pipeline::PipelineEvent` 通道（pipeline.md §1.1）；
//! - [`query_key_values`]：按文件并发扇出（pipeline.md §4.2），单文件超时/失败
//!   独立返回，互不阻塞；
//! - [`FileIndex`]：`file_id → plugin_id` 映射（导入时建立，查询路由用）。
//!
//! 说明（与 pipeline.md §6 的偏差，均为 P3-02 在胶水侧落地时的必要补充）：
//! `ImportCoordinator::new` 在文档三参数之外追加 `host`（会话拉起能力）与
//! `discovery`（manifest 预筛数据源）——ab-pipeline 未实现该类型（其 import.rs
//! 注明「由 P3-02 合并卡在 ab-app 侧实现」），编排器必须能惰性拉起会话；
//! `SessionRegistry` 以 [`HostSessionAdapter`] 实例填充（pipeline.md §4.2），
//! 首次按 plugin_id 需要时经 `PluginRuntime::get_or_spawn` 拉起并缓存。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ab_host::{PluginRegistry, PluginRuntime};
use ab_pipeline::import::MatchCandidate;
use ab_pipeline::{ParseEvent, PipelineEvent, PluginSession, SessionError, SessionRegistry, Store};
use ab_protocol::types::{
    CanHandleParams, CancelParseParams, KeyValueEntry, KeyValuesParams, LoadFileParams, MetricDef,
    ParseParams, UnloadFileParams,
};
use tokio::sync::{mpsc, watch};

use crate::host_bridge::{map_host_error, HostSessionAdapter};

/// 编排配置（测试注入短超时/固定 file_id 用；生产取默认值）。
#[derive(Clone)]
pub struct PipelineConfig {
    /// 单插件 `can_handle` 弃权超时（protocol.md §6，默认 3s）。
    pub can_handle_timeout: Duration,
    /// 单文件 `key_values` 超时（protocol.md §6，默认 10s）。
    pub key_values_timeout: Duration,
    /// 单文件导入上界（pipeline.md §1.2，默认 100MB）。
    pub max_import_bytes: u64,
    /// file_id 生成器：`Fn(seq)`，缺省用 UUID v4 形随机 id。
    /// 测试注入固定 id 以对齐回放剧本内嵌的 file_id。
    pub file_id_fn: Option<Arc<dyn Fn(u64) -> String + Send + Sync>>,
    /// load_file 重试退避序列（P2-02/C2.5 语义锁定：总尝试
    /// `len+1` 次，第 i 次失败后按 `backoffs[i]` 退避；默认 1s/3s）。
    pub load_retry_backoffs: Vec<Duration>,
}

impl std::fmt::Debug for PipelineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineConfig")
            .field("can_handle_timeout", &self.can_handle_timeout)
            .field("key_values_timeout", &self.key_values_timeout)
            .field("max_import_bytes", &self.max_import_bytes)
            .field("file_id_fn", &self.file_id_fn.is_some())
            .field("load_retry_backoffs", &self.load_retry_backoffs)
            .finish()
    }
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            can_handle_timeout: Duration::from_secs(3),
            key_values_timeout: Duration::from_secs(10),
            max_import_bytes: 100 * 1024 * 1024,
            file_id_fn: None,
            load_retry_backoffs: vec![Duration::from_secs(1), Duration::from_secs(3)],
        }
    }
}

/// `file_id → plugin_id` 映射（pipeline.md §4.2 查询路由）。
#[derive(Default)]
pub struct FileIndex {
    inner: RwLock<HashMap<String, String>>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, file_id: &str, plugin_id: &str) {
        self.inner
            .write()
            .unwrap()
            .insert(file_id.to_string(), plugin_id.to_string());
    }

    pub fn remove(&self, file_id: &str) {
        self.inner.write().unwrap().remove(file_id);
    }

    pub fn get(&self, file_id: &str) -> Option<String> {
        self.inner.read().unwrap().get(file_id).cloned()
    }

    /// 某插件当前驻留的全部 file_id（`list_plugins.loaded_file_ids` 数据源，
    /// ipc-ui.md §1.1 / §2.3「该插件当前驻留的文件」）。
    pub fn files_of(&self, plugin_id: &str) -> Vec<String> {
        let mut files: Vec<String> = self
            .inner
            .read()
            .unwrap()
            .iter()
            .filter(|(_, p)| p.as_str() == plugin_id)
            .map(|(file_id, _)| file_id.clone())
            .collect();
        files.sort();
        files
    }
}

/// 单文件 key_values 查询结果（pipeline.md §4.2 / ipc-ui.md §1.6 部分失败协议）。
#[derive(Debug, Clone, PartialEq)]
pub struct KeyValuesOutcome {
    pub file_id: String,
    pub plugin_id: String,
    pub result: Result<Vec<KeyValueEntry>, KeyValuesError>,
}

/// key_values 路由层错误（pipeline.md §4.2 转换约定 + 路由本地错误）。
#[derive(Debug, Clone, PartialEq)]
pub enum KeyValuesError {
    /// 单插件 10s 看门狗超时（§4.2）。
    Timeout,
    /// `SessionError::Plugin` → 原样透传 code/message（§4.2）。
    PluginError(i32, String),
    /// 会话退出 / 传输层故障（`SessionError::SessionGone` 映射）。
    SessionGone,
    /// 文件未导入 / 未映射到插件（ipc-ui.md §1.6「文件未 ready」）。
    FileNotReady(String),
}

/// 单文件导入结果状态（ipc-ui.md §1.0 `ImportResult.status`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStatus {
    /// 已匹配待解析（needs_user_choice 场景，未 load/parse）。
    Matched,
    Parsing,
    Ready,
    Error,
}

/// 单文件导入失败（`IpcError.code` 取值域，ipc-ui.md §1.0 错误码表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError {
    pub code: &'static str,
    pub message: String,
}

/// `import_files` 单路径结果（ipc-ui.md §1.2：与入参同序；单路径失败不影响其余）。
#[derive(Debug, Clone)]
pub struct ImportOutcome {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub file_id: Option<String>,
    pub status: ImportStatus,
    /// 自动选中的最高置信度插件；needs_user_choice 时为 `None`。
    pub matched_plugin: Option<MatchCandidate>,
    /// 全部认领者（含 matched，按 confidence 降序）。
    pub candidate_plugins: Vec<MatchCandidate>,
    pub needs_user_choice: bool,
    pub error: Option<ImportError>,
}

impl ImportOutcome {
    pub(crate) fn failed(path: &str, size_bytes: u64, code: &'static str, message: String) -> Self {
        let name = Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        ImportOutcome {
            path: path.to_string(),
            name,
            size_bytes,
            file_id: None,
            status: ImportStatus::Error,
            matched_plugin: None,
            candidate_plugins: Vec::new(),
            needs_user_choice: false,
            error: Some(ImportError { code, message }),
        }
    }
}
/// 单文件导入 job（C2.2）：每 file_id 一个，注册于
/// [`ImportCoordinatorInner::jobs`]，parse task 与 `cancel_parse` 共享
/// （Arc 所有权）。状态转换（终态事件 / frozen 写入）由 job 串行化——
/// 取消后 parse task 在关键转换点退出，不会把状态改回 ParseFailed/Ready
/// （P1-02 竞争消除；C2.2 规则 3/5）。
struct ImportJob {
    file_id: String,
    /// 每次 import 递增（本 job 生命周期内单调；诊断用，C2.2）。
    generation: u64,
    /// 取消请求已置位（cancel_parse / unload_file 并发路径）。
    cancelled: AtomicBool,
    /// parse task 结束通知（join 用）。以 watch 通道承载而非 `Notify`：
    /// `notify_waiters` 只唤醒已 armed 的 `notified()`，晚到订阅者漏唤醒
    /// 会导致 cancel 空等 10s 超时；watch 的"未读数"语义对任意晚到订阅
    /// 者都立即可读，无竞态（C2.2 规则 2）。
    done: watch::Sender<bool>,
    done_rx: watch::Receiver<bool>,
    /// 终态清理只执行一次的交换标记（C2.2 规则 4：cancel 或 task 唯一一方）。
    cleaned_up: AtomicBool,
    /// 诊断（P1-01/C2.4）：接收/丢弃/记录计数（W2-E 消费）。
    received_batches: AtomicU64,
    dropped_batches: AtomicU64,
    received_records: AtomicU64,
}

/// 单文件导入 job 诊断快照（C2.4：append 计数写入 job 诊断字段；
/// 活跃 job 与终态后快照均可读，W2-E 结构化诊断消费）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobDiagnostics {
    pub generation: u64,
    pub received_batches: u64,
    pub dropped_batches: u64,
    pub received_records: u64,
}

/// 编排器内部态（外层 Clone 句柄，便于按文件扇出 task）。
struct ImportCoordinatorInner {
    store: Arc<Store>,
    registry: Arc<SessionRegistry>,
    events: mpsc::UnboundedSender<PipelineEvent>,
    host: Arc<PluginRuntime>,
    discovery: Arc<PluginRegistry>,
    config: PipelineConfig,
    file_index: Arc<FileIndex>,
    /// ParseCompleted → 可查询（ipc-ui.md §2.1 command 状态翻转）。
    frozen: RwLock<HashSet<String>>,
    /// plugin_id → schema().metrics（get_metrics 节点构造缓存）。
    schema_cache: RwLock<HashMap<String, Vec<MetricDef>>>,
    /// plugin_id → 宿主会话句柄（reload_session 停机用；与 SessionRegistry
    /// 中适配器一一对应，创建于 ensure_session/probe_plugin）。
    host_sessions: Arc<RwLock<HashMap<String, Arc<ab_host::PluginSession>>>>,
    /// plugin_id → HostSessionAdapter（C2.4 dropped 计数读取；与
    /// host_sessions 同时填充，仅本层可解引用 trait 背后的具体类型）。
    adapters: Arc<RwLock<HashMap<String, Arc<HostSessionAdapter>>>>,
    /// file_id → 活跃导入 job（C2.2；cancel_parse/unload_file 查询入口）。
    jobs: RwLock<HashMap<String, Arc<ImportJob>>>,
    /// file_id → 终态 job 诊断快照（finish_job 留存；W2-E 完成/失败/取消
    /// 路径与诊断查询在 job 注销后仍可读）。
    last_diagnostics: RwLock<HashMap<String, JobDiagnostics>>,
    /// file_id → 源路径（get_metrics 文件节点名用）。
    paths: RwLock<HashMap<String, String>>,
    /// 同插件 load+parse 串行锁（pipeline.md §1 并发约束）。
    plugin_locks: RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    file_seq: AtomicU64,
    /// job generation 分配器（每次注册递增）。
    job_generation: AtomicU64,
}

/// 导入编排器（pipeline.md §6）。
#[derive(Clone)]
pub struct ImportCoordinator {
    inner: Arc<ImportCoordinatorInner>,
}

impl ImportCoordinator {
    /// 组装点（pipeline.md §6 签名的 ab-app 落地）：`host` 提供会话拉起能力，
    /// `discovery` 提供 manifest 预筛数据源（见模块头注）。
    pub fn new(
        store: Arc<Store>,
        registry: Arc<SessionRegistry>,
        events: mpsc::UnboundedSender<PipelineEvent>,
        host: Arc<PluginRuntime>,
        discovery: Arc<PluginRegistry>,
    ) -> Self {
        Self::with_config(
            store,
            registry,
            events,
            host,
            discovery,
            PipelineConfig::default(),
        )
    }

    /// 带注入配置（测试用）。
    pub fn with_config(
        store: Arc<Store>,
        registry: Arc<SessionRegistry>,
        events: mpsc::UnboundedSender<PipelineEvent>,
        host: Arc<PluginRuntime>,
        discovery: Arc<PluginRegistry>,
        config: PipelineConfig,
    ) -> Self {
        Self {
            inner: Arc::new(ImportCoordinatorInner {
                store,
                registry,
                events,
                host,
                discovery,
                config,
                file_index: Arc::new(FileIndex::new()),
                frozen: RwLock::new(HashSet::new()),
                schema_cache: RwLock::new(HashMap::new()),
                host_sessions: Arc::new(RwLock::new(HashMap::new())),
                adapters: Arc::new(RwLock::new(HashMap::new())),
                jobs: RwLock::new(HashMap::new()),
                last_diagnostics: RwLock::new(HashMap::new()),
                paths: RwLock::new(HashMap::new()),
                plugin_locks: RwLock::new(HashMap::new()),
                file_seq: AtomicU64::new(0),
                job_generation: AtomicU64::new(0),
            }),
        }
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.inner.store
    }

    pub fn registry(&self) -> &Arc<SessionRegistry> {
        &self.inner.registry
    }

    pub fn file_index(&self) -> &Arc<FileIndex> {
        &self.inner.file_index
    }

    pub fn key_values_timeout(&self) -> Duration {
        self.inner.config.key_values_timeout
    }

    /// 当前可查询（Frozen）文件（get_metrics 默认入参；ipc-ui.md §1.4）。
    pub fn list_frozen(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.frozen.read().unwrap().iter().cloned().collect();
        ids.sort();
        ids
    }

    /// 插件指标定义缓存（schema 拉取于导入时；未缓存返回空）。
    pub fn schema_metrics(&self, plugin_id: &str) -> Vec<MetricDef> {
        self.inner
            .schema_cache
            .read()
            .unwrap()
            .get(plugin_id)
            .cloned()
            .unwrap_or_default()
    }

    /// 文件源路径（导入时记录；未知返回 `None`）。
    pub fn path_of(&self, file_id: &str) -> Option<String> {
        self.inner.paths.read().unwrap().get(file_id).cloned()
    }

    /// 插件展示名（manifest.display_name；未发现回落 plugin_id）。
    pub fn plugin_display_name(&self, plugin_id: &str) -> String {
        self.inner
            .discovery
            .get(plugin_id)
            .map(|p| p.manifest.display_name)
            .unwrap_or_else(|| plugin_id.to_string())
    }

    /// 批量导入：每文件一个 task 扇出（pipeline.md §1「不同插件之间并行」），
    /// 返回与入参同序的结果；单路径失败不影响其余（ipc-ui.md §1.2）。
    pub async fn import_files(&self, paths: &[PathBuf]) -> Vec<ImportOutcome> {
        let mut handles = Vec::with_capacity(paths.len());
        for path in paths {
            let me = self.clone();
            let path = path.clone();
            handles.push((
                path.clone(),
                tokio::spawn(async move { me.inner.import_one(&path, None, false).await }),
            ));
        }
        let mut outcomes = Vec::with_capacity(handles.len());
        for (path, handle) in handles {
            match handle.await {
                Ok(outcome) => outcomes.push(outcome),
                Err(e) => outcomes.push(ImportOutcome::failed(
                    &path.display().to_string(),
                    0,
                    "internal",
                    format!("import task panicked: {e}"),
                )),
            }
        }
        outcomes
    }

    /// 用户手选覆盖入口（ipc-ui.md §1.2：命中 overrides 的路径跳过自动匹配）。
    pub async fn import_with_plugin(&self, path: PathBuf, plugin_id: &str) -> ImportOutcome {
        self.inner.import_one(&path, Some(plugin_id), false).await
    }

    /// 卸载文件：会话 `unload_file`（3s 超时按完成）→ store 释放 → 状态移除
    /// （ipc-ui.md §1.3；幂等：未知 file_id 视为成功）。
    ///
    /// 与取消并发（C2.2 规则 5）：置 cancelled 并注销 job 注册——进行中的
    /// parse task 在下一关键状态转换点退出，不会把状态改回 ParseFailed/Ready
    /// （状态转换由 job 所有权串行化，卸载后无旧 task 倒退）。
    pub async fn unload_file(&self, file_id: &str) {
        let Some(plugin_id) = self.inner.file_index.get(file_id) else {
            return;
        };
        // 注意：读守卫必须显式结束（作用域块），否则 if-let 临时变量把读锁
        // 保持到体内 jobs.write() → std RwLock 非重入 → 自死锁。
        let active_job = {
            let jobs = self.inner.jobs.read().unwrap();
            jobs.get(file_id).cloned()
        };
        if let Some(job) = active_job {
            job.cancelled.store(true, Ordering::SeqCst);
            self.inner.jobs.write().unwrap().remove(file_id);
        }
        if let Some(session) = self.inner.registry.get(&plugin_id) {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                session.unload_file(UnloadFileParams {
                    file_id: file_id.to_string(),
                }),
            )
            .await;
        }
        self.inner.store.unload(file_id);
        self.inner.frozen.write().unwrap().remove(file_id);
        self.inner.paths.write().unwrap().remove(file_id);
        self.inner.file_index.remove(file_id);
        self.emit(PipelineEvent::FileUnloaded {
            file_id: file_id.to_string(),
        });
    }

    /// 取消 parse（C2.2 规则 2）：置 cancelled → 调插件 `cancel_parse`（现有
    /// 逻辑）→ 等待该 job 的 parse task 结束（10s 超时按完成）→ 唯一一方
    /// 丢弃半成品（store.unload/移除 frozen/paths/file_index）→ 发
    /// `PipelineEvent::ParseCancelled`。幂等：未知 file_id（无活跃 job）或
    /// 已终态（job 已注销）直接返回（C2.1）。
    pub async fn cancel_parse(&self, file_id: &str) {
        let Some(job) = self.inner.jobs.read().unwrap().get(file_id).cloned() else {
            return;
        };
        job.cancelled.store(true, Ordering::SeqCst);
        if let Some(plugin_id) = self.inner.file_index.get(file_id) {
            if let Some(session) = self.inner.registry.get(&plugin_id) {
                let _ = session
                    .cancel_parse(CancelParseParams {
                        file_id: file_id.to_string(),
                    })
                    .await;
            }
        }
        // 等待该 job 的 parse task 结束（watch 通道对任意晚到订阅者立即可读，
        // 无漏唤醒竞态；10s 超时按完成继续——超时后由 swap 保证仅清理一次）。
        let mut done = job.done_rx.clone();
        let wait_done = async {
            if !*done.borrow() {
                let _ = done.changed().await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(10), wait_done).await;
        if !job.cleaned_up.swap(true, Ordering::SeqCst) {
            self.inner.discard_job_state(file_id);
        }
        self.inner.jobs.write().unwrap().remove(file_id);
        self.emit(PipelineEvent::ParseCancelled {
            file_id: file_id.to_string(),
        });
    }

    /// 活跃 job 诊断（C2.4：append 计数；W2-E 消费）。file_id 未知返回
    /// `None`；已终态（job 注销）返回 finish 时留存的快照。
    pub fn job_diagnostics(&self, file_id: &str) -> Option<JobDiagnostics> {
        if let Some(job) = self.inner.jobs.read().unwrap().get(file_id) {
            return Some(JobDiagnostics {
                generation: job.generation,
                received_batches: job.received_batches.load(Ordering::Relaxed),
                dropped_batches: job.dropped_batches.load(Ordering::Relaxed),
                received_records: job.received_records.load(Ordering::Relaxed),
            });
        }
        self.inner
            .last_diagnostics
            .read()
            .unwrap()
            .get(file_id)
            .copied()
    }

    /// 终止插件全部宿主会话（卸载前置清理，spec §4.4）：host_sessions 表
    /// 移除 → 会话级 shutdown（§5.2：shutdown → 3s 预算 → kill）。进程终止
    /// 后插件目录不再被 CWD 句柄占用，卸载可立即删目录（否则 Windows 上
    /// `remove_dir_all` 以 `module_in_use` 失败）。
    pub async fn shutdown_plugin_sessions(&self, plugin_id: &str) {
        self.stop_host_session(plugin_id).await;
    }

    /// 重建插件实例（ipc-ui.md §4.6 `reload_plugin` 语义）：停掉旧会话 →
    /// 经宿主拉起新实例 → 注册表替换为新适配器（新实例自带全新状态机与
    /// stderr 缓冲，host-runtime.md §5.2 重建实例）。
    pub async fn reload_session(&self, plugin_id: &str) -> Result<(), SessionError> {
        self.stop_host_session(plugin_id).await;
        let session = self
            .inner
            .host
            .get_or_spawn(plugin_id)
            .await
            .map_err(map_host_error)?;
        let adapter: Arc<dyn ab_pipeline::PluginSession> =
            Arc::new(HostSessionAdapter::new(session.clone()));
        self.inner.registry.register(adapter);
        self.inner
            .host_sessions
            .write()
            .unwrap()
            .insert(plugin_id.to_string(), session);
        Ok(())
    }

    /// 单实例停机（`reload_session` 旧实例与 [`Self::shutdown_plugin_sessions`]
    /// 共用）：host_sessions 表移除 → 会话 shutdown（终止进程）。
    async fn stop_host_session(&self, plugin_id: &str) {
        let old = self.inner.host_sessions.write().unwrap().remove(plugin_id);
        if let Some(old) = old {
            let _ = old.shutdown().await;
        }
    }

    /// 会话重开单文件（pipeline.md §5.3 步骤 3）：按会话记录 `plugin_id`
    /// 直连会话（跳过 can_handle 自动匹配）重走 load → parse → freeze 全流程。
    /// 与 `import_with_plugin` 的差别：parse 完成后补发一条 `percent:100`
    /// 的 `ab://progress`——前端重开流程以「percent≥100」将占位条目翻为
    /// `ready`（ui/src/state/session.ts 订阅逻辑），而 §2.1 规定常规导入
    /// 不发终态事件，故仅本路径补发。
    pub async fn reopen_file(&self, path: PathBuf, plugin_id: &str) -> ImportOutcome {
        self.inner.import_one(&path, Some(plugin_id), true).await
    }

    fn emit(&self, event: PipelineEvent) {
        let _ = self.inner.events.send(event);
    }
}
impl ImportCoordinatorInner {
    fn emit(&self, event: PipelineEvent) {
        let _ = self.events.send(event);
    }

    /// 注册导入 job（C2.2 规则 1：parse 前；generation 递增）。
    fn register_job(&self, file_id: &str) -> Arc<ImportJob> {
        let generation = self.job_generation.fetch_add(1, Ordering::Relaxed);
        let (done_tx, done_rx) = watch::channel(false);
        let job = Arc::new(ImportJob {
            file_id: file_id.to_string(),
            generation,
            cancelled: AtomicBool::new(false),
            done: done_tx,
            done_rx,
            cleaned_up: AtomicBool::new(false),
            received_batches: AtomicU64::new(0),
            dropped_batches: AtomicU64::new(0),
            received_records: AtomicU64::new(0),
        });
        self.jobs
            .write()
            .unwrap()
            .insert(file_id.to_string(), job.clone());
        job
    }

    /// 注册后所有出口必经（C2.2 规则 1）：置 finished → 通知 done → 注销注册
    /// → 诊断快照留存；已取消且尚未清理时，由本方执行唯一一次半成品丢弃
    /// （cleaned_up 原子交换保证，C2.2 规则 4）。
    fn finish_job(&self, job: &Arc<ImportJob>) {
        let _ = job.done.send(true);
        self.jobs.write().unwrap().remove(&job.file_id);
        self.last_diagnostics.write().unwrap().insert(
            job.file_id.clone(),
            JobDiagnostics {
                generation: job.generation,
                received_batches: job.received_batches.load(Ordering::Relaxed),
                dropped_batches: job.dropped_batches.load(Ordering::Relaxed),
                received_records: job.received_records.load(Ordering::Relaxed),
            },
        );
        if job.cancelled.load(Ordering::SeqCst) && !job.cleaned_up.swap(true, Ordering::SeqCst) {
            self.discard_job_state(&job.file_id);
        }
    }

    /// 取消后的半成品丢弃（store/索引/状态条目；幂等，C2.2 规则 4 唯一一方）。
    fn discard_job_state(&self, file_id: &str) {
        self.store.unload(file_id);
        self.frozen.write().unwrap().remove(file_id);
        self.paths.write().unwrap().remove(file_id);
        self.file_index.remove(file_id);
    }

    /// 已取消时的静默退出（C2.2 规则 3）：不发终态事件、不写 frozen；
    /// 清理由 finish_job/取消方经 swap 唯一执行。
    fn bail_cancelled(&self, job: &Arc<ImportJob>, path: &str, size_bytes: u64) -> ImportOutcome {
        self.finish_job(job);
        ImportOutcome::failed(path, size_bytes, "cancelled", "parse cancelled".to_string())
    }

    /// 惰性会话：registry 命中直接复用，否则拉起并缓存
    /// （SessionRegistry 以 HostSessionAdapter 实例填充，pipeline.md §4.2）。
    async fn ensure_session(
        &self,
        plugin_id: &str,
    ) -> Result<Arc<dyn ab_pipeline::PluginSession>, SessionError> {
        if let Some(session) = self.registry.get(plugin_id) {
            return Ok(session);
        }
        let session = self
            .host
            .get_or_spawn(plugin_id)
            .await
            .map_err(map_host_error)?;
        let adapter = Arc::new(HostSessionAdapter::new(session.clone()));
        self.registry.register(adapter.clone());
        self.adapters
            .write()
            .unwrap()
            .insert(plugin_id.to_string(), adapter.clone());
        self.host_sessions
            .write()
            .unwrap()
            .insert(plugin_id.to_string(), session);
        Ok(adapter)
    }

    async fn import_one(
        &self,
        path: &Path,
        override_plugin: Option<&str>,
        emit_final_progress: bool,
    ) -> ImportOutcome {
        let path_str = path.display().to_string();
        self.emit(PipelineEvent::ImportStarted {
            path: path_str.clone(),
        });

        let info = match read_file_info(path, self.config.max_import_bytes) {
            Ok(info) => info,
            Err((code, message)) => {
                self.emit(PipelineEvent::ImportFailed {
                    path: path_str.clone(),
                    reason: message.clone(),
                });
                return ImportOutcome::failed(&path_str, 0, code, message);
            }
        };

        // 匹配（pipeline.md §1.1）：manifest 预筛 → can_handle 扇出 → 裁定。
        let (chosen, candidates) = if let Some(plugin_id) = override_plugin {
            let plugin_id = plugin_id.to_string();
            self.emit(PipelineEvent::PluginSelected {
                path: path_str.clone(),
                plugin_id: plugin_id.clone(),
                by: "user",
            });
            (
                plugin_id.clone(),
                vec![MatchCandidate {
                    plugin_id,
                    confidence: 1.0,
                    reason: Some("user override".to_string()),
                }],
            )
        } else {
            let candidates = self.match_file(path, &info).await;
            let needs_choice = needs_user_choice(&candidates);
            self.emit(PipelineEvent::MatchCandidates {
                path: path_str.clone(),
                candidates: candidates.clone(),
                needs_user_choice: needs_choice,
            });
            if needs_choice {
                // 零候选：列出全部已发现插件供手选（ipc-ui.md §1.2）。
                let picker = if candidates.is_empty() {
                    self.discovery
                        .list()
                        .iter()
                        .map(|p| MatchCandidate {
                            plugin_id: p.manifest.id.clone(),
                            confidence: 0.0,
                            reason: None,
                        })
                        .collect()
                } else {
                    candidates.clone()
                };
                return ImportOutcome {
                    path: path_str,
                    name: info.name,
                    size_bytes: info.size_bytes,
                    file_id: None,
                    status: ImportStatus::Matched,
                    matched_plugin: None,
                    candidate_plugins: picker,
                    needs_user_choice: true,
                    error: None,
                };
            }
            let top = candidates[0].clone();
            self.emit(PipelineEvent::PluginSelected {
                path: path_str.clone(),
                plugin_id: top.plugin_id.clone(),
                by: "auto",
            });
            (top.plugin_id, candidates)
        };

        // 同插件串行：load+parse 持锁（pipeline.md §1 并发约束）。
        let lock = self.plugin_lock(&chosen).clone();
        let _guard = lock.lock().await;

        let session = match self.ensure_session(&chosen).await {
            Ok(session) => session,
            Err(e) => {
                self.emit(PipelineEvent::ImportFailed {
                    path: path_str.clone(),
                    reason: format!("session for plugin `{chosen}` unavailable: {e}"),
                });
                return ImportOutcome::failed(
                    &path_str,
                    info.size_bytes,
                    "internal",
                    format!("session for plugin `{chosen}` unavailable: {e}"),
                );
            }
        };

        let file_id = self.next_file_id();
        self.file_index.insert(&file_id, &chosen);
        // C2.2 规则 1：parse 前注册 job（load 阶段起即可被 cancel 观察到）；
        // 注册后全部出口必经 finish_job（注销 + done 通知 + 取消清理判定）。
        let job = self.register_job(&file_id);

        // load_file：按 pipeline.md §1.2 自动重试（P2-02/C2.5 语义锁定——
        // 总尝试 3 次：初始 + 重试 2 次，第 1、2 次失败后按退避序列 1s/3s；
        // 注释与实现一致，默认值由测试锁定）。
        let load_params = LoadFileParams {
            file_id: file_id.clone(),
            path: path_str.clone(),
        };
        let mut summary = None;
        let mut last_load_error = None;
        let total_attempts = self.config.load_retry_backoffs.len() + 1;
        for attempt in 0..total_attempts {
            match session.load_file(load_params.clone()).await {
                Ok(value) => {
                    summary = Some(value);
                    break;
                }
                Err(e) => {
                    last_load_error = Some(e);
                    // 已取消：不再空耗重试退避，静默退出。
                    if job.cancelled.load(Ordering::SeqCst) {
                        return self.bail_cancelled(&job, &path_str, info.size_bytes);
                    }
                    if let Some(backoff) = self.config.load_retry_backoffs.get(attempt) {
                        tokio::time::sleep(*backoff).await;
                    }
                }
            }
        }
        let Some(summary) = summary else {
            let e = last_load_error.expect("at least one attempt");
            if job.cancelled.load(Ordering::SeqCst) {
                return self.bail_cancelled(&job, &path_str, info.size_bytes);
            }
            let (code, message) = load_error_code(&e);
            self.emit(PipelineEvent::FileLoadFailed {
                file_id: file_id.clone(),
                message: message.clone(),
            });
            self.file_index.remove(&file_id);
            self.finish_job(&job);
            return ImportOutcome::failed(&path_str, info.size_bytes, code, message);
        };
        self.emit(PipelineEvent::FileLoaded {
            file_id: file_id.clone(),
            summary: Some(summary.clone()),
        });

        // schema → 白名单 + 缓存（get_metrics 节点构造用，pipeline.md §1.1）。
        let schema = match session.schema().await {
            Ok(schema) => schema,
            Err(e) => {
                if job.cancelled.load(Ordering::SeqCst) {
                    return self.bail_cancelled(&job, &path_str, info.size_bytes);
                }
                self.emit(PipelineEvent::ParseFailed {
                    file_id: file_id.clone(),
                    reason: "schema_error".to_string(),
                    detail: Some(e.to_string()),
                });
                self.file_index.remove(&file_id);
                self.finish_job(&job);
                return ImportOutcome::failed(
                    &path_str,
                    info.size_bytes,
                    "internal",
                    format!("schema failed for plugin `{chosen}`: {e}"),
                );
            }
        };
        self.schema_cache
            .write()
            .unwrap()
            .insert(chosen.clone(), schema.metrics.clone());
        let whitelist: Vec<String> = schema.metrics.iter().map(|m| m.id.clone()).collect();
        if let Err(e) = self.store.register(&file_id, Some(summary), &whitelist) {
            if job.cancelled.load(Ordering::SeqCst) {
                return self.bail_cancelled(&job, &path_str, info.size_bytes);
            }
            self.emit(PipelineEvent::ParseFailed {
                file_id: file_id.clone(),
                reason: "internal".to_string(),
                detail: Some(e.to_string()),
            });
            self.file_index.remove(&file_id);
            self.finish_job(&job);
            return ImportOutcome::failed(
                &path_str,
                info.size_bytes,
                "internal",
                format!("store.register failed: {e}"),
            );
        }
        self.paths
            .write()
            .unwrap()
            .insert(file_id.clone(), path_str.clone());

        // parse 流式编排（时序同 pipeline.md §1.1；错误分支同 §1.2 表）。
        let (tx, mut rx) = mpsc::channel::<ParseEvent>(256);
        let sink_store = self.store.clone();
        let sink_file_id = file_id.clone();
        let sink_events = self.events.clone();
        let sink_job = job.clone();
        let sink_task = tokio::spawn(async move {
            let mut append_error: Option<String> = None;
            while let Some(event) = rx.recv().await {
                match event {
                    ParseEvent::Batch(batch) => {
                        // C2.4：sink 侧接收计数（append 成败与否都算已接收）。
                        sink_job.received_batches.fetch_add(1, Ordering::Relaxed);
                        sink_job
                            .received_records
                            .fetch_add(batch.records.len() as u64, Ordering::Relaxed);
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
        let parse_result = session
            .parse_stream(
                ParseParams {
                    file_id: file_id.clone(),
                    options: None,
                },
                tx,
            )
            .await;
        let append_error = match sink_task.await {
            Ok(error) => error,
            Err(_) => Some("parse sink task panicked".to_string()),
        };

        let records_total = match parse_result {
            Ok(total) => total,
            Err(e) => {
                if job.cancelled.load(Ordering::SeqCst) {
                    return self.bail_cancelled(&job, &path_str, info.size_bytes);
                }
                self.store.unload(&file_id);
                self.file_index.remove(&file_id);
                self.paths.write().unwrap().remove(&file_id);
                self.emit(PipelineEvent::ParseFailed {
                    file_id: file_id.clone(),
                    reason: "plugin_error".to_string(),
                    detail: Some(e.to_string()),
                });
                self.finish_job(&job);
                let (code, message) = load_error_code(&e);
                return ImportOutcome::failed(&path_str, info.size_bytes, code, message);
            }
        };
        if let Some(err) = append_error {
            if job.cancelled.load(Ordering::SeqCst) {
                return self.bail_cancelled(&job, &path_str, info.size_bytes);
            }
            self.store.unload(&file_id);
            self.file_index.remove(&file_id);
            self.paths.write().unwrap().remove(&file_id);
            self.emit(PipelineEvent::ParseFailed {
                file_id: file_id.clone(),
                reason: "protocol_error".to_string(),
                detail: Some(err.clone()),
            });
            self.finish_job(&job);
            return ImportOutcome::failed(
                &path_str,
                info.size_bytes,
                "internal",
                format!("store append failed: {err}"),
            );
        }
        // C2.4 计数诊断：dropped 增量取适配器最近一次 parse 的记录（同插件
        // load+parse 由 plugin_locks 串行化，读取安全；未跟踪的 mock 会话
        // 按 0 处理）；接收计数已由 sink task 累计进 job。
        let dropped_batches = self
            .adapters
            .read()
            .unwrap()
            .get(&chosen)
            .map(|a| a.last_parse_dropped())
            .unwrap_or(0);
        job.dropped_batches
            .store(dropped_batches, Ordering::Relaxed);
        let received_records = job.received_records.load(Ordering::Relaxed);
        if let Some((code, reason)) =
            lost_batch_error(records_total, received_records, dropped_batches)
        {
            if job.cancelled.load(Ordering::SeqCst) {
                return self.bail_cancelled(&job, &path_str, info.size_bytes);
            }
            self.store.unload(&file_id);
            self.file_index.remove(&file_id);
            self.paths.write().unwrap().remove(&file_id);
            self.emit(PipelineEvent::ParseFailed {
                file_id: file_id.clone(),
                reason: reason.to_string(),
                detail: Some(format!(
                    "records_total={records_total}, received={received_records}, dropped_batches={dropped_batches}"
                )),
            });
            self.finish_job(&job);
            return ImportOutcome::failed(
                &path_str,
                info.size_bytes,
                code,
                format!("parse records lost due to backpressure: {reason}"),
            );
        }
        if let Err(e) = self.store.freeze(&file_id, records_total) {
            if job.cancelled.load(Ordering::SeqCst) {
                return self.bail_cancelled(&job, &path_str, info.size_bytes);
            }
            self.store.unload(&file_id);
            self.file_index.remove(&file_id);
            self.paths.write().unwrap().remove(&file_id);
            self.emit(PipelineEvent::ParseFailed {
                file_id: file_id.clone(),
                reason: "count_mismatch".to_string(),
                detail: Some(e.to_string()),
            });
            self.finish_job(&job);
            return ImportOutcome::failed(
                &path_str,
                info.size_bytes,
                "parse_failed",
                format!("freeze failed: {e}"),
            );
        }
        // C2.2 规则 3：Ready 前最后一道取消门——取消与完成竞态下回滚
        // frozen（swap 清理为幂等兜底），不发终态事件、不写 frozen。
        if job.cancelled.load(Ordering::SeqCst) {
            self.frozen.write().unwrap().remove(&file_id);
            return self.bail_cancelled(&job, &path_str, info.size_bytes);
        }
        self.frozen.write().unwrap().insert(file_id.clone());
        self.emit(PipelineEvent::ParseCompleted {
            file_id: file_id.clone(),
            records_total,
            warnings: self.store.warnings(&file_id).unwrap_or_default(),
        });
        self.emit(PipelineEvent::QueryReady {
            file_id: file_id.clone(),
        });
        if emit_final_progress {
            // 会话重开路径补发终态进度（见 reopen_file 注释；§2.1 常规导入不发）。
            self.emit(PipelineEvent::ParseProgress {
                file_id: file_id.clone(),
                percent: Some(100.0),
                records_so_far: records_total,
            });
        }
        self.finish_job(&job);

        ImportOutcome {
            path: path_str,
            name: info.name,
            size_bytes: info.size_bytes,
            file_id: Some(file_id),
            status: ImportStatus::Ready,
            matched_plugin: candidates.first().cloned(),
            candidate_plugins: candidates,
            needs_user_choice: false,
            error: None,
        }
    }

    /// manifest 预筛 → 逐候选 `can_handle` 扇出（跨插件并行）→
    /// confidence 降序、平手 plugin_id 字典序（pipeline.md §1.1/§6 matcher）。
    async fn match_file(&self, path: &Path, info: &FileInfo) -> Vec<MatchCandidate> {
        let params = CanHandleParams {
            path: path.display().to_string(),
            name: info.name.clone(),
            ext: info.ext.clone(),
            size_bytes: info.size_bytes,
            head_sample: info.head_sample.clone(),
        };
        let prefiltered: Vec<String> = self
            .discovery
            .list()
            .iter()
            .filter(|p| manifest_prefilter(&p.manifest, info))
            .map(|p| p.manifest.id.clone())
            .collect();
        let mut tasks = Vec::with_capacity(prefiltered.len());
        for plugin_id in &prefiltered {
            let plugin_id = plugin_id.clone();
            let params = params.clone();
            let host = self.host.clone();
            let registry = self.registry.clone();
            let host_sessions = self.host_sessions.clone();
            let adapters = self.adapters.clone();
            let timeout = self.config.can_handle_timeout;
            tasks.push(tokio::spawn(async move {
                probe_plugin(
                    host,
                    registry,
                    host_sessions,
                    adapters,
                    timeout,
                    &plugin_id,
                    &params,
                )
                .await
            }));
        }
        let mut candidates = Vec::new();
        for task in tasks {
            if let Ok(Some(candidate)) = task.await {
                candidates.push(candidate);
            }
        }
        candidates.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| a.plugin_id.cmp(&b.plugin_id))
        });
        candidates
    }

    fn plugin_lock(&self, plugin_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        if let Some(lock) = self.plugin_locks.read().unwrap().get(plugin_id) {
            return lock.clone();
        }
        self.plugin_locks
            .write()
            .unwrap()
            .entry(plugin_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn next_file_id(&self) -> String {
        let seq = self.file_seq.fetch_add(1, Ordering::Relaxed);
        match &self.config.file_id_fn {
            Some(f) => f(seq),
            None => default_file_id(seq),
        }
    }
}
/// 文件元信息（pipeline.md §1.2 头部采样）。
struct FileInfo {
    name: String,
    ext: String,
    size_bytes: u64,
    head_sample: String,
}

/// 读取文件元信息：不存在/不可读 → `file_not_found`；超过上界 → `invalid_arg`
/// （pipeline.md §1.2：拒绝导入，不进入匹配）。
fn read_file_info(path: &Path, max_bytes: u64) -> Result<FileInfo, (&'static str, String)> {
    let metadata = fs::metadata(path)
        .map_err(|e| ("file_not_found", format!("cannot read file metadata: {e}")))?;
    if !metadata.is_file() {
        return Err(("file_not_found", "not a regular file".to_string()));
    }
    let size_bytes = metadata.len();
    if size_bytes > max_bytes {
        return Err((
            "invalid_arg",
            format!("file exceeds import limit of {max_bytes} bytes"),
        ));
    }
    let mut head = [0u8; 4096];
    let mut file =
        fs::File::open(path).map_err(|e| ("file_not_found", format!("cannot open file: {e}")))?;
    let n = file
        .read(&mut head)
        .map_err(|e| ("file_not_found", format!("cannot read file head: {e}")))?;
    let head_sample = String::from_utf8_lossy(&head[..n]).into_owned();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    Ok(FileInfo {
        name,
        ext,
        size_bytes,
        head_sample,
    })
}

/// manifest 预筛（protocol.md §7.2）：扩展名命中或头指纹大小写不敏感子串命中。
fn manifest_prefilter(manifest: &ab_protocol::manifest::Manifest, info: &FileInfo) -> bool {
    if manifest
        .r#match
        .extensions
        .iter()
        .any(|e| e.eq_ignore_ascii_case(&info.ext))
    {
        return true;
    }
    if let Some(fingerprints) = &manifest.r#match.header_fingerprints {
        let lower = info.head_sample.to_ascii_lowercase();
        if fingerprints
            .iter()
            .any(|fp| lower.contains(&fp.to_ascii_lowercase()))
        {
            return true;
        }
    }
    false
}

/// 单候选探测：拉起失败弃权、超时弃权（pipeline.md §1.2）。
#[allow(clippy::too_many_arguments)]
async fn probe_plugin(
    host: Arc<PluginRuntime>,
    registry: Arc<SessionRegistry>,
    host_sessions: Arc<RwLock<HashMap<String, Arc<ab_host::PluginSession>>>>,
    adapters: Arc<RwLock<HashMap<String, Arc<HostSessionAdapter>>>>,
    timeout: Duration,
    plugin_id: &str,
    params: &CanHandleParams,
) -> Option<MatchCandidate> {
    let session = match host.get_or_spawn(plugin_id).await {
        Ok(session) => {
            let adapter = Arc::new(HostSessionAdapter::new(session.clone()));
            registry.register(adapter.clone());
            adapters
                .write()
                .unwrap()
                .insert(plugin_id.to_string(), adapter.clone());
            host_sessions
                .write()
                .unwrap()
                .insert(plugin_id.to_string(), session);
            adapter
        }
        Err(_) => return None,
    };
    match tokio::time::timeout(timeout, session.can_handle(params.clone())).await {
        Ok(Ok(result)) if result.can_handle => Some(MatchCandidate {
            plugin_id: plugin_id.to_string(),
            confidence: result.confidence,
            reason: result.reason,
        }),
        _ => None,
    }
}

/// 置信度差 < 0.1 或候选为空 → 需用户手选（pipeline.md §1.1/§6）。
fn needs_user_choice(candidates: &[MatchCandidate]) -> bool {
    match candidates {
        [] => true,
        [_single] => false,
        [top, next, ..] => top.confidence - next.confidence < 0.1,
    }
}

/// `SessionError` → 导入错误码（§1.10 映射表子集，唯一实现见
/// [`crate::ipc_errors`]；此处仅取码位与文案）。
fn load_error_code(error: &SessionError) -> (&'static str, String) {
    match error {
        SessionError::Plugin { code, message } => {
            (crate::ipc_errors::code_name(*code), message.clone())
        }
        SessionError::SessionGone => ("plugin_crashed", "plugin session gone".to_string()),
    }
}

/// 无 rand 依赖的 UUID v4 形 file_id（protocol.md §2.3 要求 UUID v4 字符串）。
fn default_file_id(seq: u64) -> String {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64).rotate_left(32)
        ^ seq.rotate_left(48);
    let mut bytes = [0u8; 16];
    for byte in bytes.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *byte = x as u8;
    }
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 10
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// key_values 按文件并发扇出（pipeline.md §4.2）：
/// 每活跃文件一条独立请求（含 10s 超时），任一失败仅置该文件 outcome，
/// 不阻塞、不取消其余；返回与 `active_file_ids` 同序（ipc-ui.md §1.6）。
pub async fn query_key_values(
    registry: &Arc<SessionRegistry>,
    file_index: &FileIndex,
    active_file_ids: &[String],
    timestamp_ms: i64,
    timeout: Duration,
) -> Vec<KeyValuesOutcome> {
    let mut tasks: Vec<(String, tokio::task::JoinHandle<KeyValuesOutcome>)> =
        Vec::with_capacity(active_file_ids.len());
    for file_id in active_file_ids {
        let plugin_id = file_index.get(file_id);
        let file_id = file_id.clone();
        let outer_file_id = file_id.clone();
        let registry = registry.clone();
        let handle = tokio::spawn(async move {
            let Some(plugin_id) = plugin_id else {
                return KeyValuesOutcome {
                    file_id: file_id.clone(),
                    plugin_id: String::new(),
                    result: Err(KeyValuesError::FileNotReady(file_id)),
                };
            };
            let Some(session) = registry.get(&plugin_id) else {
                return KeyValuesOutcome {
                    file_id,
                    plugin_id,
                    result: Err(KeyValuesError::SessionGone),
                };
            };
            let result = match tokio::time::timeout(
                timeout,
                session.key_values(KeyValuesParams {
                    file_id: file_id.clone(),
                    timestamp_ms,
                }),
            )
            .await
            {
                Ok(Ok(kv)) => Ok(kv.entries),
                Ok(Err(SessionError::Plugin { code, message })) => {
                    Err(KeyValuesError::PluginError(code, message))
                }
                Ok(Err(SessionError::SessionGone)) => Err(KeyValuesError::SessionGone),
                Err(_) => Err(KeyValuesError::Timeout),
            };
            KeyValuesOutcome {
                file_id,
                plugin_id,
                result,
            }
        });
        tasks.push((outer_file_id, handle));
    }
    let mut outcomes = Vec::with_capacity(tasks.len());
    for (file_id, task) in tasks {
        match task.await {
            Ok(outcome) => outcomes.push(outcome),
            Err(_e) => outcomes.push(KeyValuesOutcome {
                file_id,
                plugin_id: String::new(),
                result: Err(KeyValuesError::SessionGone),
            }),
        }
    }
    outcomes
}

/// C2.4 计数核验决策：`records_total` 与 sink 实际接收不一致时，若确有
/// 丢弃（dropped_batches > 0）→ 显式 `host_backpressure` 错误（不静默
/// 继续）；否则返回 `None`，维持 count_mismatch 现行为（由
/// [`Store::freeze`] 的计数校验产出 ParseFailed）。
fn lost_batch_error(
    records_total: u64,
    received_records: u64,
    dropped_batches: u64,
) -> Option<(&'static str, &'static str)> {
    if records_total != received_records && dropped_batches > 0 {
        Some(("host_backpressure", "host_backpressure"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C2.4 决策表：dropped > 0 且计数不一致 → host_backpressure；
    /// 其余组合（一致 / 不一致但无丢弃 / 一致但残留计数）→ None。
    #[test]
    fn lost_batch_error_decision_table() {
        assert_eq!(lost_batch_error(10, 10, 0), None, "计数一致");
        assert_eq!(
            lost_batch_error(10, 8, 0),
            None,
            "不一致但无丢弃 → 维持 count_mismatch 现行为"
        );
        assert_eq!(
            lost_batch_error(10, 10, 3),
            None,
            "计数一致 → 忽略陈旧 dropped 计数"
        );
        let (code, reason) = lost_batch_error(10, 8, 3).expect("dropped>0 且不一致");
        assert_eq!(code, "host_backpressure");
        assert_eq!(reason, "host_backpressure");
    }

    /// C2.5：默认重试语义锁定——退避序列恰为 1s/3s（总尝试 3 次）。
    /// 仅锁定默认值本身；时序行为由集成测试（注入短退避）验证。
    #[test]
    fn default_load_retry_backoffs_are_1s_then_3s() {
        let config = PipelineConfig::default();
        assert_eq!(config.load_retry_backoffs.len(), 2, "总尝试 3 次");
        assert_eq!(config.load_retry_backoffs[0], Duration::from_secs(1));
        assert_eq!(config.load_retry_backoffs[1], Duration::from_secs(3));
    }
}
