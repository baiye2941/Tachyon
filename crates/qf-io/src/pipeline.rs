//! 零拷贝写入管道
//!
//! 将网络接收的数据通过 BufferPool 高效写入文件。
//! 设计目标:减少内存拷贝次数和堆分配。

use bytes::Bytes;

use crate::buffer::BufferPool;
use crate::storage::AsyncStorage;
use qf_core::QfResult;

/// 写入管道,将数据从网络 buffer 写入存储
pub struct WritePipeline<S: AsyncStorage> {
    storage: S,
    buffer_pool: BufferPool,
}

impl<S: AsyncStorage> WritePipeline<S> {
    /// 创建新的写入管道
    pub fn new(storage: S, buffer_pool: BufferPool) -> Self {
        Self {
            storage,
            buffer_pool,
        }
    }

    /// 将数据写入指定偏移位置
    pub async fn write(&self, offset: u64, data: &[u8]) -> QfResult<usize> {
        let written = self.storage.write_at(offset, data).await?;
        Ok(written)
    }

    /// 从 Bytes 写入(避免额外拷贝,直接传递底层引用)
    pub async fn write_bytes(&self, offset: u64, data: &Bytes) -> QfResult<usize> {
        self.storage.write_at(offset, data.as_ref()).await
    }

    /// 将数据写入并同步到磁盘
    pub async fn write_and_sync(&self, offset: u64, data: &[u8]) -> QfResult<usize> {
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
}
