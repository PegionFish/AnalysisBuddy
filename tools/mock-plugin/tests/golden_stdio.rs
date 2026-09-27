//! G2 golden 行为等价测试：固定请求序列 → stdout 协议帧**逐字节**比对。
//!
//! mock-plugin 保持剧本回放机制不迁移 ab-plugin-rt（剧本 emit/sleep 与宿主
//! file_id 改写等语义与通用插件 runtime 不贴合，保留理由见仓库 G2 记录）；
//! 本 golden 将其线上行为钉死：三插件 golden 等价（任务卡 G2）至少覆盖
//! initialize + 错误路径 —— 剧本化 -32002（确定性文案，无 OS errno 噪声）、
//! 未知方法 -32601、未声明 custom_query 的 -32005。
//! 重捕：`GOLDEN_UPDATE=1 cargo test -p mock-plugin --test golden_stdio`。

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
    fn start(extra_args: &[&str]) -> Session {
        let script = golden_dir().join("replay.ndjson");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mock-plugin"));
        cmd.arg("--script").arg(script);
        cmd.args(extra_args);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn mock-plugin");
        let stdout = child.stdout.take().expect("stdout");
        Session {
            child,
            reader: BufReader::new(stdout),
            transcript: Vec::new(),
            next_id: 1,
        }
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
        let stdin = self.child.stdin.as_mut().expect("stdin");
        stdin
            .write_all(serde_json::to_string(&frame).unwrap().as_bytes())
            .unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read stdout");
            assert!(!line.is_empty(), "mock-plugin exited prematurely");
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

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

/// 剧本回放主序列：initialize + 错误路径 + emit 改写（无已加载文件 → 占位原样）。
#[test]
fn golden_stdio_transcript() {
    let mut sess = Session::start(&[]);

    let init = sess.roundtrip(
        "initialize",
        json!({
            "protocol_version": 1,
            "host_info": { "name": "AnalysisBuddy", "version": "0.1.0" },
        }),
    );
    assert_eq!(init["result"]["id"], "mock");
    assert_eq!(
        sess.roundtrip("subscribe", json!(null))["error"]["code"],
        -32601,
        "未知方法 -32601（剧本无块）"
    );
    // 剧本化 -32002：detail 为剧本固定文案（无 OS errno 噪声，逐字节稳定）。
    sess.roundtrip("load_file", json!({ "file_id": "f1", "path": "/x/a.csv" }));
    // parse：emit progress 中 file_id 无已加载文件可改写 → 占位原样透传。
    sess.roundtrip("parse", json!({ "file_id": "f1" }));
    // 未声明 custom_query → -32005 + 固定 message（任务契约钉死）。
    sess.roundtrip(
        "custom_query",
        json!({ "file_id": "f1", "query": "echo", "params": { "k": "v" } }),
    );

    sess.roundtrip("shutdown", json!(null));
    let rc = sess.finish();
    assert_eq!(rc, 0, "shutdown then EOF must exit 0");
    compare_or_update(&sess.transcript, "stdio.transcript");
}

/// `--caps custom_query`：initialize 能力位翻转 + 内置 echo/未知 query 分支。
#[test]
fn golden_stdio_caps_custom_query_transcript() {
    let mut sess = Session::start(&["--caps", "custom_query"]);

    let init = sess.roundtrip(
        "initialize",
        json!({
            "protocol_version": 1,
            "host_info": { "name": "AnalysisBuddy", "version": "0.1.0" },
        }),
    );
    assert_eq!(init["result"]["capabilities"]["custom_query"], true);

    let echo = sess.roundtrip(
        "custom_query",
        json!({ "file_id": "f1", "query": "echo", "params": { "k": "v" } }),
    );
    assert_eq!(echo["result"]["data"]["echo"]["file_id"], "f1");
    let unknown = sess.roundtrip(
        "custom_query",
        json!({ "file_id": "f1", "query": "nope" }),
    );
    assert_eq!(unknown["error"]["code"], -32602);

    sess.roundtrip("shutdown", json!(null));
    let rc = sess.finish();
    assert_eq!(rc, 0, "shutdown then EOF must exit 0");
    compare_or_update(&sess.transcript, "stdio_caps_custom_query.transcript");
}

fn compare_or_update(transcript: &[String], file: &str) {
    let path = golden_dir().join(file);
    let actual = transcript.join("\n") + "\n";
    if std::env::var_os("GOLDEN_UPDATE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir golden dir");
        std::fs::write(&path, &actual).expect("write golden");
        eprintln!("golden updated: {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read golden {}: {e}（GOLDEN_UPDATE=1 重捕）", path.display()));
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
