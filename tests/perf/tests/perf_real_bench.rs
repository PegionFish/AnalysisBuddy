//! P3-05 真实插件基准（qa-perf.md §4.1 测量条件）：builtin-csv × bench_100mb.csv。
//!
//! 显式 ignored：需 release 构建 + `tests/.generated/bench_100mb.csv` 夹具
//! （loggen 固定 seed 100 生成，见 tests/scripts/gen-large-fixtures.ps1 冻结哈希）。
//!
//! 运行：`cargo test -p ab-perf --release --test perf_real_bench -- --ignored --nocapture`
//!
//! 与 perf_bench.rs（mock echo 口径，F 路本地基线）互补：本测试按 qa-perf.md §4.2
//! 指标 1 口径计时——`load_file` 发出 → `records_total` 响应到达（含 load+parse 全程），
//! 冷插件进程逐次拉起 builtin-csv；5 次采样取中位数判定 PERF-01/02/04。
//! PERF-03（fps）探针不可用时记未测量（gpu=null、thresholds_pass[3]=false）。
//!
//! 报告写入 `tests/perf/reports/perf-report-<date>-<sha>.json`（schema 冻结）。
//! 注：与 perf_bench.rs 共用报告文件名（同 sha 同日），本测试须最后运行
//! （run_full_bench.ps1 保证顺序）以覆盖 mock 中间产物，入仓报告为真实插件基线。

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ab_perf::report::{filename, Metrics, PerfReport};
use ab_perf::rss;
use ab_perf::sampling::median;
use ab_perf::thresholds::{judge_median, Thresholds};

/// 8MB 帧上限（protocol §1.3）。
const FRAME_LIMIT: usize = 8 * 1024 * 1024;
/// 每项采样次数（qa-perf.md §4.2：5 次中位数；可用 AB_PERF_REPEATS 覆盖）。
const REPEATS: usize = 5;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
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

/// 单次运行统计。
#[derive(Debug, Clone)]
struct RunStats {
    /// parse 全程（load_file 发出 → records_total 到达，qa-perf.md §4.2 指标 1）。
    parse_ms: f64,
    /// IPC 吞吐（MB/s）：回传字节 ÷ 首帧到末批耗时。
    ipc_mbps: f64,
    records_total: u64,
    sum_records: u64,
    batch_frames: u64,
    seq_ok: bool,
}

