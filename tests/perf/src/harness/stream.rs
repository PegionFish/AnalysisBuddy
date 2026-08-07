//! IPC 流驱动（qa-perf.md §4.2 指标 1/3，echo 口径与 scratch/echo-driver 同源）：
//! 计时打点（load_file 发出 → records_total 到达）、stdout 回传字节累计、
//! 传输窗口（首帧 → done:true 帧）、8MB 帧上限防护、seq/总和一致性校验。

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

/// 8MB 帧上限（protocol §1.3）。
const FRAME_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    pub records_total: u64,
    pub batch_frames: u64,
    pub progress_frames: u64,
    /// 传输窗口字节：首帧 → done:true 帧（qa-perf.md §4.2 指标 3 口径）。
    pub window_bytes: u64,
    pub window_elapsed: Duration,
    /// parse 全程：parse 请求发出 → records_total 响应到达（PERF-01 口径）。
    pub parse_elapsed: Duration,
    pub seq_ok: bool,
    pub sum_ok: bool,
    pub max_frame_bytes: usize,
}

pub type Duration = std::time::Duration;

/// 驱动一次 mock 流式 parse：initialize → load_file → parse（含流）→ shutdown。
pub fn run_stream(plugin_exe: &Path, script: &Path) -> Result<StreamStats, String> {
    let mut child = Command::new(plugin_exe)
        .args(["--script", &script.to_string_lossy()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {plugin_exe:?}: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("stdin pipe")?;
    let reader = BufReader::with_capacity(1 << 20, child.stdout.take().ok_or("stdout pipe")?);
    let mut it = reader.lines();

    // initialize。注意：JSON-RPC over stdio 行尾必须为 `\n`（protocol §1.2）——
    // 无行尾的行 mock-plugin 的 `lines()` 在 EOF 前不会交付。
    let init_req =
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"host_info":{"name":"AnalysisBuddy-perf","version":"0.1.0"}}}"#;
    stdin
        .write_all(init_req.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|e| format!("write init: {e}"))?;
    let mut initialized = false;
    for _ in 0..3 {
        let line = read_line(&mut it)?;
        let v: serde_json::Value = serde_json::from_str(&line).map_err(|e| format!("init frame: {e}"))?;
        if v.get("id").and_then(serde_json::Value::as_u64) == Some(1) && v.get("result").is_some() {
            initialized = true;
            break;
        }
    }
    if !initialized {
        let _ = child.kill();
        return Err("initialize 握手失败".to_string());
    }

    // load_file。路径必须做 JSON 转义（反斜杠 → \\），否则插件拒帧导致无响应。
    let file_id = "f-perf";
    let script_escaped = script.to_string_lossy().replace('\\', "\\\\");
    let load_req = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"load_file","params":{{"file_id":"{file_id}","path":"{script_escaped}"}}}}"#
    );
    stdin
        .write_all(load_req.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|e| format!("write load_file: {e}"))?;
    let mut loaded = false;
    for _ in 0..3 {
        let line = read_line(&mut it)?;
        let v: serde_json::Value = serde_json::from_str(&line).map_err(|e| format!("load frame: {e}"))?;
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
    let parse_t0 = Instant::now();
    let parse_req = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"parse","params":{{"file_id":"{file_id}"}}}}"#
    );
    stdin
        .write_all(parse_req.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|e| format!("write parse: {e}"))?;

    let mut stats = StreamStats {
        seq_ok: true,
        sum_ok: true,
        ..StreamStats::default()
    };
    let mut first_frame: Option<Instant> = None;
    let mut done_arrival: Option<Instant> = None;
    let mut pre_window_bytes: u64 = 0;
    let mut line_bytes_total: u64 = 0;
    let mut expected_seq = 0u64;
    let mut seq_violations: Vec<(u64, u64)> = Vec::new();
    let mut sum_records = 0u64;
    let mut response_seen = false;
    let mut records_total: u64 = 0;

    while let Ok(line) = read_line(&mut it) {
        let now = Instant::now();
        if line.len() + 1 > FRAME_LIMIT {
            return Err(format!("帧超 8MB（{} 字节）", line.len() + 1));
        }
        let frame_bytes = line.len() as u64 + 1;
        stats.max_frame_bytes = stats.max_frame_bytes.max(line.len() + 1);
        line_bytes_total += frame_bytes;
        if first_frame.is_none() {
            pre_window_bytes = line_bytes_total - frame_bytes;
            first_frame = Some(now);
        }
        let v: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| format!("frame JSON: {e}"))?;
        match v.get("method").and_then(serde_json::Value::as_str) {
            Some("progress") => stats.progress_frames += 1,
            Some("RecordBatch") => {
                stats.batch_frames += 1;
                let params = v.get("params").cloned().unwrap_or(serde_json::Value::Null);
                let seq = params.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(u64::MAX);
                if seq != expected_seq {
                    stats.seq_ok = false;
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
                    stats.window_bytes = line_bytes_total - pre_window_bytes;
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

    stats.parse_elapsed = parse_t0.elapsed();
    if !stats.seq_ok {
        eprintln!(
            "[stream] seq violations (expected, got): {:?} (total frames read {})",
            seq_violations,
            stats.batch_frames
        );
    }
    if let Some(d) = done_arrival {
        stats.window_elapsed = d.duration_since(first_frame.unwrap_or(parse_t0));
    }
    stats.records_total = records_total;
    stats.sum_ok = sum_records == records_total && sum_records > 0;
    if stats.window_elapsed.is_zero() {
        stats.window_elapsed = Duration::from_millis(1);
    }

    drop(stdin);
    let _ = child.wait();
    Ok(stats)
}

fn read_line(it: &mut impl Iterator<Item = Result<String, std::io::Error>>) -> Result<String, String> {
    it.next()
        .ok_or_else(|| "stdout EOF".to_string())?
        .map_err(|e| format!("read line: {e}"))
}

/// 校验流完整性（seq 连续、总和一致、done 到达）。
pub fn assert_stream_ok(stats: &StreamStats, expected_records: u64) -> Result<(), String> {
    if !stats.seq_ok {
        return Err("seq 不连续".to_string());
    }
    if !stats.sum_ok {
        return Err(format!(
            "records_total 不一致: {} vs 批次和 {}",
            stats.records_total, expected_records
        ));
    }
    if stats.batch_frames == 0 {
        return Err("无 RecordBatch 帧".to_string());
    }
    Ok(())
}
