# 三发行版交付 · 交接文档（API 体系设计 Quest）

> 2026-09-06 · 自 Qoder「API 体系设计」Quest 交接（M1/M2 已完成并验证）。
> 读者：接手 M3/M4 的 Agent。
>
> **事实正本**（与本文冲突时以其为准）：`PLAN.md` §10 变更决策记录、
> `docs/spec/http-api-v1.md`（服务器契约）、`docs/spec/protocol-v1.md`（插件协议）、
> `docs/developer-guide/ipc-ui.md` 所在的桌面契约（`AnalysisBuddy-devdocs/deep-dive/ipc-ui.md`）。
>
> **来源声明**：原 Quest 会话中的调研对话与 M3/M4 原始措辞未随仓库落盘。
> 本文 §2（三轮调研结论）与 §5（M3/M4 规格）是从已入库的决策产物
> （PLAN.md §10、http-api-v1.md、代码结构、CI 现状）**重建**的；若与原 Quest
> 需求（36 条）冲突，以原需求为准并回改本文。

---

## 1. 任务背景

用户需求：AnalysisBuddy 交付三种发行版，共用同一引擎核心与同一套插件生态：

1. **Windows EXE 桌面版**——存量用户做存量数据分析（Quest 前已交付）；
2. **Linux 服务器版**——新测试的接入与大批量分析（无 GUI，HTTP+SSE 服务）；
3. **嵌入式版本**——作为外部库被其他应用导入，协作完成数据分析与可视化。

Quest 按依赖链拆为四个里程碑：

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| M1 | 自 `core/ab-app` 提取纯 Rust 引擎核心 `core/ab-engine` | ✅ 完成（本文 §4.1） |
| M2 | 构建 `core/ab-server`（axum HTTP+SSE 服务 = Linux 服务器版） | ✅ 完成（本文 §4.2） |
| M3 | 嵌入式 API 正式化（第三形态的完整承诺） | ⬜ 待办（本文 §5.1） |
| M4 | 三形态交付工程（CI/打包/验收/文档索引） | ⬜ 待办（本文 §5.2） |

## 2. 三轮调研结论（重建）

三节按 Quest 期三轮调研的**决策产出**重建；每条结论都给出可核对的仓库落点。

### 2.1 调研一：三形态共存 → 引擎提取策略

**问题**：三种形态如何不产生三份分叉的核心逻辑？

**结论**（落点：`PLAN.md` §10、`core/ab-engine/src/lib.rs` 模块头注）：

- 把与 UI 无关的全部逻辑从 `core/ab-app` 机械提取为 `core/ab-engine`（纯 Rust
  库，依赖树无 `tauri`/`windows-sys`/`winreg`），命令层保留 `*_logic` 纯函数 +
  DTO 形态；`ab-app` 退化为 Tauri 薄包装并整体再导出引擎公共项
  （`pub use ab_engine::{events, host_bridge, ipc_errors, network, pipeline_bridge, smoke}`），
  桌面侧 `ab_app::xxx` 公共 API 面不变 → **集成测试零修改**。
- 环境敏感输入（路径等）一律由调用方注入：`ab_engine::paths::EnginePaths`
  显式结构体；桌面壳走 `desktop_engine_paths()`（Windows 公式原样），
  headless 宿主走 `EnginePaths::linux_default()`（XDG）。
- 依赖链 M1 → M2/M3：ab-server 与嵌入式消费方都是 ab-engine 的下游；
  插件协议（manifest/protocol v1）零变更即跨形态复用。

### 2.2 调研二：服务器 API 技术选型与契约设计

**问题**：Linux 服务器形态对外暴露什么协议？响应形状如何与桌面不打架？

**结论**（落点：`docs/spec/http-api-v1.md` 全文、`core/ab-server/src/lib.rs`）：

- **传输选型**：HTTP/1.1 + REST + SSE（axum 0.8，`default-features = false`
  按需启用）。不选 WebSocket（单向事件流够用，SSE 自带重连语义与文本帧调试便利）；
  不选 gRPC（内部工具生态更倾向 curl/脚本友好面）。
- **DTO 平价原则（本契约的基石）**：响应体逐字段复用 `ab_engine::commands` 的
  DTO，与桌面 `ipc-ui.md` §1.0 形状一致（含 skip-if-empty 语义）；HTTP 面新增的
  语义（异步 job、SSE、`file_ids` 缺省 = 全部冻结文件、`max_points_per_series`
  服务端上限 50000）在 http-api-v1.md 显式文档化为**服务器扩展**。为桌面前端写的
  客户端逻辑可直接复用；契约面向厂商中立，不绑定 Tauri 私有形状。
