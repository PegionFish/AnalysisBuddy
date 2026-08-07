//! JSON-RPC 帧编解码与请求路由（host-runtime.md §4；protocol.md §1.2/§1.3/§1.4）。
//!
//! [`FrameReader`]：逐字节增量读行、长度先于内容校验（8 MB 上限）、复用行缓冲；
//! [`RpcChannel`]：宿主独占的单调递增 id 分配、按 id 的并发响应路由、写侧单一
//! mpsc 序列化出口（整行原子写出）；notification 按 method 分流。

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use ab_protocol::types::{ProgressParams, RecordBatch};

use crate::HostError;

// ---------------------------------------------------------------------------
// 帧读取（§4.1）
// ---------------------------------------------------------------------------

/// 帧读取错误（§4.1 枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// 单行超 8 MB（§1.3：整行未读完即返回，不留驻超限内存）。
    LineTooLong,
    /// 孤立 `\r` / 空行 / 非法 UTF-8。
    MalformedLine,
    /// 行可解码但 JSON 非法。
    InvalidJson,
    /// stdout 关闭 = 进程退出信号（§5.4）。
    Eof,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::LineTooLong => write!(f, "line exceeds 8MB limit"),
            FrameError::MalformedLine => write!(f, "malformed protocol line"),
            FrameError::InvalidJson => write!(f, "invalid JSON on stdout"),
            FrameError::Eof => write!(f, "stdout closed"),
        }
    }
}

impl std::error::Error for FrameError {}

/// NDJSON 行读取器（禁止 `read_line`/`lines()`：每行一次分配且无法先于内容校验长度）。
pub struct FrameReader<R> {
    reader: BufReader<R>,
    /// 复用行缓冲，clear() 后复用。
    buf: Vec<u8>,
    /// 已消费指针（未清空缓冲期间定位下一行起点）。
    consumed: usize,
    scratch: [u8; 8192],
}

/// stdout 帧读取器的具体化别名（会话用）。
pub type StdoutFrameReader = FrameReader<tokio::process::ChildStdout>;

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// 单行上限：8 × 1024 × 1024 字节（§1.3）。
    pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            buf: Vec::new(),
            consumed: 0,
            scratch: [0u8; 8192],
        }
    }

    /// 读下一帧（§4.1 算法）。超限在整行读满前返回 [`FrameError::LineTooLong`]。
    pub async fn next_frame(&mut self) -> Result<serde_json::Value, FrameError> {
        loop {
            if let Some(end) = self.scan_newline() {
                let line: Vec<u8> = self.buf[self.consumed..end].to_vec();
                self.consumed = end + 1;
                if self.consumed == self.buf.len() {
                    self.buf.clear();
                    self.consumed = 0;
                }
                return Self::parse_line(&line);
            }
            if self.buf.len() - self.consumed > Self::MAX_LINE_BYTES {
                return Err(FrameError::LineTooLong);
            }
            let n = self
                .reader
                .read(&mut self.scratch)
                .await
                .map_err(|_| FrameError::Eof)?;
            if n == 0 {
                // stdout 关闭：残留半行按 MalformedLine，否则 Eof。
                if self.buf.len() > self.consumed {
                    return Err(FrameError::MalformedLine);
                }
                return Err(FrameError::Eof);
            }
            self.buf.extend_from_slice(&self.scratch[..n]);
        }
    }

    /// 在未消费区扫描 `0x0A`。
    fn scan_newline(&self) -> Option<usize> {
        self.buf[self.consumed..]
            .iter()
            .position(|b| *b == 0x0A)
            .map(|pos| self.consumed + pos)
    }

    /// 行 → 帧：孤立 `\r` / 空行 / 非法 UTF-8 → MalformedLine；JSON 非法 → InvalidJson。
    fn parse_line(line: &[u8]) -> Result<serde_json::Value, FrameError> {
        if line.is_empty() || line.contains(&0x0D) {
            return Err(FrameError::MalformedLine);
        }
        let text = String::from_utf8(line.to_vec()).map_err(|_| FrameError::MalformedLine)?;
        serde_json::from_str(&text).map_err(|_| FrameError::InvalidJson)
    }
}

