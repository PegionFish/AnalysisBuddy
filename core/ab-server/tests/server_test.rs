//! ab-server 集成测试：真实 axum 服务器（127.0.0.1:0 临时端口）+ 真实
//! mock-plugin 子进程，覆盖 HTTP 契约端到端。
//!
//! 布线与 `core/ab-engine/src/smoke.rs` 同款：临时插件目录手装 mock
//! 回放器（manifest 结构体序列化），`AssembleOptions.file_id_fn` 固定
//! file_id 对齐 happy_path 剧本内嵌值，fixture 用仓库级
//! `tests/fixtures/small_with_header.csv`。
//!
//! JSON 快照断言（import result / series slice / key-values）锁定与桌面
//! ipc-ui.md §1.0 完全一致的 serde 字段名（契约平价）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};
use serde_json::{json, Value};

use ab_server::routes::build_router;
use ab_server::state::{assemble, AssembleOptions};

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

static SERVER_SEQ: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-server-test-{}-{}-{tag}",
            std::process::id(),
            SERVER_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("mkdir tempdir");
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

/// mock-plugin 可执行文件（缺失时现场构建；smoke.rs 同款）。
fn mock_plugin_bin() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../target"));
    let bin = target_dir
        .join("debug")
        .join(if cfg!(windows) { "mock-plugin.exe" } else { "mock-plugin" });
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

fn fixture_csv() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/small_with_header.csv")
}

/// 手装 mock 插件（Manifest 结构体序列化 → 必过宿主 validate）。
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
        ..Default::default()
    };
    fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write plugin.json");
}

// ---------------------------------------------------------------------------
// 服务器夹具
// ---------------------------------------------------------------------------

struct TestServer {
    base: String,
    client: reqwest::Client,
    state: ab_server::AppState,
    // 声明在 state 之后：drop 顺序先停 host（含全部插件进程）再删目录。
    _tmp: TempDir,
}

async fn spawn_server(tag: &str, token: Option<&str>) -> TestServer {
    let tmp = TempDir::new(tag);
    // mock 必须装在 portable 源（tmp/plugins）之下才会被发现；装在外层
    // （如 tmp/mock）则 discovery 扫不到 → 0 候选 → Matched（手选分支）。
    install_mock_plugin(
        &tmp.path().join("plugins").join("mock"),
        &repo_script("happy_path.ndjson"),
    );
    let paths = ab_engine::paths::EnginePaths {
        plugins_portable: tmp.path().join("plugins"),
        plugins_install: tmp.path().join("plugins"),
        plugins_user: tmp.path().join("plugins-user"),
        presets_dir: tmp.path().join("presets"),
        sessions_dir: tmp.path().join("sessions"),
    };
    let state = assemble(
        paths,
        AssembleOptions {
            max_concurrent_imports: 2,
            token: token.map(str::to_string),
            file_id_fn: Some(Arc::new(|_| FILE_ID.to_string())),
        },
    )
    .expect("assemble");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral");
    let addr = listener.local_addr().expect("addr");
    let serve_state = state.clone();
    tokio::spawn(async move {
        axum::serve(listener, build_router(serve_state))
            .await
            .expect("serve");
    });
    TestServer {
        base: format!("http://{addr}/api/v1"),
        client: reqwest::Client::new(),
        state,
        _tmp: tmp,
    }
}

async fn poll_job(server: &TestServer, job_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status: Value = server
            .client
            .get(format!("{}/imports/{job_id}", server.base))
            .send()
            .await
            .expect("get job")
            .json()
            .await
            .expect("job json");
        if matches!(
            status["state"].as_str(),
            Some("completed") | Some("failed") | Some("cancelled")
        ) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "job {job_id} did not finish in time: {status}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn import_fixture(server: &TestServer) -> Value {
    let resp = server
        .client
        .post(format!("{}/imports", server.base))
        .json(&json!({"paths": [fixture_csv().to_string_lossy()]}))
        .send()
        .await
        .expect("post imports");
    assert_eq!(resp.status(), 202, "POST /imports must be 202");
    let job: Value = resp.json().await.expect("job json");
    poll_job(server, job["job_id"].as_str().expect("job_id")).await
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn health_returns_protocol_version_1() {
    let server = spawn_server("health", None).await;
    let resp = server
        .client
        .get(format!("{}/health", server.base))
        .send()
        .await
        .expect("health");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["protocol_version"], 1);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test(flavor = "multi_thread")]
