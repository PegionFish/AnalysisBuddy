//! SoA 列式存储（pipeline.md §2）。
//!
//! 乱序定稿三段式（§2.2）：parse 期间以 `Ingesting` 态逐批追加（O(1) 摊还），
//! parse done 后 `freeze` 做一次配对稳定排序并置 `Frozen` 只读；查询路径
//! 不再加写锁。tags / raw_line 走旁路稀疏表（§2.3）：raw_line 按固定步幅
//! 抽样保留（≤1%），tags 默认全保留但单文件上限 100,000 条。卸载即 drop（§2.5）。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};

use ab_protocol::types::{FileSummary, RecordBatch, TimeRange};

/// 一条 `(file_id, metric)` 组合的时序列（pipeline.md §2.1）。
///
/// 按值构造/读取以支持双份共享（`Arc<Series>`）：ingest 期以 `Arc::make_mut`
/// 原地追加，`freeze` 后不再变写，查询侧直接克隆 `Arc` 零拷贝。
#[derive(Debug, Clone, Default)]
pub struct Series {
    /// UTC 毫秒；`freeze` 后严格非降序。
    pub ts: Vec<i64>,
    /// 与 `ts` 等长、同下标对应。
    pub values: Vec<f64>,
}

/// tags / raw_line 旁路稀疏表（pipeline.md §2.3）。
///
/// 键为 freeze 排序后的最终 `point_index`（ingest 期暂用批内追加序下标，
/// `freeze` 时随主序列同步重排）。
#[derive(Debug, Clone, Default)]
pub struct SparseSideTable {
    /// point_index → tags
    pub tags: HashMap<u32, BTreeMap<String, String>>,
    /// point_index → 原文（抽样保留，≤1%）
    pub raw_line: HashMap<u32, String>,
}

/// 文件状态机（pipeline.md §2.2）：`Registered → Ingesting → Frozen`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Registered,
    Ingesting,
    Frozen,
}

/// `append_batch` 返回统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendStats {
    /// 实际入库（通过白名单）的记录数。
    pub appended: u64,
    /// 未声明 metric 被丢弃的记录数。
    pub dropped_undeclared: u64,
}

/// 解析告警计数（随 `PipelineEvent::ParseCompleted` 上报 UI，pipeline.md §6）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseWarnings {
    pub dropped_undeclared: u64,
    pub dropped_tags: u64,
}

/// raw_line 抽样步幅：保留当且仅当 `counter % stride == 0`（pipeline.md §2.3），
/// `stride = max(100, 1 / 0.01)`，即抽样率 ≤1%。
const RAW_LINE_STRIDE: u64 = 100;

/// 单文件 tags 总条目上限（pipeline.md §2.3）。
const MAX_TAGS_PER_FILE: u64 = 100_000;

