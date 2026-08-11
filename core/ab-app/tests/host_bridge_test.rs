//! P3-01 集成测试：`HostSessionAdapter` 对 mock-plugin 回放器
//! （`tools/mock-plugin/scripts/happy_path.ndjson`）跑
//! 握手 → parse_stream → key_values → shutdown 全流程，断言
//! `records_total == Σ批次条数` 且 `key_values` 非空；另有 `file_id` 过滤专项
//! （自定义剧本混入其他文件的批次 / 进度）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ab_host::{PluginProcessState, PluginRegistry, PluginRuntime, RuntimeConfig};
use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};
use ab_protocol::types::{KeyValuesParams, LoadFileParams, ParseParams, UnloadFileParams};
use tokio::sync::mpsc;

use ab_app::events::{self, ProgressThrottle};
use ab_app::host_bridge::HostSessionAdapter;
use ab_pipeline::{ParseEvent, PluginSession, SessionError};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-app-a3-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&base).expect("create tempdir");
        Self(base)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mock_plugin_bin() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../target"));
    let bin = target_dir.join("debug").join(if cfg!(windows) {
        "mock-plugin.exe"
    } else {
        "mock-plugin"
    });
    if !bin.exists() {
        let out = std::process::Command::new("cargo")
            .args(["build", "-p", "mock-plugin"])
            .current_dir(&manifest_dir)
            .output()
            .expect("cargo build -p mock-plugin");
        assert!(
            out.status.success(),
            "cargo build -p mock-plugin failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    bin
}

fn repo_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/mock-plugin/scripts")
        .join(name)
}

fn install_mock_plugin(dir: &Path, script: &Path) {
    fs::create_dir_all(dir).expect("mkdir plugin dir");
    let manifest = Manifest {
        id: "mock".to_string(),
        display_name: "Mock Replay Plugin".to_string(),
        version: "0.1.0".to_string(),
        entry: PluginEntry {
            command: mock_plugin_bin().to_string_lossy().into_owned(),
            args: vec![
                "--script".to_string(),
                script.to_string_lossy().into_owned(),
            ],
            working_dir: None,
        },
        r#match: MatchRules {
            extensions: vec!["csv".to_string()],
            header_fingerprints: None,
        },
        min_protocol_version: 1,
        ..Default::default()
    };
    fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write plugin.json");
}

fn runtime(tmp: &TempDir, config: RuntimeConfig) -> (Arc<PluginRegistry>, PluginRuntime) {
    let registry = Arc::new(PluginRegistry::with_sources(
        tmp.path().to_path_buf(),
        tmp.path().to_path_buf(),
        tmp.path().join("user"),
    ));
    let runtime = PluginRuntime::with_config(registry.clone(), config);
    (registry, runtime)
}

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

