//! G2 golden 行为等价测试：固定请求序列 → stdout 协议帧**逐字节**比对。
//!
//! 与 `builtin-csv/tests/golden_stdio.rs` 同一套路（两插件共用同一传输样板，
//! golden 序列刻意同构）。golden 基线捕获于 `core/ab-plugin-rt` 抽取**之前**
//! 的插件二进制；抽取迁移后本测试必须逐字节复现。
//! 重捕：`GOLDEN_UPDATE=1 cargo test --test golden_stdio`。
//!
//! 等价口径：**stdout 协议帧逐字节等价**。-32002 的 `detail` 含 OS errno 文案
//! （跨平台不稳定），不进逐字节 golden，由 tests/e2e 三插件语义等价套件覆盖；
//! -32001 忙碌竞态依赖线程时序，同样不收录。夹具由本文件确定性生成（路径不落
//! 任何响应帧，故转录与机器无关）。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// 生成 AIBench 夹具 CSV（确定性 8 行 probe 数据，45 列真实表头）。
fn make_fixture() -> String {
    const HEADER: &str = "sn,machine_model,cpu,cpu_vendor,gpu,gpu_vendor,driver_version,vbios,bios_version,ec_version,os_version,os_arch,total_ram_gb,engine_id,backend,llama_build,model_id,task_name,n_prompt,n_gen,ngl,flash_attn,avg_ts,stddev_ts,ttft_ms_measured,ttft_ms_est,vram_peak_mb,vram_mode,load_time_ms,test_time,samples,ppl,avg_power_w,energy_per_token_mj,rounds,turns_ok,tool_time_total_s,e2e_duration_s,session_success,p95_tps,degradation_ratio,suite_name,host_version,timestamp,status";
    let mut body = String::from(HEADER);
    body.push('\n');
    for i in 0..8 {
        let task = if i % 2 == 0 {
            "probe-pp@cuda-13.3-x64"
        } else {
            "probe-tg@cuda-13.3-x64"
        };
        body.push_str(&format!(
            "SN001,X6AF,Intel CPU,GenuineIntel,NVIDIA GPU,NVIDIA,drv,vbios,bios,ec,Win11,x64,31.5,cuda-13.3-x64,cuda-13.3-x64,10621,qwen3.5-4b-q4km,{task},512,0,99,-1,6213.7,1.5,,,7206,inline,2045,0.41,5,,113.08,18.19,,,,,,,,stress48h-qwen35-4b,1.0.0+abc,2026-09-04 18:{:02}:{:02},ok\n",
            i,
            i,
        ));
    }
    body
}

struct Session {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
    transcript: Vec<String>,
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
            transcript: Vec::new(),
            next_id: 1,
        }
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("stdin");
        stdin.write_all(line.as_bytes()).expect("write request");
        stdin.write_all(b"\n").expect("write newline");
        stdin.flush().expect("flush stdin");
    }

    fn roundtrip(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send_raw(&serde_json::to_string(&frame).expect("serialize request"));
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read stdout");
            assert!(!line.is_empty(), "aibench-llama exited prematurely");
            let line = line.trim_end_matches('\n');
            let frame: Value = serde_json::from_str(line).expect("valid JSON frame");
            self.transcript.push(line.to_string());
            if frame.get("id").and_then(Value::as_i64) == Some(id) {
                return frame;
            }
        }
    }

    fn finish(&mut self) -> i32 {
        self.child.stdin.take();
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => panic!("unexpected stdout after shutdown: {line:?}"),
                Err(e) => panic!("read stdout after shutdown: {e}"),
            }
        }
        let status = self.child.wait().expect("wait child");
        status.code().unwrap_or(-1)
    }
}

fn golden_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("stdio.transcript")
}

