//! 解析引擎：can_handle 认领打分、load 全量校验、流式 parse（RecordBatch/progress）、key_values。
//!
//! 数据形态：每行 = 一次 AIBench probe/soak 任务（非高频时序）。「时间戳」取 `timestamp`
//! 列（`YYYY-MM-DD HH:MM:SS`，按 UTC 直读）；同一秒内多行按行序 +1ms 去重，保证时间轴
//! 唯一可排序。指标仅白名单 11 列，维度走 `tags`（engine/backend/model/task）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ab_plugin_rt::Sink;
use ab_protocol::types::{
    CanHandleParams, CanHandleResult, FileSummary, KeyValueEntry, KeyValuesResult, Record,
    RecordBatch, TimeRange,
};

use crate::aibench::{
    metric_col_indexes, normalize_task, parse_ts, record_tags, schema, tag_col_indexes, ValueCol,
    HEADER_COLUMNS, SIGNATURE_COLUMNS, TAG_COLUMNS, VALUE_COLUMNS,
};
use crate::csvline::{auto_delimiter, parse_number, split_line, unquote};

/// 流式批量大小（与 builtin-csv 同口径）。
pub const BATCH_SIZE: usize = 4000;
/// 重复时间戳去重步长（毫秒）。
pub const TS_DEDUPE_STEP_MS: i64 = 1;
/// 心跳间隔（协议 §3.3：解析期间必须周期性发送 progress）。
const HEARTBEAT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum ParseError {
    Cancelled,
}

/// 已加载文件（load 时冻结列对齐与全量校验结果；parse 直接回放）。
pub struct LoadedFile {
    pub file_id: String,
    pub content_bytes: usize,
    pub delimiter: char,
    pub header: Vec<String>,
    /// 白名单 metric → CSV 列下标（None = 该列缺失，metric 已从 schema 剔除）。
    pub metric_cols: Vec<Option<usize>>,
    /// 标签列 → CSV 列下标。
    pub tag_cols: Vec<Option<usize>>,
    /// timestamp 列下标。
    pub ts_col: usize,
    /// load 阶段全量校验时解析好的行（parse 直接回放，避免二次解析文本）。
    pub rows: Vec<crate::aibench::Row>,
    /// metric 定义（load pass 后按实际存在的列构建）。
    pub metrics: Vec<ab_protocol::types::MetricDef>,
    /// 任务名 → 最新一行摘要（key_values 用，按首次出现顺序）。
    pub latest_by_task: Vec<(String, LatestSnapshot)>,
    pub suite_name: String,
    pub note: String,
    pub bad_lines: usize,
    pub bad_samples: Vec<BadSample>,
    /// 精确记录数（所有行非空指标值总数）。
    pub record_count_hint: u64,
    pub time_range: Option<TimeRange>,
}

/// key_values 快照：一个任务在 ≤T 内最近一次观测的指标值。
pub struct LatestSnapshot {
    /// 观测时刻（毫秒）。
    pub ts: i64,
    /// metric id → 数值。
    pub values: Vec<(String, f64)>,
}

/// 单条坏行样例（行号 + 原因）。
pub type BadSample = (usize, String);

/// load/parse 统一错误类型。
pub type LoadError = String;

fn push_bad(bad: &mut usize, samples: &mut Vec<BadSample>, line_no: usize, reason: &str) {
    *bad += 1;
    if samples.len() < 10 {
        samples.push((line_no, reason.to_string()));
    }
}

