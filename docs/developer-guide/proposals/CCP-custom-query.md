# 契约变更提案：可选方法 custom_query（供应商中立具名查询）addendum

> 依据 `contract-change-proposal-template.md` 成文。与勘误条目不同，本提案属
> **契约扩展（addendum）**：`contract-v1` 冻结正文保持逐字不动，仅追加一个
> 可选方法 `custom_query`（protocol-v1.md §2.11）与 `Capabilities` 可选位
> `custom_query`，完全复刻 `annotate`（§2.7）的可选能力先例。
> 载荷对宿主 opaque——本提案不引入任何厂商专有语义进宿主（供应商中立，
> PLAN.md §10）。

---

## 提案基本信息

- **提案编号**：CCP-custom-query
- **关联勘误条目**：无（非勘误；属冻结契约的 addendum 扩展）
- **提出人 / 路**：API 体系设计 Quest（服务化与引擎化）原始规格 Phase 2
- **提出日期**：2026-09-06
- **审批**：用户已批准（原 Quest 规格「Phase 2（CCP 提案）」章节 + 主代理
  评审通过，随 M3 批次落地）

## 1. 变更动机

需求：内部工具插件（厂商）需要暴露**具名读取**能力（如 `cpu_thermal` 总览、
会话诊断快照），载荷语义厂商自定义。既有通路皆不匹配：

- `key_values`（§2.6）语义固定为「T 时刻状态快照」，key 语义虽 plugin-defined
  但无法携带参数、无查询名命名空间；
- `presets`（§7.2.1 addendum）=「看什么」（指标选择），不能表达「算什么」；
- 把厂商逻辑写死在宿主/manifest 专有字段 → 违反供应商中立原则（否决项，
  见原始规格 Rejected Alternatives）。

方案：新增可选方法 `custom_query`——入参 `{file_id, query, params(object)}`、
result `{data(object)}`，宿主零解释 opaque 载荷；调用方只知 `file_id`
（`plugin_id` 由宿主解析）。

## 2. 影响路清单（四处同批 + 全链路触点）

| 受影响方 | 影响内容 | 同步动作 |
|----------|----------|----------|
| docs/spec 契约文件 | `protocol-v1.md` 新增 §2.11 + §2 概览表 + §2.1 Capabilities 行 + §4.2 `-32005` 示例 + §6 超时表行；`rpc-messages.schema.json` oneOf 追加 `CustomQueryRequest`（错误码 enum 不动） | 随审批同批修订 ✅ |
| ab-protocol 类型 | `Capabilities.custom_query`（serde default + skip-if-false）+ `CustomQueryParams`/`CustomQueryResult` | 随审批同批修订 ✅ |
| validator 规则 | 追加 **BEH-13**（无能力回 -32005、有能力回合法 object）；`behavior.rs` 能力字段表追加 | M3 批次同批 |
| SDK（D1/D2） | Python `KNOWN_METHODS`/`_handle_custom_query`/`on_custom_query` 默认 -32005；dotnet `RouteAsync` case/`PluginHandlerBase` 默认 -32005/`SupportsCustomQuery` 反射探测 | M3 批次同批 |
| ab-host | `PluginSession::custom_query`（annotate 分支模式）+ `health.rs` 超时表 10s 行 | M3 批次同批 |
| ab-pipeline | 宿主桥 trait 加方法，纯透传不触 store | M3 批次同批 |
| ab-engine | `HostSessionAdapter` 实现 + `custom_query_at` 命令 + `CapabilitiesDto` 真实化 | M3 批次同批 |
| ab-server | `POST /api/v1/files/{fid}/queries/{name}` + `GET /api/v1/files/{fid}/vendor-queries` + http-api-v1.md | M3 批次同批 |
| mock-plugin / e2e | 方法实现 + 能力位开关 + validate_result 分支 + 剧本动作 + mock/real 套件用例 | M3 批次同批 |
| 开发者指南 | `02-write-a-plugin.md` / `04-manifest-reference.md` 厂商具名查询指引（Phase 1 一并） | M3 批次同批 |

## 3. 兼容性论证

- **前向兼容**：旧插件 initialize 结果缺 `custom_query` 键 → serde
  `default = false`，反序列化不受影响；旧帧在新 Schema 下仍通过（oneOf 只
  增不加约束，错误码 enum 不动）。
- **后向兼容**：新帧（CustomQueryRequest）在旧 Schema 下被拒——预期行为，
  旧宿主不会发出该方法（能力位拦截先行）。旧插件收到 `custom_query` 调用回
  `-32601`，宿主与 `-32005` 归一为 unsupported（HTTP 422），**不得落
  internal**。
- **规则影响**：validator 规则只追加 BEH-13，既有 BEH-01~12 编号/级别不变
  （规则 ID 冻结纪律）。
- **协议版本**：可加性扩展，`PROTOCOL_VERSION` 保持 `1`。
- **回归**：ab-protocol serde 快照测试（旧 initialize 帧回程逐字还原）✅；
  全工作区 cargo test 在 M3 批次收尾时全绿。

## 4. 回滚方式

git revert 本批次 commit（契约/类型/宿主/引擎/服务器/SDK/mock 各层同批
revert 或按层逐个 revert——层间仅单向依赖：schema→types→host→engine→server，
SDK/mock/e2e 独立可单独回退）。回滚后 http-api-v1.md 与开发者指南对应章节
随 revert 同步删除。

## 5. 审批记录

| 审批人 | 结论（通过/驳回/需修改） | 日期 | 备注 |
|--------|--------------------------|------|------|
| 主代理 | 通过 | 2026-09-06 | 原 Quest 规格 Phase 2 定义 + 用户指令「直接开始执行」 |
| 受影响路代表 | —— | —— | 并行批次内各路自验（host/engine/server/SDK/mock/e2e 全绿） |
