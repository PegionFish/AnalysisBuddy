//! B-03 会话文件测试：schema 往返、原子写、sha256 三态、重开接线。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ab_pipeline::mock::{FileFixture, MockSession, ParseStep, SessionFixture};
use ab_pipeline::{
    reopen_files, run_query, save_session, sha256_of_file, verify_files, ChartViewState,
    FileVerifyStatus, MetricRef, QueryRequest, SessionFile, SessionFileEntry, SessionFileError,
    SessionRegistry, Store, YAxisScale,
};
use ab_protocol::types::{Aggregation, MetricDef, Record, SchemaResult, TimeRange};
use tokio::sync::mpsc;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 每测试独立临时目录（std 自建，避免引入 tempfile 依赖）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "abp-session-{name}-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn sample_session_file() -> SessionFile {
    SessionFile {
        version: 1,
        files: vec![
            SessionFileEntry {
                path: r"C:\logs\match_20260807.csv".to_string(),
                sha256: "9f2c4a7e1b8d3f60a5e7c2d9b4f1a8e3d6c5b0a7f2e9d4c1b8a3f6e0d7c2b9a4"
                    .to_string(),
                plugin_id: "builtin-csv".to_string(),
            },
            SessionFileEntry {
                path: r"C:\logs\tool_session_0807.log".to_string(),
                sha256: "3d8e1f5a2c7b90d4e6a1f8c3b5d7e2a90f4c6b8d1e3a5f7c9b0d2e4a6f8c1b3e"
                    .to_string(),
                plugin_id: "demo-tool".to_string(),
            },
        ],
        selected_metrics: HashMap::from([
            (
                "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c".to_string(),
                vec!["fps".to_string(), "frame_ms".to_string()],
            ),
            (
                "a1b2c3d4-0e5f-4a61-8b7c-9d0e1f2a3b4c".to_string(),
                vec!["cpu_pct".to_string()],
            ),
        ]),
        chart_view_state: ChartViewState {
            time_range: Some(TimeRange {
                start_ms: 1_785_601_200_000,
                end_ms: 1_785_602_400_000,
            }),
            legend_disabled: vec!["a1b2c3d4-0e5f-4a61-8b7c-9d0e1f2a3b4c/mem_mb".to_string()],
            y_axis_scale: YAxisScale::PerSeries,
        },
        cursor_ms: Some(1_785_601_234_567),
    }
}

#[test]
fn roundtrip_example_schema_field_lossless() {
    let dir = TempDir::new("roundtrip");
    let path = dir.file("session.absession");
    let original = sample_session_file();
    save_session(&original, &path).unwrap();

    let opened = ab_pipeline::open_session(&path).unwrap();
    assert_eq!(opened, original, "往返后字段不丢失");
    assert_eq!(opened.version, 1);
    assert_eq!(opened.files.len(), 2);
    assert_eq!(opened.files[0].plugin_id, "builtin-csv");
    assert_eq!(
        opened.selected_metrics["f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c"],
        vec!["fps".to_string(), "frame_ms".to_string()]
    );
    assert_eq!(opened.chart_view_state.y_axis_scale, YAxisScale::PerSeries);
    assert_eq!(opened.cursor_ms, Some(1_785_601_234_567));
}

#[test]
fn cursor_none_omits_key_and_never_emits_null() {
    let dir = TempDir::new("omit");
    let path = dir.file("session.absession");
    let mut session = sample_session_file();
    session.cursor_ms = None;
    session.chart_view_state.time_range = None;
    save_session(&session, &path).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("cursor_ms"), "无游标时省略键");
    assert!(!text.contains("time_range"));
    assert!(!text.contains("null"), "禁止输出 null");
    // UTF-8 无 BOM
    let bytes = fs::read(&path).unwrap();
    assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    // 反序列化回 None
    let reopened = ab_pipeline::open_session(&path).unwrap();
    assert_eq!(reopened.cursor_ms, None);
    assert_eq!(reopened.chart_view_state.time_range, None);
}

#[test]
fn version_greater_than_one_rejected_with_readable_error() {
    let dir = TempDir::new("version");
    let path = dir.file("future.absession");
    let mut session = sample_session_file();
    session.version = 2;
    save_session(&session, &path).unwrap();
    let err = ab_pipeline::open_session(&path).unwrap_err();
    assert!(
        matches!(err, SessionFileError::UnsupportedVersion(2)),
        "更高版本应拒绝打开"
    );
    assert!(err.to_string().contains("upgrade"), "错误信息可读");
}

