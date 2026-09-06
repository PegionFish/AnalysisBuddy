# AnalysisBuddy 服务化与引擎化 API 体系设计（Quest 原始规格）

> 2026-09-06 入库。本文是 Qoder「API 体系设计」Quest 的设计产出正本，由用户
> 自 Quest 会话导出转录；交接文档 `three-release-forms-handoff-2026-09-06.md`
> 的 M3/M4 待办规格以此为据。实现状态：M1/M2 已完成（见交接文档 §4），
> M3/M4 待办。实现与本文的偏差记录见交接文档 §5.3。

## Summary

现有代码分层质量已被三路独立调研交叉确认：ab-host/ab-pipeline/ab-protocol 零
Tauri 依赖且 Windows 专有代码全部有 `#[cfg]` 门控（CI 已在 ubuntu 上实证编译
ab-protocol），ab-app 的 Tauri 耦合全部是薄包装——19 个命令各有纯逻辑
`*_logic` 自由函数（入参为普通引用、出参为 serde DTO），且
`core/ab-app/src/smoke.rs` 已证明无 Tauri 的全链路装配可行。因此方案是：提取
`core/ab-engine`（纯 Rust 引擎库，支撑进程内嵌入）+ 新增 `core/ab-server`
（axum HTTP+SSE，支撑 Linux Web 服务），HTTP API 逐条镜像桌面 IPC 契约
（同一批 DTO 类型），供应商自定义读取能力通过「Phase 1 零协议变更 + Phase 2
CCP 提案 custom_query」两阶段实现。

## 关键设计决策

1. **提取而非复制或 feature-gating**：ab-server 绝不依赖 ab-app（会拖入
   tauri → Linux 需 webkit2gtk）；不做 ab-app 内 feature 门控（约 25 处
   tauri 触点 cfg 化的长期成本高于一次机械提取）。
2. **HTTP API 镜像桌面契约**：19 个命令 → REST 端点，请求/响应复用
   commands/mod.rs 同一批 serde DTO；错误响应体恒
   `{"error": {code, message, data?}}`，code 与 ipc-ui.md §1.10 逐字一致。
3. **v1 单引擎单租户**：每个 server 进程一个引擎实例（桌面语义等价），信任
   边界默认 127.0.0.1 + 可选 Bearer token。多会话隔离列为后续演进（见
   Rejected Alternatives）。
4. **长任务用 Job API**：导入不阻塞 HTTP 请求，POST /imports 返回 202 +
   job_id，复用既有 job_diagnostics/cancel_parse。
5. **事件用 SSE 而非 WebSocket**：所有事件（progress/plugin-log/
   plugin-health/plugins-reloaded）均为单向推送，SSE 更简单、代理友好；复用
   既有 events::convert/convert_pipeline + ProgressThrottle 节流管线，每
   客户端有界队列防慢客户端。
6. **供应商中立**：宿主/服务器零硬编码任何具体工具；厂商专有逻辑由插件声明、
   经统一路由暴露，宿主对载荷 opaque。

## API 面（v1，全部 /api/v1 前缀）

### 核心（镜像 19 命令）

| 端点 | 映射 |
|------|------|
| `POST /api/v1/imports`（body: {paths, overrides?}，服务端本地路径，面向嵌入方/CI） | import_files_logic |
| `POST /api/v1/imports/upload`（multipart，面向 Web UI） | 同上，先落临时文件 |
| `GET /api/v1/imports/{job_id}` / `DELETE /api/v1/imports/{job_id}` | job 状态 / cancel_parse_logic |
| `DELETE /api/v1/files/{file_id}` | unload_file_logic |
| `GET /api/v1/metrics?file_ids=` | get_metrics_logic |
| `POST /api/v1/query/series`（{file_ids, metrics, t0_ms, t1_ms, max_points_per_series}） | query_series_logic（服务端强制 max_points 上限，默认沿用 LTTB 50k/序列预算） |
| `POST /api/v1/query/key-values`（{file_ids, timestamp_ms}） | key_values_at_logic（部分失败协议：永不整体 reject） |
| `GET /api/v1/plugins` / `GET /api/v1/plugins/{id}/log?limit=` / `POST /api/v1/plugins/{id}/reload` | plugin 命令 |
| `POST /api/v1/plugins/install`（multipart zip）/ `DELETE /api/v1/plugins/{id}` / `PUT /api/v1/plugins/{id}/enabled` / `GET·POST /api/v1/plugins/{id}/update` | plugin_manager 命令 |
| `POST /api/v1/sessions/save` / `POST /api/v1/sessions/load` | session 命令（服务端会话目录为路径根） |
| `GET·POST·DELETE /api/v1/presets[...]` | presets 命令（用户自定义"看哪些值"） |
| `GET /api/v1/health` | 存活 + protocol_version: 1（类比 initialize 握手） |
| `GET /api/v1/events`（SSE，?file_id=/?plugin_id= 过滤） | 四通道事件帧{channel, payload}，channel 名与 ab:// 一致 |

