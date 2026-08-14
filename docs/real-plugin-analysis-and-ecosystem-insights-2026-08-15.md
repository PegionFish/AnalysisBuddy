# 真实插件联合分析：BatteryInfoView × HWiNFO 对产品与生态的启示

> 分析基线：主仓 commit `1c4442d`；插件仓 `C:\Users\PegionFish\Desktop\AnalysisBuddy_BatteryInfoView`（下称 BIV）、`C:\Users\PegionFish\Desktop\AnalysisBuddy_HWiNFO`（下称 HWiNFO），均为 0.1.0 / GPL-3.0 / Python 插件、git 5 提交无 tag。
>
> 本文为只读研究产物，不含任何代码变更。涉及协议/manifest 变更的条目均标注「需 CCP」（契约变更提案流程见 `docs/developer-guide/contract-change-proposal-template.md`）。场景预设（Preset）功能正在另一实施流程中，本文第四节仅做契合度分析、不建议改动该计划。

## 结论速览

1. 两个真实插件与主仓 `sdk/python` 0.1.0 和 protocol-v1 高度一致，无超前使用协议特性；但暴露出 5 处轻量约定漂移/空白（详见第二节）。
2. 插件 zip 合规的唯一硬门槛是「plugin.json 位于 zip 根目录」——GitHub 源码 zip（带仓库前缀目录）不合规，生态缺官方打包/校验工具。
3. 抽象时间模式可纯映射层实现、不动协议 v1、默认关闭、切换零成本；时间轴滑条已上线，曲线拖拽校准为中等定制并与抽象时间模式构成交互闭环。
4. GB 级文件当前被 100MB 导入限额直接拒绝（第一道墙）；即使放宽，插件 load 全量预扫描、内置 CSV 整文件读取、store 全量驻留内存、查询窗口复制四个瓶颈会依次击穿，需按 P0-P2 分级治理。

---

## 一、两插件实现画像

### 1.1 BIV（batteryinfoview，TXT 电池日志）——定长列解析范式

| 维度 | 关键设计 | 证据 |
|---|---|---|
| manifest | 仅 6 个必填字段；`match.extensions=["txt"]`，指纹 `["AC Power","DC Power"]`；未用任何可选分发字段 | `AnalysisBuddy_BatteryInfoView\plugin.json` L1-14 |
| 解析 | 无表头定长 7 列，`csv.reader([line])` 引号感知切列（天然处理 `"99,072"` 千分位）；数值列 strip 逗号/`%` | `parser.py` L54-72 |
| 时间 | 正则预校验 + `strptime` 双格式（`%m/%d/%Y %I:%M:%S %p` / `%d/%m/%Y`）；**本地时间直接标为 UTC 毫秒不做时区换算** | `parser.py` L9-10、L33-51 |
| 状态索引 | `BivIndex` 对 power_state/log_type 维护有序双列表，`state_at(T)` 用 `bisect` O(log n) 取值 | `parser.py` L75-101 |
| config | 私有静态 config.json（time_format/encoding）：缺失→全默认、坏 JSON→全默认+WARN、未知键忽略 | `main.py` L27-28、L71-86 |
| 编码探测 | BOM（utf-8-sig/utf-16）→ 无 BOM 按 utf-8 replace 读前 5 行，U+FFFD 占比 ≥10% 回退 GBK | `main.py` L31-56 |
| can_handle | 时间正则 + AC/DC Power 双信号打分：0.9 / 0.5 / 弃权 | `main.py` L100-109 |
| load_file | **全文件预扫描**建全量索引并统计；好行 <2 或首行非法 → `FileLoadFailedError` | `main.py` L111-161 |
| schema | 静态 4 指标（电量/满充/当前/设计容量，均 avg），中文显示名 | `main.py` L163-169 |
| parse | 二次重读；raw_line 每 500 条抽样；每 2 万行心跳，收尾 percent=100 | `main.py` L171-205 |

### 1.2 HWiNFO（hwinfo-log，CSV 硬件监控日志）——自描述 CSV 解析范式

