//! 集成测试：拉起 `builtin-csv --stdio` 子进程，按 E 路回放最小序列逐帧校验。
//!
//! 覆盖：initialize / schema / can_handle / load_file / parse（批量+心跳）
//! / key_values / unload_file / shutdown / EOF 退出码；带表头与无表头夹具。

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::Value;

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    path.canonicalize()
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

struct Session {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

impl Session {
    fn start() -> Session {
        let mut child = Command::new(env!("CARGO_BIN_EXE_builtin-csv"))
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn builtin-csv");
        let stdout = child.stdout.take().expect("stdout");
        Session {
            child,
            reader: BufReader::new(stdout),
            next_id: 1,
        }
    }

    fn send(&mut self, method: &str, params: Option<Value>) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut frame = serde_json::Map::new();
        frame.insert("jsonrpc".into(), Value::String("2.0".into()));
        frame.insert("id".into(), Value::from(id));
        frame.insert("method".into(), Value::String(method.into()));
        if let Some(p) = params {
            frame.insert("params".into(), p);
        }
        let line = serde_json::to_string(&Value::Object(frame)).unwrap();
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all((line + "\n").as_bytes())
            .unwrap();
        self.child.stdin.as_mut().unwrap().flush().unwrap();
        id
    }

    /// 收帧直到某请求的响应到达；返回期间所有帧（含通知）。
    fn recv_until(&mut self, rid: i64) -> Vec<Value> {
        let mut frames = Vec::new();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read stdout line");
            assert!(!line.is_empty(), "builtin-csv exited prematurely");
            let frame: Value = serde_json::from_str(line.trim_end()).expect("valid JSON frame");
            let is_resp = frame.get("id").and_then(Value::as_i64) == Some(rid);
            frames.push(frame);
            if is_resp {
                return frames;
            }
        }
    }

    fn close_stdin_and_wait(&mut self) -> i32 {
        self.child.stdin.take();
        let status = self.child.wait().expect("wait child");
        status.code().unwrap_or(-1)
    }
}

fn result_of(frames: &[Value]) -> &Value {
    frames.last().unwrap().get("result").expect("result frame")
}

fn send_and_recv(sess: &mut Session, method: &str, params: Option<Value>) -> Vec<Value> {
    let rid = sess.send(method, params);
    sess.recv_until(rid)
}

fn run_session(fixture_name: &str, expected_metrics: &[&str], extra: bool) {
    let mut sess = Session::start();

    let frames = send_and_recv(
        &mut sess,
        "initialize",
        Some(serde_json::json!({
            "protocol_version": 1,
            "host_info": { "name": "AnalysisBuddy", "version": "0.1.0" },
        })),
    );
    let init = result_of(&frames);
    assert_eq!(init["id"], "builtin-csv");
    assert_eq!(init["capabilities"]["annotate"], false);
    assert_eq!(init["capabilities"]["subscribe"], false);

    // schema 在 load 前调用合法（返回空集）；load 后重查得到冻结列集合（§3.5）。
    let frames = send_and_recv(&mut sess, "schema", None);
    assert!(result_of(&frames)["metrics"].is_array());

    let path = fixture(fixture_name);
    let head = std::fs::read(&path).unwrap();
    let head = String::from_utf8_lossy(&head[..head.len().min(4096)]).into_owned();
    let frames = send_and_recv(
        &mut sess,
        "can_handle",
        Some(serde_json::json!({
            "path": path,
            "name": fixture_name,
            "ext": "csv",
            "size_bytes": std::fs::metadata(&path).unwrap().len(),
            "head_sample": head,
        })),
    );
    let can = result_of(&frames);
    assert_eq!(can["can_handle"], true, "must claim its own fixture");

    let rid = sess.send(
        "load_file",
        Some(serde_json::json!({
            "file_id": "f1", "path": path,
        })),
    );
    let load_frames = sess.recv_until(rid);
    let summary = result_of(&load_frames);
    assert!(summary.get("record_count_hint").is_some(), "hint present");
    assert!(summary.get("time_range").is_some(), "time range present");
    let note = summary["note"].as_str().unwrap_or("");
    assert!(note.contains("comma-separated"), "note: {note}");

    // load 后 schema：metric 集与夹具列对应（无表头 → 合成列名 colN）。
    let frames = send_and_recv(&mut sess, "schema", None);
    let metrics = result_of(&frames)["metrics"].as_array().unwrap().clone();
    let metric_ids: Vec<&str> = metrics.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(metric_ids, expected_metrics);

    let rid = sess.send("parse", Some(serde_json::json!({ "file_id": "f1" })));
    let parse_frames = sess.recv_until(rid);
    let batches: Vec<&Value> = parse_frames
        .iter()
        .filter(|f| f.get("method").and_then(Value::as_str) == Some("RecordBatch"))
        .collect();
    assert!(!batches.is_empty(), "RecordBatch emitted");
    let progresses: Vec<&Value> = parse_frames
        .iter()
        .filter(|f| f.get("method").and_then(Value::as_str) == Some("progress"))
        .collect();
    assert!(!progresses.is_empty(), "progress emitted");
    let mut seqs: Vec<i64> = Vec::new();
    let mut sum: usize = 0;
    for b in &batches {
        seqs.push(b["params"]["seq"].as_i64().unwrap());
        let recs = b["params"]["records"].as_array().unwrap();
        sum += recs.len();
        for r in recs {
            assert!(r["timestamp"].is_i64());
            assert!(
                metric_ids.contains(&r["metric"].as_str().unwrap()),
                "metric declared"
            );
            assert!(r["value"].is_f64() || r["value"].is_i64());
        }
    }
    assert_eq!(
        seqs,
        (0..seqs.len() as i64).collect::<Vec<_>>(),
        "seq no gaps"
    );
    assert_eq!(batches.last().unwrap()["params"]["done"], true);
    assert_eq!(
        result_of(&parse_frames)["records_total"].as_u64().unwrap() as usize,
        sum,
        "records_total == sum of batches"
    );
    assert_eq!(sum, 200 * 3, "200 rows × 3 metrics");

    if extra {
        // key_values：取文件时间范围中点（夹具时间列严格递增）。
        let start = summary["time_range"]["start_ms"].as_i64().unwrap();
        let end = summary["time_range"]["end_ms"].as_i64().unwrap();
        let mid = (start + end) / 2;
        let frames = send_and_recv(
            &mut sess,
            "key_values",
            Some(serde_json::json!({
                "file_id": "f1", "timestamp_ms": mid,
            })),
        );
        // 无低基数文本列 → 空 entries 亦合规。
        assert!(result_of(&frames)["entries"].is_array());
    }

    let frames = send_and_recv(
        &mut sess,
        "unload_file",
        Some(serde_json::json!({
            "file_id": "f1",
        })),
    );
    assert_eq!(result_of(&frames), &serde_json::json!({}));

    let frames = send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(result_of(&frames), &serde_json::json!({}));
    let rc = sess.close_stdin_and_wait();
    assert_eq!(rc, 0, "shutdown then EOF exit code 0");
}

