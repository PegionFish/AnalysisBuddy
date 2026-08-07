use std::env;
use std::io::{self, BufRead, BufWriter, Write};
use std::time::Instant;

use serde_json::{json, Value};

const BYTES_PER_RECORD_ESTIMATE: u64 = 48;

fn main() {
    let mut batch = 1000usize;
    let mut records = 100_000usize;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--batch" => batch = args.next().and_then(|v| v.parse().ok()).expect("--batch <N>"),
            "--records" => records = args.next().and_then(|v| v.parse().ok()).expect("--records <N>"),
            other => eprintln!("WARN unknown arg: {other}"),
        }
    }
    eprintln!("INFO echo-plugin start batch={batch} records={records}");

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(1 << 20, stdout.lock());
    let mut parse_count = 0u64;
    let mut max_line_bytes = 0usize;
    let start = Instant::now();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("ERROR stdin read: {e}");
                break;
            }
        };
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARN unparsable request line: {e}");
                continue;
            }
        };
        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id").cloned();
        match method {
            Some("initialize") => {
                let res = json!({
                    "id": "echo",
                    "name": "echo 吞吐原型插件",
                    "version": "0.1.0",
                    "capabilities": {"annotate": false, "subscribe": false, "binary_sidecar": false}
                });
                emit(&mut out, &json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": res}), &mut max_line_bytes);
            }
            Some("parse") => {
                parse_count += 1;
                let file_id = msg
                    .pointer("/params/file_id")
                    .and_then(Value::as_str)
                    .unwrap_or("f-echo")
                    .to_string();
                run_parse(&mut out, &file_id, batch, records, &mut max_line_bytes);
                emit(&mut out, &json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": {"records_total": records}}), &mut max_line_bytes);
            }
            Some(other) => {
                let err = json!({"code": -32601, "message": format!("method not found: {other}")});
                emit(&mut out, &json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": err}), &mut max_line_bytes);
            }
            None => {
                let err = json!({"code": -32600, "message": "invalid request"});
                emit(&mut out, &json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": err}), &mut max_line_bytes);
            }
        }
    }
    out.flush().ok();
    eprintln!(
        "INFO echo-plugin exit 0 parse_count={parse_count} max_line_bytes={max_line_bytes} up_ms={}",
        start.elapsed().as_millis()
    );
}

fn run_parse(
    out: &mut BufWriter<impl Write>,
    file_id: &str,
    batch: usize,
    records: usize,
    max_line_bytes: &mut usize,
) {
    let mut seq = 0u64;
    let mut sent = 0u64;
    let total = records as u64;
    while sent < total {
        let n = batch.min((total - sent) as usize);
        let mut recs = Vec::with_capacity(n);
        for k in 0..n {
            let i = sent + k as u64;
            recs.push(json!({
                "timestamp": 1785600000000i64 + i as i64,
                "metric": if i % 2 == 0 { "fps" } else { "frame_ms" },
                "value": (i % 100) as f64 + 0.5
            }));
        }
        let done = sent + n as u64 >= total;
        let so_far = sent + n as u64;
        let progress = json!({
            "jsonrpc": "2.0",
            "method": "progress",
            "params": {
                "file_id": file_id,
                "percent": so_far as f64 / total as f64 * 100.0,
                "records_so_far": so_far,
                "bytes_read": so_far * BYTES_PER_RECORD_ESTIMATE
            }
        });
        emit(out, &progress, max_line_bytes);
        let batch_msg = json!({
            "jsonrpc": "2.0",
            "method": "RecordBatch",
            "params": {"file_id": file_id, "seq": seq, "records": recs, "done": done}
        });
        emit(out, &batch_msg, max_line_bytes);
        sent = so_far;
        seq += 1;
    }
}

fn emit(out: &mut BufWriter<impl Write>, msg: &Value, max_line_bytes: &mut usize) {
    let s = msg.to_string();
    *max_line_bytes = (*max_line_bytes).max(s.len() + 1);
    out.write_all(s.as_bytes()).expect("stdout write");
    out.write_all(b"\n").expect("stdout write");
    out.flush().expect("stdout flush");
}
