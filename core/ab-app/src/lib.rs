//! AnalysisBuddy Tauri 2 桌面壳 + 主机胶水（P3-01）：接线 `PluginRuntime`——
//! 启动时发现（host-runtime.md §7.1）、退出时 `shutdown_all()`（§3.4 孤儿防护
//! 第 2 层）；`HostEvent` → `ab://plugin-health` / `ab://plugin-log` /
//! `ab://progress` 事件转发（ipc-ui.md §2）。
//!
//! `--smoke-host`：对 mock-plugin 走全流程冒烟（本卡验证命令）。

pub mod events;
pub mod host_bridge;
pub mod smoke;

use std::sync::Arc;

use ab_host::{PluginRegistry, PluginRuntime};
use tauri::{Emitter, Manager};

/// 应用入口（main.rs 调用）：`--smoke-host` 走冒烟，否则拉起 Tauri 壳。
pub fn run() {
    if std::env::args().any(|arg| arg == "--smoke-host") {
        std::process::exit(smoke::run_smoke());
    }
    run_tauri();
}

fn run_tauri() {
    // 启动时发现（§7.1 三源扫描，惰性缓存）。
    let registry = Arc::new(PluginRegistry::new());
    registry.discover();
    let host = PluginRuntime::new(registry);

    let app = tauri::Builder::default()
        .setup(move |app| {
            wire_events(app.handle().clone(), &host);
            app.manage(HostState {
                runtime: tokio::runtime::Runtime::new().expect("tokio runtime"),
                host,
            });
            Ok(())
        })
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
    host: PluginRuntime,
}

/// `HostEvent` → 三个 `ab://*` 通道的转发任务（ipc-ui.md §2）。
fn wire_events(app_handle: tauri::AppHandle, host: &PluginRuntime) {
    let mut receiver = host.subscribe_events();
    let mut throttle = events::ProgressThrottle::new();
    tauri::async_runtime::spawn(async move {
        while let Ok(event) = receiver.recv().await {
            for emitted in events::convert(event, &mut throttle) {
                let result = match emitted.payload {
                    events::EventPayload::Health(payload) => {
                        app_handle.emit(events::EV_PLUGIN_HEALTH, payload)
                    }
                    events::EventPayload::Log(payload) => {
                        app_handle.emit(events::EV_PLUGIN_LOG, payload)
                    }
                    events::EventPayload::Progress(payload) => {
                        app_handle.emit(events::EV_PROGRESS, payload)
                    }
                };
                if let Err(e) = result {
                    eprintln!("WARN ab-app: emit {} failed: {e}", emitted.channel);
                }
            }
        }
    });
}
