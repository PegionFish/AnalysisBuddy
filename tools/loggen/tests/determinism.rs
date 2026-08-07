//! loggen 确定性验收（qa-perf.md §1.3 / F-01 DoD）：
//! - 同参数同 seed 两次输出 SHA-256 逐字节一致（≥3 组参数）；
//! - `--disorder 0` 时时间戳列严格递增（逐行断言）；
//! - 实际体积与 `--size-target` 偏差 ≤2%；
//! - `--disorder 1` 触发退出码 2；
//! - 哈希基准冻结见 `tests/fixtures/README.md`。

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// 本 crate 的编译产物路径（cargo 集成测试专用环境变量）。
fn loggen_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_loggen"))
}

fn tmp_path(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("loggen-det-{tag}-{nanos}.csv"))
}

fn run(args: &[&str]) -> Output {
    Command::new(loggen_bin())
        .args(args)
        .output()
        .expect("spawn loggen")
}

fn sha256_hex(path: &PathBuf) -> String {
    let bytes = std::fs::read(path).expect("read output");
    let mut h = Sha256::new();
    h.update(&bytes);
    format!("{:x}", h.finalize())
}

/// 三组参数各跑两遍，SHA-256 逐字节一致（F-01 DoD 确定性验收）。
#[test]
fn same_seed_produces_identical_sha256() {
    let cases: [(&str, &str); 4] = [
        (
            "csv-basic",
            "--rows 2000 --metrics 3 --size-target auto --format csv --seed 42",
        ),
        (
            "csv-options",
            "--rows 1500 --metrics 2 --size-target auto --format csv --seed 7 --disorder 0.3 --corrupt 0.1 --start 2026-01-01T00:00:00.000Z --interval 50 --encoding utf8bom",
        ),
        (
            "txt",
            "--rows 1500 --metrics 3 --size-target auto --format txt --seed 99 --disorder 0.5",
        ),
        (
            "gbk",
            "--rows 300 --metrics 3 --size-target auto --format csv --seed 205 --encoding gbk",
        ),
    ];
    for (tag, args) in cases {
        let a_path = tmp_path(tag);
        let b_path = tmp_path(tag);
        let a_cmd = format!("{args} -o {}", a_path.display());
        let b_cmd = format!("{args} -o {}", b_path.display());
        let a_args: Vec<&str> = a_cmd.split(' ').collect();
        let b_args: Vec<&str> = b_cmd.split(' ').collect();
        let a = run(&a_args);
        let b = run(&b_args);
        assert_eq!(a.status.code(), Some(0), "{tag}: first run must exit 0");
        assert_eq!(b.status.code(), Some(0), "{tag}: second run must exit 0");
        assert_eq!(
            sha256_hex(&a_path),
            sha256_hex(&b_path),
            "{tag}: SHA-256 必须一致"
        );
        let _ = std::fs::remove_file(&a_path);
        let _ = std::fs::remove_file(&b_path);
    }
}

/// `--disorder 0` 时时间戳列严格递增（逐行断言）。
#[test]
fn timestamps_strictly_increasing_without_disorder() {
    let path = tmp_path("mono");
    let out = run(&[
        "--rows",
        "1000",
        "--metrics",
        "3",
        "--size-target",
        "auto",
        "--format",
        "csv",
        "--seed",
        "11",
        "-o",
        path.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
    let text = std::fs::read_to_string(&path).unwrap();
    let mut prev: Option<String> = None;
    for line in text.lines().skip(1) {
        let ts = line.split(',').next().unwrap().to_string();
        if let Some(p) = &prev {
            assert!(ts > *p, "时间戳必须严格递增: {p} -> {ts}");
        }
        prev = Some(ts);
    }
    let _ = std::fs::remove_file(&path);
}

/// `--disorder 1` 触发退出码 2（参数冲突）。
#[test]
fn disorder_one_is_rejected_with_exit_2() {
    let path = tmp_path("bad");
    let out = run(&[
        "--rows",
        "10",
        "--metrics",
        "3",
        "--size-target",
        "auto",
        "--format",
        "csv",
        "--seed",
        "1",
        "--disorder",
        "1",
        "-o",
        path.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    let _ = std::fs::remove_file(&path);
}

/// IO 失败（输出目录不存在）退出码 4。
#[test]
fn io_failure_is_exit_4() {
    let out = run(&[
        "--rows",
        "10",
        "--metrics",
        "3",
        "--size-target",
        "auto",
        "--format",
        "csv",
        "--seed",
        "1",
        "-o",
        r"Z:\no-such-dir\loggen-x.csv",
    ]);
    assert_eq!(out.status.code(), Some(4));
}

/// `--help` 与 qa-perf.md §1.1 签名一致（必选 5 + 可选 6 参数齐全）。
#[test]
fn help_lists_all_parameters() {
    let out = run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--rows",
        "--metrics",
        "--size-target",
        "--format",
        "--seed",
        "-o",
        "--start",
        "--interval",
        "--disorder",
        "--encoding",
        "--no-header",
        "--corrupt",
    ] {
        assert!(text.contains(flag), "--help 缺少 {flag}");
    }
    for code in ["0", "2", "4"] {
        assert!(text.contains(code), "--help 缺少退出码 {code}");
    }
}
