//! AnalysisBuddy 插件运行时（A 路）：插件发现、进程生命周期、JSON-RPC 帧、
//! 超时与健康监控。实现依据 `host-runtime.md`（AnalysisBuddy-devdocs/deep-dive/）。

pub mod discovery;
pub mod manifest;
pub mod rpc;
pub mod session;
pub mod spawner;

pub use discovery::{
    DiscoveredPlugin, DiscoveryOutcome, InvalidPlugin, PluginRegistry, PluginSource, ShadowedPlugin,
};
pub use manifest::{load_manifest, resolve_entry, validate, DiscoveryError, ResolvedEntry};
pub use rpc::{
    from_outcome, run_read_loop, FrameDisposition, FrameError, FrameReader, NotificationFan,
    NotificationHandler, PluginNotification, ReadLoopError, RpcChannel, RpcOutcome, SeqValidator,
    StdoutFrameReader,
};
pub use session::{
    ChildProcessRegistry, PluginProcessState, PluginRuntime, PluginSession, SmEvent, StateMachine,
};
pub use spawner::{PluginSpawner, SpawnedChild};

/// 宿主错误（§8.1，映射 protocol.md §4）。
#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    /// 插件返回的原样 JSON-RPC error（原样透传 UI）。
    Protocol {
        code: i32,
        message: String,
        data: Option<serde_json::Value>,
    },
    /// 宿主本地：管道 / 进程层故障。
    Transport(String),
    /// 发现阶段错误。
    Discovery(DiscoveryError),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Protocol { code, message, .. } => {
                write!(f, "JSON-RPC error {code}: {message}")
            }
            HostError::Transport(m) => write!(f, "transport error: {m}"),
            HostError::Discovery(e) => write!(f, "discovery error: {e}"),
        }
    }
}

impl std::error::Error for HostError {}

impl HostError {
    /// 宿主合成错误（§8.1）：进程退出时在途请求 → `-32003` / `"plugin process exited"`。
    pub fn process_exited() -> Self {
        Self::Protocol {
            code: ab_protocol::errors::ERR_PARSE_FAILED,
            message: "plugin process exited".to_string(),
            data: None,
        }
    }

    /// 宿主合成错误（§8.1）：帧层致命错终止会话 → `-32700`（§4.2 帧错误表措辞）。
    pub fn frame_error(message: &str) -> Self {
        Self::Protocol {
            code: ab_protocol::errors::ERR_PARSE_ERROR,
            message: message.to_string(),
            data: None,
        }
    }
}

/// 宿主事件流（§7.7，宿主本地）。
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// 插件重载完成，附带发现明细（§1.5）。
    PluginsReloaded {
        plugins: Vec<DiscoveredPlugin>,
        invalid: Vec<InvalidPlugin>,
        shadowed: Vec<ShadowedPlugin>,
    },
    /// 状态机每次成功转移（§3.2）。
    StateChanged {
        plugin_id: String,
        from: PluginProcessState,
        to: PluginProcessState,
    },
    /// 插件 parse 进度（protocol.md §3.3）。
    Progress(ab_protocol::types::ProgressParams),
    /// stderr 新行（protocol.md §9.3；A-03 落地捕获）。
    StderrLine {
        plugin_id: String,
        ts_ms: i64,
        line: String,
    },
    /// 会话终止（退出码 0 且处于 Draining = 正常 Shutdown，其余为崩溃）。
    SessionTerminated {
        plugin_id: String,
        exit_code: Option<i32>,
        summary: String,
    },
    /// 插件降级（如 schema 超时禁用指标树入口）。
    PluginDegraded { plugin_id: String, reason: String },
}
