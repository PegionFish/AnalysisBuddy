//! REST 路由（`/api/v1`）+ Bearer 认证中间件 + 请求体上限。
//!
//! 响应体复用 `ab_engine::commands` 的 DTO（serde 形状与桌面 ipc-ui.md
//! §1.0 逐字段一致）；错误恒 `{"error": <IpcError>}`（[`crate::error`]
//! 映射状态码）。服务器相对桌面的语义差异（file_ids 缺省 = 全部 Frozen、
//! job 取消协作式等）均文档化于 docs/spec/http-api-v1.md §2。

use std::collections::HashMap;
// `std::path::Path` 别名：axum 的路径参数提取器 `Path` 占用主名。
use std::path::{Path as StdPath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ab_engine::commands::import::unload_file_logic;
use ab_engine::commands::plugin::{
    get_plugin_log_logic, list_plugins_logic, reload_plugin_logic,
};
use ab_engine::commands::plugin_manager::{
    check_plugin_update_logic, install_plugin_zip_logic, set_plugin_enabled_logic,
    uninstall_plugin_logic, update_plugin_logic,
};
use ab_engine::commands::presets::{
    delete_user_preset_locked, list_user_presets_logic, save_user_preset_locked, UserPresetDto,
};
use ab_engine::commands::query::{
    get_metrics_logic, key_values_at_logic, query_series_logic, KeyValueResultDto, SeriesSliceDto,
};
use ab_engine::commands::session::{load_session_logic, save_session_logic};
use ab_engine::commands::{
    IpcError, ImportOverride, LoadResultDto, PluginInfoDto, SessionMetaDto, SessionSnapshotDto,
};
use ab_protocol::manifest::LocalizedName;
use axum::extract::multipart::MultipartRejection;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use futures_core::Stream;
use serde::Deserialize;
use serde_json::json;

use crate::error::{ApiError, ApiResult};
use crate::hub::SseEventStream;
use crate::jobs::JobStatusDto;
use crate::state::AppState;
use crate::{
    DEFAULT_MAX_POINTS_PER_SERIES, MAX_BODY_BYTES, MAX_POINTS_PER_SERIES_CAP, PROTOCOL_VERSION,
};

/// 路由表：`/api/v1` 前缀 + 认证中间件 + 64MB 请求体上限。
/// axum 0.8 路径参数语法 `{param}`；`/plugins/install` 静态段优先于
/// `/plugins/{id}` 参数段（matchit 语义，共存合法）。
pub fn build_router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/imports", post(create_import))
        .route("/imports/upload", post(upload_import))
        .route("/imports/{job_id}", get(get_job).delete(cancel_job))
        .route("/files/{file_id}", delete(unload_file))
        .route("/metrics", get(get_metrics))
        .route("/query/series", post(query_series))
        .route("/query/key-values", post(query_key_values))
        .route("/plugins", get(list_plugins))
        .route("/plugins/install", post(install_plugin))
        .route("/plugins/{id}/log", get(get_plugin_log))
        .route("/plugins/{id}/reload", post(reload_plugin))
        .route("/plugins/{id}/enabled", put(set_plugin_enabled))
        .route(
            "/plugins/{id}/update",
            get(check_plugin_update).post(update_plugin),
        )
        .route("/plugins/{id}", delete(uninstall_plugin))
        .route("/sessions/save", post(save_session))
        .route("/sessions/load", post(load_session))
        .route("/presets", get(list_presets).post(save_preset))
        .route("/presets/{id}", delete(delete_preset))
        .route("/events", get(events))
        .with_state(state.clone());
    Router::new()
        .nest("/api/v1", api)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

// ---------------------------------------------------------------------------
// 基础设施
// ---------------------------------------------------------------------------

/// GET /api/v1/health：探活 + 协议版本（认证豁免，供负载均衡探活）。
async fn health() -> Json<serde_json::Value> {
    Json(json!({
        "protocol_version": PROTOCOL_VERSION,
        "version": env!("CARGO_PKG_VERSION"),
        "status": "ok",
    }))
}

/// Bearer 认证：`--token` 提供时全端点强制，仅 GET /api/v1/health 豁免。
/// 失败 → 401 统一错误包络；比较为恒时（防时序侧信道）。
async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if req.method() == Method::GET && req.uri().path() == "/api/v1/health" {
        return next.run(req).await;
    }
    let Some(expected) = state.token.as_ref() else {
        return next.run(req).await;
    };
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match provided {
        Some(token) if tokens_equal(token, expected) => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"code": "unauthorized", "message": "missing or invalid bearer token"}})),
        )
            .into_response(),
    }
}

