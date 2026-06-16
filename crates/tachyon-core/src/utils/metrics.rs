//! 无锁下载指标计数器
//!
//! 使用 AtomicU64 实现高并发场景下的零锁性能统计。

use std::sync::atomic::{AtomicU64, Ordering};

/// 下载性能指标计数器
///
/// 使用 AtomicU64 实现无锁统计,适用于高并发下载场景。
/// 各字段含义:
/// - `bytes_downloaded`: 累计已下载字节数
/// - `fragments_completed`: 已完成的分片数
/// - `errors`: 错误计数
///
/// 注意: 当前为预留的生产可观测性接口,待下游模块集成后启用。
/// 测试代码可直接使用,生产代码调用前需确认集成状态。
#[derive(Debug)]
pub struct Metrics {
    /// 累计已下载字节数
    pub bytes_downloaded: AtomicU64,
    /// 已完成的分片数
    pub fragments_completed: AtomicU64,
    /// 错误计数
    pub errors: AtomicU64,
}

impl Metrics {
    /// 创建全零初始化的指标实例
    pub fn new() -> Self {
        Self {
            bytes_downloaded: AtomicU64::new(0),
            fragments_completed: AtomicU64::new(0),
            errors: AtomicU64::new(0),
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
}
