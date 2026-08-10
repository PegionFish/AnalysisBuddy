//! 集成测试公共工具：运行 `plugin-check` 二进制并解析 `--json` 输出。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// 被测二进制（cargo 注入的 CARGO_BIN_EXE_plugin-check）。
pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_plugin-check")
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn fixture(name: &str) -> PathBuf {
    fixtures().join(name)
}

/// 运行 `plugin check --json <args...>`；返回 (退出码, 解析后的 JSON)。
pub fn run_json(args: &[&str]) -> (i32, Value) {
    let full: Vec<String> = std::iter::once("--json".to_string())
        .chain(args.iter().map(|s| s.to_string()))
        .collect();
    let full: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
    run_raw(&full)
}

pub fn run_raw(args: &[&str]) -> (i32, Value) {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("plugin-check 应能启动");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let json: Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => panic!(
            "stdout 不是合法 JSON（退出码 {code}）：{e}\nstdout={stdout}\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        ),
    };
    (code, json)
}

pub fn rules_len(json: &Value) -> usize {
    json["rules"].as_array().map(|a| a.len()).unwrap_or(0)
}

pub fn has_rule(json: &Value, id: &str) -> bool {
    json["rules"]
        .as_array()
        .map(|a| a.iter().any(|r| r["id"].as_str() == Some(id)))
        .unwrap_or(false)
}

/// 供各测试文件选用的辅助函数；未被某测试文件引用时会产生 dead_code 警告。
#[allow(dead_code)]
pub fn summary(json: &Value) -> &Value {
    &json["summary"]
}
