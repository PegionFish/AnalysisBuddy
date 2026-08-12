//! 解析引擎：can_handle 打分（§3.3）、load 分析、流式 parse（§3.4）、schema/key_values（§3.5）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ab_protocol::types::{
    Aggregation, CanHandleParams, CanHandleResult, FileSummary, KeyValueEntry, KeyValuesResult,
    MetricDef, Record, RecordBatch, TimeRange,
};

use crate::config::{Config, Encoding, HasHeader, TimeFormat};
use crate::csvline::{auto_delimiter, is_number, parse_number, split_line, unquote};
use crate::timefmt::parse_time;

pub const BATCH_SIZE: usize = 4000;
const SCAN_ROWS: usize = 1000;
const RAW_LINE_SAMPLE: usize = 500;
const KV_CARDINALITY_LIMIT: usize = 10;

/// 单条坏行样例（行号 + 原因）。
pub type BadSample = (usize, String);

#[derive(Debug)]
pub enum ParseError {
    Cancelled,
}

/// parse 输出端：批量 + 进度（由 main.rs 以 NDJSON 通知实现）。
pub trait Sink {
    fn batch(&mut self, batch: RecordBatch);
    fn progress(&mut self, percent: Option<f64>, records_so_far: u64, bytes_read: Option<u64>);
}

/// key_values 追踪列：取值域 ≤10 的非数值列。
pub struct KvColumn {
    pub idx: usize,
    pub name: String,
    pub distinct: HashSet<String>,
    /// 行序 (timestamp, value) 样本；valid=false 后清空。
    pub samples: Vec<(i64, String)>,
    pub valid: bool,
}

/// 已加载文件（load 时分析冻结列集合，§3.5）。
pub struct LoadedFile {
    pub file_id: String,
    pub content: String,
    pub content_bytes: usize,
    pub delimiter: char,
    pub has_header: bool,
    pub header: Vec<String>,
    pub time_col: Option<usize>,
    pub time_format: TimeFormat,
    /// 列对齐：数值列 → 对应 metric id。
    pub metric_ids: Vec<Option<String>>,
    pub metrics: Vec<MetricDef>,
    pub kv: Vec<KvColumn>,
    pub note: String,
    pub bad_lines: usize,
    pub bad_samples: Vec<BadSample>,
    pub record_count_hint: Option<u64>,
    pub time_range: Option<TimeRange>,
}

/// 解码（§3.2 `encoding` 五态）。
///
/// `raw` 按值接收：UTF-8 路径直接 `String::from_utf8` 零拷贝移动（严格校验通过即
/// 采用），避免整文件第二份副本；GBK/UTF-16 由 encoding_rs 产出新 String（无中间
/// 整份 Vec<u16>）。Auto 检测顺序：UTF-8 BOM → UTF-16 LE/BE BOM → 严格 UTF-8
/// 校验 → GBK（无可信替换才采用）→ 有损回落，且非 UTF-8 路径均记 note（不再静默
/// U+FFFD 替换）。
pub fn decode(raw: Vec<u8>, enc: &Encoding) -> (String, Vec<String>) {
    const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
    match enc {
        Encoding::Auto => {
            if raw.starts_with(UTF8_BOM) {
                let mut raw = raw;
                raw.drain(0..3);
                match String::from_utf8(raw) {
                    Ok(s) => (s, vec!["UTF-8 BOM detected".to_string()]),
                    Err(e) => (
                        String::from_utf8_lossy(e.as_bytes()).into_owned(),
                        vec!["UTF-8 BOM detected; lossy fallback".to_string()],
                    ),
                }
            } else if raw.starts_with(&[0xFF, 0xFE]) {
                decode_utf16(raw, false, "UTF-16LE BOM detected")
            } else if raw.starts_with(&[0xFE, 0xFF]) {
                decode_utf16(raw, true, "UTF-16BE BOM detected")
            } else {
                match String::from_utf8(raw) {
                    Ok(s) => (s, Vec::new()),
                    Err(e) => {
                        let raw = e.into_bytes();
                        let (cow, _enc, had_errors) = encoding_rs::GBK.decode(&raw);
                        if !had_errors && !cow.contains('\u{FFFD}') {
                            (cow.into_owned(), vec!["decoded as GBK".to_string()])
                        } else {
                            (
                                String::from_utf8_lossy(&raw).into_owned(),
                                vec!["encoding fallback: lossy decode (not UTF-8/GBK)".to_string()],
                            )
                        }
                    }
                }
            }
        }
        Encoding::Utf8 => {
            if raw.starts_with(UTF8_BOM) {
                let mut raw = raw;
                raw.drain(0..3);
                (
                    String::from_utf8_lossy(&raw).into_owned(),
                    vec!["UTF-8 BOM detected".to_string()],
                )
            } else {
                (String::from_utf8_lossy(&raw).into_owned(), Vec::new())
            }
        }
        Encoding::Utf16Le => decode_utf16(raw, false, "decoded as UTF-16LE"),
        Encoding::Utf16Be => decode_utf16(raw, true, "decoded as UTF-16BE"),
        Encoding::Gbk => {
            let (cow, _enc, _had_errors) = encoding_rs::GBK.decode(&raw);
            (cow.into_owned(), vec!["decoded as GBK".to_string()])
        }
    }
}

