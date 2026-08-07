#!/usr/bin/env node
// fps_probe.mjs —— 前端帧率探针接入脚本（qa-perf.md §3.3 / F-02 DoD）。
//
// 流程：连接 Tauri dev（WebView2 经 CDP）→ 注入/探测 window.__abProbe →
// 程序化 dataZoom 拖拽 5s → 取回 fps 序列 → 输出 JSON 到 stdout。
//
// 前置：
//   1. Tauri dev 已起（C 路 UI）：  $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222"
//      npm run tauri dev
//   2. Playwright（WebView2/CDP channel）：
//      npm i -D playwright-core        # 在 ui/ 或任意 Node 环境
//
// 用法：
//   node fps_probe/probe.mjs [--cdp-port 9222] [--url http://localhost:1420]
//
// 退出码：0 成功并输出 JSON；3 探针未就绪（C 路埋点未落地，测试按 SKIP 处理）；
//         4 环境缺失（node/playwright/CDP 不可达）；1 探针测量失败。

import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
let chromium = null;
try {
  chromium = require("playwright-core");
} catch {
  try {
    chromium = require("playwright");
  } catch {
    console.error("ERROR: playwright(-core) 未安装（npm i -D playwright-core）");
    process.exit(4);
  }
}

const args = process.argv.slice(2);
const cdpPort = arg(args, "--cdp-port") ?? "9222";
const url = arg(args, "--url") ?? "http://localhost:1420";
const DRAG_SECONDS = 5;

function arg(argv, name) {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : null;
}

const browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
const contexts = browser.contexts();
if (contexts.length === 0) {
  console.error("ERROR: CDP 无浏览器上下文（Tauri dev 未起或端口不对）");
  await browser.close();
  process.exit(4);
}
const page = contexts[0].pages()[0] ?? (await contexts[0].newPage());
if (!page.url().includes("localhost") && !page.url().includes("127.0.0.1")) {
  await page.goto(url, { waitUntil: "domcontentloaded" });
}

// 探针探测（C 路埋点：ui/ 暴露 window.__abProbe）。
const probe = await page.evaluate(() => {
  const p = window.__abProbe;
  if (!p) return { present: false };
  return {
    present: true,
    hasStart: typeof p.startFps === "function",
    hasStop: typeof p.stopFps === "function",
    firstPaintMs: typeof p.getFirstPaintMs === "function" ? p.getFirstPaintMs() : null,
  };
});
if (!probe.present) {
  console.error("ABORT: window.__abProbe 不存在（C 路探针埋点未落地）；fps 层测试按 SKIP 处理");
  await browser.close();
  process.exit(3);
}

// 开始测量。
await page.evaluate(() => window.__abProbe.startFps());
const cdp = await contexts[0].newCDPSession(page);

// 程序化 dataZoom 拖拽：在页面中心水平往复拖动 5s（真实鼠标事件）。
const box = await page.evaluate(() => {
  const el = document.querySelector("canvas") ?? document.body;
  const r = el.getBoundingClientRect();
  return { x: r.x + r.width / 2, y: r.y + r.height / 2, w: r.width };
});

const fpsStart = Date.now();
let dir = 1;
while (Date.now() - fpsStart < DRAG_SECONDS * 1000) {
  const t = (Date.now() - fpsStart) / 1000;
  const dx = Math.sin(t * 3) * (box.w * 0.2);
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseMoved",
    x: box.x + dx,
    y: box.y,
  });
  if (t % 1 < 0.02) {
    // 每秒触发一次按下-拖动-释放手势（ECharts dataZoom 拖拽）。
    await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: box.x, y: box.y, button: "left", clickCount: 1 });
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: box.x + dx * 0.5, y: box.y });
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: box.x + dx * 0.5, y: box.y, button: "left", clickCount: 1 });
  }
  await new Promise((r) => setTimeout(r, 33)); // ~30Hz 采样
}
const series = await page.evaluate(() => window.__abProbe.stopFps());
const firstPaintMs = await page.evaluate(() =>
  window.__abProbe.getFirstPaintMs ? window.__abProbe.getFirstPaintMs() : null
);

const gpu = await page.evaluate(() => {
  const gl = document.createElement("canvas").getContext("webgl");
  if (!gl) return null;
  const ext = gl.getExtension("WEBGL_debug_renderer_info");
  return ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : null;
});

const out = {
  fps_series: series,
  first_paint_ms: firstPaintMs,
  gpu,
  drag_seconds: DRAG_SECONDS,
};
console.log(JSON.stringify(out));
await browser.close();
process.exit(0);
