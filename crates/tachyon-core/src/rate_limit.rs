//! 令牌桶限速器
//!
//! 提供跨分片共享的实时带宽控制。所有并发分片通过同一个
//! `RateLimiter` 实例协调,确保全局速率不超过配置上限。
//!
//! # 算法
//!
//! 令牌桶以恒定速率 `bytes_per_sec` 补充令牌。每次写入消耗对应字节数的令牌。
//! 令牌不足时,计算精确等待时间后 sleep。初始令牌等于速率值,允许首秒满速突发。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

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
    state: std::sync::Mutex<BucketState>,
    bytes_per_sec: AtomicU64,
}

struct BucketState {
    /// 当前可用令牌数(字节)
    tokens: f64,
    /// 上次补充令牌的时间
    last_refill: Instant,
}

impl RateLimiter {
    /// 创建限速器
    ///
    /// `bytes_per_sec` = 0 时等同于不限速(调用方应提前过滤)。
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            state: std::sync::Mutex::new(BucketState {
                tokens: bytes_per_sec as f64,
                last_refill: Instant::now(),
            }),
            bytes_per_sec: AtomicU64::new(bytes_per_sec),
        }
    }

    /// 获取指定字节数的令牌,不足时异步等待
    ///
    /// 调用方在每次存储写入后调用此方法,传入实际写入的字节数。
    /// 令牌充足时立即返回;不足时计算精确等待时间后返回。
    pub async fn acquire(&self, bytes: u64) {
        let rate = self.bytes_per_sec.load(Ordering::Acquire);
        if rate == 0 || bytes == 0 {
            return;
        }

        let wait_secs = {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| {
                tracing::error!("限速器锁已 poison,继续使用");
                poisoned.into_inner()
            });
            let now = Instant::now();
            let elapsed = now.duration_since(state.last_refill).as_secs_f64();
            state.last_refill = now;

            // 补充令牌(不超过桶容量)
            let capacity = rate as f64;
            state.tokens = (state.tokens + elapsed * rate as f64).min(capacity);

            if state.tokens >= bytes as f64 {
                state.tokens -= bytes as f64;
                0.0
            } else {
                let deficit = bytes as f64 - state.tokens;
                state.tokens = 0.0;
                deficit / rate as f64
            }
        };

        if wait_secs > 0.0 {
            // W-03: 截断过长等待时间,防止高并发下个别请求被饿死
            let clamped = if wait_secs > MAX_ACQUIRE_WAIT_SECS {
                tracing::warn!(
                    requested_wait_secs = wait_secs,
                    max_wait_secs = MAX_ACQUIRE_WAIT_SECS,
                    "令牌桶等待时间超限,已截断"
                );
                MAX_ACQUIRE_WAIT_SECS
            } else {
                wait_secs
            };
            tokio::time::sleep(std::time::Duration::from_secs_f64(clamped)).await;
        }
    }

    /// 动态更新限速速率(bytes/sec)
    ///
    /// 用于带宽自适应:根据调度器的带宽观测值动态调整限速。
    /// 更新立即生效,正在进行的 acquire 等待不受影响。
    pub fn update_rate(&self, bytes_per_sec: u64) {
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
}
