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

## Wave 0 完成记录（2026-09-28）

- **T0.1 完成**：rustc/cargo 1.98.1（rsproxy 镜像必经，官方源挂死）；ENVIRONMENT.md 入库
  （1389087）。基线口径：`cargo test --workspace --exclude ab-app --exclude ab-perf`
  （两个 Windows 定向 crate）；e2e_mock_suite 平台修复 967c639；e2e_real_plugins 4/6 绿
  （builtin-csv symlink shim），demo-tool 2 例 macOS 系统 Python 3.9 下 ProcessDied(72)
  已知环境限制（160 Linux 实测覆盖）。
- **T0.2 完成**：契约冻结 dbac547（ws/h/contract-ci，+214/-7）：§8 Session 模型、§9.1/9.2
  import-roots 与路径形态移除、§2.26 GET /files、§9.4 配额（413 upload_too_large /
  429 file_limit_reached / 429 upload_quota_exceeded）、§2.27 POST /plugins/rescan、
  Appendix A 网关-实例接口现状表、冻结标记 2026-09-28。
  **随附契约裁定（CCR 级，已生效）**：`overrides` 键采用 **basename**（客户端可预测，
  与 ImportResult.name 同值）——B4 实现按此。语言沿用英文（与 docs/spec 族一致）。
