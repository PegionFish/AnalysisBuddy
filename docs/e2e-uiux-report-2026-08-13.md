# AnalysisBuddy E2E UI/UX 测试报告

> 测试日期：2026-08-13 ｜ 测试对象：最新代码库 release 打包产物（便携 ZIP）｜ 测试方式：computer_use 真实驱动 + 代码核查
> 用途：供后续 session 开发优化参考。所有截图坐标/窗口句柄为测试机环境值，结论与代码位置可复现。

---

## 0. 摘要（TL;DR）

打包与启动链路**健康**；以 `builtin-csv` 为插件的**核心分析主流程（导入→解析→指标树→折线图→游标→关键值）端到端可用**。但发现 **1 个阻断级缺陷**（内置 `demo-tool` 插件因缺 SDK 在打包产物中无法运行）、**1 个功能性缺口**（生产模式无法重开会话）、若干**图表/布局/反馈类 UX 问题**。整体 UI 设计干净、双语完整、空状态与错误隔离做得好；主要改善空间集中在**多量纲图表、响应式布局、操作反馈、可访问性**。

| 维度 | 结论 |
|------|------|
| 构建/打包 | ✅ 通过（release+LTO，ZIP 清单断言全过，解压即启动） |
| 核心分析流程 | ✅ 可用（builtin-csv 全链路） |
| 内置 demo-tool | ❌ 阻断：缺 `analysisbuddy` SDK，打包后崩溃 |
| 会话重开 | ⚠️ 缺口：生产模式无 UI 入口 |
| 图表可用性 | ⚠️ 多量纲共轴、原始毫秒时间戳、图例分页 |
| 响应式 | ⚠️ <1000px 三栏溢出、右栏被裁切 |
| 操作反馈 | ⚠️ 保存无成功提示、新建会话无确认 |
| 可访问性 | ⚠️ WebView2 内容未暴露给 UIA/AX |
| i18n | ✅ 中英键覆盖完整 |

---

## 1. 测试环境与产物

- **OS**：Windows（2560×1440，测试机存在 DPI 虚拟化，见 §7）。
- **构建**：`scripts/bundle-zip.ps1 -Arch x86_64`（release + LTO）。`BUNDLE` 脚本退出码 0。
- **产物**：`dist/AnalysisBuddy-0.1.0-x86_64.zip`（5.57 MB）。解压到 `dist/e2e-test/` 实测。
- **便携布局校验**（与 `bundle-zip.ps1` 清单断言一致）：
  - `AnalysisBuddy.exe` 14.5 MB、`WebView2Loader.dll`、`README-PORTABLE.txt`
  - `plugins/builtin-csv/`（plugin.json + config.json + `target/release/builtin-csv.exe` 763 KB）
  - `plugins/demo-tool/`（main.py + parser.py + plugin.json）
- **运行时**：Microsoft Edge WebView2 正常；Python 3.14.6 在 PATH（demo-tool 依赖）。
- **测试数据**：`tests/fixtures/small_with_header.csv`（builtin-csv）、`tests/fixtures/small_txt.log`（demo-tool 目标格式）。

---

## 2. 构建与打包验证

| 项 | 结果 |
|----|------|
| `npm install`（ui/） | ✅ 268 包，9s |
| `tauri build --no-bundle`（前端 vite + Rust release） | ✅ |
| builtin-csv release 构建 | ✅ |
| ZIP 组装 + 清单断言（含 PE machine 架构断言、>1MB 体积断言） | ✅ |
| 解压即启动（主窗口 5s 内出现、进程存活） | ✅ |

> 打包链路无需改动，可作为发布基线。

---

## 3. E2E 操作流程实测（real 模式）

### 3.1 首次启动
- ✅ 窗口正常渲染，三栏布局（左：文件+指标；中：折线图；右：关键值）+ 顶栏。
- ✅ 默认**深色**主题、默认**中文**。空状态文案到位：`暂无文件` / `暂无可显示指标` / `开始分析（导入文件并勾选指标后，曲线将显示在这里）` / `点击图表设置游标以查看 T 时刻状态`。

### 3.2 文件导入（原生选择器）
- ✅ 点「选择文件」→ 原生「打开」对话框 → 选 CSV → 文件进入列表，状态 `解析中… → 就绪`。
- ✅ 自动匹配插件并显示置信度：`匹配插件：CSV Universal Parser（100%）`。
- ⚠️ 选择器**默认打开在应用安装目录**（`…\e2e-test\AnalysisBuddy`），而非「上次目录/文档/桌面」，对导入外部日志不友好。

