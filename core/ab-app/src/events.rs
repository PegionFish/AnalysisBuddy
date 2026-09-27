//! ipc-ui.md §2 事件流：`HostEvent` → `ab://plugin-health` / `ab://plugin-log` /
//! `ab://progress` 三个通道的载荷转换与节流。
//!
//! 通道名常量与 `ui/src/ipc/events.ts` 字符串逐字一致（mock/real 两侧不得分叉，
//! P3-03 将对齐）。载荷字段与 §2.2 / §2.3 的 TS 接口逐字段一致。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use ab_host::{HostEvent, PluginProcessState};
use ab_pipeline::PipelineEvent;
use ab_protocol::types::ProgressParams;

/// `ab://progress`（ipc-ui.md §2.1）。
pub const EV_PROGRESS: &str = "ab://progress";
/// `ab://plugin-log`（ipc-ui.md §2.2）。
pub const EV_PLUGIN_LOG: &str = "ab://plugin-log";
/// `ab://plugin-health`（ipc-ui.md §2.3）。
pub const EV_PLUGIN_HEALTH: &str = "ab://plugin-health";
/// `ab://plugins-reloaded`（spec §6.3：模块管理页以事件为触发器重拉
/// `list_plugins`；任务 5 接线，随 `registry.reload()` 广播）。
pub const EV_PLUGINS_RELOADED: &str = "ab://plugins-reloaded";

/// progress 节流窗口（§2.1：同一 file_id 最多每 100ms 发一条）。
pub const PROGRESS_THROTTLE_MS: Duration = Duration::from_millis(100);

/// `get_plugin_log` 环形缓冲字节预算（§2.2：host 按插件 1MB 环形缓冲，
/// protocol.md §9.3）。
pub const LOG_BUFFER_BYTES_PER_PLUGIN: usize = 1024 * 1024;

/// `get_plugin_log` 默认条数（§2.2：环形缓冲尾部，默认 200 条）。
pub const LOG_TAIL_DEFAULT: usize = 200;

/// 结构化诊断环形缓冲容量（C8.1：上限 500 条，超限淘汰最旧）。
pub const DIAGNOSTIC_BUFFER_CAPACITY: usize = 500;

/// `ab://plugin-health` 载荷（§2.3，对应 `PluginHealthPayload`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PluginHealthPayload {
    pub plugin_id: String,
    /// 当前状态（`PluginState` 小写映射）。
    pub state: String,
    pub prev_state: String,
    /// 可选：如 exit_code、超时原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `ab://plugin-log` 载荷（§2.2，对应 `PluginLogPayload`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PluginLogPayload {
    pub plugin_id: String,
    pub level: LogLevel,
    /// stderr 单行原文（去尾部换行）。
    pub line: String,
    /// host 捕获时刻，UTC 毫秒。
    pub ts_ms: i64,
}

/// stderr 行级别（§2.2 取值域 `"debug" | "info" | "warn" | "error"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// `ab://progress` 载荷（§2.1）：字段与 `ProgressParams` 逐字段一致
/// （file_id / percent? / records_so_far / bytes_read?）。
pub type ProgressPayload = ProgressParams;

/// `ab://plugins-reloaded` 载荷（spec §6.3，对应 `PluginsReloadedPayload`）：
/// UI 仅以事件为触发器重拉 `list_plugins`，载荷携带发现明细摘要
/// （id/目录/失败原因），字段与 events.ts 接口逐字一致。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PluginsReloadedPayload {
    /// 重载后已发现插件（id 列表，UI 展示序）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
    /// 非法模块（目录 + 失败原因摘要）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub invalid: Vec<String>,
    /// 被同 id 高优先级源遮蔽的模块（id 列表）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shadowed: Vec<String>,
}

/// 转换产出的一条待发事件。
#[derive(Debug)]
pub struct EmittedEvent {
    /// 通道名（`EV_*` 常量之一）。
    pub channel: &'static str,
    pub payload: EventPayload,
}

/// 载荷枚举（按类型序列化后 `emit`）。
#[derive(Debug, serde::Serialize)]
pub enum EventPayload {
    Health(PluginHealthPayload),
    Log(PluginLogPayload),
    Progress(ProgressPayload),
    PluginsReloaded(PluginsReloadedPayload),
}

/// 状态机状态 → `PluginState` 小写映射（§2.3 / ipc-ui.md §1.0）。
pub fn state_name(state: PluginProcessState) -> &'static str {
    match state {
        PluginProcessState::Discovered => "discovered",
        PluginProcessState::Spawning => "spawning",
        PluginProcessState::Initializing => "initializing",
        PluginProcessState::Ready => "ready",
        PluginProcessState::Loading => "loading",
        PluginProcessState::Parsing => "parsing",
        PluginProcessState::Draining => "draining",
        PluginProcessState::Shutdown => "shutdown",
        PluginProcessState::Crashed => "crashed",
        PluginProcessState::Timeout => "timeout",
    }
}

/// stderr 行级别解析（§2.2）：行首匹配 `INFO/WARN/ERROR/DEBUG`（大小写不敏感）
/// 前缀则取之，否则 `info`。
pub fn parse_log_level(line: &str) -> LogLevel {
    let head = line
        .chars()
        .take(6)
        .collect::<String>()
        .to_ascii_uppercase();
    if head.starts_with("ERROR") {
        LogLevel::Error
    } else if head.starts_with("WARN") {
        LogLevel::Warn
    } else if head.starts_with("INFO") {
        LogLevel::Info
    } else if head.starts_with("DEBUG") {
        LogLevel::Debug
    } else {
        LogLevel::Info
    }
}

