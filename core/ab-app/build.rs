//! tauri-build 入口：通过 `AppManifest::commands` 注册全部 invoke 命令，
//! tauri-build 据此自动生成 `allow-<command>`/`deny-<command>` ACL 权限
//! （tauri-utils `autogenerate_command_permissions`），供
//! `capabilities/default.json` 引用。缺此注册 + capabilities 文件时，
//! Tauri 2 ACL 会静默拒绝所有 invoke（任务 12 根因）。
//!
//! 该清单同时是回归测试的"生产命令注册集"事实来源
//! （tests/capabilities_test.rs 与 ui/src/ipc/real.ts、lib.rs
//! `generate_handler!` 三方交叉校验）。

/// 生产 invoke 命令注册集（须与 lib.rs `generate_handler!` 逐一同步）。
pub const REGISTERED_COMMANDS: &[&str] = &[
    "list_plugins",
    "import_files",
    "unload_file",
    "get_metrics",
    "query_series",
    "key_values_at",
    "save_session",
    "load_session",
    "get_plugin_log",
    "reload_plugin",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(REGISTERED_COMMANDS)),
    )
    .expect("failed to run tauri_build");
}
