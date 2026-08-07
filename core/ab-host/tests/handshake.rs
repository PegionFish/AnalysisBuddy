//! A-02 握手与全流程集成测试（host-runtime.md §3/§4 DoD）：
//! 以 mock-plugin 真实子进程验证 spawn → initialize → Ready → RPC → shutdown，
//! 以及 RecordBatch seq 缺号/重复终止会话、启动即崩溃 → Crashed。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ab_host::{HostEvent, PluginProcessState, PluginRegistry, PluginRuntime};
use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};
use ab_protocol::types::{
    CanHandleParams, KeyValuesParams, LoadFileParams, ParseParams, UnloadFileParams,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-host-hs-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&base).expect("create tempdir");
        Self(base)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// mock-plugin 可执行文件路径（dev-dependency bin，无 CARGO_BIN_EXE_* 环境变量；
/// 从 workspace target 目录解析；缺失时兜底构建一次）。
fn mock_plugin_bin() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../target"));
    let bin = target_dir.join("debug").join(if cfg!(windows) {
        "mock-plugin.exe"
    } else {
        "mock-plugin"
    });
    if !bin.exists() {
        let out = std::process::Command::new("cargo")
            .args(["build", "-p", "mock-plugin"])
            .current_dir(&manifest_dir)
            .output()
            .expect("cargo build -p mock-plugin");
        assert!(
            out.status.success(),
            "cargo build -p mock-plugin failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    bin
}

/// 仓库内剧本的绝对路径。
fn repo_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/mock-plugin/scripts")
        .join(name)
}

/// 把 mock-plugin 装成一个可发现的插件：manifest id = "mock"，
/// entry.command 指向 mock-plugin.exe，args 携带剧本。
fn install_mock_plugin(dir: &Path, script: &Path, manifest_id: &str) {
    fs::create_dir_all(dir).expect("mkdir plugin dir");
    let manifest = Manifest {
        id: manifest_id.to_string(),
        display_name: "Mock Replay Plugin".to_string(),
        version: "0.1.0".to_string(),
        entry: PluginEntry {
            command: mock_plugin_bin().to_string_lossy().into_owned(),
            args: vec![
                "--script".to_string(),
                script.to_string_lossy().into_owned(),
            ],
            working_dir: None,
        },
        r#match: MatchRules {
            extensions: vec!["csv".to_string()],
            header_fingerprints: None,
        },
        min_protocol_version: 1,
    };
    fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write plugin.json");
}

const HAPPY_SCRIPT: &str = "happy_path.ndjson";
const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

#[tokio::test]
async fn seq_gap_terminates_session() {
    let tmp = TempDir::new("seq-gap");
    // 自造剧本：seq 0 后直接跳 seq 2（缺号）。
    let script = tmp.path().join("seq_gap.ndjson");
    fs::write(
        &script,
        concat!(
            r#"{"kind":"reply","method":"initialize","result":{"id":"mock","name":"Mock","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}}"#,
            "\n",
            r#"{"kind":"emit","method":"RecordBatch","params":{"file_id":"","seq":0,"records":[],"done":false}}"#,
            "\n",
            r#"{"kind":"emit","method":"RecordBatch","params":{"file_id":"","seq":2,"records":[],"done":true}}"#,
            "\n",
            r#"{"kind":"reply","method":"parse","result":{"records_total":0}}"#,
            "\n",
        ),
    )
    .expect("write gap script");

    let plugin_dir = tmp.path().join("mock");
    install_mock_plugin(&plugin_dir, &script, "mock");
    let registry = std::sync::Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let runtime = PluginRuntime::new(registry);
    let mut events = runtime.subscribe_events();

    let session = runtime.get_or_spawn("mock").await.expect("spawn");
    assert_eq!(session.state(), PluginProcessState::Ready);

    let err = session
        .parse(ParseParams {
            file_id: "f1".into(),
            options: None,
        })
        .await
        .expect_err("seq gap must fail the parse");
    match &err {
        ab_host::HostError::Protocol { code, message, .. } => {
            assert_eq!(
                *code, -32003,
                "in-flight request completed with -32003: {err}"
            );
            assert_eq!(message, "plugin process exited");
        }
        other => panic!("expected protocol -32003, got {other:?}"),
    }
    assert_eq!(session.state(), PluginProcessState::Crashed);

    // 会话终止事件已发出。
    let terminated = std::iter::from_fn(|| events.try_recv().ok())
        .any(|ev| matches!(ev, HostEvent::SessionTerminated { .. }));
    assert!(terminated, "SessionTerminated must be published");

    // 后续调用快速失败（通道已死）。
    let late = session.schema().await;
    assert!(late.is_err(), "dead session rejects further calls");
    runtime.shutdown_all().await;
}

