//! EOF 退出自洽检查：stdin 关闭 → 进程退出码 0（protocol.md §9 第 5 条，
//! A 路孤儿进程防护用例依赖此行为）。

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mock-plugin"))
}

#[test]
fn stdin_eof_exits_zero() {
    // 1) stdin 直接关闭（无请求）：加载剧本后 EOF → 退出码 0，stdout 无任何输出。
    let out = bin()
        .args(["--script", "scripts/happy_path.ndjson"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn mock-plugin (file script)");
    assert!(
        out.status.success(),
        "EOF with no requests must exit 0, got {:?}",
        out.status.code()
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must stay pure (empty): {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    // 2) `--script -`：剧本来自 stdin，读完解析即退出码 0。
    let mut child = bin()
        .args(["--script", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mock-plugin (stdin script)");
    let script = std::fs::read_to_string("scripts/happy_path.ndjson").expect("read script");
    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(script.as_bytes())
        .expect("write script to stdin");
    drop(child.stdin.take());

    let out = child.wait_with_output().expect("wait for mock-plugin");
    assert!(
        out.status.success(),
        "stdin-script mode must exit 0, got {:?}",
        out.status.code()
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must stay pure (empty): {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("script loaded") && stderr.contains("exiting"),
        "stderr should report script load and exit: {stderr}"
    );
}