/// can_handle：扩展名预筛 + AIBench 特征列判定（§3.3，3s 内返回；纯函数）。
///
/// 通用 CSV 插件对无 AIBench 特征列的表格置信度较低；本插件 7 个特征列全命中时给
/// 1.0，确保多插件认领同一 CSV 时宿主优先分派给本插件。
pub fn can_handle(p: &CanHandleParams) -> CanHandleResult {
    let ext = p.ext.to_lowercase();
    let base = match ext.as_str() {
        "csv" | "tsv" => 0.5,
        _ => 0.0,
    };
    if base == 0.0 {
        return CanHandleResult {
            can_handle: false,
            confidence: 0.0,
            reason: Some(format!("extension .{ext} not claimed")),
        };
    }
    let first = p.head_sample.split('\n').next().unwrap_or("");
    let delim = auto_delimiter(first);
    let cols: Vec<String> = split_line(first, delim)
        .into_iter()
        .map(|c| unquote(&c).trim().to_lowercase())
        .collect();
    if cols.len() < 10 {
        return CanHandleResult {
            can_handle: false,
            confidence: 0.0,
            reason: Some("fewer than 10 columns in first line".to_string()),
        };
    }
    let hits = SIGNATURE_COLUMNS
        .iter()
        .filter(|s| cols.iter().any(|c| c == **s))
        .count();
    if hits < SIGNATURE_COLUMNS.len() {
        return CanHandleResult {
            can_handle: false,
            confidence: 0.0,
            reason: Some(format!(
                "missing AIBench signature columns ({hits}/{})",
                SIGNATURE_COLUMNS.len()
            )),
        };
    }
    // 特征列全部命中：认领，置信度 1.0。
    CanHandleResult {
        can_handle: true,
        confidence: 1.0,
        reason: Some("AIBench llama-bench CSV: all signature columns matched".to_string()),
    }
}

/// 表头 → 列下标映射（大小写不敏感精确匹配）。
fn header_index(header: &[String], name: &str) -> Option<usize> {
    header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
}