/// 驱动一次 builtin-csv 冷进程：initialize → load_file → parse（计时+流）→ shutdown。
/// `sample_rss` 时并行对子进程 200ms 间隔采样峰值（RSS 采样器与 perf_bench 同款）。
fn run_once(
    bin: &Path,
    csv: &Path,
    sample_rss: bool,
) -> Result<(RunStats, Option<(f64, usize)>), String> {
    let mut child = Command::new(bin)
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {bin:?}: {e}"))?;
    let pid = child.id();

    let rss_thread = if sample_rss {
        Some(std::thread::spawn(move || {
            rss::trace_peak(pid, Duration::from_secs(15), Duration::from_millis(200))
        }))
    } else {
        None
    };

    let mut stdin = child.stdin.take().ok_or("stdin pipe")?;
    let reader = BufReader::with_capacity(1 << 20, child.stdout.take().ok_or("stdout pipe")?);
    let mut it = reader.lines();

    let send = |stdin: &mut std::process::ChildStdin, s: &str| -> Result<(), String> {
        stdin
            .write_all(s.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("write: {e}"))
    };

    // initialize（行尾必须为 \n，protocol §1.2）。
    let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"host_info":{"name":"AnalysisBuddy-perf","version":"0.1.0"}}}"#;
    send(&mut stdin, init_req)?;
    let mut initialized = false;
    for _ in 0..3 {
        let line = read_line(&mut it)?;
        let v: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| format!("init frame: {e}"))?;
        if v.get("id").and_then(serde_json::Value::as_u64) == Some(1) && v.get("result").is_some() {
            initialized = true;
            break;
        }
    }
    if !initialized {
        let _ = child.kill();
        return Err("initialize 握手失败".to_string());
    }

    // load_file（路径 JSON 转义：反斜杠 → \\）。
    let file_id = "f-perf";
    let csv_escaped = csv.to_string_lossy().replace('\\', "\\\\");
    let load_req = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"load_file","params":{{"file_id":"{file_id}","path":"{csv_escaped}"}}}}"#
    );
    // 指标 1 口径起点：load_file 发出。
    let t_load_sent = Instant::now();
    send(&mut stdin, &load_req)?;
    let mut loaded = false;
    for _ in 0..3 {
        let line = read_line(&mut it)?;
        let v: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| format!("load frame: {e}"))?;
        if v.get("id").and_then(serde_json::Value::as_u64) == Some(2) && v.get("result").is_some() {
            loaded = true;
            break;
        }
    }
    if !loaded {
        let _ = child.kill();
        return Err("load_file 失败".to_string());
    }

    // parse：计时 + 字节计数。
    let parse_req = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"parse","params":{{"file_id":"{file_id}"}}}}"#
    );
    send(&mut stdin, &parse_req)?;

    let mut seq_ok = true;
    let mut sum_records = 0u64;
    let mut batch_frames = 0u64;
    let mut records_total = 0u64;
    let mut response_seen = false;
    let mut done_arrival: Option<Instant> = None;
    let mut first_frame: Option<Instant> = None;
    let mut pre_window_bytes = 0u64;
    let mut line_bytes_total = 0u64;
    let mut expected_seq = 0u64;
    let mut seq_violations: Vec<(u64, u64)> = Vec::new();

    while let Ok(line) = read_line(&mut it) {
        let now = Instant::now();
        if line.len() + 1 > FRAME_LIMIT {
            return Err(format!("帧超 8MB（{} 字节）", line.len() + 1));
        }
        let frame_bytes = line.len() as u64 + 1;
        line_bytes_total += frame_bytes;
        if first_frame.is_none() {
            pre_window_bytes = line_bytes_total - frame_bytes;
            first_frame = Some(now);
        }
        let v: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| format!("frame JSON: {e}"))?;
        match v.get("method").and_then(serde_json::Value::as_str) {
            Some("RecordBatch") => {
                batch_frames += 1;
                let params = v.get("params").cloned().unwrap_or(serde_json::Value::Null);
                let seq = params
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(u64::MAX);
                if seq != expected_seq {
                    seq_ok = false;
                    seq_violations.push((expected_seq, seq));
                }
                expected_seq = seq + 1;
                let recs = params
                    .get("records")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| a.len() as u64)
                    .unwrap_or(0);
                sum_records += recs;
                if params.get("done").and_then(serde_json::Value::as_bool) == Some(true) {
                    done_arrival = Some(now);
                }
            }
            _ => {
                if v.get("id").and_then(serde_json::Value::as_u64) == Some(3) {
                    response_seen = true;
                    if let Some(r) = v.get("result") {
                        records_total = r
                            .get("records_total")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                    }
                }
            }
        }
        if done_arrival.is_some() && response_seen {
            break;
        }
    }

    // 收尾：shutdown + 等子进程退出。
    let shutdown_req = r#"{"jsonrpc":"2.0","id":9,"method":"shutdown","params":{}}"#;
    let _ = send(&mut stdin, shutdown_req);
    drop(stdin);
    let _ = child.wait();

    let parse_elapsed = t_load_sent.elapsed();
    let window_elapsed = done_arrival
        .map(|d| d.duration_since(first_frame.unwrap_or(t_load_sent)))
        .unwrap_or(parse_elapsed);
    let ipc_mbps = if window_elapsed.is_zero() {
        0.0
    } else {
        (line_bytes_total - pre_window_bytes) as f64 / 1_000_000.0 / window_elapsed.as_secs_f64()
    };

    if !seq_ok {
        eprintln!(
            "[real-bench] seq violations (expected, got): {:?}",
            seq_violations
        );
    }

    let stats = RunStats {
        parse_ms: parse_elapsed.as_secs_f64() * 1000.0,
        ipc_mbps,
        records_total,
        sum_records,
        batch_frames,
        seq_ok,
    };
    let rss = match rss_thread {
        Some(t) => Some(t.join().map_err(|_| "RSS 采样线程 panic".to_string())??),
        None => None,
    };
    Ok((stats, rss))
}

