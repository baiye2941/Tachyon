use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use tachyon_core::{DownloadError, DownloadResult};

use crate::storage::AsyncStorage;

#[cfg(target_os = "windows")]
mod win_share {
    pub const FILE_SHARE_READ: u32 = 0x00000001;
    pub const FILE_SHARE_WRITE: u32 = 0x00000002;
    pub const FILE_SHARE_DELETE: u32 = 0x00000004;
}

pub struct TokioFile {
    path: PathBuf,
    file: Arc<std::fs::File>,
}

impl TokioFile {
    #[cfg(target_os = "windows")]
    pub async fn open<P: AsRef<Path>>(path: P) -> DownloadResult<Self> {
        let path = path.as_ref().to_path_buf();
        use std::os::windows::fs::OpenOptionsExt;
        use win_share::*;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .map_err(DownloadError::Io)?;
        Ok(Self {
            path,
            file: Arc::new(file),
        })
    }

    #[cfg(not(target_os = "windows"))]
    pub async fn open<P: AsRef<Path>>(path: P) -> DownloadResult<Self> {
        let path = path.as_ref().to_path_buf();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(DownloadError::Io)?;
        Ok(Self {
            path,
            file: Arc::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn close(&self) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }
}

#[cfg(target_os = "windows")]
impl AsyncStorage for TokioFile {
    async fn write_at(&self, offset: u64, data: Bytes) -> DownloadResult<usize> {
        use std::os::windows::fs::FileExt;
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            file.seek_write(&data, offset).map_err(DownloadError::Io)
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> DownloadResult<usize> {
        use std::os::windows::fs::FileExt;
        let file = self.file.clone();
        let buf_len = buf.len();
        let mut owned_buf = vec![0u8; buf_len];
        let (n, owned_buf) = tokio::task::spawn_blocking(move || {
            let n = file.seek_read(&mut owned_buf, offset)?;
            Ok::<_, std::io::Error>((n, owned_buf))
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
        .map_err(DownloadError::Io)?;
        buf[..n].copy_from_slice(&owned_buf[..n]);
        Ok(n)
    }

    async fn sync(&self) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn allocate(&self, size: u64) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.set_len(size).map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn file_size(&self) -> DownloadResult<u64> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            file.metadata().map(|m| m.len()).map_err(DownloadError::Io)
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn close(&self) -> DownloadResult<()> {
        self.close().await
    }
}

#[cfg(target_os = "linux")]
impl AsyncStorage for TokioFile {
    async fn write_at(&self, offset: u64, data: Bytes) -> DownloadResult<usize> {
        use std::os::unix::fs::FileExt;
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.write_at(&data, offset).map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> DownloadResult<usize> {
        use std::os::unix::fs::FileExt;
        let file = self.file.clone();
        let buf_len = buf.len();
        let mut owned_buf = vec![0u8; buf_len];
        let (n, owned_buf) = tokio::task::spawn_blocking(move || {
            let n = file.read_at(&mut owned_buf, offset)?;
            Ok::<_, std::io::Error>((n, owned_buf))
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
        .map_err(DownloadError::Io)?;
        buf[..n].copy_from_slice(&owned_buf[..n]);
        Ok(n)
    }

    async fn sync(&self) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn allocate(&self, size: u64) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            use std::os::fd::AsRawFd;
            let ret = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, size as libc::off_t) };
            if ret != 0 {
                return Err(DownloadError::Io(std::io::Error::last_os_error()));
            }
            Ok(())
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn file_size(&self) -> DownloadResult<u64> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            file.metadata().map(|m| m.len()).map_err(DownloadError::Io)
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn close(&self) -> DownloadResult<()> {
        self.close().await
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
impl AsyncStorage for TokioFile {
    async fn write_at(&self, offset: u64, data: Bytes) -> DownloadResult<usize> {
        use std::os::unix::fs::FileExt;
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.write_at(&data, offset).map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> DownloadResult<usize> {
        use std::os::unix::fs::FileExt;
        let file = self.file.clone();
        let buf_len = buf.len();
        let mut owned_buf = vec![0u8; buf_len];
        let (n, owned_buf) = tokio::task::spawn_blocking(move || {
            let n = file.read_at(&mut owned_buf, offset)?;
            Ok::<_, std::io::Error>((n, owned_buf))
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
        .map_err(DownloadError::Io)?;
        buf[..n].copy_from_slice(&owned_buf[..n]);
        Ok(n)
    }

    async fn sync(&self) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn allocate(&self, size: u64) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.set_len(size).map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn file_size(&self) -> DownloadResult<u64> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            file.metadata().map(|m| m.len()).map_err(DownloadError::Io)
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
    }

    async fn close(&self) -> DownloadResult<()> {
        self.close().await
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
        let written = storage
            .write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
        assert_eq!(written, 5);
    }

    #[tokio::test]
    async fn test_write_and_read() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage
            .write_at(0, Bytes::from_static(b"hello world"))
            .await
            .unwrap();
        let mut buf = [0u8; 11];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[tokio::test]
    async fn test_read_at_offset() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage
            .write_at(0, Bytes::from_static(b"hello world"))
            .await
            .unwrap();
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
        storage
            .write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
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
        storage
            .write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
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
                let data = Bytes::from(vec![i; 256]);
                let offset = (i as u64) * 256;
                s.write_at(offset, data).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

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

    #[tokio::test]
    async fn test_concurrent_write_at_correctness() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        let total_size = 8192u64;
        storage.allocate(total_size).await.unwrap();
        let storage = std::sync::Arc::new(storage);

        let mut handles = Vec::new();

        for i in 0u32..32 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let offset = (i as u64) * 256;
                let data: Bytes = Bytes::from(
                    (0..256u32)
                        .map(|j| ((i * 256 + j) % 256) as u8)
                        .collect::<Vec<u8>>(),
                );
                s.write_at(offset, data).await.unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        for i in 0u32..32 {
            let offset = (i as u64) * 256;
            let mut buf = [0u8; 256];
            let read = storage.read_at(offset, &mut buf).await.unwrap();
            assert_eq!(read, 256);
            for (j, &byte) in buf.iter().enumerate() {
                let expected = ((i * 256 + j as u32) % 256) as u8;
                assert_eq!(
                    byte, expected,
                    "区域 {offset} 字节 {j} 不一致:期望 {expected},实际 {byte}"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_concurrent_read_write_mixed() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(4096).await.unwrap();
        let storage = std::sync::Arc::new(storage);

        for i in 0u8..16 {
            let offset = (i as u64) * 256;
            let data = Bytes::from(vec![i; 256]);
            storage.write_at(offset, data).await.unwrap();
        }

        let mut handles = Vec::new();

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

        for i in 8u8..16 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let offset = (i as u64) * 256;
                let data = Bytes::from(vec![i + 100; 256]);
                s.write_at(offset, data).await.unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

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
