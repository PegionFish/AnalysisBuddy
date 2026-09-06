//! 查询类 Tauri command（ipc-ui.md §1.4/§1.5/§1.6）：`get_metrics`（文件→
//! 插件→指标三级树）、`query_series`（预算化降采样，`t0 > t1` reject
//! `invalid_arg`）、`key_values_at`（按文件并发扇出，部分失败逐项填 error，
//! 整体永不 reject）。逻辑体与查询 DTO 在 `ab_engine::commands::query`（M1）。

use std::sync::Arc;

use ab_engine::commands::IpcError;
use ab_engine::pipeline_bridge::ImportCoordinator;

pub use ab_engine::commands::query::{
    build_metric_tree, get_metrics_logic, key_values_at_logic, query_series_logic,
    KeyValueResultDto, MetricNodeDto, SeriesPointDto, SeriesSliceDto,
};

/// `get_metrics`（ipc-ui.md §1.4）：默认全部 Frozen 文件；仅文件全被卸载时返回空。
///
/// `rename_all = "snake_case"`：tauri-macros 默认把参数名转 camelCase 接收，
/// 而前端契约（ipc-ui.md）全 snake_case；不显式声明时 `file_ids` 等键静默
/// 落空/拒绝（任务 21 根因；command_arg_case_test 固化）。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_metrics(
    state: tauri::State<'_, Arc<ImportCoordinator>>,
    file_ids: Option<Vec<String>>,
) -> Result<Vec<MetricNodeDto>, IpcError> {
    Ok(get_metrics_logic(state.inner(), file_ids))
}

/// `query_series`（ipc-ui.md §1.5）：复合 id `file_id:plugin_id:metric_id`；
/// 仅查询 `file_ids` 内文件的序列（未授权文件静默跳过，与 mock/UI 一致）；
/// 未知/畸形 id 静默忽略并计数（宿主日志）；`t0 > t1` reject `invalid_arg`。
///
/// 任务 21 根因修复：必须 `rename_all = "snake_case"`——默认 camelCase 时
/// 前端传的 `file_ids`/`t0_ms`/`t1_ms`/`max_points_per_series` 全部对不上
/// 必填参数名，命令以参数反序列化失败被拒，图表恒空。
#[tauri::command(rename_all = "snake_case")]
pub async fn query_series(
    state: tauri::State<'_, Arc<ImportCoordinator>>,
    file_ids: Vec<String>,
    metrics: Vec<String>,
    t0_ms: i64,
    t1_ms: i64,
    max_points_per_series: usize,
) -> Result<Vec<SeriesSliceDto>, IpcError> {
    query_series_logic(
        state.inner(),
        &file_ids,
        &metrics,
        t0_ms,
        t1_ms,
        max_points_per_series,
    )
}

/// `key_values_at`（ipc-ui.md §1.6）：按文件并发、单文件 10s 超时；部分失败
/// 只在该项填 error，其余照常返回；整体永不 reject。
#[tauri::command(rename_all = "snake_case")]
pub async fn key_values_at(
    state: tauri::State<'_, Arc<ImportCoordinator>>,
    file_ids: Vec<String>,
    timestamp_ms: i64,
) -> Result<Vec<KeyValueResultDto>, IpcError> {
    key_values_at_logic(state.inner(), &file_ids, timestamp_ms).await
}
