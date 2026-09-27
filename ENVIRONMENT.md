# ENVIRONMENT（Wave 0 / T0.1 产物）· 2026-09-28

开发编排机（本机）与部署主机的工具链、网络、可达性事实。执行计划见
`docs/audit-and-dev-master-plan-2026-09-27.md`；运维背景另见工作区根 `AGENTS.md`。

## 本机（macOS arm64, darwin 27）

| 项 | 状态 |
|---|---|
| Rust | rustc/cargo **1.98.1**（rustup minimal profile；官方 static.rust-lang.org 在本网不可达，**必须走 rsproxy**：`RUSTUP_DIST_SERVER=https://rsproxy.cn`；crates.io 已在 `~/.cargo/config.toml` 配 sparse+rsproxy 镜像） |
| Node | v22.22.2 / npm 10.9.7 |
| Python | 系统 python3 可用 |
| gh CLI | 已登录 PegionFish |
| 共享编译目录 | `CARGO_TARGET_DIR=/Users/bob/AnalysisBuddy/ab-target-shared`（多 worktree 并行 agent 共用，cargo 文件锁保证安全） |
| git worktree 布局 | 主仓 `/Users/bob/AnalysisBuddy/wt/{ws-b,ws-c,ws-f,ws-h}`；WebUI 仓 `/Users/bob/AnalysisBuddy/wt-ui/ws-a` |
| 基线 | `cargo test --workspace --exclude ab-app --exclude ab-perf` 为 macOS 口径（ab-app 依赖 Windows winreg；ab-perf 链接 -lkernel32——均 Windows 定向 crate，E2 任务统一平台感知）。e2e_mock_suite 平台修复已入库（967c639）；e2e_real_plugins 4/6 绿（本地 shim：`plugins/builtin-csv/target/release/builtin-csv.exe`→ELF 软链），demo-tool 2 例在 macOS 系统 Python 3.9 下 ProcessDied(72)——**已知环境限制**，160（Linux+新 python）实测覆盖 |
| 本地 shim（不入库） | `~/bin/python`→homebrew python3.14；`pip install --break-system-packages -e sdk/python`（G3 后 demo-tool e2e 依赖真实 SDK）；`plugins/builtin-csv/target/release/builtin-csv.exe`→共享 target 的 ELF（E2 扩展名回退后已非必需） |

## 网络可达性（本机）

| 目标 | 状态 |
|---|---|
| github.com | ✅ push/pull 正常（主仓 `PegionFish/AnalysisBuddy`、镜像 `PegionFish/AnalysisBuddy_WebUI` 私有） |
| rsproxy.cn | ✅（rust 工具链与 crates 唯一可用源） |
| static.rust-lang.org | ❌ 下载挂死（curl 无限重试），勿直接使用 |
| 192.168.1.171（Gitea/开发机） | ❌ 不可达（不在内网） |
| 43.142.81.160 | ✅ SSH 密钥认证（`ssh bob@43.142.81.160`，BatchMode 直通） |

## 43.142.81.160（生产 DQA 主机，Debian 13 / 2C2G / +4G swap）

- **本会话已获人工批准**（EXECUTION-LOG #AP-1）：AnalysisBuddy 自有服务的部署与实测。
  红线不变：dqa-api/dqa.conf(:80)/PostgreSQL/Gitea(:3001)/`analysisbuddy.service`(:8600) 不触碰。
- 服务面：`ab-auth-gateway.service`（node :8602）+ nginx :8601 站点 + 租户 ab-server 127.0.0.1:8610+。
- **8601 对公网不通**（安全组未放行）：测试走 `ssh -L 8601:127.0.0.1:8601 bob@43.142.81.160` 隧道。
- 部署管线：源码 tarball → `~/ab-deploy/src`（非 git 仓）→ `~/ab-deploy/build-all.sh`
  （机上 cargo build：ab-server + builtin-csv + aibench-llama；rustup/cargo 走 rsproxy）→
  前端 dist 打包上传 → `sudo systemctl restart ab-auth-gateway`。
- 容量：`AB_MAX_TENANTS=4 × AB_TENANT_MEMORY_MB=128`，空闲 TTL 300s（`/etc/analysisbuddy/webui.env`）。

## 机制核实（T0.3）

- `AB_TENANT_MEMORY_MB` 生效链：网关 env → spawn 参数 `--memory-budget-mb` → ab-server
  `args.rs` → 引擎 `config.memory_budget_bytes`（`pipeline_bridge.rs:384` 一带）→ **进程内软件预算**。
  非 cgroup、非 rlimit：实例 RSS 无 OS 级硬顶。契约按此口径表述。
