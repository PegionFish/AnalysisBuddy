//! §2.11 custom_query（可选能力，CCP-custom-query addendum）会话级链路测试：
//! 经真实 mock-plugin 子进程驱动——echo 回包 `{"data":{...}}` 解析，以及未声明
//! 能力时插件 error 帧以 `HostError::Protocol` 原样透传（会话层零归一）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ab_host::{PluginProcessState, PluginRegistry, PluginRuntime};
use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};
use ab_protocol::types::CustomQueryParams;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-host-cq-{}-{}-{tag}",
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

/// mock-plugin 可执行文件路径（同 handshake.rs：从 workspace target 解析，缺失兜底构建）。
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

/// 把 mock-plugin 装成可发现插件；`caps` = 附加 `--caps custom_query`
/// （mock-plugin 据此在 initialize 能力位补 `custom_query:true` 并启用内置分支）。
fn install_mock_plugin(dir: &Path, script: &Path, caps: bool) {
    fs::create_dir_all(dir).expect("mkdir plugin dir");
    let mut args = vec![
        "--script".to_string(),
        script.to_string_lossy().into_owned(),
    ];
    if caps {
        args.push("--caps".to_string());
        args.push("custom_query".to_string());
    }
    let manifest = Manifest {
        id: "mock".to_string(),
        display_name: "Mock Replay Plugin".to_string(),
        version: "0.1.0".to_string(),
        entry: PluginEntry {
            command: mock_plugin_bin().to_string_lossy().into_owned(),
            args,
            working_dir: None,
        },
        r#match: MatchRules {
            extensions: vec!["csv".to_string()],
            header_fingerprints: None,
        },
        min_protocol_version: 1,
        ..Default::default()
    };
    fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write plugin.json");
}

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

/// 声明能力后调用 echo 分支：回包 `{"data":{"echo":{file_id,query,params}}}` 逐字段解析。
#[tokio::test]
async fn custom_query_result_frame_parses_into_data_map() {
    let tmp = TempDir::new("cq-echo");
    install_mock_plugin(
        &tmp.path().join("mock"),
        &repo_script("happy_path.ndjson"),
        true,
    );
    let registry = Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let runtime = PluginRuntime::new(registry);

    let session = runtime
        .get_or_spawn("mock")
        .await
        .expect("spawn + handshake");
    assert_eq!(session.state(), PluginProcessState::Ready);

    let result = session
        .custom_query(CustomQueryParams {
            file_id: FILE_ID.into(),
            query: "echo".into(),
            params: serde_json::Map::from_iter([
                ("k".to_string(), serde_json::json!("v")),
                ("n".to_string(), serde_json::json!(1)),
            ]),
        })
        .await
        .expect("custom_query");

    // 插件 echo 分支：data.echo = 请求三字段原样回显（params 缺省形状不掺假）。
    let echo = result
        .data
        .get("echo")
        .expect("echo 分支必须在 data.echo 回显");
    assert_eq!(echo["file_id"], serde_json::json!(FILE_ID));
    assert_eq!(echo["query"], serde_json::json!("echo"));
    assert_eq!(echo["params"], serde_json::json!({"k": "v", "n": 1}));

    runtime.shutdown_all().await;
}

/// 未声明能力：插件回 -32005 error 帧 → 会话层 `HostError::Protocol` 原样透传
/// （code/message 逐字保留，data 无），且单次插件 error 不终止会话。
#[tokio::test]
async fn custom_query_plugin_error_passes_through_as_protocol() {
    let tmp = TempDir::new("cq-unsupported");
    install_mock_plugin(
        &tmp.path().join("mock"),
        &repo_script("happy_path.ndjson"),
        false,
    );
    let registry = Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let runtime = PluginRuntime::new(registry);

    let session = runtime
        .get_or_spawn("mock")
        .await
        .expect("spawn + handshake");

    let err = session
        .custom_query(CustomQueryParams {
            file_id: FILE_ID.into(),
            query: "echo".into(),
            params: serde_json::Map::new(),
        })
        .await
        .expect_err("未声明能力必须收到插件 error 帧");
    match &err {
        ab_host::HostError::Protocol {
            code,
            message,
            data,
        } => {
            assert_eq!(*code, -32005, "插件错误码原样透传: {err:?}");
            assert_eq!(message, "custom_query not supported");
            assert!(data.is_none(), "error 帧无 data: {err:?}");
        }
        other => panic!("expected HostError::Protocol passthrough, got {other:?}"),
    }
    // 会话层不做归一/特判：单次插件 error 不终止会话（annotate 先例同款语义）。
    assert_eq!(session.state(), PluginProcessState::Ready);

    runtime.shutdown_all().await;
}
