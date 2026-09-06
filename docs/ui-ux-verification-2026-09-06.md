# WebUI 全面测试与验证报告（2026-09-06 下午轮）

> 承接 [ui-ux-evaluation-2026-09-06.md](ui-ux-evaluation-2026-09-06.md)。本轮为自主全面测试与验证：项目自动化套件 + GUI 黑盒深挖（预设/指标树/文件操作/会话边界/持久化/i18n/取消解析），并对上轮三个疑点做根因定位。
> 权限系统按要求搁置。截图证据：`scratch/ui-ux-eval-2026-09-06/`（v2_/v5_–v8_ 系列）。

## 0. 结论速览

- **自动化套件全绿**：typecheck ✓ / ESLint ✓ / vitest **304/304** ✓ / i18n 键对齐 231 ✓（PresetBar 有一条 React act() 控制台警告，不影响结果，建议顺手清掉）。
- **上轮"缩放后序列暗淡"降级**：正常渲染条件下 5 次受控尝试（深/浅主题 × 3/4 序列 × 游标有无 × 单/双拖拽）均未复现；结合定时器节流的观测，最可能是**环境遮挡节流冻结了 ECharts 入场动画的中间帧**，非确定性应用缺陷。建议以 `animation: false`（或更新时禁用动画）彻底消除该类风险。
- **左栏常驻横向滚动条根因锁定**：拖拽手柄 `.app-shell__handle--left`（5px）以骑跨式定位，右缘超出可滚动面板 2.3px（280→282.3）→ `scrollWidth 282 vs clientWidth 279`。手柄移出滚动容器或面板 `overflow-x: hidden` 即可修复。
- **文件侧卸载同样无确认**（实锤）：测试中一次偏移点击直接卸载了 game_session.csv，全程无确认对话框——与插件侧卸载同款问题，破坏面更大（文件+曲线+关键值一并清除）。
- 其余深挖项（预设、指标树、取消解析、会话恢复、持久化）**全部符合预期**，详见 §2。

## 1. 环境说明（影响判读，非应用缺陷）

本轮多次出现自动化管线异常，根因统一指向 **IAB 面板被遮挡时 Chromium 对页面定时器重度节流**（页面 `visibilityState` 仍报 visible）：

- mock 解析进度（150ms interval）爬行至停滞（卡在 12%/6% 数分钟），用户激活面板后恢复；
- 截图/点击管线多次 30s 超时（页面无法静默），重开标签页恢复；
- "暗淡渲染"仅在该节流状态下出现一次（动画冻结伪影）。

对真实桌面壳（Tauri 前台窗口）不构成影响；但**对 ab-server Web 形态（后台标签页）是真实风险**——建议前端对 ECharts 更新关闭动画（`animation: false`），既消除冻结风险也提速重查渲染。测试结束时已关闭全部测试标签页。

## 2. 分项验证结果

