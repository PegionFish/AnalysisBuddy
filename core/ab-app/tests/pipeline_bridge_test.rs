//! P3-02 集成测试：`ImportCoordinator` 对 mock-plugin 回放器（真实进程，
//! `tools/mock-plugin`）走 导入→解析→查询 全链路（pipeline.md §1.1），并覆盖
//! key_values 部分失败语义（ipc-ui.md §1.6）与事件集快照（ipc-ui.md §2）。

mod common;

use std::fs;
use std::sync::Arc;

use ab_host::{PluginRuntime, RuntimeConfig};
use ab_pipeline::{run_query, MetricRef, PipelineEvent, QueryRequest, SessionRegistry, Store};

use ab_app::events::{self, ProgressThrottle};
use ab_app::pipeline_bridge::{
    query_key_values, ImportCoordinator, ImportStatus, KeyValuesError, PipelineConfig,
};

use common::{
    batch_line, can_handle_line, happy_script, init_line, install_plugin, key_values_line,
    load_file_line, parse_line, progress_line, record_json, repo_script, runtime, schema_line,
    shutdown_line, TempDir,
};

/// happy_path 剧本内嵌 file_id（适配器按 parse file_id 过滤，剧本与协调器
/// 必须一致——协调器侧经 `PipelineConfig.file_id_fn` 注入固定 id）。
const HAPPY_FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

fn write_csv(tmp: &TempDir, name: &str) -> std::path::PathBuf {
    let path = tmp.path().join(name);
    fs::write(&path, "timestamp,fps,frame_ms\n1785600000123,59.8,16.7\n").expect("write csv");
    path
}

fn coordinator(
    tmp: &TempDir,
    config: PipelineConfig,
) -> (
    Arc<ImportCoordinator>,
    Arc<PluginRuntime>,
    tokio::sync::mpsc::UnboundedReceiver<PipelineEvent>,
) {
    let (registry, runtime) = runtime(tmp, RuntimeConfig::default());
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = Arc::new(runtime);
    let coordinator = Arc::new(ImportCoordinator::with_config(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
        events_tx,
        runtime.clone(),
        registry,
        config,
    ));
    (coordinator, runtime, events_rx)
}

