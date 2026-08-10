//! RPC 请求 / 响应 / 通知类型（protocol.md §2、§3）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// §2.1 initialize
// ---------------------------------------------------------------------------

/// §2.1 `initialize` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// 协议版本；当前固定为 [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION)（= 1）。
    pub protocol_version: u32,
    /// 宿主自报身份。
    pub host_info: HostInfo,
}

/// §2.1 宿主身份信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostInfo {
    /// 宿主名。
    pub name: String,
    /// 宿主版本。
    pub version: String,
}

/// §2.1 `initialize` 响应（插件元数据 + 能力声明）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// 插件唯一 id；必须与 manifest `id` 一致。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 插件版本（semver 字符串）。
    pub version: String,
    /// 能力声明。
    pub capabilities: Capabilities,
}

/// §2.1 能力声明。manifest 不声明能力，能力唯一来源是这里。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    /// 是否实现 `annotate`。
    pub annotate: bool,
    /// 是否实现实时订阅（v1 恒为 `false`，占位）。
    pub subscribe: bool,
    /// 是否支持二进制旁路（v1 恒为 `false`，v1.1 扩展位）。
    pub binary_sidecar: bool,
}

// ---------------------------------------------------------------------------
// §2.2 can_handle
// ---------------------------------------------------------------------------

/// §2.2 `can_handle` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanHandleParams {
    /// 文件绝对路径（Windows 路径）。
    pub path: String,
    /// 文件名（含扩展名）。
    pub name: String,
    /// 扩展名（小写、不含点；无后缀为 `""`）。
    pub ext: String,
    /// 文件字节数。
    pub size_bytes: u64,
    /// 头部采样：前 4 KB 文本（UTF-8 宽松解码，非法字节替换为 U+FFFD）。
    pub head_sample: String,
}

/// §2.2 `can_handle` 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanHandleResult {
    /// 是否认领该文件。
    pub can_handle: bool,
    /// 置信度，闭区间 `[0, 1]`；多插件同时认领时宿主取最高者。
    pub confidence: f64,
    /// 可选：人类可读的判定理由（用于 UI 展示）。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// §2.3 load_file
// ---------------------------------------------------------------------------

/// §2.3 `load_file` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadFileParams {
    /// 宿主分配的会话内文件唯一 id（UUID v4 字符串）；后续所有方法以此关联。
    pub file_id: String,
    /// 文件绝对路径；插件在此读取并驻留原始数据。
    pub path: String,
}

/// §2.3 `load_file` 响应（文件级摘要）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSummary {
    /// 可选：预估记录条数（可为粗略值，供 UI 展示与内存预估）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_count_hint: Option<u64>,
    /// 可选：预估时间范围（UTC 毫秒）；未知可省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRange>,
    /// 可选：任意备注。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub note: Option<String>,
}

/// §2.3 时间范围（UTC 毫秒，闭区间）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    /// 范围起点（UTC 毫秒）。
    pub start_ms: i64,
    /// 范围终点（UTC 毫秒）。
    pub end_ms: i64,
}

// ---------------------------------------------------------------------------
// §2.4 parse
// ---------------------------------------------------------------------------

/// §2.4 `parse` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParseParams {
    /// 已 load 的文件。
    pub file_id: String,
    /// 可选：预留解析选项（v1 宿主不传；插件收到非空 map 应忽略未知键）。
    #[serde(skip_serializing_if = "crate::skip_if_empty_json_map")]
    pub options: Option<serde_json::Map<String, serde_json::Value>>,
}

/// §2.4 `parse` 响应（在全部数据回传完成后才发出；数据本身走 notification）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParseResult {
    /// 本次解析产出的 Record 总条数。
    pub records_total: u64,
}

// ---------------------------------------------------------------------------
// §2.5 schema
// ---------------------------------------------------------------------------

/// §2.5 `schema` 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaResult {
    /// 本插件产出的全部指标。
    pub metrics: Vec<MetricDef>,
}

/// §2.5 指标定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDef {
    /// 指标 id，即 `Record.metric` 的取值域；会话内唯一。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 可选：单位。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub unit: Option<String>,
    /// 可选：描述。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub description: Option<String>,
    /// 降采样/合并聚合方式。
    pub aggregation: Aggregation,
}

/// §2.5 聚合方式枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregation {
    /// 取最后值（状态类）。
    Last,
    /// 求和（计数类）。
    Sum,
    /// 取平均。
    Avg,
    /// 取最小值。
    Min,
    /// 取最大值。
    Max,
}

// ---------------------------------------------------------------------------
// §2.6 key_values
// ---------------------------------------------------------------------------

/// §2.6 `key_values` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyValuesParams {
    /// 已 load 的文件。
    pub file_id: String,
    /// 游标时刻 T（UTC 毫秒）。
    pub timestamp_ms: i64,
}

