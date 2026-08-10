# 06 · Python SDK 教程与 API 摘要（analysisbuddy-sdk）

> 契约依据：`AnalysisBuddy-devdocs/deep-dive/sdk-plugins.md` §1（SDK 设计）；
> 协议行为以 [docs/spec/protocol-v1.md](../spec/protocol-v1.md) 为准。
> 本文件只摘要 API 签名与关键行为，方法语义见 `03-protocol-walkthrough.md`。

## 安装与形态

```powershell
# 仓库内开发/内网使用（包名 analysisbuddy-sdk，源码在 sdk/python）：
pip install -e sdk\python

# 公开发布渠道可用时等价于：
pip install analysisbuddy-sdk
```

- **零第三方依赖**（纯 stdlib，`dependencies = []`）；要求 Python ≥ 3.10
  （`pyproject.toml` 声明 `requires-python = ">=3.10"`，CI 覆盖 3.10~3.14）；
- 包结构：`analysisbuddy`（`plugin.py` 主循环 / `context.py` 发送器与心跳 /
  `errors.py` 异常映射 / `transport.py` NDJSON 行读写）；
- 仓库内合规样例：`sdk/python/examples/sample-plugin`（本目录即插件仓库根，
  复制后改 `plugin.json` 的 id/display_name 即可作为新插件起步）；
  完整实战例子另见 `plugins/demo-tool`（含 annotate 与 key_values 索引）。

## 核心类：`AnalysisBuddyPlugin`

注册式 handler：子类覆写方法即完成协议实现。

```python
class AnalysisBuddyPlugin:
    id: str; name: str; version: str      # 元数据（= initialize 响应素材）

    def on_initialize(self, params: dict) -> dict: ...
    def on_can_handle(self, params: dict) -> dict: ...
    def on_load_file(self, params: dict) -> dict: ...
    def on_parse(self, file_id: str, options: dict | None, ctx: EmitContext) -> int: ...
    def on_schema(self) -> dict: ...
    def on_key_values(self, file_id: str, timestamp_ms: int) -> dict: ...
    def on_annotate(self, file_id: str, range: dict) -> dict: ...   # 可选能力
    def on_unload_file(self, file_id: str) -> None: ...
```

| 方法 | 返回 | 备注 |
|------|------|------|
| `on_initialize` | `{"id", "name", "version", "capabilities": {...}}` | 默认实现返回三字段元数据；`annotate` 能力 = 是否覆写了 `on_annotate`（SDK 自动探测，覆写即 true） |
| `on_can_handle` | `{"can_handle": bool, "confidence": float, "reason": str?}` | 弃权返回 `can_handle: false` |
| `on_load_file` | `FileSummary` 或 `{}` | 抛 `FileLoadFailedError` → SDK 自动映射 `-32002` |
| `on_parse` | `records_total`（int） | 数据通过 `ctx.emit_records()` 回传；同 `file_id` 并发第二次 parse 由 SDK 拦截并回 `-32001`，不会进到本方法 |
| `on_schema` | `{"metrics": [MetricDef...]}` | `MetricDef` 至少含 `id`/`name`/`aggregation` |
| `on_key_values` | `{"entries": [KeyValueEntry...]}` | 语义由插件自定义 |
| `on_annotate` | `{"events": [...]}` | 未覆写时 SDK 对 annotate 请求自动回 `-32005` |
| `on_unload_file` | 无 | 默认无操作；幂等由 SDK 保证 |

装饰器等价写法（与子类覆写等价，择一即可）：

```python
plugin = AnalysisBuddyPlugin(id="demo-tool", name="Demo Tool", version="0.1.0")

@plugin.handler("can_handle")
def can_handle(params): ...
```

## 发送器与心跳：`EmitContext`

`on_parse` 的第三个参数，流式解析期间回传数据：

```python
class EmitContext:
    def emit_records(self, records: list[dict]) -> None: ...
    def progress(self, percent: float | None = None, bytes_read: int | None = None) -> None: ...
    def check_cancelled(self) -> None: ...
```

- **批量**：记录进入内部缓冲，凑够 `batch_size` 自动 flush 成一个 `RecordBatch`
  通知（`seq` 从 0 自增，SDK 维护）；`batch_size` 缺省 4000、合法区间 1000~8000
  （构造时校验，越界抛 `ValueError`，与协议批量建议区间一致）；
  单批序列化体积接近 900 KB（协议建议单行 ≤ 1 MB）时 SDK 自动提前 flush；
- **心跳**：serve 主循环内置守护计时器，解析期间距上次发送达到协议心跳间隔就
  自动补发一条 `progress`（`records_so_far` 取当前累计值）——作者无需手动维护；
- **末批**：`on_parse` 返回后 SDK 自动 flush 残余缓冲 + `done:true` 末批，随后才发
  最终响应 `{"records_total": N}`；
- **序列化**：`Record` 可选字段为空即省略键，禁止 `null`/空容器（skip-if-empty，
  protocol-v1.md §3.1）；`value` 为 NaN/±Infinity 时 SDK 直接丢弃该记录并 stderr
  计数告警。

## 主循环：`plugin.serve()`

