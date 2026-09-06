//! 引擎装配（ab-engine smoke.rs 同款步骤的 superset）与共享 [`AppState`]。
//!
//! 装配顺序：建目录（presets/sessions）→ `PluginRegistry::with_sources` →
//! 种入禁用集（`.ab-modules.json`，set_disabled 内部触发 reload）→
//! `discover()` → `PluginRuntime` → unbounded 管线事件通道 →
//! `ImportCoordinator::with_config` → 启动 hub forwarder → `GitHubFetcher`
//! （构造失败 fail-fast）→ `JobRegistry`。

use std::sync::Arc;

use ab_engine::commands::plugin_manager::load_module_state;
use ab_engine::events::{PluginLogBuffer, PluginMeta};
use ab_engine::network::{GitHubFetcher, UpdateFetcher};
use ab_engine::paths::EnginePaths;
use ab_engine::pipeline_bridge::{ImportCoordinator, PipelineConfig};
use ab_host::{PluginRegistry, PluginRuntime, RuntimeConfig};
use ab_pipeline::{PipelineEvent, SessionRegistry, Store};

use crate::hub::{self, EventHub};
use crate::jobs::JobRegistry;

/// 全部处理器共享的引擎状态（Clone 便宜：全 Arc）。
#[derive(Clone)]
pub struct AppState {
    pub coordinator: Arc<ImportCoordinator>,
    pub discovery: Arc<PluginRegistry>,
    pub host: Arc<PluginRuntime>,
    pub meta: Arc<PluginMeta>,
    pub log_buffer: Arc<PluginLogBuffer>,
    pub fetcher: Arc<dyn UpdateFetcher>,
    pub paths: EnginePaths,
    pub jobs: Arc<JobRegistry>,
    pub hub: Arc<EventHub>,
    /// Bearer 令牌（None = 认证关闭）。
    pub token: Option<Arc<str>>,
}

/// 装配选项（CLI / 测试注入）。
#[derive(Clone, Default)]
pub struct AssembleOptions {
    /// 并发导入上限（≥1，内部 clamp）。
    pub max_concurrent_imports: usize,
    /// Bearer 令牌（None = 认证关闭）。
    pub token: Option<String>,
    /// file_id 生成器（测试固定 id 对齐剧本；生产 None → 随机 UUID 形）。
    pub file_id_fn: Option<Arc<dyn Fn(u64) -> String + Send + Sync>>,
}

/// 装配引擎并启动事件转发任务。目录不存在时创建 presets/sessions
/// （save/preset 写路径假设目录存在）；失败以 Err(String) 返回给调用方
/// 打印退出（fail-fast，服务不半启动）。
pub fn assemble(paths: EnginePaths, options: AssembleOptions) -> Result<AppState, String> {
    std::fs::create_dir_all(&paths.presets_dir).map_err(|e| {
        format!(
            "cannot create presets dir {}: {e}",
            paths.presets_dir.display()
        )
    })?;
    std::fs::create_dir_all(&paths.sessions_dir).map_err(|e| {
        format!(
            "cannot create sessions dir {}: {e}",
            paths.sessions_dir.display()
        )
    })?;

    let discovery = Arc::new(PluginRegistry::with_sources(
        paths.plugins_portable.clone(),
        paths.plugins_install.clone(),
        paths.plugins_user.clone(),
    ));
    // 种入禁用集（状态文件 `.ab-modules.json` 落便携源目录，spec §3.2）。
    // set_disabled 内部触发 reload 广播（早期无订阅者，事件自然丢弃）。
    let disabled = load_module_state(&paths.plugins_portable);
    if !disabled.is_empty() {
        let ids: Vec<String> = disabled.into_iter().collect();
        discovery.set_disabled(&ids);
    }
    discovery.discover();

    let host = Arc::new(PluginRuntime::with_config(
        discovery.clone(),
        RuntimeConfig::default(),
    ));

    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel::<PipelineEvent>();
    let coordinator = Arc::new(ImportCoordinator::with_config(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
        events_tx,
        host.clone(),
        discovery.clone(),
        PipelineConfig {
            file_id_fn: options.file_id_fn,
            ..PipelineConfig::default()
        },
    ));

    let meta = Arc::new(PluginMeta::new());
    let log_buffer = Arc::new(PluginLogBuffer::new());
    let hub = Arc::new(EventHub::new());
    hub::spawn_forwarder(
        host.subscribe_events(),
        events_rx,
        (*hub).clone(),
        Arc::clone(&meta),
        Arc::clone(&log_buffer),
    );

    // 更新流唯一网络入口（生产 GitHubFetcher；构造失败 fail-fast——
    // 初始化问题不应拖到第一次更新请求时才爆）。
    let fetcher: Arc<dyn UpdateFetcher> = Arc::new(
        GitHubFetcher::new().map_err(|e| format!("failed to create GitHub fetcher: {e}"))?,
    );

    Ok(AppState {
        coordinator,
        discovery,
        host,
        meta,
        log_buffer,
        fetcher,
        paths,
        jobs: Arc::new(JobRegistry::new(options.max_concurrent_imports)),
        hub,
        token: options.token.map(Arc::from),
    })
}
