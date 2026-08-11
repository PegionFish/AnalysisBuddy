//! ipc-ui.md §1.10 错误映射表唯一实现：`HostError` / `SessionError` → `IpcError`。
//!
//! 映射面（§1.10 表逐行）：
//! - RPC `-32001` → `plugin_busy`；`-32002` → `file_load_failed`；
//!   `-32003` → `parse_failed`（`data` 透传插件给出的定位信息）；
//!   `-32004` → `cancelled`；`-32005` 漏网 → `internal`；
//! - 帧错三码 `-32700` / `-32600` / `-32601`：调用方提供 `terminated`——
//!   会话已终止（protocol.md §1.3/§4.1 帧错误终止会话）→ `plugin_crashed`，
//!   未终止 → `internal`；
//! - `-32602` / `-32603` → `internal`（`message` 透传插件原文）；
//! - 状态机进 `Crashed` / 会话消失（`SessionGone`）→ `plugin_crashed`；
//! - 任一看门狗超时 → [`timeout_error`]。
//!
//! 其余错误码（含 `-32005`）一律回落 `internal`（§1.10「漏网映射 internal」）。
//! 全表 9 行由 `tests` 内快照测试覆盖（见本文件测试模块与
//! `tests/real_ipc_test.rs` 的 command 级错误形状断言）。

use ab_host::HostError;
use ab_pipeline::SessionError;

use crate::commands::IpcError;

/// RPC 帧错三码（§1.10：协议帧错 / 非法请求 / 方法不存在）。
const FRAME_ERROR_CODES: [i32; 3] = [
    ab_protocol::errors::ERR_PARSE_ERROR,
    ab_protocol::errors::ERR_INVALID_REQUEST,
    ab_protocol::errors::ERR_METHOD_NOT_FOUND,
];

/// `HostError` → `IpcError`（§1.10 表）。
///
/// `terminated`：该错误是否伴随会话终止（帧错三码按此分流
/// `plugin_crashed` / `internal`；其余码位不受影响）。
pub fn map_host_error(error: HostError, terminated: bool) -> IpcError {
    match error {
        HostError::Protocol {
            code,
            message,
            data,
        } => map_protocol_error(code, message, data, terminated),
        HostError::Transport(message) => IpcError {
            code: "plugin_crashed".to_string(),
            message: format!("plugin transport error: {message}"),
            data: None,
        },
        HostError::Discovery(e) => IpcError {
            code: "internal".to_string(),
            message: format!("discovery error: {e}"),
            data: None,
        },
    }
}

/// `SessionError` → `IpcError`（§1.10 表；`terminated` 语义同 [`map_host_error`]）。
pub fn map_session_error(error: SessionError, terminated: bool) -> IpcError {
    match error {
        SessionError::Plugin { code, message } => {
            map_protocol_error(code, message, None, terminated)
        }
        SessionError::SessionGone => IpcError {
            code: "plugin_crashed".to_string(),
            message: "plugin session gone".to_string(),
            data: None,
        },
    }
}

/// 看门狗超时（§1.10「任一看门狗超时 → `timeout`」）。
pub fn timeout_error(what: impl Into<String>) -> IpcError {
    IpcError {
        code: "timeout".to_string(),
        message: format!("{} timed out", what.into()),
        data: None,
    }
}

/// §5.1 模块管理错误码（任务 5）：`module_install` / `module_conflict` /
/// `module_protected` / `module_in_use` / `state_io` / `module_not_found`。
/// 该组错误由命令层直接构造（非 `HostError`/`SessionError` 映射来源），
/// 统一经此入口，保证形状与 §1.0 一致。
pub fn module_error(code: &'static str, message: impl Into<String>) -> IpcError {
    IpcError {
        code: code.to_string(),
        message: message.into(),
        data: None,
    }
}

/// RPC 错误码 → `IpcError.code`（§1.10 表；帧错三码按未终止语义 → `internal`，
/// 供无法判定终止态的调用方使用；能判定的路径请走 [`map_host_error`]/
/// [`map_session_error`] 带 `terminated`）。
pub fn code_name(code: i32) -> &'static str {
    match code {
        ab_protocol::errors::ERR_PLUGIN_BUSY => "plugin_busy",
        ab_protocol::errors::ERR_FILE_LOAD_FAILED => "file_load_failed",
        ab_protocol::errors::ERR_PARSE_FAILED => "parse_failed",
        ab_protocol::errors::ERR_CANCELLED => "cancelled",
        _ => "internal",
    }
}

