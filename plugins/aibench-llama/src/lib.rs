//! aibench-llama 库入口：供集成测试直接引用引擎（不经过 stdio 子进程）。
//! G2：ndjson 帧层移入 `core/ab-plugin-rt`（原同构副本删除）。

pub mod aibench;
pub mod csvline;
pub mod engine;
