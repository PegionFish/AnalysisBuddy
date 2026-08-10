//! 查询类 Tauri command（ipc-ui.md §1.4/§1.5/§1.6）：
//! `get_metrics`（文件→插件→指标三级树）、`query_series`（预算化降采样，
//! `t0 > t1` reject `invalid_arg`）、`key_values_at`（按文件并发扇出，
//! 部分失败逐项填 error，整体永不 reject）。

use std::collections::HashMap;
use std::sync::Arc;

use ab_pipeline::{MetricRef, QueryRequest, SeriesSlice};
use ab_protocol::types::{Aggregation, KeyValueEntry, MetricDef};

use crate::commands::IpcError;
use crate::pipeline_bridge::{
    query_key_values, ImportCoordinator, KeyValuesError, KeyValuesOutcome,
};

/// 三级树节点（§1.0 `MetricNode`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MetricNodeDto {
    pub level: &'static str,
    pub id: String,
    pub file_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_id: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<MetricNodeDto>>,
}

/// 查询结果切片（§1.0 `SeriesSlice`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SeriesSliceDto {
    pub file_id: String,
    pub plugin_id: String,
    pub metric_id: String,
    pub point_count: usize,
    pub downsampled: bool,
    pub points: Vec<SeriesPointDto>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SeriesPointDto {
    pub t_ms: i64,
    pub v: f64,
}

/// key_values 按文件结果（§1.0 `KeyValueResult`；成功/失败同构）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct KeyValueResultDto {
    pub file_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<KeyValueEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
}

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

/// `get_metrics` 逻辑体（handler 薄包装）。
pub fn get_metrics_logic(
    coordinator: &ImportCoordinator,
    file_ids: Option<Vec<String>>,
) -> Vec<MetricNodeDto> {
    build_metric_tree(coordinator, file_ids)
}

/// `query_series`（ipc-ui.md §1.5）：复合 id `file_id:plugin_id:metric_id`；
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

