# 01 · 15 分钟跑通一个插件

> 复验状态：本卡走查路径依赖 `plugins/demo-tool`（D2-03）与 `plugin check`（E-02）。
> demo-tool 未合入期间可用 `plugins/builtin-csv`（D1-03）作等价替换；两条路径均待
> D1-02/D2-02 与 D2-03 合入后复验。

目标：把 AnalysisBuddy 的现成插件改成一个「自己的」插件，并通过校验器自检。

## 前置条件

- Windows 10/11，已安装宿主（或本仓库开发环境）；
- 语言运行时之一：Python（插件作者机器需有 `python` 可执行，见
  [protocol-v1.md §7.3](../spec/protocol-v1.md#73-entry-conventions-for-repository-ready-use)）；
- `plugin check` 校验器（源码在 `tools/plugin-validator`，`cargo build --release`
  后产物为 `plugin-check.exe`），或已加入 PATH 的发行版。

## 第 1 步：拿一个现成插件做底子（3 分钟）

把 `plugins/demo-tool`（或 `plugins/builtin-csv`）整个目录复制成自己的插件目录：

```powershell
Copy-Item -Recurse .\plugins\demo-tool .\plugins\my-tool
```

插件目录 = 一个独立文件夹，其中必须有一个**位于文件夹根部的** `plugin.json`
（发现规则见 [protocol-v1.md §7.1](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)）。

## 第 2 步：改三处（7 分钟）

1. **`plugin.json`**：改 `id`（全局唯一，且必须等于目录名）、`display_name`、
   `version`；`entry` 指向你实际的入口；`match` 声明你要认领的扩展名/指纹。
   逐字段说明见 `04-manifest-reference.md`，字段类型与必填性以
   [plugin-manifest.schema.json](../spec/plugin-manifest.schema.json) 为准。
2. **入口脚本/程序**：`entry.command`（+ `args`）指向的进程必须实现
   JSON-RPC 2.0 over stdio 协议（方法清单见 `03-protocol-walkthrough.md`）。
3. **样例日志**：准备一份你自己的小日志（几百行即可），供 `--fixture` 行为回放使用。

> 参数数值（批量条数、超时秒数、行大小上限等）均以协议正本为准，见
> [protocol-v1.md §3.2](../spec/protocol-v1.md#32-recordbatch-notification) 与
> [§6 超时表](../spec/protocol-v1.md#6-timeout-table)。

## 第 3 步：`plugin check` 自检（3 分钟）

```powershell
# 结构校验（秒级）
plugin check .\plugins\my-tool

# 全量校验（含行为回放；CI 必跑这条）
plugin check .\plugins\my-tool --behavior --fixture .\sample.log --json
```

- 退出码 `0` = 通过；`1` = 仅警告；`2` = 存在不合规；`3` = 用法错误；`4` = 校验器自身故障。
- 报错先查 `05-debugging.md` 的规则 ID 对照表；修完重跑，直到退出码为 `0`。
- 不带 `--behavior` 时，输出末尾会提示追加 `--behavior` 执行协议行为回放。

## 第 4 步：拖入 plugins 目录（2 分钟）

整个插件文件夹（可含 `.git/`、源码、构建中间产物，宿主全部无视，
见 [protocol-v1.md §7.1 第 3 条](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)）
放入宿主插件目录之一：

- 便携版：`<宿主 exe 同级>\plugins\my-tool\`
- 用户目录：`%APPDATA%\AnalysisBuddy\plugins\my-tool\`

宿主重启（或插件管理页点「重载」）后，插件出现在插件列表即成功；若没出现，
管理页会显示原因（通常对应一条 `MAN-xx` 规则），对照 `05-debugging.md` 处理。

## 常见卡点速查

| 现象 | 去查 |
|------|------|
| 拖入后不显示 | MAN-08（plugin.json 位置）/ MAN-01（字段缺失）/ MAN-05（协议版本超限） |
| 握手失败 | BEH-01（initialize 响应）/ BEH-09（stdout 混入非 JSON 内容） |
| 解析中途报「插件无响应」 | BEH-04（心跳） |
| 个别记录不上图 | BEH-05（Record.metric 未在 schema 声明） |

---

📌 章节要点（双视角）

👤 **给人**：15 分钟跑通的关键是「复制现成插件改」，而不是从零搭工程；`plugin.json`
与入口脚本的关系搞清楚后，其余都是增量知识。跳过 `--behavior` 只做结构校验也可以，
但上 CI 前必须全量。

🤖 **给 Agent**：按序执行「复制 `plugins/demo-tool` → 改写 `plugin.json` 的
`id`/`display_name`/`version`/`entry`/`match` → 实现或复用入口 → 跑
`plugin check <dir> --behavior --fixture <fixture> --json`」；成功判据 =
`exit_code` 字段为 `0` 且 `rules` 数组为空；失败时按 `rules[].id` 查
`05-debugging.md` 后重跑，不得跳过自检直接交付。
