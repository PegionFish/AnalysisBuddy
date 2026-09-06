//! 事件广播 hub（替换桌面 tauri `wire_events`/`wire_pipeline_events` 接线）：
//! 单个 forwarder 任务消费 `host.subscribe_events()` + 管线事件通道，经
//! `events::convert` / `convert_pipeline`（无节流窗口，转发不吞终态）转成
//! `EmittedEvent` 广播；每个 SSE 订阅者持有独立 [`ProgressThrottle`] 与
//! 过滤谓词——节流是逐连接的（hub 不节流，percent≥100 终态恒达每端）。
//!
//! 背压：hub 中央 broadcast（容量 512）+ 每订阅者一条有界 mpsc 队列（256，
//! 独立转发任务搬运）。某订阅者消费慢 → 它自己的 broadcast 缓冲堆积 →
//! 该订阅者收到 Lagged → 其 SSE 流发终帧 `event: error`
//! （`event_stream_lagged`）后关闭，不影响其他订阅者。hub 同时维护
//! [`PluginMeta`]（list_plugins 状态源）与 [`PluginLogBuffer`]
//! （get_plugin_log 数据源）——与桌面 ab-app 接线等价。

use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::time::Duration;

use ab_engine::events::{
    self, EmittedEvent, EventPayload, PluginLogBuffer, PluginLogPayload, PluginMeta,
    ProgressThrottle,
};
use ab_host::HostEvent;
use ab_pipeline::PipelineEvent;
use futures_core::Stream;
use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use axum::response::sse::Event;

/// 中央 broadcast 通道容量（事件洪峰缓冲；按订阅者独立积压）。
const HUB_CAPACITY: usize = 512;

/// 每订阅者 mpsc 队列容量（SSE 帧缓冲；持续满载即掉队关闭）。
const SUBSCRIBER_QUEUE: usize = 256;

/// 订阅者队列项：`Ok(event)` 正常事件；`Err(skipped)` 掉队终态（其后
/// 通道关闭，SSE 流应发 error 终帧；skipped 为丢失事件数，u64 对齐
/// broadcast Lagged）。
pub type HubItem = Result<Arc<EmittedEvent>, u64>;

/// 中央广播 hub：载荷为 `Arc<EmittedEvent>`（EmittedEvent 非 Clone，包裹
/// Arc 共享）。无订阅者时 publish 静默丢弃（无消费者不积压）。
#[derive(Clone)]
pub struct EventHub {
    tx: broadcast::Sender<Arc<EmittedEvent>>,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHub {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(HUB_CAPACITY);
        Self { tx }
    }

    /// 发布一条事件（无订阅者静默丢弃）。
    pub fn publish(&self, event: EmittedEvent) {
        let _ = self.tx.send(Arc::new(event));
    }

