# 04 · plugin.json 字段逐项参考

> 契约依据：[docs/spec/plugin-manifest.schema.json](../spec/plugin-manifest.schema.json)
> 与 [protocol-v1.md §7](../spec/protocol-v1.md#7-manifest-pluginjson)。
> 字段必填性与类型以 Schema 为准；`plugin check` 结构阶段即执行该 Schema 校验。

`plugin.json` 必须位于插件目录**根部**（`MAN-08`），是整个插件被发现与加载的
唯一入口。宿主侧的完整发现与接纳流程见 [09-install-and-layout.md](09-install-and-layout.md)。

## 字段总表

| 字段 | 必填 | 说明 |
|------|------|------|
| `id` | ✅ | 全局唯一 id；必须等于插件目录名；必须等于 `initialize` 响应的 `id`（`MAN-02`/`BEH-01`）。格式：`^[a-z0-9][a-z0-9-_]{1,63}$`（2~64 字符，仅限小写字母/数字/`-`/`_`，首字符不得为 `-`/`_`；链接：[protocol-v1.md §7.2](../spec/protocol-v1.md#72-field-definitions)） |
| `display_name` | ✅ | 展示名（UI 显示，非空） |
| `version` | ✅ | 语义化版本号字符串（`MAN-07` 校验，见下） |
| `entry` | ✅ | 启动入口对象（`PluginEntry`，见下） |
| `match` | ✅ | 文件匹配规则对象（`MatchRules`，见下） |
| `min_protocol_version` | ✅ | 要求的最低协议版本（整数，最小 1）；大于宿主支持版本（当前为 1）时插件不被加载（`MAN-05`） |
| `author` | 可选 | 作者名；一旦提供须为非空字符串（`MAN-10`） |
| `repository` | 可选 | 源码仓库地址；一旦提供须为合法 **https** URL（`MAN-10`，TLS 强制、拒绝明文 http）；可直接复用为 `update_url` 的合法值 |
| `tools` | 可选 | 宿主适配要求，每项 `{tool} {VersionReq}`（如 `AnalysisBuddy >= 0.2.0`）；宿主自检身份与版本，不满足 → 模块 invalid（`MAN-11`） |
| `update_url` | 可选 | 更新源；仅接受 `https://github.com/{owner}/{repo}`（全 URL）或裸 `{owner}/{repo}`；非法 → 模块 invalid（宿主侧校验，见「可选元信息字段」） |
| `changelog` | 可选 | 版本历史，每条 `{version, date, notes[]}`；非空时按 semver 严格降序且必须含当前 `version`（`MAN-12`/`MAN-13`） |
| `presets` | 可选 | 场景预设（addendum）：本插件关心的测试场景的指标选择集合，≤32 个；结构、上限与匹配语义见 [「presets（场景预设）」](#presets场景预设) 节 |

顶层仍允许出现**其他**任意附加字段（`additionalProperties: true`），宿主与校验器均忽略。

## `entry`（PluginEntry）

| 字段 | 必填 | 说明 |
|------|------|------|
| `command` | ✅ | 可执行命令。**含路径分隔符时一律相对 plugin.json 所在目录解析**；禁止依赖全局 PATH（唯一例外：解释器型入口按系统约定查找，见「entry 写法对照表」）。文件不存在 → `MAN-03` error |
| `args` | ✅ | 命令行参数（可为空数组） |
| `working_dir` | 可选 | 进程工作目录，相对 plugin.json 所在目录解析；缺省 = plugin.json 所在目录。目录不存在 → `MAN-03` error |

宿主侧解析规则（`core/ab-host/src/manifest.rs::resolve_entry`）：

- `command` 是绝对路径 → 直接使用（文件必须存在）；
- `command` **含路径分隔符**（如 `target/release/xxx.exe`、`.\run.exe`）→
  相对 plugin.json 目录解析，文件必须存在；
- `command` **不含路径分隔符**（如 `python`）→ 按系统约定经 PATH / PATHEXT 查找；
  校验器对解释器型入口（`python`/`py`/`python3`，可带 `.exe`）在 PATH 查不到时
  降级为 `MAN-03` warning（其余形态报 error）；
- 子进程以解析后的 `working_dir` 启动；插件私有文件请用相对路径读写，
  天然落在自己目录内（[protocol-v1.md §7.1 第 4 条](../spec/protocol-v1.md#71-directory-model-and-discovery-rules)）。

绝对路径（盘符/UNC 前缀）虽然能用，但会破坏「仓库拖入即用」的可移植性，
校验器报 `MAN-04` 警告。

## `match`（MatchRules）

| 字段 | 必填 | 说明 |
|------|------|------|
| `extensions` | ✅（可为空数组） | 认领的扩展名（小写、不带点，如 `["csv","txt"]`）；空数组 = 仅靠指纹匹配 |
| `header_fingerprints` | 可选 | 文件头指纹：大小写不敏感的子串匹配，任一命中即候选（如 `["timestamp,fps,frame_ms"]`） |

注意：

- `extensions` 与 `header_fingerprints` 同时为空 → 插件永远无法被自动发现
  （`MAN-06` 警告）。`match` 只是发现阶段的预筛，正式判定仍走 `can_handle`
  （[protocol-v1.md §7.2](../spec/protocol-v1.md#72-field-definitions)）；
- 宿主对扩展名逐项校验：限 ASCII 字母/数字及 `+#-_`，不得含路径分隔符，
  元素不得为空；并会做转小写、去前导点的规范化（写 `".CSV"` 能用，但请
  直接写 `"csv"`）；指纹元素必须非空字符串；
- 指纹匹配针对宿主传入的 `head_sample`（文件头 4 KB 的 UTF-8 宽松解码文本，
  [protocol-v1.md §2.2](../spec/protocol-v1.md#22-can_handle)）。

## 三种语言 entry 写法（protocol-v1.md §7.3 对照表）

| 插件语言/形态 | `entry` 写法示例 | 说明 |
|----------------|------------------|------|
| Rust 仓库 | `"command": "target/release/builtin-csv.exe", "args": []` | 标准 `cargo build --release` 产物路径 |
| C# 仓库 | `"command": "publish/MyTool.exe", "args": []` | 标准 `dotnet publish` 输出目录 |
| Python 仓库 | `"command": "python", "args": ["main.py"]` | 解释器 `python` 按系统约定查找（PATH/py launcher，这是「禁止依赖全局 PATH」的唯一例外）；`main.py` 相对插件目录解析 |

> 原则：`entry` 直接指向仓库标准构建产物，**无专有打包格式**；整个仓库目录
> （可含 `.git/`、源码、构建中间产物）clone/拖入 `plugins/<名>/` 即用。

## 可选元信息字段（模块管理器）

以下字段全部可选（向后兼容，`additionalProperties: true` 既有行为不变），
供插件管理页「关于/版本历史/检查更新」展示与消费；校验语义与宿主实现
（`core/ab-host/src/manifest.rs`）逐字对应：

| 字段 | 类型 | 校验语义 |
|------|------|----------|
| `author` | string | 一旦提供须为非空字符串（`MAN-10` error） |
| `repository` | string | 一旦提供须为合法 **https** URL（`MAN-10` error；TLS 强制，明文 http 拒绝）；可直接复用为 `update_url` 的合法值 |
| `tools` | string[] | 每项 `{tool} {VersionReq}`（如 `AnalysisBuddy >= 0.2.0`）；空项、缺 VersionReq 或约束非法 → `MAN-11` error。宿主按自身身份 `AnalysisBuddy` + 当前版本（`CARGO_PKG_VERSION`）自检，不满足 → 模块 invalid（错误消息「需要 AnalysisBuddy ≥ x.y.z」）；非 `AnalysisBuddy` 工具项跳过（前向兼容） |
| `update_url` | string | 仅接受 `https://github.com/{owner}/{repo}`（全 URL）或裸 `{owner}/{repo}`；owner/repo 字符集 `[A-Za-z0-9_.-]`，尾随 `/` 可容忍；非法 → 模块 invalid。校验与消费全在宿主侧（模块管理器），**校验器不对该字段判定**；更新契约见 [09-install-and-layout.md](09-install-and-layout.md)「手动检查更新」 |
| `changelog` | array | 每条 `{version, date, notes[]}`：`version` 须为 semver、`date` 须为 `YYYY-MM-DD`、`notes` 须为字符串数组（任一违反 → `MAN-12` error）；非空时版本必须**严格降序**（semver 比较，非字符串比较）且当前 `version` 必须在列（`MAN-13` error） |

```json
{
  "author": "PegionFish",
  "repository": "https://github.com/owner/repo",
  "tools": ["AnalysisBuddy >= 0.2.0"],
  "update_url": "https://github.com/owner/repo",
  "changelog": [
    { "version": "1.2.0", "date": "2026-08-01",
      "notes": ["新增：XX 功能", "修复：YY 问题"] },
    { "version": "1.1.0", "date": "2026-06-20",
      "notes": ["初始版本"] }
  ]
}
```

> **changelog UI 渲染约定**（插件管理页「版本历史」）：标题行 `v{version}` +
> 当前版本高亮徽标 + 右侧 `YYYY-MM-DD`；notes 渲染为无序列表（空 notes 显示
> 「—」）；展示层**强制按 semver 降序**（不信任数组顺序）；超过 20 条折叠为
> 「显示全部」；零 markdown 依赖，纯组件排版。

> **架构无关作者准则**：模块应与 CPU 架构无关——解释型实现、Python 脚本、
> .NET AnyCPU 便携等均属架构无关形态。安装/更新链路**不校验目标架构**，
> 正是靠这个原则，「单 `.zip` 资产」的更新规则才成立（x64 下载的模块在
> ARM64 上同样可用）。架构相关的原生产物（如 builtin-csv 的 Rust 编译 exe）
> 是**例外**：属内建模块，受保护、随应用升级。

## presets（场景预设）

> addendum 字段（契约依据：[protocol-v1.md §7.2.1](../spec/protocol-v1.md#721-presets-addendum)
> 与 [plugin-manifest.schema.json](../spec/plugin-manifest.schema.json) 的
> `#/properties/presets`、`#/definitions/localized_name`、`#/definitions/preset_entry`）。
> 冻结正文零改动，本字段纯追加，不提供它的插件完全不受影响。

`presets` 声明插件关心的**测试场景**的指标选择集合：每个预设描述「如何从本插件
自己的 metric id/name 中识别出该场景关心的指标」。场景语义是开放的——**由插件
作者自己命名场景**，核心不做任何内建场景假设。

### 结构

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `presets` | `Preset[]` | 可选 | 场景预设数组，≤32 个 |

`Preset`：

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `id` | string | ✅ | 预设 id，`^[a-z0-9][a-z0-9-_]{0,63}$`（1~64 字符，仅限小写字母/数字/`-`/`_`） |
| `name` | `LocalizedName` | ✅ | 双语展示名；`zh` 与 `en` **均必填**且非空 |
| `description` | `LocalizedName` | 可选 | 双语描述（同样 `zh`/`en` 均必填） |
| `entries` | `PresetEntry[]` | 可选 | 顶层条目，对每个分组均生效；≤1000 条 |
| `groups` | `PresetGroup[]` | 可选 | 命名分组，每组内条目各自生效 |
| `keywords` | string[] | 可选 | 模糊兜底关键词；**仅当穷举匹配整体零命中时启用**；条目必须为非空字符串（空白关键词被宿主过滤） |

`PresetGroup`：`id`（string，必填）+ `name`（双语，必填）+ `entries`（≤1000 条，可选）。

`PresetEntry`：

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `want` | string | 可选 | 语义槽位 id；同一 `want` 只取首个命中 |
| `names` | string[] | ✅（≥1 条） | 候选名：规范化 `metric_id` 或原始 metric name |

`LocalizedName`：`{ "zh": "…", "en": "…" }`，两键均必填且非空。

### 上限

- 单插件预设 **≤32** 个；
- 单预设条目（顶层或每组内）**≤1000** 条；
- 非法预设**只丢弃该预设 + 诊断，不拒绝整个插件**。

### 匹配三级语义（apply 时）

预设条目在 apply 时对插件 metric 清单做穷举匹配，共三级：

1. **穷举精确**：候选名先对规范化 `metric_id` 精确匹配，再对原始 metric name
   做**大小写不敏感**匹配；同一 `want` 只取首个命中；记录命中分组；
2. **keywords 模糊兜底**：子串匹配（`matched_by=fuzzy`），**仅当穷举整体零命中
   时启用**；
3. **零命中**：**不改动当前选择**，未命中条目以 `unmatched` 清单化提示。

**用户保存预设**：宿主从 `selectedMetrics` 反推 `plugin_id` + `metric_id` 存入
`entries`（天然精确，无模糊项）。UI 选择态是复合 id `file_id:plugin_id:metric_id`
（`file_id` 会话级），预设只存 `plugin_id` + metric 键，应用时按当前 metricTree
逐文件解析。

### 示例

```json
{
  "presets": [
    {
      "id": "perf-overview",
      "name": { "zh": "性能总览", "en": "Performance Overview" },
      "description": { "zh": "核心性能指标集合", "en": "Core performance metrics" },
      "entries": [
        { "want": "fps", "names": ["fps", "FPS"] },
        { "names": ["frame_time"] }
      ],
      "groups": [
        {
          "id": "cpu",
          "name": { "zh": "CPU", "en": "CPU" },
          "entries": [
            { "want": "cpu", "names": ["cpu_usage", "CPU Usage"] },
            { "names": ["cpu_temp"] }
          ]
        },
        {
          "id": "gpu",
          "name": { "zh": "GPU", "en": "GPU" },
          "entries": [
            { "want": "gpu", "names": ["gpu_usage", "GPU Usage"] }
          ]
        }
      ],
      "keywords": ["fps", "frame", "cpu", "gpu"]
    }
  ]
}
```

机器可校验副本：`docs/spec/examples/manifest-ok-presets.json`（对
[plugin-manifest.schema.json](../spec/plugin-manifest.schema.json) 校验通过）。

## 完整示例

```json
{
  "id": "builtin-csv",
  "display_name": "CSV Universal Parser",
  "version": "0.1.0",
  "entry": {
    "command": "target/release/builtin-csv.exe",
    "args": ["--stdio"]
  },
  "match": {
    "extensions": ["csv", "tsv", "txt"],
    "header_fingerprints": ["timestamp,", "time,"]
  },
  "min_protocol_version": 1
}
```

机器可校验副本：`docs/spec/examples/manifest-ok.json`（对
[plugin-manifest.schema.json](../spec/plugin-manifest.schema.json) 校验通过）。

## 相关规则 ID 速查

| 规则 | 级别 | 触发条件 |
|------|------|----------|
| `MAN-01` | error | 缺必填字段或类型错误（JSON Schema 判据） |
| `MAN-02` | error | `id` 与**安装后插件目录名**不一致（安装目录 = `plugins/<id>/`），或目录树内 `id` 重复。**源码仓库名不受限**——经模块管理器 ZIP 安装时管线自动解压到 `plugins/<id>/`，仓库名与 id 不同完全合法 |
| `MAN-03` | error | `entry.command`/`entry.working_dir` 指向不存在的文件/目录 |
| `MAN-04` | warning | `entry` 使用绝对路径 |
| `MAN-05` | error | `min_protocol_version` 高于宿主支持版本 |
| `MAN-06` | warning | `match` 的 extensions 与 header_fingerprints 同时为空 |
| `MAN-07` | warning | `version` 非语义化版本号 |
| `MAN-08` | error | `plugin.json` 不在目录根部 / 存在多个 |
| `MAN-09` | pass（反向验收项） | 目录内存在 `.git/`、源码、构建中间产物等无关文件不得产生任何告警 |
| `MAN-10` | error | `author` 空字符串；`repository` 非 https URL（明文 http 被拒） |
| `MAN-11` | error | `tools` 条目空、缺 VersionReq 或 VersionReq 非法（非 `{tool} {VersionReq}` 形态） |
| `MAN-12` | error | `changelog` 条目缺 `version`/`date`/`notes` 任一；`version` 非 semver；`date` 非 `YYYY-MM-DD`；`notes` 非字符串数组 |
| `MAN-13` | error | `changelog` 版本非严格降序；或非空时不含 manifest 当前 `version` |

---

📌 章节要点（双视角）

👤 **给人**：最容易踩的三个坑是「`id` 忘了等于**安装后的插件目录名**（仓库名可以不同，ZIP 安装会自动落到 `plugins/<id>/`；但直接拖目录进 `plugins/` 用时目录名须等于 id）」「`command` 用了绝对路径」
「`plugin.json` 放进了子目录」——写完后跑一遍 `plugin check`，三条都会被
`MAN-02`/`MAN-04`/`MAN-08` 拦住。

🤖 **给 Agent**：`plugin.json` 产出后必须跑
`plugin check <dir> --json` 并断言结构阶段无 `MAN-xx` error（`rules` 数组无
`level == "error"` 条目）；`min_protocol_version` 与宿主版本比较语义以
`--host-version` 参数为准（缺省 = 当前发布版）。
