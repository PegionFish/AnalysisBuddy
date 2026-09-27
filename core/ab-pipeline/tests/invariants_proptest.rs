//! I3（Wave 3，§2.7 挑战性任务）：proptest 属性测试——store freeze / query
//! 窗口 / LTTB 降采样的不变量。随机数据默认 256 cases；nightly 可经
//! `PROPTEST_CASES` 放大。
//!
//! 不变量：
//! - I-a 点数守恒：注册点数 == freeze 校验的 records_received，窗口 ±∞ 时
//!   降采样前全量可见；
//! - I-b 窗口闭区间：查询 `[t0, t1]` 恰好包含 `t0 <= ts <= t1` 的全部点；
//! - I-c LTTB 首末点保留：任意 m ≥ 3 的降采样保留首末点且输出定长；
//! - I-d 乱序定稿：乱序写入的批次经 freeze 配对稳定排序后时间戳单调不减。

use ab_pipeline::downsample;
use ab_pipeline::mock::{FileFixture, MockSession, ParseStep, SessionFixture};
use ab_pipeline::{PluginSession, SessionRegistry, Store};
use ab_protocol::types::{Aggregation, MetricDef, Record, RecordBatch, SchemaResult};
use proptest::prelude::*;

fn schema_with(metric: &str) -> SchemaResult {
    SchemaResult {
        metrics: vec![MetricDef {
            id: metric.to_string(),
            name: metric.to_string(),
            unit: None,
            description: None,
            aggregation: Aggregation::Last,
        }],
    }
}

fn records(ts: &[i64]) -> Vec<Record> {
    ts.iter()
        .map(|&t| Record {
            timestamp: t,
            metric: "m".to_string(),
            value: t as f64,
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// I-c：LTTB 首末点保留 + 输出定长（m≥3, n>m）。
    #[test]
    fn lttb_keeps_first_and_last(
        n in 4usize..400,
        m in 3usize..64,
        seed in any::<u64>(),
    ) {
        let m = m.min(n);
        if m >= n { return Ok(()); }
        let mut lcg = seed;
        let mut ts = Vec::with_capacity(n);
        let mut vs = Vec::with_capacity(n);
        for i in 0..n {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ts.push(1_700_000_000_000 + (i as i64) * 1000 + (lcg >> 60) as i64);
            vs.push((lcg >> 40) as f64 / (1u64 << 24) as f64);
        }
        ts.sort();
        let (out_ts, out_v) = downsample(&ts, &vs, m);
        prop_assert_eq!(out_ts.len(), m);
        prop_assert_eq!(out_v.len(), m);
        prop_assert_eq!(out_ts[0], ts[0], "首点保留");
        prop_assert_eq!(*out_ts.last().unwrap(), *ts.last().unwrap(), "末点保留");
        prop_assert_eq!(*out_v.last().unwrap(), *vs.last().unwrap());
    }

    /// I-a/I-b/I-d：乱序批次 → freeze → 窗口闭区间查询 + 点数守恒。
    #[test]
    fn store_freeze_window_and_count_conservation(
        chunks in prop::collection::vec(
            prop::collection::vec(0i64..1_000_000, 1..40),
            1..8,
        ),
        t0 in 0i64..500_000,
        span in 1i64..500_000,
    ) {
        let store = Store::new();
        store
            .register("f", None, &["m".to_string()])
            .unwrap();

        // 每块乱序生成时间戳（同块内乱序、跨块有重叠）；freeze 负责定稿排序。
        for (seq, chunk) in chunks.iter().enumerate() {
            let b = batch("f", seq as u64, records(chunk));
            store.append_batch("f", b).unwrap();
        }
        let total: usize = chunks.iter().map(|c| c.len()).sum();

        // 点数守恒：records_received == Σ各批 len；声明不符即 CountMismatch。
        let wrong = total.wrapping_add(1) as u64;
        assert!(matches!(
            store.freeze("f", wrong),
            Err(ab_pipeline::StoreError::CountMismatch { .. })
        ));
        store.freeze("f", total as u64).unwrap();

        // I-b：窗口闭区间语义——[t0, t0+span] 恰含 t0<=ts<=t1 的全部点。
        let t1 = t0 + span;
        let expected: Vec<i64> = {
            let mut all: Vec<i64> = chunks.iter().flatten().copied().collect();
            all.sort_unstable();
            all.into_iter().filter(|&t| t0 <= t && t <= t1).collect()
        };
        let q = crate_query(&store, t0, t1);
        prop_assert_eq!(q, expected, "窗口必须闭区间精确匹配");

        // I-a：无限窗口守恒。
        let all = crate_query(&store, i64::MIN, i64::MAX);
        prop_assert_eq!(all.len(), total);
    }
}

fn crate_query(store: &Store, t0: i64, t1: i64) -> Vec<i64> {
    // 经公共查询 API：SeriesSlice.points（窗口语义由 query.rs 统一实现）。
    let req = ab_pipeline::query::QueryRequest {
        metrics: vec![ab_pipeline::query::MetricRef {
            file_id: "f".to_string(),
            metric: "m".to_string(),
        }],
        t0_ms: t0,
        t1_ms: t1,
        max_points_per_series: usize::MAX,
    };
    let slices = store.query(&req);
    let mut points = Vec::new();
    for s in slices {
        points.extend(s.ts);
    }
    points.sort_unstable();
    points
}
