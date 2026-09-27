# DECISIONS（遗留决策登记）

## D-1 预设客户端化的一次性导出（状态：采纳，Wave 3 H3 执行）
服务端 presets 目录（租户数据目录，TTL 7 天）与清理契约（§1.1-4）直接冲突 → 预设客户端化
（localStorage + JSON 导入导出）。切换上线时为现网用户提供一次性导出工具并公告。
现网影响评估：160 现网用户数为个位数（内部使用），导出工具按最小实现（进入工作台时提示下载 JSON）。

## D-2 每源 IP 并发会话上限（状态：暂缓，默认关闭）
无鉴权模型下任何可达者都能开会话直至全局容量；既有 503+Retry-After 是基础闸门。
可选 `AB_PER_IP_SESSION_MAX`（默认 0=off）实现挂 WS-A，是否启用留待运行观察。
注：当前 8601 不对公网开放（安全组），实际暴露面 = 内网/隧道，风险可控。

## D-3（新增）WebUI 仓远端策略
WebUI 仓正本远端 Gitea（192.168.1.171）在内网，本 Mac 不可达；开发期间以 GitHub 私有镜像
`PegionFish/AnalysisBuddy_WebUI` 为 push 目标（remote `github`），Gitea remote 保留待回内网同步。
