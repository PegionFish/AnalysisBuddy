//! 三源插件发现扫描器（host-runtime.md §1；protocol.md §7.1）。
//!
//! 固定优先级 Portable(0) > InstallDir(1) > UserData(2)；三源全部参与扫描，
//! 只扫直接子文件夹、不递归、符号链接不跟随、只读不写。同 id 冲突由高优先级
//! 源胜出，落败者进 `shadowed`。

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ab_protocol::manifest::Manifest;
use tokio::sync::broadcast;

use crate::manifest::{normalize_match_rules, resolve_entry, DiscoveryError, ResolvedEntry};
use crate::HostEvent;

/// 发现源（携带固定优先级 0/1/2，§1.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    Portable,
    InstallDir,
    UserData,
}

impl PluginSource {
    /// 固定优先级，数字越小越高（§1.1）。
    pub fn priority(self) -> u8 {
        match self {
            PluginSource::Portable => 0,
            PluginSource::InstallDir => 1,
            PluginSource::UserData => 2,
        }
    }
}

impl fmt::Display for PluginSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginSource::Portable => write!(f, "Portable"),
            PluginSource::InstallDir => write!(f, "InstallDir"),
            PluginSource::UserData => write!(f, "UserData"),
        }
    }
}

/// 校验通过的插件（§1.3）。
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredPlugin {
    pub manifest: Manifest,
    /// 插件单元根目录（`plugin.json` 所在目录，绝对路径）。
    pub plugin_dir: PathBuf,
    pub source: PluginSource,
    /// 启动入口解析结果（扫描期完成解析，拉起时直接消费，宿主本地扩展字段）。
    pub resolved: ResolvedEntry,
}

/// 非法子文件夹（列出但不拉起，UI 标记 reason）。
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidPlugin {
    pub dir: PathBuf,
    pub source: PluginSource,
    pub reason: DiscoveryError,
}

/// 被同 id 高优先级源覆盖的项（告警用，§1.4）。
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowedPlugin {
    pub id: String,
    pub plugin_dir: PathBuf,
    pub source: PluginSource,
    pub winner_source: PluginSource,
}

/// 单轮扫描的完整结果（§1.3）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveryOutcome {
    pub plugins: Vec<DiscoveredPlugin>,
    pub invalid: Vec<InvalidPlugin>,
    pub shadowed: Vec<ShadowedPlugin>,
}

/// 插件注册表：三源发现 + 内存缓存 + 「重载」唯一缓存失效入口（§1.5）。
pub struct PluginRegistry {
    portable: PathBuf,
    install: PathBuf,
    user_data: PathBuf,
    cache: Mutex<Option<Arc<DiscoveryOutcome>>>,
    disabled: Mutex<HashSet<String>>,
    events: broadcast::Sender<HostEvent>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRegistry {
    /// 按宿主默认路径构造：Portable = `<exe 所在目录>/plugins`；InstallDir 在纯
    /// ZIP 布局下与 Portable 同路径（视为同一源，按 Portable 计）；UserData =
    /// `%APPDATA%\AnalysisBuddy\plugins`。
    pub fn new() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default();
        let portable = exe_dir.join("plugins");
        let install = portable.clone();
        let user_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("AnalysisBuddy")
            .join("plugins");
        Self::with_sources(portable, install, user_data)
    }

    /// 显式三源路径（测试与宿主注入用）。路径相等的源去重（视为同一源）。
    pub fn with_sources(portable: PathBuf, install: PathBuf, user_data: PathBuf) -> Self {
        Self {
            portable,
            install,
            user_data,
            cache: Mutex::new(None),
            disabled: Mutex::new(HashSet::new()),
            events: broadcast::channel(256).0,
        }
    }

    /// 惰性全量扫描：首次调用执行完整扫描并缓存，后续返回缓存（§1.5）。
    /// 唯一缓存失效入口是 [`Self::reload`]。
    pub fn discover(&self) -> DiscoveryOutcome {
        let mut guard = self.cache.lock().expect("registry cache lock poisoned");
        if guard.is_none() {
            *guard = Some(Arc::new(self.scan_all()));
        }
        guard.as_ref().expect("cache populated").as_ref().clone()
    }

    /// 丢弃缓存 → 全量重扫三源 → 重新校验与裁决 → 发布 `HostEvent::PluginsReloaded`
    /// 附带明细（§1.5）。已 Ready 的活会话不受影响（本模块只负责发现）。
    pub fn reload(&self) -> DiscoveryOutcome {
        let outcome = self.scan_all();
        *self.cache.lock().expect("registry cache lock poisoned") = Some(Arc::new(outcome.clone()));
        let _ = self.events.send(HostEvent::PluginsReloaded {
            plugins: outcome.plugins.clone(),
            invalid: outcome.invalid.clone(),
            shadowed: outcome.shadowed.clone(),
        });
        outcome
    }

    /// 三个发现源目录（Portable / InstallDir / UserData，§1.1；路径相等的
    /// 源扫描期视为同一源）。只读访问——ab-app 幽灵行判定需按全部源目录
    /// 检查插件单元是否存在（禁用中的非便携源插件不在发现列表）。
    pub fn source_dirs(&self) -> [PathBuf; 3] {
        [
            self.portable.clone(),
            self.install.clone(),
            self.user_data.clone(),
        ]
    }

