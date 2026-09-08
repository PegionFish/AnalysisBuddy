//! AIBench llama-bench 风格 CSV 的领域模型与 schema 定义。
//!
//! 语义：每行 = 一次探针/轮次任务（probe-pp / probe-tg / probe-ppl / soak48h-rNN），
//! 「时间戳」为任务完成时刻的汇总行，不是高频时序采样。45 列中仅部分数值列
//! 「每行都有意义」（avg_ts 在 pp/tg 行有意义、ppl 仅在 ppl 行有意义……），
//! 机器标识列（sn/machine_model/cpu/...）与套件元数据（suite_name/host_version）
//! 为重复常量，产出 metric 只会制造噪声，故白名单挑选。

use std::collections::BTreeMap;

use ab_protocol::types::{Aggregation, MetricDef};

/// 白名单数值列（按 CSV 列序）：(列名, metric id, 单位, 聚合, 双语描述)。
pub const VALUE_COLUMNS: &[ValueCol] = &[
    ValueCol {
        col: "avg_ts",
        id: "avg_ts",
        unit: Some("t/s"),
        agg: Aggregation::Avg,
        desc_zh: "平均生成速度（prompt processing 时为 t/s，为 eval 时为 tok/s）",
        desc_en: "Average token speed (prompt processing t/s, or eval tok/s)",
    },
    ValueCol {
        col: "stddev_ts",
        id: "stddev_ts",
        unit: Some("t/s"),
        agg: Aggregation::Avg,
        desc_zh: "生成速度标准差（多采样任务）",
        desc_en: "Token speed standard deviation (multi-sample tasks)",
    },
    ValueCol {
        col: "ttft_ms_measured",
        id: "ttft_measured",
        unit: Some("ms"),
        agg: Aggregation::Avg,
        desc_zh: "实测首 token 延迟",
        desc_en: "Measured time to first token",
    },
    ValueCol {
        col: "ttft_ms_est",
        id: "ttft_est",
        unit: Some("ms"),
        agg: Aggregation::Avg,
        desc_zh: "估算首 token 延迟（由 prompt processing 速度推算）",
        desc_en: "Estimated time to first token (derived from prompt processing speed)",
    },
    ValueCol {
        col: "vram_peak_mb",
        id: "vram_peak",
        unit: Some("MB"),
        agg: Aggregation::Max,
        desc_zh: "显存峰值占用",
        desc_en: "Peak VRAM usage",
    },
    ValueCol {
        col: "load_time_ms",
        id: "load_time",
        unit: Some("ms"),
        agg: Aggregation::Avg,
        desc_zh: "模型加载耗时",
        desc_en: "Model load time",
    },
    ValueCol {
        col: "ppl",
        id: "ppl",
        unit: None,
        agg: Aggregation::Min,
        desc_zh: "困惑度（probe-ppl 任务；越低越好）",
        desc_en: "Perplexity (probe-ppl task; lower is better)",
    },
    ValueCol {
        col: "avg_power_w",
        id: "avg_power",
        unit: Some("W"),
        agg: Aggregation::Avg,
        desc_zh: "测试期间平均功耗",
        desc_en: "Average power draw during the test",
    },
    ValueCol {
        col: "energy_per_token_mj",
        id: "energy_per_token",
        unit: Some("mJ"),
        agg: Aggregation::Avg,
        desc_zh: "每 token 能耗",
        desc_en: "Energy per token",
    },
    ValueCol {
        col: "p95_tps",
        id: "p95_tps",
        unit: Some("tok/s"),
        agg: Aggregation::Max,
        desc_zh: "会话任务 P95 token 速度",
        desc_en: "Session task P95 tokens per second",
    },
    ValueCol {
        col: "degradation_ratio",
        id: "degradation_ratio",
        unit: None,
        agg: Aggregation::Avg,
        desc_zh: "性能退化比（相对基准轮）",
        desc_en: "Performance degradation ratio (vs baseline round)",
    },
];

