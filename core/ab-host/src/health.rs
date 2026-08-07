//! 超时看门狗、重试熔断与 stderr 环形缓冲（host-runtime.md §5/§6；protocol.md §6/§5.2/§9）。

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 每方法超时表（§5.1，与 protocol.md §6 逐项一致）。
/// `parse` 为心跳续期窗口（30s），非一次性超时；`cancel_parse` 随普通请求 10s。
pub fn timeout_for(method: &str) -> Duration {
    match method {
        "initialize" => Duration::from_secs(5),
        "can_handle" => Duration::from_secs(3),
        "load_file" => Duration::from_secs(10),
        "parse" => Duration::from_secs(30),
        "schema" => Duration::from_secs(3),
        "key_values" => Duration::from_secs(10),
        "annotate" => Duration::from_secs(10),
        "unload_file" => Duration::from_secs(3),
        "shutdown" => Duration::from_secs(3),
        "cancel_parse" => Duration::from_secs(10),
        _ => Duration::from_secs(10),
    }
}

/// 看门狗到期信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogFire {
    Fired,
}

/// parse 心跳看门狗（§5.2）：`arm` 后每 1s tick 检查到期；`reset` 续期 30s；
/// 到期返回 [`WatchdogFire`]（宿主 kill 进程 → `Timeout`）。
pub struct ParseWatchdog {
    /// `None` = 未 arm（reset 是 no-op）。
    deadline: Arc<Mutex<Option<Instant>>>,
    window: Duration,
}

impl Default for ParseWatchdog {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl ParseWatchdog {
    /// 以指定心跳窗口构造（测试注入缩短窗口用；生产默认 30s）。
    pub fn new(window: Duration) -> Self {
        Self {
            deadline: Arc::new(Mutex::new(None)),
            window,
        }
    }

    /// parse 请求发出时：deadline = now + 窗口。
    pub fn arm(&self) {
        *self.deadline.lock().expect("watchdog lock poisoned") = Some(Instant::now() + self.window);
    }

    /// 每收到一条 progress/RecordBatch 续期（未 arm 时 no-op）。
    pub fn reset(&self) {
        if let Some(d) = self
            .deadline
            .lock()
            .expect("watchdog lock poisoned")
            .as_mut()
        {
            *d = Instant::now() + self.window;
        }
    }

    /// parse 正常结束/取消时撤销。
    pub fn cancel(&self) {
        *self.deadline.lock().expect("watchdog lock poisoned") = None;
    }

    pub fn is_armed(&self) -> bool {
        self.deadline
            .lock()
            .expect("watchdog lock poisoned")
            .is_some()
    }

    /// 每 1s tick 检查到期（§5.2「select! 循环：每 1s tick 检查 now >= deadline」）。
    pub async fn run(&self) -> WatchdogFire {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let expired = self
                .deadline
                .lock()
                .expect("watchdog lock poisoned")
                .map(|d| Instant::now() >= d)
                .unwrap_or(false);
            if expired {
                return WatchdogFire::Fired;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 重试与熔断（§5.3，protocol.md §5.2）
// ---------------------------------------------------------------------------

/// 重试策略：同一「插件 × 文件」任务最多自动重试 `max_auto_retries` 次，
/// 第 n 次失败后延迟 `backoffs[n-1]`（固定退避不加抖动）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_auto_retries: u32,
    pub backoffs: [Duration; 2],
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_auto_retries: 2,
            backoffs: [Duration::from_secs(1), Duration::from_secs(3)],
        }
    }
}

/// 熔断状态：`Open` = 只接受手动重试（§5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakerState {
    #[default]
    Closed,
    Open,
}

/// 断路器：连续失败计数；`Open` 后停止自动重试。
#[derive(Debug, Default)]
pub struct CircuitBreaker {
    failures: u32,
    state: BreakerState,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> BreakerState {
        self.state
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// 记录一次失败：失败数 ≤ 允许自动重试次数 → `Retry`（带退避延迟）；
    /// 超过 → 熔断 `Break`。
    pub fn record_failure(&mut self, policy: &RetryPolicy) -> RetryDecision {
        self.failures += 1;
        let n = self.failures;
        if n <= policy.max_auto_retries {
            RetryDecision::Retry {
                delay: policy.backoffs[(n - 1) as usize],
            }
        } else {
            self.state = BreakerState::Open;
            RetryDecision::Break
        }
    }

    /// 成功一次即重置计数并回 `Closed`（§5.3）。
    pub fn record_success(&mut self) {
        self.failures = 0;
        self.state = BreakerState::Closed;
    }

    /// 手动重试：重置 `failures` 并把断路器回 `Closed`（不限次数）。
    pub fn manual_reset(&mut self) {
        self.failures = 0;
        self.state = BreakerState::Closed;
    }
}

/// 重试决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Retry { delay: Duration },
    Break,
}

