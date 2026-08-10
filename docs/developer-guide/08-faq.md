# 08 · FAQ

> 数值类答案一律标注「以协议正本为准」，指向
> [docs/spec/protocol-v1.md](../spec/protocol-v1.md) 对应章节。

## 插件进程变成孤儿进程了，怎么办？

协议要求插件 **stdin EOF 必须自行退出（退出码 0）**，禁止成为孤儿进程
（[§9 第 5 条](../spec/protocol-v1.md#9-fault-tolerance-summary)）。排查顺序：

1. 确认入口进程的主循环确实在读取 stdin（很多脚本卡在 `input()`/阻塞读上，
   收不到 EOF）；
2. 用 `plugin check --behavior` 复现——规则 `BEH-12` 会在 EOF 后等待协议规定
   时长仍见进程存活时报警告；
3. 手动验证：向插件进程 stdin 发送 EOF（如 PowerShell `... | 进程`），观察是否
   在合理时间内退出。

两个 SDK 的主循环已内置 EOF 处理，作者无需实现。

## 中文内容/编码相关的问题

- 协议帧为 **UTF-8、无 BOM**；stdout 写入带 BOM 的 UTF-8 会被判 `BEH-09`；
- 插件读日志文件时自行处理源文件编码（如 GBK），输出到 stdout 的 JSON 一律 UTF-8；
- `head_sample` 为 UTF-8 宽松解码（非法字节替换为 U+FFFD），见
  [§2.2](../spec/protocol-v1.md#22-can_handle)——插件做指纹匹配时无需担心源文件
  编码，但 `can_handle` 的判定要基于这份宽松解码后的文本；
- stderr 日志无编码限制，建议 UTF-8 保持一致性。

## 日志文件很大（GB 级），有什么要注意的？

- 插件按行流式解析、分批回传（`RecordBatch`），**不要**一次性读入内存
  （批量条数与单行大小上限以协议正本为准，见
  [§3.2](../spec/protocol-v1.md#32-recordbatch-notification) 与
  [§1.3](../spec/protocol-v1.md#13-size-limits)）；
- `raw_line` 按抽样保留以控制内存（宿主侧按采样保存），详见
  [§3.1](../spec/protocol-v1.md#31-record-structure)；
- parse 期间保持心跳（`BEH-04`），长循环里别忘了周期发送。

## 内网环境怎么分发插件？

插件 = 一个自包含文件夹（manifest + 入口产物），分发方式自由：

- **git clone**：插件仓库直接 `git clone` 到 `plugins/<名>/`（仓库根即插件目录，
  可含 `.git/`，宿主全无视，[§7.1 第 3 条](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)）；
- **zip**：整个插件目录打包，解压到 `plugins/` 下即用；
- 无专有打包格式、无安装脚本；唯一要求：`plugin.json` 在目录根部，`entry` 指向
  的产物随目录一起分发（如 Rust 的 `target/release/`、C# 的 `publish/`）。

分发前在插件目录里跑一遍 `plugin check --behavior`，通过（退出码 0）再发。
目录模型与三源发现规则见 [09-install-and-layout.md](09-install-and-layout.md)。

## 我想用 Rust 写插件，没有 SDK 怎么办？

Rust 路径不依赖 SDK，直接实现 NDJSON JSON-RPC 即可；可复用仓库内的契约 crate
`core/ab-protocol`（类型/错误码/清单结构），参考实例 `plugins/builtin-csv/`
（线程模型、取消标志、发送锁齐备）。完整教程见
[02-write-a-plugin.md 的 Rust 插件开发路径](02-write-a-plugin.md)专节。

## 同一个插件 id 在多个目录里都存在，宿主用哪个？

宿主按固定优先级扫三个源：Portable（`<宿主exe目录>\plugins\`）> InstallDir >
UserData（`%APPDATA%\AnalysisBuddy\plugins\`）。同 id 冲突时高优先级源胜出，
落败者进 shadowed 清单并在插件管理页告警；同优先级源内 id 重复按目录名字典序
取先者。详见 [09-install-and-layout.md](09-install-and-layout.md)。

## 插件管理页提示「入口不存在 / 启动失败」？

多半是 `entry.command` 指向的构建产物没随目录分发（规则 `MAN-03`）：

- Rust：`target/release/<bin>.exe` 要在插件目录内；
- C#：`dotnet publish` 的产物（如 `publish/xxx.exe`）要在插件目录内；
- Python：解释器（`python`）走 PATH 查找，插件脚本路径相对 plugin.json 目录。

排查命令（PowerShell）：`Test-Path .\plugins\my-tool\target\release\my-tool.exe`。
解析规则细节见 [04-manifest-reference.md](04-manifest-reference.md) 的 entry 节。

## `plugin check` 报找不到 JSON Schema？

校验器默认相对自身可执行文件定位 `docs/spec/` 下的两份 Schema；在非仓库根
位置或自定义安装布局下运行时，用 `--schema-dir` 显式指定：

```powershell
plugin-validator.exe check .\my-tool --behavior --schema-dir <仓库>\docs\spec
```

CLI 全参数见 [05-debugging.md](05-debugging.md)。

## 为什么 stdout 不能有任何调试输出？

stdout 是协议专用通道（[§1.1](../spec/protocol-v1.md#11-transport-channels)）：
宿主逐行按 JSON 解析，任何非协议内容（banner、进度条、print 调试）都会破坏帧
解析，被判 `BEH-09`。日志一律走 stderr（宿主会收到插件日志面板）。

## 插件不显示/不被发现？

按这个顺序排查：

1. `plugin.json` 在目录根部？（`MAN-08`）
2. 目录名 == manifest `id`？（`MAN-02`）
3. Schema 校验通过？（`MAN-01`）
4. `min_protocol_version` ≤ 宿主版本？（`MAN-05`）
5. 有没有被自动发现的条件？（`MAN-06`：`match` 至少给 extensions 或指纹之一）

对照 `05-debugging.md` 逐条查。

## 宿主提示「需要更高版本宿主」？

`min_protocol_version` 大于宿主支持版本（[§7.2](../spec/protocol-v1.md#72-field-definitions)）。
要么改低版本号（当前协议版本以协议正本为准），要么升级宿主。

## 两个插件都声明认领同一文件，谁赢？

`can_handle` 返回置信度，宿主取最高者；其余插件保留为手动备选
（[§2.2](../spec/protocol-v1.md#22-can_handle)）。注意置信度必须落在闭区间
`[0, 1]`，越界按 `BEH-01` 处理。

## 我的插件要改协议能力怎么办？

能力（annotate/subscribe/binary_sidecar）的**唯一事实来源**是 `initialize`
响应的 `capabilities` 字段，manifest 不声明能力（[§7.2 设计意图](../spec/protocol-v1.md#72-field-definitions)）。
扩展位（subscribe、binary_sidecar）在 v1 中不实现，调用一律回 `-32005`，见
[§8](../spec/protocol-v1.md#8-extension-slots-not-implemented-in-v1)。

## 校验器/协议契约想改怎么办？

- 契约（`docs/spec/`）冻结于 `contract-v1`：任何修订须走「主代理审批 + 全路广播」；
- 实测中发现的 Schema 误判/漏判记录到 `docs/developer-guide/schema-errata.md`，
  并用 `docs/developer-guide/contract-change-proposal-template.md` 提变更提案；
- 指南类修订不触发契约评审，但引用被修订条款时须随契约广播同步。

---

📌 章节要点（双视角）

👤 **给人**：FAQ 覆盖「孤儿进程 / 编码 / 大文件 / 内网分发 / Rust 路径 / 多源冲突 /
入口产物缺失 / schema 定位」高频问题；拿不准
的先搜「现象 + 规则 ID」，再到 `05-debugging.md` 定位。

🤖 **给 Agent**：FAQ 条目不构成协议依据；任何行为裁决必须回到
[docs/spec/protocol-v1.md](../spec/protocol-v1.md) 正本条款。内网分发场景下，
交付前必须在本机跑 `plugin check --behavior` 且退出码为 `0`。
