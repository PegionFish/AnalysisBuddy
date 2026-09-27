# AnalysisBuddy 平台演进规格

> 状态：已确认，待实施
> 日期：2026-08-28
> 适用范围：公开 AnalysisBuddy 主干及其通用契约。领域模块、受控部署细节、内部流程和内部规则不属于本文档。

## 1. 目标与范围

AnalysisBuddy 从 Windows 桌面日志查看工具演进为一个可由不同模块扩展的分析平台。平台必须同时提供：

- Windows 桌面宿主，继续使用 Tauri 和现有 React UI；
- Linux 单节点服务宿主，提供版本化 HTTP API、SSE 事件流与浏览器工作台所需的通用能力；
- 同一套插件协议、解析管线、会话模型、健康状态、规则报告和 JSON 导出；
- 外部受控调用方的大文件两阶段提交：先上传 artifact，再以 artifact、context 和幂等键创建分析任务；
- 由插件实现的快速分析能力，以及将多个插件的标准化结果按显式 DAG 进行组合分析的能力；
- 可被自动化与未来 AI 系统消费、可脱敏、可追溯、版本化的 JSON 分析包；
- Windows 与 Linux 上的可靠手工 ZIP 模块安装、预热验证、版本激活和回滚；
- Docker/Compose 与原生 systemd 两种同等受支持的 Linux 交付方式。

本规格不要求首期实现多节点集群、实时 tail、远程 URL 拉取、自动模块更新、在线模块下载安装、一般用户自定义脚本、统一身份认证或把原始日志自动交给外部 AI 服务。

## 2. 已确认的架构决策

| 决策 | 结论 | 理由 |
|---|---|---|
| 运行平台 | Windows 桌面 + Linux Web/API | 共用核心，分别适配本地桌面与内网服务工作流。 |
| 高可用目标 | 单节点高韧性 | 先完整实现恢复、回滚、隔离、备份和健康检查；接口为双节点保留扩展位。 |
| Linux 部署 | Docker/Compose 与 systemd 同等支持 | 两种形态共用配置、数据根、健康检查和验收。 |
| 服务信任模型 | 内网受信、单用户 | 默认 loopback；显式 LAN 监听时需 API Key；SSO/多租户留给后续。 |
| Web UI | 独立私有 WebConsole | 消费公开 API 契约，不长期 fork 桌面 UI 或公开核心。 |
| 模块分发 | 发布包只含 `builtin-csv` | 其他模块由用户取得本地 ZIP 后手动安装。 |
| 上游检查 | 只读、用户触发 | 可以比较 metadata 中的上游版本；绝不下载、安装或后台轮询。 |
| 规则首期 | 插件内预编码规则 | 领域知识随各模块维护，交付快且不污染公共核心。 |
| 规则演进 | 受限 ad hoc 数据条目，后续声明式规则 | 不允许 API 或 UI 传入任意脚本/可执行表达式。 |
| AI 集成 | 先定义 JSON bundle | 核心不绑定模型或供应商；AI 是可替换的私有 consumer。 |

## 3. 运行时与 crate 边界

现有 `ab-protocol`、`ab-host`、`ab-pipeline` 与 `ab-app` 是演进基础，不重建平行解析架构。目标 workspace 边界如下：

```text
core/
├─ ab-protocol/       # 模块 RPC、manifest、JSON Schema、兼容性类型
├─ ab-host/           # 发现、进程、RPC、健康、资源边界、stderr、根策略
├─ ab-pipeline/       # 时序数据、查询、LTTB、session 数据模型
├─ ab-application/    # 与传输无关的 artifact、任务、session、分析、安装用例
├─ ab-api/            # HTTP DTO、OpenAPI、稳定错误码、SSE 事件和生成客户端输入
├─ ab-server/         # Linux HTTP/SSE、上传边界、认证配置、进程生命周期
└─ ab-desktop/        # Tauri command/event 适配；可由当前 ab-app 演进而来
```

`ab-application` 是唯一的领域用例编排层。桌面 command 和 HTTP route 都只能调用它，不能各自实现导入、解析、取消、会话恢复、模块切换或回滚逻辑。为控制迁移风险，当前 `ab-app` 先抽出无 Tauri 依赖的服务，再在 API 稳定后重命名或拆分为 `ab-desktop`。

```text
Windows 文件选择 ──┐
                  ├─> ApplicationService ─> Host + Pipeline + Storage
HTTP artifact 上传 ─┘                              │
                                                    ├─> Tauri events
                                                    └─> HTTP snapshot + SSE
```

## 4. 受控 artifact、任务与会话

Linux 服务器只接受受控上传，不能接受浏览器/调用方提供的任意服务器绝对路径。数据根必须可配置，Docker 使用持久卷，systemd 使用受权限保护的数据目录：

