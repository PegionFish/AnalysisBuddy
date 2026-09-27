//! I2 协议 fuzz harness（挑战任务 §2.7）。
//!
//! 两个 fuzz 目标的共享实现，供两种驱动方式复用（同一段被测逻辑，避免
//! libFuzzer 路径与稳定版 soak 路径漂移）：
//!
//! - `fuzz/`（cargo-fuzz + libFuzzer，nightly，覆盖率引导，主路径）；
//! - `src/bin/soak.rs`（稳定工具链的随机语料循环，CI smoke / 降级路径）。
//!
//! # 不变量（harness 内的断言即契约）
//!
//! - **T① 帧层**（[`fuzz_frame_read`]）：任意字节流喂给
//!   [`ab_host::rpc::FrameReader`] 不 panic、不挂死（帧数有界上界）、
//!   8 MB 上限内返回 `LineTooLong`（内存有界）；帧内容经 run_read_loop
//!   （§4.3/§4.4）同构分流后反序列化进全部入站类型同样不 panic。
//! - **T② JSON-RPC 入站**（[`fuzz_rpc_types`]）：任意字节 / 任意 JSON 值
//!   反序列化进 ab-protocol 全部入站类型不 panic；深嵌套受 serde_json
//!   递归上限（128 层）约束必须报错而非栈溢出。

use std::cell::RefCell;

use ab_host::rpc::{FrameError, FrameReader, RpcOutcome, SeqValidator};
use ab_protocol::types::{
    AnnotateResult, CanHandleParams, CanHandleResult, CancelParseParams, CustomQueryParams,
    CustomQueryResult, FileSummary, InitializeParams, InitializeResult, KeyValuesParams,
    KeyValuesResult, LoadFileParams, ParseParams, ParseResult, ProgressParams, Record, RecordBatch,
    SchemaResult, UnloadFileParams,
};
use arbitrary::Unstructured;
use serde_json::Value;

thread_local! {
    /// FrameReader::next_frame 是 async；libFuzzer / soak 均单线程迭代，
    /// thread_local 复用同一个 current_thread runtime（避免每次迭代重建）。
    static RT: RefCell<Option<tokio::runtime::Runtime>> = const { RefCell::new(None) };
}

fn with_rt<F, R>(f: F) -> R
where
    F: FnOnce(&tokio::runtime::Runtime) -> R,
{
    RT.with(|cell| {
        let mut slot = cell.borrow_mut();
        let rt = slot.get_or_insert_with(|| {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("fuzz tokio runtime (current_thread)")
        });
        f(rt)
    })
}

// ---------------------------------------------------------------------------
// T① 帧层
// ---------------------------------------------------------------------------

/// 目标①：随机字节流 → [`FrameReader`]（长度上限 + UTF-8/JSON 校验 + 分流）。
///
/// 覆盖形态：截断帧（EOF 处残行）、超长行（8 MB 上限）、伪长度头（长无换行
/// 序列）、非法 UTF-8、孤立 `\r`、空行、深嵌套 JSON、重复键、错位类型。
/// 内部以 run_read_loop（rpc.rs §4.3/§4.4）同构逻辑分流每个成功帧。
pub fn fuzz_frame_read(data: &[u8]) {
    with_rt(|rt| {
        rt.block_on(async {
            let mut frames = FrameReader::new(data);
            let mut seq = SeqValidator::new();
            // 处置表与真实会话（§4.2 / session.rs on_frame_error）同构：
            // Eof / LineTooLong / MalformedLine → Stop；InvalidJson 仅首次 Continue，
            // 重复即 Stop。**不可**在错误后无条件继续 next_frame——LineTooLong /
            // EOF 残行场景下缓冲区滞留未解析字节，重复轮询会退化为 O(n²) 重扫
            // （真实会话永不这样做，harness 亦不可，首轮实测曾出现 64KB slow-unit）。
            let mut invalid_json_seen = false;
            // 不挂死上界（双保险）：每个成功帧 / 已消费错误行至少推进 1 字节。
            let max_turns = data.len() + 8;
            for _ in 0..max_turns {
                match frames.next_frame().await {
                    Ok(frame) => dispatch_frame(&frame, &mut seq),
                    Err(FrameError::Eof | FrameError::LineTooLong | FrameError::MalformedLine) => {
                        break
                    }
                    Err(FrameError::InvalidJson) => {
                        if invalid_json_seen {
                            break;
                        }
                        invalid_json_seen = true;
                    }
                }
            }
        })
    });
}