/// 白名单数值列定义。
pub struct ValueCol {
    /// CSV 原始列名。
    pub col: &'static str,
    /// 暴露给宿主的 metric id。
    pub id: &'static str,
    /// 单位。
    pub unit: Option<&'static str>,
    /// 降采样聚合方式。
    pub agg: Aggregation,
    /// 中文描述。
    pub desc_zh: &'static str,
    /// 英文描述。
    pub desc_en: &'static str,
}

/// AIBench 45 列表头的固定列名（can_handle 特征判定与 load 列对齐共用）。
pub const HEADER_COLUMNS: &[&str] = &[
    "sn",
    "machine_model",
    "cpu",
    "cpu_vendor",
    "gpu",
    "gpu_vendor",
    "driver_version",
    "vbios",
    "bios_version",
    "ec_version",
    "os_version",
    "os_arch",
    "total_ram_gb",
    "engine_id",
    "backend",
    "llama_build",
    "model_id",
    "task_name",
    "n_prompt",
    "n_gen",
    "ngl",
    "flash_attn",
    "avg_ts",
    "stddev_ts",
    "ttft_ms_measured",
    "ttft_ms_est",
    "vram_peak_mb",
    "vram_mode",
    "load_time_ms",
    "test_time",
    "samples",
    "ppl",
    "avg_power_w",
    "energy_per_token_mj",
    "rounds",
    "turns_ok",
    "tool_time_total_s",
    "e2e_duration_s",
    "session_success",
    "p95_tps",
    "degradation_ratio",
    "suite_name",
    "host_version",
    "timestamp",
    "status",
];

/// can_handle 特征列：首行含这些列名（大小写不敏感）即高度疑似 AIBench 日志。
pub const SIGNATURE_COLUMNS: &[&str] = &[
    "machine_model",
    "engine_id",
    "backend",
    "task_name",
    "n_prompt",
    "avg_ts",
    "llama_build",
];

/// 标签维度列（产出 Record.tags）。
pub const TAG_COLUMNS: &[&str] = &["engine_id", "backend", "model_id", "task_name"];

/// parse 期间逐行解析好的数据（load 阶段校验复用同一结构）。
pub struct Row {
    /// 时间戳（UTC 毫秒；同一时刻多行按行序 +1ms 去重）。
    pub timestamp_ms: i64,
    /// 白名单数值列的值（None = 空值，不产出 Record）。
    pub values: Vec<Option<f64>>,
    /// 标签值（按 TAG_COLUMNS 顺序）。
    pub tags: Vec<String>,
}

/// 解析时间文本 `YYYY-MM-DD HH:MM:SS`（视为本地无时区 → 按 UTC 直读，与
/// llama-bench 采集端写盘口径一致）→ UTC 毫秒。
pub fn parse_ts(text: &str) -> Option<i64> {
    let t = text.trim();
    if t.len() != 19 {
        return None;
    }
    let naive = chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S").ok()?;
    Some(naive.and_utc().timestamp_millis())
}

/// schema：白名单列 → MetricDef（id 与白名单 id 一致，names 用 CSV 原始列名）。
pub fn schema() -> Vec<MetricDef> {
    VALUE_COLUMNS
        .iter()
        .map(|c| MetricDef {
            id: c.id.to_string(),
            name: c.col.to_string(),
            unit: c.unit.map(String::from),
            description: Some(format!("{} / {}", c.desc_zh, c.desc_en)),
            aggregation: c.agg,
        })
        .collect()
}

/// metric id → CSV 列下标映射（load 时构建；列缺失 → 不产出该 metric）。
pub fn metric_col_indexes(header: &[String]) -> Vec<Option<usize>> {
    VALUE_COLUMNS
        .iter()
        .map(|c| {
            header
                .iter()
                .position(|h| h.trim().eq_ignore_ascii_case(c.col))
        })
        .collect()
}

/// 标签列下标映射（load 时构建）。
pub fn tag_col_indexes(header: &[String]) -> Vec<Option<usize>> {
    TAG_COLUMNS
        .iter()
        .map(|c| {
            header
                .iter()
                .position(|h| h.trim().eq_ignore_ascii_case(c))
        })
        .collect()
}

