//! 基于 tokio 的异步文件 I/O 实现
//!
//! 跨平台的文件存储后端,使用 tokio::fs::File 实现异步读写。

use std::path::{Path, PathBuf};

use qf_core::{QfError, QfResult};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::storage::AsyncStorage;

/// 基于 tokio 的异步文件存储
pub struct TokioFile {
    path: PathBuf,
    file: tokio::sync::Mutex<File>,
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
            .map_err(|e| QfError::Io(e))?;
        Ok(Self {
            path,
            file: tokio::sync::Mutex::new(file),
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
    async fn test_write_at_offset() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.write_at(0, b"AAAA").await.unwrap();
        storage.write_at(4, b"BBBB").await.unwrap();

        let mut buf = [0u8; 8];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 8);
        assert_eq!(&buf, b"AAAABBBB");
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
        storage.write_at(0, b"test").await.unwrap();
        assert!(storage.sync().await.is_ok());
    }

    #[tokio::test]
    async fn test_read_past_end() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.write_at(0, b"abc").await.unwrap();

        let mut buf = [0u8; 10];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 3);
    }

    #[tokio::test]
    async fn test_file_size() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 0);
        storage.write_at(0, b"12345").await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 5);
    }

    #[tokio::test]
    async fn test_path() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        assert_eq!(storage.path(), tmp.path());
    }
}
