//! mock-plugin —— AnalysisBuddy 协议回放插件（NDJSON 剧本驱动）。
//!
//! 宿主按 protocol-v1.md §1.1 以 stdin/stdout 与本工具通信；本工具按剧本
//! （`--script <file>`，未给时回落环境变量 `AB_MOCK_SCRIPT`）对每个请求回放应答。
//! 剧本行四种 `kind`：`reply`（回 result 或 error）、`emit`（parse 期间推送
//! progress / RecordBatch 通知）、`sleep`（推送间睡眠，heartbeat_stop 剧本用）。
//!
//! 例外：可选方法 `custom_query`（§2.11，CCP-custom-query addendum）不走剧本——
//! 其 echo 回显语义依赖逐请求 params，静态剧本表达不了；由内置分支按 `--caps`
//! 旗标三态应答（未声明 `-32005` / `echo` 回显 / 未知 query `-32602`，
//! 见 `custom_query_reply`）。
//!
//! 合规纪律（A 路容错用例依赖，protocol.md §1.1 / §9 第 5 条）：
//! - stdout 只输出协议帧：单行 JSON-RPC 2.0，行尾仅 `\n`（无 `\r`），每帧后 flush；
//! - 全部日志走 stderr（`INFO`/`WARN`/`ERROR` 前缀）；
//! - stdin EOF 即退出码 0；收到 `shutdown` 请求应答后立即退出。
//!
//! 剧本行格式（回放器私有约定，非协议契约）与联调入口约定见 `README.md`。

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufWriter, Read, Write};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use ab_protocol::errors::{
    ERR_INTERNAL_ERROR, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_UNSUPPORTED_IN_V1,
};
use ab_protocol::types::{
    AnnotateResult, CanHandleResult, CustomQueryParams, CustomQueryResult, FileSummary,
    InitializeResult, KeyValuesResult, ParseResult, ProgressParams, RecordBatch, SchemaResult,
};

/// 剧本 = 方法 → 指令序列（块内按剧本行顺序执行；每个块以 reply 行收尾）。
type Script = BTreeMap<String, Vec<Instruction>>;

/// 剧本行原文（未校验）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RawLine {
    Reply {
        method: String,
        result: Option<Value>,
        error: Option<ErrorPayload>,
    },
    Emit {
        method: String,
        params: Value,
    },
    Sleep {
        ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ErrorPayload {
    code: i32,
    message: String,
}

/// 校验后的剧本指令。
#[derive(Debug, Clone, PartialEq)]
enum Instruction {
    /// 对所属方法块对应的请求回 result / error。
    Reply { reply: ReplyPayload },
    /// 推送通知（method 限 `RecordBatch` | `progress`）。
    Emit { method: String, params: Value },
    /// 推送间睡眠（毫秒）。
    Sleep { ms: u64 },
}

#[derive(Debug, Clone, PartialEq)]
enum ReplyPayload {
    Result(Value),
    Error { code: i32, message: String },
}

/// 解析剧本行列表为指令表（纯函数，可单测）。
///
/// 块划分：`reply` 行终结当前块，其前的 `emit`/`sleep` 行并入该块（按剧本行顺序
/// 执行）。校验：reply 行必须恰有 result/error 之一；result 必须能反序列化为对应
/// method 的契约类型（并据此重新序列化，skip-if-empty 等序列化约定逐帧成立）；
/// emit 的 method 限 RecordBatch / progress；每个方法最多一个块；剧本末尾不允许
/// 悬挂未终结块。任何违规以带行号的中文错误拒绝整个剧本。
fn parse_script(lines: &[String]) -> Result<Script, String> {
    let mut script: Script = BTreeMap::new();
    let mut pending: Vec<Instruction> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let lineno = idx + 1;
        let raw: RawLine = serde_json::from_str(line)
            .map_err(|e| format!("line {lineno}: invalid script line: {e}"))?;
        match raw {
            RawLine::Reply {
                method,
                result,
                error,
            } => {
                let reply = match (result, error) {
                    (Some(result), None) => ReplyPayload::Result(
                        validate_result(&method, &result)
                            .map_err(|e| format!("line {lineno}: {e}"))?,
                    ),
                    (None, Some(error)) => ReplyPayload::Error {
                        code: error.code,
                        message: error.message,
                    },
                    (Some(_), Some(_)) => {
                        return Err(format!(
                            "line {lineno}: reply line must have exactly one of `result`/`error`"
                        ));
                    }
                    (None, None) => {
                        return Err(format!(
                            "line {lineno}: reply line requires `result` or `error`"
                        ));
                    }
                };
                pending.push(Instruction::Reply { reply });
                let duplicate = script
                    .insert(method.clone(), std::mem::take(&mut pending))
                    .is_some();
                if duplicate {
                    return Err(format!(
                        "line {lineno}: duplicate block for method `{method}`"
                    ));
                }
            }
            RawLine::Emit { method, params } => {
                let params =
                    validate_emit(&method, &params).map_err(|e| format!("line {lineno}: {e}"))?;
                pending.push(Instruction::Emit { method, params });
            }
            RawLine::Sleep { ms } => pending.push(Instruction::Sleep { ms }),
        }
    }

    if !pending.is_empty() {
        return Err(
            "script ends with an unterminated block: every block must end with a `reply` line"
                .to_string(),
        );
    }
    Ok(script)
}

