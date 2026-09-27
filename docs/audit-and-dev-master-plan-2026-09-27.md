# AnalysisBuddy 审计与开发总计划（单一交付报告）· 2026-09-27

> **本文是后续开发工作的唯一执行依据，自包含。** 由三份分册于 2026-09-27 合并而成（原 `docs/multi-agent-code-audit-2026-09-27.md`、`docs/dev-master-plan-2026-09-27.md`、`AnalysisBuddy_WebUI/docs/ui-style-unification-2026-09-27.md` 已删除，内容全部并入本文，勿再引用旧路径）。
> 执行者预设：善于开子代理、并行处理、善用工具的强 AI 编排者。运维背景另见工作区根 `AGENTS.md`。

## 编排者快速启动（TL;DR）

1. **阅读顺序**：卷一（目标架构裁定，最高依据）→ 卷二 §2.5（多子代理并行规则）→ 卷二 §2.6 的 Wave 0，随即开始派发。卷三是证据基座（按 finding id 引用，不必全文通读），卷四是 WS-J 的实现级规格。
2. **派发纪律**：子代理 prompt 必须自包含——只给它的任务卡 + 所属 WS 章节 + 卷三对应 finding 锚点；**不要把整份报告塞进子代理**。
3. **红线零容忍**（卷二 §2.1）：生产机 `43.142.81.160` 任何写操作须先获人类批准；不碰两机的 DQA 既有服务；审计行号基于 2026-09-27 代码，动手前必须重读目标文件。
4. **台账**：`docs/dev-plan-2026-09-27/ledger/`（卷二 §2.8），没有台账记录的任务视为未完成。

---

# 卷一 目标架构裁定（最高依据）

## §1.1 模型定义（产品所有者裁定，2026-09-27）

以下裁定是后续所有安全/架构开发的最高依据；与本文冲突的既有设计（含 AGENTS.md、`tenant-isolation.md` 中的相关论述）一律以本卷为准并需更新文档：

1. **唯一的鉴权边界是插件管理**。安装/更新/卸载插件必须满足二者之一：
   - **管理员验证**：经网关的插件管理 API，要求 DQA `sysadmin` 角色；
   - **服务器端操作**：由持有 root/sudo 的运维在服务器上直接操作（放置目录/执行 CLI），并提供等价的触发重扫机制。
2. **除插件管理外基本不存在鉴权环节**。工作台/分析功能对可达网络匿名开放，不引入登录。
3. **安全性的实现方式是 Session 隔离，而非认证**：
   - Session ≡ 一个浏览器会话或一个 API 客户端会话 ≡ 一个独占的 ab-server 实例（沙箱）；
   - **每个 Session 只能读取自己上传的文件**；不得访问服务器本地文件系统上的任意路径，也不得访问其他 Session 的文件；
   - 文件进入系统的唯一通道是**上传**，按路径引用服务器文件的能力在服务形态下不对外开放。
4. **Session 结束时所有文件必须被清理干净**（进程、内存数据、临时目录、磁盘数据目录、上传副本），不残留任何会话数据。
5. **WebUI 的 UI/UX 风格统一参考 `/Users/bob/DQA_Unified_Database`**（实现规格见卷四）。

## §1.2 信任边界与总则

```
浏览器/API客户端 ──HTTP──▶ nginx(:8601) ──▶ auth-gateway(:8602, loopback, 唯一安全边界)
                                             │ 每 Session 一个独占 ab-server 实例
                                             ▼
                              ab-server(127.0.0.1:8610+, 随机token, 随机 --import-roots)
                                             │ stdio IPC
                                             ▼
                                     插件子进程（不信任输入）
```

- 网关用"每 Session 一进程"包住单租户 ab-server——这是既有"网关多租户"方向的**加固**，不是重写；进程边界即隔离边界（独立 store/内存/临时目录）。

## §1.3 Session 身份与生命周期

- 身份：`ab_sid` cookie，服务端**首次接触时签发**（128bit+ CSPRNG；HttpOnly；SameSite=Lax；启用 TLS 后 Secure；绝对寿命 ≤12h）。**客户端自报/植入的 `ab_tenant` 值一律忽略**（修复卷三 A4#3：现行 cookie 即裸凭证）。
- 生命周期参数：空闲 TTL（现 5-10min 语义不变）、绝对寿命 12h、容量驱逐（仅驱逐空闲者，超限时 503 `tenant_capacity` + Retry-After，语义保持）。
- 会话终结的**全部触发**（六类，每类都必须走同一个 `teardownSession(sid)` 幂等函数）：显式结束（`DELETE /api/v1/session` + 前端"结束会话"按钮）、空闲 TTL 到期、绝对寿命到期、容量驱逐、网关重启/停机（=全部会话终结，启动时清扫）、实例崩溃孤儿（reaper 兜底）。
- `teardownSession(sid)` 清理矩阵（**清理契约的规范文本**）：

| 工件 | 清理动作 | 验证手段 |
|---|---|---|
| ab-server 子进程 | SIGTERM → 3s → SIGKILL，等待 exit 后才归还端口 | 进程表无该 sid 命令行 |
| `/dev/shm/ab-tenants/<sid>`（TMPDIR） | `rm -rf` | 目录不存在 |
| 租户数据目录（sessions/presets 磁盘） | `rm -rf`（预设已客户端化，§1.7） | 目录不存在 |
| 内存 store/jobs/diagnostics | 随进程消亡 | —（进程死即清） |
| 上传副本（实例内 `<TMPDIR>/ab-server-uploads/`） | job 终态即删（不等会话终结，见 WS-B2） | soak 断言 0 残留 |
| Cookie | `Set-Cookie: ab_sid=; Max-Age=0` | — |

- **不变式 I-1（机器验证）**：任意会话终结事件后 60s 内，该 sid 的工件集合（进程+两个目录）为空。由 WS-D 的 residue checker 在 chaos 基准中断言（卷二 §2.7）。
- 网关启动 = 全量清扫：扫描 `/dev/shm/ab-tenants/*`、租户数据目录、本机遗留 ab-server 进程，全部清除（幂等、带日志）。

## §1.4 API Session（非浏览器客户端）的隔离逻辑

浏览器有"窗口关闭"这个天然终结信号；API 客户端没有。因此 API Session 采用**显式租约（lease）模型**：`ab_sid` 是路由与隔离的 capability 句柄（bearer，128bit+ 不可猜），不绑定 TCP 连接、不绑定身份、不跨会话合并。文件句柄/进程是它的正确类比：**显式打开、显式关闭、超时回收**。

**创建**
- 显式（API 客户端标准入口）：`POST /api/v1/session` → `201 {sid}`（同时 Set-Cookie，cookie jar 客户端两种方式通吃）。
- 隐式（仅浏览器兜底）：不带 sid 的**只读 GET** → 网关铸造新 sid 并 Set-Cookie（浏览器首载透明完成）；**不带 sid 的非 GET 请求（upload/import/query 等）一律 `400 session_required`**——堵住"无 cookie 脚本每次请求静默新建会话、上传的数据下一次查询就'消失'"的经典 footgun。

**终结（与浏览器语义逐项对应）**

| API 客户端 | 浏览器对应 | 说明 |
|---|---|---|
| `DELETE /api/v1/session`（幂等，可重试） | 关闭窗口 | 客户端责任，用完即还；立即触发 teardown 清理矩阵 |
| 空闲 TTL（无 in-flight 且超过 TTL 无请求） | 同款兜底 | 客户端崩溃/断网/忘记归还的安全网 |
| 绝对寿命 12h | 同 | 泄漏上界；超长分析任务需重建会话并重传文件（契约写明） |
| 网关重启 / 容量驱逐（仅空闲） | 同 | 清理契约（§1.3 矩阵）不变 |

**语义要点**
- 持有 sid = 拥有该会话全部数据面；**sid 丢失即数据丢失**（空闲 TTL 后回收），无跨会话找回——这是隔离的代价，属预期行为，写入契约供客户端设计重试逻辑。
- 同一客户端可同时持多个 sid = 多个完全隔离的沙箱（并行分析任务的推荐用法）；同 sid 的并发请求共享该实例的文件（等同同一浏览器多标签页）。
- in-flight 请求计入活跃（busy 会话不被驱逐，沿用现语义）；"空闲" = 无 in-flight 请求 且 距上次请求超过 TTL。
- 会话已过期后的请求返回 `404 session_not_found`（明确错误码），客户端据此**重建会话 + 重传文件**，而不是误读为"数据丢了"。
- 防滥用（遗留决策 D-2）：无鉴权模型下任何可达者都能开会话直至全局容量；既有 503 + Retry-After 是基础闸门，可选增加每源 IP 并发会话上限（默认关闭，env 开启），是否启用留待运行观察后决定。

## §1.5 文件摄入（Session 命名空间）

- 唯一通道：`POST /imports/upload`（multipart）。**公开面移除** `POST /imports {paths}` 与 `/sessions/load {path}` 的任意路径形态（网关 404/405 该形态；前端删除路径输入框）。
- ab-server 纵深防御：新增 `--import-roots <dir>[,<dir>]` 启动参数，import/sessions-load 的路径参数必须落在 roots 内（词法规范化 + 组件级 starts_with，沿用 session 路径校验的既有实现风格），否则 403 `path_forbidden`。网关为每实例只传其自身上传目录。**桌面形态不传该参数 → 本地路径能力完整保留**（同一个二进制两种形态）。
- 新增 `GET /files`：返回本实例（=本会话）已加载文件清单（file_id、basename、size、状态、来源）——填补卷三 A1 已知缺口，也是前端"只显示自己文件"的数据源。
- 配额（413/429 语义进契约）：单文件 ≤64MB（沿用请求体上限）、并发已加载文件数（默认 32）、累计上传字节/会话（默认 512MB，可 env 覆盖）。实例内存预算沿用 `AB_TENANT_MEMORY_MB`（机制在 Wave 0 T0.3 核实：cgroup/rlimit/env 协商中的哪一种，写进契约）。
- 上传副本清理：job 进入终态（completed/failed/cancelled）即删；进程启动时清扫本实例上传根的历史残留。

