//! 引擎构建脚本（M1 自 core/ab-app/build.rs 机械迁移）：扫描仓库 `plugins/`
//! 目录（含 `plugin.json` 的直接子目录），生成 `gen/builtin_ids.rs`
//! （`pub const BUILTIN_PLUGIN_IDS: &[&str]`），供 lib.rs `include!` 接线——
//! 任何新增内建模块无需改代码即自动纳入清单（任务 4 机制，行为不变）。
//!
//! 路径语义：ab-engine 位于 `core/ab-engine`，`CARGO_MANIFEST_DIR/../..`
//! 与原 core/ab-app 同样指向 workspace 根，扫描相对路径原样保留。
//! core/ab-app/build.rs 依赖本脚本先行执行（Cargo 依赖拓扑序），把本产物
//! 复制到自家 `gen/`（`tests/builtin_ids_test.rs` 以 `include!` 消费该路径）。

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    generate_builtin_ids();
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
        "// 由 core/ab-engine/build.rs 自动生成（任务 4 机制，M1 迁入）——勿手改。\n\
         // 内容 = 仓库 plugins/ 下含 plugin.json 的直接子目录名（按名排序）。\n\
         {body}\n"
    );

    let out_dir = manifest_dir.join("gen");
    fs::create_dir_all(&out_dir).expect("failed to create gen dir");
    fs::write(out_dir.join("builtin_ids.rs"), out).expect("failed to write gen/builtin_ids.rs");
}
