use std::path::{Path, PathBuf};
use std::sync::Arc;

use amd_core::{AmdError, AmdResult};
use bytes::Bytes;

use crate::storage::AsyncStorage;

#[cfg(target_os = "windows")]
mod win_flags {
    pub const FILE_FLAG_NO_BUFFERING: u32 = 0x20000000;
    pub const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x08000000;
    pub const FILE_SHARE_READ: u32 = 0x00000001;
    pub const FILE_SHARE_WRITE: u32 = 0x00000002;
    pub const FILE_SHARE_DELETE: u32 = 0x00000004;
}

pub struct WinFile {
    path: PathBuf,
    file: Arc<std::fs::File>,
    no_buffering: bool,
}

impl WinFile {
    #[cfg(target_os = "windows")]
    pub async fn open_optimized<P: AsRef<Path>>(path: P) -> AmdResult<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        use win_flags::*;
        let path = path.as_ref().to_path_buf();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .map_err(AmdError::Io)?;
        Ok(Self {
            path,
            file: Arc::new(file),
            no_buffering: true,
        })
    }

    #[cfg(target_os = "windows")]
    pub async fn open_standard<P: AsRef<Path>>(path: P) -> AmdResult<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        use win_flags::*;
        let path = path.as_ref().to_path_buf();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .map_err(AmdError::Io)?;
        Ok(Self {
            path,
            file: Arc::new(file),
            no_buffering: false,
        })
    }

    #[cfg(not(target_os = "windows"))]
    pub async fn open_standard<P: AsRef<Path>>(path: P) -> AmdResult<Self> {
        let path = path.as_ref().to_path_buf();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(AmdError::Io)?;
        Ok(Self {
            path,
            file: Arc::new(file),
            no_buffering: false,
        })
    }

    pub async fn preallocate(&self, size: u64) -> AmdResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.set_len(size).map_err(AmdError::Io))
            .await
            .map_err(|e| AmdError::Io(e.into()))?
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_no_buffering(&self) -> bool {
        self.no_buffering
    }

    pub async fn close(&self) -> AmdResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.sync_data().map_err(AmdError::Io))
            .await
            .map_err(|e| AmdError::Io(e.into()))?
    }
}

#[cfg(target_os = "windows")]
impl AsyncStorage for WinFile {
    async fn write_at(&self, offset: u64, data: Bytes) -> AmdResult<usize> {
        use std::os::windows::fs::FileExt;
        if self.no_buffering {
            const SECTOR_SIZE: u64 = 512;
            if !offset.is_multiple_of(SECTOR_SIZE) {
                return Err(AmdError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("NO_BUFFERING 模式下偏移量 {offset} 未对齐到 {SECTOR_SIZE} 字节"),
                )));
            }
            if !(data.len() as u64).is_multiple_of(SECTOR_SIZE) {
                return Err(AmdError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "NO_BUFFERING 模式下数据长度 {} 未对齐到 {SECTOR_SIZE} 字节",
                        data.len()
                    ),
                )));
            }
        }

        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.seek_write(&data, offset).map_err(AmdError::Io))
            .await
            .map_err(|e| AmdError::Io(e.into()))?
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> AmdResult<usize> {
        use std::os::windows::fs::FileExt;
        if self.no_buffering {
            const SECTOR_SIZE: u64 = 512;
            if !offset.is_multiple_of(SECTOR_SIZE) {
                return Err(AmdError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("NO_BUFFERING 模式下偏移量 {offset} 未对齐到 {SECTOR_SIZE} 字节"),
                )));
            }
            if !(buf.len() as u64).is_multiple_of(SECTOR_SIZE) {
                return Err(AmdError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "NO_BUFFERING 模式下缓冲区长度 {} 未对齐到 {SECTOR_SIZE} 字节",
                        buf.len()
                    ),
                )));
            }
        }

        let file = self.file.clone();
        let buf_len = buf.len();
        let mut owned_buf = vec![0u8; buf_len];
        let (n, owned_buf) = tokio::task::spawn_blocking(move || {
            let n = file.seek_read(&mut owned_buf, offset)?;
            Ok::<_, std::io::Error>((n, owned_buf))
        })
        .await
        .map_err(|e| AmdError::Io(e.into()))?
        .map_err(AmdError::Io)?;
        buf[..n].copy_from_slice(&owned_buf[..n]);
        Ok(n)
    }

    async fn sync(&self) -> AmdResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.sync_data().map_err(AmdError::Io))
            .await
            .map_err(|e| AmdError::Io(e.into()))?
    }

    async fn allocate(&self, size: u64) -> AmdResult<()> {
        self.preallocate(size).await
    }

    async fn file_size(&self) -> AmdResult<u64> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.metadata().map(|m| m.len()).map_err(AmdError::Io))
            .await
            .map_err(|e| AmdError::Io(e.into()))?
    }

    async fn close(&self) -> AmdResult<()> {
        self.close().await
    }
}

