//! 前端帧率探针 e2e（qa-perf.md §3.3，F-02 第三层）——显式 ignored。
//!
//! 前置：Tauri dev + WebView2 CDP（C 路 UI 与探针埋点就绪后）。
//! 运行：`cargo test -p ab-e2e --test e2e_fps_probe -- --ignored`
//!
//! 判读规则：
//! - 探针缺失（window.__abProbe 未落地）→ 退出码 3 → 本测试按 SKIP 处理；
//! - 环境缺失（node / playwright / CDP 不可达）→ 退出码 4 → SKIP；
//! - 拖拽序列 ≥30fps（95 分位）为 PERF-03 门槛：`AB_E2E_FPS_STRICT=1` 时硬断言，
//!   缺省仅输出参考值（headless 软件渲染结果仅标记参考，qa-perf.md §5 矩阵约束）。

use std::path::PathBuf;
use std::process::Command;

use ab_e2e::fixtures_ref;

fn probe_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fps_probe")
        .join("probe.mjs")
}

#[test]
#[ignore = "需 Tauri dev 环境（C 路 UI + __abProbe 埋点 + WebView2 CDP）"]
fn fps_probe_drag_5s() {
    let script = probe_script();
    if !script.exists() {
        eprintln!("[SKIP] fps_probe: probe.mjs 缺失");
        return;
    }
    let node = Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !node {
        eprintln!("[SKIP] fps_probe: node 不可用");
        return;
    }

    let out = Command::new("node")
        .arg(&script)
        .arg("--cdp-port")
        .arg(std::env::var("AB_CDP_PORT").unwrap_or_else(|_| "9222".to_string()))
        .output()
        .expect("run probe.mjs");

    let exit = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if exit == 3 || exit == 4 {
        eprintln!("[SKIP] fps_probe: {stderr}");
        return;
    }
    if exit != 0 {
        panic!("probe.mjs 退出码 {exit}: {stderr}");
    }
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("probe 输出非 JSON: {e}\n{stdout}"));

    let series = v["fps_series"].as_array().expect("fps_series 数组");
    assert!(!series.is_empty(), "fps 序列为空");
    let mut fps: Vec<f64> = series
        .iter()
        .filter_map(|e| e["fps"].as_f64().or_else(|| e.as_f64()))
        .collect();
    assert!(!fps.is_empty(), "序列不含 fps 值: {stdout}");
    fps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = fps[(fps.len() as f64 * 0.95).floor() as usize];
    let gpu = v["gpu"].as_str().unwrap_or("unknown");
    let first_paint = &v["first_paint_ms"];

    eprintln!(
        "[fps-probe] samples={} p95={p95:.1}fps gpu={gpu} first_paint_ms={first_paint}",
        fps.len()
    );
    let _ = fixtures_ref::workspace_root();
    if std::env::var("AB_E2E_FPS_STRICT").as_deref() == Ok("1") {
        assert!(
            p95 >= 30.0,
            "PERF-03：拖拽 95 分位 {p95:.1}fps < 30fps（严格模式）"
        );
    } else {
        eprintln!("[fps-probe] 参考模式（软渲染不硬判）；PERF-03 以本地实机报告为准");
    }
}
