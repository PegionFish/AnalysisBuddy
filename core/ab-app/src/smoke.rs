//! `--smoke-host` 冒烟（P3-01 验证命令）：对 mock-plugin 回放器
//! （`tools/mock-plugin/scripts/happy_path.ndjson`）走
//! 握手 → parse_stream → key_values → shutdown 全流程，并验证事件转换
//! （ipc-ui.md §2）。`cargo run -p ab-app -- --smoke-host`。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ab_host::{PluginProcessState, PluginRegistry, PluginRuntime, RuntimeConfig};
use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};
use ab_protocol::types::{KeyValuesParams, LoadFileParams, ParseParams};
use tokio::sync::mpsc;

use crate::events::{self, ProgressThrottle};
use crate::host_bridge::{HostSessionAdapter, ParseEvent, PluginSession};

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-app-smoke-{}-{}-{tag}",
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

/// mock-plugin 可执行文件（缺失时现场构建）。
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

/// 冒烟主流程；返回进程退出码（0 = 全绿）。
pub fn run_smoke() -> i32 {
    println!("smoke-host: AnalysisBuddy ab-app --smoke-host");
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    match runtime.block_on(smoke_flow()) {
        Ok(()) => {
            println!("smoke-host: ALL GREEN");
            0
        }
        Err(e) => {
            eprintln!("smoke-host: FAILED: {e}");
            1
        }
    }
}

async fn smoke_flow() -> Result<(), String> {
    let tmp = TempDir::new("happy");
    let script = repo_script("happy_path.ndjson");
    install_mock_plugin(&tmp.path().join("mock"), &script);
    println!(
        "smoke-host: mock-plugin @ {} (script {})",
        mock_plugin_bin().display(),
        script.display()
    );

    let registry = Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let discovered = registry.discover();
    let mock = discovered
        .plugins
        .iter()
        .find(|p| p.manifest.id == "mock")
        .ok_or("mock plugin not discovered")?;
    println!(
        "smoke-host: discovered plugin `{}` v{} ({})",
        mock.manifest.id, mock.manifest.version, mock.source
    );

    let runtime = PluginRuntime::with_config(registry.clone(), RuntimeConfig::default());
    let mut events_rx = runtime.subscribe_events();
    let mut throttle = ProgressThrottle::new();

    // 握手（get_or_spawn 封装 spawn → 250ms 快速退出检测 → initialize → Ready）。
    let session = runtime
        .get_or_spawn("mock")
        .await
        .map_err(|e| format!("handshake failed: {e}"))?;
    if session.state() != PluginProcessState::Ready {
        return Err(format!("handshake state not Ready: {:?}", session.state()));
    }
    println!("smoke-host: handshake OK (state ready)");

    let adapter = HostSessionAdapter::new(session.clone());
    if adapter.plugin_id() != "mock" {
        return Err("plugin_id mismatch".to_string());
    }

    adapter
        .load_file(LoadFileParams {
            file_id: FILE_ID.to_string(),
            path: "C:\\logs\\match.csv".to_string(),
        })
        .await
        .map_err(|e| format!("load_file failed: {e}"))?;
    println!("smoke-host: load_file OK");

    // parse_stream：有界 sink，收集 RecordBatch / progress。
    let (tx, mut rx) = mpsc::channel::<ParseEvent>(16);
    let collector = tokio::spawn(async move {
        let mut batches = 0u64;
        let mut records = 0u64;
        let mut progress = 0u64;
        while let Some(event) = rx.recv().await {
            match event {
                ParseEvent::Batch(b) => {
                    batches += 1;
                    records += b.records.len() as u64;
                }
                ParseEvent::Progress(_) => progress += 1,
            }
        }
        (batches, records, progress)
    });
    let records_total = adapter
        .parse_stream(
            ParseParams {
                file_id: FILE_ID.to_string(),
                options: None,
            },
            tx,
        )
        .await
        .map_err(|e| format!("parse_stream failed: {e}"))?;
    let (batches, records, progress) = collector.await.expect("collector");
    if records_total != 3 {
        return Err(format!("records_total = {records_total}, expected 3"));
    }
    if batches != 2 || records != 3 {
        return Err(format!(
            "batch stream wrong: {batches} batches / {records} records, expected 2 / 3"
        ));
    }
    println!(
        "smoke-host: parse OK (records_total={records_total}, batches={batches}, progress={progress})"
    );

    let kv = adapter
        .key_values(KeyValuesParams {
            file_id: FILE_ID.to_string(),
            timestamp_ms: 1785603599870,
        })
        .await
        .map_err(|e| format!("key_values failed: {e}"))?;
    if kv.entries.is_empty() {
        return Err("key_values returned empty entries".to_string());
    }
    println!("smoke-host: key_values OK ({} entries)", kv.entries.len());

    // 优雅停机 + 宿主全量停机（§3.4 孤儿防护第 2 层）。
    session
        .shutdown()
        .await
        .map_err(|e| format!("shutdown failed: {e}"))?;
    if session.state() != PluginProcessState::Shutdown {
        return Err(format!("state not Shutdown: {:?}", session.state()));
    }
    runtime.shutdown_all().await;
    println!("smoke-host: shutdown OK (state shutdown, orphans swept)");

    // 事件转换抽样：状态机迁移 → ab://plugin-health 载荷。
    let mut health = 0;
    while let Ok(event) = events_rx.try_recv() {
        for emitted in events::convert(event, &mut throttle) {
            let line = match &emitted.payload {
                events::EventPayload::Health(payload) => {
                    serde_json::to_string(payload).expect("serialize health")
                }
                events::EventPayload::Log(payload) => {
                    serde_json::to_string(payload).expect("serialize log")
                }
                events::EventPayload::Progress(payload) => {
                    serde_json::to_string(payload).expect("serialize progress")
                }
            };
            println!("smoke-host: {} {line}", emitted.channel);
            if let events::EventPayload::Health(_) = emitted.payload {
                health += 1;
            }
        }
    }
    if health == 0 {
        return Err("no ab://plugin-health events converted".to_string());
    }
    println!("smoke-host: event conversion OK ({health} health events)");
    Ok(())
}
