//! 集成测试共享工具：mock-plugin 安装/回放运行时、剧本行构造（UTF-8 安全）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ab_host::{PluginRegistry, PluginRuntime, RuntimeConfig};
use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};

pub static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-app-p3-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&base).expect("create tempdir");
        Self(base)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// mock-plugin 可执行文件（缺失时现场构建）。
pub fn mock_plugin_bin() -> PathBuf {
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

/// 仓库剧本（tools/mock-plugin/scripts）。
pub fn repo_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/mock-plugin/scripts")
        .join(name)
}

/// 安装单插件（扩展名 csv，预筛命中测试用）。
pub fn install_plugin(dir: &Path, plugin_id: &str, script: &Path) {
    fs::create_dir_all(dir).expect("mkdir plugin dir");
    let manifest = Manifest {
        id: plugin_id.to_string(),
        display_name: format!("Mock {plugin_id}"),
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

/// 发现 + 运行时（临时目录作 portable/install 源）。
pub fn runtime(tmp: &TempDir, config: RuntimeConfig) -> (Arc<PluginRegistry>, PluginRuntime) {
    let registry = Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    registry.discover();
    let runtime = PluginRuntime::with_config(registry.clone(), config);
    (registry, runtime)
}

// ---------------------------------------------------------------------------
// 剧本行构造（mock-plugin 私有回放格式；与 tools/mock-plugin 逐字对齐）
// ---------------------------------------------------------------------------

pub fn init_line(plugin_id: &str) -> String {
    format!(
        r#"{{"kind":"reply","method":"initialize","result":{{"id":"{plugin_id}","name":"Mock {plugin_id}","version":"0.1.0","capabilities":{{"annotate":false,"subscribe":false,"binary_sidecar":false}}}}}}"#
    )
}

pub fn can_handle_line() -> String {
    r#"{"kind":"reply","method":"can_handle","result":{"can_handle":true,"confidence":1.0}}"#
        .to_string()
}

/// schema 声明 `metrics`（id 列表；name 回落 id）。
pub fn schema_line(metrics: &[&str]) -> String {
    let defs: Vec<String> = metrics
        .iter()
        .map(|m| {
            format!(
                r#"{{"id":"{m}","name":"{m}","unit":"u","description":"d","aggregation":"last"}}"#
            )
        })
        .collect();
    format!(
        r#"{{"kind":"reply","method":"schema","result":{{"metrics":[{}]}}}}"#,
        defs.join(",")
    )
}

pub fn load_file_line() -> String {
    r#"{"kind":"reply","method":"load_file","result":{"record_count_hint":1}}"#.to_string()
}

pub fn progress_line(file_id: &str, percent: f64, records_so_far: u64) -> String {
    format!(
        r#"{{"kind":"emit","method":"progress","params":{{"file_id":"{file_id}","percent":{percent},"records_so_far":{records_so_far}}}}}"#
    )
}

/// 一条 Record 的 JSON（不带原始引用的最小形状）。
pub fn record_json(ts: i64, metric: &str, value: f64) -> String {
    format!(r#"{{"timestamp":{ts},"metric":"{metric}","value":{value}}}"#)
}

pub fn batch_line(file_id: &str, seq: u64, records_json: &str) -> String {
    format!(
        r#"{{"kind":"emit","method":"RecordBatch","params":{{"file_id":"{file_id}","seq":{seq},"records":[{records_json}],"done":false}}}}"#
    )
}

pub fn parse_line(records_total: u64) -> String {
    format!(r#"{{"kind":"reply","method":"parse","result":{{"records_total":{records_total}}}}}"#)
}

pub fn key_values_line(entries_json: &str) -> String {
    format!(r#"{{"kind":"reply","method":"key_values","result":{{"entries":[{entries_json}]}}}}"#)
}

pub fn shutdown_line() -> String {
    r#"{"kind":"reply","method":"shutdown","result":{}}"#.to_string()
}

/// happy_path 剧本对应的最小自包含剧本（动态 file_id，供并行多插件场景）。
pub fn happy_script(plugin_id: &str, file_id: &str) -> String {
    [
        init_line(plugin_id),
        can_handle_line(),
        schema_line(&["fps", "frame_ms"]),
        load_file_line(),
        progress_line(file_id, 0.5, 0),
        batch_line(
            file_id,
            0,
            &format!(
                "{},{}",
                record_json(1000, "fps", 59.8),
                record_json(1000, "frame_ms", 16.7)
            ),
        ),
        parse_line(2),
        key_values_line(r#"{"key":"scene","value":"boss"}"#),
        shutdown_line(),
    ]
    .join("\n")
}
