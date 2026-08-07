//! Phase 2 行为回放校验：BEH-01 ~ BEH-12（docs-validator.md §2.2/§3.1/§3.5）。
//!
//! 请求顺序固定：initialize → schema → can_handle → load_file → load_file（BEH-11
//! 幂等探测）→ parse → key_values → unload_file → shutdown；`id` 依次 1~9
//! （docs-validator.md §3.5 基序 id 1~8，BEH-11 探测插入 load 阶段使序列扩展为 9 步），
//! `file_id` 固定 UUID（可复现定位）。can_handle 不认领 fixture 时跳过 load/parse/
//! key_values 并以 warning 提示换 `--fixture`。结束必杀进程；stderr 只记录不判定
//! （protocol-v1.md §1.1）。
//!
//! 帧级结构校验只经 `docs/spec/rpc-messages.schema.json`（单源，docs-validator.md
//! §3.2）：Schema 失败折算为对应 BEH 规则 error（折算表见 [`Session::schema_failure_rule`]）。
//!
//! 规则映射决策（超出冻结规则文本的直接归属，冻结集内无更贴切编号）：
//! - `initialize` 任何错误响应（含 -32601 以外）→ BEH-01（握手失败，宿主终止会话）；
//! - `schema` 任何错误响应 / 无响应 → BEH-03（必选方法行为不合规；宿主禁用指标树）；
//! - `schema` 响应结构无效 → BEH-03；`parse` 响应结构/records_total → BEH-06；
//! - `can_handle` 响应结构无效 / 置信度越界 → BEH-01（docs-validator.md §3.5）；
//! - `load_file`/`parse` 的 `-32002`/`-32003`/`-32004` 为合法失败路径，不判违规；
//! - `-32601`（必选方法）与非标准错误码 → BEH-03；其余错误响应不判违规。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ab_protocol::manifest::Manifest;
use jsonschema::{ValidationError, Validator as JsonSchemaValidator};
use serde_json::{json, Value};

use crate::harness::{classify, FrameLine, LineKind, LineReader, Watchdog};
use crate::rules::Finding;

/// 固定 `file_id`（docs-validator.md §3.3：可复现报错定位）。
const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

/// 行为回放输入。
pub struct BehaviorInput<'a> {
    pub plugin_dir: std::path::PathBuf,
    pub manifest: Manifest,
    /// 已解析为绝对路径的 fixture（can_handle/load_file 传参用）。
    pub fixture: std::path::PathBuf,
    pub scale: f64,
    pub rpc_schema: &'a JsonSchemaValidator,
}

/// 行为回放结果。
pub struct BehaviorOutcome {
    pub findings: Vec<Finding>,
    pub stderr_dump: Option<String>,
    /// 中止点（致命协议错误时 Some(阶段名)）。
    pub aborted_at: Option<String>,
    pub notes: Vec<String>,
}

/// 行事件（stdout reader 线程 → 主线程）。
enum LineEvent {
    Line(FrameLine),
    Eof,
}

/// 等待请求响应的结局。
enum WaitOutcome {
    Response(Value),
    Timeout,
    ProcessEnded,
}

struct Session<'a> {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<LineEvent>,
    stderr_path: std::path::PathBuf,
    watchdog: Watchdog,
    schema: &'a JsonSchemaValidator,
    findings: Vec<Finding>,
    notes: Vec<String>,
    /// 在途请求：id → method（BEH-02 关联）。
    pending: HashMap<u64, String>,
    manifest_id: String,
    metrics_valid: bool,
    metrics: HashSet<String>,
    progress_seen: bool,
    last_activity: Option<Instant>,
    parse_expected_seq: Option<u64>,
    parse_done_seen: bool,
    parse_record_sum: u64,
    fatal: bool,
    aborted_at: Option<String>,
}

/// 行为回放入口。`Err` = 校验器自身故障（插件进程无法拉起且非插件原因 → 退出码 4）。
pub fn run(input: &BehaviorInput) -> Result<BehaviorOutcome, String> {
    let mut session =
        Session::spawn(input).map_err(|e| format!("无法拉起插件进程（非插件原因）：{e}"))?;
    session.drive(input);
    let outcome = session.finish();
    Ok(outcome)
}

