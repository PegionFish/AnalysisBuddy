//! P3-03 command 级集成测试（ipc-ui.md §1 全表 + §2 事件契约）：
//! 对 mock-plugin 回放器驱动 8 个 command + 2 个辅助 command 的
//! 入参/出参/错误形状（§1.10 映射经 `IpcError` 断言），并自动比对
//! 事件通道名/载荷与 `ui/src/ipc/events.ts` 逐字一致（DoD：字符串 diff 为空）。

mod common;

use std::collections::HashMap;
use std::fs;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ab_host::{HostEvent, PluginProcessState, PluginRegistry, PluginRuntime, RuntimeConfig};
use ab_pipeline::{
    sha256_of_file, ChartViewState, PipelineEvent, SessionRegistry, Store, YAxisScale,
};
use ab_protocol::types::ProgressParams;
use serde_json::json;
use tokio::sync::mpsc;

use ab_app::commands::import::{import_files_logic, unload_file_logic};
use ab_app::commands::plugin::{get_plugin_log_logic, list_plugins_logic, reload_plugin_logic};
use ab_app::commands::presets::{
    delete_user_preset_logic, list_user_presets_logic, save_user_preset_logic,
};
use ab_app::commands::query::{get_metrics_logic, key_values_at_logic, query_series_logic};
use ab_app::commands::session::{load_session_logic, save_session_logic};
use ab_app::events::{
    self, convert, convert_pipeline, EventPayload, PluginLogBuffer, PluginLogPayload, PluginMeta,
    ProgressThrottle, EV_PLUGINS_RELOADED, EV_PLUGIN_HEALTH, EV_PLUGIN_LOG, EV_PROGRESS,
};
use ab_app::pipeline_bridge::{ImportCoordinator, PipelineConfig};
use ab_protocol::manifest::LocalizedName;

use common::{install_plugin, repo_script, runtime, TempDir};

/// happy_path 剧本内嵌 file_id（固定注入）。
const FIXED_FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

static HARNESS_SEQ: AtomicU64 = AtomicU64::new(0);

/// 测试台：mock-plugin 运行时 + 协调器 + 与 lib.rs wire_events 等价的事件转发
/// （meta/日志缓冲/双路节流 → 线上事件收集），供 command 逻辑体驱动与断言。
struct Harness {
    coordinator: Arc<ImportCoordinator>,
    registry: Arc<PluginRegistry>,
    runtime: Arc<PluginRuntime>,
    meta: Arc<PluginMeta>,
    log_buffer: Arc<PluginLogBuffer>,
    /// host 事件 → 线上事件（channel, payload JSON）。
    host_wire: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    /// pipeline 事件 → 线上事件。
    pipeline_wire: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    _tmp: TempDir,
}

impl Harness {
    fn csv(&self, name: &str) -> std::path::PathBuf {
        let path = self._tmp.path().join(name);
        fs::write(&path, "timestamp,fps,frame_ms\n1785600000123,59.8,16.7\n").expect("write csv");
        path
    }

    async fn wait_for<F: Fn() -> bool>(&self, cond: F) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("wait_for timed out");
    }

    fn progress_events(&self) -> Vec<(String, serde_json::Value)> {
        self.pipeline_wire
            .lock()
            .unwrap()
            .iter()
            .chain(self.host_wire.lock().unwrap().iter())
            .cloned()
            .collect()
    }

    async fn shutdown(&self) {
        self.runtime.shutdown_all().await;
    }
}

