//! Quest M4.2 内存预算硬顶 engine 侧集成测试：复刻 custom_query_test 的
//! MockSession 注入模式（override 路径跳过 can_handle 自动匹配，避免拉起
//! 真实插件进程），覆盖：
//! - 预算内导入成功（默认 None 不设限 / 预算充足）；
//! - 超预算导入 → 该文件 outcome `status:"error"` 且
//!   `error.code = "memory_budget_exceeded"`，数据已卸载
//!   （store 清空、get_metrics 不含该文件）；
//! - 同批其他文件不受影响（逐文件语义与桌面 import_files 一致）。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ab_engine::commands::import::import_files_logic;
use ab_engine::commands::query::get_metrics_logic;
use ab_engine::commands::ImportOverride;
use ab_engine::pipeline_bridge::{
    ImportCoordinator, ImportStatus, PipelineConfig, MEMORY_BUDGET_EXCEEDED,
};
use ab_pipeline::mock::{FileFixture, MockSession, ParseStep, SessionFixture};
use ab_pipeline::{PipelineEvent, PluginSession, SessionRegistry, Store};
use ab_protocol::types::{Aggregation, MetricDef, Record, RecordBatch, SchemaResult};
use tokio::sync::mpsc;

/// 每条纯记录（metric `"m"`）的近似记账：24 + 1 = 25 字节。
const BYTES_PER_RECORD: u64 = 24 + 1;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-engine-budget-{}-{}-{tag}",
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

/// 两个导入夹具：big 20 条（500 B）必超预算，small 2 条（50 B）必在预算内。
struct Fixture {
    _dir: TempDir,
    big: PathBuf,
    small: PathBuf,
    /// big/small 的 path.display() 字符串（MockSession fixture 键）。
    big_key: String,
    small_key: String,
}

fn fixture() -> Fixture {
    let dir = TempDir::new("files");
    let big = dir.path().join("big.csv");
    let small = dir.path().join("small.csv");
    // read_file_info 需要真实文件（元信息 + 头部采样）；内容不参与 parse。
    fs::write(&big, "timestamp,m\n1,1\n").expect("write big");
    fs::write(&small, "timestamp,m\n1,1\n").expect("write small");
    Fixture {
        big_key: big.display().to_string(),
        small_key: small.display().to_string(),
        big,
        small,
        _dir: dir,
    }
}