#[test]
fn atomic_write_leaves_no_tmp_and_survives_mid_write_failure() {
    let dir = TempDir::new("atomic");
    let path = dir.file("session.absession");
    let original = sample_session_file();
    save_session(&original, &path).unwrap();

    // 成功路径：无 *.tmp 残留
    let residues: Vec<_> = fs::read_dir(&dir.path)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(residues.is_empty(), "落盘后无 tmp 残留");

    // 失败注入：tmp 路径被同名目录占据 → create 失败，旧文件不损坏
    let tmp_blocker = dir.file("session.absession.tmp");
    fs::create_dir(&tmp_blocker).unwrap();
    let before = fs::read(&path).unwrap();
    let err = save_session(&original, &path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    let after = fs::read(&path).unwrap();
    assert_eq!(before, after, "写入中途失败不损坏旧文件");
    fs::remove_dir(&tmp_blocker).unwrap();

    // 失败后仍可正常重写
    save_session(&original, &path).unwrap();
    assert_eq!(ab_pipeline::open_session(&path).unwrap(), original);
}

#[test]
fn sha256_is_64_lowercase_hex_and_three_state_verify() {
    let dir = TempDir::new("sha");
    let path = dir.file("a.log");
    fs::write(&path, "hello world").unwrap();
    let digest = sha256_of_file(&path).unwrap();
    assert_eq!(digest.len(), 64);
    assert!(digest
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));

    let session = SessionFile {
        version: 1,
        files: vec![SessionFileEntry {
            path: path.to_string_lossy().to_string(),
            sha256: digest,
            plugin_id: "mock-csv".to_string(),
        }],
        selected_metrics: HashMap::new(),
        chart_view_state: ChartViewState {
            time_range: None,
            legend_disabled: vec![],
            y_axis_scale: YAxisScale::Shared,
        },
        cursor_ms: None,
    };
    let status = verify_files(&session);
    assert_eq!(status[&session.files[0].path], FileVerifyStatus::Ok);

    // Missing：文件删除
    fs::remove_file(&path).unwrap();
    let status = verify_files(&session);
    assert_eq!(status[&session.files[0].path], FileVerifyStatus::Missing);

    // Modified：内容改动
    fs::write(&path, "hello world!").unwrap();
    let status = verify_files(&session);
    assert_eq!(status[&session.files[0].path], FileVerifyStatus::Modified);
}

#[test]
fn session_schema_contains_no_record_or_parse_data_fields() {
    let dir = TempDir::new("schema-static");
    let path = dir.file("session.absession");
    save_session(&sample_session_file(), &path).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    for forbidden in ["timestamp", "value", "raw_line", "records_total", "seq"] {
        assert!(
            !text.contains(forbidden),
            "会话文件不得含解析数据字段: {forbidden}"
        );
    }
}