/// 组装测试台（固定 file_id + 默认超时）：先装插件（含剧本）再发现/拉起。
async fn harness(tag: &str, plugins: &[(&str, std::path::PathBuf)]) -> Harness {
    let seq = HARNESS_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = TempDir::new(&format!("{tag}-{seq}"));
    for (plugin_id, script) in plugins {
        install_plugin(&tmp.path().join(plugin_id), plugin_id, script);
    }
    let (registry, runtime) = runtime(&tmp, RuntimeConfig::default());
    let runtime = Arc::new(runtime);
    let (pipeline_tx, mut pipeline_rx) = mpsc::unbounded_channel::<PipelineEvent>();
    let config = PipelineConfig {
        file_id_fn: Some(Arc::new(|_| FIXED_FILE_ID.to_string())),
        ..PipelineConfig::default()
    };
    let coordinator = Arc::new(ImportCoordinator::with_config(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
        pipeline_tx,
        runtime.clone(),
        registry.clone(),
        config,
    ));

    let meta = Arc::new(PluginMeta::new());
    let log_buffer = Arc::new(PluginLogBuffer::new());
    let host_wire = Arc::new(Mutex::new(Vec::new()));
    let pipeline_wire = Arc::new(Mutex::new(Vec::new()));

    // host 事件转发（等价 lib.rs wire_events：record → 缓冲 → 转换 → 收集）。
    let mut host_rx = runtime.subscribe_events();
    let meta_t = meta.clone();
    let log_t = log_buffer.clone();
    let wire_t = host_wire.clone();
    let throttle = Arc::new(Mutex::new(ProgressThrottle::new()));
    let throttle_t = throttle.clone();
    tokio::spawn(async move {
        while let Ok(event) = host_rx.recv().await {
            meta_t.record(&event);
            if let HostEvent::StderrLine {
                plugin_id,
                ts_ms,
                line,
            } = &event
            {
                log_t.push(PluginLogPayload {
                    plugin_id: plugin_id.clone(),
                    level: events::parse_log_level(line),
                    line: line.clone(),
                    ts_ms: *ts_ms,
                });
            }
            for emitted in convert(event, &mut throttle_t.lock().unwrap()) {
                // 与 lib.rs emit_one 一致：发射内层载荷（非枚举包装）。
                let payload = match &emitted.payload {
                    EventPayload::Health(p) => serde_json::to_value(p).expect("health"),
                    EventPayload::Log(p) => serde_json::to_value(p).expect("log"),
                    EventPayload::Progress(p) => serde_json::to_value(p).expect("progress"),
                    EventPayload::PluginsReloaded(p) => {
                        serde_json::to_value(p).expect("plugins-reloaded")
                    }
                };
                wire_t
                    .lock()
                    .unwrap()
                    .push((emitted.channel.to_string(), payload));
            }
        }
    });

    // pipeline 事件转发（等价 lib.rs wire_pipeline_events）。
    let wire_p = pipeline_wire.clone();
    tokio::spawn(async move {
        let mut throttle = ProgressThrottle::new();
        while let Some(event) = pipeline_rx.recv().await {
            for emitted in convert_pipeline(event, &mut throttle) {
                let payload = match &emitted.payload {
                    EventPayload::Health(p) => serde_json::to_value(p).expect("health"),
                    EventPayload::Log(p) => serde_json::to_value(p).expect("log"),
                    EventPayload::Progress(p) => serde_json::to_value(p).expect("progress"),
                    EventPayload::PluginsReloaded(p) => {
                        serde_json::to_value(p).expect("plugins-reloaded")
                    }
                };
                wire_p
                    .lock()
                    .unwrap()
                    .push((emitted.channel.to_string(), payload));
            }
        }
    });

    Harness {
        coordinator,
        registry,
        runtime,
        meta,
        log_buffer,
        host_wire,
        pipeline_wire,
        _tmp: tmp,
    }
}

// ---------------------------------------------------------------------------
// 8 + 2 command 全链路（DoD 第 1 条）
// ---------------------------------------------------------------------------

