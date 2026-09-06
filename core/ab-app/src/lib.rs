//! AnalysisBuddy Tauri 2 桌面壳 + 主机胶水（P3-01/P3-02）：接线 `PluginRuntime`——
//! 启动时发现（host-runtime.md §7.1）、退出时 `shutdown_all()`（§3.4 孤儿防护
//! 第 2 层）；`HostEvent` → `ab://plugin-health` / `ab://plugin-log` /
//! `ab://progress` 事件转发（ipc-ui.md §2）；`ImportCoordinator`（P3-02）接管
//! 导入→解析→存储→查询全链路，`PipelineEvent` → `ab://progress` + command 侧
//! 状态翻转（ipc-ui.md §2.1）。
//!
//! `--smoke-host`：对 mock-plugin 走 A 层冒烟；`--smoke-pipeline`：走
//! 导入→解析→查询全链路冒烟（P3-02 验证命令；fixture 由 F 路交付，此前以
//! mock-plugin 剧本驱动）。

pub mod commands;
pub mod webview2;

// M1 提取（core/ab-engine）：纯 Rust 引擎核心整体再导出——导入编排/事件/
// 宿主适配/IPC 错误/更新网络/冒烟装配。`ab_app::<module>::...` 公共 API
// 面由此保持不变（集成测试零修改）。
pub use ab_engine::{events, host_bridge, ipc_errors, network, pipeline_bridge, smoke};

// 内建模块 id 清单：扫描机制迁至 core/ab-engine/build.rs（仓库 plugins/
// 不变），ab-app build.rs 复制其产物到自家 gen/ 供 tests include!；此处
// 转发常量（安装冲突判定与卸载保护依赖，缺接线即运行时保护失效）。
pub use ab_engine::BUILTIN_PLUGIN_IDS;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ab_host::{HostEvent, PluginRegistry, PluginRuntime};
use ab_pipeline::{PipelineEvent, SessionRegistry, Store};
use tauri::{Emitter, Manager};

/// 应用入口（main.rs 调用）：冒烟开关或拉起 Tauri 壳。
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--smoke-host") {
        std::process::exit(smoke::run_smoke());
    }
    if args.iter().any(|arg| arg == "--smoke-pipeline") {
        std::process::exit(smoke::run_smoke_pipeline());
    }
    // P4-01 WebView2 门禁（ipc-ui.md §8.1）：生产路径在建窗前探测运行时；
    // 缺失则弹引导框（打开下载页/退出），不再创建 WebView 窗口。
    // debug 构建跳过探测，`cargo tauri dev` 不受影响。
    if !webview2::ensure_webview2() {
        std::process::exit(1);
    }
    run_tauri();
}

