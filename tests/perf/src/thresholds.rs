//! PERF-01..04 硬性门槛常量与判定（qa-perf.md §4.1/§5）：
//! 门槛值冻结，任何调整须走主代理评审（不得私改，见 tests/perf/README.md 分诊流程）。

/// PERF-01：100MB 解析耗时 ≤10s（release、冷插件进程、含 load_file+parse 全程）。
pub const PERF_01_PARSE_SECS: f64 = 10.0;
/// PERF-02：宿主内存峰值 ≤1GB（RSS，解析完成 + 全指标查询驻留时刻）。
pub const PERF_02_RSS_MB: f64 = 1024.0;
/// PERF-03：dataZoom 拖拽帧率 ≥30fps（5s 拖拽窗口 95 分位，LTTB 已触发）。
pub const PERF_03_DRAG_FPS_P95: f64 = 30.0;
/// PERF-04：JSON IPC 吞吐 ≥20MB/s（回传总字节 ÷ 首帧到末批耗时）。
pub const PERF_04_IPC_MBPS: f64 = 20.0;

/// perf-smoke 10MB 档等比折算门槛（qa-perf.md §5：parse ≤1s、RSS ≤300MB）。
pub const SMOKE_10MB_PARSE_SECS: f64 = 1.0;
pub const SMOKE_10MB_RSS_MB: f64 = 300.0;

/// 门槛集合。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub parse_secs: f64,
    pub rss_mb: f64,
    pub drag_fps_p95: f64,
    pub ipc_mbps: f64,
}

impl Thresholds {
    /// 硬性门槛（PERF-01..04，Phase 3 出口标准）。
    pub fn full() -> Self {
        Self {
            parse_secs: PERF_01_PARSE_SECS,
            rss_mb: PERF_02_RSS_MB,
            drag_fps_p95: PERF_03_DRAG_FPS_P95,
            ipc_mbps: PERF_04_IPC_MBPS,
        }
    }

    /// perf-smoke 10MB 等比折算门槛。
    pub fn smoke_10mb() -> Self {
        Self {
            parse_secs: SMOKE_10MB_PARSE_SECS,
            rss_mb: SMOKE_10MB_RSS_MB,
            drag_fps_p95: PERF_03_DRAG_FPS_P95,
            ipc_mbps: PERF_04_IPC_MBPS,
        }
    }
}

/// 逐门槛判定（顺序 = PERF-01..04；未采样（None）判不达标——保守语义）。
pub fn judge(
    parse_secs: Option<f64>,
    rss_mb: Option<f64>,
    ipc_mbps: Option<f64>,
    drag_fps_p95: Option<f64>,
    t: &Thresholds,
) -> [bool; 4] {
    [
        parse_secs.map(|v| v <= t.parse_secs).unwrap_or(false),
        rss_mb.map(|v| v <= t.rss_mb).unwrap_or(false),
        ipc_mbps.map(|v| v >= t.ipc_mbps).unwrap_or(false),
        drag_fps_p95.map(|v| v >= t.drag_fps_p95).unwrap_or(false),
    ]
}

/// 中位数判定（5 次采样纪律）的便捷包装。
pub fn judge_median(
    parse_secs_samples: &[f64],
    rss_mb: Option<f64>,
    ipc_mbps_samples: &[f64],
    drag_fps_p95: Option<f64>,
    t: &Thresholds,
) -> [bool; 4] {
    [
        crate::sampling::median_pass(parse_secs_samples, t.parse_secs, true),
        rss_mb.map(|v| v <= t.rss_mb).unwrap_or(false),
        crate::sampling::median_pass(ipc_mbps_samples, t.ipc_mbps, false),
        drag_fps_p95.map(|v| v >= t.drag_fps_p95).unwrap_or(false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_thresholds_match_doc() {
        let t = Thresholds::full();
        assert_eq!(t.parse_secs, 10.0, "PERF-01");
        assert_eq!(t.rss_mb, 1024.0, "PERF-02");
        assert_eq!(t.drag_fps_p95, 30.0, "PERF-03");
        assert_eq!(t.ipc_mbps, 20.0, "PERF-04");
    }

    #[test]
    fn smoke_scaling_10mb() {
        let t = Thresholds::smoke_10mb();
        assert_eq!(t.parse_secs, 1.0, "10MB 档 parse ≤1s");
        assert_eq!(t.rss_mb, 300.0, "10MB 档 RSS ≤300MB");
        // IPC/帧率门槛不随体积缩放。
        assert_eq!(t.ipc_mbps, PERF_04_IPC_MBPS);
        assert_eq!(t.drag_fps_p95, PERF_03_DRAG_FPS_P95);
    }

    #[test]
    fn judge_all_four_gates() {
        let t = Thresholds::full();
        assert_eq!(
            judge(Some(9.5), Some(900.0), Some(21.0), Some(31.0), &t),
            [true, true, true, true]
        );
        assert_eq!(
            judge(Some(10.1), Some(900.0), Some(21.0), Some(31.0), &t),
            [false, true, true, true],
            "PERF-01 超标"
        );
        // 未采样 → 不达标（保守）。
        assert_eq!(
            judge(None, Some(100.0), Some(50.0), None, &t),
            [false, true, true, false]
        );
    }

    #[test]
    fn judge_uses_median_of_five() {
        let t = Thresholds::smoke_10mb();
        // 5 次 parse 采样中位数 0.9 ≤ 1.0。
        let parse = [1.2, 0.9, 0.8, 0.7, 0.6];
        let ipc = [25.0, 26.0, 27.0, 24.0, 23.0];
        let r = judge_median(&parse, Some(280.0), &ipc, None, &t);
        assert_eq!(r, [true, true, true, false]);
    }
}
