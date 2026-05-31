//! Buffer 池管理
//!
//! 基于 crossbeam `ArrayQueue` 的 lock-free 预分配 buffer 池。
//! 通过 `tokio::sync::Semaphore` 实现反压:
//! - `alloc()` 在池许可耗尽时阻塞等待,天然将磁盘慢压力传导到网络层
//! - `release()` 归还 buffer 并释放信号量许可,唤醒等待的 alloc
//!
//! 反压链路:磁盘写入慢 -> buffer 归还慢 -> 池许可耗尽 -> 网络层阻塞 -> 自动限速

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::BytesMut;
use crossbeam_queue::ArrayQueue;
use tokio::sync::Semaphore;

/// 预分配的 buffer 池,支持异步反压与 buffer 回收复用
///
/// 内部使用 lock-free `ArrayQueue` 存储 buffer,`tokio::sync::Semaphore`
/// 控制最大并发 buffer 数。信号量许可数等于池容量,`alloc()` 在许可耗尽
/// 时阻塞,`release()` 归还许可。
///
/// 不变量: `semaphore.available_permits() + outstanding == capacity`
pub struct BufferPool {
    buffer_size: usize,
    capacity: usize,
    pool: Arc<ArrayQueue<BytesMut>>,
    /// 信号量许可数等于 capacity,用于控制最大并发 buffer 数
    semaphore: Arc<Semaphore>,
    /// 当前已分配出去(未归还)的 buffer 数量
    outstanding: Arc<AtomicUsize>,
}