async fn import_to_query_lifecycle() {
    let server = spawn_server("lifecycle", None).await;

    // 导入 → 轮询完成 → ImportResult 快照键集合（桌面契约平价）。
    let job = import_fixture(&server).await;
    assert_eq!(job["state"], "completed");
    let files = job["files"].as_array().expect("files");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["status"], "ready");
    assert_eq!(files[0]["file_id"], FILE_ID);
    assert_eq!(files[0]["name"], "small_with_header.csv");
    for key in ["file_id", "path", "name", "size_bytes", "status", "candidate_plugins"] {
        assert!(files[0].get(key).is_some(), "missing key `{key}` in {}", files[0]);
    }
    assert!(files[0].get("error").is_none(), "ready file must not carry error key");

    // metrics 树：file 节点 → plugin 节点 → metric 叶（复合 id）。
    let metrics: Value = server
        .client
        .get(format!("{}/metrics", server.base))
        .send()
        .await
        .expect("metrics")
        .json()
        .await
        .expect("metrics json");
    let arr = metrics.as_array().expect("metrics array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["level"], "file");
    assert_eq!(arr[0]["id"], FILE_ID);
    assert_eq!(arr[0]["children"][0]["plugin_id"], "mock");
    assert_eq!(
        arr[0]["children"][0]["children"][0]["id"],
        format!("{FILE_ID}:mock:fps")
    );

    // series：切片形状 + 快照键集合。
    let series: Value = server
        .client
        .post(format!("{}/query/series", server.base))
        .json(&json!({
            "metrics": [format!("{FILE_ID}:mock:fps")],
            "t0_ms": 0,
            "t1_ms": 2_000_000_000_000i64,
        }))
        .send()
        .await
        .expect("series")
        .json()
        .await
        .expect("series json");
    let slices = series.as_array().expect("slices");
    assert_eq!(slices.len(), 1);
    assert_eq!(slices[0]["file_id"], FILE_ID);
    assert_eq!(slices[0]["plugin_id"], "mock");
    assert_eq!(slices[0]["metric_id"], "fps");
    assert_eq!(slices[0]["point_count"], 1);
    assert_eq!(slices[0]["downsampled"], false);
    assert_eq!(slices[0]["points"][0], json!({"t_ms": 1785600000123i64, "v": 59.8}));
    for key in ["file_id", "plugin_id", "metric_id", "point_count", "downsampled", "points"] {
        assert!(slices[0].get(key).is_some(), "missing key `{key}` in {}", slices[0]);
    }

    // 卸载 → 204 → metrics 空。
    let resp = server
        .client
        .delete(format!("{}/files/{FILE_ID}", server.base))
        .send()
        .await
        .expect("unload");
    assert_eq!(resp.status(), 204);
    let metrics: Value = server
        .client
        .get(format!("{}/metrics", server.base))
        .send()
        .await
        .expect("metrics")
        .json()
        .await
        .expect("metrics json");
    assert_eq!(metrics.as_array().expect("array").len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn key_values_partial_failure_shape_preserved() {
    let server = spawn_server("keyvalues", None).await;
    let _ = import_fixture(&server).await;

    // 已知文件 + 幽灵文件：永不整体 reject，逐文件 entries/error。
    let resp = server
        .client
        .post(format!("{}/query/key-values", server.base))
        .json(&json!({"file_ids": [FILE_ID, "ghost-file"], "timestamp_ms": 1785603599870i64}))
        .send()
        .await
        .expect("key-values");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    let items = body.as_array().expect("items");
    assert_eq!(items.len(), 2);
    let ok = items
        .iter()
        .find(|item| item["file_id"] == FILE_ID)
        .expect("ok item");
    assert!(ok.get("error").is_none(), "ok item must not carry error key");
    assert_eq!(ok["entries"][0]["key"], "scene");
    assert_eq!(ok["entries"][0]["value"], "boss");
    let err = items
        .iter()
        .find(|item| item["file_id"] == "ghost-file")
        .expect("err item");
    assert!(err.get("entries").is_none(), "err item must not carry entries key");
    assert_eq!(err["error"]["code"], "file_not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_receives_progress_frames() {
    let server = spawn_server("sse", None).await;
    let resp = server
        .client
        .get(format!("{}/events", server.base))
        .send()
        .await
        .expect("sse connect");
    assert_eq!(resp.status(), 200);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/event-stream"),
        "unexpected content-type: {content_type}"
    );

    // 订阅建立后再触发导入（保证事件先于导入发生）。
    let client = server.client.clone();
    let url = format!("{}/imports", server.base);
    let body = json!({"paths": [fixture_csv().to_string_lossy()]});
    let spawner = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        client.post(url).json(&body).send().await.expect("post imports")
    });

    let mut resp = resp;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut buffer = String::new();
    let mut got_progress = false;
    while Instant::now() < deadline {
        match resp.chunk().await.expect("chunk") {
            Some(chunk) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
            None => break,
        }
        if buffer.contains("event: progress") && buffer.contains("data:") {
            got_progress = true;
            break;
        }
    }
    assert!(got_progress, "no progress frame received; got: {buffer}");
    let _ = spawner.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn presets_crud_roundtrip() {
    let server = spawn_server("presets", None).await;
    let list: Value = server
        .client
        .get(format!("{}/presets", server.base))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("list json");
    assert_eq!(list.as_array().expect("array").len(), 0);

    let body = json!({
        "name": {"zh": "Boss 战", "en": "Boss Fight"},
        "entries": {"mock": ["fps", "player_hp"]},
    });
    let resp = server
        .client
        .post(format!("{}/presets", server.base))
        .json(&body)
        .send()
        .await
        .expect("save");
    assert_eq!(resp.status(), 201);
    let preset: Value = resp.json().await.expect("preset json");
    // id = slugify_id(name.zh)（presets.rs prepare_save）：“Boss 战”→“boss”。
    assert_eq!(preset["id"], "boss");

    // 重名 → 409 preset_conflict 统一包络。
    let resp = server
        .client
        .post(format!("{}/presets", server.base))
        .json(&body)
        .send()
        .await
        .expect("save duplicate");
    assert_eq!(resp.status(), 409);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "preset_conflict");

    let list: Value = server
        .client
        .get(format!("{}/presets", server.base))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("list json");
    assert_eq!(list.as_array().expect("array").len(), 1);

    let resp = server
        .client
        .delete(format!("{}/presets/boss", server.base))
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status(), 204);
    let list: Value = server
        .client
        .get(format!("{}/presets", server.base))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("list json");
    assert_eq!(list.as_array().expect("array").len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_token_gates_all_but_health() {
    let server = spawn_server("auth", Some("s3cret")).await;

    // health 豁免（探活）。
    let resp = server
        .client
        .get(format!("{}/health", server.base))
        .send()
        .await
        .expect("health");
    assert_eq!(resp.status(), 200);

    // 无 token → 401 统一包络。
    let resp = server
        .client
        .get(format!("{}/plugins", server.base))
        .send()
        .await
        .expect("plugins");
    assert_eq!(resp.status(), 401);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "unauthorized");

    // 错 token → 401；对 token → 200。
    let resp = server
        .client
        .get(format!("{}/plugins", server.base))
        .header("Authorization", "Bearer wrong")
        .send()
        .await
        .expect("plugins");
    assert_eq!(resp.status(), 401);
    let resp = server
        .client
        .get(format!("{}/plugins", server.base))
        .header("Authorization", "Bearer s3cret")
        .send()
        .await
        .expect("plugins");
    assert_eq!(resp.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_import_matches_and_completes() {
    let server = spawn_server("upload", None).await;
    let part = reqwest::multipart::Part::bytes(b"timestamp,fps\n1785600000123,59.8\n".to_vec())
        .file_name("tiny.csv");
    let form = reqwest::multipart::Form::new().part("file", part);
    let resp = server
        .client
        .post(format!("{}/imports/upload", server.base))
        .multipart(form)
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), 202);
    let job: Value = resp.json().await.expect("job json");
    let done = poll_job(&server, job["job_id"].as_str().expect("job_id")).await;
    assert_eq!(done["state"], "completed");
    assert_eq!(done["files"][0]["status"], "ready");
    assert_eq!(done["files"][0]["name"], "tiny.csv");
    assert_eq!(done["files"][0]["file_id"], FILE_ID);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_plugins_shows_mock() {
    let server = spawn_server("plugins", None).await;
    let plugins: Value = server
        .client
        .get(format!("{}/plugins", server.base))
        .send()
        .await
        .expect("plugins")
        .json()
        .await
        .expect("plugins json");
    let arr = plugins.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "mock");
    assert_eq!(arr[0]["state"], "discovered");
    assert_eq!(arr[0]["source"], "portable");
    assert_eq!(arr[0]["builtin"], false);
    assert_eq!(arr[0]["disabled"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_series_rejects_points_above_cap() {
    let server = spawn_server("cap", None).await;
    let resp = server
        .client
        .post(format!("{}/query/series", server.base))
        .json(&json!({
            "metrics": ["x:mock:fps"],
            "t0_ms": 0,
            "t1_ms": 1,
            "max_points_per_series": 50_001,
        }))
        .send()
        .await
        .expect("series");
    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "invalid_arg");
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_save_rejects_paths_outside_sessions_dir() {
    let server = spawn_server("sessions", None).await;
    let _ = import_fixture(&server).await;

    // 越界绝对路径 → 400 invalid_arg。
    let outside = std::env::temp_dir().join("ab-server-outside.absession");
    let resp = server
        .client
        .post(format!("{}/sessions/save", server.base))
        .json(&json!({"path": outside.to_string_lossy()}))
        .send()
        .await
        .expect("save outside");
    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "invalid_arg");

    // 省略 path → 自动命名落 sessions_dir；meta 回显 file_count=1。
    let resp = server
        .client
        .post(format!("{}/sessions/save", server.base))
        .json(&json!({}))
        .send()
        .await
        .expect("save auto");
    assert_eq!(resp.status(), 200);
    let meta: Value = resp.json().await.expect("meta json");
    assert_eq!(meta["file_count"], 1);
    let saved_path = meta["path"].as_str().expect("path").to_string();
    let sessions_prefix = server.state.paths.sessions_dir.to_string_lossy().into_owned();
    assert!(
        saved_path.starts_with(&sessions_prefix),
        "saved path {saved_path} escapes {sessions_prefix}"
    );

    // load 回读（桌面语义：load 前文件不在场——load_session 对会话内全部
    // 文件重走 reopen 管线，对已注册文件会 reopen_failed）：先卸载，
    // loaded_file_ids 含固定 FILE_ID。
    let resp = server
        .client
        .delete(format!("{}/files/{FILE_ID}", server.base))
        .send()
        .await
        .expect("unload before load");
    assert_eq!(resp.status(), 204);
    let resp = server
        .client
        .post(format!("{}/sessions/load", server.base))
        .json(&json!({"path": saved_path}))
        .send()
        .await
        .expect("load");
    assert_eq!(resp.status(), 200);
    let loaded: Value = resp.json().await.expect("load json");
    assert_eq!(loaded["loaded_file_ids"][0], FILE_ID);
}