fn run_tauri() {
    // A11y（e2e-uiux-report §6）：WebView2 渲染器 AX 树开关。
    // 本仓库窗口由 tauri.conf.json `app.windows[]` 声明并自动创建，Tauri 无
    // Rust 侧注入点（tauri::setup 在用户 setup 回调前建窗），故唯一生效通道是
    // 窗口级 `additionalBrowserArgs`。期望取值见
    // `webview2::webview2_a11y_browser_args()`，集成时照抄进
    // `tauri.conf.json` `app.windows[0].additionalBrowserArgs`。
    // 启动时发现（§7.1 三源扫描，惰性缓存）。M1：路径公式集中在
    // `desktop_engine_paths()`（与原 `PluginRegistry::new()` 逐值等价），
    // 引擎侧经 with_sources 显式注入，逻辑不再读环境变量。
    let paths = desktop_engine_paths();
    let discovery = Arc::new(PluginRegistry::with_sources(
        paths.plugins_portable.clone(),
        paths.plugins_install.clone(),
        paths.plugins_user.clone(),
    ));
    // 禁用状态持久化（spec §3.2）：启动时从 `.ab-modules.json` 回灌 registry
    // 禁用集合（损坏/缺失回退空集，load_module_state 内处理）；此后
    // set_plugin_enabled 每次按需读写状态文件，无全局缓存。
    let seeded_disabled = commands::plugin_manager::load_module_state(&paths.plugins_portable);
    if !seeded_disabled.is_empty() {
        let ids: Vec<String> = seeded_disabled.into_iter().collect();
        discovery.set_disabled(&ids);
    }
    discovery.discover();
    let host = Arc::new(PluginRuntime::new(discovery.clone()));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // P3-02 组装点（pipeline.md §6）：Store + SessionRegistry +
            // ImportCoordinator；会话经 HostSessionAdapter 惰性填充注册表。
            let (pipeline_tx, pipeline_rx) = tokio::sync::mpsc::unbounded_channel();
            let coordinator = Arc::new(pipeline_bridge::ImportCoordinator::new(
                Arc::new(Store::new()),
                Arc::new(SessionRegistry::new()),
                pipeline_tx,
                host.clone(),
                discovery.clone(),
            ));

            // progress 节流（§2.1 100ms/文件）：host 转发与管线两路共用同一
            // 窗口，同 file_id 双源自然去重。
            let throttle = Arc::new(Mutex::new(events::ProgressThrottle::new()));
            // 插件元数据 / stderr 环形缓冲（list_plugins / get_plugin_log 数据源）。
            let meta = Arc::new(events::PluginMeta::new());
            let log_buffer = Arc::new(events::PluginLogBuffer::new());
            wire_events(
                app.handle().clone(),
                &host,
                throttle.clone(),
                meta.clone(),
                log_buffer.clone(),
            );
            wire_pipeline_events(app.handle().clone(), pipeline_rx, throttle);

            app.manage(coordinator);
            app.manage(HostState {
                runtime: tokio::runtime::Runtime::new().expect("tokio runtime"),
                host,
            });
            app.manage(discovery);
            app.manage(meta);
            app.manage(log_buffer);
            // 模块管理更新流（任务 6）：生产 GitHubFetcher 注入
            // PluginManagerState（构造失败即启动中止，无静默降级）。
            let fetcher: Arc<dyn network::UpdateFetcher> = Arc::new(
                network::GitHubFetcher::new()
                    .map_err(|e| format!("failed to create GitHub fetcher: {e}"))?,
            );
            app.manage(commands::plugin_manager::PluginManagerState { fetcher });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::plugin::list_plugins,
            commands::import::import_files,
            commands::import::unload_file,
            commands::import::cancel_parse,
            commands::query::get_metrics,
            commands::query::query_series,
            commands::query::key_values_at,
            commands::session::save_session,
            commands::session::load_session,
            commands::plugin::get_plugin_log,
            commands::plugin::reload_plugin,
            commands::plugin_manager::install_plugin_zip,
            commands::plugin_manager::uninstall_plugin,
            commands::plugin_manager::set_plugin_enabled,
            commands::plugin_manager::check_plugin_update,
            commands::plugin_manager::update_plugin,
            commands::presets::list_user_presets,
            commands::presets::save_user_preset,
            commands::presets::delete_user_preset,
        ])
        .build(tauri::generate_context!())
        .expect("error while building AnalysisBuddy");

    // 退出时全量停机（§3.4 孤儿防护第 2 层：shutdown → 3s 预算 → kill；
    // 第 3 层由 PluginRuntime Drop sweep 兜底）。
    app.run(|app_handle, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            if let Some(state) = app_handle.try_state::<HostState>() {
                state.runtime.block_on(state.host.shutdown_all());
            }
        }
    });
}

/// 桌面壳路径公式（M1 步骤 4；Windows 公式原样保留）：复刻原
/// `PluginRegistry::new()` 的三源公式（exe 同目录 `plugins`、APPDATA
/// 用户目录）+ presets/sessions 目录，打包为 `EnginePaths` 注入引擎。
/// headless/Linux 宿主改用 `ab_engine::EnginePaths::linux_default()`（XDG）。
fn desktop_engine_paths() -> ab_engine::EnginePaths {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("AnalysisBuddy");
    ab_engine::EnginePaths {
        plugins_portable: exe_dir.join("plugins"),
        plugins_install: exe_dir.join("plugins"),
        plugins_user: appdata.join("plugins"),
        presets_dir: appdata.join("presets"),
        sessions_dir: appdata.join("sessions"),
    }
}