- **T0.3 完成**：worktree 布局、ws/* 分支、DECISIONS.md（D-1/D-2/D-3）。
- **F1 完成**：e481b14（TimelineChart ResizeObserver；typecheck+build+304 vitest 绿）。
- F1 与 T0.2 已合入 main 并 push GitHub（21c3ece / ba9055b）。

## Wave 1a/1b 派发记录
- WS-A（A1→A2→A3→B1网关半，wt-ui/ws-a）、WS-B（B1服务端半→B2→C1，wt/ws-b）、
  WS-C（C2，wt/ws-c）运行中。
- 追加派发（并行利用空闲 worktree）：F4（wt/ws-f 续）、H1（wt/ws-h 续 + wt-ui/ws-h 双仓 CI）。
- B1 拆分说明：网关 404/405 + 前端删路径框归 WS-A；--import-roots + 403 path_forbidden
  归 WS-B；集成窗口合流。

## Wave 1 集成窗口与 160 生产部署实测（2026-09-28 00:30-01:10）

### 集成窗口（G-wave 门禁）
- 合并序：ws/c(C2) → ws/b(B1/B2/B4/C1) → main；WebUI：ws/a(A1/A2/A3/B1网关半) → main。
- 门禁结果：主仓 `cargo fmt --all --check` 绿、clippy 警告 **0**、
  `cargo test --workspace --exclude ab-app --exclude ab-perf` **299 passed / 0 failed**；
  WebUI tsc 绿 / vitest 20 绿 / build 绿 / 网关 node:test **31 绿**。
- 双仓已 push GitHub：主仓 643e7a2..01fbdf0；WebUI 4e5b9b6..aaa97ee。

### 生产部署（#AP-1 范围内）
- stamp 20260928-005446：dist 本地构建 → git archive → 160 机上增量构建（14.3s）→
  安装 ab-server + 网关 + 前端 dist（各带备份）→ 重启 ab-auth-gateway。
- **生产实弹发现并修复**：`/dev/shm/ab-tenants` 不存在时 systemd
  `ProtectSystem=strict` 的 NAMESPACE 设置在 ExecStartPre 之前执行 →
  status=226/NAMESPACE 重启循环。修复：`/etc/tmpfiles.d/analysisbuddy.conf`
  开机重建三目录 + 手动 mkdir 恢复；单元文件注释已登记该层序陷阱。
- 网关新模型运行确认：日志 `auth model: server-issued ab_sid (legacy client
  ab_tenant cookie is ignored)`。

### 160 API 实测（ops/test-160-api.sh）：**19/19 全绿**
- T1 health 200；T2 会话模型 6 断言（400 session_required / 201+Set-Cookie /
  匿名 GET 铸造 / 404 session_not_found / 植入 ab_tenant 被忽略）全过；
- T3 路径导入两形态网关 404；T4 上传→job completed→builtin-csv 匹配，副本落
  `/dev/shm/ab-tenants/<sid>/ab-server-uploads/`（--import-roots 生效）；
- T5 跨会话隔离（sid2 /metrics 为空）；T6 DELETE 204 + shm 目录清除 + 后续 404；
- T7 匿名 /plugins 401。

### 160 WebUI 浏览器实测（SSH 隧道 :18601 + 浏览器自动化）
1. 匿名工作台正常渲染；「服务器路径导入」输入框已消失（B1 前端落地）；
   顶栏「结束会话」入口在位（A3 前端落地）。
2. 页面侧构造 CSV（600 行 fps/frame_ms/scene）→ 真实上传链路 → job completed →
   builtin-csv 置信度 90% → 指标树（fps/frame_ms）→ 勾选 fps → ECharts 曲线
   正弦波完整渲染（截图归档）。
3. /plugins 未登录 → 登录页；DQA sysadmin（bootstrap 账号，凭据未回显）登录 →
   管理员徽标正确 → 7 插件全列出（内建 3 就绪，含 aibench-llama——「已内建
   却不可用」历史问题随新部署目录+注册表重扫消解）。
4. 「结束会话」→ 整页刷新 → 全部插件「已就绪→已发现」、加载计数归零 =
   旧实例 teardown + 全新沙箱，清理契约在真实环境闭环。
- DQA 侧零触碰（dqa-api/PG/:80/Gitea 全程未动，仅只读读取账号配置一次）。

### 遗留与移交
- Wave 1b 剩余卡（A4 容量竞态/A5 全量清扫/A6 TLS、B3 GET /files、C3/C4、
  F2/F3/F4、H1 CI）与 Wave 2/3 未在本会话窗口完成；子代理配额 05:09 重置后
  可按原任务卡继续派发（卡文本都在本文件与总计划 §2.6）。
- C2 的 Reviewer 评审、B/A 分支的 Reviewer 评审未完成（配额中断）——
  G-wave 机器门禁已全绿，人工/代理评审列为下会话首要事项。

## 决策：全量自主执行模式（2026-09-28 01:15）
用户指示"不要遗留，自主完成所有后续工作，目标是完成整个方案"。子代理配额 05:09 重置，
重置前由编排者亲自串行执行（对 §2.5"编排者不直接改 WS 独占文件"的临时豁免，理由：
唯一可用执行者；豁免范围与每卡记录见下）。重置后大块独立工作（J 系/G 系/I 系）重新并行派发。

## Wave 2 第一批 + D2 chaos 实弹（2026-09-28 02:00-03:20）
- 落地：C8（入站有限性+confidence 夹逼）、C9（终态取消快照+cancel_parse 接线）、
  C10（核实已被 C2.4 覆盖——lost_batch_error/host_backpressure 决策表在位，finding-invalidated）、
  E1（服务器包带插件制品+交付一致性断言）、E2（entry.platforms+扩展名回退，删 CI .exe hack）、
  E3（安装冒烟 5s，tmp 阶段天然回滚）、E4（rescan 端点+修复性安装放行）、E5（unix_mode 恢复）、
  C5（spawn 锁 per-plugin 分桶+握手 5s 超时弃权）、C6 第一段（CatchPanic+jobs parking_lot）、
  D1（residue checker）、D2（chaos harness）。
- 门禁：306→307 测试全绿、clippy 0、fmt 绿；main push（1639d4e）；160 重部署。

### D2 chaos 实弹（M2 验收）——发现并修复一个真缺陷
- Run1（60 轮，seed7）：8 失败——**teardown rmrf 在进程退出前执行**，
  SIGTERM 宽限期内实例重建 TMPDIR 目录后无人清理（I-1 残留）。
  修复：teardownSession 挂 proc exit finalSweep 双保险（WebUI aaa→push）。
- Run2（80 轮，seed99）：13 失败——kill9 语义误判（进程外杀后会话仍注册，
  重连即拉新实例属设计；harness 修正为 DELETE 后断言）。
- Run3（80 轮，seed4242）：**failure_count=0**；abandon 类 3 会话在
  idleTtl 300s 后被 reaper 全量回收（0 目录 0 进程，TTL 收敛验证过）。
- 结论：I-1 不变式在生产成立（60-80 轮档；1000 会话档由 CI nightly 承接）。

## Wave 3 第一批（2026-09-28 03:30-04:40）
- G1 版本回显协商闭环（含 serde default=v1/省略键兼容；host 侧裁决激活）。
- G3 demo-tool 去 vendoring（漂移副本删除、pip 单源、打包注入 CI 字节级断言、本机自测 5 文件 byte-identical）。
- G4 防漂移测试解析正本（Python + dotnet 字面量断言退役）。
- C7 Store per-file 锁 + C6 第二段（pipeline_bridge parking_lot 化）。
- H2 nightly CI（全量+chaos+棘轮）；I5 内存棘轮（160 实弹：三周期峰值 +0.9%/+0.2% clean）；
  H4 CHANGELOG 立册。
- H3 核实：.absession 版本头+高版本拒绝**已实现**（session_file.rs）；
  WebUI 预设客户端化为 no-op（前端零消费者，D-1 无需导出工具）——finding-already-satisfied。
- I4（160 侧）：deploy runbook 实弹演练 3 次（pack→build→install→verify 全流程 +
  systemd 故障处置 + 回滚点备份），chaos/棘轮回归脚本实弹通过；171 侧不可达（内网）留待。

## I1 红队 v1 实弹（2026-09-28 03:50，对 160 生产实例）
- 工具：tools/redteam/redteam_v1.sh（26 项探测）+ zip-slip 定向探针。
- 结果：**0 穿透**。伪造 sid×6→404；植入 ab_tenant→被忽略；文件名穿越/URL
  编码/NUL/超长/Unicode→basename 净化；匿名 install/rescan/uninstall→401；
  json paths 导入→404；SSE 混淆→会话层先行拒绝；直连实例端口→无监听不可旁路；
  zip-slip（管理员身份，../ + 绝对路径条目）→400 拒绝且无文件逃逸。
- 判据说明：3 项初判 PWN 复核为脚本瑕疵（400=fail-closed；000=无监听），非穿透。
- M1 验收「红队报告无 P0/P1」达成。zip-slip 符合 extract 的 enclosed_name +
  E5/E2 防线（单测既有覆盖）。
