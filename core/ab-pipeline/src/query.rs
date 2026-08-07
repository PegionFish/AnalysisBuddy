//! 预算化查询 API（pipeline.md §3）：双二分时间窗定位 + LTTB 降采样。
//!
//! 仅接受 `Frozen` 文件；`Ingesting`/未注册文件不进结果（pipeline.md §3.1）。
//! 点数预算由调用方传入，前端固定传 4000；预算 0 用默认 50_000（§3.3）。

use crate::lttb::downsample;
use crate::store::Store;

/// 查询目标（file_id + metric）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricRef {
    pub file_id: String,
    pub metric: String,
}

/// 查询请求：闭区间 `[t0_ms, t1_ms]`，逐序列预算降采样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryRequest {
    pub metrics: Vec<MetricRef>,
    pub t0_ms: i64,
    pub t1_ms: i64,
    /// 0 → 用 [`DEFAULT_MAX_POINTS_PER_SERIES`]。
    pub max_points_per_series: usize,
}

/// 查询结果切片：窗口内点数超预算时经 LTTB 降采样，`downsampled` 透传前端提示。
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesSlice {
    pub file_id: String,
    pub metric: String,
    pub ts: Vec<i64>,
    pub values: Vec<f64>,
    pub downsampled: bool,
}

/// 未显式传预算时的默认上限（对应 PLAN.md §3.4「>5 万点触发 LTTB」）。
pub const DEFAULT_MAX_POINTS_PER_SERIES: usize = 50_000;

/// 预算化查询（pipeline.md §3.1/§3.2）。
///
/// 逐目标序列：二分定位窗口（`lo = partition_point(t < t0)`，
/// `hi = partition_point(t <= t1)`），窗口为空（`lo == hi`）或文件非
/// `Frozen` 时该序列不进结果；点数超预算走 LTTB。
pub fn run_query(store: &Store, req: &QueryRequest) -> Vec<SeriesSlice> {
    let budget = if req.max_points_per_series == 0 {
        DEFAULT_MAX_POINTS_PER_SERIES
    } else {
        req.max_points_per_series
    };
    let mut out = Vec::new();
    for mref in &req.metrics {
        let Some(series) = store.frozen_series(&mref.file_id, &mref.metric) else {
            continue;
        };
        let lo = series.ts.partition_point(|&t| t < req.t0_ms);
        let hi = series.ts.partition_point(|&t| t <= req.t1_ms);
        // 空窗口（lo == hi，含 t0 > t1 的退化区间）不出序列
        if lo >= hi {
            continue;
        }
        let window_ts = series.ts[lo..hi].to_vec();
        let window_values = series.values[lo..hi].to_vec();
        let n = window_ts.len();
        let downsampled = n > budget;
        let (ts, values) = if downsampled {
            downsample(&window_ts, &window_values, budget)
        } else {
            (window_ts, window_values)
        };
        out.push(SeriesSlice {
            file_id: mref.file_id.clone(),
            metric: mref.metric.clone(),
            ts,
            values,
            downsampled,
        });
    }
    out
}
