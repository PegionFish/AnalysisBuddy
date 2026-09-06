# 三发行版交付 · 交接文档（API 体系设计 Quest）

> 2026-09-06 · 自 Qoder「API 体系设计」Quest 交接（M1/M2 已完成并验证）。
> 读者：接手 M3/M4 的 Agent。
>
> **事实正本**（与本文冲突时以其为准）：
> 1. `api-system-design-quest-spec-2026-09-06.md`——Quest 原始设计规格
>    （调研结论、关键决策、API 面、M1–M4 里程碑、测试计划、风险、否决方案）；
> 2. `PLAN.md` §10 变更决策记录；
> 3. `docs/spec/http-api-v1.md`（服务器契约，**实现后契约以此为准**）；
> 4. `docs/spec/protocol-v1.md`（插件协议）。

---

## 1. 任务背景

用户需求：AnalysisBuddy 交付三种发行版，共用同一引擎核心与同一套插件生态：

1. **Windows EXE 桌面版**——存量用户做存量数据分析（Quest 前已交付）；
2. **Linux 服务器版**——新测试的接入与大批量分析（无 GUI，HTTP+SSE 服务）；
3. **嵌入式版本**——作为外部库被其他应用导入，协作完成数据分析与可视化。

Quest 按依赖链拆为四个里程碑（定义见原始规格「实施里程碑」）：

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| M1 | 提取 `core/ab-engine`（纯机械搬移，逻辑零改动） | ✅ 完成（本文 §4.1） |
| M2 | 新建 `core/ab-server`（axum HTTP+SSE） | ✅ 完成（本文 §4.2） |
| M3 | **供应商中立扩展**（presets-as-vendor-view + CCP custom_query + CapabilitiesDto 真实化） | ⬜ 待办（本文 §5.1） |
| M4 | **CI 与加固**（Linux CI job + 内存预算硬顶 + Arrow 内容协商可选） | ⬜ 待办（本文 §5.2） |

## 2. 调研结论（Quest 原始 Summary 摘录）

现有代码分层质量已被**三路独立调研交叉确认**：ab-host/ab-pipeline/
ab-protocol 零 Tauri 依赖且 Windows 专有代码全部有 `#[cfg]` 门控（CI 已在
ubuntu 上实证编译 ab-protocol），ab-app 的 Tauri 耦合全部是薄包装——19 个
命令各有纯逻辑 `*_logic` 自由函数（入参普通引用、出参 serde DTO），且
`smoke.rs` 已证明无 Tauri 的全链路装配可行。因此方案：**提取**
`core/ab-engine`（进程内嵌入）+ **新增** `core/ab-server`（HTTP+SSE），
HTTP API 逐条镜像桌面 IPC 契约（同一批 DTO）；供应商自定义读取能力走
「Phase 1 零协议变更 + Phase 2 CCP custom_query」两阶段。

六条关键设计决策（全文见原始规格）：

1. **提取而非复制或 feature-gating**——ab-server 绝不依赖 ab-app；
   ~25 处 tauri 触点 cfg 化的长期成本高于一次机械提取。
2. **HTTP API 镜像桌面契约**——复用同一批 serde DTO；错误恒
   `{"error": {code, message, data?}}`，code 与 ipc-ui.md §1.10 逐字一致。
3. **v1 单引擎单租户**——每进程一个引擎实例；信任边界默认 127.0.0.1 +
   可选 Bearer；多会话隔离推迟（`PipelineConfig.file_id_fn` 已预留挂点）。
4. **长任务用 Job API**——POST /imports 返回 202 + job_id，不阻塞 HTTP。
5. **事件用 SSE 而非 WebSocket**——事件均为单向推送；复用
   `events::convert*` + `ProgressThrottle`，每客户端有界队列防慢客户端。
6. **供应商中立**——宿主/服务器零硬编码具体工具；厂商逻辑由插件声明、
   统一路由暴露，宿主对载荷 opaque。

## 3. 三种发行版定位

| | Windows EXE 桌面版 | Linux 服务器版 | 嵌入式引擎库 |
|---|---|---|---|
| 承载 crate | `core/ab-app`（Tauri 2 壳） | `core/ab-server`（axum bin） | `core/ab-engine`（lib） |
| 消费方 | 终端用户（存量数据分析） | 自动化/批量接入、新测试接入 | 其他应用的 Rust 宿主（v1）；非 Rust 走 sidecar HTTP |
| 界面 | WebView2 React UI | 无 GUI，HTTP+SSE（`/api/v1`） | 进程内函数调用 + 事件流 |
| 契约 | `ipc-ui.md`（IPC 命令） | `docs/spec/http-api-v1.md` | Rust API（engine_embed 示例即公开用法） |
| 路径公式 | `desktop_engine_paths()`（exe 同目录 plugins + `%APPDATA%`） | Windows dev 对齐桌面；Linux 走 XDG | 调用方注入 `EnginePaths` |
| 状态 | ✅ 已交付（Quest 前） | ✅ M2 完成 | ✅ M1 完成（`examples/engine_embed.rs`） |

