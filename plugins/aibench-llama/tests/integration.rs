//! 集成测试：拉起 `aibench-llama --stdio` 子进程，按协议回放最小序列逐帧校验。
//!
//! 覆盖：initialize / schema / can_handle（认领 + 弃权）/ load_file / parse
//! （批量 + 心跳 + records_total）/ key_values / unload_file / shutdown / EOF 退出码。
//! 夹具在运行时生成到临时目录（与真实 stress48h 日志同构的 45 列 CSV）。

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::Value;

/// 生成 AIBench 夹具 CSV（n 行 probe 数据），返回路径。
fn make_fixture(rows: usize) -> std::path::PathBuf {
    const HEADER: &str = "sn,machine_model,cpu,cpu_vendor,gpu,gpu_vendor,driver_version,vbios,bios_version,ec_version,os_version,os_arch,total_ram_gb,engine_id,backend,llama_build,model_id,task_name,n_prompt,n_gen,ngl,flash_attn,avg_ts,stddev_ts,ttft_ms_measured,ttft_ms_est,vram_peak_mb,vram_mode,load_time_ms,test_time,samples,ppl,avg_power_w,energy_per_token_mj,rounds,turns_ok,tool_time_total_s,e2e_duration_s,session_success,p95_tps,degradation_ratio,suite_name,host_version,timestamp,status";
    let dir = std::env::temp_dir().join(format!("aibench-llama-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stress48h-fixture.csv");
    let mut body = String::from(HEADER);
    body.push('\n');
    for i in 0..rows {
        let task = if i % 2 == 0 {
            "probe-pp@cuda-13.3-x64"
        } else {
            "probe-tg@cuda-13.3-x64"
        };
        body.push_str(&format!(
            "SN001,X6AF,Intel CPU,GenuineIntel,NVIDIA GPU,NVIDIA,drv,vbios,bios,ec,Win11,x64,31.5,cuda-13.3-x64,cuda-13.3-x64,10621,qwen3.5-4b-q4km,{task},512,0,99,-1,6213.7,1.5,,,7206,inline,2045,0.41,5,,113.08,18.19,,,,,,,,stress48h-qwen35-4b,1.0.0+abc,2026-09-04 18:{:02}:{:02},ok\n",
            (i / 60) % 60,
            i % 60,
        ));
    }
    std::fs::write(&path, &body).unwrap();
    path
}

struct Session {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

impl Session {
    fn start() -> Session {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aibench-llama"))
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn aibench-llama");
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
            assert!(!line.is_empty(), "aibench-llama exited prematurely");
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

fn initialize(sess: &mut Session) {
    send_and_recv(
        sess,
        "initialize",
        Some(serde_json::json!({
            "protocol_version": 1,
            "host_info": { "name": "AnalysisBuddy", "version": "0.1.0" },
        })),
    );
}

#[test]
fn full_session_aibench_csv() {
    let mut sess = Session::start();
    initialize(&mut sess);

    let frames = send_and_recv(&mut sess, "schema", None);
    assert!(result_of(&frames)["metrics"].is_array(), "load 前 schema 合法（空集）");

    let path = make_fixture(50);
    let head_bytes = std::fs::read(&path).unwrap();
    let head = String::from_utf8_lossy(&head_bytes[..head_bytes.len().min(4096)]).into_owned();
    let frames = send_and_recv(
        &mut sess,
        "can_handle",
        Some(serde_json::json!({
            "path": path,
            "name": "stress48h-fixture.csv",
            "ext": "csv",
            "size_bytes": head_bytes.len() as u64,
            "head_sample": head,
        })),
    );
    let can = result_of(&frames);
    assert_eq!(can["can_handle"], true, "must claim AIBench fixture: {can}");
    assert_eq!(can["confidence"], 1.0);

    let rid = sess.send(
        "load_file",
        Some(serde_json::json!({ "file_id": "f1", "path": path })),
    );
    let load_frames = sess.recv_until(rid);
    let summary = result_of(&load_frames);
    assert!(summary.get("record_count_hint").is_some(), "hint present");
    assert!(summary.get("time_range").is_some(), "time range present");
    let note = summary["note"].as_str().unwrap_or("");
    assert!(note.contains("AIBench schema"), "note: {note}");

    // load 后 schema：45 列表头齐全 → 全部 11 个白名单 metric。
    let frames = send_and_recv(&mut sess, "schema", None);
    let metrics = result_of(&frames)["metrics"].as_array().unwrap().clone();
    let metric_ids: Vec<&str> = metrics.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(
        metric_ids,
        vec![
            "avg_ts",
            "stddev_ts",
            "ttft_measured",
            "ttft_est",
            "vram_peak",
            "load_time",
            "ppl",
            "avg_power",
            "energy_per_token",
            "p95_tps",
            "degradation_ratio",
        ]
    );

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
            assert!(r["value"].as_f64().is_some());
            // 每条记录带 tags（backend/task/model/engine）。
            assert!(r["tags"].is_object(), "tags present: {r}");
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
    // 50 行 × 6 个非空指标（avg_ts, stddev, vram, load, power, energy；ttft/ppl 空）。
    assert_eq!(sum, 50 * 6, "50 rows × 6 non-empty metrics");

    // key_values：时间范围中点后每任务最新快照。
    let frames = send_and_recv(
        &mut sess,
        "key_values",
        Some(serde_json::json!({
            "file_id": "f1", "timestamp_ms": 9999999999999i64,
        })),
    );
    let entries = result_of(&frames)["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["key"] == "suite_name"));
    assert!(
        entries
            .iter()
            .any(|e| e["key"].as_str().unwrap().starts_with("task:probe-pp:")),
        "probe-pp snapshot present: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e["key"] == "task:probe-pp:avg_ts"),
        "probe-pp avg_ts present: {entries:?}"
    );

    let frames = send_and_recv(
        &mut sess,
        "unload_file",
        Some(serde_json::json!({ "file_id": "f1" })),
    );
    assert_eq!(result_of(&frames), &serde_json::json!({}));

    let frames = send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(result_of(&frames), &serde_json::json!({}));
    let rc = sess.close_stdin_and_wait();
    assert_eq!(rc, 0, "shutdown then EOF exit code 0");
}

#[test]
fn can_handle_rejects_generic_csv() {
    let mut sess = Session::start();
    initialize(&mut sess);
    let frames = send_and_recv(
        &mut sess,
        "can_handle",
        Some(serde_json::json!({
            "path": "C:\\x\\fps.csv",
            "name": "fps.csv",
            "ext": "csv",
            "size_bytes": 1024,
            "head_sample": "timestamp,fps,frame_ms\n2026-08-07T00:00:00Z,60,16\n",
        })),
    );
    let can = result_of(&frames);
    assert_eq!(can["can_handle"], false, "generic CSV must decline: {can}");
    assert_eq!(can["confidence"], 0.0);
    send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(sess.close_stdin_and_wait(), 0);
}

#[test]
fn unknown_method_annotate_and_busy() {
    let mut sess = Session::start();
    initialize(&mut sess);
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

    // 未 load 的文件 parse → -32602。
    let frames = send_and_recv(
        &mut sess,
        "parse",
        Some(serde_json::json!({ "file_id": "nope" })),
    );
    assert_eq!(frames.last().unwrap()["error"]["code"], -32602);

    // 不存在的文件 load → -32002。
    let frames = send_and_recv(
        &mut sess,
        "load_file",
        Some(serde_json::json!({
            "file_id": "f1", "path": "C:\\no\\such\\aibench.csv",
        })),
    );
    assert_eq!(frames.last().unwrap()["error"]["code"], -32002);

    // 非 AIBench CSV load → -32002（表头特征校验）。
    let path = std::env::temp_dir().join(format!("aibench-llama-bad-{}.csv", std::process::id()));
    std::fs::write(&path, "a,b,c\n1,2,3\n").unwrap();
    let frames = send_and_recv(
        &mut sess,
        "load_file",
        Some(serde_json::json!({ "file_id": "f2", "path": path })),
    );
    assert_eq!(frames.last().unwrap()["error"]["code"], -32002);

    send_and_recv(&mut sess, "shutdown", None);
    assert_eq!(sess.close_stdin_and_wait(), 0);
}

#[test]
fn eof_exits_zero() {
    let mut sess = Session::start();
    initialize(&mut sess);
    sess.child.stdin.take();
    let rc = sess.child.wait().expect("wait");
    assert_eq!(rc.code(), Some(0), "stdin EOF → exit code 0");
}