/// 将 reply 的 result 反序列化为对应 method 的契约类型并重新序列化
/// （字段名、可选字段 skip-if-empty、字段顺序均以 ab-protocol 类型为准）。
fn validate_result(method: &str, value: &Value) -> Result<Value, String> {
    let frame: Result<Value, serde_json::Error> =
        match method {
            "initialize" => serde_json::from_value::<InitializeResult>(value.clone())
                .and_then(serde_json::to_value),
            "can_handle" => serde_json::from_value::<CanHandleResult>(value.clone())
                .and_then(serde_json::to_value),
            "load_file" => {
                serde_json::from_value::<FileSummary>(value.clone()).and_then(serde_json::to_value)
            }
            "parse" => {
                serde_json::from_value::<ParseResult>(value.clone()).and_then(serde_json::to_value)
            }
            "schema" => {
                serde_json::from_value::<SchemaResult>(value.clone()).and_then(serde_json::to_value)
            }
            "key_values" => serde_json::from_value::<KeyValuesResult>(value.clone())
                .and_then(serde_json::to_value),
            "annotate" => serde_json::from_value::<AnnotateResult>(value.clone())
                .and_then(serde_json::to_value),
            "unload_file" | "cancel_parse" | "shutdown" => {
                match serde_json::from_value::<Map<String, Value>>(value.clone()) {
                    Ok(map) if map.is_empty() => return Ok(Value::Object(map)),
                    Ok(_) => {
                        return Err(format!(
                            "method `{method}`: result must be the empty object `{{}}`"
                        ));
                    }
                    Err(e) => {
                        return Err(format!(
                            "method `{method}`: result must be the empty object `{{}}`: {e}"
                        ));
                    }
                }
            }
            other => return Err(format!("unknown method `{other}`")),
        };
    frame.map_err(|e| format!("method `{method}`: result does not match contract type: {e}"))
}

/// 将 emit 的 params 反序列化为契约通知类型并重新序列化。
fn validate_emit(method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "RecordBatch" => serde_json::from_value::<RecordBatch>(params.clone())
            .and_then(serde_json::to_value)
            .map_err(|e| format!("emit `RecordBatch`: params do not match contract type: {e}")),
        "progress" => serde_json::from_value::<ProgressParams>(params.clone())
            .and_then(serde_json::to_value)
            .map_err(|e| format!("emit `progress`: params do not match contract type: {e}")),
        other => Err(format!(
            "emit: unknown notification method `{other}` (allowed: `RecordBatch` | `progress`)"
        )),
    }
}