## §1.6 插件管理（管理员双通道）

- 网关通道：`needsAdmin` 分支 revalidate 后 `mapRole !== 'sysadmin' → 403 permission_denied`（修卷三 P0-1）；管理操作审计日志（who/plugin/action/result 时间线，JSON 行落盘）。
- 运维通道：`POST /plugins/rescan`（或 `abctl plugins rescan` CLI，二者择一实现，root 可调）触发 `registry.reload()` 全量重扫；builtin 目录缺失/损坏时允许**修复性安装**（放行条件：部署目录中该 id 不存在或 manifest 无效）。这同时解开卷三 A5-P1-7 的"修复死路"。
- 安装即用：安装管线第⑥步后插入 initialize 冒烟（spawn→握手→shutdown，5s 预算，失败回滚并把 stderr 尾部带入错误响应）——"安装 200 ≠ 可用"（P0-3-3）就此关闭。裁定 §1.1 要求插件安装是管理员操作，则**安装必须一次成功可用**。
- 交付一致性：builtin 清单从 build.rs（源码目录推导）移交打包层，发布产物生成清单并断言 `BUILTIN_PLUGIN_IDS ⊆ 交付插件目录`；manifest entry 平台感知（`entry.platforms` 或扩展名回退），消灭 `.exe` hack（P0-3-1/2）。

## §1.7 数据与预设

- 服务端**不**跨会话保存用户数据：预设客户端化（localStorage + JSON 导出/导入），切换上线时为现网用户提供一次性导出工具并公告（遗留决策 D-1）。
- 桌面 `.absession` 增加版本头 + 前向兼容读取策略；preset 引用失效时降级为"置灰+提示"而非静默丢弃。

## §1.8 UI/UX 统一裁定

WebUI 的 UI/UX 风格统一参考 `/Users/bob/DQA_Unified_Database`：token 层已对齐（勿动），壳层与组件层按卷四规格统一；实现任务为 WS-J（卷二 §2.6）。

## §1.9 受影响审计发现的重新解读

| 发现 | 原判定/修复方向 | 裁定下 | 变化 |
|---|---|---|---|
| A1#1 + A4#4 匿名任意路径导入 | P0；建议"加登录或目录白名单" | **P0 不变，修复方向变更**：目标模型禁止 Session 访问本地文件，因此不是加登录，而是**公开面彻底移除路径形态**（网关路由层 + ab-server `--import-roots` 双层，§1.5），并同步修正契约 §7.3 虚假断言与 `tenant-isolation.md` 中"刻意不启用 PrivateTmp 以便导入宿主 /tmp"的过时设计依据 | 修复方向 |
| A4#1 管理 API 无角色校验 | P0；加 sysadmin 判断 | **P0 不变，与裁定 §1.1-1 完全一致**；同时补齐**服务器端 root/sudo 运维通道**（rescan/CLI）为同等公民，联动 A1#6 / A5-P1-7 | 范围扩大 |
| A4#3 登出不回收租户、`ab_tenant` 裸凭证 | P1；建议 tid 与 sid 绑定 | **P1 不变，修复方式明确化**：改为服务端签发 `ab_sid` + 显式会话终结 + 全工件 teardown（§1.3）；预置数据 7 天磁盘保留与清理契约**直接冲突**，必须客户端化（§1.7） | 修复方式 |
| A4#5 明文 HTTP / Cookie 无 Secure | P2 | **P2 维持但实施优先级提至 M1**：`ab_sid` 成为数据面唯一凭证后，嗅探即接管会话；与角色门同批实施 | 优先级 |
| A1#2（P0-4）上传副本残留 | P0/P1；job 终态清理 | **升格为清理契约的强制组成部分**（裁定 §1.1-4），非可选优化 | 定性 |
| A1#10 / A2#3 / A3#1 资源生命周期四环 | 分散的 P1/P2/P3 | **统一并入清理契约**，验收合并为"会话终结 residue=0"（chaos 基准，卷二 §2.7） | 定性 |
| A4#2 容量竞态 | P1 | P1 不变：属资源安全，与隔离模型正交 | 无 |

---

# 卷二 开发执行计划

## §2.1 使用说明与环境事实（编排者必读）

1. **仓库布局**：主仓 `/Users/bob/AnalysisBuddy/AnalysisBuddy`（Rust workspace + ui + plugins + sdk）；在线服务版 `/Users/bob/AnalysisBuddy/AnalysisBuddy_WebUI`（React 前端 + `server/auth-gateway.js` 网关 + deploy）。
2. **环境事实（Wave 0 核实/引导）**：
   - 本机（macOS arm64）**无 Rust 工具链**——Wave 0 任务 T0.1 安装 rustup 并建立 `cargo test/check` 全绿基线（若本机网络受限，备选：在 192.168.1.171 用既有 `/home/bob/ab-rustbuild` 沙箱远程构建，ssh 手段见 `.abwebui-ops/sshx.py`）。
   - Node/前端工具链可用性同样在 T0.1 核实（`npm ci && tsc && vite build`）。
   - 部署主机两台：`192.168.1.171`（开发/验证机，bob 免密 sudo）与 `43.142.81.160`（生产 DQA 主机，2C2G，与 dqa-api+PostgreSQL 共存）。
3. **生产红线（不可协商）**：
   - `43.142.81.160` 上任何**写**操作（部署、systemd、插件安装、放行端口）必须先向人类请求明确批准，批准记录写入台账后才可执行；只读诊断可直接做。
   - 两台机器上的既有服务不得触碰：dqa-api/dqa.conf(:80)/PostgreSQL/Gitea(:3001)/`analysisbuddy.service`(:8600 旧实例)。
   - 禁止 `pkill -f` 模糊匹配（会自杀，AGENTS 有前科记录）；清理进程用精确 PID。
   - 禁止回显任何密钥/口令（`/etc/dqa/dqa.env` 可读但只许引用不许打印）。
4. **审计时效**：卷三行号基于 2026-09-27 代码。**任何子代理动手前必须重读目标文件核实**，行号漂移不影响 finding 本体；若代码已变导致 finding 失效，停手并在台账记录 `finding-invalidated`。

## §2.2 里程碑与验收（可证伪的完成定义）

| 里程碑 | 内容 | 验收（全部机器可判） |
|---|---|---|
| **M1 安全闭环** | P0-1/P0-2 角色门+路径面移除+`--import-roots`、sid 签发、TLS/Secure、cookie/DQA 容错超时 | 隔离矩阵测试全绿（§2.7-I1 脚本）；红队报告无 P0/P1；演示：viewer 装插件 403、匿名导入 `/etc/passwd` 被拒、植入 ab_tenant 无效 |
| **M2 资源闭环** | 清理契约全量实现（teardown 矩阵、上传副本终态清理、unload_file 补齐、diagnostics/jobs 回收、UI 会话重置卸载） | chaos 1000 会话基准 residue=0（不变式 I-1）；上传目录 0 残留；重复导入内存平台期（棘轮断言） |
| **M3 稳定性** | spawn_blocking×5、stderr 有界读、spawn 锁分桶+超时、parking_lot+CatchPanic、per-file store 锁、死会话复活接线、入站有限性校验 | soak ≥35min×2 次 0 失败（对齐历史基准口径）；阻塞 IO 清单清零；kill 单插件后自动恢复（无需 reload） |
| **M4 插件交付层** | builtin 打包层清单+一致性断言、entry 平台感知、安装冒烟、rescan/修复性安装、Linux exec 位、validator 接入 | Linux 上"安装→立即可解析文件"端到端；builtin 清单≠交付目录时构建失败；破坏 builtin 后走运维通道 5min 内恢复 |
| **M5 协议/SDK 单源** | 版本回显协商、ab-plugin-rt 抽取、Python SDK 去 vendoring、防漂移测试读正本 | 三插件跑在 ab-plugin-rt 上行为等价（golden 测试）；SDK 字节级一致性 CI 断言；错误版本握手得到明确错误码 |
| **M6 发布就绪** | 版本/CHANGELOG 纪律、双主机 runbook、CI 全量（插件测试/网关/soak 入库）、仓库卫生、**WebUI UI/UX 统一验收** | tag→产物→部署→回归全流程在 171 演练通过；160 的生产变更经人工批准执行；UI 统一规格（卷四 §4.4）验收清单全过（J 任务可在 Wave 1 起随时并行，验收挂 M6） |

## §2.3 工作流（WS）划分与文件所有权地图

**所有权规则是并行开发的地基**：每个 WS 独占一组路径；热点共享文件（§2.3.2）由编排者在集成窗口串行处理；无主文件一律归编排者。

### §2.3.1 WS 清单