    /// 按 id 索引（§7.1）。
    pub fn get(&self, id: &str) -> Option<DiscoveredPlugin> {
        self.discover()
            .plugins
            .into_iter()
            .find(|p| p.manifest.id == id)
    }

    /// 全部已发现插件（宿主本地，供 UI 插件管理页）。
    pub fn list(&self) -> Vec<DiscoveredPlugin> {
        self.discover().plugins
    }

    /// 整体替换禁用集合并触发 [`Self::reload`]（禁用模块从发现列表消失，
    /// 但 id 仍可通过 [`Self::is_disabled`] 查询）。
    pub fn set_disabled(&self, ids: &[String]) -> DiscoveryOutcome {
        let mut guard = self
            .disabled
            .lock()
            .expect("registry disabled lock poisoned");
        guard.clear();
        guard.extend(ids.iter().cloned());
        drop(guard);
        self.reload()
    }

    /// 查询 id 是否在禁用集合中。
    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled
            .lock()
            .expect("registry disabled lock poisoned")
            .contains(id)
    }

    /// 订阅发现级事件（目前仅有 `PluginsReloaded`）。
    pub fn subscribe_events(&self) -> broadcast::Receiver<HostEvent> {
        self.events.subscribe()
    }

    /// 三源全量扫描 + 同 id 冲突裁决（§1.2 / §1.4）。
    fn scan_all(&self) -> DiscoveryOutcome {
        let mut plugins: BTreeMap<String, DiscoveredPlugin> = BTreeMap::new();
        let mut invalid: Vec<InvalidPlugin> = Vec::new();
        let mut shadowed: Vec<ShadowedPlugin> = Vec::new();
        let mut seen_dirs: Vec<PathBuf> = Vec::new();

        let sources = [
            (PluginSource::Portable, &self.portable),
            (PluginSource::InstallDir, &self.install),
            (PluginSource::UserData, &self.user_data),
        ];
        for (source, dir) in sources {
            // ZIP 布局下 InstallDir 与 Portable 同路径 → 视为同一源，按 Portable 计（§1.1）。
            if seen_dirs.iter().any(|d| d == dir) {
                continue;
            }
            seen_dirs.push(dir.clone());
            Self::scan_source(dir, source, &mut plugins, &mut invalid, &mut shadowed);
        }

        let mut list: Vec<DiscoveredPlugin> = plugins.into_values().collect();
        // 禁用集合过滤（§1.5）：禁用模块从发现列表消失，但 id 仍可通过
        // is_disabled 查询；过滤发生在扫描结果上，list()/discover()/reload() 一致。
        let disabled = self
            .disabled
            .lock()
            .expect("registry disabled lock poisoned");
        list.retain(|p| !disabled.contains(&p.manifest.id));
        drop(disabled);
        list.sort_by(|a, b| {
            (a.source.priority(), &a.plugin_dir).cmp(&(b.source.priority(), &b.plugin_dir))
        });
        DiscoveryOutcome {
            plugins: list,
            invalid,
            shadowed,
        }
    }

    /// 扫描单个源目录（§1.2 五条硬规则）。
    fn scan_source(
        dir: &Path,
        source: PluginSource,
        plugins: &mut BTreeMap<String, DiscoveredPlugin>,
        invalid: &mut Vec<InvalidPlugin>,
        shadowed: &mut Vec<ShadowedPlugin>,
    ) {
        // 源目录不存在视为「该源为空」，不报错、不创建（§1.1）。
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let ft = e.file_type().ok()?;
                // 符号链接子文件夹不跟随（junction 场景，§1.2 规则 4）。
                if ft.is_symlink() || !ft.is_dir() {
                    return None;
                }
                Some(e.path())
            })
            .collect();
        // 同优先级源 id 重复按目录名字典序取先者（§1.4）。
        dirs.sort();

        for dir in dirs {
            match Self::scan_plugin(&dir, source) {
                Ok(plugin) => {
                    // 已在更高（或同优先先序）源登记的同 id → 落败者进 shadowed；
                    // 高优先级源先扫，因此该分支天然实现「高优先级覆盖低优先级」（§1.4）。
                    let id = plugin.manifest.id.clone();
                    if let Some(prev) = plugins.get(&id) {
                        shadowed.push(ShadowedPlugin {
                            id,
                            plugin_dir: plugin.plugin_dir,
                            source: plugin.source,
                            winner_source: prev.source,
                        });
                    } else {
                        plugins.insert(id, plugin);
                    }
                }
                Err(reason) => invalid.push(InvalidPlugin {
                    dir,
                    source,
                    reason,
                }),
            }
        }
    }

    /// 单个候选插件单元：加载 → 校验 → 规范化 → 解析入口（§2 降级策略）。
    fn scan_plugin(dir: &Path, source: PluginSource) -> Result<DiscoveredPlugin, DiscoveryError> {
        let manifest = crate::manifest::load_manifest(dir)?;
        crate::manifest::validate(&manifest)?;
        let mut manifest = manifest;
        normalize_match_rules(&mut manifest.r#match);
        let resolved = resolve_entry(&manifest, dir)?;
        Ok(DiscoveredPlugin {
            manifest,
            plugin_dir: dir.to_path_buf(),
            source,
            resolved,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_order_is_fixed() {
        assert_eq!(PluginSource::Portable.priority(), 0);
        assert_eq!(PluginSource::InstallDir.priority(), 1);
        assert_eq!(PluginSource::UserData.priority(), 2);
    }
}
