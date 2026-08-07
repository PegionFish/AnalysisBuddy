//! 对 A 路 `PluginSession` 的 mock 实现（pipeline.md §4.1 适配层约定）。
//!
//! fixtures 驱动：`SessionFixture` 声明 schema / can_handle / 逐文件
//! load_file 结果与脚本化 parse 步骤；`CallStats` 记录各方法调用次数
//! （B-03 重开接线用「can_handle 未调用」断言）。B 路全程复用，Phase 3
//! 由 ab-app 的 `HostSessionAdapter` 换层。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ab_protocol::types::{
    CanHandleParams, CanHandleResult, CancelParseParams, FileSummary, KeyValuesParams,
    KeyValuesResult, LoadFileParams, ParseParams, ProgressParams, RecordBatch, SchemaResult,
    UnloadFileParams,
};
use tokio::sync::mpsc;

use crate::{ParseEvent, PluginSession, SessionError};

/// 脚本化 parse 步骤（pipeline.md §4.1 通道约定：Batch / Progress 通知 + 错误注入）。
#[derive(Debug, Clone)]
pub enum ParseStep {
    Batch(RecordBatch),
    Progress(ProgressParams),
    /// 提前终止：sink 停推，`parse_stream` future 返回该错误。
    Fail(SessionError),
}

/// 逐文件行为夹具。
#[derive(Debug, Clone, Default)]
pub struct FileFixture {
    pub load_file: Option<Result<FileSummary, SessionError>>,
    /// parse 脚本；`None` 各步骤默认见 [`MockSession::parse_stream`]。
    pub parse_script: Vec<ParseStep>,
    /// 最终返回的 `records_total`；`None` = Σ各批 `records.len()`（首段错误
    /// 步骤出现时以其错误返回）。
    pub parse_result: Option<Result<u64, SessionError>>,
    pub key_values: Option<Result<KeyValuesResult, SessionError>>,
}

/// 会话级夹具（`None` 字段取默认成功值，见 [`MockSession`] 各方法文档）。
#[derive(Debug, Clone, Default)]
pub struct SessionFixture {
    pub plugin_id: String,
    pub schema: Option<Result<SchemaResult, SessionError>>,
    pub can_handle: Option<Result<CanHandleResult, SessionError>>,
    /// key = 文件路径（load_file 时以 path 关联 file_id）。
    pub files: HashMap<String, FileFixture>,
}

/// 调用统计快照（`Arc<Mutex>` 共享，测试断言用）。
#[derive(Debug, Clone, Default)]
pub struct CallStats {
    pub schema_calls: u64,
    pub can_handle_calls: u64,
    pub load_file_calls: u64,
    pub parse_calls: u64,
    pub cancel_parse_calls: u64,
    pub key_values_calls: u64,
    pub unload_file_calls: u64,
    /// load_file 实际收到的 file_id（按调用序）。
    pub loaded_file_ids: Vec<String>,
    /// parse_stream 实际收到的 file_id（按调用序）。
    pub parsed_file_ids: Vec<String>,
}

/// `PluginSession` 的 mock 实现（fixtures 驱动）。
pub struct MockSession {
    fixture: SessionFixture,
    /// file_id → 文件路径（load_file 时登记，parse 脚本按 path 解析）。
    file_paths: Mutex<HashMap<String, String>>,
    stats: Arc<Mutex<CallStats>>,
}

impl MockSession {
    pub fn new(fixture: SessionFixture) -> Arc<Self> {
        Arc::new(MockSession {
            fixture,
            file_paths: Mutex::new(HashMap::new()),
            stats: Arc::new(Mutex::new(CallStats::default())),
        })
    }

    /// 调用统计快照。
    pub fn stats(&self) -> CallStats {
        self.stats.lock().unwrap().clone()
    }

    /// 追加调用统计（测试注入外部计数场景用）。
    pub fn record(&self, f: impl FnOnce(&mut CallStats)) {
        f(&mut self.stats.lock().unwrap());
    }
}

