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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ab_host::{PluginRegistry, PluginRuntime};
use ab_pipeline::import::MatchCandidate;
use ab_pipeline::{ParseEvent, PipelineEvent, SessionError, SessionRegistry, Store};
use ab_protocol::types::{
    CanHandleParams, CancelParseParams, KeyValueEntry, KeyValuesParams, LoadFileParams, MetricDef,
    ParseParams, UnloadFileParams,
};
use tokio::sync::mpsc;

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
}

impl std::fmt::Debug for PipelineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineConfig")
            .field("can_handle_timeout", &self.can_handle_timeout)
            .field("key_values_timeout", &self.key_values_timeout)
            .field("max_import_bytes", &self.max_import_bytes)
            .field("file_id_fn", &self.file_id_fn.is_some())
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
    /// file_id → 源路径（get_metrics 文件节点名用）。
    paths: RwLock<HashMap<String, String>>,
    /// 同插件 load+parse 串行锁（pipeline.md §1 并发约束）。
    plugin_locks: RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    file_seq: AtomicU64,
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
                paths: RwLock::new(HashMap::new()),
                plugin_locks: RwLock::new(HashMap::new()),
                file_seq: AtomicU64::new(0),
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
                tokio::spawn(async move { me.inner.import_one(&path, None).await }),
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
        self.inner.import_one(&path, Some(plugin_id)).await
    }

    /// 卸载文件：会话 `unload_file`（3s 超时按完成）→ store 释放 → 状态移除
    /// （ipc-ui.md §1.3；幂等：未知 file_id 视为成功）。
    pub async fn unload_file(&self, file_id: &str) {
        let Some(plugin_id) = self.inner.file_index.get(file_id) else {
            return;
        };
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

    /// 取消 parse：会话 `cancel_parse` + 丢弃半成品（pipeline.md §1.2）。
    pub async fn cancel_parse(&self, file_id: &str) {
        let Some(plugin_id) = self.inner.file_index.get(file_id) else {
            return;
        };
        if let Some(session) = self.inner.registry.get(&plugin_id) {
            let _ = session
                .cancel_parse(CancelParseParams {
                    file_id: file_id.to_string(),
                })
                .await;
        }
        self.inner.store.unload(file_id);
        self.inner.frozen.write().unwrap().remove(file_id);
        self.emit(PipelineEvent::ParseCancelled {
            file_id: file_id.to_string(),
        });
    }

    fn emit(&self, event: PipelineEvent) {
        let _ = self.inner.events.send(event);
    }
}
impl ImportCoordinatorInner {
    fn emit(&self, event: PipelineEvent) {
        let _ = self.events.send(event);
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
        let adapter: Arc<dyn ab_pipeline::PluginSession> =
            Arc::new(HostSessionAdapter::new(session));
        self.registry.register(adapter.clone());
        Ok(adapter)
    }

    async fn import_one(&self, path: &Path, override_plugin: Option<&str>) -> ImportOutcome {
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

        // load_file：按 pipeline.md §1.2 自动重试（最多 2 次，退避 1s/3s）。
        let load_params = LoadFileParams {
            file_id: file_id.clone(),
            path: path_str.clone(),
        };
        let mut summary = None;
        let mut last_load_error = None;
        for (index, backoff) in [Duration::from_secs(1), Duration::from_secs(3)]
            .iter()
            .enumerate()
        {
            match session.load_file(load_params.clone()).await {
                Ok(value) => {
                    summary = Some(value);
                    break;
                }
                Err(e) => {
                    last_load_error = Some(e);
                    if index + 1 < 2 {
                        tokio::time::sleep(*backoff).await;
                    }
                }
            }
        }
        let Some(summary) = summary else {
            let e = last_load_error.expect("at least one attempt");
            let (code, message) = load_error_code(&e);
            self.emit(PipelineEvent::FileLoadFailed {
                file_id: file_id.clone(),
                message: message.clone(),
            });
            self.file_index.remove(&file_id);
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
                self.emit(PipelineEvent::ParseFailed {
                    file_id: file_id.clone(),
                    reason: "schema_error".to_string(),
                    detail: Some(e.to_string()),
                });
                self.file_index.remove(&file_id);
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
            self.emit(PipelineEvent::ParseFailed {
                file_id: file_id.clone(),
                reason: "internal".to_string(),
                detail: Some(e.to_string()),
            });
            self.file_index.remove(&file_id);
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
                self.store.unload(&file_id);
                self.file_index.remove(&file_id);
                self.paths.write().unwrap().remove(&file_id);
                self.emit(PipelineEvent::ParseFailed {
                    file_id: file_id.clone(),
                    reason: "plugin_error".to_string(),
                    detail: Some(e.to_string()),
                });
                let (code, message) = load_error_code(&e);
                return ImportOutcome::failed(&path_str, info.size_bytes, code, message);
            }
        };
        if let Some(err) = append_error {
            self.store.unload(&file_id);
            self.file_index.remove(&file_id);
            self.paths.write().unwrap().remove(&file_id);
            self.emit(PipelineEvent::ParseFailed {
                file_id: file_id.clone(),
                reason: "protocol_error".to_string(),
                detail: Some(err.clone()),
            });
            return ImportOutcome::failed(
                &path_str,
                info.size_bytes,
                "internal",
                format!("store append failed: {err}"),
            );
        }
        if let Err(e) = self.store.freeze(&file_id, records_total) {
            self.store.unload(&file_id);
            self.file_index.remove(&file_id);
            self.paths.write().unwrap().remove(&file_id);
            self.emit(PipelineEvent::ParseFailed {
                file_id: file_id.clone(),
                reason: "count_mismatch".to_string(),
                detail: Some(e.to_string()),
            });
            return ImportOutcome::failed(
                &path_str,
                info.size_bytes,
                "parse_failed",
                format!("freeze failed: {e}"),
            );
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
            let timeout = self.config.can_handle_timeout;
            tasks.push(tokio::spawn(async move {
                probe_plugin(host, registry, timeout, &plugin_id, &params).await
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
async fn probe_plugin(
    host: Arc<PluginRuntime>,
    registry: Arc<SessionRegistry>,
    timeout: Duration,
    plugin_id: &str,
    params: &CanHandleParams,
) -> Option<MatchCandidate> {
    let session = match host.get_or_spawn(plugin_id).await {
        Ok(session) => {
            let adapter: Arc<dyn ab_pipeline::PluginSession> =
                Arc::new(HostSessionAdapter::new(session));
            registry.register(adapter.clone());
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

/// `SessionError` → 导入错误码（ipc-ui.md §1.10 映射表子集）。
fn load_error_code(error: &SessionError) -> (&'static str, String) {
    match error {
        SessionError::Plugin { code, message } => match *code {
            ab_protocol::errors::ERR_PLUGIN_BUSY => ("plugin_busy", message.clone()),
            ab_protocol::errors::ERR_FILE_LOAD_FAILED => ("file_load_failed", message.clone()),
            ab_protocol::errors::ERR_PARSE_FAILED => ("parse_failed", message.clone()),
            ab_protocol::errors::ERR_CANCELLED => ("cancelled", message.clone()),
            _ => ("internal", message.clone()),
        },
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
