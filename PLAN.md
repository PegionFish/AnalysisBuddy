# AnalysisBuddy 开发计划

> 本计划由规划会话定稿（2026-08-07），是后续所有开发工作的唯一事实依据。
> 任何与本文件冲突的临时决定，以本文件为准；修改本文件须经主代理评审并说明理由。

---

## 1. 产品定位

**AnalysisBuddy** 是一个 Windows 桌面日志分析工作台：

- 导入不同程序（内部工具、游戏测试工具等）产出的格式化 Log 文件（CSV、TXT、无后缀纯文本等）；
- 通过**插件**将各工具的 Log 解析为统一的时序数据；
- 在基于时间戳的折线图上叠加显示用户自选的指标；
- 在任一时间戳（游标）处查看各插件报告的**关键状态值**（`key_values(t)`）；
- 会话（导入的文件集合 + 指标选择 + 视图状态）可保存、可重开。

**非目标（本期不做）**：实时 tail 跟踪、远程/网络日志源、Log 编辑与导出改写、超 100MB 文件的索引与分块读取。协议层为实时模式预留扩展位。

## 2. 已锁定的关键决策

| # | 决策点 | 结论 | 依据 |
|---|--------|------|------|
| 1 | 平台/架构 | Windows only，x86_64 + ARM64 | 需求指定 |
| 2 | 宿主技术栈 | **Rust + Tauri 2** | 解析性能好、包体小、官方 `aarch64-pc-windows-msvc` target、WebView2 + ECharts 图表生态成熟 |
| 3 | 插件模型 | **独立进程 + JSON-RPC 2.0 over stdio**（MCP 风格） | 语言无关；进程隔离；内部工具插件留在内网不进主仓库（保密约束的核心解法） |
| 4 | 数据源模式 | 仅导入已有文件（事后分析） | 架构最简；协议预留实时扩展位 |
| 5 | 数据量级 | 单文件 ≤100MB，全内存解析 | 无需 SQLite 持久缓存、无需多级降采样金字塔 |
| 6 | UI 语言 | 中英双语切换（i18n 从第一天做） | 用户拍板 |
| 7 | 文档语言 | 协议规范英文（对齐 MCP 惯例、Agent 友好）；开发指南中文（团队友好） | 用户拍板 |
| 8 | 分发形态 | NSIS 安装包 + 绿色免安装 zip，双架构各出一份 | 用户拍板 |
| 9 | 首期 SDK | Python + C# | Python 贴合游戏测试工具链，C# 贴合 Windows 生态 |
| 10 | 许可证 | GPL-3.0 | 仓库已定；依赖选型须兼容（优先 MIT/Apache） |

## 3. 总体架构

```
┌──────────────────────────────────────────────────────────┐
│  前端 UI（WebView2，React + TS + ECharts，i18n 双语）      │
│  文件面板 │ 指标选择树 │ 时间轴折线图 │ T 时刻关键值面板     │
└────────────────▲─────────────────────────────────────────┘
                 │ Tauri IPC（命令 / 查询 / 事件流）
┌────────────────┴─────────────────────────────────────────┐
│  Host 核心（Rust）                                        │
│  ┌───────────┐ ┌───────────┐ ┌───────────┐ ┌──────────┐  │
│  │ 导入/会话  │ │ 解析管线   │ │ 内存存储   │ │ 插件运行时 │  │
│  │ (session) │ │ (dispatch)│ │ + 查询API  │ │ (runtime)│  │
│  └───────────┘ └─────▲─────┘ └───────────┘ └────┬─────┘  │
└──────────────────────┼──────────────────────────┼────────┘
          JSON-RPC 2.0 over stdio（AnalysisBuddy Plugin Protocol v1）
┌──────────────┐ ┌──────────────┐ ┌──────────────┐
│ CSV 通用插件  │ │ 示例工具插件  │ │ 内部工具插件  │ ← 独立进程、
│ (内置,Rust)  │ │ (demo)       │ │ (内网私有)   │   任意语言
└──────────────┘ └──────────────┘ └──────────────┘
```

### 3.1 插件协议（AnalysisBuddy Plugin Protocol v1）

传输：stdio，每行一个 JSON-RPC 2.0 消息（newline-delimited JSON）。宿主是 client，插件是 server。

**生命周期方法（宿主 → 插件）：**

| 方法 | 说明 |
|------|------|
| `initialize` | 握手：协议版本、插件元数据（id/name/version）、能力协商 |
| `load_file` | 加载一个 Log 文件（传路径），插件保留数据以支持后续查询；返回文件级摘要 |
| `unload_file` | 卸载文件，释放插件侧内存 |
| `shutdown` | 优雅退出 |