// ---------------------------------------------------------------------------
// 调用结果与请求路由（§4.3）
// ---------------------------------------------------------------------------

/// 单次调用的结局。
#[derive(Debug, Clone, PartialEq)]
pub enum RpcOutcome {
    /// 插件返回的 result。
    Result(serde_json::Value),
    /// 插件返回的 JSON-RPC error（原样透传 UI）。
    Error {
        code: i32,
        message: String,
        data: Option<serde_json::Value>,
    },
    /// 宿主合成：进程退出 / 会话终止 / 超时（§8.1）。
    TransportError(HostError),
}

/// 路由表中一个在途调用（§4.3）。
pub struct PendingCall {
    pub method: String,
    pub issued_at: Instant,
    pub tx: oneshot::Sender<RpcOutcome>,
}

/// 通知分流枚举（§4.4）。
#[derive(Debug, Clone, PartialEq)]
pub enum PluginNotification {
    Progress(ProgressParams),
    RecordBatch(RecordBatch),
}

/// 有界通知扇出：满则丢并计数（§4.4「mpsc 有界通道、满则丢旧并计数」）。
///
/// 订阅者 API 保持 `mpsc::Receiver`（§4.4 签名）；mpsc 只能从持有 receiver 的一侧
/// 丢弃积压消息，而 receiver 归订阅者所有，故满时丢弃**新**通知并计数（读泵不受
/// 阻塞、队列恒 ≤ 容量）；订阅者及时排空时行为等价于丢旧。
pub struct NotificationFan {
    subs: Mutex<Vec<mpsc::Sender<PluginNotification>>>,
    dropped: AtomicU64,
}

const NOTIFICATION_CAP: usize = 1024;

impl Default for NotificationFan {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationFan {
    pub fn new() -> Self {
        Self {
            subs: Mutex::new(Vec::new()),
            dropped: AtomicU64::new(0),
        }
    }

    /// 订阅通知流（管线 / UI 各自订阅）。
    pub fn subscribe(&self) -> mpsc::Receiver<PluginNotification> {
        let (tx, rx) = mpsc::channel(NOTIFICATION_CAP);
        self.subs.lock().expect("fan lock poisoned").push(tx);
        rx
    }

    /// 扇出一条通知；订阅者已关闭则移除。
    pub fn fan_out(&self, msg: PluginNotification) {
        let mut subs = self.subs.lock().expect("fan lock poisoned");
        subs.retain(|tx| {
            match tx.try_send(msg.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // 队列满：丢弃该通知并计数。
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
    }

    /// 因积压被丢弃的通知计数（诊断）。
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// `RecordBatch` 的 `seq` 连续性校验器（§4.4 / protocol.md §3.2）。
/// 期望值 = 该 `file_id` 上一 seq + 1（首批 0）；缺号 / 重复 = 协议致命错。
#[derive(Debug, Default)]
pub struct SeqValidator {
    last_seq: HashMap<String, u64>,
}

impl SeqValidator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 新一次 parse 开始时复位该文件的状态（seq 按 parse 计，从 0 单调递增）。
    pub fn reset(&mut self, file_id: &str) {
        self.last_seq.remove(file_id);
    }

    /// 校验并记录批次序号。非法返回协议致命错描述。
    pub fn accept(&mut self, batch: &RecordBatch) -> Result<(), String> {
        let expected = self
            .last_seq
            .get(&batch.file_id)
            .map(|last| last + 1)
            .unwrap_or(0);
        if batch.seq != expected {
            return Err(format!(
                "RecordBatch seq discontinuity for file {}: expected {}, got {}",
                batch.file_id, expected, batch.seq
            ));
        }
        self.last_seq.insert(batch.file_id.clone(), batch.seq);
        Ok(())
    }

    /// 会话终止时清空全部校验状态。
    pub fn reset_all(&mut self) {
        self.last_seq.clear();
    }
}

/// 帧错误的会话侧处置决定（§4.2 处置表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDisposition {
    /// 记日志告警后继续读（首次 InvalidJson）。
    Continue,
    /// 终止会话（其余帧错误）。
    Stop,
}

/// 读泵对 notification 的分流处理（§4.4）。返回 `Err` = 协议致命错（终止会话）。
pub trait NotificationHandler: Send {
    fn on_notification(&mut self, method: &str, params: &serde_json::Value) -> Result<(), String>;

    /// 帧错误处置（默认全部 Stop；会话侧对首次 InvalidJson 返回 Continue）。
    fn on_frame_error(&mut self, _err: FrameError) -> FrameDisposition {
        FrameDisposition::Stop
    }
}

/// 读泵结束原因：帧错误或协议致命错。
#[derive(Debug)]
pub enum ReadLoopError {
    Frame(FrameError),
    /// notification 分流返回的协议致命错（如 seq 缺号）。
    Fatal(String),
}

impl fmt::Display for ReadLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadLoopError::Frame(e) => write!(f, "{e}"),
            ReadLoopError::Fatal(r) => write!(f, "fatal protocol error: {r}"),
        }
    }
}