impl<'a> Session<'a> {
    fn spawn(input: &BehaviorInput<'a>) -> io::Result<Session<'a>> {
        let working_dir = match &input.manifest.entry.working_dir {
            Some(w) => input.plugin_dir.join(w),
            None => input.plugin_dir.clone(),
        };
        let mut cmd = Command::new(&input.manifest.entry.command);
        cmd.args(&input.manifest.entry.args)
            .current_dir(&working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdin = child.stdin.take();

        // stderr 实时转储到临时目录（docs-validator.md §1.2：结束时打印路径；只记录不判定）
        let stderr_path = std::env::temp_dir().join(format!(
            "plugin-check-{}-{}.stderr.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let stderr_file = fs::File::create(&stderr_path)?;
        std::thread::spawn(move || {
            let mut src = BufReader::new(stderr);
            let mut dst = stderr_file;
            let _ = io::copy(&mut src, &mut dst);
        });

        // stdout NDJSON reader（8MB 先行校验，BEH-08 依赖此实现）
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = LineReader::new(stdout);
            let mut no: u64 = 0;
            loop {
                match reader.next_line() {
                    Ok(Some(raw)) => {
                        no += 1;
                        let mut fl = classify(&raw);
                        fl.no = no;
                        if tx.send(LineEvent::Line(fl)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            let _ = tx.send(LineEvent::Eof);
        });

        Ok(Session {
            child,
            stdin,
            frames: rx,
            stderr_path,
            watchdog: Watchdog::new(input.scale),
            schema: input.rpc_schema,
            findings: Vec::new(),
            notes: Vec::new(),
            pending: HashMap::new(),
            manifest_id: input.manifest.id.clone(),
            metrics_valid: false,
            metrics: HashSet::new(),
            progress_seen: false,
            last_activity: None,
            parse_expected_seq: None,
            parse_done_seen: false,
            parse_record_sum: 0,
            fatal: false,
            aborted_at: None,
        })
    }

    // -----------------------------------------------------------------------
    // 主流程（docs-validator.md §3.1/§3.5）
    // -----------------------------------------------------------------------

    fn drive(&mut self, input: &BehaviorInput) {
        let fixture = FixtureInfo::from_path(&input.fixture);

        // ① initialize（5s × scale）
        self.send_request(
            1,
            "initialize",
            json!({"protocol_version": ab_protocol::PROTOCOL_VERSION,
                   "host_info": {"name": "plugin-check", "version": "1"}}),
        );
        match self.wait_for(
            1,
            "initialize",
            self.watchdog.deadline(Duration::from_secs(5)),
            false,
        ) {
            WaitOutcome::Response(v) => self.handle_initialize_response(&v),
            _ => {
                self.findings.push(Finding::error(
                    "BEH-01",
                    "initialize 无响应或进程提前退出（握手失败；宿主标记 Crashed/Timeout，protocol-v1.md §5.1）",
                    "initialize（id=1）",
                ));
                self.fatal = true;
            }
        }
        if self.fatal {
            self.abort("initialize");
            return;
        }

        // ② schema（3s × scale）
        self.send_request(2, "schema", json!({}));
        match self.wait_for(
            2,
            "schema",
            self.watchdog.deadline(Duration::from_secs(3)),
            false,
        ) {
            WaitOutcome::Response(v) => self.handle_schema_response(&v),
            _ => {
                self.findings.push(Finding::error(
                    "BEH-03",
                    "schema 无响应（必选方法；宿主重试后禁用指标树）",
                    "schema（id=2）",
                ));
                self.fatal = true;
            }
        }
        if self.fatal {
            self.abort("schema");
            return;
        }

        // ③ can_handle（3s × scale）
        self.send_request(3, "can_handle", fixture.can_handle_params());
        let claimed = match self.wait_for(
            3,
            "can_handle",
            self.watchdog.deadline(Duration::from_secs(3)),
            false,
        ) {
            WaitOutcome::Response(v) => self.handle_can_handle_response(&v),
            WaitOutcome::Timeout | WaitOutcome::ProcessEnded => {
                self.notes.push(
                    "can_handle 无响应：按宿主语义视为弃权（protocol-v1.md §6），跳过 load/parse/key_values"
                        .to_string(),
                );
                false
            }
        };
        if self.fatal {
            self.abort("can_handle");
            return;
        }
        if !claimed {
            self.notes.push(format!(
                "can_handle 未认领 fixture `{}`，跳过 load/parse/key_values；可用 --fixture 换用插件认领的日志",
                input
                    .fixture
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ));
            self.phase_shutdown();
            return;
        }

        // ④ load_file（10s × scale）
        let mut time_range: Option<(i64, i64)> = None;
        let mut load_failed = false;
        self.send_request(
            4,
            "load_file",
            json!({"file_id": FILE_ID, "path": fixture.absolute}),
        );
        match self.wait_for(
            4,
            "load_file",
            self.watchdog.deadline(Duration::from_secs(10)),
            false,
        ) {
            WaitOutcome::Response(v) => {
                if v.get("error").is_some() {
                    let code = v["error"]["code"].as_i64().unwrap_or(0);
                    if code == -32601 {
                        self.findings.push(Finding::error(
                            "BEH-03",
                            "必选方法 load_file 返回 -32601（协议要求实现）",
                            "load_file（id=4）响应",
                        ));
                        self.fatal = true;
                    } else {
                        self.notes.push(format!(
                            "load_file 返回错误码 {code}（宿主按 -32002 语义处理）；跳过 parse/key_values"
                        ));
                    }
                    load_failed = true;
                } else if let Some(tr) = extract_time_range(&v) {
                    time_range = Some(tr);
                }
            }
            _ => {
                self.notes.push(
                    "load_file 无响应（宿主视为文件加载失败）；跳过 parse/key_values".to_string(),
                );
                load_failed = true;
            }
        }
        if self.fatal {
            self.abort("load_file");
            return;
        }

        // ⑤ BEH-11 幂等探测：同一 file_id 二次 load（protocol-v1.md §9 第 2 条）
        self.send_request(
            5,
            "load_file",
            json!({"file_id": FILE_ID, "path": fixture.absolute}),
        );
        match self.wait_for(
            5,
            "load_file",
            self.watchdog.deadline(Duration::from_secs(10)),
            false,
        ) {
            WaitOutcome::Response(v) => {
                if v.get("error").is_some() {
                    self.findings.push(Finding::warn(
                        "BEH-11",
                        "同一 file_id 二次 load_file 返回错误（幂等重入失败；协议要求 reloading 同一 file_id 等价于 unload-then-load，protocol-v1.md §9 第 2 条）",
                        "load_file（id=5 幂等探测）响应",
                    ));
                }
            }
            _ => {
                self.findings.push(Finding::warn(
                    "BEH-11",
                    "同一 file_id 二次 load_file 无响应（幂等重入失败）",
                    "load_file（id=5 幂等探测）",
                ));
            }
        }
        if self.fatal {
            self.abort("load_file");
            return;
        }

        if load_failed {
            // load 失败路径：跳过 parse/key_values，直接收尾
            self.phase_shutdown();
            return;
        }

        // ⑥ parse（心跳看门狗 30s × scale，BEH-04/05/06/08/09）
        self.send_request(6, "parse", json!({"file_id": FILE_ID}));
        match self.wait_for(
            6,
            "parse",
            self.watchdog.deadline(Duration::from_secs(30)),
            true,
        ) {
            WaitOutcome::Response(v) => self.handle_parse_response(&v),
            _ => {
                self.findings.push(Finding::error(
                    "BEH-04",
                    "parse 期间超过协议心跳上限（30s × scale）无任何 progress/RecordBatch，或进程提前退出（心跳看门狗；protocol-v1.md §3.3/§6）",
                    "parse（id=6）",
                ));
                self.fatal = true;
            }
        }
        if self.fatal {
            self.abort("parse");
            return;
        }

        // ⑦ key_values（10s × scale；时间戳取 fixture time_range 中点）
        let mid = time_range.map(|(s, e)| s + (e - s) / 2).unwrap_or(0);
        self.send_request(
            7,
            "key_values",
            json!({"file_id": FILE_ID, "timestamp_ms": mid}),
        );
        match self.wait_for(
            7,
            "key_values",
            self.watchdog.deadline(Duration::from_secs(10)),
            false,
        ) {
            WaitOutcome::Response(v) => self.handle_key_values_response(&v),
            _ => {
                self.findings.push(Finding::error(
                    "BEH-07",
                    "key_values 超时无响应（>10s × scale 看门狗）或进程提前退出",
                    "key_values（id=7）",
                ));
                self.fatal = true;
            }
        }
        if self.fatal {
            self.abort("key_values");
            return;
        }

        // ⑧ unload_file（3s × scale；幂等，超时视为已卸载不判违规）
        self.send_request(8, "unload_file", json!({"file_id": FILE_ID}));
        let _ = self.wait_for(
            8,
            "unload_file",
            self.watchdog.deadline(Duration::from_secs(3)),
            false,
        );

        self.phase_shutdown();
    }

    /// 收尾：shutdown（3s×scale）→ 等退出 ≤3s×scale（BEH-10）→ 关 stdin 等 5s×scale
    /// （BEH-12）→ 必杀进程（docs-validator.md §3.4）。
    fn phase_shutdown(&mut self) {
        self.send_request(9, "shutdown", json!({}));
        let responded = matches!(
            self.wait_for(
                9,
                "shutdown",
                self.watchdog.deadline(Duration::from_secs(3)),
                false
            ),
            WaitOutcome::Response(_)
        );

        let exit_deadline = Instant::now() + self.watchdog.deadline(Duration::from_secs(3));
        let mut exited = false;
        let mut exit_code: Option<i32> = None;
        while Instant::now() < exit_deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                exited = true;
                exit_code = status.code();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !exited {
            self.findings.push(Finding::warn(
                "BEH-10",
                if responded {
                    "shutdown 响应后进程未在 3s × scale 内退出（宿主将 kill；protocol-v1.md §2.9）"
                } else {
                    "shutdown 无响应且进程未在 3s × scale 内退出（宿主将 kill）"
                },
                "shutdown（id=9）",
            ));
        } else if exit_code != Some(0) {
            self.findings.push(Finding::warn(
                "BEH-10",
                format!("shutdown 后进程退出码非 0（实际 {exit_code:?}；协议要求退出码 0）"),
                "shutdown（id=9）",
            ));
        }

        // 关 stdin → EOF → 等 5s×scale（BEH-12：EOF 未自退 = 孤儿进程风险）
        self.stdin.take();
        let eof_deadline = Instant::now() + self.watchdog.deadline(Duration::from_secs(5));
        let mut alive = true;
        while Instant::now() < eof_deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if alive {
            self.findings.push(Finding::warn(
                "BEH-12",
                "stdin EOF 后插件未自行退出（孤儿进程风险；协议要求 EOF → 退出码 0 退出，protocol-v1.md §9 第 5 条）",
                "stdin EOF",
            ));
        }
        self.kill_and_reap();
    }

    fn abort(&mut self, at: &str) {
        self.kill_and_reap();
        self.aborted_at = Some(at.to_string());
    }

    fn kill_and_reap(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn finish(mut self) -> BehaviorOutcome {
        self.kill_and_reap();
        Finding::sort_by_rule(&mut self.findings);
        BehaviorOutcome {
            findings: std::mem::take(&mut self.findings),
            stderr_dump: Some(self.stderr_path.display().to_string()),
            aborted_at: std::mem::take(&mut self.aborted_at),
            notes: std::mem::take(&mut self.notes),
        }
    }

    // -----------------------------------------------------------------------
    // 请求 / 等待 / 帧处理
    // -----------------------------------------------------------------------

    fn send_request(&mut self, id: u64, method: &str, params: Value) {
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = frame.to_string();
        line.push('\n');
        if let Some(stdin) = &mut self.stdin {
            let _ = stdin.write_all(line.as_bytes());
            let _ = stdin.flush();
        }
    }

    /// 等待指定请求的响应；`heartbeat=true`（parse 阶段）时限随 progress/RecordBatch
    /// 滑动（心跳看门狗）。期间逐帧做 Schema 校验与 BEH 判定。
    fn wait_for(
        &mut self,
        want_id: u64,
        method: &str,
        timeout: Duration,
        heartbeat: bool,
    ) -> WaitOutcome {
        self.pending.insert(want_id, method.to_string());
        if heartbeat {
            self.last_activity = Some(Instant::now());
        }
        let mut deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline || self.fatal {
                self.pending.remove(&want_id);
                return WaitOutcome::Timeout;
            }
            match self.frames.recv_timeout(deadline - now) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.pending.remove(&want_id);
                    return WaitOutcome::Timeout;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.pending.remove(&want_id);
                    return WaitOutcome::ProcessEnded;
                }
                Ok(LineEvent::Eof) => {
                    self.pending.remove(&want_id);
                    return WaitOutcome::ProcessEnded;
                }
                Ok(LineEvent::Line(f)) => {
                    if let Some((id, v)) = self.on_frame(&f, heartbeat) {
                        self.pending.remove(&want_id);
                        if id == want_id {
                            return WaitOutcome::Response(v);
                        }
                    }
                    if heartbeat {
                        if let Some(la) = self.last_activity {
                            deadline = la + timeout;
                        }
                    }
                }
            }
        }
    }

    /// 处理一行帧：返回 `Some((id, value))` = 该帧是在途请求的响应（按 id 关联）。
    fn on_frame(&mut self, f: &FrameLine, heartbeat: bool) -> Option<(u64, Value)> {
        let loc = format!("stdout line {}", f.no);
        match &f.kind {
            LineKind::TooLong { bytes } => {
                self.findings.push(Finding::error(
                    "BEH-08",
                    format!(
                        "单行消息 {bytes} 字节超过 8 MB 上限（8,388,608；宿主立即终止会话，protocol-v1.md §1.3）"
                    ),
                    &loc,
                ));
                self.fatal = true;
                None
            }
            LineKind::Bom => {
                self.findings.push(Finding::error(
                    "BEH-09",
                    "帧携带 UTF-8 BOM（protocol-v1.md §1.2：帧为 UTF-8 无 BOM）",
                    &loc,
                ));
                None
            }
            LineKind::CarriageReturn => {
                self.findings.push(Finding::error(
                    "BEH-09",
                    "帧含原始 `\\r`（`\\r\\n` 行尾或孤立 `\\r` 均违反 NDJSON 行尾约定，protocol-v1.md §1.2）",
                    &loc,
                ));
                None
            }
            LineKind::NotJson => {
                if f.text.contains("NaN") || f.text.contains("Infinity") {
                    self.findings.push(Finding::error(
                        "BEH-05",
                        "帧内出现 NaN/Infinity 字面量（JSON 非法；protocol-v1.md §3.1 禁止输出非有限数值）",
                        &loc,
                    ));
                } else {
                    self.findings.push(Finding::error(
                        "BEH-09",
                        "stdout 混入非 JSON 内容（protocol-v1.md §1.2：stdout 只准单行 JSON-RPC 帧）",
                        &loc,
                    ));
                }
                None
            }
            LineKind::Json(v) => self.on_json_frame(v, &loc, heartbeat),
        }
    }

    fn on_json_frame(&mut self, v: &Value, loc: &str, heartbeat: bool) -> Option<(u64, Value)> {
        // 帧级结构校验（rpc-messages.schema.json；单源；失败折算对应 BEH 规则）
        let collected: Vec<_> = self.schema.iter_errors(v).collect();
        if !collected.is_empty() {
            let total = collected.len();
            let details: Vec<String> = collected.iter().take(2).map(|e| e.to_string()).collect();
            let rule = self.schema_failure_rule(v, &collected);
            let msg = format!(
                "帧未通过 rpc-messages Schema 校验（{total} 处）：{}",
                details.join("；")
            );
            self.findings.push(Finding::error(rule, msg, loc));
            // Schema 失败后仍尽力按可提取字段继续（id/params 存在时）
        }

        // 通知帧
        if let Some(method) = v.get("method").and_then(Value::as_str) {
            match method {
                "progress" => {
                    if heartbeat {
                        self.progress_seen = true;
                        self.last_activity = Some(Instant::now());
                    }
                }
                "RecordBatch" if heartbeat => {
                    self.last_activity = Some(Instant::now());
                    if let Some(params) = v.get("params") {
                        self.check_record_batch(params, loc);
                    }
                }
                _ => {
                    // 未知通知：宿主记录并忽略（protocol-v1.md §1.4）
                }
            }
            return None;
        }

        // 响应帧（按 id 关联）
        let Some(id) = v.get("id").and_then(Value::as_u64) else {
            return None; // 无 id：Schema 已兜底
        };
        if !self.pending.contains_key(&id) {
            self.findings.push(Finding::error(
                "BEH-02",
                format!("响应 id {id} 无对应在途请求（或重复响应；protocol-v1.md §1.4）"),
                loc,
            ));
            return None;
        }
        Some((id, v.clone()))
    }

    /// Schema 失败 → 对应 BEH 规则折算表（docs-validator.md §3.2）。
    fn schema_failure_rule(&self, v: &Value, errs: &[ValidationError]) -> &'static str {
        if let Some(method) = v.get("method").and_then(Value::as_str) {
            return match method {
                "RecordBatch" => {
                    if errs
                        .iter()
                        .any(|e| e.instance_path.to_string().contains("seq"))
                    {
                        "BEH-06"
                    } else {
                        "BEH-05"
                    }
                }
                "progress" => "BEH-04",
                _ => "BEH-09",
            };
        }
        if v.get("error").is_some() {
            return "BEH-03";
        }
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            return match self.pending.get(&id).map(String::as_str) {
                Some("initialize") | Some("can_handle") => "BEH-01",
                Some("schema") => "BEH-03",
                Some("parse") => "BEH-06",
                Some("key_values") => "BEH-07",
                _ => "BEH-02",
            };
        }
        "BEH-02"
    }

    // -----------------------------------------------------------------------
    // 各方法响应判定
    // -----------------------------------------------------------------------

    /// BEH-01：initialize 元数据 + id 一致；任何错误响应视为握手失败。
    fn handle_initialize_response(&mut self, v: &Value) {
        let loc = "initialize（id=1）响应";
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            if code == -32601 {
                self.findings.push(Finding::error(
                    "BEH-03",
                    "必选方法 initialize 返回 -32601（协议要求实现）",
                    loc,
                ));
            }
            self.findings.push(Finding::error(
                "BEH-01",
                format!(
                    "initialize 返回错误码 {code}（握手失败；宿主不重试握手，protocol-v1.md §2.1）"
                ),
                loc,
            ));
            self.fatal = true;
            return;
        }
        let Some(result) = v.get("result").filter(|r| r.is_object()) else {
            return; // 结构问题由 Schema 折算 BEH-01 兜底
        };
        for field in ["id", "name", "version", "capabilities"] {
            if result.get(field).is_none() {
                self.findings.push(Finding::error(
                    "BEH-01",
                    format!("initialize 元数据缺 `{field}`（必选：id/name/version/capabilities）"),
                    loc,
                ));
            }
        }
        if let Some(id) = result.get("id").and_then(Value::as_str) {
            if id != self.manifest_id {
                self.findings.push(Finding::error(
                    "BEH-01",
                    format!(
                        "initialize 返回 id `{id}` 与 manifest id `{}` 不一致",
                        self.manifest_id
                    ),
                    loc,
                ));
            }
        }
        if let Some(caps) = result.get("capabilities") {
            for field in ["annotate", "subscribe", "binary_sidecar"] {
                if !caps.get(field).and_then(Value::as_bool).is_some() {
                    self.findings.push(Finding::error(
                        "BEH-01",
                        format!("capabilities.{field} 缺失或非布尔（initialize 元数据不完整）"),
                        loc,
                    ));
                }
            }
        }
    }

    /// BEH-03：schema 必选方法行为合规 + 收集指标集合（BEH-05 基线）。
    fn handle_schema_response(&mut self, v: &Value) {
        let loc = "schema（id=2）响应";
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let msg = if code == -32601 {
                "必选方法 schema 返回 -32601（协议要求实现）".to_string()
            } else {
                format!(
                    "必选方法 schema 返回错误码 {code}（宿主将禁用指标树；合规插件必须可声明指标）"
                )
            };
            self.findings.push(Finding::error("BEH-03", msg, loc));
            self.fatal = true;
            return;
        }
        let Some(result) = v.get("result") else {
            return;
        };
        let Some(metrics) = result.get("metrics").and_then(Value::as_array) else {
            self.findings.push(Finding::error(
                "BEH-03",
                "schema 响应缺 metrics 数组（SchemaResult.metrics 必选）",
                loc,
            ));
            self.fatal = true;
            return;
        };
        let mut ids = HashSet::new();
        let mut valid = true;
        for (i, m) in metrics.iter().enumerate() {
            let mloc = format!("{loc}, SchemaResult.metrics[{i}]");
            match m.get("id").and_then(Value::as_str) {
                Some(id) if !id.is_empty() => {
                    ids.insert(id.to_string());
                }
                _ => {
                    self.findings.push(Finding::error(
                        "BEH-03",
                        format!("MetricDef[{i}] 缺 id 或 id 为空字符串"),
                        &mloc,
                    ));
                    valid = false;
                }
            }
            if m.get("name").and_then(Value::as_str).is_none() {
                self.findings.push(Finding::error(
                    "BEH-03",
                    format!("MetricDef[{i}] 缺 name（string）"),
                    &mloc,
                ));
                valid = false;
            }
            let agg_ok = m
                .get("aggregation")
                .and_then(Value::as_str)
                .is_some_and(|a| matches!(a, "last" | "sum" | "avg" | "min" | "max"));
            if !agg_ok {
                self.findings.push(Finding::error(
                    "BEH-03",
                    format!("MetricDef[{i}].aggregation 非法（可选值：last/sum/avg/min/max）"),
                    &mloc,
                ));
                valid = false;
            }
        }
        self.metrics = ids;
        self.metrics_valid = valid;
    }