/// §2.1 覆盖式节流：同一 `file_id` 的 progress 最多每 `window` 发一条；窗口内
/// 到达的更新保留最新值，窗口到期（或 `flush`）时补发最新值。
#[derive(Debug)]
pub struct ProgressThrottle {
    window: Duration,
    last_emit: HashMap<String, Instant>,
    pending: HashMap<String, ProgressParams>,
}

impl ProgressThrottle {
    /// 默认窗口 100ms（§2.1）。
    pub fn new() -> Self {
        Self::with_window(PROGRESS_THROTTLE_MS)
    }

    /// 测试注入窗口。
    pub fn with_window(window: Duration) -> Self {
        Self {
            window,
            last_emit: HashMap::new(),
            pending: HashMap::new(),
        }
    }
    /// 接收一条 progress。窗口到期 → 返回该 file_id 的最新值（待发）；未到期 →
    /// `None`（最新值保留，到期补发）。
    ///
    /// 终态直发：`percent ≥ 100` 的进度不经节流直接返回（§2.1「UI 收到
    /// percent:100 即可置完成中」——终态不得被窗口吞掉，否则重开/收尾
    /// 路径前端永远等不到 100），同时清掉窗口内遗留 pending（终态即最新值）。
    pub fn accept(&mut self, params: ProgressParams) -> Option<ProgressParams> {
        let file_id = params.file_id.clone();
        if params.percent.is_some_and(|p| p >= 100.0) {
            self.pending.remove(&file_id);
            self.last_emit.insert(file_id, Instant::now());
            return Some(params);
        }
        let due = match self.last_emit.get(&file_id) {
            Some(&last) => last.elapsed() >= self.window,
            None => true,
        };
        self.pending.insert(file_id.clone(), params);
        if due {
            self.flush_file(&file_id)
        } else {
            None
        }
    }

    /// 补发全部未发出的最新值（会话结束 / 测试用）。
    pub fn flush(&mut self) -> Vec<ProgressParams> {
        let ids: Vec<String> = self.pending.keys().cloned().collect();
        ids.into_iter()
            .filter_map(|id| self.flush_file(&id))
            .collect()
    }

    fn flush_file(&mut self, file_id: &str) -> Option<ProgressParams> {
        let params = self.pending.remove(file_id)?;
        self.last_emit.insert(file_id.to_string(), Instant::now());
        Some(params)
    }
}

impl Default for ProgressThrottle {
    fn default() -> Self {
        Self::new()
    }
}

/// 插件侧元数据（`list_plugins` 数据源）：最新状态 + 最近失败摘要。
/// 由 host 事件流驱动（wire_events 先 `record` 再 `convert`，保证
/// `ab://plugin-health` 与 `list_plugins` 同一事实）。
#[derive(Debug, Default)]
pub struct PluginMeta {
    states: RwLock<HashMap<String, String>>,
    last_errors: RwLock<HashMap<String, String>>,
}

impl PluginMeta {
    pub fn new() -> Self {
        Self::default()
    }

    /// 消费一条 `HostEvent`：
    /// - `StateChanged` → 更新状态；回到 `ready` 时清空失败摘要（§4.6）；
    /// - `SessionTerminated`（非优雅退出码 0）→ 记录失败摘要；
    /// - 其余事件不记录。
    pub fn record(&self, event: &HostEvent) {
        match event {
            HostEvent::StateChanged { plugin_id, to, .. } => {
                let mut states = self.states.write().expect("states lock poisoned");
                states.insert(plugin_id.clone(), state_name(*to).to_string());
                if *to == PluginProcessState::Ready {
                    self.last_errors
                        .write()
                        .expect("last_errors lock poisoned")
                        .remove(plugin_id);
                }
            }
            HostEvent::SessionTerminated {
                plugin_id,
                exit_code,
                summary,
                ..
            } if exit_code != &Some(0) && !summary.is_empty() => {
                // 优雅停机（退出码 0）不算失败（host-runtime.md §7.7）。
                self.last_errors
                    .write()
                    .expect("last_errors lock poisoned")
                    .insert(plugin_id.clone(), summary.clone());
            }
            _ => {}
        }
    }

    /// 已知状态（`state_name` 小写映射）；未知（未发生过事件）→ `None`。
    pub fn state_of(&self, plugin_id: &str) -> Option<String> {
        self.states
            .read()
            .expect("states lock poisoned")
            .get(plugin_id)
            .cloned()
    }

    /// 最近失败摘要（`crashed`/`timeout` 时非空；回到 ready 后清空）。
    pub fn last_error_of(&self, plugin_id: &str) -> Option<String> {
        self.last_errors
            .read()
            .expect("last_errors lock poisoned")
            .get(plugin_id)
            .cloned()
    }
}

/// 每插件 stderr 行环形缓冲（`get_plugin_log` 数据源，§2.2）：按字节预算
/// （1MB/插件）淘汰最旧行；`tail` 取尾部 N 条。
#[derive(Debug)]
pub struct PluginLogBuffer {
    inner: Mutex<HashMap<String, VecDeque<(PluginLogPayload, usize)>>>,
    /// 每插件字节预算（默认 [`LOG_BUFFER_BYTES_PER_PLUGIN`]）。
    budget: usize,
}

impl Default for PluginLogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginLogBuffer {
    pub fn new() -> Self {
        Self::with_budget(LOG_BUFFER_BYTES_PER_PLUGIN)
    }

