# ARM64 冒烟检查清单（P3-06）

> 状态：**待 ARM64 实机签核**（pending）
>
> 背景：本机（x64 开发机）无 ARM64 实机、无 ARM64 MSVC 工具集，P0-03 实测 CI 命中
> **cross 档**（x64 runner + MSVC amd64_arm64 交叉链接，产出 `AnalysisBuddy-aarch64-dev`
> artifact）。按 PLAN.md §8 风险 1 降级路径，cross/check 档的冒烟由人工在 ARM64 实机
> 按本清单逐项签核后回填；native 档（windows-11-arm 原生 runner）由
> `scripts/arm64-smoke.ps1` 全自动执行，本清单留痕兜底。

## 一、环境信息（实机签核时填写）

| 项 | 填写 |
|----|------|
| 机器型号 | __________ |
| Windows 版本（Winver） | __________ |
| 处理器架构（`echo %PROCESSOR_ARCHITECTURE%`） | ☐ ARM64 / ☐ ARM64（x64 仿真） |
| 构建档位（取 P0-03 最高可用档位） | ☐ native / ☐ cross / ☐ check |
| WebView2 Runtime | ☐ 已安装（版本：__________） |
| 冒烟产物 | `target/aarch64-pc-windows-msvc/release/ab-app.exe` 或 CI artifact `AnalysisBuddy-aarch64-dev` |
| 签核人 / 日期 | __________ / __________ |

## 二、执行前置（ARM64 实机操作）

1. 按 ZIP 布局摆放产物（与 P4-01 出包布局一致，host-runtime.md §1.1 Portable 源）：

   ```
   AnalysisBuddy/
   ├── ab-app.exe
   └── plugins/
       ├── builtin-csv/   (plugin.json + target/release/builtin-csv.exe)
       └── demo-tool/     (plugin.json + main.py)
   ```

2. 运行自动化冒烟（脚本自判档位；native 档全自动，cross/check 档输出
   `ARM64_SMOKE_RESULT=manual` 后走下方人工步骤）：

   ```powershell
   .\scripts\arm64-smoke.ps1 -Fixture tests\fixtures\small_with_header.csv -GuiCheck
   ```

3. 人工 GUI 冒烟：双击 `ab-app.exe` 启动宿主，按下表逐项操作并勾选结论。

## 三、逐项记录表（每项必填「结论」；「截图路径」为 GUI 档必填）

| # | 冒烟项 | 自动化代理（CLI 档，headless 可跑） | 人工操作（GUI 档） | 结论 | 截图路径 |
|---|--------|--------------------------------------|--------------------|------|----------|
| 1 | 宿主启动且主窗口出现（WebView2 就绪，ipc-ui.md §8.1 不误报） | `ab-app --smoke-host` 全绿（输出含 `smoke-host: ALL GREEN`；`-GuiCheck` 时追加 `gui_window=True`） | 双击 ab-app.exe，主窗口出现且 WebView2 内容渲染正常 | ☐ 通过 / ☐ 失败 | |
| 2 | 便携 `plugins/` 发现 builtin-csv（host-runtime.md §1.1 Portable 源优先） | 脚本静态断言：`<exe>/plugins/builtin-csv/plugin.json` 存在、manifest id == 目录名、match 含 csv、entry 可解析 | 插件列表页出现 builtin-csv v0.1.0（源 = Portable） | ☐ 通过 / ☐ 失败 | |
| 3 | 导入 fixture → parse 完成 → run_query 非空 | `ab-app --smoke-pipeline` 全绿（`import OK` + `query OK`，点数 > 0）；fixture 头列断言（timestamp/fps/frame_ms） | 拖入 `tests/fixtures/small_with_header.csv` → 解析进度走完 → 图表曲线出现 | ☐ 通过 / ☐ 失败 | |
| 4 | key_values 非空 | `--smoke-host` 输出 `key_values OK (N entries)`，N > 0 | 图表取点 → key_values 面板显示条目 | ☐ 通过 / ☐ 失败 | |
| 5 | 退出后无孤儿插件进程（protocol.md §9 第 5 条） | 脚本 `Get-Process` 断言 builtin-csv / demo-tool / mock-plugin 进程数 == 0 | 关闭宿主后任务管理器确认无残留进程 | ☐ 通过 / ☐ 失败 | |

## 四、总体结论

| 项 | 填写 |
|----|------|
| 5 项全过？ | ☐ 全部通过 / ☐ 存在失败项（#________） |
| 失败项描述 | |
| 自动化重试记录（如有） | `ARM64_SMOKE_SUMMARY` 的 `retries` 字段（已知竞态，见备注） |
| 后续动作 | ☐ 闭环缺陷后重签 / ☐ 提交 M3 出口记录（arch:"aarch64"） |

## 五、备注（2026-08-10 本机冒烟自动化 bring-up 记录）

- **已知缺陷（x64/ARM64 通用，非本卡引入）**：`HostSessionAdapter::parse_stream`
  （core/ab-app/src/host_bridge.rs）在 parse 响应到达时直接 `forward.abort()`，与通知扇出
  （`NotificationFan`，mpsc 1024）中排队未消费的 RecordBatch 存在调度竞态——偶发
  `freeze failed: records_total mismatch: declared 3, received 0`（本机 x64 复现率约
  30~50%，紧邻前序冒烟运行时更易触发）。冒烟脚本对该签名失败重试一次并在
  `ARM64_SMOKE_SUMMARY.retries` 记录；**根因闭环需后续修复卡**（建议：abort 前先排空
  通知队列，或 parse 响应与通知按序 join）。本缺陷不阻塞冒烟证据，但 P4-01 排期前应闭环。
- **本机 x64 冒烟结果（2026-08-10，x64-sanity 档）**：5/5 通过；GUI 主窗口探测成功
  （`gui_window=True`），可作为 checklist 的 x64 对偶记录。
- 本清单签核完成后回填：机器型号、Windows 版本、档位、逐项结论与截图路径，并把本文件
  状态从「待 ARM64 实机签核」改为「已签核（日期）」。

## 六、变更记录

| 日期 | 内容 | 签名 |
|------|------|------|
| 2026-08-10 | 清单创建（P3-06），状态：待 ARM64 实机签核；本机为 x64，无法执行 ARM64 冒烟 | 主代理 |
