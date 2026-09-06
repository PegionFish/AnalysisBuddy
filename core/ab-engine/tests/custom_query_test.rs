//! §2.11 custom_query 全链路（CCP-custom-query addendum）engine 侧测试：
//! 复刻 key_values 既有测试模式（fixture 注入 + 错误映射断言），覆盖
//! `query_custom_query` 扇出与 `custom_query_at_logic` 错误归一：
//! - 正常回包 `{"data":{...}}`（Value 必须是 Object）；
//! - `-32005` / legacy `-32601` 归一 → `unsupported`（§2.11，不得落 internal）；
//! - `-32602` → `invalid_params`；`-32001` → `plugin_busy`（§1.10 全局表）；
//! - 看门狗超时 → `timeout`；SessionGone → `plugin_crashed`；
//!   未知 file_id → `file_not_found`；
//! - 入参校验（空 query / 空 file_id → `invalid_arg`）与 params 零解释透传。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ab_engine::commands::query::{custom_query_at_logic, CustomQueryResultDto};
use ab_engine::pipeline_bridge::{query_custom_query, ImportCoordinator, PipelineConfig};
use ab_pipeline::mock::{FileFixture, MockSession, SessionFixture};
use ab_pipeline::{PluginSession, SessionError};
use ab_protocol::types::{
    CanHandleParams, CanHandleResult, CancelParseParams, CustomQueryParams, CustomQueryResult,
    FileSummary, KeyValuesParams, KeyValuesResult, LoadFileParams, ParseParams, SchemaResult,
    UnloadFileParams,
};
use tokio::sync::mpsc;

/// 空配置 coordinator（无宿主插件；会话经 registry 注入）。
fn coordinator() -> ImportCoordinator {
    coordinator_with(PipelineConfig::default())
}

/// 注入配置 coordinator（超时等测试用）。
fn coordinator_with(config: PipelineConfig) -> ImportCoordinator {
    ImportCoordinator::with_config(
        Arc::new(ab_pipeline::Store::new()),
        Arc::new(ab_pipeline::SessionRegistry::new()),
        mpsc::unbounded_channel().0,
        Arc::new(ab_host::PluginRuntime::new(Arc::new(
            ab_host::PluginRegistry::new(),
        ))),
        Arc::new(ab_host::PluginRegistry::new()),
        config,
    )
}

/// 构造带 custom_query 夹具的 mock 会话（fixture 按路径关联，先 load_file 登记）。
async fn register_session(
    coordinator: &ImportCoordinator,
    plugin_id: &str,
    file_id: &str,
    custom_query: Option<Result<CustomQueryResult, SessionError>>,
) {
    let mut files = HashMap::new();
    files.insert(
        "a.csv".to_string(),
        FileFixture {
            custom_query,
            ..Default::default()
        },
    );
    let session = MockSession::new(SessionFixture {
        plugin_id: plugin_id.to_string(),
        files,
        ..Default::default()
    });
    // MockSession 的 custom_query 夹具按 file_id → path 反查，须先 load 登记。
    session
        .load_file(LoadFileParams {
            file_id: file_id.to_string(),
            path: "a.csv".to_string(),
        })
        .await
        .expect("mock load_file");
    coordinator.registry().register(session);
    coordinator.file_index().insert(file_id, plugin_id);
}

/// 带 §2.11 插件错误码夹具的快捷构造。
fn plugin_error(code: i32) -> Option<Result<CustomQueryResult, SessionError>> {
    Some(Err(SessionError::Plugin {
        code,
        message: "plugin says no".to_string(),
    }))
}

#[tokio::test]
async fn custom_query_happy_path_returns_data_object() {
    let coordinator = coordinator();
    let mut data = serde_json::Map::new();
    data.insert("top".to_string(), serde_json::json!(5));
    register_session(
        &coordinator,
        "mock",
        "f1",
        Some(Ok(CustomQueryResult { data })),
    )
    .await;

    let dto = custom_query_at_logic(&coordinator, "f1", "topn", serde_json::Map::new())
        .await
        .expect("正常回包不应 reject");
    assert_eq!(
        dto.data,
        serde_json::json!({ "top": 5 }),
        "§2.11 data 零解释透传"
    );
    // 序列化形状：{"data": {...}}（data 必须是 Object）。
    let value = serde_json::to_value(&dto).expect("serialize");
    assert_eq!(
        value,
        serde_json::json!({ "data": { "top": 5 } }),
        "CustomQueryResultDto 序列化形状"
    );
    assert!(dto.data.is_object(), "data 必须为 JSON object");
}