    /// 测试注入预算。
    pub fn with_budget(budget: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            budget,
        }
    }

    /// 追加一行（含行字节数统计；超预算淘汰最旧）。
    pub fn push(&self, payload: PluginLogPayload) {
        let mut inner = self.inner.lock().expect("log buffer lock poisoned");
        let queue = inner.entry(payload.plugin_id.clone()).or_default();
        let bytes = payload.line.len() + 64;
        queue.push_back((payload, bytes));
        let mut total: usize = queue.iter().map(|(_, b)| b).sum();
        while total > self.budget {
            if let Some((_, b)) = queue.pop_front() {
                total -= b;
            } else {
                break;
            }
        }
    }

    /// 取尾部 `limit` 条（`limit == 0` 或空缓冲 → 空）。
    pub fn tail(&self, plugin_id: &str, limit: usize) -> Vec<PluginLogPayload> {
        let inner = self.inner.lock().expect("log buffer lock poisoned");
        inner
            .get(plugin_id)
            .map(|queue| {
                queue
                    .iter()
                    .rev()
                    .take(limit)
                    .rev()
                    .map(|(p, _)| p.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 缓冲总行数（测试/诊断）。
    pub fn len_of(&self, plugin_id: &str) -> usize {
        self.inner
            .lock()
            .expect("log buffer lock poisoned")
            .get(plugin_id)
            .map(VecDeque::len)
            .unwrap_or(0)
    }
}

/// 诊断事件类别（C8.1：导入生命周期 + 插件崩溃 + 慢查询预留）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    /// 导入成功（Ready 终态）。
    ImportDone,
    /// 导入失败（非取消的 Error 终态）。
    ImportFailed,
    /// 导入被取消（cancel_parse / 卸载并发）。
    ImportCancelled,
    /// 插件进程崩溃（预留接线点）。
    PluginCrash,
    /// 查询慢（预留接线点）。
    QuerySlow,
}

/// 结构化诊断条目（C8.1）：跨层 session 级生命周期记录。无值可选字段
/// 序列化时省略键（`skip_serializing_if`，与 §2 载荷风格一致）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticEntry {
    /// 进程启动时生成一次（[`DiagnosticBuffer`] 构造时；record 统一盖章）。
    pub session_id: String,
    /// 记录时刻，UTC 毫秒（record 统一盖章）。
    pub ts_ms: i64,
    pub kind: DiagnosticKind,
    /// 源文件路径（导入生命周期以路径为准；file_id 在早失败路径不存在）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// 文件 SHA-256（如有；本期不强制采集）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_source: Option<String>,
    /// host（ab-app）版本（编译期注入）。
    pub host_version: String,
    /// 导入耗时毫秒（import_one 起始至终态）。
    pub duration_ms: u64,
    /// 解析记录总数（Ready 终态；失败/取消路径为 0，进度看 received 计数）。
    pub records_total: u64,
    pub received_batches: u64,
    pub dropped_batches: u64,
    /// 进程 RSS（MB；可选，不强制采集）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_mb: Option<u64>,
    /// 错误码（ImportFailed；取值域同 `IpcError.code`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// 错误/取消详情（可选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 进程级结构化诊断环形缓冲（C8.1）：容量 [`DIAGNOSTIC_BUFFER_CAPACITY`]
/// 条，超限淘汰最旧；线程安全（Mutex）。纯内存闭环——不落盘、无网络、
/// 不上传任何数据。
#[derive(Debug)]
pub struct DiagnosticBuffer {
    inner: Mutex<VecDeque<DiagnosticEntry>>,
    /// 容量上限（测试注入）。
    capacity: usize,
    /// 进程启动时生成一次（本缓冲全部条目共用）。
    session_id: String,
}

impl Default for DiagnosticBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticBuffer {
    /// 默认容量 [`DIAGNOSTIC_BUFFER_CAPACITY`]；session_id 此刻生成（进程
    /// 启动语义——每进程一个缓冲实例）。
    pub fn new() -> Self {
        Self::with_capacity(DIAGNOSTIC_BUFFER_CAPACITY)
    }

    /// 测试注入容量。
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            capacity,
            session_id: new_session_id(),
        }
    }

    /// 本进程 session_id（构造时生成一次）。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 记录一条诊断：session_id 与 ts_ms 由缓冲统一盖章；超限淘汰最旧。
    pub fn record(&self, mut entry: DiagnosticEntry) {
        entry.session_id = self.session_id.clone();
        entry.ts_ms = now_ms();
        let mut inner = self.inner.lock().expect("diagnostic buffer lock poisoned");
        inner.push_back(entry);
        while inner.len() > self.capacity {
            inner.pop_front();
        }
    }

    /// 取最近 `n` 条（时间序）；`n == 0` → 空；`n` 超长 → 全量。
    pub fn recent(&self, n: usize) -> Vec<DiagnosticEntry> {
        let inner = self.inner.lock().expect("diagnostic buffer lock poisoned");
        inner.iter().rev().take(n).rev().cloned().collect()
    }

    /// 当前条数。
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("diagnostic buffer lock poisoned")
            .len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 进程 session_id（无 rand 依赖，pid + 纳秒时钟混合，启动唯一即可）。
fn new_session_id() -> String {
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("ab-{:08x}-{:016x}", std::process::id(), ns)
}

/// 当前 UTC 毫秒。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `HostEvent` → 待发事件（0..n 条）。
///
/// 映射面：`StateChanged` → health（§2.3，状态机每次迁移发一条）；
/// `StderrLine` → log（§2.2）；`Progress` → progress（§2.1 节流）；
/// `PluginsReloaded` → `ab://plugins-reloaded`（spec §6.3，模块管理页
/// 刷新触发器；任务 5 接线，不再丢弃）。
/// 其余事件不映射：`SessionTerminated` 紧跟吸收态 `StateChanged`（徽标已被覆盖），
/// stderr 摘要走 `StderrLine`；`PluginDegraded` 留待后续卡。
pub fn convert(event: HostEvent, throttle: &mut ProgressThrottle) -> Vec<EmittedEvent> {
    match event {
        HostEvent::StateChanged {
            plugin_id,
            from,
            to,
        } => vec![EmittedEvent {
            channel: EV_PLUGIN_HEALTH,
            payload: EventPayload::Health(PluginHealthPayload {
                plugin_id,
                state: state_name(to).to_string(),
                prev_state: state_name(from).to_string(),
                detail: None,
            }),
        }],
        HostEvent::StderrLine {
            plugin_id,
            ts_ms,
            line,
        } => vec![EmittedEvent {
            channel: EV_PLUGIN_LOG,
            payload: EventPayload::Log(PluginLogPayload {
                level: parse_log_level(&line),
                plugin_id,
                line,
                ts_ms,
            }),
        }],
        HostEvent::Progress(params) => throttle
            .accept(params)
            .map(|params| EmittedEvent {
                channel: EV_PROGRESS,
                payload: EventPayload::Progress(params),
            })
            .into_iter()
            .collect(),
        HostEvent::PluginsReloaded {
            plugins,
            invalid,
            shadowed,
        } => vec![EmittedEvent {
            channel: EV_PLUGINS_RELOADED,
            payload: EventPayload::PluginsReloaded(PluginsReloadedPayload {
                plugins: plugins.iter().map(|p| p.manifest.id.clone()).collect(),
                invalid: invalid
                    .iter()
                    .map(|p| format!("{} ({})", p.dir.display(), p.reason))
                    .collect(),
                shadowed: shadowed.iter().map(|s| s.id.clone()).collect(),
            }),
        }],
        HostEvent::SessionTerminated { .. } | HostEvent::PluginDegraded { .. } => Vec::new(),
    }
}

/// `PipelineEvent` → 待发事件（ipc-ui.md §2.1 与 command 状态翻转）。
///
/// 映射面：`ParseProgress` → `ab://progress`（走与 host 事件共用节流窗口，同
/// file_id 的 host 转发 progress 与本路 progress 自然去重）；其余 `PipelineEvent`
/// 不产生线上事件——它们驱动 command 侧状态（`ParseCompleted` → Store Frozen →
/// `get_metrics`/`query_series` 可查；`FileUnloaded` → 状态移除），与 ipc-ui.md
/// §2.1「parse 完成不发 progress 终态事件，以 command 侧状态翻转（get_metrics
/// 可查）为准」一致。
pub fn convert_pipeline(
    event: PipelineEvent,
    throttle: &mut ProgressThrottle,
) -> Vec<EmittedEvent> {
    match event {
        PipelineEvent::ParseProgress {
            file_id,
            percent,
            records_so_far,
        } => throttle
            .accept(ProgressParams {
                file_id,
                percent,
                records_so_far,
                bytes_read: None,
            })
            .map(|params| EmittedEvent {
                channel: EV_PROGRESS,
                payload: EventPayload::Progress(params),
            })
            .into_iter()
            .collect(),
        PipelineEvent::ImportStarted { .. }
        | PipelineEvent::ImportFailed { .. }
        | PipelineEvent::MatchCandidates { .. }
        | PipelineEvent::PluginSelected { .. }
        | PipelineEvent::FileLoaded { .. }
        | PipelineEvent::FileLoadFailed { .. }
        | PipelineEvent::ParseCompleted { .. }
        | PipelineEvent::ParseFailed { .. }
        | PipelineEvent::ParseCancelled { .. }
        | PipelineEvent::QueryReady { .. }
        | PipelineEvent::FileUnloaded { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn state_changed(from: PluginProcessState, to: PluginProcessState) -> HostEvent {
        HostEvent::StateChanged {
            plugin_id: "mock".to_string(),
            from,
            to,
        }
    }

    #[test]
    fn state_maps_to_lowercase_plugin_state() {
        let all = [
            (PluginProcessState::Discovered, "discovered"),
            (PluginProcessState::Spawning, "spawning"),
            (PluginProcessState::Initializing, "initializing"),
            (PluginProcessState::Ready, "ready"),
            (PluginProcessState::Loading, "loading"),
            (PluginProcessState::Parsing, "parsing"),
            (PluginProcessState::Draining, "draining"),
            (PluginProcessState::Shutdown, "shutdown"),
            (PluginProcessState::Crashed, "crashed"),
            (PluginProcessState::Timeout, "timeout"),
        ];
        for (state, expected) in all {
            assert_eq!(state_name(state), expected, "§2.3 小写映射");
        }
    }

    #[test]
    fn stderr_level_parsed_from_line_prefix() {
        assert_eq!(parse_log_level("INFO loading 1000 rows"), LogLevel::Info);
        assert_eq!(parse_log_level("WARN: schema slow"), LogLevel::Warn);
        assert_eq!(parse_log_level("Error occurred"), LogLevel::Error);
        assert_eq!(parse_log_level("debug trace line"), LogLevel::Debug);
        assert_eq!(parse_log_level("info"), LogLevel::Info);
        // 无前缀 / 非四级前缀 → info（§2.2）。
        assert_eq!(parse_log_level("plain line"), LogLevel::Info);
        assert_eq!(parse_log_level("NOTICE something"), LogLevel::Info);
    }

    #[test]
    fn health_payload_snapshot_matches_ipc_ui_section2() {
        // §2.3 `PluginHealthPayload` 逐字段：plugin_id / state / prev_state / detail?。
        let mut throttle = ProgressThrottle::new();
        let events = convert(
            state_changed(PluginProcessState::Parsing, PluginProcessState::Ready),
            &mut throttle,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].channel, EV_PLUGIN_HEALTH);
        let payload = match &events[0].payload {
            EventPayload::Health(p) => p,
            other => panic!("expected health payload, got {other:?}"),
        };
        assert_eq!(
            serde_json::to_value(payload).expect("serialize health"),
            json!({
                "plugin_id": "mock",
                "state": "ready",
                "prev_state": "parsing",
            }),
            "§2.3 载荷快照（无 detail 时省略该键）"
        );
        // detail 非空时序列化。
        let with_detail = PluginHealthPayload {
            plugin_id: "mock".to_string(),
            state: "crashed".to_string(),
            prev_state: "parsing".to_string(),
            detail: Some("exit code 1".to_string()),
        };
        assert_eq!(
            serde_json::to_value(with_detail).expect("serialize health with detail"),
            json!({
                "plugin_id": "mock",
                "state": "crashed",
                "prev_state": "parsing",
                "detail": "exit code 1",
            })
        );
    }

    #[test]
    fn log_payload_snapshot_matches_ipc_ui_section2() {
        // §2.2 `PluginLogPayload` 逐字段：plugin_id / level / line / ts_ms。
        let mut throttle = ProgressThrottle::new();
        let events = convert(
            HostEvent::StderrLine {
                plugin_id: "mock".to_string(),
                ts_ms: 1785600000123,
                line: "WARN slow parse".to_string(),
            },
            &mut throttle,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].channel, EV_PLUGIN_LOG);
        let payload = match &events[0].payload {
            EventPayload::Log(p) => p,
            other => panic!("expected log payload, got {other:?}"),
        };
        assert_eq!(
            serde_json::to_value(payload).expect("serialize log"),
            json!({
                "plugin_id": "mock",
                "level": "warn",
                "line": "WARN slow parse",
                "ts_ms": 1785600000123_i64,
            }),
            "§2.2 载荷快照"
        );
    }

    #[test]
    fn progress_payload_snapshot_matches_ipc_ui_section2() {
        // §2.1 `ProgressPayload` 逐字段：file_id / percent? / records_so_far / bytes_read?。
        let mut throttle = ProgressThrottle::new();
        let events = convert(
            HostEvent::Progress(ProgressParams {
                file_id: "f1".to_string(),
                percent: Some(0.5),
                records_so_far: 42,
                bytes_read: Some(512),
            }),
            &mut throttle,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].channel, EV_PROGRESS);
        let payload = match &events[0].payload {
            EventPayload::Progress(p) => p,
            other => panic!("expected progress payload, got {other:?}"),
        };
        assert_eq!(
            serde_json::to_value(payload).expect("serialize progress"),
            json!({
                "file_id": "f1",
                "percent": 0.5,
                "records_so_far": 42,
                "bytes_read": 512,
            }),
            "§2.1 载荷快照"
        );
    }

    #[test]
    fn plugins_reloaded_payload_snapshot_matches_events_ts() {
        // §6.3 `PluginsReloadedPayload` 逐字段：plugins / invalid / shadowed
        // （字段与 ui/src/ipc/events.ts 接口逐字一致；空集合省略键）。
        let mut throttle = ProgressThrottle::new();
        let events = convert(
            HostEvent::PluginsReloaded {
                plugins: vec![discovered_fixture("a"), discovered_fixture("b")],
                invalid: vec![ab_host::InvalidPlugin {
                    dir: PathBuf::from(r"C:\plugins\broken"),
                    source: ab_host::PluginSource::Portable,
                    reason: ab_host::DiscoveryError::MissingManifest,
                }],
                shadowed: vec![ab_host::ShadowedPlugin {
                    id: "s1".to_string(),
                    plugin_dir: PathBuf::from(r"C:\plugins\s1"),
                    source: ab_host::PluginSource::UserData,
                    winner_source: ab_host::PluginSource::Portable,
                }],
            },
            &mut throttle,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].channel, EV_PLUGINS_RELOADED);
        let payload = match &events[0].payload {
            EventPayload::PluginsReloaded(p) => p,
            other => panic!("expected plugins-reloaded payload, got {other:?}"),
        };
        assert_eq!(payload.plugins, vec!["a".to_string(), "b".to_string()]);
        let value = serde_json::to_value(payload).expect("serialize");
        assert_eq!(
            value["plugins"],
            json!(["a", "b"]),
            "§6.3 plugins 字段（id 列表）"
        );
        assert_eq!(
            value["invalid"],
            json!([r"C:\plugins\broken (plugin.json is missing)"]),
            "§6.3 invalid 字段（目录 + 失败原因摘要）"
        );
        assert_eq!(value["shadowed"], json!(["s1"]), "§6.3 shadowed 字段（id）");
    }

    fn discovered_fixture(id: &str) -> ab_host::DiscoveredPlugin {
        use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};
        ab_host::DiscoveredPlugin {
            manifest: Manifest {
                id: id.to_string(),
                display_name: format!("Mock {id}"),
                version: "0.1.0".to_string(),
                entry: PluginEntry {
                    command: "mock".to_string(),
                    args: vec![],
                    working_dir: None,
                },
                r#match: MatchRules {
                    extensions: vec!["csv".to_string()],
                    header_fingerprints: None,
                },
                min_protocol_version: 1,
                ..Default::default()
            },
            plugin_dir: PathBuf::from("."),
            source: ab_host::PluginSource::Portable,
            resolved: ab_host::ResolvedEntry {
                program: PathBuf::from("mock"),
                args: vec![],
                working_dir: PathBuf::from("."),
            },
        }
    }

    #[test]
    fn progress_throttle_emits_once_per_window_with_latest_value() {
        // 1 小时窗口：首次立即发，窗口内后续到达只更新不重发。
        let mut throttle = ProgressThrottle::with_window(Duration::from_secs(3600));
        let first = throttle.accept(ProgressParams {
            file_id: "f1".to_string(),
            percent: Some(0.1),
            records_so_far: 10,
            bytes_read: None,
        });
        assert!(first.is_some(), "first progress emits immediately");
        assert_eq!(first.expect("first").records_so_far, 10);

        let second = throttle.accept(ProgressParams {
            file_id: "f1".to_string(),
            percent: Some(0.9),
            records_so_far: 90,
            bytes_read: None,
        });
        assert!(second.is_none(), "within-window progress is suppressed");

        // 最新值保留，flush 补发。
        let flushed = throttle.flush();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].records_so_far, 90, "latest value wins (§2.1)");
        assert_eq!(
            flushed[0].percent,
            Some(0.9),
            "percent carried through replacement"
        );
    }

    #[test]
    fn progress_throttle_terminal_percent_100_bypasses_window() {
        // 1 小时窗口：非终态在窗口内被抑制；percent:100 直发且清掉遗留 pending
        // （§2.1「UI 收到 percent:100 即可置完成中」——终态不得被节流吞掉）。
        let mut throttle = ProgressThrottle::with_window(Duration::from_secs(3600));
        assert!(throttle
            .accept(ProgressParams {
                file_id: "f1".to_string(),
                percent: Some(0.5),
                records_so_far: 1,
                bytes_read: None,
            })
            .is_some());
        assert!(
            throttle
                .accept(ProgressParams {
                    file_id: "f1".to_string(),
                    percent: Some(99.0),
                    records_so_far: 2,
                    bytes_read: None,
                })
                .is_none(),
            "窗口内非终态仍被抑制"
        );
        let terminal = throttle
            .accept(ProgressParams {
                file_id: "f1".to_string(),
                percent: Some(100.0),
                records_so_far: 3,
                bytes_read: None,
            })
            .expect("percent 100 直发（不节流）");
        assert_eq!(terminal.records_so_far, 3);
        // 遗留 pending（99.0）已被终态覆盖清空：flush 不再补发旧值。
        assert!(
            throttle.flush().is_empty(),
            "终态覆盖窗口内 pending（最新值语义）"
        );
    }

    #[test]
    fn progress_throttle_per_file_id_independent() {
        let mut throttle = ProgressThrottle::with_window(Duration::from_secs(3600));
        assert!(throttle
            .accept(ProgressParams {
                file_id: "f1".to_string(),
                percent: None,
                records_so_far: 1,
                bytes_read: None,
            })
            .is_some());
        // f2 不受 f1 的窗口影响。
        assert!(throttle
            .accept(ProgressParams {
                file_id: "f2".to_string(),
                percent: None,
                records_so_far: 5,
                bytes_read: None,
            })
            .is_some());
        assert!(throttle
            .accept(ProgressParams {
                file_id: "f2".to_string(),
                percent: None,
                records_so_far: 6,
                bytes_read: None,
            })
            .is_none());
    }

    #[tokio::test]
    async fn progress_throttle_respects_real_window() {
        let mut throttle = ProgressThrottle::with_window(Duration::from_millis(50));
        let first = throttle.accept(ProgressParams {
            file_id: "f1".to_string(),
            percent: Some(0.0),
            records_so_far: 0,
            bytes_read: None,
        });
        assert!(first.is_some());
        assert!(throttle
            .accept(ProgressParams {
                file_id: "f1".to_string(),
                percent: Some(0.2),
                records_so_far: 20,
                bytes_read: None,
            })
            .is_none());
        // 窗口过后：补发最新值。
        tokio::time::sleep(Duration::from_millis(60)).await;
        let after = throttle.accept(ProgressParams {
            file_id: "f1".to_string(),
            percent: Some(0.3),
            records_so_far: 30,
            bytes_read: None,
        });
        let after = after.expect("window elapsed → emit");
        assert_eq!(
            after.records_so_far, 30,
            "latest value emitted after window"
        );
    }

    /// PipelineEvent 全集 → 事件集快照（ipc-ui.md §2/§2.1）：
    /// 仅 `ParseProgress` 产生 `ab://progress`（载荷逐字段 = §2.1 `ProgressPayload`），
    /// 其余 PipelineEvent 不发线上事件（command 侧状态翻转，§2.1）。
    #[test]
    fn pipeline_event_set_snapshot_matches_ipc_ui_section2() {
        let mut throttle = ProgressThrottle::new();
        let progress = convert_pipeline(
            PipelineEvent::ParseProgress {
                file_id: "f1".to_string(),
                percent: Some(50.0),
                records_so_far: 42,
            },
            &mut throttle,
        );
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].channel, EV_PROGRESS, "§2 通道名");
        let payload = match &progress[0].payload {
            EventPayload::Progress(p) => p,
            other => panic!("expected progress payload, got {other:?}"),
        };
        assert_eq!(
            serde_json::to_value(payload).expect("serialize progress"),
            json!({
                "file_id": "f1",
                "percent": 50.0,
                "records_so_far": 42,
            }),
            "§2.1 ProgressPayload 逐字段快照（bytes_read 省略）"
        );

        // 其余 PipelineEvent 变体：零线上事件（仅 command 侧状态）。
        let others = [
            PipelineEvent::ImportStarted {
                path: "a.csv".to_string(),
            },
            PipelineEvent::ImportFailed {
                path: "a.csv".to_string(),
                reason: "boom".to_string(),
            },
            PipelineEvent::MatchCandidates {
                path: "a.csv".to_string(),
                candidates: vec![],
                needs_user_choice: true,
            },
            PipelineEvent::PluginSelected {
                path: "a.csv".to_string(),
                plugin_id: "mock".to_string(),
                by: "auto",
            },
            PipelineEvent::FileLoaded {
                file_id: "f1".to_string(),
                summary: None,
            },
            PipelineEvent::FileLoadFailed {
                file_id: "f1".to_string(),
                message: "nope".to_string(),
            },
            PipelineEvent::ParseCompleted {
                file_id: "f1".to_string(),
                records_total: 3,
                warnings: ab_pipeline::store::ParseWarnings::default(),
            },
            PipelineEvent::ParseFailed {
                file_id: "f1".to_string(),
                reason: "plugin_error".to_string(),
                detail: None,
            },
            PipelineEvent::ParseCancelled {
                file_id: "f1".to_string(),
            },
            PipelineEvent::QueryReady {
                file_id: "f1".to_string(),
            },
            PipelineEvent::FileUnloaded {
                file_id: "f1".to_string(),
            },
        ];
        for event in others {
            assert!(
                convert_pipeline(event, &mut throttle).is_empty(),
                "非 ParseProgress 的 PipelineEvent 不产生线上事件（§2.1 command 状态翻转）"
            );
        }
    }

    /// PluginMeta：状态机事件驱动状态；崩溃记录失败摘要；回 ready 清空（§4.6）。
    #[test]
    fn plugin_meta_tracks_state_and_last_error() {
        let meta = PluginMeta::new();
        assert_eq!(meta.state_of("mock"), None, "未发生事件 → 未知");
        meta.record(&HostEvent::StateChanged {
            plugin_id: "mock".to_string(),
            from: PluginProcessState::Spawning,
            to: PluginProcessState::Ready,
        });
        assert_eq!(meta.state_of("mock").as_deref(), Some("ready"));
        assert_eq!(meta.last_error_of("mock"), None);

        meta.record(&HostEvent::StateChanged {
            plugin_id: "mock".to_string(),
            from: PluginProcessState::Ready,
            to: PluginProcessState::Crashed,
        });
        assert_eq!(meta.state_of("mock").as_deref(), Some("crashed"));
        meta.record(&HostEvent::SessionTerminated {
            plugin_id: "mock".to_string(),
            exit_code: Some(1),
            summary: "exit code 1".to_string(),
        });
        assert_eq!(
            meta.last_error_of("mock").as_deref(),
            Some("exit code 1"),
            "崩溃摘要记录（§1.0 PluginInfo.last_error）"
        );

        // 回到 ready 清空失败摘要（§4.6 重载/重启后）。
        meta.record(&HostEvent::StateChanged {
            plugin_id: "mock".to_string(),
            from: PluginProcessState::Crashed,
            to: PluginProcessState::Ready,
        });
        assert_eq!(meta.last_error_of("mock"), None);

        // 优雅停机（退出码 0）不记录失败摘要。
        let clean = PluginMeta::new();
        clean.record(&HostEvent::SessionTerminated {
            plugin_id: "mock".to_string(),
            exit_code: Some(0),
            summary: "bye".to_string(),
        });
        assert_eq!(
            clean.last_error_of("mock"),
            None,
            "退出码 0 = 正常 Shutdown"
        );
    }

    /// PluginLogBuffer：尾部 N 条 + 1MB 字节预算淘汰最旧（§2.2）。
    #[test]
    fn plugin_log_buffer_tail_and_byte_budget() {
        let buffer = PluginLogBuffer::new();
        for i in 0..5 {
            buffer.push(PluginLogPayload {
                plugin_id: "mock".to_string(),
                level: LogLevel::Info,
                line: format!("line {i}"),
                ts_ms: i as i64,
            });
        }
        let tail = buffer.tail("mock", 3);
        assert_eq!(
            tail.iter().map(|p| p.ts_ms).collect::<Vec<_>>(),
            vec![2, 3, 4],
            "tail 取最新 N 条"
        );
        assert_eq!(buffer.tail("ghost", 10).len(), 0, "未知插件空");
        assert!(buffer.tail("mock", 0).is_empty(), "limit 0 → 空");

        // 字节预算：小预算下最旧行被淘汰（1024 预算：1KB 行 + 4×364B 行
        // → 淘汰 1、2、3，保留 4、5）。
        let small = PluginLogBuffer::with_budget(1024);
        small.push(PluginLogPayload {
            plugin_id: "mock".to_string(),
            level: LogLevel::Info,
            line: "x".repeat(1024),
            ts_ms: 1,
        });
        for i in 2..6 {
            small.push(PluginLogPayload {
                plugin_id: "mock".to_string(),
                level: LogLevel::Info,
                line: "y".repeat(300),
                ts_ms: i,
            });
        }
        let tail = small.tail("mock", 10);
        assert_eq!(tail.first().map(|p| p.ts_ms), Some(4), "最旧行被预算淘汰");
        assert!(tail.iter().all(|p| p.ts_ms >= 4));
    }

    /// C8.1 测试夹具：占位条目（session_id/ts_ms 由缓冲统一盖章）。
    fn entry_fixture(kind: DiagnosticKind, duration_ms: u64) -> DiagnosticEntry {
        DiagnosticEntry {
            session_id: String::new(),
            ts_ms: 0,
            kind,
            file_path: Some("a.csv".to_string()),
            file_sha256: None,
            plugin_id: Some("mock".to_string()),
            plugin_version: Some("0.1.0".to_string()),
            plugin_source: Some("Portable".to_string()),
            host_version: env!("CARGO_PKG_VERSION").to_string(),
            duration_ms,
            records_total: 0,
            received_batches: 0,
            dropped_batches: 0,
            rss_mb: None,
            error_code: None,
            message: None,
        }
    }

    /// C8.1：环形容量上限（500）——超限淘汰最旧；`recent(n)` 尾部 n 条
    /// （时间序）、n=0 空、n 超长全量。
    #[test]
    fn diagnostic_buffer_ring_capacity_500_evicts_oldest() {
        let buffer = DiagnosticBuffer::new();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.recent(10).is_empty(), "空缓冲 recent → 空");
        for i in 0..(DIAGNOSTIC_BUFFER_CAPACITY + 25) as u64 {
            buffer.record(entry_fixture(DiagnosticKind::ImportDone, i));
        }
        assert_eq!(buffer.len(), DIAGNOSTIC_BUFFER_CAPACITY, "环形上限 500");
        assert_eq!(
            buffer
                .recent(3)
                .iter()
                .map(|e| e.duration_ms)
                .collect::<Vec<_>>(),
            vec![522, 523, 524],
            "recent 取尾部（0..525 共 525 条，保留 25..524）"
        );
        assert!(buffer.recent(0).is_empty(), "n=0 → 空");
        assert_eq!(
            buffer.recent(10_000).len(),
            DIAGNOSTIC_BUFFER_CAPACITY,
            "n 超长 → 全量"
        );
    }

    /// C8.1：小容量环（测试注入）逐条淘汰最旧，顺序保持。
    #[test]
    fn diagnostic_buffer_small_ring_evicts_oldest_in_order() {
        let buffer = DiagnosticBuffer::with_capacity(3);
        for i in 0..5 {
            buffer.record(entry_fixture(DiagnosticKind::ImportDone, i));
        }
        assert_eq!(buffer.len(), 3);
        assert_eq!(
            buffer
                .recent(10)
                .iter()
                .map(|e| e.duration_ms)
                .collect::<Vec<_>>(),
            vec![2, 3, 4],
            "超限淘汰最旧"
        );
        assert_eq!(
            buffer
                .recent(2)
                .iter()
                .map(|e| e.duration_ms)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    /// C8.1：serde_json round-trip 全字段一致；kind 序列化为 snake_case
    /// （import_done/import_failed/import_cancelled/plugin_crash/query_slow）；
    /// 无值可选字段（file_sha256/rss_mb 等）省略键。
    #[test]
    fn diagnostic_entry_serde_round_trip_and_kind_names() {
        let entry = DiagnosticEntry {
            session_id: "test-session".to_string(),
            ts_ms: 1785600000123,
            kind: DiagnosticKind::ImportFailed,
            file_path: Some(r"C:\data\a.csv".to_string()),
            file_sha256: None,
            plugin_id: Some("builtin-csv".to_string()),
            plugin_version: Some("0.1.0".to_string()),
            plugin_source: Some("Portable".to_string()),
            host_version: env!("CARGO_PKG_VERSION").to_string(),
            duration_ms: 1234,
            records_total: 0,
            received_batches: 7,
            dropped_batches: 0,
            rss_mb: None,
            error_code: Some("host_backpressure".to_string()),
            message: Some("parse records lost due to backpressure".to_string()),
        };
        let value = serde_json::to_value(&entry).expect("serialize");
        assert_eq!(value["kind"], "import_failed", "kind 序列化为 snake_case");
        assert_eq!(value["session_id"], "test-session");
        assert_eq!(value["ts_ms"], 1785600000123_i64);
        assert_eq!(value["host_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["error_code"], "host_backpressure");
        assert!(
            !value.as_object().unwrap().contains_key("file_sha256"),
            "无值可选字段省略"
        );
        assert!(!value.as_object().unwrap().contains_key("rss_mb"));
        let back: DiagnosticEntry = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back, entry, "round-trip 全字段一致");

        for (kind, name) in [
            (DiagnosticKind::ImportDone, "import_done"),
            (DiagnosticKind::ImportFailed, "import_failed"),
            (DiagnosticKind::ImportCancelled, "import_cancelled"),
            (DiagnosticKind::PluginCrash, "plugin_crash"),
            (DiagnosticKind::QuerySlow, "query_slow"),
        ] {
            assert_eq!(serde_json::to_value(kind).expect("kind"), json!(name));
        }
    }

    /// C8.1：record 统一盖章 session_id（进程启动一次）与 ts_ms。
    #[test]
    fn diagnostic_buffer_stamps_session_id_and_timestamp() {
        let buffer = DiagnosticBuffer::new();
        assert!(!buffer.session_id().is_empty(), "session_id 非空");
        buffer.record(entry_fixture(DiagnosticKind::ImportCancelled, 1));
        buffer.record(entry_fixture(DiagnosticKind::ImportDone, 2));
        let all = buffer.recent(10);
        assert_eq!(all.len(), 2);
        assert!(
            all.iter().all(|e| e.session_id == buffer.session_id()),
            "全部条目同属本进程 session"
        );
        assert!(all.iter().all(|e| e.ts_ms > 0), "缓冲统一盖章时刻");
    }
}
