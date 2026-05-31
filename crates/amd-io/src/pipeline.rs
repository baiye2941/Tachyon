//! 零拷贝写入管道
//!
//! 将网络接收的数据通过 BufferPool 高效写入文件。
//! 设计目标:减少内存拷贝次数和堆分配。
//!
//! 核心优化:
//! - `write()` 通过 BufferPool 信号量实现反压
//! - `write_batch()` 自动合并相邻连续 segment,减少 syscall 次数

use bytes::Bytes;

use crate::buffer::BufferPool;
use crate::storage::AsyncStorage;
use amd_core::AmdResult;

/// 写入管道,将数据从网络 buffer 写入存储
///
/// 集成 BufferPool 实现反压:当磁盘写入变慢时,buffer 归还延迟,
/// 信号量许可耗尽,网络层写入自动阻塞,防止内存无限增长。
pub struct WritePipeline<S: AsyncStorage> {
    storage: S,
    buffer_pool: BufferPool,
    max_pending: usize,
}

impl<S: AsyncStorage> WritePipeline<S> {
    /// 创建新的写入管道
    pub fn new(storage: S, buffer_pool: BufferPool) -> Self {
        Self {
            storage,
            buffer_pool,
            max_pending: 64,
        }
    }

    pub fn with_max_pending(mut self, max_pending: usize) -> Self {
        self.max_pending = max_pending.max(1);
        self
    }

    /// 将数据写入指定偏移位置
    ///
    /// 通过 BufferPool 信号量获取许可后再写入,实现反压控制。
    /// 当池许可耗尽时(磁盘慢,buffer 未及时归还),此方法会阻塞。
    pub async fn write(&self, offset: u64, data: &[u8]) -> AmdResult<usize> {
        let _buf = self.buffer_pool.alloc().await;
        let written = self
            .storage
            .write_at(offset, Bytes::copy_from_slice(data))
            .await?;
        self.buffer_pool.release(_buf);
        Ok(written)
    }

    pub async fn write_bytes(&self, offset: u64, data: &Bytes) -> AmdResult<usize> {
        let _buf = self.buffer_pool.alloc().await;
        let written = self.storage.write_at(offset, data.clone()).await?;
        self.buffer_pool.release(_buf);
        Ok(written)
    }

    /// 将数据写入并同步到磁盘
    pub async fn write_and_sync(&self, offset: u64, data: &[u8]) -> AmdResult<usize> {
        let written = self.write(offset, data).await?;
        self.storage.sync().await?;
        Ok(written)
    }

    /// 获取 buffer 池引用
    pub fn buffer_pool(&self) -> &BufferPool {
        &self.buffer_pool
    }

    /// 获取存储引用
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// 批量写入多个分片数据,自动合并相邻连续 segment 以减少 syscall
    ///
    /// 优化策略:
    /// 1. 按偏移排序
    /// 2. 相邻连续 segment(前一个 end_offset == 后一个 offset)合并为一次 `write_at`
    /// 3. 最后统一 sync 一次
    ///
    /// 例如 `[(0, a), (4, b), (8, c)]` 合并为 `write_at(0, [a+b+c])`,
    /// 将 N 次 write_at 减少到 1 次。
    pub async fn write_batch(&self, segments: &[(u64, &[u8])]) -> AmdResult<usize> {
        if segments.is_empty() {
            self.storage.sync().await?;
            return Ok(0);
        }

        let _buf = self.buffer_pool.alloc().await;
        let flush_threshold = self.max_pending * self.buffer_pool.buffer_size();

        let total = if segments.len() == 1 {
            let (offset, data) = segments[0];
            self.storage
                .write_at(offset, Bytes::copy_from_slice(data))
                .await?
        } else {
            let mut indices: Vec<usize> = (0..segments.len()).collect();
            indices.sort_unstable_by_key(|&i| segments[i].0);

            let mut total_written: usize = 0;
            let mut merged_start = segments[indices[0]].0;
            let mut merged_end = merged_start + segments[indices[0]].1.len() as u64;
            let mut merged_data: Vec<u8> = Vec::with_capacity(segments[indices[0]].1.len());
            merged_data.extend_from_slice(segments[indices[0]].1);

            for &idx in &indices[1..] {
                let (off, data) = segments[idx];
                let len = data.len() as u64;

                if off == merged_end && len > 0 {
                    merged_end += len;
                    merged_data.extend_from_slice(data);
                    if merged_data.len() >= flush_threshold {
                        total_written += self
                            .storage
                            .write_at(merged_start, Bytes::from(merged_data.clone()))
                            .await?;
                        merged_data.clear();
                        merged_start = off + len;
                        merged_end = merged_start;
                    }
                } else {
                    total_written += self
                        .storage
                        .write_at(merged_start, Bytes::from(merged_data.clone()))
                        .await?;
                    merged_start = off;
                    merged_end = off + len;
                    merged_data.clear();
                    merged_data.extend_from_slice(data);
                }
            }
            if !merged_data.is_empty() {
                total_written += self
                    .storage
                    .write_at(merged_start, Bytes::from(merged_data))
                    .await?;
            }
            total_written
        };

        self.buffer_pool.release(_buf);
        self.storage.sync().await?;
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::BufferPool;
    use tempfile::NamedTempFile;

    use crate::tokio_file::TokioFile;

    #[tokio::test]
    async fn test_pipeline_write() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        let written = pipeline.write(0, b"pipeline test").await.unwrap();
        assert_eq!(written, 13);
    }

