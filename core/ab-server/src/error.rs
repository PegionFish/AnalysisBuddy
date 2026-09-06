//! `IpcError` → HTTP 响应映射：状态码表（http-api-v1.md §4 错误模型的
//! 唯一实现）+ 统一错误包络 `{"error": <IpcError>}`。
//!
//! 桌面契约（ipc-ui.md §1.0）的错误形状逐字段透传；HTTP 状态码只是
//! 传输层附加值（机器读 `code`，人读 `message`——RFC 7807 精神）。
//!
//! 取舍（均文档化于 http-api-v1.md §4）：
//! - `cancelled` → 409（可重试的请求冲突；499 是 nginx 私有码，不采用）；
//! - `unsupported` / `invalid_params` → 422（请求结构有效但服务器/插件不支持
//!   该能力或语义无效；后者来自 custom_query -32602 归一，CCP-custom-query）；
//! - RPC 数字码（JSON-RPC -32700..-32602 风格，如传输层透传）→ 400；
//! - 未知码 → 500（保守）。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use ab_engine::commands::IpcError;

/// `IpcError` 的 HTTP 包装（IntoResponse 产出统一错误包络）。
#[derive(Debug, Clone)]
pub struct ApiError(pub IpcError);

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self(IpcError {
            code: code.into(),
            message: message.into(),
            data: None,
        })
    }

    pub fn invalid_arg(message: impl Into<String>) -> Self {
        Self::new("invalid_arg", message)
    }
}

impl From<IpcError> for ApiError {
    fn from(error: IpcError) -> Self {
        Self(error)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0.code, self.0.message)
    }
}

impl std::error::Error for ApiError {}

/// 处理器返回类型别名（Ok 分支直接 `T: IntoResponse`）。
pub type ApiResult<T> = Result<T, ApiError>;

/// 错误码 → HTTP 状态（docs/spec/http-api-v1.md §4 表）。
pub fn status_for(code: &str) -> u16 {
    match code {
        "invalid_arg" => 400,
        "file_not_found" | "module_not_found" => 404,
        "plugin_busy" | "cancelled" | "module_conflict" | "module_protected"
        | "module_in_use" | "preset_conflict" => 409,
        "parse_failed" | "file_load_failed" | "module_install" | "update_not_available"
        | "unsupported" | "invalid_params" => 422,
        "plugin_crashed" | "network" => 502,
        "timeout" => 504,
        "session_io" | "state_io" | "internal" | "host_backpressure" => 500,
        other => {
            if other.parse::<i64>().is_ok() {
                400 // JSON-RPC 风格数字错误码（-32700..-32602 等）→ 请求问题
            } else {
                500 // 未知码保守回落 500
            }
        }
    }
}

fn status_code(code: &str) -> StatusCode {
    StatusCode::from_u16(status_for(code)).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = status_code(&self.0.code);
        (status, Json(json!({ "error": self.0 }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn status_table_snapshot() {
        let table: &[(&str, u16)] = &[
            ("invalid_arg", 400),
            ("file_not_found", 404),
            ("module_not_found", 404),
            ("plugin_busy", 409),
            ("cancelled", 409),
            ("module_conflict", 409),
            ("module_protected", 409),
            ("module_in_use", 409),
            ("preset_conflict", 409),
            ("parse_failed", 422),
            ("file_load_failed", 422),
            ("module_install", 422),
            ("update_not_available", 422),
            ("unsupported", 422),
            ("invalid_params", 422),
            ("plugin_crashed", 502),
            ("network", 502),
            ("timeout", 504),
            ("session_io", 500),
            ("state_io", 500),
            ("internal", 500),
            ("host_backpressure", 500),
            ("-32700", 400),
            ("-32602", 400),
            ("something_new", 500),
        ];
        for (code, expected) in table {
            assert_eq!(status_for(code), *expected, "status for `{code}`");
        }
    }

    #[tokio::test]
    async fn envelope_shape_is_error_wrapped_ipc_error() {
        let error = ApiError(IpcError {
            code: "file_not_found".to_string(),
            message: "nope".to_string(),
            data: Some(json!({"line": 1})),
        });
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(
            value,
            json!({"error": {"code": "file_not_found", "message": "nope", "data": {"line": 1}}})
        );
    }
}
