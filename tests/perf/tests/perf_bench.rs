//! perf 基准实测（qa-perf.md §4）——显式 ignored，需 release 构建 + 大档/脚本夹具。
//!
//! 运行：`cargo test -p ab-perf --release --test perf_bench -- --ignored --nocapture`
//!
//! 测量内容（本机基线，供 P3-05 报告）：
//! ① parse 耗时（load_file → records_total，5 次中位数）；② RSS 峰值（Rust 采样器，
//!    200ms 间隔）；③ IPC 吞吐（回传字节 ÷ 传输窗口，5 次中位数）；④⑤ 首屏出图 /
//!    拖拽帧率（fps 探针，Tauri dev 可用时）。
//!
//! 判据：PERF-01..04 门槛（qa-perf.md §4.1/§5；`AB_PERF_MODE=smoke` 时按 10MB 等比折算，
//! 见 `Thresholds::for_mode`）；debug 构建拒绝写报告。
//! 报告写入 `tests/perf/reports/perf-report-<date>-<sha>.json`（schema 冻结）。

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ab_perf::harness::{assert_stream_ok, gen_mock_script, run_stream};
use ab_perf::report::{filename, Metrics, PerfReport};
use ab_perf::rss;
use ab_perf::sampling::{median, p95};
use ab_perf::thresholds::{judge_median, Thresholds, MODE_ENV};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

fn generated_dir() -> PathBuf {
    workspace_root().join("tests").join(".generated")
}

fn mock_plugin_release_bin() -> PathBuf {
    let ws = workspace_root();
    let bin = ws.join("target").join("release").join("mock-plugin.exe");
    if !bin.exists() {
        let st = Command::new("cargo")
            .current_dir(&ws)
            .args(["build", "--release", "-p", "mock-plugin"])
            .status()
            .expect("cargo build --release -p mock-plugin");
        assert!(st.success(), "release mock-plugin 构建失败");
    }
    bin
}