/// UTF-16 解码：encoding_rs 直接产出 String（不再有中间整份 Vec<u16>）。
fn decode_utf16(raw: Vec<u8>, big_endian: bool, note: &str) -> (String, Vec<String>) {
    let raw = if big_endian {
        raw.strip_prefix(&[0xFE, 0xFF]).unwrap_or(&raw)
    } else {
        raw.strip_prefix(&[0xFF, 0xFE]).unwrap_or(&raw)
    };
    let (cow, _enc, _had_errors) = if big_endian {
        encoding_rs::UTF_16BE.decode(raw)
    } else {
        encoding_rs::UTF_16LE.decode(raw)
    };
    (cow.into_owned(), vec![note.to_string()])
}

/// 时间列解析（§3.2）：显式名 / auto 正则名 / 首列可解析回退。
fn resolve_time_column(names: &[String], cfg: &Config) -> Option<usize> {
    if cfg.time_column.eq_ignore_ascii_case("auto") {
        names.iter().position(|n| {
            matches!(
                n.trim().to_lowercase().as_str(),
                "timestamp" | "time" | "ts" | "datetime" | "date"
            )
        })
    } else {
        names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(cfg.time_column.trim()))
    }
}

fn metric_id_of(name: &str) -> String {
    name.trim().to_lowercase().replace(' ', "_")
}

/// 行校验：列数 / 时间列 / 数值列；返回行时间戳（毫秒）。
fn row_ts(
    fields: &[String],
    header_len: usize,
    time_col: Option<usize>,
    time_format: &TimeFormat,
    metric_ids: &[Option<String>],
) -> Result<i64, &'static str> {
    if fields.len() != header_len {
        return Err("column count mismatch");
    }
    let ts = match time_col {
        Some(ti) => parse_time(&fields[ti], time_format).ok_or("bad timestamp value")?,
        None => return Err("no time column"),
    };
    for j in 0..metric_ids.len() {
        if metric_ids[j].is_some() && parse_number(&fields[j]).is_none() {
            return Err("non-numeric value in numeric column");
        }
    }
    Ok(ts)
}

fn push_bad(bad: &mut usize, samples: &mut Vec<BadSample>, line_no: usize, reason: &str) {
    *bad += 1;
    if samples.len() < 10 {
        samples.push((line_no, reason.to_string()));
    }
}