/// Record.tags 组装（skip-if-empty 契约：空标签整体省略键）。
pub fn record_tags(tag_values: &[String]) -> Option<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    for (name, v) in TAG_COLUMNS.iter().zip(tag_values.iter()) {
        let v = v.trim();
        if !v.is_empty() {
            m.insert(name.to_string(), v.to_string());
        }
    }
    (!m.is_empty()).then_some(m)
}

/// task_name 归一：`probe-pp@cuda-13.3-x64` → task=`probe-pp`，backend 已单列。
/// 实际上 backend 已有独立列，task 维度取 `@` 前段即可。
pub fn normalize_task(task_name: &str) -> String {
    match task_name.find('@') {
        Some(i) => task_name[..i].to_string(),
        None => task_name.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ts_basic() {
        assert_eq!(
            parse_ts("2026-09-04 18:34:02"),
            Some(1788546842000)
        );
        assert_eq!(parse_ts(" 2026-09-04 18:34:02 "), Some(1788546842000));
        assert_eq!(parse_ts("2026-09-04T18:34:02"), None);
        assert_eq!(parse_ts("2026-9-4 18:34:02"), None);
        assert_eq!(parse_ts("nope"), None);
        assert_eq!(parse_ts(""), None);
    }

    #[test]
    fn value_columns_have_unique_ids() {
        let mut ids: Vec<&str> = VALUE_COLUMNS.iter().map(|c| c.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "metric id 必须唯一");
    }

    #[test]
    fn value_columns_exist_in_header() {
        for c in VALUE_COLUMNS {
            assert!(
                HEADER_COLUMNS.contains(&c.col),
                "白名单列 {} 必须存在于 45 列表头",
                c.col
            );
        }
        for s in SIGNATURE_COLUMNS {
            assert!(HEADER_COLUMNS.contains(s));
        }
        for t in TAG_COLUMNS {
            assert!(HEADER_COLUMNS.contains(t));
        }
    }

    #[test]
    fn col_index_lookup_case_insensitive() {
        let header: Vec<String> = HEADER_COLUMNS.iter().map(|s| s.to_string()).collect();
        let idx = metric_col_indexes(&header);
        let avg_ts_idx = VALUE_COLUMNS.iter().position(|c| c.col == "avg_ts").unwrap();
        assert_eq!(idx[avg_ts_idx], Some(22));
        // 列缺失 → None。
        let short = vec!["sn".to_string(), "avg_ts".to_string()];
        let idx2 = metric_col_indexes(&short);
        let ppl_idx = VALUE_COLUMNS.iter().position(|c| c.col == "ppl").unwrap();
        assert_eq!(idx2[ppl_idx], None);
    }

    #[test]
    fn record_tags_skips_empty() {
        // 顺序与 TAG_COLUMNS 一致：engine_id, backend, model_id, task_name。
        let tags = vec![
            "cuda-13.3-x64".to_string(),
            String::new(),
            "qwen3.5-4b".to_string(),
            "probe-pp".to_string(),
        ];
        let m = record_tags(&tags).unwrap();
        assert_eq!(m.get("engine_id"), Some(&"cuda-13.3-x64".to_string()));
        assert_eq!(m.get("model_id"), Some(&"qwen3.5-4b".to_string()));
        assert_eq!(m.get("task_name"), Some(&"probe-pp".to_string()));
        assert!(!m.contains_key("backend"));
        // 全空 → None。
        let empty = vec![String::new(); TAG_COLUMNS.len()];
        assert!(record_tags(&empty).is_none());
        // 长度不齐：只取交集。
        let short = vec!["b1".to_string()];
        let m2 = record_tags(&short).unwrap();
        assert_eq!(m2.len(), 1);
    }

    #[test]
    fn normalize_task_splits_at_backend() {
        assert_eq!(normalize_task("probe-pp@cuda-13.3-x64"), "probe-pp");
        assert_eq!(normalize_task("soak48h-r01@vulkan-x64"), "soak48h-r01");
        assert_eq!(normalize_task("probe-ppl"), "probe-ppl");
        assert_eq!(normalize_task(" @ "), " ");
    }
}
