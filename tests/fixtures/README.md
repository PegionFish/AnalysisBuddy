# tests/fixtures —— 12 项夹具矩阵

夹具矩阵与验收目标映射的**防漏测检查表**（qa-perf.md §2，照录）。文件名冻结，e2e
代码按名引用（`tests/e2e/src/fixtures_ref.rs` 常量表）。全部小体积夹具**入仓**；
大档夹具（50/100MB）**不入仓**，由 `tests/scripts/gen-large-fixtures.ps1` 按固定
seed 现生成到 `tests/.generated/`（gitignore）并经哈希比对保证复现。

## 12 项夹具矩阵

| fixture | 来源 | 内容 | 校验目的 |
|---------|------|------|----------|
| `small_with_header.csv` | 入仓（~8KB） | 200 行、3 指标、表头 `timestamp,fps,frame_ms,mem_mb`、时间严格递增 | 主路径冒烟；同时是 `plugin check` 内置 fixture 的同源文件（docs-validator.md §3.3） |
| `small_no_header.csv` | 入仓 | 同上但无表头，首行即数据 | builtin-csv 无表头探测与列推断（D1 DoD） |
| `small_txt.log` | 入仓 | txt 格式 200 行、含三级 level | demo-tool 路径冒烟；`Record.level` 覆盖 |
| `empty.csv` | 入仓（0 字节） | 空文件 | `load_file` 成功但 `parse` 产出 0 条；UI 空态、除零防护 |
| `malformed_lines.csv` | 入仓 | 200 行中 20 行畸形：缺列、非数值 value、时间戳非 ISO、超长单行（>1KB 但 <8MB） | 插件容错：跳过坏行并计数，不得整盘 `-32003`；坏行率报告入查询 API |
| `enc_utf8_bom.csv` | 入仓 | 带 UTF-8 BOM 的 small 变体 | BOM 宽松处理（protocol.md `FileSummary.note` 约定「检测到 BOM」） |
| `enc_gbk.csv` | 入仓 | GBK 编码中文备注列 | 非 UTF-8 降级路径：宽松解码 U+FFFD 替换而非崩溃（protocol.md §2.2 head_sample 同款策略） |
| `bench_10mb.csv` | loggen `--size-target` 缺省按 rows、`--seed 10` | 10MB 档 | PR 门禁 perf-smoke（qa-perf.md §5） |
| `bench_50mb.csv` | loggen `--seed 50` | 50MB 档 | nightly 中档（PLAN.md Phase 3 性能验证） |
| `bench_100mb.csv` | loggen `--size-target 100MB --seed 100` | 100MB 档、`--disorder 0.02` | 硬性门槛基准（qa-perf.md §4）；乱序 2% 贴近真实 |
| `disorder_20pct.csv` | loggen `--disorder 0.2 --seed 21` | 小体积、20% 乱序 | 乱序极限：排序正确性与图面不乱 |
| `single_long_line.csv` | 入仓 | 含一行 ~7MB 的合法行 | 逼近 8MB 帧上限但不触发（protocol.md §1.3 边界）；要求插件调小含 raw_line 的批量 |

> 大档夹具（bench_10mb / bench_50mb / bench_100mb / disorder_20pct）不入仓：CI 先跑
> `loggen --seed <固定值>` 生成到 `tests/.generated/`，哈希比对确保复现。

## 防漏测检查表（验收目标 ↔ fixture ↔ 所属层）

| 验收目标 | 主用 fixture | 所属层 |
|----------|----------------|--------|
| 主路径 + validator 内置回放 | `small_with_header.csv` | e2e / validator |
| 无表头推断 | `small_no_header.csv` | e2e（D1） |
| level 字段链路 | `small_txt.log` | e2e（D2） |
| 空文件与除零防护 | `empty.csv` | e2e |
| 坏行容错与计数 | `malformed_lines.csv` | e2e |
| BOM / GBK 编码降级 | `enc_utf8_bom.csv` / `enc_gbk.csv` | e2e |
| PR 性能门禁 | `bench_10mb.csv` | perf-smoke |
| 硬性门槛 PERF-01/02/04 | `bench_100mb.csv` | perf-nightly |
| 乱序排序正确性 | `disorder_20pct.csv` | e2e |
| 8MB 帧边界 | `single_long_line.csv` | e2e |

## 小夹具生成命令（loggen，F-01）

全部小夹具由 `tools/loggen`（`cargo build --release`）按固定 seed 生成，内容与
哈希均已冻结（见下）；需要重新生成时逐条执行：