**能力/解析方法：**

| 方法 | 说明 |
|------|------|
| `can_handle(file_info)` | 入参含文件名、后缀、头部采样（前 N 行/字节）；返回是否可处理及置信度 |
| `parse(file_id)` | 流式返回归一化记录（分块 progress 通知 + 最终结果），支持超时与取消 |
| `schema()` | 声明本插件产出的指标清单：id、名称、单位、描述、聚合方式 |

**查询方法：**

| 方法 | 说明 |
|------|------|
| `key_values(file_id, timestamp)` | 返回该文件在时间戳 T 处的关键状态值集合（插件自定义语义，通常取 ≤T 的最新状态） |
| `annotate(file_id, range)` *(可选)* | 返回时间范围内的事件/标记，用于图上打点 |

**预留扩展位**：`subscribe(file_id)` / `push_records`（实时 tail 模式，v1 不实现，消息格式占位）。

**容错约定**：所有方法调用有宿主侧超时（解析类长任务用 progress 心跳续期）；插件崩溃/超时不影响宿主，UI 上报错误并允许重试；插件 stderr 由宿主捕获进插件日志面板。

### 3.2 插件发现

目录扫描，免注册表、可绿色部署：

1. 内置目录：应用安装目录下 `plugins/`（随包分发 builtin-csv、demo-tool）；
2. 用户目录：`%APPDATA%\AnalysisBuddy\plugins`（**内部私有插件放这里，绝不进主仓库**）；
3. 便携模式：exe 同级 `plugins/` 存在时优先（绿色版场景）。

每个插件一个目录：`plugin.json` manifest（id、入口命令、参数、支持的后缀/文件头指纹、最低协议版本）+ 可执行文件。manifest 有 JSON Schema 校验（`plugin check` CLI 复用同一 Schema）。

### 3.3 数据模型与管线

```
导入文件 → 插件匹配（manifest 指纹 / 用户手选） → 拉起插件进程（或复用常驻进程）
→ load_file + parse（流式回传） → 归一化 Record → 宿主内存存储（按时间排序）
→ 查询 API（时间范围切片 + 可选 LTTB 降采样） → 前端渲染
```

**归一化记录**：

```
Record {
  timestamp:  i64,          // UTC 毫秒
  metric:     string,       // 插件 schema 声明的指标 id
  value:      f64,
  level:      string?,      // info/warn/error 等，可选
  tags:       map?,         // 可选维度
  raw_line:   string?,      // 原文引用（抽样保留，控制内存）
}
```

**内存策略（≤100MB 前提）**：宿主保留全部归一化记录用于画图；`raw_line` 按抽样保留；插件进程保留自己的原始数据副本以支持 `key_values(t)`。单文件解析完成后数据双份驻留属预期设计，会话关闭/文件卸载时回收。

**会话文件**（`.absession`，JSON）：文件列表（路径 + 内容哈希，重开时校验）、已选指标、图表视图状态、游标位置。**不缓存解析结果**——重开时由插件重新解析（≤100MB 下可接受）。

### 3.4 UI 设计

| 区域 | 内容 |
|------|------|
| 左侧·文件面板 | 导入（对话框/拖拽）、解析进度、启用/停用、卸载；显示匹配到的插件 |
| 左侧·指标选择树 | `文件 → 插件 → 指标` 三级树，复选框控制上图 |
| 中央·折线图 | ECharts 多序列；缩放/平移（dataZoom）；多 Y 轴；图例开关；>5 万点触发 LTTB 降采样 |
| 右侧·关键值面板 | 游标（axisPointer / 点击定位）停在 T 时，调各活跃插件 `key_values(file, T)`，属性网格式展示 |
| 插件管理页 | 已发现插件列表、健康状态、stderr 日志、重载、配置入口 |
| 通用 | 深浅色主题；中英双语（i18next，语言切换即时生效） |

## 4. 仓库布局与子代理目录主权

