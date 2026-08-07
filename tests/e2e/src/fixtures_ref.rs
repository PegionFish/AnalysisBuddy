//! F 路夹具冻结文件名常量表（qa-perf.md §2 表，文件名冻结，e2e 代码按名引用）。

use std::path::PathBuf;

/// 入仓小夹具（`tests/fixtures/`）。
pub const SMALL_WITH_HEADER: &str = "small_with_header.csv";
pub const SMALL_NO_HEADER: &str = "small_no_header.csv";
pub const SMALL_TXT: &str = "small_txt.log";
pub const EMPTY: &str = "empty.csv";
pub const MALFORMED_LINES: &str = "malformed_lines.csv";
pub const ENC_UTF8_BOM: &str = "enc_utf8_bom.csv";
pub const ENC_GBK: &str = "enc_gbk.csv";
pub const SINGLE_LONG_LINE: &str = "single_long_line.csv";

/// 生成式大夹具（`tests/.generated/`，gitignore，由 gen-large-fixtures.ps1 现生成）。
pub const BENCH_10MB: &str = "bench_10mb.csv";
pub const BENCH_50MB: &str = "bench_50mb.csv";
pub const BENCH_100MB: &str = "bench_100mb.csv";
pub const DISORDER_20PCT: &str = "disorder_20pct.csv";

/// 全部 12 项夹具名（qa-perf.md §2 矩阵照录）。
pub const ALL_FIXTURES: [&str; 12] = [
    SMALL_WITH_HEADER,
    SMALL_NO_HEADER,
    SMALL_TXT,
    EMPTY,
    MALFORMED_LINES,
    ENC_UTF8_BOM,
    ENC_GBK,
    SINGLE_LONG_LINE,
    BENCH_10MB,
    BENCH_50MB,
    BENCH_100MB,
    DISORDER_20PCT,
];

/// 入仓夹具目录。
pub fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("fixtures")
}

/// 生成式夹具目录（tests/.generated，gitignore）。
pub fn generated_dir() -> PathBuf {
    workspace_root().join("tests").join(".generated")
}

/// 入仓夹具的绝对路径。
pub fn fixture_path(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

/// 生成式夹具的绝对路径。
pub fn generated_path(name: &str) -> PathBuf {
    generated_dir().join(name)
}

/// 仓库根（`tests/e2e` → 上溯两级）。
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("tests/e2e must be a two-level-deep workspace member")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_names_are_distinct_and_frozen() {
        let mut names: Vec<&str> = ALL_FIXTURES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL_FIXTURES.len(), "夹具名必须唯一");
    }

    #[test]
    fn committed_fixtures_resolve_within_repo() {
        for name in ALL_FIXTURES {
            assert!(
                fixture_path(name).starts_with(workspace_root()),
                "{name} 必须解析在仓库内"
            );
        }
    }
}
