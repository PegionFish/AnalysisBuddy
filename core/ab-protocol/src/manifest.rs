//! 插件清单（plugin.json）类型（protocol.md §7.2）。

use serde::{Deserialize, Serialize};

/// §7.2 插件清单。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// 全局唯一 id，`^[a-z0-9][a-z0-9-_]{1,63}$`；必须与 `initialize` 响应的 `id` 一致。
    pub id: String,
    /// 展示名。
    pub display_name: String,
    /// semver 版本。
    pub version: String,
    /// 启动入口。
    pub entry: PluginEntry,
    /// 文件匹配规则（发现阶段预筛，正式判定仍走 `can_handle`）。
    #[serde(rename = "match")]
    pub r#match: MatchRules,
    /// 要求的最低协议版本；大于宿主支持版本时不加载并提示升级宿主。
    pub min_protocol_version: u32,
}

/// §7.2 启动入口。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginEntry {
    /// 可执行命令。一律相对 `plugin.json` 所在目录解析，禁止依赖全局 PATH
    /// （唯一例外：解释器型入口按系统约定查找解释器）。
    pub command: String,
    /// 命令行参数（可为空数组）。
    pub args: Vec<String>,
    /// 可选：进程工作目录，相对 `plugin.json` 所在目录解析；
    /// 默认 = `plugin.json` 所在目录。
    #[serde(skip_serializing_if = "crate::skip_if_empty_str")]
    pub working_dir: Option<String>,
}

/// §7.2 文件匹配规则。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchRules {
    /// 认领的扩展名（小写无点）；空数组表示仅靠指纹匹配。
    pub extensions: Vec<String>,
    /// 可选：文件头指纹（大小写不敏感的子串匹配，任一命中即候选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_fingerprints: Option<Vec<String>>,
}