### 供应商中立扩展（分阶段）

**Phase 1（零协议变更）**：`GET /api/v1/plugins` 已透出 manifest presets——
厂商具名视图经 preset want 别名键（如 `cpu_thermal`）解析为 metric_id 后走
标准 series 查询；"游标处状态快照"类快速读取走 key_values（厂商自定义 key
语义，协议 §2.6 明确 semantics plugin-defined）。改动仅文档。

**Phase 2（CCP 提案：可选方法 custom_query，完全复刻 annotate 先例）**：

- 协议：custom_query 入参 `{file_id, query, params(object)}`，result
  `{data(object)}`（宿主不解释 opaque 载荷）；未实现 → -32005；未知 query
  名 → -32602；parse 占用中 → -32001；§6 超时表加 10s 行；Capabilities 增
  可选位 custom_query（serde default + skip-if-empty，旧插件缺省 false，
  PROTOCOL_VERSION 保持 1，可加性兼容）。
- 触点（CCP 模板要求四处同批）：`docs/spec/protocol-v1.md` 新增 §2.11 +
  `docs/spec/rpc-messages.schema.json` oneOf 追加（错误码 enum 不动）+
  `core/ab-protocol/src/types.rs` 新类型 + `tools/plugin-validator` 追加
  BEH-13（无能力回 -32005、有能力回合法对象）。
- ab-host：`PluginSession::custom_query`（session.rs annotate 分支模式）+
  health.rs 超时表追加。ab-pipeline：trait 加方法纯透传，不触 store。
  ab-engine：HostSessionAdapter 实现 + 新命令
  `custom_query_at(file_id, query, params)` 镜像 query_key_values 扇出模式
  （逐项 error、永不 reject）。
- SDK 双实现复刻 annotate 链路：Python KNOWN_METHODS 加 custom_query +
  `_handle_custom_query`（先探测实现再 -32005）+ 默认 on_custom_query 抛
  UnsupportedInV1Error；dotnet RouteAsync 加 case + PluginHandlerBase 默认
  -32005 + SupportsCustomQuery 反射探测。
- mock-plugin/e2e：validate_result 加分支 + harness 加 custom_query() 驱动
  方法 + mock/real 套件用例。
- 服务器路由（中立性：调用方只知 file_id，plugin_id 由 FileIndex 解析；
  name/params 对宿主 opaque）：
  - `POST /api/v1/files/{fid}/queries/{name}`（body {params?: object}）→
    200 {data} / 409 plugin_busy / 504 timeout / 422 unsupported（-32005 与
    旧 SDK 的 -32601 归一，不得落 internal）/ 422 invalid_params
  - `GET /api/v1/files/{fid}/vendor-queries`（具名查询清单）
- 配套修正：CapabilitiesDto 目前恒报 annotate:false
  （commands/mod.rs:165-173）——在首次 Ready 后缓存真实 capabilities，使能力
  发现端点有真实数据。

**Phase 3（可选，仅当 Phase 2 有真实需求后）**：独立可选方法 list_queries
返回 {name, description(LocalizedName), params schema} 清单（不塞
initialize，查询集可能随 load_file 变化）。