/// `-32003` 时透传插件 `data`（§1.10「data 透传插件给出的定位信息」），
/// 其余码位不携带 data。
fn map_protocol_error(
    code: i32,
    message: String,
    data: Option<serde_json::Value>,
    terminated: bool,
) -> IpcError {
    let code_name = if FRAME_ERROR_CODES.contains(&code) && terminated {
        "plugin_crashed"
    } else {
        code_name(code)
    };
    IpcError {
        code: code_name.to_string(),
        message,
        data: (code == ab_protocol::errors::ERR_PARSE_FAILED)
            .then_some(data)
            .flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn protocol(code: i32, message: &str) -> HostError {
        HostError::Protocol {
            code,
            message: message.to_string(),
            data: None,
        }
    }

    /// §1.10 全表 9 行快照：码位 → `IpcError` 形状（含 data 透传与
    /// terminated 分流、`-32602/-32603` message 透传、看门狗超时）。
    #[test]
    fn section1_10_error_table_snapshot_all_nine_rows() {
        // 行 1：-32001 → plugin_busy。
        assert_eq!(
            map_host_error(
                protocol(ab_protocol::errors::ERR_PLUGIN_BUSY, "busy"),
                false
            ),
            IpcError {
                code: "plugin_busy".to_string(),
                message: "busy".to_string(),
                data: None,
            }
        );
        // 行 2：-32002 → file_load_failed。
        assert_eq!(
            map_host_error(
                protocol(ab_protocol::errors::ERR_FILE_LOAD_FAILED, "load failed"),
                false
            ),
            IpcError {
                code: "file_load_failed".to_string(),
                message: "load failed".to_string(),
                data: None,
            }
        );
        // 行 3：-32003 → parse_failed，data 透传。
        let with_data = map_host_error(
            HostError::Protocol {
                code: ab_protocol::errors::ERR_PARSE_FAILED,
                message: "parse failed".to_string(),
                data: Some(json!({ "line": 42 })),
            },
            false,
        );
        assert_eq!(with_data.code, "parse_failed");
        assert_eq!(
            with_data.data.as_ref(),
            Some(&json!({ "line": 42 })),
            "§1.10: -32003 data 透传插件定位信息"
        );
        assert_eq!(
            serde_json::to_value(&with_data).expect("serialize"),
            json!({ "code": "parse_failed", "message": "parse failed", "data": { "line": 42 } }),
            "IpcError 序列化形状（data 存在时不省略键）"
        );
        // 行 4：-32004 → cancelled。
        assert_eq!(
            map_host_error(
                protocol(ab_protocol::errors::ERR_CANCELLED, "cancelled"),
                false
            ),
            IpcError {
                code: "cancelled".to_string(),
                message: "cancelled".to_string(),
                data: None,
            }
        );
        // 行 5：-32005（v1 不支持能力）漏网 → internal。
        assert_eq!(
            map_host_error(
                protocol(ab_protocol::errors::ERR_UNSUPPORTED_IN_V1, "unsupported"),
                false
            ),
            IpcError {
                code: "internal".to_string(),
                message: "unsupported".to_string(),
                data: None,
            }
        );
        // 行 6：帧错三码 -32700/-32600/-32601 —— 会话终止 → plugin_crashed。
        for code in FRAME_ERROR_CODES {
            assert_eq!(
                map_host_error(protocol(code, "frame broke"), true),
                IpcError {
                    code: "plugin_crashed".to_string(),
                    message: "frame broke".to_string(),
                    data: None,
                },
                "§1.10: 帧错致会话终止 → plugin_crashed ({code})"
            );
            // 未终止场景 → internal。
            assert_eq!(
                map_host_error(protocol(code, "frame broke"), false),
                IpcError {
                    code: "internal".to_string(),
                    message: "frame broke".to_string(),
                    data: None,
                },
                "§1.10: 帧错未终止 → internal ({code})"
            );
        }
        // 行 7：会话消失 / 状态机 Crashed → plugin_crashed。
        assert_eq!(
            map_session_error(SessionError::SessionGone, true),
            IpcError {
                code: "plugin_crashed".to_string(),
                message: "plugin session gone".to_string(),
                data: None,
            }
        );
        // 行 8：看门狗超时 → timeout。
        assert_eq!(
            timeout_error("key_values"),
            IpcError {
                code: "timeout".to_string(),
                message: "key_values timed out".to_string(),
                data: None,
            }
        );
        // 行 9：-32602/-32603 → internal，message 透传插件原文。
        for code in [
            ab_protocol::errors::ERR_INVALID_PARAMS,
            ab_protocol::errors::ERR_INTERNAL_ERROR,
        ] {
            let mapped = map_host_error(protocol(code, "plugin original message"), false);
            assert_eq!(mapped.code, "internal");
            assert_eq!(
                mapped.message, "plugin original message",
                "§1.10: -32602/-32603 message 透传原文 ({code})"
            );
        }
    }

    /// `SessionError` 经同一张表映射（适配层收敛后唯一入口）。
    #[test]
    fn session_error_maps_through_same_table() {
        let mapped = map_session_error(
            SessionError::Plugin {
                code: ab_protocol::errors::ERR_FILE_LOAD_FAILED,
                message: "load failed".to_string(),
            },
            false,
        );
        assert_eq!(mapped.code, "file_load_failed");
        assert_eq!(mapped.message, "load failed");
    }

    /// `code_name`（无法判定 terminated 的路径）帧错三码回落 internal。
    #[test]
    fn code_name_falls_back_to_internal_for_frame_codes() {
        for code in FRAME_ERROR_CODES {
            assert_eq!(code_name(code), "internal");
        }
        assert_eq!(code_name(12345), "internal");
    }
}
