//! pipeline.md §4.1 适配层：`HostSessionAdapter` 包装 ab-host 会话，为管线消费
//! A 路能力实现 `ab_pipeline::PluginSession` trait（逐方法转发 + `parse_stream`
//! 按 `file_id` 过滤组装 sink）。
//!
//! P3-02 集成裁决事项（已执行）：P3-01 曾在本模块按 pipeline.md §4.1 以本地
//! 镜像定义 `PluginSession` / `SessionError` / `ParseEvent`（当时 ab-pipeline
//! 尚未合入 main）；B 路合入后已删除本地定义，改引 `ab_pipeline` 正式 trait，
//! 适配器 impl 零改动（仅 trait 引用路径变化）。
//!
//! `From<ab_host::HostError>` 因孤儿规则无法由本 crate 为 `ab_pipeline` 类型
//! 实现，故映射收敛为 [`map_host_error`] 函数（映射约定与 pipeline.md §4.1 一致）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ab_host::PluginNotification;
use ab_pipeline::{ParseEvent, PluginSession, SessionError};
use ab_protocol::types::{
    CanHandleParams, CanHandleResult, CancelParseParams, FileSummary, KeyValuesParams,
    KeyValuesResult, LoadFileParams, ParseParams, SchemaResult, UnloadFileParams,
};
use tokio::sync::{mpsc, Notify};

/// `HostError` → `SessionError` 两段式映射（pipeline.md §4.1）：
/// `Protocol` → `Plugin`；`Transport` / `Discovery` → `SessionGone`。
///
/// 孤儿规则禁止 `impl From<ab_host::HostError> for ab_pipeline::SessionError`，
/// 以等价自由函数承载映射（适配器与编排层统一经此入口转换）。
pub fn map_host_error(error: ab_host::HostError) -> SessionError {
    match error {
        ab_host::HostError::Protocol { code, message, .. } => {
            SessionError::Plugin { code, message }
        }
        ab_host::HostError::Transport(_) | ab_host::HostError::Discovery(_) => {
            SessionError::SessionGone
        }
    }
}

/// 适配器：逐方法转发 ab-host 会话（pipeline.md §4.1 适配层约定）。
pub struct HostSessionAdapter {
    /// 被包装的真实宿主会话。
    pub session: Arc<ab_host::PluginSession>,
    /// `parse_stream` sink 满时被丢弃的通知累计数（§4.1「满则丢并计数」）。
    dropped: Arc<AtomicU64>,
}