```python
def serve(self, stdin=None, stdout=None, stderr=None) -> None:
    # stdin/stdout 缺省 sys.stdin.buffer / sys.stdout.buffer（协议流量）；
    # stderr 缺省 sys.stderr（文本日志流）
```

关键行为（与协议 §1/§9 对齐）：

1. stdin 逐行读，行长度先于内容校验，超上限 → stderr 记日志后退出；帧尾 `\r`
   视为协议错，stderr 记录后退出；
2. stdout 整行原子写出（缓冲后一次 write + `\n` + flush），杜绝半行 JSON；
3. stderr 留给日志：`plugin.log(level, msg)` 输出 `INFO|demo-tool|...` 形态；
   **插件代码禁止 `print` 不带 `file=sys.stderr`**；
4. **stdin EOF → 自行退出（退出码 0）**；
5. 并发：读线程收请求分发，parse 在执行线程运行期间 `key_values`/`schema`/
   `cancel_parse` 等仍即时应答；
6. method 路由共 10 个（initialize/can_handle/load_file/parse/schema/key_values/
   annotate/unload_file/shutdown/cancel_parse）；未知方法回 `-32601`、结构非法
   请求回 `-32600`、参数非法回 `-32602`；
7. `shutdown` / `cancel_parse` / 幂等 `load_file` 重入由 SDK 自动处理。

## 异常 → 错误码映射（`analysisbuddy.errors`）

| SDK 异常 | code | 名称 |
|----------|------|------|
| `PluginBusyError` | `-32001` | plugin_busy |
| `FileLoadFailedError` | `-32002` | file_load_failed |
| `ParseFailedError` | `-32003` | parse_failed（异常 `data` 属性 → error.data） |
| `CancelledError` | `-32004` | cancelled（配合 `ctx.check_cancelled()`） |
| `UnsupportedInV1Error` | `-32005` | unsupported_in_v1 |
| `InvalidParamsError` | `-32602` | Invalid params |
| 其它未分类异常 | `-32603` | Internal error（SDK 兜底，stderr 记 traceback） |

错误对象统一形态 `{"code", "message", "data"?}`；**严禁使用 `-32001`~`-32005`
以外的自定义 code**（SDK 序列化时硬校验）。

## 最小插件（完整可运行）

```python
import os

from analysisbuddy import AnalysisBuddyPlugin, FileLoadFailedError

class HelloPlugin(AnalysisBuddyPlugin):
    id, name, version = "hello-plugin", "Hello", "0.1.0"

    def __init__(self):
        super().__init__()
        self._files = {}   # file_id -> path

    def on_can_handle(self, p):
        return {"can_handle": p["ext"] == "log", "confidence": 0.9}

    def on_load_file(self, p):
        if not os.path.exists(p["path"]):
            raise FileLoadFailedError("file not found", data={"path": p["path"]})
        self._files[p["file_id"]] = p["path"]
        return {}

    def on_parse(self, file_id, options, ctx):
        total = 0
        for line in open(self._files[file_id], encoding="utf-8"):
            ctx.check_cancelled()
            ts, value = parse_line(line)          # 你自己的解析逻辑
            ctx.emit_records([{"timestamp": ts, "metric": "demo", "value": value}])
            total += 1
        return total

    def on_schema(self):
        return {"metrics": [{"id": "demo", "name": "示例", "unit": "ms",
                             "aggregation": "avg"}]}

    def on_key_values(self, file_id, ts):
        return {"entries": [{"key": "state", "value": "idle"}]}

if __name__ == "__main__":
    HelloPlugin().serve()
```

配套的 `plugin.json`（`entry` 写法见 [04-manifest-reference.md](04-manifest-reference.md)）：

```json
{
  "id": "hello-plugin",
  "display_name": "Hello",
  "version": "0.1.0",
  "entry": { "command": "python", "args": ["main.py"] },
  "match": { "extensions": ["log"], "header_fingerprints": [] },
  "min_protocol_version": 1
}
```

仓库内可直接参照运行的实例：`sdk/python/examples/sample-plugin`（最小合规样例，
带 `sample.log` 夹具）与 `plugins/demo-tool`（全能力示例：三指标 + tags +
key_values 状态索引 + annotate 事件标注）。两者都用
`plugin check <dir> --behavior --fixture <样例日志>` 自检通过。

---

📌 章节要点（双视角）

👤 **给人**：Python SDK 的收益是「零依赖 + 无构建」——插件仓库里没有 `requirements`
之外的产物，`plugin.json` 直接写 `"command": "python", "args": ["main.py"]` 即可；
批量与心跳都交给 SDK，作者只需要写解析循环与 `ctx` 调用。

🤖 **给 Agent**：使用本 SDK 时逐条对照「BEH 自证清单」（`02-write-a-plugin.md`
第 3 步）：SDK 已自动保证 BEH-02/03（id 回显、-32601 兜底）、BEH-04（心跳）、
BEH-06（seq/records_total）、BEH-08/09（帧纪律）、BEH-10/12（shutdown/EOF 退出）；
作者侧仍须自证 BEH-01（`id` 与 manifest 一致）、BEH-05（`Record.metric` ∈
`on_schema()` 声明）。API 签名以本文件与 sdk-plugins.md §1.2 为准，不得臆造方法。