### 3.3 指标树与图表
- ✅ 三级树 `文件 → 插件 → 指标` 正确生成，含指标描述与聚合方式（`聚合方式：平均`）。
- ✅ 勾选文件级复选框可全选子指标；图表渲染多序列 + 图例 + dataZoom 缩放条 + 时间轴 + 「重置缩放」。
- 🐛 **插件级（中间层）复选框联动错误**：子指标全选时，插件级复选框仍显示「未选中」（文件级正确显示选中）。见 §4-P3。
- ⚠️ **多量纲共轴**：`mem_mb`（千级）与 `fps`/`frame_ms`（十级）共用单一 Y 轴（0–4000），小量纲曲线被压扁贴底、几乎不可见。见 §4-P6。
- ⚠️ 图例项过长（`small_with_header.csv / fps`）触发分页 `1/2`。

### 3.4 游标与关键值
- ✅ 点击图表网格任意位置设置游标（zrender 层 click，大数据 `large` 模式可用）：出现虚线游标 + 工具栏 `游标: 08:00:09.945`。
- ✅ 关键值面板按文件分组刷新（`small_with_header.csv | CSV Universal Parser`）。
- 🐛 **图表上出现原始 epoch 毫秒** `1785542409945`（游标处 axisPointer 标签未格式化为时间）。见 §4-P4。
- ⚠️ `builtin-csv` 返回 0 项关键值时，面板仅显示空表头（键/值/单位），无「该插件无关键值」友好提示。

### 3.5 插件消歧（多候选）
- ✅ 导入 `small_txt.log` 同时命中 builtin-csv 与 demo-tool 时，进入「待选插件」态，给出候选按钮供手选——**消歧 UX 设计良好**。
- 🐛 **两候选置信度均 0%**：demo-tool 的 `header_fingerprints` 为 `frame fps=`/`state scene=`（小写），而实际日志为 `FRAME fps=`/`STATE scene=`（大写），大小写不匹配导致**永远 0% 置信度、无法自动匹配**。见 §4-P2。

### 3.6 插件管理页（/plugins）
- ✅ 插件列表 + 10 态健康徽标（`就绪`/`已崩溃` 等，带配色）+ 内建标记（随应用分发）+ 来源（便携安装）+ 内联「最近错误」。
- ✅ 详情抽屉完整：关于 / 版本历史 / 能力（注释标注、实时订阅、二进制伴生）/ 已加载文件 / **STDERR 日志**（时间戳+级别+「跟随滚动」）。stderr 捕获工作正常。
- ✅ 模块安装区（拖入 ZIP / 选择文件）。
- 🐛 **徽标文字竖排**：`随应用分发`、`便携安装` 被压成竖排单字换行，视觉破损。见 §4-P5。
- ❌ **demo-tool 崩溃**：`ModuleNotFoundError: No module named 'analysisbuddy'`。见 §4-P1。

### 3.7 会话（保存/新建/重开）
- ✅ 「另存为…」→ 原生保存对话框，**预填默认名 `session`**（合理）。
- ⚠️ **保存成功无任何反馈**（无 toast/确认；代码确认 `saveSession` 仅在失败时 `setSaveError`，成功路径零提示）。见 §4-P8。
- 🐛 **生产模式无「打开会话」入口**：`openSession`/`load_session` 后端与状态层已接线，但 TopBar 的打开输入仅 mock 模式渲染；无文件关联、无深链、拖拽 `.absession` 也不会作为会话打开。**用户可保存却无法重开会话**。见 §4-P7。
- ⚠️ 「新建会话」直接 `session/reset`，**无确认对话框**——误点即丢失全部未保存的文件/指标/游标。

### 3.8 主题 / 语言
- ✅ 主题切换即时生效（深↔浅），并持久化到 `localStorage`（`ab.theme`）。两套主题配色均干净、对比度良好。
- ✅ i18n：`en.json` 与 `zh.json` **键结构完全一致、无缺键**，插值占位符规范（`{{count}}`/`{{message}}` 等）。
- ⚠️ 原生对话框标题未本地化（如保存框标题 `Save AnalysisBuddy Session` 为英文，而 UI 为中文）。
- ⚠️ 指标描述为英文（`column fps of small_with_header.csv`，来自插件 schema），与中文 UI 混排。

