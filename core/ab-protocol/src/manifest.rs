//! 插件清单（plugin.json）类型（protocol.md §7.2）。

use serde::{Deserialize, Serialize};

/// §7.2 插件清单。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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
    /// 可选元信息：作者。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// 可选元信息：源码仓库地址。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// 可选元信息：宿主适配要求，每项形如 `{tool} {semver 约束}`（如 `AnalysisBuddy >= 0.1.0`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// 可选元信息：更新源 URL。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_url: Option<String>,
    /// 可选元信息：更新日志（版本严格降序）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<Vec<ChangelogEntry>>,
    /// 可选：场景预设集合（≤32；宿主侧过滤非法项）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<Vec<PresetDef>>,
}

/// §7.2 更新日志条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangelogEntry {
    /// semver 版本号。
    pub version: String,
    /// 发布日期，`YYYY-MM-DD`。
    pub date: String,
    /// 变更说明。
    pub notes: Vec<String>,
}

/// §7.2 启动入口。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MatchRules {
    /// 认领的扩展名（小写无点）；空数组表示仅靠指纹匹配。
    pub extensions: Vec<String>,
    /// 可选：文件头指纹（大小写不敏感的子串匹配，任一命中即候选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_fingerprints: Option<Vec<String>>,
}

/// 双语文本（zh/en 均必填）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LocalizedName {
    pub zh: String,
    pub en: String,
}

/// 预设条目：want 语义槽 + names 候选列表。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresetEntry {
    /// 语义槽 id（可选；同 want 仅首个命中生效）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub want: Option<String>,
    /// 候选名（规范化 metric_id 或原始指标名）。
    pub names: Vec<String>,
}

/// 预设标签分组（核心不解释语义）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresetGroup {
    pub id: String,
    pub name: LocalizedName,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<PresetEntry>,
}

/// §7.2 场景预设（附加段；与用户预设同构）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresetDef {
    /// `^[a-z0-9][a-z0-9-_]{0,63}$`。
    pub id: String,
    /// 强制双语展示名。
    pub name: LocalizedName,
    /// 可选双语描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<LocalizedName>,
    /// 顶层条目：对所有分组生效。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<PresetEntry>,
    /// 可选标签分组（平台/供应商/任意分类，核心不解释语义）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<PresetGroup>,
    /// 模糊兜底关键词（仅穷举整体零命中时启用）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}