/// 加载并分析：解码后的 content → 行记录 + 摘要（全量校验 pass）。
pub fn load_content(file_id: &str, content: &str) -> Result<LoadedFile, LoadError> {
    let content_bytes = content.len();
    let mut note: Vec<String> = Vec::new();
    let mut lines: Vec<&str> = content
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let first = lines.first().copied().unwrap_or("");
    let delimiter = auto_delimiter(first);
    let header: Vec<String> = split_line(first, delimiter)
        .into_iter()
        .map(|c| unquote(&c))
        .collect();

    // —— 表头特征校验：45 列全对齐（大小写不敏感）——
    let missing: Vec<&str> = HEADER_COLUMNS
        .iter()
        .filter(|expected| header_index(&header, expected).is_none())
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "not an AIBench llama-bench CSV: missing columns {}",
            missing.join(", ")
        ));
    }
    note.push(format!("AIBench schema ({} columns)", header.len()));

    let metric_cols = metric_col_indexes(&header);
    let tag_cols = tag_col_indexes(&header);
    let Some(ts_col) = header_index(&header, "timestamp") else {
        return Err("no timestamp column".to_string());
    };
    let suite_col = header_index(&header, "suite_name");
    let status_col = header_index(&header, "status");

    // —— 全量解析 pass：行记录 + 坏行计数 + 时间范围 ——
    let mut rows: Vec<crate::aibench::Row> = Vec::new();
    let mut bad = 0usize;
    let mut samples: Vec<BadSample> = Vec::new();
    let mut t_min: Option<i64> = None;
    let mut t_max: Option<i64> = None;
    let mut suite_name = String::new();
    let mut ts_counts: HashMap<i64, usize> = HashMap::new();

    for (i, line) in lines.iter().enumerate().skip(1) {
        let line_no = i + 1;
        let fields = split_line(line, delimiter);
        if fields.len() != header.len() {
            push_bad(&mut bad, &mut samples, line_no, "column count mismatch");
            continue;
        }
        // status != ok 的行按坏行计（样例日志全为 ok）。
        if let Some(sc) = status_col {
            let s = fields[sc].trim().to_lowercase();
            if !s.is_empty() && s != "ok" {
                push_bad(&mut bad, &mut samples, line_no, "status not ok");
                continue;
            }
        }
        let Some(mut ts) = parse_ts(&fields[ts_col]) else {
            push_bad(&mut bad, &mut samples, line_no, "bad timestamp");
            continue;
        };
        // 同一秒多行：按行序 +1ms 步进去重。
        let n = ts_counts.entry(ts).or_insert(0);
        *n += 1;
        ts += (*n as i64 - 1) * TS_DEDUPE_STEP_MS;

        let mut values = vec![None; VALUE_COLUMNS.len()];
        for (k, mc) in metric_cols.iter().enumerate() {
            let Some(j) = mc else { continue };
            let cell = fields.get(*j).map(String::as_str).unwrap_or("");
            if cell.trim().is_empty() {
                continue; // 空值：该任务无此指标（如 ppl 行无 avg_ts）
            }
            match parse_number(cell) {
                Some(v) => values[k] = Some(v),
                None => {
                    push_bad(&mut bad, &mut samples, line_no, "non-numeric value column");
                    continue;
                }
            }
        }
        if values.iter().all(|v| v.is_none()) {
            push_bad(&mut bad, &mut samples, line_no, "all value columns empty");
            continue;
        }
        let tags: Vec<String> = TAG_COLUMNS
            .iter()
            .enumerate()
            .map(|(k, _)| {
                tag_cols[k]
                    .and_then(|j| fields.get(j))
                    .map(|v| unquote(v))
                    .unwrap_or_default()
            })
            .collect();
        if suite_name.is_empty() {
            if let Some(sc) = suite_col {
                suite_name = fields.get(sc).map(|v| unquote(v)).unwrap_or_default();
            }
        }
        t_min = Some(t_min.map_or(ts, |m: i64| m.min(ts)));
        t_max = Some(t_max.map_or(ts, |m: i64| m.max(ts)));
        rows.push(crate::aibench::Row {
            timestamp_ms: ts,
            values,
            tags,
        });
    }
    if bad > 0 {
        note.push(format!("skipped {bad} bad lines"));
    }
    note.push(format!("{}/{} rows parsed", rows.len(), lines.len() - 1));

    // —— metric 定义（剔除列缺失者）——
    let metrics = schema()
        .into_iter()
        .enumerate()
        .filter(|(k, _)| metric_cols[*k].is_some())
        .map(|(_, m)| m)
        .collect();

    // —— 任务名 → 最新快照（key_values 用；按行序遍历，后写覆盖先写）——
    let mut latest: HashMap<String, LatestSnapshot> = HashMap::new();
    let mut task_order: Vec<String> = Vec::new();
    let task_tag_idx = TAG_COLUMNS
        .iter()
        .position(|t| *t == "task_name")
        .unwrap_or(0);
    for row in &rows {
        let raw_task = row.tags.get(task_tag_idx).cloned().unwrap_or_default();
        let task = normalize_task(&raw_task);
        if task.is_empty() {
            continue;
        }
        let values: Vec<(String, f64)> = VALUE_COLUMNS
            .iter()
            .enumerate()
            .filter(|(k, _)| row.values[*k].is_some())
            .map(|(k, c)| (c.id.to_string(), row.values[k].unwrap()))
            .collect();
        let e = latest.entry(task.clone()).or_insert_with(|| {
            task_order.push(task.clone());
            LatestSnapshot {
                ts: 0,
                values: Vec::new(),
            }
        });
        e.ts = row.timestamp_ms;
        e.values = values;
    }
    let latest_by_task: Vec<(String, LatestSnapshot)> = task_order
        .into_iter()
        .filter_map(|t| latest.remove_entry(&t))
        .collect();

    // —— 精确记录数：所有行非空指标值总数 ——
    let record_count_hint: u64 = rows
        .iter()
        .map(|r| r.values.iter().filter(|v| v.is_some()).count() as u64)
        .sum();

    Ok(LoadedFile {
        file_id: file_id.to_string(),
        content_bytes,
        delimiter,
        header,
        metric_cols,
        tag_cols,
        ts_col,
        rows,
        metrics,
        latest_by_task,
        suite_name,
        note: note.join(", "),
        bad_lines: bad,
        bad_samples: samples,
        record_count_hint,
        time_range: match (t_min, t_max) {
            (Some(s), Some(e)) => Some(TimeRange {
                start_ms: s,
                end_ms: e,
            }),
            _ => None,
        },
    })
}