| WS | 名称 | 独占路径 | 对应卷三发现 |
|---|---|---|---|
| **WS-A** | 网关：安全 + Session 生命周期 | `AnalysisBuddy_WebUI/server/**`、`AnalysisBuddy_WebUI/deploy/**`、`AnalysisBuddy_WebUI/src/{api,auth}/**` | P0-1、A4#2/3/4/5/6/7/10、§1.9 |
| **WS-B** | 摄入与会话命名空间 | `core/ab-server/src/{routes,jobs}.rs`、`ab-engine/src/pipeline_bridge.rs` 的**导入/清理区块**、契约 §Session | P0-2、P0-4、A1#3/4、A1 明细 P3(无 GET /files) |
| **WS-C** | 引擎生命周期与稳定性 | `core/ab-engine/**`（除 WS-B 区块）、`core/ab-host/**`、`core/ab-pipeline/**` | 主题 2/3/4/5、A2#1-10、A1#8 |
| **WS-D** | 清理验证与混沌（新代码） | `tools/cleanup-verify/**`（新建） | 不变式 I-1、M2/M3 验收 |
| **WS-E** | 插件交付层 | `plugins/**`、`core/ab-engine/build.rs`、`ab-engine/src/commands/plugin_manager.rs`、`ab-host/src/{manifest,discovery}.rs`、`scripts/**`、`.github/workflows/release.yml` | P0-3 全部、A2#4(Linux exec)、A5-P1-7 |
| **WS-F** | 桌面 UI 修正 | `ui/src/**`、`core/ab-app/**`、`core/ab-engine/src/commands/query.rs`（列式 DTO 例外授予） | A3 全部 |
| **WS-G** | 协议/SDK 单源化 | `core/ab-protocol/**`、`sdk/**`、新建 `core/ab-plugin-rt/` | 主题 5(协商)、主题 6、A5-P1-4/5/6 |
| **WS-H** | 契约/测试/CI/文档 | `docs/spec/**`、`docs/developer-guide/**`、`.github/workflows/ci.yml`、`tests/**`、台账目录 | §3.6 拓展缺口清单 |
| **WS-I** | 红队与挑战验证（Wave 2+） | `tools/redteam/**`（新建）、`tools/fuzz/**`（新建） | §2.7 |
| **WS-J** | WebUI UI/UX 风格统一（DQA 对齐） | `AnalysisBuddy_WebUI/src/styles/**`、`src/components/**` 与 `src/pages/**` 的**样式层**（className/CSS）、`src/lib/chartTheme.ts` | 卷三 §3.5 + 卷四规格 |

### §2.3.2 热点共享文件（冲突高发，串行窗口处理）

`pipeline_bridge.rs`（B/C 分区块仍同文件）、`routes.rs`（B/C7 CatchPanic）、`plugin_manager.rs`（C/E）、`plugins/*/src/main.rs`（E/G2）、根 `Cargo.toml`（编排者独占）、`docs/spec/http-api-v1.md`（冻结期 WS-H 独占，之后变更走 CCR）、`AGENTS.md`（编排者独占）、`AnalysisBuddy_WebUI/src/{FilePanel,WorkspacePage,PluginsPage}.tsx`（A/B 行为改动 × J 样式改动共用，串行窗口；J 只碰 className 与 CSS 文件）。

## §2.4 任务卡格式与单任务 DoD

每个任务是一张卡（编排者派发时填好，子代理开工前可申请修订）：

```
TASK <WS>-<n> <标题>
审计锚点: <卷三 finding id 或 "new">
独占文件: <paths>  共享热点: <paths 或 无>
依赖: <task ids 或 无>
DoD:
  [ ] 行为变更 + 对应单测/集成测试（先测后改优先）
  [ ] 契约可见行为 → 契约 diff 同批（CCR 已批）
  [ ] 本地门: cargo fmt+clippy+test / tsc+vite build / node test（按所涉语言全跑）
  [ ] diff ≤400 行（超出需拆分或申请豁免并说明）
  [ ] 台账更新（§2.8 模板）
验收命令: <可复制粘贴的验证命令>
```

## §2.5 多子代理并行开发规则（核心，必须遵守）

### 2.5.1 角色与派发拓扑

- **编排者（你）**：拆解/派发任务卡、维护台账、跑集成窗口、做 go/no-go 门禁决策、写决策日志。**不直接修改 WS 独占文件**（例外：§2.3.2 编排者独占文件、合并冲突的最小修复）。
- **Implementer**：一次领 1 张任务卡，改代码+写测试+跑本地门。**Implementer 不得再开子代理**（保持派发树扁平，只有编排者开代理），需要外部信息时在结果中报告而非自行扩大范围。
- **Reviewer**：每张任务卡完成后由**不同的**子代理评审（prompt 含任务卡+diff+卷三锚点），清单：目标模型合规（是否触碰 Session 边界/是否引入新鉴权面）、契约合规、测试是否真的覆盖 DoD、质量红线（§2.5.6）。Reviewer 只给结论+意见，不改代码。
- **Verifier**：跑长任务（soak/chaos/e2e/容量），可 `run_in_background`，结果写入台账。Verifier 不改产品代码。
- **Red-Team**（Wave 2+）：只做攻击性验证（§2.7-I1/I2），产出报告与 PoC，不修漏洞。
- **Archivist**（每 Wave 末一次）：归档 Wave 结果、更新 CHANGELOG 草稿与 ADR。

### 2.5.2 并发与批次

- 并发上限 **10 个子代理**（Implementer×≤8 + Reviewer/Verifier 若干）；任务卡不足时优先把 Verifier 类长任务后台化，保持流水线满。
- 批次 = Wave（§2.6）。Wave 内无依赖任务全部并行派发；有依赖的按依赖图拓扑排序分小波。
- **派发即完整**：子代理 prompt 必须自包含（快速启动 §2），并声明"审计行号可能漂移，动手前重读文件"。

### 2.5.3 冲突避免

1. **所有权即法律**：子代理只许写自己任务卡声明的独占/共享文件；发现需要改别人的文件 = 立即停手，报 `cross-ws-dependency` 给编排者，由编排者创建新卡或调整归属。
2. **热点文件串行窗口**：涉及 §2.3.2 文件的任务，编排者保证同一时刻只有一个在飞（用台账中的 `hot-file-lock` 字段登记持有者）。
3. **契约冻结**：Wave 0 后 `http-api-v1.md` 冻结；实现中发现契约必须改 → 提 CCR（contract change request：一段话说清变更+影响面），编排者串行评审合并后才能继续相关任务。
4. **分支模型**：每 WS 一条长分支 `ws/<id>/<slug>`（从 `main` 切），任务卡 = 分支上的 commit 序列（conventional commits：`feat(ws-b): ...`）。禁止跨 WS 分支互相 merge；禁止 force-push 已进入集成窗口的分支。

### 2.5.4 集成窗口（每个 Wave 结束）

编排者按依赖序执行：`ws/E → ws/C → ws/B → ws/A → ws/F → ws/G → ws/H`（E 的插件布局变更是 C/B 测试依赖）逐支 rebase 到集成分支 → 跑 **G-wave 门**（§2.5.5）→ 全绿合并回 main → 台账记录 → 派发下一 Wave。任何一支门禁失败：只回滚该支，其余照常；失败卡回炉重派（同卡重试 ≤2 次，第 3 次编排者亲自诊断拆卡）。

### 2.5.5 三级验证门

- **G-task（每张卡）**：Implementer 本地门全绿 + Reviewer 通过。
- **G-wave（每集成窗口）**：合并树上跑——`cargo fmt --check && cargo clippy && cargo test --workspace`、逐插件 `cargo test`、前端 `tsc && vite build && test`、网关 `node --test`、e2e fast 套件、隔离矩阵 smoke（§2.7-I1 脚本快速版）、residue 快检（10 会话）。
- **G-milestone（每里程碑）**：M2=chaos 1000 会话；M3=soak 35min×2；M1=红队全量；M4=Linux 端到端安装即用；均由 Verifier 后台跑、报告进台账。**防 flaky：任何门禁失败必须根因归档后才可重跑；同一门禁最多 3 次。**

### 2.5.6 质量红线（Reviewer 清单的一部分）

- 不新增锁路径上的 `unwrap/expect`（现有 ~206 处只减不增，新代码用 parking_lot 或显式错误）；
- 不新增 `unsafe`；不引入新依赖不经编排者批准（Cargo.toml 是编排者独占文件）；
- 会话边界行为（sid 签发/teardown/配额/import-roots）必须有测试，不允许"显然正确"；
- 契约可见行为不带契约 diff 一律打回；
- 生产红线（§2.1）零容忍。

### 2.5.7 阻塞与升级

子代理 30 分钟无实质进展或连续 2 次本地门失败 → 在结果中输出 `BLOCKED: <原因>` 终止。编排者处置：重派 / 拆卡 / 换实现路径 / 升级给人类（仅当涉及生产红线、CCR 争议、需要购买决策）。

## §2.6 Wave 计划（任务卡索引）

### Wave 0（串行，≤3 子代理 + 编排者）
- **T0.1 工具链引导**：本机 rustup + cargo 基线全绿；Node 工具链核实；远程构建沙箱备用路径验证。产物：`ENVIRONMENT.md`。
- **T0.2 契约冻结**：按卷一 §1.3-§1.7 更新 `http-api-v1.md`（§Session、--import-roots、GET /files、DELETE /api/v1/session、配额、/plugins/rescan、§7.3 改口）+ 网关-实例接口清单（env/flag）+ cookie 规格。评审合并后冻结。
- **T0.3 机制核实与仓库准备**：`AB_TENANT_MEMORY_MB` 生效机制报告；建 `ws/*` 分支与台账目录（`docs/dev-plan-2026-09-27/ledger/`）；登记遗留决策 D-1/D-2。