### 3.9 响应式 / 窗口缩放
- ✅ 插件页（单列）在窄宽度下正常重排。
- 🐛 **工作台三栏在 <1000px 宽度溢出**：左 280 + 中 min 400 + 右 320 = 1000px 下限；窗口更窄时右栏（关键值）被裁切且 `app-shell__body` 为 `overflow:hidden` 无横向滚动，右栏内容不可达。见 §4-P9。

---

## 4. 问题清单（按严重度）

> 代码位置均相对仓库根目录。严重度：🔴 阻断 ｜ 🟠 高 ｜ 🟡 中 ｜ 🟢 低。

### 🔴 P1 内置 demo-tool 插件在打包产物中无法运行
- **现象**：选 demo-tool 解析 → 文件 `失败 / 内部错误`；插件页 `已崩溃`，stderr：`ModuleNotFoundError: No module named 'analysisbuddy'`（`plugins/demo-tool/main.py:17`）。
- **根因**：`main.py` 依赖 Python SDK 包 `analysisbuddy`（位于 `sdk/python`），但便携包未捆绑该包、未设 `PYTHONPATH`、未 `pip install`。README 声称 demo-tool 随包分发，实际开箱即坏。
- **建议**（任选）：① 打包时将 `sdk/python` 的 `analysisbuddy` 包 vendor 进 `plugins/demo-tool/` 并在入口注入 `sys.path`；② 在 demo-tool 目录内联 SDK；③ 打包脚本 `bundle-zip.ps1` 增加「demo-tool 可启动」冒烟断言（当前仅断言文件就位，未断言可运行）。

### 🟠 P2 demo-tool 文件指纹大小写不匹配，永远 0% 置信度
- **现象**：`small_txt.log` 命中 demo-tool 但置信度 0%，需手选。
- **根因**：`plugins/demo-tool/plugin.json` 的 `header_fingerprints` 为 `["frame fps=", "state scene="]`，而 `parser.py` 实际解析的行前缀为大写 `FRAME`/`STATE`。指纹与真实格式大小写不一致。
- **建议**：指纹改为与实际行匹配（如 `FRAME fps=`、`STATE scene=`），或宿主侧指纹匹配改为大小写不敏感。同时排查 `builtin-csv` 对 `.txt` 的误命中权重，避免无意义双候选。

### 🟠 P7 生产模式无法重开会话（功能性缺口）
- **现象**：可「保存/另存为」，但 real 模式顶栏无「打开会话」入口；无 `.absession` 文件关联/深链。
- **根因**：`ui/src/components/TopBar.tsx` 中打开会话输入被 `{mock && (…)}` 包裹，仅 mock 模式可见；`openSession`/`load_session`（`ui/src/state/session.ts`、`core/ab-app` ACL `allow-load-session`）已就绪但无 UI 触发点。
- **建议**：real 模式顶栏增加「打开会话…」按钮（`plugin-dialog` 选 `.absession` → `load_session`）；可选注册 `.absession` 文件关联/拖拽打开。这是 PLAN.md §1「会话可保存、可重开」的验收项，当前未闭环。

### 🟡 P3 指标树插件级复选框联动状态错误
- **现象**：子指标全选时，插件级复选框显示「未选中」（文件级正确）。
- **位置**：`ui/src/components/MetricTree.tsx`。文件行经 `renderFileRow` 传入 `indeterminateRef`，而插件行在 `children.map` 中渲染 `TreeNodeRow` 时**未传 `indeterminateRef`**，且其 `checked=allChecked` 在全选时未正确反映。
- **建议**：为所有非叶节点统一接入半选/全选逻辑（传 `indeterminateRef` 并核对 `allChecked` 计算），保证 `文件/插件/指标` 三级勾选态一致。

### 🟡 P4 图表游标处显示原始 epoch 毫秒
- **现象**：设置游标后，图表顶部出现 `1785542409945`（原始 UTC 毫秒），未格式化为时间。
- **位置**：`ui/src/chart/options.ts` 的 axisPointer/tooltip 标签格式化。
- **建议**：X 轴 axisPointer label 与 tooltip 统一走 `formatTime`（与工具栏 `游标: 08:00:09.945` 一致）。

