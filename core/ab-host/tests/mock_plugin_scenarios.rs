//! A-03 容错端到端验收（host-runtime.md §5/§6/§8；附录 A14）：
//! mock-plugin 三剧本 —— ① 正常全流程无告警；② 心跳停止 → Timeout 并丢弃已收批次；
//! ③ 立即崩溃 → 自动重试 2 次后熔断，`SessionTerminated` 附 stderr tail_summary。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ab_host::{
    retry_loop, BreakerState, CircuitBreaker, HostEvent, PluginProcessState, PluginRegistry,
    PluginRuntime, RetryPolicy, RuntimeConfig,
};
use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};
use ab_protocol::types::{LoadFileParams, ParseParams, UnloadFileParams};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-host-a3-{}-{}-{tag}",
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

fn repo_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/mock-plugin/scripts")
        .join(name)
}

fn install_mock_plugin(dir: &Path, script: &Path) {
    fs::create_dir_all(dir).expect("mkdir plugin dir");
    let manifest = Manifest {
        id: "mock".to_string(),
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

fn runtime(tmp: &TempDir, config: RuntimeConfig) -> (Arc<PluginRegistry>, PluginRuntime) {
    let registry = Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let runtime = PluginRuntime::with_config(registry.clone(), config);
    (registry, runtime)
}

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

/// 等待 `n` 个 `SessionTerminated` 事件（带 10s 超时），返回各自的 summary 与退出码。
async fn wait_for_terminations(
    events: &mut tokio::sync::broadcast::Receiver<HostEvent>,
    n: usize,
) -> Vec<(Option<i32>, String)> {
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while out.len() < n {
            if let Ok(ev) = events.try_recv() {
                if let HostEvent::SessionTerminated {
                    exit_code, summary, ..
                } = ev
                {
                    out.push((exit_code, summary));
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("SessionTerminated events must arrive in time");
    out
}

/// 剧本①：正常握手 → parse（progress 心跳 + RecordBatch）→ shutdown 全程无告警。
#[tokio::test]
async fn scenario1_happy_path_no_alarms() {
    let tmp = TempDir::new("s1");
    install_mock_plugin(&tmp.path().join("mock"), &repo_script("happy_path.ndjson"));
    let (_registry, runtime) = runtime(&tmp, RuntimeConfig::default());
    let mut events = runtime.subscribe_events();

    let session = runtime.get_or_spawn("mock").await.expect("spawn");
    assert_eq!(session.state(), PluginProcessState::Ready);

    // load → parse（心跳 + 批次）→ key_values → unload。
    session
        .load_file(LoadFileParams {
            file_id: FILE_ID.into(),
            path: "C:\\logs\\match.csv".into(),
        })
        .await
        .expect("load_file");
    let mut notifications = session.subscribe_notifications();
    let parse = session
        .parse(ParseParams {
            file_id: FILE_ID.into(),
            options: None,
        })
        .await
        .expect("parse");
    assert_eq!(parse.records_total, 3);
    let mut seqs = Vec::new();
    while let Ok(notification) =
        tokio::time::timeout(Duration::from_secs(2), notifications.recv()).await
    {
        let notification = notification.expect("notification");
        match notification {
            ab_host::PluginNotification::Progress(_) => {}
            ab_host::PluginNotification::RecordBatch(b) => {
                seqs.push(b.seq);
                if b.done {
                    break;
                }
            }
        }
    }
    assert_eq!(seqs, [0, 1], "heartbeat + batches streamed cleanly");
    session
        .unload_file(UnloadFileParams {
            file_id: FILE_ID.into(),
        })
        .await
        .expect("unload_file");

    // 优雅停机：退出码 0 → Shutdown。
    session.shutdown().await.expect("shutdown");
    assert_eq!(session.state(), PluginProcessState::Shutdown);

    // 全程无告警：无 Crashed/Timeout 转移、无协议错误、终止事件带退出码 0。
    let mut alarmed = false;
    while let Ok(ev) = events.try_recv() {
        match ev {
            HostEvent::StateChanged { to, .. } => {
                if matches!(
                    to,
                    PluginProcessState::Crashed | PluginProcessState::Timeout
                ) {
                    alarmed = true;
                }
            }
            HostEvent::SessionTerminated { exit_code, .. } => {
                assert_eq!(exit_code, Some(0), "graceful exit code 0");
            }
            HostEvent::Progress(_) | HostEvent::StderrLine { .. } => {}
            other => panic!("unexpected event during happy path: {other:?}"),
        }
    }
    assert!(!alarmed, "no crash/timeout alarms on the happy path");
    runtime.shutdown_all().await;
}

/// 剧本②：parse 期间停止心跳（测试注入 1s 窗口）→ Timeout + 丢弃已收批次。
#[tokio::test]
async fn scenario2_heartbeat_stop_times_out_and_discards_batches() {
    let tmp = TempDir::new("s2");
    install_mock_plugin(
        &tmp.path().join("mock"),
        &repo_script("heartbeat_stop.ndjson"),
    );
    let config = RuntimeConfig {
        parse_watchdog_window: Duration::from_secs(1),
        idle_reclaim: Duration::from_secs(300),
    };
    let (_registry, runtime) = runtime(&tmp, config);
    let mut events = runtime.subscribe_events();

    let session = runtime.get_or_spawn("mock").await.expect("spawn");
    session
        .load_file(LoadFileParams {
            file_id: FILE_ID.into(),
            path: "C:\\logs\\match.csv".into(),
        })
        .await
        .expect("load_file");

    let mut notifications = session.subscribe_notifications();
    let parse = session
        .parse(ParseParams {
            file_id: FILE_ID.into(),
            options: None,
        })
        .await
        .expect_err("heartbeat stop must fail the parse");
    assert!(
        matches!(&parse, ab_host::HostError::Protocol { code: -32003, .. }),
        "inflight parse completed with -32003: {parse:?}"
    );

    // 状态 Timeout + 会话终止事件。
    assert_eq!(session.state(), PluginProcessState::Timeout);
    let mut saw_timeout = false;
    while let Ok(ev) = events.try_recv() {
        if let HostEvent::StateChanged { to, .. } = ev {
            if to == PluginProcessState::Timeout {
                saw_timeout = true;
            }
        }
    }
    assert!(saw_timeout, "Timeout state transition must be published");

    // 丢弃已收批次：只收到 seq 0，未等到 seq 1 即终止。
    let mut seqs = Vec::new();
    while let Ok(notification) =
        tokio::time::timeout(Duration::from_millis(300), notifications.recv()).await
    {
        if let ab_host::PluginNotification::RecordBatch(b) = notification.expect("notification") {
            seqs.push(b.seq);
        }
    }
    assert_eq!(
        seqs,
        [0],
        "batch seq 0 received, seq 1 discarded with the session"
    );

    // 重试可重建会话（§5.2 吸收态语义：新实例从 Discovered 重走）。
    let session2 = runtime
        .get_or_spawn("mock")
        .await
        .expect("respawn after timeout");
    assert_eq!(session2.state(), PluginProcessState::Ready);
    assert_ne!(
        session2.session_seq(),
        session.session_seq(),
        "new session instance"
    );
    runtime.shutdown_all().await;
}

/// 剧本③：进程立即崩溃 → 自动重试 2 次（1s/3s 退避，测试注入毫秒级）→ 熔断，
/// `SessionTerminated` 附 stderr tail_summary。
#[tokio::test]
async fn scenario3_crash_retries_then_breaker_open_with_stderr_summary() {
    let tmp = TempDir::new("s3");
    // 剧本路径不存在 → mock-plugin 立即退出码 1（每次尝试都崩溃）。
    install_mock_plugin(&tmp.path().join("mock"), &tmp.path().join("nope.ndjson"));
    let (_registry, runtime) = runtime(&tmp, RuntimeConfig::default());
    let mut events = runtime.subscribe_events();

    let policy = RetryPolicy {
        max_auto_retries: 2,
        backoffs: [Duration::from_millis(60), Duration::from_millis(120)],
    };
    let mut breaker = CircuitBreaker::new();
    let started = std::time::Instant::now();

    let result = retry_loop(&policy, &mut breaker, || runtime.get_or_spawn("mock")).await;
    assert!(result.is_err(), "all attempts crash");
    assert_eq!(
        breaker.state(),
        BreakerState::Open,
        "breaker open after 3 failures"
    );
    assert_eq!(breaker.failures(), 3);
    // 固定退避 60ms + 120ms（不加抖动）。
    assert!(
        started.elapsed() >= Duration::from_millis(170),
        "backoff delays applied"
    );

    // 3 次尝试 → 3 个 SessionTerminated，末次附 stderr tail_summary。
    let summaries = wait_for_terminations(&mut events, 3).await;
    assert_eq!(summaries.len(), 3, "one SessionTerminated per attempt");
    for (code, _) in &summaries {
        assert_eq!(*code, Some(1), "mock-plugin exits 1 on missing script");
    }
    let joined = summaries
        .iter()
        .map(|(_, s)| s.clone())
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        joined.contains("mock-plugin") && joined.contains("ERROR"),
        "SessionTerminated summary carries stderr tail: {joined:?}"
    );

    // 熔断后手动重试回 Closed（§5.3：不限次数，重置计数）。
    breaker.manual_reset();
    assert_eq!(breaker.state(), BreakerState::Closed);
    assert_eq!(breaker.failures(), 0);
    runtime.shutdown_all().await;
}

/// 附录 A14 全流程（真实退避 1s/3s 版本，慢测试：#![ignore] 由验证命令 --include-ignored 跑）。
#[tokio::test]
#[ignore = "runs with the production 1s/3s backoff (≈4s); covered fast by scenario3"]
async fn a14_full_flow_with_production_backoff() {
    let tmp = TempDir::new("a14");
    install_mock_plugin(&tmp.path().join("mock"), &tmp.path().join("nope.ndjson"));
    let (_registry, runtime) = runtime(&tmp, RuntimeConfig::default());

    let mut breaker = CircuitBreaker::new();
    let result = retry_loop(&RetryPolicy::default(), &mut breaker, || {
        runtime.get_or_spawn("mock")
    })
    .await;
    assert!(result.is_err());
    assert_eq!(breaker.state(), BreakerState::Open);
    runtime.shutdown_all().await;
}
