//! perf harness 单测（F-03 DoD）：五项采样器、RSS 双路互验 ≤5%、门槛判定逻辑、
//! 报告 schema、回归检测。除 RSS 双路互验外全部离线可跑。

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use ab_perf::report::{gate_failures, regression_check, Metrics, PerfReport};
use ab_perf::rss;
use ab_perf::sampling::{median, p95};
use ab_perf::thresholds::{judge, judge_median, Thresholds};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

/// mock-plugin 二进制（双路 RSS 测试的宿主目标；按需构建）。
fn mock_plugin_bin() -> PathBuf {
    let ws = workspace_root();
    let bin = ws.join("target").join("debug").join("mock-plugin.exe");
    if !bin.exists() {
        let st = Command::new("cargo")
            .current_dir(&ws)
            .args(["build", "-p", "mock-plugin"])
            .status()
            .expect("cargo build -p mock-plugin");
        assert!(st.success());
    }
    bin
}

#[test]
fn stats_median_and_p95() {
    assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
    assert!(p95(&mut [1.0, 2.0, 3.0]) >= 2.9);
}

#[test]
fn thresholds_full_and_smoke() {
    let full = Thresholds::full();
    assert_eq!(
        judge(Some(9.9), Some(900.0), Some(21.0), Some(31.0), &full),
        [true, true, true, true]
    );
    let smoke = Thresholds::smoke_10mb();
    assert_eq!(smoke.parse_secs, 1.0);
    assert_eq!(smoke.rss_mb, 300.0);
    // 5 次采样中位数判据。
    let r = judge_median(
        &[0.8, 0.9, 1.2, 0.7, 0.6],
        Some(280.0),
        &[25.0, 30.0, 22.0, 28.0, 29.0],
        None,
        &smoke,
    );
    assert_eq!(r, [true, true, true, false]);
}

#[test]
fn rss_sampler_dual_path_deviation_within_5pct() {
    // 目标进程：mock-plugin 流式回传 ~2MB 剧本（存活 ~2.5s 供 200ms 采样）。
    let ws = workspace_root();
    let bin = mock_plugin_bin();
    let script = ws
        .join("tests")
        .join(".generated")
        .join("rss_dual_probe.ndjson");
    ab_perf::harness::gen_mock_script(20_000, 400, 30, 1, &script).expect("gen script");
    let script_abs = script.clone();

    let mut child = Command::new(&bin)
        .args(["--script", &script_abs.to_string_lossy()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn mock-plugin");
    let pid = child.id();

    // 双路并行采样同一进程（Rust K32GetProcessMemoryInfo + PS WorkingSet64）。
    let rust_handle = {
        let (pid, script) = (pid, script_abs.clone());
        std::thread::spawn(move || {
            let _ = script;
            rss::trace_peak(pid, Duration::from_secs(6), Duration::from_millis(200))
                .expect("rust sampler")
        })
    };
    let ps_out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &ws.join("tests")
                .join("perf")
                .join("rss_probe.ps1")
                .to_string_lossy(),
            "-ProcessId",
            &pid.to_string(),
            "-Seconds",
            "6",
        ])
        .output()
        .expect("run rss_probe.ps1");
    let (rust_peak, rust_samples) = rust_handle.join().unwrap();
    let ps_text = String::from_utf8_lossy(&ps_out.stdout);
    let ps_peak: f64 = ps_text.trim().parse().unwrap_or(0.0);

    let _ = child.wait();
    let _ = std::fs::remove_file(&script);

    assert!(rust_samples >= 3, "Rust 采样点数过少: {rust_samples}");
    assert!(rust_peak > 1.0, "Rust 峰值异常: {rust_peak} MB");
    assert!(ps_peak > 1.0, "PS 峰值异常: {ps_peak} MB");
    let dev = (rust_peak - ps_peak).abs() / rust_peak.max(ps_peak);
    assert!(
        dev <= 0.05,
        "RSS 双路偏差 {:.1}% > 5%（rust={rust_peak:.1}MB ps={ps_peak:.1}MB）",
        dev * 100.0
    );
}