// ---------------------------------------------------------------------------
// RpcChannel（§4.3）
// ---------------------------------------------------------------------------

/// 宿主侧 JSON-RPC 通道：id 分配 + 写侧序列化出口 + pending 路由表。
/// 读侧由 [`run_read_loop`] 泵入。
pub struct RpcChannel {
    next_id: Arc<AtomicU64>,
    pending: Arc<Mutex<HashMap<u64, PendingCall>>>,
    writer: Mutex<Option<mpsc::Sender<String>>>,
    broken: Mutex<Option<HostError>>,
    fan: Arc<NotificationFan>,
}

impl RpcChannel {
    /// 以写侧序列化出口构造通道。写者任务（持有子进程 stdin）由会话启动。
    pub fn new(writer: mpsc::Sender<String>) -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(0)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            writer: Mutex::new(Some(writer)),
            broken: Mutex::new(None),
            fan: Arc::new(NotificationFan::new()),
        }
    }

    /// 发起一次调用：分配 id → 登记 pending → 整行写出 → 按超时等待响应。
    ///
    /// 超时返回 `Ok(RpcOutcome::TransportError(_))`（宿主合成，§8.1）；
    /// 通道已死（写侧 BrokenPipe / 会话终止）返回 `Err`。
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<RpcOutcome, HostError> {
        if let Some(err) = self.broken.lock().expect("broken lock").as_ref() {
            return Err(err.clone());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("pending lock poisoned").insert(
            id,
            PendingCall {
                method: method.to_string(),
                issued_at: Instant::now(),
                tx,
            },
        );
        let frame = Self::build_request_frame(id, method, &params);
        let sender = self
            .writer
            .lock()
            .expect("writer lock poisoned")
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                self.pending
                    .lock()
                    .expect("pending lock poisoned")
                    .remove(&id);
                HostError::Transport("plugin stdin channel closed".to_string())
            })?;
        let sent = sender.send(frame).await;
        if sent.is_err() {
            // 写者任务已死（进程退出 / BrokenPipe）→ 等价进程退出。
            let err = HostError::Transport("plugin stdin write channel closed".to_string());
            self.drain_pending(err.clone());
            return Err(err);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(HostError::Transport(
                "response channel dropped while waiting".to_string(),
            )),
            Err(_elapsed) => {
                self.pending
                    .lock()
                    .expect("pending lock poisoned")
                    .remove(&id);
                Ok(RpcOutcome::TransportError(HostError::Transport(format!(
                    "request `{method}` timed out after {timeout:?}"
                ))))
            }
        }
    }

    /// 类型化便捷封装（泛型外壳，内部走 [`Self::call`]）。
    pub async fn call_typed<Req, Resp>(
        &self,
        method: &str,
        req: Req,
        timeout: std::time::Duration,
    ) -> Result<Resp, HostError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let params = serde_json::to_value(req)
            .map_err(|e| HostError::Transport(format!("serialize `{method}` params: {e}")))?;
        let outcome = self.call(method, params, timeout).await?;
        from_outcome::<Resp>(outcome)
    }

    /// 在途请求数（诊断）。
    pub fn pending_count(&self) -> usize {
        self.pending.lock().expect("pending lock poisoned").len()
    }

    /// 会话终止时清空路由表：全部在途请求以 `TransportError(err)` 完成（§5.2）。
    pub fn drain_pending(&self, err: HostError) {
        *self.broken.lock().expect("broken lock") = Some(err.clone());
        let mut pending = self.pending.lock().expect("pending lock poisoned");
        for (_, call) in std::mem::take(&mut *pending) {
            let _ = call.tx.send(RpcOutcome::TransportError(err.clone()));
        }
    }

    /// 关闭写侧：drop 序列化出口 → 写者任务结束 → 子进程 stdin EOF（§9 第 5 条）。
    pub fn close_stdin(&self) {
        *self.writer.lock().expect("writer lock poisoned") = None;
    }

    /// 订阅本会话的 notification 流（§4.4）。
    pub fn subscribe_notifications(&self) -> mpsc::Receiver<PluginNotification> {
        self.fan.subscribe()
    }

    /// 扇出一条通知给全部订阅者（读泵分流调用）。
    pub fn fan_out(&self, msg: PluginNotification) {
        self.fan.fan_out(msg);
    }

    /// 请求帧组装：`{"jsonrpc":"2.0","id":N,"method":M,"params":...}`；
    /// `params` 为空对象时省略（与 protocol-v1.md §3.5 示例一致）。
    fn build_request_frame(id: u64, method: &str, params: &serde_json::Value) -> String {
        let mut map = serde_json::Map::new();
        map.insert(
            "jsonrpc".to_string(),
            serde_json::Value::String("2.0".into()),
        );
        map.insert("id".to_string(), serde_json::json!(id));
        map.insert(
            "method".to_string(),
            serde_json::Value::String(method.into()),
        );
        let empty = params.as_object().map(|m| m.is_empty()).unwrap_or(true);
        if !empty {
            map.insert("params".to_string(), params.clone());
        }
        let mut frame = serde_json::Value::Object(map).to_string();
        frame.push('\n');
        frame
    }
}

