//! ab-perf —— 性能基准 harness 公共库（qa-perf.md §4）。
//!
//! 五项指标采样器：parse 耗时（单调时钟，5 次取中位数）、RSS 峰值（Rust
//! `K32GetProcessMemoryInfo` 采样器，与 `rss_probe.ps1` 双路互验 ≤5%）、IPC 吞吐
//! （stdout 回传字节 ÷ 传输窗口）、首屏出图 / 拖拽帧率（fps 探针接入）。
//! 门槛常量与判定、报告 JSON（schema 冻结）见 `thresholds.rs` / `report.rs`。

pub mod harness;
pub mod report;
pub mod rss;
pub mod sampling;
pub mod thresholds;