/// 加载并分析（含全量校验 pass，产出精确 note/bad_lines/hint/time_range）。
///
/// `content` 按值接收并直接存入 `LoadedFile.content`（零拷贝移动，不再整文件克隆；
/// 与解码结果之间最多一份副本）。
pub fn load_content(file_id: &str, name: &str, content: String, cfg: &Config) -> LoadedFile {
    let content_bytes = content.len();
    let mut note: Vec<String> = Vec::new();
    let lines: Vec<&str> = content
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let first_idx = lines.iter().position(|l| !l.is_empty());
    let first = first_idx.map(|i| lines[i]).unwrap_or("");

    // —— 分隔符 ——
    let delimiter = cfg
        .delimiter
        .as_char()
        .unwrap_or_else(|| auto_delimiter(first));
    note.push(match delimiter {
        ',' => "comma-separated".to_string(),
        ';' => "semicolon-separated".to_string(),
        '\t' => "tab-separated".to_string(),
        _ => "comma-separated".to_string(),
    });

    // —— 表头 ——
    let first_cols = split_line(first, delimiter);
    let has_header = match cfg.has_header {
        HasHeader::Yes => true,
        HasHeader::No => false,
        HasHeader::Auto => {
            // 首行全非数值文本 且 其余行首格为数值或可解析时间（§3.2 的宽松超集，
            // 覆盖「首列时间戳」的常见夹具形态）。
            let second = lines
                .iter()
                .skip(first_idx.map(|i| i + 1).unwrap_or(0))
                .find(|l| !l.is_empty())
                .copied();
            match second {
                Some(s) => {
                    let sc = split_line(s, delimiter);
                    !sc.is_empty()
                        && !first_cols.is_empty()
                        && first_cols.iter().all(|c| !is_number(c))
                        && (is_number(&sc[0]) || parse_time(&sc[0], &cfg.time_format).is_some())
                }
                None => false,
            }
        }
    };
    if has_header {
        note.push("header row detected".to_string());
    } else {
        note.push("no header row".to_string());
    }

    let header: Vec<String> = if has_header {
        first_cols
    } else {
        (0..first_cols.len()).map(|i| format!("col{i}")).collect()
    };

    // —— 数据行（表头之后非空行）——
    let data_start = if has_header {
        first_idx.map(|i| i + 1).unwrap_or(0)
    } else {
        first_idx.unwrap_or(0)
    };
    let data_lines: Vec<&str> = lines
        .iter()
        .enumerate()
        .skip(data_start)
        .filter(|(_, l)| !l.is_empty())
        .map(|(_, l)| *l)
        .collect();

    // —— 数值列扫描（前 1000 行，§3.5 冻结）——
    let mut numeric = vec![false; header.len()];
    for row in data_lines.iter().take(SCAN_ROWS) {
        let fields = split_line(row, delimiter);
        for (j, cell) in fields.iter().enumerate() {
            if j < numeric.len() && is_number(cell) {
                numeric[j] = true;
            }
        }
    }

    // —— 时间列（§3.2 名匹配；无命中回退首列可解析）——
    let time_col = resolve_time_column(&header, cfg).or_else(|| {
        data_lines.first().and_then(|row| {
            let f = split_line(row, delimiter);
            f.first()
                .map(|c| parse_time(c, &cfg.time_format).is_some())
                .unwrap_or(false)
                .then_some(0)
        })
    });
    if time_col.is_none() {
        note.push("no time column detected".to_string());
    }
    if let Some(ti) = time_col {
        numeric[ti] = false; // 时间列不产出 metric
    }

    // —— metric 定义（每数值列一个，id 去重）——
    let mut metrics: Vec<MetricDef> = Vec::new();
    let mut metric_ids: Vec<Option<String>> = vec![None; header.len()];
    let mut used: HashMap<String, usize> = HashMap::new();
    for (j, col_name) in header.iter().enumerate() {
        if !numeric[j] {
            continue;
        }
        let base = metric_id_of(col_name);
        let id = if base.is_empty() {
            format!("column_{j}")
        } else {
            base
        };
        let id = dedupe_id(&mut used, &id);
        metric_ids[j] = Some(id.clone());
        metrics.push(MetricDef {
            id,
            name: col_name.clone(),
            unit: None,
            description: Some(format!("column {col_name} of {name}")),
            aggregation: Aggregation::Avg,
        });
    }

    // —— 全量校验 pass：坏行计数 + hint + time_range + kv 候选 ——
    let mut bad = 0usize;
    let mut samples: Vec<BadSample> = Vec::new();
    let mut t_min: Option<i64> = None;
    let mut t_max: Option<i64> = None;
    let mut good_rows = 0u64;
    let kv: Vec<KvColumn> = (0..header.len())
        .filter(|j| !numeric[*j] && time_col != Some(*j) && !header[*j].is_empty())
        .map(|j| KvColumn {
            idx: j,
            name: header[j].clone(),
            distinct: HashSet::new(),
            samples: Vec::new(),
            valid: true,
        })
        .collect();

    for (i, row) in data_lines.iter().enumerate() {
        let line_no = i + data_start + 1;
        let fields = split_line(row, delimiter);
        match row_ts(
            &fields,
            header.len(),
            time_col,
            &cfg.time_format,
            &metric_ids,
        ) {
            Ok(ts) => {
                good_rows += 1;
                t_min = Some(t_min.map_or(ts, |m| m.min(ts)));
                t_max = Some(t_max.map_or(ts, |m| m.max(ts)));
            }
            Err(reason) => push_bad(&mut bad, &mut samples, line_no, reason),
        }
    }
    if bad > 0 {
        note.push(format!("skipped {bad} bad lines"));
    }

    let metric_count = metrics.len() as u64;
    LoadedFile {
        file_id: file_id.to_string(),
        content,
        content_bytes,
        delimiter,
        has_header,
        header,
        time_col,
        time_format: cfg.time_format.clone(),
        metric_ids,
        metrics,
        kv,
        note: note.join(", "),
        bad_lines: bad,
        bad_samples: samples,
        record_count_hint: (good_rows > 0).then_some(good_rows * metric_count),
        time_range: match (t_min, t_max) {
            (Some(s), Some(e)) => Some(TimeRange {
                start_ms: s,
                end_ms: e,
            }),
            _ => None,
        },
    }
}

fn dedupe_id(used: &mut HashMap<String, usize>, id: &str) -> String {
    let n = used.entry(id.to_string()).or_insert(0);
    *n += 1;
    if *n == 1 {
        id.to_string()
    } else {
        format!("{id}_{n}")
    }
}

/// 加载文件（fs 读 + 解码 + 分析）。
pub fn load_file(file_id: &str, path: &str, cfg: &Config) -> Result<LoadedFile, String> {
    let raw = std::fs::read(path).map_err(|e| format!("cannot read file: {e}"))?;
    let (content, enc_notes) = decode(raw, &cfg.encoding);
    let name = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let mut lf = load_content(file_id, &name, content, cfg);
    if !enc_notes.is_empty() {
        lf.note = format!(
            "{enc}, {lf_note}",
            enc = enc_notes.join(", "),
            lf_note = lf.note
        );
    }
    Ok(lf)
}

