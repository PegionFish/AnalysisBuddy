//! core/ab-server：AnalysisBuddy 无头 HTTP+SSE 服务（M2 任务 3）。
//!
//! axum 承载 ab-engine（M1 提取的纯 Rust 引擎核心），把桌面 19 个 IPC
//! 命令映射为 REST 端点（`/api/v1`），把 `events::convert` /
//! `convert_pipeline` 产出的事件改为 SSE 推送。响应体逐字段复用
//! `ab_engine::commands` 的 DTO（serde 形状与桌面 ipc-ui.md §1.0 一致）；
//! 错误恒为 `{"error": <IpcError>}`，HTTP 状态码按 [`error::status_for`]
//! 映射。规格见 `docs/spec/http-api-v1.md`，快速上手见
//! `docs/developer-guide/10-server-mode.md`。
//!
//! 模块划分：
//! - [`args`]：CLI 解析与 [`ab_engine::paths::EnginePaths`] 平台默认公式；
//! - [`state`]：引擎装配（smoke.rs 同款）与 [`state::AppState`]；
//! - [`routes`]：路由表 + 处理器 + Bearer 认证中间件；
//! - [`jobs`]：导入任务注册表（队列/并发闸/协作式取消）；
//! - [`hub`]：事件广播 hub + 每订阅者节流的 SSE 流；
//! - [`error`]：`IpcError` → HTTP 状态映射与统一错误包络。

pub mod args;
pub mod error;
pub mod hub;
pub mod jobs;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use hub::EventHub;
pub use jobs::{JobRegistry, JobState, JobStatusDto};
pub use state::{assemble, AppState, AssembleOptions};

/// 协议版本（`/api/v1`；health 端点回显）。
pub const PROTOCOL_VERSION: u32 = 1;

/// `query/series` 缺省 `max_points_per_series`（与桌面默认一致）。
pub const DEFAULT_MAX_POINTS_PER_SERIES: usize = 4000;

/// `max_points_per_series` 服务端硬上限（超出 → 400 invalid_arg）。
pub const MAX_POINTS_PER_SERIES_CAP: usize = 50_000;

/// 请求体上限（64MB：上传 CSV/插件 ZIP 需要 headroom；ZIP 自身另有
/// 引擎侧 100MB 限额）。
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