## 实施里程碑（依赖顺序）

### M1 — 提取 core/ab-engine（纯机械搬移，逻辑零改动）✅ 已完成

1. 新建 core/ab-engine/（lib crate：依赖
   ab-protocol/ab-host/ab-pipeline/async-trait/reqwest/semver/serde/
   serde_json/tokio/zip；无 tauri、无 windows-sys），加入根 Cargo.toml
   members。
2. 搬迁（`crate::` → `ab_engine::`，同 commit 内禁改逻辑）：
   pipeline_bridge.rs、host_bridge.rs、events.rs、ipc_errors.rs、
   network.rs、smoke.rs、commands/*.rs 的全部 _logic 函数与 DTO
   （`#[tauri::command]` wrapper 留在 ab-app）、plugin_manager.rs 的
   UpdateFetcher/状态管理、build.rs 的 builtin_ids 扫描（路径改锚定工作区
   根）。
3. core/ab-app/src/lib.rs 顶部
   `pub use ab_engine::{commands, events, host_bridge, ipc_errors,
   pipeline_bridge, network, smoke};` 保兼容——现有 16 个 ab-app 集成测试
   文件零修改继续编译运行，作为机械性回归护栏。
4. 路径可配置化（Linux 就绪）：ab-engine 新增 EnginePaths
   { plugins_portable, plugins_install, plugins_user, presets_dir,
   sessions_dir }，贯穿 PluginRegistry::with_sources()
   （core/ab-host/src/discovery.rs:120 现成接缝）；PluginRegistry::new() 与
   presets_dir() 的 APPDATA 硬编码上移至 ab-app 调用方，ab-engine 不读环境
   变量。Linux 缺省 XDG `~/.local/share/AnalysisBuddy`。
5. 验收：Windows 全量 cargo test --workspace 全绿 + clippy 零告警。

### M2 — 新建 core/ab-server（axum HTTP+SSE）✅ 已完成

1. bin crate：axum（workspace.dependencies 统一锁定 0.8.x，
   default-features=false）+ tokio + ab-engine。CLI：--addr（默认
   127.0.0.1:8600）、--plugins-dir、--user-data-dir、--token（可选 Bearer）。
2. src/state.rs：Engine 装配照抄 smoke.rs 模式 + EnginePaths；src/routes/
   按上表逐端点实现；src/error.rs 固定映射表（invalid_arg→400、
   file_not_found→404、plugin_busy→409、timeout→504、plugin_crashed→502、
   internal→500）。
3. SSE hub：每客户端有界队列（滞留即断开），复用 ProgressThrottle 每客户端
   实例；载荷 JSON 与桌面 ab://* 契约逐字一致。
4. Job API：POST /imports 立即返回 {job_id} + 202；job 状态查询复用
   job_diagnostics；导入并发用 tokio::sync::Semaphore（默认 2）限流。
5. 安全基线：默认 localhost 绑定；文档写明威胁模型（插件进程 = 任意代码
   执行）；请求体上限独立于插件 8MB 行限。
6. 文档：docs/spec/http-api-v1.md（仿 protocol-v1.md 编号式：
   Transport/Endpoints/DTO 表/错误表/事件帧/Versioning——v1 内只允许增量，
   破坏性变更走 /api/v2）；docs/developer-guide/10-server-mode.md（curl
   示例 + Linux 部署 + 插件需提供 Linux entry 的说明）；PLAN.md 增补决策行
   （PLAN.md 是唯一事实源）。
7. core/ab-engine/examples/engine_embed.rs：最小嵌入示例（构造 Engine → 调
   ImportCoordinator），即"作为引擎嵌入"的公开用法。

### M3 — 供应商中立扩展（依赖 M1/M2）⬜ 待办

1. Phase 1 文档先行（02-write-a-plugin.md / 04-manifest-reference.md 增补
   presets-as-vendor-view 与 key_values fast-read 模式说明）。
2. Phase 2 按 CCP 模板成文并四处同批落地（协议/schema/ab-protocol 类型/
   validator/SDK×2/ab-host/ab-engine/server 路由/mock-plugin/e2e）。
3. CapabilitiesDto 真实化 + unsupported 错误映射修正（ipc_errors.rs）。

### M4 — CI 与加固（M1 后可先行 CI 部分）⬜ 待办

1. `.github/workflows/ci.yml` 增 build-linux job：`cargo test -p
   ab-protocol -p ab-host -p ab-pipeline -p ab-engine -p ab-server -p
   mock-plugin` + builtin-csv 经 --manifest-path + tests/e2e 的 mock/real
   套件（排除 ab-app 与 fps probe）；既有 Windows job 一律不动。
2. 加固（可作为后续迭代）：store.rs FileData 内存记账 + 引擎内存预算硬顶
   （超限 job 终态 reason=memory_budget_exceeded）；查询响应二进制内容协商
   （Accept: application/x-ab-arrow，Arrow IPC 列式）。

## 测试计划

- **M1 回归**：ab-app 现有 16 个测试文件零修改全绿（机械搬移护栏）。
- **M2**：ab-server 集成测试仿 core/ab-app/tests/pipeline_bridge_test.rs
  （mock-plugin + 临时插件目录 + 临时端口）；HTTP 响应 DTO 与桌面契约逐字段
  JSON 快照对拍；e2e：HTTP 导入 fixture → SSE 收 progress → query_series
  断点数。
- **M3**：validator BEH-13；SDK 单测（Python pytest / dotnet xunit）覆盖
  -32005 默认与探测声明；e2e custom_query 用例（mock-plugin 剧本）。
- **M4**：Linux CI job 实际激活验证；SSE 背压与并发导入限流测试。

## 风险与缓解

| 风险 | 缓解 |
|------|------|
| 搬迁 ~2000 行回归桌面壳 | 纯机械 commit + re-export 别名 + 16 个既有测试全绿后才合入；逻辑改动另起 commit |
| BUILTIN_PLUGIN_IDS 双消费方（ab-app/ab-engine）漂移 | 扫描逻辑归入 ab-engine，CI 双平台编译期断言（builtin_ids_wired 测试） |
| 旧 SDK 插件对 custom_query 回 -32601 | host 归一映射为 unsupported（422），不落 internal |
| opaque 参数安全（NDA 场景） | 宿主零解释、不做插值；server 请求体上限；10s 超时 + 看门狗兜底；per-file 查询串行（镜像 key_values 扇出） |
| GB 级文件内存 × 多客户端 | v1 单租户 + 导入信号量限流；内存硬顶列 M4 |
| Linux 插件产物缺失（现有 .exe entry） | 协议不变；部署文档要求插件按平台提供 entry；数据目录经 with_sources 显式注入规避 APPDATA |

## Rejected Alternatives

- **ab-server 依赖 ab-app**：传递引入 tauri/windows-sys，Linux 编译失败——否决。
- **ab-app feature-gating**：约 25 处触点 cfg 噪声 + tauri-build/capabilities/
  WebView2 与发布工程绑定，长期每个新命令双路接线——否决。
- **服务器复制核心逻辑**：ImportCoordinator 60KB 逻辑双份漂移——否决。
- **多会话引擎隔离（每 session 独立 Store/Coordinator）**：隔离崩溃域但显著
  增加 v1 复杂度且用户未要求多租户——推迟；PipelineConfig.file_id_fn
  （pipeline_bridge.rs:1237-1243）已预留会话命名空间挂点，作为 P2 演进。
- **WebSocket 替代 SSE**：事件均为单向推送，WS 增加协议复杂度无对应收益——
  v1 用 SSE，若未来出现厂商订阅流式需求再评估。
- **厂商逻辑写死在宿主/manifest 专有字段**：违反供应商中立原则——一律走
  声明式能力 + opaque 转发。
- **preset 承载查询定义**：preset = "看什么"（指标选择，§7.2.1 冻结
  addendum），custom_query = "算什么"——保持正交。
- **Arrow 二进制响应首期交付**：LTTB 预算已封顶响应尺寸（50k 点/序列），
  JSON 默认够用——列 M4 可选项。
