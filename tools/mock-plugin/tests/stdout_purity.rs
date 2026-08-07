//! stdout 纯净自洽检查：以 happy_path 剧本驱动完整请求序列，断言 stdout 只含
//! 合法协议帧（单行 JSON-RPC 2.0、LF 行尾、应答 id 逐帧回显、parse 期间通知
//! 先于最终响应、逐帧形状与 protocol-v1.md §3.5 示例一致）。

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mock-plugin"))
}

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

const REQUESTS: [&str; 8] = [
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"host_info":{"name":"AnalysisBuddy","version":"0.1.0"}}}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"schema"}"#,
    r#"{"jsonrpc":"2.0","id":3,"method":"can_handle","params":{"path":"C:\\logs\\match_20260807.csv","name":"match_20260807.csv","ext":"csv","size_bytes":1048576,"head_sample":"timestamp,fps,frame_ms"}}"#,
    r#"{"jsonrpc":"2.0","id":4,"method":"load_file","params":{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","path":"C:\\logs\\match_20260807.csv"}}"#,
    r#"{"jsonrpc":"2.0","id":5,"method":"parse","params":{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c"}}"#,
    r#"{"jsonrpc":"2.0","id":6,"method":"key_values","params":{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","timestamp_ms":1785601234567}}"#,
    r#"{"jsonrpc":"2.0","id":7,"method":"unload_file","params":{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c"}}"#,
    r#"{"jsonrpc":"2.0","id":8,"method":"shutdown"}"#,
];

#[test]
fn stdout_carries_only_valid_protocol_frames() {
    let mut child = bin()
        .args(["--script", "scripts/happy_path.ndjson"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mock-plugin");

    let mut input = REQUESTS.join("\n");
    input.push('\n');
    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write requests to stdin");
    drop(child.stdin.take());

    let out = child.wait_with_output().expect("wait for mock-plugin");
    assert!(
        out.status.success(),
        "shutdown + stdin EOF must exit 0, got {:?}",
        out.status.code()
    );

    // 日志只走 stderr（§9.3：INFO/WARN/ERROR 前缀）。
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("INFO mock-plugin") && stderr.contains("request method=initialize"),
        "stderr should carry the per-request logs:\n{stderr}"
    );

    // stdout 逐行必须是合法 JSON-RPC 帧，行尾无 \r。
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    assert!(!stdout.is_empty(), "stdout must carry the replies");
    assert!(!stdout.contains('\r'), "frames must be LF-only, no \\r");
    let frames: Vec<Value> = stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|e| panic!("non-JSON line on stdout: {e}: {line:?}"))
        })
        .collect();
    for (n, frame) in frames.iter().enumerate() {
        assert_eq!(frame["jsonrpc"], "2.0", "frame {n} carries jsonrpc 2.0");
    }

    let is_response = |f: &Value| {
        f.get("id").is_some() && (f.get("result").is_some() || f.get("error").is_some())
    };
    let is_notification =
        |f: &Value| f.get("id").is_none() && f.get("method").is_some() && f.get("params").is_some();
    for (n, frame) in frames.iter().enumerate() {
        assert!(
            is_response(frame) || is_notification(frame),
            "frame {n} must be a response or a notification: {frame}"
        );
        assert!(
            !(is_response(frame) && is_notification(frame)),
            "frame {n} must not mix response and notification: {frame}"
        );
    }

    // 期望序列：8 应答 + 2 progress + 2 RecordBatch = 12 帧。
    assert_eq!(
        frames.len(),
        12,
        "8 replies + 2 progress + 2 RecordBatch, got: {stdout}"
    );

    // §3.5 ① initialize：id 回显，capabilities 三 false。
    assert_eq!(frames[0]["id"], json!(1));
    assert_eq!(frames[0]["result"]["id"], "mock");
    assert_eq!(frames[0]["result"]["name"], "Mock Replay Plugin");
    assert_eq!(frames[0]["result"]["version"], "0.1.0");
    assert_eq!(
        frames[0]["result"]["capabilities"],
        json!({"annotate": false, "subscribe": false, "binary_sidecar": false})
    );

    // schema：3 个 MetricDef，聚合方式 snake_case。
    let metrics = frames[1]["result"]["metrics"]
        .as_array()
        .expect("metrics array");
    let ids: Vec<&str> = metrics.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["fps", "frame_ms", "player_hp"]);
    assert_eq!(metrics[0]["aggregation"], "last");

    // can_handle / load_file。
    assert_eq!(frames[2]["result"]["can_handle"], json!(true));
    assert_eq!(frames[2]["result"]["confidence"], json!(1.0));
    assert!(
        frames[2]["result"].get("reason").is_none(),
        "optional reason omitted"
    );
    assert_eq!(frames[3]["result"]["record_count_hint"], json!(3));

    // parse 流：通知先于最终响应；RecordBatch seq 0 → 1、done false → true。
    for frame in &frames[4..=7] {
        assert!(
            is_notification(frame),
            "parse streaming frames are notifications"
        );
        assert_eq!(frame["params"]["file_id"], FILE_ID);
    }
    assert_eq!(frames[4]["method"], "progress");
    assert_eq!(frames[5]["method"], "progress");
    assert_eq!(frames[6]["method"], "RecordBatch");
    assert_eq!(frames[6]["params"]["seq"], json!(0));
    assert_eq!(frames[6]["params"]["done"], json!(false));
    assert_eq!(frames[7]["method"], "RecordBatch");
    assert_eq!(frames[7]["params"]["seq"], json!(1));
    assert_eq!(frames[7]["params"]["done"], json!(true));
    assert_eq!(
        frames[8]["id"],
        json!(5),
        "parse response last, after notifications"
    );

    // 每条 Record 的 metric 都声明于 schema，且 Σrecords == records_total。
    let mut total = 0u64;
    for frame in &frames[6..=7] {
        for record in frame["params"]["records"].as_array().expect("records") {
            assert!(
                ids.contains(&record["metric"].as_str().expect("metric id")),
                "metric must be declared in schema: {record}"
            );
            assert!(record["value"].as_f64().expect("numeric value").is_finite());
            total += 1;
        }
    }
    assert_eq!(frames[8]["result"]["records_total"], json!(total));

    // key_values / unload_file / shutdown。
    assert_eq!(frames[9]["result"]["entries"][0]["key"], "scene");
    assert_eq!(frames[9]["result"]["entries"][0]["value"], "boss");
    assert_eq!(frames[10]["result"], json!({}));
    assert_eq!(frames[11]["result"], json!({}));
}
