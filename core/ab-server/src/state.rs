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
    /// 导入路径白名单根（`--import-roots`，WS-B1/P0-2 纵深防御）。
    /// `None` = 不限制（桌面形态）；`Some` 已在装配时做词法规范化。
    pub import_roots: Option<Arc<[std::path::PathBuf]>>,
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
    /// 引擎内存预算硬顶（Quest M4.2；None = 不设限）。进
    /// `PipelineConfig.memory_budget_bytes`：超限文件的导入 outcome error
    /// `memory_budget_exceeded`，同批其他文件不受影响。
    pub memory_budget_bytes: Option<u64>,
    /// 导入路径白名单根（`--import-roots`，WS-B1/P0-2 纵深防御）。
    /// `None` = 不限制（桌面形态，本地路径能力完整保留）。
    pub import_roots: Option<Vec<std::path::PathBuf>>,
}

/// 装配引擎并启动事件转发任务。目录不存在时创建 presets/sessions
/// （save/preset 写路径假设目录存在）；失败以 Err(String) 返回给调用方
/// 打印退出（fail-fast，服务不半启动）。
pub fn assemble(paths: EnginePaths, options: AssembleOptions) -> Result<AppState, String> {
    // WS-B2（P0-4）：启动清扫——移除本上传根下其他已死进程的历史副本目录
    //（kill -9 残留兜底；同 pid 与存活进程目录跳过，见 jobs.rs）。
    crate::jobs::sweep_stale_uploads();
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
            memory_budget_bytes: options.memory_budget_bytes,
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
        // WS-B1：roots 装配时做词法规范化（`.`/`..`），此后路由层每次
        // 校验只规范化候选路径（与 routes::normalize_lexical 同一算法）。
        import_roots: options.import_roots.map(|roots| {
            Arc::from(
                roots
                    .iter()
                    .map(|root| crate::routes::normalize_lexical(root))
                    .collect::<Vec<_>>(),
            )
        }),
    })
}
