//! B-02 查询 API 测试：双二分时间窗、预算化 LTTB、非 Frozen 跳过。

use ab_pipeline::{
    run_query, MetricRef, QueryRequest, SeriesSlice, Store, DEFAULT_MAX_POINTS_PER_SERIES,
};
use ab_protocol::types::{Record, RecordBatch};

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

fn batch(file_id: &str, seq: u64, records: Vec<Record>) -> RecordBatch {
    RecordBatch {
        file_id: file_id.to_string(),
        seq,
        records,
        done: false,
    }
}

/// 跨批乱序灌入 5 个点（10..50），freeze 后 ts 严格非降序。
fn frozen_store() -> Store {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    store
        .append_batch(
            "f1",
            batch("f1", 0, vec![record(50, "m", 5.0), record(10, "m", 1.0)]),
        )
        .unwrap();
    store
        .append_batch(
            "f1",
            batch("f1", 1, vec![record(30, "m", 3.0), record(20, "m", 2.0)]),
        )
        .unwrap();
    store
        .append_batch("f1", batch("f1", 2, vec![record(40, "m", 4.0)]))
        .unwrap();
    store.freeze("f1", 5).unwrap();
    store
}

fn req(metrics: Vec<MetricRef>, t0: i64, t1: i64, budget: usize) -> QueryRequest {
    QueryRequest {
        metrics,
        t0_ms: t0,
        t1_ms: t1,
        max_points_per_series: budget,
    }
}

fn one(file_id: &str, metric: &str) -> Vec<MetricRef> {
    vec![MetricRef {
        file_id: file_id.to_string(),
        metric: metric.to_string(),
    }]
}

#[test]
fn window_crosses_batches_and_boundaries_are_inclusive() {
    let store = frozen_store();
    // 闭区间 [10, 50]：全部命中（跨 3 批），且边界点包含
    let slices = run_query(&store, &req(one("f1", "m"), 10, 50, 1000));
    assert_eq!(slices.len(), 1);
    assert_eq!(slices[0].ts, vec![10, 20, 30, 40, 50]);
    assert!(!slices[0].downsampled);
    // 边界外 1ms 排除
    let slices = run_query(&store, &req(one("f1", "m"), 11, 49, 1000));
    assert_eq!(slices[0].ts, vec![20, 30, 40]);
    // 半开区间左边界 t=10 含、右边界 t=40 含
    let slices = run_query(&store, &req(one("f1", "m"), 10, 40, 1000));
    assert_eq!(slices[0].ts, vec![10, 20, 30, 40]);
}

#[test]
fn empty_window_yields_no_slice() {
    let store = frozen_store();
    let slices = run_query(&store, &req(one("f1", "m"), 60, 100, 1000));
    assert!(slices.is_empty(), "lo == hi 空窗口不出序列");
    // t0 > t1 同样为空
    let slices = run_query(&store, &req(one("f1", "m"), 50, 10, 1000));
    assert!(slices.is_empty());
}

#[test]
fn no_downsample_when_within_budget() {
    let store = frozen_store();
    let slices = run_query(&store, &req(one("f1", "m"), 0, 1000, 5));
    assert_eq!(slices.len(), 1);
    assert_eq!(slices[0].ts.len(), 5);
    assert!(!slices[0].downsampled);
    assert_eq!(slices[0].ts, vec![10, 20, 30, 40, 50]);
}

#[test]
fn downsample_when_over_budget() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    for seq in 0..20u64 {
        let records: Vec<Record> = (0..1000)
            .map(|i| record((seq * 1000 + i) as i64, "m", ((i * 7) % 100) as f64))
            .collect();
        store.append_batch("f1", batch("f1", seq, records)).unwrap();
    }
    store.freeze("f1", 20_000).unwrap();
    let slices = run_query(&store, &req(one("f1", "m"), 0, 2_000_000, 1000));
    assert_eq!(slices.len(), 1);
    assert!(slices[0].downsampled);
    assert_eq!(slices[0].ts.len(), 1000);
    // 首末点保留
    assert_eq!(slices[0].ts[0], 0);
    assert_eq!(*slices[0].ts.last().unwrap(), 19_999);
}

#[test]
fn frontend_budget_path_4000_of_20000() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    for seq in 0..20u64 {
        let records: Vec<Record> = (0..1000)
            .map(|i| record((seq * 1000 + i) as i64, "m", i as f64))
            .collect();
        store.append_batch("f1", batch("f1", seq, records)).unwrap();
    }
    store.freeze("f1", 20_000).unwrap();
    // 前端固定传 4000（ipc-ui.md §5.2）
    let slices = run_query(&store, &req(one("f1", "m"), 0, 2_000_000, 4000));
    assert_eq!(slices[0].ts.len(), 4000);
    assert!(slices[0].downsampled);
    assert_eq!(slices[0].values.len(), 4000);
}