/// `--caps custom_query` 的剧本后处理：把 initialize 块 reply result 的
/// capabilities 补上 `"custom_query":true`（initialize 结果由剧本持有，
/// 旗标只翻转能力位，id/name/version 仍以剧本为准）。剧本未剧本化
/// initialize 时返回 false（调用方记 WARN，内置分支仍按旗标应答）。
fn enable_custom_query(script: &mut Script) -> bool {
    let Some(block) = script.get_mut("initialize") else {
        return false;
    };
    for instr in block {
        if let Instruction::Reply {
            reply: ReplyPayload::Result(result),
        } = instr
        {
            // 契约校验已保证 capabilities 是对象；此处置入即落在对象上。
            result["capabilities"]["custom_query"] = Value::Bool(true);
        }
    }
    true
}

/// stdin 上的一条宿主请求。
struct Request {
    id: Option<Value>,
    method: String,
    /// 请求 params（缺省空对象；`custom_query` 内置分支回显用）。
    params: Value,
}

/// 解析 stdin 请求行；`None` = 合法 JSON 但无 `method`（宿主侧不应出现，记日志忽略）。
fn parse_request(line: &str) -> Result<Option<Request>, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| format!("invalid JSON on stdin: {e}"))?;
    let method = match value.get("method").and_then(Value::as_str) {
        Some(m) => m.to_string(),
        None => return Ok(None),
    };
    Ok(Some(Request {
        id: value.get("id").cloned(),
        method,
        params: value
            .get("params")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
    }))
}

/// 处理一条请求的结局。
enum Outcome {
    /// 继续读 stdin。
    Continue,
    /// 已应答 `shutdown`，应退出进程。
    Shutdown,
}

/// 按剧本回放一条请求；响应/通知逐帧写 stdout（LF 行尾、每帧 flush）。
/// `custom_query` 在剧本查找前由内置分支拦截（见 `custom_query_reply`）。
/// emit 通知 file_id 改写：剧本占位常量 → 宿主 load_file 实际分配值。
/// 通知无 file_id 键或尚无已加载文件时原样返回。
fn rewrite_emit_file_id(params: &Value, loaded: &Option<String>) -> Value {
    let Some(fid) = loaded else {
        return params.clone();
    };
    let Some(map) = params.as_object() else {
        return params.clone();
    };
    if !map.contains_key("file_id") {
        return params.clone();
    }
    let mut map = map.clone();
    map.insert("file_id".to_string(), Value::String(fid.clone()));
    Value::Object(map)
}

fn handle(
    req: &Request,
    script: &Script,
    caps: CapsFlags,
    loaded: &mut Option<String>,
    out: &mut impl Write,
) -> Result<Outcome, String> {
    let Some(id) = &req.id else {
        eprintln!(
            "WARN mock-plugin: request method={} without id ignored (host must not send notifications)",
            req.method
        );
        return Ok(Outcome::Continue);
    };
    if req.method == "custom_query" {
        return match custom_query_reply(caps, &req.params) {
            Ok(result) => {
                write_frame(
                    out,
                    &ResponseResult {
                        jsonrpc: "2.0",
                        id,
                        result: &result,
                    },
                )?;
                Ok(Outcome::Continue)
            }
            Err(error) => {
                write_frame(
                    out,
                    &ResponseError {
                        jsonrpc: "2.0",
                        id,
                        error,
                    },
                )?;
                Ok(Outcome::Continue)
            }
        };
    }
    let Some(lines) = script.get(&req.method) else {
        write_frame(
            out,
            &ResponseError {
                jsonrpc: "2.0",
                id,
                error: ErrorPayload {
                    code: ERR_METHOD_NOT_FOUND,
                    message: "Method not found".to_string(),
                },
            },
        )?;
        return Ok(Outcome::Continue);
    };
    if req.method == "load_file" {
        // 协议合规：通知必须回显宿主分配的 file_id（protocol-v1.md §3.2/§3.3
        // ——通知携带所属请求的 file_id）。剧本 emit 行里的 file_id 是占位
        // 常量；生产宿主分配随机 UUID，原样回放会被管线判为未知文件丢弃
        // （freeze records_total mismatch）。此处记录实际值，emit 时改写。
        if let Some(fid) = req.params.get("file_id").and_then(Value::as_str) {
            *loaded = Some(fid.to_string());
        }
    }
    for instr in lines {
        match instr {
            Instruction::Sleep { ms } => thread::sleep(Duration::from_millis(*ms)),
            Instruction::Emit { method, params } => {
                let params = rewrite_emit_file_id(params, loaded);
                write_frame(
                    out,
                    &NotificationFrame {
                        jsonrpc: "2.0",
                        method,
                        params: &params,
                    },
                )?
            }
            Instruction::Reply { reply } => match reply {
                ReplyPayload::Result(result) => write_frame(
                    out,
                    &ResponseResult {
                        jsonrpc: "2.0",
                        id,
                        result,
                    },
                )?,
                ReplyPayload::Error { code, message } => write_frame(
                    out,
                    &ResponseError {
                        jsonrpc: "2.0",
                        id,
                        error: ErrorPayload {
                            code: *code,
                            message: message.clone(),
                        },
                    },
                )?,
            },
        }
    }
    Ok(if req.method == "shutdown" {
        Outcome::Shutdown
    } else {
        Outcome::Continue
    })
}

