# real_plugin_suite —— 真实插件 E2E（qa-perf.md §3.2）

builtin-csv × csv 夹具、demo-tool × txt 夹具的真实插件端到端用例。

## 激活条件（跨路依赖）

| 插件 | 交付卡 | 状态 |
|------|--------|------|
| builtin-csv | D1-03（Phase 2） | plugin.json + target/release/builtin-csv.exe 就绪后自动激活 |
| demo-tool | D2-03（Phase 2） | plugin.json + main.py + Python SDK 就绪后自动激活 |

插件未构建时本套件各用例打印 `[SKIP]` 并跳过（不误报失败）；插件落地后无需改动
测试代码即自动进入断言路径。

## 用例清单

| 用例（test） | fixture × 插件 | 核心断言（qa-perf.md §3.2） |
|--------------|----------------|-----------------------------|
| `fixtures_integrity` | 8 入仓夹具（无插件） | 行数/指标列/严格递增/恰 20 畸形行/BOM/GBK 高位字节/7MB 单行 |
| `builtin_csv_matrix` | builtin-csv × 7 csv 档 | manifest 预筛 + can_handle 置信度 ≥0.8 → parse → 记录数/时间范围/指标集 == schema() |
| `demo_tool_small_txt` | demo-tool × small_txt.log | 3 指标（fps/frame_time/cpu_temp）；FRAME×3 Record；key_values 场景名/血量真实语义 |
| `disorder_20pct_sorting` | builtin-csv × disorder_20pct.csv | 20% 乱序输入 → 查询 API 时间序列不乱 |
| `multi_file_overlay_same_axis` | builtin-csv + demo-tool 同轴 | 两插件数据同一时间轴可查询且切片正确 |

## 失败定位

断言失败时 stderr 转储到 `target/test-artifacts/e2e/<case>.stderr.log`
（protocol §9.3 环形缓冲同款格式：插件日志 + 会话错误消息）。
