//! 令牌桶限速器
//!
//! 提供跨分片共享的实时带宽控制。所有并发分片通过同一个
//! `RateLimiter` 实例协调,确保全局速率不超过配置上限。
//!
//! # 算法
//!
//! 令牌桶以恒定速率 `bytes_per_sec` 补充令牌。每次写入消耗对应字节数的令牌。
//! 令牌不足时,计算精确等待时间后 sleep。初始令牌等于速率值,允许首秒满速突发。

use std::sync::Mutex;
use std::time::Instant;

/// 令牌桶限速器
///
/// 线程安全,可跨多个异步分片任务共享。
pub struct RateLimiter {
    state: Mutex<BucketState>,
    bytes_per_sec: u64,
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
            state: Mutex::new(BucketState {
                tokens: bytes_per_sec as f64,
                last_refill: Instant::now(),
            }),
            bytes_per_sec,
        }
    }

    /// 获取指定字节数的令牌,不足时异步等待
    ///
    /// 调用方在每次存储写入后调用此方法,传入实际写入的字节数。
    /// 令牌充足时立即返回;不足时计算精确等待时间后返回。
    pub async fn acquire(&self, bytes: u64) {
        if self.bytes_per_sec == 0 || bytes == 0 {
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
            let capacity = self.bytes_per_sec as f64;
            state.tokens = (state.tokens + elapsed * self.bytes_per_sec as f64).min(capacity);

            if state.tokens >= bytes as f64 {
                state.tokens -= bytes as f64;
                0.0
            } else {
                let deficit = bytes as f64 - state.tokens;
                state.tokens = 0.0;
                deficit / self.bytes_per_sec as f64
            }
        };

        if wait_secs > 0.0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait_secs)).await;
        }
    }

    /// 获取配置的速率(bytes/sec)
    pub fn bytes_per_sec(&self) -> u64 {
        self.bytes_per_sec
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
}
