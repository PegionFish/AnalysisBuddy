//! 迷你宿主插件会话（qa-perf.md §3.1 / protocol-v1.md §5、§6）：
//! 进程拉起、JSON-RPC over stdio、方法级看门狗、parse 心跳看门狗、会话状态机、
//! stderr 环形缓冲（protocol §9.3 同款）与进程回收（含 panic 路径，无孤儿进程）。

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ab_protocol::types::{
    CanHandleParams, CancelParseParams, FileSummary, KeyValuesParams, KeyValuesResult,
    LoadFileParams, ParseResult, Record, RecordBatch, SchemaResult, UnloadFileParams,
};
use serde_json::{json, Value};

use super::frames::{FrameError, FrameReader, StderrRing};
use super::store::Store;

/// 方法级看门狗（protocol-v1.md §6）：initialize 5s / can_handle·schema 3s /
/// load_file·key_values·cancel_parse 10s / unload_file 3s / shutdown 3s。
pub const TIMEOUT_INITIALIZE: Duration = Duration::from_secs(5);
pub const TIMEOUT_CAN_HANDLE: Duration = Duration::from_secs(3);
pub const TIMEOUT_LOAD: Duration = Duration::from_secs(10);
pub const TIMEOUT_SCHEMA: Duration = Duration::from_secs(3);
pub const TIMEOUT_KEY_VALUES: Duration = Duration::from_secs(10);
pub const TIMEOUT_CANCEL: Duration = Duration::from_secs(10);
pub const TIMEOUT_UNLOAD: Duration = Duration::from_secs(3);
pub const TIMEOUT_SHUTDOWN: Duration = Duration::from_secs(3);
/// parse 心跳看门狗：30s 无 progress/RecordBatch 即判死（protocol §3.3）。
pub const WATCHDOG_PARSE: Duration = Duration::from_secs(30);
/// shutdown 后进程未在期限内退出的收割宽限。
pub const KILL_GRACE: Duration = Duration::from_secs(3);

/// 会话状态（protocol §5.1 状态图子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Discovered,
    Initializing,
    Ready,
    Loading,
    Parsing,
    Draining,
    Shutdown,
    Crashed,
    Timeout,
    /// 致命协议错误（seq 缺号/重复、超 8MB 帧）→ 会话终止。
    Terminated,
}

/// 宿主侧错误（供 UI 呈现与断言）。
#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    /// 插件返回的 JSON-RPC error。
    Rpc { code: i32, message: String },
    /// 看门狗超时（进程已 kill，状态 → Timeout）。
    Timeout,
    /// 进程意外退出（状态 → Crashed）。
    ProcessDied(Option<i32>),
    /// 致命协议违规（seq 缺号、帧超限等；状态 → Terminated）。
    ProtocolViolation(String),
    /// IO / 内部错误。
    Io(String),
}

impl HostError {
    pub fn message(&self) -> String {
        match self {
            HostError::Rpc { code, message } => format!("rpc error {code}: {message}"),
            HostError::Timeout => "timeout".to_string(),
            HostError::ProcessDied(c) => format!("process died (exit {c:?})"),
            HostError::ProtocolViolation(m) => format!("protocol violation: {m}"),
            HostError::Io(m) => m.clone(),
        }
    }
}

/// parse 结果（PERF-01 口径：load_file 发出 → records_total 到达）。
#[derive(Debug, Clone)]
pub struct ParseOutcome {
    pub records_total: u64,
    pub batches: u64,
    pub sum_records: u64,
    pub done_seen: bool,
    pub elapsed: Duration,
}

/// 插件进程调用方式。
#[derive(Debug, Clone)]
pub struct PluginInvocation {
    pub exe: std::path::PathBuf,
    pub args: Vec<String>,
    /// 进程工作目录（protocol.md §7.2）；`None` = 继承宿主 cwd。
    /// 真实插件调用层按 §7.2 传 plugin.json 所在目录（manifest entry 相对其解析）。
    pub working_dir: Option<std::path::PathBuf>,
}

/// 文件条目状态（load_failed 用例：置灰 + 可重试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEntryState {
    NotLoaded,
    Loading,
    Loaded,
    LoadFailed,
    Parsing,
    Parsed,
}

/// 泵帧期间的动作（由 on_frame 回调决定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpAction {
    Continue,
    /// 立即 kill 进程（崩溃模拟）。
    KillCrashed,
    /// 协议违规终止（kill + Terminated）。
    KillTerminated,
}

