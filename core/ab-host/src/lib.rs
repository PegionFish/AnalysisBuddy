//! AnalysisBuddy 插件运行时（A 路）：插件发现、进程生命周期、JSON-RPC 帧、
//! 超时与健康监控。实现依据 `host-runtime.md`（AnalysisBuddy-devdocs/deep-dive/）。

pub mod discovery;
pub mod manifest;

pub use discovery::{
    DiscoveredPlugin, DiscoveryOutcome, InvalidPlugin, PluginRegistry, PluginSource, ShadowedPlugin,
};
pub use manifest::{load_manifest, resolve_entry, validate, DiscoveryError, ResolvedEntry};

/// 宿主事件流（§7.7，宿主本地）。`PluginsReloaded` 由 [`PluginRegistry`] 发布；
/// 其余变体随 A-02/A-03 落地。
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// 插件重载完成，附带发现明细（§1.5）。
    PluginsReloaded {
        plugins: Vec<DiscoveredPlugin>,
        invalid: Vec<InvalidPlugin>,
        shadowed: Vec<ShadowedPlugin>,
    },
}
