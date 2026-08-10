//! mock 剧本回放套件（qa-perf.md §3.1）：7 用例，契约联调底座。
//!
//! 用例 = `mock_plugin_suite/cases/<name>.json`（剧本 + 断言文件）：
//! 测试读取剧本路径与 `expect` 字段，按 protocol-v1.md §3/§5/§6 驱动迷你宿主断言。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ab_e2e::fixtures_ref;
use ab_e2e::harness::{
    dump_on_failure, FileEntryState, HostError, PluginInvocation, PluginSession, SessionState,
};
use serde_json::Value;

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";
const T_SLICE: i64 = 1_785_600_000_123;
const T_END: i64 = 1_785_603_599_870;

/// 定位 mock-plugin 二进制；未构建时按需 `cargo build -p mock-plugin`。
/// （mock-plugin 是纯 bin crate，不能作为 cargo 依赖，故手动定位。）
fn mock_plugin_bin() -> PathBuf {
    let ws = fixtures_ref::workspace_root();
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let bin = ws.join("target").join(profile).join("mock-plugin.exe");
    if !bin.exists() {
        let status = std::process::Command::new("cargo")
            .current_dir(&ws)
            .args(["build", "-p", "mock-plugin"])
            .status()
            .expect("cargo build -p mock-plugin");
        assert!(status.success(), "cargo build -p mock-plugin failed");
    }
    assert!(bin.exists(), "mock-plugin 二进制缺失: {}", bin.display());
    bin
}

fn cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("mock_plugin_suite")
        .join("cases")
}

fn replay_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("mock_plugin_suite")
        .join("replay_scripts")
}

/// 读取用例断言文件。
fn case_json(name: &str) -> Value {
    let path = cases_dir().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read case {name}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse case {name}: {e}"))
}

/// 拉起回放器（按剧本应答）。
fn spawn_case(name: &str) -> PluginSession {
    let script = case_json(name)["script"]
        .as_str()
        .expect("script field")
        .to_string();
    let script_path = replay_dir().join(&script);
    assert!(script_path.exists(), "剧本 {script_path:?} 必须存在");
    let inv = PluginInvocation {
        exe: mock_plugin_bin(),
        args: vec![
            "--script".to_string(),
            script_path.to_string_lossy().into_owned(),
        ],
        working_dir: None, // mock 回放器读绝对路径脚本，继承宿主 cwd
    };
    PluginSession::spawn(&inv, 1 << 20).expect("spawn mock-plugin")
}

/// 标准前置序列：initialize → schema → can_handle → load_file。
fn setup(s: &mut PluginSession) {
    let init = s
        .initialize("AnalysisBuddy-test", "0.1.0")
        .expect("initialize");
    assert_eq!(init["id"], "mock", "initialize 返回插件元数据");
    assert_eq!(init["capabilities"]["binary_sidecar"], false);
    s.schema().expect("schema");
    let can = s
        .can_handle(&ab_protocol::types::CanHandleParams {
            path: fixtures_ref::fixture_path(fixtures_ref::SMALL_WITH_HEADER)
                .to_string_lossy()
                .into_owned(),
            name: fixtures_ref::SMALL_WITH_HEADER.to_string(),
            ext: "csv".to_string(),
            size_bytes: 8747,
            head_sample: "timestamp,fps,frame_ms,mem_mb".to_string(),
        })
        .expect("can_handle");
    assert!(can.can_handle, "can_handle 必须认领");
    assert!(can.confidence >= 0.8, "confidence 断言");
    let _ = s
        .load_file(
            FILE_ID,
            &fixtures_ref::fixture_path(fixtures_ref::SMALL_WITH_HEADER),
        )
        .expect("load_file");
    assert_eq!(s.file_state(FILE_ID), FileEntryState::Loaded);
}

// ---------------------------------------------------------------------------
// 用例 1：happy_path —— 完整正常流
// ---------------------------------------------------------------------------