fn read_line(
    it: &mut impl Iterator<Item = Result<String, std::io::Error>>,
) -> Result<String, String> {
    it.next()
        .ok_or_else(|| "stdout EOF".to_string())?
        .map_err(|e| format!("read line: {e}"))
}

fn assert_stream_ok(st: &RunStats) -> Result<(), String> {
    if !st.seq_ok {
        return Err("seq 不连续".to_string());
    }
    if st.sum_records != st.records_total || st.sum_records == 0 {
        return Err(format!(
            "records_total 不一致: {} vs 批次和 {}",
            st.records_total, st.sum_records
        ));
    }
    if st.batch_frames == 0 {
        return Err("无 RecordBatch 帧".to_string());
    }
    Ok(())
}

/// 调用 fps 探针（Tauri dev 可用时）；不可用返回 None（PERF-03 未测量）。
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
    let fps_p95 = ab_perf::sampling::p95(&mut fps);
    let first_paint = v["first_paint_ms"].as_f64();
    let gpu = v["gpu"].as_str().unwrap_or("unknown").to_string();
    Some((first_paint.unwrap_or(f64::NAN), fps_p95, gpu))
}

/// 单档采样结果（中位数）。
#[derive(Debug, Clone)]
struct TierResult {
    parse_ms: f64,
    ipc_mbps: f64,
    rss_peak_mb: f64,
}

/// 对一档夹具做 1 次预热（丢弃）+ N 次采样，返回各项中位数。
fn sample_tier(
    bin: &Path,
    fixture: &Path,
    label: &str,
    repeats: usize,
) -> Result<TierResult, String> {
    let fixture_bytes = std::fs::metadata(fixture).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "[real-bench] tier {label}: {} ({:.1}MB), plugin: {}",
        fixture.file_name().unwrap_or_default().to_string_lossy(),
        fixture_bytes as f64 / 1_048_576.0,
        bin.display()
    );

    // 预热 1 次丢弃（文件系统缓存预热，qa-perf.md §4.3）。
    let (warmup, _) = run_once(bin, fixture, false)?;
    assert_stream_ok(&warmup)?;
    eprintln!(
        "[real-bench] {label} warmup 丢弃: parse={:.1}ms records={} batches={}",
        warmup.parse_ms, warmup.records_total, warmup.batch_frames
    );

    let mut parse_ms: Vec<f64> = Vec::new();
    let mut ipc_mbps: Vec<f64> = Vec::new();
    let mut rss_peaks: Vec<f64> = Vec::new();
    for i in 0..repeats {
        let (st, rss) = run_once(bin, fixture, true)?;
        assert_stream_ok(&st)?;
        parse_ms.push(st.parse_ms);
        ipc_mbps.push(st.ipc_mbps);
        let (rss_mb, rss_samples) = rss.expect("RSS 采样缺失");
        rss_peaks.push(rss_mb);
        eprintln!(
            "[real-bench] {label} run {i}: parse={:.1}ms ipc={:.1}MB/s rss_peak={:.1}MB ({}点) records={} batches={}",
            st.parse_ms,
            st.ipc_mbps,
            rss_mb,
            rss_samples,
            st.records_total,
            st.batch_frames
        );
    }
    let r = TierResult {
        parse_ms: median(&mut parse_ms),
        ipc_mbps: median(&mut ipc_mbps),
        rss_peak_mb: median(&mut rss_peaks),
    };
    eprintln!(
        "[real-bench] {label} 中位数: parse={:.1}ms ipc={:.1}MB/s rss={:.1}MB（{repeats} 次采样，预热已弃）",
        r.parse_ms, r.ipc_mbps, r.rss_peak_mb
    );
    Ok(r)
}