/// can_handle 指纹打分（§3.3，3s 内返回；纯函数）。
pub fn can_handle(p: &CanHandleParams, cfg: &Config, fingerprints: &[String]) -> CanHandleResult {
    let ext = p.ext.to_lowercase();
    let base = match ext.as_str() {
        "csv" | "tsv" => 0.5,
        "txt" => 0.3,
        _ => 0.0,
    };
    if base == 0.0 {
        return CanHandleResult {
            can_handle: false,
            confidence: 0.0,
            reason: Some(format!("extension .{ext} not claimed")),
        };
    }
    let mut score: f64 = base;
    let mut parts = vec![format!("extension .{ext}")];
    let head = &p.head_sample;
    let first = head.split('\n').next().unwrap_or("");
    let delim = cfg
        .delimiter
        .as_char()
        .unwrap_or_else(|| auto_delimiter(first));
    let cols = split_line(first, delim);
    if cols.len() >= 2 {
        score += 0.2; // 仅列数达标 +0.2；时间列可识别再 +0.2（合计 +0.4）
        let second = head.split('\n').nth(1).unwrap_or("");
        let second_cols = split_line(second, delim);
        let header_detected = match cfg.has_header {
            HasHeader::Yes => true,
            HasHeader::No => false,
            HasHeader::Auto => {
                !second_cols.is_empty()
                    && cols.iter().all(|c| !is_number(c))
                    && (is_number(&second_cols[0])
                        || parse_time(&second_cols[0], &cfg.time_format).is_some())
            }
        };
        let names: Vec<String> = if header_detected {
            cols.clone()
        } else {
            (0..cols.len()).map(|i| format!("col{i}")).collect()
        };
        let data_row: &[String] = if header_detected { &second_cols } else { &cols };
        match resolve_time_column(&names, cfg) {
            Some(ti)
                if ti < data_row.len() && parse_time(&data_row[ti], &cfg.time_format).is_some() =>
            {
                score += 0.2;
                parts.push(format!("time column '{}' detected", names[ti]));
            }
            Some(ti) => parts.push(format!("time column '{}' value unparseable", names[ti])),
            None if !data_row.is_empty()
                && parse_time(&data_row[0], &cfg.time_format).is_some() =>
            {
                score += 0.2;
                parts.push("first column parses as time".to_string());
            }
            None => parts.push("no recognizable time column".to_string()),
        }
    } else {
        parts.push("fewer than 2 columns".to_string());
    }
    for fp in fingerprints {
        if !fp.is_empty() && head.to_lowercase().contains(&fp.to_lowercase()) {
            score += 0.1;
            parts.push(format!("header fingerprint '{fp}' matched"));
        }
    }
    let confidence = score.min(1.0);
    CanHandleResult {
        can_handle: true,
        confidence,
        reason: Some(parts.join(", ")),
    }
}

