# 09 · 插件安装布局与目录模型

> 契约依据：[protocol-v1.md §7.1 目录模型与发现规则](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)；
> 宿主实现：`core/ab-host/src/discovery.rs`（三源扫描）与
> `core/ab-host/src/manifest.rs`（清单校验与入口解析）。
> 本章描述宿主**实际**如何找到并接纳一个插件，是「拖入即用」承诺的全部细节。

## 核心心智模型

**一个插件 = `plugins/` 下的一个直接子文件夹**。文件夹本身就是插件单元：

- `plugin.json` 必须位于该文件夹**根部**（`MAN-08`），是被发现的唯一入口；
- 文件夹**本身可以是一个独立 git 仓库**——`git clone` 到 `plugins/<名>/` 即被识别，
  无需二次打包、无专有容器格式、无安装脚本；
- 文件夹是自包含的：manifest 与 `entry` 指向的产物都在其中；
- **一插件一仓库**是设计目标：标准 git 仓库构建产物放进 `plugins/` 直接可用。

## 宿主的三个发现源（固定优先级）

宿主运行时扫描**三个**插件目录（`core/ab-host/src/discovery.rs`），只扫
**直接子文件夹**、不递归、不跟随符号链接、只读不写：

| 优先级 | 源 | 缺省路径 | 说明 |
|--------|----|----------|------|
| 0（最高） | Portable | `<宿主 exe 所在目录>\plugins\` | 纯 ZIP 分发布局的主目录 |
| 1 | InstallDir | 纯 ZIP 布局下与 Portable 同路径（视为同一源，按 Portable 计） | 安装器布局预留位 |
| 2（最低） | UserData | `%APPDATA%\AnalysisBuddy\plugins\` | 用户级插件目录 |

裁决规则：

- **同 id 冲突**：高优先级源胜出，落败者进 `shadowed` 清单并在插件管理页告警
  （例如 Portable 与 `%APPDATA%` 下都有 `id: my-tool`，Portable 生效）；
- **同优先级源内 id 重复**：按目录名字典序取先者；
- **源目录不存在**：视为该源为空，不报错、不创建目录。

PowerShell 下两个常用目录的直观写法：

```powershell
# 便携版（随宿主 ZIP 分发）：宿主 exe 同级
<宿主exe目录>\plugins\my-tool\plugin.json

# 用户目录
"$env:APPDATA\AnalysisBuddy\plugins\my-tool\plugin.json"
```

## 单个插件单元的接纳流程

对每个直接子文件夹，宿主依次执行（`core/ab-host/src/manifest.rs`）：

1. **读清单**：根部 `plugin.json` 必须存在、可读、UTF-8 可解码、顶层是 JSON 对象；
2. **逐字段校验**：
   - `id` 匹配 `^[a-z0-9][a-z0-9-_]{1,63}$`（2~64 字符，全小写字母/数字/`-`/`_`）；
   - `display_name` 非空；`version` 是合法 semver；
   - `match.extensions` 逐项合法（小写字母/数字及 `+#-_`，不得含路径分隔符；
     宿主会做转小写、去前导点的规范化）；`header_fingerprints` 元素非空；
   - `min_protocol_version` 不得大于宿主支持版本（当前为 1，否则拒绝加载并建议升级宿主）；