/// §2.6 `key_values` 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyValuesResult {
    /// 该文件在 T 处的关键状态值集合；语义由插件自定义，通常取 ≤T 的最新状态。
    pub entries: Vec<KeyValueEntry>,
}

/// §2.6 关键状态值条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyValueEntry {
    /// 状态名。
    pub key: String,
    /// 状态值。
    ///
    /// 契约（protocol.md §2.6）约定为 string / number / boolean 三类标量；Rust 侧
    /// 用 `serde_json::Value` 承载，不做运行时形状校验（`rpc-messages.schema.json`
    /// 对 result 亦无逐方法深层形状定义，见 schema-errata E-01），插件应自律只发
    /// 标量，不得放入对象或数组。
    pub value: serde_json::Value,
    /// 可选：单位。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub unit: Option<String>,
}

// ---------------------------------------------------------------------------
// §2.7 annotate（可选能力）
// ---------------------------------------------------------------------------

/// §2.7 `annotate` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotateParams {
    /// 已 load 的文件。
    pub file_id: String,
    /// 时间范围（UTC 毫秒，闭区间）。
    pub range: TimeRange,
}

/// §2.7 `annotate` 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotateResult {
    /// 范围内事件/标记，用于折线图上打点；无事件时返回空数组。
    pub events: Vec<AnnotateEvent>,
}

/// §2.7 事件标记。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotateEvent {
    /// 事件时刻（UTC 毫秒）。
    pub timestamp_ms: i64,
    /// 事件文案。
    pub label: String,
    /// 可选：级别（`"info" | "warn" | "error"` 或插件自定义）。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub level: Option<String>,
}

// ---------------------------------------------------------------------------
// §2.8 unload_file / §3.4 cancel_parse
// ---------------------------------------------------------------------------

/// §2.8 `unload_file` 请求参数。结果为空对象；要求幂等。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnloadFileParams {
    /// 关联文件。
    pub file_id: String,
}

/// §3.4 `cancel_parse` 请求参数。结果为空对象；对未在解析的 `file_id` 同样回 `{}`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelParseParams {
    /// 关联文件。
    pub file_id: String,
}

// ---------------------------------------------------------------------------
// §3.1 Record / §3.2 RecordBatch / §3.3 progress
// ---------------------------------------------------------------------------

/// §3.1 归一化记录。
///
/// 可选字段序列化约定（skip if empty）：`level` / `tags` / `raw_line` 为空时
/// 整体省略该键，禁止输出 `null` 或空容器。`value` 为非有限数（`NaN` /
/// `±Infinity`）时序列化报错（JSON 中不可表示），插件侧自行过滤或置 0。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// UTC 毫秒。
    pub timestamp: i64,
    /// 指标 id；必须属于 `schema().metrics[].id`。
    pub metric: String,
    /// 数值；非有限数（`NaN` / `±Infinity`）序列化报错。
    #[serde(serialize_with = "serialize_finite_f64")]
    pub value: f64,
    /// 可选：级别（如 `"info" / "warn" / "error"`）。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub level: Option<String>,
    /// 可选：维度标签。
    #[serde(skip_serializing_if = "crate::skip_if_empty_map")]
    pub tags: Option<BTreeMap<String, String>>,
    /// 可选：原文引用（抽样保留以控制内存）。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub raw_line: Option<String>,
}

/// §3.2 `RecordBatch` 通知参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordBatch {
    /// 关联文件。
    pub file_id: String,
    /// 本次 parse 的批序号，从 `0` 单调递增。
    pub seq: u64,
    /// 本批记录。
    pub records: Vec<Record>,
    /// 是否为末批；末批 `records` 可为空数组。
    pub done: bool,
}

/// §3.3 `progress` 通知参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressParams {
    /// 关联文件。
    pub file_id: String,
    /// 可选：进度 `[0, 100]`；无法估算时省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// 已产出记录数。
    pub records_so_far: u64,
    /// 可选：已读字节数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_read: Option<u64>,
}

/// `Record.value` 专用序列化器：非有限数（`NaN` / `±Infinity`）直接报错。
///
/// serde_json ≥ 1.0.132 对非有限 `f64` 默认输出 `null`（静默），与契约
/// 「`NaN`/`±Infinity` 禁止输出」相悖；此处显式报错，保证行为不随
/// serde_json 版本漂移（插件侧仍需自行过滤或置 0）。
fn serialize_finite_f64<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    if value.is_finite() {
        serializer.serialize_f64(*value)
    } else {
        Err(serde::ser::Error::custom(
            "Record.value must be finite (NaN/Infinity is not representable in JSON)",
        ))
    }
}
