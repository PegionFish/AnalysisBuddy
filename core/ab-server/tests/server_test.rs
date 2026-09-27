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

fn fixture_csv() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/small_with_header.csv")
}

/// 带附加 CLI 参数的安装（如 `--caps custom_query`，§2.11 集成测试用）。
fn install_mock_plugin_with_args(dir: &Path, script: &Path, extra_args: &[&str]) {
    fs::create_dir_all(dir).expect("mkdir plugin dir");
    let manifest = Manifest {
        id: "mock".to_string(),
        display_name: "Mock Replay Plugin".to_string(),
        version: "0.1.0".to_string(),
        entry: PluginEntry {
            command: mock_plugin_bin().to_string_lossy().into_owned(),
            args: [
                vec![
                    "--script".to_string(),
                    script.to_string_lossy().into_owned(),
                ],
                extra_args.iter().map(|s| s.to_string()).collect(),
            ]
            .concat(),
            working_dir: None,
            platforms: Default::default(),
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
    spawn_server_with_plugin_args(tag, token, &[]).await
}

/// 附加 mock 插件 CLI 参数的 spawn 变体（`--caps custom_query` 等）。
async fn spawn_server_with_plugin_args(
    tag: &str,
    token: Option<&str>,
    extra_args: &[&str],
) -> TestServer {
    spawn_server_full(tag, token, extra_args, None).await
}

/// 全参数 spawn 变体（Quest M4.2：注入引擎内存预算；`None` = 不设限）。
async fn spawn_server_full(
    tag: &str,
    token: Option<&str>,
    extra_args: &[&str],
    memory_budget_bytes: Option<u64>,
) -> TestServer {
    spawn_server_opts(SpawnOpts {
        tag: Some(tag),
        token,
        extra_args: Some(extra_args),
        memory_budget_bytes,
        ..Default::default()
    })
    .await
}

/// spawn 选项（默认值 = 现行 spawn_server 行为）。
#[derive(Default)]
struct SpawnOpts<'a> {
    tag: Option<&'a str>,
    token: Option<&'a str>,
    extra_args: Option<&'a [&'a str]>,
    /// Quest M4.2：引擎内存预算（None = 不设限）。
    memory_budget_bytes: Option<u64>,
    /// WS-B1：`--import-roots` 白名单（None = 不限制，桌面形态）。
    import_roots: Option<Vec<PathBuf>>,
    /// 显式 sessions_dir（sessions/load 白名单用例需要预先知道目录）。
    sessions_dir: Option<PathBuf>,
    /// mock 插件剧本名（默认 happy_path.ndjson；失败路径用 load_failed.ndjson）。
    script: Option<&'a str>,
    /// B3：并发已加载文件数上限（None = 不设限）。
    max_loaded_files: Option<usize>,
    /// B3：累计上传配额 MB（None = 不设限）。
    upload_quota_mb: Option<u64>,
}

