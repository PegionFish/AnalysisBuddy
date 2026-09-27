//! 响应/通知写出端：stdout 发送锁 + 整行原子写（protocol-v1.md §1.2、§9 第 3 条）。
//!
//! 自两插件 main.rs 逐字相同的 `StdioSink` / `respond` / `respond_error` 收拢；
//! 日志前缀（`ERROR {tag}: …`）随 [`Out::new`] 的 tag 参数化。

use std::io::{self, BufWriter, Write};
use std::sync::{Arc, Mutex};

use ab_protocol::types::RecordBatch;
use serde_json::{json, Value};

use crate::frame::write_frame;

/// 共享 NDJSON 输出端（stdout + 发送锁，整行原子写）。克隆廉价（内部 Arc）。
#[derive(Clone)]
pub struct Out {
    inner: Arc<OutInner>,
}

struct OutInner {
    out: Mutex<BufWriter<io::Stdout>>,
    tag: &'static str,
}

impl Out {
    /// `tag` 用于 stdout 写失败时的 stderr 日志前缀（如 `"builtin-csv"`）。
    pub fn new(tag: &'static str) -> Self {
        Out {
            inner: Arc::new(OutInner {
                out: Mutex::new(BufWriter::new(io::stdout())),
                tag,
            }),
        }
    }

    /// 响应帧（result 型）；字段顺序 jsonrpc,id,result 与 protocol-v1.md §3.5 一致。
    pub fn respond(&self, id: &Value, result: &Value) {
        let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        self.send(&frame);
    }

    /// 响应帧（error 型）；`data` 存在时才输出该键。
    pub fn respond_error(&self, id: &Value, code: i32, message: &str, data: Option<Value>) {
        let mut error = json!({ "code": code, "message": message });
        if let Some(d) = data {
            error["data"] = d;
        }
        let frame = json!({ "jsonrpc": "2.0", "id": id, "error": error });
        self.send(&frame);
    }

    /// 通知帧（无 id）。
    pub fn notify(&self, method: &str, params: Value) {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.send(&frame);
    }

    fn send(&self, frame: &Value) {
        let mut out = self.inner.out.lock().expect("stdout lock");
        if let Err(e) = write_frame(&mut *out, frame) {
            eprintln!("ERROR {}: stdout write failed: {e}", self.inner.tag);
        }
    }

    pub fn flush(&self) -> io::Result<()> {
        let mut out = self.inner.out.lock().expect("stdout lock");
        out.flush()
    }
}

/// parse 输出端契约（§3.4）：批量 + 进度。引擎经此回调推送，实现方负责落 stdout。
/// （自两插件 engine.rs 的同名 trait 收拢；`parse_file` 以 `&mut dyn Sink` 消费。）
pub trait Sink {
    fn batch(&mut self, batch: RecordBatch);
    fn progress(&mut self, percent: Option<f64>, records_so_far: u64, bytes_read: Option<u64>);
}

/// [`Sink`] 的 stdout 标准实现：batch/progress → NDJSON 通知（经发送锁整行原子写）。
pub struct StdioSink {
    out: Out,
    file_id: String,
}

impl StdioSink {
    pub fn new(out: Out, file_id: impl Into<String>) -> Self {
        StdioSink {
            out,
            file_id: file_id.into(),
        }
    }
}

impl Sink for StdioSink {
    fn batch(&mut self, batch: RecordBatch) {
        let params = serde_json::to_value(&batch).expect("RecordBatch serializable");
        self.out.notify("RecordBatch", params);
    }

    fn progress(&mut self, percent: Option<f64>, records_so_far: u64, bytes_read: Option<u64>) {
        let mut params = json!({ "file_id": self.file_id, "records_so_far": records_so_far });
        if let Some(p) = percent {
            params["percent"] = json!(p);
        }
        if let Some(b) = bytes_read {
            params["bytes_read"] = json!(b);
        }
        self.out.notify("progress", params);
    }
}