/// 加载文件（fs 读 + BOM 剥除 + UTF-8 解码 + 分析）。
pub fn load_file(file_id: &str, path: &str) -> Result<LoadedFile, LoadError> {
    let raw = std::fs::read(path).map_err(|e| format!("cannot read file: {e}"))?;
    let raw = if raw.starts_with(&[0xEFu8, 0xBB, 0xBF]) {
        raw[3..].to_vec()
    } else {
        raw
    };
    let content = String::from_utf8(raw).map_err(|_| "file is not valid UTF-8".to_string())?;
    load_content(file_id, &content)
}

/// 流式 parse（§3.4）：回放 load 阶段解析好的行、心跳、BATCH_SIZE 批量、raw_line 抽样 1/500。
pub fn parse_file(
    lf: &mut LoadedFile,
    sink: &mut dyn Sink,
    cancel: &AtomicBool,
) -> Result<u64, ParseError> {
    let mut seq: u64 = 0;
    let mut buf: Vec<Record> = Vec::with_capacity(BATCH_SIZE);
    let mut total: u64 = 0;
    let mut last_hb = Instant::now();
    let row_total = lf.rows.len().max(1);
    let content_bytes = lf.content_bytes.max(1);

    sink.progress(Some(0.0), 0, Some(0));
    for (i, row) in lf.rows.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ParseError::Cancelled);
        }
        if last_hb.elapsed() >= HEARTBEAT || (i % 20000 == 0 && i > 0) {
            let percent = (i + 1) as f64 / row_total as f64 * 100.0;
            sink.progress(Some(percent), total, Some(content_bytes as u64));
            last_hb = Instant::now();
        }
        // raw_line 抽样 1/500（防内存膨胀契约），内容为行号。
        let raw_opt = if (i + 1) % 500 == 0 {
            Some(format!("row {}", i + 1))
        } else {
            None
        };
        let tags = record_tags(&row.tags);
        for (k, col) in VALUE_COLUMNS.iter().enumerate() {
            let Some(v) = row.values[k] else { continue };
            if lf.metric_cols[k].is_none() {
                continue; // 列缺失的 metric 已从 schema 剔除
            }
            buf.push(Record {
                timestamp: row.timestamp_ms,
                metric: col.id.to_string(),
                value: v,
                level: None,
                tags: tags.clone(),
                raw_line: raw_opt.clone(),
            });
            total += 1;
            if buf.len() >= BATCH_SIZE {
                sink.batch(RecordBatch {
                    file_id: lf.file_id.clone(),
                    seq,
                    records: std::mem::take(&mut buf),
                    done: false,
                });
                seq += 1;
            }
        }
    }
    sink.batch(RecordBatch {
        file_id: lf.file_id.clone(),
        seq,
        records: buf,
        done: true,
    });
    Ok(total)
}

/// key_values（§3.5）：≤T 每任务最近一次观测的指标值 + 套件标识。
pub fn key_values(lf: &LoadedFile, timestamp_ms: i64) -> KeyValuesResult {
    let mut entries: Vec<KeyValueEntry> = Vec::new();
    if !lf.suite_name.is_empty() {
        entries.push(KeyValueEntry {
            key: "suite_name".to_string(),
            value: serde_json::Value::String(lf.suite_name.clone()),
            unit: None,
        });
    }
    for (task, snap) in &lf.latest_by_task {
        if snap.ts > timestamp_ms {
            continue;
        }
        let prefix = format!("task:{task}");
        for (mid, v) in &snap.values {
            entries.push(KeyValueEntry {
                key: format!("{prefix}:{mid}"),
                value: serde_json::json!(v),
                unit: value_col_by_id(mid).and_then(|c| c.unit.map(String::from)),
            });
        }
    }
    KeyValuesResult { entries }
}

fn value_col_by_id(id: &str) -> Option<&'static ValueCol> {
    VALUE_COLUMNS.iter().find(|c| c.id == id)
}

/// FileSummary 组装（§2.3）。
pub fn to_summary(lf: &LoadedFile) -> FileSummary {
    FileSummary {
        record_count_hint: if lf.rows.is_empty() {
            None
        } else {
            Some(lf.record_count_hint)
        },
        time_range: lf.time_range,
        note: Some(lf.note.clone()),
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod engine_tests;
