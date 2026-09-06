//! tauri-build 入口：通过 `AppManifest::commands` 注册全部 invoke 命令，
//! tauri-build 据此自动生成 `allow-<command>`/`deny-<command>` ACL 权限
//! （tauri-utils `autogenerate_command_permissions`），供
//! `capabilities/default.json` 引用。缺此注册 + capabilities 文件时，
//! Tauri 2 ACL 会静默拒绝所有 invoke（任务 12 根因）。
//!
//! 该清单同时是回归测试的"生产命令注册集"事实来源
//! （tests/capabilities_test.rs 与 ui/src/ipc/real.ts、lib.rs
//! `generate_handler!` 三方交叉校验）。
//!
//! builtin_ids 生成已迁至 core/ab-engine/build.rs（M1）：扫描仓库 `plugins/`
//! 目录（含 `plugin.json` 的直接子目录），生成
//! `gen/builtin_ids.rs`（`pub const BUILTIN_PLUGIN_IDS: &[&str]`）。
//! 本脚本依赖 Cargo 依赖拓扑序（ab-engine 是 ab-app 依赖，其 build script
//! 先行执行）把引擎产物复制到自家 `gen/builtin_ids.rs`——
//! tests/builtin_ids_test.rs `include!` 本 crate gen/ 路径，文件必须存在。
//! 任何新增内建模块无需改代码即自动纳入清单（任务 4：内建模块 id 清单）。

use std::env;
use std::fs;
use std::path::PathBuf;

/// 生产 invoke 命令注册集（须与 lib.rs `generate_handler!` 逐一同步）。
pub const REGISTERED_COMMANDS: &[&str] = &[
    "list_plugins",
    "import_files",
    "unload_file",
    "cancel_parse",
    "get_metrics",
    "query_series",
    "key_values_at",
    "save_session",
    "load_session",
    "get_plugin_log",
    "reload_plugin",
    "install_plugin_zip",
    "uninstall_plugin",
    "set_plugin_enabled",
    "check_plugin_update",
    "update_plugin",
    "list_user_presets",
    "save_user_preset",
    "delete_user_preset",
];

fn main() {
    sync_builtin_ids_from_engine();

    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(REGISTERED_COMMANDS)),
    )
    .expect("failed to run tauri_build");
}

/// 自 core/ab-engine 生成产物复制 `gen/builtin_ids.rs`（扫描逻辑与生成
/// 格式原样在 `core/ab-engine/build.rs`；其产物含 engine 侧头注释，
/// 内容常量与迁移前逐值一致）。依赖顺序保证产物已就绪。
fn sync_builtin_ids_from_engine() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let engine_gen = manifest_dir.join("../ab-engine/gen/builtin_ids.rs");
    println!("cargo:rerun-if-changed={}", engine_gen.display());
    let content = fs::read_to_string(&engine_gen).unwrap_or_else(|e| {
        panic!(
            "cannot read ab-engine generated builtin ids ({}): {e}",
            engine_gen.display()
        )
    });
    let out_dir = manifest_dir.join("gen");
    fs::create_dir_all(&out_dir).expect("failed to create gen dir");
    fs::write(out_dir.join("builtin_ids.rs"), content).expect("failed to write gen/builtin_ids.rs");
}