三者共用：`ab-protocol`（契约类型）、`ab-host`（插件运行时）、`ab-pipeline`
（导入/存储/查询）、`ab-engine`（命令逻辑/事件/网络）与同一套插件目录布局。

## 4. 已完成工作

### 4.1 M1：`core/ab-engine` 引擎提取

- **迁移量**：`core/ab-app` 净减约 7,752 行（`events.rs`、`host_bridge.rs`、
  `ipc_errors.rs`、`network/`、`pipeline_bridge.rs`、`smoke.rs` 整体删除，
  `commands/*` 中的逻辑体抽出），全部落位 `core/ab-engine/src/` 同名模块；
  git rename 追踪完整（commit `64aec4d`）。
- **引擎模块**：`commands`（19 个 `*_logic` 纯函数 + DTO）、`events`
  （`convert`/`convert_pipeline`、`ProgressThrottle`、`PluginMeta`、
  `PluginLogBuffer`、`EV_*` 通道常量）、`host_bridge`（`HostSessionAdapter`）、
  `ipc_errors`、`network`（`GitHubFetcher`/`UpdateFetcher`）、`paths`
  （`EnginePaths` + `linux_default()` XDG）、`pipeline_bridge`
  （`ImportCoordinator` + `PipelineConfig`）、`smoke`（无头装配证明）。
- **兼容护栏**：ab-app `lib.rs` 顶部整体再导出引擎公共项，桌面
  `ab_app::xxx` API 面不变 → **16 个 ab-app 集成测试文件零修改全绿**
  （原始规格 M1 验收条件，达成）。
- **内建 id 机制随迁**：`build.rs` 扫描仓库 `plugins/` 生成
  `gen/builtin_ids.rs` 归入 ab-engine；ab-app build.rs 复制产物供自家测试
  `include!`，常量经 `pub use ab_engine::BUILTIN_PLUGIN_IDS` 转发
  （原始规格风险表第 2 行的漂移对策）。

### 4.2 M2：`core/ab-server` 无头服务

- **端点面**：`/api/v1` 23 个端点（health / imports×5 / files / metrics /
  query×2 / plugins×8 / sessions×2 / presets×3 / events SSE），覆盖原始
  规格核心表全部行；每个处理器恰好调用一个引擎 `*_logic`。
- **基础设施**：Bearer 认证中间件（恒时比较、health 豁免）、64MB 请求体上限
  （独立于插件 8MB 行限，规格 M2.5）、统一错误包络 + 状态码映射
  （`error.rs::status_for`，规格 6 行基础上的超集表）、multipart 上传落盘
  （basename 防穿越）、**会话路径禁闭**（词法规范化 + 前缀判定，规格
  「服务端会话目录为路径根」的加码）、CLI 手写解析（无 clap 依赖）。
- **导入 Job 系统**（`jobs.rs`）：`job-N` 单调编号、`queued→running→
  completed/failed/cancelled` 状态机、Semaphore 并发闸（默认 2，规格 M2.4）、
  协作式取消（文件边界停止、失败优先于取消）。
- **SSE 事件系统**（`hub.rs`）：中央 broadcast 512 + 每订阅者队列 256 +
  逐连接节流/过滤；掉队发 `event_stream_lagged` 终帧后断开（防慢客户端，
  规格 M2.3）；host/pipeline 双源 forwarder 复用引擎 `events::convert*`。
- **文档**：`docs/spec/http-api-v1.md`（英文契约正本，7 章，含 Versioning：
  v1 只许增量、破坏性变更走 /api/v2）+ `docs/developer-guide/
  10-server-mode.md`（curl 闭环 + Linux 部署 + 插件 Linux entry 说明）+
  PLAN.md §10 决策行 + developer-guide README 索引。
- **嵌入示例**：`core/ab-engine/examples/engine_embed.rs`（规格 M2.7，
  已跑通：装配→导入→指标树→查询→停机）。

### 4.3 验收证据（2026-09-06 实测）

| 验收项 | 结果 |
|--------|------|
| `cargo test --workspace` | **386 通过 / 0 失败**（含 ab-server 6 个模块单测 + 10 个真服务器集成测试：health、导入→查询闭环、key_values 部分失败形状、SSE progress、presets CRUD、token 认证门、上传导入、插件列表、points 上限 400、会话路径越界 400） |
| `cargo clippy --workspace --all-targets` | 0 警告（M1 验收条件，达成） |
| `cargo run -p ab-engine --example engine_embed` | 嵌入式闭环冒烟通过 |
| rustfmt | ⚠️ 见 §6 已知问题（预存，非本次引入） |

