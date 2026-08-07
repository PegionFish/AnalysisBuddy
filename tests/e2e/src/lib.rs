//! ab-e2e —— AnalysisBuddy 三层递进 e2e harness 公共库（qa-perf.md §3）。
//!
//! ① `harness`：宿主进程驱动（JSON-RPC over stdio 迷你宿主）、查询 API 断言
//!    工具（时间切片 / LTTB / 排序）、stderr 环形缓冲转储（protocol §9.3 同款）；
//! ② `fixtures_ref`：F 路夹具冻结文件名常量表（qa-perf.md §2，文件名冻结）。

pub mod fixtures_ref;
pub mod harness;
