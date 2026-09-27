# EXECUTION-LOG（编排者决策日志）

执行依据：`docs/audit-and-dev-master-plan-2026-09-27.md`（2026-09-27 总计划，下称"总计划"）。

## 2026-09-28 会话启动

### 生产批准记录（§2.1 红线）
- **批准 #AP-1**：用户本次会话指令明确要求"将服务部署到我们的160云端服务器上进行实际的API Call和WebUI操作测试"。
  此即对 43.142.81.160 上 **AnalysisBuddy 自有服务**（ab-auth-gateway :8602、nginx :8601 站点、8610+ 租户实例、~/ab-deploy 构建树）的部署与测试批准。
  范围仍排除：dqa-api/dqa.conf(:80)/PostgreSQL/Gitea(:3001)/analysisbuddy.service(:8600 旧实例) 一律不触碰（总计划 §2.1-3）。
- 只读诊断已执行：确认 160 上 ab-auth-gateway 运行中（node pid 1673593 :8602）、nginx :80/:8601、dqa-api :8000、负载 0.16。

### 仓库基线（Wave 0 前置）
- 主仓 `AnalysisBuddy`：本地 main 曾严重落后（落后 origin/main 30 提交 + 1 本地 docs 提交），已重置到 origin/main；
  `feature/aibench-plugin`（52a385b，含 aibench-llama 插件）ff 合入；随后两个卫生提交：
  - 76b799c docs: 总计划注册 + 平台演进规格/路线图入库
  - a16b65b chore(repo): ab-app M1 迁移死代码副本入库留痕（R-4；origin/main 的 lib.rs 用 `pub use ab_engine::*` 再导出，这些文件不参与编译，clone 不坏；入库留痕，WS-F F3 正式删除）
- **push GitHub 成功**：`b1393a6..a16b65b main -> main`（github.com/PegionFish/AnalysisBuddy）。
- WebUI 仓 `AnalysisBuddy_WebUI`：工作区 455 行未提交网关增强（busy 计数/插件广播同步/tmpfs 监控）以 4e5b9b6 入库。
  Gitea（192.168.1.171:3001）从本 Mac **不可达**（不在内网）；已建 GitHub 私有镜像
  `github.com/PegionFish/AnalysisBuddy_WebUI` 并推送 main（remote 名 `github`；Gitea remote 保留）。

### 环境事实（T0.1）
- 本机 macOS arm64（darwin 27）；Node v22.22.2 / npm 10.9.7 可用；Python3 可用。
- Rust：本机原无工具链，rustup 安装中（stable minimal）。cargo 基线待全绿记录。
- SSH：本机 `bob@43.142.81.160` 密钥认证可用（BatchMode 直通）。
- gh CLI 可用（账号 PegionFish）。

### 机制核实（T0.3 部分）
- `AB_TENANT_MEMORY_MB` 生效链：env（网关，默认 512）→ spawn 参数 `--memory-budget-mb N` →
  ab-server `args.rs` 解析 → 引擎 `config.memory_budget_bytes` → **进程内软件预算**（Quest M4.2，
  `pipeline_bridge.rs:384`）。**不是 cgroup 也不是 rlimit**：实例进程自身 RSS 无 OS 级硬顶，
  预算只约束引擎侧数据结构增长。契约表述按此口径（WS-B/WS-H 落实）。

### 流程适配（对总计划 §2.5 的执行层调整，报备）
- 多子代理并行用 git worktree 隔离：`/Users/bob/AnalysisBuddy/wt/ws-{a,b,c,f,h}`（ws 分支
  `ws/<id>/<slug>` 均自 main a16b65b 切出）。Rust 构建共享 `CARGO_TARGET_DIR=/Users/bob/AnalysisBuddy/ab-target-shared`（cargo 文件锁保证并发安全）。
- 台账由编排者统一更新（子代理以结构化结果回报，不直接写 main 上的台账文件），DoD 证据不缺失。
- Wave 1a 的 A1/A2/A3 三卡同属 WS-A（同一网关文件域），按卡序在**同一 worktree 串行**执行（同 WS 串行、跨 WS 并行）。