- **导入异步化**：桌面 `import_files` 同步；HTTP 侧拆为 `202 + job_id` 轮询 +
  协作式取消（文件边界停止），文件级结果形状不变（含 `needs_user_choice`
  手选分支，用 `overrides` 重提交）。
- **错误模型**：恒 `{"error": <IpcError>}` 包络；`code` 是机器可读正本，
  HTTP 状态码只是传输层附加值（`error.rs::status_for` 唯一映射表；
  `cancelled→409`、RPC 数字码→400、未知码→500 保守回落）。

### 2.3 调研三：部署、安全与嵌入式消费模式

**问题**：服务器形态在真实部署里要防什么？嵌入方怎么用引擎？

**结论**（落点：http-api-v1.md §5/§7、`10-server-mode.md`、`state.rs`/`hub.rs`）：

- **安全模型**：默认只绑 `127.0.0.1:8600`；`--token` 启用恒时比较的 Bearer
  认证（health 豁免供探活）；**插件即任意代码**——install/update 端点等价于
  授予代码执行，只应对可信操作者开放；会话路径禁闭在 `--sessions-dir`（词法
  规范化 + 前缀判定）；上传落服务端临时目录且导入后清理；请求体上限 64MB。
- **背压与事件语义**：中央 broadcast（512）+ 每订阅者有界队列（256）；掉队者
  收 `event: error`（`event_stream_lagged`）终帧后断开，**绝不静默丢帧**；
  进度节流是逐连接的 100ms/file_id（`percent>=100` 终态直发），帧名 = 引擎
  通道去 `ab://` 前缀。
- **跨平台细节**：插件必须有 Linux 入口（manifest `entry.command` 按
  protocol-v1 §7.3 解析；只带 Windows 二进制的插件在 Linux 无法启动，Python
  插件天然跨平台）；模块状态文件（禁用集）落便携源目录；`SIGINT` 优雅停机
  （先停 HTTP 监听，再关停全部插件进程并收割，无孤儿）。
- **嵌入消费模式**：Rust 宿主跳过 HTTP 直接以库方式用 ab-engine
  （`examples/engine_embed.rs` 是最小闭环：装配四件套 → 导入 → 查询 → 停机）；
  非 Rust 宿主在 v1 走 ab-server sidecar（HTTP）——C-FFI 列为 M3 的显式决策点。

## 3. 三种发行版定位

| | Windows EXE 桌面版 | Linux 服务器版 | 嵌入式引擎库 |
|---|---|---|---|
| 承载 crate | `core/ab-app`（Tauri 2 壳） | `core/ab-server`（axum bin） | `core/ab-engine`（lib） |
| 消费方 | 终端用户（存量数据分析） | 自动化/批量接入、新测试接入 | 其他应用的 Rust 宿主（v1）；非 Rust 走 sidecar HTTP |
| 界面 | WebView2 React UI | 无 GUI，HTTP+SSE（`/api/v1`） | 进程内函数调用 + 事件流 |
| 契约 | `ipc-ui.md`（IPC 命令） | `docs/spec/http-api-v1.md` | Rust API（M3 正式化，§5.1） |
| 路径公式 | `desktop_engine_paths()`（exe 同目录 plugins + `%APPDATA%`） | Windows dev 对齐桌面；Linux 走 XDG | 调用方注入 `EnginePaths` |
| 状态 | ✅ 已交付（Quest 前） | ✅ M2 完成 | 🔶 M1 引擎已就绪，M3 正式化 API |

三者共用：`ab-protocol`（契约类型）、`ab-host`（插件运行时）、`ab-pipeline`
（导入/存储/查询）、`ab-engine`（命令逻辑/事件/网络）与同一套插件目录布局。

## 4. 已完成工作

### 4.1 M1：`core/ab-engine` 引擎提取

- **迁移量**：`core/ab-app` 净减约 7,752 行（`events.rs`、`host_bridge.rs`、
  `ipc_errors.rs`、`network/`、`pipeline_bridge.rs`、`smoke.rs` 整体删除，
  `commands/*` 中的逻辑体抽出），全部落位 `core/ab-engine/src/` 同名模块。
- **引擎模块**：`commands`（19 个 `*_logic` 纯函数 + DTO）、`events`
  （`convert`/`convert_pipeline`、`ProgressThrottle`、`PluginMeta`、
  `PluginLogBuffer`、`EV_*` 通道常量）、`host_bridge`（`HostSessionAdapter`）、
  `ipc_errors`、`network`（`GitHubFetcher`/`UpdateFetcher`）、`paths`
  （`EnginePaths` + `linux_default()`）、`pipeline_bridge`
  （`ImportCoordinator` + `PipelineConfig`）、`smoke`（无头装配证明）。
