//! custom_query（§2.11 可选能力，CCP-custom-query addendum）自洽检查：
//! 进程级走 stdio 验证 `--caps` 旗标三态——未声明 `-32005` / `echo` 回显 /
//! 未知 query `-32602`，以及 initialize 能力位与旗标联动、stdout 纯净性不破。

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mock-plugin"))
}

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

/// 一次性会话：写入全部请求行 → 关 stdin → 收全量 stdout/stderr（回放器逐行应答）。
fn exchange(args: &[&str], requests: &[String]) -> (Vec<Value>, String, std::process::ExitStatus) {
    let mut child = bin()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mock-plugin");
    let mut input = requests.join("\n");
    input.push('\n');
    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write requests to stdin");
    drop(child.stdin.take());

    let out = child.wait_with_output().expect("wait for mock-plugin");
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    let frames: Vec<Value> = if stdout.is_empty() {
        Vec::new()
    } else {
        stdout
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("non-JSON stdout line: {e}: {line:?}"))
            })
            .collect()
    };
    (
        frames,
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status,
    )
}

fn req(id: u64, method: &str, params: Value) -> String {
    serde_json::to_string(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .expect("serialize request")
}

#[test]
fn without_caps_capability_is_absent_and_calls_get_unsupported() {
    let requests = [
        req(
            1,
            "initialize",
            json!({"protocol_version":1,"host_info":{"name":"AnalysisBuddy","version":"0.1.0"}}),
        ),
        req(
            2,
            "custom_query",
            json!({"file_id": FILE_ID, "query": "echo"}),
        ),
        req(3, "shutdown", json!({})),
    ];
    let (frames, stderr, status) = exchange(&["--script", "scripts/happy_path.ndjson"], &requests);
    assert!(status.success(), "shutdown 后退出码 0, got {status:?}");

    // initialize：能力对象与旧形状一致——不新增 custom_query 键。
    assert_eq!(frames.len(), 3, "3 应答、无通知: {frames:?}");
    assert_eq!(
        frames[0]["result"]["capabilities"],
        json!({"annotate": false, "subscribe": false, "binary_sidecar": false})
    );
    assert!(
        frames[0]["result"]["capabilities"]
            .get("custom_query")
            .is_none(),
        "未设旗标不得新增能力键"
    );

    // 规则 1：直接调用 custom_query → -32005 + 固定 message。
    assert_eq!(frames[1]["id"], json!(2));
    assert_eq!(frames[1]["error"]["code"], json!(-32005));
    assert_eq!(frames[1]["error"]["message"], "custom_query not supported");

    // shutdown 正常；日志仍走 stderr（stdout 纯净性不破）。
    assert_eq!(frames[2]["result"], json!({}));
    assert!(
        stderr.contains("INFO mock-plugin"),
        "logs on stderr:\n{stderr}"
    );
}

#[test]
fn with_caps_echo_invalid_and_capability_bit() {
    let requests = [
        req(
            1,
            "initialize",
            json!({"protocol_version":1,"host_info":{"name":"AnalysisBuddy","version":"0.1.0"}}),
        ),
        // echo：params 原样回显。
        req(
            2,
            "custom_query",
            json!({"file_id": FILE_ID, "query": "echo", "params": {"k": "v", "n": 1}}),
        ),
        // echo：params 缺省 → 回显空对象。
        req(
            3,
            "custom_query",
            json!({"file_id": FILE_ID, "query": "echo"}),
        ),
        // 未知 query → -32602。
        req(
            4,
            "custom_query",
            json!({"file_id": FILE_ID, "query": "no_such_query"}),
        ),
        // params 不符契约形状（缺 query）→ 同样 -32602。
        req(5, "custom_query", json!({"file_id": FILE_ID})),
        req(6, "shutdown", json!({})),
    ];
    let (frames, _stderr, status) = exchange(
        &[
            "--script",
            "scripts/happy_path.ndjson",
            "--caps",
            "custom_query",
        ],
        &requests,
    );
    assert!(status.success(), "shutdown 后退出码 0, got {status:?}");
    assert_eq!(frames.len(), 6, "6 应答、无通知: {frames:?}");

    // 能力位与旗标联动：custom_query:true，其余键不动。
    assert_eq!(
        frames[0]["result"]["capabilities"],
        json!({"annotate": false, "subscribe": false, "binary_sidecar": false, "custom_query": true})
    );

    // 规则 2：echo 逐字段回显（id 逐帧回显）。
    assert_eq!(frames[1]["id"], json!(2));
    assert_eq!(
        frames[1]["result"],
        json!({"data": {"echo": {"file_id": FILE_ID, "query": "echo", "params": {"k": "v", "n": 1}}}})
    );
    assert_eq!(frames[2]["id"], json!(3));
    assert_eq!(frames[2]["result"]["data"]["echo"]["params"], json!({}));

    // 规则 3：未知 query / 缺 query → -32602。
    assert_eq!(frames[3]["error"]["code"], json!(-32602));
    assert!(
        frames[3]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no_such_query"),
        "message 指明未知 query: {}",
        frames[3]["error"]["message"]
    );
    assert_eq!(frames[4]["error"]["code"], json!(-32602));

    assert_eq!(frames[5]["result"], json!({}));
}

#[test]
fn caps_flag_forms_and_unknown_names() {
    // 可重复 + 逗号分隔 + `=` 形式等价。
    for args in [
        vec![
            "--script",
            "scripts/happy_path.ndjson",
            "--caps",
            "custom_query",
            "--caps",
            "custom_query",
        ],
        vec![
            "--script",
            "scripts/happy_path.ndjson",
            "--caps=custom_query,custom_query",
        ],
    ] {
        let (frames, _stderr, status) = exchange(&args, &[req(1, "shutdown", json!({}))]);
        assert!(status.success());
        // shutdown 前无 initialize：本用例只验证旗标形式可解析、进程正常起停。
        assert!(
            frames.is_empty() || frames[0]["result"] == json!({}),
            "{frames:?}"
        );
    }

    // 未知能力名：退出码 2、无 stdout 输出（纯净性）。
    let out = bin()
        .args([
            "--script",
            "scripts/happy_path.ndjson",
            "--caps",
            "annotate",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("spawn mock-plugin");
    assert_eq!(out.status.code(), Some(2), "unknown capability must exit 2");
    assert!(out.stdout.is_empty(), "stdout must stay pure");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown capability `annotate`"),
        "stderr 指明未知能力名:\n{stderr}"
    );
}