/// 恒时字符串比较（长度差立即入累计值，逐字节异或）。
fn tokens_equal(provided: &str, expected: &str) -> bool {
    let a = provided.as_bytes();
    let b = expected.as_bytes();
    a.iter()
        .zip(b)
        .fold(a.len() ^ b.len(), |acc, (x, y)| acc | ((x ^ y) as usize))
        == 0
}

/// JSON body 提取器：解析失败 → 400 invalid_arg（统一错误包络，而非
/// axum 默认纯文本 rejection）。
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(JsonBody(value)),
            Err(_) => Err(ApiError::invalid_arg("invalid JSON body")),
        }
    }
}

// axum 0.8 的 `MultipartRejection` 仅表示 boundary 非法，无文本详情可取。
fn multipart_rejection(_rejection: MultipartRejection) -> ApiError {
    ApiError::invalid_arg("invalid multipart body")
}

static UPLOAD_SEQ: AtomicU64 = AtomicU64::new(0);

/// 上传落盘：`%TEMP%/ab-server-uploads/<pid>-<seq>-<nanos>/<basename>`——
/// 唯一性由子目录名承担，basename 保留客户端文件名（ImportResult.name 与
/// 桌面「取 basename」语义一致，不被服务器前缀污染）。basename 经
/// `Path::file_name()` 提取防路径穿越，缺省 `upload.bin`。
fn save_temp_upload(bytes: &[u8], filename: Option<&str>) -> Result<PathBuf, IpcError> {
    let nanos = now_nanos();
    let dir = std::env::temp_dir().join("ab-server-uploads").join(format!(
        "{}-{}-{nanos}",
        std::process::id(),
        UPLOAD_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| IpcError {
        code: "internal".to_string(),
        message: format!("cannot create upload dir: {e}"),
        data: None,
    })?;
    let base = filename
        .map(StdPath::new)
        .and_then(StdPath::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty() && name.len() < 128)
        .unwrap_or_else(|| "upload.bin".to_string());
    let path = dir.join(&base);
    std::fs::write(&path, bytes).map_err(|e| IpcError {
        code: "internal".to_string(),
        message: format!("cannot save upload: {e}"),
        data: None,
    })?;
    Ok(path)
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 导入（imports / files）
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateImportBody {
    paths: Vec<String>,
    #[serde(default)]
    overrides: Option<HashMap<String, ImportOverride>>,
}

/// POST /imports：包装为后台任务，202 + queued 快照。
async fn create_import(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<CreateImportBody>,
) -> ApiResult<(StatusCode, Json<JobStatusDto>)> {
    if body.paths.is_empty() || body.paths.iter().all(|p| p.trim().is_empty()) {
        return Err(ApiError::invalid_arg("paths must not be empty"));
    }
    let status = state
        .jobs
        .spawn_import(state.coordinator.clone(), body.paths, body.overrides);
    Ok((StatusCode::ACCEPTED, Json(status)))
}

/// POST /imports/upload：multipart `file`（必填）、`filename`（可选覆盖名）、
/// `overrides`（可选 JSON，形状同 POST /imports 的 overrides 对象）。
/// 文件落服务临时目录后走同一 job 流程。
async fn upload_import(
    State(state): State<AppState>,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult<(StatusCode, Json<JobStatusDto>)> {
    let mut multipart = multipart.map_err(multipart_rejection)?;
    let mut file_name: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut overrides: Option<HashMap<String, ImportOverride>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::invalid_arg(format!("multipart read failed: {e}")))?
    {
        match field.name().unwrap_or_default() {
            "file" => {
                file_name = field.file_name().map(str::to_string);
                file_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| {
                            ApiError::invalid_arg(format!("multipart field `file` read failed: {e}"))
                        })?
                        .to_vec(),
                );
            }
            "filename" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| {
                        ApiError::invalid_arg(format!("multipart field `filename` read failed: {e}"))
                    })?;
                file_name = Some(text);
            }
            "overrides" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| {
                        ApiError::invalid_arg(format!("multipart field `overrides` read failed: {e}"))
                    })?;
                overrides = Some(serde_json::from_str(&text)
                    .map_err(|e| ApiError::invalid_arg(format!("invalid overrides JSON: {e}")))?);
            }
            _ => {} // 未知字段忽略（前向兼容）
        }
    }
    let Some(bytes) = file_bytes else {
        return Err(ApiError::invalid_arg("multipart field `file` is required"));
    };
    let saved = save_temp_upload(&bytes, file_name.as_deref())?;
    let path = saved.to_string_lossy().into_owned();
    let status = state
        .jobs
        .spawn_import(state.coordinator.clone(), vec![path], overrides);
    Ok((StatusCode::ACCEPTED, Json(status)))
}