/// run_read_loop 的同构分流（去掉 mpsc 路由 / stderr 日志 / 事件面）：
/// id 帧 → outcome 合成 + 应答类型反序列化；method 帧 → 通知分流 + seq 校验。
fn dispatch_frame(frame: &Value, seq: &mut SeqValidator) {
    if let Some(id) = frame.get("id").and_then(|v| v.as_u64()) {
        let _ = id; // id 仅用于路由；无 pending 时真实侧记日志丢弃，此处无副作用。
        let outcome = if let Some(err) = frame.get("error") {
            // 与 run_read_loop 逐字段同构：code 缺省 -32603、message 缺省 ""。
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
            RpcOutcome::Result(frame.get("result").cloned().unwrap_or(Value::Null))
        };
        exercise_outcome(&outcome);
        return;
    }
    if let Some(method) = frame.get("method").and_then(|m| m.as_str()) {
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        // SessionHandler（session.rs §4.4）同构：progress/RecordBatch 反序列化 +
        // SeqValidator。Err 在真实会话 = 协议致命（终止）；fuzz 侧仅复位后继续
        // （不变量是「不 panic」，终止语义由单测覆盖）。
        let _ = exercise_notification(method, &params, seq);
    }
}

/// 通知分流 + seq 连续性校验（与 SessionHandler::on_notification 同构）。
fn exercise_notification(
    method: &str,
    params: &Value,
    seq: &mut SeqValidator,
) -> Result<(), String> {
    match method {
        "progress" => {
            let _p: ProgressParams = serde_json::from_value(params.clone())
                .map_err(|e| format!("invalid progress notification: {e}"))?;
        }
        "RecordBatch" => {
            let batch: RecordBatch = serde_json::from_value(params.clone())
                .map_err(|e| format!("invalid RecordBatch notification: {e}"))?;
            // seq 缺号 / 重复 → Err（不 panic 即不变量）。
            seq.accept(&batch)?;
        }
        _ => {}
    }
    Ok(())
}

/// 响应负载按 §2.x 各应答类型逐一反序列化（与各调用方的 from_outcome 同构）。
fn exercise_outcome(outcome: &RpcOutcome) {
    let RpcOutcome::Result(v) = outcome else {
        return;
    };
    let _ = serde_json::from_value::<InitializeResult>(v.clone());
    let _ = serde_json::from_value::<CanHandleResult>(v.clone());
    let _ = serde_json::from_value::<FileSummary>(v.clone());
    let _ = serde_json::from_value::<ParseResult>(v.clone());
    let _ = serde_json::from_value::<SchemaResult>(v.clone());
    let _ = serde_json::from_value::<KeyValuesResult>(v.clone());
    let _ = serde_json::from_value::<AnnotateResult>(v.clone());
    let _ = serde_json::from_value::<CustomQueryResult>(v.clone());
}

// ---------------------------------------------------------------------------
// T② JSON-RPC 入站类型
// ---------------------------------------------------------------------------

/// 目标②：随机字节 / 随机 JSON 值 → ab-protocol 全部入站类型反序列化。
///
/// 三条路径：原始字节直灌（截断 / 非法 UTF-8 / 1e999 / 重复键）、合成
/// 任意 `Value`（arbitrary 派生随机标量，f64 直接取随机位模式含 NaN/±∞）、
/// 定向深嵌套（serde_json 128 层递归上限必须报错）。
pub fn fuzz_rpc_types(data: &[u8]) {
    // A) 原始字节直灌：每个入站类型都试一遍（Err 是预期结局，不 panic 即不变量）。
    let _ = serde_json::from_slice::<Value>(data);
    try_all_inbound_bytes(data);
    if let Ok(value) = serde_json::from_slice::<Value>(data) {
        // B) 已解析 Value → from_value（serde derive 的另一条入口）。
        try_all_inbound_value(&value);
    }
    // C) arbitrary 结构化合成：随机标量拼嵌套 Value → from_value。
    let mut u = Unstructured::new(data);
    if let Ok(v) = synth_json(&mut u, 0) {
        let _ = serde_json::from_value::<RecordBatch>(v.clone());
        let _ = serde_json::from_value::<ProgressParams>(v.clone());
        let _ = serde_json::from_value::<Record>(v.clone());
        let _ = serde_json::from_value::<CanHandleResult>(v.clone());
        let _ = serde_json::from_value::<InitializeResult>(v.clone());
    }
    // D) 定向深嵌套：递归上限是深嵌套内存/栈安全的第一道闸，必须报错。
    //    （若未来依赖改动使 100k 层竟能解析成功，此处立即成为可复现崩溃。）
    for depth in [129usize, 100_000] {
        let deep = nested_arrays(depth);
        assert!(
            serde_json::from_slice::<Value>(&deep).is_err(),
            "serde_json accepted {depth}-deep nesting: recursion limit drift"
        );
    }
}