- **内建 id 机制随迁**：`build.rs` 扫描仓库 `plugins/` 生成
  `gen/builtin_ids.rs` 的机制迁到 ab-engine；ab-app 的 build.rs 复制其产物供
  自家测试 `include!`，常量经 `pub use ab_engine::BUILTIN_PLUGIN_IDS` 转发。
- **验证**：桌面全部集成测试不改一行即绿（再导出保持 API 面）；工作区
  clippy 0 警告。

### 4.2 M2：`core/ab-server` 无头服务

- **端点面**：`/api/v1` 23 个端点（health / imports×5 / files / metrics /
  query×2 / plugins×8 / sessions×2 / presets×3 / events SSE），与
  http-api-v1.md §2 表一一对应；`routes.rs` 单文件承载，每个处理器恰好调用
  一个引擎 `*_logic`。
- **基础设施**：Bearer 认证中间件（恒时比较、health 豁免）、64MB 请求体上限、
  统一错误包络 + 状态码映射（`error.rs`，带快照单测）、multipart 上传落盘
  （basename 防穿越）、会话路径禁闭（词法规范化 + 前缀判定）、CLI 手写解析
  （`--addr/--token/--max-concurrent-imports/--user-data-dir/…`，无 clap 依赖）。
- **导入 Job 系统**（`jobs.rs`）：`job-N` 单调编号、`queued→running→
  completed/failed/cancelled` 状态机、Semaphore 并发闸（默认 2）、协作式取消
  （文件边界停止、失败优先于取消、终态保留已 Results）。
- **SSE 事件系统**（`hub.rs`）：中央 broadcast 512 + 每订阅者队列 256 +
  逐连接节流/过滤；掉队发 `event_stream_lagged` 终帧后断开；host/pipeline
  双源 forwarder 复用引擎 `events::convert*`，health 失败态补 `detail`
  （与桌面等价）。
- **文档**：`docs/spec/http-api-v1.md`（英文契约正本，7 章）+
  `docs/developer-guide/10-server-mode.md`（中文上手：构建/旗标/第一个
  curl 闭环/SSE/Linux 部署/排错表）+ developer-guide README 章节索引补录。

### 4.3 验收证据（2026-09-06 实测）

| 验收项 | 结果 |
|--------|------|
| `cargo test --workspace` | **386 通过 / 0 失败**（含 ab-server 6 个模块单测 + 10 个真服务器集成测试：health、导入→查询闭环、key_values 部分失败形状、SSE progress、presets CRUD、token 认证门、上传导入、插件列表、points 上限 400、会话路径越界 400） |
| `cargo clippy --workspace --all-targets` | 0 警告 |
| `cargo run -p ab-engine --example engine_embed` | 嵌入式闭环冒烟通过（装配→导入 fixture→指标树→series 查询→优雅停机） |
| rustfmt | ⚠️ 见 §6 已知问题（预存，非本次引入） |

## 5. 待办规格

### 5.1 M3：嵌入式 API 正式化（第三形态的完整承诺）

**目标**：把「能跑通示例的引擎库」升级为「有稳定承诺、有文档、有事件便利层
的嵌入 API」。当前嵌入方要自己装配四件套并自己接事件转换——这是 M3 要消除的
摩擦。

**任务**：

1. **决策点（先做）——非 Rust 宿主的消费方式**。建议 v1 承诺两条路：
   Rust 库（ab-engine）+ sidecar HTTP（ab-server）；**C-FFI 延后**。理由：
   内部工具生态（Python/C#）已有插件协议与 HTTP 两条通路；C ABI 的回调/内存
   安全与跨平台发布矩阵成本高。若用户明确要 .NET 进程内嵌入，再立 M3.5 评估
   cbindgen 方案。
2. **`ab_engine::embed` 门面模块**：`EmbedEngine::new(EnginePaths, config)` →
   `import / get_metrics / query_series / key_values_at / save_session /
   load_session / subscribe_events(…) / shutdown`。事件订阅内部整合
   forwarder + `convert*`（ab-server `hub.rs::spawn_forwarder` 是参考实现，
   下沉为引擎侧可复用后 server 改用之——顺带证明门面充分）。
3. **嵌入指南**：`docs/developer-guide/11-embedding.md`（中文）：装配→导入→
   事件订阅→查询→停机主线；`EnginePaths` 平台公式；「嵌入 vs sidecar」选型
   表；版本与 semver 承诺（DTO 与 ipc-ui.md §1.0 平价 = 契约级冻结）。