fn plain_records(count: usize) -> Vec<Record> {
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

fn batch(file_id: &str, seq: u64, records: Vec<Record>) -> RecordBatch {
    RecordBatch {
        file_id: file_id.to_string(),
        seq,
        records,
        done: false,
    }
}

/// 白名单非空（metric "m"）的 schema，保证记录实际入库（记账才累加）。
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

/// 注册 mock 会话：big 20 条 / small 2 条（键为 path.display() 字符串）。
fn mock_session(fixture: &Fixture) -> Arc<MockSession> {
    let mut files = HashMap::new();
    files.insert(
        fixture.big_key.clone(),
        FileFixture {
            parse_script: vec![
                ParseStep::Batch(batch("f", 0, plain_records(10))),
                ParseStep::Batch(batch("f", 1, plain_records(10))),
            ],
            ..Default::default()
        },
    );
    files.insert(
        fixture.small_key.clone(),
        FileFixture {
            parse_script: vec![ParseStep::Batch(batch("f", 0, plain_records(2)))],
            ..Default::default()
        },
    );
    MockSession::new(SessionFixture {
        plugin_id: "mock".to_string(),
        schema: Some(Ok(schema_with_metric())),
        files,
        ..Default::default()
    })
}

/// 注入预算配置的 coordinator（事件通道可观测；会话经 registry 注入）。
fn coordinator_with(
    budget: Option<u64>,
) -> (ImportCoordinator, mpsc::UnboundedReceiver<PipelineEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let coordinator = ImportCoordinator::with_config(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
        tx,
        Arc::new(ab_host::PluginRuntime::new(Arc::new(
            ab_host::PluginRegistry::new(),
        ))),
        Arc::new(ab_host::PluginRegistry::new()),
        PipelineConfig {
            memory_budget_bytes: budget,
            ..PipelineConfig::default()
        },
    );
    (coordinator, rx)
}

#[tokio::test]
async fn budget_defaults_to_unlimited_and_within_budget_import_succeeds() {
    let fixture = fixture();
    assert_eq!(
        PipelineConfig::default().memory_budget_bytes,
        None,
        "默认不设限（None = unlimited）"
    );
    let (coordinator, _rx) = coordinator_with(None);
    coordinator
        .registry()
        .register(mock_session(&fixture) as Arc<dyn PluginSession>);
    let outcome = coordinator
        .import_with_plugin(fixture.small.clone(), "mock")
        .await;
    assert_eq!(outcome.status, ImportStatus::Ready, "{outcome:?}");
    assert_eq!(coordinator.memory_budget_bytes(), None, "访问器与配置一致");
    // 2 条 × 25 B = 50 B 驻留。
    let file_id = outcome.file_id.expect("ready outcome carries file_id");
    assert_eq!(coordinator.store().memory_bytes(), 2 * BYTES_PER_RECORD);
    assert_eq!(
        coordinator.store().memory_bytes_of(&file_id),
        Some(2 * BYTES_PER_RECORD)
    );
}

#[tokio::test]
async fn over_budget_import_errors_and_unloads_data() {
    let fixture = fixture();
    // small（50 B）< 预算（100 B）< big（500 B）。
    let (coordinator, mut rx) = coordinator_with(Some(100));
    coordinator
        .registry()
        .register(mock_session(&fixture) as Arc<dyn PluginSession>);

    let outcome = coordinator
        .import_with_plugin(fixture.big.clone(), "mock")
        .await;
    assert_eq!(outcome.status, ImportStatus::Error, "{outcome:?}");
    let error = outcome.error.expect("over-budget outcome carries error");
    assert_eq!(error.code, MEMORY_BUDGET_EXCEEDED);
    assert!(
        error.message.contains("memory budget"),
        "message 应说明预算超限: {error:?}"
    );
    assert_eq!(outcome.file_id, None, "error outcome 不携带 file_id");

    // 数据已卸载：store 清空、无可查询文件。
    assert_eq!(coordinator.store().memory_bytes(), 0, "半成品已 unload");
    assert!(coordinator.list_frozen().is_empty(), "未进入 frozen");
    assert!(
        get_metrics_logic(&coordinator, None).is_empty(),
        "get_metrics 不含超限文件"
    );

    // 终态事件：ParseFailed reason = memory_budget_exceeded。
    let mut parse_failed = None;
    while let Ok(event) = rx.try_recv() {
        if let PipelineEvent::ParseFailed { reason, .. } = event {
            parse_failed = Some(reason);
        }
    }
    assert_eq!(
        parse_failed.as_deref(),
        Some(MEMORY_BUDGET_EXCEEDED),
        "ParseFailed 事件 reason 应为预算错误码"
    );
}

#[tokio::test]
async fn same_batch_over_budget_file_does_not_affect_others() {
    let fixture = fixture();
    let (coordinator, _rx) = coordinator_with(Some(100));
    coordinator
        .registry()
        .register(mock_session(&fixture) as Arc<dyn PluginSession>);

    // 同批两文件（经 overrides 直连 mock 会话，跳过 can_handle 探测）：
    // 逐文件 outcome 语义与桌面 import_files 一致——超限文件 error，
    // 其余文件照常 ready。
    let overrides = HashMap::from([
        (
            fixture.big_key.clone(),
            ImportOverride {
                plugin_id: "mock".to_string(),
            },
        ),
        (
            fixture.small_key.clone(),
            ImportOverride {
                plugin_id: "mock".to_string(),
            },
        ),
    ]);
    let results = import_files_logic(
        &coordinator,
        vec![
            fixture.big.display().to_string(),
            fixture.small.display().to_string(),
        ],
        Some(overrides),
    )
    .await
    .expect("import_files_logic");

    assert_eq!(results.len(), 2, "与入参同序");
    let big = &results[0];
    assert_eq!(big.status, "error", "超限文件 outcome error");
    let error = big.error.as_ref().expect("error present");
    assert_eq!(error.code, MEMORY_BUDGET_EXCEEDED);

    let small = &results[1];
    assert_eq!(small.status, "ready", "预算内文件不受影响");
    assert!(!small.file_id.is_empty(), "ready 携带 file_id");

    // 只有 small 驻留（big 已 unload）；get_metrics 仅含 small。
    assert_eq!(coordinator.store().memory_bytes(), 2 * BYTES_PER_RECORD);
    assert_eq!(
        coordinator.store().memory_bytes_of(&small.file_id),
        Some(2 * BYTES_PER_RECORD)
    );
    let tree = get_metrics_logic(&coordinator, None);
    let file_nodes: Vec<&str> = tree
        .iter()
        .filter(|node| node.level == "file")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        file_nodes,
        vec![small.file_id.as_str()],
        "get_metrics 仅含 small"
    );
}