impl MockSession {
    fn fixture_for_path(&self, file_id: &str) -> Option<FileFixture> {
        let path = self.file_paths.lock().unwrap().get(file_id).cloned()?;
        self.fixture.files.get(&path).cloned()
    }
}

#[async_trait::async_trait]
impl PluginSession for MockSession {
    fn plugin_id(&self) -> &str {
        &self.fixture.plugin_id
    }

    /// 默认成功：空指标清单（白名单为空 → 全部记录丢弃，便于联调）。
    async fn schema(&self) -> Result<SchemaResult, SessionError> {
        self.stats.lock().unwrap().schema_calls += 1;
        self.fixture
            .schema
            .clone()
            .unwrap_or(Ok(SchemaResult { metrics: vec![] }))
    }

    /// 默认弃权：`can_handle=false, confidence=0`。
    async fn can_handle(&self, _p: CanHandleParams) -> Result<CanHandleResult, SessionError> {
        self.stats.lock().unwrap().can_handle_calls += 1;
        self.fixture
            .can_handle
            .clone()
            .unwrap_or(Ok(CanHandleResult {
                can_handle: false,
                confidence: 0.0,
                reason: None,
            }))
    }

    /// 默认成功：空摘要。
    async fn load_file(&self, p: LoadFileParams) -> Result<FileSummary, SessionError> {
        self.stats.lock().unwrap().load_file_calls += 1;
        self.stats
            .lock()
            .unwrap()
            .loaded_file_ids
            .push(p.file_id.clone());
        self.file_paths
            .lock()
            .unwrap()
            .insert(p.file_id.clone(), p.path.clone());
        let result = self
            .fixture_for_path(&p.file_id)
            .and_then(|f| f.load_file)
            .unwrap_or(Ok(FileSummary {
                record_count_hint: None,
                time_range: None,
                note: None,
            }));
        if result.is_err() {
            self.file_paths.lock().unwrap().remove(&p.file_id);
        }
        result
    }

    /// 按脚本逐条推入 sink（Batch / Progress）；脚本含 `Fail` 时以其错误
    /// 返回；否则返回 `parse_result`（缺省 = Σ各批 len）。
    async fn parse_stream(
        &self,
        p: ParseParams,
        sink: mpsc::Sender<ParseEvent>,
    ) -> Result<u64, SessionError> {
        self.stats.lock().unwrap().parse_calls += 1;
        self.stats
            .lock()
            .unwrap()
            .parsed_file_ids
            .push(p.file_id.clone());
        let script = self
            .fixture_for_path(&p.file_id)
            .map(|f| f.parse_script)
            .unwrap_or_default();
        let mut total = 0u64;
        for step in &script {
            match step {
                ParseStep::Batch(b) => {
                    total += b.records.len() as u64;
                    if sink.send(ParseEvent::Batch(b.clone())).await.is_err() {
                        return Err(SessionError::SessionGone);
                    }
                }
                ParseStep::Progress(prog) => {
                    if sink.send(ParseEvent::Progress(prog.clone())).await.is_err() {
                        return Err(SessionError::SessionGone);
                    }
                }
                ParseStep::Fail(e) => return Err(e.clone()),
            }
        }
        Ok(self
            .fixture_for_path(&p.file_id)
            .and_then(|f| f.parse_result)
            .unwrap_or(Ok(total))?)
    }

    async fn cancel_parse(&self, _p: CancelParseParams) -> Result<(), SessionError> {
        self.stats.lock().unwrap().cancel_parse_calls += 1;
        Ok(())
    }

    /// 默认成功：空关键值列表。
    async fn key_values(&self, p: KeyValuesParams) -> Result<KeyValuesResult, SessionError> {
        self.stats.lock().unwrap().key_values_calls += 1;
        self.fixture_for_path(&p.file_id)
            .and_then(|f| f.key_values)
            .unwrap_or(Ok(KeyValuesResult { entries: vec![] }))
    }

    async fn unload_file(&self, _p: UnloadFileParams) -> Result<(), SessionError> {
        self.stats.lock().unwrap().unload_file_calls += 1;
        Ok(())
    }
}