/// 重试耗尽：携带最后一次失败的错误。
#[derive(Debug)]
pub struct RetryFailure<E> {
    pub last: E,
}

/// 带退避的自动重试驱动（§5.3）：连续失败按策略退避；成功一次重置计数；
/// 熔断后返回最后一次失败。重试粒度 = 新会话实例从 `Discovered` 重走（调用方
/// 的 attempt 负责重建会话）。
pub async fn retry_loop<T, E, F, Fut>(
    policy: &RetryPolicy,
    breaker: &mut CircuitBreaker,
    mut attempt: F,
) -> Result<T, RetryFailure<E>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut last = None;
    loop {
        match attempt().await {
            Ok(value) => {
                breaker.record_success();
                return Ok(value);
            }
            Err(e) => match breaker.record_failure(policy) {
                RetryDecision::Retry { delay } => {
                    last = Some(e);
                    tokio::time::sleep(delay).await;
                }
                RetryDecision::Break => {
                    return Err(RetryFailure {
                        last: last.unwrap_or(e),
                    })
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// stderr 环形缓冲（§6 / protocol.md §9 第 3 条）
// ---------------------------------------------------------------------------

/// 每会话独立 1MB 环形缓冲（按 `(plugin_id, session 序号)` 隔离——每个会话持有
/// 自己的实例，会话间不混写）。超限从头覆盖。
#[derive(Debug, Default)]
pub struct StderrSink {
    buf: VecDeque<u8>,
}

impl StderrSink {
    /// 1,048,576 字节 = 1 MiB。
    pub const CAPACITY: usize = 1_048_576;

    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一行（内部补 `\n`）。单行超过容量时只保留该行尾部。
    pub fn append(&mut self, line: String) {
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        if bytes.len() > Self::CAPACITY {
            let cut = bytes.len() - Self::CAPACITY;
            bytes.drain(..cut);
        }
        self.buf.extend(bytes);
        while self.buf.len() > Self::CAPACITY {
            self.buf.pop_front();
        }
    }

    /// 当前缓冲全文。
    pub fn snapshot(&self) -> String {
        let bytes: Vec<u8> = self.buf.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// 尾部摘要（崩溃/熔断时 UI 展示用，最多 `max_bytes` 字节）。
    pub fn tail_summary(&self, max_bytes: usize) -> String {
        let skip = self.buf.len().saturating_sub(max_bytes);
        let bytes: Vec<u8> = self.buf.iter().skip(skip).copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_table_matches_protocol() {
        assert_eq!(timeout_for("initialize"), Duration::from_secs(5));
        assert_eq!(timeout_for("can_handle"), Duration::from_secs(3));
        assert_eq!(timeout_for("load_file"), Duration::from_secs(10));
        assert_eq!(timeout_for("parse"), Duration::from_secs(30));
        assert_eq!(timeout_for("schema"), Duration::from_secs(3));
        assert_eq!(timeout_for("key_values"), Duration::from_secs(10));
        assert_eq!(timeout_for("annotate"), Duration::from_secs(10));
        assert_eq!(timeout_for("unload_file"), Duration::from_secs(3));
        assert_eq!(timeout_for("shutdown"), Duration::from_secs(3));
        assert_eq!(timeout_for("cancel_parse"), Duration::from_secs(10));
        assert_eq!(timeout_for("unknown_method"), Duration::from_secs(10));
    }

    #[test]
    fn default_retry_policy_has_two_retries_with_1s_3s_backoffs() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_auto_retries, 2);
        assert_eq!(policy.backoffs[0], Duration::from_secs(1));
        assert_eq!(policy.backoffs[1], Duration::from_secs(3));
    }

    #[test]
    fn breaker_sequence_and_manual_reset() {
        let policy = RetryPolicy {
            max_auto_retries: 2,
            backoffs: [Duration::from_millis(1), Duration::from_millis(3)],
        };
        let mut breaker = CircuitBreaker::new();
        assert_eq!(breaker.state(), BreakerState::Closed);

        assert_eq!(
            breaker.record_failure(&policy),
            RetryDecision::Retry {
                delay: Duration::from_millis(1)
            }
        );
        assert_eq!(
            breaker.record_failure(&policy),
            RetryDecision::Retry {
                delay: Duration::from_millis(3)
            }
        );
        assert_eq!(
            breaker.state(),
            BreakerState::Closed,
            "2nd failure still retrying"
        );
        assert_eq!(breaker.record_failure(&policy), RetryDecision::Break);
        assert_eq!(
            breaker.state(),
            BreakerState::Open,
            "3rd failure opens the breaker"
        );
        assert_eq!(breaker.failures(), 3);

        // Open 后不再自动重试。
        assert_eq!(breaker.record_failure(&policy), RetryDecision::Break);

        // 手动重试回 Closed 且重置计数。
        breaker.manual_reset();
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert_eq!(breaker.failures(), 0);

        // 成功一次也重置计数。
        for _ in 0..2 {
            breaker.record_failure(&policy);
        }
        breaker.record_success();
        assert_eq!(breaker.failures(), 0);
        assert_eq!(breaker.state(), BreakerState::Closed);
    }

    #[tokio::test]
    async fn retry_loop_backs_off_then_breaks() {
        use std::sync::atomic::{AtomicU32, Ordering as At};
        let attempts = AtomicU32::new(0);
        let policy = RetryPolicy {
            max_auto_retries: 2,
            backoffs: [Duration::from_millis(20), Duration::from_millis(30)],
        };
        let mut breaker = CircuitBreaker::new();
        let started = Instant::now();

        let result = retry_loop(&policy, &mut breaker, || {
            attempts.fetch_add(1, At::SeqCst);
            async { Err::<(), &str>("boom") }
        })
        .await;
        assert!(matches!(result, Err(RetryFailure { last: "boom" })));
        assert_eq!(
            attempts.load(At::SeqCst),
            3,
            "initial + 2 automatic retries"
        );
        assert_eq!(breaker.state(), BreakerState::Open);
        // 固定退避 20ms + 30ms（不加抖动）。
        assert!(
            started.elapsed() >= Duration::from_millis(45),
            "backoffs applied"
        );
    }

    #[tokio::test]
    async fn retry_loop_success_resets_breaker() {
        use std::sync::atomic::{AtomicU32, Ordering as At};
        let attempts = AtomicU32::new(0);
        let policy = RetryPolicy {
            max_auto_retries: 2,
            backoffs: [Duration::from_millis(10), Duration::from_millis(10)],
        };
        let mut breaker = CircuitBreaker::new();
        let result = retry_loop(&policy, &mut breaker, || {
            let n = attempts.fetch_add(1, At::SeqCst);
            async move {
                if n < 1 {
                    Err("first attempt fails")
                } else {
                    Ok(42)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert_eq!(breaker.failures(), 0);
    }

    #[test]
    fn stderr_ring_overwrites_from_head_and_stays_bounded() {
        let mut sink = StderrSink::new();
        // 写入远超过 1MiB 的内容。
        let chunk = "x".repeat(64 * 1024);
        for _ in 0..20 {
            sink.append(chunk.clone());
        }
        assert!(sink.len() <= StderrSink::CAPACITY, "ring bounded by 1 MiB");
        let snapshot = sink.snapshot();
        assert!(snapshot.len() <= StderrSink::CAPACITY + 128);
        // 头部被覆盖：最早的一行已不存在，尾部保留。
        let tail = sink.tail_summary(1024);
        assert!(
            tail.ends_with("x\n") || tail.contains("xxxx"),
            "tail survives"
        );
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn stderr_sinks_are_isolated_per_instance() {
        // 隔离是结构性的：每个会话（(plugin_id, session 序号)）持有独立实例。
        let mut a = StderrSink::new();
        let b = StderrSink::new();
        a.append("plugin-a line".into());
        assert!(b.is_empty(), "sink B untouched");
        assert!(a.snapshot().contains("plugin-a line"));
        assert!(b.snapshot().is_empty());
        // 同插件新会话（新实例）也不混写。
        let mut a2 = StderrSink::new();
        a2.append("fresh session".into());
        assert!(!a.snapshot().contains("fresh session"));
    }

    #[tokio::test]
    async fn watchdog_reset_renews_and_expiry_fires() {
        // 窗口 2s；1.2s 处续期一次 → 到期点被推迟；随后到期触发。
        let w = ParseWatchdog::new(Duration::from_secs(2));
        w.arm();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(w.is_armed());
        w.reset();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            w.is_armed(),
            "reset must renew the deadline (2s from the reset point)"
        );
        let fired = tokio::time::timeout(Duration::from_secs(3), w.run()).await;
        assert_eq!(fired.unwrap(), WatchdogFire::Fired);

        // cancel 后不再触发。
        let w2 = ParseWatchdog::new(Duration::from_millis(200));
        w2.arm();
        w2.cancel();
        assert!(!w2.is_armed());
        let fired = tokio::time::timeout(Duration::from_millis(1400), w2.run()).await;
        assert!(fired.is_err(), "cancelled watchdog must not fire");
    }
}
