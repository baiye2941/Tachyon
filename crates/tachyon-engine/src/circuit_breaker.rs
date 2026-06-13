use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 熔断器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// 正常 — 请求放行
    Closed,
    /// 熔断 — 请求被拒绝
    Open,
    /// 半开 — 允许试探请求通过
    HalfOpen,
}

/// 单个源的熔断器
pub struct CircuitBreaker {
    /// 连续失败次数
    failure_count: u32,
    /// 熔断阈值(连续失败多少次后熔断)
    failure_threshold: u32,
    /// 熔断持续时间
    open_duration: Duration,
    /// 熔断开启时刻
    opened_at: Option<Instant>,
    /// 半开状态下的试探请求是否已发出
    half_open_probe_sent: bool,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, open_duration: Duration) -> Self {
        Self {
            failure_count: 0,
            failure_threshold,
            open_duration,
            opened_at: None,
            half_open_probe_sent: false,
        }
    }

    /// 查询当前状态
    pub fn state(&self) -> CircuitState {
        match self.opened_at {
            None => CircuitState::Closed,
            Some(opened_at) => {
                if opened_at.elapsed() >= self.open_duration {
                    CircuitState::HalfOpen
                } else {
                    CircuitState::Open
                }
            }
        }
    }

    /// 请求是否被放行
    pub fn allow(&mut self) -> bool {
        match self.state() {
            CircuitState::Closed => true,
            CircuitState::Open => false,
            CircuitState::HalfOpen => {
                if self.half_open_probe_sent {
                    false
                } else {
                    self.half_open_probe_sent = true;
                    true
                }
            }
        }
    }

    /// 记录成功
    pub fn record_success(&mut self) {
        self.failure_count = 0;
        self.opened_at = None;
        self.half_open_probe_sent = false;
    }

    /// 记录失败
    pub fn record_failure(&mut self) {
        self.failure_count += 1;
        if self.failure_count >= self.failure_threshold {
            self.opened_at = Some(Instant::now());
            self.half_open_probe_sent = false;
        }
    }
}

/// 每源熔断器管理器
pub struct SourceCircuitBreakers {
    breakers: Arc<Mutex<HashMap<String, CircuitBreaker>>>,
    failure_threshold: u32,
    open_duration: Duration,
}

impl Clone for SourceCircuitBreakers {
    fn clone(&self) -> Self {
        Self {
            breakers: Arc::clone(&self.breakers),
            failure_threshold: self.failure_threshold,
            open_duration: self.open_duration,
        }
    }
}

impl SourceCircuitBreakers {
    pub fn new(failure_threshold: u32, open_duration: Duration) -> Self {
        Self {
            breakers: Arc::new(Mutex::new(HashMap::new())),
            failure_threshold,
            open_duration,
        }
    }

    /// 检查指定源是否被放行
    pub fn allow(&self, source: &str) -> bool {
        let mut breakers = self.breakers.lock().unwrap();
        breakers
            .entry(source.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.failure_threshold, self.open_duration))
            .allow()
    }

    /// 记录指定源成功
    pub fn record_success(&self, source: &str) {
        let mut breakers = self.breakers.lock().unwrap();
        if let Some(b) = breakers.get_mut(source) {
            b.record_success();
        }
    }

    /// 记录指定源失败
    pub fn record_failure(&self, source: &str) {
        let mut breakers = self.breakers.lock().unwrap();
        breakers
            .entry(source.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.failure_threshold, self.open_duration))
            .record_failure();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_circuit_breaker_closed_by_default() {
        let cb = CircuitBreaker::new(5, Duration::from_secs(30));
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_allow_when_closed() {
        let mut cb = CircuitBreaker::new(5, Duration::from_secs(30));
        assert!(cb.allow());
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow());
    }

    #[test]
    fn test_circuit_breaker_half_open_after_duration() {
        let mut cb = CircuitBreaker::new(2, Duration::from_millis(50));
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        thread::sleep(Duration::from_millis(60));
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_breaker_half_open_allows_one_probe() {
        let mut cb = CircuitBreaker::new(2, Duration::from_millis(50));
        cb.record_failure();
        cb.record_failure();
        thread::sleep(Duration::from_millis(60));
        assert!(cb.allow()); // 第一个试探请求通过
        assert!(!cb.allow()); // 第二个被挡
    }

    #[test]
    fn test_circuit_breaker_success_closes() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_failure();
        cb.record_failure();
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.failure_count, 0);
    }

    #[test]
    fn test_circuit_breaker_probe_success_closes() {
        let mut cb = CircuitBreaker::new(2, Duration::from_millis(50));
        cb.record_failure();
        cb.record_failure();
        thread::sleep(Duration::from_millis(60));
        assert!(cb.allow());
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_probe_failure_reopens() {
        let mut cb = CircuitBreaker::new(2, Duration::from_millis(50));
        cb.record_failure();
        cb.record_failure();
        thread::sleep(Duration::from_millis(60));
        assert!(cb.allow());
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_source_circuit_breakers_allow_and_record() {
        let scb = SourceCircuitBreakers::new(3, Duration::from_secs(30));
        assert!(scb.allow("http://example.com/a"));
        scb.record_failure("http://example.com/a");
        scb.record_failure("http://example.com/a");
        assert!(scb.allow("http://example.com/a"));
        scb.record_failure("http://example.com/a");
        assert!(!scb.allow("http://example.com/a"));
    }

    #[test]
    fn test_source_circuit_breakers_isolated_per_source() {
        let scb = SourceCircuitBreakers::new(2, Duration::from_secs(30));
        scb.record_failure("http://example.com/a");
        scb.record_failure("http://example.com/a");
        assert!(!scb.allow("http://example.com/a"));
        // 源 B 不受影响
        assert!(scb.allow("http://example.com/b"));
    }

    #[test]
    fn test_source_circuit_breakers_success_resets() {
        let scb = SourceCircuitBreakers::new(2, Duration::from_secs(30));
        scb.record_failure("http://example.com/a");
        scb.record_success("http://example.com/a");
        scb.record_failure("http://example.com/a");
        // 成功重置了失败计数,所以第二次失败后仍处于 Closed
        assert!(scb.allow("http://example.com/a"));
    }

    #[test]
    fn test_source_circuit_breakers_half_open_probe() {
        let scb = SourceCircuitBreakers::new(2, Duration::from_millis(50));
        scb.record_failure("http://example.com/a");
        scb.record_failure("http://example.com/a");
        assert!(!scb.allow("http://example.com/a"));
        thread::sleep(Duration::from_millis(60));
        assert!(scb.allow("http://example.com/a"));
        assert!(!scb.allow("http://example.com/a"));
    }
}
