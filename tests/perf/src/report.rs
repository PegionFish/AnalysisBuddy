//! 性能报告 JSON（qa-perf.md §5，schema 冻结）：
//! `{ git_sha, arch, machine, gpu, fixture, metrics: { parse_ms, rss_peak_mb, ipc_mbps,
//!   first_paint_ms, drag_fps_p95 }, thresholds_pass: bool[] }`。
//! 文件名 `perf-report-<date>-<sha>.json`；nightly/tag 报告入仓 `tests/perf/reports/`，
//! PR 档仅作 artifact（CI 断言见 tests/scripts/perf-*.yml 与本模块单测）。

use serde::{Deserialize, Serialize};

/// 五项指标（未采样为 None）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Metrics {
    /// ① parse 耗时（ms）：load_file 发出 → records_total 到达，5 次取中位数。
    pub parse_ms: Option<f64>,
    /// ② RSS 峰值（MB）。
    pub rss_peak_mb: Option<f64>,
    /// ③ IPC 吞吐（MB/s）：回传总字节 ÷ 首帧到末批耗时。
    pub ipc_mbps: Option<f64>,
    /// ④ 首屏出图（ms）：导入完成事件 → 首个 series 渲染帧。
    pub first_paint_ms: Option<f64>,
    /// ⑤ 拖拽帧率 95 分位（fps）。
    pub drag_fps_p95: Option<f64>,
}

/// 报告（字段名冻结，勿增删；machine/gpu 为 qa-perf.md §4.3/§5 预留字段）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerfReport {
    pub git_sha: String,
    pub arch: String,
    pub machine: String,
    pub gpu: Option<String>,
    pub fixture: String,
    pub metrics: Metrics,
    /// 阈值判定逐项（bool[4]，下标顺序冻结：parse、rss、ipc、fps →
    /// PERF-01/02/04/03，见 qa-perf.md §4.1）。
    pub thresholds_pass: Vec<bool>,
}

/// 冻结字段清单（§5 schema）。
pub const FROZEN_FIELDS: [&str; 6] = [
    "git_sha",
    "arch",
    "metrics",
    "fixture",
    "thresholds_pass",
    "machine",
];
/// 冻结指标字段清单。
pub const FROZEN_METRIC_FIELDS: [&str; 5] = [
    "parse_ms",
    "rss_peak_mb",
    "ipc_mbps",
    "first_paint_ms",
    "drag_fps_p95",
];

/// thresholds_pass 下标 → qa-perf.md §4.1 PERF 编号：下标顺序 [parse, rss, ipc, fps]
/// = PERF-01/02/04/03（IPC 在下标 2 = PERF-04；fps 在下标 3 = PERF-03）。
pub const PERF_ID_BY_INDEX: [usize; 4] = [1, 2, 4, 3];

/// perf-smoke 门禁判定（qa-perf.md §5；perf-smoke.yml Gate step 的 Rust 对偶）：
/// 仅对已测量的门槛判定——未测量（如 PERF-03 探针不可用、gpu=null）跳过不判；
/// 返回未通过门槛的 PERF 编号（1..=4，按 PERF_ID_BY_INDEX 映射），空 Vec = 门禁通过。
pub fn gate_failures(r: &PerfReport) -> Vec<usize> {
    let m = &r.metrics;
    let measured = [
        m.parse_ms.is_some(),
        m.rss_peak_mb.is_some(),
        m.ipc_mbps.is_some(),
        m.drag_fps_p95.is_some(),
    ];
    r.thresholds_pass
        .iter()
        .enumerate()
        .filter(|&(i, &pass)| measured[i] && !pass)
        .map(|(i, _)| PERF_ID_BY_INDEX[i])
        .collect()
}