## 5. 待办规格

### 5.1 M3：供应商中立扩展（依赖 M1/M2）

> 完整定义见原始规格「供应商中立扩展（分阶段）」；以下为落地清单。

**M3.0 Phase 1 收尾核对（零协议变更，改动仅文档）**：

- 核对 `02-write-a-plugin.md` / `04-manifest-reference.md` 是否已把
  presets-as-vendor-view（preset want 别名键 → metric_id → 标准 series
  查询）与 key_values fast-read（semantics plugin-defined，协议 §2.6）两种
  厂商模式写成指引。preset 机制本体已随 MAN-14 工作落地
  （protocol-v1.md 19 处、04-manifest 9 处提及 preset），缺的只是
  「厂商具名视图」视角的章节。

**M3.1 Phase 2：CCP custom_query（完全复刻 annotate 先例，按 CCP 模板四处
同批）**：

1. **提案成文**：用 `docs/developer-guide/contract-change-proposal-template.md`
   立项。协议内容：可选方法 custom_query，入参
   `{file_id, query, params(object)}`、result `{data(object)}`（宿主对载荷
   opaque）；未实现 → -32005；未知 query 名 → -32602；parse 占用中 →
   -32001；§6 超时表加 10s 行；Capabilities 增可选位 custom_query（serde
   default + skip-if-empty，旧插件缺省 false，PROTOCOL_VERSION 保持 1）。
2. **四处同批触点**：`protocol-v1.md` 新增 §2.11 + `rpc-messages.schema.json`
   oneOf 追加（错误码 enum 不动）+ `ab-protocol/src/types.rs` 新类型 +
   `plugin-validator` 追加 BEH-13（无能力回 -32005、有能力回合法对象）。
3. **宿主链路**：ab-host `PluginSession::custom_query`（session.rs annotate
   分支模式）+ health.rs 超时表；ab-pipeline trait 加方法纯透传；
   ab-engine `HostSessionAdapter` 实现 + 新命令
   `custom_query_at(file_id, query, params)`（镜像 key_values 扇出：逐项
   error、永不 reject）。
4. **SDK×2**：Python `KNOWN_METHODS` + `_handle_custom_query`（先探测实现再
   -32005）+ 默认 `on_custom_query` 抛 UnsupportedInV1Error；dotnet
   RouteAsync 加 case + PluginHandlerBase 默认 -32005 +
   SupportsCustomQuery 反射探测。
5. **服务器路由**（中立性：调用方只知 file_id，plugin_id 服务端解析；
   name/params opaque）：
   - `POST /api/v1/files/{fid}/queries/{name}`（body {params?}）→ 200
     {data} / 409 plugin_busy / 504 timeout / **422 unsupported**（-32005 与
     旧 SDK -32601 归一，不得落 internal）/ 422 invalid_params；
   - `GET /api/v1/files/{fid}/vendor-queries`（具名查询清单）。
6. **配套修正**：`CapabilitiesDto` 目前恒报 `annotate:false`
   （`core/ab-engine/src/commands/mod.rs:299` 附近）——首次 Ready 后缓存真实
   capabilities，使能力发现端点有真实数据。
7. **mock-plugin/e2e**：validate_result 加分支 + harness 加 custom_query()
   驱动方法 + mock/real 套件用例。

**M3.2 Phase 3（可选，仅当 Phase 2 有真实需求后）**：可选方法 list_queries
返回 {name, description, params schema} 清单（不塞 initialize）。

**测试**（规格）：validator BEH-13；SDK 单测（pytest / xunit）覆盖 -32005
默认与探测声明；e2e custom_query 用例（mock-plugin 剧本）。

### 5.2 M4：CI 与加固（CI 部分可立即先行）

**M4.1 Linux CI job**（规格原文）：`.github/workflows/ci.yml` 增
build-linux job（ubuntu-latest）：`cargo test -p ab-protocol -p ab-host -p
ab-pipeline -p ab-engine -p ab-server -p mock-plugin` + builtin-csv 经
`--manifest-path` + tests/e2e 的 mock/real 套件（**排除 ab-app 与 fps
probe**——tauri 在 Linux 需 webkit2gtk 系统库）；既有 Windows job 一律不动。

**M4.2 加固（可作后续迭代）**：store.rs FileData 内存记账 + 引擎内存预算
硬顶（超限 job 终态 `reason=memory_budget_exceeded`）；查询响应二进制内容
协商（`Accept: application/x-ab-arrow`，Arrow IPC 列式——LTTB 已封顶响应
尺寸，属可选项）。

**交接建议补充（非原规格，接手 Agent 与用户确认后做）**：