/// 19 个入站类型的 from_slice 矩阵（§2.1-§2.11 请求/响应 + §3.x 通知参数）。
fn try_all_inbound_bytes(data: &[u8]) {
    let _ = serde_json::from_slice::<InitializeParams>(data);
    let _ = serde_json::from_slice::<InitializeResult>(data);
    let _ = serde_json::from_slice::<CanHandleParams>(data);
    let _ = serde_json::from_slice::<CanHandleResult>(data);
    let _ = serde_json::from_slice::<LoadFileParams>(data);
    let _ = serde_json::from_slice::<FileSummary>(data);
    let _ = serde_json::from_slice::<ParseParams>(data);
    let _ = serde_json::from_slice::<ParseResult>(data);
    let _ = serde_json::from_slice::<SchemaResult>(data);
    let _ = serde_json::from_slice::<KeyValuesParams>(data);
    let _ = serde_json::from_slice::<KeyValuesResult>(data);
    let _ = serde_json::from_slice::<AnnotateResult>(data);
    let _ = serde_json::from_slice::<CustomQueryParams>(data);
    let _ = serde_json::from_slice::<CustomQueryResult>(data);
    let _ = serde_json::from_slice::<UnloadFileParams>(data);
    let _ = serde_json::from_slice::<CancelParseParams>(data);
    let _ = serde_json::from_slice::<Record>(data);
    let _ = serde_json::from_slice::<RecordBatch>(data);
    let _ = serde_json::from_slice::<ProgressParams>(data);
}

/// 同上矩阵的 from_value 入口（serde_json::Value → 类型化）。
fn try_all_inbound_value(v: &Value) {
    let _ = serde_json::from_value::<InitializeParams>(v.clone());
    let _ = serde_json::from_value::<InitializeResult>(v.clone());
    let _ = serde_json::from_value::<CanHandleParams>(v.clone());
    let _ = serde_json::from_value::<CanHandleResult>(v.clone());
    let _ = serde_json::from_value::<LoadFileParams>(v.clone());
    let _ = serde_json::from_value::<FileSummary>(v.clone());
    let _ = serde_json::from_value::<ParseParams>(v.clone());
    let _ = serde_json::from_value::<ParseResult>(v.clone());
    let _ = serde_json::from_value::<SchemaResult>(v.clone());
    let _ = serde_json::from_value::<KeyValuesParams>(v.clone());
    let _ = serde_json::from_value::<KeyValuesResult>(v.clone());
    let _ = serde_json::from_value::<AnnotateResult>(v.clone());
    let _ = serde_json::from_value::<CustomQueryParams>(v.clone());
    let _ = serde_json::from_value::<CustomQueryResult>(v.clone());
    let _ = serde_json::from_value::<UnloadFileParams>(v.clone());
    let _ = serde_json::from_value::<CancelParseParams>(v.clone());
    let _ = serde_json::from_value::<Record>(v.clone());
    let _ = serde_json::from_value::<RecordBatch>(v.clone());
    let _ = serde_json::from_value::<ProgressParams>(v.clone());
}