/// §2.11 归一：-32005 与 legacy -32601 同义 → unsupported（不得落 internal）。
#[tokio::test]
async fn custom_query_unsupported_codes_normalize() {
    for code in [
        ab_protocol::errors::ERR_UNSUPPORTED_IN_V1,
        ab_protocol::errors::ERR_METHOD_NOT_FOUND,
    ] {
        let coordinator = coordinator();
        register_session(&coordinator, "mock", "f1", plugin_error(code)).await;
        let err = custom_query_at_logic(&coordinator, "f1", "topn", serde_json::Map::new())
            .await
            .expect_err("插件错误应 reject");
        assert_eq!(err.code, "unsupported", "code {code} → unsupported");
        assert_eq!(err.message, "plugin says no", "message 透传插件原文");
    }
}

/// §2.11：未知查询名 / 非法 params → invalid_params（归一只发生在
/// custom_query 转换器；code_name 全局映射保持 -32602 → internal 不变）。
#[tokio::test]
async fn custom_query_invalid_params_code_maps_to_invalid_params() {
    let coordinator = coordinator();
    register_session(
        &coordinator,
        "mock",
        "f1",
        plugin_error(ab_protocol::errors::ERR_INVALID_PARAMS),
    )
    .await;
    let err = custom_query_at_logic(&coordinator, "f1", "topn", serde_json::Map::new())
        .await
        .expect_err("插件错误应 reject");
    assert_eq!(err.code, "invalid_params");
    assert_eq!(
        crate_ipc_code_name(ab_protocol::errors::ERR_INVALID_PARAMS),
        "internal",
        "-32602 全局映射保持 internal（归一仅限 custom_query 转换器）"
    );
}

/// 其余插件码走 §1.10 全局表（-32001 → plugin_busy）。
#[tokio::test]
async fn custom_query_plugin_busy_maps_via_global_table() {
    let coordinator = coordinator();
    register_session(
        &coordinator,
        "mock",
        "f1",
        plugin_error(ab_protocol::errors::ERR_PLUGIN_BUSY),
    )
    .await;
    let err = custom_query_at_logic(&coordinator, "f1", "topn", serde_json::Map::new())
        .await
        .expect_err("插件错误应 reject");
    assert_eq!(err.code, "plugin_busy");
}

/// 会话级 SessionGone → plugin_crashed（同 key_values 对应分支）。
#[tokio::test]
async fn custom_query_session_gone_maps_to_plugin_crashed() {
    let coordinator = coordinator();
    register_session(
        &coordinator,
        "mock",
        "f1",
        Some(Err(SessionError::SessionGone)),
    )
    .await;
    let err = custom_query_at_logic(&coordinator, "f1", "topn", serde_json::Map::new())
        .await
        .expect_err("SessionGone 应 reject");
    assert_eq!(err.code, "plugin_crashed");
}

/// 未知 file_id（FileIndex 查不到）→ file_not_found（同 key_values
/// FileNotReady 分支）。
#[tokio::test]
async fn custom_query_unknown_file_maps_to_file_not_found() {
    let coordinator = coordinator();
    let err = custom_query_at_logic(&coordinator, "ghost", "topn", serde_json::Map::new())
        .await
        .expect_err("未知文件应 reject");
    assert_eq!(err.code, "file_not_found");
    // 扇出函数直连：同语义（CustomQueryError::FileNotReady）。
    let err = query_custom_query(
        coordinator.registry(),
        coordinator.file_index(),
        "ghost",
        "topn",
        serde_json::Map::new(),
        Duration::from_secs(1),
    )
    .await
    .expect_err("未知文件应 reject");
    assert_eq!(err, ab_engine::pipeline_bridge::CustomQueryError::FileNotReady("ghost".into()));
}

/// 看门狗超时 → timeout（注入短超时 + 阻塞会话）。
#[tokio::test]
async fn custom_query_timeout_maps_to_timeout() {
    let coordinator = coordinator_with(PipelineConfig {
        custom_query_timeout: Duration::from_millis(50),
        ..Default::default()
    });
    coordinator.registry().register(Arc::new(SlowSession) as Arc<dyn PluginSession>);
    coordinator.file_index().insert("f1", "slow");
    let err = custom_query_at_logic(&coordinator, "f1", "topn", serde_json::Map::new())
        .await
        .expect_err("超时应 reject");
    assert_eq!(err.code, "timeout");
}

/// 入参校验：空 query / 空 file_id → invalid_arg（镜像 query_series 风格）。
#[tokio::test]
async fn custom_query_rejects_empty_query_and_file_id() {
    let coordinator = coordinator();
    let err = custom_query_at_logic(&coordinator, "f1", "  ", serde_json::Map::new())
        .await
        .expect_err("空 query 应 reject");
    assert_eq!(err.code, "invalid_arg");
    let err = custom_query_at_logic(&coordinator, "", "topn", serde_json::Map::new())
        .await
        .expect_err("空 file_id 应 reject");
    assert_eq!(err.code, "invalid_arg");
}

