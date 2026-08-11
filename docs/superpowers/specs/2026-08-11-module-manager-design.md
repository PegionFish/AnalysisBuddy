# 模块管理器设计（Module Manager）

- 日期：2026-08-11
- 状态：已评审（brainstorming 四节逐节确认）
- 关联：04-manifest-reference.md（manifest 参考）、09-install-and-layout.md（目录布局）、protocol-v1.md §7.1（发现协议）

## 1. 术语与范围

- **模块（module）** = 现有 **plugin（插件）**：一个直接子目录 + 根目录 `plugin.json`。本文档统称「模块」，代码/契约沿用 plugin 命名。
- 本设计新增 GUI 模块管理（扩展 `/plugins` 页）：
  1. 查看已安装模块详情（版本、来源、状态、作者、源码库、适配工具、版本历史）
  2. ZIP 安装新模块（拖入 / 文件选择器）
  3. 卸载模块（内建除外）
  4. 手动检查更新（metafile `update_url` → GitHub Releases API）
  5. 禁用 / 启用模块
- **不在范围**：启动时自动检查更新；多模块 ZIP；非 GitHub 更新源；签名 / 校验和验证；markdown 渲染器（changelog 结构化 + UI 排版）。

## 2. 架构无关原则（用户决策 2026-08-11）

- 模块应与 CPU 架构无关（解释型实现、Python 脚本、.NET AnyCPU 便携等）。
- 安装 / 更新**不校验目标架构**——单 .zip 资产规则成立（x64 下载的模块在 ARM64 上同样可用）。
- 内建模块（如 builtin-csv，Rust 原生 exe）是架构相关例外，受保护、随应用升级。
- 该原则写入模块作者指南（06-sdk-python.md / 07-sdk-dotnet.md / 04-manifest-reference.md）。

## 3. 数据模型

### 3.1 manifest（plugin.json）扩展字段

全部可选、向后兼容（`additionalProperties: true` 既有行为不变，新字段转为正式定义并纳入 schema 与校验）：

```json
{
  "id": "myplugin",
  "display_name": "我的模块",
  "version": "1.2.0",
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

| 字段 | 类型 | 规则 |
|------|------|------|
| `author` | string | 可选；非空字符串 |
| `repository` | string | 可选；合法 https URL；可同时作为 `update_url` 的合法值 |
| `tools` | string[] | 可选；`semver::VersionReq` 语法约束（如 `AnalysisBuddy >= 0.2.0`）；宿主自检身份 `AnalysisBuddy` + 自身版本，不满足 → 模块 invalid（错误消息：`需要 AnalysisBuddy >= x.y.z`） |
| `update_url` | string | 可选；https://github.com 仓库地址（全 URL 或裸 `owner/repo`）；非法 → 模块 invalid |
| `changelog` | array | 可选；`{ version, date, notes[] }`；version 为 semver，date 为 `YYYY-MM-DD`，notes 为字符串数组 |

### 3.2 禁用状态持久化

- 文件：`<appdir>\plugins\.ab-modules.json`，内容 `{ "disabled": ["id1", ...] }`
- 位于 plugins/ 根（发现扫描只扫直接子目录，不误识别）
- 损坏 → 回退空集合 + 宿主日志告警，不阻塞发现

### 3.3 内建模块标记

- `build.rs` 构建时扫描仓库 `plugins/` 目录，生成 `BUILTIN_PLUGIN_IDS: &[&str]` 常量（未来新增内建只需加目录，重构建即纳入保护）
- 内建模块：仅可查看与禁用，卸载 / 覆盖被拒（`module_protected`）

### 3.4 安装位置与冲突

- 一律安装到 `<appdir>\plugins\<id>\`（便携随身；用户决策）
- 已存在同 id：版本相同 → 「已安装」；版本不同 → UI 确认后覆盖（卸载→解压→reload 原子序列）；内建 → 拒绝
- 边界：应用升级包若含同名模块，以随包版本为准（覆盖解压既有行为，文档注明）

## 4. 命令与数据流

### 4.1 新增 5 个命令

`core/ab-app/src/commands/plugin_manager.rs`，薄 handler + `*_logic` 可测函数，`rename_all = "snake_case"`，注册进 `lib.rs` invoke_handler + `capabilities/default.json` 补 `allow-*`：

| 命令 | 参数 | 返回 | 说明 |
|------|------|------|------|
| `install_plugin_zip` | `path`（本地 ZIP 绝对路径）, `overwrite`（bool，同 id 不同版本时 UI 确认后传 true） | `PluginInfoDto` | 拖入/文件选择后的导入入口 |
| `uninstall_plugin` | `plugin_id` | `Unit` | 内建拒绝；运行中会话先关闭 |
| `set_plugin_enabled` | `plugin_id`, `enabled` | `Unit` | 写状态文件 + reload |
| `check_plugin_update` | `plugin_id` | `UpdateInfoDto` | 调 GitHub API，只查询不下载 |
| `update_plugin` | `plugin_id` | `PluginInfoDto` | 下载→校验→覆盖→reload |

### 4.2 安装管线（install_plugin_zip / update_plugin 共用）

```
ZIP 路径 → ① 大小/条目数限额（≤100MB、≤2000 条目）
        → ② zip-slip 防护（reject 绝对路径/..，zip crate enclosed_name）
        → ③ 解压到应用临时目录
        → ④ 根目录必须有 plugin.json → 解析 + host validate（id/版本/entry/tools 适配）
        → ⑤ id 冲突判定（内建→拒绝 / 同版本→已安装 / 不同版本→确认后继续）
        → ⑥ 原子搬入 plugins/<id>/（先删旧再搬）
        → ⑦ PluginRegistry::reload() → PluginsReloaded 事件 → UI 刷新
