//! 宿主级集成测试（任务 21 必做）：`ImportCoordinator` 真实导入 →
//! `query_series_logic`（query_series 命令逻辑体）全链路。
//!
//! 覆盖缺口（复验者指出）：tests/e2e 仅覆盖插件级 parse/query，未覆盖
//! 宿主 `ImportCoordinator → query_series` 命令链路（复合 id 解析、
//! Store 查询、DTO 形状），此前无测试拦截。
//!
//! 链路：MockSession（脚本化 parse，数据域 2026-08-01）→
//! `import_with_plugin` 真实编排（load → schema → register → parse →
//! freeze）→ `query_series_logic`（与 Tauri command 同一逻辑体）→
//! 断言切片非空、时间戳落数据域、DTO 字段形状 `t_ms/v` 与前端一致。
//!
//! 注意：本测试拦截「宿主链路数据断流」类缺陷；命令参数命名失配
//! （任务 21 根因：tauri-macros 默认 camelCase vs 前端 snake_case）
//! 由 `command_arg_case_test` 静态固化（本机测试宿主无法构建 MockRuntime
//! 应用，见 acl_runtime_test 头注）。

use std::collections::HashMap;
use std::sync::Arc;

use ab_host::{PluginRegistry, PluginRuntime};
use ab_pipeline::mock::{FileFixture, MockSession, ParseStep, SessionFixture};
use ab_pipeline::{SessionRegistry, Store};
use ab_protocol::types::{
    Aggregation, FileSummary, MetricDef, Record, RecordBatch, SchemaResult, TimeRange,
};

use ab_app::commands::query::query_series_logic;
use ab_app::pipeline_bridge::{ImportCoordinator, ImportStatus};

/// 数据域：2026-08-01T00:00:00Z（UTC 毫秒），与五轮复验真实数据同域。
const T_BASE_MS: i64 = 1_785_542_400_000;
const PLUGIN_ID: &str = "mock-csv";

/// 单路径 fixture：schema 声明 `fps`，parse 吐 3 点（2026-08-01 域）。
fn file_fixture() -> FileFixture {
    let records: Vec<Record> = (0..3)
        .map(|i| Record {
            timestamp: T_BASE_MS + i * 1_000,
            metric: "fps".to_string(),
            value: 60.0 - i as f64,
            level: None,
            tags: None,
            raw_line: None,
        })
        .collect();
    let batch = RecordBatch {
        file_id: String::new(), // MockSession 按 parse file_id 回显
        seq: 0,
        records,
        done: true,
    };
    FileFixture {
        load_file: Some(Ok(FileSummary {
            record_count_hint: Some(3),
            time_range: Some(TimeRange {
                start_ms: T_BASE_MS,
                end_ms: T_BASE_MS + 2_000,
            }),
            note: None,
        })),
        parse_script: vec![ParseStep::Batch(batch)],
        parse_result: None, // 缺省 = Σ各批 len = 3
        key_values: None,
    }
}

fn session_with(files: HashMap<String, FileFixture>) -> Arc<MockSession> {
    MockSession::new(SessionFixture {
        plugin_id: PLUGIN_ID.to_string(),
        schema: Some(Ok(SchemaResult {
            metrics: vec![MetricDef {
                id: "fps".to_string(),
                name: "Frames per second".to_string(),
                unit: Some("Hz".to_string()),
                description: None,
                aggregation: Aggregation::Last,
            }],
        })),
        can_handle: None,
        files,
    })
}

/// 构造一条脚本化会话：schema 声明 `fps`，parse 吐 3 点（2026-08-01 域）。
fn scripted_session(csv_path_str: &str) -> Arc<MockSession> {
    let mut files = HashMap::new();
    files.insert(csv_path_str.to_string(), file_fixture());
    session_with(files)
}

/// 双路径脚本化会话（file_ids 过滤链路测试：每路径独立 file_id）。
fn scripted_session_two(a: &str, b: &str) -> Arc<MockSession> {
    let mut files = HashMap::new();
    files.insert(a.to_string(), file_fixture());
    files.insert(b.to_string(), file_fixture());
    session_with(files)
}

