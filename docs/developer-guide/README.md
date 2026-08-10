# AnalysisBuddy 插件开发指南（中文）

本目录是 AnalysisBuddy 插件开发的**中文指引**。协议与代码中的标识符（字段名、
方法名、规则 ID、CLI 参数）一律保留英文，正文表述使用中文。

## 读者地图

| 读者 | 建议路线 |
|------|----------|
| 插件作者（人） | `01-quickstart.md` 15 分钟跑通 → `02-write-a-plugin.md` 从零写插件 → `05-debugging.md` 排错 |
| 内部工具团队（要接入自家日志格式） | `02-write-a-plugin.md` → `03-protocol-walkthrough.md` → `04-manifest-reference.md` → 对应 SDK 章节 |
| 只想把插件装进宿主/搞清楚目录布局 | `09-install-and-layout.md`（三源发现、clone 即识别、私有文件纪律） |
| Agent（自动写插件） | 只看 `02-write-a-plugin.md` 正文 + 各章「🤖 给 Agent」提示框即可产出合规插件，产出后必须跑 `plugin check` 自检 |
| 维护协议/校验器的人 | `03-protocol-walkthrough.md`、`docs/spec/` 契约文件、`tools/plugin-validator/` 源码 |

## 指南与契约的关系

- **契约正本**是 `docs/spec/protocol-v1.md`（冻结于 `contract-v1`）与其配套的两份
  JSON Schema（`docs/spec/plugin-manifest.schema.json`、`docs/spec/rpc-messages.schema.json`）。
  本指南**不是规范本身**，是规范的中文导读与上手教程。
- 指南只引用契约条款，不复述参数数值（超时秒数、批量条数、字节上限等一律以
  「以协议正本为准」或章节链接为准）；若本指南与契约正本表述不一致，**以契约正本为准**。
- 校验器 `plugin check`（`tools/plugin-validator/`）与宿主复用同一份 JSON Schema
  ——「`plugin check` 通过 ⇒ 宿主可发现、可运行」语义一致（详见 `docs-validator.md` §1.1）。

## 目录结构

```text
docs/developer-guide/
├── README.md                 # 本文件：导读与读者地图
├── 01-quickstart.md          # 15 分钟跑通：改 demo-tool → plugin check → 拖入 plugins/
├── 02-write-a-plugin.md      # ★「从零写一个插件」七步主线（人 + Agent 双视角）
├── 03-protocol-walkthrough.md# 协议逐方法走读（protocol-v1.md 的中文导读版）
├── 04-manifest-reference.md  # plugin.json 字段逐项参考 + 三种语言 entry 写法
├── 05-debugging.md           # 排错手册：症状 → 规则 ID → 根因 → 修复动作对照表
├── 06-sdk-python.md          # Python SDK（analysisbuddy-sdk）教程与 API 摘要
├── 07-sdk-dotnet.md          # C# SDK（AnalysisBuddy.Sdk）教程与 API 摘要
├── 08-faq.md                 # FAQ：孤儿进程、编码、大文件、内网插件分发等
├── 09-install-and-layout.md  # 插件安装布局与目录模型（三源发现、clone 即识别）
├── contract-change-proposal-template.md  # 契约变更提案模板
└── schema-errata.md          # 契约实测勘误报告（E-03，随契约审批动态更新）
```

## 语言与引用纪律

- 代码示例、字段名、方法名、规则 ID 保持英文原样；注释与说明用中文。
- 规则 ID（`MAN-xx` / `BEH-xx`）在本指南、`plugin check` 输出、`05-debugging.md`
  三处拼写完全一致；规则 ID 一经发布即冻结，新增规则只能追加编号。
- 指南内出现任何协议数值（超时、批量、字节上限等）时，均标注「以协议正本为准」
  或直接链接到 `docs/spec/protocol-v1.md` 对应章节。

## 如何快速验证自己没写错

```powershell
# 结构自检：十章齐全 + 规则 ID 引用 + 「以协议正本为准」标注（仓库根执行）
Get-ChildItem docs/developer-guide/ | Select-Object Name
Select-String -Path docs/developer-guide/*.md -Pattern 'MAN-\d{2}|BEH-\d{2}' | Measure-Object
Select-String -Path docs/developer-guide/*.md -Pattern '以协议正本为准' | Measure-Object
```

---

📌 章节要点（双视角）

👤 **给人**：先跑 `01-quickstart.md` 建立「插件 = 一个文件夹 + 一个 plugin.json +
一个可执行入口」的心智模型，再动手写自己的插件；目录布局与安装方式看
`09-install-and-layout.md`；遇到报错一律先查 `05-debugging.md` 的规则 ID。

🤖 **给 Agent**：写插件前必须通读 `02-write-a-plugin.md` 全文；产出后必须运行
`plugin check <dir> --behavior --fixture <样例日志> --json` 并断言 `exit_code == 0`；
规则 ID 与 `05-debugging.md`、validator 输出三处拼写必须逐字符一致。
