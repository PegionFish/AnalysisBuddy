//! AnalysisBuddy Plugin Protocol 共享类型——契约唯一事实来源。
//!
//! 设计正本见 `protocol.md`（AnalysisBuddy-devdocs/deep-dive/），Phase 1
//! 契约冻结（tag `contract-v1`）后同步至 `docs/spec/protocol-v1.md`。
//! 本 crate 仅承载类型定义，不含任何业务逻辑。
//!
//! # 序列化契约（protocol.md §3.1「可选字段序列化约定」）
//!
//! - 字段名全 snake_case（serde 默认）；
//! - 可选字段 skip-if-empty：`None`、空字符串、空 map 一律不输出该键，
//!   禁止输出 `null` 或空容器（Rust 侧统一 `#[serde(skip_serializing_if = ...)]`，
//!   TS/Python SDK 序列化器行为相同）。
//!
//! `Record.value` 为 `f64`：`NaN` / `±Infinity` 在 JSON 中不可表示，序列化时
//! 直接报错（`#[serde(serialize_with)]` 显式拦截——serde_json ≥ 1.0.132 对非有限
//! 数默认静默输出 `null`，与契约相悖；插件侧自行过滤或置 0）。

pub mod errors;
pub mod manifest;
pub mod types;

#[cfg(test)]
mod serde_tests;

use std::collections::BTreeMap;

/// 当前协议版本；`InitializeParams.protocol_version` 必须等于此值。
pub const PROTOCOL_VERSION: u32 = 1;

/// skip-if-empty 谓词：`Option<String>` 为 `None` 或空字符串时省略该键。
pub(crate) fn skip_if_empty_str(value: &Option<String>) -> bool {
    match value {
        None => true,
        Some(s) => s.is_empty(),
    }
}

/// skip-if-empty 谓词：`Option<BTreeMap>` 为 `None` 或空 map 时省略该键。
pub(crate) fn skip_if_empty_map(value: &Option<BTreeMap<String, String>>) -> bool {
    value.as_ref().is_none_or(BTreeMap::is_empty)
}

/// skip-if-empty 谓词：`Option<serde_json::Map>` 为 `None` 或空 map 时省略该键。
pub(crate) fn skip_if_empty_json_map(
    value: &Option<serde_json::Map<String, serde_json::Value>>,
) -> bool {
    value.as_ref().is_none_or(serde_json::Map::is_empty)
}
