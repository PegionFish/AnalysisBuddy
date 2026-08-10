# tests/perf —— 性能基准 harness（qa-perf.md §4/§5）

五项指标采样器、PERF-01..04 硬性门槛判定、报告 JSON（schema 冻结）入仓。

## 运行

```powershell
# 单测（离线可跑；含 RSS 双路互验 ≤5%）
cargo test -p ab-perf --test perf_harness

# 生成大档夹具（bench_10mb/50mb/100mb + disorder_20pct）
powershell -NoProfile -File tests/scripts/gen-large-fixtures.ps1

# 实测基线（release + LTO；显式 ignored；需要 mock-plugin release 构建）
cargo test -p ab-perf --release --test perf_bench -- --ignored --nocapture
Get-ChildItem tests/perf/reports/*.json | Select-Object Name,Length

# 真实插件基准（P3-05：builtin-csv × bench_10/50/100mb.csv，5 次中位数 + 预热 1 次丢弃；
# 需 tests/.generated/ 夹具：先跑 tests/scripts/gen-large-fixtures.ps1）
cargo test -p ab-perf --release --test perf_real_bench -- --ignored --nocapture

# 全量编排（生成夹具+哈希校验 → mock 交叉 → 真实插件 → 报告校验+门禁）
powershell -NoProfile -ExecutionPolicy Bypass -File tests/perf/run_full_bench.ps1
```

> 报告文件名共用 `perf-report-<date>-<sha>.json`：`perf_bench`（mock）与
> `perf_real_bench`（真实插件）同日同 sha 产物同名，**后者须最后运行**（
> `run_full_bench.ps1` 保证顺序），入仓报告为真实插件基线。

perf-smoke 模式（10MB 等比折算门槛，parse ≤1s / RSS ≤300MB）：

```powershell
$env:AB_PERF_MODE = "smoke"
cargo test -p ab-perf --release --test perf_bench -- --ignored --nocapture
```

未设置（或非 `smoke`）时走 PERF-01..04 硬性门槛。PERF-03 探针不可用（`gpu=null`、
`drag_fps_p95=null`）时报告记 `thresholds_pass[3]=false` 表示「未测量」，门禁
（`report::gate_failures` / perf-smoke.yml Gate step）按 metrics 跳过该门槛。

## 门槛表（qa-perf.md §4.1，冻结，任一不达标阻塞 M3）

| 门槛 | 指标 | 门槛 | 测量条件 |
|------|------|------|----------|
| `PERF-01` | 100MB 解析耗时 | ≤10s | release、冷插件进程、含 load_file+parse 全程（builtin-csv × bench_100mb.csv） |
| `PERF-02` | 宿主内存峰值 | ≤1GB（RSS） | 解析完成 + 全指标查询驻留时刻 |
| `PERF-03` | dataZoom 拖拽帧率 | ≥30fps（5s 窗口 95 分位） | 100MB 上图、>5 万点触发 LTTB |
| `PERF-04` | JSON IPC 吞吐 | ≥20MB/s | 回传总字节 ÷ 首帧到末批耗时 |

## 采样纪律（§4.2/§4.3）

- 固定电源模式「最佳性能」；Windows Defender 实时扫描排除测试目录；
- 每个门槛连续 **5 次取中位数**判定，单次不作数；正式采样前预热 1 次丢弃；
- release + LTO 构建（`[profile.release] lto=true`）；**debug 数据不入报告**；
- RSS 双路互验：Rust `K32GetProcessMemoryInfo` 采样器与 `rss_probe.ps1`
  （`Process.WorkingSet64`）偏差 ≤5%；
- 报告 `arch` 字段区分架构；ARM64 仅构建 + 冒烟，性能数据在实机补采。

## 失败分诊流程（F-03 DoD；不得私改门槛值）

1. **本地复现**：基准机上跑 `cargo test -p ab-perf --release --test perf_bench -- --ignored`，
   排除 CI runner 噪声（共享 runner 波动大，用中位数 + 15% 回归阈值判定）。
2. **按 §4.1 处置列定位**：
   - PERF-01 超标 → 定位插件解析瓶颈；确属 stdio IPC 瓶颈 → 触发 v1.1
     binary_sidecar 旁路预案评审（protocol.md §8.2）；
   - PERF-02 超标 → 审查 raw_line 抽样率与 Record 布局；
   - PERF-03 超标 → 审查 LTTB 降采样阈值与 ECharts 配置；
   - PERF-04 超标 → 先调批量（protocol.md §3.2 建议 1000~8000 条区间上探），
     仍不达标 → 触发 §8.2 旁路预案。
3. **上报主代理**：确认为架构级瓶颈时附本地复现数据 + 两次报告 JSON diff，
   启动协议修订评审；**严禁私自修改门槛值**（门槛冻结在 `src/thresholds.rs`）。

## 报告与回归

- 每次运行产出 `perf-report-<date>-<sha>.json`（schema 冻结见 `src/report.rs`）；
- nightly/tag 报告提交入仓 `tests/perf/reports/`（保留最近 90 天，CI 滚动清理）；
  PR 档报告仅作 artifact 不入库；
- 回归判定：与上次入仓报告对比任一指标劣化 >15% → 自动开 issue（附两次 JSON
  diff；演练记录见 `reports/regression-drill.md`）。

## 状态（P3-05 已交付：真实插件基线入仓）

- 2026-08-10 入仓报告：`reports/perf-report-2026-08-10-<sha>.json` = builtin-csv ×
  `bench_100mb.csv`（release+LTO 冷进程，`load_file` 发出 → `records_total` 到达全程，
  5 次中位数 + 预热 1 次丢弃）；PERF-01/02/04 全过，PERF-03 因无图形探针（Tauri dev
  未起、`gpu=null`）记未测量（`thresholds_pass[3]=false`，门禁按 `report::gate_failures`
  跳过）。
- 三档实测中位数（同机同会话，入仓报告同源）：10MB parse 918ms / RSS 19.5MB /
  IPC 56.5MB/s；50MB parse 4409ms / RSS 141.3MB / IPC 59.1MB/s；100MB parse 9245ms /
  RSS 278.9MB / IPC 56.3MB/s。100MB 档 parse 距 PERF-01（≤10s）余量约 7.5%，后续
  优化可关注 RecordBatch 序列化/读取路径（IPC 窗口 ~500MB 回传）。
- mock 交叉基线（echo 口径，与 F 路 2026-08-07 报告同方法论）：parse 148.5ms、
  RSS 111.7MB、IPC 69.7MB/s——数值与 F 路报告一致，无回归。
- 机器：`machine` 字段记录 CPU 型号（`Intel64 Family 6 Model 198 Stepping 2,
  GenuineIntel`，即 Core Ultra 7 270HX Plus，32GB 内存）。
- 注意：本报告起基线从 mock 10MB 切换为真实插件 100MB，与 2026-08-07 报告
  **不同口径**，不做数值直接对比；后续 nightly 报告在同口径下回归。
