//! 请求分发主循环：协议通用臂（initialize/schema/unload/cancel/shutdown/parse 槽位
//! 管理…）由 runtime 实现，插件只实现 [`Plugin`] 业务 handler 映射。
//!
//! 控制流自两插件 main.rs 的 `handle` / `main` 逐字移植（G2）：分支顺序、busy 判定、
//! 锁的持锁窗口、线程 spawn 时序、响应帧字段顺序均保持不变（golden 转录钉住）。

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use ab_protocol::errors::{
    ERR_CANCELLED, ERR_FILE_LOAD_FAILED, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_PLUGIN_BUSY,
    ERR_UNSUPPORTED_IN_V1,
};
use ab_protocol::types::{
    CanHandleParams, CanHandleResult, Capabilities, FileSummary, InitializeResult, KeyValuesParams,
    KeyValuesResult, LoadFileParams, MetricDef, ParseParams, SchemaResult,
};
use serde_json::{json, Value};

use crate::args::{parse_stdio_args, PluginMeta, StdioArgs};
use crate::frame::FrameReader;
use crate::sink::{Out, Sink, StdioSink};

/// parse 被取消（§3.4 `cancel_parse` 经共享原子标志生效）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// 插件业务 handler 映射：插件只为下列方法提供实现；initialize/schema/
/// unload_file/cancel_parse/annotate/shutdown/未知方法与 parse 槽位管理等协议
/// 通用臂全部由 runtime 承担。
pub trait Plugin: Send + Sync + 'static {
    /// 已加载文件的插件侧状态（`load_file` 成功的产物；parse 期间被移入专用线程）。
    type Loaded: Send + 'static;

    /// initialize.id（须与 manifest `id` 一致）。
    fn id(&self) -> &str;

    /// initialize.name。
    fn name(&self) -> &str;

    /// initialize.version（插件自身版本，通常 `env!("CARGO_PKG_VERSION")`）。
    fn version(&self) -> &str;

    /// initialize.capabilities（缺省：v1 全 false）。
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            annotate: false,
            subscribe: false,
            binary_sidecar: false,
            custom_query: false,
        }
    }

    /// §2.2 can_handle 打分。
    fn can_handle(&self, params: &CanHandleParams) -> CanHandleResult;

    /// §2.3 load_file：读取并分析文件。`Err(detail)` → `-32002`
    /// （data = `{"path": …, "detail": …}`）。坏行 WARN 等副作用日志由实现自行
    /// 输出（先于响应帧，与抽取前时序一致）。
    fn load_file(&self, params: &LoadFileParams) -> Result<Self::Loaded, String>;

    /// §2.3 load_file 响应载荷。
    fn summarize(loaded: &Self::Loaded) -> FileSummary;

    /// §2.5 schema：该文件 load 时冻结的 metric 集（unload 后仍可查）。
    fn metrics(loaded: &Self::Loaded) -> Vec<MetricDef>;

    /// §2.4 parse：流式推 batch/progress 通知，返回记录总数；取消返回 [`Cancelled`]。
    fn parse(
        loaded: &mut Self::Loaded,
        sink: &mut dyn Sink,
        cancel: &AtomicBool,
    ) -> Result<u64, Cancelled>;

    /// §2.6 key_values（`timestamp_ms` 游标取值）。
    fn key_values(loaded: &Self::Loaded, timestamp_ms: i64) -> KeyValuesResult;
}

/// parse 专用槽位（单 parse 线程；§3.4 取消经共享原子标志）。
struct ParseSlot {
    file_id: String,
    cancel: Arc<AtomicBool>,
}

/// runtime 状态：插件实例 + 已加载文件 + 最近 metric 集 + 输出端 + parse 槽位。
struct Runtime<P: Plugin> {
    plugin: P,
    tag: &'static str,
    out: Out,
    files: Mutex<HashMap<String, P::Loaded>>,
    last_metrics: Mutex<Vec<MetricDef>>,
    parse_slot: Mutex<Option<ParseSlot>>,
}

/// 请求 params 反序列化（缺省空对象；`Err` 文案进 -32602 的 data）。
pub fn parse_params<T: serde::de::DeserializeOwned>(msg: &Value) -> Result<T, String> {
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    serde_json::from_value(params).map_err(|e| e.to_string())
}

impl<P: Plugin> Runtime<P> {
    fn is_parsing(&self, file_id: &str) -> bool {
        self.parse_slot
            .lock()
            .expect("parse slot lock")
            .as_ref()
            .map(|s| s.file_id == file_id)
            .unwrap_or(false)
    }