impl BufferPool {
    /// 创建新的 buffer 池
    ///
    /// 初始池为空,`alloc()` 首次调用时会创建新 buffer。
    /// 信号量许可数等于 `capacity`,限制最大并发 buffer 数量。
    pub fn new(buffer_size: usize, capacity: usize) -> Self {
        Self {
            buffer_size,
            capacity,
            pool: Arc::new(ArrayQueue::new(capacity.max(1))),
            semaphore: Arc::new(Semaphore::new(capacity)),
            outstanding: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 创建并预填充 buffer 池
    ///
    /// 预填充消耗信号量许可,后续 `alloc()` 从池中取出 buffer 时
    /// 不需要额外分配堆内存。
    pub fn with_prefill(buffer_size: usize, capacity: usize) -> Self {
        let pool = Arc::new(ArrayQueue::new(capacity.max(1)));
        let semaphore = Semaphore::new(capacity);
        // 预填充消耗信号量许可
        for _ in 0..capacity {
            // 队列刚创建,容量充足,不会失败
            let _ = pool.push(BytesMut::with_capacity(buffer_size));
            // acquire 成功后 forget permit,净效果:许可 -1
            let permit = semaphore.try_acquire().expect("信号量初始许可应始终足够");
            permit.forget();
        }
        Self {
            buffer_size,
            capacity,
            pool,
            semaphore: Arc::new(semaphore),
            outstanding: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 获取 buffer 大小
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }

    /// 获取池容量
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 从池中获取一个 buffer
    ///
    /// 当所有许可都被占用(已有 capacity 个 buffer 在外使用)时,
    /// 此方法会阻塞等待,直到有 buffer 被归还。这是反压的核心机制:
    /// 磁盘慢 -> buffer 归还慢 -> alloc 阻塞 -> 网络写入自动限速。
    pub async fn alloc(&self) -> BytesMut {
        // 获取信号量许可,许可耗尽时阻塞
        let permit = self
            .semaphore
            .acquire()
            .await
            .expect("BufferPool 信号量不应被关闭");
        // 立即释放许可对象,仅保留计数效果
        permit.forget();

        self.outstanding.fetch_add(1, Ordering::AcqRel);

        // 从队列中取出复用 buffer,或新建一个
        self.pool
            .pop()
            .unwrap_or_else(|| BytesMut::with_capacity(self.buffer_size))
    }

    /// 归还 buffer 到池中
    ///
    /// 归还后释放一个信号量许可,唤醒可能在 `alloc()` 中等待的任务。
    /// 如果队列已满,buffer 会被丢弃(释放内存),但许可仍然归还。
    ///
    /// 维护不变量: `permits + outstanding == capacity`
    pub fn release(&self, mut buf: BytesMut) {
        self.outstanding.fetch_sub(1, Ordering::AcqRel);
        buf.clear();
        let pushed = self.pool.push(buf).is_ok();
        if !pushed {
            // 队列已满,buffer 被丢弃。不额外加许可,因为 outstanding 已减。
            // 不变量恢复: permits = capacity - (old_outstanding - 1) = capacity - new_outstanding
            return;
        }
        // buffer 放回队列,加一个许可恢复不变量
        self.semaphore.add_permits(1);
    }

    /// 当前可用的信号量许可数(可无阻塞分配的次数)
    ///
    /// 用于监控反压状态:0 表示已触发背压,`alloc()` 会阻塞。
    pub fn available(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// 预热 buffer 池,预分配所有 buffer 避免运行时分配
    ///
    /// 在下载任务开始前调用,确保后续 `alloc()` 不会触发堆分配。
    /// 如果池中已有 buffer,只会补充不足的部分。
    ///
    /// 仅补充 `capacity - pool.len() - outstanding` 个 buffer,
    /// 确保不会超出总容量预算。
    pub async fn prewarm(&self) {
        let outstanding = self.outstanding.load(Ordering::Acquire);
        // 可填充数量 = capacity - 已分配 - 队列中已有
        let to_fill = self
            .capacity
            .saturating_sub(outstanding)
            .saturating_sub(self.pool.len());

        for _ in 0..to_fill {
            if self.pool.is_full() {
                break;
            }
            // 获取许可(应立即成功,因为 permits = capacity - outstanding >= to_fill)
            let permit = self
                .semaphore
                .acquire()
                .await
                .expect("BufferPool 信号量不应被关闭");
            permit.forget();
            let _ = self.pool.push(BytesMut::with_capacity(self.buffer_size));
        }
    }

    /// 获取池统计信息
    pub fn stats(&self) -> BufferPoolStats {
        BufferPoolStats {
            available: self.available(),
            capacity: self.capacity,
            buffer_size: self.buffer_size,
        }
    }

    /// 从池中获取一个 RAII 保护的 buffer
    ///
    /// 返回 `BufferGuard`,在析构时自动归还 buffer 到池中。
    /// 避免手动调用 `release()` 遗忘导致的资源泄漏。
    pub async fn alloc_guarded(&self) -> BufferGuard {
        let buf = self.alloc().await;
        BufferGuard {
            buf: Some(buf),
            pool: self.clone(),
        }
    }
}

/// Buffer 池统计信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferPoolStats {
    /// 当前可用信号量许可数(可无阻塞分配次数)
    pub available: usize,
    /// 池最大容量
    pub capacity: usize,
    /// 每个 buffer 的字节大小
    pub buffer_size: usize,
}

impl Clone for BufferPool {
    fn clone(&self) -> Self {
        Self {
            buffer_size: self.buffer_size,
            capacity: self.capacity,
            pool: Arc::clone(&self.pool),
            semaphore: Arc::clone(&self.semaphore),
            outstanding: Arc::clone(&self.outstanding),
        }
    }
}

/// RAII 包装器,在析构时自动归还 buffer 到池中
///
/// 使用 `BufferPool::alloc_guarded()` 获取,确保即使发生提前返回或
/// panic 也能正确归还 buffer,避免资源泄漏。
pub struct BufferGuard {
    buf: Option<BytesMut>,
    pool: BufferPool,
}

impl BufferGuard {
    pub fn buf(&self) -> &BytesMut {
        self.buf.as_ref().expect("BufferGuard 不应在被 drop 后访问")
    }

    pub fn buf_mut(&mut self) -> &mut BytesMut {
        self.buf.as_mut().expect("BufferGuard 不应在被 drop 后访问")
    }
}

impl Drop for BufferGuard {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            self.pool.release(buf);
        }
    }
}

impl std::ops::Deref for BufferGuard {
    type Target = BytesMut;
    fn deref(&self) -> &Self::Target {
        self.buf()
    }
}

impl std::ops::DerefMut for BufferGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buf_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_pool_empty() {
        let pool = BufferPool::new(4096, 10);
        assert_eq!(pool.buffer_size(), 4096);
        assert_eq!(pool.capacity(), 10);
        // 新建池许可全可用
        assert_eq!(pool.available(), 10);
    }

