//! G2 三插件语义等价 golden（qa-perf.md §3 补充）：builtin-csv / aibench-llama /
//! mock-plugin 对同一请求序列的行为在**语义层**等价。
//!
//! 与各插件 crate 内的逐字节 golden（`golden_stdio` 转录）互补：这里断言的是
//! 「协议骨架等价」——
//! ① initialize：id == manifest id、`protocol_version` 缺省/回显 = 1（G1）、
//!    `capabilities.annotate == false`；
//! ② 未知方法 → `-32601` + `"Method not found"`（mock 无数据键，语义同判）；
//! ③ annotate → `-32005`（mock 以剧本回放同一错误，message 可比）；
//! ④ shutdown → 空对象应答、EOF 后退出码 0。
//!
//! -32002（file load failed）的 `detail` 含 OS errno 文案，逐字节不可移植，
//! 在此做**语义**断言（code == -32002），补齐逐字节 golden 有意排除的错误路径。
//!
//! 二进制定位：builtin-csv / aibench-llama 为独立 workspace，取
//! `plugins/<id>/target/release/<id>[.exe]`（缺失 → SKIP，与 e2e_real_plugins
//! 同策略）；mock-plugin 随宿主 workspace（CARGO_TARGET_DIR 感知，缺失自动构建，
//! 与 e2e_mock_suite 同策略）。

use std::path::PathBuf;

use ab_e2e::fixtures_ref;
use ab_e2e::harness::{HostError, PluginInvocation, PluginSession};
use serde_json::{json, Value};

/// mock-plugin 语义套件剧本：与两真实插件同一错误面（annotate -32005 固定文案、
/// load 失败 -32002），initialize 与历史逐字形状一致。
const MOCK_SCRIPT: &str = concat!(
    r#"{"kind":"reply","method":"initialize","result":{"id":"mock","name":"Mock","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}}"#,
    "\n",
    r#"{"kind":"reply","method":"annotate","error":{"code":-32005,"message":"annotate is not supported by this plugin"}}"#,
    "\n",
    r#"{"kind":"reply","method":"load_file","error":{"code":-32002,"message":"file load failed"}}"#,
    "\n",
    r#"{"kind":"reply","method":"shutdown","result":{}}"#,
    "\n",
);

/// mock-plugin 的 annotate/load_file 走剧本回放，错误消息可逐字与真实插件对齐。
const ANNOTATE_MSG: &str = "annotate is not supported by this plugin";
const METHOD_NOT_FOUND_MSG: &str = "Method not found";

enum Bin {
    /// 独立 workspace 插件：manifest + 产物路径。
    Real { dir: PathBuf, id: &'static str },
    /// 宿主 workspace 内的 mock-plugin（剧本回放）。
    Mock { script: PathBuf },
}

fn write_mock_script() -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("ab-e2e-golden-mock-{}.ndjson", std::process::id()));
    std::fs::write(&path, MOCK_SCRIPT).expect("write mock script");
    path
}

/// 三插件定位（builtin-csv / aibench-llama 未构建 → None，测试跳过）。
fn resolve_all() -> Vec<(&'static str, Option<Bin>)> {
    let ws = fixtures_ref::workspace_root();
    let exe_name = |id: &str| {
        if cfg!(windows) {
            format!("{id}.exe")
        } else {
            id.to_string()
        }
    };
    let real = |id: &'static str| {
        let dir = ws.join("plugins").join(id);
        let bin = dir.join("target/release").join(exe_name(id));
        let manifest = dir.join("plugin.json");
        if bin.exists() && manifest.exists() {
            Some(Bin::Real { dir, id })
        } else {
            None
        }
    };
    vec![
        ("builtin-csv", real("builtin-csv")),
        ("aibench-llama", real("aibench-llama")),
        (
            "mock-plugin",
            Some(Bin::Mock {
                script: write_mock_script(),
            }),
        ),
    ]
}

fn invocation(bin: &Bin) -> PluginInvocation {
    match bin {
        Bin::Real { dir, id } => PluginInvocation {
            exe: dir.join("target/release").join(exe_of(id)),
            args: vec!["--stdio".to_string()],
            working_dir: Some(dir.clone()),
        },
        Bin::Mock { script } => PluginInvocation {
            exe: mock_plugin_bin(),
            args: vec![
                "--script".to_string(),
                script.to_string_lossy().into_owned(),
            ],
            working_dir: None,
        },
    }
}

fn exe_of(id: &str) -> String {
    if cfg!(windows) {
        format!("{id}.exe")
    } else {
        id.to_string()
    }
}