/// §2.11 `custom_query` 内置应答（CCP-custom-query addendum）。
///
/// 行为本任务钉死（确定性优先，host 超时上限内正常应答）：
/// 1) 未声明能力（无 `--caps custom_query`）→ `-32005 unsupported_in_v1`，
///    message 固定 `"custom_query not supported"`；
/// 2) 能力已声明且 `query == "echo"` → `{"data":{"echo":{...}}}`，`file_id`/
///    `params` 逐字段原样回显（params 缺省回显为空对象）；
/// 3) 能力已声明且 query 为其他名（或 params 不符合契约形状）→ `-32602`。
fn custom_query_reply(caps: CapsFlags, params: &Value) -> Result<Value, ErrorPayload> {
    if !caps.custom_query {
        return Err(ErrorPayload {
            code: ERR_UNSUPPORTED_IN_V1,
            message: "custom_query not supported".to_string(),
        });
    }
    let parsed: CustomQueryParams =
        serde_json::from_value(params.clone()).map_err(|e| ErrorPayload {
            code: ERR_INVALID_PARAMS,
            message: format!("invalid params for custom_query: {e}"),
        })?;
    if parsed.query != "echo" {
        return Err(ErrorPayload {
            code: ERR_INVALID_PARAMS,
            message: format!("unknown query `{}`", parsed.query),
        });
    }
    let echo = json!({
        "file_id": parsed.file_id,
        "query": parsed.query,
        "params": Value::Object(parsed.params),
    });
    let mut data = Map::new();
    data.insert("echo".to_string(), echo);
    serde_json::to_value(CustomQueryResult { data }).map_err(|e| ErrorPayload {
        code: ERR_INTERNAL_ERROR,
        message: format!("serialize custom_query result: {e}"),
    })
}

/// 响应帧（result 型）；字段顺序与 protocol-v1.md §3.5 示例一致。
#[derive(Serialize)]
struct ResponseResult<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: &'a Value,
}

/// 响应帧（error 型）。
#[derive(Serialize)]
struct ResponseError<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    error: ErrorPayload,
}

/// 通知帧（无 id）。
#[derive(Serialize)]
struct NotificationFrame<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: &'a Value,
}

/// 整帧写 stdout：单行 JSON + `\n`，随后 flush（宿主按行增量读取）。
fn write_frame(out: &mut impl Write, frame: &impl Serialize) -> Result<(), String> {
    let mut line = serde_json::to_string(frame).map_err(|e| format!("frame serialize: {e}"))?;
    line.push('\n');
    out.write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| format!("stdout write failed: {e}"))
}

/// 剧本来源。
enum ScriptSource {
    File(String),
    Stdin,
}

/// `--caps` 旗标解析结果：可选能力开关（v1 只支持 `custom_query` 一个值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CapsFlags {
    custom_query: bool,
}

impl CapsFlags {
    /// 追加一组逗号分隔的能力名（旗标可重复传，同一值幂等）；未知名字拒绝启动。
    fn merge_str(&mut self, value: &str) -> Result<(), String> {
        for name in value.split(',') {
            match name.trim() {
                "custom_query" => self.custom_query = true,
                other => {
                    return Err(format!(
                        "unknown capability `{other}` (only `custom_query` is supported)"
                    ))
                }
            }
        }
        Ok(())
    }
}

