# 发布验收记录（P4-02 / qa-perf.md §6）

> 本文件为 `v*` tag 触发的发布流水线（`.github/workflows/release.yml`）的**干净机验收记录归档**：
> 验收依据 qa-perf.md §6 的 7 项清单 × 双架构逐项留痕（结论 / 截图路径 / 虚机信息）。
> 任一验收项不通过即阻塞 M4 出口；本文件随正式 tag 一并归档入仓。
>
> **环境决策（用户确认，2026-08-10）**：ARM64 不做实机测试，仅保留构建目标——ARM64 行按
> 「CI 构建产物（cross 档已验证）+ 人工冒烟降级签核」执行（PLAN.md §8 风险 1 / 第 7 项降级路径）。
> 本环境无干净 Windows 虚机：**x64 本机预演结果如实填入，虚机正式验收待用户执行**。

## 1. 验收环境（虚机验收时逐台填写）

| 字段 | x64 虚机 | ARM64 虚机 |
|------|----------|-----------|
| 机器 / 型号 | __________ | __________ |
| Windows 版本（Winver） | __________ | __________ |
| 架构（`echo %PROCESSOR_ARCHITECTURE%`） | AMD64 | ARM64 |
| WebView2 Runtime 版本 | __________ | __________ |
| 被测 ZIP | `AnalysisBuddy-{version}-x86_64.zip` | `AnalysisBuddy-{version}-aarch64.zip` |
| 验收人 / 日期 | __________ | __________ |

## 2. 七项验收清单（双架构）

| # | 验收项 | 通过判据 | x64 状态 | ARM64 状态 |
|---|--------|----------|----------|-----------|
| 1 | 解压即用（含空格/中文路径） | 5s 内出主窗口、无运行时缺失弹窗 | ✅ 本机预演通过（普通路径；空格/中文路径待虚机） | ⏳ 待虚机 |
| 2 | 便携 plugins/ 识别 | 插件页列出 builtin-csv + demo-tool 且健康 | ✅ 本机预演（宿主发现/健康链路全绿；插件页展示待虚机） | ⏳ 待虚机 |
| 3 | WebView2 缺失引导 | 未装机上展示双语下载引导，不白屏崩溃 | ⏳ 待虚机（本机已装 WebView2，无法触发缺失路径） | ⏳ 待虚机 |
| 4 | 拖入自建插件仓库（含 .git/） | 重载即发现且零告警（protocol.md §7.1 第 3/5 条） | ⏳ 待虚机（需 UI 拖放交互） | ⏳ 待虚机 |
| 5 | 用户目录插件 | %APPDATA% 插件并存列出，id 冲突按优先级提示 | ⏳ 待虚机（发现机制有单测覆盖，行为验收待虚机） | ⏳ 待虚机 |
| 6 | 端到端走查 | 导入 bench_10mb → 上图 → key_values → 保存/重开会话自动重解析 | 🔶 部分预演（管道全链路全绿；UI 走查被 D1-D4 阻塞，见 §4） | ⏳ 待虚机 |
| 7 | 双架构一致性 | x64/ARM64 结论一致；ARM64 允许「出包后人工冒烟」降级 | ✅ 产物 + 清单断言通过 | 🔶 降级签核待执行（见 §5） |

图例：✅ 已通过 / 🔶 部分通过或降级 / ⏳ 待虚机验收。

## 3. 逐项记录（x64 本机预演，2026-08-10，机器：开发机 x64，WebView2 已装）

### 项 1 — 解压即用
- 复测方式：`Expand-Archive dist/AnalysisBuddy-0.1.0-x86_64.zip` → `dist/smoke-x64`；启动 5s 后检查主窗口句柄，再杀进程。
- 结果：主窗口 5s 内出现（`hwnd=` 非零，见 §6 输出），无运行时缺失弹窗。
- 既有证据：P4-01 打包脚本内建冒烟 `BUNDLE_SMOKE=x86_64:ok`（`dist/smoke-x86_64` 为当时解压副本）。
- 空格/中文路径场景：本机未测，待虚机补录。

### 项 2 — 便携 plugins/ 识别
- ZIP 清单断言：`plugins/builtin-csv/`（plugin.json + 架构 exe）、`plugins/demo-tool/`（plugin.json + main.py）随包在位（§6 输出全绿）。
- 宿主发现/健康链路：release 构建 `--smoke-host` 全绿（发现 → 握手 → parse → key_values → shutdown → 事件转换，`smoke-host: ALL GREEN`）——与插件页同一 `PluginRegistry`/`PluginRuntime` 路径。
- 插件页 UI 列表展示（builtin-csv + demo-tool 健康状态）：无 CLI 出口，待虚机截图补录。

