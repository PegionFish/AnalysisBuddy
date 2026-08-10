//! 统计工具（qa-perf.md §4.2 采样纪律：每门槛连续 5 次取中位数；95 分位抗毛刺）。

/// 中位数（就地排序；空输入返回 NaN）。
pub fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// 95 分位（就地排序）。
pub fn p95(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((values.len() as f64) * 0.95).floor() as usize;
    values[idx.min(values.len() - 1)]
}

/// 均值。
pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// 中位数判定（5 次采样纪律）：`limit` 为上界时（耗时/内存）取中位数 ≤ limit；
/// `upper_limit=true` 表示"越小越好"，`false` 表示"越大越好"（吞吐）。
pub fn median_pass(samples: &[f64], limit: f64, upper_limit: bool) -> bool {
    if samples.is_empty() {
        return false;
    }
    let mut v = samples.to_vec();
    let m = median(&mut v);
    if m.is_nan() {
        return false;
    }
    if upper_limit {
        m <= limit
    } else {
        m >= limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), 2.5);
    }

    #[test]
    fn p95_ignores_transient_spikes() {
        let mut v = vec![30.0, 31.0, 32.0, 33.0, 30.5, 31.5, 32.5, 29.0, 60.0, 100.0];
        let p = p95(&mut v);
        assert!((32.5..=100.0).contains(&p), "p95={p} 应抗住单点毛刺");
    }

    #[test]
    fn median_rule_five_runs() {
        // 5 次采样中 3 次超标：中位数超标 → 不达标（中位数而非平均判据）。
        let samples = [1.2, 0.9, 0.8, 0.7, 0.6];
        assert!(median_pass(&samples, 1.0, true), "中位数 0.9 ≤ 1.0 → 达标");
        let samples2 = [1.2, 1.1, 1.05, 0.9, 0.8];
        assert!(!median_pass(&samples2, 1.0, true), "中位数 1.05 > 1.0 → 不达标");
    }

    #[test]
    fn empty_samples_never_pass() {
        assert!(!median_pass(&[], 1.0, true));
        assert!(!median_pass(&[], 20.0, false));
    }
}
