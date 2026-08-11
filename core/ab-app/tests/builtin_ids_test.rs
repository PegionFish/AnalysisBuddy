//! 内建模块 id 清单回归（任务 4）：build.rs 扫描仓库 `plugins/` 目录
//! （含 `plugin.json` 的直接子目录）生成 `gen/builtin_ids.rs`
//! （`pub const BUILTIN_PLUGIN_IDS: &[&str]`），本测试 include 该产物并断言：
//!
//! 1. 非空——仓库内必须始终存在已注册内建模块；
//! 2. 必须包含 `builtin-csv`（首个内建，被移除/漏扫即失败）；
//! 3. 无重复；
//! 4. 每个 id 符合插件 id 正则 `^[a-z0-9][a-z0-9-_]{1,63}$`。

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/gen/builtin_ids.rs"));

/// 插件 id 合法性（等价 `^[a-z0-9][a-z0-9-_]{1,63}$`）：
/// 小写字母/数字开头，后续可为小写字母/数字/`-`/`_`，总长 2..=64。
fn is_valid_plugin_id(id: &str) -> bool {
    let mut chars = id.chars();
    if !matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return false;
    }
    let rest_len = chars
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_')
        .count();
    (1..=63).contains(&rest_len) && rest_len == id.chars().count() - 1
}

/// 内建清单必须非空——`plugins/` 下至少存在一个含 `plugin.json` 的目录。
#[test]
fn builtin_ids_non_empty() {
    assert!(
        !BUILTIN_PLUGIN_IDS.is_empty(),
        "BUILTIN_PLUGIN_IDS 为空——plugins/ 下没有任何含 plugin.json 的目录？"
    );
}

/// 首块内建必须始终在册。
#[test]
fn builtin_ids_contains_builtin_csv() {
    assert!(
        BUILTIN_PLUGIN_IDS.contains(&"builtin-csv"),
        "BUILTIN_PLUGIN_IDS 缺少 builtin-csv——插件被移出 plugins/ 或扫描漏掉？"
    );
}

/// 同一目录不可能重复，但防生成逻辑失误，仍断言无重复。
#[test]
fn builtin_ids_have_no_duplicates() {
    let mut seen = std::collections::HashSet::new();
    for id in BUILTIN_PLUGIN_IDS {
        assert!(seen.insert(*id), "内建 id 重复: {id}");
    }
}

/// 每个 id 必须满足插件 id 正则。
#[test]
fn builtin_ids_match_id_pattern() {
    for id in BUILTIN_PLUGIN_IDS {
        assert!(
            is_valid_plugin_id(id),
            "非法内建 id: {id}（须匹配 ^[a-z0-9][a-z0-9-_]{{1,63}}$）"
        );
    }
}