/// GET /imports/{job_id}：任务状态（未知 job_id → 404 file_not_found）。
async fn get_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> ApiResult<Json<JobStatusDto>> {
    state
        .jobs
        .status(&job_id)
        .map(Json)
        .ok_or_else(|| job_not_found(&job_id))
}

/// DELETE /imports/{job_id}：协作式取消（见 jobs.rs 模块注）。
async fn cancel_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> ApiResult<Json<JobStatusDto>> {
    state
        .jobs
        .cancel(&job_id)
        .map(Json)
        .ok_or_else(|| job_not_found(&job_id))
}

fn job_not_found(job_id: &str) -> ApiError {
    ApiError(IpcError {
        code: "file_not_found".to_string(),
        message: format!("job `{job_id}` not found"),
        data: None,
    })
}

/// DELETE /files/{file_id}：桌面 unload_file（未知 file_id 幂等成功 → 204）。
async fn unload_file(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> ApiResult<StatusCode> {
    unload_file_logic(&state.coordinator, file_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 查询（metrics / series / key-values）
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MetricsQuery {
    file_ids: Option<String>,
}

/// GET /metrics?file_ids=a,b：逗号分隔；缺省/空 = 全部 Frozen 文件
/// （与桌面 get_metrics 默认入参语义一致）。
async fn get_metrics(
    State(state): State<AppState>,
    Query(query): Query<MetricsQuery>,
) -> Json<Vec<ab_engine::commands::query::MetricNodeDto>> {
    let file_ids = split_csv_ids(query.file_ids.as_deref());
    Json(get_metrics_logic(&state.coordinator, file_ids))
}

/// "a,b ,c" → ["a","b","c"]；None / 空串 / 全空段 → None（= 全部）。
fn split_csv_ids(raw: Option<&str>) -> Option<Vec<String>> {
    let raw = raw?;
    let ids: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    (!ids.is_empty()).then_some(ids)
}

#[derive(Deserialize)]
struct QuerySeriesBody {
    #[serde(default)]
    file_ids: Option<Vec<String>>,
    metrics: Vec<String>,
    t0_ms: i64,
    t1_ms: i64,
    max_points_per_series: Option<usize>,
}

/// POST /query/series。file_ids 缺省/空 = 全部 Frozen 文件（**服务器语义**
/// 与桌面「空列表 = 静默空结果」不同，文档化于 http-api-v1.md §2）；
/// max_points_per_series 缺省 4000，> 50000 → 400 invalid_arg。
async fn query_series(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<QuerySeriesBody>,
) -> ApiResult<Json<Vec<SeriesSliceDto>>> {
    if let Some(max) = body.max_points_per_series {
        if max > MAX_POINTS_PER_SERIES_CAP {
            return Err(ApiError::invalid_arg(format!(
                "max_points_per_series {max} exceeds server cap of {MAX_POINTS_PER_SERIES_CAP}"
            )));
        }
    }
    let file_ids = effective_file_ids(&state, body.file_ids);
    let slices = query_series_logic(
        &state.coordinator,
        &file_ids,
        &body.metrics,
        body.t0_ms,
        body.t1_ms,
        body.max_points_per_series
            .unwrap_or(DEFAULT_MAX_POINTS_PER_SERIES),
    )?;
    Ok(Json(slices))
}

#[derive(Deserialize)]
struct QueryKeyValuesBody {
    #[serde(default)]
    file_ids: Option<Vec<String>>,
    timestamp_ms: i64,
}

/// POST /query/key-values：部分失败协议保留——永不整体 reject，逐文件
/// entries/error 进结果（ipc-ui.md §1.6）。
async fn query_key_values(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<QueryKeyValuesBody>,
) -> ApiResult<Json<Vec<KeyValueResultDto>>> {
    let file_ids = effective_file_ids(&state, body.file_ids);
    let results = key_values_at_logic(&state.coordinator, &file_ids, body.timestamp_ms).await?;
    Ok(Json(results))
}

/// file_ids 入参归一：None/空列表 → 全部 Frozen 文件（服务器语义）。
fn effective_file_ids(state: &AppState, file_ids: Option<Vec<String>>) -> Vec<String> {
    match file_ids {
        Some(ids) if !ids.is_empty() => ids,
        _ => state.coordinator.list_frozen(),
    }
}

// ---------------------------------------------------------------------------
// 插件管理（plugins）
// ---------------------------------------------------------------------------

/// GET /plugins：发现列表 + 实时状态合并（桌面 list_plugins 等价）。
async fn list_plugins(State(state): State<AppState>) -> Json<Vec<PluginInfoDto>> {
    Json(list_plugins_logic(
        &state.discovery,
        &state.meta,
        &state.coordinator,
        &state.paths.plugins_portable,
    ))
}

#[derive(Deserialize)]
struct LogQuery {
    limit: Option<usize>,
}

/// GET /plugins/{id}/log?limit=N：环形缓冲尾部（默认 200，上限 10000）。
async fn get_plugin_log(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<Vec<ab_engine::events::PluginLogPayload>>> {
    let lines = get_plugin_log_logic(&state.log_buffer, &plugin_id, query.limit)?;
    Ok(Json(lines))
}

/// POST /plugins/{id}/reload：重建插件会话（PluginsReloaded 事件经 SSE 广播）。
async fn reload_plugin(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<PluginInfoDto>> {
    let info = reload_plugin_logic(&state.discovery, &state.meta, &state.coordinator, &plugin_id)
        .await?;
    Ok(Json(info))
}

/// POST /plugins/install：multipart `file`（ZIP）+ `overwrite`（可选
/// "true"/"1"/"yes"/"on"，大小写不敏感）。ZIP 先落服务临时目录再走
/// install_plugin_zip_logic，安装完成后清理临时文件。
async fn install_plugin(
    State(state): State<AppState>,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult<Json<PluginInfoDto>> {
    let mut multipart = multipart.map_err(multipart_rejection)?;
    let mut zip_bytes: Option<Vec<u8>> = None;
    let mut overwrite = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::invalid_arg(format!("multipart read failed: {e}")))?
    {
        match field.name().unwrap_or_default() {
            "file" => {
                zip_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| {
                            ApiError::invalid_arg(format!("multipart field `file` read failed: {e}"))
                        })?
                        .to_vec(),
                );
            }
            "overwrite" => {
                let text = field.text().await.unwrap_or_default();
                overwrite = matches!(
                    text.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                );
            }
            _ => {}
        }
    }
    let Some(bytes) = zip_bytes else {
        return Err(ApiError::invalid_arg("multipart field `file` is required"));
    };
    let saved = save_temp_upload(&bytes, Some("plugin.zip"))?;
    let result = install_plugin_zip_logic(
        &state.coordinator,
        &state.discovery,
        &state.paths.plugins_portable,
        &saved.to_string_lossy(),
        overwrite,
    )
    .await;
    let _ = std::fs::remove_file(&saved);
    Ok(Json(result?))
}

/// DELETE /plugins/{id}：卸载（关闭会话 → 终止进程 → 删目录 → reload）。
async fn uninstall_plugin(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
) -> ApiResult<StatusCode> {
    uninstall_plugin_logic(
        &state.coordinator,
        &state.discovery,
        &state.paths.plugins_portable,
        &plugin_id,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct EnabledBody {
    enabled: bool,
}

/// PUT /plugins/{id}/enabled：写状态文件 + set_disabled（spec §4.4）。
async fn set_plugin_enabled(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
    JsonBody(body): JsonBody<EnabledBody>,
) -> ApiResult<StatusCode> {
    set_plugin_enabled_logic(
        &state.discovery,
        &state.paths.plugins_portable,
        &plugin_id,
        body.enabled,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /plugins/{id}/update：检查更新（GitHub Releases；无可用更新 → 422）。
async fn check_plugin_update(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<ab_engine::commands::plugin_manager::UpdateInfoDto>> {
    let info = check_plugin_update_logic(
        state.fetcher.as_ref(),
        &state.discovery,
        &state.paths.plugins_portable,
        &plugin_id,
    )
    .await?;
    Ok(Json(info))
}

/// POST /plugins/{id}/update：下载覆盖安装（完成后返回新 PluginInfo）。
async fn update_plugin(
    State(state): State<AppState>,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<PluginInfoDto>> {
    let info = update_plugin_logic(
        state.fetcher.as_ref(),
        &state.coordinator,
        &state.discovery,
        &state.paths.plugins_portable,
        &plugin_id,
    )
    .await?;
    Ok(Json(info))
}

// ---------------------------------------------------------------------------
// 会话（sessions）
// ---------------------------------------------------------------------------

static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
struct SaveSessionBody {
    /// 相对路径以 sessions_dir 为根；缺省自动命名。越界 → 400 invalid_arg。
    path: Option<String>,
    #[serde(default)]
    snapshot: Option<SessionSnapshotDto>,
}

/// POST /sessions/save：落盘 `.absession`（相对路径限 sessions_dir 内）。
async fn save_session(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<SaveSessionBody>,
) -> ApiResult<Json<SessionMetaDto>> {
    let path = resolve_session_path(&state.paths.sessions_dir, body.path.as_deref())?;
    let meta = save_session_logic(&state.coordinator, &path, body.snapshot)?;
    Ok(Json(meta))
}

#[derive(Deserialize)]
struct LoadSessionBody {
    path: String,
}

/// POST /sessions/load：读回会话并重走导入管线（同样的路径约束）。
async fn load_session(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<LoadSessionBody>,
) -> ApiResult<Json<LoadResultDto>> {
    let path = resolve_session_path(&state.paths.sessions_dir, Some(&body.path))?;
    let result = load_session_logic(&state.coordinator, &path).await?;
    Ok(Json(result))
}

/// 会话路径归一：相对 → sessions_dir 拼接；词法规范化（`.`/`..`）后强制
/// 仍在 sessions_dir 内（越界 → invalid_arg 400）。
fn resolve_session_path(
    sessions_dir: &StdPath,
    raw: Option<&str>,
) -> Result<PathBuf, IpcError> {
    let path = match raw {
        None => sessions_dir.join(format!(
            "session-{}-{}-{nanos}.absession",
            std::process::id(),
            SESSION_SEQ.fetch_add(1, Ordering::Relaxed),
            nanos = now_nanos(),
        )),
        Some(raw) => {
            let raw = raw.trim();
            if raw.is_empty() {
                return Err(IpcError::invalid_arg("path must not be empty"));
            }
            let candidate = PathBuf::from(raw);
            if candidate.is_absolute() {
                candidate
            } else {
                sessions_dir.join(candidate)
            }
        }
    };
    let normalized = normalize_lexical(&path);
    if !normalized.starts_with(sessions_dir) {
        return Err(IpcError::invalid_arg(format!(
            "path escapes sessions dir: {}",
            path.display()
        )));
    }
    Ok(normalized)
}

/// 纯词法规范化（不触盘）：解析 `.`/`..`；根段之外的 `..` 保留，由
/// starts_with 前缀判定兜底拒绝。
fn normalize_lexical(path: &StdPath) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 用户预设（presets）
// ---------------------------------------------------------------------------

/// GET /presets。
async fn list_presets(State(state): State<AppState>) -> Json<Vec<UserPresetDto>> {
    Json(list_user_presets_logic(&state.paths.presets_dir))
}

#[derive(Deserialize)]
struct SavePresetBody {
    name: LocalizedName,
    #[serde(default)]
    entries: HashMap<String, Vec<String>>,
}

/// POST /presets：保存用户预设（201；同名冲突 → 409 preset_conflict）。
async fn save_preset(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<SavePresetBody>,
) -> ApiResult<(StatusCode, Json<UserPresetDto>)> {
    let preset =
        save_user_preset_locked(&state.paths.presets_dir, body.name, body.entries).await?;
    Ok((StatusCode::CREATED, Json(preset)))
}

/// DELETE /presets/{id}。
async fn delete_preset(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    delete_user_preset_locked(&state.paths.presets_dir, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 事件（SSE）
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct EventsQuery {
    /// 只推送该 file_id 的 progress。
    file_id: Option<String>,
    /// 只推送该 plugin_id 的 health/log。
    plugin_id: Option<String>,
}

/// GET /events（SSE）：帧名 = EV_* 常量去 `ab://` 前缀（progress /
/// plugin-log / plugin-health / plugins-reloaded；`error` = 掉队终帧）。
/// 每订阅者独立 100ms/file_id 节流（percent≥100 终态直发）。
async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>> {
    let stream = SseEventStream::new(
        state.hub.subscribe(),
        query.file_id.filter(|s| !s.is_empty()),
        query.plugin_id.filter(|s| !s.is_empty()),
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}
