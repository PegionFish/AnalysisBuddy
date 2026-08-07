//! ipc-ui.md §2 事件流：`HostEvent` → `ab://plugin-health` / `ab://plugin-log` /
//! `ab://progress` 三个通道的载荷转换与节流。
//!
//! 通道名常量与 `ui/src/ipc/events.ts` 字符串逐字一致（mock/real 两侧不得分叉，
//! P3-03 将对齐）。载荷字段与 §2.2 / §2.3 的 TS 接口逐字段一致。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ab_host::{HostEvent, PluginProcessState};
use ab_protocol::types::ProgressParams;

/// `ab://progress`（ipc-ui.md §2.1）。
pub const EV_PROGRESS: &str = "ab://progress";
/// `ab://plugin-log`（ipc-ui.md §2.2）。
pub const EV_PLUGIN_LOG: &str = "ab://plugin-log";
/// `ab://plugin-health`（ipc-ui.md §2.3）。
pub const EV_PLUGIN_HEALTH: &str = "ab://plugin-health";

/// progress 节流窗口（§2.1：同一 file_id 最多每 100ms 发一条）。
pub const PROGRESS_THROTTLE_MS: Duration = Duration::from_millis(100);

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
    pub fn accept(&mut self, params: ProgressParams) -> Option<ProgressParams> {
        let file_id = params.file_id.clone();
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

/// `HostEvent` → 待发事件（0..n 条）。
///
/// 映射面：`StateChanged` → health（§2.3，状态机每次迁移发一条）；
/// `StderrLine` → log（§2.2）；`Progress` → progress（§2.1 节流）。
/// 其余事件不映射：`SessionTerminated` 紧跟吸收态 `StateChanged`（徽标已被覆盖），
/// stderr 摘要走 `StderrLine`；`PluginsReloaded` / `PluginDegraded` 留待后续卡
/// 的 command 层消费，本层不虚构事件源。
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
        HostEvent::PluginsReloaded { .. }
        | HostEvent::SessionTerminated { .. }
        | HostEvent::PluginDegraded { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