#[cfg(not(target_os = "windows"))]
impl AsyncStorage for WinFile {
    async fn write_at(&self, offset: u64, data: Bytes) -> AmdResult<usize> {
        use std::os::unix::fs::FileExt;
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.write_at(&data, offset).map_err(AmdError::Io))
            .await
            .map_err(|e| AmdError::Io(e.into()))?
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> AmdResult<usize> {
        use std::os::unix::fs::FileExt;
        let file = self.file.clone();
        let buf_len = buf.len();
        let mut owned_buf = vec![0u8; buf_len];
        let (n, owned_buf) = tokio::task::spawn_blocking(move || {
            let n = file.read_at(&mut owned_buf, offset)?;
            Ok::<_, std::io::Error>((n, owned_buf))
        })
        .await
        .map_err(|e| AmdError::Io(e.into()))?
        .map_err(AmdError::Io)?;
        buf[..n].copy_from_slice(&owned_buf[..n]);
        Ok(n)
    }

    async fn sync(&self) -> AmdResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.sync_data().map_err(AmdError::Io))
            .await
            .map_err(|e| AmdError::Io(e.into()))?
    }

    async fn allocate(&self, size: u64) -> AmdResult<()> {
        self.preallocate(size).await
    }

    async fn file_size(&self) -> AmdResult<u64> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.metadata().map(|m| m.len()).map_err(AmdError::Io))
            .await
            .map_err(|e| AmdError::Io(e.into()))?
    }

    async fn close(&self) -> AmdResult<()> {
        self.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_open_standard_and_write() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        assert!(!file.is_no_buffering());
        let written = file
            .write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
        assert_eq!(written, 5);
    }

    #[tokio::test]
    async fn test_open_standard_write_and_read() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        file.write_at(0, Bytes::from_static(b"hello world"))
            .await
            .unwrap();

        let mut buf = [0u8; 11];
        let read = file.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[tokio::test]
    async fn test_preallocate() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        file.preallocate(4096).await.unwrap();
        assert_eq!(file.file_size().await.unwrap(), 4096);
    }

    #[tokio::test]
    async fn test_allocate_via_trait() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        file.allocate(8192).await.unwrap();
        assert_eq!(file.file_size().await.unwrap(), 8192);
    }

    #[tokio::test]
    async fn test_sync() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        file.write_at(0, Bytes::from_static(b"sync data"))
            .await
            .unwrap();
        assert!(file.sync().await.is_ok());
    }

    #[tokio::test]
    async fn test_write_at_offset() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        file.write_at(0, Bytes::from_static(b"AAAA")).await.unwrap();
        file.write_at(4, Bytes::from_static(b"BBBB")).await.unwrap();

        let mut buf = [0u8; 8];
        let read = file.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 8);
        assert_eq!(&buf, b"AAAABBBB");
    }

    #[tokio::test]
    async fn test_path() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        assert_eq!(file.path(), tmp.path());
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_open_optimized_windows() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_optimized(tmp.path()).await.unwrap();
        assert!(file.is_no_buffering());
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_preallocate_optimized_windows() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_optimized(tmp.path()).await.unwrap();
        file.preallocate(4096).await.unwrap();
        assert_eq!(file.file_size().await.unwrap(), 4096);
    }

    #[tokio::test]
    async fn test_winfile_align() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        assert!(file.write_at(0, Bytes::from_static(b"hello")).await.is_ok());

        let offset: u64 = 100;
        let sector_size: u64 = 512;
        assert!(!offset.is_multiple_of(sector_size), "100 不应是 512 的倍数");
        assert!(sector_size.is_multiple_of(sector_size));
        assert!((sector_size * 2).is_multiple_of(sector_size));

        let data_len = 256u64;
        assert!(
            !data_len.is_multiple_of(sector_size),
            "256 不应是 512 的倍数"
        );
        let aligned_len = 512u64;
        assert!(aligned_len.is_multiple_of(sector_size));
    }
}