/// Store 操作错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// 未注册的 file_id。
    UnknownFile(String),
    /// 重复注册。
    AlreadyRegistered(String),
    /// 向非 Ingesting 态文件追加批次。
    NotIngesting(String),
    /// 对已 Frozen 文件再次 freeze。
    AlreadyFrozen(String),
    /// 批次的 file_id 与调用方不一致（协议错）。
    BatchFileMismatch { expected: String, got: String },
    /// seq 缺号（协议错，protocol.md §3.2）。
    SeqGap { expected: u64, got: u64 },
    /// seq 重复（协议错，protocol.md §3.2）。
    SeqDuplicate { expected: u64, got: u64 },
    /// `freeze` 时 `records_total` ≠ Σ各批 len（协议错，protocol.md §3.2）。
    CountMismatch { declared: u64, received: u64 },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::UnknownFile(id) => write!(f, "unknown file: {id}"),
            StoreError::AlreadyRegistered(id) => write!(f, "file already registered: {id}"),
            StoreError::NotIngesting(id) => write!(f, "file is not in Ingesting state: {id}"),
            StoreError::AlreadyFrozen(id) => write!(f, "file already frozen: {id}"),
            StoreError::BatchFileMismatch { expected, got } => {
                write!(f, "batch file_id mismatch: expected {expected}, got {got}")
            }
            StoreError::SeqGap { expected, got } => {
                write!(f, "sequence gap: expected seq {expected}, got {got}")
            }
            StoreError::SeqDuplicate { expected, got } => {
                write!(f, "duplicate sequence: expected seq {expected}, got {got}")
            }
            StoreError::CountMismatch { declared, received } => write!(
                f,
                "records_total mismatch: declared {declared}, received {received}"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

/// 单文件内存数据（内部私有结构）。
struct FileData {
    state: FileState,
    summary: Option<FileSummary>,
    metric_whitelist: HashSet<String>,
    /// metric → 时序列（ingest 期经 `Arc::make_mut` 原地追加）。
    series: HashMap<String, Arc<Series>>,
    /// metric → 旁路稀疏表。
    side: HashMap<String, Arc<SparseSideTable>>,
    /// metric → 携带 raw_line 的记录计数器（抽样步幅计数）。
    raw_counters: HashMap<String, u64>,
    /// 单文件 tags 总条目计数（上限 [`MAX_TAGS_PER_FILE`]）。
    tags_total: u64,
    dropped_undeclared: u64,
    dropped_tags: u64,
    /// Σ各批 `records.len()`，`freeze` 时与 `records_total` 比对。
    records_received: u64,
    /// 期望的下一批 seq（从 0 起单调递增）。
    seq_next: u64,
}

impl FileData {
    fn new(summary: Option<FileSummary>, metric_whitelist: &[String]) -> Self {
        FileData {
            state: FileState::Registered,
            summary,
            metric_whitelist: metric_whitelist.iter().cloned().collect(),
            series: HashMap::new(),
            side: HashMap::new(),
            raw_counters: HashMap::new(),
            tags_total: 0,
            dropped_undeclared: 0,
            dropped_tags: 0,
            records_received: 0,
            seq_next: 0,
        }
    }

    /// 追加一条已通过白名单的记录，返回该点在主序列中的当前下标。
    fn append_record(&mut self, metric: &str, timestamp: i64, value: f64) -> u32 {
        let entry = self
            .series
            .entry(metric.to_string())
            .or_insert_with(|| Arc::new(Series::default()));
        let series = Arc::make_mut(entry);
        series.ts.push(timestamp);
        series.values.push(value);
        (series.ts.len() - 1) as u32
    }

    /// 旁路表按当前（未排序）下标写入；`raw_line` 按步幅抽样，`tags` 按文件上限。
    fn append_side(
        &mut self,
        metric: &str,
        point: u32,
        raw_line: Option<&str>,
        tags: Option<&BTreeMap<String, String>>,
    ) {
        if let Some(line) = raw_line {
            if !line.is_empty() {
                let counter = self.raw_counters.entry(metric.to_string()).or_insert(0);
                *counter += 1;
                if (*counter).is_multiple_of(RAW_LINE_STRIDE) {
                    let table = self
                        .side
                        .entry(metric.to_string())
                        .or_insert_with(|| Arc::new(SparseSideTable::default()));
                    Arc::make_mut(table)
                        .raw_line
                        .insert(point, line.to_string());
                }
            }
        }
        if let Some(map) = tags {
            if !map.is_empty() {
                let len = map.len() as u64;
                if self.tags_total + len <= MAX_TAGS_PER_FILE {
                    let table = self
                        .side
                        .entry(metric.to_string())
                        .or_insert_with(|| Arc::new(SparseSideTable::default()));
                    Arc::make_mut(table).tags.insert(point, map.clone());
                    self.tags_total += len;
                } else {
                    self.dropped_tags += len;
                }
            }
        }
    }

    /// 冻结排序：对每条 Series 做 (ts, values) 配对稳定排序，旁路键随置换重排。
    fn freeze_sort(&mut self) {
        for (metric, series_arc) in &mut self.series {
            let series = Arc::make_mut(series_arc);
            let n = series.ts.len();
            if n == 0 {
                continue;
            }
            // 稳定排序等价写法：以 (ts, 原下标) 为键做不稳定排序，结果与
            // 稳定排序一致（键全序），避免 `stable_sort_primitive` 告警。
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_unstable_by_key(|&i| (series.ts[i], i));
            let new_ts: Vec<i64> = order.iter().map(|&i| series.ts[i]).collect();
            let new_values: Vec<f64> = order.iter().map(|&i| series.values[i]).collect();
            series.ts = new_ts;
            series.values = new_values;
            if let Some(side_arc) = self.side.get_mut(metric) {
                let table = Arc::make_mut(side_arc);
                let mut new_pos = vec![0u32; n];
                for (pos, &old) in order.iter().enumerate() {
                    new_pos[old] = pos as u32;
                }
                table.tags = table
                    .tags
                    .iter()
                    .map(|(&k, v)| (new_pos[k as usize], v.clone()))
                    .collect();
                table.raw_line = table
                    .raw_line
                    .iter()
                    .map(|(&k, v)| (new_pos[k as usize], v.clone()))
                    .collect();
            }
        }
    }
}

/// 内存存储（pipeline.md §2）。
///
/// 内部 `RwLock<HashMap<file_id, FileData>>`；`Frozen` 之后查询路径只取读锁，
/// 直接二分，不再有写操作。
pub struct Store {
    inner: RwLock<HashMap<String, FileData>>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Store {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// 注册文件并声明 metric 白名单（来自插件 `schema()`，pipeline.md §1.1）。
    pub fn register(
        &self,
        file_id: &str,
        summary: Option<FileSummary>,
        metric_whitelist: &[String],
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        if inner.contains_key(file_id) {
            return Err(StoreError::AlreadyRegistered(file_id.to_string()));
        }
        inner.insert(
            file_id.to_string(),
            FileData::new(summary, metric_whitelist),
        );
        Ok(())
    }

    /// 追加一批记录：校验 seq 连续性（缺号/重复 = 协议错）与 metric 白名单
    /// （越界记录丢弃并计数，不中断，protocol.md §2.5）。
    pub fn append_batch(
        &self,
        file_id: &str,
        batch: RecordBatch,
    ) -> Result<AppendStats, StoreError> {
        if batch.file_id != file_id {
            return Err(StoreError::BatchFileMismatch {
                expected: file_id.to_string(),
                got: batch.file_id,
            });
        }
        let mut inner = self.inner.write().unwrap();
        let Some(data) = inner.get_mut(file_id) else {
            return Err(StoreError::UnknownFile(file_id.to_string()));
        };
        if data.state == FileState::Frozen {
            return Err(StoreError::NotIngesting(file_id.to_string()));
        }
        if batch.seq != data.seq_next {
            return if batch.seq > data.seq_next {
                Err(StoreError::SeqGap {
                    expected: data.seq_next,
                    got: batch.seq,
                })
            } else {
                Err(StoreError::SeqDuplicate {
                    expected: data.seq_next,
                    got: batch.seq,
                })
            };
        }
        data.state = FileState::Ingesting;
        let mut appended = 0u64;
        for record in &batch.records {
            if !data.metric_whitelist.contains(&record.metric) {
                data.dropped_undeclared += 1;
                continue;
            }
            let point = data.append_record(&record.metric, record.timestamp, record.value);
            data.append_side(
                &record.metric,
                point,
                record.raw_line.as_deref(),
                record.tags.as_ref(),
            );
            appended += 1;
        }
        data.seq_next += 1;
        data.records_received += batch.records.len() as u64;
        Ok(AppendStats {
            appended,
            dropped_undeclared: data.dropped_undeclared,
        })
    }

    /// parse done 校验通过后调用（pipeline.md §2.2）：配对稳定排序 + 旁路下标
    /// 重排 + 置 `Frozen`。`records_total` 与 Σ各批 len 不一致返回
    /// `CountMismatch`（调用方应丢弃该文件数据）。
    pub fn freeze(&self, file_id: &str, records_total: u64) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let Some(data) = inner.get_mut(file_id) else {
            return Err(StoreError::UnknownFile(file_id.to_string()));
        };
        if data.state == FileState::Frozen {
            return Err(StoreError::AlreadyFrozen(file_id.to_string()));
        }
        if data.records_received != records_total {
            return Err(StoreError::CountMismatch {
                declared: records_total,
                received: data.records_received,
            });
        }
        data.freeze_sort();
        data.state = FileState::Frozen;
        Ok(())
    }

    /// 时间范围：`Frozen` 文件取数据实际 min/max；未冻结时回退到
    /// `FileSummary.time_range` 预估（若有）。
    pub fn time_range(&self, file_id: &str) -> Option<TimeRange> {
        let inner = self.inner.read().unwrap();
        let data = inner.get(file_id)?;
        if data.state == FileState::Frozen {
            let mut min = None;
            let mut max = None;
            for series in data.series.values() {
                if let (Some(&first), Some(&last)) = (series.ts.first(), series.ts.last()) {
                    min = Some(min.map_or(first, |m: i64| m.min(first)));
                    max = Some(max.map_or(last, |m: i64| m.max(last)));
                }
            }
            if let (Some(start_ms), Some(end_ms)) = (min, max) {
                return Some(TimeRange { start_ms, end_ms });
            }
        }
        data.summary.as_ref().and_then(|s| s.time_range)
    }

    /// 该文件的全部 metric id（字典序，确定性输出）。
    pub fn metrics_of(&self, file_id: &str) -> Vec<String> {
        let inner = self.inner.read().unwrap();
        let mut metrics: Vec<String> = inner
            .get(file_id)
            .map(|d| d.series.keys().cloned().collect())
            .unwrap_or_default();
        metrics.sort();
        metrics
    }

    /// 取 metric 的旁路稀疏表（Arc 共享，零拷贝）。
    pub fn side_table(&self, file_id: &str, metric: &str) -> Option<Arc<SparseSideTable>> {
        let inner = self.inner.read().unwrap();
        inner.get(file_id).and_then(|d| d.side.get(metric)).cloned()
    }

    /// 解析告警计数（coordinator 组装 `ParseCompleted` 事件用）。
    pub fn warnings(&self, file_id: &str) -> Option<ParseWarnings> {
        let inner = self.inner.read().unwrap();
        inner.get(file_id).map(|d| ParseWarnings {
            dropped_undeclared: d.dropped_undeclared,
            dropped_tags: d.dropped_tags,
        })
    }

    /// 卸载即 drop（pipeline.md §2.5）：移除 `FileData`，RAII 即刻归还内存。
    pub fn unload(&self, file_id: &str) {
        self.inner.write().unwrap().remove(file_id);
    }

    /// 预算化查询（pipeline.md §6 Store API；实现见 [`crate::query`]）。
    pub fn query(&self, q: &crate::query::QueryRequest) -> Vec<crate::query::SeriesSlice> {
        crate::query::run_query(self, q)
    }

    /// 查询用：仅返回 `Frozen` 文件的指定序列（`Arc` 共享，零拷贝）；非
    /// `Frozen` 或未注册返回 `None`（pipeline.md §3.1「仅接受 Frozen」）。
    pub fn frozen_series(&self, file_id: &str, metric: &str) -> Option<Arc<Series>> {
        let inner = self.inner.read().unwrap();
        let data = inner.get(file_id)?;
        if data.state != FileState::Frozen {
            return None;
        }
        data.series.get(metric).cloned()
    }
}