enum ParsedArgs {
    Run {
        source: ScriptSource,
        caps: CapsFlags,
    },
    Help,
}

fn parse_args() -> Result<ParsedArgs, String> {
    let mut script: Option<ScriptSource> = None;
    let mut caps = CapsFlags::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(ParsedArgs::Help);
        }
        if arg == "--caps" || arg.starts_with("--caps=") {
            let value = match arg.strip_prefix("--caps=") {
                Some(v) => v.to_string(),
                None => args.next().ok_or("--caps requires a value")?,
            };
            caps.merge_str(&value)?;
            continue;
        }
        let value = if let Some(v) = arg.strip_prefix("--script=") {
            Some(v.to_string())
        } else if arg == "--script" {
            args.next()
        } else {
            return Err(format!("unknown argument `{arg}`"));
        };
        script = Some(match value.as_deref() {
            Some("-") => ScriptSource::Stdin,
            Some(path) => ScriptSource::File(path.to_string()),
            None => return Err("--script requires a value".to_string()),
        });
    }
    let source = match script {
        Some(s) => s,
        None => match env::var("AB_MOCK_SCRIPT").ok().filter(|v| !v.is_empty()) {
            Some(v) if v == "-" => ScriptSource::Stdin,
            Some(v) => ScriptSource::File(v),
            None => {
                return Err(
                    "no script given: use `--script <file>` or set `AB_MOCK_SCRIPT`".to_string(),
                );
            }
        },
    };
    Ok(ParsedArgs::Run { source, caps })
}

fn print_usage() {
    eprintln!(
        "mock-plugin — AnalysisBuddy protocol replay plugin (NDJSON scripts)\n\n\
         USAGE:\n    \
         mock-plugin --script <path|-> [--caps <name>]\n\n\
         OPTIONS:\n    \
         --script <path>   NDJSON replay script; `-` reads the script from stdin, then exits\n    \
         --caps <name>     declare an optional capability (repeatable/comma-separated;\n    \
                           only `custom_query` is supported); adds `custom_query:true`\n    \
                           to the initialize reply and enables the built-in handler\n    \
         --help            print this help\n\n\
         ENVIRONMENT:\n    \
         AB_MOCK_SCRIPT     fallback script path when --script is not given\n\n\
         STDOUT carries protocol frames only; all logs go to stderr."
    );
}

/// 剧本文本 → 非空行列表（容忍 `\r\n` 换行）。
fn lines_of(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .filter(|line| !line.trim().is_empty())
        .collect()
}