/// 文件名：`perf-report-<date>-<sha>.json`（UTC 日期 + 短 sha）。
pub fn filename(sha: &str, now_utc_ms: i64) -> String {
    let days = now_utc_ms.div_euclid(86_400_000);
    let (y, mo, d) = civil_from_days(days);
    let short = sha.get(..12).unwrap_or(sha);
    format!("perf-report-{y:04}-{mo:02}-{d:02}-{short}.json")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// 回归检测（qa-perf.md §5）：任一指标劣化 >15% → Some(issue 正文，附两次 JSON diff)。
pub fn regression_check(baseline: &PerfReport, current: &PerfReport) -> Option<String> {
    let b = &baseline.metrics;
    let c = &current.metrics;
    let mut rows: Vec<String> = Vec::new();
    let worse = |b: Option<f64>, c: Option<f64>, lower_is_better: bool| -> Option<(f64, f64)> {
        match (b, c) {
            (Some(bv), Some(cv)) => {
                let ratio = (cv - bv).abs() / bv.abs().max(1e-9);
                let degraded = if lower_is_better {
                    cv > bv * 1.15
                } else {
                    cv < bv * 0.85
                };
                if degraded {
                    Some((bv, cv))
                } else {
                    let _ = ratio;
                    None
                }
            }
            _ => None,
        }
    };
    let checks = [
        ("parse_ms", worse(b.parse_ms, c.parse_ms, true), true),
        (
            "rss_peak_mb",
            worse(b.rss_peak_mb, c.rss_peak_mb, true),
            true,
        ),
        (
            "first_paint_ms",
            worse(b.first_paint_ms, c.first_paint_ms, true),
            true,
        ),
        ("ipc_mbps", worse(b.ipc_mbps, c.ipc_mbps, false), false),
        (
            "drag_fps_p95",
            worse(b.drag_fps_p95, c.drag_fps_p95, false),
            false,
        ),
    ];
    for (name, pair, _) in checks {
        if let Some((bv, cv)) = pair {
            rows.push(format!(
                "| {name} | {bv:.2} | {cv:.2} | 劣化 {:.1}% |",
                (cv - bv).abs() / bv.abs().max(1e-9) * 100.0
            ));
        }
    }
    if rows.is_empty() {
        return None;
    }
    let base_json = serde_json::to_string_pretty(baseline).unwrap();
    let cur_json = serde_json::to_string_pretty(current).unwrap();
    Some(format!(
        "## 性能回归检测（>15% 门槛）\n\n对比基线 `{}`（fixture `{}`）\n\n| 指标 | 基线 | 当前 | 变化 |\n|---|---|---|---|\n{}\n\n### JSON diff（片段）\n\n基线：\n```json\n{}\n```\n\n当前：\n```json\n{}\n```",
        baseline.git_sha, baseline.fixture, rows.join("\n"), base_json, cur_json
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report(parse_ms: f64) -> PerfReport {
        PerfReport {
            git_sha: "abc123def456".to_string(),
            arch: "x86_64".to_string(),
            machine: "test-machine".to_string(),
            gpu: None,
            fixture: "bench_100mb.csv".to_string(),
            metrics: Metrics {
                parse_ms: Some(parse_ms),
                rss_peak_mb: Some(512.0),
                ipc_mbps: Some(60.0),
                first_paint_ms: Some(300.0),
                drag_fps_p95: Some(45.0),
            },
            thresholds_pass: vec![true, true, true, true],
        }
    }

    #[test]
    fn serialized_fields_match_frozen_schema() {
        let r = sample_report(8.0);
        let json = serde_json::to_value(&r).unwrap();
        let obj = json.as_object().unwrap();
        for f in FROZEN_FIELDS {
            assert!(obj.contains_key(f), "缺失冻结字段 {f}");
        }
        let m = obj["metrics"].as_object().unwrap();
        for f in FROZEN_METRIC_FIELDS {
            assert!(m.contains_key(f), "缺失冻结指标字段 {f}");
        }
        assert_eq!(r.thresholds_pass.len(), 4, "thresholds_pass 长度 4");
    }

    #[test]
    fn filename_format_matches_contract() {
        // 2026-08-01T00:00:00Z = 1785542400000。
        let name = filename("0123456789abcdef0123456789abcdef", 1_785_542_400_000);
        assert_eq!(name, "perf-report-2026-08-01-0123456789ab.json");
    }

    #[test]
    fn regression_detects_15pct_degradation() {
        let base = sample_report(100.0);
        let cur = sample_report(120.0); // +20% → 触发
        let issue = regression_check(&base, &cur).expect("必须触发回归");
        assert!(issue.contains("parse_ms"), "issue 正文含指标名");
        assert!(issue.contains("20.0%"), "issue 正文含劣化百分比");
        assert!(issue.contains("```json"), "issue 正文附两次 JSON diff");

        let ok = sample_report(110.0); // +10% < 15% → 不触发
        assert!(regression_check(&base, &ok).is_none());
    }

    #[test]
    fn regression_throughput_direction() {
        let base = sample_report(8.0);
        let mut cur = sample_report(8.0);
        // IPC 吞吐劣化：60 → 40（<85%）。
        cur.metrics.ipc_mbps = Some(40.0);
        let issue = regression_check(&base, &cur).expect("IPC 劣化必须触发");
        assert!(issue.contains("ipc_mbps"));
    }
}