### 🟡 P5 插件页徽标文字竖排（CSS 缺陷）
- **现象**：`随应用分发`、`便携安装` 徽标被压成竖排单字。
- **位置**：`ui/src/components/PluginManagerPage.css` 的 `.plugin-row__marker`（宽度受限 + 中文换行）。
- **建议**：给徽标 `white-space: nowrap` 或调整 flex 布局/最小宽度，避免中文逐字换行。

### 🟡 P6 多量纲指标共用单一 Y 轴，小量纲不可见
- **现象**：`mem_mb`（千级）与 `fps`/`frame_ms`（十级）同轴，后者贴底不可读。
- **根因**：PLAN.md §3.4 规划的「多 Y 轴」未实现；`options.ts` 所有序列共用一个 yAxis。
- **建议**：按量纲/单位自动分轴（ECharts 多 yAxis + 序列 `yAxisIndex`），或提供「归一化/对数轴」切换；至少对量纲差异大的序列做视觉区分。这是图表核心可用性项。

### 🟡 P8 会话保存无成功反馈
- **现象**：保存后无任何确认（仅失败有横幅）。
- **位置**：`ui/src/state/session.ts` `saveSession`/`saveSessionAs` 仅 `catch` 设 `saveError`，成功无提示。
- **建议**：成功时给出轻量 toast（如「会话已保存到 …」），与现有错误横幅对称。

### 🟡 P9 窄窗口三栏溢出、右栏不可达
- **现象**：窗口 <1000px 时右栏（关键值）被裁切，无横向滚动。
- **位置**：`ui/src/components/AppShell.css` `grid-template-columns: 280px minmax(400px,1fr) 320px` + `overflow:hidden`。
- **建议**：窄屏降级策略——侧栏可折叠/抽屉化，或允许横向滚动，或断点下改为上下堆叠。`theme.css` 已定义 `--ab-left-w-min/max` 但未启用拖拽调宽，可一并实现侧栏可调。

### 🟢 P10 其它低优先级
- 图例项过长触发分页：图例用短名（指标 id）+ tooltip 全名，或图例换行/滚动。
- 指标描述英文混排：插件 schema 的 `description` 支持本地化，或宿主提供中英映射。
- 原生对话框标题未本地化：Tauri dialog 标题传入 `t(...)`。
- 文件选择器默认目录：记忆上次导入目录或默认「文档」。
- `builtin-csv` 0 关键值：面板显示「该插件未提供关键值」而非空表头。
- 「新建会话」无确认：破坏性操作加确认对话框（或撤销）。
- 文件面板插件失败仅显示笼统「内部错误」：可在条目上提供「查看日志」直达插件 stderr。

---

## 5. UI/UX 设计评估

### 做得好的
1. **设计系统规范**：语义化 CSS token（`--ab-*`）+ 深/浅双主题，配色克制、对比度达标，组件只用变量。
2. **空状态与引导**：各面板空状态文案清晰，告知用户下一步（导入→勾选→看曲线→点游标）。
3. **错误隔离**：按文件、按插件隔离失败（单文件失败不影响其它；插件崩溃进日志面板），符合 PLAN.md 容错约定。
4. **插件管理页信息密度合理**：健康徽标、能力、关于、版本历史、stderr 抽屉 + 跟随滚动，专业且完整。
5. **插件消歧**：多候选时给出手选按钮，处理歧义优雅。
6. **i18n 工程化**：中英键覆盖完整、占位符规范、`check:i18n` 脚本保障。
7. **游标交互健壮**：zrender 层 click + `containPixel` 守卫 + `convertFromPixel` 反算，大数据 `large` 模式与缩放后均可设游标。

### 主要改善空间（按价值排序）
1. **多量纲图表**（P6）——分析工具的核心，当前小量纲不可读，优先级最高。
2. **会话闭环**（P7）——「可重开」是产品承诺，当前断裂。
3. **内置插件可用性**（P1/P2）——demo-tool 是 dogfood 门面，开箱即坏损害信任。
4. **操作反馈**（P8 + 新建确认）——保存/重置等关键操作缺乏反馈与保护。
5. **响应式**（P9）——侧栏固定宽 + 中心 min 宽，窄屏不可用。
6. **细节打磨**（P3/P4/P5/P10）——复选框联动、时间格式化、徽标排版、图例/描述本地化。
7. **可访问性**（见 §6）。

---

## 6. 可访问性（Accessibility）