fn coordinator_with(session: Arc<MockSession>) -> ImportCoordinator {
    let registry = Arc::new(SessionRegistry::new());
    registry.register(session); // 预注册 → ensure_session 命中，无需拉起进程
    let discovery = Arc::new(PluginRegistry::new());
    ImportCoordinator::new(
        Arc::new(Store::new()),
        registry,
        tokio::sync::mpsc::unbounded_channel().0,
        Arc::new(PluginRuntime::new(discovery.clone())),
        discovery,
    )
}

/// fixture 真实路径（导入编排会读取文件元信息，路径必须存在）。
fn fixture_csv() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/small_with_header.csv");
    assert!(path.exists(), "缺少测试 fixture：{}", path.display());
    path
}

/// 主链路：真实导入后 query_series_logic 必须返回非空切片，时间戳落
/// 2026-08-01 数据域，DTO 形状与前端 SeriesSlice/SeriesPoint 一致。
#[tokio::test]
async fn import_then_query_series_returns_data_in_range() {
    let csv = fixture_csv();
    let csv_str = csv.display().to_string();
    let coordinator = coordinator_with(scripted_session(&csv_str));

    // —— 真实导入编排（load → schema → register → parse → freeze）——
    let outcome = coordinator.import_with_plugin(csv.clone(), PLUGIN_ID).await;
    assert_eq!(
        outcome.status,
        ImportStatus::Ready,
        "导入应 Ready，实际：{:?}",
        outcome.error
    );
    let file_id = outcome.file_id.clone().expect("Ready 必带 file_id");
    assert!(
        coordinator.list_frozen().contains(&file_id),
        "导入完成后文件必须 Frozen（query 前置条件）"
    );

    // —— query_series 命令逻辑体（复合 id `file:plugin:metric`）——
    let composite = format!("{file_id}:{PLUGIN_ID}:fps");
    let slices = query_series_logic(
        &coordinator,
        std::slice::from_ref(&file_id),
        &[composite],
        T_BASE_MS,
        T_BASE_MS + 2_000,
        4000,
    )
    .expect("query_series 不应 reject");
    assert_eq!(slices.len(), 1, "单一 metric 应返回单一切片");
    let slice = &slices[0];
    assert_eq!(slice.point_count, 3, "3 条记录全部落窗");
    assert!(!slice.downsampled, "3 点远低于预算，不应降采样");
    assert_eq!(slice.file_id, file_id);
    assert_eq!(slice.plugin_id, PLUGIN_ID, "plugin_id 必须经复合 id 回填");
    assert_eq!(slice.metric_id, "fps");
    for point in &slice.points {
        assert!(
            (T_BASE_MS..=T_BASE_MS + 2_000).contains(&point.t_ms),
            "时间戳必须落在 2026-08-01 数据域：{}",
            point.t_ms
        );
    }

    // —— DTO 序列化形状 == 前端契约（SeriesPointDto {t_ms, v}）——
    let value = serde_json::to_value(&slices).expect("serialize");
    let first = &value[0]["points"][0];
    assert!(first.get("t_ms").is_some(), "DTO 必须用 snake_case t_ms");
    assert!(first.get("v").is_some(), "DTO 必须用 v");
    assert!(first.get("tMs").is_none(), "不得出现 camelCase 键");
}

/// 时间窗与数据域不相交 → 空结果（任务 19 前的症状复现窗口：视口
/// epoch 0~600s 查 2026 数据必空；视口适配后不再出现，此为语义基线）。
#[tokio::test]
async fn query_outside_data_range_yields_empty() {
    let csv = fixture_csv();
    let csv_str = csv.display().to_string();
    let coordinator = coordinator_with(scripted_session(&csv_str));
    let outcome = coordinator.import_with_plugin(csv.clone(), PLUGIN_ID).await;
    let file_id = outcome.file_id.expect("Ready");
    let composite = format!("{file_id}:{PLUGIN_ID}:fps");
    let slices = query_series_logic(&coordinator, &[file_id], &[composite], 0, 600_000, 4000)
        .expect("query_series 不应 reject");
    assert!(slices.is_empty(), "窗口与数据域不相交 → 空切片");
}

