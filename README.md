# AnalysisBuddy

Little handy tool for analysing logs from multiple tools

**AnalysisBuddy** 是一个 Windows 桌面日志分析工作台：通过插件将不同工具的 Log 解析为统一时序数据，支持指标叠加折线图与关键状态值查看。

`docs: baseline`（基线提交）：PLAN.md 为本仓库唯一事实依据，后续开发与争议一律以 PLAN.md 为准。

开发计划见 [PLAN.md](./PLAN.md)。

## 下载与使用（Download & Usage）

- 发布产物：GitHub Actions 以 `v*` tag 触发的发布流水线产出
  `AnalysisBuddy-{version}-{arch}.zip`（`x86_64` / `aarch64` 各一份），从流水线
  artifact 或 GitHub Releases 获取。
- 绿色便携版：**无安装器**，解压即用。解压到任意目录（含空格/中文路径均可），
  双击 `AnalysisBuddy.exe` 启动。
- 升级：下载新版 ZIP 覆盖解压即可；`plugins/` 与 `%APPDATA%\AnalysisBuddy\plugins`
  私有插件不受覆盖影响。
- 运行时依赖：Microsoft Edge WebView2（Evergreen）。缺失时应用启动即弹双语引导
  下载页，不会白屏崩溃；详见 ZIP 内 `README-PORTABLE.txt`。
- 内置插件：`plugins/builtin-csv`（Rust 静态分发，零运行时依赖）、
  `plugins/demo-tool`（Python 3.10+ 脚本，按 PATH 解析解释器）。
- 双架构：x86_64 产物经构建 + 清单断言 + 解压启动冒烟；ARM64 按
  「构建目标保留 + 人工冒烟签核」交付，签核清单见 [docs/arm64-smoke-checklist.md](./docs/arm64-smoke-checklist.md)，
  发布验收记录见 [docs/release-acceptance.md](./docs/release-acceptance.md)。
