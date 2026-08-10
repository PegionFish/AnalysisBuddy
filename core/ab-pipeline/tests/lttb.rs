//! B-02 LTTB 降采样测试：与 pipeline.md §3.3 伪代码参考实现逐点比对。

use ab_pipeline::downsample;

/// 伪代码级参考实现（pipeline.md §3.3 逐行照抄，含余数摊派）。
fn reference_lttb(ts: &[i64], values: &[f64], m: usize) -> (Vec<i64>, Vec<f64>) {
    let n = ts.len();
    if m >= n || m < 3 {
        return (ts.to_vec(), values.to_vec());
    }
    let mut out_ts = Vec::with_capacity(m);
    let mut out_v = Vec::with_capacity(m);
    let bucket_size = (n - 2) / (m - 2);
    let remainder = (n - 2) % (m - 2);
    let mut bounds = Vec::with_capacity(m - 1);
    let mut cursor = 1usize;
    for i in 0..(m - 2) {
        let start = cursor;
        cursor += bucket_size + if i < remainder { 1 } else { 0 };
        bounds.push((start, cursor));
    }
    out_ts.push(ts[0]);
    out_v.push(values[0]);
    let mut prev_selected = 0usize;
    for i in 0..(m - 2) {
        let (s, e) = bounds[i];
        let (avg_x, avg_y) = if i + 1 < m - 2 {
            let (ns, ne) = bounds[i + 1];
            let avg_x = ts[ns..ne].iter().sum::<i64>() as f64 / (ne - ns) as f64;
            let avg_y = values[ns..ne].iter().sum::<f64>() / (ne - ns) as f64;
            (avg_x, avg_y)
        } else {
            (ts[n - 1] as f64, values[n - 1])
        };
        let ax = ts[prev_selected] as f64;
        let ay = values[prev_selected];
        let mut best_idx = s;
        let mut best_area = -1.0;
        for j in s..e {
            let area = ((ax - avg_x) * (values[j] - ay) - (ax - ts[j] as f64) * (avg_y - ay)).abs();
            if area > best_area {
                best_area = area;
                best_idx = j;
            }
        }
        out_ts.push(ts[best_idx]);
        out_v.push(values[best_idx]);
        prev_selected = best_idx;
    }
    out_ts.push(ts[n - 1]);
    out_v.push(values[n - 1]);
    (out_ts, out_v)
}

/// 确定性的伪随机序列（LCG），避免引入 rand 依赖。
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn i64_ms(&mut self) -> i64 {
        (self.next() % 1_000_000_000) as i64
    }
    fn f64_val(&mut self) -> f64 {
        (self.next() % 1000) as f64 / 10.0
    }
}

#[test]
fn matches_reference_implementation_on_deterministic_cases() {
    // 含余数摊派的用例：n=10, m=5 → bucket_size=2, remainder=2
    let ts = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let values = vec![1.0, 9.0, 2.0, 8.0, 3.0, 7.0, 4.0, 6.0, 5.0, 0.5];
    for m in 3..=12 {
        let got = downsample(&ts, &values, m);
        let want = reference_lttb(&ts, &values, m);
        assert_eq!(got, want, "m={m}");
    }
    // n=13, m=4 → bucket_size=11/2=5, remainder=1
    let ts2 = (0..13).collect::<Vec<i64>>();
    let values2: Vec<f64> = ts2.iter().map(|&t| ((t * 37) % 11) as f64).collect();
    for m in 3..=15 {
        let got = downsample(&ts2, &values2, m);
        let want = reference_lttb(&ts2, &values2, m);
        assert_eq!(got, want, "m={m}");
    }
    // 波动序列（面积区分度）
    let wave_ts = (0..20).map(|i| i * 1000).collect::<Vec<i64>>();
    let wave_v: Vec<f64> = (0..20)
        .map(|i| (i as f64).sin() * 100.0 + (i % 7) as f64)
        .collect();
    for m in 3..=22 {
        let got = downsample(&wave_ts, &wave_v, m);
        let want = reference_lttb(&wave_ts, &wave_v, m);
        assert_eq!(got, want, "m={m}");
    }
}

#[test]
fn matches_reference_implementation_on_random_inputs() {
    let mut rng = Lcg(0x05EE_DB01);
    for _ in 0..200 {
        let n = 3 + (rng.next() % 40) as usize;
        let ts: Vec<i64> = (0..n)
            .map(|i| (i as i64) * 1000 + rng.i64_ms() % 37)
            .collect();
        let values: Vec<f64> = (0..n).map(|_| rng.f64_val()).collect();
        let m = 3 + (rng.next() % (n as u64 + 5)) as usize;
        let got = downsample(&ts, &values, m);
        let want = reference_lttb(&ts, &values, m);
        assert_eq!(got, want, "n={n} m={m}");
    }
}

#[test]
fn first_and_last_points_always_preserved() {
    let ts = (0..50).collect::<Vec<i64>>();
    let values: Vec<f64> = ts.iter().map(|&t| (t % 17) as f64).collect();
    for m in 3..=49 {
        let (out_ts, _) = downsample(&ts, &values, m);
        assert_eq!(out_ts.len(), m);
        assert_eq!(out_ts[0], ts[0]);
        assert_eq!(*out_ts.last().unwrap(), *ts.last().unwrap());
    }
}

#[test]
fn remainder_spread_across_first_buckets() {
    // n=10, m=5：bucket_size=2, remainder=2 → 桶区间 [1,3)[3,5)[5,8)
    let ts = (0..10).collect::<Vec<i64>>();
    let values = vec![1.0, 9.0, 2.0, 8.0, 3.0, 7.0, 4.0, 6.0, 5.0, 0.5];
    let got = downsample(&ts, &values, 5);
    assert_eq!(got, reference_lttb(&ts, &values, 5));
    // 桶数 = m - 2 = 3，首末点加 3 桶各 1 点 = 5 点
    assert_eq!(got.0.len(), 5);
}

#[test]
fn m_less_than_three_returns_input_unchanged() {
    let ts = vec![1, 2, 3, 4];
    let values = vec![1.0, 2.0, 3.0, 4.0];
    for m in [0usize, 1, 2] {
        let (out_ts, out_v) = downsample(&ts, &values, m);
        assert_eq!(out_ts, ts);
        assert_eq!(out_v, values);
    }
}

#[test]
fn m_greater_or_equal_n_returns_input_unchanged() {
    let ts = vec![1, 2, 3, 4, 5];
    let values = vec![5.0, 4.0, 3.0, 2.0, 1.0];
    for m in [5usize, 6, 100] {
        let (out_ts, out_v) = downsample(&ts, &values, m);
        assert_eq!(out_ts, ts);
        assert_eq!(out_v, values);
    }
}

#[test]
fn single_point_and_empty_inputs_do_not_panic() {
    let (out_ts, out_v) = downsample(&[42], &[1.0], 100);
    assert_eq!(out_ts, vec![42]);
    assert_eq!(out_v, vec![1.0]);
    let (out_ts, out_v) = downsample(&[], &[], 100);
    assert!(out_ts.is_empty());
    assert!(out_v.is_empty());
}