```text
<DATA_DIR>/
├─ metadata.sqlite             # 任务、session、artifact、活动模块与审计元数据
├─ artifacts/sha256/<hash>/    # 不可变输入内容
├─ sessions/<session-id>/      # 会话视图和派生索引
├─ plugins/{user,managed}/     # 版本化的外部模块根
├─ staging/                    # 永不被发现器加载的上传/安装临时区
├─ diagnostics/                # 轮转、限额、脱敏的本地证据
└─ backups/                    # 明确创建的恢复包
```

### 4.1 两阶段大文件提交

第一阶段创建可续传上传并返回 `upload_id`；分块请求携带偏移和内容摘要。完成阶段验证大小与 SHA-256，随后提升为不可变 `artifact_id`。相同内容可按哈希去重。

第二阶段以 `artifact_id`、`source_id`、`correlation_id`、`context`、可选的受限 `rule_input` 和调用方生成的 `idempotency_key` 创建分析任务。返回 `ingestion_id`、`session_id` 和 `task_id`。未关联 artifact 根据保留策略回收。

### 4.2 状态、重试与恢复

任务状态为 `queued`、`running`、`succeeded`、`failed`、`cancelled`、`interrupted`。状态转换持久化且具有 revision。服务重启将残留 `running` 任务标为 `interrupted`，保留 artifact、错误和诊断引用；首期仅支持显式重试，不承诺从插件进程中间状态断点续算。

解析链路必须使用背压或明确失败，不得以有界队列 `try_send` 静默丢弃 record batch。每个任务、模块和 session 都拥有输入字节、记录数、series 数、墙钟时间、RSS、stderr、并发数和导出大小预算。

session 必须钉扎 artifact 哈希、模块 ID/版本、协议版本、规则/分析版本和结果摘要。历史 session 优先使用被钉扎版本；该版本不可用时返回明确错误，不能静默换用新版本得到不同结论。

## 5. HTTP API 与 SSE

Linux API 使用 `/api/v1`。破坏性语义变更只能进入新主版本。耗时写操作异步返回 `task_id`，可重试写请求必须支持 `idempotency_key`。稳定错误码由 `ab-api` 定义，UI 自行国际化。

```text
GET  /health/live
GET  /health/ready
GET  /api/v1/capabilities

POST /api/v1/uploads
PATCH /api/v1/uploads/{upload_id}/chunks
POST /api/v1/uploads/{upload_id}:complete
POST /api/v1/ingestions

GET  /api/v1/tasks/{task_id}
POST /api/v1/tasks/{task_id}:cancel
POST /api/v1/tasks/{task_id}:retry

GET  /api/v1/sessions/{session_id}
POST /api/v1/query/series
POST /api/v1/query/key-values
GET  /api/v1/sessions/{session_id}/analysis-bundle

GET  /api/v1/plugins
POST /api/v1/plugins/install
POST /api/v1/plugins/{plugin_id}:verify
POST /api/v1/plugins/{plugin_id}:rollback
POST /api/v1/plugins/{plugin_id}:check-version

GET  /api/v1/events
```

SSE 是单向实时通知而非状态唯一来源。事件包含单调 `event_id`、对象 ID、revision、类型和最小载荷；重要终态先持久化再推送。客户端以 `Last-Event-ID` 续接；过期时服务器发送 `resync_required`，客户端重新获取任务、session 和模块快照。

服务默认绑定 loopback 且拒绝 CORS。需要 LAN 监听时必须配置 API Key；密钥来自受保护文件或进程环境，日志中永不输出其值。WebConsole 推荐通过同源内网反向代理访问，代理由部署者控制而不是由浏览器保存高权限密钥。

## 6. 分析、规则与多模块组合

模块在既有 `parse` 之后可声明可选 `analyze` 能力。它接收经宿主封装的 `session_id`、`file_id`、受限 `context`、受限 `rule_input` 和执行预算，输出通用 `AnalysisReport`。不支持该能力的模块保持完全可用。

`context` 与 `rule_input.entries` 的内容由模块自己的 schema 解释；公共核心只处理大小、结构、脱敏和生命周期边界。ad hoc 条目只能是数据，例如阈值、目标、策略 ID、开关、枚举条件和比较对象，不能包含脚本、Shell、动态代码或任意可执行表达式。

`AnalysisReport` 包含模块定义的规则 ID/修订、结果（`pass`、`warning`、`fail`、`not_applicable`、`error`）、严重级、纯文本摘要和证据引用。模块可额外发布可组合的 `AnalysisFact`。其他模块不能读取其原始文件、内存、环境或私有 context。

