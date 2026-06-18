//! 令牌桶限速器
//!
//! 提供跨分片共享的实时带宽控制。所有并发分片通过同一个
//! `RateLimiter` 实例协调,确保全局速率不超过配置上限。
//!
//! # 算法
//!
//! 采用"累计债务"模型实现无锁限速:
//! - 以进程启动后的相对纳秒数为时间基准;
//! - `debt` 记录自 `baseline` 以来累计申请的令牌(字节);
//! - 当前允许的字节数为 `elapsed * rate / 1e9`;
//! - 若 `debt > allowed`,则 sleep 差值对应的时间。
//!
//! 为保留原 Mutex 实现"初始令牌 = bytes_per_sec"的突发能力,
//! `baseline` 初始设为 `-1_000_000_000`(即基准时间比锚点早 1 秒),
//! 因此刚创建时 `allowed ≈ rate`。
//!
//! 该实现完全基于原子操作,消除了 `Mutex` 在 Windows 高并发
//! 场景下的锁竞争与上下文切换,同时保持原有公共接口不变。

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 单次 acquire 最大等待时间(秒)
///
/// 超过此阈值的等待会被截断并记录警告,防止在高并发场景下
/// 个别请求因令牌长期不足而被无限期饿死。
const MAX_ACQUIRE_WAIT_SECS: f64 = 5.0;

/// 令牌桶限速器
///
/// 线程安全,可跨多个异步分片任务共享。
/// 支持运行时动态更新速率(bytes_per_sec),用于带宽自适应限速。
pub struct RateLimiter {
    /// 债务计数的时间基准: debt 在该时刻(相对于 anchor 的 ns 偏移)理论值为 0。
    /// 初始设为 -1 秒,等价于初始可用令牌 = rate,允许首秒满速突发。
    baseline_ns: AtomicI64,
    /// 自 baseline 以来累计申请的令牌(字节)。
    debt: AtomicU64,
    /// 当前限速速率(bytes/sec)。0 表示不限速。
    bytes_per_sec: AtomicU64,
    /// 固定的锚点时间,用于获取单调递增的相对时间。
    anchor: Instant,
}

/// 取消安全 guard:若 Future 在 sleep 期间被 abort,
/// drop 时回滚已增加的 debt,避免令牌桶永久泄漏。
/// 正常完成时调用 `disarm` 解除回滚。
struct CancelGuard<'a> {
    debt: &'a AtomicU64,
    /// `Some(bytes)` 表示仍需要回滚的字节数;`None` 表示已正常完成。
    bytes: Option<u64>,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if let Some(bytes) = self.bytes.take() {
            // 仅发布 debt 减量即可;Acquire 语义不必要,
            // 因为 drop 不会基于旧值做决策,只须让后续 Acquire 读可见。
            self.debt.fetch_sub(bytes, Ordering::Release);
        }
    }
}

impl<'a> CancelGuard<'a> {
    /// 标记任务正常完成,drop 时不再回滚债务。
    fn disarm(mut self) {
        self.bytes = None;
    }
}