/// params 零解释透传（§2.11「宿主零解释、零插值」）：本地 echo 会话回显
/// 收到的 params，宿主侧原样送达。
#[tokio::test]
async fn custom_query_passes_params_verbatim() {
    let coordinator = coordinator();
    coordinator.registry().register(Arc::new(EchoSession) as Arc<dyn PluginSession>);
    coordinator.file_index().insert("f1", "echo");
    let mut params = serde_json::Map::new();
    params.insert("window".to_string(), serde_json::json!("60s"));
    params.insert("limit".to_string(), serde_json::json!(10));
    let dto = custom_query_at_logic(&coordinator, "f1", "topn", params)
        .await
        .expect("echo 回包");
    assert_eq!(
        dto.data,
        serde_json::json!({ "window": "60s", "limit": 10 }),
        "params 原样送达插件并由 echo 会话回显"
    );
}

// ---------------------------------------------------------------------------
// 本地测试会话：阻塞（超时用）与 params echo（透传用）。
// ---------------------------------------------------------------------------

struct SlowSession;

struct EchoSession;

#[async_trait::async_trait]
impl PluginSession for SlowSession {
    fn plugin_id(&self) -> &str {
        "slow"
    }
    async fn schema(&self) -> Result<SchemaResult, SessionError> {
        Ok(SchemaResult { metrics: vec![] })
    }
    async fn can_handle(&self, _p: CanHandleParams) -> Result<CanHandleResult, SessionError> {
        Ok(CanHandleResult {
            can_handle: false,
            confidence: 0.0,
            reason: None,
        })
    }
    async fn load_file(&self, _p: LoadFileParams) -> Result<FileSummary, SessionError> {
        Ok(FileSummary {
            record_count_hint: None,
            time_range: None,
            note: None,
        })
    }
    async fn parse_stream(
        &self,
        _p: ParseParams,
        _sink: mpsc::Sender<ab_pipeline::ParseEvent>,
    ) -> Result<u64, SessionError> {
        Ok(0)
    }
    async fn cancel_parse(&self, _p: CancelParseParams) -> Result<(), SessionError> {
        Ok(())
    }
    async fn key_values(&self, _p: KeyValuesParams) -> Result<KeyValuesResult, SessionError> {
        Ok(KeyValuesResult { entries: vec![] })
    }
    async fn custom_query(
        &self,
        _p: CustomQueryParams,
    ) -> Result<CustomQueryResult, SessionError> {
        // 远超注入的 50ms 看门狗。
        tokio::time::sleep(Duration::from_secs(2)).await;
        Ok(CustomQueryResult {
            data: serde_json::Map::new(),
        })
    }
    async fn unload_file(&self, _p: UnloadFileParams) -> Result<(), SessionError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl PluginSession for EchoSession {
    fn plugin_id(&self) -> &str {
        "echo"
    }
    async fn schema(&self) -> Result<SchemaResult, SessionError> {
        Ok(SchemaResult { metrics: vec![] })
    }
    async fn can_handle(&self, _p: CanHandleParams) -> Result<CanHandleResult, SessionError> {
        Ok(CanHandleResult {
            can_handle: false,
            confidence: 0.0,
            reason: None,
        })
    }
    async fn load_file(&self, _p: LoadFileParams) -> Result<FileSummary, SessionError> {
        Ok(FileSummary {
            record_count_hint: None,
            time_range: None,
            note: None,
        })
    }
    async fn parse_stream(
        &self,
        _p: ParseParams,
        _sink: mpsc::Sender<ab_pipeline::ParseEvent>,
    ) -> Result<u64, SessionError> {
        Ok(0)
    }
    async fn cancel_parse(&self, _p: CancelParseParams) -> Result<(), SessionError> {
        Ok(())
    }
    async fn key_values(&self, _p: KeyValuesParams) -> Result<KeyValuesResult, SessionError> {
        Ok(KeyValuesResult { entries: vec![] })
    }
    async fn custom_query(
        &self,
        p: CustomQueryParams,
    ) -> Result<CustomQueryResult, SessionError> {
        // §2.11 echo 语义：params 原样回显（同 mock-plugin 内置分支）。
        Ok(CustomQueryResult { data: p.params })
    }
    async fn unload_file(&self, _p: UnloadFileParams) -> Result<(), SessionError> {
        Ok(())
    }
}

/// `code_name` 全局映射直查（断言 -32602 全局行为未变）。
fn crate_ipc_code_name(code: i32) -> &'static str {
    ab_engine::ipc_errors::code_name(code)
}

/// CustomQueryResultDto 供外部断言导入自检（保持 pub API 引用）。
#[allow(dead_code)]
fn dto_type_check(_dto: CustomQueryResultDto) {}