    #[tokio::test]
    async fn test_pipeline_write_bytes() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        let data = Bytes::from_static(b"bytes test");
        let written = pipeline.write_bytes(0, &data).await.unwrap();
        assert_eq!(written, 10);
    }

    #[tokio::test]
    async fn test_pipeline_write_and_sync() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        let written = pipeline.write_and_sync(0, b"sync test").await.unwrap();
        assert_eq!(written, 9);
    }

    #[tokio::test]
    async fn test_pipeline_multi_write() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        pipeline.write(0, b"AAAA").await.unwrap();
        pipeline.write(4, b"BBBB").await.unwrap();
        pipeline.write(8, b"CCCC").await.unwrap();

        let mut buf = [0u8; 12];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 12);
        assert_eq!(&buf, b"AAAABBBBCCCC");
    }

    #[tokio::test]
    async fn test_pipeline_write_batch() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        // 连续 segment 应被合并为一次 write_at
        let segments: Vec<(u64, &[u8])> = vec![(0, b"AAAA"), (4, b"BBBB"), (8, b"CCCC")];
        let total = pipeline.write_batch(&segments).await.unwrap();
        assert_eq!(total, 12);

        let mut buf = [0u8; 12];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 12);
        assert_eq!(&buf, b"AAAABBBBCCCC");
    }

    #[tokio::test]
    async fn test_pipeline_write_batch_empty() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        let segments: Vec<(u64, &[u8])> = vec![];
        let total = pipeline.write_batch(&segments).await.unwrap();
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn test_pipeline_write_batch_single() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        let segments: Vec<(u64, &[u8])> = vec![(0, b"single")];
        let total = pipeline.write_batch(&segments).await.unwrap();
        assert_eq!(total, 6);

        let mut buf = [0u8; 6];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 6);
        assert_eq!(&buf, b"single");
    }

    #[tokio::test]
    async fn test_pipeline_write_batch_non_contiguous() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        // 不连续的 segment 不应合并:gap at offset 100..200
        let segments: Vec<(u64, &[u8])> = vec![(0, b"AAAA"), (200, b"BBBB")];
        let total = pipeline.write_batch(&segments).await.unwrap();
        assert_eq!(total, 8);

        let mut buf = [0u8; 4];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 4);
        assert_eq!(&buf, b"AAAA");

        let read = pipeline.storage().read_at(200, &mut buf).await.unwrap();
        assert_eq!(read, 4);
        assert_eq!(&buf, b"BBBB");
    }

    #[tokio::test]
    async fn test_pipeline_write_batch_mixed_contiguous() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        // 混合:前三个连续,后两个有间隔,最后两个连续
        let segments: Vec<(u64, &[u8])> = vec![
            (0, b"AAA"),
            (3, b"BBB"),
            (6, b"CCC"),
            (100, b"DDD"),
            (200, b"EEE"),
            (203, b"FFF"),
        ];
        let total = pipeline.write_batch(&segments).await.unwrap();
        assert_eq!(total, 18);

        // 验证前三个被合并写入
        let mut buf = [0u8; 9];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 9);
        assert_eq!(&buf, b"AAABBBCCC");

        // 验证中间单独写入
        let mut buf = [0u8; 3];
        let read = pipeline.storage().read_at(100, &mut buf).await.unwrap();
        assert_eq!(read, 3);
        assert_eq!(&buf, b"DDD");

        // 验证最后两个被合并写入
        let mut buf = [0u8; 6];
        let read = pipeline.storage().read_at(200, &mut buf).await.unwrap();
        assert_eq!(read, 6);
        assert_eq!(&buf, b"EEEFFF");
    }

    #[tokio::test]
    async fn test_pipeline_write_batch_unordered() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        // 乱序输入,应按偏移排序后合并
        let segments: Vec<(u64, &[u8])> = vec![(8, b"CCCC"), (0, b"AAAA"), (4, b"BBBB")];
        let total = pipeline.write_batch(&segments).await.unwrap();
        assert_eq!(total, 12);

        let mut buf = [0u8; 12];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 12);
        assert_eq!(&buf, b"AAAABBBBCCCC");
    }

    #[tokio::test]
    async fn test_pipeline_backpressure() {
        // 验证反压:容量为 1 的池,并发写入时第二个会等待
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 1);
        let pipeline = WritePipeline::new(storage, pool);

        // 第一个写入消耗许可
        let written = pipeline.write(0, b"first").await.unwrap();
        assert_eq!(written, 5);

        // 写入后许可已归还,可继续写入
        let written = pipeline.write(5, b"second").await.unwrap();
        assert_eq!(written, 6);

        // 验证数据正确
        let mut buf = [0u8; 11];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 11);
        assert_eq!(&buf, b"firstsecond");
    }

    /// 相邻 segment 合并写入:多个连续偏移的 segment 写入后数据正确拼接
    #[tokio::test]
    async fn test_pipeline_adjacent_segments_merge() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(30).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        // 模拟 3 个相邻分片写入
        let seg1 = b"AAAAAAAAAA"; // offset 0, len 10
        let seg2 = b"BBBBBBBBBB"; // offset 10, len 10
        let seg3 = b"CCCCCCCCCC"; // offset 20, len 10

        pipeline.write(0, seg1).await.unwrap();
        pipeline.write(10, seg2).await.unwrap();
        pipeline.write(20, seg3).await.unwrap();

        // 验证完整数据
        let mut buf = [0u8; 30];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 30);
        assert_eq!(&buf[..10], seg1);
        assert_eq!(&buf[10..20], seg2);
        assert_eq!(&buf[20..30], seg3);
    }

    /// 批量写入相邻 segment:write_batch 一次写入多个连续 segment
    #[tokio::test]
    async fn test_pipeline_batch_adjacent_segments() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(24).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        let segments: Vec<(u64, &[u8])> = vec![
            (0, b"SEG1"),  // 4 bytes
            (4, b"SEG2"),  // 4 bytes
            (8, b"SEG3"),  // 4 bytes
            (12, b"SEG4"), // 4 bytes
            (16, b"SEG5"), // 4 bytes
            (20, b"SEG6"), // 4 bytes
        ];

        let total = pipeline.write_batch(&segments).await.unwrap();
        assert_eq!(total, 24);

        let mut buf = [0u8; 24];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 24);
        assert_eq!(&buf, b"SEG1SEG2SEG3SEG4SEG5SEG6");
    }

    /// 重叠 segment 写入:后写入覆盖先写入
    #[tokio::test]
    async fn test_pipeline_overlapping_segments() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(10).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool);

        pipeline.write(0, b"AAAAAAAAAA").await.unwrap();
        // 覆盖中间 4 字节
        pipeline.write(3, b"BBBB").await.unwrap();

        let mut buf = [0u8; 10];
        pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(&buf, b"AAABBBBAAA");
    }

    #[tokio::test]
    async fn test_pipeline_max_pending_backpressure() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(8192).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool).with_max_pending(1);

        let mut segments: Vec<(u64, &[u8])> = vec![];
        let data_a = vec![0x41u8; 2048];
        let data_b = vec![0x42u8; 2048];
        let data_c = vec![0x43u8; 2048];
        segments.push((0, &data_a));
        segments.push((2048, &data_b));
        segments.push((4096, &data_c));

        let total = pipeline.write_batch(&segments).await.unwrap();
        assert_eq!(total, 6144);

        let mut buf = vec![0u8; 6144];
        let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 6144);
        assert_eq!(&buf[..2048], &data_a);
        assert_eq!(&buf[2048..4096], &data_b);
        assert_eq!(&buf[4096..6144], &data_c);
    }

    #[tokio::test]
    async fn test_pipeline_with_max_pending_clamps_to_one() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let pool = BufferPool::new(4096, 4);
        let pipeline = WritePipeline::new(storage, pool).with_max_pending(0);
        assert_eq!(pipeline.max_pending, 1);
    }
}