| 维度 | 关键设计 | 证据 |
|---|---|---|
| manifest | `extensions=["csv"]`，指纹 `["Date,Time,"]`；仅必填字段 | `AnalysisBuddy_HWiNFO\plugin.json` L1-8 |
| schema 冻结 | load 期表头切列 + **前 1000 行样本**判列类型（numeric/bool/drop）；Date/Time 恒 drop | `parser.py` L121-146；`main.py` L209-215 |
| 单位提取 | 列名尾部 `[unit]` 正则提取进 MetricDef.unit | `parser.py` L14、L131-135 |
| metric_id | 去单位后缀→小写→非 `[a-z0-9_]` 替 `_`→折叠下划线；冲突追加 `_2/_3` | `parser.py` L99-114 |
| 日期探测 | 两分支都返回 d.m.y（首字段 >12 才无歧义，否则诚实放弃靠 config 覆盖） | `parser.py` L71-96 |
| can_handle | 结构化三段打分：0.6+0.2+0.1 封顶 1.0 | `main.py` L51-66 |
| load_file | 编码 auto 含 GBK 二次回退；**全文件预扫描**计行与时间范围；schema 冻结 | `main.py` L68-125、L217-226 |
| schema 聚合 | 全部已 load 文件数值列**并集去重**（metric_id 先到先得）；aggregation 一律 avg | `main.py` L127-141 |
| parse | 逐行逐列；bool 列按配置产 1/0；percent 按 load 期 row_count 估算 | `main.py` L143-179 |

**真实数据形态**（fixture `tests\fixtures\hwinfo_sample.csv`，**523 列**）：层级命名（`P-core 0 Voltage [V]`、`E-core (LP) 14 Clock [MHz]`）、聚合后缀（`(avg)/(sum)`）、**跨设备重名列**（`GPU Clock [MHz]` 出现两次——独显+集显）、空单位（`Current cTDP Level []`）、特殊字符（`Framerate Presented (1% low) [FPS]`、`IA: PROCHOT [Yes/No]`）、非常规单位（`°C`、`GT/s`、`x`、`T`、`Wh`、`KB/s`、`FPS`）。

### 1.3 两仓共性模式（可模板化沉淀）

- config 容错三件套：缺失→默认 / 坏 JSON→默认+WARN / 未知键忽略（两仓逐行同构）。
- raw_line 抽样各自手写 `total % 500`；心跳每 2 万行 + SDK 2s 守护兜底。
- 各自维护 ~150 行 `analysisbuddy_stub.py` 测试替身 + conftest 注入（已出现与真实 SDK 的语义漂移，见第二节 #5）。
- 工程化齐备：GPL-3.0 LICENSE、README（列定义/配置表/维护指引）、pytest 全套；但均无 git tag/Release、manifest 无分发字段。

---

## 二、一致性核查结论（对 sdk/python 0.1.0 与 protocol-v1）

**总体：高度一致，无超前使用；5 处轻量漂移/空白，均不构成破坏。**

| # | 核查项 | 结论 | 证据 |
|---|---|---|---|
| 1 | SDK 公共 API 面 | 一致：仅子类覆写 `on_*` + `serve()` + `EmitContext` 文档面内 API | 两仓 main.py；主仓 `sdk\python\analysisbuddy\plugin.py` L65-167 |
| 2 | 协议方法使用 | 一致：10 方法按协议语义；未实现 annotate（capabilities 自动探测 false，符合设计意图） | 主仓 `plugin.py` L103-124 |
| 3 | manifest 合规 | 均可过 schema；但 author/repository/update_url/changelog/tools 全部未用 → 更新链路对两仓休眠 | 两仓 plugin.json；主仓 `plugin_manager.rs` L668-690 |
| 4 | skip-if-empty 约定 | **轻度漂移**：can_handle 返回 `"reason": None`（JSON null）；约定仅强制 Record，SDK 对 handler result 只透传不清洗 | BIV `main.py` L106；HWiNFO `main.py` L66 |
| 5 | 测试替身漂移 | stub 的 `on_parse` 默认 `return 0`（真实 SDK 抛 -32005）；`on_annotate` 默认返回 `{"events":[]}`（真实抛异常） | HWiNFO `tests\analysisbuddy_stub.py` L123-133 |
| 6 | 指纹健壮性 | HWiNFO 指纹 `"Date,Time,"` 无法命中带引号表头（`"Date","Time"` 形态），只削弱发现期预筛 | HWiNFO plugin.json L6 |
| 7 | 运行时依赖声明空白 | entry 均为 `python`（解释器例外合法），版本要求只存在 README 文字，Spawning 失败无定向提示 | 两仓 plugin.json；protocol-v1.md §7.3 |
| 8 | 心跳/批处理 | 一致：≤2s 义务满足；raw_line 抽样控批体积符合 §3.2 建议 | 两仓 parse 段；主仓 `context.py` L212-221 |

---

## 三、新思路清单（按价值分级）

### 3.1 高价值