#[tokio::test]
async fn reopen_uses_recorded_plugin_id_without_can_handle() {
    let dir = TempDir::new("reopen");
    let log_path = dir.file("a.log");
    fs::write(&log_path, "fps,frame_ms\n60,16\n30,33").unwrap();

    let metric_fps = MetricDef {
        id: "fps".to_string(),
        name: "FPS".to_string(),
        unit: None,
        description: None,
        aggregation: Aggregation::Last,
    };
    let fixture = SessionFixture {
        plugin_id: "mock-csv".to_string(),
        schema: Some(Ok(SchemaResult {
            metrics: vec![metric_fps],
        })),
        can_handle: None,
        files: HashMap::from([(
            log_path.to_string_lossy().to_string(),
            FileFixture {
                load_file: Some(Ok(ab_protocol::types::FileSummary {
                    record_count_hint: Some(2),
                    time_range: None,
                    note: None,
                })),
                parse_script: vec![ParseStep::Batch(ab_protocol::types::RecordBatch {
                    file_id: String::new(),
                    seq: 0,
                    records: vec![
                        Record {
                            timestamp: 1_000,
                            metric: "fps".to_string(),
                            value: 60.0,
                            level: None,
                            tags: None,
                            raw_line: None,
                        },
                        Record {
                            timestamp: 2_000,
                            metric: "fps".to_string(),
                            value: 30.0,
                            level: None,
                            tags: None,
                            raw_line: None,
                        },
                    ],
                    done: true,
                })],
                parse_result: Some(Ok(2)),
                key_values: None,
            },
        )]),
    };
    let mock = MockSession::new(fixture);

    let registry = Arc::new(SessionRegistry::new());
    registry.register(mock.clone());

    let session = SessionFile {
        version: 1,
        files: vec![SessionFileEntry {
            path: log_path.to_string_lossy().to_string(),
            sha256: sha256_of_file(&log_path).unwrap(),
            plugin_id: "mock-csv".to_string(),
        }],
        selected_metrics: HashMap::new(),
        chart_view_state: ChartViewState {
            time_range: None,
            legend_disabled: vec![],
            y_axis_scale: YAxisScale::Shared,
        },
        cursor_ms: None,
    };

    let store = Arc::new(Store::new());
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let event_drain = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(ev) = events_rx.recv().await {
            seen.push(ev);
        }
        seen
    });

    let outcomes = reopen_files(store.clone(), registry, &session, &events_tx).await;
    drop(events_tx);
    let events = event_drain.await.unwrap();

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].verify, FileVerifyStatus::Ok);
    assert_eq!(outcomes[0].error, None);
    let file_id = outcomes[0].file_id.as_ref().expect("校验通过应重解析");

    // 数据落库可查询
    let slices = run_query(
        &store,
        &QueryRequest {
            metrics: vec![MetricRef {
                file_id: file_id.clone(),
                metric: "fps".to_string(),
            }],
            t0_ms: 0,
            t1_ms: 100_000,
            max_points_per_series: 1000,
        },
    );
    assert_eq!(slices.len(), 1);
    assert_eq!(slices[0].ts, vec![1_000, 2_000]);

    // 重开走记录的 plugin_id：can_handle 一次都不调用，load+parse 各一次
    let stats = mock.stats();
    assert_eq!(
        stats.can_handle_calls, 0,
        "重开不得触发 can_handle 自动匹配"
    );
    assert_eq!(stats.load_file_calls, 1);
    assert_eq!(stats.parse_calls, 1);
    assert_eq!(stats.loaded_file_ids, vec![file_id.clone()]);
    assert_eq!(stats.parsed_file_ids, vec![file_id.clone()]);

    // 事件序：FileLoaded → ParseCompleted → QueryReady
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            ab_pipeline::PipelineEvent::FileLoaded { .. } => "FileLoaded",
            ab_pipeline::PipelineEvent::ParseCompleted { .. } => "ParseCompleted",
            ab_pipeline::PipelineEvent::QueryReady { .. } => "QueryReady",
            other => {
                let _ = other;
                "other"
            }
        })
        .collect();
    assert_eq!(kinds, vec!["FileLoaded", "ParseCompleted", "QueryReady"]);
}

#[tokio::test]
async fn reopen_marks_missing_files_and_skips_them() {
    let dir = TempDir::new("reopen-missing");
    let good = dir.file("good.log");
    fs::write(&good, "data").unwrap();
    let gone = dir.file("gone.log"); // 不创建

    let mock = MockSession::new(SessionFixture {
        plugin_id: "mock-csv".to_string(),
        schema: None,
        can_handle: None,
        files: HashMap::new(),
    });
    let registry = Arc::new(SessionRegistry::new());
    registry.register(mock.clone());

    let session = SessionFile {
        version: 1,
        files: vec![
            SessionFileEntry {
                path: good.to_string_lossy().to_string(),
                sha256: sha256_of_file(&good).unwrap(),
                plugin_id: "mock-csv".to_string(),
            },
            SessionFileEntry {
                path: gone.to_string_lossy().to_string(),
                sha256: "0".repeat(64),
                plugin_id: "mock-csv".to_string(),
            },
        ],
        selected_metrics: HashMap::new(),
        chart_view_state: ChartViewState {
            time_range: None,
            legend_disabled: vec![],
            y_axis_scale: YAxisScale::Shared,
        },
        cursor_ms: None,
    };

    let store = Arc::new(Store::new());
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let outcomes = reopen_files(store, registry, &session, &events_tx).await;

    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].verify, FileVerifyStatus::Ok);
    assert_eq!(
        outcomes[1].verify,
        FileVerifyStatus::Missing,
        "缺失文件标记 missing"
    );
    assert_eq!(outcomes[1].file_id, None, "缺失文件不进入解析");
    // 缺失不阻塞其余文件：good 正常解析（空脚本 → 0 条）
    assert_eq!(outcomes[0].error, None);
    assert_eq!(mock.stats().can_handle_calls, 0);
    assert_eq!(mock.stats().load_file_calls, 1);
    assert_eq!(mock.stats().parse_calls, 1);
}
