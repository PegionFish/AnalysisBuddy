//! LTTB（Largest-Triangle-Three-Buckets）降采样（pipeline.md §3.3）。
//!
//! 输入须已按 `ts` 非降序；`m < 3` 或 `m >= n` 时原样返回。复杂度 **O(n)**：
//! 每个点仅被遍历常数次（一次桶划分 + 一次候选比较 + 一次均值累计）。
//! 实现照抄 pipeline.md §3.3 伪代码（约 50 行），测试侧另存逐点比对参考实现。

/// 降采样：从 `n` 点中选出至多 `m` 点，保留首末点，中间均分 `m-2` 个桶，
/// 每桶取与「上一选中点 + 下一桶均值点」构成三角形面积最大者（面积用
/// 叉积绝对值比较，免 `/2`；x 轴 i64 毫秒转 f64 参与计算）。
pub fn downsample(ts: &[i64], values: &[f64], m: usize) -> (Vec<i64>, Vec<f64>) {
    let n = ts.len();
    if m >= n || m < 3 {
        return (ts.to_vec(), values.to_vec());
    }
    let mut out_ts = Vec::with_capacity(m);
    let mut out_v = Vec::with_capacity(m);

    // 桶划分：首点（下标 0）与末点（下标 n-1）强制保留，中间 n-2 点
    // 均分为 m-2 个桶；余数摊给前 r 个桶。
    let bucket_size = (n - 2) / (m - 2);
    let remainder = (n - 2) % (m - 2);
    let mut bounds = Vec::with_capacity(m - 1);
    let mut cursor = 1usize;
    for i in 0..(m - 2) {
        let start = cursor;
        cursor += bucket_size + usize::from(i < remainder);
        bounds.push((start, cursor));
    }

    out_ts.push(ts[0]);
    out_v.push(values[0]);
    let mut prev_selected = 0usize;

    for i in 0..(m - 2) {
        let (s, e) = bounds[i];
        // 下一桶全部点的均值点；最后一个中间桶的「下一桶」即末点。
        let (avg_x, avg_y) = if i + 1 < m - 2 {
            let (ns, ne) = bounds[i + 1];
            let avg_x = ts[ns..ne].iter().sum::<i64>() as f64 / (ne - ns) as f64;
            let avg_y = values[ns..ne].iter().sum::<f64>() / (ne - ns) as f64;
            (avg_x, avg_y)
        } else {
            (ts[n - 1] as f64, values[n - 1])
        };
        // 上一轮选中点
        let ax = ts[prev_selected] as f64;
        let ay = values[prev_selected];
        let mut best_idx = s;
        let mut best_area = -1.0f64;
        for j in s..e {
            // 叉积面积 ×2，免除法
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