**H1｜load_file 全量预扫描 vs 10s 超时预算（GB 级下必然击穿，见第七节 P0）**
两个真实插件都在 load_file 内整文件扫描（BIV 建全量索引；HWiNFO 全量计行后 parse 再扫一遍）。协议 load_file 超时仅 10s（protocol-v1.md §6），数百 MB 起即超预算、插件按 -32002 报废。落地：短期在开发者指南新增「load 性能纪律」（load 只做 O(表头+样本)，hint 本就是粗估字段）；中期 v1.1 CCP 考虑 load 阶段 progress 或两阶段语义。

**H2｜插件配置从「私有黑盒文件」走向「manifest 声明 schema + 宿主可视化 + 运行时下发」**
两仓各自实现同构的 config.json 容错逻辑，证明这是通用需求；HWiNFO README 明示 date_format 歧义需用户手改文件重新 load。协议已预留 `parse.options` 扩展点。落地：manifest 可选 `config_schema`（key/类型/默认/枚举/双语文案）→ 宿主渲染设置面板、写入仍落插件目录 config.json → 变更触发 reload。〔需 CCP，仿 presets 纯追加路径〕

**H3｜指标语义层增强：分组/限定符/聚合语义指导，应对高基数动态 schema**
HWiNFO 实测 523 指标；跨设备重名列被迫以无语义 `_2` 后缀区分，两条 `GPU Clock [MHz]` 显示名完全相同用户无法分辨；MetricDef 仅五字段无分组/限定符。聚合语义问题：累计量（`Total Host Writes [GB]`、PCIe 错误计数）被一律 avg，应 sum/last；bool 列 avg 实为占空比。落地：短期 UI 启发式分组 + 指南补聚合语义决策表；中期 CCP 追加 MetricDef 可选 `group`/`qualifier`/`source_column`。

**H4｜官方插件测试基座（test kit）+ 插件 CI 模板，消灭双仓重复造 stub**
两仓各复制 ~150 行 stub 且已漂移。落地：SDK 增 `analysisbuddy.testing` 子模块（通知收集器、内存管道假宿主、生命周期驱动器）+ 官方插件仓模板（GitHub Actions：plugin-validator 校验 + pytest）。

### 3.2 中价值

- **M1｜SDK 共享文本工具层**：编码探测/切列/日期探测两仓逐行重复；head_sample 按协议是 UTF-8 宽松解码，GBK 文件到插件已满屏替换符、被迫重开文件读原始字节。落地：SDK 增 `analysisbuddy.textio` 纯工具模块；v1.1 可考虑 CanHandleParams 追加 BOM/字节统计提示〔需 CCP〕。
- **M2｜confidence 打分规范 + 多插件争抢 UX**：BIV 两档 / HWiNFO 组合打分 / demo-tool 二值，三种风格并存；builtin-csv 也声明 csv/txt 必然争抢。落地：指南推荐分档（<0.5 弃权、0.5~0.7 弱认领带 reason、≥0.9 强认领）；争抢时 UI 展示各插件 reason 供人工改判。
- **M3｜布尔/状态列展示语义分工**：HWiNFO 约 120 个 `[Yes/No]` 列建模为 1/0 折线语义差，更接近 annotate 事件带/key_values，但 annotate 在真实生态零采用。落地：UI 为值域 {0,1} 指标提供阶跃/事件带渲染（场景无关机制）+ 指南决策表。
- **M4｜进度 percent 自动化**：SDK 提供按文件偏移自动估算 percent 的可选能力，作者登记句柄、心跳自动附 bytes_read/percent。
- **M5｜分发元数据生态卫生**：主仓更新链路（check_plugin_update → GitHub Releases 单 zip → semver 比较）对第三方完全空转。落地：发布插件仓模板（update_url/changelog/tools 示例 + tag 规范）+ docs 09 上架清单。
- **M6｜运行时依赖可声明化**：manifest 可选 `runtime` 声明（如 `{"python": ">=3.10"}`）〔需 CCP〕，宿主 Spawning 前探测并给定向错误（呼应 WebView2 引导 UX 哲学）。

### 3.3 供参考

- **L1｜本地时间直读 UTC 的跨时区隐患**：两 parser 都把 naive 本地时间直接标 UTC（BIV parser.py L51；HWiNFO L96）。是第六节「time_basis 声明」的直接动因。
- **L2｜raw_line 抽样约定各自为政**：可沉淀为 SDK 助手；BIV 抽样行 4 条 Record 全附 raw_line 放大批体积。
- **L3｜日期顺序歧义客观不可解**：HWiNFO `detect_date_format` 两分支同返 d.m.y 是诚实处理，反向强化 H2（配置需要 UI 而非手改文件）。
- **L4｜key_values 可作「文件档案面板」通道**：暴露已解析的编码/日期格式/列数等文件级事实。
- **L5｜派生指标需求萌芽**（BIV 满充/设计容量之比 = 电池健康度）：「宿主侧表达式派生通道」作 v2 备选储备，不建议近期立项。