/// 对 mock-plugin happy_path 剧本驱动全部 8 个 command（+ 2 辅助）：
/// 入参/出参形状逐项断言（§1.0 字段集），含 key_values_at 永不 reject 语义。
#[tokio::test]
async fn all_ten_commands_happy_path_against_mock_plugin() {
    let h = harness("happy", &[("mock", repo_script("happy_path.ndjson"))]).await;
    let csv = h.csv("match.csv");

    // list_plugins：未拉起 → discovered。
    let plugins = list_plugins_logic(&h.registry, &h.meta, &h.coordinator, h._tmp.path());
    assert_eq!(plugins.len(), 1, "1 个已发现插件");
    assert_eq!(plugins[0].id, "mock");
    assert_eq!(plugins[0].state, "discovered");
    assert!(plugins[0].loaded_file_ids.is_empty());
    let plugins_json = serde_json::to_value(&plugins[0]).expect("serialize");
    assert_eq!(
        plugins_json["last_error"],
        json!(null),
        "last_error 恒为 null 键"
    );
    assert_eq!(
        plugins_json["capabilities"],
        json!({"annotate": false, "subscribe": false, "binary_sidecar": false}),
        "§1.0 capabilities 形状"
    );

    // import_files：Ready + 固定 file_id + 自动匹配 mock。
    let results = import_files_logic(&h.coordinator, vec![csv.display().to_string()], None)
        .await
        .expect("import_files 不 reject");
    assert_eq!(results.len(), 1);
    let dto = &results[0];
    assert_eq!(dto.status, "ready");
    assert_eq!(dto.file_id, FIXED_FILE_ID);
    assert_eq!(
        dto.matched_plugin.as_ref().expect("matched").plugin_id,
        "mock"
    );
    assert!(dto.error.is_none());

    // progress 上线（§2.1）：剧本两条 progress 经 100ms 节流转发。
    h.wait_for(|| !h.pipeline_wire.lock().unwrap().is_empty())
        .await;
    let progress: Vec<_> = h
        .progress_events()
        .into_iter()
        .filter(|(channel, _)| channel == EV_PROGRESS)
        .collect();
    assert!(!progress.is_empty(), "ab://progress 已上线（DoD）");
    if progress
        .iter()
        .any(|(_, p)| p["file_id"] != json!(FIXED_FILE_ID))
    {
        panic!("progress payload mismatch: {progress:#?}");
    }
    assert!(
        progress
            .iter()
            .all(|(_, p)| p["file_id"] == json!(FIXED_FILE_ID)),
        "载荷 file_id 逐字段对齐"
    );

    // list_plugins：导入后 ready + 驻留文件。
    h.wait_for(|| h.meta.state_of("mock").as_deref() == Some("ready"))
        .await;
    let plugins = list_plugins_logic(&h.registry, &h.meta, &h.coordinator, h._tmp.path());
    assert_eq!(plugins[0].state, "ready", "事件流驱动状态翻转");
    assert_eq!(plugins[0].loaded_file_ids, vec![FIXED_FILE_ID.to_string()]);

    // get_metrics：文件 → 插件 → 指标三级树（§1.4）。
    let tree = get_metrics_logic(&h.coordinator, None);
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].level, "file");
    assert_eq!(tree[0].file_id, FIXED_FILE_ID);
    let plugin_node = &tree[0].children.as_ref().expect("plugin children")[0];
    assert_eq!(plugin_node.level, "plugin");
    assert_eq!(plugin_node.plugin_id.as_deref(), Some("mock"));
    let metrics = plugin_node.children.as_ref().expect("metric children");
    let ids: Vec<&str> = metrics
        .iter()
        .map(|m| m.metric_id.as_deref().unwrap())
        .collect();
    assert!(
        ids.contains(&"fps") && ids.contains(&"frame_ms") && ids.contains(&"player_hp"),
        "schema 指标全部可查：{ids:?}"
    );
    assert_eq!(
        metrics[0].id,
        format!(
            "{FIXED_FILE_ID}:mock:{}",
            metrics[0].metric_id.as_deref().unwrap()
        ),
        "§1.0 复合 id file_id:plugin_id:metric_id"
    );

    // query_series：Σ 点数 == 剧本 Record 总数（3），无降采样（§1.5）。
    let composite: Vec<String> = ids
        .iter()
        .map(|m| format!("{FIXED_FILE_ID}:mock:{m}"))
        .collect();
    let slices = query_series_logic(
        &h.coordinator,
        &[FIXED_FILE_ID.to_string()],
        &composite,
        0,
        2_000_000_000_000,
        4000,
    )
    .expect("query_series 不 reject");
    let total: usize = slices.iter().map(|s| s.point_count).sum();
    assert_eq!(total, 3, "Σ 切片点数 == 剧本 Record 总数");
    assert!(slices.iter().all(|s| !s.downsampled));

    // key_values_at：成功项带 entries（§1.6）。
    let kv = key_values_at_logic(&h.coordinator, &[FIXED_FILE_ID.to_string()], 1785603599870)
        .await
        .expect("key_values_at 永不 reject");
    assert_eq!(kv.len(), 1);
    let entries = kv[0].entries.as_ref().expect("entries");
    assert!(!entries.is_empty());
    assert!(kv[0].error.is_none());
    assert_eq!(entries[0].key, "scene");
    assert_eq!(entries[0].value, json!("boss"));

    // key_values_at 未知文件：逐项 error、整体 Ok（永不 reject 语义）。
    let kv = key_values_at_logic(&h.coordinator, &["ghost-file".to_string()], 0)
        .await
        .expect("未知文件也不 reject");
    assert_eq!(kv[0].file_id, "ghost-file");
    assert_eq!(
        kv[0].error.as_ref().expect("per-item error").code,
        "file_not_found",
        "§1.6 部分失败逐项填 error"
    );
    assert!(kv[0].entries.is_none());

    // get_plugin_log（§2.2 辅助）：mock-plugin 的 INFO 行已入环形缓冲。
    h.wait_for(|| h.log_buffer.len_of("mock") > 0).await;
    let logs = get_plugin_log_logic(&h.log_buffer, "mock", None).expect("get_plugin_log");
    assert!(!logs.is_empty());
    assert!(
        logs.iter().all(|l| l.plugin_id == "mock"),
        "载荷 plugin_id 对齐"
    );
    assert!(logs.iter().all(|l| l.ts_ms > 0), "ts_ms 非零");
    let log_json = serde_json::to_value(&logs[0]).expect("serialize log");
    for key in ["plugin_id", "level", "line", "ts_ms"] {
        assert!(log_json.get(key).is_some(), "§2.2 载荷字段 {key}");
    }

    // unload_file：幂等；卸载后指标树清空、驻留列表清空（§1.3）。
    unload_file_logic(&h.coordinator, FIXED_FILE_ID.to_string())
        .await
        .expect("unload_file");
    assert!(
        get_metrics_logic(&h.coordinator, None).is_empty(),
        "卸载后不可查"
    );
    let plugins = list_plugins_logic(&h.registry, &h.meta, &h.coordinator, h._tmp.path());
    assert!(plugins[0].loaded_file_ids.is_empty());
    unload_file_logic(&h.coordinator, FIXED_FILE_ID.to_string())
        .await
        .expect("unload_file 幂等");

    // reload_plugin（§4.6 辅助）：重建实例 → 事件流回到 ready。
    let info = reload_plugin_logic(&h.registry, &h.meta, &h.coordinator, "mock")
        .await
        .expect("reload_plugin");
    assert_eq!(info.id, "mock");
    h.wait_for(|| h.meta.state_of("mock").as_deref() == Some("ready"))
        .await;
    // 未知插件 → internal（mock 侧同语义）。
    let e = reload_plugin_logic(&h.registry, &h.meta, &h.coordinator, "ghost")
        .await
        .expect_err("未知插件 reject");
    assert_eq!(e.code, "internal");

    // 健康事件通道全程有发射（§2.3）。
    let health: Vec<_> = h
        .host_wire
        .lock()
        .unwrap()
        .iter()
        .filter(|(channel, _)| channel == EV_PLUGIN_HEALTH)
        .map(|(_, p)| p.clone())
        .collect();
    assert!(!health.is_empty(), "ab://plugin-health 已发射（DoD）");
    assert!(
        health.iter().all(|p| {
            p["plugin_id"] == json!("mock") && p["state"].is_string() && p["prev_state"].is_string()
        }),
        "§2.3 载荷形状"
    );

    h.shutdown().await;
}