/// 流式 parse（§3.4）：逐行、坏行跳过计数、心跳、4000 批量、raw_line 1/500 抽样。
pub fn parse_file(
    lf: &mut LoadedFile,
    sink: &mut dyn Sink,
    cancel: &AtomicBool,
) -> Result<u64, ParseError> {
    let mut seq: u64 = 0;
    let mut buf: Vec<Record> = Vec::with_capacity(BATCH_SIZE);
    let mut total: u64 = 0;
    let mut bad = 0usize;
    let mut bad_samples: Vec<BadSample> = Vec::new();
    let mut bytes_read = 0usize;
    let content_bytes = lf.content_bytes.max(1);

    // 首条 progress（保证 BEH-04「从未发过 progress」不触发；§3.3）。
    sink.progress(Some(0.0), 0, Some(0));
    let mut last_hb = Instant::now();

    for (line_no, raw_line) in lf.content.split('\n').enumerate() {
        let line_no = line_no + 1;
        bytes_read += raw_line.len() + 1;
        if cancel.load(Ordering::Relaxed) {
            return Err(ParseError::Cancelled);
        }
        if line_no % 20000 == 0 || last_hb.elapsed() >= Duration::from_secs(2) {
            let percent = bytes_read as f64 / content_bytes as f64 * 100.0;
            sink.progress(Some(percent), total, Some(bytes_read as u64));
            last_hb = Instant::now();
        }
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || (lf.has_header && line_no == 1) {
            continue;
        }
        let fields = split_line(line, lf.delimiter);
        match row_ts(
            &fields,
            lf.header.len(),
            lf.time_col,
            &lf.time_format,
            &lf.metric_ids,
        ) {
            Ok(ts) => {
                let raw_opt = if line_no % RAW_LINE_SAMPLE == 0 {
                    Some(line.to_string())
                } else {
                    None
                };
                for (j, metric) in lf.metric_ids.iter().enumerate() {
                    let Some(metric) = metric else { continue };
                    let value = match parse_number(&fields[j]) {
                        Some(v) => v,
                        None => {
                            // row_ts 已校验，理论上不可达；防御性跳过。
                            push_bad(&mut bad, &mut bad_samples, line_no, "non-numeric value");
                            continue;
                        }
                    };
                    buf.push(Record {
                        timestamp: ts,
                        metric: metric.clone(),
                        value,
                        level: None,
                        tags: None,
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
                for kv in lf.kv.iter_mut() {
                    if !kv.valid || kv.idx >= fields.len() {
                        continue;
                    }
                    let v = unquote(&fields[kv.idx]);
                    if v.is_empty() {
                        continue;
                    }
                    if kv.distinct.insert(v.clone()) && kv.distinct.len() > KV_CARDINALITY_LIMIT {
                        kv.valid = false;
                        kv.samples.clear();
                    }
                    if kv.valid {
                        kv.samples.push((ts, v));
                    }
                }
            }
            Err(reason) => push_bad(&mut bad, &mut bad_samples, line_no, reason),
        }
    }
    sink.batch(RecordBatch {
        file_id: lf.file_id.clone(),
        seq,
        records: buf,
        done: true,
    });
    lf.bad_lines += bad;
    lf.bad_samples.extend(bad_samples);
    Ok(total)
}

/// key_values（§3.5）：取值域 ≤10 的非数值列取 ≤T 最新一行。
pub fn key_values(lf: &LoadedFile, timestamp_ms: i64) -> KeyValuesResult {
    let mut entries = Vec::new();
    for kv in &lf.kv {
        if !kv.valid {
            continue;
        }
        if let Some((_, v)) = kv.samples.iter().rev().find(|(ts, _)| *ts <= timestamp_ms) {
            entries.push(KeyValueEntry {
                key: kv.name.clone(),
                value: serde_json::Value::String(v.clone()),
                unit: None,
            });
        }
    }
    KeyValuesResult { entries }
}

/// FileSummary 组装（§2.3）。
pub fn to_summary(lf: &LoadedFile) -> FileSummary {
    FileSummary {
        record_count_hint: lf.record_count_hint,
        time_range: lf.time_range,
        note: Some(lf.note.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn cfg() -> Config {
        Config::default()
    }

    fn lf_from(content: &str) -> LoadedFile {
        load_content("f1", "a.csv", content.to_string(), &cfg())
    }

    #[derive(Default)]
    struct Rec {
        batches: Vec<RecordBatch>,
        progresses: Vec<(Option<f64>, u64, Option<u64>)>,
    }
    impl Sink for Rec {
        fn batch(&mut self, batch: RecordBatch) {
            self.batches.push(batch);
        }
        fn progress(&mut self, percent: Option<f64>, records_so_far: u64, bytes_read: Option<u64>) {
            self.progresses.push((percent, records_so_far, bytes_read));
        }
    }

    const HEADER: &str = "timestamp,fps,frame_ms,mem_mb\n";
    const ROW: &str = "2026-08-07T10:00:00.000+08:00,59.8,16.6,1024\n";

    #[test]
    fn header_detection_with_and_without() {
        let with = lf_from(&format!(
            "{HEADER}{ROW}2026-08-07T10:00:00.500+08:00,60.1,16.4,1030\n"
        ));
        assert!(with.has_header);
        assert_eq!(with.header, vec!["timestamp", "fps", "frame_ms", "mem_mb"]);
        let without = lf_from("2026-08-07T10:00:00.000+08:00,59.8,16.6,1024\n");
        assert!(!without.has_header);
        assert_eq!(without.header, vec!["col0", "col1", "col2", "col3"]);
        assert_eq!(without.time_col, Some(0));
        assert_eq!(with.time_col, Some(0));
    }

    #[test]
    fn schema_frozen_from_first_rows() {
        // 首 1000 行数值，第 1001 行出现数值的列不产生 metric（列集合 load 时冻结）。
        let mut content = HEADER.to_string();
        for i in 0..1005 {
            let hh = i % 24;
            let ts = format!("2026-08-07T{hh:02}:00:00.000+08:00");
            content.push_str(&format!("{ts},59.8,16.6,1024\n"));
        }
        let lf = lf_from(&content);
        let ids: Vec<&str> = lf.metrics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["fps", "frame_ms", "mem_mb"]);
        assert_eq!(lf.metrics[0].aggregation, Aggregation::Avg);
        assert_eq!(lf.metrics[0].name, "fps");
        assert_eq!(
            lf.metrics[0].description.as_deref(),
            Some("column fps of a.csv")
        );
    }

    #[test]
    fn metric_id_normalization_and_dedupe() {
        let content = "timestamp,Cpu Temp,CPU temp\n2026-08-07T00:00:00.000+08:00,50,60\n";
        let lf = lf_from(content);
        let ids: Vec<&str> = lf.metrics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["cpu_temp", "cpu_temp_2"]);
    }

    #[test]
    fn parse_batches_seq_and_total() {
        let mut content = HEADER.to_string();
        for i in 0..5000 {
            let hh = i / 3600;
            let mm = (i / 60) % 60;
            let ss = i % 60;
            let ts = format!("2026-08-07T{hh:02}:{mm:02}:{ss:02}.000+08:00");
            content.push_str(&format!("{ts},60.0,16.0,1000\n"));
        }
        let mut lf = lf_from(&content);
        let mut rec = Rec::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let total = parse_file(&mut lf, &mut rec, &cancel).unwrap();
        assert_eq!(total, 5000 * 3);
        let seqs: Vec<u64> = rec.batches.iter().map(|b| b.seq).collect();
        assert_eq!(seqs.len() as u64, total / 4000 + 1);
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());
        let sum: usize = rec.batches.iter().map(|b| b.records.len()).sum();
        assert_eq!(sum as u64, total);
        assert!(rec.batches.last().unwrap().done);
        assert_eq!(rec.batches[0].records.len(), 4000);
        assert!(
            rec.progresses.iter().any(|p| p.0.is_some()),
            "progress sent"
        );
    }

    #[test]
    fn raw_line_sampling_1_of_500() {
        let mut content = HEADER.to_string();
        for i in 0..1000 {
            let hh = i / 3600;
            let mm = (i / 60) % 60;
            let ss = i % 60;
            let ts = format!("2026-08-07T{hh:02}:{mm:02}:{ss:02}.000+08:00");
            content.push_str(&format!("{ts},60.0,16.0,1000\n"));
        }
        let mut lf = lf_from(&content);
        let mut rec = Rec::default();
        let cancel = Arc::new(AtomicBool::new(false));
        parse_file(&mut lf, &mut rec, &cancel).unwrap();
        let raw_count: usize = rec
            .batches
            .iter()
            .flat_map(|b| b.records.iter())
            .filter(|r| r.raw_line.is_some())
            .count();
        assert_eq!(raw_count, 1000 / 500 * 3, "every 500th line × 3 metrics");
    }

    #[test]
    fn bad_lines_skipped_counted_not_fatal() {
        let content = "timestamp,fps,frame_ms,mem_mb\n\
2026-08-07T00:00:00.000+08:00,59.8,16.6,1024\n\
2026-08-07T00:00:00.500+08:00,60.1,16.4,1030\n\
broken line without commas\n\
2026-08-07T00:00:01.000+08:00,abc,16.0,1000\n\
not-a-time,60.2,17.0,1010\n\
2026-08-07T00:00:02.000+08:00,58.2,17.1,1020\n";
        let mut lf = lf_from(content);
        assert_eq!(lf.bad_lines, 3, "load 时全量校验计数");
        assert!(lf.note.contains("skipped 3 bad lines"));
        let mut rec = Rec::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let total = parse_file(&mut lf, &mut rec, &cancel).unwrap();
        assert_eq!(total, 3 * 3, "3 行好行 × 3 指标");
        assert_eq!(lf.bad_lines, 6, "parse 再跑一遍计数累计");
        assert!(lf.bad_samples.len() <= 10);
        assert!(lf
            .bad_samples
            .iter()
            .any(|(_, r)| r == "column count mismatch"));
    }

    #[test]
    fn quoted_delimiter_inside_field() {
        let content = "timestamp,fps,note\n2026-08-07T00:00:00.000+08:00,59.8,\"a,b\"\n";
        let lf = lf_from(content);
        assert_eq!(lf.metrics.len(), 1);
        assert_eq!(lf.note, "comma-separated, header row detected");
    }

    #[test]
    fn key_values_low_cardinality_and_t_bound() {
        let content = "timestamp,fps,scene,mem_mb\n\
2026-08-07T00:00:01.000+08:00,60.0,menu,1000\n\
2026-08-07T00:00:02.000+08:00,60.0,boss,1000\n\
2026-08-07T00:00:03.000+08:00,60.0,boss,1000\n\
2026-08-07T00:00:04.000+08:00,60.0,menu,1000\n";
        let mut lf = lf_from(content);
        let mut rec = Rec::default();
        let cancel = Arc::new(AtomicBool::new(false));
        parse_file(&mut lf, &mut rec, &cancel).unwrap();
        let t3 = parse_time("2026-08-07T00:00:03.000+08:00", &TimeFormat::Auto).unwrap();
        let r = key_values(&lf, t3);
        let map: HashMap<&str, &str> = r
            .entries
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_str().unwrap_or("")))
            .collect();
        assert_eq!(map.get("scene"), Some(&"boss"));
        let t1 = parse_time("2026-08-07T00:00:01.500+08:00", &TimeFormat::Auto).unwrap();
        let r = key_values(&lf, t1);
        assert_eq!(r.entries[0].value, serde_json::Value::String("menu".into()));
        assert!(
            !r.entries.iter().any(|e| e.key == "fps"),
            "数值列不出 key_values"
        );
    }

    #[test]
    fn key_values_cardinality_above_10_excluded() {
        let mut scene_col = String::new();
        for i in 0..12 {
            let ts = format!("2026-08-07T00:00:{i:02}.000+08:00");
            scene_col.push_str(&format!("{ts},60.0,s{i}\n"));
        }
        let mut lf = lf_from(&format!("timestamp,fps,scene\n{scene_col}"));
        let mut rec = Rec::default();
        let cancel = Arc::new(AtomicBool::new(false));
        parse_file(&mut lf, &mut rec, &cancel).unwrap();
        let r = key_values(&lf, i64::MAX);
        assert!(r.entries.is_empty(), "高基数列不产出 key_values");
    }

    #[test]
    fn can_handle_scoring_branches() {
        let cfg = cfg();
        let fps: Vec<String> = vec!["timestamp,".to_string(), "time,".to_string()];
        let params = |ext: &str, head: &str| CanHandleParams {
            path: "x".into(),
            name: format!("a.{ext}"),
            ext: ext.to_string(),
            size_bytes: 100,
            head_sample: head.to_string(),
        };
        // 弃权：其它扩展名。
        let r = can_handle(&params("bin", "anything"), &cfg, &fps);
        assert!(!r.can_handle && r.confidence == 0.0);
        // csv 基础 0.5 + 列数 0.2 + 时间列 0.2 + 指纹 0.1 = 1.0（封顶，浮点容差）。
        let head = "timestamp,fps,frame_ms\n2026-08-07T00:00:00.000+08:00,59.8,16.6\n";
        let r = can_handle(&params("csv", head), &cfg, &fps);
        assert!(r.can_handle);
        assert!((r.confidence - 1.0).abs() < 1e-9, "{}", r.confidence);
        assert!(r
            .reason
            .as_deref()
            .unwrap()
            .contains("time column 'timestamp'"));
        // txt 基础 0.3 + 列数 0.2 + 时间列 0.2 + 指纹 0.1 = 0.8。
        let r = can_handle(&params("txt", head), &cfg, &fps);
        assert!((r.confidence - 0.8).abs() < 1e-9, "{}", r.confidence);
        // 仅列数达标（无表头、无时间列可识别）：csv 0.5 + 0.2 = 0.7。
        let head2 = "a,b,c\nhello,2,3\n";
        let r = can_handle(&params("csv", head2), &cfg, &fps);
        assert!((r.confidence - 0.7).abs() < 1e-9, "{}", r.confidence);
        // 无表头 + 首列时间：csv 0.5 + 0.2 + 0.2 = 0.9。
        let head3 = "2026-08-07T00:00:00.000+08:00,59.8,16.6\n";
        let r = can_handle(&params("csv", head3), &cfg, &fps);
        assert!((r.confidence - 0.9).abs() < 1e-9, "{}", r.confidence);
    }

    #[test]
    fn can_handle_timing_below_three_seconds() {
        let cfg = cfg();
        let fps: Vec<String> = vec!["timestamp,".to_string()];
        let head = "timestamp,fps,frame_ms\n2026-08-07T00:00:00.000+08:00,59.8,16.6\n";
        let t0 = Instant::now();
        for _ in 0..1000 {
            can_handle(
                &CanHandleParams {
                    path: "x".into(),
                    name: "a.csv".into(),
                    ext: "csv".into(),
                    size_bytes: 100,
                    head_sample: head.to_string(),
                },
                &cfg,
                &fps,
            );
        }
        assert!(t0.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn encodings_decode_without_crash() {
        // UTF-8 BOM
        let mut raw = vec![0xEF, 0xBB, 0xBF];
        raw.extend_from_slice(b"timestamp,fps\n2026-08-07T00:00:00.000+08:00,60.0\n");
        let (s, notes) = decode(raw, &Encoding::Auto);
        assert!(s.starts_with("timestamp"));
        assert!(notes.iter().any(|n| n.contains("BOM")));
        // UTF-16LE（显式模式）
        let utf16: Vec<u16> = "timestamp,fps\n2026-08-07T00:00:00.000+08:00,60.0\n"
            .encode_utf16()
            .collect();
        let mut raw16 = vec![0xFF, 0xFE];
        for u in &utf16 {
            raw16.extend_from_slice(&u.to_le_bytes());
        }
        let (s, _) = decode(raw16.clone(), &Encoding::Utf16Le);
        assert!(s.starts_with("timestamp"));
        // GBK（显式模式；中文列名）
        let (gbk_cow, _enc, _had_errors) = encoding_rs::GBK
            .encode("timestamp,fps,备注\n2026-08-07T00:00:00.000+08:00,60.0,正常\n");
        let gbk_bytes = gbk_cow.into_owned();
        let (s, notes) = decode(gbk_bytes.clone(), &Encoding::Gbk);
        assert!(s.contains("备注"));
        assert!(notes.iter().any(|n| n.contains("GBK")));
        // Auto 正确识别 UTF-16LE BOM（不再宽松乱码）。
        let (s, notes) = decode(raw16, &Encoding::Auto);
        assert!(s.starts_with("timestamp"));
        assert!(notes.iter().any(|n| n.contains("UTF-16")));
        // Auto 正确识别 GBK（不再静默 U+FFFD 有损）。
        let (s, notes) = decode(gbk_bytes, &Encoding::Auto);
        assert!(s.contains("备注"));
        assert!(!s.contains('\u{FFFD}'));
        assert!(notes.iter().any(|n| n.contains("GBK")));
    }

    #[test]
    fn decode_auto_utf16be_bom() {
        let text = "timestamp,fps\n2026-08-07T00:00:00.000+08:00,60.0\n";
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let mut raw = vec![0xFE, 0xFF];
        for u in &utf16 {
            raw.extend_from_slice(&u.to_be_bytes());
        }
        let (s, notes) = decode(raw, &Encoding::Auto);
        assert!(s.starts_with("timestamp"));
        assert!(s.contains("2026-08-07"));
        assert!(notes.iter().any(|n| n.contains("UTF-16")));
    }

    #[test]
    fn decode_auto_strict_utf8_without_bom() {
        let raw = "timestamp,fps\n2026-08-07T00:00:00.000+08:00,60.0\n"
            .as_bytes()
            .to_vec();
        let (s, notes) = decode(raw, &Encoding::Auto);
        assert_eq!(s, "timestamp,fps\n2026-08-07T00:00:00.000+08:00,60.0\n");
        assert!(notes.is_empty(), "纯 UTF-8 无 BOM 不记 note: {notes:?}");
    }

    #[test]
    fn decode_auto_lossy_fallback_notes_it() {
        // 既非 UTF-8 也非 GBK 可完整解码的字节 → 有损回落 + note。
        let raw = vec![0x80, 0xFF, 0x41, 0x42];
        let (s, notes) = decode(raw, &Encoding::Auto);
        assert!(s.contains('\u{FFFD}'));
        assert!(
            notes.iter().any(|n| n.contains("fallback") || n.contains("lossy")),
            "notes: {notes:?}"
        );
    }

    #[test]
    fn decode_utf8_path_moves_buffer_no_copy() {
        // UTF-8（无 BOM）路径零拷贝：返回 String 与原缓冲同一指针（无整文件第二份副本）。
        let raw = "timestamp,fps\n2026-08-07T00:00:00.000+08:00,60.0\n"
            .as_bytes()
            .to_vec();
        let ptr = raw.as_ptr();
        let (s, _) = decode(raw, &Encoding::Auto);
        assert_eq!(s.as_ptr(), ptr);
    }

    #[test]
    fn load_content_holds_decoded_string_without_clone() {
        let content = "timestamp,fps,scene\n2026-08-07T00:00:00.000+08:00,60.0,menu\n".to_string();
        let ptr = content.as_ptr();
        let lf = load_content("f1", "a.csv", content, &cfg());
        assert_eq!(
            lf.content.as_ptr(),
            ptr,
            "content 必须直接持有解码结果（load_content 不再 to_string 克隆）"
        );
        assert_eq!(lf.content_bytes, lf.content.len());
    }

    #[test]
    fn load_file_content_exact_no_duplicate() {
        // 完整 fs 读 + 解码路径：content 与文件字节一一对应，且无第二份副本（容量不翻倍）。
        let bytes = b"timestamp,fps\n2026-08-07T00:00:00.000+08:00,60.0\n".to_vec();
        let dir = std::env::temp_dir().join(format!("ab-csv-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("load_roundtrip.csv");
        std::fs::write(&path, &bytes).unwrap();
        let lf = load_file("f1", path.to_str().unwrap(), &cfg()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(lf.content.len(), bytes.len());
        assert_eq!(lf.content_bytes, bytes.len());
        assert_eq!(lf.content, String::from_utf8(bytes).unwrap());
    }

    #[test]
    fn load_file_gbk_auto_detects_encoding() {
        let (gbk_cow, _enc, _had_errors) = encoding_rs::GBK
            .encode("timestamp,fps,备注\n2026-08-07T00:00:00.000+08:00,60.0,正常\n");
        let bytes = gbk_cow.into_owned();
        let dir = std::env::temp_dir().join(format!("ab-csv-gbk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gbk_auto.csv");
        std::fs::write(&path, &bytes).unwrap();
        let lf = load_file("f1", path.to_str().unwrap(), &cfg()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(lf.content.contains("备注"), "Auto 正确识别 GBK: {}", lf.content);
        assert!(!lf.content.contains('\u{FFFD}'));
        assert!(lf.note.contains("GBK"), "note: {}", lf.note);
    }

    #[test]
    fn time_range_and_record_count_hint() {
        let content = "timestamp,fps\n\
2026-08-07T00:00:00.000+08:00,60.0\n\
2026-08-07T00:01:00.000+08:00,61.0\n";
        let lf = lf_from(content);
        let tr = lf.time_range.unwrap();
        assert_eq!(
            tr.start_ms,
            parse_time("2026-08-07T00:00:00.000+08:00", &TimeFormat::Auto).unwrap()
        );
        assert_eq!(
            tr.end_ms,
            parse_time("2026-08-07T00:01:00.000+08:00", &TimeFormat::Auto).unwrap()
        );
        assert_eq!(lf.record_count_hint, Some(2));
    }

    #[test]
    fn cancellation_returns_cancelled() {
        let mut content = HEADER.to_string();
        for i in 0..100000 {
            let hh = i / 3600;
            let mm = (i / 60) % 60;
            let ss = i % 60;
            let ts = format!("2026-08-07T{hh:02}:{mm:02}:{ss:02}.000+08:00");
            content.push_str(&format!("{ts},60.0,16.0,1000\n"));
        }
        let mut lf = lf_from(&content);
        let mut rec = Rec::default();
        let cancel = Arc::new(AtomicBool::new(false));
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            parse_file(&mut lf, &mut rec, &cancel),
            Err(ParseError::Cancelled)
        ));
    }
}