跨模块分析以宿主编排的 `AnalysisPlan` DAG 完成：节点只消费显式发布的指标、事件、facts 与报告，声明版本依赖、资源预算和失败策略。依赖失败时组合节点必须报告 `blocked`、`partial` 或 `not_applicable`，不得把缺失数据伪装为正常结论。时间关联统一使用 UTC 毫秒，并携带时间解析置信度。

## 7. 面向自动化和 AI 的 JSON 输出

公共仓定义并测试版本化 schema：

```text
AnalysisReport v1
AnalysisFact v1
CompositeAnalysisReport v1
SessionAnalysisBundle v1
```

`SessionAnalysisBundle` 是自动化与未来 AI 的标准输入。它包含 schema 版本、artifact 哈希、插件/规则版本、时序摘要、关键事件、报告、组合结论、不确定性、失败/降级原因和可追溯证据索引。它以规范化 JSON 输出，并支持 `summary`、`review`、`automation`、`ai-safe` 导出 profile。

默认 bundle 不包含原始日志行、完整 context、绝对路径、密钥、内部 URL、未脱敏 stderr 或无限时序数据。大数据只以统计、采样和受控引用表示。每个 bundle 都记录所用脱敏 profile 与被省略字段原因。

AI 不属于核心执行路径。未来私有 adapter 在显式授权后接收 bundle，负责额外脱敏、预算、模型调用、提示词模板、输出 schema 校验和审计。模型输出只能是 `suggestion`，不得覆写事实、规则报告或人工结论；输入中的任何指令式文本均视为不可信数据。

## 8. 模块根、手工安装、版本检查与回滚

发现根演进为 `PluginRoot`，每个根声明 kind、path、优先级、读写能力、安装/删除能力、允许来源、信任策略、元数据策略和覆盖策略。首期至少支持 `builtin`、`portable`、`user`、`managed` 根。

公开发行包只内置 `builtin-csv`。`demo-tool` 保留为开发、SDK 示例和 E2E fixture，不得进入最终 ZIP。所有额外模块均由用户取得本地 ZIP 并手动安装。

安装采用版本化目录和活动指针：

```text
<root>/<plugin-id>/
├─ versions/<version>/
├─ active.json
├─ previous.json
└─ install-journal.json
```

流程为 staging 解压、路径安全检查、manifest/兼容性检查、哈希/清单检查、validator、预热握手、写入版本目录、原子替换 `active.json`、保留 previous。不得先删旧版本。每个模块的切换串行化，运行中进程固定到启动版本，启动恢复依据 journal 选择最后健康版本。

首期删除所有远程下载、自动安装、后台轮询和自动更新能力。仅保留用户触发的只读上游版本检查：metadata 可选地声明 `repository` 与受限 `version_probe`；结果仅为当前版本、最新可见版本、检查时间和状态。检查不会下载、安装、运行远程代码或执行 Git 操作。

## 9. 发布边界与文档边界

公开主干、公开构建和公开发行 ZIP 都不得包含用户模块、额外模块包、非通用 fixture、工作区父目录内容、未跟踪配置或受控部署资料。发布脚本和 CI 必须对 ZIP 做正向清单断言：唯一允许的插件目录为 `builtin-csv`。

公共协议、schema、SDK 和开发指南必须在公开仓内自包含，不能引用未跟踪设计文档。任何领域模块、内部部署、私有 WebConsole、规则、schema、测试样本和上游地址只在相应受控仓库维护；公共文档只描述通用接口与中性行为。

## 10. Linux 运行与恢复

Docker/Compose 与 systemd 共用数据根、配置 schema、健康端点、备份格式、模块包格式和验收测试。容器和服务都以专用低权限账户运行，使用持久数据目录，具备优雅停止、自动重启、启动限频、资源限制和日志轮转。

`live` 表示进程可响应，`ready` 还必须确认 metadata 可用、恢复流程完成、任务调度器可接收任务且无未处理模块事务。备份包至少包含 SQLite 一致性快照、session 元数据、artifact/module 指针和校验清单；是否包含大型 artifact 由保留策略显式决定。

## 11. 验收总则

完成实现的条件包括：

1. 干净 worktree 与隔离构建目录可构建、测试和打包；
2. 桌面和 HTTP 适配层对同一领域用例给出相同状态与错误语义；
3. 两阶段上传、任务恢复、模块回滚、SSE 续接、数据背压和 JSON bundle 都有自动化测试；
4. Docker 与 systemd 都通过启动、重启、持久卷/数据目录、故障恢复和健康检查演练；
5. 发行 ZIP 仅包含 `builtin-csv`；
6. 公共 CI 不依赖父工作区或受控模块；
7. 实现者之外的独立验收者完成黑盒与故障注入验证。

实施顺序、任务主权、依赖和验收命令见 [平台开发路线图](../development/2026-08-28-platform-roadmap.md)。