4. **示例与测试**：`engine_embed` 扩充事件订阅示例；embed 门面集成测试
   （mock 插件走全流程）。
5. **发布策略**：GPL-3.0 下 crates.io 可行；若内网优先，先以 path/vendor +
   tag（`engine-v0.1`）交付，发布渠道由用户拍板。

**DoD**：ab-server 与 engine_embed 示例均改走 embed 门面且全测试绿；
11-embedding.md 入库；事件订阅有集成测试覆盖。

### 5.2 M4：三形态交付工程

**目标**：让服务器版与嵌入库达到与桌面版同等的「可构建、可发布、可验收」。

**任务**：

1. **CI Linux job**：`ci.yml` 目前全部 `windows-latest`。新增 `ubuntu-latest`
   job 跑无 GUI 子集（`cargo test -p ab-protocol -p ab-host -p ab-pipeline
   -p ab-engine -p ab-server -p mock-plugin` 及 `tests/e2e`）。注意：
   `cargo test --workspace` 在 Linux 会拉起 ab-app 的 tauri 系统依赖
   （webkit2gtk），故按 crate 列举而非全 workspace；若想全量，需给 ab-app
   的 tauri 依赖加 feature 门（另行评审）。
2. **release 产物**：`release.yml` 增加 `build-server` job（ubuntu，
   `cargo build --release -p ab-server`），产出
   `analysisbuddy-server-<version>-x86_64.tar.gz`（ab-server 二进制 +
   `plugins/` 布局说明 + systemd unit 样例）；Windows 侧可选随桌面包附带
   `ab-server.exe`。
3. **server-mode e2e**：`tests/e2e` 增补 HTTP 面（POST /imports → SSE 收帧 →
   query/series → 优雅停机），或在 e2e-suite.yml 直接引用 ab-server 集成测试。
4. **文档索引**：主 README 增补「三种发行版」一节（桌面/服务器/嵌入的构建与
   入口链接）；`docs/architecture.md` 缺失，补三形态架构图（或明确并入
   PLAN.md §3）。
5. **部署验收**：干净 Linux 容器按 `10-server-mode.md` §Linux 部署要点做
   systemd 冒烟；`docs/release-acceptance.md` 增补服务器产物验收清单。
6. **rustfmt 基线修复（预存问题，见 §6）**：统一 rustfmt 版本或全仓重排，
   恢复 `lint.yml` 的 `cargo fmt --check` 绿。

**DoD**：Linux CI 绿；release 产物含服务器包并通过干净环境部署冒烟；
README/架构文档三形态齐备。

## 6. 已知问题与工作规约

- **rustfmt 基线（预存）**：本地 rustfmt 1.9（rustc 1.97.1）对多处**已提交**
  文件（`ab-protocol/serde_tests.rs`、`ab-host/manifest.rs`、ab-app 测试等）
  的 `--check` 报折行建议（含 70 字符的中文字符串行），与仓库既有格式化版本
  行为不一致（疑似 CJK 宽度计算差异）。本次改动**未**引入新的违规且**未**做
  全仓重排（避免范围外噪音）；lint.yml 在 main 上可能因此红，M4 任务 6 处理。
- **未提交状态**：M1/M2 全部成果在本交接时落为三个 commit（engine / server /
  docs），见 git log。
- **`.qoder/`**：Qoder 工具的本地知识库缓存（repowiki 自动生成），不入库，
  已加入 `.gitignore`。
- **规约延续**：契约变更仍走主代理评审（PLAN.md §6）；commit message 用
  `feat/fix/docs/test/ci(scope): 描述`；新依赖须过 GPL-3.0 兼容检查
  （本次新增：axum 0.8 / futures-core 0.3，均 MIT/Apache 双许可 ✅）。

## 7. 关键文件索引

| 文件 | 内容 |
|------|------|
| `core/ab-engine/src/lib.rs` | 引擎模块清单与提取说明 |
| `core/ab-engine/examples/engine_embed.rs` | 嵌入式最小闭环（M3 起点样例） |
| `core/ab-server/src/{routes,state,hub,jobs,error,args}.rs` | M2 全部实现（模块头注含设计取舍） |
| `core/ab-server/tests/server_test.rs` | 10 个真服务器集成测试 |
| `docs/spec/http-api-v1.md` | 服务器契约正本（英文） |
| `docs/developer-guide/10-server-mode.md` | 服务器模式中文上手 |
| `PLAN.md` §10 | 三形态架构决策记录 |
| `core/ab-app/src/lib.rs` | 桌面壳薄包装 + 再导出（M1 接缝） |