    /// 订阅：为该连接建独立转发任务（中央 broadcast → 有界 mpsc），
    /// 返回 SSE 流消费的接收端。掉队以 `Err(skipped)` 终态项投递。
    pub fn subscribe(&self) -> mpsc::Receiver<HubItem> {
        let (tx, rx) = mpsc::channel::<HubItem>(SUBSCRIBER_QUEUE);
        let mut host_rx = self.tx.subscribe();
        tokio::spawn(async move {
            loop {
                match host_rx.recv().await {
                    Ok(event) => {
                        if tx.send(Ok(event)).await.is_err() {
                            break; // 订阅者已断开
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let _ = tx.send(Err(skipped)).await;
                        break; // 掉队终态：投递后关闭
                    }
                    Err(broadcast::error::RecvError::Closed) => break, // hub 停机
                }
            }
        });
        rx
    }
}

/// SSE 帧事件名：`ab://` 前缀剥短（`ab://progress` → `progress`）。
/// 客户端按短名过滤；完整通道名与帧格式见 `docs/spec/http-api-v1.md` §5。
pub fn channel_short_name(channel: &str) -> &str {
    channel.strip_prefix("ab://").unwrap_or(channel)
}

/// 前向任务：消费 host 事件 + 管线事件 → 转换 → 广播（直至两路通道均关闭）。
pub fn spawn_forwarder(
    mut host_rx: broadcast::Receiver<HostEvent>,
    mut pipeline_rx: mpsc::UnboundedReceiver<PipelineEvent>,
    hub: EventHub,
    meta: Arc<PluginMeta>,
    log_buffer: Arc<PluginLogBuffer>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // 转发面无节流（窗口 0 恒 due）：节流责任在订阅者。
        let mut throttle = ProgressThrottle::with_window(Duration::ZERO);
        loop {
            tokio::select! {
                event = host_rx.recv() => match event {
                    Ok(event) => {
                        forward_host(event, &hub, &meta, &log_buffer, &mut throttle);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                event = pipeline_rx.recv() => match event {
                    Some(event) => {
                        for emitted in events::convert_pipeline(event, &mut throttle) {
                            hub.publish(emitted);
                        }
                    }
                    None => break,
                },
            }
        }
    })
}

/// host 事件转发一步：日志缓冲 → meta 记账 → 转换 → 广播（health 失败态
/// 附带最近失败摘要，与桌面 detail 补全等价）。
fn forward_host(
    event: HostEvent,
    hub: &EventHub,
    meta: &PluginMeta,
    log_buffer: &PluginLogBuffer,
    throttle: &mut ProgressThrottle,
) {
    if let HostEvent::StderrLine {
        plugin_id,
        ts_ms,
        line,
    } = &event
    {
        log_buffer.push(PluginLogPayload {
            plugin_id: plugin_id.clone(),
            level: events::parse_log_level(line),
            line: line.clone(),
            ts_ms: *ts_ms,
        });
    }
    meta.record(&event);
    for mut emitted in events::convert(event, throttle) {
        if let EventPayload::Health(mut payload) = emitted.payload {
            if (payload.state == "crashed" || payload.state == "timeout")
                && payload.detail.is_none()
            {
                payload.detail = meta.last_error_of(&payload.plugin_id);
            }
            emitted.payload = EventPayload::Health(payload);
        }
        hub.publish(emitted);
    }
}

/// 单个 SSE 订阅者的适配流：hub 队列 → 过滤/节流 → `axum` SSE 帧。
/// 掉队（`Err(skipped)` 终态项）→ 发 `error` 终帧后结束；通道关闭
/// （服务停机 / 订阅被移除）→ 直接结束。
pub struct SseEventStream {
    rx: mpsc::Receiver<HubItem>,
    throttle: ProgressThrottle,
    file_filter: Option<String>,
    plugin_filter: Option<String>,
    done: bool,
}

impl SseEventStream {
    /// 构造订阅者流（routes.rs 消费；节流窗口 = 桌面 100ms/file_id）。
    pub fn new(
        rx: mpsc::Receiver<HubItem>,
        file_filter: Option<String>,
        plugin_filter: Option<String>,
    ) -> Self {
        Self {
            rx,
            throttle: ProgressThrottle::new(),
            file_filter,
            plugin_filter,
            done: false,
        }
    }