```powershell
$lg = "tools/loggen/target/release/loggen.exe"
$fx = "tests/fixtures"
& $lg --rows 200 --metrics 3 --size-target auto --format csv --seed 200 -o "$fx/small_with_header.csv"
& $lg --rows 200 --metrics 3 --size-target auto --format csv --seed 201 --no-header -o "$fx/small_no_header.csv"
& $lg --rows 200 --metrics 3 --size-target auto --format txt --seed 202 -o "$fx/small_txt.log"
& $lg --rows 200 --metrics 3 --size-target auto --format csv --seed 203 --corrupt 0.10 -o "$fx/malformed_lines.csv"
& $lg --rows 200 --metrics 3 --size-target auto --format csv --seed 204 --encoding utf8bom -o "$fx/enc_utf8_bom.csv"
& $lg --rows 200 --metrics 3 --size-target auto --format csv --seed 205 --encoding gbk -o "$fx/enc_gbk.csv"
# empty.csv：0 字节空文件（直接创建）；single_long_line.csv：7MB 单行（见 gen 脚本内注释）
```

## 冻结哈希（SHA-256，同 seed 逐字节复现基准）

| fixture | 字节数 | SHA-256 |
|---------|--------|---------|
| `small_with_header.csv` | 8747 | `293774d4f9ffc733d31ae7f91092f14a4cd767b4aa51bfe843bf9e52518e9c8c` |
| `small_no_header.csv` | 8743 | `745f2da93fe3bc652c3419ebc4c2ef1a6409e912d647d491055636b0ba41a628` |
| `small_txt.log` | 15978 | `8a14ba04b8cb3263742df659af9032641fa912552565dddc21eb67093c2fbe42` |
| `empty.csv` | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `malformed_lines.csv` | 25056 | `10802a265359bee71bb80f387c5c9e810e071d81166dffb6ffd2c53bb04e0d91` |
| `enc_utf8_bom.csv` | 8700 | `632f6d5671752bb264c88547ceb47de0c91a67bedb63e9ed06856f6529308f0f` |
| `enc_gbk.csv` | 9799 | `96d8be7fd5f170662a517419dcb213f738401e1f47d19c19da97c211b2719f6a` |
| `single_long_line.csv` | 7340032 | `9e47516e593ed7cc2289cf4ee556128fc23279898bfd664f72b89a1df7f613bc` |

## 大档夹具生成（`tests/scripts/gen-large-fixtures.ps1`）

```powershell
& tests/scripts/gen-large-fixtures.ps1   # 生成 + 哈希比对 + 体积/计时断言（PS 5.1 兼容）
```

- `bench_10mb.csv`（`--seed 10`）：~10MB；perf-smoke 档
- `bench_50mb.csv`（`--seed 50`）：~50MB；nightly 中档
- `bench_100mb.csv`（`--size-target 100MB --seed 100 --disorder 0.02`）：~100MB；PERF-01/02/04 基准
- `disorder_20pct.csv`（`--disorder 0.2 --seed 21`）：小体积 20% 乱序

生成到 `tests/.generated/`（.gitignore，不入仓），哈希与冻结值比对不通过即失败。

## 格式约定（loggen 输出）

- **csv**：表头固定首列 `timestamp`（RFC3339 毫秒精度，`2026-08-01T00:00:00.000Z`），
  后列为指标名（`fps,frame_ms,mem_mb,...`），`match.header_fingerprints` 可命中
  `"timestamp,"` → builtin-csv 零配置解析（sdk-plugins.md §3）。
- **txt**：demo-tool 约定行格式（sdk-plugins.md §4.1）：
  - FRAME 行 `ISO FRAME fps=.. frame_ms=.. cpu_temp=.. level=info|warn|error`（85/12/3 分布）；
  - STATE 行（每 40 行）`ISO STATE scene=.. hero_hp=.. stamina=..`（key_values 语义）；
  - EVENT 行（每 100 行）`ISO EVENT <name> reason=".." level=..`（annotate 语义）。
- 时间戳基础序列严格递增（`start + i × interval + jitter(0~interval/2)`）；`--disorder`
  只交换时间戳、不修复顺序（验证宿主/插件乱序容忍与排序正确性）。
- 行尾统一 LF（`\n`，无 `\r`）；编码变体 utf8 / utf8bom（EF BB BF）/ gbk（encoding_rs）。
- 生成性能：100MB ≤60s（CI 计时断言；本机实测 ~2s）。