---

## 四、插件独立仓库与 ZIP 分发生态（NDA 权责分离）

用户目标：**所有插件各自维护独立 git repository，通过分发 zip 包安装与配置**；工作场景涉及 NDA 工具，需要核心应用与插件的权责分离。

### 4.1 现状链路证据

- **安装**（`core\ab-app\src\commands\plugin_manager.rs`）：限额 zip ≤100 MB / 条目 ≤2000 / 单条目 ≤500 MiB / 累计 ≤1 GiB（L34-41）；zip-slip 防护（L242-251）；**L280 要求 plugin.json 位于 zip 解压根目录**；安装落点为 exe 旁 Portable `plugins/`（L92-93），临时目录 + 同卷 rename 原子搬入；冲突判定：内建保护 / 同版本拒绝 / 异版本需 overwrite。
- **三源发现优先级**（`core\ab-host\src\discovery.rs`）：Portable(0) > InstallDir(1) > UserData(2，`%APPDATA%\AnalysisBuddy\plugins`)。
- **更新**：`update_url` 必须为 GitHub `owner/repo`（L611）；取最新 Release tag 转 semver 比较（L672 起）；更新下载 zip **无校验和、无签名**；`.ab-modules.json` 只记状态不记来源。
- **断言工具缺口**：`scripts\verify-zip-manifest.ps1` 只管主应用发布包（`AnalysisBuddy/` 前缀 + PE 架构断言），**插件 zip 没有任何布局/内容断言工具**。

### 4.2 第三方独立仓库产出合规 zip 的硬性要求

1. **zip 根目录直接是 plugin.json**（不允许前缀目录）——GitHub "Download ZIP" 源码包必然被拒，必须专门打包。
2. 满足限额、无路径穿越条目；manifest `id` 唯一稳定、`version` semver。
3. 接入更新链路：manifest 带 `update_url`，仓库以 semver tag 发 Release 且该 Release **恰好一个 zip 资产**，zip 内 id/version 自洽且严格递增。

### 4.3 缺口清单（G1-G7）

| # | 缺口 | 影响 |
|---|---|---|
| G1 | GitHub 源码 zip 不合规，但宿主无官方打包工具/文档模板 | 第三方首次安装极易失败 |
| G2 | 无插件 zip 布局断言脚本 | 插件作者无法在 CI 自检合规性 |
| G3 | 两真实仓无 tag/Release/update_url | 更新链路对它们完全不可用，只能手动重装 |
| G4 | 更新 zip 无校验和/签名；无安装来源记录 | NDA 场景来源不可追溯、不可审计 |
| G5 | **升级即丢插件数据**：安装管线先删旧目录再 rename 新目录，更新复用同管线 → 插件自带 config.json 每次升级被删除 | 权责边界被意外破坏；宿主无迁移钩子 |
| G6 | 默认落点 Portable 目录且优先级最高 | NDA 插件与主应用目录混放；同名时 UserData 插件被遮蔽 |
| G7 | 插件进程为 stdio 子进程，无文件系统沙箱 | 「插件数据不出插件目录」只能靠纪律约束 |

### 4.4 「独立插件仓库标准模板」要素（差距即清单）

1. 标准目录布局：`plugin.json`（仓库根）、入口脚本、config.json 模板、`tests/`（SDK stub 可直接模板化）、README（安装/更新/NDA 数据纪律节）、LICENSE。〔插件仓〕
2. **打包脚本**：产出「plugin.json 在 zip 根」的发布包，明确禁止 GitHub 源码 zip。〔插件仓〕
3. **CI 发布管线**：semver tag 触发 → 打包 → 合规自检 → 上传为 Release 唯一 zip 资产 → 附 SHA256SUMS。〔插件仓 + 宿主补校验〕
4. manifest 完整字段纪律：version（与 tag 一致）、update_url、author、repository、changelog。〔schema 已支持，补齐即可〕
5. **NDA 数据纪律声明**（README 必备节）：插件数据只写插件自身目录；卸载即彻底清除（现状 `remove_dir_all` 恰好满足「数据随插件走」，NDA 角度是优点）；升级时插件自行迁移 config.json（G5 修复前的临时纪律）。
6. 可选：签名/校验和文件（模板预留，宿主验证能力是 G4 缺口）。

### 4.5 宿主侧改进建议

