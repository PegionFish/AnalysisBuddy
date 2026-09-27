# Changelog

版本纪律（H4 / 总计划 §3.6.6）：产品版本由 **git tag 派生**（`v*`），crate 版本
仅作 crate 语义化版本；面向用户的变更记录在本文件。格式参照
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

### 安全（M1 安全闭环）
- **P0-1** 网关插件管理面增加 DQA `sysadmin` 角色门——viewer 装插件的任意代码执行向量关闭。
- **P0-2** 服务器路径导入从公开面移除（网关 404 + `--import-roots` 白名单 + 前端删路径框）。
- **P0-4** 上传副本在 job 终态即删 + 启动清扫（内存盘耗尽向量关闭）。
- `ab_sid` 服务端签发（256bit CSPRNG）；客户端自报 `ab_tenant` 一律忽略；
  `POST/DELETE /api/v1/session` 显式生命周期；六类终结共用幂等 teardownSession。
- 网关容错：坏 cookie 跳过、DQA 上游超时与 `AB_AUTH_FAIL_POLICY`、代理请求级超时（SSE 豁免）。
- nginx：`/assets/` 与 `/index.html` 补齐安全响应头；deploy 回滚改恢复备份；`AB_COOKIE_SECURE`。

### 资源与稳定性（M2/M3）
- 失败/取消/预算超限出口补调插件 `unload_file`（插件进程不再驻留原始数据）。
- stderr 泵块读有界（超长单行 OOM 关闭）；五处同步 IO 移出 tokio worker。
- Store 锁 per-file 化 + parking_lot（freeze 不再阻塞全库查询；锁毒化免疫）；
  spawn 锁 per-plugin 分桶 + 握手 5s 超时弃权；CatchPanic 层（panic → §4 包络）。
- 死会话复活接线（插件崩溃后导入自动恢复）；终态 job 取消返回快照 + cancel_parse 接线。
- 容量检查计入 starting（并发突破 AB_MAX_TENANTS 关闭）；网关启动全量清扫 + teardown 进程退出后 final sweep。

### 插件交付层（M4）
- 服务器包带内建插件制品 + 交付一致性断言（builtin 清单移交打包层）。
- manifest entry 平台感知（`platforms` 覆盖 + 扩展名回退），CI 的 ELF→.exe 复制 hack 删除。
- 安装冒烟（5s 握手，失败回滚带错误详情）；`POST /plugins/rescan` 运维通道 + builtin 修复性安装放行；
  ZIP 解压恢复 Unix 执行位。

### 协议与 SDK（M5）
- `InitializeResult.protocol_version` 回显协商（v1 缺省/省略键前向兼容）。
- demo-tool 去 vendoring（内嵌漂移副本删除）；打包注入字节级一致性 CI 断言；
  防漂移测试改读协议正本。
- 入站 `Record.value` 有限性拒绝 + `confidence` 夹逼 [0,1]。

### WebUI / 桌面
- ECharts ResizeObserver（画布随容器尺寸）；load_session 装载互斥 + loading 指示；
  会话重置/装载前卸载旧 file_id + 保存收集改前端清单；IPC 开关显式环境变量优先；ab-app 死代码删除。

### 工具与验收
- `tools/cleanup-verify/`：residue checker（D1）+ chaos harness（D2）+ 内存棘轮（I5）。
- CI：插件测试 job（Rust×2 + pytest）；WebUI 仓首个 CI；nightly 全量测试 + chaos + 棘轮。