#[test]
fn mock_happy_path() {
    let expect = case_json("happy_path")["expect"].clone();
    let mut s = spawn_case("happy_path");
    setup(&mut s);

    let outcome = match s.parse(FILE_ID) {
        Ok(o) => o,
        Err(e) => {
            dump_on_failure("happy_path", Some(&s), &e.message());
            panic!("parse failed: {e:?}");
        }
    };
    assert_eq!(
        outcome.records_total,
        expect["records_total"].as_u64().unwrap()
    );
    assert_eq!(outcome.batches, expect["batches"].as_u64().unwrap());
    assert_eq!(
        outcome.sum_records,
        expect["records_total"].as_u64().unwrap()
    );
    assert!(outcome.done_seen);
    assert_eq!(s.file_state(FILE_ID), FileEntryState::Parsed);

    // 查询 API：时间切片条数 == 剧本 Record 总数（qa-perf.md §3.1）。
    let slice = s.store.slice(T_SLICE, T_END);
    assert_eq!(slice.len() as u64, expect["slice_count"].as_u64().unwrap());
    // LTTB 降采样点数。
    let buckets = expect["lttb_buckets"].as_u64().unwrap() as usize;
    let down = s.store.lttb(buckets);
    assert_eq!(down.len(), buckets);
    assert_eq!(down.first().map(|p| p.1), Some(59.8));

    // key_values 结果。
    let kv = s.key_values(FILE_ID, T_END).expect("key_values");
    assert_eq!(
        kv.entries.len() as u64,
        expect["key_values_entries"].as_u64().unwrap()
    );
    let scene = kv
        .entries
        .iter()
        .find(|e| e.key == "scene")
        .expect("scene entry");
    assert_eq!(scene.value, Value::String("boss".into()));

    s.unload_file(FILE_ID).expect("unload_file");
    assert_eq!(s.file_state(FILE_ID), FileEntryState::NotLoaded);
    s.shutdown().expect("shutdown");
    assert_eq!(s.state(), SessionState::Shutdown);
}

// ---------------------------------------------------------------------------
// 用例 2：load_failed —— -32002 文件条目置灰、可重试入口存在
// ---------------------------------------------------------------------------

#[test]
fn mock_load_failed() {
    let expect = case_json("load_failed")["expect"].clone();
    let mut s = spawn_case("load_failed");
    let _ = s
        .initialize("AnalysisBuddy-test", "0.1.0")
        .expect("initialize");
    s.schema().expect("schema");

    let err = s
        .load_file(
            FILE_ID,
            &fixtures_ref::fixture_path(fixtures_ref::SMALL_WITH_HEADER),
        )
        .expect_err("load_file 必须失败");
    match err {
        HostError::Rpc { code, .. } => {
            assert_eq!(code, expect["error_code"].as_i64().unwrap() as i32)
        }
        other => panic!("expected rpc -32002, got {other:?}"),
    }
    // 文件条目置灰（LoadFailed）、会话仍可用。
    assert_eq!(s.file_state(FILE_ID), FileEntryState::LoadFailed);
    assert_eq!(s.state(), SessionState::Ready);
    // 可重试入口存在：再次 load_file 可重复调用（幂等可重入）。
    assert!(expect["retry_available"].as_bool().unwrap());
    let again = s.load_file(
        FILE_ID,
        &fixtures_ref::fixture_path(fixtures_ref::SMALL_WITH_HEADER),
    );
    assert!(matches!(again, Err(HostError::Rpc { code: -32002, .. })));

    s.shutdown().expect("shutdown");
    assert_eq!(s.state(), SessionState::Shutdown);
}

// ---------------------------------------------------------------------------
// 用例 3：parse_failed_mid —— 三批后 -32003，已收批次全丢弃
// ---------------------------------------------------------------------------

#[test]
fn mock_parse_failed_mid() {
    let expect = case_json("parse_failed_mid")["expect"].clone();
    let mut s = spawn_case("parse_failed_mid");
    setup(&mut s);

    let err = s.parse(FILE_ID).expect_err("parse 必须失败");
    match err {
        HostError::Rpc { code, message } => {
            assert_eq!(code, expect["error_code"].as_i64().unwrap() as i32);
            assert!(message.contains("corrupt"), "message 带定位信息: {message}");
        }
        other => panic!("expected rpc -32003, got {other:?}"),
    }
    // 已收批次全丢弃，存储无残留（protocol §3.2/§4.2 -32003 处置）。
    assert_eq!(
        s.store.count(),
        expect["stored_after_failure"].as_u64().unwrap() as usize
    );
    // 状态回滚到已加载未解析。
    assert_eq!(s.file_state(FILE_ID), FileEntryState::Loaded);
    assert_eq!(s.state(), SessionState::Ready);

    s.unload_file(FILE_ID).expect("unload_file");
    s.shutdown().expect("shutdown");
}