```

### 4.3 更新流（手动触发）

```
check_plugin_update(id)
  → 解析 update_url → owner/repo
  → GET api.github.com/repos/{owner}/{repo}/releases/latest（User-Agent 必填）
  → 取唯一 .zip asset（多个 → update_not_available：要求发布者整理）
  → 版本比较：tag_name 去 v 前缀 → semver 比对（tag 非 semver → update_not_available）
  → 返回 { latest_version, asset_name, is_newer }
UI 显示「发现 v1.2.0 → 更新」
update_plugin(id)
  → 下载 asset（限时/限大小）→ 走安装管线
  → 关键校验：ZIP 内 plugin.json 的 id 必须 == 被更新插件的 id
  → reload 后自动重启该插件的运行中会话（复用 reload_plugin 语义）
```

### 4.4 卸载 / 禁用语义

- 卸载：关闭该插件全部运行中会话（复用 shutdown 语义）→ 删目录 → reload
- 禁用：写状态文件 → `PluginRegistry::set_disabled()`（ab-host 新增）→ reload 后不再加载/不可 spawn（`reload_plugin` 对禁用 id 报错）；启用即移出集合 + reload
- ab-host 侧发现/加载路径统一过滤（宿主级强制，UI 无法绕过）

### 4.5 网络层（可测试性）

- 抽象 `UpdateFetcher` trait：`fetch_latest_release(owner, repo)` / `download(url, dest)`；生产实现 = reqwest（minimal + rustls），测试实现 = 内存 mock；更新逻辑全部走 trait，单测零网络
- GitHub API 响应 serde 反序列化，未知字段忽略

## 5. 错误处理

### 5.1 新增 IpcError 码（`ipc_errors.rs` 表扩展 + i18n 标签）

| code | 场景 |
|------|------|
| `module_install` | ZIP 无效/根缺 plugin.json/manifest 校验失败/解压失败/目录无写权限 |
| `module_conflict` | id 冲突（不同版本待覆盖，或与已安装其他模块同 id） |
| `module_protected` | 卸载/覆盖内建模块 |
| `module_in_use` | 会话关闭超时（卸载/禁用前的清理失败） |
| `update_not_available` | 无 release/无 .zip asset/多个 asset/tag 非 semver/版本不新 |
| `network` | GitHub API 失败/下载失败/超时（含状态码细节） |
| `state_io` | 状态文件读写失败 |
| `module_not_found` | 目标插件不存在 |

### 5.2 安全

- **zip-slip**：安装管线第②步强制（`enclosed_name()`，绝对路径/`..`/盘符前缀一律拒绝）
- **限额**：ZIP ≤100MB、≤2000 条目；更新下载 ≤500MB、30s 无数据超时
- **id 校验**：安装后 plugin.json 的 id 必须等于目标目录 id；更新必须等于被更新插件 id
- **信任模型**：不做签名/校验和验证——模块由用户主动从其选定的 URL 安装/更新（与「拖入目录即用」基线一致）；TLS 强制（拒绝明文 http）
- **无新执行面**：下载内容仅落盘不执行；spawn 走既有 PluginRuntime 通道
- **状态文件**：只读解析 + 损坏回退空集，不信任其中任何路径
- **并发**：同一插件操作以命令层互斥锁串行；UI 按钮按操作中状态禁用
- **崩溃兜底**：安装/更新中断留下残缺目录 → 下次 reload 进 invalid 列表展示（现有机制兜底）

## 6. UI

### 6.1 页面结构（扩展 `/plugins` PluginManagerPage）

```
┌─────────────────────────────────────────────┐
│ [导入模块 ZIP] 拖入 ZIP 或点选文件            │  ← dropzone（复用 tauri://drag-drop 模式，过滤 .zip）
│ ┌──────────────┬──────────────────────────┐  │
│ │ 模块列表       │ 详情面板                  │  │
│ │ id/名称/版本   │ 关于：作者/源码库/适配工具  │  │
│ │ 更新角标       │ 版本历史（changelog 渲染） │  │
│ │ [禁用][卸载]   │ [检查更新]→[确认更新]      │  │
│ │ [查看日志]     │ 进度指示（下载中/安装中）    │  │
│ └──────────────┴──────────────────────────┘  │
└─────────────────────────────────────────────┘
```

- 内建模块：「随应用分发」标记，隐藏卸载按钮（保留禁用）
- 禁用中：灰色 + 「启用」按钮；有 `update_url` 时显示更新按钮
- 操作中（下载/安装/卸载）：行级 spinner + 禁用该行按钮
- 错误：错误横幅 + i18n（`plugins.install.*` / `plugins.update.*` 命名空间）
- mock 层：`mock.ts` 内存版实现 5 命令（install 模拟解压、update 模拟新版本），fixtures 扩展

### 6.2 changelog 渲染格式

- 标题行：`v{version}` + 当前版本高亮徽标 + 右侧 `YYYY-MM-DD`
- notes：无序列表（`•`），每条一行；空 notes → i18n「—」
- 排序：semver 降序（展示层强制，不信任数组顺序）
- 溢出：>20 条折叠「显示全部」懒展开
- 零 markdown 依赖，纯组件排版

### 6.3 数据流

- `Ipc` 接口 + `types.ts`：`UpdateInfoDto`；`PluginInfoDto` 增 `update_url`、`source`、`builtin`、`disabled`
- `PluginsReloaded` 事件从「丢弃」改为「UI 刷新插件列表」（events.ts 消费）
- session.ts：`plugins/install` `plugins/uninstall` `plugins/enabled` `plugins/update` action 与 reducer 分支

## 7. 测试策略（TDD）

**Rust 单测（零网络/真实 ZIP 依赖）**
- `update_url` 解析（全 URL/裸 owner/repo/非法）
- tag→semver（v1.2.0 / 1.2.0 / 非 semver）
- asset 选择（唯一 zip / 多 zip / 无 zip）
- 安装管线：good-zip / 缺 plugin.json / zip-slip（`../`、绝对路径、盘符）/ 超限 / id 不匹配 / tools 不适配（fixture ZIP 测试内生成）

**Rust 集成测试（ab-app，host_query_chain_test 风格）**
- 安装 fixture ZIP → reload → 可发现 → 导入走真实管线
- 卸载运行中插件 → 会话关闭 → 目录消失 → 列表更新
- 禁用 → 不可 spawn（reload_plugin 拒绝）→ 启用恢复
- 更新流：mock UpdateFetcher → 下载 → 覆盖 → 版本变化 → 会话自动重启

**UI 测试（vitest + 扩展 mock）**
- dropzone：拖入 .zip → 调 install_plugin_zip；非 .zip 忽略
- 更新按钮流：check 新版 → 确认 → update → 列表刷新
- 内建保护：无卸载按钮；卸载被拒时错误横幅
- 禁用/启用切换与徽标；changelog 渲染（排序/徽标/折叠）

**门禁**：cargo test + clippy + fmt；vitest + typecheck + check:i18n（key 双语一致）

## 8. 文档与验证器更新

- `plugin-manifest.schema.json`：新增 5 字段定义
- `04-manifest-reference.md`：字段表 + 架构无关作者准则
- `09-install-and-layout.md`：安装/卸载/更新/禁用操作说明 + 升级覆盖边界
- `tools/plugin-validator` 新规则：MAN-10 author/repository 格式、MAN-11 tools 约束语法、MAN-12 changelog 结构、MAN-13 changelog 版本降序 + 当前版本在列（非空时）
- `protocol-v1.md`：无需改动（发现契约不变；新字段属 manifest 层）