/// 迷你宿主会话。
pub struct PluginSession {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: std::sync::mpsc::Receiver<Result<String, FrameError>>,
    stderr_ring: Arc<Mutex<StderrRing>>,
    stderr_handle: Option<JoinHandle<()>>,
    reader_handle: Option<JoinHandle<()>>,
    next_id: u64,
    state: SessionState,
    /// 最近一次宿主侧错误消息（UI 报错断言用）。
    last_error: Option<String>,
    files: HashMap<String, FileEntryState>,
    /// 全文件共享的查询存储（多文件同轴叠加）。
    pub store: Store,
}

impl PluginSession {
    pub fn spawn(invocation: &PluginInvocation, stderr_cap: usize) -> Result<Self, String> {
        let mut cmd = Command::new(&invocation.exe);
        cmd.args(&invocation.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(wd) = &invocation.working_dir {
            cmd.current_dir(wd);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", invocation.exe.display()))?;

        let stdout = child.stdout.take().ok_or("stdout pipe missing")?;
        let stderr = child.stderr.take().ok_or("stderr pipe missing")?;
        let stdin = child.stdin.take().ok_or("stdin pipe missing")?;

        let (tx, rx) = std::sync::mpsc::channel();
        let reader_handle = std::thread::spawn(move || {
            let mut fr = FrameReader::new(stdout);
            loop {
                match fr.next_frame() {
                    Ok(frame) => {
                        if tx.send(Ok(frame)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });

        let ring = Arc::new(Mutex::new(StderrRing::new(stderr_cap)));
        let ring_t = ring.clone();
        let stderr_handle = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(l) => ring_t.lock().unwrap().push(format!("{l}\n").as_bytes()),
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            child,
            stdin: Some(stdin),
            rx,
            stderr_ring: ring,
            stderr_handle: Some(stderr_handle),
            reader_handle: Some(reader_handle),
            next_id: 1,
            state: SessionState::Discovered,
            last_error: None,
            files: HashMap::new(),
            store: Store::new(),
        })
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    /// 最近一次宿主侧错误（UI 报错断言）。
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn file_state(&self, file_id: &str) -> FileEntryState {
        self.files
            .get(file_id)
            .copied()
            .unwrap_or(FileEntryState::NotLoaded)
    }

    // ------------------------------------------------------------------
    // RPC 底层
    // ------------------------------------------------------------------

    fn send(&mut self, method: &str, params: Value) -> Result<Value, HostError> {
        let id = json!(self.next_id);
        self.next_id += 1;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let line = serde_json::to_string(&frame)
            .map_err(|e| HostError::Io(format!("serialize request: {e}")))?;
        let mut line_bytes = line.into_bytes();
        line_bytes.push(b'\n');
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| HostError::Io("stdin closed".to_string()))?;
        stdin
            .write_all(&line_bytes)
            .and_then(|()| stdin.flush())
            .map_err(|_| HostError::ProcessDied(self.try_exit_code()))?;
        Ok(id)
    }

    /// 当前进程退出码（进程已退出时）；未退出返回 None。
    fn try_exit_code(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => status.code(),
            _ => None,
        }
    }

    /// 泵帧直到响应到达或看门狗到期；`on_frame(self, frame)` 处理通知帧。
    /// 回调内可直接修改会话（kill / 存储），避免闭包与 `&mut self` 双借用。
    fn pump(
        &mut self,
        id: Value,
        timeout: Duration,
        mut on_frame: impl FnMut(&mut Self, Value) -> Result<PumpAction, HostError>,
    ) -> Result<Value, HostError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill(SessionState::Timeout);
                self.last_error = Some("timeout".to_string());
                return Err(HostError::Timeout);
            }
            match self.rx.recv_timeout(remaining) {
                Ok(Ok(frame)) => {
                    let v: Value = serde_json::from_str(&frame).map_err(|e| {
                        HostError::ProtocolViolation(format!("invalid JSON frame: {e}"))
                    })?;
                    if v.get("id") == Some(&id) {
                        return Ok(v);
                    }
                    if v.get("id").is_none() {
                        match on_frame(self, v)? {
                            PumpAction::Continue => {}
                            PumpAction::KillCrashed => self.kill(SessionState::Crashed),
                            PumpAction::KillTerminated => self.kill(SessionState::Terminated),
                        }
                    }
                }
                Ok(Err(FrameError::Eof)) => {
                    let code = self.child.wait().ok().and_then(|s| s.code());
                    self.state = SessionState::Crashed;
                    self.last_error = Some(format!("process exited (code {code:?})"));
                    return Err(HostError::ProcessDied(code));
                }
                Ok(Err(FrameError::LineExceedsLimit(len))) => {
                    self.kill(SessionState::Terminated);
                    let msg = format!("line exceeds 8MB limit (len {len})");
                    self.last_error = Some(msg.clone());
                    return Err(HostError::ProtocolViolation(msg));
                }
                Ok(Err(FrameError::Io(e))) => {
                    self.kill(SessionState::Crashed);
                    return Err(HostError::Io(e));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // 看门狗到期（上方 deadline 检查的双保险路径）。
                    self.kill(SessionState::Timeout);
                    self.last_error = Some("timeout".to_string());
                    return Err(HostError::Timeout);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // reader 线程退出（管道关闭）= 进程已死。
                    self.kill(SessionState::Crashed);
                    return Err(HostError::ProcessDied(self.try_exit_code()));
                }
            }
        }
    }