/// happy_path 剧本全流程：握手 → load → parse_stream（Σ批次 = records_total）
/// → key_values 非空 → unload → 优雅停机。
#[tokio::test]
async fn happy_path_via_adapter() {
    let tmp = TempDir::new("happy");
    install_mock_plugin(&tmp.path().join("mock"), &repo_script("happy_path.ndjson"));
    let (_registry, runtime) = runtime(&tmp, RuntimeConfig::default());
    let mut events_rx = runtime.subscribe_events();

    // 握手：get_or_spawn 封装 spawn → 250ms 快速退出检测 → initialize → Ready。
    let session = runtime
        .get_or_spawn("mock")
        .await
        .expect("handshake via get_or_spawn");
    assert_eq!(session.state(), PluginProcessState::Ready);

    let adapter = HostSessionAdapter::new(session.clone());
    assert_eq!(adapter.plugin_id(), "mock");

    adapter
        .load_file(LoadFileParams {
            file_id: FILE_ID.to_string(),
            path: "C:\\logs\\match.csv".to_string(),
        })
        .await
        .expect("load_file");

    let (tx, mut rx) = mpsc::channel(16);
    let collector = tokio::spawn(async move {
        let mut batches = Vec::new();
        let mut records = 0u64;
        let mut progress = 0u32;
        while let Some(event) = rx.recv().await {
            match event {
                ParseEvent::Batch(b) => {
                    records += b.records.len() as u64;
                    batches.push(b.seq);
                }
                ParseEvent::Progress(_) => progress += 1,
            }
        }
        (batches, records, progress)
    });
    let records_total = adapter
        .parse_stream(
            ParseParams {
                file_id: FILE_ID.to_string(),
                options: None,
            },
            tx,
        )
        .await
        .expect("parse_stream");

    let (batches, records, progress) = collector.await.expect("collector finished");
    assert_eq!(records_total, 3, "records_total from parse response");
    assert_eq!(
        (batches, records),
        (vec![0u64, 1], 3u64),
        "Σ批次条数 == records_total（DoD）"
    );
    assert_eq!(progress, 2, "progress notifications forwarded");
    assert_eq!(
        adapter.dropped_notifications(),
        0,
        "有界 sink 在 happy path 上未饱和"
    );

    // key_values 非空（DoD）。
    let kv = adapter
        .key_values(KeyValuesParams {
            file_id: FILE_ID.to_string(),
            timestamp_ms: 1785603599870,
        })
        .await
        .expect("key_values");
    assert!(!kv.entries.is_empty(), "key_values 返回非空（DoD）");
    assert_eq!(kv.entries.len(), 3);
    assert_eq!(kv.entries[0].key, "scene");
    assert_eq!(kv.entries[0].value, serde_json::json!("boss"));

    adapter
        .unload_file(UnloadFileParams {
            file_id: FILE_ID.to_string(),
        })
        .await
        .expect("unload_file");

    // 优雅停机：退出码 0 → Shutdown。
    session.shutdown().await.expect("shutdown");
    assert_eq!(session.state(), PluginProcessState::Shutdown);

    // 事件流：状态机迁移经 events::convert 产出 ab://plugin-health 载荷。
    let mut health = Vec::new();
    while let Ok(event) = events_rx.try_recv() {
        for emitted in events::convert(event, &mut ProgressThrottle::new()) {
            if let events::EventPayload::Health(payload) = emitted.payload {
                health.push((payload.state, payload.prev_state));
            }
        }
    }
    assert!(
        health.iter().any(|(state, prev)| {
            state == "ready" && (prev == "initializing" || prev == "parsing")
        }),
        "handshake Ready 迁移已转换：{health:?}"
    );
    assert!(
        health
            .iter()
            .any(|(state, prev)| { state == "shutdown" && prev == "draining" }),
        "优雅停机迁移已转换：{health:?}"
    );

    runtime.shutdown_all().await;
}

/// 自定义剧本：parse 期间混入其他 file_id 的 RecordBatch / progress，
/// 适配器必须按 file_id 过滤（pipeline.md §4.1「只放行本次 parse」）。
#[tokio::test]
async fn parse_stream_filters_foreign_file_ids() {
    let tmp = TempDir::new("filter");
    let script = tmp.path().join("filter.ndjson");
    let foreign = r#"{"kind":"emit","method":"RecordBatch","params":{"file_id":"other-file","seq":0,"records":[{"timestamp":1,"metric":"fps","value":1.0}],"done":true}}"#;
    let foreign_progress = r#"{"kind":"emit","method":"progress","params":{"file_id":"other-file","percent":0.5,"records_so_far":0}}"#;
    let own_batch_0 = r#"{"kind":"emit","method":"RecordBatch","params":{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","seq":0,"records":[{"timestamp":1,"metric":"fps","value":1.0},{"timestamp":2,"metric":"frame_ms","value":16.7}],"done":false}}"#;
    let own_batch_1 = r#"{"kind":"emit","method":"RecordBatch","params":{"file_id":"f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c","seq":1,"records":[{"timestamp":3,"metric":"player_hp","value":73}],"done":true}}"#;
    let script_body = [
        r#"{"kind":"reply","method":"initialize","result":{"id":"mock","name":"Mock Replay Plugin","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}}"#,
        foreign,
        foreign_progress,
        own_batch_0,
        own_batch_1,
        r#"{"kind":"reply","method":"parse","result":{"records_total":3}}"#,
        r#"{"kind":"reply","method":"shutdown","result":{}}"#,
    ]
    .join("\n");
    fs::write(&script, script_body).expect("write filter script");

    install_mock_plugin(&tmp.path().join("mock"), &script);
    let (_registry, runtime) = runtime(&tmp, RuntimeConfig::default());
    let session = runtime.get_or_spawn("mock").await.expect("handshake");
    let adapter = HostSessionAdapter::new(session.clone());

    let (tx, mut rx) = mpsc::channel(16);
    let collector = tokio::spawn(async move {
        let mut batches = Vec::new();
        let mut foreign_batches = 0u32;
        let mut progress = 0u32;
        while let Some(event) = rx.recv().await {
            match event {
                ParseEvent::Batch(b) if b.file_id == "other-file" => foreign_batches += 1,
                ParseEvent::Batch(b) => batches.push(b.seq),
                ParseEvent::Progress(p) if p.file_id == "other-file" => progress += 1,
                ParseEvent::Progress(_) => progress += 1,
            }
        }
        (batches, foreign_batches, progress)
    });
    let records_total = adapter
        .parse_stream(
            ParseParams {
                file_id: FILE_ID.to_string(),
                options: None,
            },
            tx,
        )
        .await
        .expect("parse_stream");

    let (batches, foreign_batches, progress) = collector.await.expect("collector finished");
    assert_eq!(records_total, 3);
    assert_eq!(batches, [0, 1], "只放行本次 parse 的批次");
    assert_eq!(foreign_batches, 0, "其他 file_id 的批次被过滤");
    assert_eq!(progress, 0, "其他 file_id 的 progress 被过滤");

    runtime.shutdown_all().await;
}

