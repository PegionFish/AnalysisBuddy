//! mock-plugin —— AnalysisBuddy 协议回放插件（NDJSON 剧本驱动）。
//!
//! 宿主按 protocol-v1.md §1.1 以 stdin/stdout 与本工具通信；本工具按剧本
//! （`--script <file>`，未给时回落环境变量 `AB_MOCK_SCRIPT`）对每个请求回放应答。
//! 剧本行四种 `kind`：`reply`（回 result 或 error）、`emit`（parse 期间推送
//! progress / RecordBatch 通知）、`sleep`（推送间睡眠，heartbeat_stop 剧本用）。
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
use serde_json::{Map, Value};

use ab_protocol::errors::ERR_METHOD_NOT_FOUND;
use ab_protocol::types::{
    AnnotateResult, CanHandleResult, FileSummary, InitializeResult, KeyValuesResult, ParseResult,
    ProgressParams, RecordBatch, SchemaResult,
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

/// stdin 上的一条宿主请求。
struct Request {
    id: Option<Value>,
    method: String,
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
fn handle(req: &Request, script: &Script, out: &mut impl Write) -> Result<Outcome, String> {
    let Some(id) = &req.id else {
        eprintln!(
            "WARN mock-plugin: request method={} without id ignored (host must not send notifications)",
            req.method
        );
        return Ok(Outcome::Continue);
    };
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
    for instr in lines {
        match instr {
            Instruction::Sleep { ms } => thread::sleep(Duration::from_millis(*ms)),
            Instruction::Emit { method, params } => write_frame(
                out,
                &NotificationFrame {
                    jsonrpc: "2.0",
                    method,
                    params,
                },
            )?,
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

enum ParsedArgs {
    Run(ScriptSource),
    Help,
}

fn parse_args() -> Result<ParsedArgs, String> {
    let mut script: Option<ScriptSource> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(ParsedArgs::Help);
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
    Ok(ParsedArgs::Run(source))
}

fn print_usage() {
    eprintln!(
        "mock-plugin — AnalysisBuddy protocol replay plugin (NDJSON scripts)\n\n\
         USAGE:\n    \
         mock-plugin --script <path|->\n\n\
         OPTIONS:\n    \
         --script <path>   NDJSON replay script; `-` reads the script from stdin, then exits\n    \
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
    let parsed = match parse_args() {
        Ok(ParsedArgs::Run(source)) => source,
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

    let script = match parse_script(&lines) {
        Ok(script) => script,
        Err(e) => {
            eprintln!("ERROR mock-plugin: script rejected: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!(
        "INFO mock-plugin: script loaded: {} ({} blocks)",
        match &parsed {
            ScriptSource::File(path) => path.as_str(),
            ScriptSource::Stdin => "-",
        },
        script.len()
    );

    if from_stdin {
        eprintln!("INFO mock-plugin: script read from stdin; no request channel remains, exiting");
        return ExitCode::SUCCESS;
    }

    run(script)
}

/// 事件循环：逐行读 stdin 请求 → 按剧本回放 → stdin EOF / shutdown 后退出码 0。
fn run(script: Script) -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
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
        match handle(&request, &script, &mut out) {
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

    const INIT_RESULT: &str = r#"{"id":"mock","name":"Mock","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}"#;

    #[test]
    fn script_parsing_covers_all_kinds_block_grouping_and_validation() {
        let script = parse_script(&lines(&[
            &format!(r#"{{"kind":"reply","method":"initialize","result":{INIT_RESULT}}}"#),
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
            &format!(r#"{{"kind":"reply","method":"initialize","result":{INIT_RESULT}}}"#),
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
            &format!(r#"{{"kind":"reply","method":"initialize","result":{INIT_RESULT}}}"#),
            &format!(r#"{{"kind":"reply","method":"initialize","result":{INIT_RESULT}}}"#),
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
}
