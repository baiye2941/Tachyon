//! Buffer 池管理
//!
//! 预分配 buffer 池,减少运行时堆分配。
//! 支持 buffer 归还复用,降低内存碎片。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bytes::BytesMut;

/// 预分配的 buffer 池,支持 buffer 回收复用
pub struct BufferPool {
    buffer_size: usize,
    capacity: usize,
    pool: Arc<Mutex<VecDeque<BytesMut>>>,
}

impl BufferPool {
    /// 创建新的 buffer 池
    pub fn new(buffer_size: usize, capacity: usize) -> Self {
        let pool = Arc::new(Mutex::new(VecDeque::with_capacity(capacity)));
        Self {
            buffer_size,
            capacity,
            pool,
        }
    }

    /// 创建并预填充 buffer 池
    pub fn with_prefill(buffer_size: usize, capacity: usize) -> Self {
        let mut queue = VecDeque::with_capacity(capacity);
        for _ in 0..capacity {
            queue.push_back(BytesMut::with_capacity(buffer_size));
        }
        Self {
            buffer_size,
            capacity,
            pool: Arc::new(Mutex::new(queue)),
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

    /// 从池中获取一个 buffer,池空时新建
    pub fn alloc(&self) -> BytesMut {
        let mut pool = self.pool.lock().unwrap();
        pool.pop_front()
            .unwrap_or_else(|| BytesMut::with_capacity(self.buffer_size))
    }

    /// 归还 buffer 到池中,超出容量时丢弃
    pub fn release(&self, mut buf: BytesMut) {
        buf.clear();
        let mut pool = self.pool.lock().unwrap();
        if pool.len() < self.capacity {
            pool.push_back(buf);
        }
    }

    /// 当前池中可用 buffer 数量
    pub fn available(&self) -> usize {
        self.pool.lock().unwrap().len()
    }
}

impl Clone for BufferPool {
    fn clone(&self) -> Self {
        Self {
            buffer_size: self.buffer_size,
            capacity: self.capacity,
            pool: Arc::clone(&self.pool),
        }
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
        assert_eq!(pool.available(), 0);
    }

    #[test]
    fn test_prefill_pool() {
        let pool = BufferPool::with_prefill(1024, 5);
        assert_eq!(pool.available(), 5);
    }

    #[test]
    fn test_alloc_from_prefill() {
        let pool = BufferPool::with_prefill(1024, 3);
        let _buf = pool.alloc();
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn test_alloc_from_empty_creates_new() {
        let pool = BufferPool::new(4096, 10);
        let buf = pool.alloc();
        assert_eq!(buf.capacity(), 4096);
        assert_eq!(pool.available(), 0);
    }

    #[test]
    fn test_release_returns_to_pool() {
        let pool = BufferPool::new(1024, 5);
        let buf = pool.alloc();
        assert_eq!(pool.available(), 0);
        pool.release(buf);
        assert_eq!(pool.available(), 1);
    }

    #[test]
    fn test_release_clears_buffer() {
        let pool = BufferPool::new(1024, 5);
        let mut buf = pool.alloc();
        buf.extend_from_slice(b"some data");
        assert!(!buf.is_empty());
        pool.release(buf);
        let buf = pool.alloc();
        assert!(buf.is_empty());
    }

    #[test]
    fn test_release_discards_when_full() {
        let pool = BufferPool::new(1024, 2);
        let buf1 = pool.alloc();
        let buf2 = pool.alloc();
        let buf3 = pool.alloc();
        pool.release(buf1);
        pool.release(buf2);
        assert_eq!(pool.available(), 2);
        pool.release(buf3);
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn test_clone_shares_pool() {
        let pool = BufferPool::with_prefill(1024, 3);
        let pool2 = pool.clone();
        let _buf = pool.alloc();
        assert_eq!(pool2.available(), 2);
    }
}
