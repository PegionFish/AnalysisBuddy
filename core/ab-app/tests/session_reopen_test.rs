//! 会话保存→重开命令级回归（P0-01）：`load_session` 必须在响应内携带
//! 重开成功文件的**完整 `ImportResult`**（status ready + 真实文件名/路径/
//! 大小/匹配插件/时间域），前端直接写终态，不依赖重放进度事件的到达时序。
//!
//! 真实 Tauri 事件顺序下：后端 `load_session_logic` 内 `await` 重放全程，
//! `percent:100` 进度事件在响应返回**之前**即已发出；若前端只拿
//! `loaded_file_ids` 挂占位行，就永远收不到翻 ready 的事件（报告 P0-01
//! 症状：25 秒后仍是“解析中…（{{records}} 条）”）。本测试锁定
//! `LoadResultDto.files` 契约，杜绝该竞态回归。

use std::collections::HashMap;
use std::sync::Arc;

use ab_host::{PluginRegistry, PluginRuntime};
use ab_pipeline::mock::{FileFixture, MockSession, ParseStep, SessionFixture};
use ab_pipeline::{SessionRegistry, Store};
use ab_protocol::types::{
    Aggregation, FileSummary, MetricDef, Record, RecordBatch, SchemaResult, TimeRange,
};

use ab_app::commands::session::{load_session_logic, save_session_logic};
use ab_app::commands::{SessionSnapshotDto, TimeRangeDto};
use ab_app::pipeline_bridge::{ImportCoordinator, ImportStatus};

/// 数据域：2026-08-01T00:00:00Z（UTC 毫秒），与 real_plugin_suite 同域。
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
        parse_result: None,
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

/// 构造一条脚本化会话（预注册 → reopen 命中，无需拉起进程）。
fn coordinator_with(session: Arc<MockSession>) -> ImportCoordinator {
    let registry = Arc::new(SessionRegistry::new());
    registry.register(session);
    let discovery = Arc::new(PluginRegistry::new());
    ImportCoordinator::new(
        Arc::new(Store::new()),
        registry,
        tokio::sync::mpsc::unbounded_channel().0,
        Arc::new(PluginRuntime::new(discovery.clone())),
        discovery,
    )
}

