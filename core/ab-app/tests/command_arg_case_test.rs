//! 命令参数命名契约防线（任务 21 根因固化）：
//!
//! 根因：`#[tauri::command]` 的 tauri-macros 默认 `ArgumentCase::Camel`
//! （tauri-macros-2.6.3/src/command/wrapper.rs `WrapperAttributes` 初值），
//! 即 JS 侧参数键按 camelCase 接收；而 AnalysisBuddy 前端契约（ipc-ui.md，
//! ui/src/ipc/types.ts）全部 snake_case。修复前 `query_series` 的必填参数
//! `file_ids`/`t0_ms`/`t1_ms`/`max_points_per_series` 在 camelCase 下全部
//! 失配 → 参数反序列化失败 → invoke 拒绝 → 前端 `.catch(() => undefined)`
//! 静默吞掉 → 勾选指标后图表恒空。
//!
//! 防线 1（修复前必失败）：src/commands/ 下每个 `#[tauri::command`
//! 注解必须显式携带 `rename_all = "snake_case"`。
//! 防线 2：`query_series` 的 Rust 参数名集合必须与前端
//! `QuerySeriesArgs`（ui/src/ipc/types.ts）的键集合完全一致。

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const COMMAND_SOURCES: &[&str] = &[
    "src/commands/import.rs",
    "src/commands/plugin.rs",
    "src/commands/query.rs",
    "src/commands/session.rs",
];

/// 防线 1：任何命令注解缺失 `rename_all = "snake_case"` 都会使前端
/// snake_case 参数静默失配（camelCase 默认值），必须编译期之外再加这道
/// 静态断言——宏展开后的命名在普通单测里不可见。
#[test]
fn every_tauri_command_declares_snake_case_args() {
    for rel in COMMAND_SOURCES {
        let path = manifest_dir().join(rel);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 {} 失败：{e}", path.display()));
        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with("#[tauri::command") {
                continue;
            }
            assert_eq!(
                trimmed,
                "#[tauri::command(rename_all = \"snake_case\")]",
                "{rel}:{} 命令注解必须显式 `rename_all = \"snake_case\"`——\
                 tauri-macros 默认 camelCase，前端契约全 snake_case，\n实际行：{trimmed}\n\
                 （任务 21 根因：query_series 图表恒空）",
                index + 1
            );
        }
    }
}

/// 从 query.rs 提取 `query_series` 的 Rust 参数名（跳过 State 注入参数）。
fn rust_query_series_args() -> Vec<String> {
    let source = std::fs::read_to_string(manifest_dir().join("src/commands/query.rs"))
        .expect("read query.rs");
    let start = source
        .find("pub async fn query_series(")
        .expect("query_series 命令存在");
    let tail = &source[start..];
    let end = tail.find(") -> Result<").expect("签名闭合");
    let signature = &tail[..end];
    signature
        .lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',');
            let (name, _) = line.split_once(':')?;
            let name = name.trim();
            if name.is_empty() || name == "state" {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

/// 从前端 types.ts 提取 `QuerySeriesArgs` 的键集合。
fn ts_query_series_arg_keys() -> Vec<String> {
    let source = std::fs::read_to_string(manifest_dir().join("../../ui/src/ipc/types.ts"))
        .expect("read ui/src/ipc/types.ts");
    let start = source
        .find("export interface QuerySeriesArgs")
        .expect("QuerySeriesArgs 接口存在");
    let tail = &source[start..];
    let end = tail.find('}').expect("接口闭合");
    tail[..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with("/**") || line.starts_with('*') || line.contains("interface") {
                return None;
            }
            let (key, _) = line.split_once(':')?;
            let key = key.trim().trim_end_matches('?');
            if key.is_empty() {
                return None;
            }
            Some(key.to_string())
        })
        .collect()
}

/// 防线 2：Rust 命令参数名 ↔ 前端 QuerySeriesArgs 键逐字一致
/// （含 `rename_all = "snake_case"` 生效后的 wire 形态）。
#[test]
fn query_series_args_match_frontend_contract_exactly() {
    let rust_args = rust_query_series_args();
    let ts_keys = ts_query_series_arg_keys();
    assert_eq!(
        rust_args, ts_keys,
        "query_series 参数名与前端 QuerySeriesArgs 键不一致——\
         任一侧改动都会导致 invoke 参数失配（任务 21 缺陷复现）。\n\
         Rust: {rust_args:?}\nTS: {ts_keys:?}"
    );
    // 关键必填参数在场（防止提取逻辑退化出空集仍"相等"）。
    for key in ["file_ids", "metrics", "t0_ms", "t1_ms", "max_points_per_series"] {
        assert!(
            rust_args.iter().any(|a| a == key),
            "query_series 缺必填参数 {key}"
        );
    }
}