    /// 解释响应帧：result 或 error。
    fn interpret(resp: Value) -> Result<Value, HostError> {
        if let Some(err) = resp.get("error") {
            return Err(HostError::Rpc {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0) as i32,
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
        resp.get("result").cloned().ok_or_else(|| {
            HostError::ProtocolViolation("response without result/error".to_string())
        })
    }

    // ------------------------------------------------------------------
    // 协议方法（qa-perf.md §3.1 流程：initialize→can_handle→load_file→parse→…）
    // ------------------------------------------------------------------

    pub fn initialize(&mut self, host_name: &str, host_version: &str) -> Result<Value, HostError> {
        self.state = SessionState::Initializing;
        let id = self.send(
            "initialize",
            json!({
                "protocol_version": 1,
                "host_info": {"name": host_name, "version": host_version},
            }),
        )?;
        let resp = self.pump(id, TIMEOUT_INITIALIZE, |_, _| Ok(PumpAction::Continue))?;
        let result = Self::interpret(resp)?;
        self.state = SessionState::Ready;
        Ok(result)
    }

    pub fn can_handle(
        &mut self,
        params: &CanHandleParams,
    ) -> Result<ab_protocol::types::CanHandleResult, HostError> {
        let id = self.send(
            "can_handle",
            serde_json::to_value(params).map_err(|e| HostError::Io(e.to_string()))?,
        )?;
        let resp = self.pump(id, TIMEOUT_CAN_HANDLE, |_, _| Ok(PumpAction::Continue))?;
        let result = Self::interpret(resp)?;
        serde_json::from_value(result)
            .map_err(|e| HostError::Io(format!("bad can_handle result: {e}")))
    }

    pub fn schema(&mut self) -> Result<SchemaResult, HostError> {
        let id = self.send("schema", json!({}))?;
        let resp = self.pump(id, TIMEOUT_SCHEMA, |_, _| Ok(PumpAction::Continue))?;
        let result = Self::interpret(resp)?;
        serde_json::from_value(result).map_err(|e| HostError::Io(format!("bad schema result: {e}")))
    }

    pub fn load_file(&mut self, file_id: &str, path: &Path) -> Result<FileSummary, HostError> {
        self.state = SessionState::Loading;
        let id = self.send(
            "load_file",
            serde_json::to_value(LoadFileParams {
                file_id: file_id.to_string(),
                path: path.to_string_lossy().into_owned(),
            })
            .map_err(|e| HostError::Io(e.to_string()))?,
        )?;
        let resp = self.pump(id, TIMEOUT_LOAD, |_, _| Ok(PumpAction::Continue))?;
        let result = match Self::interpret(resp) {
            Ok(r) => r,
            Err(HostError::Rpc { code, message }) => {
                self.files
                    .insert(file_id.to_string(), FileEntryState::LoadFailed);
                self.state = SessionState::Ready;
                self.last_error = Some(format!("{code}: {message}"));
                return Err(HostError::Rpc { code, message });
            }
            Err(e) => return Err(e),
        };
        let summary: FileSummary = serde_json::from_value(result)
            .map_err(|e| HostError::Io(format!("bad load_file result: {e}")))?;
        self.files
            .insert(file_id.to_string(), FileEntryState::Loaded);
        self.state = SessionState::Ready;
        Ok(summary)
    }

    /// 流式 parse：心跳看门狗 30s；seq 缺号/重复 → 终止会话并 kill 进程。
    /// 失败（-32003/-32004/超时/崩溃）时已收批次全丢弃（protocol §3.3/§3.4）。
    pub fn parse(&mut self, file_id: &str) -> Result<ParseOutcome, HostError> {
        self.parse_with_hook(file_id, None)
    }

    /// `hook`：每个 RecordBatch 通知落库前调用；返回 true 时宿主立即模拟进程
    /// 崩溃（kill）——crash_retry 用例专用（回放器在 parse 中途退出进程）。
    pub fn parse_with_hook(
        &mut self,
        file_id: &str,
        mut hook: Option<&mut dyn FnMut(&RecordBatch) -> bool>,
    ) -> Result<ParseOutcome, HostError> {
        self.state = SessionState::Parsing;
        self.files
            .insert(file_id.to_string(), FileEntryState::Parsing);
        let started = Instant::now();
        let id = self.send("parse", json!({"file_id": file_id}))?;

        let mut expected_seq = 0u64;
        let mut sum = 0u64;
        let mut batches = 0u64;
        let mut done_seen = false;
        let mut watchdog = Instant::now() + WATCHDOG_PARSE;
        let mut staged: Vec<Record> = Vec::new();

        let pump_result = self.pump(id, WATCHDOG_PARSE, |_s, v| {
            match v.get("method").and_then(Value::as_str) {
                Some("progress") => {
                    watchdog = Instant::now() + WATCHDOG_PARSE;
                    Ok(PumpAction::Continue)
                }
                Some("RecordBatch") => {
                    let batch: RecordBatch =
                        serde_json::from_value(v.get("params").cloned().unwrap_or(Value::Null))
                            .map_err(|e| {
                                HostError::ProtocolViolation(format!("bad RecordBatch: {e}"))
                            })?;
                    if batch.seq != expected_seq {
                        let msg = format!(
                            "seq gap or duplicate: expected {expected_seq}, got {}",
                            batch.seq
                        );
                        return Err(HostError::ProtocolViolation(msg));
                    }
                    expected_seq += 1;
                    sum += batch.records.len() as u64;
                    batches += 1;
                    done_seen = done_seen || batch.done;
                    let crash = hook.as_mut().map(|h| h(&batch)).unwrap_or(false);
                    staged.extend(batch.records);
                    watchdog = Instant::now() + WATCHDOG_PARSE;
                    if crash {
                        Ok(PumpAction::KillCrashed)
                    } else {
                        Ok(PumpAction::Continue)
                    }
                }
                _ => Ok(PumpAction::Continue),
            }
        });

        let resp = match pump_result {
            Ok(r) => r,
            Err(HostError::ProtocolViolation(msg)) => {
                self.kill(SessionState::Terminated);
                self.last_error = Some(msg.clone());
                return Err(HostError::ProtocolViolation(msg));
            }
            Err(e) => {
                // 失败路径：已收批次全丢弃，存储无残留。
                staged.clear();
                return Err(e);
            }
        };
        let result = match Self::interpret(resp) {
            Ok(r) => r,
            Err(e) => {
                staged.clear();
                self.files
                    .insert(file_id.to_string(), FileEntryState::Loaded);
                self.state = SessionState::Ready;
                return Err(e);
            }
        };
        let pr: ParseResult = serde_json::from_value(result)
            .map_err(|e| HostError::Io(format!("bad parse result: {e}")))?;
        if pr.records_total != sum || !done_seen {
            let msg = format!(
                "records_total mismatch: plugin {pr:?} vs batch sum {sum} (done_seen {done_seen})"
            );
            self.last_error = Some(msg.clone());
            staged.clear();
            self.files
                .insert(file_id.to_string(), FileEntryState::Loaded);
            self.state = SessionState::Ready;
            return Err(HostError::ProtocolViolation(msg));
        }
        self.store.insert_batch(staged);
        self.state = SessionState::Ready;
        self.files
            .insert(file_id.to_string(), FileEntryState::Parsed);
        Ok(ParseOutcome {
            records_total: pr.records_total,
            batches,
            sum_records: sum,
            done_seen,
            elapsed: started.elapsed(),
        })
    }

    pub fn cancel_parse(&mut self, file_id: &str) -> Result<(), HostError> {
        let id = self.send(
            "cancel_parse",
            serde_json::to_value(CancelParseParams {
                file_id: file_id.to_string(),
            })
            .map_err(|e| HostError::Io(e.to_string()))?,
        )?;
        let resp = self.pump(id, TIMEOUT_CANCEL, |_, _| Ok(PumpAction::Continue))?;
        Self::interpret(resp)?;
        Ok(())
    }

    pub fn key_values(
        &mut self,
        file_id: &str,
        timestamp_ms: i64,
    ) -> Result<KeyValuesResult, HostError> {
        let id = self.send(
            "key_values",
            serde_json::to_value(KeyValuesParams {
                file_id: file_id.to_string(),
                timestamp_ms,
            })
            .map_err(|e| HostError::Io(e.to_string()))?,
        )?;
        let resp = self.pump(id, TIMEOUT_KEY_VALUES, |_, _| Ok(PumpAction::Continue))?;
        let result = Self::interpret(resp)?;
        serde_json::from_value(result)
            .map_err(|e| HostError::Io(format!("bad key_values result: {e}")))
    }

    pub fn unload_file(&mut self, file_id: &str) -> Result<(), HostError> {
        let id = self.send(
            "unload_file",
            serde_json::to_value(UnloadFileParams {
                file_id: file_id.to_string(),
            })
            .map_err(|e| HostError::Io(e.to_string()))?,
        )?;
        let resp = self.pump(id, TIMEOUT_UNLOAD, |_, _| Ok(PumpAction::Continue))?;
        Self::interpret(resp)?;
        self.files.remove(file_id);
        Ok(())
    }

    /// 优雅停机：shutdown → 等退出 ≤3s → 超时 kill（protocol §2.9/§5.2）。
    pub fn shutdown(&mut self) -> Result<(), HostError> {
        self.state = SessionState::Draining;
        if self.stdin.is_some() {
            let id = self.send("shutdown", json!({}))?;
            let resp = self.pump(id, TIMEOUT_SHUTDOWN, |_, _| Ok(PumpAction::Continue));
            if let Ok(resp) = resp {
                let _ = Self::interpret(resp);
            }
        }
        let deadline = Instant::now() + KILL_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        self.state = SessionState::Shutdown;
        self.close_pipes();
        Ok(())
    }

    /// 强杀进程（崩溃模拟 / 超时处置）。
    pub fn kill(&mut self, state: SessionState) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.state = state;
        self.close_pipes();
    }

    /// 进程是否还活着。
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    fn close_pipes(&mut self) {
        self.stdin = None;
    }

    /// stderr 环形缓冲转储到文件（断言失败定位用）。
    pub fn dump_stderr(&self, path: &Path) {
        let ring = self.stderr_ring.lock().unwrap();
        let _ = ring.dump_to(path);
    }

    pub fn stderr_text(&self) -> String {
        self.stderr_ring.lock().unwrap().as_text()
    }
}

/// Drop 守卫：任何离开路径（含 panic 展开）都收割进程并回收管道线程，
/// 保证 harness 结束无残留子进程（F-02 DoD）。
impl Drop for PluginSession {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.stdin = None;
        if let Some(h) = self.reader_handle.take() {
            let _ = h.join();
        }
        if let Some(h) = self.stderr_handle.take() {
            let _ = h.join();
        }
    }
}

/// 断言失败时的标准转储（写插件 stderr + 会话错误到测试产物）。
pub fn dump_on_failure(test_name: &str, session: Option<&PluginSession>, extra: &str) {
    let dir = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("target")
        })
        .join("test-artifacts")
        .join("e2e");
    let path = dir.join(format!("{test_name}.stderr.log"));
    if let Some(s) = session {
        s.dump_stderr(&path);
    }
    let note = dir.join(format!("{test_name}.err.txt"));
    let _ = std::fs::write(&note, extra);
    eprintln!(
        "[e2e] failure artifacts: {}, {}",
        path.display(),
        note.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_have_no_partial_eq_surprises() {
        assert_eq!(SessionState::Ready, SessionState::Ready);
        assert_ne!(SessionState::Crashed, SessionState::Timeout);
    }
}
