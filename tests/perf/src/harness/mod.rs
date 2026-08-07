//! 基准驱动（qa-perf.md §4.2）：mock 流式剧本生成、IPC 流驱动（计时打点 +
//! 字节计数）、RSS 采样线程编排。
//!
//! 说明：正式 PERF-01/02/04 基线以 A/B 路宿主 + D1-03 builtin-csv 就绪后为准；
//! 本 harness 目前以 mock-plugin 流式回传（echo 口径，与 P1-01 scratch/echo-*
//! 同方法论）测量 IPC 吞吐上界与 RSS，作为本地基线供 P3-05 报告。

pub mod scriptgen;
pub mod stream;

pub use scriptgen::gen_mock_script;
pub use stream::{assert_stream_ok, run_stream, StreamStats};
