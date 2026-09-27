//! C1（卷三主题 2 第一环）engine 侧集成测试：load 成功后的失败出口必须
//! best-effort 通知插件 `unload_file`——插件侧 loaded_files 清空后其空闲
//! 回收才能启动，否则每次「load 成功但 parse 失败」都让插件进程驻留整份
//! 原始数据直到会话终结。复刻 memory_budget_test 的 MockSession 注入模式
//!（override 路径跳过 can_handle 匹配，不拉真实插件进程）。
//!
//! 覆盖出口（import_one post-load 段）：
//! - schema 失败（`SessionFixture.schema = Err`）；
//! - parse 失败（`ParseStep::Fail`）；
//! - 成功路径不调 unload_file（对照，防误伤）。
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ab_engine::pipeline_bridge::{ImportCoordinator, ImportStatus, PipelineConfig};
use ab_pipeline::mock::{FileFixture, MockSession, ParseStep, SessionFixture};
use ab_pipeline::{PipelineEvent, PluginSession, SessionError, SessionRegistry, Store};
use ab_protocol::types::{Aggregation, MetricDef, Record, RecordBatch, SchemaResult};
use tokio::sync::mpsc;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-engine-c1-{}-{}-{tag}",
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

fn schema_with_metric() -> SchemaResult {
    SchemaResult {
        metrics: vec![MetricDef {
            id: "m".to_string(),
            name: "m".to_string(),
            unit: None,
            description: None,
            aggregation: Aggregation::Last,
        }],
    }
}

fn plugin_error() -> SessionError {
    SessionError::Plugin {
        code: -32002,
        message: "scripted failure".to_string(),
    }
}

fn batch(records: Vec<Record>) -> RecordBatch {
    RecordBatch {
        file_id: "f".to_string(),
        seq: 0,
        records,
        done: false,
    }
}

fn records(count: usize) -> Vec<Record> {
    (0..count)
        .map(|i| Record {
            timestamp: i as i64,
            metric: "m".to_string(),
            value: i as f64,
            level: None,
            tags: None,
            raw_line: None,
        })
        .collect()
}

fn coordinator() -> (ImportCoordinator, mpsc::UnboundedReceiver<PipelineEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        ImportCoordinator::with_config(
            Arc::new(Store::new()),
            Arc::new(SessionRegistry::new()),
            tx,
            Arc::new(ab_host::PluginRuntime::new(Arc::new(
                ab_host::PluginRegistry::new(),
            ))),
            Arc::new(ab_host::PluginRegistry::new()),
            PipelineConfig::default(),
        ),
        rx,
    )
}

fn temp_csv(tag: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new(tag);
    let file = dir.path().join(format!("{tag}.csv"));
    fs::write(&file, "timestamp,m\n1,1\n").expect("write fixture");
    (dir, file)
}

/// schema 失败（load 已成功）：出口必须通知插件 unload_file。
#[tokio::test]
async fn schema_failure_notifies_plugin_unload() {
    let (_dir, file) = temp_csv("c1-schema");
    let (coordinator, _rx) = coordinator();
    let session = MockSession::new(SessionFixture {
        live: None,
        plugin_id: "mock".to_string(),
        schema: Some(Err(plugin_error())),
        files: HashMap::from([(
            file.display().to_string(),
            FileFixture::default(), // load 成功（缺省 Ok）
        )]),
        ..Default::default()
    });
    coordinator
        .registry()
        .register(session.clone() as Arc<dyn PluginSession>);

    let outcome = coordinator.import_with_plugin(file.clone(), "mock").await;
    assert_eq!(outcome.status, ImportStatus::Error, "{outcome:?}");
    assert_eq!(
        session.stats().unload_file_calls,
        1,
        "schema 失败出口必须通知插件 unload_file（其空闲回收才能启动）"
    );
}

/// parse 失败（load 已成功、批次已推）：出口必须通知插件 unload_file。
#[tokio::test]
async fn parse_failure_notifies_plugin_unload() {
    let (_dir, file) = temp_csv("c1-parse");
    let (coordinator, _rx) = coordinator();
    let session = MockSession::new(SessionFixture {
        live: None,
        plugin_id: "mock".to_string(),
        schema: Some(Ok(schema_with_metric())),
        files: HashMap::from([(
            file.display().to_string(),
            FileFixture {
                parse_script: vec![
                    ParseStep::Batch(batch(records(2))),
                    ParseStep::Fail(plugin_error()),
                ],
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
    coordinator
        .registry()
        .register(session.clone() as Arc<dyn PluginSession>);

    let outcome = coordinator.import_with_plugin(file.clone(), "mock").await;
    assert_eq!(outcome.status, ImportStatus::Error, "{outcome:?}");
    assert_eq!(
        session.stats().unload_file_calls,
        1,
        "parse 失败出口必须通知插件 unload_file"
    );
}

/// 对照：成功路径不得调 unload_file（数据仍驻留供查询）。
#[tokio::test]
async fn successful_import_does_not_unload() {
    let (_dir, file) = temp_csv("c1-ok");
    let (coordinator, _rx) = coordinator();
    let session = MockSession::new(SessionFixture {
        live: None,
        plugin_id: "mock".to_string(),
        schema: Some(Ok(schema_with_metric())),
        files: HashMap::from([(
            file.display().to_string(),
            FileFixture {
                parse_script: vec![ParseStep::Batch(batch(records(2)))],
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
    coordinator
        .registry()
        .register(session.clone() as Arc<dyn PluginSession>);

    let outcome = coordinator.import_with_plugin(file.clone(), "mock").await;
    assert_eq!(outcome.status, ImportStatus::Ready, "{outcome:?}");
    assert_eq!(
        session.stats().unload_file_calls,
        0,
        "成功导入不得卸载（数据供查询）"
    );
}

/// C3（卷三主题 5）：registry 中的死会话不得短路 ensure_session——必须被
/// 移除并走 get_or_spawn 复活路径（此处宿主无真实插件 → spawn 失败，
/// 但断言点在「死会话被逐出且未被复用」，不依赖真实插件）。
#[tokio::test]
async fn dead_session_is_evicted_not_reused() {
    let (_dir, file) = temp_csv("c3-dead");
    let (coordinator, _rx) = coordinator();
    let dead = MockSession::new(SessionFixture {
        plugin_id: "mock".to_string(),
        live: Some(false),
        ..Default::default()
    });
    coordinator
        .registry()
        .register(dead.clone() as Arc<dyn PluginSession>);

    let outcome = coordinator.import_with_plugin(file.clone(), "mock").await;
    // 宿主 spawn 失败（无真实 mock 插件进程）→ outcome error，但绝不能是
    // 死会话被复用后产生的插件级成功/插件错误
    assert_eq!(
        outcome.status,
        ab_engine::pipeline_bridge::ImportStatus::Error
    );
    assert!(
        dead.stats().load_file_calls == 0,
        "死会话不得被复用（load_file 不应被调用）"
    );
    assert!(
        coordinator.registry().get("mock").is_none(),
        "死会话条目必须被逐出（复活路径前置清理）"
    );
}