/// DoD：对 mock-plugin happy_path 剧本走 导入→parse→查询，断言
/// `run_query` 切片点数总和 == 剧本 Record 总数（3）、无降采样；
/// 同时验证 PipelineEvent 事件集（ipc-ui.md §2.1 command 状态翻转）。
#[tokio::test]
async fn import_parse_query_happy_path_matches_script_record_total() {
    let tmp = TempDir::new("happy");
    install_plugin(
        &tmp.path().join("mock"),
        "mock",
        &repo_script("happy_path.ndjson"),
    );
    let csv = write_csv(&tmp, "match.csv");

    let config = PipelineConfig {
        file_id_fn: Some(Arc::new(|_| HAPPY_FILE_ID.to_string())),
        ..PipelineConfig::default()
    };
    let (coordinator, _runtime, mut events_rx) = coordinator(&tmp, config);

    let outcomes = coordinator.import_files(std::slice::from_ref(&csv)).await;
    assert_eq!(outcomes.len(), 1);
    let outcome = &outcomes[0];
    assert_eq!(
        outcome.status,
        ImportStatus::Ready,
        "happy_path 全流程 Ready"
    );
    assert_eq!(outcome.file_id.as_deref(), Some(HAPPY_FILE_ID));
    let matched = outcome.matched_plugin.as_ref().expect("auto matched");
    assert_eq!(matched.plugin_id, "mock");
    assert!(!outcome.needs_user_choice);

    // 查询全量：Σ 切片点数 == 剧本 Record 总数（3），happy 数据量低于预算。
    let metrics = vec![
        MetricRef {
            file_id: HAPPY_FILE_ID.to_string(),
            metric: "fps".to_string(),
        },
        MetricRef {
            file_id: HAPPY_FILE_ID.to_string(),
            metric: "frame_ms".to_string(),
        },
        MetricRef {
            file_id: HAPPY_FILE_ID.to_string(),
            metric: "player_hp".to_string(),
        },
    ];
    let slices = run_query(
        coordinator.store(),
        &QueryRequest {
            metrics,
            t0_ms: 0,
            t1_ms: 2_000_000_000_000,
            max_points_per_series: 4000,
        },
    );
    let total: usize = slices.iter().map(|s| s.ts.len()).sum();
    assert_eq!(total, 3, "Σ 切片点数 == 剧本 Record 总数（DoD）");
    assert!(
        slices.iter().all(|s| !s.downsampled),
        "happy 数据量低于预算 → 不降采样"
    );

    // key_values 单文件查询：同序、成功。
    let kv = query_key_values(
        coordinator.registry(),
        coordinator.file_index(),
        &[HAPPY_FILE_ID.to_string()],
        1785603599870,
        coordinator.key_values_timeout(),
    )
    .await;
    assert_eq!(kv.len(), 1);
    assert_eq!(kv[0].file_id, HAPPY_FILE_ID);
    assert_eq!(kv[0].plugin_id, "mock");
    assert_eq!(kv[0].result.as_ref().expect("kv ok").len(), 3);

    // 事件集快照（pipeline.md §1.1 时序；ipc-ui.md §2.1 状态翻转）。
    let mut events: Vec<PipelineEvent> = Vec::new();
    while let Ok(event) = events_rx.try_recv() {
        events.push(event);
    }
    let kinds = |e: &PipelineEvent| matches!(e, PipelineEvent::ImportStarted { .. });
    assert!(events.iter().any(kinds), "ImportStarted 已发");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::MatchCandidates { .. })),
        "MatchCandidates 已发"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::PluginSelected { by: "auto", .. })),
        "PluginSelected(by auto) 已发"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::FileLoaded { .. })),
        "FileLoaded 已发"
    );
    let progress_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::ParseProgress { .. }))
        .count();
    assert_eq!(progress_count, 2, "happy_path 两条 progress 通知全部透传");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::ParseCompleted { .. })),
        "ParseCompleted 已发"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::QueryReady { .. })),
        "QueryReady 已发（查询 API 可用）"
    );

    // PipelineEvent → ab://progress 载荷快照（§2.1 ProgressPayload 字段集）。
    let mut throttle = ProgressThrottle::new();
    let mut on_wire = Vec::new();
    for event in events {
        for emitted in events::convert_pipeline(event, &mut throttle) {
            on_wire.push(emitted);
        }
    }
    assert!(!on_wire.is_empty(), "progress 已上线");
    assert!(
        on_wire.iter().all(|e| e.channel == events::EV_PROGRESS),
        "PipelineEvent 仅产生 ab://progress（§2.1）"
    );
    // 首条 = 剧本第一条 progress（0.5/0）；同窗口内后续被 100ms 节流覆盖，
    // flush 后补发最新值（1.0/2）。
    let payload = match &on_wire[0].payload {
        events::EventPayload::Progress(p) => p,
        other => panic!("expected progress payload, got {other:?}"),
    };
    assert_eq!(payload.file_id, HAPPY_FILE_ID);
    assert_eq!(
        serde_json::to_value(payload).expect("serialize"),
        serde_json::json!({
            "file_id": HAPPY_FILE_ID,
            "percent": 0.5,
            "records_so_far": 0,
        }),
        "§2.1 ProgressPayload 快照（bytes_read 省略键）"
    );
    let flushed = throttle.flush();
    assert!(
        !flushed.is_empty() && flushed.last().expect("last").records_so_far == 2,
        "节流窗口到期/冲刷补发最新值（1.0/2）"
    );

    // command 状态翻转：卸载后 frozen 集合清空（ipc-ui.md §2.1）。
    coordinator.unload_file(HAPPY_FILE_ID).await;
    assert!(coordinator.list_frozen().is_empty(), "卸载后文件不再可查询");
    let mut remaining = Vec::new();
    while let Ok(event) = events_rx.try_recv() {
        remaining.push(event);
    }
    assert!(
        remaining
            .iter()
            .any(|e| matches!(e, PipelineEvent::FileUnloaded { .. })),
        "FileUnloaded 已发"
    );
}

/// DoD：超预算序列 → LTTB 降采样（`downsampled == true`，点数 == 预算）。
#[tokio::test]
async fn over_budget_series_is_downsampled() {
    let tmp = TempDir::new("over-budget");
    const FILE_ID: &str = "f5c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";
    const N: u64 = 6000;

    let records: Vec<String> = (0..N)
        .map(|i| record_json(i as i64, "fps", (i % 97) as f64))
        .collect();
    let script = [
        init_line("mock"),
        can_handle_line(),
        schema_line(&["fps"]),
        load_file_line(),
        progress_line(FILE_ID, 0.5, 0),
        batch_line(FILE_ID, 0, &records.join(",")),
        parse_line(N),
        key_values_line(""),
        shutdown_line(),
    ]
    .join("\n");
    let script_path = tmp.path().join("over_budget.ndjson");
    fs::write(&script_path, script).expect("write script");
    install_plugin(&tmp.path().join("mock"), "mock", &script_path);
    let csv = write_csv(&tmp, "big.csv");

    let config = PipelineConfig {
        file_id_fn: Some(Arc::new(|_| FILE_ID.to_string())),
        ..PipelineConfig::default()
    };
    let (coordinator, _runtime, mut _events) = coordinator(&tmp, config);

    let outcomes = coordinator.import_files(&[csv]).await;
    assert_eq!(
        outcomes[0].status,
        ImportStatus::Ready,
        "6000 点剧本导入成功"
    );

    // 前端固定预算 4000（ipc-ui.md §5.2）：6000 > 4000 → 降采样。
    let slices = run_query(
        coordinator.store(),
        &QueryRequest {
            metrics: vec![MetricRef {
                file_id: FILE_ID.to_string(),
                metric: "fps".to_string(),
            }],
            t0_ms: 0,
            t1_ms: 2_000_000_000_000,
            max_points_per_series: 4000,
        },
    );
    assert_eq!(slices.len(), 1);
    let slice = &slices[0];
    assert!(slice.downsampled, "超预算序列 downsampled == true（DoD）");
    assert_eq!(slice.ts.len(), 4000, "LTTB 输出点数 == 预算");
    assert_eq!(slice.ts.first(), Some(&0), "首点保留");
    assert_eq!(slice.ts.last(), Some(&(N as i64 - 1)), "末点保留");
    assert_eq!(slice.values.len(), 4000);
}

