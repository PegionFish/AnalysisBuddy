#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! AnalysisBuddy 桌面壳（Tauri 2）入口。业务逻辑与主机胶水在 `ab_app` lib
//! （P3-01 起）；UI 挂载点指向 `ui/`（Vite dev server / 构建产物）。

fn main() {
    ab_app::run();
}