impl RateLimiter {
    /// 创建限速器
    ///
    /// `bytes_per_sec` = 0 时等同于不限速(调用方应提前过滤)。
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            baseline_ns: AtomicI64::new(-1_000_000_000),
            debt: AtomicU64::new(0),
            bytes_per_sec: AtomicU64::new(bytes_per_sec),
            anchor: Instant::now(),
        }
    }

    /// 获取指定字节数的令牌,不足时异步等待
    ///
    /// 调用方在每次存储写入后调用此方法,传入实际写入的字节数。
    /// 令牌充足时立即返回;不足时计算精确等待时间后返回。
    ///
    /// 取消安全:若 Future 在等待期间被取消(如任务 abort),
    /// 已增加的 `debt` 会被回滚,避免令牌桶永久泄漏。
    pub async fn acquire(&self, bytes: u64) {
        let rate = self.bytes_per_sec.load(Ordering::Acquire);
        if rate == 0 || bytes == 0 {
            return;
        }

        // 原子增加债务;获取到的 old_debt 是本请求之前的累计值。
        let _old_debt = self.debt.fetch_add(bytes, Ordering::AcqRel);
        let total_debt = _old_debt + bytes;

        let now_ns = self.anchor.elapsed().as_nanos() as i64;
        let baseline = self.baseline_ns.load(Ordering::Acquire);
        let elapsed = now_ns.saturating_sub(baseline) as u64;
        let allowed = (elapsed as u128 * rate as u128 / 1_000_000_000) as u64;

        if total_debt > allowed {
            let deficit = total_debt - allowed;
            let wait_ns = (deficit as u128 * 1_000_000_000 / rate as u128) as u64;
            let clamped = wait_ns.min((MAX_ACQUIRE_WAIT_SECS * 1e9) as u64);

            // CancelSafeSleep:在 sleep 完成后解除 guard,否则在 drop 时回滚 debt
            let cancel_guard = CancelGuard {
                debt: &self.debt,
                bytes: Some(bytes),
            };
            tokio::time::sleep(Duration::from_nanos(clamped)).await;
            // sleep 正常完成,解除 guard 防止 drop 时回滚
            cancel_guard.disarm();
        }
    }

    /// 动态更新限速速率(bytes/sec)
    ///
    /// 用于带宽自适应:根据调度器的带宽观测值动态调整限速。
    /// 更新立即生效,正在进行的 acquire 等待不受影响。
    pub fn update_rate(&self, bytes_per_sec: u64) {
        let old_rate = self.bytes_per_sec.load(Ordering::Acquire);
        if bytes_per_sec == 0 {
            self.bytes_per_sec.store(0, Ordering::Release);
            return;
        }

        let now_ns = self.anchor.elapsed().as_nanos() as i64;
        if old_rate == 0 {
            // 从无限速切换到限速:重置时间基准并清空债务
            self.baseline_ns.store(now_ns, Ordering::Release);
            self.debt.store(0, Ordering::Release);
        } else {
            // 从限速 A 切换到限速 B:按旧速率将已用时间折算为债务,
            // 以当前时间为新基准,保持限速连续性。
            let baseline = self.baseline_ns.load(Ordering::Acquire);
            let elapsed = (now_ns.saturating_sub(baseline)) as u64;
            let used = (elapsed as u128 * old_rate as u128 / 1_000_000_000) as u64;
            self.baseline_ns.store(now_ns, Ordering::Release);

            // 用 CAS 保留并发 acquire 新增的 debt,避免 store 覆盖并发增量。
            // 计算目标值 = used + (current - snapshot),其中 snapshot 为进入更新时读到的 debt。
            // 若期间无并发 acquire,current == snapshot,目标值即为 used;
            // 若有并发 acquire,current > snapshot,目标值叠加了并发增量。
            let snapshot = self.debt.load(Ordering::Acquire);
            let mut current = snapshot;
            loop {
                let new = current.saturating_add(used).saturating_sub(snapshot);
                match self.debt.compare_exchange_weak(
                    current,
                    new,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
        }
        self.bytes_per_sec.store(bytes_per_sec, Ordering::Release);
    }

    /// 获取当前速率(bytes/sec)
    pub fn bytes_per_sec(&self) -> u64 {
        self.bytes_per_sec.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn acquire_zero_bytes_returns_immediately() {
        let limiter = RateLimiter::new(1024);
        let start = Instant::now();
        limiter.acquire(0).await;
        assert!(start.elapsed().as_millis() < 10);
    }

    #[tokio::test]
    async fn acquire_within_initial_tokens_returns_immediately() {
        // 初始令牌 = bytes_per_sec = 1024
        let limiter = RateLimiter::new(1024);
        let start = Instant::now();
        limiter.acquire(512).await;
        assert!(start.elapsed().as_millis() < 10);
    }

    #[tokio::test]
    async fn acquire_exceeding_tokens_waits() {
        // 初始令牌 = 100 bytes/sec
        let limiter = RateLimiter::new(100);
        // 消耗初始令牌
        limiter.acquire(100).await;
        // 再请求 100 字节,应等待约 1 秒
        let start = Instant::now();
        limiter.acquire(100).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() >= 800,
            "应等待约 1 秒,实际: {}ms",
            elapsed.as_millis()
        );
    }

    #[tokio::test]
    async fn concurrent_acquire_does_not_panic() {
        let limiter = Arc::new(RateLimiter::new(1024 * 1024)); // 1MB/s
        let mut handles = Vec::new();
        for _ in 0..10 {
            let limiter = limiter.clone();
            handles.push(tokio::spawn(async move {
                limiter.acquire(1024).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
    }

    #[tokio::test]
    async fn bytes_per_sec_returns_configured_value() {
        let limiter = RateLimiter::new(4096);
        assert_eq!(limiter.bytes_per_sec(), 4096);
    }

    #[tokio::test]
    async fn update_rate_changes_bytes_per_sec() {
        let limiter = RateLimiter::new(1024);
        assert_eq!(limiter.bytes_per_sec(), 1024);
        limiter.update_rate(2048);
        assert_eq!(limiter.bytes_per_sec(), 2048);
    }

    #[tokio::test]
    async fn update_rate_to_zero_disables_limiting() {
        let limiter = RateLimiter::new(100);
        // 消耗初始令牌
        limiter.acquire(100).await;
        // 更新为 0 应禁用限速
        limiter.update_rate(0);
        let start = Instant::now();
        limiter.acquire(1000).await;
        assert!(
            start.elapsed().as_millis() < 10,
            "rate=0 时 acquire 应立即返回"
        );
    }

    #[tokio::test]
    async fn concurrent_acquire_honors_rate_limit() {
        // 100 bytes/sec, 10 个并发任务各请求 50 字节 = 500 字节
        // 初始突发 100 tokens,还需 400 字节,按 100 bytes/sec 至少 4 秒
        let limiter = Arc::new(RateLimiter::new(100));
        let start = Instant::now();

        let mut handles = Vec::new();
        for _ in 0..10 {
            let limiter = limiter.clone();
            handles.push(tokio::spawn(async move {
                limiter.acquire(50).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs_f64() >= 3.5,
            "并发请求应受限速约束,实际: {:.2}s",
            elapsed.as_secs_f64()
        );
        assert!(
            elapsed.as_secs_f64() < 7.0,
            "并发请求不应过度等待,实际: {:.2}s",
            elapsed.as_secs_f64()
        );
    }

    #[tokio::test]
    async fn concurrent_update_rate_preserves_debt() {
        // 并发 acquire 与 update_rate 不应导致 debt 被覆盖或 panic。
        // 该测试主要验证:在大量并发更新下,限速器仍保持一致性并可查询。
        let limiter = Arc::new(RateLimiter::new(1_000_000));
        let mut handles = Vec::new();

        for _ in 0..4 {
            let limiter = limiter.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    limiter.acquire(1024).await;
                }
            }));
        }

        let updater = {
            let limiter = limiter.clone();
            tokio::spawn(async move {
                for i in 1..100 {
                    limiter.update_rate(1_000_000 + i * 100);
                }
            })
        };

        for h in handles {
            h.await.unwrap();
        }
        updater.await.unwrap();

        assert!(limiter.bytes_per_sec() > 0);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // 随机创建速率并在零/非零之间切换,限速器不应 panic 且 bytes_per_sec 保持一致
    proptest! {
        #[test]
        fn test_rate_limiter_update_rate_invariant(
            initial_rate in 0u64..1024 * 1024u64,
            updates in prop::collection::vec(0u64..1024 * 1024u64, 1..20),
        ) {
            let limiter = RateLimiter::new(initial_rate);
            prop_assert_eq!(limiter.bytes_per_sec(), initial_rate);

            for rate in updates {
                limiter.update_rate(rate);
                prop_assert_eq!(limiter.bytes_per_sec(), rate);
            }
        }

        // rate=0 或 bytes=0 时 acquire 应立即返回且不 panic
        #[test]
        fn test_rate_limiter_acquire_zero_or_rate_zero_no_panic(
            zero_rate in prop::bool::ANY,
            value in 0u64..1024u64,
        ) {
            let (rate, bytes) = if zero_rate {
                (0, value)
            } else {
                (value, 0)
            };
            let limiter = RateLimiter::new(rate);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            rt.block_on(async {
                limiter.acquire(bytes).await;
            });
            prop_assert_eq!(limiter.bytes_per_sec(), rate);
        }
    }
}
