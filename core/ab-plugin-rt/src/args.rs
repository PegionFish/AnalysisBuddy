//! CLI 参数（`--stdio` 约定）与入口元数据。
//!
//! 语义与抽取前两插件逐字一致：无参数默认 stdio（宽容）；`--help`/`-h` 打印
//! 帮助到 stdout 后退出 0；其余任何参数拒绝启动（调用方以退出码 2 结束）。

use std::env;

/// 参数解析结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioArgs {
    /// 进入 stdio 主循环。
    Run,
    /// 已打印帮助（stdout），进程应以 0 退出。
    Help,
}

/// 解析 `--stdio` 约定参数；`Ok(Help)` 时帮助文本已打印到 stdout。
/// `Err(原因)` 由调用方打印 `ERROR {tag}: …` + USAGE 行后以退出码 2 结束。
pub fn parse_stdio_args(help_text: &str) -> Result<StdioArgs, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return Ok(StdioArgs::Run); // 无参数默认 stdio 模式（宽容）
    }
    if args.len() == 1 && args[0] == "--stdio" {
        return Ok(StdioArgs::Run);
    }
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{help_text}");
        return Ok(StdioArgs::Help);
    }
    Err(format!("unknown arguments: {}", args.join(" ")))
}

/// 插件入口元数据（日志前缀 / 帮助文本 / USAGE 行）。
#[derive(Debug, Clone, Copy)]
pub struct PluginMeta {
    /// stderr 日志前缀（如 `"builtin-csv"`；亦用于 stdout 写失败日志）。
    pub log_tag: &'static str,
    /// `--help` 全文（逐字打印到 stdout）。
    pub help_text: &'static str,
    /// 参数错误时打印到 stderr 的 USAGE 行。
    pub usage_line: &'static str,
}
