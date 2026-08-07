# 03 · 协议逐方法走读

> 本文件是 [docs/spec/protocol-v1.md](../spec/protocol-v1.md) 的中文导读，**不是规范本身**。
> 一切字段、错误码、超时、字节上限以协议正本为准；本文不复述数值，需要时标注
> 「以协议正本为准」或链接对应章节。

## 0. 总体形态

插件 = 一个子进程 + 一份 `plugin.json`。宿主是 JSON-RPC 2.0 **客户端**，插件是
**服务端**，通过 stdin/stdout 通信（stdout 只准有协议帧，日志全走 stderr，
[§1.1](../spec/protocol-v1.md#11-transport-channels)）。帧格式为 NDJSON：一行一条
完整 JSON 消息，UTF-8 无 BOM，行尾 `\n`（[§1.2](../spec/protocol-v1.md#12-frame-format-ndjson)）。

三种帧：

| 帧种类 | 特征 | 例子 |
|--------|------|------|
| 请求 | 带 `id` + `method` | 宿主 → 插件的所有方法调用 |
| 响应 | 带 `id`（回显请求 id）+ `result` 或 `error` | 插件对每个请求的应答 |
| 通知 | 无 `id` + `method` | 插件主动推送 `progress` / `RecordBatch` |

`id` 由宿主生成（单调递增整数），插件不得自造 id；插件靠 `id` 关联请求与响应
（[§1.4](../spec/protocol-v1.md#14-concurrency-and-ordering)）。

## 1. initialize —— 握手

生命周期第一帧。宿主传 `protocol_version` 与 `host_info`，插件回插件元数据：
`id`（必须等于 manifest `id`）/ `name` / `version` / `capabilities`
（`annotate`/`subscribe`/`binary_sidecar` 三个布尔）。能力声明的唯一事实来源是
这里——manifest 不声明能力（[§2.1](../spec/protocol-v1.md#21-initialize)）。

## 2. schema —— 指标声明

无参数。返回 `metrics` 数组：每个 `MetricDef` 至少含 `id`/`name`/`aggregation`
（`last`/`sum`/`avg`/`min`/`max` 之一），`unit`/`description` 可选。约束：
`parse` 产出的每条 `Record.metric` 必须在此声明过，否则宿主丢弃该记录并计数警告
（[§2.5](../spec/protocol-v1.md#25-schema)）。

## 3. can_handle —— 文件认领探测

宿主把候选文件的元信息（路径、文件名、扩展名、大小、头部采样）传进来，插件回
`can_handle` 布尔 + `confidence` 置信度（闭区间内）+ 可选 `reason`。多插件认领时
宿主取置信度最高者（[§2.2](../spec/protocol-v1.md#22-can_handle)）。

## 4. load_file → parse → RecordBatch/progress —— 主数据流

1. `load_file`：宿主给 `file_id`（UUID）与绝对路径，插件读入并驻留原始数据，回
   文件级摘要（可含记录数预估与时间范围）。
2. `parse`：插件开始流式解析，数据**不走响应体**，而是通过两类通知回传：
   - `RecordBatch`：按批回传归一化记录（`seq` 从 0 单调递增、`done` 标记末批）；
   - `progress`：进度与心跳（解析期间必须周期性发送，间隔以协议正本为准，
     [§3.3](../spec/protocol-v1.md#33-progress-notification)）。
3. `parse` 的最终响应只回 `records_total`，宿主会把它与各批 `records.length`
   之和核对（[§3.2](../spec/protocol-v1.md#32-recordbatch-notification)）。

`Record` 三必填：`timestamp`（UTC 毫秒整数）/ `metric`（∈ schema）/ `value`
（有限数值）。可选字段 `level`/`tags`/`raw_line` 遵循 skip-if-empty 约定：
为空时整体省略该键，禁止 `null` 或空容器（[§3.1](../spec/protocol-v1.md#31-record-structure)）。

## 5. key_values —— 时刻快照

宿主传 `file_id` + 时刻 T（UTC 毫秒），插件回 `entries` 数组（`key` + 标量
`value` + 可选 `unit`），语义通常是「≤T 的最新状态」（[§2.6](../spec/protocol-v1.md#26-key_values)）。

## 6. annotate —— 可选能力

仅当 `capabilities.annotate == true` 时宿主才会调用；否则宿主应自行拦截，收到
调用时插件回 `-32005 unsupported_in_v1`（[§2.7](../spec/protocol-v1.md#27-annotate-optional-capability)）。

## 7. unload_file / shutdown / cancel_parse —— 收尾三件套

- `unload_file`：幂等卸载，释放内存；对未加载的 `file_id` 同样回成功。
- `shutdown`：收到后应答 `{}` 并退出进程（退出码 0，时限以协议正本为准，
  [§2.9](../spec/protocol-v1.md#29-shutdown)）。
- `cancel_parse`：取消在途 parse（幂等）；被取消的 parse 请求必须回
  `-32004 cancelled`，不得回成功（[§3.4](../spec/protocol-v1.md#34-cancellation-semantics)）。

## 8. 错误码速查

标准码沿用 JSON-RPC 2.0（`-32700` 解析错 / `-32600` 非法请求 / `-32601` 方法不存在 /
`-32602` 参数非法 / `-32603` 内部错误）；协议自定义码固定五枚 `-32001` ~ `-32005`
（busy / file_load_failed / parse_failed / cancelled / unsupported_in_v1）。
插件严禁使用集合外的自定义 code（[§4](../spec/protocol-v1.md#4-error-codes)）。
注意：对**必选方法**回 `-32601` 视为不合规（BEH-03）。

## 9. 生命周期与故障语义

- 状态机：Discovered → Spawning → Initializing → Ready →（Loading/Parsing）→
  Draining → Shutdown；异常路径进入 Crashed/Timeout（吸收态）
  （[§5.1](../spec/protocol-v1.md#51-state-diagram)）。
- 失败重试：同一任务自动重试次数与退避间隔以协议正本为准
  （[§5.2](../spec/protocol-v1.md#52-host-retry-policy)）。
- 宿主对每类超时的处理各不相同（有的 kill、有的降级、有的弃权），见
  [§6 超时表](../spec/protocol-v1.md#6-timeout-table)（数值以协议正本为准）。

## 10. 插件侧的强制义务（§9 清单）

1. stdout 只输出协议帧；整行原子写，绝不吐半行 JSON；
2. 崩溃时 stderr 记日志再退出；
3. `load_file`/`parse` 必须可反复调用（幂等重入）；
4. stderr 日志建议带 `INFO/WARN/ERROR` 前缀；批量数据不准写 stderr；
5. **stdin EOF → 自行退出（退出码 0）**，禁止孤儿进程。

完整对照见 [§9](../spec/protocol-v1.md#9-fault-tolerance-summary)。

---

📌 章节要点（双视角）

👤 **给人**：读协议的正手顺序是「握手 → 主数据流 → 收尾」三段式：initialize 与
shutdown 之间的全部方法都在一个请求-响应循环里，记住「数据走通知、响应只报数」
就不会混淆 parse 的形态。

🤖 **给 Agent**：实现协议时以 [protocol-v1.md §3.5 完整示例](../spec/protocol-v1.md#35-complete-message-examples)
为帧形状基线（机器可校验副本在 `docs/spec/examples/frame-ok-*.json`）；
逐帧跑 `plugin check --behavior` 自检，规则映射见 `02-write-a-plugin.md` 第 3 步。