| 建议 | 职责 | CCP | 可行性 |
|---|---|---|---|
| 官方 `ab-plugin pack` 打包/校验命令或 CI Action（补 G1/G2） | 宿主 | 否 | 轻量定制 |
| 升级前保留/迁移插件数据目录（区分代码目录与数据目录，或 preserve 清单）（G5） | 宿主 | **是**（目录主权模型） | 中等定制 |
| 安装来源记录进 `.ab-modules.json` + 更新下载 SHA256 校验（G4） | 宿主 | 校验否 / 签名体系是 | 中等定制 |
| NDA 插件建议装 UserData 源 + UI 标注来源优先级冲突（G6） | 宿主 UI + 文档 | 否 | 现成可做 |
| 仓库模板 + 打包脚本 + CI Release 管线 | 插件仓 | 否 | 现成可做 |

---

## 五、与 Preset 方案的契合度（仅分析，不建议改在实施计划）

> 对照物：在实施的 Preset 设计（穷举精确 → keywords 整体模糊兜底 → 零命中不动选择）。观察事实：04 指南已含 presets 章节但 schema 文件尚未含 presets 定义——实施在途，本分析以设计文本为准。

**BIV：契合度高，穷举表的理想样板。** 静态 4 个跨机器恒定的 metric_id，穷举 names 硬写 4 条永不失配；双语 name 已存在。唯一缺口：最有价值的状态信息（power_state/log_type）走 key_values 通道，而预设只作用于 metric 选择——机制边界而非缺陷。

**HWiNFO：契合度中偏低，暴露穷举表对「动态 schema 插件」的结构性张力：**
- **跨机器不可移植**：指标集由本机硬件决定（523 列），`gpu_clock` / `gpu_clock_2` 的出现取决于是否有独显+集显，写死 `_2` 后缀的穷举表在另一台机器会错配/失配。
- **重名歧义**：两条 `GPU Clock [MHz]` 显示名完全相同，大小写不敏感匹配会命中两条，`want` 首个命中按列序随机决定设备——语义不确定。
- **模糊兜底是预设级而非条目级**：混入任何可移植条目触发穷举命中后，其余不可移植条目失配不会落到模糊兜底、只进 unmatched 清单——实际是「半命中」，作者需预先理解该规则。
- **规范化损耗**：`Framerate Presented (1% low) [FPS]` → `framerate_presented_1_low`；非 ASCII 列名兜底为 `col/col_2`——候选表须同写规范化 id 与原始表头串（设计已支持双通道），`col` 类兜底 id 对预设无意义。

**给两类插件作者的预设写法指引**：静态 schema 插件（BIV 型）放心用穷举 names、可完全不用 keywords；动态 schema 插件（HWiNFO 型）优先 `want` 语义槽 + 宽候选族让「首个命中」吸收硬件差异，keywords 只作整预设完全失配的最后防线。真实插件证据支持「条目级模糊降级」与「模式/前缀候选」两个匹配语义微调方向，可作 Preset 上线后的数据驱动迭代输入。

---

## 六、多来源时间同步与抽象时间模式

### 6.1 关键事实核实

- **`t_ms` 是绝对 UTC epoch**：协议 `Record.timestamp` = UTC 毫秒（protocol-v1.md L244）；`Series.ts` 同为 UTC ms、freeze 后严格非降（`core\ab-pipeline\src\store.rs` L18-23）。
- **但抽象时间模式依然是纯 UI/查询映射层策略，而非数据变换**：每文件起点 `start_ms` 已全链路透传到前端（ImportResult.time_range → LoadResult.time_ranges → session.ts L73-77、L200-211），归零所需 per-file origin **已在前端手里、无需新 IPC**；Frozen 存储只读（store.rs L37-43），任何数据变换方案都违背架构。
- **当前只有一条全局绝对时间轴**：`run_query` 以单一闭区间对跨文件所有序列统一二分（query.rs L16-24）；视口按并集适配（session.ts L510-533）。
- **插件侧错位根因**：BIV parser.py L51 与 HWiNFO parser.py L96 均为本地 naive 时间直接标 UTC——协议要求 UTC，插件只能诚实地把机器本地时间当 UTC 上报。

### 6.2 叠加显示会发生什么（问题）

1. **假性对齐/系统性错位**：不同时区/时钟配置机器的两份「直标 UTC」日志叠加后错位数小时且用户无感知（协议无时区/时间基准声明字段）。
2. **起始时刻悬殊 → 视口被并集拉爆**：文件 A 全天记录、文件 B 只记 10 分钟测试 → 并集视口下 B 压成细线；放大 B 则 A 完全出窗。
3. **只有相对时间的工具**：伪造的绝对值毫无对比意义。
4. 采样率不同本身不是问题（LTTB 已处理），真正问题是时间域错位。现状无「各自为轴」退路。

