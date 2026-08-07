# mock-plugin — 协议回放插件（NDJSON 剧本）

AnalysisBuddy 的联调底座（P1-04）：一个按剧本回放应答的假插件。A/B/C/F 四路在
`contract-v1` 冻结前即可用它对宿主管线开发与测试，零等待真实插件。

宿主以 JSON-RPC 2.0 over stdio 与本插件通信（protocol-v1.md §1.1）：stdin 写请求、
stdout 读应答/通知；剧本决定每个方法怎么回。

## 构建与运行

```powershell
cargo build --release -p mock-plugin
.\target\release\mock-plugin.exe --script scripts\happy_path.ndjson
```

| 参数 | 说明 |
|------|------|
| `--script <file>` | 剧本文件路径 |
| `--script -` | 剧本从 stdin 读取（解析完即退出码 0；该模式无剩余请求通道） |
| `--help` | 用法说明（输出到 stderr） |

## 环境变量 `AB_MOCK_SCRIPT`（A/B/C/F 联调入口约定）

未给 `--script` 时回落 `AB_MOCK_SCRIPT`（内容为剧本路径或 `-`）；两者皆无则退出码 2。

```powershell
$env:AB_MOCK_SCRIPT = "C:\...\tools\mock-plugin\scripts\happy_path.ndjson"
.\mock-plugin.exe          # 免参数拉起
```

## 剧本行格式（回放器私有约定，非协议契约）

NDJSON 剧本：每行一条指令，四种 `kind`，按行序执行。

| kind | 字段 | 语义 |
|------|------|------|
| `reply` | `method` + `result` | 收到该 method 请求即回 result（id 逐字回显） |
| `reply` | `method` + `error` | 收到该 method 请求即回 error（`{code, message}`） |
| `emit` | `method`（`RecordBatch`/`progress`）+ `params` | 作为通知推送（parse 期间用） |
| `sleep` | `ms` | 推送间睡眠（heartbeat_stop 剧本用） |

**块语义**：连续指令归入下一个 `reply` 行所属的方法块；收到某方法请求时按剧本行
顺序执行该块（先 emit/sleep，最后 reply）。每个方法最多一个块，块必须以 `reply`
收尾。

**契约校验**：剧本加载时，`result`/`params` 必须能反序列化为 ab-protocol 契约类型
（`initialize`→`InitializeResult`、`parse`→`ParseResult`、`RecordBatch`→`RecordBatch`
通知等；`unload_file`/`cancel_parse`/`shutdown` 的 result 必须为 `{}`）；输出前以
契约类型重新序列化，skip-if-empty 等序列化约定逐帧成立。校验失败即拒绝启动
（stderr 报错，退出码 1），不产生任何 stdout 输出。

**未剧本化的方法**回 `-32601 Method not found`；非法 JSON 请求行记 stderr 警告并跳过。

## stdout / stderr 纪律（A 路容错用例依赖，protocol.md §1.1 / §9）

- stdout 只输出协议帧：单行 JSON-RPC 2.0（行尾 `\n`，禁止 `\r`），每帧后 flush；
- 全部日志走 stderr，`INFO`/`WARN`/`ERROR` 前缀；
- stdin EOF → 退出码 0；收到 `shutdown` 请求应答后立即退出。

## 手工驱动

```powershell
'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"host_info":{"name":"AnalysisBuddy","version":"0.1.0"}}}' |
  .\target\release\mock-plugin.exe --script scripts\happy_path.ndjson
```

EOF 后进程退出码 0；应答逐帧与 protocol-v1.md §3.5 示例形状一致。

## 剧本清单

| 剧本 | 回放行为 | 用途 |
|------|----------|------|
| `happy_path.ndjson` | initialize→schema→can_handle→load_file→parse（2×progress + RecordBatch seq0/seq1 done:true，`records_total:3`）→key_values→unload_file→shutdown | 全绿流程联调 |
| `load_failed.ndjson` | load_file 回 `-32002 file_load_failed`（message `"file load failed"`） | protocol.md §4.2 加载失败路径 |
| `heartbeat_stop.ndjson` | parse 首批 RecordBatch 后静默 40s（40s ≥ 35s > 30s 看门狗）再回 `records_total` | protocol.md §6 心跳停止 → Timeout |

## 自洽检查

```powershell
cargo test -p mock-plugin   # 剧本解析 / EOF 退出 / stdout 纯净
```