### Wave 1a（并行 ≤8：安全与资源闭环的第一刀）
| 任务 | 内容 | 依赖 |
|---|---|---|
| A1 | sysadmin 角色门（P0-1，一行核心+测试） | T0.2 |
| A2 | parseCookies 容错、DQA 超时+fail 策略、上游请求级超时 | — |
| A3 | `ab_sid` 服务端签发 + 忽略客户端 `ab_tenant` + `DELETE /api/v1/session` + teardownSession 骨架 | T0.2 |
| B1 | `--import-roots` + 公开面移除路径形态（含前端删输入框） | T0.2 |
| B2 | 上传副本 job 终态清理 + 实例启动清扫（P0-4） | — |
| C1 | 失败/取消/预算超限路径补 `session.unload_file`（4 处出口） | — |
| C2 | stderr 有界读（块读+截断，照抄 FrameReader 范式） | — |
| F1 | ECharts ResizeObserver（8 行级，快速收益） | — |

### Wave 1b（并行 ≤8：闭环收尾）
| 任务 | 内容 | 依赖 |
|---|---|---|
| A4 | 容量竞态修复（starting 计入检查与驱逐） | A3 |
| A5 | teardown 全工件清理矩阵 + 网关启动全量清扫 + 磁盘预设目录停用（配合 H3 前端迁移） | A3 |
| A6 | TLS/Secure 配置化 + nginx 安全头修复 + deploy 回滚改恢复备份 | T0.2 |
| B3 | `GET /files` + 会话配额（文件数/累计字节）+ 拒绝码 | T0.2 |
| B4 | upload `overrides` 死路修复（回显存储路径或 basename 键） | B1 |
| C3 | `ensure_session` is_live 复活接线（或 SessionTerminated 移除注册表条目） | — |
| C4 | spawn_blocking×5 处（ZIP/SHA-256/读头/上传写/下载写） | — |
| F2 | 桌面会话重置/替换导入时卸载旧 file_id + 保存收集改前端清单 | B3 |
| F3 | IPC 开关修复 + 删除 ab-app 4.6k 死代码 | — |
| F4 | load_session in-flight 守卫 + loading 态 | — |
| H1 | CI：插件 `cargo test` job + 网关 node test + tsc/vite（先于功能合入，立即生效） | T0.1 |

→ **G-milestone M1 + M2 验收**（红队 I1 前置版 + chaos 先导 100 会话）。

### Wave 2（并行 ≤10：稳定性 + 交付层 + 挑战开始）
| 任务 | 内容 | 依赖 |
|---|---|---|
| C5 | spawn 锁按 plugin_id 分桶 + probe 超时弃权 | C3 |
| C6 | parking_lot 迁移（jobs/frozen/paths/store）+ CatchPanic 层 | B 波合并后 |
| C7 | Store per-file 锁粒度 + freeze 出 worker | C6 |
| C8 | 入站 Record.value/confidence 有限性校验 | — |
| C9 | 终态 job 取消语义 + cancel_parse 接线（A1#3） | B2 |
| C10 | 通知丢批计数打通（fan→adapter→诊断口径） | — |
| E1 | builtin 清单移交打包层 + 一致性断言 + release.yml 带插件制品 | T0.2 |
| E2 | manifest entry 平台感知 + 制品 bin/ 目录 + 删 CI 复制 hack | — |
| E3 | 安装冒烟（5s 预算、失败回滚带 stderr） | E2 |
| E4 | 运维通道：rescan 端点/CLI + 修复性安装 | E1 |
| E5 | Linux exec 位恢复（unix_mode） | — |
| D1 | residue checker CLI（进程/目录/fd 断言，输出 JSON） | A5,B2 |
| D2 | chaos soak harness（随机会话×上传×杀，1000 会话档） | D1 |
| I1 | 红队 v1：隔离矩阵穿透（cookie 伪造/上传名穿越/zip-slip/直连实例端口/SSE 混淆/越权插件管理） | Wave1 合并 |
| J1 | 壳层对齐：topbar 72px/0 34px/去 blur/品牌 mark 作首页入口、主区 padding 34px 36px 60px、page-head 规范化 | 卷四 §4.3.1 |
| J2 | 组件层对齐（CSS-only）：按钮/tag/输入/表头/panel 头/空状态 | 卷四 §4.3.2-4.3.5 |
| J3 | 登录页重做：删 radial-gradient（V1）、平底布局、440px 卡/方形 brand mark（允许改组件结构与测试） | 卷四 §4.3.4 |
| J4 | Toast 深底彩点模式、弹层宽度归档 400/560/860（V3）、遮罩 token 化（V2）、拖放区对齐 up-drop | 卷四 §4.3.6 |
| J5 | PluginsPage 管理台骨架：一页一卡、左列表/右详情、限高内滚、动作弹层 560px（允许改组件结构与测试） | 卷四 §4.3.8 |

→ **G-milestone M3 + M4 验收**（soak 35min×2；Linux 安装即用端到端）。

### Wave 3（并行 ≤10：单源化 + 挑战全量 + 发布）
| 任务 | 内容 | 依赖 |
|---|---|---|
| G1 | InitializeResult 版本回显 + 协商 + 错误码 | — |
| G2 | ab-plugin-rt 抽取 + 三插件分步迁移 + golden 行为测试 | E 波合并后 |
| G3 | demo-tool 去 vendoring + 打包单源注入 + CI 字节级断言 | — |
| G4 | 防漂移测试改读正本（生成或 parse ab-protocol） | G1 |
| F5 | 图表列式数据通道（engine DTO 列式 + 前端 typed array + 缓存映射） | — |
| H2 | soak/chaos 脚本入库参数化 + CI 定期触发（nightly） | D2 |
| H3 | 预设客户端化迁移 + 一次性导出工具（D-1）+ `.absession` 版本头 | A5 |
| H4 | 版本纪律（tag 派生版本/CHANGELOG 生成）+ 历史文档归档标注 + 仓库卫生（scratch/产物清出） | — |
| I2 | 协议 fuzz：cargo-fuzz FrameReader + JSON-RPC 反序列化（nightly） | C8 |
| I3 | proptest：store freeze/query/LTTB 不变量 | C7 |
| I4 | 双主机 runbook 演练（171 自动；160 仅 dry-run 报告，实弹需人工批准） | M4 |
| I5 | 内存棘轮回归（RSS 平台期断言入 CI） | D2 |
| J6 | chartTheme 逐 token 核对 + 卷四 §4.4 验收（grep 零裸色值、弹层三宽度、并排截图目检） | J1-J5 |

→ **G-milestone M5 + M6 验收** → 发布。

## §2.7 挑战性任务（刻意设计的上限测试，鼓励编排者全力并行）

这些任务为强模型设计，允许失败但失败必须留下可复现的根因记录：

- **I1 红队（攻击性安全）**：以匿名与 viewer 两种身份，尝试：植入/伪造 `ab_sid` 与 `ab_tenant`、上传文件名路径穿越与编码混淆（`../`、URL 编码、NUL、超长名、Unicode 归一化）、ZIP 安装 zip-slip 与符号链接、从租户进程内探测并直连其他实例端口、SSE 事件混淆、越权调用插件管理、利用 `overrides`/`GET /files` 新端点的注入面。产出：漏洞报告（复现步骤+PoC+建议），P0/P1 项阻塞发布。
- **D2 chaos 基准（清理不变式验证）**：随机化生成 1000 会话生命周期事件流（创建/上传/导入/显式结束/随机 SIGKILL 实例/网关重启注入），全程与结束后断言不变式 I-1 与 `/dev/shm` 字节数、网关 fd 数、进程数回到基线。这是 M2 的唯一验收口径。
- **I2 协议 fuzz**：cargo-fuzz 目标对 FrameReader（长度前导帧）与 JSON-RPC 反序列化跑 ≥1h corpus，崩溃最小化后归档。
- **I3 属性测试**：proptest 对 store 冻结/查询/LTTB 建立不变量（点数守恒、首末点保留、窗口闭区间语义、乱序时间戳），随机数据 ≥10k cases。
- **I4+I5 部署与回归自动化**：171 全自动部署+回归；160 生成 dry-run 变更计划待批；内存棘轮防回归。
- **编排自由度**：Wave 2/3 中编排者可自主增派子代理做交叉评审、双实现竞标（同一任务卡派两个 Implementer 各出一版，Reviewer 裁决择优）——鼓励使用，成本可控即可。

## §2.8 台账协议（`docs/dev-plan-2026-09-27/ledger/`）

- 每 WS 一个 `ledger/<ws>.md`（模板：任务卡表 + 状态机 `todo/doing/blocked/done` + 每卡证据链接：commit、测试输出摘要、Reviewer 结论）。
- 编排者维护 `EXECUTION-LOG.md`（决策日志：CCR 裁决、集成窗口结果、门禁失败根因、生产批准记录）与 `DECISIONS.md`（遗留决策登记：D-1 预设一次性导出、D-2 每源 IP 会话上限）。
- 台账更新是任务 DoD 的一部分——没有台账记录的任务视为未完成。

---

# 卷三 审计证据基座（2026-09-27 多 Agent 审计）

## §3.1 方法与统计

5 个并行审计 Agent，全部只读，每条结论均有 `file:line` 证据：A1=ab-server+ab-protocol+契约；A2=ab-engine/ab-pipeline/ab-host；A3=ab-app+桌面 ui；A4=WebUI+网关；A5=插件+SDK+仓库治理。限制：本机（macOS）当时未安装 Rust 工具链，未运行 `cargo check/clippy`，全部为静态审计。