| # | 验证点 | 结果 | 证据/备注 |
|---|--------|------|-----------|
| V1 | typecheck / lint / vitest / check:i18n | ✅ 全绿 | 304/304；i18n 231 键一致；PresetBar act() 警告 |
| V2 | 暗淡渲染触发矩阵 | ✅ 未复现→降级 | 5 组合受控尝试全部鲜亮；见 §1 |
| V3 | 左栏 3px 溢出根因 | ✅ 定位 | 拖拽手柄超出 2.3px（几何实测） |
| V4 | 日志抽屉"无法静默" | ⚠️ 归因环境 | 打开抽屉与页面忙态相关；节流统一解释；桌面壳复测保留 |
| V5 | 预设：零命中/保存/冲突 | ✅ 全过 | 零命中绿色内联提示"matched nothing: selection unchanged"（不静默清空，好设计）；保存 toast + 下拉刷新（约 1s 后收敛）；重名 → 红色横幅 "user preset 'my-preset' already exists"；localStorage 按插件相对 id 存储（跨文件可复用） |
| V6 | 指标树：搜索/收藏/只看收藏 | ✅ 全过 | 搜索过滤生效；星标 ☆→★ + `ab.metric.favorites` 复合 id；只看收藏 aria-checked 正确、空态文案 "No matching metrics"；备注：星标 aria-label 恒为 "Favorite" 不区分指标且选中后不变文案 |
| V7 | 文件操作 | ✅（含 1 实锤缺陷） | 停用→曲线清除（画布蓝像素 0）、启用→恢复（13641 像素）；取消解析 → Error 徽标 + "Operation cancelled" + Retry ✓；**文件卸载无确认**（意外实锤）；错误路径导入 mock 不校验真实文件系统（局限） |
| V8 | 会话边界 | ✅ 全过 | 另存为 toast "Saved to mock-session-30.absession"；恢复摘要（横幅+顶栏徽标+详情展开：逐项路径/File not found/Retry/Copy diagnostics，按钮文案随展开态切换）；重开匹配会话正确清除 missing；mock 有路径关键字钩子（含 "missing"/"reopen"）——伪造缺失会话做测试，注意别当真实错误路径语义 |
| V9 | 折叠图例/技术字段/降采样徽标 | ⏸ 未完成 | 工具管线劣化，放弃；建议人工 2 分钟补查 |
| V10 | 持久化 | ✅ 全过 | 侧栏折叠态、主题、语言、首启引导"仅一次"全部跨刷新保持 |
| V11 | i18n 残留 | ✅ | 中文模式可见范围无英文 UI 残留（插件数据类英文如指标描述除外，前轮已记录） |
| V12 | 窄屏抽屉/首启引导 | ✅（前轮已验） | 抽屉开关、引导一次性逻辑均正常 |

### 遗留未决（建议人工快速复核）
1. **部分选中时父级复选框应显示半选**：受管线卡顿影响未能完成一次干净的"取消单个叶子"点击；代码中文件行有 `indeterminateRef` 管线，请人工点一次确认插件级是否同样接入（08-13 报告 P3 的变体）。
2. **语言切换后恢复横幅是否保留**：复现尝试被点击失效阻断；从状态管理看 `state.missing` 不应受 `lang/set` 影响，风险低，人工确认即可。
3. V9 三个图表小项。

## 3. 更新后的问题清单（合并两轮，按优先级）

| 级别 | 问题 | 状态 |
|------|------|------|
| P0 | 卸载无确认（插件侧 08-12 起；**文件侧本轮实锤**） | 未修，修复优先级最高 |
| P0→P2 | 缩放重查序列暗淡 | **降级**：环境动画冻结伪影；建议 `animation: false` 一并解决 |
| P1 | 窄屏顶栏溢出（787px）/ 插件页内容列 57% 宽 | 未修 |
| P1 | Y 轴无单位标注 + 双右轴贴邻 | 未修（多量纲分轴已实现） |
| P2 | 左栏 3px 溢出（根因已定位：手柄超出 2.3px） | 未修，修复成本低 |
| P2 | 窄屏 KV 抽屉标题重复 / axisPointer 压图例 / dataZoom 端标签格式不一致 | 未修 |
| P2 | 拖拽区无键盘语义 / 控件 24–25px·12px 字号 | 未修 |
| P3 | 星标 aria-label 不随选中态变化、不含指标名 | 本轮新增 |
| P3 | PresetBar act() 警告；错误横幅无自动消退（长时驻留易过期） | 本轮新增（低） |
| 观察 | 日志抽屉打开期页面忙态；后台标签页节流对 Web 形态的影响 | 建议 `animation:false` + 桌面壳复测 |

## 4. 建议的下一步

1. 修 P0（卸载确认 Dialog，文件+插件两处统一封装）——在线服务版前置条件。
2. 低成本快修：左栏 overflow（一行 CSS）、PresetBar act 警告、星标 aria 文案。
3. 人工 5 分钟复核 §2 遗留三项。
4. 图表 `animation: false` 评估（同时缓解节流冻结与重查渲染开销）。

---

## 5. 修复轮补记（2026-09-06 晚，自主后续工作）

### 5.1 上一轮两个"疑点"的最终定论（修正本报告 §0/§2.11 相关结论）