fn main() -> ExitCode {
    let (parsed, caps) = match parse_args() {
        Ok(ParsedArgs::Run { source, caps }) => (source, caps),
        Ok(ParsedArgs::Help) => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("ERROR mock-plugin: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    let (lines, from_stdin) = match &parsed {
        ScriptSource::File(path) => {
            let content = match fs::read_to_string(path) {
                Ok(content) => content,
                Err(e) => {
                    eprintln!("ERROR mock-plugin: cannot read script `{path}`: {e}");
                    return ExitCode::from(1);
                }
            };
            (lines_of(&content), false)
        }
        ScriptSource::Stdin => {
            let mut content = String::new();
            if let Err(e) = io::stdin().read_to_string(&mut content) {
                eprintln!("ERROR mock-plugin: cannot read script from stdin: {e}");
                return ExitCode::from(1);
            }
            (lines_of(&content), true)
        }
    };

    let mut script = match parse_script(&lines) {
        Ok(script) => script,
        Err(e) => {
            eprintln!("ERROR mock-plugin: script rejected: {e}");
            return ExitCode::from(1);
        }
    };
    if caps.custom_query && !enable_custom_query(&mut script) {
        eprintln!(
            "WARN mock-plugin: --caps custom_query given but script has no `initialize` block; capability not announced"
        );
    }
    eprintln!(
        "INFO mock-plugin: script loaded: {} ({} blocks, custom_query={})",
        match &parsed {
            ScriptSource::File(path) => path.as_str(),
            ScriptSource::Stdin => "-",
        },
        script.len(),
        caps.custom_query
    );

    if from_stdin {
        eprintln!("INFO mock-plugin: script read from stdin; no request channel remains, exiting");
        return ExitCode::SUCCESS;
    }

    run(script, caps)
}

/// 事件循环：逐行读 stdin 请求 → 按剧本回放 → stdin EOF / shutdown 后退出码 0。
fn run(script: Script, caps: CapsFlags) -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut loaded: Option<String> = None;
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("ERROR mock-plugin: stdin read failed: {e}");
                return ExitCode::from(1);
            }
        };
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            eprintln!("WARN mock-plugin: empty line on stdin ignored");
            continue;
        }
        let request = match parse_request(line) {
            Ok(Some(request)) => request,
            Ok(None) => {
                eprintln!("WARN mock-plugin: line without `method` ignored");
                continue;
            }
            Err(e) => {
                eprintln!("WARN mock-plugin: {e}");
                continue;
            }
        };
        eprintln!(
            "INFO mock-plugin: request method={} id={}",
            request.method,
            request
                .id
                .as_ref()
                .map(Value::to_string)
                .unwrap_or_else(|| "-".to_string())
        );
        match handle(&request, &script, caps, &mut loaded, &mut out) {
            Ok(Outcome::Continue) => {}
            Ok(Outcome::Shutdown) => break,
            Err(e) => {
                eprintln!("ERROR mock-plugin: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if let Err(e) = out.flush() {
        eprintln!("ERROR mock-plugin: final stdout flush failed: {e}");
        return ExitCode::from(1);
    }
    eprintln!("INFO mock-plugin: stdin EOF, exiting with code 0");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|row| row.to_string()).collect()
    }

    /// initialize result 剧本行模板：按 `custom_query` 旗标构造能力对象。
    /// 旗标关闭时与历史字面量逐字节一致（stdout_purity / e2e 断言依赖旧形状）。
    fn init_result_json(custom_query: bool) -> String {
        if custom_query {
            r#"{"id":"mock","name":"Mock","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false,"custom_query":true}}"#.to_string()
        } else {
            r#"{"id":"mock","name":"Mock","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}"#.to_string()
        }
    }

    #[test]
    fn init_result_default_is_byte_identical_to_history() {
        // 未设 --caps 时能力对象不得新增键（既有断言依赖旧形状）。
        assert_eq!(
            init_result_json(false),
            r#"{"id":"mock","name":"Mock","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}"#
        );
        // 设旗标时在能力对象末尾补 custom_query:true（经契约类型校验合法）。
        let script = parse_script(&lines(&[&format!(
            r#"{{"kind":"reply","method":"initialize","result":{}}}"#,
            init_result_json(true)
        )]))
        .expect("flagged init result should parse as contract type");
        match &script["initialize"][0] {
            Instruction::Reply {
                reply: ReplyPayload::Result(v),
            } => assert_eq!(v["capabilities"]["custom_query"], serde_json::json!(true)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn script_parsing_covers_all_kinds_block_grouping_and_validation() {
        let script = parse_script(&lines(&[
            &format!(
                r#"{{"kind":"reply","method":"initialize","result":{}}}"#,
                init_result_json(false)
            ),
            r#"{"kind":"reply","method":"load_file","error":{"code":-32002,"message":"file load failed"}}"#,
            r#"{"kind":"emit","method":"progress","params":{"file_id":"f1","records_so_far":0}}"#,
            r#"{"kind":"sleep","ms":40000}"#,
            r#"{"kind":"reply","method":"parse","result":{"records_total":1}}"#,
            r#"{"kind":"reply","method":"shutdown","result":{}}"#,
        ]))
        .expect("script should parse");

        assert_eq!(
            script.len(),
            4,
            "blocks: initialize/load_file/parse/shutdown"
        );

        // parse 块：emit + sleep + reply 按剧本行顺序归组。
        let parse = &script["parse"];
        assert_eq!(parse.len(), 3);
        assert!(matches!(&parse[0], Instruction::Emit { method, .. } if method == "progress"));
        assert!(matches!(&parse[1], Instruction::Sleep { ms: 40_000 }));
        assert!(matches!(
            &parse[2],
            Instruction::Reply {
                reply: ReplyPayload::Result(_)
            }
        ));

        // error 载荷逐字段保留。
        match &script["load_file"][0] {
            Instruction::Reply {
                reply: ReplyPayload::Error { code, message },
            } => {
                assert_eq!(*code, -32002);
                assert_eq!(message, "file load failed");
            }
            other => panic!("unexpected: {other:?}"),
        }

        // 空对象 result（unload_file/shutdown 契约形状）。
        match &script["shutdown"][0] {
            Instruction::Reply {
                reply: ReplyPayload::Result(v),
            } => {
                assert_eq!(*v, serde_json::json!({}));
            }
            other => panic!("unexpected: {other:?}"),
        }

        // initialize result 经契约类型重序列化（capabilities 三字段齐备）。
        match &script["initialize"][0] {
            Instruction::Reply {
                reply: ReplyPayload::Result(v),
            } => {
                assert_eq!(
                    v["capabilities"],
                    serde_json::json!({"annotate":false,"subscribe":false,"binary_sidecar":false})
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        // —— 非法剧本逐一被拒 ——

        // 未知 emit 方法。
        let e = parse_script(&lines(&[
            r#"{"kind":"emit","method":"bogus","params":{}}"#,
            &format!(
                r#"{{"kind":"reply","method":"initialize","result":{}}}"#,
                init_result_json(false)
            ),
        ]))
        .unwrap_err();
        assert!(e.contains("unknown notification method"), "{e}");

        // reply 同时带 result 与 error。
        let e = parse_script(&lines(&[
            r#"{"kind":"reply","method":"initialize","result":{},"error":{"code":-1,"message":"x"}}"#,
        ]))
        .unwrap_err();
        assert!(e.contains("exactly one of"), "{e}");

        // 悬挂块（sleep 后无 reply 收尾）。
        let e = parse_script(&lines(&[r#"{"kind":"sleep","ms":1}"#])).unwrap_err();
        assert!(e.contains("unterminated block"), "{e}");

        // 同一方法两个块。
        let e = parse_script(&lines(&[
            &format!(
                r#"{{"kind":"reply","method":"initialize","result":{}}}"#,
                init_result_json(false)
            ),
            &format!(
                r#"{{"kind":"reply","method":"initialize","result":{}}}"#,
                init_result_json(false)
            ),
        ]))
        .unwrap_err();
        assert!(e.contains("duplicate block"), "{e}");

        // result 不符合契约类型（records_total 为负）。
        let e = parse_script(&lines(&[
            r#"{"kind":"reply","method":"parse","result":{"records_total":-1}}"#,
        ]))
        .unwrap_err();
        assert!(e.contains("does not match contract type"), "{e}");

        // 未知方法名。
        let e =
            parse_script(&lines(&[r#"{"kind":"reply","method":"nope","result":{}}"#])).unwrap_err();
        assert!(e.contains("unknown method"), "{e}");

        // 非法 JSON 行。
        let e = parse_script(&lines(&["not json"])).unwrap_err();
        assert!(e.contains("line 1"), "{e}");

        // 非空 result 的 unload_file。
        let e = parse_script(&lines(&[
            r#"{"kind":"reply","method":"unload_file","result":{"x":1}}"#,
        ]))
        .unwrap_err();
        assert!(e.contains("empty object"), "{e}");
    }

    #[test]
    fn custom_query_flag_patches_initialize_block() {
        // --caps custom_query：剧本持有的 initialize 结果补能力位，既有键不丢。
        let mut script = parse_script(&lines(&[&format!(
            r#"{{"kind":"reply","method":"initialize","result":{}}}"#,
            init_result_json(false)
        )]))
        .expect("script should parse");
        assert!(enable_custom_query(&mut script), "有 initialize 块应补位");
        match &script["initialize"][0] {
            Instruction::Reply {
                reply: ReplyPayload::Result(v),
            } => {
                assert_eq!(
                    v["capabilities"],
                    serde_json::json!({
                        "annotate": false,
                        "subscribe": false,
                        "binary_sidecar": false,
                        "custom_query": true
                    })
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        // 未剧本化 initialize：补位失败返回 false（main 记 WARN，不拒启动）。
        let mut script = parse_script(&lines(&[
            r#"{"kind":"reply","method":"shutdown","result":{}}"#,
        ]))
        .expect("script should parse");
        assert!(!enable_custom_query(&mut script));
    }

    #[test]
    fn rewrite_emit_file_id_follows_host_assignment() {
        // 无已加载文件 / 无 file_id 键 → 原样；有 → 改写为宿主分配值。
        let scripted = json!({"file_id": "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c", "percent": 0.5});
        assert_eq!(rewrite_emit_file_id(&scripted, &None), scripted);

        let no_fid = json!({"seq": 1});
        assert_eq!(rewrite_emit_file_id(&no_fid, &Some("h1".into())), no_fid);

        let host = Some("host-assigned-uuid".to_string());
        let out = rewrite_emit_file_id(&scripted, &host);
        assert_eq!(out["file_id"], "host-assigned-uuid");
        assert_eq!(out["percent"], 0.5, "其余字段不动");
    }

    #[test]
    fn custom_query_reply_follows_pinned_rules() {
        let caps_on = CapsFlags { custom_query: true };
        let caps_off = CapsFlags::default();

        // 规则 1：未声明能力 → -32005 + 固定 message（任务契约钉死）。
        let e = custom_query_reply(
            caps_off,
            &serde_json::json!({"file_id":"f1","query":"echo"}),
        )
        .expect_err("未声明能力必须被拒");
        assert_eq!(e.code, ERR_UNSUPPORTED_IN_V1);
        assert_eq!(e.message, "custom_query not supported");

        // 规则 2：echo —— file_id/params 原样回显，result 与契约类型互认。
        let result = custom_query_reply(
            caps_on,
            &serde_json::json!({"file_id":"f1","query":"echo","params":{"k":"v","n":1}}),
        )
        .expect("echo 查询应成功");
        let parsed: CustomQueryResult =
            serde_json::from_value(result).expect("result 必须符合 CustomQueryResult 契约");
        assert_eq!(parsed.data["echo"]["file_id"], serde_json::json!("f1"));
        assert_eq!(parsed.data["echo"]["query"], serde_json::json!("echo"));
        assert_eq!(
            parsed.data["echo"]["params"],
            serde_json::json!({"k": "v", "n": 1})
        );

        // params 缺省 → 回显空对象（§2.11：absent = empty object）。
        let result =
            custom_query_reply(caps_on, &serde_json::json!({"file_id":"f1","query":"echo"}))
                .expect("缺省 params 的 echo 应成功");
        assert_eq!(result["data"]["echo"]["params"], serde_json::json!({}));

        // 规则 3：未知 query → -32602 invalid params。
        let e = custom_query_reply(caps_on, &serde_json::json!({"file_id":"f1","query":"nope"}))
            .expect_err("未知 query 必须被拒");
        assert_eq!(e.code, ERR_INVALID_PARAMS);
        assert!(e.message.contains("nope"), "message 指明未知 query: {e:?}");

        // params 不符合契约形状（缺 query）→ 同样 -32602。
        let e = custom_query_reply(caps_on, &serde_json::json!({"file_id":"f1"}))
            .expect_err("缺 query 必须被拒");
        assert_eq!(e.code, ERR_INVALID_PARAMS);
    }

    #[test]
    fn caps_flag_parsing_is_repeatable_and_comma_separated() {
        let mut caps = CapsFlags::default();
        caps.merge_str("custom_query").expect("单值应接受");
        assert!(caps.custom_query);
        // 重复旗标 / 逗号分隔幂等。
        caps.merge_str("custom_query, custom_query")
            .expect("重复值幂等");
        assert!(caps.custom_query);

        // 未知能力名拒绝启动（只支持 custom_query）。
        let mut caps = CapsFlags::default();
        let e = caps.merge_str("annotate").unwrap_err();
        assert!(e.contains("unknown capability `annotate`"), "{e}");
        assert!(!caps.custom_query, "失败解析不得部分生效");
        // 空值同样拒绝。
        assert!(caps
            .merge_str("")
            .unwrap_err()
            .contains("unknown capability"));
    }
}