/// 宿主运行时状态（Tauri managed state；停机是 async、run 回调是 sync，
/// 故额外持有同步 runtime 供 block_on）。
struct HostState {
    runtime: tokio::runtime::Runtime,
    host: Arc<PluginRuntime>,
}

/// `HostEvent` → 三个 `ab://*` 通道的转发任务（ipc-ui.md §2）。
/// 先经 `PluginMeta::record` 维护插件元数据，再转换发射；健康载荷在
/// `crashed`/`timeout` 吸收态补失败摘要（§2.3 `detail` 语义）。
fn wire_events(
    app_handle: tauri::AppHandle,
    host: &PluginRuntime,
    throttle: Arc<Mutex<events::ProgressThrottle>>,
    meta: Arc<events::PluginMeta>,
    log_buffer: Arc<events::PluginLogBuffer>,
) {
    let mut receiver = host.subscribe_events();
    tauri::async_runtime::spawn(async move {
        while let Ok(event) = receiver.recv().await {
            meta.record(&event);
            if let HostEvent::StderrLine {
                plugin_id,
                ts_ms,
                line,
            } = &event
            {
                log_buffer.push(events::PluginLogPayload {
                    plugin_id: plugin_id.clone(),
                    level: events::parse_log_level(line),
                    line: line.clone(),
                    ts_ms: *ts_ms,
                });
            }
            for mut emitted in events::convert(event, &mut throttle.lock().unwrap()) {
                if let events::EventPayload::Health(payload) = &mut emitted.payload {
                    if (payload.state == "crashed" || payload.state == "timeout")
                        && payload.detail.is_none()
                    {
                        payload.detail = meta.last_error_of(&payload.plugin_id);
                    }
                }
                emit_one(&app_handle, emitted);
            }
        }
    });
}

/// `PipelineEvent` → `ab://progress` 的转发任务（ipc-ui.md §2.1）：
/// 仅 `ParseProgress` 上线；其余事件驱动 command 侧状态（store Frozen 后
/// `get_metrics`/`query_series` 可查），不虚构线上事件。
fn wire_pipeline_events(
    app_handle: tauri::AppHandle,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<PipelineEvent>,
    throttle: Arc<Mutex<events::ProgressThrottle>>,
) {
    tauri::async_runtime::spawn(async move {
        while let Some(event) = receiver.recv().await {
            for emitted in events::convert_pipeline(event, &mut throttle.lock().unwrap()) {
                emit_one(&app_handle, emitted);
            }
        }
    });
}

fn emit_one(app_handle: &tauri::AppHandle, emitted: events::EmittedEvent) {
    let result = match emitted.payload {
        events::EventPayload::Health(payload) => app_handle.emit(events::EV_PLUGIN_HEALTH, payload),
        events::EventPayload::Log(payload) => app_handle.emit(events::EV_PLUGIN_LOG, payload),
        events::EventPayload::Progress(payload) => app_handle.emit(events::EV_PROGRESS, payload),
        events::EventPayload::PluginsReloaded(payload) => {
            app_handle.emit(events::EV_PLUGINS_RELOADED, payload)
        }
    };
    if let Err(e) = result {
        eprintln!("WARN ab-app: emit {} failed: {e}", emitted.channel);
    }
}

#[cfg(test)]
mod tests {
    /// 任务 4 产物经 lib.rs 接线（crate::BUILTIN_PLUGIN_IDS）：安装冲突
    /// 判定（module_protected）与卸载保护依赖 crate 根常量，缺接线即
    /// 编译期失败（builtin_ids_test 只验产物本身，不验接线）。
    #[test]
    fn builtin_ids_wired_into_crate_root() {
        assert!(!crate::BUILTIN_PLUGIN_IDS.is_empty());
        assert!(
            crate::BUILTIN_PLUGIN_IDS.contains(&"builtin-csv"),
            "首块内建必须经 include! 接线"
        );
    }
}