### 6.3 机制设计：抽象时间模式（可选开关，默认关闭）

**硬约束**：会话级用户开关；默认 = 现状绝对时间轴（完全向后兼容）；开启 = 各文件起点归零 00:00:00（或对齐用户指定锚点）；可随时切换、不触碰存储数据、切换零成本；**绝不强制覆盖任何测试场景**。

**对齐策略三级**：

| 级别 | 语义 | origin(file) | 职责/CCP |
|---|---|---|---|
| ① 起点归零 | 各文件自身 start 为 0 | `time_range.start_ms`（已有数据） | UI + 宿主查询命令层，**无需协议改动** |
| ② 锚点对齐 | 以「开始测试」类事件为共同 0 点 | 锚点事件 timestamp | 需事件选择模型，**建议 CCP** |
| ③ 拖拽微调 | 手动平移某文件 | ①/② 基础上 + 用户偏移 | 见 6.5 交互闭环 |

**最小实现路径（级别①）**：
- 显示层：展示坐标 = `t_ms − origin(file)`；tooltip/轴标签/游标 formatter 统一入口已存在（options.ts L246/L285），双显「抽象时间 + 原始时间」仅需扩展这两个 formatter。
- 查询层：per-file 窗口不同（`t_abs = t_abstract + origin_i`），建议 `query_series` 增加 per-file origin 映射参数——host 内部命令，**不触碰冻结的插件协议 v1**（本方案关键优势）。游标 `key_values_at` 同样 per-file 反映射。
- **偏移落点分工（GB 级约束下的最优解）**：窗口反映射必须在查询层（二分正确性前提）；点坐标加常数放渲染层最省（只对 ≤4000 降采样点做加法，且 LTTB 对 x 轴常数平移不变）。总代价与文件大小无关。反向验证：任何在 store 里改 ts 的方案需 O(n) 重写 + 破坏 Frozen 只读，GB 级下绝对不可行——纯映射是唯一可行路径。
- 持久化：模式开关 + per-file 偏移进会话快照（`.absession` 可选字段、缺省回落绝对模式，向后兼容）。〔快照兼容策略建议过 CCP〕
- **混合可靠性场景**（部分文件真绝对时间、部分没有）：现状宿主无法区分两者。近期：抽象模式下一视同仁归零天然规避误导；绝对模式下靠插件 README/manifest note 提示。远期：`FileSummary` 增可选 `time_basis`（`utc`/`local_as_utc`/`relative`）由插件诚实声明〔**需 CCP**，可选字段向后兼容〕；绝对模式下对 `time_basis != utc` 文件显示「时间基准不确定」徽标。

### 6.4 拖拽校准与时间轴滑条（交互子节）

- **时间轴滑条已是现成且已上线能力**：options.ts L321-324 配置了双 dataZoom（inside 滚轮缩放/拖拽平移 + 底部 slider），视口联动闭环已存在（datazoom → 150ms 防抖 → `chart/window` → 重查询，TimelineChart.tsx L62-79）。**用户想要的「下方滑条辅助精确定位」今天就能用。**
- **拖拽曲线语义 A（价值最高，与抽象时间强联动）**：按住某文件曲线水平拖动 = 实时调整该文件 origin 偏移 → 松手落定 per-file offset → 重查询 → 快照持久化。滑条粗定位、拖拽直觉微调、游标读数核对构成闭环；状态模型只需新增 `fileOffsets: Record<file_id, number>`。技术路径：echarts 无内建曲线拖拽，需 `getZr().on(mousedown/mousemove/mouseup)` + 自研命中判定（`large + symbol:'none'` 下无图形可命中——任务 23 已验证 series 级事件永不触发，需 convertToPixel 采样取最近序列或临时隐形 symbol）；拖拽中间态避免逐帧 setOption 全量重建（当前 notMerge:true）。**可行性：中等定制**（基础设施全部就位）。
- **拖拽语义 B（框选时间区间）**：echarts 内建 brush `lineX` 现成可用，但与 inside dataZoom 拖拽平移手势互斥，需「选择/平移模式」切换。**可行性：现成能力 + 轻量配置**。
- **滑条不建议自绘**：现成 dataZoom slider 即全域缩略 minimap；仅当出现 per-file 泳道/锚点事件轨/多游标需求时才值得自研（依赖抽象时间模式落地后再评估）。
- 注意：`onDataZoom` 的 `Math.max(0, …)` epoch 钳制（TimelineChart.tsx L68-69）与抽象轴从 0 起步恰好兼容，但绝对模式假设需在重构时参数化。

### 6.5 可行性分级