- release.yml 增服务器产物（Linux tar.gz：ab-server + plugins 布局说明 +
  systemd unit 样例）；`docs/release-acceptance.md` 增补服务器验收清单；
  主 README 补「三种发行版」一节。
- 嵌入门面（`ab_engine::embed`：把装配四件套 + 事件 forwarder 收敛为单一
  入口类型，ab-server `hub.rs::spawn_forwarder` 下沉引擎侧复用）——原规格
  以 `engine_embed.rs` 为嵌入公开用法即可；若嵌入消费方多起来再立项。

### 5.3 实现与原规格的偏差记录

以下偏差均为实现期的**有意修正/细化**，契约以 `http-api-v1.md`（实现后正
本）为准；接手 Agent 读原始规格时以此节对照：

| # | 原规格表述 | 实现状态 | 说明 |
|---|-----------|---------|------|
| 1 | SSE 帧 `{channel, payload}` 信封，channel 名与 `ab://` 一致 | 短帧名（去 `ab://` 前缀，如 `progress`）+ **裸内层载荷**（无信封） | 调试更直观；`http-api-v1.md` §5 为正本 |
| 2 | job 状态查询复用 `job_diagnostics` | 服务器自建 `JobRegistry`（jobs.rs） | 行为符合契约（202/job_id/状态机/取消）；桌面的 job_diagnostics 面向 IPC 诊断，不复用更干净 |
| 3 | CLI `--plugins-dir` | 拆为 `--plugins-portable/--plugins-install/--plugins-user`（+ `--max-concurrent-imports`） | 三源发现需要三个目录；单旗标无法表达 |
| 4 | query/series「默认沿用 LTTB 50k/序列预算」 | 默认 **4000**（桌面默认一致）+ 服务端硬顶 **50000** | 4000 是桌面实际默认入参；50k 是上限而非默认 |
| 5 | 错误映射 6 行基础表 | 超集表（cancelled→409、module_*/preset_conflict→409、RPC 数字码→400、未知码→500） | `error.rs::status_for` 唯一实现 + 快照单测 |
| 6 | 「服务端会话目录为路径根」 | 加码为词法规范化 + 前缀判定的路径禁闭（越界 400） | 防路径穿越 |
| 7 | 请求体上限「独立于插件 8MB 行限」 | 64MB（`MAX_BODY_BYTES`） | 上传 CSV/插件 ZIP 需要 headroom |

## 6. 已知问题与工作规约

- **rustfmt 基线（预存）**：本地 rustfmt 1.9（rustc 1.97.1）对多处**已提交**
  文件（`ab-protocol/serde_tests.rs`、`ab-host/manifest.rs`、ab-app 测试等）
  的 `--check` 报折行建议（含 70 字符的中文字符串行），与仓库既有格式化版本
  行为不一致（疑似 CJK 宽度计算差异）。M1/M2 改动**未**引入新的违规且**未**
  做全仓重排（避免范围外噪音）；lint.yml 在 main 上可能因此红，建议接手后
  统一 rustfmt 版本或全仓重排一次（可并入 M4）。
- **提交状态**：M1/M2 全部成果已落为 commit（`64aec4d` engine /
  `ffaa208` server / `2136664`+后续 handoff 文档），工作树干净。
- **`.qoder/`**：Qoder 工具本地缓存（repowiki 自动生成 + 空 specs 目录），
  已加入 `.gitignore` 不入库。
- **规约延续**：契约变更仍走主代理评审 + CCP 四处同批（PLAN.md §6）；
  commit message 用 `feat/fix/docs/test/ci(scope): 描述`；新依赖须过
  GPL-3.0 兼容检查（本次新增：axum 0.8 / futures-core 0.3，均 MIT/Apache ✅）。

## 7. 关键文件索引

| 文件 | 内容 |
|------|------|
| `docs/api-system-design-quest-spec-2026-09-06.md` | **Quest 原始规格正本**（调研/决策/API 面/M1–M4/风险/否决方案） |
| `core/ab-engine/src/lib.rs` | 引擎模块清单与提取说明 |
| `core/ab-engine/examples/engine_embed.rs` | 嵌入式最小闭环（嵌入形态公开用法） |
| `core/ab-server/src/{routes,state,hub,jobs,error,args}.rs` | M2 全部实现（模块头注含设计取舍） |
| `core/ab-server/tests/server_test.rs` | 10 个真服务器集成测试 |
| `docs/spec/http-api-v1.md` | 服务器契约正本（英文，实现后以此为准） |
| `docs/developer-guide/10-server-mode.md` | 服务器模式中文上手 |
| `PLAN.md` §10 | 三形态架构决策记录 |
| `core/ab-app/src/lib.rs` | 桌面壳薄包装 + 再导出（M1 接缝） |
| `core/ab-engine/src/commands/mod.rs`（~L166-299） | CapabilitiesDto 现状（M3.1-6 修正点） |