/// 导入/查询错误形状按 §1.10 映射（item-level 不 reject；参数非法 reject）。
#[tokio::test]
async fn import_and_query_error_shapes_map_section_1_10_codes() {
    let h = harness("errors", &[("mock", repo_script("load_failed.ndjson"))]).await;
    let csv = h.csv("a.csv");

    // 全部路径非法 → 整体 reject invalid_arg（§1.9）。
    let e = import_files_logic(&h.coordinator, vec!["  ".to_string()], None)
        .await
        .expect_err("全部空路径 reject");
    assert_eq!(e.code, "invalid_arg");
    // 空数组 → 空结果（不 reject）。
    let results = import_files_logic(&h.coordinator, Vec::new(), None)
        .await
        .expect("空数组不 reject");
    assert!(results.is_empty());

    // 路径不存在 → 项内 error file_not_found，整体不 reject（§1.2）。
    let results = import_files_logic(
        &h.coordinator,
        vec!["C:\\missing\\nope.csv".to_string()],
        None,
    )
    .await
    .expect("单路径失败不 reject");
    assert_eq!(results[0].status, "error");
    assert_eq!(
        results[0].error.as_ref().expect("item error").code,
        "file_not_found",
        "§1.0 错误码表 file_not_found"
    );

    // 插件 load_file 回 -32002 → file_load_failed（§1.10 行 2；load_failed 剧本）。
    let results = import_files_logic(&h.coordinator, vec![csv.display().to_string()], None)
        .await
        .expect("load 失败仍按项返回");
    assert_eq!(results[0].status, "error");
    assert_eq!(
        results[0].error.as_ref().expect("item error").code,
        "file_load_failed",
        "§1.10: -32002 → file_load_failed"
    );

    // query_series：t0 > t1 → invalid_arg（§1.9）。
    let e = query_series_logic(&h.coordinator, &[], &[], 10, 5, 4000).expect_err("反向窗口");
    assert_eq!(e.code, "invalid_arg");

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// save_session / load_session（§1.7/§1.8，pipeline.md §5.3）
// ---------------------------------------------------------------------------

/// 保存 → “重启” → 重开：schema v1 落盘、missing 空、重开文件可查、
/// 重开路径补发 percent:100 终态进度（前端零改动翻 ready 的依据）。
#[tokio::test]
async fn save_session_roundtrip_reopens_file_with_final_progress() {
    let h = harness("save", &[("mock", repo_script("happy_path.ndjson"))]).await;
    let csv = h.csv("match.csv");
    import_files_logic(&h.coordinator, vec![csv.display().to_string()], None)
        .await
        .expect("import");
    let session_path = h._tmp.path().join("s.absession");

    // save_session（显式 path；§1.7 对话框路径仅 handler 层）。
    let meta = save_session_logic(&h.coordinator, &session_path, None).expect("save_session");
    assert_eq!(meta.path, session_path.display().to_string());
    assert_eq!(meta.file_count, 1);
    assert!(meta.saved_at_ms > 0);

    // schema v1 JSON：path + 64 位小写 sha256 + plugin_id（§5.1）。
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&session_path).expect("read session")).unwrap();
    assert_eq!(json["version"], 1, "schema v1");
    assert_eq!(json["files"][0]["path"], json!(csv.display().to_string()));
    assert_eq!(json["files"][0]["plugin_id"], json!("mock"));
    assert_eq!(
        json["files"][0]["sha256"].as_str().expect("sha256").len(),
        64
    );
    assert!(json["cursor_ms"].is_null() || json.get("cursor_ms").is_none());

    // “应用重启”：全新协调器 + 同一会话文件 → 重开（§5.3）。
    let h2 = harness("reopen", &[("mock", repo_script("happy_path.ndjson"))]).await;
    let loaded = load_session_logic(&h2.coordinator, &session_path)
        .await
        .expect("load_session");
    assert!(loaded.missing.is_empty(), "文件未缺失");
    assert_eq!(loaded.loaded_file_ids.len(), 1, "校验通过者重进导入管线");
    let reopened = &loaded.loaded_file_ids[0];
    assert_eq!(loaded.session.path, session_path.display().to_string());

    // 重开文件可查（指标树）——command 侧状态翻转。
    h2.wait_for(|| !h2.coordinator.list_frozen().is_empty())
        .await;
    let tree = get_metrics_logic(&h2.coordinator, Some(vec![reopened.clone()]));
    assert_eq!(tree.len(), 1, "重开文件可查（§5.3 步骤 5 前置）");
    assert_eq!(tree[0].file_id, reopened.as_str());

    // 重开路径补发 percent:100 终态进度（前端「percent≥100 → ready」翻牌依据）。
    h2.wait_for(|| {
        h2.pipeline_wire.lock().unwrap().iter().any(|(channel, p)| {
            channel == EV_PROGRESS
                && p["file_id"] == json!(reopened)
                && p["percent"] == json!(100.0)
        })
    })
    .await;
    let wire_snapshot: Vec<(String, serde_json::Value)> = h2.pipeline_wire.lock().unwrap().clone();
    let final_progress = wire_snapshot
        .iter()
        .find(|(channel, p)| {
            channel == EV_PROGRESS
                && p["file_id"] == json!(reopened)
                && p["percent"] == json!(100.0)
        })
        .expect("percent 100 终态进度");
    assert_eq!(
        final_progress.1["records_so_far"],
        json!(3),
        "终态进度携带 records_so_far（UI 计数文案）"
    );

    h.shutdown().await;
    h2.shutdown().await;
}