#[tokio::test]
async fn happy_path_spawn_handshake_rpc_shutdown() {
    let tmp = TempDir::new("happy");
    let plugin_dir = tmp.path().join("mock");
    install_mock_plugin(&plugin_dir, &repo_script(HAPPY_SCRIPT), "mock");

    let registry = std::sync::Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let runtime = PluginRuntime::new(registry.clone());
    let mut events = runtime.subscribe_events();

    let session = runtime
        .get_or_spawn("mock")
        .await
        .expect("spawn + handshake");
    assert_eq!(session.plugin_id(), "mock");
    assert_eq!(session.state(), PluginProcessState::Ready);
    assert!(session.loaded_files().is_empty(), "no file loaded yet");

    // schema：3 个指标。
    let schema = session.schema().await.expect("schema");
    assert_eq!(schema.metrics.len(), 3);

    // can_handle。
    let can = session
        .can_handle(CanHandleParams {
            path: "C:\\logs\\match.csv".into(),
            name: "match.csv".into(),
            ext: "csv".into(),
            size_bytes: 1024,
            head_sample: "timestamp,fps,frame_ms".into(),
        })
        .await
        .expect("can_handle");
    assert!(can.can_handle);

    // load_file → loaded_files 跟踪。
    let summary = session
        .load_file(LoadFileParams {
            file_id: FILE_ID.into(),
            path: "C:\\logs\\match.csv".into(),
        })
        .await
        .expect("load_file");
    assert_eq!(summary.record_count_hint, Some(3));
    assert_eq!(session.loaded_files(), [FILE_ID]);

    // parse：通知流（2×progress + RecordBatch seq 0/1）+ 最终响应。
    let mut notifications = session.subscribe_notifications();
    let parse = session
        .parse(ParseParams {
            file_id: FILE_ID.into(),
            options: None,
        })
        .await
        .expect("parse");
    assert_eq!(parse.records_total, 3);

    let mut progress = 0u64;
    let mut seqs = Vec::new();
    let mut done = false;
    while let Ok(notification) =
        tokio::time::timeout(Duration::from_secs(2), notifications.recv()).await
    {
        let notification = notification.expect("notification");
        match notification {
            ab_host::PluginNotification::Progress(p) => {
                progress += 1;
                assert_eq!(p.file_id, FILE_ID);
            }
            ab_host::PluginNotification::RecordBatch(b) => {
                assert_eq!(b.file_id, FILE_ID);
                seqs.push(b.seq);
                done = b.done;
                if b.done {
                    break;
                }
            }
        }
    }
    assert!(progress >= 1, "at least one progress notification");
    assert_eq!(seqs, [0, 1], "RecordBatch seq 0 → 1");
    assert!(done, "final batch marked done");

    // key_values / unload_file。
    let kv = session
        .key_values(KeyValuesParams {
            file_id: FILE_ID.into(),
            timestamp_ms: 1785601234567,
        })
        .await
        .expect("key_values");
    assert_eq!(kv.entries.len(), 3);
    session
        .unload_file(UnloadFileParams {
            file_id: FILE_ID.into(),
        })
        .await
        .expect("unload_file");
    assert!(session.loaded_files().is_empty());

    // shutdown：优雅停机 → Shutdown，退出码 0。
    session.shutdown().await.expect("shutdown");
    assert_eq!(session.state(), PluginProcessState::Shutdown);

    // 事件流核对：Ready 前存在 Spawning→Initializing→Ready 转移。
    let states: Vec<PluginProcessState> = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|ev| match ev {
            HostEvent::StateChanged { to, .. } => Some(to),
            _ => None,
        })
        .collect();
    let have = |s: PluginProcessState| states.contains(&s);
    assert!(have(PluginProcessState::Spawning), "states: {states:?}");
    assert!(have(PluginProcessState::Initializing), "states: {states:?}");
    assert!(have(PluginProcessState::Ready), "states: {states:?}");

    runtime.shutdown_all().await;
}

#[tokio::test]
async fn duplicate_seq_terminates_session() {
    let tmp = TempDir::new("seq-dup");
    let script = tmp.path().join("seq_dup.ndjson");
    fs::write(
        &script,
        concat!(
            r#"{"kind":"reply","method":"initialize","result":{"id":"mock","name":"Mock","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}}"#,
            "\n",
            r#"{"kind":"emit","method":"RecordBatch","params":{"file_id":"","seq":0,"records":[],"done":false}}"#,
            "\n",
            r#"{"kind":"emit","method":"RecordBatch","params":{"file_id":"","seq":0,"records":[],"done":true}}"#,
            "\n",
            r#"{"kind":"reply","method":"parse","result":{"records_total":0}}"#,
            "\n",
        ),
    )
    .expect("write dup script");

    let plugin_dir = tmp.path().join("mock");
    install_mock_plugin(&plugin_dir, &script, "mock");
    let registry = std::sync::Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let runtime = PluginRuntime::new(registry);

    let session = runtime.get_or_spawn("mock").await.expect("spawn");
    let err = session
        .parse(ParseParams {
            file_id: "f1".into(),
            options: None,
        })
        .await
        .expect_err("duplicate seq must terminate the session");
    assert!(
        matches!(&err, ab_host::HostError::Protocol { code: -32003, .. }),
        "expected -32003 transport completion, got {err:?}"
    );
    assert_eq!(session.state(), PluginProcessState::Crashed);
    runtime.shutdown_all().await;
}

#[tokio::test]
async fn immediate_crash_after_spawn_is_crashed_with_exit_code() {
    let tmp = TempDir::new("crash");
    // 剧本路径不存在 → mock-plugin 加载失败立即退出（退出码 1）。
    let plugin_dir = tmp.path().join("mock");
    install_mock_plugin(&plugin_dir, &tmp.path().join("nope.ndjson"), "mock");

    let registry = std::sync::Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let runtime = PluginRuntime::new(registry);
    let mut events = runtime.subscribe_events();

    let err = runtime.get_or_spawn("mock").await.expect_err("must fail");
    assert!(
        err.to_string().contains("exited immediately"),
        "expected immediate-exit error, got {err}"
    );
    let terminated = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|ev| match ev {
            HostEvent::SessionTerminated {
                exit_code, summary, ..
            } => Some((exit_code, summary)),
            _ => None,
        })
        .next()
        .expect("SessionTerminated published");
    assert_eq!(
        terminated.0,
        Some(1),
        "mock-plugin exits 1 on missing script"
    );
    runtime.shutdown_all().await;
}