/// 空协调器（无注册会话/发现：reopen 必失败 → reopen_failed 通道）。
fn empty_coordinator() -> ImportCoordinator {
    let discovery = Arc::new(PluginRegistry::new());
    ImportCoordinator::new(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
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

/// 每测试独立临时目录（并行测试互不干扰）。
fn tmp_dir(name: &str) -> std::path::PathBuf {
    let tmp =
        std::env::temp_dir().join(format!("ab-app-session-reopen-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mkdir");
    tmp
}

/// 把共享 fixture 复制到测试私有目录（缺失文件用例会删除数据文件，
/// 不得动共享 fixture 供并行测试使用）。
fn private_csv(tmp: &std::path::Path) -> std::path::PathBuf {
    let copy = tmp.join("small_with_header.csv");
    std::fs::copy(fixture_csv(), &copy).expect("copy fixture");
    copy
}

/// P0-01 主回归：load_session 响应携带完整 ready `ImportResult`（文件名/
/// 路径/大小/匹配插件/时间域），且 loaded_file_ids/missing/reopen_failed/
/// snapshot 均正确。
#[tokio::test]
async fn load_session_returns_ready_files_with_full_import_result() {
    let tmp = tmp_dir("ready");
    let csv = private_csv(&tmp);
    let csv_str = csv.display().to_string();

    // —— 首次导入 → Ready → 保存会话（含快照）——
    let mut files = HashMap::new();
    files.insert(csv_str.clone(), file_fixture());
    let coordinator = coordinator_with(session_with(files));
    let outcome = coordinator.import_with_plugin(csv.clone(), PLUGIN_ID).await;
    assert_eq!(
        outcome.status,
        ImportStatus::Ready,
        "首轮导入应 Ready：{:?}",
        outcome.error
    );
    let first_file_id = outcome.file_id.clone().expect("Ready 必带 file_id");

    let session_path = tmp.join("roundtrip.absession");
    let snapshot = SessionSnapshotDto {
        selected_metrics: HashMap::from([(
            first_file_id.clone(),
            vec![format!("{first_file_id}:{PLUGIN_ID}:fps")],
        )]),
        chart_view_state: Some(ab_app::commands::ChartViewStateDto {
            time_range: Some(TimeRangeDto {
                start_ms: T_BASE_MS,
                end_ms: T_BASE_MS + 2_000,
            }),
            legend_disabled: vec![],
            y_axis_scale: Some("shared".to_string()),
        }),
        cursor_ms: Some(T_BASE_MS + 1_000),
    };
    save_session_logic(&coordinator, &session_path, Some(snapshot.clone()))
        .expect("save session");

    // —— 重开：load_session_logic（与 Tauri command 同一逻辑体）——
    let result = load_session_logic(&coordinator, &session_path)
        .await
        .expect("load session");

    assert!(
        result.missing.is_empty(),
        "文件在盘且哈希一致 → 无 missing：{:?}",
        result.missing
    );
    assert!(
        result.reopen_failed.is_empty(),
        "重放成功 → 无 reopen_failed：{:?}",
        result.reopen_failed
    );

    // P0-01 核心：响应内必须携带完整 ready ImportResult。
    assert_eq!(result.files.len(), 1, "单文件会话 → 单个 ready 结果");
    let file = &result.files[0];
    assert_eq!(file.status, "ready");
    assert_eq!(file.name, "small_with_header.csv", "真实文件名");
    assert_eq!(file.path, csv_str, "真实路径");
    assert_eq!(
        file.size_bytes,
        std::fs::metadata(&csv).expect("metadata").len(),
        "真实文件大小"
    );
    let matched = file
        .matched_plugin
        .as_ref()
        .expect("ready 文件必须带匹配插件");
    assert_eq!(matched.plugin_id, PLUGIN_ID);
    let range = file.time_range.expect("ready 文件必须带实际数据时间域");
    assert!(range.start_ms <= range.end_ms);
    assert_eq!(range.start_ms, T_BASE_MS, "视口适配数据来自重开结果");

    // 兼容字段一致：loaded_file_ids 与 files 同源。
    assert_eq!(
        result.loaded_file_ids,
        vec![file.file_id.clone()],
        "loaded_file_ids 与 files 的 file_id 一致"
    );

    // 快照原样透传（后端不解析，恢复时原样返回）。
    assert_eq!(result.snapshot, Some(snapshot));

    // 序列化形状：snake_case 键 + files[] 可被前端直接消费。
    let json = serde_json::to_value(&result).expect("serialize");
    assert!(json.get("files").is_some(), "LoadResult 必须带 files 键");
    assert_eq!(json["files"][0]["status"], "ready");
    assert_eq!(json["files"][0]["file_id"], file.file_id);
    assert!(json["files"][0].get("matched_plugin").is_some());

    let _ = std::fs::remove_dir_all(&tmp);
}

/// 重开失败通道：插件会话不可用（空 registry/discovery）→ 该文件进入
/// `reopen_failed`，不进 `files`/`loaded_file_ids`（UI 逐项提示，P1-03）。
#[tokio::test]
async fn load_session_reports_reopen_failure_per_file() {
    let tmp = tmp_dir("reopen-failed");
    let csv = private_csv(&tmp);
    let csv_str = csv.display().to_string();

    // 首轮正常导入并保存。
    let mut files = HashMap::new();
    files.insert(csv_str.clone(), file_fixture());
    let coordinator = coordinator_with(session_with(files));
    let outcome = coordinator.import_with_plugin(csv.clone(), PLUGIN_ID).await;
    assert_eq!(outcome.status, ImportStatus::Ready);
    let session_path = tmp.join("reopen-fail.absession");
    save_session_logic(&coordinator, &session_path, None).expect("save");

    // 全新协调器（无插件可用）重开 → 逐项 reopen_failed。
    let result = load_session_logic(&empty_coordinator(), &session_path)
        .await
        .expect("load session");
    assert!(result.files.is_empty(), "失败文件不得伪装 ready");
    assert!(result.loaded_file_ids.is_empty());
    assert!(result.missing.is_empty(), "文件在盘 → 不是 missing");
    assert_eq!(result.reopen_failed.len(), 1);
    assert_eq!(result.reopen_failed[0].path, csv_str);
    assert_eq!(result.reopen_failed[0].reason, "reopen_failed");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// 缺失文件通道：记录文件已被删除 → `missing`（not_found），不影响其余。
#[tokio::test]
async fn load_session_marks_missing_files_without_pretending_ready() {
    let tmp = tmp_dir("missing");
    let csv = private_csv(&tmp);
    let csv_str = csv.display().to_string();

    let mut files = HashMap::new();
    files.insert(csv_str.clone(), file_fixture());
    let coordinator = coordinator_with(session_with(files));
    let outcome = coordinator.import_with_plugin(csv.clone(), PLUGIN_ID).await;
    assert_eq!(outcome.status, ImportStatus::Ready);
    let session_path = tmp.join("missing.absession");
    save_session_logic(&coordinator, &session_path, None).expect("save");

    // 删除数据文件 → verify_files 判 Missing。
    std::fs::remove_file(&csv).expect("remove csv");

    let result = load_session_logic(&coordinator, &session_path)
        .await
        .expect("load session");
    assert!(result.files.is_empty());
    assert!(result.loaded_file_ids.is_empty());
    assert_eq!(result.missing.len(), 1);
    assert_eq!(result.missing[0].path, csv_str);
    assert_eq!(result.missing[0].reason, "not_found");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// 会话文件不存在 → `file_not_found`（load_session 顶层错误形状）。
#[tokio::test]
async fn load_session_missing_session_file_rejects_file_not_found() {
    let coordinator = empty_coordinator();
    let err = load_session_logic(&coordinator, &std::path::PathBuf::from("Z:\\nope\\ghost.absession"))
        .await
        .expect_err("不存在的会话文件必须 reject");
    assert_eq!(err.code, "file_not_found");
}