#[test]
fn full_session_with_header() {
    run_session(
        "small_with_header.csv",
        &["fps", "frame_ms", "mem_mb"],
        true,
    );
}

#[test]
fn full_session_no_header() {
    run_session("small_no_header.csv", &["col1", "col2", "col3"], true);
}

#[test]
fn malformed_lines_skipped_note_never_fatal() {
    let mut sess = Session::start();
    send_and_recv(
        &mut sess,
        "initialize",
        Some(serde_json::json!({
            "protocol_version": 1,
            "host_info": { "name": "A", "version": "1" },
        })),
    );
    let path = fixture("malformed_lines.csv");
    let rid = sess.send(
        "load_file",
        Some(serde_json::json!({ "file_id": "f1", "path": path })),
    );
    let frames = sess.recv_until(rid);
    let summary = result_of(&frames);
    let note = summary["note"].as_str().unwrap_or("");
    assert!(note.contains("skipped 20 bad lines"), "note: {note}");
    let rid = sess.send("parse", Some(serde_json::json!({ "file_id": "f1" })));
    let frames = sess.recv_until(rid);
    let resp = frames.last().unwrap();
    assert!(resp.get("result").is_some(), "no -32003: {resp}");
    assert_eq!(resp["result"]["records_total"].as_u64().unwrap(), 180 * 3);
    send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(sess.close_stdin_and_wait(), 0);
}

#[test]
fn can_handle_txt_requires_csv_shape() {
    let mut sess = Session::start();
    send_and_recv(
        &mut sess,
        "initialize",
        Some(serde_json::json!({
            "protocol_version": 1,
            "host_info": { "name": "A", "version": "1" },
        })),
    );
    // demo-tool 格式（FRAME/STATE 空格分隔）的 *.txt：builtin-csv 弃权，
    // 不再与日志解析器形成无意义双候选（P2 / T9）。
    let demo_head = "2026-08-07T10:00:00.123+08:00 FRAME fps=60.1 frame_ms=16.6 cpu_temp=63.2\n";
    let frames = send_and_recv(
        &mut sess,
        "can_handle",
        Some(serde_json::json!({
            "path": "C:\\x\\demo.txt",
            "name": "demo.txt",
            "ext": "txt",
            "size_bytes": 1024,
            "head_sample": demo_head,
        })),
    );
    let can = result_of(&frames);
    assert_eq!(can["can_handle"], false, "non-CSV .txt must decline: {can}");
    assert_eq!(can["confidence"], 0.0);
    // CSV 形态的 *.txt 仍被认领。
    let csv_head = "timestamp,fps,frame_ms\n2026-08-07T00:00:00.000+08:00,59.8,16.6\n";
    let frames = send_and_recv(
        &mut sess,
        "can_handle",
        Some(serde_json::json!({
            "path": "C:\\x\\data.txt",
            "name": "data.txt",
            "ext": "txt",
            "size_bytes": 1024,
            "head_sample": csv_head,
        })),
    );
    let can = result_of(&frames);
    assert_eq!(can["can_handle"], true, "CSV-shaped .txt must claim: {can}");
    assert!(can["confidence"].as_f64().unwrap() >= 0.7);
    send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(sess.close_stdin_and_wait(), 0);
}