/// 读泵：逐帧读取 stdout → 按 id 路由响应 / 按 method 分流 notification（§4.3/§4.4）。
///
/// 响应找不到对应 pending（迟到响应 / 插件自造 id）→ 记日志丢弃；
/// 无 id 无 method 的帧 → 记日志忽略。返回 `Err` 表示读泵结束（帧错误 /
/// 协议致命错 / EOF），会话据此走终止流程。
pub async fn run_read_loop<R, H>(
    chan: &RpcChannel,
    reader: R,
    mut handler: H,
) -> Result<(), ReadLoopError>
where
    R: AsyncRead + Unpin,
    H: NotificationHandler,
{
    let mut frames = FrameReader::new(reader);
    loop {
        let frame = match frames.next_frame().await {
            Ok(frame) => frame,
            Err(e) => match handler.on_frame_error(e) {
                FrameDisposition::Continue => continue,
                FrameDisposition::Stop => return Err(ReadLoopError::Frame(e)),
            },
        };

        if let Some(id) = frame.get("id").and_then(|v| v.as_u64()) {
            let outcome = if let Some(err) = frame.get("error") {
                RpcOutcome::Error {
                    code: err.get("code").and_then(|c| c.as_i64()).unwrap_or(-32_603) as i32,
                    message: err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string(),
                    data: err.get("data").cloned(),
                }
            } else {
                RpcOutcome::Result(
                    frame
                        .get("result")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                )
            };
            let delivered = chan
                .pending
                .lock()
                .expect("pending lock poisoned")
                .remove(&id)
                .map(|call| call.tx.send(outcome));
            if delivered.is_none() {
                eprintln!(
                    "WARN ab-host: response for unknown id {id} dropped (late or plugin-invented)"
                );
            }
            continue;
        }

        if let Some(method) = frame.get("method").and_then(|m| m.as_str()) {
            let params = frame
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            handler
                .on_notification(method, &params)
                .map_err(ReadLoopError::Fatal)?;
            continue;
        }

        eprintln!("WARN ab-host: frame without id and method ignored");
    }
}

