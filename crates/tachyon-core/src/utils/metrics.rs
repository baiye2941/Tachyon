//! 无锁下载指标计数器
//!
//! 使用 AtomicU64 实现高并发场景下的零锁性能统计。
//! 每个原子字段独占一个 Cache Line(64 字节),消除多核并发
//! 场景下的 False Sharing。

use crossbeam_utils::CachePadded;
use std::sync::atomic::{AtomicU64, Ordering};

/// 下载性能指标计数器
///
/// 使用 AtomicU64 + CachePadded 实现无锁统计,适用于高并发下载场景。
/// 每个字段独占一个 Cache Line,避免 16+ 并发分片同时写入时的
/// Cache Line 弹跳(Cache Line Bouncing)。
///
/// 各字段含义:
/// - `bytes_downloaded`: 累计已下载字节数
/// - `fragments_completed`: 已完成的分片数
/// - `errors`: 错误计数
///
/// 注意: 当前为预留的生产可观测性接口,待下游模块集成后启用。
/// 测试代码可直接使用,生产代码调用前需确认集成状态。
#[derive(Debug)]
pub struct Metrics {
    /// 累计已下载字节数(独占 Cache Line)
    pub bytes_downloaded: CachePadded<AtomicU64>,
    /// 已完成的分片数(独占 Cache Line)
    pub fragments_completed: CachePadded<AtomicU64>,
    /// 错误计数(独占 Cache Line)
    pub errors: CachePadded<AtomicU64>,
}

impl Metrics {
    /// 创建全零初始化的指标实例
    pub fn new() -> Self {
        Self {
            bytes_downloaded: CachePadded::new(AtomicU64::new(0)),
            fragments_completed: CachePadded::new(AtomicU64::new(0)),
            errors: CachePadded::new(AtomicU64::new(0)),
        }
    }

    /// 原子累加下载字节数
    pub fn add_bytes(&self, n: u64) {
        self.bytes_downloaded.fetch_add(n, Ordering::AcqRel);
    }

    /// 原子递增完成分片数
    pub fn inc_fragment(&self) {
        self.fragments_completed.fetch_add(1, Ordering::AcqRel);
    }

    /// 原子递增错误计数
    pub fn inc_error(&self) {
        self.errors.fetch_add(1, Ordering::AcqRel);
    }

    /// 读取当前指标快照(Acquire 语义,保证看到最新的写入)
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.bytes_downloaded.load(Ordering::Acquire),
            self.fragments_completed.load(Ordering::Acquire),
            self.errors.load(Ordering::Acquire),
        )
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_metrics_counters() {
        let m = Metrics::new();
        assert_eq!(m.bytes_downloaded.load(Ordering::Relaxed), 0);
        assert_eq!(m.fragments_completed.load(Ordering::Relaxed), 0);
        assert_eq!(m.errors.load(Ordering::Relaxed), 0);

        m.add_bytes(1024);
        m.add_bytes(2048);
        assert_eq!(m.bytes_downloaded.load(Ordering::Relaxed), 3072);

        m.inc_fragment();
        m.inc_fragment();
        m.inc_fragment();
        assert_eq!(m.fragments_completed.load(Ordering::Relaxed), 3);

        m.inc_error();
        assert_eq!(m.errors.load(Ordering::Relaxed), 1);

        let m2 = Metrics::default();
        assert_eq!(m2.bytes_downloaded.load(Ordering::Relaxed), 0);
    }

    // -----------------------------------------------------------------------
    // P1: snapshot 与并发正确性
    // -----------------------------------------------------------------------

    #[test]
    fn test_metrics_snapshot() {
        let m = Metrics::new();
        assert_eq!(m.snapshot(), (0, 0, 0));

        m.add_bytes(100);
        m.inc_fragment();
        m.inc_error();
        assert_eq!(m.snapshot(), (100, 1, 1));
    }

    #[test]
    fn test_metrics_concurrent_updates_final_counts() {
        let m = std::sync::Arc::new(Metrics::new());
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let m = Arc::clone(&m);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        m.add_bytes(10);
                        m.inc_fragment();
                        m.inc_error();
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        assert_eq!(m.snapshot(), (40_000, 4_000, 4_000));
    }
}
