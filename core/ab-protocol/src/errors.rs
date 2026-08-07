//! JSON-RPC 错误码常量（protocol.md §4）。
//!
//! 值必须与 protocol.md §4 错误码表逐一相等，并由 `serde_tests::error_codes_match_doc`
//! 固化比对。标准码沿用 JSON-RPC 2.0 约定；自定义码仅限 `-32001` ~ `-32005`。

// --- §4.1 标准错误码 ---

/// Parse error：插件输出非法 JSON / 单行超 8 MB。
pub const ERR_PARSE_ERROR: i32 = -32_700;
/// Invalid Request：插件收到结构非法的请求。
pub const ERR_INVALID_REQUEST: i32 = -32_600;
/// Method not found：插件未实现该方法。
pub const ERR_METHOD_NOT_FOUND: i32 = -32_601;
/// Invalid params：插件判定参数非法。
pub const ERR_INVALID_PARAMS: i32 = -32_602;
/// Internal error：插件内部未分类异常。
pub const ERR_INTERNAL_ERROR: i32 = -32_603;

// --- §4.2 自定义错误码 ---

/// `plugin_busy`：插件正忙（如对同一 `file_id` 并发 `parse`）。
pub const ERR_PLUGIN_BUSY: i32 = -32_001;
/// `file_load_failed`：`load_file` 失败（文件缺失 / 无读权限 / 格式明显不符）。
pub const ERR_FILE_LOAD_FAILED: i32 = -32_002;
/// `parse_failed`：解析中途失败（数据损坏、内部异常等）。
pub const ERR_PARSE_FAILED: i32 = -32_003;
/// `cancelled`：请求被宿主取消（`cancel_parse` 生效）。
pub const ERR_CANCELLED: i32 = -32_004;
/// `unsupported_in_v1`：能力未开启或 v1 不支持。
pub const ERR_UNSUPPORTED_IN_V1: i32 = -32_005;
