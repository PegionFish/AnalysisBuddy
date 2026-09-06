# 10 · 服务器模式（ab-server：HTTP + SSE）

> 本章面向需要在无 GUI 环境运行 AnalysisBuddy 的使用者：Linux 服务器、
> 容器、本地自动化脚本。REST/SSE 契约正本是
> [http-api-v1.md](../spec/http-api-v1.md)（英文，端点/DTO/错误表/事件语义
> 均以其为准）；本章只讲怎么跑起来、怎么调通第一个「导入 → 查询」闭环。

`core/ab-server` 是一个独立的 bin crate：内部装配与桌面壳完全相同的
`ab-engine` headless 引擎（插件发现/进程管理/导入编排/存储查询），对外以
HTTP 暴露桌面 19 个命令的等价端点，外加 SSE 事件流。响应 DTO 与桌面
ipc-ui.md §1.0 形状逐字段一致——为桌面前端写的客户端逻辑可以直接复用。

## 前置条件

- Rust 工具链（workspace 版本，见根 `rust-toolchain`/`Cargo.toml`）；
- 要解析真实文件时：一个匹配该文件类型的插件，放进插件目录（便携源）。

## 构建与启动

```powershell
cargo build --release -p ab-server
# 产物：target\release\ab-server.exe
.\target\release\ab-server.exe --help
```

最小启动（默认 `127.0.0.1:8600`，路径取平台默认，见下文旗标表）：

```powershell
.\target\release\ab-server.exe
# ab-server: listening on http://127.0.0.1:8600 (protocol v1)
```

启用认证（除 `GET /api/v1/health` 外全部要求 Bearer）：

```powershell
.\target\release\ab-server.exe --token "s3cret" --addr 127.0.0.1:8600
```

## CLI 旗标

| 旗标 | 默认 | 说明 |
|------|------|------|
| `--addr <ip:port>` | `127.0.0.1:8600` | 监听地址。远程部署请配反向代理 + TLS，勿直接暴露。 |
| `--token <token>` | 无（不启用认证） | 启用后所有端点（health 除外）要求 `Authorization: Bearer <token>`。 |
| `--max-concurrent-imports <n>` | `2` | 并发导入上限（Semaphore；≥1）。 |
| `--plugins-portable <dir>` | 平台默认 | 便携插件源（模块状态文件也在这里）。 |
| `--plugins-install <dir>` | 同便携源 | 安装源插件目录（ZIP 安装落点）。 |
| `--plugins-user <dir>` | 平台默认 | 用户数据插件目录。 |
| `--presets-dir <dir>` | 平台默认 | 用户预设目录。 |
| `--sessions-dir <dir>` | 平台默认 | 会话目录（save/load 的路径边界）。 |
| `--user-data-dir <dir>` | 无 | 快捷方式：一次设 `<dir>/{plugins,presets,sessions}` 三个子目录；个别旗标可再覆盖。 |

