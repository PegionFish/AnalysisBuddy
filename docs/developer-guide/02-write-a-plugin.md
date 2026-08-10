# 02 · 从零写一个插件（七步主线）

> 本章是「从零写一个插件」的完整主线：**七步，按序执行**。每一步都给出
> 「👤 给人」的经验提示与「🤖 给 Agent」的机器可执行检查点。
> 读者范围：既有插件作者，也有只看本章 + 提示框即可产出合规插件的 Agent。
>
> 契约依据：全部方法、报文、错误码、超时与字节上限以
> [docs/spec/protocol-v1.md](../spec/protocol-v1.md)（冻结于 `contract-v1`）为准，
> 本章不复述数值，需要时标注「以协议正本为准」或链接到对应章节。

## 第 1 步：环境准备

- Windows 10/11；
- 语言运行时之一：Python（插件作者机器需有解释器，见
  [protocol-v1.md §7.3](../spec/protocol-v1.md#73-entry-conventions-for-repository-ready-use)）、
  .NET 8（C# SDK）、Rust 工具链；
- 安装 `plugin check` 校验器（`tools/plugin-validator`，**独立 workspace**：
  `cargo build --release --manifest-path tools\plugin-validator\Cargo.toml`，
  产物 `plugin-check.exe`）；
- 确认宿主版本（`plugin check --host-version <ver>` 可模拟宿主版本做判定）。

> 先不写代码。用现成的 `plugins/demo-tool`（或 `plugins/builtin-csv`）把
> 「结构校验 → 行为回放 → 拖入 plugins/」整条链路先跑通一遍（见
> `01-quickstart.md`），确认环境 OK 再动手。

👤 **给人**：环境验证 15 分钟搞定，跳过它会在第 5 步把「环境问题」和「代码问题」
混在一起，排错成本翻倍。

🤖 **给 Agent**：执行 `plugin check --version` 与 `python --version`，两者退出码
均必须为 `0`（`plugin check` 缺省依赖 Python 解释器型入口的场景才需要
`python --version`，见 [protocol-v1.md §7.3](../spec/protocol-v1.md#73-entry-conventions-for-repository-ready-use)）；
任一非 0 → 先修环境再继续，禁止带病前进。

## 第 2 步：选 SDK

| 你的源工具/团队情况 | 选择 |
|---------------------|------|
| 脚本型工具链（Python/批处理/内部脚本生态） | Python SDK（`analysisbuddy-sdk`，见 `06-sdk-python.md`） |
| Windows 原生 / 已有 C# 代码库 | C# SDK（`AnalysisBuddy.Sdk`，见 `07-sdk-dotnet.md`） |
| 极致性能 / 零依赖、随宿主静态分发 | Rust 裸协议（复用 `ab-protocol` 契约 crate，见本章「Rust 插件开发路径」节与 `03-protocol-walkthrough.md`） |
| 不确定 | 选 Python（纯 stdlib、零第三方依赖、无需构建） |

👤 **给人**：不确定就选 Python。C#/Rust 的收益（性能、免解释器）只有在
确知瓶颈时才成立；可移植性（终端用户机器有没有解释器/运行时）在选型时就要想清楚。

🤖 **给 Agent**：按输入特征输出**唯一**选择——源工具语言是脚本类 → Python SDK；
已有 C# 代码 → C# SDK；需要零运行时分发 → Rust 裸协议；无法判断 → Python SDK。
**禁止双 SDK 混写**（一个插件只允许一种实现语言）。

## 第 3 步：写 handler（协议方法路由）

插件是 JSON-RPC 2.0 over stdio 的**服务端**：宿主向 stdin 写请求，插件向 stdout
写单行 JSON 响应/通知（传输与帧规则见
[protocol-v1.md §1](../spec/protocol-v1.md#1-transport-and-framing)）。

需要实现的方法（协议方法清单见 [protocol-v1.md §2](../spec/protocol-v1.md#2-method-signatures)）：

| 方法 | 必选 | 作用 |
|------|------|------|
| `initialize` | ✅ | 握手：返回插件元数据（id/name/version/capabilities） |
| `schema` | ✅ | 返回指标声明（`metrics` 数组，每个 `MetricDef` 含 id/name/aggregation） |
| `can_handle` | ✅ | 判定是否认领某文件（返回 `can_handle` 布尔 + 置信度） |
| `load_file` | ✅ | 加载文件并驻留原始数据 |
| `parse` | ✅ | 流式解析：通过 `RecordBatch`/`progress` 通知回传数据 |
| `key_values` | ✅ | 返回某时刻的关键状态值 |
| `unload_file` | ✅ | 释放文件内存（幂等） |
| `shutdown` | ✅ | 收到后应答并退出进程 |
| `annotate` | 可选 | 事件标注（仅当 capabilities.annotate 为 true 时才被调用） |
| `cancel_parse` | ✅ | 取消在途 parse（幂等） |

**先跑通最小集**：`initialize` / `schema` / `can_handle` / `load_file` / `parse` /
`key_values` / `unload_file` / `shutdown` 八个方法齐了就够一个能用的插件；
`annotate` 等可选能力最后再加。

以 Python SDK 为例，最小 handler 集长这样（API 细节见 `06-sdk-python.md`）：

```python
import os

from analysisbuddy import AnalysisBuddyPlugin, FileLoadFailedError

class MyToolPlugin(AnalysisBuddyPlugin):
    id, name, version = "my-tool", "我的工具解析器", "0.1.0"

    def __init__(self):
        super().__init__()
        self._paths = {}   # file_id -> 驻留路径

    def on_can_handle(self, p):
        return {"can_handle": p["ext"] == "log", "confidence": 0.9,
                "reason": "extension .log matched"}

    def on_load_file(self, p):
        if not os.path.exists(p["path"]):
            raise FileLoadFailedError("file not found", data={"path": p["path"]})
        self._paths[p["file_id"]] = p["path"]
        return {"record_count_hint": None}

    def on_parse(self, file_id, options, ctx):
        total = 0
        for line in open(self._paths[file_id], encoding="utf-8"):
            ctx.check_cancelled()                      # 周期调用，支持取消
            ts, metric, value = parse_line(line)       # 你自己的解析逻辑
            ctx.emit_records([{"timestamp": ts, "metric": metric, "value": value}])
            total += 1
            if total % 20000 == 0:
                ctx.progress(bytes_read=len(line) * total)
        return total                                   # records_total

    def on_schema(self):
        return {"metrics": [{"id": "frame_time", "name": "帧耗时",
                             "unit": "ms", "aggregation": "avg"}]}

    def on_key_values(self, file_id, timestamp_ms):
        return {"entries": [{"key": "scene", "value": "boss"}]}

if __name__ == "__main__":
    MyToolPlugin().serve()
```

纪律要点（全部以协议正本为准）：

- **stdout 只准出现协议帧**（单行 JSON + `\n`），一切调试打印走 stderr
  （[protocol-v1.md §1.1](../spec/protocol-v1.md#11-transport-channels)）；
- **可选字段 skip-if-empty**：`level`/`tags`/`raw_line` 为空必须整体省略该键，
  禁止输出 `null` 或空容器（[protocol-v1.md §3.1](../spec/protocol-v1.md#31-record-structure)）；
- **`Record.metric` 必须属于 `schema()` 声明的指标集合**
  （[protocol-v1.md §2.5](../spec/protocol-v1.md#25-schema)）；
- **parse 期间必须周期性发心跳**（`progress` 或 `RecordBatch`，间隔以协议正本为准，
  见 [protocol-v1.md §3.3](../spec/protocol-v1.md#33-progress-notification)）；
- **stdin EOF → 自行退出**（退出码 0），禁止成为孤儿进程
  （[protocol-v1.md §9 第 5 条](../spec/protocol-v1.md#9-fault-tolerance-summary)）；
- **stdout 日志与批量条数**等数值约束见
  [protocol-v1.md §3.2](../spec/protocol-v1.md#32-recordbatch-notification)。

👤 **给人**：先跑通最小集（八个方法，parse 直接全量吐数据），确认「拖进去能出图」
再补能力。常见误区：一上来就做 `annotate` + 采样 + 复杂批处理，结果连握手都过不了。

🤖 **给 Agent**：按 [protocol-v1.md §2](../spec/protocol-v1.md#2-method-signatures)
方法清单逐项生成 stub（每个方法一个空实现），再逐项对照 BEH 规则编号自证：
`initialize`→`BEH-01`、响应 id→`BEH-02`、必选方法不得回 `-32601`→`BEH-03`、
心跳→`BEH-04`、Record 三必填 + metric ∈ schema→`BEH-05`、seq 连续 + records_total
一致→`BEH-06`、stdout 纯净→`BEH-09`、shutdown 后退出→`BEH-10`、EOF 自杀→`BEH-12`。
缺任何一条自证，禁止进入第 5 步。

## 第 4 步：写 plugin.json

字段逐项说明见 `04-manifest-reference.md`；此处给出最小形态：

```json
{
  "id": "my-tool",
  "display_name": "我的工具解析器",
  "version": "0.1.0",
  "entry": { "command": "python", "args": ["main.py"] },
  "match": {
    "extensions": ["log", "txt"],
    "header_fingerprints": ["frame fps="]
  },
  "min_protocol_version": 1
}
```

要点：

- `id` 必须与插件目录名一致（发现模型以目录名为物理锚点，
  [protocol-v1.md §7.1](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)）；
- `entry.command` 一律**相对 plugin.json 所在目录解析**，禁止依赖全局 PATH
  （唯一例外：解释器型入口按系统约定查找，见
  [protocol-v1.md §7.3](../spec/protocol-v1.md#73-entry-conventions-for-repository-ready-use)）；
- `match` 至少给 `extensions` 或 `header_fingerprints` 之一，否则插件永远无法被
  自动发现（MAN-06 警告）；
- `min_protocol_version` 大于宿主支持版本时插件不会被加载（MAN-05）。

👤 **给人**：复制 `plugins/builtin-csv/plugin.json` 改最稳——id/display_name/
version/entry/match 五处改完就基本不会漏字段。

🤖 **给 Agent**：生成 `plugin.json` 后**必须**用
[docs/spec/plugin-manifest.schema.json](../spec/plugin-manifest.schema.json) 校验
（`plugin check` 结构阶段内部即执行此 Schema 校验）；校验不通过禁止进入第 5 步。

## 第 5 步：`plugin check` 自检

```powershell
# 结构校验（秒级）
plugin check .\plugins\my-tool

# 全量校验（含行为回放，CI 必跑）
plugin check .\plugins\my-tool --behavior --fixture .\sample.log --json
```

- 退出码 `0` = 通过（CI 通过线）；`1` = 仅警告；`2` = 存在 error；
  `3` = 用法错误；`4` = 校验器自身故障。
- 结构阶段出 error 时 `--behavior` 会被跳过（进程拉不起来或结果无意义）。
- `--json` 输出 `rules` 数组逐条带 `id`/`level`/`message`/`location`，机器可解析。

👤 **给人**：报错先查 `05-debugging.md` 的规则 ID 对照表——每个 `MAN-xx`/`BEH-xx`
都有对应的症状与修复动作，不要瞎试。

🤖 **给 Agent**：循环执行「跑 `plugin check --behavior --fixture <fixture> --json` →
解析 `rules` 数组 → 按 `id` 对照 `05-debugging.md` 修复 → 重跑」，
直至 `exit_code` 字段为 `0`。循环超过 5 次仍不收敛 → 检查是否违背第 3 步的
BEH 自证清单，而不是继续盲改。

## 第 6 步：拖入 plugins 目录

整个插件文件夹（可含 `.git/`、源码、构建中间产物——宿主只认根部的 `plugin.json`
和 `entry` 指向的入口，[protocol-v1.md §7.1](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)）
放入：

- 便携版：`<宿主 exe 同级>\plugins\my-tool\`（优先）；
- 用户目录：`%APPDATA%\AnalysisBuddy\plugins\my-tool\`。

宿主重启或插件管理页点「重载」后生效；插件页可看健康状态与 stderr 日志。
三源优先级与冲突裁决见 [09-install-and-layout.md](09-install-and-layout.md)。

👤 **给人**：便携版优先——插件的「发现目录」就在宿主旁边，整目录拖走就是分发；
别把插件文件散落到 `%APPDATA%` 根下（插件私有配置也只准写自己文件夹内，
[protocol-v1.md §7.1 第 4 条](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)）。

🤖 **给 Agent**：断言「插件目录名 == manifest `id`」且「`plugin.json` 位于该目录
根级」；任一不成立 → 回到第 4/6 步修复。拖入后通过插件管理页确认状态为就绪
（对应宿主状态机见 [protocol-v1.md §5.1](../spec/protocol-v1.md#51-state-diagram)）。

## 第 7 步：排错

排错手册见 `05-debugging.md`；重试语义以协议正本为准（自动重试次数与退避间隔见
[protocol-v1.md §5.2](../spec/protocol-v1.md#52-host-retry-policy)）。

排查顺序建议：

1. 插件管理页看健康状态与 stderr 日志（stderr 是插件日志的唯一合法通道，
   [protocol-v1.md §1.1](../spec/protocol-v1.md#11-transport-channels)）；
2. 用 `05-debugging.md` 的「症状 → 规则 ID → 修复动作」表定位；
3. 行为问题用 `plugin check --behavior --fixture <样例>` 在本地复现。

👤 **给人**：进程崩溃先看 stderr 最后一屏——十有八九是异常堆栈；stdout 里出现
任何非 JSON 输出（print 调试）都按 BEH-09 处理。

🤖 **给 Agent**：按「症状 → 规则 ID → 修复动作」三元组表驱动（即
`05-debugging.md` 的四列表），修复后重跑第 5 步自检闭环；禁止在未跑自检时
声明「已修复」。

## Rust 插件开发路径（无 SDK，直接实现 NDJSON JSON-RPC）

Rust 插件**没有独立 SDK**：直接按协议正本收发 NDJSON，可复用 `core/ab-protocol`
契约 crate 的类型定义（序列化后与协议帧逐字段一致）。参考实例：
`plugins/builtin-csv`（随宿主分发的内置 CSV 解析插件，零运行时依赖）。

### 工程骨架

1. **建独立 crate**：插件仓库自带 `[workspace]`（与 `builtin-csv` 一样声明
   独立 workspace，不加入宿主根 workspace），以 path 依赖引用契约类型：

   ```toml
   [package]
   name = "my-tool"          # 建议与插件 id 一致
   version = "0.1.0"
   edition = "2021"

   [workspace]               # 独立 workspace 根

   [dependencies]
   ab-protocol = { path = "../../core/ab-protocol" }  # 仓库内插件；外部仓库可自行定义类型
   serde = { version = "1", features = ["derive"] }
   serde_json = "1"

   [profile.release]
   lto = true
   ```

2. **消费契约类型**：`ab_protocol::types` 提供 `InitializeResult`/`CanHandleParams`/
   `LoadFileParams`/`ParseParams`/`RecordBatch`/`SchemaResult`/`MetricDef` 等全部
   协议结构（skip-if-empty 已用 serde 属性落实）；`ab_protocol::errors` 提供
   `ERR_PLUGIN_BUSY`(-32001) ~ `ERR_UNSUPPORTED_IN_V1`(-32005) 等错误码常量。
3. **自己实现主循环**（builtin-csv 的线程模型可直接参照）：
   - 读线程逐行读 stdin（帧长度先于内容校验，超 8 MB 上限按协议处理；帧形状以
     [protocol-v1.md §1.2](../spec/protocol-v1.md#12-frame-format-ndjson) 为准）；
   - `parse` 移入专用线程运行，`cancel_parse` 经共享原子标志（`AtomicBool`）
     即时应答，被取消的 parse 回 `-32004`；
   - 全部 stdout 写经发送锁（`Mutex<BufWriter<Stdout>>`）整行原子写出，
     每帧后 flush；日志全走 stderr；
   - 十个 method 全路由；同 `file_id` 并发 parse 回 `-32001`；
     `load_file` 幂等重入（等价先 unload 再 load）；
   - **stdin EOF → 退出码 0**；收到 `shutdown` 应答 `{}` 后立即退出。
4. **plugin.json 指向构建产物**（见 `plugins/builtin-csv/plugin.json`）：

   ```json
   {
     "entry": { "command": "target/release/my-tool.exe", "args": [] }
   }
   ```

   构建：`cargo build --release`（在插件仓库根）；产物路径相对 plugin.json
   目录解析，因此**分发前必须先构建**，否则宿主报入口解析失败（MAN-03 语义）。

### 与 SDK 路径的差异自查

没有 SDK 兜底，下列协议义务全部自己实现（对照 `05-debugging.md` 的 BEH 规则）：

| 义务 | 对应规则 |
|------|----------|
| initialize 元数据四字段 + id 与 manifest 一致 | BEH-01 |
| 响应 id 逐字回显 | BEH-02 |
| 必选方法不回 `-32601`、错误码只用标准集 ∪ `-32001`~`-32005` | BEH-03 |
| parse 期间心跳（progress/RecordBatch，间隔以协议正本为准） | BEH-04 |
| Record 三必填 + metric ∈ schema + skip-if-empty | BEH-05 |
| seq 从 0 连续递增、records_total 与各批之和一致 | BEH-06 |
| 单行 ≤ 8 MB（建议 ≤ 1 MB，靠缩小批量） | BEH-08 |
| stdout 只出协议帧（UTF-8 无 BOM、LF 行尾） | BEH-09 |
| shutdown 后退出码 0；stdin EOF 自杀 | BEH-10 / BEH-12 |

交付前同样必须跑 `plugin check <dir> --behavior --fixture <样例> --json`
断言 `exit_code == 0`；调试可用 `tools/mock-plugin` 对照帧形状
（见 [05-debugging.md](05-debugging.md)）。

---

📌 章节要点（双视角）

👤 **给人**：七步的骨架是「环境 → 选型 → 代码 → 清单 → 自检 → 部署 → 排错」，
其中第 5 步自检是唯一硬门槛：`plugin check` 通过 = 宿主可发现可运行。

🤖 **给 Agent**：交付判据 = 第 5 步 `plugin check --behavior` 的 `exit_code` 为 `0`
且 `rules` 为空；本步不可跳过、不可用「我认为没问题」代替。规则 ID 拼写必须与
`05-debugging.md`、validator 输出逐字符一致。
