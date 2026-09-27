//! ab-plugin-rt —— AnalysisBuddy Rust 插件运行时（最小 SDK）。
//!
//! 抽取自 `plugins/builtin-csv` 与 `plugins/aibench-llama` 的 main.rs 逐字相同的
//! 传输样板（G2，迁移前 diff ~141 行 × 多份拷贝）：CLI 参数（`--stdio` 约定）、
//! stdin NDJSON 读循环（8MB 先行校验、CRLF/UTF-8 帧纪律，与 ab-protocol §1.2/§1.3
//! 一致）、请求解析 → handler 分发、响应/通知写出（发送锁 + 整行原子写 + 每帧
//! flush）、shutdown/退出码语义（EOF → 0，协议违规 → 1，参数错 → 2）。
//!
//! # 用法
//!
//! 插件只实现 [`Plugin`]（业务 handler 映射），其余全部由 runtime 承担：
//!
//! ```no_run
//! use ab_plugin_rt as rt;
//! use ab_protocol::types::{CanHandleParams, CanHandleResult, FileSummary, KeyValuesResult, LoadFileParams, MetricDef};
//! use std::process::ExitCode;
//! use std::sync::atomic::AtomicBool;
//!
//! struct MyPlugin;
//!
//! impl rt::Plugin for MyPlugin {
//!     type Loaded = my_engine::Loaded;
//!
//!     fn id(&self) -> &str { "my-plugin" }
//!     fn name(&self) -> &str { "My Plugin" }
//!     fn version(&self) -> &str { env!("CARGO_PKG_VERSION") }
//!     fn can_handle(&self, p: &CanHandleParams) -> CanHandleResult { todo!() }
//!     fn load_file(&self, p: &LoadFileParams) -> Result<Self::Loaded, String> { todo!() }
//!     fn summarize(loaded: &Self::Loaded) -> FileSummary { todo!() }
//!     fn metrics(loaded: &Self::Loaded) -> Vec<MetricDef> { todo!() }
//!     fn parse(loaded: &mut Self::Loaded, sink: &mut dyn rt::Sink, cancel: &AtomicBool)
//!         -> Result<u64, rt::Cancelled> { todo!() }
//!     fn key_values(loaded: &Self::Loaded, timestamp_ms: i64) -> KeyValuesResult { todo!() }
//! }
//!
//! # mod my_engine { pub type Loaded = (); }
//! # fn build() -> MyPlugin { MyPlugin }
//!
//! fn main() -> ExitCode {
//!     rt::plugin_main(
//!         &rt::PluginMeta {
//!             log_tag: "my-plugin",
//!             help_text: "my-plugin -- AnalysisBuddy plugin\n\nUSAGE:\n    my-plugin --stdio",
//!             usage_line: "USAGE: my-plugin --stdio",
//!         },
//!         build, // cfg 加载 / 指纹读取等启动副作用在此完成
//!     )
//! }
//! ```
//!
//! # 行为契约（与抽取前逐字节等价，golden 转录钉住）
//!
//! - 响应帧字段顺序 `jsonrpc,id,result|error`；通知 `jsonrpc,method,params`；
//! - stdout 只出协议帧：单行 + `\n`，发送锁下整行原子写，每帧 flush；日志走 stderr；
//! - `initialize.protocol_version` 回显 [`ab_protocol::PROTOCOL_VERSION`]（G1；
//!   v1 时该键被 serde skip-if-default 省略，与历史报文逐字节兼容）；
//! - parse 单槽位：进行中再 parse → `-32001`；`cancel_parse` 经共享原子标志生效；
//! - schema 返回最近一次 load 冻结的 metric 集（unload 后仍可查）；
//! - 退出码：stdin EOF / shutdown 应答后 → 0；帧违规 / handler 致命错 → 1；
//!   未知参数 → 2（`--help` → stdout 帮助后 0）。

pub mod args;
pub mod dispatch;
pub mod frame;
pub mod sink;

pub use args::{parse_stdio_args, PluginMeta, StdioArgs};
pub use dispatch::{parse_params, plugin_main, run, Cancelled, Plugin};
pub use frame::{write_frame, FrameReader, MAX_LINE_BYTES};
pub use sink::{Out, Sink, StdioSink};
