# fps_probe —— 前端帧率探针接入（qa-perf.md §3.3，F-02 第三层）

Tauri dev 起宿主 → WebView2 自动化（Playwright CDP channel）→ 注入/探测
`window.__abProbe` → 程序化 dataZoom 拖拽 5s → 取回 fps 序列断言 ≥30fps
（PERF-03 门槛）。

## 运行前置（C 路 UI 就绪后）

```powershell
# 1. ui/ 安装 Playwright core（CDP 客户端）
cd ui
npm i -D playwright-core

# 2. 以 WebView2 CDP 端口起 Tauri dev
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222"
npm run tauri dev
```

## 运行

```powershell
# 显式 ignored（需 Tauri dev 环境）
cargo test -p ab-e2e --test e2e_fps_probe -- --ignored

# 严格模式：拖拽 95 分位 ≥30fps 硬断言（PERF-03）
$env:AB_E2E_FPS_STRICT=1
cargo test -p ab-e2e --test e2e_fps_probe -- --ignored
```

## 判读规则（qa-perf.md §5 矩阵约束）

| 情况 | 处理 |
|------|------|
| `window.__abProbe` 未落地（C 路未合入） | 退出码 3 → 测试 SKIP |
| node / playwright / CDP 不可达 | 退出码 4 → 测试 SKIP |
| headless 软件渲染 | 结果仅参考，输出 `gpu` 字段；不硬判 |
| 本地实机（独立/核显） | 默认参考 + `AB_E2E_FPS_STRICT=1` 硬判 ≥30fps |

## 输出 JSON（schema）

```json
{ "fps_series": [{"fps": 60.0, "dropped_frames": 0}, ...],
  "first_paint_ms": 312.5, "gpu": "NVIDIA ...", "drag_seconds": 5 }
```

`first_paint_ms` = 导入完成事件 → 首个 series 渲染帧间隔（C 路探针同源记录）。