**统计：62 项发现（P0×4、P1×14、P2×29、P3×15）= 安全/架构审计 55 项 + UI 一致性 7 项（§3.5）。**

## §3.2 P0：可被利用 / 生态必然踩坑（4 项）

### P0-1 网关管理 API 只验"已登录"不验角色 —— viewer 即可安装插件获得宿主任意代码执行
- 证据：`AnalysisBuddy_WebUI/server/auth-gateway.js:1054-1063`（`needsAdmin` 分支仅 `getSession`+`revalidate`，无角色判断）；`mapRole()`（:168-171）只用于响应体展示，从未参与门禁。前端承诺"服务端是唯一边界"（`src/pages/PluginsPage.tsx:173-175`、`src/auth/session.tsx:115`）。
- 影响：任何 viewer 角色账号带自己的 `ab_session` 直接 `curl POST /api/v1/plugins/install`，`plugin.json` 的 `entry.command` 是任意命令 → 以 bob 身份在宿主机执行任意代码。违反卷一 §1.1-1。
- 修复：`needsAdmin` 分支 revalidate 后加 `mapRole(...) !== 'sysadmin' → 403`（前端已有 403 文案支持），一行闭合。

### P0-2 匿名"服务器路径导入" = 任意读本地文件（两个 Agent 独立命中）
- 证据：`core/ab-server/src/routes.rs:207-224` 接受任意 `paths`，`ab-engine/src/pipeline_bridge.rs:1354-1365` 仅校验存在性与 100MB；契约 `docs/spec/http-api-v1.md:428`（§7.3）却断言 *"No endpoint accepts arbitrary server-side file reads"*。网关侧除 `/api/v1/plugins*` 外全部 `/api/*` 免登录（`auth-gateway.js:1049-1069`），前端提供任意路径输入框（`src/components/FilePanel.tsx:116-137`）。
- 影响：能访问 :8601 的任何主机**无需账号**即可导入 bob 可读的任意文件并经 `/query/key-values`、`/metrics` 回读内容。`/sessions/load {path}` 同理。
- 修复（按卷一 §1.5）：公开面移除路径导入形态；ab-server 加 `--import-roots`；契约与 tenant-isolation.md 同步改口。

### P0-3 插件体系缺少"交付层"——三条 P0 共享同一根因
1. **builtin 双源真相**：内建身份由构建机**源码目录布局**推导（`core/ab-engine/build.rs:18-52` 扫描 `plugins/` 目录名），发现层只看**部署目录**（`ab-host/src/discovery.rs:104-117`），零一致性校验；release 流水线 server 包里 `plugins/` 只有 README（`.github/workflows/release.yml:157-183`）。这就是"已内建却不可用 + 409 module_protected 死锁"的结构性根因（git 中 `gen/builtin_ids.rs` 仍是旧清单 `["builtin-csv","demo-tool"]`，feature 分支新增 aibench-llama 已造成编译期/交付期分裂）。
2. **manifest 把构建细节泄漏进协议**：`plugin.json` 的 `entry.command` 硬编码 `target/release/*.exe`（`plugins/builtin-csv/plugin.json:6`），Linux 靠 CI 把 ELF 复制成 `.exe` 别名续命（`ci.yml:174-181` 注释自认是 hack）；`resolve_entry`（`manifest.rs:323-364`）无平台感知。一份 plugin.json 无法同时服务 Windows 与 Linux。
3. **安装门禁缺最后一公里**：安装管线止步于"文件形状校验"（`plugin_manager.rs:8-12` 七步），从不拉起进程冒烟；`tools/plugin-validator` 有完整行为回放能力（BE-01..13 + 27 条冻结规则）但**安装路径零调用**。"安装 200 ≠ 可用"（Python SDK ModuleNotFoundError 事故）是结构性缺口。

### P0-4 上传副本永久残留（清理契约强制项）
- 证据：`core/ab-server/src/routes.rs:280-284` 落盘后直接 `spawn_import`，全仓库无任何清理路径（对照同文件 :574 插件安装有 `remove_file`）；契约 §7.3 明文 *"each upload copy is removed after the import call"*。
- 影响：租户网关部署下 `TMPDIR=/dev/shm/ab-tenants/<租户>` 是**内存盘**——64MB/次的副本在实例被 kill -9 后永久残留，足以耗尽 2G 主机内存。直接违反卷一 §1.1-4。

## §3.3 跨模块系统性问题（6 大主题）

### 主题 1：单租户内核 × 多租户外壳的所有摩擦点
- Session 身份可被客户端植入（`ab_tenant` cookie 即凭证，`auth-gateway.js:998-1002`）；登出不轮换/回收（:288-303），共用电脑时跨用户数据残留（磁盘 7 天，`:74` dirTtlMs）→ 按卷一 §1.3 改服务端签发 + teardown；
- 容量检查与租户注册存在 ~32ms 异步间隙：`tenants.size >= T.max`（:467）与 `tenants.set`（:487）之间隔 `await waitHealthy`（:472），并发可突破 `AB_MAX_TENANTS`，匿名可触发 OOM。修复：`tenants.size + starting.size >= T.max`，驱逐时同样计入 starting。

### 主题 2：文件/资源生命周期不闭环（"17 个同名实例"的完整根因链）
1. **引擎层**：导入失败/取消/预算超限路径不调插件 `unload_file`（`pipeline_bridge.rs:1058,1086,1156,1195` 只清宿主侧 store），且 `loaded_files` 非空使插件进程的空闲回收永不启动（`ab-host/src/session.rs:643-650`）——每次"load 成功但 parse 失败"（坏文件，常态事件）都让插件进程多驻留 ≤100MB 原始文件；
2. **API 层**：`last_diagnostics` 每 file_id 一条、`unload_file` 不清理（`pipeline_bridge.rs:645-653`）；`jobs` HashMap 无 TTL 无上限（`ab-server/src/jobs.rs:59-61`）；
3. **UI 层**：桌面版 `newSession/openSession` 只重置前端 store，从不向后端 `unload_file`（`ui/src/state/session.ts:683-692,822-874`），而 `save_session` 收集源是后端 `list_frozen()`——"新建会话→保存"会把已从界面消失的旧文件重新写进 `.absession`，重开后"复活"；
4. **上传层**：P0-4 的副本残留。

### 主题 3：同步阻塞 IO 直接跑在 tokio worker 上（两个 Agent 独立命中）
- `ab-server/src/routes.rs:188-192`（64MB 同步写）、`plugin_manager.rs:211-278`（≤1GiB ZIP 同步解压）、`session_file.rs:202-213`（100MB×N 串行 SHA-256）、`pipeline_bridge.rs:1354-1388`（同步读头）、`update_fetcher.rs:283`（同步下载写盘）。
- 2C2G 部署机 tokio 默认 2 worker：两个并发插件安装/大会话装载即占满全部 worker，**所有**请求（含 `/health`）秒级停顿；看门狗 1s tick 也被推迟。修复：统一包 `tokio::task::spawn_blocking`。

### 主题 4：锁策略与 panic 边界
- 编排层 `std::sync::RwLock/Mutex` 一律 `.unwrap()`（非测试代码 ~206 处 unwrap/expect，`pipeline_bridge.rs` 48 处）——任一持锁 panic 即毒化，之后同资源所有请求永久失败，只能重启；
- `ab-server` 无 CatchPanic 层（无 tower-http catch_panic 依赖），handler panic = 裸断连，违背契约 §4 错误包络；
- `ab-pipeline/src/store.rs:384-401`：freeze 排序（O(n)×3 临时向量）持**全局**写锁，一个百万点文件 freeze 阻塞所有文件的一切查询。修复：锁粒度降到 per-file + parking_lot（不毒化）。

### 主题 5：恢复/自愈路径"设计了一半，没接线"
- 插件崩溃或空闲回收后，管线层 `ensure_session` 复用死会话（`pipeline_bridge.rs:736-738` 的 `if let` 短路了 ab-host 的 `get_or_spawn` 复活路径），后续手选导入/reopen 直接 `plugin_crashed`，只能人肉 reload；
- `ab-host/src/health.rs:195` 的 retry_loop/CircuitBreaker 是零调用死代码；
- builtin 损坏的修复路径是死路（409 保护 + reload 不重扫）；卷一 §1.6 的运维通道正是解法；
- 协议版本协商名存实亡：`InitializeResult` 无 `protocol_version` 回显（`ab-protocol/src/types.rs:31-40`），两个 SDK 的常量无人读取，"防漂移测试"断言的是硬编码字面量 1 而非正本（`sdk/python/tests/test_protocol_version.py:19`、`sdk/dotnet/tests/ProtocolVersionTests.cs:22`）。

### 主题 6：SDK/实现的"多份拷贝"漂移
- Python SDK 三份实现：`sdk/python/`（正本）、`plugins/demo-tool/analysisbuddy/`（内嵌拷贝，**已漂移 64 行**，缺 custom_query 支持，且 `sys.path[0]` 遮蔽 pip 正式版）、`pack_plugins.py --sdk-python`（打包时再注入第三份）；
- Rust 插件无 Rust SDK：builtin-csv 与 aibench-llama 的 `main.rs` diff 仅 141 行（全套传输样板复制），`ndjson.rs` 逐字相同，mock-plugin/plugin-validator 是第三、四份手写帧循环；
- `ab-app/src` 有 ~4,587 行 M1 迁移死副本（`lib.rs:12-18` 只编 commands/webview2，其余不参与编译且已与 ab-engine 活体分叉）。

## §3.4 分模块缺陷明细

