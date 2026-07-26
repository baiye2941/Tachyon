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
/// - `aligned_write_passthrough`: 写路径缓冲已 512 对齐、零拷贝直写次数
/// - `aligned_write_copied`: 写路径缓冲未对齐、拷入 AlignedBuf 次数
/// - `rebalance_count`: 慢片 rebalance 成功拆分并重入队次数
/// - `rebalance_dropped`: rebalance 因 channel Full/Closed 回滚次数
///
/// 引擎写路径已集成 `aligned_write_*` 计数器(见 `DownloadTask::write_all_at`);
/// `bytes_downloaded` / `fragments_completed` / `errors` 亦由任务热路径更新。
/// rebalance 在 `try_rebalance_slowest_fragment` 成功/回滚路径递增。
/// 本轮不暴露 UI/IPC;测试与进程内观测 / 吞吐基线 harness 直接读原子字段。
#[derive(Debug)]
pub struct Metrics {
    /// 累计已下载字节数(独占 Cache Line)
    pub bytes_downloaded: CachePadded<AtomicU64>,
    /// 已完成的分片数(独占 Cache Line)
    pub fragments_completed: CachePadded<AtomicU64>,
    /// 错误计数(独占 Cache Line)
    pub errors: CachePadded<AtomicU64>,
    /// 写路径指针对齐命中(零拷贝直写)次数
    pub aligned_write_passthrough: CachePadded<AtomicU64>,
    /// 写路径指针对齐未命中(拷入 AlignedBuf)次数
    pub aligned_write_copied: CachePadded<AtomicU64>,
    /// 慢片 rebalance 成功次数
    pub rebalance_count: CachePadded<AtomicU64>,
    /// rebalance 入队失败回滚次数
    pub rebalance_dropped: CachePadded<AtomicU64>,
}

impl Metrics {
    /// 创建全零初始化的指标实例
    pub fn new() -> Self {
        Self {
            bytes_downloaded: CachePadded::new(AtomicU64::new(0)),
            fragments_completed: CachePadded::new(AtomicU64::new(0)),
            errors: CachePadded::new(AtomicU64::new(0)),
            aligned_write_passthrough: CachePadded::new(AtomicU64::new(0)),
            aligned_write_copied: CachePadded::new(AtomicU64::new(0)),
            rebalance_count: CachePadded::new(AtomicU64::new(0)),
            rebalance_dropped: CachePadded::new(AtomicU64::new(0)),
        }
    }

    /// 原子累加下载字节数
    pub fn add_bytes(&self, n: u64) {
        // Relaxed:独立计数器,无通过本原子发布的其它内存依赖;snapshot 允许短暂滞后。
        self.bytes_downloaded.fetch_add(n, Ordering::Relaxed);
    }

    /// 原子递增完成分片数
    pub fn inc_fragment(&self) {
        self.fragments_completed.fetch_add(1, Ordering::Relaxed);
    }

    /// 原子递增错误计数
    pub fn inc_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次写路径指针对齐命中(零拷贝)
    pub fn inc_aligned_write_passthrough(&self) {
        self.aligned_write_passthrough
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次写路径指针对齐未命中(拷贝对齐)
    pub fn inc_aligned_write_copied(&self) {
        self.aligned_write_copied.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次慢片 rebalance 成功拆分
    pub fn inc_rebalance(&self) {
        self.rebalance_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次 rebalance 入队失败并回滚
    pub fn inc_rebalance_dropped(&self) {
        self.rebalance_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// 读取当前指标快照(Acquire 语义,保证看到最新的写入)
    ///
    /// 返回 `(bytes, fragments, errors, aligned_passthrough, aligned_copied, rebalance, rebalance_dropped)`。
    pub fn snapshot(&self) -> (u64, u64, u64, u64, u64, u64, u64) {
        (
            self.bytes_downloaded.load(Ordering::Acquire),
            self.fragments_completed.load(Ordering::Acquire),
            self.errors.load(Ordering::Acquire),
            self.aligned_write_passthrough.load(Ordering::Acquire),
            self.aligned_write_copied.load(Ordering::Acquire),
            self.rebalance_count.load(Ordering::Acquire),
            self.rebalance_dropped.load(Ordering::Acquire),
        )
    }

    /// 对齐写命中率(0.0–1.0);无样本时返回 0.0
    pub fn aligned_write_hit_rate(&self) -> f64 {
        let pass = self.aligned_write_passthrough.load(Ordering::Acquire);
        let copy = self.aligned_write_copied.load(Ordering::Acquire);
        let total = pass.saturating_add(copy);
        if total == 0 {
            0.0
        } else {
            pass as f64 / total as f64
        }
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
        assert_eq!(m.rebalance_count.load(Ordering::Relaxed), 0);
        assert_eq!(m.rebalance_dropped.load(Ordering::Relaxed), 0);

        m.add_bytes(1024);
        m.add_bytes(2048);
        assert_eq!(m.bytes_downloaded.load(Ordering::Relaxed), 3072);

        m.inc_fragment();
        m.inc_fragment();
        m.inc_fragment();
        assert_eq!(m.fragments_completed.load(Ordering::Relaxed), 3);

        m.inc_error();
        assert_eq!(m.errors.load(Ordering::Relaxed), 1);

        m.inc_rebalance();
        assert_eq!(m.rebalance_count.load(Ordering::Relaxed), 1);
        m.inc_rebalance_dropped();
        assert_eq!(m.rebalance_dropped.load(Ordering::Relaxed), 1);

        let m2 = Metrics::default();
        assert_eq!(m2.bytes_downloaded.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_metrics_snapshot() {
        let m = Metrics::new();
        assert_eq!(m.snapshot(), (0, 0, 0, 0, 0, 0, 0));

        m.add_bytes(100);
        m.inc_fragment();
        m.inc_error();
        m.inc_aligned_write_passthrough();
        m.inc_aligned_write_copied();
        m.inc_aligned_write_copied();
        m.inc_rebalance();
        m.inc_rebalance_dropped();
        assert_eq!(m.snapshot(), (100, 1, 1, 1, 2, 1, 1));
        assert!((m.aligned_write_hit_rate() - 1.0 / 3.0).abs() < 1e-9);
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
                        m.inc_aligned_write_passthrough();
                        m.inc_aligned_write_copied();
                        m.inc_rebalance();
                        m.inc_rebalance_dropped();
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        assert_eq!(
            m.snapshot(),
            (40_000, 4_000, 4_000, 4_000, 4_000, 4_000, 4_000)
        );
    }
}