- ⚠️ **WebView2 内容未暴露给 UIA/AX 树**：`get_window_state` 的 AX 树仅含标题栏（最小化/最大化/关闭），整个应用 UI（按钮、输入、图表、树）位于 `WRY_WEBVIEW → Chrome_RenderWidgetHostHWND`，对辅助技术不可见。
- **影响**：读屏器、键盘导航、基于 AX 的自动化均无法访问应用主体；本次测试被迫改用真实 `SendInput` 像素点击驱动。
- **建议**：评估 WebView2 无障碍开关（`CoreWebView2Settings` / `--force-renderer-accessibility`），并确保关键控件有 ARIA 标签（组件已部分使用 `aria-label`/`role`/`data-testid`，基础良好）。

---

## 7. 测试环境限制说明（非应用缺陷）

以下为本测试机/自动化环境的限制，**不代表应用 bug**，复现时需注意：

1. **原生文件选择器需应用在前台才正常渲染**：应用未获焦时，「打开/保存」对话框以 0 尺寸窗口打开（`PickerHost`/`#32770` 不渲染）。通过强制置前（`AttachThreadInput`+`SetForegroundWindow` 绕过前台锁）后正常。疑与提权（High 完整性）/VM shell 相关。
2. **WebView2 内容无法用 PostMessage 像素点击送达**：computer_use 的「post to pid」点击落在重叠窗口/不进入 WebView2 子窗口；改用真实 `SendInput`（移动光标+点击）可靠。
3. **原生 `<select>` 下拉难以用合成输入驱动**：语言切换的下拉弹出为独立窗口，合成点击/方向键均未生效；i18n 完整性改以 `en.json`/`zh.json` 代码核查确认。
4. **DPI 虚拟化**：`list_windows` 边界与 Win32 `GetWindowRect` 存在 ~1.5× 偏差（cua-driver 为 DPI 不感知），截图坐标换算需按实际窗口矩形校准。
5. **CDP 未启用**：`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port` 未被该 Tauri/wry 构建采纳（疑 wry 覆盖了 `additionalBrowserArguments`），故未用 CDP 驱动，全程以真实输入 + AX（原生对话框）+ 截图观察完成。

---

## 8. 后续开发优先级建议

| 优先级 | 事项 | 关联 |
|--------|------|------|
| P0 | 修复 demo-tool 打包缺 SDK（vendor/内联 + 打包冒烟断言） | P1 |
| P0 | 生产模式补「打开会话」入口（+ 可选文件关联） | P7 |
| P1 | 图表多 Y 轴 / 量纲分离 | P6 |
| P1 | demo-tool 指纹大小写修正 | P2 |
| P1 | 保存成功反馈 + 新建会话确认 | P8 |
| P2 | 指标树三级复选框联动 | P3 |
| P2 | 游标/axisPointer 时间格式化 | P4 |
| P2 | 插件徽标竖排修复 | P5 |
| P2 | 窄屏响应式（侧栏折叠/可调宽） | P9 |
| P3 | 图例/描述本地化、选择器默认目录、0 关键值提示、WebView2 无障碍 | P10/§6 |

---

## 附：测试覆盖矩阵

| 流程 | real 模式实测 | 结果 |
|------|--------------|------|
| 启动/布局/空状态 | ✅ | 通过 |
| 导入（选择器）→解析→就绪 | ✅ | 通过 |
| 插件自动匹配+置信度 | ✅ | 通过（builtin-csv） |
| 插件消歧（多候选手选） | ✅ | 通过（但 0% 置信度，P2） |
| 指标树生成/全选联动 | ✅ | 部分（插件级联动 P3） |
| 折线图多序列/缩放/图例 | ✅ | 通过（多量纲 P6、图例 P10） |
| 游标设置→关键值刷新 | ✅ | 通过（原始毫秒 P4） |
| 插件页（健康/抽屉/stderr/安装区） | ✅ | 通过（徽标 P5） |
| demo-tool 解析 | ✅ | ❌ 崩溃（P1） |
| 会话保存 | ✅ | 通过（无反馈 P8） |
| 会话重开 | — | ❌ 无入口（P7） |
| 主题切换+持久化 | ✅ | 通过 |
| 语言切换 | 代码核查 | i18n 完整（下拉驱动受环境限制） |
| 响应式缩放 | ✅ | 窄屏溢出（P9） |

*报告完。建议下一 session 从 P0（demo-tool 打包、会话重开）切入。*