### ab-server / ab-protocol（A1）
| 级别 | 发现 | 证据 |
|---|---|---|
| P1 | 任意路径导入违背契约 §7.3（→P0-2） | `routes.rs:207-224` |
| P1 | 上传副本永不删除（→P0-4） | `routes.rs:280-284` |
| P2 | 终态 job 取消返回 404 而非契约的终态快照；in-flight 解析不会被真正取消（从不调已存在的 `cancel_parse`，`pipeline_bridge.rs:502`） | `jobs.rs:163-165,127-131` vs `http-api-v1.md:119` |
| P2 | `/imports/upload` 的 `overrides` 永远无法命中：键是服务端随机存储路径（`<pid>-<seq>-<nanos>`），客户端不可预知 → needs_user_choice 流程死路 | `routes.rs:171-175` + `jobs.rs:133-136` |
| P2 | `POST /plugins/{id}/reload` 未知 id 返回 500 而非 404；不取插件互斥锁，与 uninstall/update 并发可拉起指向已删目录的僵尸进程；reload 只做会话重建、从不 `registry.reload()` | `ab-engine/src/commands/plugin.rs:179-184,160-191` |
| P3 | 413 响应不符合 §4 错误包络（JSON 路径把 body-too-large 压平成 400） | `routes.rs:89,150-155` |
| P3 | 无 panic 恢复层 + 锁毒化级联（→主题 4） | `routes.rs:55-90` |
| P3 | hub forwarder 对 host→hub 段 Lagged 静默 `continue`，事件无感知丢失（含 plugin-health 终态） | `hub.rs:114` |
| P3 | `last_diagnostics`/`jobs` 无界增长（→主题 2） | `pipeline_bridge.rs:645-653` |
| P3 | 无 `GET /files`（会话文件清单）端点——目标模型下 Session 需要可靠枚举自己的文件 | `routes.rs:56-81` 路由表 |

正面核实：token 恒时比较正确、zip-slip/膨胀防护齐备（enclosed_name + 500MB/1GiB/2000 条限额）、session 路径 `..` 拒绝正确（仅 symlink 逃逸未防）、错误码映射与契约逐项一致、SSE hub→订阅者段背压契约达成、更新流 https-only+双限+白名单质量好、ab-protocol 序列化稳健（非有限数出站拦截、skip-if-empty、manifest 前向兼容）。

### ab-engine / ab-pipeline / ab-host（A2）
| 级别 | 发现 | 证据 |
|---|---|---|
| P1 | stderr 泵 `read_until` 无界读行：64KB 截断发生在整行读入**之后**，一行无 `\n` 的超长输出可 OOM 整个宿主（stdout 侧 FrameReader 已有正确范本） | `ab-host/src/session.rs:1259-1276` |
| P1 | 死会话复用/崩溃恢复未接线（→主题 5） | `pipeline_bridge.rs:736-738` |
| P1 | 导入失败/取消不调插件 `unload_file`（→主题 2 第一环） | `pipeline_bridge.rs:1058` 等 4 处 |
| P2 | ZIP 安装在 Linux 丢失可执行位（未读 `entry.unix_mode()`）→ 装得上的 Rust 插件永远拉不起来，误报 `plugin_crashed` | `plugin_manager.rs:268-269` |
| P2 | 全局 spawn 锁 + `probe_plugin` 对 `get_or_spawn` 无超时：一个 initialize 挂死的插件独占全局锁 5s，N 个此类插件使每次导入匹配阶段 N×5s | `session.rs:924,1084` + `pipeline_bridge.rs:1423` |
| P2 | Store 全局 RwLock，freeze/append 持写锁做 O(n) 工作（→主题 4） | `store.rs:384-401,242-275` |
| P2 | 入站 `Record.value` 不校验有限性：插件注入 `1e999`→∞ 后 LTTB 静默失效；`confidence` 无 [0,1] 夹逼，NaN 使 needs_user_choice 判断恒 false | `session.rs:852` vs `types.rs:363-374`；`pipeline_bridge.rs:1454` |
| P2 | 同步阻塞 IO×4 处（→主题 3） | 见主题 3 |
| P2 | 锁毒化（→主题 4） | `pipeline_bridge.rs` 48 处 |
| P2 | 通知扇出"满则丢新"但回压诊断口径脱节：`NotificationFan::dropped_count()` 无消费者，丢批被 freeze 误判为 `count_mismatch` 而非 `host_backpressure` | `rpc.rs:194-207` + `host_bridge.rs:97-101` + `pipeline_bridge.rs:1112-1123` |
| P3 | `update_plugin` 固定写 portable 目录，UserData 源插件更新后双副本；覆盖搬入存在"旧已删新未落"窗口 | `plugin_manager.rs:764,409-417` |
| P3 | 事件通道 unbounded + 退出时孤儿清理 best-effort | `pipeline_bridge.rs:271`、`session.rs:1123-1127` |

正面核实：stdout 帧层（长度先验+8MB 上限）、优雅退出 3s 预算时序、查询闭区间双二分、LTTB、UTC 毫秒时间戳、状态机查表转移、Windows 专项（CREATE_NO_WINDOW/verbatim 路径/MoveFileExW）均正确。

### ab-app + 桌面 UI（A3）
| 级别 | 发现 | 证据 |
|---|---|---|
| P1 | UI 会话重置与 engine store 永久漂移：newSession/openSession/换插件/同路径重导入均不卸载旧 file_id，保存时旧文件"复活"（→主题 2；清理契约要求此环闭合） | `ui/src/state/session.ts:683-692,822-874,213-224` + `engine/commands/session.rs:88-91,176-192` |
| P1 | ECharts 实例从不随容器 resize：全仓无 `chart.resize()`/ResizeObserver——窗口最大化、拖拽/折叠侧栏后核心画布空白/裁切 | `ui/src/components/TimelineChart.tsx:119-144`、`AppShell.tsx:129-157` |
| P2 | ~4,587 行 M1 迁移死代码且已与活体分叉 | `ab-app/src/lib.rs:12-18` |
| P2 | dev/prod IPC 开关短路：`VITE_AB_IPC=real` 永远无效，`cargo tauri dev` 恒 mock——IPC 契约回归开发期不可发现 | `ui/src/ipc/ipc.ts:60-63` |
| P2 | load_session 串行重放阻塞数秒 + 前端无 in-flight 守卫/loading 态：晚到的会话装载整体覆盖新操作 | `engine/commands/session.rs:88-91` + `session.ts:822-874` |
| P2 | query_series 逐点 JSON 对象 + 前端每次 setOption 全量 `map()` 重建，无列式/typed-array 通道 | `engine/commands/query.rs:53-57` + `ui/src/chart/options.ts:353` |
| P3 | `KeyValueEntry.value` 两侧类型契约不一致（Rust `serde_json::Value` vs TS 标量联合） | `ab-protocol/src/types.rs:212-221` vs `ui/src/ipc/types.ts:201` |
| P3 | 无菜单/全局快捷键；`tauri.conf.json` 无 CSP | `tauri.conf.json` |
| P3 | `session.ts` 915 行上帝模块（六类职责混载） | `ui/src/state/session.ts` |

正面核实：TS 纪律优秀（strict 全开、`as any` 0 处）；后端超时矩阵完善；无撤销/多标签系单会话模型有意裁剪。

### WebUI + 认证网关（A4）
| 级别 | 发现 | 证据 |
|---|---|---|
| P0 | 管理 API 无角色校验（→P0-1） | `auth-gateway.js:1054-1063` |
| P1 | 容量检查异步间隙可并发突破 AB_MAX_TENANTS | `auth-gateway.js:467,472,487` |
| P1 | Session 身份客户端可控、登出不回收、跨用户残留（→§1.9） | `auth-gateway.js:288-303,994-1002,74` |
| P1 | 匿名路径导入任意读（→P0-2） | `auth-gateway.js:1049-1069` + `FilePanel.tsx:116-137` |
| P2 | 全链路明文 HTTP + Cookie 无 Secure（优先级提至 M1） | `auth-gateway.js:274,995` + `deploy/nginx-...conf:19` |
| P2 | `parseCookies` 无容错：单个坏 cookie 使该客户端全部 /api 请求 500 | `auth-gateway.js:93` |
| P2 | DQA 上游无超时 + revalidate fail-open：故障期已注销/降权会话保持全部权限 | `auth-gateway.js:139-148,213-215` |
| P2 | nginx add_header 继承陷阱：`/assets/` 与 `= /index.html` 丢失全部安全响应头 | `deploy/nginx-...conf:29-31,61-69` |
| P2 | deploy 脚本 nginx 校验失败的"回滚"是删除配置而非恢复备份——下次 reload 全站下线 | `deploy/deploy-webui.sh:85-87,100-104` |
| P3 | 上游代理无请求级超时 | `auth-gateway.js:522-523,1089` |
| P3 | SSE 断开后前端不重连，导入进度永久停更 | `src/api/client.ts:163-176` |
| P3 | systemd 硬化不完整（缺 CapabilityBoundingSet/MemoryMax 等，该单元会执行插件提供的可执行文件） | `server/ab-auth-gateway.service:40-47` |

正面核实：fd 泄漏与端口竞态两处既有修复确认在位；请求体流式转发、hop-by-hop 过滤、断连 destroy 均正确；前端无 dangerouslySetInnerHTML/localStorage，XSS 面干净。

