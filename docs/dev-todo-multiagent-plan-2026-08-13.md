# AnalysisBuddy 修复开发 TODO List（多子代理并行编排版）

> 输入依据：`docs/e2e-uiux-report-2026-08-13.md`（E2E 实测报告，问题编号 P1–P10 + A11y）。
> 执行者画像：能力强、善用子代理/并行/工具的大模型。本计划**刻意拉高并行度**（Wave-1 七路并发），并给出防冲突的并行开发契约。
> 模型偏好：**所有子代理统一使用 `qwen3.8-max-preview`**（用户偏好，性价比）。用 `create_sub_session` 时传 `model: "qwen3.8-max-preview"`。
> 日期：2026-08-13

---

## 0. 总目标与完成标准

把 E2E 报告中的 🔴/🟠/🟡 问题全部修复并通过：
1. 单元/组件测试（`npm --prefix ui run test`）与 Rust 测试（`cargo test`）全绿；
2. lint / typecheck / clippy / fmt / `check:i18n` 全绿；
3. **重新打包**（`scripts/bundle-zip.ps1`）并用 `scratch/` 辅助脚本重跑 E2E 关键路径（导入→图表→游标→关键值；demo-tool 可运行；会话可保存**且可重开**）；
4. 更新 E2E 报告状态（已修复项打勾）。

---

## 1. 多子代理并行开发契约（The Contract）

> 所有子代理与 leader 必须遵守。违反任一条即视为任务失败。

### 1.1 单一写者原则（Single-Writer）
- 每个文件在任一时刻**只有一个**代理可写。所有权见 §3 矩阵。
- 子代理**只允许**修改自己拥有的文件；读到他人文件可以，改不行。
- 若发现自己必须改他人文件：**停**，在回报中列为「跨域请求」，交 leader 裁决/转派，禁止越权改。

### 1.2 共享文件锁（Leader 独占或指定 owner）
以下「高冲突」文件默认**leader 独占**，子代理不得直接改，只能提「跨域请求」：
- `ui/src/i18n/en.json` / `zh.json`（唯一例外：AG-i18n，见 Wave-2）
- `Cargo.toml` / `Cargo.lock` / `tauri.conf.json` / `ui/package.json`
- `.github/**`、根级配置（`rustfmt.toml`/`clippy.toml`/`eslint.config.js`）

### 1.3 隔离与分支
- 每个子代理用 **git worktree 隔离**（`agent` 工具 `isolation: "worktree"`），在独立分支工作，**不得**直接 commit 到 `main`。
- 分支命名：`fix/<agent-id>-<slug>`（如 `fix/ag-chart-multi-y-axis`）。
- 提交粒度：一个逻辑修复一个 commit；commit message 用现有风格（`fix(scope): …`，参考 `git log`）。

### 1.4 子代理自检清单（Definition of Done）
每个子代理结束前必须在其 worktree 内跑通**与自身域相关**的检查并贴出结果：
- UI 域：`npm --prefix ui run test`、`npm --prefix ui run lint`、`npm --prefix ui run typecheck`
- Rust 域：`cargo test -p <crate>`、`cargo clippy --workspace -- -D warnings`、`cargo fmt --check`
- i18n 域：`npm --prefix ui run check:i18n`
- 插件/打包域：`powershell -File scripts\bundle-zip.ps1 -Arch x86_64 -NoLaunch`（或至少 demo-tool 本地可启动冒烟）
- 行为修复必须**新增/更新对应测试**（组件用 vitest + testing-library；Rust 用 #[test]）。

### 1.5 回报格式（子代理 → leader）
```
AGENT: <id>
STATUS: done | blocked | partial
BRANCH/WORKTREE: <…>
CHANGED FILES: <列表>
CHECKS: <每条命令 + pass/fail>
CROSS-DOM REQUESTS: <需要他人/leader 改的文件与诉求，含 i18n 新键清单>
NOTES: <对集成者重要的信息>
```

### 1.6 通信与任务板
- 用 `team_create` 建队 + `task_create/task_update` 维护共享任务板；每任务一 owner。
- 子代理完成/受阻用 `send_message` 通报 leader；leader 负责裁决跨域请求与冲突。
- **禁止**子代理之间互相改对方 worktree；冲突一律上交 leader。

### 1.7 集成协议（leader）
- Wave 结束后 leader 按序 merge 各分支；冲突由 leader 解决（优先保留双方意图）。
- merge 后 leader 跑**全量**检查 + 重新打包 + 重跑 E2E（§6），再更新报告。
- 任一子代理检查红 → leader 打回该代理修复，不 merge。

---

## 2. 文件所有权矩阵（防冲突核心）