// ---------------------------------------------------------------------------
// 用例 4：cancel_flow —— cancel_parse 后 -32004，状态回滚、无残留
// ---------------------------------------------------------------------------

#[test]
fn mock_cancel_flow() {
    let expect = case_json("cancel_flow")["expect"].clone();
    let mut s = spawn_case("cancel_flow");
    setup(&mut s);

    let err = s.parse(FILE_ID).expect_err("parse 必须以 -32004 结束");
    match err {
        HostError::Rpc { code, .. } => {
            assert_eq!(code, expect["error_code"].as_i64().unwrap() as i32);
        }
        other => panic!("expected rpc -32004, got {other:?}"),
    }
    // -32004：半收数据全丢弃（protocol §3.4 第 3 步）。
    assert_eq!(
        s.store.count(),
        expect["stored_after_cancel"].as_u64().unwrap() as usize
    );
    assert_eq!(
        s.file_state(FILE_ID),
        FileEntryState::Loaded,
        "状态回滚到已加载未解析"
    );

    // 宿主发 cancel_parse：返回 {}，幂等。
    assert!(expect["cancel_parse_ok"].as_bool().unwrap());
    s.cancel_parse(FILE_ID).expect("cancel_parse 应答 {}");
    // 对未在解析的 file_id 再取消：幂等返回 {}（protocol §3.4 第 4 步）。
    s.cancel_parse(FILE_ID).expect("cancel_parse 幂等");

    s.unload_file(FILE_ID).expect("unload_file");
    s.shutdown().expect("shutdown");
}

// ---------------------------------------------------------------------------
// 用例 5：seq_gap —— RecordBatch seq 0,1,3：会话终止、进程被 kill
// ---------------------------------------------------------------------------

#[test]
fn mock_seq_gap() {
    let expect = case_json("seq_gap")["expect"].clone();
    let mut s = spawn_case("seq_gap");
    setup(&mut s);

    let err = s.parse(FILE_ID).expect_err("seq 缺号必须终止会话");
    match err {
        HostError::ProtocolViolation(msg) => {
            assert!(
                msg.contains("seq gap") && msg.contains("expected 2"),
                "错误消息指明缺号: {msg}"
            );
            assert!(expect["error_mentions_seq"].as_bool().unwrap());
        }
        other => panic!("expected protocol violation, got {other:?}"),
    }
    assert_eq!(s.state(), SessionState::Terminated);
    assert!(expect["process_killed"].as_bool().unwrap());
    assert!(!s.is_alive(), "进程必须已被 kill");
    assert!(
        s.last_error().map(|e| e.contains("seq")).unwrap_or(false),
        "UI 报错含 seq 信息"
    );
}

// ---------------------------------------------------------------------------
// 用例 6：heartbeat_stop —— 30s 心跳看门狗 → Timeout、进程被 kill
// ---------------------------------------------------------------------------

#[test]
fn mock_heartbeat_stop() {
    let expect = case_json("heartbeat_stop")["expect"].clone();
    let mut s = spawn_case("heartbeat_stop");
    setup(&mut s);

    let t0 = Instant::now();
    let err = s.parse(FILE_ID).expect_err("心跳停止必须超时");
    let elapsed = t0.elapsed();
    match err {
        HostError::Timeout => {}
        other => panic!("expected timeout, got {other:?}"),
    }
    let min_s = expect["timeout_elapsed_min_secs"].as_u64().unwrap();
    let max_s = expect["timeout_elapsed_max_secs"].as_u64().unwrap();
    assert!(
        elapsed >= Duration::from_secs(min_s) && elapsed <= Duration::from_secs(max_s),
        "看门狗触发时间 {elapsed:?} 应在 [{min_s}s, {max_s}s]"
    );
    assert_eq!(s.state(), SessionState::Timeout);
    assert!(expect["process_killed"].as_bool().unwrap());
    assert!(!s.is_alive(), "超时进程必须已被 kill");
    // 错误可重试：状态机吸收，重试 = 新实例（§5.2）。
    assert_eq!(s.last_error(), Some("timeout"));
}