| 项 | 分级 |
|---|---|
| 时间轴滑条 / 视口拖拽平移缩放 | **现成能力，已实现** |
| brush 框选时间区间 | 现成能力 + 轻量配置（需模式切换） |
| tooltip/游标双显抽象+绝对时间 | 轻量定制（formatter 入口已统一） |
| 模式开关 + 起点归零（显示映射 + per-file 查询反映射） | 中等定制（UI + host 命令层，无协议改动） |
| 快照持久化模式/偏移 | 轻量定制（可选字段向后兼容） |
| 拖拽整条曲线做偏移校准 | 中等定制（zrender + 自研命中判定） |
| 锚点事件对齐 | 中等~自研（**需 CCP**） |
| `time_basis` 协议扩展 | **需 CCP** |
| per-file 泳道 minimap / 锚点轨 | 需自研（后置评估） |

---

## 七、GB 级文件架构适配

**约束**：单个 CSV 文件最大可达 GB 级（如 20 列数值 × 数千万行），架构必须适配。

### 7.1 承受力现状核实

| # | 现状 | 证据 | GB 级后果 |
|---|---|---|---|
| ① | **导入限额 100MB 硬编码**（`max_import_bytes` 无 UI 配置、无插件放宽机制），超限直接 `invalid_arg` 拒绝、不进插件匹配 | `core\ab-app\src\pipeline_bridge.rs` L44-45、L71、L714、L1255-1267 | **GB 级文件连门都进不了——第一道墙** |
| ② | store 全量驻留内存（`RwLock<HashMap<file_id, FileData>>`），无 mmap/落盘/上限/LRU；freeze 排序构造三份临时缓冲，瞬时峰值 ≈ 常驻 2.5× | `core\ab-pipeline\src\store.rs` L242-248、L206-239、L341-358、L407-410 | 1GB CSV（20 列 × 4000 万行 ≈ 8 亿点 × 16B）≈ **12.8 GB 常驻、freeze 峰值 ~30 GB**；另有 8 亿条 Record 经 NDJSON stdio（~64-80 GB 流量、按实测吞吐约 16-26 分钟） |
| ③ | run_query 窗口先 `to_vec` 整窗复制再判降采样 | `core\ab-pipeline\src\query.rs` L61-66 | 「重置缩放」=全窗 → 每选中序列瞬时复制数 GB，纯浪费 |
| ④ | builtin-csv load 整文件 `std::fs::read` + 解码 String 长期持有 | `plugins\builtin-csv\src\engine.rs` L201、L404-406、L1061 | 单文件 load 峰值 ≥2-3 GB 且击穿 10s 预算 |
| ⑤ | 两个真实插件 load 全量预扫描 | BIV `main.py` L133-143；HWiNFO `main.py` L96-110 | Python 逐行解析 GB 级 = 分钟级，**必然 Timeout 插件报废**（不是优化问题） |
| ⑥ | UI 侧 4000 点降采样预算在后端查询期完成（LTTB），UI 只拿降采样结果 | query.rs L64-69；session.ts L27、L560；options.ts L42-49 | **交互层点数规模对 GB 级不敏感**，流畅性风险在 ②③ 不在渲染 |

### 7.2 瓶颈清单与对策（按优先级）

| 优先级 | 瓶颈 | 对策 | 职责 | CCP | 可行性 |
|---|---|---|---|---|---|
| **P0** | 100MB 导入限额 | 分级放宽：全局上限提到 GB 级护栏（如 2-4GB）+ manifest 可选 `max_file_bytes` 能力声明取较严者；UI 明示限额 | 宿主 | 能力字段扩展是 | 轻量~中等 |
| **P0** | 插件 load 全量预扫描 | load 纪律：只做头部采样（表头/前 N 行/seek 读末几行）估算 range 与 hint（协议本就允许粗估/缺省）；精确统计推迟到 parse 完成由宿主汇总；**写进开发者指南 02/06**；两真实仓需改造 | 插件 + 文档 | 否 | 现成可做 |
| **P0** | builtin-csv load 整文件读 | load 改流式头部探测（BufReader 读头尾）；parse 逐块流式，不持有整文件 String | 宿主 | 否 | 中等定制 |
| **P1** | store 全量驻留无上限 | 三步走：① 导入前按 hint×列数预估内存并告警/限额（近期）② 序列按时间块分片 + 查询期按块裁剪 LTTB 归并（中期）③ Frozen 落盘为内存映射只读列文件（远期） | 宿主 | ②③ 是 | ①轻量 ②③自研 |
| **P1** | freeze 峰值 2.5× | 原地置换排序/分块外排；或协议文档引导插件尽量时序输出使 freeze 退化为校验 | 宿主 + 插件纪律 | 否 | 中等定制 |
| **P1** | run_query 窗口复制 | LTTB 迭代器化直读原切片（`Arc<Series>` 零拷贝已在手），仅输出 ≤budget 新 Vec | 宿主 | 否 | 中等定制 |
| **P2** | NDJSON 传输总量 | 纪律：批次取下限 1000~2000；远期二进制/压缩传输（协议冻结，变更需 CCP） | 插件 + 远期协议 | 远期是 | 近期现成 |
| **P2** | 指南缺大文件纪律章节 | 新增「大文件处理纪律」：load 只采样、parse 流式+心跳、禁止整文件 read、hint 粗估语义 | 文档 | 否 | 现成可做 |