impl HostSessionAdapter {
    pub fn new(session: Arc<ab_host::PluginSession>) -> Self {
        Self {
            session,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 因 sink 满被丢弃的通知累计数（诊断，pipeline.md §4.1）。
    pub fn dropped_notifications(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// 按 file_id 过滤一条通知并 `try_send` 进 sink（满则丢并计数，§4.1）；
/// 返回 `false` 表示 sink 已关闭（调用方应停止转发）。recv 分支与完成信号
/// 排空分支共用，保证两条路径转发语义一致。
fn forward_notification(
    notification: PluginNotification,
    file_id: &str,
    sink: &mpsc::Sender<ParseEvent>,
    dropped: &Arc<AtomicU64>,
) -> bool {
    let event = match notification {
        PluginNotification::RecordBatch(batch) if batch.file_id == file_id => {
            ParseEvent::Batch(batch)
        }
        PluginNotification::Progress(progress) if progress.file_id == file_id => {
            ParseEvent::Progress(progress)
        }
        // 非本次 parse 的 file_id：忽略。
        _ => return true,
    };
    match sink.try_send(event) {
        Ok(()) => true,
        // 有界通道满则丢并计数（沿用 host-runtime.md §4.4 丢旧策略；
        // 订阅方及时排空时行为等价于丢旧，完整性兜底见 pipeline.md §4.1）。
        Err(mpsc::error::TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

#[async_trait::async_trait]
impl PluginSession for HostSessionAdapter {
    fn plugin_id(&self) -> &str {
        self.session.plugin_id()
    }

    async fn schema(&self) -> Result<SchemaResult, SessionError> {
        self.session.schema().await.map_err(map_host_error)
    }

    async fn can_handle(&self, p: CanHandleParams) -> Result<CanHandleResult, SessionError> {
        self.session.can_handle(p).await.map_err(map_host_error)
    }

    async fn load_file(&self, p: LoadFileParams) -> Result<FileSummary, SessionError> {
        self.session.load_file(p).await.map_err(map_host_error)
    }

    async fn parse_stream(
        &self,
        p: ParseParams,
        sink: mpsc::Sender<ParseEvent>,
    ) -> Result<u64, SessionError> {
        // 适配层组装（pipeline.md §4.1）：先订阅会话通知流，再起转发任务按
        // file_id 过滤（只放行本次 parse 的 RecordBatch / Progress）推入有界
        // sink（满则丢并计数）；主体 await 宿主 parse() 响应得到 records_total。
        //
        // 完成协议（P3-06 竞态修复）：parse 的最终 response 与最后一批通知
        // 同源于 stdio 读泵——响应到达时通知流 mpsc 中可能仍缓冲着尚未转发的
        // 批次，旧实现 `forward.abort()` 直接截断 → Σ批次 < records_total。
        // 现改为「完成信号 + 排空」：主体在 parse() 返回（无论 Ok/Err）后
        // `done.notify_one()`；转发任务收到完成信号先 `try_recv` 排空 mpsc 中
        // 已缓冲的剩余通知（仍按 file_id 过滤、满则丢并计数）再退出，不 abort、
        // 不留悬挂任务。select 在 recv 与完成信号同时就绪时随机选择，故完成
        // 分支必须自排空：排空循环持续到 Empty，保证 done 之前已入缓冲的
        // 全部通知都被处理。
        let mut notifications = self.session.subscribe_notifications();
        let file_id = p.file_id.clone();
        let dropped = self.dropped.clone();
        let done = Arc::new(Notify::new());
        let done_task = done.clone();
        let forward = tokio::spawn(async move {
            loop {
                tokio::select! {
                    notification = notifications.recv() => {
                        let Some(notification) = notification else { break };
                        if !forward_notification(notification, &file_id, &sink, &dropped) {
                            break;
                        }
                    }
                    _ = done_task.notified() => {
                        while let Ok(notification) = notifications.try_recv() {
                            if !forward_notification(notification, &file_id, &sink, &dropped) {
                                break;
                            }
                        }
                        break;
                    }
                }
            }
        });
        let result = self.session.parse(p).await;
        done.notify_one();
        let _ = forward.await;
        result.map(|r| r.records_total).map_err(map_host_error)
    }

    async fn cancel_parse(&self, p: CancelParseParams) -> Result<(), SessionError> {
        self.session.cancel_parse(p).await.map_err(map_host_error)
    }

    async fn key_values(&self, p: KeyValuesParams) -> Result<KeyValuesResult, SessionError> {
        self.session.key_values(p).await.map_err(map_host_error)
    }

    async fn unload_file(&self, p: UnloadFileParams) -> Result<(), SessionError> {
        self.session.unload_file(p).await.map_err(map_host_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_error_protocol_maps_to_plugin() {
        let error = ab_host::HostError::Protocol {
            code: -32003,
            message: "parse failed".to_string(),
            data: Some(serde_json::json!({ "line": 42 })),
        };
        assert_eq!(
            map_host_error(error),
            SessionError::Plugin {
                code: -32003,
                message: "parse failed".to_string(),
            },
            "§4.1: HostError::Protocol → SessionError::Plugin（原样透传 code/message）"
        );
    }

    #[test]
    fn host_error_transport_and_discovery_map_to_session_gone() {
        assert_eq!(
            map_host_error(ab_host::HostError::Transport("pipe broke".to_string())),
            SessionError::SessionGone,
            "§4.1: HostError::Transport → SessionError::SessionGone"
        );
        assert_eq!(
            map_host_error(ab_host::HostError::Discovery(
                ab_host::DiscoveryError::MissingManifest
            )),
            SessionError::SessionGone,
            "§4.1: HostError::Discovery → SessionError::SessionGone"
        );
    }
}
