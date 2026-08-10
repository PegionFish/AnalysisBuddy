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
pub mod events;
pub mod host_bridge;
pub mod ipc_errors;
pub mod pipeline_bridge;
pub mod smoke;

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
    run_tauri();
}

fn run_tauri() {
    // 启动时发现（§7.1 三源扫描，惰性缓存）。
    let discovery = Arc::new(PluginRegistry::new());
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::plugin::list_plugins,
            commands::import::import_files,
            commands::import::unload_file,
            commands::query::get_metrics,
            commands::query::query_series,
            commands::query::key_values_at,
            commands::session::save_session,
            commands::session::load_session,
            commands::plugin::get_plugin_log,
            commands::plugin::reload_plugin,
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
    };
    if let Err(e) = result {
        eprintln!("WARN ab-app: emit {} failed: {e}", emitted.channel);
    }
}
