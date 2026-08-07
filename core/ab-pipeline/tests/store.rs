//! B-01 集成测试：SoA 列式存储、乱序入库、抽样旁路、卸载；mock harness 脚本。

use std::collections::{BTreeMap, HashMap};

use ab_pipeline::mock::{FileFixture, MockSession, ParseStep, SessionFixture};
use ab_pipeline::{ParseEvent, PluginSession, Store, StoreError};
use ab_protocol::types::{
    FileSummary, LoadFileParams, ParseParams, ProgressParams, Record, RecordBatch, SchemaResult,
    TimeRange,
};
use tokio::sync::mpsc;

fn record(ts: i64, metric: &str, value: f64) -> Record {
    Record {
        timestamp: ts,
        metric: metric.to_string(),
        value,
        level: None,
        tags: None,
        raw_line: None,
    }
}

fn record_raw(ts: i64, metric: &str, value: f64, raw_line: &str) -> Record {
    Record {
        timestamp: ts,
        metric: metric.to_string(),
        value,
        level: None,
        tags: None,
        raw_line: Some(raw_line.to_string()),
    }
}

fn batch(file_id: &str, seq: u64, records: Vec<Record>) -> RecordBatch {
    RecordBatch {
        file_id: file_id.to_string(),
        seq,
        records,
        done: false,
    }
}

fn default_summary() -> FileSummary {
    FileSummary {
        record_count_hint: None,
        time_range: None,
        note: None,
    }
}

// ---------------------------------------------------------------------------
// mock harness：脚本化 parse_stream（批次 / progress / 错误注入）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mock_parse_stream_plays_script_with_batches_progress_and_total() {
    let fixture = SessionFixture {
        plugin_id: "mock-test".to_string(),
        schema: Some(Ok(SchemaResult { metrics: vec![] })),
        can_handle: None,
        files: HashMap::from([(
            "f.log".to_string(),
            FileFixture {
                load_file: Some(Ok(default_summary())),
                parse_script: vec![
                    ParseStep::Batch(batch(
                        "f1",
                        0,
                        vec![record(1, "m", 1.0), record(2, "m", 2.0)],
                    )),
                    ParseStep::Progress(ProgressParams {
                        file_id: "f1".to_string(),
                        percent: Some(50.0),
                        records_so_far: 2,
                        bytes_read: None,
                    }),
                    ParseStep::Batch(batch("f1", 1, vec![record(3, "m", 3.0)])),
                ],
                parse_result: Some(Ok(3)),
                key_values: None,
            },
        )]),
    };
    let session = MockSession::new(fixture);

    let schema = session.schema().await.unwrap();
    assert!(schema.metrics.is_empty());
    let loaded = session
        .load_file(LoadFileParams {
            file_id: "f1".to_string(),
            path: "f.log".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(loaded, default_summary());

    let (tx, mut rx) = mpsc::channel(16);
    let parse_fut = session.parse_stream(
        ParseParams {
            file_id: "f1".to_string(),
            options: None,
        },
        tx,
    );
    let drain = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(ev) = rx.recv().await {
            seen.push(ev);
        }
        seen
    });
    let total = parse_fut.await.unwrap();
    assert_eq!(total, 3, "parse_stream 应返回脚本 parse_result");
    let seen = drain.await.unwrap();
    let batch_count = seen
        .iter()
        .filter(|e| matches!(e, ParseEvent::Batch(_)))
        .count();
    let progress_count = seen
        .iter()
        .filter(|e| matches!(e, ParseEvent::Progress(_)))
        .count();
    assert_eq!(batch_count, 2);
    assert_eq!(progress_count, 1);

    let stats = session.stats();
    assert_eq!(stats.schema_calls, 1);
    assert_eq!(stats.load_file_calls, 1);
    assert_eq!(stats.parse_calls, 1);
}

#[tokio::test]
async fn mock_parse_stream_injects_error() {
    let fixture = SessionFixture {
        plugin_id: "mock-error".to_string(),
        schema: None,
        can_handle: None,
        files: HashMap::from([(
            "f.log".to_string(),
            FileFixture {
                load_file: None,
                parse_script: vec![
                    ParseStep::Batch(batch("f1", 0, vec![record(1, "m", 1.0)])),
                    ParseStep::Fail(ab_pipeline::SessionError::Plugin {
                        code: -32003,
                        message: "boom".to_string(),
                    }),
                ],
                parse_result: None,
                key_values: None,
            },
        )]),
    };
    let session = MockSession::new(fixture);
    session
        .load_file(LoadFileParams {
            file_id: "f1".to_string(),
            path: "f.log".to_string(),
        })
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::channel(16);
    let result = session
        .parse_stream(
            ParseParams {
                file_id: "f1".to_string(),
                options: None,
            },
            tx,
        )
        .await;
    assert!(matches!(
        result,
        Err(ab_pipeline::SessionError::Plugin { code: -32003, .. })
    ));
    while rx.recv().await.is_some() {}
}