| 代理 | 独占文件/目录 | 任务 | Wave |
|------|--------------|------|------|
| AG-plugin | `plugins/demo-tool/**`、`scripts/bundle-zip.ps1`（`sdk/python` 只读） | P1+P2 | 1 |
| AG-chart | `ui/src/chart/**` | P4+P6 | 1 |
| AG-tree | `ui/src/components/MetricTree.*`（tsx/css/test） | P3 | 1 |
| AG-css | `ui/src/components/PluginManagerPage.css`、`AppShell.css`、`ui/src/styles/theme.css` | P5+P9 | 1 |
| AG-rust | `core/ab-app/**` | A11y | 1 |
| AG-session | `ui/src/components/TopBar.*`、`ui/src/state/session.*` | P7+P8+新建确认 | 1 |
| AG-kv | `ui/src/components/KeyValuesPanel.*` | 0 关键值友好提示 | 1 |
| AG-i18n | `ui/src/i18n/*.json`、`ui/src/ipc/real.ts`（对话框标题/默认路径） | P10+汇总新键 | 2 |
| AG-desc | `plugins/builtin-csv/**`、`core/ab-host/**` | 指标描述本地化（可选） | 2 |
| leader | §1.2 共享文件 + 集成 | 合并/回归/报告 | 3 |

> Wave-1 七路在文件层面**两两不相交**，可安全并发。Wave-2 依赖 Wave-1 的「i18n 新键请求」，故串行在后。

---

## 3. TODO 任务清单

### Wave-1（七路并发）

- [ ] **T1 [AG-plugin] P1+P2：让内置 demo-tool 在打包产物中可运行且能自动匹配**
  - P1：把 `sdk/python` 的 `analysisbuddy` 包 vendor 进 `plugins/demo-tool/`（或入口注入 `sys.path`），消除 `ModuleNotFoundError`；在 `bundle-zip.ps1` 增加「demo-tool 可启动」冒烟断言（现仅断言文件就位）。
  - P2：修 `plugin.json` 的 `header_fingerprints` 大小写（`frame fps=`→`FRAME fps=`、`state scene=`→`STATE scene=`），或宿主匹配改大小写不敏感（若改宿主属跨域，提请求给 AG-desc/leader）。
  - 验证：本地 `python plugins/demo-tool/main.py` 冒烟 + 打包冒烟；`small_txt.log` 置信度 >0 且可解析出 fps/frame_ms/cpu_temp + STATE 关键值。

- [ ] **T2 [AG-chart] P4+P6：图表时间格式化 + 多 Y 轴**
  - P4：axisPointer label / tooltip 统一走 `formatTime`，消除原始 epoch 毫秒。
  - P6：按单位/量纲自动分轴（ECharts 多 `yAxis` + 序列 `yAxisIndex`），或提供归一化/对数切换；保证小量纲（fps/frame_ms）可读。
  - 验证：`ui/src/chart/options.test.ts` 增加分轴与格式化用例；人工/E2E 看 mem_mb 与 fps 分轴。

- [ ] **T3 [AG-tree] P3：指标树三级复选框联动**
  - 为所有非叶节点统一半选/全选逻辑（补 `indeterminateRef`、核对 `allChecked`），使 文件/插件/指标 勾选态一致。
  - 验证：`MetricTree.test.tsx` 增加「子全选→父选中」「子部分选→父半选」用例。

- [ ] **T4 [AG-css] P5+P9：徽标竖排 + 窄屏响应式**
  - P5：`.plugin-row__marker` 防中文逐字换行（`white-space:nowrap`/最小宽/flex 调整）。
  - P9：窄屏（<1000px）降级——侧栏折叠/抽屉或允许横向滚动或断点堆叠；可顺带启用 `--ab-left-w-min/max` 做侧栏拖拽调宽。
  - 验证：`viewport-fit.test.tsx`/新增窄屏用例；E2E 缩到 950px 右栏可达。

- [ ] **T5 [AG-rust] A11y：WebView2 内容暴露给辅助技术**
  - 评估并启用 WebView2 无障碍（`CoreWebView2Settings` / `--force-renderer-accessibility`），确保 AX 树可见应用主体；保留现有 `data-testid`/ARIA。
  - 验证：`get_window_state` AX 树应能看到按钮/输入（用 `scratch/` 工具或 UIA 抽查）。

- [ ] **T6 [AG-session] P7+P8+新建确认：会话闭环与反馈**
  - P7：real 模式顶栏加「打开会话…」按钮（`plugin-dialog` 选 `.absession` → `load_session`），解除 `{mock && …}` 限制。
  - P8：`saveSession/saveSessionAs` 成功时 toast「已保存到 …」（与错误横幅对称）。
  - 新建会话加确认对话框（破坏性操作保护）。
  - **不要**改 i18n json；把所需新键列入回报的 CROSS-DOM（i18n 清单）。
  - 验证：`TopBar.test.tsx`/`session` 相关用例（打开会话触发 load_session、保存成功 toast、新建弹确认）。

- [ ] **T7 [AG-kv] 0 关键值友好提示**
  - `builtin-csv` 返回 0 项时显示「该插件未提供关键值」而非空表头。
  - 验证：`KeyValuesPanel.test.tsx` 用例。