/// DoD（ipc-ui.md §1.6）：一个文件映射到超时剧本（key_values 阻塞 30s），
/// 其余照常返回；结果同序、永不整体失败、超时项独立标记。
#[tokio::test]
async fn key_values_at_partial_failure_isolates_timeout_file() {
    let tmp = TempDir::new("kv-partial");
    const FILE_A: &str = "f6c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";
    const FILE_B: &str = "f7c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

    // 插件 A：正常 key_values；插件 B：key_values 块先 sleep 5s（远超注入的
    // 200ms 超时窗口 → 看门狗超时路径；5s 上界也约束了极端情况下未 kill 时的
    // 自然退出，避免测试悬挂）。
    let script_a = happy_script("mock-a", FILE_A);
    let script_b = happy_script("mock-b", FILE_B).replace(
        &key_values_line(r#"{"key":"scene","value":"boss"}"#),
        &format!(
            "{}\n{}",
            r#"{"kind":"sleep","ms":5000}"#,
            key_values_line(r#"{"key":"scene","value":"boss"}"#)
        ),
    );
    let script_a_path = tmp.path().join("a.ndjson");
    let script_b_path = tmp.path().join("b.ndjson");
    fs::write(&script_a_path, script_a).expect("write script a");
    fs::write(&script_b_path, script_b).expect("write script b");
    install_plugin(&tmp.path().join("mock-a"), "mock-a", &script_a_path);
    install_plugin(&tmp.path().join("mock-b"), "mock-b", &script_b_path);
    let csv_a = write_csv(&tmp, "a.csv");
    let csv_b = write_csv(&tmp, "b.csv");

    // 固定 file_id：按 seq 奇偶分配（两文件并行导入，seq 分配有竞态）。
    let config = PipelineConfig {
        file_id_fn: Some(Arc::new(|seq: u64| {
            if seq.is_multiple_of(2) {
                FILE_A.to_string()
            } else {
                FILE_B.to_string()
            }
        })),
        key_values_timeout: std::time::Duration::from_millis(200),
        ..PipelineConfig::default()
    };
    let (coordinator, runtime, _events_rx) = coordinator(&tmp, config);

    // 两插件 manifest 均认领 csv（扩展名预筛），can_handle 剧本无法按文件区分
    // → 以用户手选覆盖入口（ipc-ui.md §1.2 overrides 语义）显式指定插件。
    let outcome_a = coordinator.import_with_plugin(csv_a, "mock-a").await;
    let outcome_b = coordinator.import_with_plugin(csv_b, "mock-b").await;
    assert_eq!(
        outcome_a.status,
        ImportStatus::Ready,
        "A 导入 Ready: {outcome_a:?}"
    );
    assert_eq!(
        outcome_b.status,
        ImportStatus::Ready,
        "B 导入 Ready: {outcome_b:?}"
    );

    // 部分失败：A 正常返回条目；B 超时（200ms 注入窗口），互不阻塞。
    let kv = query_key_values(
        coordinator.registry(),
        coordinator.file_index(),
        &[FILE_A.to_string(), FILE_B.to_string()],
        1000,
        std::time::Duration::from_millis(200),
    )
    .await;
    assert_eq!(kv.len(), 2, "与入参同序同长（§1.6）");
    assert_eq!(kv[0].file_id, FILE_A);
    assert_eq!(kv[1].file_id, FILE_B);
    assert!(
        !kv[0].result.as_ref().expect("A 成功").is_empty(),
        "A 文件照常返回条目"
    );
    assert_eq!(
        kv[1].result.as_ref().expect_err("B 失败"),
        &KeyValuesError::Timeout,
        "超时剧本文件独立标记 Timeout，不影响其余（§1.6/§4.2）"
    );

    // 未映射文件 → FileNotReady（永不 reject 的另一形态）。
    let unknown = query_key_values(
        coordinator.registry(),
        coordinator.file_index(),
        &["ghost-file".to_string()],
        1000,
        std::time::Duration::from_millis(200),
    )
    .await;
    assert!(matches!(
        &unknown[0].result,
        Err(KeyValuesError::FileNotReady(_))
    ));
    // 显式停机：mock-b 仍阻塞在 key_values 剧本 sleep 中，走 shutdown → 3s
    // 预算 → kill 路径回收（sweep 兜底仅依赖 Drop，此处显式以确保确定性）。
    runtime.shutdown_all().await;
}