// ---------------------------------------------------------------------------
// register / append_batch 校验
// ---------------------------------------------------------------------------

#[test]
fn register_duplicate_and_unknown_file() {
    let store = Store::new();
    let whitelist = vec!["m".to_string()];
    store.register("f1", None, &whitelist).unwrap();
    assert_eq!(
        store.register("f1", None, &whitelist),
        Err(StoreError::AlreadyRegistered("f1".to_string()))
    );
    assert_eq!(
        store.append_batch("nope", batch("nope", 0, vec![])),
        Err(StoreError::UnknownFile("nope".to_string()))
    );
}

#[test]
fn batch_file_mismatch_is_protocol_error() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    let err = store
        .append_batch("f1", batch("other", 0, vec![]))
        .unwrap_err();
    assert!(matches!(err, StoreError::BatchFileMismatch { .. }));
}

#[test]
fn seq_gap_and_duplicate_are_protocol_errors() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    store
        .append_batch("f1", batch("f1", 0, vec![record(1, "m", 1.0)]))
        .unwrap();
    assert_eq!(
        store.append_batch("f1", batch("f1", 2, vec![])),
        Err(StoreError::SeqGap {
            expected: 1,
            got: 2
        })
    );
    store.append_batch("f1", batch("f1", 1, vec![])).unwrap();
    assert_eq!(
        store.append_batch("f1", batch("f1", 1, vec![])),
        Err(StoreError::SeqDuplicate {
            expected: 2,
            got: 1
        })
    );
}

#[test]
fn undeclared_metric_records_dropped_and_counted() {
    let store = Store::new();
    store
        .register("f1", None, &["m".to_string(), "n".to_string()])
        .unwrap();
    let stats = store
        .append_batch(
            "f1",
            batch(
                "f1",
                0,
                vec![
                    record(1, "m", 1.0),
                    record(2, "ghost", 2.0),
                    record(3, "n", 3.0),
                ],
            ),
        )
        .unwrap();
    assert_eq!(stats.appended, 2);
    assert_eq!(stats.dropped_undeclared, 1);
    store.freeze("f1", 3).unwrap();
    assert_eq!(store.warnings("f1").unwrap().dropped_undeclared, 1);
}

// ---------------------------------------------------------------------------
// 乱序定稿：freeze 配对稳定排序
// ---------------------------------------------------------------------------

#[test]
fn out_of_order_batches_freeze_to_sorted_stable_series() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    // 批间乱序：b0 先到 [30, 10]，b1 到 [10, 20]，b2 到 [10]
    store
        .append_batch(
            "f1",
            batch("f1", 0, vec![record(30, "m", 1.0), record(10, "m", 2.0)]),
        )
        .unwrap();
    store
        .append_batch(
            "f1",
            batch("f1", 1, vec![record(10, "m", 3.0), record(20, "m", 4.0)]),
        )
        .unwrap();
    store
        .append_batch("f1", batch("f1", 2, vec![record(10, "m", 5.0)]))
        .unwrap();
    store.freeze("f1", 5).unwrap();

    // 冻结后严格非降序；同 timestamp 点保持产出顺序（稳定排序）
    let series = store.frozen_series("f1", "m").unwrap();
    assert_eq!(series.ts, vec![10, 10, 10, 20, 30]);
    assert_eq!(series.values, vec![2.0, 3.0, 5.0, 4.0, 1.0]);
}

#[test]
fn freeze_checks_records_total_against_sum_of_batches() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    store
        .append_batch(
            "f1",
            batch("f1", 0, vec![record(1, "m", 1.0), record(2, "m", 2.0)]),
        )
        .unwrap();
    assert_eq!(
        store.freeze("f1", 3),
        Err(StoreError::CountMismatch {
            declared: 3,
            received: 2
        })
    );
    assert_eq!(
        store.freeze("f1", 4),
        Err(StoreError::CountMismatch {
            declared: 4,
            received: 2
        })
    );
}

#[test]
fn freeze_rejects_already_frozen_and_append_after_freeze() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    store
        .append_batch("f1", batch("f1", 0, vec![record(1, "m", 1.0)]))
        .unwrap();
    store.freeze("f1", 1).unwrap();
    assert_eq!(
        store.freeze("f1", 1),
        Err(StoreError::AlreadyFrozen("f1".to_string()))
    );
    assert_eq!(
        store.append_batch("f1", batch("f1", 1, vec![record(2, "m", 2.0)])),
        Err(StoreError::NotIngesting("f1".to_string()))
    );
}

// ---------------------------------------------------------------------------
// raw_line 抽样（≤1%，stride = 100）
// ---------------------------------------------------------------------------