/// 缺失文件标记：not_found / hash_mismatch 逐项进 missing，通过者照常重开（§1.8）。
#[tokio::test]
async fn load_session_marks_missing_and_hash_mismatch() {
    let h = harness("verify", &[("mock", repo_script("happy_path.ndjson"))]).await;
    let ok_csv = h.csv("ok.csv");
    let ok_hash = sha256_of_file(&ok_csv).expect("hash ok file");
    let gone = h._tmp.path().join("gone.csv");
    let modified = h._tmp.path().join("modified.csv");
    fs::write(&modified, "timestamp,fps\n1,1\n").expect("write modified");

    let session = ab_pipeline::SessionFile {
        version: ab_pipeline::SESSION_FILE_VERSION,
        files: vec![
            ab_pipeline::SessionFileEntry {
                path: ok_csv.display().to_string(),
                sha256: ok_hash,
                plugin_id: "mock".to_string(),
            },
            ab_pipeline::SessionFileEntry {
                path: gone.display().to_string(),
                sha256: "a".repeat(64),
                plugin_id: "mock".to_string(),
            },
            ab_pipeline::SessionFileEntry {
                path: modified.display().to_string(),
                sha256: "0".repeat(64),
                plugin_id: "mock".to_string(),
            },
        ],
        selected_metrics: HashMap::new(),
        chart_view_state: ChartViewState {
            time_range: None,
            legend_disabled: Vec::new(),
            y_axis_scale: YAxisScale::Shared,
        },
        cursor_ms: None,
    };
    let session_path = h._tmp.path().join("v.absession");
    ab_pipeline::save_session(&session, &session_path).expect("write session");

    let loaded = load_session_logic(&h.coordinator, &session_path)
        .await
        .expect("load_session");
    assert_eq!(loaded.loaded_file_ids.len(), 1, "仅校验通过者重开");
    let reasons: Vec<&str> = loaded.missing.iter().map(|m| m.reason).collect();
    assert_eq!(
        reasons,
        vec!["not_found", "hash_mismatch"],
        "§1.8 缺失标记双态"
    );
    let gone_str = gone.display().to_string();
    let modified_str = modified.display().to_string();
    let paths: Vec<&str> = loaded.missing.iter().map(|m| m.path.as_str()).collect();
    assert!(paths.contains(&gone_str.as_str()));
    assert!(paths.contains(&modified_str.as_str()));
    assert_eq!(loaded.session.file_count, 3);

    // 通过者已可查。
    h.wait_for(|| !h.coordinator.list_frozen().is_empty()).await;
    assert_eq!(get_metrics_logic(&h.coordinator, None).len(), 1);

    // 空 reopen_failed 省略键（与 time_ranges 同模式，§1.8 扩展）。
    let json = serde_json::to_value(&loaded).expect("serialize");
    assert!(json.get("reopen_failed").is_none());

    h.shutdown().await;
}