    #[test]
    fn test_prefill_pool() {
        let pool = BufferPool::with_prefill(1024, 5);
        // 预填充消耗许可,available 为 0(许可已分配给预填充 buffer)
        assert_eq!(pool.available(), 0);
    }

    #[tokio::test]
    async fn test_alloc_from_prefill() {
        let pool = BufferPool::with_prefill(1024, 3);
        // 预填充后许可为 0,需要先释放一个 buffer 才能 alloc
        // 模拟:从队列取出 -> release -> alloc
        let buf = pool.pool.pop().expect("预填充后队列应有 buffer");
        pool.release(buf);
        assert_eq!(pool.available(), 1);
        let _buf = pool.alloc().await;
        assert_eq!(pool.available(), 0);
    }

    #[tokio::test]
    async fn test_alloc_from_empty_creates_new() {
        let pool = BufferPool::new(4096, 10);
        let buf = pool.alloc().await;
        assert_eq!(buf.capacity(), 4096);
        // 消耗了一个许可
        assert_eq!(pool.available(), 9);
    }

    #[tokio::test]
    async fn test_alloc_blocks_when_permits_exhausted() {
        let pool = BufferPool::new(4096, 2);
        let _buf1 = pool.alloc().await;
        let _buf2 = pool.alloc().await;
        assert_eq!(pool.available(), 0);

        // 第三次 alloc 应阻塞;用 timeout 验证
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), pool.alloc()).await;
        assert!(result.is_err(), "无可用许可时 alloc 应阻塞");
    }

    #[tokio::test]
    async fn test_alloc_unblocks_after_release() {
        let pool = BufferPool::new(4096, 1);
        let buf = pool.alloc().await;
        assert_eq!(pool.available(), 0);

        // 在另一个任务中等待 alloc
        let pool_clone = pool.clone();
        let alloc_task = tokio::spawn(async move { pool_clone.alloc().await });

        // 短暂延迟后释放,唤醒等待的 alloc
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        pool.release(buf);

        // alloc 应成功完成
        let buf2 = alloc_task.await.expect("任务应成功");
        assert_eq!(buf2.capacity(), 4096);
    }

    #[tokio::test]
    async fn test_release_returns_to_pool() {
        let pool = BufferPool::new(1024, 5);
        let buf = pool.alloc().await;
        assert_eq!(pool.available(), 4);
        pool.release(buf);
        assert_eq!(pool.available(), 5);
    }

    #[tokio::test]
    async fn test_release_clears_buffer() {
        let pool = BufferPool::new(1024, 5);
        let mut buf = pool.alloc().await;
        buf.extend_from_slice(b"some data");
        assert!(!buf.is_empty());
        pool.release(buf);
        let buf = pool.alloc().await;
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn test_release_permits_stable() {
        let pool = BufferPool::new(1024, 2);
        let buf1 = pool.alloc().await;
        let buf2 = pool.alloc().await;
        // 释放两个 buffer
        pool.release(buf1);
        pool.release(buf2);
        assert_eq!(pool.available(), 2);

        // 再次分配和释放,许可数应稳定
        let buf1 = pool.alloc().await;
        pool.release(buf1);
        assert_eq!(pool.available(), 2);
    }

    #[tokio::test]
    async fn test_clone_shares_pool() {
        let pool = BufferPool::new(1024, 3);
        let pool2 = pool.clone();
        let _buf = pool.alloc().await;
        // 共享信号量,许可一致
        assert_eq!(pool2.available(), 2);
    }

    #[tokio::test]
    async fn test_prewarm_fills_empty_pool() {
        let pool = BufferPool::new(4096, 8);
        assert_eq!(pool.available(), 8);
        pool.prewarm().await;
        // prewarm 消耗许可(预分配 buffer 视为"已占用")
        assert_eq!(pool.available(), 0);
    }

    #[tokio::test]
    async fn test_prewarm_after_alloc() {
        let pool = BufferPool::new(1024, 5);
        let _b1 = pool.alloc().await;
        let _b2 = pool.alloc().await;
        assert_eq!(pool.available(), 3);
        // prewarm 补充池中 buffer(to_fill = 5 - 2 outstanding - 0 in queue = 3)
        pool.prewarm().await;
        // outstanding=2,池中有 3 个预分配 buffer -> 许可全被占用
        assert_eq!(pool.available(), 0);
    }

    #[tokio::test]
    async fn test_prewarm_idempotent() {
        let pool = BufferPool::new(2048, 4);
        pool.prewarm().await;
        assert_eq!(pool.available(), 0);
        // 再次 prewarm:队列已满,outstanding=0,to_fill=0,无额外操作
        pool.prewarm().await;
        assert_eq!(pool.available(), 0);
    }

    #[tokio::test]
    async fn test_prewarm_buffers_have_correct_capacity() {
        let pool = BufferPool::new(4096, 3);
        pool.prewarm().await;
        // prewarm 后许可为 0,先释放一个才能 alloc
        let buf = pool.pool.pop().expect("prewarm 后队列应有 buffer");
        pool.release(buf);
        let buf = pool.alloc().await;
        assert!(buf.capacity() >= 4096);
    }

    #[test]
    fn test_stats_after_prefill() {
        let pool = BufferPool::with_prefill(1024, 5);
        let stats = pool.stats();
        assert_eq!(
            stats,
            BufferPoolStats {
                available: 0,
                capacity: 5,
                buffer_size: 1024,
            }
        );
    }

    #[tokio::test]
    async fn test_stats_after_alloc_and_release() {
        let pool = BufferPool::new(512, 3);
        pool.prewarm().await;
        // prewarm 后许可为 0
        assert_eq!(pool.available(), 0);
        // 释放一个才能 alloc
        let buf = pool.pool.pop().expect("prewarm 后队列应有 buffer");
        pool.release(buf);
        assert_eq!(pool.available(), 1);
        let buf = pool.alloc().await;
        let stats = pool.stats();
        assert_eq!(stats.available, 0);
        pool.release(buf);
        let stats = pool.stats();
        assert_eq!(stats.available, 1);
    }

    // ------ 并发测试 ------

    /// 并发 alloc/release 安全性:多个任务同时操作不应 panic 或数据损坏
    #[tokio::test]
    async fn test_concurrent_alloc_release_safety() {
        // 使用 new() 而非 with_prefill():new() 初始许可=capacity,可直接 alloc
        let pool = std::sync::Arc::new(BufferPool::new(1024, 16));
        let mut handles = Vec::new();

        // 32 个并发任务,每个 alloc 并 release
        for _ in 0..32 {
            let p = std::sync::Arc::clone(&pool);
            handles.push(tokio::spawn(async move {
                for _ in 0..5 {
                    let buf = p.alloc().await;
                    let _cap = buf.capacity();
                    p.release(buf);
                }
            }));
        }

        for handle in handles {
            handle.await.expect("并发 alloc/release 不应 panic");
        }

        // 所有 buffer 归还后,许可应恢复到 capacity
        let available = pool.available();
        assert_eq!(
            available,
            pool.capacity(),
            "所有 buffer 归还后许可应恢复到 capacity"
        );
    }

    /// 并发高竞争:所有许可被耗尽后归还
    #[tokio::test]
    async fn test_concurrent_exhaustion_and_return() {
        let pool = std::sync::Arc::new(BufferPool::new(512, 4));
        let mut handles = Vec::new();

        // 8 个并发任务竞争 4 个许可
        for _ in 0..8 {
            let p = std::sync::Arc::clone(&pool);
            handles.push(tokio::spawn(async move {
                let buf = p.alloc().await;
                // 持有 buffer 一小段时间(允许其他任务也尝试 alloc)
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                p.release(buf);
            }));
        }

        for handle in handles {
            handle.await.expect("高竞争 alloc/release 不应 panic");
        }

        // 所有许可应被归还
        assert_eq!(pool.available(), 4, "所有许可应已归还");
    }

    // ------ BufferGuard 测试 ------

    #[tokio::test]
    async fn test_buffer_guard_auto_release() {
        let pool = BufferPool::new(4096, 2);
        {
            let _guard = pool.alloc_guarded().await;
            assert_eq!(pool.available(), 1);
        }
        assert_eq!(pool.available(), 2, "BufferGuard drop 应自动归还 buffer");
    }

    #[tokio::test]
    async fn test_buffer_guard_deref() {
        let pool = BufferPool::new(4096, 2);
        let mut guard = pool.alloc_guarded().await;
        guard.extend_from_slice(b"hello");
        assert_eq!(&guard[..5], b"hello");
    }
}
