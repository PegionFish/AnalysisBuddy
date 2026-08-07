//! e2e harness 公共库：帧读取 / 迷你宿主会话 / 查询存储（qa-perf.md §3）。

pub mod frames;
pub mod session;
pub mod store;

pub use frames::{drain_stderr, FrameError, FrameReader, StderrRing};
pub use session::{
    dump_on_failure, FileEntryState, HostError, ParseOutcome, PluginInvocation, PluginSession,
    SessionState,
};
pub use store::{lttb, Store};
