//! IPC 命令速率限制器
//!
//! 使用滑动窗口计数器限制 Tauri IPC 命令的调用频率，
//! 防止恶意 webview 内容通过高频调用消耗资源。

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 滑动窗口速率限制器
///
/// 在固定时间窗口内限制最大请求数。使用 `Mutex<(Instant, u32)>` 保护窗口状态，
/// IPC 命令级别的锁竞争可忽略，实现简单且正确。
pub struct SlidingWindowRateLimiter {
    /// 窗口大小
    window: Duration,
    /// 窗口内最大请求数
    max_requests: u32,
    /// 当前窗口起始时间与请求计数
    state: Mutex<(Instant, u32)>,
}

impl SlidingWindowRateLimiter {
    /// 创建新的滑动窗口速率限制器
    ///
    /// # 参数
    /// - `window`: 时间窗口大小
    /// - `max_requests`: 窗口内允许的最大请求数
    pub fn new(window: Duration, max_requests: u32) -> Self {
        Self {
            window,
            max_requests,
            state: Mutex::new((Instant::now(), 0)),
        }
    }

    /// 尝试获取一个许可
    ///
    /// 返回 `true` 表示允许请求，`false` 表示超限。
    pub fn try_acquire(&self) -> bool {
        let now = Instant::now();
        let mut guard = self.state.lock().unwrap();
        let (window_start, count) = &mut *guard;

        if now.duration_since(*window_start) >= self.window {
            // 窗口已过期，重置
            *window_start = now;
            *count = 1;
            true
        } else {
            if *count < self.max_requests {
                *count += 1;
                true
            } else {
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_normal_requests_allowed() {
        let limiter = SlidingWindowRateLimiter::new(Duration::from_secs(1), 10);
        for _ in 0..10 {
            assert!(limiter.try_acquire(), "前 10 次请求应通过");
        }
    }

    #[test]
    fn test_excess_requests_rejected() {
        let limiter = SlidingWindowRateLimiter::new(Duration::from_secs(1), 10);
        for _ in 0..10 {
            assert!(limiter.try_acquire(), "前 10 次请求应通过");
        }
        assert!(!limiter.try_acquire(), "第 11 次请求应被拒绝");
    }

    #[test]
    fn test_window_resets_after_duration() {
        let limiter = SlidingWindowRateLimiter::new(Duration::from_millis(50), 2);
        assert!(limiter.try_acquire(), "第 1 次请求应通过");
        assert!(limiter.try_acquire(), "第 2 次请求应通过");
        assert!(!limiter.try_acquire(), "第 3 次请求应被拒绝");

        // 等待窗口过期
        thread::sleep(Duration::from_millis(60));
        assert!(limiter.try_acquire(), "窗口过期后请求应通过");
    }

    #[test]
    fn test_concurrent_requests_race_safe() {
        let limiter = std::sync::Arc::new(SlidingWindowRateLimiter::new(Duration::from_secs(1), 100));
        let mut handles = Vec::new();

        for _ in 0..10 {
            let limiter = limiter.clone();
            handles.push(thread::spawn(move || {
                let mut success = 0;
                for _ in 0..20 {
                    if limiter.try_acquire() {
                        success += 1;
                    }
                }
                success
            }));
        }

        let total_success: u32 = handles.into_iter().map(|h| h.join().unwrap() as u32).sum();
        // 总共 200 次请求，但窗口内只允许 100 次
        assert_eq!(total_success, 100, "并发请求总数应被限制在 100");
    }
}
