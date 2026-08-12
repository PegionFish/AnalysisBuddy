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
//! 此外，本脚本扫描仓库 `plugins/` 目录（含 `plugin.json` 的直接子目录），
//! 生成 `gen/builtin_ids.rs`（`pub const BUILTIN_PLUGIN_IDS: &[&str]`），
//! 供 lib.rs/测试 `include!`——任何新增内建模块无需改代码即自动纳入清单
//! （任务 4：内建模块 id 清单）。

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
];

fn main() {
    generate_builtin_ids();

    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(REGISTERED_COMMANDS)),
    )
    .expect("failed to run tauri_build");
}

/// 扫描 `CARGO_MANIFEST_DIR/../../plugins` 下含 `plugin.json` 的直接子目录，
/// 按目录名（即插件 id）生成 `gen/builtin_ids.rs` 常量文件。
/// 目录增删会触发重跑（`cargo:rerun-if-changed`），清单随之刷新。
fn generate_builtin_ids() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let plugins_dir = manifest_dir.join("../../plugins");

    println!("cargo:rerun-if-changed={}", plugins_dir.display());

    let mut ids: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&plugins_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() && path.join("plugin.json").is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    ids.push(name.to_string());
                }
            }
        }
    }
    ids.sort();
    ids.dedup();

    // rustfmt 稳定格式：短清单单行；超宽（>100 列）转垂直、每项一行。
    let body = {
        let inline = ids
            .iter()
            .map(|id| format!("{id:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let one_line = format!("pub const BUILTIN_PLUGIN_IDS: &[&str] = &[{inline}];");
        if one_line.len() <= 100 {
            one_line
        } else {
            let items = ids
                .iter()
                .map(|id| format!("    {id:?},"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("pub const BUILTIN_PLUGIN_IDS: &[&str] = &[\n{items}\n];")
        }
    };
    let out = format!(
        "// 由 core/ab-app/build.rs 自动生成（任务 4）——勿手改。\n\
         // 内容 = 仓库 plugins/ 下含 plugin.json 的直接子目录名（按名排序）。\n\
         {body}\n"
    );

    let out_dir = manifest_dir.join("gen");
    fs::create_dir_all(&out_dir).expect("failed to create gen dir");
    fs::write(out_dir.join("builtin_ids.rs"), out).expect("failed to write gen/builtin_ids.rs");
}
