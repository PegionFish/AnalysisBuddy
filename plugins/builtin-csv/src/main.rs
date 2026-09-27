//! builtin-csv —— AnalysisBuddy 内置 CSV 解析插件（Rust）。
//!
//! 定位（sdk-plugins.md §3）：随宿主静态分发、终端用户零运行时依赖。G2 起传输
//! 样板（CLI 参数 / stdin NDJSON 读循环 / 请求分发 / 响应通知写出 / 退出码）由
//! `core/ab-plugin-rt` 承担，本文件只剩业务 handler 映射（engine 的类型化调用）。
//!
//! 入口：`builtin-csv --stdio`（manifest entry）。线程模型（由 ab-plugin-rt 承担）：
//! 读线程处理请求，`parse` 移入专用线程运行（cancel_parse 走共享原子标志即时应答），
//! 全部 stdout 写经发送锁整行原子写出。

mod config;
mod csvline;
mod engine;
mod timefmt;

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use ab_plugin_rt as rt;
use ab_protocol::types::{
    CanHandleParams, CanHandleResult, FileSummary, KeyValuesResult, LoadFileParams, MetricDef,
};

use config::Config;

const PLUGIN_ID: &str = "builtin-csv";
const PLUGIN_NAME: &str = "CSV Universal Parser";

/// 业务状态：can_handle 打分用的配置与头部指纹（§3.3）。
struct CsvPlugin {
    cfg: Config,
    fingerprints: Vec<String>,
}

impl rt::Plugin for CsvPlugin {
    type Loaded = engine::LoadedFile;

    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn can_handle(&self, params: &CanHandleParams) -> CanHandleResult {
        engine::can_handle(params, &self.cfg, &self.fingerprints)
    }

    fn load_file(&self, params: &LoadFileParams) -> Result<Self::Loaded, String> {
        let lf = engine::load_file(&params.file_id, &params.path, &self.cfg)?;
        if !lf.bad_samples.is_empty() {
            let first: Vec<String> = lf
                .bad_samples
                .iter()
                .map(|(n, r)| format!("line {n}: {r}"))
                .collect();
            eprintln!(
                "WARN builtin-csv: {} bad lines (first {} shown): {}",
                lf.bad_lines,
                first.len(),
                first.join("; ")
            );
        }
        Ok(lf)
    }

    fn summarize(loaded: &Self::Loaded) -> FileSummary {
        engine::to_summary(loaded)
    }

    fn metrics(loaded: &Self::Loaded) -> Vec<MetricDef> {
        loaded.metrics.clone()
    }

    fn parse(
        loaded: &mut Self::Loaded,
        sink: &mut dyn rt::Sink,
        cancel: &AtomicBool,
    ) -> Result<u64, rt::Cancelled> {
        match engine::parse_file(loaded, sink, cancel) {
            Ok(total) => Ok(total),
            Err(engine::ParseError::Cancelled) => Err(rt::Cancelled),
        }
    }

    fn key_values(loaded: &Self::Loaded, timestamp_ms: i64) -> KeyValuesResult {
        engine::key_values(loaded, timestamp_ms)
    }
}

/// 启动时读取 plugin.json 的 `match.header_fingerprints`（can_handle 打分用）。
fn load_fingerprints() -> Vec<String> {
    for dir in [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(std::path::PathBuf::from)),
    ]
    .into_iter()
    .flatten()
    {
        let Ok(text) = std::fs::read_to_string(dir.join("plugin.json")) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(fps) = manifest
            .get("match")
            .and_then(|m| m.get("header_fingerprints"))
            .and_then(serde_json::Value::as_array)
        {
            return fps
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect();
        }
    }
    Vec::new()
}

fn main() -> ExitCode {
    rt::plugin_main(
        &rt::PluginMeta {
            log_tag: "builtin-csv",
            help_text:
                "builtin-csv -- AnalysisBuddy built-in CSV plugin\n\nUSAGE:\n    builtin-csv --stdio",
            usage_line: "USAGE: builtin-csv --stdio",
        },
        || {
            let (cfg, warnings) = config::load_config();
            for w in &warnings {
                eprintln!("WARN builtin-csv: {w}");
            }
            CsvPlugin {
                cfg,
                fingerprints: load_fingerprints(),
            }
        },
    )
}