/// `RpcOutcome` → 类型化结果（§8.1 错误映射的公共入口）。
pub fn from_outcome<T: DeserializeOwned>(outcome: RpcOutcome) -> Result<T, HostError> {
    match outcome {
        RpcOutcome::Result(v) => serde_json::from_value(v)
            .map_err(|e| HostError::Transport(format!("response decode failed: {e}"))),
        RpcOutcome::Error {
            code,
            message,
            data,
        } => Err(HostError::Protocol {
            code,
            message,
            data,
        }),
        RpcOutcome::TransportError(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ab_protocol::types::Record;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn seq_validator_accepts_sequence_and_rejects_gaps_and_duplicates() {
        let batch = |seq: u64| RecordBatch {
            file_id: "f1".into(),
            seq,
            records: Vec::<Record>::new(),
            done: false,
        };
        let mut v = SeqValidator::new();
        assert!(v.accept(&batch(0)).is_ok(), "first batch seq 0");
        assert!(v.accept(&batch(1)).is_ok());
        let err = v.accept(&batch(3)).expect_err("gap 0,1,3 must be rejected");
        assert!(err.contains("expected 2, got 3"), "{err}");
        let err = v.accept(&batch(1)).expect_err("duplicate seq rejected");
        assert!(err.contains("expected 2, got 1"), "{err}");
        v.reset("f1");
        assert!(v.accept(&batch(0)).is_ok(), "seq restarts per parse");
        // 不同 file 互不干扰。
        let mut b = batch(0);
        b.file_id = "f2".into();
        assert!(v.accept(&b).is_ok());
    }

    #[test]
    fn request_frame_omits_empty_params_and_appends_lf() {
        // JSON 键序非契约；逐字段语义校验。
        let f = RpcChannel::build_request_frame(1, "schema", &serde_json::json!({}));
        let v: serde_json::Value = serde_json::from_str(&f).expect("valid JSON frame");
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], serde_json::json!(1));
        assert_eq!(v["method"], "schema");
        assert!(
            v.get("params").is_none(),
            "empty params must be omitted (protocol-v1.md §3.5)"
        );
        assert!(f.ends_with('\n'), "frames are LF-terminated");

        let f =
            RpcChannel::build_request_frame(2, "load_file", &serde_json::json!({"file_id": "x"}));
        let v: serde_json::Value = serde_json::from_str(&f).expect("valid JSON frame");
        assert_eq!(v["params"]["file_id"], "x");
        assert!(f.ends_with('\n'));
    }

    #[tokio::test]
    async fn concurrent_calls_routed_to_their_own_oneshots_out_of_order() {
        let (tx, mut frames_rx) = mpsc::channel::<String>(16);
        let chan = Arc::new(RpcChannel::new(tx));
        let (mut writer, reader) = tokio::io::duplex(1 << 20);

        let pump_chan = chan.clone();
        let pump = tokio::spawn(async move {
            run_read_loop(&pump_chan, reader, RecordingHandler::default()).await
        });

        let call_a_chan = chan.clone();
        let call_a = tokio::spawn(async move {
            call_a_chan
                .call(
                    "alpha",
                    serde_json::json!({}),
                    std::time::Duration::from_secs(5),
                )
                .await
        });
        let call_b_chan = chan.clone();
        let call_b = tokio::spawn(async move {
            call_b_chan
                .call(
                    "beta",
                    serde_json::json!({}),
                    std::time::Duration::from_secs(5),
                )
                .await
        });

        // 等两个 pending 都登记后，注入乱序响应（id 2 先于 id 1）。
        while chan.pending_count() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let out_of_order = concat!(
            r#"{"jsonrpc":"2.0","id":2,"result":{"which":"beta"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":1,"result":{"which":"alpha"}}"#,
            "\n",
        );
        writer.write_all(out_of_order.as_bytes()).await.unwrap();

        let oa = call_a.await.unwrap().unwrap();
        let ob = call_b.await.unwrap().unwrap();
        assert_eq!(
            oa,
            RpcOutcome::Result(serde_json::json!({"which": "alpha"}))
        );
        assert_eq!(ob, RpcOutcome::Result(serde_json::json!({"which": "beta"})));

        // 写侧帧：id 单调 1,2，method 各自对应。
        let fa = frames_rx.recv().await.unwrap();
        let fb = frames_rx.recv().await.unwrap();
        assert!(fa.contains(r#""id":1"#) && fa.contains(r#""method":"alpha""#));
        assert!(fb.contains(r#""id":2"#) && fb.contains(r#""method":"beta""#));

        // 读泵任务在测试结束时随 runtime 一并中止（duplex 无 EOF）。
        let _ = pump;
    }

    #[tokio::test]
    async fn pump_dispatches_notifications_and_ignores_unknown_methods() {
        let (tx, _frames_rx) = mpsc::channel::<String>(16);
        let chan = Arc::new(RpcChannel::new(tx));
        let frames = concat!(
            r#"{"jsonrpc":"2.0","method":"unknown_method","params":{"x":1}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"progress","params":{"file_id":"f1","records_so_far":0}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":99,"result":{}}"#,
            "\n",
        );
        let handler = RecordingHandler::default();
        let reader = std::io::Cursor::new(frames.as_bytes());

        // 未知 method + progress 均不应中断读泵；未知 id 响应被丢弃。
        let result = run_read_loop(&chan, reader, handler).await;
        // 读泵结束原因应为 Eof（而非 Fatal），说明未知 method 未破坏会话。
        assert!(
            matches!(result, Err(ReadLoopError::Frame(FrameError::Eof))),
            "unknown-method notifications must not terminate the loop: {result:?}"
        );
    }

    #[tokio::test]
    async fn drain_pending_completes_inflight_with_transport_error() {
        let (tx, _frames_rx) = mpsc::channel::<String>(16);
        let chan = Arc::new(RpcChannel::new(tx));
        let (writer, reader) = tokio::io::duplex(1 << 20);
        let pump_chan = chan.clone();
        let pump = tokio::spawn(async move {
            run_read_loop(&pump_chan, reader, RecordingHandler::default()).await
        });

        let call_chan = chan.clone();
        let call = tokio::spawn(async move {
            call_chan
                .call(
                    "x",
                    serde_json::json!({}),
                    std::time::Duration::from_secs(10),
                )
                .await
        });
        while chan.pending_count() < 1 {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let err = HostError::Transport("plugin process exited".to_string());
        chan.drain_pending(err.clone());

        // 在途请求以 Ok(RpcOutcome::TransportError) 完成（§4.3，调用方经 from_outcome 转 Err）。
        let outcome = call.await.unwrap().unwrap();
        assert!(
            matches!(&outcome, RpcOutcome::TransportError(e) if *e == err),
            "inflight call completed with transport error: {outcome:?}"
        );
        // 通道已死：后续调用快速失败。
        let late = chan
            .call(
                "y",
                serde_json::json!({}),
                std::time::Duration::from_secs(1),
            )
            .await;
        assert!(late.is_err());
        let _ = writer;
        let _ = pump;
    }

    #[derive(Default)]
    struct RecordingHandler {
        methods: Arc<Mutex<Vec<String>>>,
    }

    impl NotificationHandler for RecordingHandler {
        fn on_notification(
            &mut self,
            method: &str,
            _params: &serde_json::Value,
        ) -> Result<(), String> {
            self.methods
                .lock()
                .expect("recorder lock")
                .push(method.to_string());
            Ok(())
        }
    }
}