```
AnalysisBuddy/
├── README.md / LICENSE / PLAN.md
├── docs/
│   ├── spec/                    # 【E 路】协议规范（英文 RFC 式）+ JSON Schema
│   │   ├── protocol-v1.md
│   │   ├── plugin-manifest.schema.json
│   │   └── rpc-messages.schema.json
│   ├── developer-guide/         # 【E 路】插件开发指南（中文，面向人+Agent）
│   └── architecture.md          # 【主代理】架构文档
├── core/                        # Rust workspace
│   ├── ab-protocol/             # 【Phase1 冻结】共享类型 = 契约唯一事实来源
│   ├── ab-host/                 # 【A 路】插件运行时：发现/生命周期/RPC/超时/健康
│   ├── ab-pipeline/             # 【B 路】导入→解析调度→内存存储→查询 API
│   └── ab-app/                  # 【主代理】Tauri 壳 + IPC 胶水（集成裁判持有）
├── ui/                          # 【C 路】React + TS + ECharts + i18n
├── plugins/
│   ├── builtin-csv/             # 【D1 路】内置通用 CSV 插件（Rust 二进制，零运行时依赖）
│   └── demo-tool/               # 【D2 路】演示插件（Python，用 SDK 写，dogfood）
├── sdk/
│   ├── python/                  # 【D1 路】analysisbuddy-sdk (PyPI 风格包)
│   └── dotnet/                  # 【D2 路】AnalysisBuddy.Sdk (NuGet 风格包)
├── tools/
│   ├── plugin-validator/        # 【E 路】`plugin check` CLI（manifest+协议行为自检）
│   └── loggen/                  # 【F 路】合成 Log 生成器（测试/性能基准）
└── tests/
    ├── fixtures/                # 【F 路】样例日志夹具
    └── e2e/                     # 【F 路】端到端测试
```

**七路子代理分工**（Phase 2）：

| 路 | 负责 | 目录主权 | 依赖 |
|----|------|----------|------|
| A 插件运行时 | 发现、进程生命周期、RPC 帧、超时/健康监控 | `core/ab-host` | ab-protocol |
| B 数据管线 | 导入、解析调度、内存存储、查询 API、会话文件 | `core/ab-pipeline` | ab-protocol、A（mock） |
| C 前端 | UI 骨架、图表、指标树、关键值面板、i18n | `ui/` | 契约类型（mock IPC） |
| D1 Python SDK + 内置 CSV | Python SDK、builtin-csv 插件（Rust 实现但按协议契约） | `sdk/python`、`plugins/builtin-csv` | spec |
| D2 C# SDK + demo 插件 | C# SDK、demo-tool 插件 | `sdk/dotnet`、`plugins/demo-tool` | spec |
| E 文档 + 校验器 | 协议规范、开发者指南、validator CLI | `docs/`、`tools/plugin-validator` | spec（与 Phase1 同源） |
| F QA | 夹具、loggen、集成测试框架、性能基准 | `tools/loggen`、`tests/` | 契约类型 |

> 注：builtin-csv 用 Rust 写（随宿主静态分发，终端用户零运行时依赖）；demo-tool 用 Python SDK 写（dogfood SDK，开发环境运行）。D1/D2 按 SDK 语言拆分而非插件拆分。

## 5. 开发阶段与验收标准（DoD）