/// 构造「批次爆发」剧本：initialize + `batches` 条固定大小 RecordBatch 推送
/// （末批 `done:true`）+ 任意 parse 回复行。批次与回复相邻回放，模拟读泵在
/// parse response 到达前已把全部批次扇出到通知流缓冲——P3-06 竞态回归场景。
fn parse_burst_script(batches: usize, per_batch: usize, parse_reply: &str) -> String {
    let record = r#"{"timestamp":1,"metric":"fps","value":1.0}"#;
    let records_json = std::iter::repeat_n(record, per_batch)
        .collect::<Vec<_>>()
        .join(",");
    let mut lines: Vec<String> = vec![
        r#"{"kind":"reply","method":"initialize","result":{"id":"mock","name":"Mock Replay Plugin","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}}"#.to_string(),
    ];
    for seq in 0..batches {
        let done = seq + 1 == batches;
        lines.push(format!(
            r#"{{"kind":"emit","method":"RecordBatch","params":{{"file_id":"{FILE_ID}","seq":{seq},"records":[{records_json}],"done":{done}}}}}"#
        ));
    }
    lines.push(parse_reply.to_string());
    lines.join("\n")
}

/// 回归（P3-06 竞态）：同一脚本连续 ≥10 次 parse_stream，每次断言
/// `records_total == Σ批次条数 + dropped`。旧实现 `forward.abort()` 在响应
/// 到达时截断通知流缓冲中的批次 → mismatch；「完成信号 + 排空」修复后该
/// 不变式每次成立（满则丢为 §4.1 有意设计，若发生以 dropped 计数补偿）。
#[tokio::test]
async fn parse_stream_stress_always_matches_records_total() {
    const ITERATIONS: usize = 12;
    const BATCHES: usize = 200;
    const PER_BATCH: usize = 2;
    const RECORDS_TOTAL: usize = BATCHES * PER_BATCH;

    let tmp = TempDir::new("stress");
    let script = tmp.path().join("stress.ndjson");
    fs::write(
        &script,
        parse_burst_script(
            BATCHES,
            PER_BATCH,
            &format!(r#"{{"kind":"reply","method":"parse","result":{{"records_total":{RECORDS_TOTAL}}}}}"#),
        ),
    )
    .expect("write stress script");
    install_mock_plugin(&tmp.path().join("mock"), &script);
    let (_registry, runtime) = runtime(&tmp, RuntimeConfig::default());
    let session = runtime.get_or_spawn("mock").await.expect("handshake");
    let adapter = HostSessionAdapter::new(session.clone());

    for i in 0..ITERATIONS {
        let (tx, mut rx) = mpsc::channel(16);
        let dropped_before = adapter.dropped_notifications();
        let collector = tokio::spawn(async move {
            let mut sum = 0u64;
            while let Some(event) = rx.recv().await {
                if let ParseEvent::Batch(batch) = event {
                    sum += batch.records.len() as u64;
                }
            }
            sum
        });
        let records_total = adapter
            .parse_stream(
                ParseParams {
                    file_id: FILE_ID.to_string(),
                    options: None,
                },
                tx,
            )
            .await
            .expect("parse_stream");
        let sum = collector.await.expect("collector finished");
        let dropped = adapter.dropped_notifications() - dropped_before;
        assert_eq!(records_total, RECORDS_TOTAL as u64, "iteration {i}");
        assert_eq!(
            sum + dropped * PER_BATCH as u64,
            RECORDS_TOTAL as u64,
            "iteration {i}: Σ批次条数 + dropped == records_total（竞态回归）"
        );
    }

    runtime.shutdown_all().await;
}

/// 回归（P3-06 竞态）：sink 端延迟消费模拟慢消费者——sink 有界、满则丢并
/// 计数，断言不变式 `records_total == Σ成功送入 sink 的条数 + dropped 计数`。
/// 满则丢是 §4.1 有意设计，不要求零丢弃；abort 截断在慢消费下必然破坏该
/// 一致不变式（缓冲批次被静默丢弃且不计数）。
#[tokio::test]
async fn parse_stream_slow_consumer_keeps_count_invariant() {
    const BATCHES: usize = 200;
    const PER_BATCH: usize = 2;
    const RECORDS_TOTAL: usize = BATCHES * PER_BATCH;

    let tmp = TempDir::new("slow-consumer");
    let script = tmp.path().join("slow_consumer.ndjson");
    fs::write(
        &script,
        parse_burst_script(
            BATCHES,
            PER_BATCH,
            &format!(r#"{{"kind":"reply","method":"parse","result":{{"records_total":{RECORDS_TOTAL}}}}}"#),
        ),
    )
    .expect("write slow-consumer script");
    install_mock_plugin(&tmp.path().join("mock"), &script);
    let (_registry, runtime) = runtime(&tmp, RuntimeConfig::default());
    let session = runtime.get_or_spawn("mock").await.expect("handshake");
    let adapter = HostSessionAdapter::new(session.clone());

    // 容量 4 的有界 sink + 每事件 5ms 消费延迟 → 必然触发满则丢。
    let (tx, mut rx) = mpsc::channel(4);
    let collector = tokio::spawn(async move {
        let mut sum = 0u64;
        while let Some(event) = rx.recv().await {
            if let ParseEvent::Batch(batch) = event {
                sum += batch.records.len() as u64;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        sum
    });
    let records_total = adapter
        .parse_stream(
            ParseParams {
                file_id: FILE_ID.to_string(),
                options: None,
            },
            tx,
        )
        .await
        .expect("parse_stream");
    let sum = collector.await.expect("collector finished");
    let dropped = adapter.dropped_notifications();
    assert!(
        dropped > 0,
        "慢消费者必须触发满则丢路径（否则用例未命中场景）"
    );
    assert_eq!(
        sum + dropped * PER_BATCH as u64,
        records_total,
        "records_total == Σ送入 sink 条数 + dropped 计数（§4.1 一致不变式）"
    );

    runtime.shutdown_all().await;
}

/// 回归（P3-06 竞态）：parse 失败（Err 路径）同样 notify 让转发任务排空后
/// 退出——不 abort、不留悬挂任务。剧本在 parse 块推送批次后以 error 回复；
/// 断言错误返回前已缓冲批次仍全部送达 sink，且转发任务退出关闭 sink
/// （collector 因通道关闭收尾 → 无悬挂任务）。
#[tokio::test]
async fn parse_stream_error_path_drains_and_exits() {
    const BATCHES: usize = 3;
    const PER_BATCH: usize = 2;

    let tmp = TempDir::new("parse-error");
    let script = tmp.path().join("parse_error.ndjson");
    fs::write(
        &script,
        parse_burst_script(
            BATCHES,
            PER_BATCH,
            r#"{"kind":"reply","method":"parse","error":{"code":-32003,"message":"parse failed"}}"#,
        ),
    )
    .expect("write parse-error script");
    install_mock_plugin(&tmp.path().join("mock"), &script);
    let (_registry, runtime) = runtime(&tmp, RuntimeConfig::default());
    let session = runtime.get_or_spawn("mock").await.expect("handshake");
    let adapter = HostSessionAdapter::new(session.clone());

    let (tx, mut rx) = mpsc::channel(16);
    let collector = tokio::spawn(async move {
        let mut sum = 0u64;
        while let Some(event) = rx.recv().await {
            if let ParseEvent::Batch(batch) = event {
                sum += batch.records.len() as u64;
            }
        }
        sum
    });
    let result = adapter
        .parse_stream(
            ParseParams {
                file_id: FILE_ID.to_string(),
                options: None,
            },
            tx,
        )
        .await;
    assert!(
        matches!(result, Err(SessionError::Plugin { code: -32003, .. })),
        "parse 错误原样映射为 SessionError::Plugin：{result:?}"
    );
    let sum = collector.await.expect("collector finished");
    assert_eq!(
        sum,
        (BATCHES * PER_BATCH) as u64,
        "Err 路径已缓冲批次仍全部转发（排空后退出，无截断）"
    );

    runtime.shutdown_all().await;
}