### 7.3 与抽象时间/拖拽方案的交叉确认

**结论成立：GB 级下所有交互只操作降采样渲染数据与视口元数据，不触碰原始序列。**
1. 原始序列只在 run_query 以 `Arc<Series>` 只读句柄被读（store.rs L419-426），出查询层即被 LTTB 压到 ≤4000 点；dataZoom/游标/滑条/拖拽全部建立在这 4000 点与 viewWindow 元数据之上。拖拽方案修改的是 origin 元数据与查询窗口反映射参数，从不改写 Series 数据——与 Frozen 只读架构天然相容。
2. 偏移落点（查询层窗口反映射 + 渲染层坐标平移）总代价与文件大小无关，满足「切换零成本、GB 级零重写零重索引」。
3. **唯一遗留耦合**：run_query 窗口复制（P1 项）在 GB 级 + 全窗「重置缩放」下会成为抽象时间/拖拽交互的响应瓶颈——**消除查询复制应与抽象时间方案同批实施**。

---

## 八、建议后续动作（不排期，仅清单）

1. **指南先行（零代码）**：load 性能纪律 + 大文件处理纪律（第七节 P0）、聚合语义决策表（H3）、confidence 推荐分档（M2）、「何时用 0/1 metric / key_values / annotate」决策表（M3）。
2. **插件仓工程化（现成可做）**：独立插件仓库标准模板（第 4.4 节六要素）、两真实仓补 update_url/tag/Release 与打包脚本、两仓 load 预扫描改造（第七节 P0）。
3. **宿主轻量改造**：官方插件打包/校验工具（G1/G2）、导入限额分级放宽、builtin-csv 流式化、run_query 复制消除（与抽象时间同批）、安装来源记录 + SHA256 校验。
4. **CCP 候选项（按价值排序）**：抽象时间快照字段与 `time_basis` 声明、插件 `config_schema`、MetricDef 分组/限定符、manifest `runtime` 与 `max_file_bytes`、插件数据目录主权（G5 升级迁移）、store 分片/落盘（远期）、签名体系（远期）。
5. **探索项（开放性）**：曲线拖拽偏移校准（6.4 语义 A）、brush 框选、锚点事件对齐、派生指标通道（v2 储备）。

---

## 附录 A：真实 HWiNFO 传感器名形态样本（预设穷举表的书写依据）

- 层级命名：`P-core 0 Voltage [V]`、`E-core (LP) 14 Clock [MHz]`、`IA: PROCHOT [Yes/No]`
- 聚合后缀：`Framerate Presented (avg) [FPS]`、`Total Host Writes [GB] (sum)` 类
- 跨设备重名：`GPU Clock [MHz]`、`GPU Memory Available [MB]`、`Total DL [MB]`、`GPU D3D Usage [%]` 各出现两次（独显+集显/双网卡）
- 空单位：`Current cTDP Level []`、`Gear Mode []`
- 特殊字符：冒号/括号/百分号（`Framerate Presented (1% low) [FPS]`）
- 非常规单位：`°C`、`GT/s`、`x`、`T`、`Wh`、`KB/s`、`FPS`
- 规范化结果示例：`framerate_presented_1_low`、`ia_prochot`；纯非 ASCII 列名兜底 `col/col_2`

## 附录 B：风险与限制说明

- 两插件仓无 git tag、无 Release 资产，分发链路结论基于主仓代码推断而非实际分发经验。
- HWiNFO 523 列证据来自仓内 fixture（真实 HWiNFO64 导出样本），跨机器差异结论基于列命名结构推理，未做多机器实测。
- GB 级内存估算按 16B/点常驻 + freeze 三份临时缓冲推算，未实测；NDJSON 传输时长按 protocol-v1.md 引用的实测吞吐线性外推。
- Preset 实施以设计文本为准；schema 文件当前无 presets 定义属在途状态，非本次分析对象。