    /// BEH-01（confidence 越界，docs-validator.md §3.5）+ 认领判定。
    fn handle_can_handle_response(&mut self, v: &Value) -> bool {
        let loc = "can_handle（id=3）响应";
        if v.get("error").is_some() {
            return false; // 视为弃权（protocol-v1.md §6：超时/失败按弃权处理）
        }
        let Some(result) = v.get("result").filter(|r| r.is_object()) else {
            return false;
        };
        let claimed = result
            .get("can_handle")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(c) = result.get("confidence") {
            let in_range = c.as_f64().is_some_and(|f| (0.0..=1.0).contains(&f));
            if !in_range {
                self.findings.push(Finding::error(
                    "BEH-01",
                    format!("can_handle 置信度越界（confidence = {c}，必须在闭区间 [0,1]，protocol-v1.md §2.2）"),
                    loc,
                ));
            }
        }
        claimed
    }

    /// BEH-04（心跳）/ BEH-06（records_total / 末批）收尾判定。
    fn handle_parse_response(&mut self, v: &Value) {
        let loc = "parse（id=6）响应";
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            if code == -32601 {
                self.findings.push(Finding::error(
                    "BEH-03",
                    "必选方法 parse 返回 -32601（协议要求实现）",
                    loc,
                ));
                self.fatal = true;
            } else {
                // -32003/-32004 等为合法失败路径（宿主可重试）；records_total 无从核对
                self.notes.push(format!(
                    "parse 返回错误码 {code}（宿主按 -32003 语义处理并允许重试）"
                ));
            }
            return;
        }
        if !self.progress_seen {
            self.findings.push(Finding::error(
                "BEH-04",
                "parse 全程未发送任何 progress 心跳（protocol-v1.md §3.3 心跳义务；fixture 足够小时同样检查）",
                loc,
            ));
        }
        let Some(result) = v.get("result") else {
            return;
        };
        match result.get("records_total").and_then(Value::as_u64) {
            None => {
                self.findings.push(Finding::error(
                    "BEH-06",
                    "parse 响应缺 records_total（ParseResult 必选字段，protocol-v1.md §2.4）",
                    loc,
                ));
            }
            Some(total) => {
                if total != self.parse_record_sum {
                    self.findings.push(Finding::error(
                        "BEH-06",
                        format!(
                            "records_total = {total} ≠ 各批 records.length 之和 {}（protocol-v1.md §3.2）",
                            self.parse_record_sum
                        ),
                        loc,
                    ));
                }
            }
        }
    }

    /// BEH-07：key_values 响应结构（KeyValuesResult）。
    fn handle_key_values_response(&mut self, v: &Value) {
        let loc = "key_values（id=7）响应";
        if v.get("error").is_some() {
            return; // 错误响应：宿主面板显示错误，无冻结规则判定
        }
        let Some(result) = v.get("result") else {
            return;
        };
        let Some(entries) = result.get("entries").and_then(Value::as_array) else {
            self.findings.push(Finding::error(
                "BEH-07",
                "key_values 响应缺 entries 数组（KeyValuesResult 结构不符，protocol-v1.md §2.6）",
                loc,
            ));
            return;
        };
        for (i, e) in entries.iter().enumerate() {
            let eloc = format!("{loc}, KeyValuesResult.entries[{i}]");
            if e.get("key").and_then(Value::as_str).is_none() {
                self.findings.push(Finding::error(
                    "BEH-07",
                    "KeyValueEntry 缺 key（string）",
                    &eloc,
                ));
            }
            match e.get("value") {
                Some(Value::Object(_)) | Some(Value::Array(_)) => {
                    self.findings.push(Finding::error(
                        "BEH-07",
                        "KeyValueEntry.value 必须是 string/number/boolean（protocol-v1.md §2.6 限定，禁止对象/数组）",
                        &eloc,
                    ));
                }
                None => {
                    self.findings
                        .push(Finding::error("BEH-07", "KeyValueEntry 缺 value", &eloc));
                }
                _ => {}
            }
            if let Some(u) = e.get("unit") {
                if !u.is_string() {
                    self.findings.push(Finding::error(
                        "BEH-07",
                        "KeyValueEntry.unit 必须为 string",
                        &eloc,
                    ));
                }
            }
        }
    }

    /// BEH-05（Record 三必填 + metric ∈ schema + 可选字段非空容器）/
    /// BEH-06（seq 连续性 / done 后无批）逐批判定。
    fn check_record_batch(&mut self, params: &Value, loc: &str) {
        let seq = params.get("seq").and_then(Value::as_u64);
        if let Some(seq) = seq {
            if let Some(expected) = self.parse_expected_seq {
                if seq != expected {
                    self.findings.push(Finding::error(
                        "BEH-06",
                        format!(
                            "RecordBatch.seq 缺号或重复：期望 {expected}，收到 {seq}（宿主将中止会话，protocol-v1.md §3.2）"
                        ),
                        loc,
                    ));
                }
            }
            if self.parse_done_seen {
                self.findings.push(Finding::error(
                    "BEH-06",
                    "done:true 末批之后又收到 RecordBatch（protocol-v1.md §3.2）",
                    loc,
                ));
            }
            self.parse_expected_seq = Some(seq + 1);
        }
        if let Some(records) = params.get("records").and_then(Value::as_array) {
            for (i, r) in records.iter().enumerate() {
                let seq_text = seq
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let rloc = format!("{loc}, RecordBatch seq={seq_text}, record #{i}");
                // timestamp/metric/value 三必填 + 可选字段 null/空容器 → Schema 判据（BEH-05 折算）
                if let Some(metric) = r.get("metric").and_then(Value::as_str) {
                    if self.metrics_valid && !self.metrics.contains(metric) {
                        let declared = if self.metrics.is_empty() {
                            "（空）".to_string()
                        } else {
                            let mut v: Vec<&String> = self.metrics.iter().collect();
                            v.sort();
                            v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                        };
                        self.findings.push(Finding::error(
                            "BEH-05",
                            format!(
                                "Record.metric `{metric}` 未在 schema() 声明（已声明：{declared}；宿主将丢弃该记录并计数，protocol-v1.md §2.5）"
                            ),
                            &rloc,
                        ));
                    }
                }
            }
            self.parse_record_sum += records.len() as u64;
        }
        if params.get("done").and_then(Value::as_bool) == Some(true) {
            self.parse_done_seen = true;
        }
    }
}