#[test]
fn unknown_method_and_annotate() {
    let mut sess = Session::start();
    let frames = send_and_recv(&mut sess, "subscribe", None);
    assert_eq!(frames.last().unwrap()["error"]["code"], -32601);
    let frames = send_and_recv(
        &mut sess,
        "annotate",
        Some(serde_json::json!({
            "file_id": "f1", "range": { "start_ms": 0, "end_ms": 1 },
        })),
    );
    assert_eq!(frames.last().unwrap()["error"]["code"], -32005);
    send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(sess.close_stdin_and_wait(), 0);
}

#[test]
fn concurrent_parse_is_busy() {
    let mut sess = Session::start();
    send_and_recv(
        &mut sess,
        "initialize",
        Some(serde_json::json!({
            "protocol_version": 1,
            "host_info": { "name": "A", "version": "1" },
        })),
    );
    let path = fixture("small_with_header.csv");
    send_and_recv(
        &mut sess,
        "load_file",
        Some(serde_json::json!({
            "file_id": "f1", "path": path,
        })),
    );
    let id1 = sess.send("parse", Some(serde_json::json!({ "file_id": "f1" })));
    // 第二 parse 立即 -32001（解析线程在跑）。
    let frames = send_and_recv(
        &mut sess,
        "parse",
        Some(serde_json::json!({ "file_id": "f1" })),
    );
    assert_eq!(frames.last().unwrap()["error"]["code"], -32001);
    let frames = sess.recv_until(id1);
    assert!(frames.last().unwrap().get("result").is_some());
    send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(sess.close_stdin_and_wait(), 0);
}

#[test]
fn eof_exits_zero() {
    let mut sess = Session::start();
    send_and_recv(
        &mut sess,
        "initialize",
        Some(serde_json::json!({
            "protocol_version": 1,
            "host_info": { "name": "A", "version": "1" },
        })),
    );
    sess.child.stdin.take();
    let rc = sess.child.wait().expect("wait");
    assert_eq!(rc.code(), Some(0), "stdin EOF → exit code 0");
}

#[test]
fn missing_file_returns_neg_32002() {
    let mut sess = Session::start();
    send_and_recv(
        &mut sess,
        "initialize",
        Some(serde_json::json!({
            "protocol_version": 1,
            "host_info": { "name": "A", "version": "1" },
        })),
    );
    let frames = send_and_recv(
        &mut sess,
        "load_file",
        Some(serde_json::json!({
            "file_id": "f1", "path": "C:\\no\\such\\file.csv",
        })),
    );
    assert_eq!(frames.last().unwrap()["error"]["code"], -32002);
    send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(sess.close_stdin_and_wait(), 0);
}

#[test]
fn bom_and_gbk_fixtures_load_without_crash() {
    for (name, enc, expect_note) in [
        ("enc_utf8_bom.csv", "auto", Some("BOM")),
        ("enc_utf16le_bom.csv", "auto", Some("UTF-16")),
        ("enc_gbk.csv", "gbk", None),
        ("enc_gbk.csv", "auto", Some("GBK")), // Auto 正确识别 GBK，不再静默 U+FFFD 有损
    ] {
        let mut sess = Session::start();
        send_and_recv(
            &mut sess,
            "initialize",
            Some(serde_json::json!({
                "protocol_version": 1,
                "host_info": { "name": "A", "version": "1" },
            })),
        );
        let path = fixture(name);
        let rid = sess.send(
            "load_file",
            Some(serde_json::json!({
                "file_id": "f1", "path": path,
            })),
        );
        let frames = sess.recv_until(rid);
        assert!(
            frames.last().unwrap().get("result").is_some(),
            "{name}/{enc}: {frames:?}"
        );
        if let Some(needle) = expect_note {
            let note = frames.last().unwrap()["result"]["note"]
                .as_str()
                .unwrap_or("");
            assert!(
                note.contains(needle),
                "{name}/{enc}: note 应含 {needle}: {note}"
            );
        }
        let rid = sess.send("parse", Some(serde_json::json!({ "file_id": "f1" })));
        let frames = sess.recv_until(rid);
        assert!(
            frames.last().unwrap().get("result").is_some(),
            "parse ok {name}/{enc}"
        );
        send_and_recv(&mut sess, "shutdown", None);
        assert_eq!(sess.close_stdin_and_wait(), 0);
    }
}
