//! AnalysisBuddy 数据管线（B 路）：导入编排、解析调度、内存存储、查询 API、
//! 会话文件。实现依据 `pipeline.md`（AnalysisBuddy-devdocs/deep-dive/）。
//!
//! 对 A 路（ab-host）的依赖以 [`PluginSession`] trait 边界声明（pipeline.md §4.1）：
//! trait 定义于本 crate 内，具体实现由 ab-app 胶水以 `HostSessionAdapter` 包装
//! ab-host 会话提供；本路开发期间以 [`mock::MockSession`] 顶替（B-01 卡）。

pub mod import;
pub mod lttb;
pub mod mock;
pub mod query;
pub mod session_file;
pub mod store;

pub use import::{reopen_files, PipelineEvent, ReopenOutcome, SessionRegistry};
pub use lttb::downsample;
pub use query::{run_query, MetricRef, QueryRequest, SeriesSlice, DEFAULT_MAX_POINTS_PER_SERIES};
pub use session_file::{
    open_session, save_session, sha256_of_file, verify_files, ChartViewState, FileVerifyStatus,
    SessionFile, SessionFileEntry, SessionFileError, YAxisScale, SESSION_FILE_VERSION,
};
pub use store::{
    AppendStats, FileState, ParseWarnings, Series, SparseSideTable, Store, StoreError,
};

use ab_protocol::types::{
    CanHandleParams, CanHandleResult, CancelParseParams, FileSummary, KeyValuesParams,
    KeyValuesResult, LoadFileParams, ParseParams, ProgressParams, RecordBatch, SchemaResult,
    UnloadFileParams,
};
use tokio::sync::mpsc;

/// A 路插件会话适配层 trait（pipeline.md §4.1）。
///
/// 边界定义于本 crate（B 路），与 A 路并行开发；Phase 3 合并时由 ab-app 提供
/// `HostSessionAdapter` 实现（包装 `ab_host::PluginSession`，见 pipeline.md §4.1
/// 「适配层约定」）。
#[async_trait::async_trait]
pub trait PluginSession: Send + Sync {
    /// 插件唯一 id（出处：host-runtime.md §7.4 `PluginSession::plugin_id()`）。
    fn plugin_id(&self) -> &str;

    /// 指标清单声明；幂等，宿主可缓存（protocol.md §2.5）。
    async fn schema(&self) -> Result<SchemaResult, SessionError>;

    /// 文件认领探测（protocol.md §2.2）。
    async fn can_handle(&self, p: CanHandleParams) -> Result<CanHandleResult, SessionError>;

    /// 文件加载，返回文件级摘要（protocol.md §2.3）。
    async fn load_file(&self, p: LoadFileParams) -> Result<FileSummary, SessionError>;

    /// 发起 parse：RecordBatch / progress 通知经 sink 流式回调（有界通道，
    /// 适配器满则丢旧，pipeline.md §4.1）；future 解析为 `records_total`。
    async fn parse_stream(
        &self,
        p: ParseParams,
        sink: mpsc::Sender<ParseEvent>,
    ) -> Result<u64, SessionError>;

    /// 取消 parse；幂等（protocol.md §3.4）。
    async fn cancel_parse(&self, p: CancelParseParams) -> Result<(), SessionError>;

    /// 游标关键值（protocol.md §2.6）。
    async fn key_values(&self, p: KeyValuesParams) -> Result<KeyValuesResult, SessionError>;

    /// 卸载文件；幂等（protocol.md §2.8）。
    async fn unload_file(&self, p: UnloadFileParams) -> Result<(), SessionError>;
}

/// parse 流式通知（pipeline.md §4.1）。
#[derive(Debug, Clone)]
pub enum ParseEvent {
    Batch(RecordBatch),
    Progress(ProgressParams),
}

/// 插件会话错误（pipeline.md §4.1 映射约定）。
///
/// `Plugin` 承载插件原样错误（← `HostError::Protocol` 的 code/message）；
/// `SessionGone` 对应进程退出 / 传输层故障（← `HostError::Transport` / `Discovery`）。
/// A 路交付后由 ab-app 适配层完成 `From<ab_host::HostError>` 映射，本 crate 不
/// 依赖 ab-host 类型。
#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    Plugin { code: i32, message: String },
    SessionGone,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Plugin { code, message } => write!(f, "plugin error {code}: {message}"),
            SessionError::SessionGone => write!(f, "plugin session gone"),
        }
    }
}

impl std::error::Error for SessionError {}