### 插件 / SDK / 仓库治理（A5）
| 级别 | 发现 | 证据 |
|---|---|---|
| P0 | builtin 双源真相 / entry 硬编码 .exe / 安装无冒烟（→P0-3） | `build.rs:18-52`、`plugin.json:6`、`ci.yml:174-181`、`plugin_manager.rs:8-12` |
| P1 | 协议版本协商名存实亡（→主题 5） | `types.rs:31-40` + 两 SDK 字面量断言 |
| P1 | Python SDK 三份实现已漂移：demo-tool 内嵌拷贝缺 64 行且遮蔽 pip 正式版 | `plugins/demo-tool/analysisbuddy/` vs `sdk/python/` |
| P1 | Rust 插件无 Rust SDK，传输样板三抄 | 三个 main.rs/ndjson.rs diff 实测 |
| P1 | builtin 损坏修复路径死路（→主题 5；卷一 §1.6 为解） | `plugin_manager.rs:373-379` + `discovery.rs:131-139` |
| P2 | 插件"独立 workspace 根"治理税：5 份 Cargo.lock、`version.workspace=true` 部分树不可解析、**CI 从不跑插件测试** | 各 Cargo.toml + `ci.yml:62,165` |
| P2 | 更新流锁定 GitHub-only 与真实分发渠道（私有 Gitea+手工 ZIP）矛盾；与演进规格"绝不下载安装"决策冲突 | `manifest.rs:187-206` + `docs/architecture/2026-08-28-platform-evolution-spec.md` §2 |
| P2 | 版本纪律缺失：全生态恒 0.1.0、零 CHANGELOG、crate 版本充当产品版本、preset 引用失效无降级 | 各 manifest/pyproject/csproj |
| P2 | 仓库卫生：scratch/ 入库、中文命名根文档、契约正本存于另仓（devdocs）、dotnet publish 产物与 __pycache__ 已提交、ab-app 核心源文件 untracked、docs 历史报告无过时标注 | `git ls-files`/`git status` 实测 |

## §3.5 UI/UX 风格一致性（裁定：统一参考 DQA_Unified_Database，规格见卷四）

**背景**：token 层本就逐字对齐 DQA 视觉规范 v1.0；壳层与组件层存在系统性漂移。DQA 侧 `docs/rd-test-data-platform-20260922-ab-integration-and-launch-plan.md` §1.4 亦背书"视觉对齐 = tokens + chartTheme"。

| 级别 | 发现 | 证据 |
|---|---|---|
| P2 | 登录页大面积 `radial-gradient` 背景，违反 DQA 规范 §4"不使用大面积渐变" | `AnalysisBuddy_WebUI/src/styles/app.css:156-159` |
| P2 | modal 遮罩裸 `oklch(24% 0.02 250 / 0.32)` 字面量，违反 `ui.css` 自身头约"不出现裸 oklch/hex" | `AnalysisBuddy_WebUI/src/styles/ui.css:248` |
| P2 | 弹层宽度自成一档（460/720px），违反规范 §8"弹层只有 560/860 两档"（+确认层 400px） | `ui.css:252,262` |
| P2 | Toast 整条换底色表达类型，DQA 为深底+彩色圆点模式；定位/宽度/字号均不同 | `ui.css:228-243` vs DQA `components.css:201-224` |
| P2 | 表头大写+窄内边距、panel 头 surface-2 底、按钮固定高+过渡、tag 药丸形、输入 focus outline 环——与 DQA 登记样式成套分歧 | `ui.css` 全文 vs DQA `components.css` |
| P3 | topbar 52px/22px/blur vs DQA 72px/34px/无 blur；主区 padding 与 page-head 字号口径不同 | `app.css:14-27,79-103` vs DQA `shell.css:22-47` |
| P3 | PluginsPage 未按规范 §8"管理台骨架"（一页一卡、左列表/右详情、限高内滚、弹层两档）组织 | `src/pages/PluginsPage.tsx`（666 行单卡流式布局） |

正面核实：13 个原始 token 与派生变量同值；单列壳层与 DQA 2026-09-10 rail 隐藏裁定一致；ConfirmDialog 的 dialog 语义（role/aria-modal/Esc/焦点圈闭/条件渲染）已满足规范 §8；`.ab-mono` 等宽约定、视觉性格（§3）无违规；低置信度"降级读取"橙色语义符合 design-baseline §4.1。

## §3.6 拓展章节（v1 审计补全）

### 3.6.1 可观测性
现状：ab-server 无 panic 恢复层；hub 对 host→hub 段 Lagged 静默丢弃（`hub.rs:114`）；`NotificationFan::dropped_count()` 无消费者——三处"静默劣化"。网关唯一观测端点 `GET /_tenants`；插件 stderr 环形缓冲。全系统**未发现任何 metrics/tracing/correlation-id 设施**；排障依赖 `.abwebui-ops/probe_*.py` 家族（50+ 个脚本，正因缺内建观测而膨胀）。缺口（WS-H）：请求级 correlation id（网关→实例→插件三层穿透）、计数器/直方图、三处静默劣化点补显式信号、`/_stats` 端点。最低目标：结构化 JSON 日志 + correlation id + 计数器，不引入重型 APM。

### 3.6.2 测试与 CI
CI（`ci.yml`）：根 workspace 有测试；**插件只有 `cargo build --release`，无任何 workflow 跑插件 `cargo test`**；aibench-llama 在 feature 分支无 CI 足迹；WebUI playwright 测试存在但无网关级集成 CI；桌面 e2e 是 scratch/ 的 PowerShell 一次性脚本。历史教训（AGENTS 原文）："**长稳是唯一能发现端口竞态的手段**"——但 soak 脚本在 `.abwebui-ops/` 不入库、无 CI 触发。SDK 防漂移测试断言字面量（无效，主题 5）。缺口（WS-H）：插件测试进 CI、网关测试进 CI、soak/chaos 入库参数化、契约驱动 API 回归。

### 3.6.3 性能与容量
已知基准（实测）：实例冷启动 32ms；空闲 6.9MB；35min/1627 轮导入卸载 0 失败（24 实例合计 ~420MB 平台期）；单文件 100MB/请求体 64MB 上限；前端 4000 点/series + 150ms 防抖全量往返。**未定义**：查询 P50/P99 预算、导入吞吐目标、freeze 期间查询延迟上限、并发会话 SLO、2C2G 机最大可用会话数正式口径。结构性风险：主题 3/4 使长尾与导入强耦合——修复前任何延迟预算不可承诺（M3 验收含"建立基准→修复→复测"）。

### 3.6.4 部署拓扑与双主机差异

| | 192.168.1.171 | 43.142.81.160 |
|---|---|---|
| 角色 | 开发/验证主机 | **生产 DQA 主机**（dqa-api+PG17+:80 共存） |
| 容量 | 24×256MB=6GB / 15.4GB | 4×128MB / 2GB+4G swap |
| 入口 | nginx:8601 同源反代 | 8601 **公网不通**（安全组未放行），需隧道 |
| 构建 | — | 机上构建，rustup 走 **rsproxy**（官方源不可达） |

风险：①160 是生产机，与 DQA 抢 2C/2G——列为人工批准红线；②两机容量参数/构建源差异无配置化管理；③`analysisbuddy.service`（:8600）虽停用但 unit 文件仍在，存在误启复活风险；④8601 公网不通是"隐式安全"，若放行则 P0-2 立即暴露公网——**安全项必须先于任何端口放行**。

### 3.6.5 数据与预设生命周期
现状：服务端 presets 存于租户数据目录（TTL 7 天，`auth-gateway.js:74`）——与清理契约**直接冲突**；桌面 `.absession` 无版本字段/迁移；preset 引用失效无降级。裁定：卷一 §1.7。遗留决策 D-1：现网用户服务端预设是否提供一次性导出（建议提供，切换前公告）。

### 3.6.6 发布、回滚与风险登记
发布管线缺口：release.yml 的 server 包不含插件制品（→P0-3）；builtin_ids.rs 陈旧副本在 git 中；deploy-webui.sh 回滚是删除配置；无 CHANGELOG/版本纪律。回滚能力：网关/ab-server 均无版本化交付物。风险登记 Top5：R-1 生产机资源争抢；R-2 安全组放行时间差；R-3 契约正本在 devdocs 仓、主仓是手工同步副本（改契约极易改错仓）；R-4 ab-app 核心 untracked 源文件（提交树不完整，clone 即坏）；R-5 双 UI（桌面/在线）契约共享但无共享测试资产，行为漂移不可见。

## §3.7 审计覆盖度声明

各 Agent 报告了"查过未见问题"的方向（token 恒时比较、zip-slip、错误码映射、SSE 背压、LTTB/时间戳/状态机、Windows 专项、TS 类型纪律、网关流式转发等），已在 §3.4/§3.5 各模块末尾列出；本文只收录有代码证据的缺陷。AGENTS.md 中的已知问题全部得到确认，且多数找到了更深一层根因（如"17 实例累积"的完整四环链条、"Linux 容忍 .exe"实为 CI 复制 ELF 的 hack）。

---

# 卷四 WebUI UI/UX 统一规格（WS-J 实现依据）

> 权威来源优先级：`DQA_Unified_Database/docs/dqa-visual-language-spec.md`（视觉规范 v1.0，含 2026-09-10 rail 隐藏裁定）> DQA 生产登记样式（`web-frontend/assets/css/{shell,components,auth}.css`、`web-frontend/src/shared/theme/tokens.css`）> 本卷映射表。冲突时以 DQA 登记值为准。

## §4.1 已对齐项（不要动）

