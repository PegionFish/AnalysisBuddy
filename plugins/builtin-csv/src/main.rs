//! builtin-csv —— AnalysisBuddy 内置 CSV 解析插件（Rust，零运行时依赖）。
//!
//! 定位（sdk-plugins.md §3）：随宿主静态分发、终端用户零运行时依赖；不依赖任何
//! SDK，直接按协议正本收发 NDJSON（复用 `core/ab-protocol` 契约类型 + serde_json）。
//!
//! 入口：`builtin-csv --stdio`（manifest entry）。主循环线程模型：读线程处理请求，
//! `parse` 移入专用线程运行（cancel_parse 走共享原子标志即时应答），全部 stdout
//! 写经发送锁整行原子写出。

mod config;
mod csvline;
mod engine;
mod ndjson;
mod timefmt;

use std::collections::HashMap;
use std::io::{self, BufWriter, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use ab_protocol::errors::{
    ERR_CANCELLED, ERR_FILE_LOAD_FAILED, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_PLUGIN_BUSY,
    ERR_UNSUPPORTED_IN_V1,
};
use ab_protocol::types::{
    CanHandleParams, Capabilities, InitializeResult, KeyValuesParams, LoadFileParams, ParseParams,
    RecordBatch, SchemaResult,
};
use serde_json::{json, Value};

use config::Config;
use engine::{ParseError, Sink};
use ndjson::{write_frame, FrameReader};

const PLUGIN_ID: &str = "builtin-csv";
const PLUGIN_NAME: &str = "CSV Universal Parser";

/// parse 专用槽位（单 parse 线程；§3.4 取消经共享原子标志）。
struct ParseSlot {
    file_id: String,
    cancel: Arc<AtomicBool>,
}

/// NDJSON 输出端（stdout + 发送锁，整行原子写）。
struct StdioSink {
    out: Arc<Mutex<BufWriter<io::Stdout>>>,
    file_id: String,
}

impl Sink for StdioSink {
    fn batch(&mut self, batch: RecordBatch) {
        let params = serde_json::to_value(&batch).expect("RecordBatch serializable");
        self.send("RecordBatch", params);
    }

    fn progress(&mut self, percent: Option<f64>, records_so_far: u64, bytes_read: Option<u64>) {
        let mut params = json!({ "file_id": self.file_id, "records_so_far": records_so_far });
        if let Some(p) = percent {
            params["percent"] = json!(p);
        }
        if let Some(b) = bytes_read {
            params["bytes_read"] = json!(b);
        }
        self.send("progress", params);
    }
}

impl StdioSink {
    fn send(&mut self, method: &str, params: Value) {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut out = self.out.lock().expect("stdout lock");
        if let Err(e) = write_frame(&mut *out, &frame) {
            eprintln!("ERROR builtin-csv: stdout write failed: {e}");
        }
    }
}

/// 应用状态（全程经 Arc 共享，parse 线程与读线程同用）。
struct App {
    cfg: Arc<Config>,
    fingerprints: Arc<Vec<String>>,
    files: Mutex<HashMap<String, engine::LoadedFile>>,
    /// 最近一次 load 的 metrics（schema 按 load 分析惰性确定，§3.5；
    /// 即使文件已 unload 仍保留，保证固定回放序 schema→load 后重查可得）。
    last_metrics: Mutex<Vec<ab_protocol::types::MetricDef>>,
    out: Arc<Mutex<BufWriter<io::Stdout>>>,
    parse_slot: Mutex<Option<ParseSlot>>,
}

impl App {
    fn respond(&self, id: &Value, result: &Value) {
        let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let mut out = self.out.lock().expect("stdout lock");
        if let Err(e) = write_frame(&mut *out, &frame) {
            eprintln!("ERROR builtin-csv: stdout write failed: {e}");
        }
    }

    fn respond_error(&self, id: &Value, code: i32, message: &str, data: Option<Value>) {
        let mut error = json!({ "code": code, "message": message });
        if let Some(d) = data {
            error["data"] = d;
        }
        let frame = json!({ "jsonrpc": "2.0", "id": id, "error": error });
        let mut out = self.out.lock().expect("stdout lock");
        if let Err(e) = write_frame(&mut *out, &frame) {
            eprintln!("ERROR builtin-csv: stdout write failed: {e}");
        }
    }

    fn is_parsing(&self, file_id: &str) -> bool {
        self.parse_slot
            .lock()
            .expect("parse slot lock")
            .as_ref()
            .map(|s| s.file_id == file_id)
            .unwrap_or(false)
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(msg: &Value) -> Result<T, String> {
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    serde_json::from_value(params).map_err(|e| e.to_string())
}

/// 处理一条请求；返回是否继续读 stdin（false = shutdown 已应答，应退出）。
fn handle(app: &Arc<App>, msg: Value, id: Value, method: &str) -> Result<bool, String> {
    match method {
        "initialize" => {
            let result = InitializeResult {
                id: PLUGIN_ID.to_string(),
                name: PLUGIN_NAME.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                capabilities: Capabilities {
                    annotate: false,
                    subscribe: false,
                    binary_sidecar: false,
                },
            };
            let v = serde_json::to_value(&result).expect("serializable");
            app.respond(&id, &v);
        }
        "can_handle" => {
            let params: CanHandleParams = match parse_params(&msg) {
                Ok(p) => p,
                Err(e) => {
                    app.respond_error(&id, ERR_INVALID_PARAMS, "Invalid params", Some(json!(e)));
                    return Ok(true);
                }
            };
            let result = engine::can_handle(&params, &app.cfg, &app.fingerprints);
            let v = serde_json::to_value(&result).expect("serializable");
            app.respond(&id, &v);
        }
        "load_file" => {
            let params: LoadFileParams = match parse_params(&msg) {
                Ok(p) => p,
                Err(e) => {
                    app.respond_error(&id, ERR_INVALID_PARAMS, "Invalid params", Some(json!(e)));
                    return Ok(true);
                }
            };
            if app.is_parsing(&params.file_id) {
                app.respond_error(&id, ERR_PLUGIN_BUSY, "plugin busy", None);
                return Ok(true);
            }
            match engine::load_file(&params.file_id, &params.path, &app.cfg) {
                Ok(lf) => {
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
                    let summary = engine::to_summary(&lf);
                    let v = serde_json::to_value(&summary).expect("serializable");
                    let metrics = lf.metrics.clone();
                    {
                        let mut files = app.files.lock().expect("files lock");
                        files.insert(params.file_id, lf);
                    }
                    {
                        let mut last = app.last_metrics.lock().expect("last metrics lock");
                        *last = metrics;
                    }
                    app.respond(&id, &v);
                }
                Err(e) => {
                    eprintln!("ERROR builtin-csv: load failed: {e}");
                    app.respond_error(
                        &id,
                        ERR_FILE_LOAD_FAILED,
                        "file load failed",
                        Some(json!({ "path": params.path, "detail": e })),
                    );
                }
            }
        }
        "parse" => {
            let params: ParseParams = match parse_params(&msg) {
                Ok(p) => p,
                Err(e) => {
                    app.respond_error(&id, ERR_INVALID_PARAMS, "Invalid params", Some(json!(e)));
                    return Ok(true);
                }
            };
            let file_id = params.file_id.clone();
            let loaded = {
                let files = app.files.lock().expect("files lock");
                files.contains_key(&file_id)
            };
            if !loaded {
                app.respond_error(
                    &id,
                    ERR_INVALID_PARAMS,
                    "Invalid params: file_id not loaded",
                    Some(json!({ "file_id": file_id })),
                );
                return Ok(true);
            }
            let mut slot = app.parse_slot.lock().expect("parse slot lock");
            if slot.is_some() {
                app.respond_error(
                    &id,
                    ERR_PLUGIN_BUSY,
                    "plugin busy: a parse is already running",
                    Some(json!({ "file_id": file_id })),
                );
                return Ok(true);
            }
            let cancel = Arc::new(AtomicBool::new(false));
            *slot = Some(ParseSlot {
                file_id: file_id.clone(),
                cancel: cancel.clone(),
            });
            drop(slot);
            let app = app.clone();
            thread::spawn(move || {
                let lf = {
                    let mut files = app.files.lock().expect("files lock");
                    files.remove(&file_id).expect("loaded file present")
                };
                let mut sink = StdioSink {
                    out: app.out.clone(),
                    file_id: file_id.clone(),
                };
                let mut lf = lf;
                let outcome = engine::parse_file(&mut lf, &mut sink, &cancel);
                {
                    let mut files = app.files.lock().expect("files lock");
                    files.insert(file_id.clone(), lf);
                }
                {
                    let mut slot = app.parse_slot.lock().expect("parse slot lock");
                    *slot = None;
                }
                match outcome {
                    Ok(total) => {
                        let v = json!({ "records_total": total });
                        app.respond(&id, &v);
                    }
                    Err(ParseError::Cancelled) => {
                        app.respond_error(&id, ERR_CANCELLED, "parse cancelled by host", None);
                    }
                }
            });
        }
        "schema" => {
            let result = {
                let last = app.last_metrics.lock().expect("last metrics lock");
                SchemaResult {
                    metrics: last.clone(),
                }
            };
            let v = serde_json::to_value(&result).expect("serializable");
            app.respond(&id, &v);
        }
        "key_values" => {
            let params: KeyValuesParams = match parse_params(&msg) {
                Ok(p) => p,
                Err(e) => {
                    app.respond_error(&id, ERR_INVALID_PARAMS, "Invalid params", Some(json!(e)));
                    return Ok(true);
                }
            };
            if app.is_parsing(&params.file_id) {
                app.respond_error(&id, ERR_PLUGIN_BUSY, "plugin busy", None);
                return Ok(true);
            }
            let files = app.files.lock().expect("files lock");
            match files.get(&params.file_id) {
                Some(lf) => {
                    let result = engine::key_values(lf, params.timestamp_ms);
                    let v = serde_json::to_value(&result).expect("serializable");
                    drop(files);
                    app.respond(&id, &v);
                }
                None => {
                    drop(files);
                    app.respond_error(
                        &id,
                        ERR_INVALID_PARAMS,
                        "Invalid params: file_id not loaded",
                        Some(json!({ "file_id": params.file_id })),
                    );
                }
            }
        }
        "annotate" => {
            app.respond_error(
                &id,
                ERR_UNSUPPORTED_IN_V1,
                "annotate is not supported by this plugin",
                None,
            );
        }
        "unload_file" => {
            let file_id = msg
                .get("params")
                .and_then(|p| p.get("file_id"))
                .and_then(Value::as_str)
                .filter(|f| !f.is_empty());
            let file_id = match file_id {
                Some(f) => f.to_string(),
                None => {
                    app.respond_error(
                        &id,
                        ERR_INVALID_PARAMS,
                        "Invalid params: file_id required",
                        None,
                    );
                    return Ok(true);
                }
            };
            if app.is_parsing(&file_id) {
                app.respond_error(&id, ERR_PLUGIN_BUSY, "plugin busy", None);
                return Ok(true);
            }
            {
                let mut files = app.files.lock().expect("files lock");
                files.remove(&file_id);
            }
            app.respond(&id, &Value::Object(Default::default()));
        }
        "cancel_parse" => {
            let file_id = msg
                .get("params")
                .and_then(|p| p.get("file_id"))
                .and_then(Value::as_str)
                .filter(|f| !f.is_empty());
            let file_id = match file_id {
                Some(f) => f.to_string(),
                None => {
                    app.respond_error(
                        &id,
                        ERR_INVALID_PARAMS,
                        "Invalid params: file_id required",
                        None,
                    );
                    return Ok(true);
                }
            };
            {
                let slot = app.parse_slot.lock().expect("parse slot lock");
                if let Some(slot) = slot.as_ref() {
                    if slot.file_id == file_id {
                        slot.cancel.store(true, Ordering::Relaxed);
                    }
                }
            }
            app.respond(&id, &Value::Object(Default::default()));
        }
        "shutdown" => {
            app.respond(&id, &Value::Object(Default::default()));
            return Ok(false);
        }
        other => {
            app.respond_error(
                &id,
                ERR_METHOD_NOT_FOUND,
                "Method not found",
                Some(json!({ "method": other })),
            );
        }
    }
    Ok(true)
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
        let Ok(manifest) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(fps) = manifest
            .get("match")
            .and_then(|m| m.get("header_fingerprints"))
            .and_then(Value::as_array)
        {
            return fps
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
        }
    }
    Vec::new()
}

fn parse_args() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Ok(true); // 无参数默认 stdio 模式（宽容）
    }
    if args.len() == 1 && args[0] == "--stdio" {
        return Ok(true);
    }
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!(
            "builtin-csv -- AnalysisBuddy built-in CSV plugin\n\nUSAGE:\n    builtin-csv --stdio"
        );
        return Ok(false);
    }
    Err(format!("unknown arguments: {}", args.join(" ")))
}

