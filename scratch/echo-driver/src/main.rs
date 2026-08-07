use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use serde_json::Value;

struct Stats {
    batch: usize,
    records: usize,
    bytes: u64,
    max_line_bytes: usize,
    progress_gap_ms: f64,
    batch_frames: u64,
    progress_frames: u64,
    seq_ok: bool,
    sum_ok: bool,
    records_total: u64,
    done_seen: bool,
    elapsed_ms: f64,
}

fn main() {
    let mut plugin: Option<String> = None;
    let mut batch = 1000usize;
    let mut records = 100_000usize;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--plugin" => plugin = args.next(),
            "--batch" => batch = args.next().and_then(|v| v.parse().ok()).expect("--batch <N>"),
            "--records" => records = args.next().and_then(|v| v.parse().ok()).expect("--records <N>"),
            other => eprintln!("WARN unknown arg: {other}"),
        }
    }
    let plugin = plugin.expect("--plugin <exe>");

    let mut child = Command::new(&plugin)
        .args(["--batch", &batch.to_string(), "--records", &records.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {plugin}: {e}"));

    let mut child_stderr = child.stderr.take().unwrap();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stderr.read_to_end(&mut buf);
        eprint!("{}", String::from_utf8_lossy(&buf));
    });

    let mut child_stdin = child.stdin.take().unwrap();
    let reader = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
    let mut it = reader.lines();

    let init_req = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocol_version\":1,\"host_info\":{\"name\":\"AnalysisBuddy\",\"version\":\"0.1.0\"}}}\n";
    child_stdin
        .write_all(init_req.as_bytes())
        .expect("write initialize");
    child_stdin.flush().ok();

    let mut init_ok = false;
    for _ in 0..2 {
        let line = match it.next() {
            Some(Ok(l)) => l,
            Some(Err(e)) => panic!("read init response: {e}"),
            None => panic!("plugin exited before init response"),
        };
        let v: Value = serde_json::from_str(&line).expect("init response JSON");
        if v.get("id").and_then(Value::as_u64) == Some(1) && v.get("result").is_some() {
            init_ok = true;
            break;
        }
    }
    if !init_ok {
        eprintln!("FAIL initialize handshake");
        let _ = child.kill();
        std::process::exit(2);
    }

    let parse_req = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"parse\",\"params\":{\"file_id\":\"f-echo\"}}\n";
    child_stdin
        .write_all(parse_req.as_bytes())
        .expect("write parse");
    child_stdin.flush().ok();
    drop(child_stdin);

    let mut last_arrival: Option<Instant> = None;
    let mut first_frame: Option<Instant> = None;
    let mut done_arrival: Option<Instant> = None;
    let mut window_bytes: u64 = 0;
    let mut max_gap_ms = 0.0f64;
    let mut line_bytes_total: u64 = 0;
    let mut pre_window_bytes: u64 = 0;
    let mut max_line_bytes = 0usize;
    let mut batch_frames = 0u64;
    let mut progress_frames = 0u64;
    let mut expected_seq = 0u64;
    let mut seq_ok = true;
    let mut sum_records: u64 = 0;
    let mut done_seen = false;
    let mut records_total: u64 = 0;
    let mut response_seen = false;

    loop {
        let line = match it.next() {
            Some(Ok(l)) => l,
            Some(Err(e)) => panic!("read frame: {e}"),
            None => break,
        };
        let now = Instant::now();
        line_bytes_total += line.len() as u64 + 1;
        max_line_bytes = max_line_bytes.max(line.len() + 1);
        if let Some(prev) = last_arrival {
            let gap = now.duration_since(prev).as_secs_f64() * 1000.0;
            max_gap_ms = max_gap_ms.max(gap);
        }
        last_arrival = Some(now);
        if first_frame.is_none() {
            pre_window_bytes = line_bytes_total - line.len() as u64 - 1;
            first_frame = Some(now);
        }

        let v: Value = serde_json::from_str(&line).expect("frame JSON");
        match v.get("method").and_then(Value::as_str) {
            Some("progress") => progress_frames += 1,
            Some("RecordBatch") => {
                batch_frames += 1;
                let params = v.get("params").unwrap_or(&Value::Null);
                let seq = params.get("seq").and_then(Value::as_u64).unwrap_or(u64::MAX);
                if seq != expected_seq {
                    seq_ok = false;
                }
                expected_seq = seq + 1;
                let recs = params
                    .get("records")
                    .and_then(Value::as_array)
                    .map(|a| a.len() as u64)
                    .unwrap_or(0);
                sum_records += recs;
                if params.get("done").and_then(Value::as_bool) == Some(true) {
                    done_seen = true;
                    done_arrival = Some(now);
                    window_bytes = line_bytes_total;
                }
            }
            _ => {
                if v.get("id").and_then(Value::as_u64) == Some(2) {
                    response_seen = true;
                    if let Some(r) = v.get("result") {
                        records_total = r.get("records_total").and_then(Value::as_u64).unwrap_or(0);
                    }
                }
            }
        }
        if done_arrival.is_some() && response_seen {
            break;
        }
    }

    // 窗口口径（qa-perf.md §4.2 指标 3 / 任务口径）：首帧到达 → done:true 帧到达；
    // 字节 = 窗口内累计帧字节（line_bytes_total 扣除窗口前帧）
    let window_bytes = window_bytes - pre_window_bytes;
    let window_elapsed_s = done_arrival
        .and_then(|d| first_frame.map(|f| d.duration_since(f).as_secs_f64()))
        .unwrap_or(0.0);
    let mbps = if window_elapsed_s > 0.0 {
        window_bytes as f64 / 1_000_000.0 / window_elapsed_s
    } else {
        0.0
    };

    let sum_ok = done_seen && records_total == records as u64 && sum_records == records as u64;
    let status = child.wait().map(|s| s.code()).unwrap_or(None);
    let s = Stats {
        batch,
        records,
        bytes: window_bytes,
        max_line_bytes,
        progress_gap_ms: max_gap_ms,
        batch_frames,
        progress_frames,
        seq_ok,
        sum_ok,
        records_total,
        done_seen,
        elapsed_ms: window_elapsed_s * 1000.0,
    };
    println!("RESULT\tbatch={}\trecords={}\telapsed_ms={:.2}\tmbps={:.2}\tmax_line_bytes={}\tprogress_gap_ms={:.3}\tbytes={}\tbatch_frames={}\tprogress_frames={}\tseq_ok={}\tsum_ok={}\trecords_total={}\tdone_seen={}\texit={:?}", s.batch, s.records, s.elapsed_ms, mbps, s.max_line_bytes, s.progress_gap_ms, s.bytes, s.batch_frames, s.progress_frames, s.seq_ok, s.sum_ok, s.records_total, s.done_seen, status);
    eprintln!("INFO done window_ms={:.2} init_ok={init_ok}", s.elapsed_ms);

    let ok = s.seq_ok && s.sum_ok && s.done_seen && response_seen;
    std::process::exit(if ok { 0 } else { 1 });
}