- **"缩放后序列暗淡"彻底定案——不是缺陷，是 P2-02 设计功能**：`chart/options.ts:331` 的
  `emphasis: { focus: 'series' }` 在指针悬停某序列时压暗其余序列。自动化测试的虚拟指针
  扫过画布后停在原处不再移动，ZRender 收不到 mouseout，压暗态冻结在后续截图里；
  点击图例"恢复鲜亮"正是因为图例点击本身是画布事件、重新求值了悬停。真实用户鼠标
  移开即恢复；触屏上"点序列聚焦、点空白还原"即为该特性的设计交互。**无需修改。**
  本报告 §0 中"动画冻结伪影"的猜测不成立——`animation: false` 在 options.ts 两处本就
  已设置（评估报告中的对应建议作废）。
- **插件级半选联动早已实现**：`MetricTree.tsx:145-148` 的 ref 回调对文件/插件两级非叶
  节点统一写入 `indeterminate`（子部分选中 → 半选）。§2 遗留项 1 关闭，无需修改。
- **语言切换与恢复横幅**：`sessionReducer` 的 `lang/set` 只改 lang，`state.missing` 不受
  影响，横幅保留有代码保证；§2 遗留项 2 关闭（当时观察到的"消失"实为重开匹配会话
  正确清除 missing 所致）。

### 5.2 本轮已实施的修复

| 修复 | 文件 | 说明 |
|------|------|------|
| **P0 卸载确认（插件侧）** | `PluginManagerPage.tsx` | 「卸载」先弹主题化确认框（含模块显示名与不可撤销警告），取消不动、确认执行 `runUninstall` |
| **P0 卸载确认（文件侧）** | `FilePanel.tsx` | 同款确认框（含文件名，说明曲线与关键值将一并清除） |
| 确认框组件 | `ConfirmDialog.tsx/.css`（新增） | 仅用 `--ab-*` 语义变量；`role=dialog`+`aria-modal`；打开聚焦取消键（破坏性操作默认安全项）；Escape/遮罩=取消；关闭后焦点归还唤起元素；确认键红色 danger 样式 |
| 左栏常驻横向滚动条 | `AppShell.css` | 侧栏 `overflow-x: hidden`（拖拽手柄 ±3px 骑跨定位落在滚动容器内是溢出源，裁剪后用户可见滚动条消失，手柄仍在） |
| 星标可访问名 | `MetricTree.tsx` + i18n | aria-label 由静态"收藏"改为 `收藏 {{name}}`/`取消收藏 {{name}}`（随选中态切换，读屏可区分多个星标）；`fav_toggle` 键移除 |
| act() 警告收敛 | `viewport-fit.test.tsx` | `wireInvokes` 补 `list_user_presets`；新增 `close()`（裸 act 冲微任务后再卸载）。注：act 内 await 真实 setTimeout 会与真实 ECharts 组合冲突导致 cleanup 阶段 document 丢失（实测），故 act 只能裸调用；waitFor 轮询期间天然存在 1 条落在 act 外的更新（既有模式，未新增） |

### 5.3 回归与 GUI 验证

- **全量回归全绿**：typecheck ✓ / ESLint ✓ / `check:i18n` 237 键一致 ✓ / vitest **304/304** ✓。
  测试同步更新：FilePanel 卸载用例（取消+确认双路径）、PluginManagerPage 卸载用例（同）、
  `session.snapshot` 卸载链路补确认点击、`viewport-fit` 卸载用例补确认点击、MetricTree
  星标断言升级为动态 aria-label。
- **GUI 实测（dev server + mock）**：插件「卸载」→ 对话框含 "Demo Tool" 与不可撤销警告 →
  取消保留 / 确认移除 ✓；文件「卸载」→ 对话框含 "game_session.csv" → 确认移除 ✓；
  左栏底部横向滚动条消失 ✓。截图：`scratch/ui-ux-eval-2026-09-06/fix_*.png`。

### 5.4 剩余待办（未在本轮处理）

1. 窄屏顶栏溢出（787px）+ 插件页内容列宽度——P1，需布局断点设计。
2. Y 轴单位标注与轴归属可读性——P1，涉及 options.ts 轴配置。
3. KV 抽屉标题重复、axisPointer 压图例、dataZoom 端标签格式——P3 打磨。
4. 拖拽区键盘语义与控件尺寸基线——P2 可访问性。

