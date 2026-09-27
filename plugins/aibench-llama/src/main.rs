//! aibench-llama —— AnalysisBuddy AIBench 基准日志解析插件（Rust）。
//!
//! 定位（sdk-plugins.md §3）：按厂商具名读取三通路的 E 路（独立插件进程 + stdio
//! JSON-RPC）接入。G2 起传输样板（CLI 参数 / stdin NDJSON 读循环 / 请求分发 /
//! 响应通知写出 / 退出码）由 `core/ab-plugin-rt` 承担，本文件只剩业务 handler
//! 映射（engine 的类型化调用）。
//!
//! 入口：`aibench-llama --stdio`（manifest entry）。线程模型（由 ab-plugin-rt 承担，
//! 与 builtin-csv 一致）：读线程处理请求，`parse` 移入专用线程运行（cancel_parse
//! 走共享原子标志即时应答），全部 stdout 写经发送锁整行原子写出。

mod aibench;
mod csvline;
mod engine;

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use ab_plugin_rt as rt;
use ab_protocol::types::{
    CanHandleParams, CanHandleResult, FileSummary, KeyValuesResult, LoadFileParams, MetricDef,
};

const PLUGIN_ID: &str = "aibench-llama";
const PLUGIN_NAME: &str = "AIBench LLM Benchmark Parser";

/// 业务状态（无配置；can_handle/load_file 均为纯引擎调用）。
struct AibenchPlugin;

impl rt::Plugin for AibenchPlugin {
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
        engine::can_handle(params)
    }

    fn load_file(&self, params: &LoadFileParams) -> Result<Self::Loaded, String> {
        let lf = engine::load_file(&params.file_id, &params.path)?;
        if !lf.bad_samples.is_empty() {
            let first: Vec<String> = lf
                .bad_samples
                .iter()
                .map(|(n, r)| format!("line {n}: {r}"))
                .collect();
            eprintln!(
                "WARN aibench-llama: {} bad lines (first {} shown): {}",
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

fn main() -> ExitCode {
    rt::plugin_main(
        &rt::PluginMeta {
            log_tag: "aibench-llama",
            help_text: "aibench-llama -- AnalysisBuddy AIBench benchmark log plugin\n\nUSAGE:\n    aibench-llama --stdio",
            usage_line: "USAGE: aibench-llama --stdio",
        },
        || AibenchPlugin,
    )
}
