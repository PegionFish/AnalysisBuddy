//! 嵌入示例（engine as a library）：在本进程内装配一套 headless 引擎——
//! 临时数据目录 + mock 回放插件 → 经命令层导入仓库小 fixture → 打印指标
//! 树概要 → 查询 series 并打印点数 → 优雅停机。布线与 `src/smoke.rs` 的
//! `smoke_pipeline_flow` 同款，但导入/查询走 `*_logic` 命令纯函数
//! （embedding 的推荐入口，与桌面 Tauri 命令一一对应）。
//!
//! 运行：`cargo run -p ab-engine --example engine_embed`
//! 前置：仓库内 `tools/mock-plugin`（缺二进制时现场构建）与
//! `tests/fixtures/small_with_header.csv`。无平台特定代码。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ab_host::{PluginRegistry, PluginRuntime, RuntimeConfig};
use ab_pipeline::{SessionRegistry, Store};
use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};
use tokio::sync::mpsc;

use ab_engine::commands::import::import_files_logic;
use ab_engine::commands::query::{get_metrics_logic, query_series_logic};
use ab_engine::paths::EnginePaths;
use ab_engine::pipeline_bridge::{ImportCoordinator, PipelineConfig};

/// 固定 file_id 对齐 happy_path 剧本内嵌值（`PipelineConfig.file_id_fn`
/// 强制分配；真实 embedding 交给引擎自增分配即可）。
const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("engine_embed: FAILED: {e}");
        std::process::exit(1);
    }
}

/// 主流程；退出前尽力清理临时目录（成功/失败路径均覆盖）。
async fn run() -> Result<(), String> {
    let root = temp_root();
    let result = embed_flow(&root).await;
    let _ = fs::remove_dir_all(&root);
    result
}

async fn embed_flow(root: &Path) -> Result<(), String> {
    let script = workspace_file("../../tools/mock-plugin/scripts/happy_path.ndjson");
    let fixture = workspace_file("../../tests/fixtures/small_with_header.csv");
    if !fixture.is_file() {
        return Err(format!("fixture missing: {}", fixture.display()));
    }
    // mock 插件必须装在便携源（<root>/plugins）之下才会被发现。
    install_mock_plugin(&root.join("plugins").join("mock"), &script);

    let paths = EnginePaths {
        plugins_portable: root.join("plugins"),
        plugins_install: root.join("plugins"),
        plugins_user: root.join("plugins-user"),
        presets_dir: root.join("presets"),
        sessions_dir: root.join("sessions"),
    };

    // 装配四件套（与桌面 ab-app 启动等价，见 core/ab-server/src/state.rs）：
    // 插件发现 → 宿主运行时 → 管线事件通道 → 导入编排器。
    let discovery = Arc::new(PluginRegistry::with_sources(
        paths.plugins_portable,
        paths.plugins_install,
        paths.plugins_user,
    ));
    discovery.discover();
    if discovery.list().iter().all(|p| p.manifest.id != "mock") {
        return Err("mock plugin not discovered under plugins/".to_string());
    }
    let host = Arc::new(PluginRuntime::with_config(
        discovery.clone(),
        RuntimeConfig::default(),
    ));
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let config = PipelineConfig {
        file_id_fn: Some(Arc::new(|_| FILE_ID.to_string())),
        ..PipelineConfig::default()
    };
    let coordinator = ImportCoordinator::with_config(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
        events_tx,
        host.clone(),
        discovery,
        config,
    );

    // ① 导入（命令层 `import_files`：与入参同序逐项 outcome，单路径失败
    //    不影响其余，全部路径为空串才整体 reject）。
    let files = import_files_logic(
        &coordinator,
        vec![fixture.to_string_lossy().into_owned()],
        None,
    )
    .await
    .map_err(|e| format!("import_files: {e:?}"))?;
    let first = files.first().ok_or("import returned no outcome")?;
    if first.status != "ready" {
        return Err(format!(
            "import status = {} (expected ready); error {:?}",
            first.status, first.error
        ));
    }
    println!(
        "engine_embed: import OK (file_id {}, matched {:?})",
        first.file_id, first.matched_plugin
    );

    // ② 指标树（GET /api/v1/metrics 同源逻辑）。
    let metrics = get_metrics_logic(&coordinator, None);
    println!(
        "engine_embed: metrics tree has {} root node(s)",
        metrics.len()
    );

    // ③ 查询 series（metric id 形如 `<file_id>:<plugin_id>:<metric>`；
    //    file_ids 是权威白名单，这里显式传入）。
    let slices = query_series_logic(
        &coordinator,
        &[FILE_ID.to_string()],
        &[format!("{FILE_ID}:mock:fps")],
        0,
        2_000_000_000_000,
        4000,
    )
    .map_err(|e| format!("query_series: {e:?}"))?;
    let points: usize = slices.iter().map(|s| s.point_count).sum();
    println!(
        "engine_embed: query OK ({points} points across {} slice(s))",
        slices.len()
    );

    // ④ 优雅停机：关停全部插件进程（stdout/stderr 汇聚结束，无孤儿）。
    host.shutdown_all().await;
    println!("engine_embed: shutdown OK");
    Ok(())
}

// ---------------------------------------------------------------------------
// 夹具（与 src/smoke.rs 同款：临时目录 / mock 二进制定位 / 手装 manifest）
// ---------------------------------------------------------------------------

fn temp_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("ab-engine-example-{}-{nanos}", std::process::id()))
}

fn workspace_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
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