/// mock-plugin 二进制定位（与 e2e_mock_suite 同策略：CARGO_TARGET_DIR 感知 + 按需构建）。
fn mock_plugin_bin() -> PathBuf {
    let ws = fixtures_ref::workspace_root();
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| ws.join("target"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let bin_name = if cfg!(windows) {
        "mock-plugin.exe"
    } else {
        "mock-plugin"
    };
    let bin = target_dir.join(profile).join(bin_name);
    if !bin.exists() {
        let status = std::process::Command::new("cargo")
            .current_dir(&ws)
            .args(["build", "-p", "mock-plugin"])
            .status()
            .expect("cargo build -p mock-plugin");
        assert!(status.success(), "cargo build -p mock-plugin failed");
    }
    assert!(bin.exists(), "mock-plugin 二进制缺失: {}", bin.display());
    bin
}

fn rpc_code(err: &HostError) -> i32 {
    match err {
        HostError::Rpc { code, .. } => *code,
        other => panic!("预期 JSON-RPC error，得到 {other:?}"),
    }
}

fn rpc_message(err: &HostError) -> String {
    match err {
        HostError::Rpc { message, .. } => message.clone(),
        other => panic!("预期 JSON-RPC error，得到 {other:?}"),
    }
}

/// 单插件的语义等价序列：initialize → 未知方法 → annotate → -32002 → shutdown/EOF。
fn drive_golden(name: &str, bin: &Bin, expect_id: &str) {
    let inv = invocation(bin);
    let mut s =
        PluginSession::spawn(&inv, 1 << 20).unwrap_or_else(|e| panic!("[{name}] spawn: {e}"));

    // ① initialize：id / 版本回显 / 能力位。
    let init = s
        .initialize("AnalysisBuddy-golden", "0.1.0")
        .unwrap_or_else(|e| panic!("[{name}] initialize: {}", e.message()));
    assert_eq!(init["id"], expect_id, "[{name}] initialize id");
    let pv = init
        .get("protocol_version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    assert_eq!(pv, 1, "[{name}] protocol_version 缺省/回显必须为 1（G1）");
    assert_eq!(
        init["capabilities"]["annotate"], false,
        "[{name}] annotate 能力未声明"
    );
    assert_eq!(
        init["capabilities"]["subscribe"], false,
        "[{name}] subscribe 能力未声明"
    );

    // ② 未知方法：-32601 + 固定 message。
    let err = s
        .request("subscribe", json!({}))
        .expect_err("[{name}] subscribe 应被拒");
    assert_eq!(rpc_code(&err), -32601, "[{name}] 未知方法错误码");
    assert_eq!(
        rpc_message(&err),
        METHOD_NOT_FOUND_MSG,
        "[{name}] 未知方法 message"
    );

    // ③ annotate：-32005 + 固定 message。
    let err = s
        .request(
            "annotate",
            json!({ "file_id": "f1", "range": { "start_ms": 0, "end_ms": 1 } }),
        )
        .expect_err("[{name}] annotate 应被拒");
    assert_eq!(rpc_code(&err), -32005, "[{name}] annotate 错误码");
    assert_eq!(rpc_message(&err), ANNOTATE_MSG, "[{name}] annotate message");

    // ④ -32002（OS errno 文案不逐字节比对，语义断言；mock 由剧本回放同一错误码）。
    let err = s
        .request(
            "load_file",
            json!({ "file_id": "f1", "path": "/x/ab-golden-missing.csv" }),
        )
        .expect_err("[{name}] load_file 应失败");
    assert_eq!(rpc_code(&err), -32002, "[{name}] load 失败错误码");

    // ⑤ shutdown → 空对象应答 + 退出码 0。
    s.shutdown()
        .unwrap_or_else(|e| panic!("[{name}] shutdown: {}", e.message()));
    assert_eq!(
        s.exit_status().and_then(|st| st.code()),
        Some(0),
        "[{name}] 退出码 0"
    );
}

#[test]
fn golden_semantic_equivalence_three_plugins() {
    let mut missing = Vec::new();
    for (name, bin) in resolve_all() {
        let Some(bin) = bin else {
            missing.push(name);
            continue;
        };
        let expect_id = match bin {
            Bin::Real { id, .. } => id,
            Bin::Mock { .. } => "mock",
        };
        drive_golden(name, &bin, expect_id);
    }
    if !missing.is_empty() {
        println!(
            "[SKIP 部分] 未构建的插件（各自目录 cargo build --release 后自动激活）: {missing:?}"
        );
    }
}