// ---------------------------------------------------------------------------
// 用例 7：crash_retry —— 自动重试 ≤2 次、退避 1s/3s、第三次后熔断保留手动重试
// ---------------------------------------------------------------------------

#[test]
fn mock_crash_retry() {
    let expect = case_json("crash_retry")["expect"].clone();
    let expected_retries = expect["auto_retries"].as_u64().unwrap();
    let tol1 = Duration::from_millis(expect["backoff_1s_tol_ms"].as_u64().unwrap());
    let tol3 = Duration::from_millis(expect["backoff_3s_tol_ms"].as_u64().unwrap());
    let script_path = replay_dir().join("crash_retry.ndjson");

    /// 单次尝试：拉起新实例，parse 首批后由 hook 模拟进程崩溃退出。
    fn attempt(
        script_path: &Path,
        file_id: &str,
    ) -> Result<ab_e2e::harness::ParseOutcome, HostError> {
        let mut s = PluginSession::spawn(
            &PluginInvocation {
                exe: mock_plugin_bin(),
                args: vec![
                    "--script".to_string(),
                    script_path.to_string_lossy().into_owned(),
                ],
                working_dir: None,
            },
            1 << 20,
        )
        .expect("spawn");
        let _ = s
            .initialize("AnalysisBuddy-test", "0.1.0")
            .expect("initialize");
        let _ = s
            .load_file(
                file_id,
                &fixtures_ref::fixture_path(fixtures_ref::SMALL_WITH_HEADER),
            )
            .expect("load_file");
        let mut killed_first_batch = false;
        let result = s.parse_with_hook(
            file_id,
            Some(&mut |batch| {
                if batch.seq == 0 && !killed_first_batch {
                    killed_first_batch = true;
                    return true; // 回放器在 parse 中途退出进程
                }
                false
            }),
        );
        match &result {
            Err(HostError::ProcessDied(_)) => {}
            Err(e) => panic!("期望进程崩溃语义，得到 {e:?}"),
            Ok(_) => panic!("parse 不应成功"),
        }
        assert_eq!(s.state(), SessionState::Crashed);
        result
    }

    let t0 = Instant::now();
    let mut failures = 0u64;
    let mut attempt_ends: Vec<Instant> = Vec::new();
    let mut gaps: Vec<Duration> = Vec::new();

    for i in 0..3 {
        let r = attempt(&script_path, FILE_ID);
        if r.is_err() {
            failures += 1;
        }
        let ended = Instant::now();
        attempt_ends.push(ended);
        // 自动重试退避：第 1 次失败后等 1s，第 2 次失败后等 3s（protocol §5.2）。
        if i < 2 {
            let expect_sleep = if i == 0 {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(3)
            };
            std::thread::sleep(expect_sleep);
        }
    }
    // 相邻尝试间隔（= 退避 + 少量开销）。
    for w in attempt_ends.windows(2) {
        gaps.push(w[1].duration_since(w[0]));
    }
    assert_eq!(gaps.len(), 2);
    for (i, gap) in gaps.iter().enumerate() {
        let (expect_sleep, tol) = if i == 0 {
            (Duration::from_secs(1), tol1)
        } else {
            (Duration::from_secs(3), tol3)
        };
        assert!(
            *gap >= expect_sleep.saturating_sub(tol)
                && *gap <= expect_sleep + tol + Duration::from_millis(300),
            "第 {} 次退避实测 {gap:?} 应在 {:?}±{:?}",
            i + 1,
            expect_sleep,
            tol
        );
    }

    // 三次尝试全部失败 → 熔断：不再自动重试（自动重试计数 == 2）。
    assert_eq!(failures, 3, "三次尝试全部失败");
    assert_eq!(failures - 1, expected_retries, "自动重试次数 == 2");
    // 手动重试入口保留（熔断后人工可重试）。
    assert!(expect["manual_retry"].as_bool().unwrap());
    // 套件级预算：退避 1s+3s + 三次尝试，总耗时在 [4s, 8s] 窗口。
    let total = t0.elapsed();
    assert!(
        total >= Duration::from_secs(4) && total <= Duration::from_secs(8),
        "总耗时 {total:?} 超出退避预算"
    );
}