/// arbitrary 派生的随机 JSON：f64 取随机位模式（含 NaN/±∞，检验入站有限性
/// 拒绝路径）；字符串长度有界；嵌套深度有界。输入耗尽即放弃（Ok/Err 皆可）。
fn synth_json(u: &mut Unstructured<'_>, depth: u8) -> Result<Value, arbitrary::Error> {
    let choice: u8 = u.int_in_range(0..=5)?;
    match choice {
        0 => Ok(Value::Bool(u.arbitrary::<bool>()?)),
        1 => Ok(Value::Number(u.arbitrary::<u64>()?.into())),
        2 => Ok(Value::Number(
            // 任意位模式 → 极端幅值（次正规 / ±1e308 / -0.0）；NaN/±∞ 在
            // serde_json::Value 中不可表示（from_f64 → None → 0），JSON 字面量
            // 溢出形态（1e999）由 A 路径原始字节直灌覆盖（C8：入站拒绝）。
            serde_json::Number::from_f64(f64::from_bits(u.arbitrary::<u64>()?))
                .unwrap_or(serde_json::Number::from(0)),
        )),
        3 => {
            let len = u.int_in_range(0..=32usize)?;
            let bytes: Vec<u8> = (0..len)
                .map(|_| u.arbitrary::<u8>())
                .collect::<Result<_, _>>()?;
            Ok(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        }
        4 if depth < 4 => {
            let len = u.int_in_range(0..=4usize)?;
            let mut map = serde_json::Map::new();
            for _ in 0..len {
                let klen = u.int_in_range(1..=8usize)?;
                let key: String = (0..klen)
                    .map(|_| u.arbitrary::<u8>())
                    .collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .map(|b| (b'a' + (b % 26)) as char)
                    .collect();
                map.insert(key, synth_json(u, depth + 1)?);
            }
            Ok(Value::Object(map))
        }
        _ if depth < 4 => {
            let len = u.int_in_range(0..=4usize)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(synth_json(u, depth + 1)?);
            }
            Ok(Value::Array(items))
        }
        _ => u.arbitrary::<bool>().map(Value::Bool),
    }
}

/// 深嵌套字节串（定向用例与 soak 驱动共用）。
pub fn nested_arrays(depth: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(depth * 2);
    v.extend(std::iter::repeat_n(b'[', depth));
    v.extend(std::iter::repeat_n(b']', depth));
    v
}

// ---------------------------------------------------------------------------
// 自检（cargo test --manifest-path tools/fuzz/Cargo.toml）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_minimal_inputs_do_not_panic() {
        fuzz_frame_read(b"");
        fuzz_frame_read(b"\n");
        fuzz_frame_read(b"{");
        fuzz_rpc_types(b"");
        fuzz_rpc_types(b"{");
    }

    #[test]
    fn seeds_smoke() {
        for name in [
            "seed_01_responses.json",
            "seed_02_notifications.json",
            "seed_03_truncated.json",
            "seed_04_binary.bin",
            "seed_05_crlf.json",
            "seed_06_deepnest.json",
            "seed_07_bignumbers.json",
            "seed_08_dupkeys.json",
            "seed_09_typemismatch.json",
        ] {
            let path = format!("{}/corpus/frame_read/{name}", env!("CARGO_MANIFEST_DIR"));
            let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
            fuzz_frame_read(&data);
            fuzz_rpc_types(&data);
        }
    }

    /// 定向：8 MB 上限（§1.3）——7.9 MB 行可读（内存有界），9 MB 行必 LineTooLong。
    #[test]
    fn line_length_cap_is_enforced() {
        let mut ok = vec![b'1'; 7 * 1024 * 1024];
        ok.push(b'\n');
        let got = with_rt(|rt| {
            rt.block_on(async { FrameReader::new(&ok[..]).next_frame().await.map(|_| ()) })
        });
        assert!(
            matches!(got, Err(FrameError::InvalidJson)),
            "7MB line must reach JSON validation, got {got:?}"
        );

        let over = vec![b'A'; FrameReader::<&[u8]>::MAX_LINE_BYTES + 1024];
        let got =
            with_rt(|rt| rt.block_on(async { FrameReader::new(&over[..]).next_frame().await }));
        assert_eq!(got, Err(FrameError::LineTooLong));
    }

    #[test]
    fn seq_overflow_boundary_does_not_panic() {
        let mut seq = SeqValidator::new();
        let batch = RecordBatch {
            file_id: "f".into(),
            seq: u64::MAX,
            records: Vec::new(),
            done: true,
        };
        let _ = seq.accept(&batch);
        let _ = seq.accept(&batch);
    }
}