/// `query_series` 逻辑体（handler 薄包装）。
pub fn query_series_logic(
    coordinator: &ImportCoordinator,
    file_ids: &[String],
    metrics: &[String],
    t0_ms: i64,
    t1_ms: i64,
    max_points_per_series: usize,
) -> Result<Vec<SeriesSliceDto>, IpcError> {
    let _ = file_ids;
    if t0_ms > t1_ms {
        return Err(IpcError::invalid_arg(format!(
            "t0_ms ({t0_ms}) must not exceed t1_ms ({t1_ms})"
        )));
    }
    let mut refs = Vec::with_capacity(metrics.len());
    let mut plugin_by_series: HashMap<(String, String), String> = HashMap::new();
    let mut malformed = 0u64;
    for composite in metrics {
        match parse_metric_ref(composite) {
            Some((file_id, plugin_id, metric_id)) => {
                plugin_by_series.insert((file_id.clone(), metric_id.clone()), plugin_id);
                refs.push(MetricRef {
                    file_id,
                    metric: metric_id,
                });
            }
            None => malformed += 1,
        }
    }
    if malformed > 0 {
        // ipc-ui.md §1.5：未知 metric id 静默忽略并在宿主日志计数。
        eprintln!("query_series: ignored {malformed} malformed metric ids");
    }
    let slices = coordinator.store().query(&QueryRequest {
        metrics: refs,
        t0_ms,
        t1_ms,
        max_points_per_series,
    });
    Ok(slices
        .into_iter()
        .map(|s| to_slice_dto(s, &plugin_by_series))
        .collect())
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

/// `key_values_at` 逻辑体（handler 薄包装）。
pub async fn key_values_at_logic(
    coordinator: &ImportCoordinator,
    file_ids: &[String],
    timestamp_ms: i64,
) -> Result<Vec<KeyValueResultDto>, IpcError> {
    let outcomes = query_key_values(
        coordinator.registry(),
        coordinator.file_index(),
        file_ids,
        timestamp_ms,
        coordinator.key_values_timeout(),
    )
    .await;
    Ok(outcomes.into_iter().map(to_key_value_dto).collect())
}

fn to_slice_dto(
    slice: SeriesSlice,
    plugin_by_series: &HashMap<(String, String), String>,
) -> SeriesSliceDto {
    let plugin_id = plugin_by_series
        .get(&(slice.file_id.clone(), slice.metric.clone()))
        .cloned()
        .unwrap_or_default();
    let point_count = slice.ts.len();
    SeriesSliceDto {
        file_id: slice.file_id,
        plugin_id,
        metric_id: slice.metric,
        point_count,
        downsampled: slice.downsampled,
        points: slice
            .ts
            .into_iter()
            .zip(slice.values)
            .map(|(t_ms, v)| SeriesPointDto { t_ms, v })
            .collect(),
    }
}

fn to_key_value_dto(outcome: KeyValuesOutcome) -> KeyValueResultDto {
    match outcome.result {
        Ok(entries) => KeyValueResultDto {
            file_id: outcome.file_id,
            entries: Some(entries),
            error: None,
        },
        Err(error) => KeyValueResultDto {
            file_id: outcome.file_id,
            entries: None,
            error: Some(to_key_values_error(&error)),
        },
    }
}

fn to_key_values_error(error: &KeyValuesError) -> IpcError {
    match error {
        KeyValuesError::Timeout => crate::ipc_errors::timeout_error("key_values"),
        KeyValuesError::PluginError(code, message) => IpcError {
            code: crate::ipc_errors::code_name(*code).to_string(),
            message: message.clone(),
            data: None,
        },
        KeyValuesError::SessionGone => {
            crate::ipc_errors::map_session_error(ab_pipeline::SessionError::SessionGone, true)
        }
        KeyValuesError::FileNotReady(_) => IpcError {
            code: "file_not_found".to_string(),
            message: "file is not loaded".to_string(),
            data: None,
        },
    }
}

/// 复合 id `file_id:plugin_id:metric_id` 解析（§1.5；畸形 → `None`）。
fn parse_metric_ref(composite: &str) -> Option<(String, String, String)> {
    let mut parts = composite.splitn(3, ':');
    let file_id = parts.next()?;
    let plugin_id = parts.next()?;
    let metric_id = parts.next()?;
    if file_id.is_empty() || plugin_id.is_empty() || metric_id.is_empty() || metric_id.contains(':')
    {
        return None;
    }
    Some((
        file_id.to_string(),
        plugin_id.to_string(),
        metric_id.to_string(),
    ))
}

/// `get_metrics` 树构造（纯函数，便于单测）：文件 → 插件 → 指标三级节点。
pub fn build_metric_tree(
    coordinator: &ImportCoordinator,
    file_ids: Option<Vec<String>>,
) -> Vec<MetricNodeDto> {
    let frozen = coordinator.list_frozen();
    let selected: Vec<String> = match file_ids {
        Some(ids) if !ids.is_empty() => ids.into_iter().filter(|id| frozen.contains(id)).collect(),
        _ => frozen,
    };
    let mut tree = Vec::with_capacity(selected.len());
    for file_id in selected {
        let plugin_id = match coordinator.file_index().get(&file_id) {
            Some(plugin_id) => plugin_id,
            None => continue,
        };
        let name = coordinator
            .path_of(&file_id)
            .and_then(|p| {
                std::path::Path::new(&p)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| file_id.clone());
        let metric_nodes: Vec<MetricNodeDto> = coordinator
            .store()
            .metrics_of(&file_id)
            .into_iter()
            .map(|metric_id| {
                let def = coordinator
                    .schema_metrics(&plugin_id)
                    .into_iter()
                    .find(|m| m.id == metric_id);
                metric_node(&file_id, &plugin_id, &metric_id, def.as_ref())
            })
            .collect();
        let plugin_node = MetricNodeDto {
            level: "plugin",
            id: plugin_id.clone(),
            file_id: file_id.clone(),
            plugin_id: Some(plugin_id.clone()),
            metric_id: None,
            name: coordinator.plugin_display_name(&plugin_id),
            unit: None,
            description: None,
            aggregation: None,
            children: Some(metric_nodes),
        };
        tree.push(MetricNodeDto {
            level: "file",
            id: file_id.clone(),
            file_id,
            plugin_id: None,
            metric_id: None,
            name,
            unit: None,
            description: None,
            aggregation: None,
            children: Some(vec![plugin_node]),
        });
    }
    tree
}

fn metric_node(
    file_id: &str,
    plugin_id: &str,
    metric_id: &str,
    def: Option<&MetricDef>,
) -> MetricNodeDto {
    MetricNodeDto {
        level: "metric",
        id: format!("{file_id}:{plugin_id}:{metric_id}"),
        file_id: file_id.to_string(),
        plugin_id: Some(plugin_id.to_string()),
        metric_id: Some(metric_id.to_string()),
        name: def
            .map(|d| d.name.clone())
            .unwrap_or_else(|| metric_id.to_string()),
        unit: def.and_then(|d| d.unit.clone()),
        description: def.and_then(|d| d.description.clone()),
        aggregation: def.map(|d| aggregation_name(d.aggregation)),
        children: None,
    }
}

fn aggregation_name(aggregation: Aggregation) -> &'static str {
    match aggregation {
        Aggregation::Last => "last",
        Aggregation::Sum => "sum",
        Aggregation::Avg => "avg",
        Aggregation::Min => "min",
        Aggregation::Max => "max",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_ref_parsing() {
        let parsed = parse_metric_ref("f1:mock:fps").expect("well-formed");
        assert_eq!(
            parsed,
            ("f1".to_string(), "mock".to_string(), "fps".to_string())
        );
        // 畸形：缺段 / 空段 / metric 内冒号。
        assert!(parse_metric_ref("f1:mock").is_none());
        assert!(parse_metric_ref(":mock:fps").is_none());
        assert!(parse_metric_ref("f1::fps").is_none());
        assert!(parse_metric_ref("f1:mock:a:b").is_none());
    }

    #[test]
    fn rpc_code_mapping_matches_ipc_ui_section1_10() {
        use ab_protocol::errors::*;
        // 唯一实现收敛在 crate::ipc_errors（§1.10 表）。
        assert_eq!(crate::ipc_errors::code_name(ERR_PLUGIN_BUSY), "plugin_busy");
        assert_eq!(
            crate::ipc_errors::code_name(ERR_FILE_LOAD_FAILED),
            "file_load_failed"
        );
        assert_eq!(
            crate::ipc_errors::code_name(ERR_PARSE_FAILED),
            "parse_failed"
        );
        assert_eq!(crate::ipc_errors::code_name(ERR_CANCELLED), "cancelled");
        assert_eq!(crate::ipc_errors::code_name(ERR_INTERNAL_ERROR), "internal");
    }

    #[test]
    fn key_values_error_mapping_never_rejects_shape() {
        let dto = to_key_values_error(&KeyValuesError::Timeout);
        assert_eq!(dto.code, "timeout");
        let dto = to_key_values_error(&KeyValuesError::PluginError(
            ab_protocol::errors::ERR_PLUGIN_BUSY,
            "busy".to_string(),
        ));
        assert_eq!(dto.code, "plugin_busy");
        let dto = to_key_values_error(&KeyValuesError::SessionGone);
        assert_eq!(dto.code, "plugin_crashed");
        let dto = to_key_values_error(&KeyValuesError::FileNotReady("f1".to_string()));
        assert_eq!(dto.code, "file_not_found");
    }

    #[test]
    fn series_slice_dto_shape() {
        let slice = SeriesSlice {
            file_id: "f1".to_string(),
            metric: "fps".to_string(),
            ts: vec![1, 2],
            values: vec![60.0, 59.0],
            downsampled: false,
        };
        let mut map = HashMap::new();
        map.insert(("f1".to_string(), "fps".to_string()), "mock".to_string());
        let dto = to_slice_dto(slice, &map);
        assert_eq!(dto.point_count, 2);
        assert_eq!(dto.points[0], SeriesPointDto { t_ms: 1, v: 60.0 });
        let value = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(value["metric_id"], "fps");
        assert_eq!(value["downsampled"], false);
    }
}
