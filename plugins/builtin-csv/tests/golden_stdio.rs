//! G2 golden 行为等价测试：固定请求序列 → stdout 协议帧**逐字节**比对。
//!
//! golden 基线捕获于 `core/ab-plugin-rt` 抽取**之前**的插件二进制（任务卡 G2 的
//! 「迁移前后等价」锚点）；传输样板抽取后本测试必须逐字节复现同一转录。
//! 重捕：`GOLDEN_UPDATE=1 cargo test --test golden_stdio`（仅允许行为变更评审时）。
//!
//! 等价口径（任务卡二选一，此处选**逐字节等价**）：
//! - 序列只收录载荷完全确定的请求：错误路径含 -32601（未知方法）、-32602
//!   （invalid params，data 为 serde 确定性文案）、-32005（annotate 不支持）；
//!   -32002 的 `detail` 含 OS errno 文案（跨平台不稳定），不进逐字节 golden，
//!   由 tests/e2e 的三插件语义等价套件覆盖；
//! - stdin 容错路径（非法 JSON / 无 method 帧）隐式覆盖：两者均不产生 stdout
//!   输出，转录逐字节一致即证明「无多余帧」；
//! - 忙碌路径（-32001 parse 竞态）依赖线程时序，不收录（见
//!   `integration.rs::concurrent_parse_is_busy`）；
//! - stderr 日志与退出码不在逐字节比对内；退出码单独断言（shutdown/EOF → 0）。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

struct Session {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
    transcript: Vec<String>,
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

    /// 发送一条请求，录下直到该 id 的响应到达为止的全部 stdout 帧（含通知），
    /// 返回最后一帧（即该响应）。
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
            assert!(!line.is_empty(), "builtin-csv exited prematurely");
            let line = line.trim_end_matches('\n');
            let frame: Value = serde_json::from_str(line).expect("valid JSON frame");
            self.transcript.push(line.to_string());
            if frame.get("id").and_then(Value::as_i64) == Some(id) {
                return frame;
            }
        }
    }

    /// 无 method / 非法 JSON 帧：协议要求忽略且不产生任何 stdout 输出。
    /// 后续请求的转录连续性隐式断言了「无多余帧」。
    fn send_noise(&mut self, line: &str) {
        self.send_raw(line);
    }

    fn finish(&mut self) -> i32 {
        self.child.stdin.take();
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => break, // EOF
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

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    path.canonicalize()
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn golden_stdio_transcript() {
    let mut sess = Session::start();

    // 1. initialize：G1 版本回显（v1 键省略，与历史报文逐字节兼容）。
    let init = sess.roundtrip(
        "initialize",
        json!({
            "protocol_version": 1,
            "host_info": { "name": "AnalysisBuddy", "version": "0.1.0" },
        }),
    );
    assert_eq!(init["result"]["id"], "builtin-csv");

    // 2. load 前 schema：空 metric 集。
    sess.roundtrip("schema", json!(null));

    // 3. stdin 容错：非法 JSON / 无 method 帧（无输出）。
    sess.send_noise("this is not json");
    sess.send_noise(r#"{"jsonrpc":"2.0","id":99}"#);

    // 4. 错误路径：未知方法 -32601、annotate -32005。
    sess.roundtrip("subscribe", json!(null));
    sess.roundtrip(
        "annotate",
        json!({ "file_id": "f1", "range": { "start_ms": 0, "end_ms": 1 } }),
    );

    // 5. can_handle：认领自家夹具 + invalid params（serde 确定性文案）。
    let path = fixture("small_with_header.csv");
    let head = std::fs::read(&path).expect("read fixture");
    let head = String::from_utf8_lossy(&head[..head.len().min(4096)]).into_owned();
    sess.roundtrip(
        "can_handle",
        json!({
            "path": path,
            "name": "small_with_header.csv",
            "ext": "csv",
            "size_bytes": std::fs::metadata(&path).unwrap().len(),
            "head_sample": head,
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

    // 6. load_file → schema（冻结列集）。
    sess.roundtrip("load_file", json!({ "file_id": "f1", "path": path }));
    sess.roundtrip("schema", json!(null));

    // 7. parse：progress/RecordBatch 通知 + records_total 响应。
    sess.roundtrip("parse", json!({ "file_id": "f1" }));

    // 8. 未加载文件上 parse：-32602（确定性 data）。
    sess.roundtrip("parse", json!({ "file_id": "ghost" }));

    // 9. key_values（时间范围中点）→ unload（幂等两次）。
    //    中点由夹具首末时间推得，见 golden 内 key_values 响应。
    let frames = &sess.transcript;
    let summary_line = frames
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

    // 10. shutdown → EOF。
    sess.roundtrip("shutdown", json!(null));
    let rc = sess.finish();
    assert_eq!(rc, 0, "shutdown then EOF must exit 0");

    compare_or_update(&sess.transcript);
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