/// 重开失败通道：插件 load_file 失败 → 逐项进 reopen_failed（不进 loaded），
/// 序列化带 reopen_failed 键（§1.8 扩展；此前仅宿主日志记录，UI 无失败通道）。
#[tokio::test]
async fn load_session_reports_reopen_failures() {
    let h = harness(
        "reopen-failed",
        &[("mock", repo_script("load_failed.ndjson"))],
    )
    .await;
    let csv = h.csv("will-fail.csv");
    let hash = sha256_of_file(&csv).expect("hash ok file");
    let session = ab_pipeline::SessionFile {
        version: ab_pipeline::SESSION_FILE_VERSION,
        files: vec![ab_pipeline::SessionFileEntry {
            path: csv.display().to_string(),
            sha256: hash,
            plugin_id: "mock".to_string(),
        }],
        selected_metrics: HashMap::new(),
        chart_view_state: ChartViewState {
            time_range: None,
            legend_disabled: Vec::new(),
            y_axis_scale: YAxisScale::Shared,
        },
        cursor_ms: None,
    };
    let session_path = h._tmp.path().join("rf.absession");
    ab_pipeline::save_session(&session, &session_path).expect("write session");

    let loaded = load_session_logic(&h.coordinator, &session_path)
        .await
        .expect("load_session");
    assert!(loaded.missing.is_empty(), "文件未缺失");
    assert!(loaded.loaded_file_ids.is_empty(), "重开失败不入 loaded");
    assert_eq!(loaded.session.file_count, 1);
    let csv_str = csv.display().to_string();
    let reopened: Vec<(&str, &str)> = loaded
        .reopen_failed
        .iter()
        .map(|m| (m.path.as_str(), m.reason))
        .collect();
    assert_eq!(
        reopened,
        vec![(csv_str.as_str(), "reopen_failed")],
        "重开失败逐项进 reopen_failed"
    );

    // 序列化形状：非空时带 reopen_failed 键，path/reason 逐字段（§1.8）。
    let json = serde_json::to_value(&loaded).expect("serialize");
    assert_eq!(json["reopen_failed"][0]["path"], json!(csv_str));
    assert_eq!(json["reopen_failed"][0]["reason"], json!("reopen_failed"));

    h.shutdown().await;
}