### 项 3 — WebView2 缺失引导
- P4-01 已实现生产路径门禁（core/ab-app lib.rs `ensure_webview2`：缺失 → 双语引导框，不建窗白屏）。
- 本机已装 WebView2，缺失路径无法本地触发；虚机「未装机」场景待测。

### 项 4 — 拖入自建插件仓库（含 .git/）
- 依赖 UI 拖放交互，本机未做。虚机按 protocol.md §7.1 第 3/5 条：重载即发现且零告警。

### 项 5 — 用户目录插件
- 三源发现机制（便携/随包/用户目录）有单测覆盖（ab-host discovery 套件）；本机行为验收待虚机。

### 项 6 — 端到端走查
- 预演（部分）：release 构建 `--smoke-pipeline` ALL GREEN——导入 → parse → query（切片点数 == 剧本记录数）→ key_values → 事件转换（`ab://progress`）全链路经 `ImportCoordinator` 走通。
- 受阻点（如实记录，C 路缺陷卡 P3-03 报告）：D1 real 模式无法导入（无文件对话框）、D2 无法打开会话、D3 视图状态不落盘、D4 重开失败无通道。UI 侧「导入 bench_10mb → 上图 → 保存/重开会话自动重解析」需 D1-D4 修复后复验；bench_10mb 规模同理。
- 结论：管道层已验，UI 层待缺陷修复 + 虚机复验。

### 项 7 — 双架构一致性
- x64：ZIP 产物就位，`verify-zip-manifest.ps1` 断言全绿。
- ARM64：CI cross 档构建产物（`AnalysisBuddy-aarch64-dev` artifact，P0-03 已验证）；发布流水线 aarch64 job 按降级链自动落档。实机签核见 §5。

## 4. C 路缺陷遗留（D1-D4，如实记录）

| 卡 | 描述 | 对验收的影响 |
|----|------|-------------|
| D1 | real 模式无法导入（无文件对话框） | 阻塞项 6「导入」环节、项 4/5 的 UI 走查 |
| D2 | 无法打开会话 | 阻塞项 6「重开」环节 |
| D3 | 视图状态不落盘 | 阻塞项 6「保存」完整性 |
| D4 | 重开失败无通道 | 阻塞项 6「自动重解析」 |

以上不阻塞 ZIP 产出与流水线（P4-02 范围），按缺陷卡跟踪；修复后回填项 6 复验记录。

## 5. ARM64 降级签核（用户决策 2026-08-10）

- 决策：本机/本环境不做 ARM64 实机测试，仅保留构建目标。
- 降级路径（PLAN.md §8 风险 1 第 7 项允许）：发布流水线 aarch64 job → CI 产物 → **人工冒烟**按 `docs/arm64-smoke-checklist.md` 在 ARM64 实机/虚拟化环境签核留痕后回填本表。
- 当前状态：流水线 aarch64 job 待 tag 触发实测；签核 ⏳ 待执行。

## 6. 本机预演命令输出（证据留存）

```
# 清单断言（§2 项 2/7 的依据；exit 0，12 项检查全过）
.\scripts\verify-zip-manifest.ps1 -Zip dist/AnalysisBuddy-0.1.0-x86_64.zip
VERIFY_MANIFEST=dist/AnalysisBuddy-0.1.0-x86_64.zip:ok

# 项 1 解压启动（dist/smoke-x64，5s 窗口检查后杀进程）
exe exists: True / HasExited: False / MainWindowHandle: 3211578
window title: AnalysisBuddy / killed after 5s OK

# 项 2 宿主发现/健康链路（release 构建，dist/smoke-x64 解压副本）
dist\smoke-x64\AnalysisBuddy\AnalysisBuddy.exe --smoke-host
smoke-host: discovered plugin `mock` v0.1.0 (Portable)
smoke-host: handshake OK (state ready) / parse OK (records_total=3, batches=2)
smoke-host: key_values OK (3 entries) / shutdown OK
smoke-host: event conversion OK (9 health events)
smoke-host: ALL GREEN

# 项 6 端到端管道链路（release 构建）
dist\smoke-x64\AnalysisBuddy\AnalysisBuddy.exe --smoke-pipeline
smoke-pipeline: import OK (matched mock) / query OK (3 points across 3 slices)
smoke-pipeline: progress events OK / shutdown OK
smoke-pipeline: ALL GREEN

# 回归（P4-02 未触碰 Rust，确认无回归）
cargo test --workspace → 全部 suite test result: ok, 0 failed
```

## 7. 填写指引（虚机验收时）

1. 每台虚机完成 §1 环境表；截图统一存 `docs/acceptance-shots/{vm}-{item}/` 并在此记录相对路径。
2. 逐项验收后回填 §2 状态列与 §3 对应小节（含截图路径与命令输出）。
3. 全部通过 → 回填 §5 签核结论 → 随正式 tag 提交本文件（DoD：验收记录随 tag 归档，全过 → M4 关闭）。