#[test]
fn golden_stdio_transcript() {
    let fixture = make_fixture();
    let dir = std::env::temp_dir().join(format!("aibench-golden-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir fixture dir");
    let path = dir.join("stress48h-golden.csv");
    std::fs::write(&path, &fixture).expect("write fixture");
    let path = path.to_string_lossy().into_owned();

    let mut sess = Session::start();

    // 1. initialize（G1 版本回显：v1 键省略）。
    let init = sess.roundtrip(
        "initialize",
        json!({
            "protocol_version": 1,
            "host_info": { "name": "AnalysisBuddy", "version": "0.1.0" },
        }),
    );
    assert_eq!(init["result"]["id"], "aibench-llama");

    // 2. load 前 schema + stdin 容错（无输出）。
    sess.roundtrip("schema", json!(null));
    sess.send_noise("this is not json");
    sess.send_noise(r#"{"jsonrpc":"2.0","id":99}"#);

    // 3. 错误路径：-32601 / -32005 / -32602（serde 确定性文案）。
    sess.roundtrip("subscribe", json!(null));
    sess.roundtrip(
        "annotate",
        json!({ "file_id": "f1", "range": { "start_ms": 0, "end_ms": 1 } }),
    );

    // 4. can_handle：认领自家夹具 + invalid params。
    sess.roundtrip(
        "can_handle",
        json!({
            "path": path,
            "name": "stress48h-golden.csv",
            "ext": "csv",
            "size_bytes": fixture.len() as u64,
            "head_sample": fixture.chars().take(4096).collect::<String>(),
        }),
    );
    sess.roundtrip(
        "can_handle",
        json!({
            "path": "/x/a.csv",
            "name": "a.csv",
            "ext": "csv",
            "size_bytes": 10,
        }),
    );

    // 5. load_file → schema（冻结白名单 metric 集）。
    sess.roundtrip("load_file", json!({ "file_id": "f1", "path": path }));
    sess.roundtrip("schema", json!(null));

    // 6. parse：progress/RecordBatch + records_total。
    sess.roundtrip("parse", json!({ "file_id": "f1" }));

    // 7. 未加载文件上 parse：-32602。
    sess.roundtrip("parse", json!({ "file_id": "ghost" }));

    // 8. key_values（时间范围中点）→ unload（幂等两次）。
    let summary_line = sess
        .transcript
        .iter()
        .find(|l| l.contains("\"record_count_hint\""))
        .expect("load_file summary in transcript");
    let summary: Value = serde_json::from_str(summary_line).expect("summary frame");
    let start = summary["result"]["time_range"]["start_ms"]
        .as_i64()
        .expect("start_ms");
    let end = summary["result"]["time_range"]["end_ms"]
        .as_i64()
        .expect("end_ms");
    sess.roundtrip(
        "key_values",
        json!({ "file_id": "f1", "timestamp_ms": (start + end) / 2 }),
    );
    sess.roundtrip("unload_file", json!({ "file_id": "f1" }));
    sess.roundtrip("unload_file", json!({ "file_id": "f1" }));

    // 9. shutdown → EOF。
    sess.roundtrip("shutdown", json!(null));
    let rc = sess.finish();
    assert_eq!(rc, 0, "shutdown then EOF must exit 0");

    compare_or_update(&sess.transcript);
}

impl Session {
    /// 无 method / 非法 JSON 帧：协议要求忽略且不产生任何 stdout 输出。
    fn send_noise(&mut self, line: &str) {
        self.send_raw(line);
    }
}

fn compare_or_update(transcript: &[String]) {
    let path = golden_file();
    let actual = transcript.join("\n") + "\n";
    if std::env::var_os("GOLDEN_UPDATE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir golden dir");
        std::fs::write(&path, &actual).expect("write golden");
        eprintln!("golden updated: {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read golden {}: {e}（GOLDEN_UPDATE=1 重捕）",
            path.display()
        )
    });
    if expected != actual {
        let exp: Vec<&str> = expected.lines().collect();
        let act: Vec<&str> = actual.lines().collect();
        for i in 0..exp.len().max(act.len()) {
            let e = exp.get(i).copied().unwrap_or("<missing>");
            let a = act.get(i).copied().unwrap_or("<missing>");
            if e != a {
                panic!(
                    "golden transcript diverges at line {} (1-based):\n  expected: {e}\n  actual:   {a}",
                    i + 1
                );
            }
        }
        panic!("golden transcript length diverges: {exp:?} vs {act:?}");
    }
}
