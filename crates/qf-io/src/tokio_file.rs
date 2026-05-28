//! 基于 tokio 的异步文件 I/O 实现
//!
//! 使用 `tokio::sync::Mutex` 保护文件句柄,确保异步安全。
//! 后续可升级为 `pwrite`/`pread` positioned I/O 消除锁竞争。

use std::path::{Path, PathBuf};

use qf_core::{QfError, QfResult};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::storage::AsyncStorage;

/// 基于 tokio 的异步文件存储
///
/// 使用 `tokio::sync::Mutex` 保护文件句柄,支持并发安全的文件 I/O。
pub struct TokioFile {
    path: PathBuf,
    file: Mutex<tokio::fs::File>,
}

impl TokioFile {
    /// 打开或创建文件
    pub async fn open<P: AsRef<Path>>(path: P) -> QfResult<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .await
            .map_err(QfError::Io)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// 获取文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AsyncStorage for TokioFile {
    async fn write_at(&self, offset: u64, data: &[u8]) -> QfResult<usize> {
        let mut file = self.file.lock().await;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.write_all(data).await?;
        Ok(data.len())
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> QfResult<usize> {
        let mut file = self.file.lock().await;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let read = file.read(buf).await?;
        Ok(read)
    }

    async fn sync(&self) -> QfResult<()> {
        let file = self.file.lock().await;
        file.sync_data().await?;
        Ok(())
    }

    async fn allocate(&self, size: u64) -> QfResult<()> {
        let file = self.file.lock().await;
        file.set_len(size).await?;
        Ok(())
    }

    async fn file_size(&self) -> QfResult<u64> {
        let file = self.file.lock().await;
        let metadata = file.metadata().await?;
        Ok(metadata.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_open_and_write() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let written = storage.write_at(0, b"hello").await.unwrap();
        assert_eq!(written, 5);
    }

    #[tokio::test]
    async fn test_write_and_read() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.write_at(0, b"hello world").await.unwrap();
        let mut buf = [0u8; 11];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[tokio::test]
    async fn test_read_at_offset() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.write_at(0, b"hello world").await.unwrap();
        let mut buf = [0u8; 5];
        let read = storage.read_at(6, &mut buf).await.unwrap();
        assert_eq!(read, 5);
        assert_eq!(&buf, b"world");
    }

    #[tokio::test]
    async fn test_file_size() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 0);
        storage.write_at(0, b"hello").await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 5);
    }

    #[tokio::test]
    async fn test_allocate() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(1024).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 1024);
    }

    #[tokio::test]
    async fn test_sync() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.write_at(0, b"hello").await.unwrap();
        storage.sync().await.unwrap();
    }

    #[tokio::test]
    async fn test_concurrent_writes() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let storage = std::sync::Arc::new(storage);

        let mut handles = Vec::new();
        for i in 0u8..16 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let data = vec![i; 256];
                let offset = (i as u64) * 256;
                s.write_at(offset, &data).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // 验证每个区域写入正确
        for i in 0u8..16 {
            let offset = (i as u64) * 256;
            let mut buf = [0u8; 256];
            storage.read_at(offset, &mut buf).await.unwrap();
            assert!(
                buf.iter().all(|&b| b == i),
                "区域 {offset} 数据不一致，期望全部为 {i}"
            );
        }
    }

    /// 并发写入正确性:32 个任务同时写入不同偏移区域,全部数据一致
    #[tokio::test]
    async fn test_concurrent_write_at_correctness() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let total_size = 8192u64;
        storage.allocate(total_size).await.unwrap();
        let storage = std::sync::Arc::new(storage);

        let mut handles = Vec::new();

        // 32 个并发任务,每个写入 256 字节区域
        for i in 0u32..32 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let offset = (i as u64) * 256;
                // 写入可验证的数据模式:每个字节 = (offset + position) % 256
                let data: Vec<u8> = (0..256u32).map(|j| ((i * 256 + j) % 256) as u8).collect();
                s.write_at(offset, &data).await.unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // 验证每个区域的数据完整正确
        for i in 0u32..32 {
            let offset = (i as u64) * 256;
            let mut buf = [0u8; 256];
            let read = storage.read_at(offset, &mut buf).await.unwrap();
            assert_eq!(read, 256);
            for j in 0..256usize {
                let expected = ((i * 256 + j as u32) % 256) as u8;
                assert_eq!(
                    buf[j], expected,
                    "区域 {offset} 字节 {j} 不一致:期望 {expected},实际 {}",
                    buf[j]
                );
            }
        }
    }

    /// 并发读写:多个任务同时读写不同区域
    #[tokio::test]
    async fn test_concurrent_read_write_mixed() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(4096).await.unwrap();
        let storage = std::sync::Arc::new(storage);

        // 先写入初始数据
        for i in 0u8..16 {
            let offset = (i as u64) * 256;
            let data = vec![i; 256];
            storage.write_at(offset, &data).await.unwrap();
        }

        let mut handles = Vec::new();

        // 8 个读任务
        for i in 0u8..8 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let offset = (i as u64) * 256;
                let mut buf = [0u8; 256];
                let read = s.read_at(offset, &mut buf).await.unwrap();
                assert_eq!(read, 256);
                assert!(buf.iter().all(|&b| b == i), "读取区域 {offset} 数据不一致");
            }));
        }

        // 8 个写任务(写入另一半区域)
        for i in 8u8..16 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let offset = (i as u64) * 256;
                let data = vec![i + 100; 256];
                s.write_at(offset, &data).await.unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // 验证写入区域的数据已更新
        for i in 8u8..16 {
            let offset = (i as u64) * 256;
            let mut buf = [0u8; 256];
            storage.read_at(offset, &mut buf).await.unwrap();
            assert!(
                buf.iter().all(|&b| b == i + 100),
                "写入区域 {offset} 数据不一致"
            );
        }
    }
}