/// 未知/畸形复合 id 静默忽略（§1.5）：不 reject、不进结果。
#[tokio::test]
async fn malformed_composite_ids_are_ignored_not_rejected() {
    let csv = fixture_csv();
    let csv_str = csv.display().to_string();
    let coordinator = coordinator_with(scripted_session(&csv_str));
    let outcome = coordinator.import_with_plugin(csv.clone(), PLUGIN_ID).await;
    let file_id = outcome.file_id.expect("Ready");
    let slices = query_series_logic(
        &coordinator,
        &[file_id],
        &["not-a-composite".to_string(), "a:b".to_string()],
        T_BASE_MS,
        T_BASE_MS + 2_000,
        4000,
    )
    .expect("畸形 id 只忽略不 reject");
    assert!(slices.is_empty());
}

/// `file_ids` 权威过滤（契约修复）：metrics 混入未授权文件的复合 id 时
/// 其切片必须被静默忽略（与 mock/UI 一致）；空 `file_ids` → 空结果。
#[tokio::test]
async fn query_series_file_ids_authoritatively_filters_series() {
    let csv_a = fixture_csv();
    let csv_b = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/small_no_header.csv");
    assert!(csv_b.exists(), "缺少测试 fixture：{}", csv_b.display());
    let coordinator = coordinator_with(scripted_session_two(
        &csv_a.display().to_string(),
        &csv_b.display().to_string(),
    ));

    // —— 两文件真实导入（每路径独立 file_id）——
    let outcome_a = coordinator.import_with_plugin(csv_a, PLUGIN_ID).await;
    let outcome_b = coordinator.import_with_plugin(csv_b, PLUGIN_ID).await;
    assert_eq!(outcome_a.status, ImportStatus::Ready);
    assert_eq!(outcome_b.status, ImportStatus::Ready);
    let id_a = outcome_a.file_id.expect("Ready 必带 file_id");
    let id_b = outcome_b.file_id.expect("Ready 必带 file_id");
    assert_ne!(id_a, id_b, "两文件必须分配不同 file_id");
    assert!(coordinator.list_frozen().contains(&id_a));
    assert!(coordinator.list_frozen().contains(&id_b));

    let composites = vec![
        format!("{id_a}:{PLUGIN_ID}:fps"),
        format!("{id_b}:{PLUGIN_ID}:fps"),
    ];

    // 只授权 id_a：id_b 的切片必须被过滤。
    let slices = query_series_logic(
        &coordinator,
        std::slice::from_ref(&id_a),
        &composites,
        T_BASE_MS,
        T_BASE_MS + 2_000,
        4000,
    )
    .expect("query_series 不应 reject");
    let file_ids: Vec<&str> = slices.iter().map(|s| s.file_id.as_str()).collect();
    assert_eq!(file_ids, vec![id_a.as_str()], "仅 id_a 的切片可返回");

    // 只授权 id_b：镜像断言。
    let slices = query_series_logic(
        &coordinator,
        std::slice::from_ref(&id_b),
        &composites,
        T_BASE_MS,
        T_BASE_MS + 2_000,
        4000,
    )
    .expect("query_series 不应 reject");
    let file_ids: Vec<&str> = slices.iter().map(|s| s.file_id.as_str()).collect();
    assert_eq!(file_ids, vec![id_b.as_str()], "仅 id_b 的切片可返回");

    // 空 file_ids → 空结果（无文件匹配；与 mock 行为一致）。
    let slices = query_series_logic(
        &coordinator,
        &[],
        &composites,
        T_BASE_MS,
        T_BASE_MS + 2_000,
        4000,
    )
    .expect("query_series 不应 reject");
    assert!(slices.is_empty(), "空 file_ids 必须返回空切片");
}
