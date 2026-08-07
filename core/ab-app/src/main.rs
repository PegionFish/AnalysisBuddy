#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! AnalysisBuddy 桌面壳（Tauri 2）：占位主窗口，仅验证工具链可编译。
//! UI 挂载点指向 `ui/`（Vite dev server / 构建产物），业务逻辑全部留待后续卡。

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running AnalysisBuddy");
}

/// 验证 test harness 可用的占位单测。
#[test]
fn test_harness_works() {
    assert!(true);
}