- **13 个原始色彩 token + 全部派生变量**：`src/styles/tokens.css` 与 DQA 同值（AB 额外的 `--hex-*` sRGB 近似块是为 ECharts 服务的扩展，保留）；
- **单列壳层方向**：已按 2026-09-10 裁定实现"顶部工具栏 + 中央工作区、无 rail"，`content` max-width 1660px 与 DQA 一致；
- **ConfirmDialog 语义**：`role="dialog"` + `aria-modal` + Esc + 焦点圈闭 + 遮罩点击关闭 + 条件渲染，已满足规范 §8；
- **视觉性格**（规范 §3）：无 Hero、无玻璃拟态、无渐变文字、表格开放阅读面、等宽机器字符串（`.ab-mono`）；
- **图表主题方向**：chartTheme 引用 token（J6 做核对，不是重做）。

## §4.2 硬性违规（必须修）

| # | 现状 | 违反 | 修复 |
|---|---|---|---|
| V1 | 登录页 `radial-gradient(1100px 480px …)` 大面积渐变背景（`app.css:156-159`） | 规范 §4"不使用大面积渐变" | 改平底 `var(--bg)` 舞台，按 DQA `auth.css` 重排（§4.3.4） |
| V2 | `ui.css:248` modal 遮罩 `oklch(24% 0.02 250 / 0.32)` 裸色值 | 本文件头部自约"不出现裸 oklch/hex"；DQA confirm 用 `var(--ink)` 派生 | 遮罩改 `background: var(--ink); opacity: .38;` |
| V3 | 弹层宽度自成一档：modal 460px / wide 720px | 规范 §8"弹层尺寸只有两档：560px、860px"（+确认层 400px） | 全站只允许 400/560/860 三个宽度 |

## §4.3 逐组件映射表（现状 → 目标；目标值 = DQA 登记值）

### 4.3.1 壳层（app.css ↔ shell.css）

| 项 | AB 现状 | 目标（DQA） |
|---|---|---|
| topbar 高度/内边距 | `min-height: 52px; padding: 8px 22px; flex-wrap` | `height: 72px; padding: 0 34px;` 不换行；≤1180px `padding: 0 24px` |
| topbar 材质 | `backdrop-filter: blur(6px)` | 去掉 blur（DQA topbar 为 0.92 半透明无模糊） |
| 顶栏最左 | 品牌块（mark+双行文字）+ 面包屑 | 保留品牌 mark 作为**首页入口**（等价 DQA「⌂ 首页」），面包屑紧随；品牌双行文字 ≤760px 隐藏 |
| 主区 padding | `20px 22px 44px` | `34px 36px 60px`；≤1180px 左右 24px；≤820px `14px 12px 32px` |
| page-head | `h1 22px/680; p 12px; mb 16px` | `h1 clamp(27px, 3vw, 38px)/680/-0.04em`（规范 §5"大标题仅确有必要页面用"——工作台走面包屑为主，page-head 只在 Plugins/About 用）；`p 13px/1.55; mb 26px`；可加 `.eyebrow`（11px/750/blue/大写字距 .13em） |

### 4.3.2 按钮（ui.css ↔ components.css）

| 项 | AB 现状 | 目标 |
|---|---|---|
| 形状 | 固定高 `height:32px; padding:0 12px; radius var(--radius-sm)` | `padding: 9px 13px; border-radius: 8px`（不固定高）；sm 档 `5px 9px / 10px` |
| 字重/过渡 | 600 + `transition .12s` | 650，去过渡 |
| disabled | `opacity:.5` | 四件套：`opacity .45 + surface-2 底 + muted 字 + line 边框`，primary 同；补全局 `button:disabled{cursor:not-allowed}`；**禁用原因走 `title`** |
| danger | 红字 + line 边框 | `border-color var(--red); background transparent; color var(--red)`，hover `red-soft` 底 |

### 4.3.3 标签/徽标

| 项 | AB 现状 | 目标 |
|---|---|---|
| 形状 | 药丸 `999px; height:20px; 透明边框` | 矩形 `radius 5px; padding 4px 7px; 10px/600`，无边框 |
| 蓝档文字色 | `--callout-ink` | `--blue` |
| 语义沿用 | matched/parsing/ready/error、插件状态映射 | **不变**（只换皮）；低置信"降级读取"橙色语义保留 |

### 4.3.4 登录页（app.css .login ↔ auth.css）

- 舞台：删渐变，`min-height:100vh; flex column 居中; padding 34px 20px`，平底 `--bg`；
- 卡：宽 `min(440px, 100%)`，padding `30px 30px 26px`，底子= `.panel`；
- 品牌行：36px **方形** outline mark（`border: 1px solid var(--rail-brand-border)`，无圆角），strong 14px + span 10px/大写字距；标题行 `21px/680/-0.03em`，副文 11px/1.55；
- 字段：label 11px muted 上置（保留 `.field` 结构）；提交 `btn--primary btn-block`（全宽 `10px 13px`）；"返回工作台"为卡下方次要链接。

### 4.3.5 表格 / 面板 / 输入 / 空状态

| 项 | AB 现状 | 目标 |
|---|---|---|
| 表头 | `10.5px/650/大写+字距/sticky` | `11px/600/muted/surface-2`，**去大写**；sticky 保留（限高容器内合理增强） |
| 全宽页表格单元格 | `th 8px 12px; td 9px 12px` | `th 10px 19px; td 12px 19px`（Plugins 等整页表）；**工作台侧栏紧凑列表维持 12px 水平内边距**（密度按容器分级，在此登记） |
| 行交互 | `tbody tr:hover` | 补 `tr.data-row{cursor:pointer}` + hover/focus-visible 同款（可点行才加） |
| panel 头 | `surface-2 底; 10px 14px; h2 13px` | 透明底 `17px 19px; h2 14px`，副文 `p 11px muted`；panel 补 `box-shadow: var(--shadow)` |
| 输入框 | `radius-sm; focus=border+outline 2px` | `radius-xs; hover 边框 var(--btn-hover-border); focus=border var(--focus-border-blue) + box-shadow 0 0 0 3px var(--blue-soft)`（软光环） |
| 空状态 | `.empty` 一行式 | 结构化 `.empty-state`：title 12px/650 + sub 11px/1.5；**搜索/筛选无结果时保留搜索词**（mono 小 chip） |

### 4.3.6 Toast / 弹层 / 拖放区

- **Toast 改 DQA 模式**：深底 `--toast-bg` + 左侧 **7px 彩色圆点**（info 蓝/success 绿/warn 橙/error 红），**不再整条换底色**；`top 20px; right 24px; max-width 380px; 11px`；出入场过渡保留；类名 `toast--err` 等保留只改样式（测试零改动）。
- **弹层宽度归档**（V3）：常规 560px、宽 860px、确认层 400px；遮罩按 V2 改 token 派生；标题 `14px/680`。
- **拖放区对齐 `.up-drop`**：`1px dashed var(--btn-hover-border)` + `surface-2` 底 + hover/drag `border-color var(--blue)`；文案 `b 主行 + 11px hint` 保留。

### 4.3.7 AB 特有组件（保留功能，统一语气）

指标树（.tree）、关键值表（.kv）、进度条、status-dot、log-view：**保留**；统一项仅限——圆角引用既有 token、hover/选中用 `--row-hover-bg`/`--blue-soft`（已符合）、字号落在规范 §5 区间。不新造颜色。

### 4.3.8 PluginsPage 管理台骨架（规范 §8）

按"管理台骨架"重排：**一页一张卡**（列表卡满宽），卡内**左列表/右详情**（左=插件目录可筛选，右=当前插件详情，未选中显示空态说明、不自动选中首条）；两栏各自限高内部滚动、列表表头滚动常驻；"安装/更新"等动作触发弹层（560px 档），触发按钮放卡头右侧动作区；只读视角（viewer）文案与禁用态沿用现有逻辑。

## §4.4 实施约束与验收（J 任务 DoD 的一部分）

1. **CSS 优先、类名与 `data-testid` 不变**：除登录页与 PluginsPage 骨架外，改动应限于 `styles/*.css` 与 chartTheme；组件文件只动 `className` 串。J3/J5 同步更新对应测试。
2. 禁止新增裸 oklch/hex；完成时 `grep -nE 'oklch\(|#[0-9a-fA-F]{3,8}' src/styles/*.css` 应只剩 `--hex-*` 登记块。
3. 弹层宽度全站只允许 400/560/860 三值。
4. 动画只保留 toast 出入场与进度条宽度；其余 hover 无过渡。
5. 每项任务验收 = 与 DQA 对应登记文件并排目检（`vite build && vite preview` 截图）+ `tsc && vite build` + 既有 vitest 全绿。
6. 本卷未覆盖的新组件：先读 DQA 规范 §11 使用协议（定页面类别→复用规则→登记偏离），再动手。

---

# 附录：里程碑 × 审计发现映射

| 里程碑 | 吸收的发现 |
|---|---|
| M1 | P0-1、P0-2、A4#2/3/4/5/6/7、§1.9 全部重解读项 |
| M2 | P0-4、主题 2 全链（A1#10、A2#3、A3#1）、A1 明细（无 GET /files）、A1#4 |
| M3 | 主题 3/4/5、A2#1/2/5/6/7/8/9/10、A1#3/6/8/9、A3 P2(load_session/IPC/死代码) |
| M4 | P0-3 全部、A2#4、A5-P1-7、§3.6.6 发布管线缺口 |
| M5 | 主题 5(协商)、主题 6、A5-P1-4/5/6、A3 P2(列式通道) |
| M6 | §3.6.1 可观测性、§3.6.2 测试 CI、§3.6.6 风险登记、A5-P2(治理四项)、§3.5 UI 一致性（→WS-J/卷四） |