#[test]
fn zero_budget_uses_default_50000() {
    let store = Store::new();
    store.register("f1", None, &["m".to_string()]).unwrap();
    // 2 万点 < 默认 5 万 → 原样返回
    for seq in 0..20u64 {
        let records: Vec<Record> = (0..1000)
            .map(|i| record((seq * 1000 + i) as i64, "m", i as f64))
            .collect();
        store.append_batch("f1", batch("f1", seq, records)).unwrap();
    }
    store.freeze("f1", 20_000).unwrap();
    let slices = run_query(&store, &req(one("f1", "m"), 0, 2_000_000, 0));
    assert_eq!(slices[0].ts.len(), 20_000);
    assert!(!slices[0].downsampled);

    // 6 万点 > 默认 5 万 → LTTB 到 50_000
    let store2 = Store::new();
    store2.register("f2", None, &["m".to_string()]).unwrap();
    for seq in 0..60u64 {
        let records: Vec<Record> = (0..1000)
            .map(|i| record((seq * 1000 + i) as i64, "m", i as f64))
            .collect();
        store2
            .append_batch("f2", batch("f2", seq, records))
            .unwrap();
    }
    store2.freeze("f2", 60_000).unwrap();
    let slices = run_query(&store2, &req(one("f2", "m"), 0, 2_000_000, 0));
    assert_eq!(slices[0].ts.len(), DEFAULT_MAX_POINTS_PER_SERIES);
    assert!(slices[0].downsampled);
}

#[test]
fn non_frozen_and_unregistered_files_are_skipped() {
    let store = Store::new();
    // f1 已注册但未 parse（Registered/Ingesting 态）
    store.register("f1", None, &["m".to_string()]).unwrap();
    store
        .append_batch("f1", batch("f1", 0, vec![record(1, "m", 1.0)]))
        .unwrap();
    // f2 冻结有数据
    store.register("f2", None, &["m".to_string()]).unwrap();
    store
        .append_batch("f2", batch("f2", 0, vec![record(1, "m", 1.0)]))
        .unwrap();
    store.freeze("f2", 1).unwrap();
    let slices = run_query(
        &store,
        &req(
            vec![
                MetricRef {
                    file_id: "f1".to_string(),
                    metric: "m".to_string(),
                },
                MetricRef {
                    file_id: "f2".to_string(),
                    metric: "m".to_string(),
                },
                MetricRef {
                    file_id: "ghost".to_string(),
                    metric: "m".to_string(),
                },
                MetricRef {
                    file_id: "f2".to_string(),
                    metric: "nope".to_string(),
                },
            ],
            0,
            1000,
            100,
        ),
    );
    assert_eq!(slices.len(), 1, "仅 Frozen 且白名单内 metric 出序列");
    assert_eq!(slices[0].file_id, "f2");
    assert_eq!(slices[0].ts, vec![1]);
}

#[test]
fn multi_metric_and_multi_file_request() {
    let store = Store::new();
    store
        .register("f1", None, &["a".to_string(), "b".to_string()])
        .unwrap();
    store
        .append_batch(
            "f1",
            batch(
                "f1",
                0,
                vec![
                    record(1, "a", 1.0),
                    record(2, "a", 2.0),
                    record(3, "b", 3.0),
                ],
            ),
        )
        .unwrap();
    store.freeze("f1", 3).unwrap();
    store.register("f2", None, &["c".to_string()]).unwrap();
    store
        .append_batch("f2", batch("f2", 0, vec![record(5, "c", 5.0)]))
        .unwrap();
    store.freeze("f2", 1).unwrap();

    let metrics = vec![
        MetricRef {
            file_id: "f1".to_string(),
            metric: "b".to_string(),
        },
        MetricRef {
            file_id: "f2".to_string(),
            metric: "c".to_string(),
        },
        MetricRef {
            file_id: "f1".to_string(),
            metric: "a".to_string(),
        },
    ];
    let slices = run_query(&store, &req(metrics, 0, 100, 100));
    // 按请求顺序输出
    let keys: Vec<(&str, &str)> = slices
        .iter()
        .map(|s| (s.file_id.as_str(), s.metric.as_str()))
        .collect();
    assert_eq!(keys, vec![("f1", "b"), ("f2", "c"), ("f1", "a")]);
}

#[test]
fn store_query_delegates_to_run_query() {
    let store = frozen_store();
    let q = req(one("f1", "m"), 10, 50, 1000);
    assert_eq!(store.query(&q), run_query(&store, &q));
    assert_eq!(
        store.query(&q),
        vec![SeriesSlice {
            file_id: "f1".to_string(),
            metric: "m".to_string(),
            ts: vec![10, 20, 30, 40, 50],
            values: vec![1.0, 2.0, 3.0, 4.0, 5.0],
            downsampled: false,
        }]
    );
}
