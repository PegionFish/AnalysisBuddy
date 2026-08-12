//! 命令层 DTO 与 `IpcError`（ipc-ui.md §1.0 错误形状）。
//!
//! `IpcError` 为全部 command 的 reject 值形状；`key_values_at` 除外——它走
//! 部分失败协议（§1.6），逐文件错误进入结果项，整体永不 reject。
//! 错误映射唯一实现见 [`crate::ipc_errors`]（§1.10 表）。

pub mod import;
pub mod plugin;
pub mod plugin_manager;
pub mod query;
pub mod session;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 统一错误形状（ipc-ui.md §1.0 `IpcError`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl IpcError {
    pub fn invalid_arg(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_arg".to_string(),
            message: message.into(),
            data: None,
        }
    }
}

/// 手选覆盖入参（ipc-ui.md §1.2 `overrides`）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ImportOverride {
    pub plugin_id: String,
}

/// 匹配候选（§1.0 `PluginMatch`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginMatchDto {
    pub plugin_id: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 数据时间范围（UTC 毫秒闭区间；protocol-v1 §2.3 `TimeRange` 的 DTO 透传，
/// 任务 19：前端视口自动适配消费）。仅 DTO 透传，非契约变更。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeRangeDto {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// 前端提交的会话快照（save_session 入参 / load_session 返回透传；
/// 键可省略，空内容整体回落）。字段与 `SessionFile` 同形映射（契约 C1）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshotDto {
    /// file_id → metric 复合 id（`file_id:plugin_id:metric_id`）列表。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub selected_metrics: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub chart_view_state: Option<ChartViewStateDto>,
    #[serde(default)]
    pub cursor_ms: Option<i64>,
}

/// 图表视图状态（契约 C1）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartViewStateDto {
    /// 省略 = 全量。
    pub time_range: Option<TimeRangeDto>,
    #[serde(default)]
    pub legend_disabled: Vec<String>,
    /// "shared" | "per_series"（后端映射 YAxisScale；其他/缺省回落 Shared）。
    pub y_axis_scale: Option<String>,
}

/// `LoadResult` 逐文件时间范围（任务 19：会话重开后视口适配；
/// 空数组省略键）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileTimeRangeDto {
    pub file_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// 单文件导入结果（§1.0 `ImportResult`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportResultDto {
    pub file_id: String,
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_plugin: Option<PluginMatchDto>,
    pub candidate_plugins: Vec<PluginMatchDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_user_choice: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
    /// 就绪文件的实际数据时间范围（任务 19 视口适配；非 ready/未知省略键）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRangeDto>,
}

/// 插件信息（§1.0 `PluginInfo`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginInfoDto {
    pub id: String,
    pub display_name: String,
    pub version: String,
    /// `PluginState` 小写映射（`state_name`）。
    pub state: String,
    /// 该插件当前驻留的文件（宿主文件索引侧事实）。
    pub loaded_file_ids: Vec<String>,
    pub capabilities: CapabilitiesDto,
    /// 最近失败摘要；无则 `null`（§1.0 `last_error: string | null`）。
    pub last_error: Option<String>,
    /// 来源（§6.3）：`portable` / `user`。无效/影子模块不进发现列表，
    /// `invalid` 为保留值（发现列表扩展后使用）。
    pub source: String,
    /// 是否内建模块（`BUILTIN_PLUGIN_IDS` 判定；内建不可卸载/覆盖）。
    pub builtin: bool,
    /// 是否处于禁用状态（registry 禁用集合，spec §4.4）。
    pub disabled: bool,
    /// 更新源（manifest.update_url，§3.1）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_url: Option<String>,
    /// 作者（manifest.author，§7.2）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// 源码仓库地址（manifest.repository，§7.2）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// 宿主适配要求（manifest.tools，§7.2）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// 更新日志（manifest.changelog，§7.2）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<Vec<ab_protocol::manifest::ChangelogEntry>>,
}

/// `PluginInfoDto.source` 映射（§6.3）：Portable / InstallDir（ZIP 布局下
/// InstallDir 与 Portable 同路径）→ `portable`；UserData → `user`。
pub fn plugin_source_name(source: ab_host::PluginSource) -> &'static str {
    match source {
        ab_host::PluginSource::Portable | ab_host::PluginSource::InstallDir => "portable",
        ab_host::PluginSource::UserData => "user",
    }
}

/// 能力声明（§2.1 `Capabilities`；v1 未拉起插件无 initialize 结果，恒默认
/// `false`——v1 中 `subscribe`/`binary_sidecar` 协议恒 false，`annotate`
/// 在发现侧不可知）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CapabilitiesDto {
    pub annotate: bool,
    pub subscribe: bool,
    pub binary_sidecar: bool,
}

/// 会话保存结果（§1.0 `SessionMeta`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionMetaDto {
    /// 实际落盘的 `.absession` 绝对路径。
    pub path: String,
    /// UTC 毫秒。
    pub saved_at_ms: i64,
    pub file_count: usize,
    pub selected_metric_count: usize,
}

/// 缺失文件条目（§1.0 `MissingFileEntry`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MissingFileEntryDto {
    pub path: String,
    pub reason: &'static str,
}

/// 会话重开结果（§1.0 `LoadResult`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoadResultDto {
    pub session: SessionMetaDto,
    /// 校验通过并重新进入导入管线的文件。
    pub loaded_file_ids: Vec<String>,
    /// 缺失/校验失败文件（UI 标记）。
    pub missing: Vec<MissingFileEntryDto>,
    /// 重开失败（未达 Ready）文件（UI 提示；空则省略键）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reopen_failed: Vec<MissingFileEntryDto>,
    /// 重开成功文件的实际数据时间范围（任务 19 视口适配；空则省略键）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub time_ranges: Vec<FileTimeRangeDto>,
    /// 会话文件内保存的快照（契约 C1.3；文件内无快照字段时省略键）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SessionSnapshotDto>,
}

impl PluginInfoDto {
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        id: String,
        display_name: String,
        version: String,
        state: String,
        loaded_file_ids: Vec<String>,
        last_error: Option<String>,
        source: &'static str,
        builtin: bool,
        disabled: bool,
        update_url: Option<String>,
        author: Option<String>,
        repository: Option<String>,
        tools: Option<Vec<String>>,
        changelog: Option<Vec<ab_protocol::manifest::ChangelogEntry>>,
    ) -> Self {
        Self {
            id,
            display_name,
            version,
            state,
            loaded_file_ids,
            capabilities: CapabilitiesDto {
                annotate: false,
                subscribe: false,
                binary_sidecar: false,
            },
            last_error,
            source: source.to_string(),
            builtin,
            disabled,
            update_url,
            author,
            repository,
            tools,
            changelog,
        }
    }
}