impl Drop for Session<'_> {
    /// 兜底：校验结束必杀进程，保证无残留子进程（docs-validator.md §3.4）。
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// fixture 元信息（can_handle 参数，protocol-v1.md §2.2）。
struct FixtureInfo {
    absolute: String,
    name: String,
    ext: String,
    size_bytes: u64,
    head_sample: String,
}

impl FixtureInfo {
    fn from_path(p: &Path) -> Self {
        let absolute = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = p
            .extension()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let size_bytes = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let head_sample = read_head(p, 4096);
        FixtureInfo {
            absolute: absolute.display().to_string(),
            name,
            ext,
            size_bytes,
            head_sample,
        }
    }

    fn can_handle_params(&self) -> Value {
        json!({
            "path": self.absolute,
            "name": self.name,
            "ext": self.ext,
            "size_bytes": self.size_bytes,
            "head_sample": self.head_sample,
        })
    }
}

/// 前 N 字节 UTF-8 宽松解码（与 protocol-v1.md §2.2 head_sample 规则一致）。
fn read_head(p: &Path, n: usize) -> String {
    use std::io::Read;
    let mut buf = Vec::with_capacity(n);
    if let Ok(mut f) = fs::File::open(p) {
        let mut chunk = [0u8; 4096];
        while buf.len() < n {
            match f.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => buf.extend_from_slice(&chunk[..read]),
            }
        }
    }
    buf.truncate(n);
    String::from_utf8_lossy(&buf).into_owned()
}

fn extract_time_range(v: &Value) -> Option<(i64, i64)> {
    let result = v.get("result")?;
    let tr = result.get("time_range")?;
    let start = tr.get("start_ms").and_then(Value::as_i64)?;
    let end = tr.get("end_ms").and_then(Value::as_i64)?;
    Some((start, end))
}