3. **解析入口**（`resolve_entry`）：
   - `command` 是绝对路径 → 直接使用（文件必须存在）；
   - `command` **含路径分隔符** → 一律相对 `plugin.json` 所在目录解析为绝对路径，
     文件必须存在；
   - `command` **不含路径分隔符**（如 `python`）→ 按系统约定经 PATH / PATHEXT 查找
     （这是协议「禁止依赖全局 PATH」的唯一例外，见
     [protocol-v1.md §7.3](../spec/protocol-v1.md#73-entry-conventions-for-repository-ready-use)）；
   - `working_dir` 省略时默认 = `plugin.json` 所在目录；给出时相对该目录解析且必须存在；
4. **接纳**：以上全部通过的插件进入已发现清单（状态机起点 `Discovered`，
   [protocol-v1.md §5.1](../spec/protocol-v1.md#51-state-diagram)）；
   任何一步失败 → 该文件夹**列出但不拉起**，原因显示在插件管理页。

> 注意：「`id` 与目录名一致」由校验器按 `MAN-02` 强制（发现模型以目录名为物理锚点）；
> 交付前请用 `plugin check` 自检，不要依赖宿主侧行为。

## 无关文件容忍（clone 即识别的前提）

文件夹内**任何额外文件都允许**：`.git/`、源码、构建中间产物（`target/`、`obj/`）、
测试、文档……宿主只认 `plugin.json` 与 `entry` 指向的入口，对无关文件不报错
（[protocol-v1.md §7.1 第 3 条](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)）。
校验器侧的对应规则是 `MAN-09`（反向验收项：存在这些文件不得产生任何告警）。

现成示例：`plugins/builtin-csv/` 就是一个完整 git 仓库形态的插件单元——
`Cargo.toml`、`src/`、`target/`、`config.json` 与 `plugin.json` 并存，
宿主只消费 `plugin.json` 和 `target/release/builtin-csv.exe`。

## 插件私有文件的纪律

插件的私有配置/状态文件（如果有）**只准写在自己的文件夹内**，禁止写全局位置
（注册表、`%APPDATA%` 根等）——[protocol-v1.md §7.1 第 4 条](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)。
参考实例：`plugins/builtin-csv/config.json`（时间列/分隔符/编码等解析配置，
位于插件目录根部，随插件一起分发）。插件进程的工作目录缺省就是插件目录
（见上「解析入口」），用相对路径读写私有文件天然落在自己文件夹内。

## 安装方式一览（内网/离线场景）

| 方式 | 操作 | 备注 |
|------|------|------|
| git clone | `git clone <仓库地址> plugins\my-tool` | 仓库根即插件目录；`.git/` 被宿主无视 |
| ZIP 解压 | 整个插件目录打包，解压到 `plugins\` 下 | 宿主分发本身就是纯 ZIP（每架构一份），插件同理 |
| 目录复制 | `Copy-Item -Recurse .\my-tool <宿主exe目录>\plugins\my-tool` | 最朴素的方式 |

共同要求：`plugin.json` 在目录根部；`entry` 指向的产物**随目录一起分发**
（如 Rust 的 `target/release/`、C# 的 `publish/`）。无安装脚本、无注册步骤。
分发前在插件目录跑一遍 `plugin check --behavior`（退出码 0 再发，见
[05-debugging.md](05-debugging.md)）。

## 生效与重载

- 宿主**重启**后自动全量扫描三源；
- 插件管理页点「重载」= 唯一缓存失效入口：丢弃发现缓存 → 全量重扫 →
  重新校验与裁决 → 发布 `PluginsReloaded` 事件（附 plugins/invalid/shadowed
  三份明细，UI 直接渲染）；
- 重载只影响**发现层**：已 Ready 的活会话不受影响；
- 非法子文件夹（缺 `plugin.json`、Schema 违规、入口解析失败等）不会让宿主报错，
  只在管理页显示原因——原因文案与 `MAN-xx` 规则语义对应，排错对照见
  [05-debugging.md](05-debugging.md)。

---

📌 章节要点（双视角）

👤 **给人**：记住三件事——「插件 = `plugins/` 的直接子文件夹」「`plugin.json`
必须在根部」「clone/解压即用，无任何安装步骤」。同 id 插件放了多个目录时，
Portable 目录优先，其余进 shadowed 告警。

🤖 **给 Agent**：交付插件目录前断言三条：① `plugin.json` 位于目录根部且唯一；
② `id` == 目录名且匹配 `^[a-z0-9][a-z0-9-_]{1,63}$`；③ `entry` 产物在目录内
（相对路径可达）。随后跑 `plugin check <dir> --behavior --fixture <样例> --json`
断言 `exit_code == 0`；布局语义以 protocol-v1.md §7.1 与本章为准。