#[test]
#[ignore = "需 release 构建 + tests/.generated/ 夹具；P3-05 真实插件基准入口"]
fn perf_real_bench_builtin_csv_100mb() {
    if cfg!(debug_assertions) {
        eprintln!(
            "[SKIP] perf_real_bench 需 release 构建（`cargo test --release`）；debug 数据不入报告"
        );
        return;
    }

    let ws = workspace_root();
    let bin = ws
        .join("plugins")
        .join("builtin-csv")
        .join("target")
        .join("release")
        .join("builtin-csv.exe");
    assert!(bin.exists(), "builtin-csv release 缺失: {}", bin.display());

    let gen = ws.join("tests").join(".generated");
    let tiers: [(&str, &str); 3] = [
        ("bench_10mb.csv", "10MB"),
        ("bench_50mb.csv", "50MB"),
        ("bench_100mb.csv", "100MB"),
    ];
    for (name, _) in tiers {
        let f = gen.join(name);
        assert!(
            f.exists(),
            "夹具缺失: {}（先跑 tests/scripts/gen-large-fixtures.ps1 生成）",
            f.display()
        );
    }

    let repeats = std::env::var("AB_PERF_REPEATS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(REPEATS);
    assert!(
        repeats >= 3,
        "AB_PERF_REPEATS 必须 ≥3（采样纪律：5 次中位数，qa-perf.md §4.2）"
    );

    // 三档全采样（10/50/100MB × parse/RSS/IPC）；100MB 档为硬性门槛判定档。
    let mut tier_results: Vec<(String, TierResult)> = Vec::new();
    for (name, label) in tiers {
        let r = sample_tier(&bin, &gen.join(name), label, repeats)
            .unwrap_or_else(|e| panic!("tier {label}: {e}"));
        tier_results.push((label.to_string(), r));
    }
    let hundred = &tier_results[2].1;

    // ④⑤ 前端探针（Tauri dev 可用时）。
    let (first_paint_ms, drag_fps_p95, gpu) = match probe_frontend() {
        Some((fp, fps, gpu)) => {
            eprintln!("[real-bench] ④ 首屏出图 {fp}ms；⑤ 拖拽帧率 p95 {fps:.1}fps（gpu={gpu}）");
            (Some(fp), Some(fps), Some(gpu))
        }
        None => {
            eprintln!("[real-bench] ④⑤ 探针不可用（Tauri dev 未起），记未测量");
            (None, None, None)
        }
    };

    // 门槛判定（PERF-01..04，硬性门槛；单位换算：parse 采样 ms → s）。
    // 下标顺序 [parse, rss, ipc, fps] = PERF-01/02/04/03。
    // PERF-03 未测量（探针不可用、gpu=null）→ thresholds_pass[3]=false 表示「未测量」，
    // 门禁（report::gate_failures）按 metrics 跳过，不判不达标。
    let thresholds = Thresholds::full();
    let parse_secs: Vec<f64> = vec![hundred.parse_ms / 1000.0];
    let ipc_samples: Vec<f64> = vec![hundred.ipc_mbps];
    let pass = judge_median(
        &parse_secs,
        Some(hundred.rss_peak_mb),
        &ipc_samples,
        drag_fps_p95,
        &thresholds,
    );
    eprintln!(
        "[real-bench] thresholds_pass PERF-01..04 = {pass:?}（①{:.1}ms/≤{:.1}s ②{:.1}MB/≤{:.1}MB ③{:.1}MB/s/≥{:.1} ④⑤{:?}/≥{:.1}fps）",
        hundred.parse_ms,
        thresholds.parse_secs,
        hundred.rss_peak_mb,
        thresholds.rss_mb,
        hundred.ipc_mbps,
        thresholds.ipc_mbps,
        drag_fps_p95,
        thresholds.drag_fps_p95
    );

    // 报告 JSON 入仓（schema 冻结；machine/gpu 为既有扩展字段）。
    let report = PerfReport {
        git_sha: git_sha(),
        arch: std::env::consts::ARCH.to_string(),
        machine: machine_name(),
        gpu,
        fixture: "bench_100mb.csv × builtin-csv（真实插件，release+LTO 冷进程，load_file 发出→records_total 全程，5 次中位数）".to_string(),
        metrics: Metrics {
            parse_ms: Some(hundred.parse_ms),
            rss_peak_mb: Some(hundred.rss_peak_mb),
            ipc_mbps: Some(hundred.ipc_mbps),
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
    eprintln!("[real-bench] 报告已写: {}", path.display());
}
