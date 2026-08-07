# scratch/ —— 原型与临时工具区（用毕即冻结）

本目录**不属于 Cargo workspace**（见根 Cargo.toml `[workspace] members`），CI 不构建、不 lint。
每个子目录都是独立 crate：`cargo build --release` 需在各自目录内执行，
或 `cargo run --manifest-path scratch/<name>/Cargo.toml --release`。

- `echo-plugin/`：echo 证伪原型插件端（JSON-RPC 2.0 over stdio，合成 RecordBatch + progress）
- `echo-driver/`：echo 证伪原型宿主端（吞吐实测，monotonic clock）
- `run-bench.ps1`：batch ∈ {1000,2000,4000,8000} × 10 万条 × 3 次矩阵脚本

> 结论文档在仓库外：`AnalysisBuddy-devdocs/deep-dive/notes/echo-bench-findings.md`（不入 git）。
> **原型用毕即冻结**：本目录代码是 P1-01 一次性实测产物，任何后续开发不得依赖其实现，
> 也不得将本目录加入 workspace / CI。