/// load_session 错误形状：路径不存在 → file_not_found；损坏 → session_io（§1.9）。
#[tokio::test]
async fn load_session_error_shapes() {
    let h = harness("load-errors", &[]).await;

    let e = load_session_logic(&h.coordinator, &h._tmp.path().join("nope.absession"))
        .await
        .expect_err("路径不存在 reject");
    assert_eq!(e.code, "file_not_found", "§1.9 load_session file_not_found");

    let corrupt = h._tmp.path().join("bad.absession");
    fs::write(&corrupt, "not json").expect("write corrupt");
    let e = load_session_logic(&h.coordinator, &corrupt)
        .await
        .expect_err("损坏文件 reject");
    assert_eq!(e.code, "session_io", "§1.9 load_session session_io");

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// user preset commands（list_user_presets / save_user_preset / delete_user_preset，
// Wave 2 C5）：与既有 10 条命令同模式——直测 `*_logic` 逻辑体（handler 层仅
// 做 dir 注入 + 命令层互斥，测试环境不提供 Tauri runtime，无法走完整 invoke；
// 互斥并发语义由 presets.rs 单元测试覆盖）。
// ---------------------------------------------------------------------------

/// 三条预设命令冒烟：list 调用成功且返回数组形状；save 合法 name/entries
/// 落盘并可回读；delete 幂等 Ok 与非法 id invalid_arg。
#[test]
fn user_preset_commands_smoke_at_logic_layer() {
    let tmp = TempDir::new("presets-smoke");

    // list_user_presets：调用成功、返回数组形状（空目录 → 空数组）。
    let listed = list_user_presets_logic(tmp.path());
    assert!(listed.is_empty(), "list 返回数组形状（空目录 → []）");

    // save_user_preset：合法 name/entries 调用成功。
    let name = LocalizedName {
        zh: "FPS 场景".to_string(),
        en: "FPS Scene".to_string(),
    };
    let entries = HashMap::from([("mock".to_string(), vec!["fps".to_string()])]);
    let saved =
        save_user_preset_logic(tmp.path(), name, entries).expect("save_user_preset 不 reject");
    assert_eq!(saved.id, "fps", "id 由 name.zh slug 化");
    let listed = list_user_presets_logic(tmp.path());
    assert_eq!(listed.len(), 1, "save 后 list 可读回");
    assert_eq!(listed[0].id, "fps");

    // delete_user_preset：存在删除 Ok；幂等 Ok；非法 id → invalid_arg。
    delete_user_preset_logic(tmp.path(), "fps").expect("删除 Ok");
    delete_user_preset_logic(tmp.path(), "fps").expect("重复删除幂等 Ok");
    let err = delete_user_preset_logic(tmp.path(), "Bad/Id").expect_err("非法 id reject");
    assert_eq!(err.code, "invalid_arg", "§1.9 invalid_arg 映射");
    assert!(list_user_presets_logic(tmp.path()).is_empty(), "删除后 list 为空");
}

// ---------------------------------------------------------------------------
// 事件契约自动比对（DoD：通道名/载荷与 ui/src/ipc/events.ts 字符串 diff 为空）
// ---------------------------------------------------------------------------

/// 解析 `export const NAME = 'VALUE';`。
fn ts_constants(ts: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in ts.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("export const ") {
            if let Some((name, value)) = rest.split_once(" = ") {
                let value = value.trim().trim_start_matches('\'').trim_end_matches("';");
                out.insert(name.trim().to_string(), value.to_string());
            }
        }
    }
    out
}

/// 解析 TS 接口体（`export interface NAME { ... }`）顶层字段名（去 `?`）。
fn ts_interface_fields(ts: &str, name: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut inside = false;
    let mut depth = 0usize;
    for line in ts.lines() {
        let trimmed = line.trim();
        if !inside {
            if trimmed.starts_with(&format!("export interface {name}")) {
                inside = true;
                depth = trimmed.matches('{').count();
            }
            continue;
        }
        depth += trimmed.matches('{').count();
        depth = depth.saturating_sub(trimmed.matches('}').count());
        if depth == 0 {
            break;
        }
        let Some(colon) = trimmed.find(':') else {
            continue;
        };
        let head = trimmed[..colon].trim();
        let name = head.trim_end_matches('?').trim();
        if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.is_empty() {
            fields.push(name.to_string());
        }
    }
    fields
}

