//! 迷你宿主查询 API（qa-perf.md §3.1）：时间切片、LTTB 降采样、时间排序存储。
//!
//! 宿主内存存储按时间排序（PLAN.md §3.3）；`disorder` 夹具的排序正确性断言
//! 即针对此存储：查询结果时间序列不乱。

use ab_protocol::types::Record;

/// 按 (timestamp, metric) 排序的内存存储。
#[derive(Debug, Default)]
pub struct Store {
    records: Vec<Record>,
    sorted: bool,
}

impl Store {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            sorted: true,
        }
    }

    /// 追加一批记录（parse 回传批次）；插入后标记待排序。
    pub fn insert_batch(&mut self, records: Vec<Record>) {
        if records.is_empty() {
            return;
        }
        self.sorted = false;
        self.records.extend(records);
    }

    fn ensure_sorted(&mut self) {
        if !self.sorted {
            self.records
                .sort_by_key(|r| (r.timestamp, r.metric.clone()));
            self.sorted = true;
        }
    }

    /// 总记录数。
    pub fn count(&self) -> usize {
        self.records.len()
    }

    /// 时间范围（闭区间），空存储返回 None。
    pub fn time_range(&self) -> Option<(i64, i64)> {
        if self.records.is_empty() {
            return None;
        }
        let min = self.records.iter().map(|r| r.timestamp).min().unwrap();
        let max = self.records.iter().map(|r| r.timestamp).max().unwrap();
        Some((min, max))
    }

    /// 时间切片 `[start_ms, end_ms]` 的记录（时间升序）。
    pub fn slice(&mut self, start_ms: i64, end_ms: i64) -> Vec<&Record> {
        self.ensure_sorted();
        self.records
            .iter()
            .filter(|r| r.timestamp >= start_ms && r.timestamp <= end_ms)
            .collect()
    }

    /// 指定指标在时间切片内的点数。
    pub fn slice_metric_count(&mut self, start_ms: i64, end_ms: i64, metric: &str) -> usize {
        self.slice(start_ms, end_ms)
            .iter()
            .filter(|r| r.metric == metric)
            .count()
    }

    /// LTTB 降采样（>5 万点触发场景）：对全量 (timestamp, value) 序列抽 `buckets` 点。
    /// 记录数 ≤ buckets 时原样返回（点少无需降采样）。
    pub fn lttb(&mut self, buckets: usize) -> Vec<(i64, f64)> {
        self.ensure_sorted();
        let pts: Vec<(i64, f64)> = self
            .records
            .iter()
            .map(|r| (r.timestamp, r.value))
            .collect();
        lttb(&pts, buckets)
    }
}

/// 标准 LTTB（largest-triangle-three-buckets）降采样，确定性强。
pub fn lttb(points: &[(i64, f64)], buckets: usize) -> Vec<(i64, f64)> {
    let n = points.len();
    if n <= 2 || buckets >= n {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(buckets);
    out.push(points[0]);
    let bucket_size = (n - 2) as f64 / (buckets - 1) as f64;
    let mut a = 0usize;
    for i in 0..buckets - 2 {
        // 当前桶的平均值（作为下一桶候选的中心点）。
        let avg_start = ((i as f64 + 1.0) * bucket_size).floor() as usize;
        let avg_end = ((i as f64 + 2.0) * bucket_size).floor() as usize;
        let avg_end = avg_end.max(avg_start + 1).min(n - 1);
        let (mut ax, mut ay) = (0f64, 0f64);
        for p in &points[avg_start..avg_end] {
            ax += p.0 as f64;
            ay += p.1;
        }
        let len = (avg_end - avg_start) as f64;
        ax /= len;
        ay /= len;

        // 在下一桶内选与 (avg, a) 三角形面积最大的点。
        let range_start = ((i as f64 + 2.0) * bucket_size).floor() as usize;
        let range_end = ((i as f64 + 3.0) * bucket_size).floor() as usize;
        let range_end = range_end.max(range_start + 1).min(n - 1);
        let (px, py) = (points[a].0 as f64, points[a].1);
        let mut best = range_start;
        let mut best_area = -1f64;
        for (j, p) in points.iter().enumerate().take(range_end).skip(range_start) {
            let (bx, by) = (p.0 as f64, p.1);
            let area = ((px - ax) * (by - py) - (px - bx) * (ay - py)).abs();
            if area > best_area {
                best_area = area;
                best = j;
            }
        }
        out.push(points[best]);
        a = best;
    }
    out.push(points[n - 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: i64, metric: &str, v: f64) -> Record {
        Record {
            timestamp: ts,
            metric: metric.to_string(),
            value: v,
            level: None,
            tags: None,
            raw_line: None,
        }
    }

    #[test]
    fn slice_returns_sorted_subset() {
        let mut s = Store::new();
        // 故意乱序插入（模拟 disorder 输入）。
        s.insert_batch(vec![rec(300, "fps", 3.0), rec(100, "fps", 1.0), rec(200, "fps", 2.0)]);
        let slice = s.slice(150, 350);
        let ts: Vec<i64> = slice.iter().map(|r| r.timestamp).collect();
        assert_eq!(ts, vec![200, 300], "查询结果时间序列不乱（排序正确性）");
        // 闭区间语义（protocol §2.3 TimeRange）：[100, 300] 含全部三点。
        assert_eq!(s.slice_metric_count(100, 300, "fps"), 3);
    }

    #[test]
    fn lttb_reduces_and_preserves_extremes() {
        let pts: Vec<(i64, f64)> = (0..1000).map(|i| (i, i as f64)).collect();
        let down = lttb(&pts, 100);
        assert_eq!(down.len(), 100);
        assert_eq!(down.first(), Some(&(0, 0.0)));
        assert_eq!(down.last(), Some(&(999, 999.0)));
        // 单调递增数据降采样后仍单调。
        assert!(down.windows(2).all(|w| w[0].1 <= w[1].1));
    }

    #[test]
    fn lttb_short_series_passthrough() {
        let pts = vec![(1, 1.0), (2, 2.0)];
        assert_eq!(lttb(&pts, 5), pts);
    }
}