#[test]
fn raw_line_sampled_at_1pct_with_stride_100() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    let total = 10_000u64;
    for seq in 0..10u64 {
        let records: Vec<Record> = (0..1000)
            .map(|i| {
                let idx = seq * 1000 + i;
                record_raw(idx as i64, "m", idx as f64, &format!("line {idx}"))
            })
            .collect();
        store.append_batch("f1", batch("f1", seq, records)).unwrap();
    }
    store.freeze("f1", total).unwrap();
    let side = store.side_table("f1", "m").unwrap();
    assert_eq!(
        side.raw_line.len(),
        100,
        "10_000 条 raw_line 应恰保留 100 条"
    );
    // 抽样下标：counter % 100 == 0 → 全局第 99、199、... 条（0 基）
    let mut kept: Vec<u32> = side.raw_line.keys().copied().collect();
    kept.sort_unstable();
    assert_eq!(kept.len(), 100);
    assert_eq!(kept[0], 99);
    assert_eq!(kept[99], 9999);
}

#[test]
fn raw_line_sampling_keeps_nothing_below_stride() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    let records: Vec<Record> = (0..50)
        .map(|i| record_raw(i, "m", i as f64, &format!("line {i}")))
        .collect();
    store.append_batch("f1", batch("f1", 0, records)).unwrap();
    // 50 条 < stride=100：一条都不保留，旁路表不存在
    store.freeze("f1", 50).unwrap();
    assert!(store.side_table("f1", "m").is_none());
}

// ---------------------------------------------------------------------------
// tags 上限（单文件 100,000 条，超限丢弃并计数）
// ---------------------------------------------------------------------------

#[test]
fn tags_capped_at_100k_then_dropped_and_counted() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    let tags_per_record = 5u64;
    let record_count = 20_001u64;
    for seq in 0..41u64 {
        let start = seq * 500;
        let end = (start + 500).min(record_count);
        let records: Vec<Record> = (start..end)
            .map(|i| {
                let mut tags = BTreeMap::new();
                for t in 0..tags_per_record {
                    tags.insert(format!("k{t}"), format!("v{i}"));
                }
                Record {
                    timestamp: i as i64,
                    metric: "m".to_string(),
                    value: i as f64,
                    level: None,
                    tags: Some(tags),
                    raw_line: None,
                }
            })
            .collect();
        if records.is_empty() {
            continue;
        }
        store.append_batch("f1", batch("f1", seq, records)).unwrap();
    }
    store.freeze("f1", record_count).unwrap();
    let side = store.side_table("f1", "m").unwrap();
    let kept: u64 = side.tags.values().map(|t| t.len() as u64).sum();
    assert_eq!(kept, 100_000, "恰达上限");
    assert_eq!(store.warnings("f1").unwrap().dropped_tags, 5);
    assert_eq!(side.tags.len() as u64, 20_000);
}

// ---------------------------------------------------------------------------
// 卸载即 drop + 冻结后只读查询面
// ---------------------------------------------------------------------------

#[test]
fn unload_drops_all_file_data_immediately() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    let records: Vec<Record> = (0..100)
        .map(|i| record_raw(i, "m", i as f64, &format!("line {i}")))
        .collect();
    store.append_batch("f1", batch("f1", 0, records)).unwrap();
    store.freeze("f1", 100).unwrap();
    assert_eq!(store.metrics_of("f1"), vec!["m".to_string()]);
    assert!(store.side_table("f1", "m").is_some());
    assert!(store.time_range("f1").is_some());

    store.unload("f1");
    assert!(store.metrics_of("f1").is_empty());
    assert!(store.side_table("f1", "m").is_none());
    assert!(store.time_range("f1").is_none());
    assert!(store.warnings("f1").is_none());
    // 幂等
    store.unload("f1");
}

#[test]
fn time_range_frozen_from_data_and_unfrozen_from_summary() {
    // 未冻结：回退 summary 预估
    let summary = FileSummary {
        record_count_hint: None,
        time_range: Some(TimeRange {
            start_ms: 5,
            end_ms: 50,
        }),
        note: None,
    };
    let store = Store::new();
    store
        .register("f1", Some(summary.clone()), &["m".to_string()])
        .unwrap();
    assert_eq!(
        store.time_range("f1"),
        Some(TimeRange {
            start_ms: 5,
            end_ms: 50
        })
    );
    store
        .append_batch(
            "f1",
            batch("f1", 0, vec![record(10, "m", 1.0), record(30, "m", 2.0)]),
        )
        .unwrap();
    store.freeze("f1", 2).unwrap();
    assert_eq!(
        store.time_range("f1"),
        Some(TimeRange {
            start_ms: 10,
            end_ms: 30
        })
    );
    store.unload("f1");
    assert!(store.time_range("f1").is_none());
}

#[test]
fn metrics_of_is_deterministic_sorted() {
    let store = Store::new();
    store
        .register("f1", None, &["b".to_string(), "a".to_string()])
        .unwrap();
    store
        .append_batch(
            "f1",
            batch(
                "f1",
                0,
                vec![
                    record(1, "b", 1.0),
                    record(1, "a", 2.0),
                    record(1, "b", 3.0),
                ],
            ),
        )
        .unwrap();
    store.freeze("f1", 3).unwrap();
    assert_eq!(
        store.metrics_of("f1"),
        vec!["a".to_string(), "b".to_string()]
    );
}
