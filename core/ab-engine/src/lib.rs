//! AnalysisBuddy 纯 Rust 引擎核心（headless embedding，M1 自 `core/ab-app`
//! 机械提取）：导入编排（`pipeline_bridge::ImportCoordinator`）、宿主会话
//! 适配（`host_bridge::HostSessionAdapter`）、事件转换（`events::convert*`）、
//! IPC 错误映射（`ipc_errors`）、更新网络（`network`）、模块/预设命令逻辑
//! （`commands::*_logic`）与无头装配证明（`smoke`）。
//!
//! 本 crate 不依赖 Tauri/WebView（依赖树无 tauri/windows-sys/winreg）：
//! 路径等环境敏感输入一律由调用方注入（见 [`paths::EnginePaths`]），
//! 桌面壳 `core/ab-app` 仅保留 Tauri 包装层并整体再导出本 crate 公共项。

pub mod commands;
pub mod events;
pub mod host_bridge;
pub mod ipc_errors;
pub mod network;
pub mod paths;
pub mod pipeline_bridge;
pub mod smoke;

// 路径注入 seam 常用入口（宿主构造后传入引擎逻辑；headless/Linux 宿主
// 用 `EnginePaths::linux_default()`）。
pub use paths::EnginePaths;

// 内建模块 id 清单（build.rs 扫描仓库 plugins/ 生成，任务 4 机制随引擎
// 迁入）：安装冲突判定与卸载保护依赖此常量，缺接线即运行时保护失效。
// core/ab-app 经 `pub use ab_engine::BUILTIN_PLUGIN_IDS` 转发（其
// gen/builtin_ids.rs 由 ab-app build.rs 自本 crate 产物复制，供其
// tests/builtin_ids_test.rs include!）。
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/gen/builtin_ids.rs"));