fn git_sha() -> String {
    let out = Command::new("git")
        .current_dir(workspace_root())
        .args(["rev-parse", "HEAD"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    out
}

fn machine_name() -> String {
    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into());
    let cpu = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".into());
    format!("{name} ({cpu})")
}

/// 调用 fps 探针（Tauri dev 可用时）；不可用返回 None。
fn probe_frontend() -> Option<(f64, f64, String)> {
    let script = workspace_root()
        .join("tests")
        .join("e2e")
        .join("fps_probe")
        .join("probe.mjs");
    if !script.exists() {
        return None;
    }
    let out = Command::new("node")
        .arg(&script)
        .arg("--cdp-port")
        .arg(std::env::var("AB_CDP_PORT").unwrap_or_else(|_| "9222".into()))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let series = v["fps_series"].as_array()?;
    let mut fps: Vec<f64> = series
        .iter()
        .filter_map(|e| e["fps"].as_f64().or_else(|| e.as_f64()))
        .collect();
    if fps.is_empty() {
        return None;
    }
    let fps_p95 = p95(&mut fps);
    let first_paint = v["first_paint_ms"].as_f64();
    let gpu = v["gpu"].as_str().unwrap_or("unknown").to_string();
    Some((first_paint.unwrap_or(f64::NAN), fps_p95, gpu))
}

#[test]
#[ignore = "需 release 构建 + 夹具生成；F-03 perf 实测入口"]
fn perf_bench_local_baseline() {
    // 采样纪律：release + LTO 数据才入报告（qa-perf.md §4.3）。
    if cfg!(debug_assertions) {
        eprintln!(
            "[SKIP] perf_bench 需 release 构建（`cargo test --release`）；debug 数据不入报告"
        );
        return;
    }

    let ws = workspace_root();
    let gen = generated_dir();
    std::fs::create_dir_all(&gen).expect("create tests/.generated");

    // 剧本：~10MB 回传流（echo 口径；fast = 纯流式，slow = 批间 40ms 供 RSS 采样）。
    const RECORDS: u64 = 180_000;
    const BATCH: usize = 4000;
    let fast = gen.join("mock_bench_10mb_fast.ndjson");
    let slow = gen.join("mock_bench_10mb_slow.ndjson");
    let t0 = Instant::now();
    gen_mock_script(RECORDS, BATCH, 0, 10, &fast).expect("gen fast script");
    gen_mock_script(RECORDS, BATCH, 40, 10, &slow).expect("gen slow script");
    eprintln!("[bench] 剧本生成 {}ms", t0.elapsed().as_millis());

    let plugin = mock_plugin_release_bin();
    let fast_bytes = std::fs::metadata(&fast).unwrap().len();
    eprintln!(
        "[bench] fixture: mock_stream_10mb ({} KB), plugin: {}",
        fast_bytes / 1024,
        plugin.display()
    );

    // ① parse 耗时 + ③ IPC 吞吐：5 次取中位数。
    let mut parse_ms: Vec<f64> = Vec::new();
    let mut ipc_mbps: Vec<f64> = Vec::new();
    for i in 0..5 {
        let st = run_stream(&plugin, &fast).unwrap_or_else(|e| panic!("run {i}: {e}"));
        assert_stream_ok(&st, RECORDS).unwrap_or_else(|e| panic!("run {i} 完整性: {e}"));
        parse_ms.push(st.parse_elapsed.as_secs_f64() * 1000.0);
        let mbps = st.window_bytes as f64 / 1_000_000.0 / st.window_elapsed.as_secs_f64();
        ipc_mbps.push(mbps);
        eprintln!(
            "[bench] run {i}: parse={:.1}ms ipc={mbps:.1}MB/s batches={} max_frame={}KB",
            st.parse_elapsed.as_secs_f64() * 1000.0,
            st.batch_frames,
            st.max_frame_bytes / 1024
        );
    }
    let parse_ms_med = median(&mut parse_ms);
    let ipc_mbps_med = median(&mut ipc_mbps);
    eprintln!("[bench] ① parse 中位数 {parse_ms_med:.1}ms；③ IPC 中位数 {ipc_mbps_med:.1}MB/s");

    // ② RSS 峰值：slow 流期间 Rust 采样器 200ms 间隔。
    let pid_holder = {
        let plugin = plugin.clone();
        let slow = slow.clone();
        std::thread::spawn(move || run_stream(&plugin, &slow))
    };
    // 找到子进程 pid：由 run_stream 内部生成，此处无法直接取 → 改用 trace 自身已知 pid 的
    // 方案不可行；改为在采样线程里枚举同名进程。
    let plugin_name = plugin.file_name().unwrap().to_string_lossy().to_string();
    let rss_peak = std::thread::spawn(move || {
        // 轮询找到 mock-plugin 子进程后按 200ms 采样 5s。
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(out) = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!("(Get-Process -Name '{0}' -ErrorAction SilentlyContinue | Select-Object -First 1).Id", plugin_name.trim_end_matches(".exe")),
                ])
                .output()
            {
                let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
                if pid > 0 {
                    return rss::trace_peak(pid, Duration::from_secs(6), Duration::from_millis(200));
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err("未找到 mock-plugin 进程".to_string())
    });
    let stream_result = pid_holder.join().unwrap();
    let stream_stats = stream_result.unwrap_or_else(|e| panic!("slow 流失败: {e}"));
    assert_stream_ok(&stream_stats, RECORDS).expect("slow 流完整性");
    let (rss_peak_mb, rss_samples) = rss_peak
        .join()
        .unwrap()
        .unwrap_or_else(|e| panic!("RSS 采样失败: {e}"));
    eprintln!("[bench] ② RSS 峰值 {rss_peak_mb:.1}MB（{rss_samples} 个采样点）");

    // ④⑤ 前端探针（Tauri dev 可用时）。
    let (first_paint_ms, drag_fps_p95, gpu) = match probe_frontend() {
        Some((fp, fps, gpu)) => {
            eprintln!("[bench] ④ 首屏出图 {fp}ms；⑤ 拖拽帧率 p95 {fps:.1}fps（gpu={gpu}）");
            (Some(fp), Some(fps), Some(gpu))
        }
        None => {
            eprintln!("[bench] ④⑤ 探针不可用（Tauri dev 未起/探针未落地），记 None");
            (None, None, None)
        }
    };

    // 门槛判定（PERF-01..04；按运行模式取门槛：perf-smoke 置 AB_PERF_MODE=smoke →
    // 10MB 等比折算 parse ≤1s/RSS ≤300MB，其余默认硬性门槛）。
    // 注意单位：门槛表 parse 用秒（≤10s），采样以 ms 记录 → 换算后判定。
    // PERF-03 未测量（探针不可用、gpu=null）→ thresholds_pass[3]=false 表示「未测量」，
    // 门禁（perf-smoke.yml Gate step / report::gate_failures）按 metrics 跳过，不判不达标。
    let parse_secs: Vec<f64> = parse_ms.iter().map(|m| m / 1000.0).collect();
    let thresholds = Thresholds::for_mode(std::env::var(MODE_ENV).ok().as_deref());
    let pass = judge_median(
        &parse_secs,
        Some(rss_peak_mb),
        &ipc_mbps,
        drag_fps_p95,
        &thresholds,
    );
    eprintln!(
        "[bench] thresholds_pass PERF-01..04 = {pass:?}（①{:.1}ms/≤{:.1}s ②{:.1}MB/≤{:.1}MB ③{:.1}MB/s/≥{:.1} ④⑤{:?}/≥{:.1}fps，mode={:?}）",
        parse_ms_med,
        thresholds.parse_secs,
        rss_peak_mb,
        thresholds.rss_mb,
        ipc_mbps_med,
        thresholds.ipc_mbps,
        drag_fps_p95,
        thresholds.drag_fps_p95,
        std::env::var(MODE_ENV).ok()
    );

    // 报告 JSON 入仓（schema 冻结）。
    let report = PerfReport {
        git_sha: git_sha(),
        arch: std::env::consts::ARCH.to_string(),
        machine: machine_name(),
        gpu,
        fixture: "mock_stream_10mb (echo 口径; 正式基线待宿主+builtin-csv)".to_string(),
        metrics: Metrics {
            parse_ms: Some(parse_ms_med),
            rss_peak_mb: Some(rss_peak_mb),
            ipc_mbps: Some(ipc_mbps_med),
            first_paint_ms,
            drag_fps_p95,
        },
        thresholds_pass: pass.to_vec(),
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let name = filename(&report.git_sha, now);
    let reports_dir = ws.join("tests").join("perf").join("reports");
    std::fs::create_dir_all(&reports_dir).expect("create reports dir");
    let path = reports_dir.join(&name);
    let json = serde_json::to_string_pretty(&report).expect("serialize report");
    std::fs::write(&path, json).expect("write report");
    eprintln!("[bench] 报告已写: {}", path.display());
}