fn main() -> ExitCode {
    match parse_args() {
        Ok(true) => {}
        Ok(false) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ERROR builtin-csv: {e}");
            eprintln!("USAGE: builtin-csv --stdio");
            return ExitCode::from(2);
        }
    }

    let (cfg, warnings) = config::load_config();
    for w in &warnings {
        eprintln!("WARN builtin-csv: {w}");
    }
    let fingerprints = load_fingerprints();

    let app = Arc::new(App {
        cfg: Arc::new(cfg),
        fingerprints: Arc::new(fingerprints),
        files: Mutex::new(HashMap::new()),
        last_metrics: Mutex::new(Vec::new()),
        out: Arc::new(Mutex::new(BufWriter::new(io::stdout()))),
        parse_slot: Mutex::new(None),
    });

    let stdin = io::stdin();
    let mut reader = FrameReader::new(stdin.lock());
    loop {
        let line = match reader.read_frame() {
            Ok(Some(line)) => line,
            Ok(None) => break, // stdin EOF → 退出码 0（§9 约定 5）
            Err(e) => {
                eprintln!("ERROR builtin-csv: protocol error on stdin: {e}");
                return ExitCode::from(1);
            }
        };
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARN builtin-csv: malformed JSON on stdin ignored: {e}");
                continue;
            }
        };
        let method = match value.get("method").and_then(Value::as_str) {
            Some(m) => m.to_string(),
            None => {
                eprintln!("WARN builtin-csv: frame without method ignored");
                continue;
            }
        };
        let id = value.get("id").cloned().unwrap_or_else(|| Value::Null);
        match handle(&app, value, id, &method) {
            Ok(true) => {}
            Ok(false) => break, // shutdown 已应答
            Err(e) => {
                eprintln!("ERROR builtin-csv: {e}");
                let mut out = app.out.lock().expect("stdout lock");
                let _ = out.flush();
                return ExitCode::from(1);
            }
        }
    }
    {
        let mut out = app.out.lock().expect("stdout lock");
        if let Err(e) = out.flush() {
            eprintln!("ERROR builtin-csv: final stdout flush failed: {e}");
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}
