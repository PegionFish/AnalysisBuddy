# tools/fuzz —— 协议 fuzz（任务卡 I2，挑战任务 §2.7）

针对插件协议**读入路径**的畸形输入 fuzz，不变量：不 panic、内存/栈有界。
被测代码不改一行（`core/ab-host/src/rpc.rs` 帧层 + `core/ab-protocol` 入站类型）。

- **target① `frame_read`**：随机字节流 → `ab_host::rpc::FrameReader`
  （8 MB 长度上限、UTF-8/JSON 校验），帧内容再经 run_read_loop（§4.3/§4.4）
  **同构分流**：id 帧 → outcome 合成 + 应答类型反序列化；method 帧 →
  progress/RecordBatch 反序列化 + `SeqValidator` 连续性校验。错误处置表与
  session.rs 同构（InvalidJson 仅首次 Continue）——不可在错误后无条件续读，
  否则滞留缓冲会被反复重扫（O(n²)，仅 harness 伪影，真实会话 Stop）。
- **target② `rpc_types`**：随机字节/JSON → ab-protocol 全部 19 个入站类型的
  serde 矩阵（from_slice + from_value 双入口），定向覆盖深嵌套（serde_json
  128 层递归上限必须报错，100k 层在 harness 内断言）、1e999、重复键、
  NaN/±∞ 位模式。

## 目录

- `src/lib.rs`：两个 target 的共享被测实现（harness 内断言即契约）+ 自检测试；
- `src/bin/soak.rs`：**稳定工具链**随机语料循环驱动（种子突变 + 定向用例，
  CI smoke / 无 nightly 时的等价降级路径）；
- `fuzz/`：cargo-fuzz crate（libFuzzer 覆盖率引导，nightly + ASan，独立 workspace）；
- `corpus/frame_read/`、`corpus/rpc_types/`：种子语料（仅 `*seed*` 入库；
  libFuzzer 运行产物不入库）+ 运行统计与最小复现归档。

## 运行

```bash
# 主路径：libFuzzer（前置：rustup toolchain install nightly；cargo install cargo-fuzz --locked）
cd tools/fuzz
cargo +nightly fuzz run frame_read corpus/frame_read -- -max_total_time=330 -max_len=1048576 -rss_limit_mb=2560 -timeout=25
cargo +nightly fuzz run rpc_types  corpus/rpc_types  -- -max_total_time=330 -max_len=262144  -rss_limit_mb=2560 -timeout=25

# 稳定版等价（5 万次循环 × 2 target + 4 组定向用例；stdout 输出 JSON 统计）
cargo run --release --bin soak -- --iterations 50000 --seed 20260928 --quiet

# 自检（含 8MB 上限 / 100k 深嵌套 / u64::MAX seq 定向断言）
cargo test
```

崩溃处置：`cargo +nightly fuzz tmin fuzz/artifacts/<target>/crash-*` 最小化后，
将最小复现 + 触发说明归档到 `corpus/crashes/`，报告编排者（修复走产品代码，
本目录只发现不修复）。CI：`.github/workflows/fuzz.yml`。