平台默认路径：Linux（及一切非 Windows）走 XDG
（`$XDG_DATA_HOME` 或 `~/.local/share` 下的 `AnalysisBuddy/…`）；Windows
与桌面壳公式一致（exe 同目录 `plugins` + `%APPDATA%\AnalysisBuddy`），
保证开发机上两条交付形态看同一份数据。完整表见
[http-api-v1.md §7.4](../spec/http-api-v1.md#74-data-directories)。

## 第一个闭环：导入 → 查询（curl / PowerShell）

```powershell
# ① 探活（免认证）
curl http://127.0.0.1:8600/api/v1/health
# {"protocol_version":1,"version":"0.1.0","status":"ok"}

# ② 导入（异步 job；202 返回 queued 快照）
$job = curl -s -X POST http://127.0.0.1:8600/api/v1/imports `
  -H "Content-Type: application/json" `
  -d '{\"paths\":[\"C:\\\\logs\\\\game.csv\"]}' | ConvertFrom-Json
$job.job_id        # job-1

# ③ 轮询到终态（queued → running → completed/failed/cancelled）
curl http://127.0.0.1:8600/api/v1/imports/job-1
# {"job_id":"job-1","state":"completed","files":[{"file_id":"…","status":"ready",…}]}

# ④ 指标树（叶节点 id 即复合 metric id）
curl http://127.0.0.1:8600/api/v1/metrics

# ⑤ 查询序列（file_ids 缺省 = 全部已冻结文件；max_points_per_series 缺省 4000，上限 50000）
curl -s -X POST http://127.0.0.1:8600/api/v1/query/series `
  -H "Content-Type: application/json" `
  -d '{\"metrics\":[\"<file_id>:<plugin_id>:fps\"],\"t0_ms\":0,\"t1_ms\":9007199254740991}'

# ⑥ 游标关键值（部分失败协议：永不整体 reject，逐文件 entries/error）
curl -s -X POST http://127.0.0.1:8600/api/v1/query/key-values `
  -H "Content-Type: application/json" `
  -d '{\"timestamp_ms\":1785603599870}'
```

注意三个与桌面命令语义的差异（均为文档化的服务器扩展，见
[http-api-v1.md §2](../spec/http-api-v1.md#2-endpoints)）：

1. **导入是异步 job**：桌面 `import_files` 同步返回结果；HTTP 侧拆成
   202 + job 轮询。文件级结果形状不变（含 `matched` + `needs_user_choice`
   手选分支——用 `overrides` 重新提交即可）。
2. **`file_ids` 缺省 = 全部已冻结文件**（桌面空列表 = 空结果）。
3. **`max_points_per_series` 服务器上限 50000**（超限 400）。

## SSE 事件流

```powershell
curl -N http://127.0.0.1:8600/api/v1/events
```

帧名 = 引擎通道名去 `ab://` 前缀：`progress` / `plugin-log` /
`plugin-health` / `plugins-reloaded`（外加掉队终帧 `error`）。每个连接独立
100ms/file_id 进度节流；`?file_id=` / `?plugin_id=` 可过滤。订阅积压不会
静默丢帧——该连接收到 `event: error`（`event_stream_lagged`）终帧后关闭，
重订阅即可。帧格式与载荷表见
[http-api-v1.md §5](../spec/http-api-v1.md#5-events-sse)。

## 会话与预设

- `POST /sessions/save|load`：路径（相对/绝对）必须落在 `--sessions-dir`
  内（词法规范化后判定，越界 400）。`load` 会对会话内全部文件重走导入
  管线——已在场的文件会 `reopen_failed`，加载前先 `DELETE /files/{id}`。
- `GET|POST /presets`、`DELETE /presets/{id}`：id 由 `name.zh` slug 化派生，
  同 id 重复保存 409 `preset_conflict`，删除幂等。

## Linux 部署要点

- 无 GUI 依赖：纯 tokio + axum，systemd 下
  `Restart=on-failure` 即可；`SIGINT` 优雅停机（先停 HTTP，再关停全部插件
  进程）。
- **插件必须提供 Linux 入口**：manifest `entry.command` 按
  [protocol-v1.md §7.3](../spec/protocol-v1.md) 解析（绝对路径或 PATH 查找）；
  只带 Windows 二进制的插件在 Linux 上无法启动。Python 类插件天然跨平台。
- 目录：`--user-data-dir /var/lib/analysisbuddy` 会得到
  `/var/lib/analysisbuddy/{plugins,presets,sessions}`；或省略旗标走 XDG。
- 更新源（`/plugins/{id}/update`）固定 GitHub Releases；服务器启动即构造
  fetcher，不可用直接失败退出（fail-fast）。
- 安全模型一句话：**插件就是任意代码**——`/plugins/install`、
  `/plugins/{id}/update` 只应对可信操作者开放（token + 网络策略）；
  默认只绑回环地址。详见 [http-api-v1.md §7](../spec/http-api-v1.md#7-security--deployment)。

## 嵌入式形态：不用 HTTP

如果调用方是 Rust 程序，可以跳过 HTTP 直接以库方式使用引擎——
`core/ab-engine` 即发布为库 crate。最小示例：

```powershell
cargo run -p ab-engine --example engine_embed
```

示例在同进程内完成「临时目录装配 → 导入 fixture → 查询 series → 优雅
停机」，装配四件套（`PluginRegistry` → `PluginRuntime` → 事件通道 →
`ImportCoordinator`）与 ab-server `state::assemble` 一致，可直接抄作嵌入
起点。服务器本身（`core/ab-server/src/state.rs`）也是「引擎装配 + 薄路由」
的参考实现。

## 排错速查

| 症状 | 原因 | 处置 |
|------|------|------|
| 导入结果 `status:"matched"` 且 `needs_user_choice:true` | 零候选（没插件认领该扩展名）或多候选 | 装对插件目录后重试；或带 `overrides` 手选 |
| `/plugins` 列表为空 | 便携源目录不存在 / 插件没装在源目录之下 | 检查 `--plugins-portable`；插件必须是「源目录/插件名/plugin.json」 |
| 会话 load 报 `reopen_failed` | 会话内文件当前仍在场 | 先 `DELETE /files/{id}` 再 load |
| SSE 连上但无帧 | 无解析活动（progress 只在 parse 时发） | 先触发一次导入；keep-alive 冒号注释帧属正常 |
| 401 `unauthorized` | 启用了 `--token` 但请求缺/错 Bearer | 补 `Authorization: Bearer <token>`；health 豁免 |