    /// 处理一条请求；返回是否继续读 stdin（false = shutdown 已应答，应退出）。
    fn handle(rt: &Arc<Self>, msg: Value, id: Value, method: &str) -> Result<bool, String> {
        match method {
            "initialize" => {
                // G1 版本回显：protocol_version 填协议常量；v1 时该键被 serde
                // skip-if-default 省略（与历史报文逐字节兼容）。
                let result = InitializeResult {
                    id: rt.plugin.id().to_string(),
                    name: rt.plugin.name().to_string(),
                    version: rt.plugin.version().to_string(),
                    capabilities: rt.plugin.capabilities(),
                    protocol_version: ab_protocol::PROTOCOL_VERSION,
                };
                let v = serde_json::to_value(&result).expect("serializable");
                rt.out.respond(&id, &v);
            }
            "can_handle" => {
                let params: CanHandleParams = match parse_params(&msg) {
                    Ok(p) => p,
                    Err(e) => {
                        rt.out.respond_error(
                            &id,
                            ERR_INVALID_PARAMS,
                            "Invalid params",
                            Some(json!(e)),
                        );
                        return Ok(true);
                    }
                };
                let result = rt.plugin.can_handle(&params);
                let v = serde_json::to_value(&result).expect("serializable");
                rt.out.respond(&id, &v);
            }
            "load_file" => {
                let params: LoadFileParams = match parse_params(&msg) {
                    Ok(p) => p,
                    Err(e) => {
                        rt.out.respond_error(
                            &id,
                            ERR_INVALID_PARAMS,
                            "Invalid params",
                            Some(json!(e)),
                        );
                        return Ok(true);
                    }
                };
                if rt.is_parsing(&params.file_id) {
                    rt.out
                        .respond_error(&id, ERR_PLUGIN_BUSY, "plugin busy", None);
                    return Ok(true);
                }
                match rt.plugin.load_file(&params) {
                    Ok(lf) => {
                        let summary = P::summarize(&lf);
                        let v = serde_json::to_value(&summary).expect("serializable");
                        let metrics = P::metrics(&lf);
                        {
                            let mut files = rt.files.lock().expect("files lock");
                            files.insert(params.file_id, lf);
                        }
                        {
                            let mut last = rt.last_metrics.lock().expect("last metrics lock");
                            *last = metrics;
                        }
                        rt.out.respond(&id, &v);
                    }
                    Err(e) => {
                        eprintln!("ERROR {}: load failed: {e}", rt.tag);
                        rt.out.respond_error(
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
                        rt.out.respond_error(
                            &id,
                            ERR_INVALID_PARAMS,
                            "Invalid params",
                            Some(json!(e)),
                        );
                        return Ok(true);
                    }
                };
                let file_id = params.file_id.clone();
                let loaded = {
                    let files = rt.files.lock().expect("files lock");
                    files.contains_key(&file_id)
                };
                if !loaded {
                    rt.out.respond_error(
                        &id,
                        ERR_INVALID_PARAMS,
                        "Invalid params: file_id not loaded",
                        Some(json!({ "file_id": file_id })),
                    );
                    return Ok(true);
                }
                let mut slot = rt.parse_slot.lock().expect("parse slot lock");
                if slot.is_some() {
                    rt.out.respond_error(
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
                let rt = rt.clone();
                thread::spawn(move || {
                    let lf = {
                        let mut files = rt.files.lock().expect("files lock");
                        files.remove(&file_id).expect("loaded file present")
                    };
                    let mut sink = StdioSink::new(rt.out.clone(), file_id.clone());
                    let mut lf = lf;
                    let outcome = P::parse(&mut lf, &mut sink, &cancel);
                    {
                        let mut files = rt.files.lock().expect("files lock");
                        files.insert(file_id.clone(), lf);
                    }
                    {
                        let mut slot = rt.parse_slot.lock().expect("parse slot lock");
                        *slot = None;
                    }
                    match outcome {
                        Ok(total) => {
                            let v = json!({ "records_total": total });
                            rt.out.respond(&id, &v);
                        }
                        Err(Cancelled) => {
                            rt.out.respond_error(
                                &id,
                                ERR_CANCELLED,
                                "parse cancelled by host",
                                None,
                            );
                        }
                    }
                });
            }
            "schema" => {
                let result = {
                    let last = rt.last_metrics.lock().expect("last metrics lock");
                    SchemaResult {
                        metrics: last.clone(),
                    }
                };
                let v = serde_json::to_value(&result).expect("serializable");
                rt.out.respond(&id, &v);
            }
            "key_values" => {
                let params: KeyValuesParams = match parse_params(&msg) {
                    Ok(p) => p,
                    Err(e) => {
                        rt.out.respond_error(
                            &id,
                            ERR_INVALID_PARAMS,
                            "Invalid params",
                            Some(json!(e)),
                        );
                        return Ok(true);
                    }
                };
                if rt.is_parsing(&params.file_id) {
                    rt.out
                        .respond_error(&id, ERR_PLUGIN_BUSY, "plugin busy", None);
                    return Ok(true);
                }
                let files = rt.files.lock().expect("files lock");
                match files.get(&params.file_id) {
                    Some(lf) => {
                        let result = P::key_values(lf, params.timestamp_ms);
                        let v = serde_json::to_value(&result).expect("serializable");
                        drop(files);
                        rt.out.respond(&id, &v);
                    }
                    None => {
                        drop(files);
                        rt.out.respond_error(
                            &id,
                            ERR_INVALID_PARAMS,
                            "Invalid params: file_id not loaded",
                            Some(json!({ "file_id": params.file_id })),
                        );
                    }
                }
            }
            "annotate" => {
                rt.out.respond_error(
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
                        rt.out.respond_error(
                            &id,
                            ERR_INVALID_PARAMS,
                            "Invalid params: file_id required",
                            None,
                        );
                        return Ok(true);
                    }
                };
                if rt.is_parsing(&file_id) {
                    rt.out
                        .respond_error(&id, ERR_PLUGIN_BUSY, "plugin busy", None);
                    return Ok(true);
                }
                {
                    let mut files = rt.files.lock().expect("files lock");
                    files.remove(&file_id);
                }
                rt.out.respond(&id, &Value::Object(Default::default()));
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
                        rt.out.respond_error(
                            &id,
                            ERR_INVALID_PARAMS,
                            "Invalid params: file_id required",
                            None,
                        );
                        return Ok(true);
                    }
                };
                {
                    let slot = rt.parse_slot.lock().expect("parse slot lock");
                    if let Some(slot) = slot.as_ref() {
                        if slot.file_id == file_id {
                            slot.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                }
                rt.out.respond(&id, &Value::Object(Default::default()));
            }
            "shutdown" => {
                rt.out.respond(&id, &Value::Object(Default::default()));
                return Ok(false);
            }
            other => {
                rt.out.respond_error(
                    &id,
                    ERR_METHOD_NOT_FOUND,
                    "Method not found",
                    Some(json!({ "method": other })),
                );
            }
        }
        Ok(true)
    }
}

/// 插件 stdio 主循环（`--stdio` 参数已合法的前提下）：
/// stdin EOF → 退出码 0（§9 约定 5）；帧违规 → 1；handler 致命错 → 1（尽力
/// flush 后退出）；shutdown 应答后 → 0；最终 flush 失败 → 1。
pub fn run<P: Plugin>(plugin: P, log_tag: &'static str) -> ExitCode {
    let rt = Arc::new(Runtime {
        plugin,
        tag: log_tag,
        out: Out::new(log_tag),
        files: Mutex::new(HashMap::new()),
        last_metrics: Mutex::new(Vec::new()),
        parse_slot: Mutex::new(None),
    });

    let stdin = io::stdin();
    let mut reader = FrameReader::new(stdin.lock());
    loop {
        let line = match reader.read_frame() {
            Ok(Some(line)) => line,
            Ok(None) => break, // stdin EOF → 退出码 0（§9 约定 5）
            Err(e) => {
                eprintln!("ERROR {log_tag}: protocol error on stdin: {e}");
                return ExitCode::from(1);
            }
        };
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARN {log_tag}: malformed JSON on stdin ignored: {e}");
                continue;
            }
        };
        let method = match value.get("method").and_then(Value::as_str) {
            Some(m) => m.to_string(),
            None => {
                eprintln!("WARN {log_tag}: frame without method ignored");
                continue;
            }
        };
        let id = value.get("id").cloned().unwrap_or_else(|| Value::Null);
        match Runtime::handle(&rt, value, id, &method) {
            Ok(true) => {}
            Ok(false) => break, // shutdown 已应答
            Err(e) => {
                eprintln!("ERROR {log_tag}: {e}");
                let _ = rt.out.flush();
                return ExitCode::from(1);
            }
        }
    }
    match rt.out.flush() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ERROR {log_tag}: final stdout flush failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// `--stdio` 约定入口：参数解析（未知参数 → 打印 ERROR+USAGE 后退出 2；`--help`
/// → stdout 帮助后退出 0）→ 构建插件（cfg 加载等启动副作用在 `make` 内完成，
/// 其 stderr 日志先于任何协议帧）→ 主循环。
pub fn plugin_main<P: Plugin, F: FnOnce() -> P>(meta: &PluginMeta, make: F) -> ExitCode {
    match parse_stdio_args(meta.help_text) {
        Ok(StdioArgs::Run) => {}
        Ok(StdioArgs::Help) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ERROR {}: {}", meta.log_tag, e);
            eprintln!("{}", meta.usage_line);
            return ExitCode::from(2);
        }
    }
    run(make(), meta.log_tag)
}