    /// 过滤 + 订阅者侧节流；`None` = 该事件对本连接不可见。
    fn visible(&mut self, event: &EmittedEvent) -> Option<Event> {
        let frame = frame_of(event)?;
        match &event.payload {
            EventPayload::Health(payload) => {
                plugin_visible(&self.plugin_filter, &payload.plugin_id).then_some(frame)
            }
            EventPayload::Log(payload) => {
                plugin_visible(&self.plugin_filter, &payload.plugin_id).then_some(frame)
            }
            EventPayload::Progress(payload) => {
                let file_ok = self
                    .file_filter
                    .as_ref()
                    .map(|f| f == &payload.file_id)
                    .unwrap_or(true);
                if !file_ok {
                    return None;
                }
                // 订阅者侧节流（100ms/file_id；percent≥100 终态直发）。
                self.throttle.accept(payload.clone()).map(|_| frame)
            }
            EventPayload::PluginsReloaded(_) => Some(frame),
        }
    }
}

fn plugin_visible(filter: &Option<String>, plugin_id: &str) -> bool {
    filter.as_ref().map(|f| f == plugin_id).unwrap_or(true)
}

/// 事件 → SSE 帧：内层 payload 按变体序列化（不 serde 整个枚举——避免
/// 外层变体标签进入线上形状），帧名 = 通道短名。
fn frame_of(event: &EmittedEvent) -> Option<Event> {
    let payload = match &event.payload {
        EventPayload::Health(payload) => serde_json::to_value(payload).ok()?,
        EventPayload::Log(payload) => serde_json::to_value(payload).ok()?,
        EventPayload::Progress(payload) => serde_json::to_value(payload).ok()?,
        EventPayload::PluginsReloaded(payload) => serde_json::to_value(payload).ok()?,
    };
    Some(
        Event::default()
            .event(channel_short_name(event.channel))
            .data(payload.to_string()),
    )
}

impl Stream for SseEventStream {
    type Item = Result<Event, std::convert::Infallible>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // 全字段 Unpin（Receiver/HashMap/Option<String>/bool）。
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        loop {
            match ready!(this.rx.poll_recv(cx)) {
                Some(Ok(event)) => {
                    if let Some(frame) = this.visible(&event) {
                        return Poll::Ready(Some(Ok(frame)));
                    }
                    // 不可见：继续拉下一条。
                }
                Some(Err(skipped)) => {
                    // 掉队：为避免静默缺口，发终帧后关闭该连接。
                    this.done = true;
                    let frame = Event::default().event("error").data(
                        json!({
                            "code": "event_stream_lagged",
                            "message": "subscriber fell behind; connection closed to avoid silent gaps",
                            "skipped": skipped,
                        })
                        .to_string(),
                    );
                    return Poll::Ready(Some(Ok(frame)));
                }
                None => return Poll::Ready(None), // 通道关闭（hub 停机）
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_engine::events::EV_PROGRESS;
    use std::pin::pin;

    fn progress_event(file_id: &str, percent: Option<f64>) -> EmittedEvent {
        EmittedEvent {
            channel: EV_PROGRESS,
            payload: EventPayload::Progress(ab_protocol::types::ProgressParams {
                file_id: file_id.to_string(),
                percent,
                records_so_far: 1,
                bytes_read: None,
            }),
        }
    }

    #[test]
    fn channel_short_name_strips_prefix() {
        assert_eq!(channel_short_name("ab://progress"), "progress");
        assert_eq!(channel_short_name("ab://plugin-log"), "plugin-log");
        assert_eq!(channel_short_name("plain"), "plain");
    }

    #[tokio::test]
    async fn hub_publish_reaches_subscriber() {
        let hub = EventHub::new();
        let mut rx = hub.subscribe();
        hub.publish(progress_event("f1", Some(10.0)));
        let received = rx.recv().await.expect("event").expect("ok item");
        assert_eq!(received.channel, EV_PROGRESS);
    }

    #[test]
    fn subscriber_filters_and_throttles_progress() {
        let (_, rx) = mpsc::channel::<HubItem>(16);
        let mut stream = SseEventStream::new(rx, Some("f1".to_string()), None);

        // f1 首条可见；f2 被文件过滤吞掉。
        assert!(stream.visible(&progress_event("f1", Some(10.0))).is_some());
        assert!(stream.visible(&progress_event("f2", Some(10.0))).is_none());
        // 同 file_id 100ms 窗口内第二条被节流吞掉（桌面 §2.1 语义）。
        assert!(stream.visible(&progress_event("f1", Some(20.0))).is_none());
        // percent≥100 终态直发，不受窗口影响。
        assert!(stream.visible(&progress_event("f1", Some(100.0))).is_some());

        // 无过滤时可见性恢复。
        let (_, rx) = mpsc::channel::<HubItem>(16);
        let mut stream = SseEventStream::new(rx, None, None);
        assert!(stream.visible(&progress_event("f2", Some(30.0))).is_some());
    }

    #[tokio::test]
    async fn lagged_stream_emits_error_final_frame_then_closes() {
        // 队列仅含掉队终态项（真实流程中 Lagged 时转发任务立即投递
        // Err 并 break，正常事件早已被消费完）。
        let (tx, rx) = mpsc::channel::<HubItem>(4);
        tx.send(Err(7)).await.expect("send lag marker");
        drop(tx); // 关闭：终帧之后下一次 poll 应为 None

        let mut stream = pin!(SseEventStream::new(rx, None, None));
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        match stream.as_mut().poll_next(&mut cx) {
            Poll::Ready(Some(Ok(_frame))) => {} // error 终帧（内容由 SSE 集成测试断言）
            other => panic!("expected lag error frame, got {other:?}"),
        }
        // done → 后续 poll 直接结束。
        assert!(matches!(
            stream.as_mut().poll_next(&mut cx),
            Poll::Ready(None)
        ));
    }
}