---

## 6. 修复轮二（2026-09-06 晚，继续修复与验证）

### 6.1 已实施修复

| 修复 | 文件 | 说明 |
|------|------|------|
| **P1 顶栏窄屏溢出** | `TopBar.css` + `AppShell.css` | 顶栏 `flex-wrap` + `row-gap`，`min-height:44px`（内容驱动行高）；`app-shell` 顶行 `grid-template-rows: auto 1fr` 随内容增高。**390px 视口 `docScrollW` 787→390，整页横向溢出清零**，图表区首屏可见 |
| **P1 插件页内容列宽** | `PluginManagerPage.css` | `__list`/`__install` `max-width` 720→960px（1280 视口占比 57%→75%） |
| **P3 KV 抽屉标题去重** | `KeyValuesPanel.tsx` + `AppShell.tsx` | 面板新增 `showHeading` prop（默认 true）；抽屉内 `showHeading={false}` 不重复渲染——抽屉内 h2 从 2 个减为 1 个 |
| **P3 游标标签压图例** | `chart/options.ts` | markLine label 从线顶端（图例行处）改为 `insideEndTop` + 边框色胶囊背景（网格内、可读） |
| **P3 dataZoom 端标签** | `chart/options.ts` | slider `labelFormatter` 统一走 `formatTime`（此前左端 `08:00:00`、右端 `1970-01-01 08:10:0…` 截断） |
| **P2 拖拽区键盘语义** | `FilePanel.tsx/.css` + `PluginManagerPage.tsx/.css` | 真实模式 dropzone：`role=button` + `tabIndex=0` + Enter/Space 打开文件选择 + `focus-visible` 焦点环（mock 模式保持 div，无意义焦点不引入） |
| **P2 控件触达尺寸** | `TopBar.css`/`FilePanel.css`/`PluginManagerPage.css` | 顶栏按钮/输入、面板主按钮 `min-height:30px`；插件行按钮 28px、文件行内紧凑按钮 26px（信息密度与触达平衡） |

### 6.2 P1「Y 轴无单位标注」判定修正（评估报告误报）

运行时截图核查（`f2_yaxis_units_check.png`）：Y 轴 name（单位）**已渲染且可读**，且 P2-02
轴色编码正常（轴名/轴线颜色 = 该轴首序列颜色，悬停序列时对应轴加粗强调）。评估报告
§2.2「轴上没有单位标注」系误读（把轴名当刻度）。多量纲可读性实际状态：分轴 ✓、
单位标注 ✓、轴色归属 ✓——该项关闭。

### 6.3 回归与 GUI 验证

- **全量回归全绿**：typecheck ✓ / ESLint ✓ / vitest **304/304** ✓ / i18n 237 键 ✓。
- **GUI 实测**（390×844 + 1280×720 双视口）：
  - 390px 工作台：顶栏三行整齐换行、无横向滚动、图表首屏可见（`f2_narrow_workbench_wrapped.png`）；
  - 390px 插件页：`docScrollW=390` 无溢出；1280px 列宽 960px；
  - KV 抽屉：标题仅 1 个（`f2_drawer_single_title.png`）；
  - 游标 markLine 标签为网格内胶囊、不再覆盖图例（`f2_markline_label_pill.png`）；
  - dataZoom 端标签无带日期截断文本（`f2_datazoom_labels.png`）；
  - Y 轴单位标注 + 轴色编码可读（`f2_yaxis_units_check.png`）。
  - 拖拽区键盘语义为真实模式特性，mock GUI 无法触发文件对话框，逻辑由代码审查覆盖
    （role/tabIndex/onKeyDown 条件挂载 + focus-visible 样式）。

### 6.4 两轮修复后的剩余项

- 错误横幅无自动消退（P3 低，长驻留易过期）。
- 指标描述英文混排（需插件 schema 双语或宿主映射，跨仓库项）。
- 控件 12px 字号仅剩行内紧凑按钮（`--ab-fs-xs`），主操作均已 ≥12px+30px 高。
- WebView2 无障碍树暴露、原生对话框标题本地化（real/桌面壳范围，需打包环境）。