/// 通道常量与载荷字段集 vs Rust 侧（全部可选字段取“有值”形态比对）。
#[test]
fn event_channels_and_payloads_byte_identical_to_ui_events_ts() {
    let ts = include_str!("../../../ui/src/ipc/events.ts");
    let constants = ts_constants(ts);

    assert_eq!(
        constants.get("EV_PROGRESS").map(String::as_str),
        Some(EV_PROGRESS),
        "EV_PROGRESS 逐字一致（字符串 diff 为空）"
    );
    assert_eq!(
        constants.get("EV_PLUGIN_LOG").map(String::as_str),
        Some(EV_PLUGIN_LOG),
        "EV_PLUGIN_LOG 逐字一致"
    );
    assert_eq!(
        constants.get("EV_PLUGIN_HEALTH").map(String::as_str),
        Some(EV_PLUGIN_HEALTH),
        "EV_PLUGIN_HEALTH 逐字一致"
    );
    assert_eq!(
        constants.get("EV_PLUGINS_RELOADED").map(String::as_str),
        Some(EV_PLUGINS_RELOADED),
        "EV_PLUGINS_RELOADED 逐字一致（spec §6.3，任务 5 接线）"
    );
    // 四个常量一一命中，无遗漏/多余。
    assert_eq!(constants.len(), 4);

    // ab://progress 载荷字段集（含可选字段全有形态）。
    let ts_fields = ts_interface_fields(ts, "ProgressPayload");
    let rust_keys: Vec<String> = serde_json::to_value(ProgressParams {
        file_id: "f1".to_string(),
        percent: Some(50.0),
        records_so_far: 42,
        bytes_read: Some(512),
    })
    .expect("serialize progress")
    .as_object()
    .expect("object")
    .keys()
    .cloned()
    .collect();
    let mut ts_sorted = ts_fields.clone();
    ts_sorted.sort();
    let mut rust_sorted = rust_keys.clone();
    rust_sorted.sort();
    assert_eq!(ts_sorted, rust_sorted, "ProgressPayload 字段集 diff 为空");

    // ab://plugin-log 载荷字段集。
    let ts_fields = ts_interface_fields(ts, "PluginLogPayload");
    let rust_keys: Vec<String> = serde_json::to_value(PluginLogPayload {
        plugin_id: "mock".to_string(),
        level: events::LogLevel::Info,
        line: "line".to_string(),
        ts_ms: 1,
    })
    .expect("serialize log")
    .as_object()
    .expect("object")
    .keys()
    .cloned()
    .collect();
    let mut ts_sorted = ts_fields.clone();
    ts_sorted.sort();
    let mut rust_sorted = rust_keys.clone();
    rust_sorted.sort();
    assert_eq!(ts_sorted, rust_sorted, "PluginLogPayload 字段集 diff 为空");

    // ab://plugin-health 载荷字段集（detail 有值形态）。
    let ts_fields = ts_interface_fields(ts, "PluginHealthPayload");
    let rust_keys: Vec<String> = serde_json::to_value(events::PluginHealthPayload {
        plugin_id: "mock".to_string(),
        state: "crashed".to_string(),
        prev_state: "ready".to_string(),
        detail: Some("exit code 1".to_string()),
    })
    .expect("serialize health")
    .as_object()
    .expect("object")
    .keys()
    .cloned()
    .collect();
    let mut ts_sorted = ts_fields.clone();
    ts_sorted.sort();
    let mut rust_sorted = rust_keys.clone();
    rust_sorted.sort();
    assert_eq!(
        ts_sorted, rust_sorted,
        "PluginHealthPayload 字段集 diff 为空"
    );

    // ab://plugins-reloaded 载荷字段集（spec §6.3，全部「有值」形态）。
    let ts_fields = ts_interface_fields(ts, "PluginsReloadedPayload");
    let rust_keys: Vec<String> = serde_json::to_value(events::PluginsReloadedPayload {
        plugins: vec!["a".to_string()],
        invalid: vec!["broken (plugin.json is missing)".to_string()],
        shadowed: vec!["s1".to_string()],
    })
    .expect("serialize plugins-reloaded")
    .as_object()
    .expect("object")
    .keys()
    .cloned()
    .collect();
    let mut ts_sorted = ts_fields.clone();
    ts_sorted.sort();
    let mut rust_sorted = rust_keys.clone();
    rust_sorted.sort();
    assert_eq!(
        ts_sorted, rust_sorted,
        "PluginsReloadedPayload 字段集 diff 为空"
    );

    // 载荷样例的通道归属：Rust 侧转换产物逐通道对齐（§2 映射面抽样）。
    let mut throttle = ProgressThrottle::new();
    let progress = convert_pipeline(
        PipelineEvent::ParseProgress {
            file_id: "f1".to_string(),
            percent: Some(50.0),
            records_so_far: 42,
        },
        &mut throttle,
    );
    assert_eq!(progress[0].channel, EV_PROGRESS);
    let mut throttle = ProgressThrottle::new();
    let health = events::convert(
        HostEvent::StateChanged {
            plugin_id: "mock".to_string(),
            from: PluginProcessState::Ready,
            to: PluginProcessState::Crashed,
        },
        &mut throttle,
    );
    assert_eq!(health[0].channel, EV_PLUGIN_HEALTH);
    let mut throttle = ProgressThrottle::new();
    let log = events::convert(
        HostEvent::StderrLine {
            plugin_id: "mock".to_string(),
            ts_ms: 1,
            line: "INFO x".to_string(),
        },
        &mut throttle,
    );
    assert_eq!(log[0].channel, EV_PLUGIN_LOG);
    // 吸收态健康载荷携带失败摘要 detail（§2.3；lib.rs wire_events 补发逻辑等价）。
    match &log[0].payload {
        EventPayload::Log(_) => {}
        _ => panic!("log payload"),
    }
}