/// 全参数 spawn 变体（WS-B1：注入 `--import-roots` 白名单与显式
/// sessions_dir；缺省 = 现行行为）。
async fn spawn_server_opts(opts: SpawnOpts<'_>) -> TestServer {
    let tag = opts.tag.unwrap_or("srv");
    let extra_args = opts.extra_args.unwrap_or(&[]);
    let tmp = TempDir::new(tag);
    // mock 必须装在 portable 源（tmp/plugins）之下才会被发现；装在外层
    // （如 tmp/mock）则 discovery 扫不到 → 0 候选 → Matched（手选分支）。
    install_mock_plugin_with_args(
        &tmp.path().join("plugins").join("mock"),
        &repo_script(opts.script.unwrap_or("happy_path.ndjson")),
        extra_args,
    );
    let paths = ab_engine::paths::EnginePaths {
        plugins_portable: tmp.path().join("plugins"),
        plugins_install: tmp.path().join("plugins"),
        plugins_user: tmp.path().join("plugins-user"),
        presets_dir: tmp.path().join("presets"),
        sessions_dir: opts
            .sessions_dir
            .unwrap_or_else(|| tmp.path().join("sessions")),
    };
    let state = assemble(
        paths,
        AssembleOptions {
            max_concurrent_imports: 2,
            token: opts.token.map(str::to_string),
            file_id_fn: Some(Arc::new(|_| FILE_ID.to_string())),
            memory_budget_bytes: opts.memory_budget_bytes,
            import_roots: opts.import_roots,
            max_loaded_files: opts.max_loaded_files,
            upload_quota_bytes: opts.upload_quota_mb.map(|mb| mb * 1024 * 1024),
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
    for key in [
        "file_id",
        "path",
        "name",
        "size_bytes",
        "status",
        "candidate_plugins",
    ] {
        assert!(
            files[0].get(key).is_some(),
            "missing key `{key}` in {}",
            files[0]
        );
    }
    assert!(
        files[0].get("error").is_none(),
        "ready file must not carry error key"
    );

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
    assert_eq!(
        slices[0]["points"][0],
        json!({"t_ms": 1785600000123i64, "v": 59.8})
    );
    for key in [
        "file_id",
        "plugin_id",
        "metric_id",
        "point_count",
        "downsampled",
        "points",
    ] {
        assert!(
            slices[0].get(key).is_some(),
            "missing key `{key}` in {}",
            slices[0]
        );
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
    assert!(
        ok.get("error").is_none(),
        "ok item must not carry error key"
    );
    assert_eq!(ok["entries"][0]["key"], "scene");
    assert_eq!(ok["entries"][0]["value"], "boss");
    let err = items
        .iter()
        .find(|item| item["file_id"] == "ghost-file")
        .expect("err item");
    assert!(
        err.get("entries").is_none(),
        "err item must not carry entries key"
    );
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
        client
            .post(url)
            .json(&body)
            .send()
            .await
            .expect("post imports")
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
async fn memory_budget_exceeded_rejects_import_per_file() {
    // Quest M4.2：引擎内存预算硬顶（--memory-budget-mb → PipelineConfig）。
    // 预算 1 字节：任何记录入库即超限 → 该文件 outcome error
    // （code=memory_budget_exceeded）；逐文件语义与桌面一致——job 终态
    // completed（不整体 failed），数据已卸载（get_metrics 为空）。
    let server = spawn_server_full("budget", None, &[], Some(1)).await;
    let job = import_fixture(&server).await;
    assert_eq!(
        job["state"], "completed",
        "per-file error 不整体 failed: {job}"
    );
    let files = job["files"].as_array().expect("files");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["status"], "error");
    assert_eq!(files[0]["error"]["code"], "memory_budget_exceeded");

    // 数据已卸载：metrics 树为空（get_metrics 不含超限文件）。
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
    let sessions_prefix = server
        .state
        .paths
        .sessions_dir
        .to_string_lossy()
        .into_owned();
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

#[tokio::test(flavor = "multi_thread")]
async fn vendor_custom_query_capable_plugin_end_to_end() {
    // mock 带 --caps custom_query：echo 回显 / 未知名 -32602→invalid_params
    // 422 / 清单端点占位空集 / 未知文件 404（§2.24–§2.25，CCP-custom-query）。
    let server =
        spawn_server_with_plugin_args("srv-vendor-query", None, &["--caps", "custom_query"]).await;
    import_fixture(&server).await;

    // ① echo 具名查询：params 原样回显，data 为 object。
    let resp = server
        .client
        .post(format!("{}/files/{FILE_ID}/queries/echo", server.base))
        .json(&json!({"params": {"k": "v"}}))
        .send()
        .await
        .expect("post vendor query");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("query json");
    assert_eq!(
        body,
        json!({"data": {"echo": {"file_id": FILE_ID, "query": "echo", "params": {"k": "v"}}}}),
        "echo 回显形状（opaque 载荷原样透传）"
    );

    // ② body 省略 = 无参（params 缺省 {}）。
    let resp = server
        .client
        .post(format!("{}/files/{FILE_ID}/queries/echo", server.base))
        .send()
        .await
        .expect("post no-body vendor query");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("query json");
    assert_eq!(body["data"]["echo"]["params"], json!({}));

    // ③ 未知 query 名 → -32602 归一 invalid_params → 422。
    let resp = server
        .client
        .post(format!("{}/files/{FILE_ID}/queries/__nope__", server.base))
        .json(&json!({}))
        .send()
        .await
        .expect("post unknown query");
    assert_eq!(resp.status(), 422);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "invalid_params");

    // ④ 请求体非法 JSON → 400 invalid_arg。
    let resp = server
        .client
        .post(format!("{}/files/{FILE_ID}/queries/echo", server.base))
        .header("content-type", "application/json")
        .body("not-json")
        .send()
        .await
        .expect("post bad body");
    assert_eq!(resp.status(), 400);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "invalid_arg");

    // ⑤ 具名查询清单：Phase 2 无发现方法 → 恒空集占位。
    let resp = server
        .client
        .get(format!("{}/files/{FILE_ID}/vendor-queries", server.base))
        .send()
        .await
        .expect("get vendor queries");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("list json");
    assert_eq!(body, json!({"queries": []}));

    // ⑥ 未知 file_id → 404 file_not_found。
    let resp = server
        .client
        .get(format!("{}/files/ghost/vendor-queries", server.base))
        .send()
        .await
        .expect("get unknown file");
    assert_eq!(resp.status(), 404);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "file_not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn vendor_custom_query_without_capability_maps_unsupported() {
    // 无 --caps：插件未声明能力仍被调用 → -32005 → unsupported → 422
    // （§2.11 归一；不得落 internal/500）。
    let server = spawn_server("srv-vendor-query-nocap", None).await;
    import_fixture(&server).await;
    let resp = server
        .client
        .post(format!("{}/files/{FILE_ID}/queries/echo", server.base))
        .json(&json!({}))
        .send()
        .await
        .expect("post vendor query");
    assert_eq!(resp.status(), 422);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "unsupported");
}

// ---------------------------------------------------------------------------
// WS-B1：--import-roots 导入路径白名单（P0-2 服务端纵深防御）
// ---------------------------------------------------------------------------

/// 递归收集 root 下全部文件路径（上传残留断言用）。
fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn import_roots_gate_rejects_outside_and_allows_inside() {
    let tmp = TempDir::new("roots-gate");
    let uploads = tmp.path().join("uploads");
    fs::create_dir_all(&uploads).expect("mkdir uploads");
    fs::copy(fixture_csv(), uploads.join("small_with_header.csv")).expect("copy fixture");
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("roots-gate"),
        import_roots: Some(vec![uploads.clone()]),
        ..Default::default()
    })
    .await;

    // root 内 → 202 + 正常完成。
    let inside = uploads.join("small_with_header.csv");
    let resp = server
        .client
        .post(format!("{}/imports", server.base))
        .json(&json!({"paths": [inside.to_string_lossy()]}))
        .send()
        .await
        .expect("post inside");
    assert_eq!(resp.status(), 202, "root 内路径必须放行");
    let job: Value = resp.json().await.expect("job json");
    let done = poll_job(&server, job["job_id"].as_str().expect("job_id")).await;
    assert_eq!(done["state"], "completed");
    assert_eq!(done["files"][0]["status"], "ready");

    // root 外 → 403 path_forbidden（统一错误包络）。
    let resp = server
        .client
        .post(format!("{}/imports", server.base))
        .json(&json!({"paths": ["/etc/passwd"]}))
        .send()
        .await
        .expect("post outside");
    assert_eq!(resp.status(), 403);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "path_forbidden");

    // `..` 穿越（词法规范化后落 root 外）→ 403。
    let traversal = uploads.join("../../../../etc/passwd");
    let resp = server
        .client
        .post(format!("{}/imports", server.base))
        .json(&json!({"paths": [traversal.to_string_lossy()]}))
        .send()
        .await
        .expect("post traversal");
    assert_eq!(resp.status(), 403);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "path_forbidden");

    // 混合批次：一个 root 内 + 一个 root 外 → 整批 fail fast 403。
    let resp = server
        .client
        .post(format!("{}/imports", server.base))
        .json(&json!({"paths": [inside.to_string_lossy(), "/etc/hosts"]}))
        .send()
        .await
        .expect("post mixed");
    assert_eq!(resp.status(), 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn import_roots_absent_keeps_desktop_behavior() {
    // 不传 --import-roots（桌面形态）：任意本地路径能力完整保留。
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("roots-desktop"),
        ..Default::default()
    })
    .await;
    let dir = TempDir::new("roots-desktop-any");
    let anywhere = dir.path().join("loose.csv");
    fs::create_dir_all(dir.path()).expect("mkdir loose parent");
    fs::write(&anywhere, "timestamp,fps\n1,60.0\n").expect("write loose");
    let resp = server
        .client
        .post(format!("{}/imports", server.base))
        .json(&json!({"paths": [anywhere.to_string_lossy()]}))
        .send()
        .await
        .expect("post loose path");
    assert_eq!(resp.status(), 202, "无 roots 时不得拒绝任何路径");
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_with_matching_roots_succeeds_and_mismatch_rejects_without_residue() {
    // 服务形态契约：网关只传本实例上传根 → 上传副本路径天然在 roots 内。
    let upload_root = std::env::temp_dir().join("ab-server-uploads");
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("roots-upload-ok"),
        import_roots: Some(vec![upload_root.clone()]),
        ..Default::default()
    })
    .await;
    let part = reqwest::multipart::Part::bytes(b"timestamp,fps\n1785600000123,59.8\n".to_vec())
        .file_name("tiny.csv");
    let resp = server
        .client
        .post(format!("{}/imports/upload", server.base))
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), 202, "上传根在 roots 内必须放行");
    let job: Value = resp.json().await.expect("job json");
    let done = poll_job(&server, job["job_id"].as_str().expect("job_id")).await;
    assert_eq!(done["state"], "completed");

    // roots 指向他处 → 上传副本路径不在 roots 内 → 403，且被拒绝的副本
    // 立即删除（无新增残留文件）。
    let before = files_under(&upload_root);
    let elsewhere_keep = TempDir::new("roots-upload-no");
    let elsewhere = elsewhere_keep.path().join("elsewhere");
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("roots-upload-no"),
        import_roots: Some(vec![elsewhere]),
        ..Default::default()
    })
    .await;
    let part =
        reqwest::multipart::Part::bytes(b"timestamp,fps\n1,1\n".to_vec()).file_name("rejected.csv");
    let resp = server
        .client
        .post(format!("{}/imports/upload", server.base))
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .expect("upload mismatch");
    assert_eq!(resp.status(), 403);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "path_forbidden");
    let after = files_under(&upload_root);
    let leaked: Vec<_> = after
        .iter()
        .filter(|p| p.ends_with("rejected.csv") && !before.iter().any(|b| b == *p))
        .collect();
    assert!(
        leaked.is_empty(),
        "被拒绝的上传副本必须当场删除: {leaked:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_copy_removed_on_terminal_state_completed_and_failed() {
    // WS-B2（P0-4）：上传副本在 job 终态即删——completed 与 failed 两路都
    // 不残留（清理契约：不等会话终结）。
    let upload_root = std::env::temp_dir().join("ab-server-uploads");

    // ① completed：正常小 CSV 导入成功后副本消失。
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("b2-ok"),
        ..Default::default()
    })
    .await;
    let uniq = format!("b2-ok-{}.csv", std::process::id());
    let part = reqwest::multipart::Part::bytes(b"timestamp,fps\n1785600000123,59.8\n".to_vec())
        .file_name(uniq.clone());
    let resp = server
        .client
        .post(format!("{}/imports/upload", server.base))
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .expect("upload ok");
    assert_eq!(resp.status(), 202);
    let job: Value = resp.json().await.expect("job json");
    let done = poll_job(&server, job["job_id"].as_str().expect("job_id")).await;
    assert_eq!(done["state"], "completed", "前置：导入本身成功");
    assert!(
        !files_under(&upload_root).iter().any(|p| p.ends_with(&uniq)),
        "completed 后上传副本必须删除"
    );

    // ② failed：load_failed 剧本使 load 阶段失败，副本同样删除。
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("b2-fail"),
        script: Some("load_failed.ndjson"),
        ..Default::default()
    })
    .await;
    let uniq2 = format!("b2-fail-{}.csv", std::process::id());
    let part =
        reqwest::multipart::Part::bytes(b"timestamp,fps\n1,1\n".to_vec()).file_name(uniq2.clone());
    let resp = server
        .client
        .post(format!("{}/imports/upload", server.base))
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .expect("upload fail");
    assert_eq!(resp.status(), 202);
    let job: Value = resp.json().await.expect("job json");
    let done = poll_job(&server, job["job_id"].as_str().expect("job_id")).await;
    // load 失败在批语义下是 per-file error（job 可为 completed 但文件带错）
    let file_failed = done["files"]
        .as_array()
        .map(|fs| {
            fs.iter()
                .any(|f| f["status"] == "error" || f.get("error").is_some_and(|e| !e.is_null()))
        })
        .unwrap_or(false)
        || done["state"] == "failed";
    assert!(file_failed, "前置：导入确已失败: {done}");
    assert!(
        !files_under(&upload_root)
            .iter()
            .any(|p| p.ends_with(&uniq2)),
        "失败终态后上传副本必须删除"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_load_paths_gated_by_import_roots() {
    // 形态①：会话文件本身在 roots 内，但快照内文件路径在 roots 外
    // （任意读向量的真正入口）→ 403 path_forbidden。
    let sessions_tmp = TempDir::new("roots-sessions");
    let sessions_dir = sessions_tmp.path().join("sessions");
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("roots-sessions"),
        import_roots: Some(vec![sessions_dir.clone()]),
        sessions_dir: Some(sessions_dir.clone()),
        ..Default::default()
    })
    .await;
    let session_path = sessions_dir.join("crafted.absession");
    fs::write(
        &session_path,
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "chart_view_state": {"y_axis_scale": "shared"},
            "files": [
                {"path": "/etc/passwd", "sha256": "00", "plugin_id": "mock"}
            ]
        }))
        .expect("serialize session"),
    )
    .expect("write crafted session");
    let resp = server
        .client
        .post(format!("{}/sessions/load", server.base))
        .json(&json!({"path": session_path.to_string_lossy()}))
        .send()
        .await
        .expect("load crafted");
    assert_eq!(resp.status(), 403, "快照内 root 外路径必须拒绝");
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "path_forbidden");

    // 快照内路径在 roots 内（即使文件缺失）→ 过白名单，交给既有
    // missing 语义（load 200，reason not_found）。
    let session_path = sessions_dir.join("inside.absession");
    fs::write(
        &session_path,
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "chart_view_state": {"y_axis_scale": "shared"},
            "files": [
                {"path": sessions_dir.join("innocent.csv").to_string_lossy(), "sha256": "00", "plugin_id": "mock"}
            ]
        }))
        .expect("serialize session"),
    )
    .expect("write inside session");
    let resp = server
        .client
        .post(format!("{}/sessions/load", server.base))
        .json(&json!({"path": session_path.to_string_lossy()}))
        .send()
        .await
        .expect("load inside");
    assert_eq!(resp.status(), 200, "roots 内路径不得误拒");
    let loaded: Value = resp.json().await.expect("load json");
    assert_eq!(loaded["missing"][0]["reason"], "not_found");
    drop(server);

    // 形态②：会话文件本身在 roots 外（roots 只含上传根；服务形态下
    // sessions_dir ≠ 上传根）→ 403（目标模型：服务形态无服务端会话）。
    let upload_root = std::env::temp_dir().join("ab-server-uploads");
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("roots-sessions-file"),
        import_roots: Some(vec![upload_root]),
        ..Default::default()
    })
    .await;
    let session_path = server.state.paths.sessions_dir.join("any.absession");
    fs::write(
        &session_path,
        serde_json::to_string(&serde_json::json!({"version": 1})).expect("serialize session"),
    )
    .expect("write session");
    let resp = server
        .client
        .post(format!("{}/sessions/load", server.base))
        .json(&json!({"path": session_path.to_string_lossy()}))
        .send()
        .await
        .expect("load outside-file");
    assert_eq!(resp.status(), 403);
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "path_forbidden");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_files_reports_uploaded_entry_ready_and_upload_source() {
    // B3（契约 §2.26）：空清单 → 上传导入后清单含条目（ready/upload/名称/大小）。
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("b3-list"),
        ..Default::default()
    })
    .await;
    let empty: Value = server
        .client
        .get(format!("{}/files", server.base))
        .send()
        .await
        .expect("list empty")
        .json()
        .await
        .expect("json");
    assert_eq!(empty["files"].as_array().map(Vec::len), Some(0));

    let uniq = format!("b3-entry-{}.csv", std::process::id());
    let body = "timestamp,fps\n1785600000123,59.8\n".to_string();
    let part = reqwest::multipart::Part::bytes(body.clone().into_bytes()).file_name(uniq.clone());
    let resp = server
        .client
        .post(format!("{}/imports/upload", server.base))
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), 202);
    let job: Value = resp.json().await.expect("job json");
    let done = poll_job(&server, job["job_id"].as_str().expect("job_id")).await;
    assert_eq!(done["state"], "completed");

    let listed: Value = server
        .client
        .get(format!("{}/files", server.base))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    let files = listed["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1);
    let entry = &files[0];
    assert_eq!(entry["name"], uniq.as_str());
    assert_eq!(entry["status"], "ready");
    assert_eq!(entry["source"], "upload");
    assert_eq!(entry["size_bytes"], body.len() as u64);
    assert!(
        entry["file_id"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "file_id 必须非空"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn max_loaded_files_quota_returns_429_and_unload_frees_slot() {
    // B3（契约 §9.4）：--max-loaded-files 1 —— 第二个导入预检 429；
    // 卸载后名额释放，可再次导入。引擎侧纵深校验由 outcome error 覆盖。
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("b3-limit"),
        max_loaded_files: Some(1),
        ..Default::default()
    })
    .await;

    let upload = |name: &str| {
        let server = &server;
        let name = name.to_string();
        async move {
            let part =
                reqwest::multipart::Part::bytes(b"timestamp,fps\n1785600000123,59.8\n".to_vec())
                    .file_name(name);
            server
                .client
                .post(format!("{}/imports/upload", server.base))
                .multipart(reqwest::multipart::Form::new().part("file", part))
                .send()
                .await
                .expect("upload")
        }
    };

    let resp = upload("b3-first.csv").await;
    assert_eq!(resp.status(), 202);
    let job: Value = resp.json().await.expect("job json");
    let done = poll_job(&server, job["job_id"].as_str().expect("job_id")).await;
    assert_eq!(done["state"], "completed");

    // 第二个上传：已加载 1 == 上限 1 → 预检 429 file_limit_reached
    let resp = upload("b3-second.csv").await;
    assert_eq!(resp.status(), 429, "超配额上传必须 429");
    let err: Value = resp.json().await.expect("err json");
    assert_eq!(err["error"]["code"], "file_limit_reached");

    // 卸载后名额释放
    let file_id = done["files"][0]["file_id"]
        .as_str()
        .expect("file_id")
        .to_string();
    let resp = server
        .client
        .delete(format!("{}/files/{}", server.base, file_id))
        .send()
        .await
        .expect("unload");
    assert_eq!(resp.status(), 204);
    let resp = upload("b3-third.csv").await;
    assert_eq!(resp.status(), 202, "卸载后应可再次导入");
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_quota_exceeded_returns_429() {
    // B3（契约 §9.4）：--upload-quota-mb 1 —— 累计超 1MB 后上传 429。
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("b3-quota"),
        upload_quota_mb: Some(1),
        ..Default::default()
    })
    .await;
    let big = vec![b'x'; 600 * 1024];
    for i in 0..2 {
        let part = reqwest::multipart::Part::bytes(big.clone()).file_name(format!("b3q-{i}.csv"));
        let resp = server
            .client
            .post(format!("{}/imports/upload", server.base))
            .multipart(reqwest::multipart::Form::new().part("file", part))
            .send()
            .await
            .expect("upload");
        if i == 0 {
            assert!(
                resp.status() == 202 || resp.status() == 200,
                "首个 600KB 上传应放行（导入可能因非 CSV 内容 error，不影响配额断言）: {}",
                resp.status()
            );
        } else {
            assert_eq!(resp.status(), 429, "累计超 1MB 后必须 429");
            let err: Value = resp.json().await.expect("err json");
            assert_eq!(err["error"]["code"], "upload_quota_exceeded");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_terminal_job_returns_snapshot_not_404() {
    // C9（卷三 A1#3，契约 §2）：终态 job 取消 → 200 终态快照（此前 404）；
    // 未知 job → 404 不变。
    let server = spawn_server_opts(SpawnOpts {
        tag: Some("c9-terminal"),
        ..Default::default()
    })
    .await;
    let part = reqwest::multipart::Part::bytes(b"timestamp,fps\n1785600000123,59.8\n".to_vec())
        .file_name("c9.csv");
    let resp = server
        .client
        .post(format!("{}/imports/upload", server.base))
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .expect("upload");
    let job: Value = resp.json().await.expect("job json");
    let job_id = job["job_id"].as_str().expect("job_id").to_string();
    let done = poll_job(&server, &job_id).await;
    assert_eq!(done["state"], "completed", "前置：任务已完成（终态）");

    let resp = server
        .client
        .delete(format!("{}/imports/{}", server.base, job_id))
        .send()
        .await
        .expect("cancel terminal");
    assert_eq!(resp.status(), 200, "终态取消必须 200 终态快照");
    let snap: Value = resp.json().await.expect("snapshot json");
    assert_eq!(snap["state"], "completed");

    let resp = server
        .client
        .delete(format!("{}/imports/{}", server.base, "job-99999"))
        .send()
        .await
        .expect("cancel unknown");
    assert_eq!(resp.status(), 404, "未知 job 仍 404");
}