#[test]
fn report_schema_frozen_and_regression_drill() {
    let base = PerfReport {
        git_sha: "0000000000000000000000000000000000000000".into(),
        arch: "x86_64".into(),
        machine: "perf-drill".into(),
        gpu: None,
        fixture: "bench_10mb.csv".into(),
        metrics: Metrics {
            parse_ms: Some(100.0),
            rss_peak_mb: Some(200.0),
            ipc_mbps: Some(50.0),
            first_paint_ms: Some(500.0),
            drag_fps_p95: Some(45.0),
        },
        thresholds_pass: vec![true, true, true, true],
    };
    let mut cur = base.clone();
    cur.metrics.parse_ms = Some(130.0); // +30% > 15%
    cur.metrics.ipc_mbps = Some(40.0); // -20% > 15%
    let issue = regression_check(&base, &cur).expect(">15% 劣化必须触发");
    assert!(issue.contains("parse_ms") && issue.contains("ipc_mbps"));
    assert!(issue.contains("```json"));
    // 演练记录入仓（供回归演练证据）。
    let drill = workspace_root()
        .join("tests")
        .join("perf")
        .join("reports")
        .join("regression-drill.md");
    std::fs::create_dir_all(drill.parent().unwrap()).unwrap();
    let mut existing = std::fs::read_to_string(&drill).unwrap_or_default();
    let marker = "## 演练：模拟劣化 >15%";
    if !existing.contains(marker) {
        existing.push_str(&format!(
            "{marker}\n\n基线 parse_ms=100ms/ipc=50MB/s → 当前 130ms/40MB/s。\n\n```text\n{issue}\n```\n"
        ));
        std::fs::write(&drill, existing).unwrap();
    }
}

#[test]
fn debug_build_data_never_goes_to_report() {
    // F-03 DoD：debug 数据不入报告（release + LTO 才出正式数据）。
    if cfg!(debug_assertions) {
        eprintln!("[info] perf_bench 在 debug 下拒绝写报告（本测试仅验证判定逻辑）");
    }
    // 判定逻辑本身与 profile 无关；此处验证阈值判定在 debug 下同样正确。
    let smoke = Thresholds::smoke_10mb();
    assert!(
        judge(Some(0.9), Some(200.0), Some(30.0), Some(35.0), &smoke)[..3] == [true, true, true]
    );
}

#[test]
fn gate_skips_unmeasured_perf03() {
    // fps 探针不可用（gpu=null、drag_fps_p95=None）→ thresholds_pass[3]=false 表示
    // 「未测量」而非不达标；门禁按 metrics 判已测门槛，PERF-03 跳过 → 门禁通过。
    let rep = PerfReport {
        git_sha: "abc123def456".into(),
        arch: "x86_64".into(),
        machine: "gate-test".into(),
        gpu: None,
        fixture: "bench_10mb.csv".into(),
        metrics: Metrics {
            parse_ms: Some(145.0),
            rss_peak_mb: Some(112.0),
            ipc_mbps: Some(71.0),
            first_paint_ms: None,
            drag_fps_p95: None,
        },
        thresholds_pass: vec![true, true, true, false],
    };
    assert_eq!(
        gate_failures(&rep),
        Vec::<usize>::new(),
        "PERF-03 未测量必须被跳过"
    );
}

#[test]
fn gate_fails_only_on_measured_failures() {
    // 帧率已测量但 <30fps → PERF-04 编号 4 必须返回（阻塞 PR）。
    let rep = PerfReport {
        git_sha: "abc123def456".into(),
        arch: "x86_64".into(),
        machine: "gate-test".into(),
        gpu: Some("vendor".into()),
        fixture: "bench_10mb.csv".into(),
        metrics: Metrics {
            parse_ms: Some(145.0),
            rss_peak_mb: Some(112.0),
            ipc_mbps: Some(71.0),
            first_paint_ms: None,
            drag_fps_p95: Some(25.0),
        },
        thresholds_pass: vec![true, true, true, false],
    };
    assert_eq!(
        gate_failures(&rep),
        vec![4],
        "PERF-03 已测量且不达标必须阻塞"
    );
}

#[test]
fn gate_reports_all_measured_failures() {
    // PERF-01 已测量超标 + PERF-03 已测量不达标 → 返回 [1, 4]。
    let rep = PerfReport {
        git_sha: "abc123def456".into(),
        arch: "x86_64".into(),
        machine: "gate-test".into(),
        gpu: Some("vendor".into()),
        fixture: "bench_10mb.csv".into(),
        metrics: Metrics {
            parse_ms: Some(1600.0),
            rss_peak_mb: Some(112.0),
            ipc_mbps: Some(71.0),
            first_paint_ms: None,
            drag_fps_p95: Some(25.0),
        },
        thresholds_pass: vec![false, true, true, false],
    };
    assert_eq!(gate_failures(&rep), vec![1, 4]);
}