### Phase 0｜决策定稿 + 脚手架（主代理，约 1 批）
- [x] 8 项关键决策锁定（见 §2）
- [ ] monorepo 骨架：Cargo workspace（core/*）、ui/（Vite + React + TS）、目录占位
- [ ] CI（GitHub Actions）：x64 构建+测试；ARM64 交叉构建（`aarch64-pc-windows-msvc`，MSVC linker）；产物上传 artifact
- [ ] `.gitignore`、rustfmt/clippy/ESLint 基线
- **DoD**：空壳 Tauri 应用在 x64 与 ARM64 双架构均可构建出可运行产物

### Phase 1｜契约先行（单代理，关键路径，先行完成才开并行）
- [ ] `docs/spec/protocol-v1.md`（英文 RFC 式：消息格式、全部方法、错误码、超时约定、扩展位）
- [ ] `plugin-manifest.schema.json` + `rpc-messages.schema.json`
- [ ] `core/ab-protocol`：共享 Rust 类型（Record、各 RPC 请求/响应、manifest 结构）
- [ ] mock 插件 harness：能回放固定响应的假插件（供 A/B/C/F 提前联调）
- **DoD**：契约评审通过并打 tag `contract-v1`；此后契约变更必须走主代理审批 + 广播

### Phase 2｜七路并行开发
各路按 §4 目录主权推进，遵守 §6 并行规则。各路 DoD：

| 路 | DoD 摘要 |
|----|----------|
| A | 用 mock 插件完成发现→拉起→握手→parse→key_values→shutdown 全流程；崩溃/超时用例有容错；单测覆盖 RPC 帧与状态机 |
| B | 导入→调度→存储→查询全链路（对 mock）；会话保存/重开；时间切片与降采样查询有单测 |
| C | 全 UI 可用（mock IPC 数据源）：文件面板/指标树/折线图/游标关键值/插件页/双语切换/深浅主题 |
| D1 | Python SDK 可写一个合规插件并通过 validator；builtin-csv 能解析常见 CSV（含带/不带表头、时间列可配） |
| D2 | C# SDK 同上；demo-tool 输出 ≥3 个指标 + key_values 有真实语义 |
| E | 规范覆盖全部方法与错误码；指南含「从零写插件」步骤（人+Agent 视角）；validator 能检出 ≥10 类常见错误 |
| F | loggen 可生成指定规模/指标数的合成日志；集成测试框架跑通 mock 插件端到端 |

### Phase 3｜集成（主代理做合并裁判）
- [ ] 真实插件端到端：导入 → builtin-csv/demo-tool 解析 → 画图 → 游标 key_values
- [ ] 多文件多插件叠加同一时间轴验证
- [ ] 性能验证：50MB/100MB 合成日志解析耗时与内存达标（基准由 F 提供）
- [ ] ARM64 实机/模拟器冒烟
- **DoD**：E2E 套件全绿；性能基准报告入仓

### Phase 4｜打包发布
- [ ] Tauri bundler：NSIS 安装包 + 绿色 zip，x64/ARM64 各一份
- [ ] 绿色版便携模式插件目录验证
- [ ] 发布流水线（tag 触发）产物齐全
- **DoD**：四种产物可在干净 Windows 上安装/解压即用

## 6. 多子代理并行开发规则（强制）

1. **契约先行**：Phase 1 打 tag `contract-v1` 前不开并行；契约变更须经主代理审批并向所有路广播，受影响路同步修订。
2. **目录主权**：每路只写自己目录（§4）；跨目录需求以 issue/任务形式上报，严禁直接改他人目录。
3. **对契约开发**：下游一律用契约类型 + mock 开发，不等待上游完成；`ab-app` 集成胶水由主代理独占。
4. **小批量提交**：每批工作完成立即 commit；commit message 用 `feat/fix/docs/test/ci(scope): 描述` 格式。
5. **增量测试**：只测自己新增/变更的模块；全量回归由集成阶段统一跑。
6. **自主闭环**：各路自行完成 验证→测试→debug 闭环，只在架构级歧义或契约冲突时上报主代理。
7. **合并裁判**：冲突与集成顺序由主代理裁定；各路不得自行 merge 主干以外分支。
8. **依赖合规**：新增第三方依赖须检查许可证与 GPL-3.0 兼容（MIT/Apache/BSD 可，GPL 系需评审）。

## 7. 测试策略

| 层级 | 工具 | 覆盖 |
|------|------|------|
| 单元 | `cargo test` / vitest / pytest / xunit | 各路自有模块（RPC 帧、解析、查询、组件） |
| 契约一致性 | `plugin check` validator | manifest 合规 + 协议行为回放检查（SDK 插件 CI 必跑） |
| 集成 | tests/e2e（Rust harness 或 Node） | mock/真实插件 × 真实文件 → 查询 API |
| E2E/性能 | loggen + 基准脚本 | 50/100MB 解析耗时、内存峰值、图表渲染帧率 |
| 双架构 | CI matrix | x64 全量测试；ARM64 构建 + 冒烟 |

## 8. 风险与对策

| 风险 | 影响 | 对策 |
|------|------|------|
| ARM64 交叉构建环境配置繁琐 | Phase 0 卡壳 | 优先 GitHub Actions windows runner + MSVC ARM64 构建工具；不行则降级为「x64 全量 + ARM64 出包后人工冒烟」 |
| 插件进程 IPC 吞吐（大文件 parse 回传） | 解析慢 | 分块回传 + 批量 Record 编码；必要时支持插件侧写临时二进制文件、宿主直读（协议 v1.1 扩展位） |
| WebView2 在部分 ARM64 机器缺失 | 应用无法启动 | 安装包内置 Evergreen bootstrapper；启动检测缺失时提示下载 |
| 内部插件作者不熟悉协议 | 插件质量参差 | validator CLI + 中文开发指南 + SDK 模板工程，Agent 可直接按指南生成插件 |
| i18n 从第一天做拖慢 UI 进度 | C 路延期 | 文案集中 JSON、key 命名规范先行，禁止硬编码；初期仅中英两语 |

## 9. 里程碑总览

| 里程碑 | 内容 | 出口标准 |
|--------|------|----------|
| M0 | 脚手架 | 双架构空壳可构建 |
| M1 | 契约冻结 | `contract-v1` tag |
| M2 | 七路并行完成 | 各路 DoD 达成 |
| M3 | 集成达标 | E2E 全绿 + 性能报告 |
| M4 | 发布 | 4 份产物可用 |

---

*接手说明：本文件承接自规划会话导出（qwen-code-export-2026-08-07T02-14-51-556Z.md）。两轮问答锁定的全部决策已固化于 §2，Phase 0 为下一步执行起点。*