### Wave-2（依赖 Wave-1 的 i18n 请求）

- [ ] **T8 [AG-i18n] P10+汇总：本地化收尾**
  - 汇总各代理回报的 i18n 新键，统一写入 `en.json`/`zh.json`（跑 `check:i18n`）。
  - `real.ts` 对话框标题改 `t(...)`（如保存框标题）；`defaultPath` 记忆上次导入目录。
  - 图例短名/描述混排的 UI 侧文案（若属 UI 组件则提跨域）。

- [ ] **T9 [AG-desc] 指标描述本地化（可选/P3）**
  - 插件 schema `description` 支持本地化或宿主提供中英映射（`builtin-csv` + `ab-host`）。

### Wave-3（leader 集成）

- [ ] **T10 集成 + 全量回归**：merge 全部分支 → 全量检查 → 重新打包 → 重跑 E2E（§6）→ 更新报告打勾。

---

## 4. 并行执行波次图

```
Wave-1 (并发 7, worktree 隔离, 文件不相交)
  AG-plugin  AG-chart  AG-tree  AG-css  AG-rust  AG-session  AG-kv
     \_________\_________|________|________|_________|________/
                          ↓ (收集 i18n 新键请求)
Wave-2 (并发 2)
  AG-i18n   AG-desc
     \_________/
          ↓
Wave-3 (leader 串行)
  merge → 全量检查 → 打包 → E2E 回归 → 更新报告
```

---

## 5. 子代理任务简报模板（直接作为 agent prompt）

> 通用头（每个代理都带）：
> 「你在 AnalysisBuddy（Rust+Tauri2+React 日志分析工作台）的独立 git worktree 中工作，分支 `fix/<id>-<slug>`，模型 qwen3.8-max-preview。先读 `docs/e2e-uiux-report-2026-08-13.md` 与本计划 §1 契约。你**只允许**修改：<OWNED>。禁止改 i18n json 与根级配置（提跨域请求）。结束前跑 <CHECKS> 并按 §1.5 格式回报。」

- AG-plugin：OWNED=`plugins/demo-tool/**, scripts/bundle-zip.ps1`；CHECKS=python 冒烟 + bundle 冒烟。
- AG-chart：OWNED=`ui/src/chart/**`；CHECKS=ui test/lint/typecheck。
- AG-tree：OWNED=`ui/src/components/MetricTree.*`；CHECKS=ui test/lint/typecheck。
- AG-css：OWNED=`PluginManagerPage.css, AppShell.css, styles/theme.css`；CHECKS=ui test/lint/typecheck。
- AG-rust：OWNED=`core/ab-app/**`；CHECKS=cargo test/clippy/fmt。
- AG-session：OWNED=`TopBar.*, state/session.*`；CHECKS=ui test/lint/typecheck；输出 i18n 新键清单。
- AG-kv：OWNED=`KeyValuesPanel.*`；CHECKS=ui test/lint/typecheck。
- AG-i18n：OWNED=`i18n/*.json, ipc/real.ts`；CHECKS=check:i18n + ui test。
- AG-desc：OWNED=`plugins/builtin-csv/**, core/ab-host/**`；CHECKS=cargo test + ui test。

---

## 6. 集成后回归验证（leader）

1. `cargo test --workspace`、`cargo clippy --workspace -- -D warnings`、`cargo fmt --check`
2. `npm --prefix ui run test && lint && typecheck && check:i18n`
3. `powershell -File scripts\bundle-zip.ps1 -Arch x86_64`（含冒烟）
4. 用 `scratch/forceclick.ps1` 等脚本对解压产物重跑 E2E 关键路径：
   导入 CSV→就绪 / 勾选→分轴图表 / 点游标→格式化时间+关键值 / 导入 log→demo-tool 就绪（非崩溃）/ 保存会话→toast / **打开会话→恢复** / 窄屏 950px 右栏可达 / AX 树可见主体。
5. 将结果回写 `docs/e2e-uiux-report-2026-08-13.md`（已修复项 ✅）。

---

## 7. 风险与回退

- **worktree 合并冲突**：Wave-1 文件不相交，理论无冲突；若 CSS 全局变量（theme.css）被多方需要，提前在矩阵中收口到 AG-css。
- **i18n 键遗漏**：AG-i18n 以各代理回报清单为准；`check:i18n` 兜底。
- **demo-tool vendor 体积**：若 SDK 过大，改「打包时内联 + 冒烟断言」方案；保持 ZIP <10MB。
- **A11y 副作用**：启用 renderer accessibility 可能影响性能/渲染，AG-rust 需验证图表帧率无明显回归。
- **回退**：任一 Wave-1 分支检查红或 E2E 回归失败 → leader 单独 revert 该分支，不阻塞其余集成。

*计划完。建议 leader 先 `team_create` + 建任务板，再按 Wave-1 七路并发派发。*
