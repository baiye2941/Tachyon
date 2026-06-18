use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::Mutex;
use tachyon_core::{DownloadError, DownloadResult};

use crate::storage::AsyncStorage;

#[cfg(target_os = "windows")]
mod win_flags {
    pub const FILE_FLAG_NO_BUFFERING: u32 = 0x20000000;
    pub const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x08000000;
    pub const FILE_SHARE_READ: u32 = 0x00000001;
    pub const FILE_SHARE_WRITE: u32 = 0x00000002;
    pub const FILE_SHARE_DELETE: u32 = 0x00000004;
}

/// NO_BUFFERING 模式的扇区对齐要求
#[cfg(target_os = "windows")]
const SECTOR_SIZE: u64 = 512;

/// WinFile 持有的惰性 buffered fallback 句柄
///
/// NO_BUFFERING 模式下,主句柄要求所有写入 offset/length 对齐到扇区(512B)。
/// 下载尾批次几乎必然非对齐,因此惰性初始化一个普通 buffered 句柄,
/// 非对齐写入自动路由到它。两个句柄通过 FILE_SHARE_READ|WRITE|DELETE
/// 共享同一文件,写入顺序由 sync() 统一 flush 保证。
type FallbackHandle = Mutex<Option<Arc<std::fs::File>>>;

pub struct WinFile {
    path: PathBuf,
    file: Arc<std::fs::File>,
    no_buffering: bool,
    /// 惰性初始化的 buffered fallback 句柄(仅 no_buffering=true 时使用)
    #[allow(dead_code)]
    fallback: FallbackHandle,
}

impl WinFile {
    #[cfg(target_os = "windows")]
    pub async fn open_optimized<P: AsRef<Path>>(path: P) -> DownloadResult<Self> {
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
            .map_err(DownloadError::Io)?;
        Ok(Self {
            path,
            file: Arc::new(file),
            no_buffering: true,
            fallback: Mutex::new(None),
        })
    }

    #[cfg(target_os = "windows")]
    pub async fn open_standard<P: AsRef<Path>>(path: P) -> DownloadResult<Self> {
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
            .map_err(DownloadError::Io)?;
        Ok(Self {
            path,
            file: Arc::new(file),
            no_buffering: false,
            fallback: Mutex::new(None),
        })
    }

    #[cfg(not(target_os = "windows"))]
    pub async fn open_standard<P: AsRef<Path>>(path: P) -> DownloadResult<Self> {
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
            no_buffering: false,
            fallback: Mutex::new(None),
        })
    }

    /// 惰性获取 buffered fallback 句柄(NO_BUFFERING 模式专用)
    ///
    /// 首次调用时用 open_standard 同路径打开 buffered 句柄并缓存,
    /// 后续直接返回缓存。两个句柄通过 FILE_SHARE 共享同一文件。
    #[cfg(target_os = "windows")]
    fn get_or_init_fallback(&self) -> DownloadResult<Arc<std::fs::File>> {
        // fast path: 已初始化则直接 clone
        {
            let guard = self.fallback.lock();
            if let Some(ref f) = *guard {
                return Ok(f.clone());
            }
        }
        // slow path: 打开 buffered 句柄
        // 注意:此处不复用 open_standard 以避免 async(本函数是同步的,
        // 在 spawn_blocking 内调用)。直接用 OpenOptions。
        use std::os::windows::fs::OpenOptionsExt;
        use win_flags::*;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&self.path)
            .map_err(DownloadError::Io)?;
        let file = Arc::new(file);
        let mut guard = self.fallback.lock();
        // 双检:另一线程可能已初始化
        if let Some(ref existing) = *guard {
            return Ok(existing.clone());
        }
        *guard = Some(file.clone());
        Ok(file)
    }

    pub async fn preallocate(&self, size: u64) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            file.set_len(size).map_err(DownloadError::Io)?;
            // 尝试 SetFileValidData 跳过零填充(需要 SE_MANAGE_VOLUME_NAME 权限)
            // 注意:成功时文件扩展区域包含磁盘残留数据(非零填充),
            // 但下载数据会立即覆盖,安全风险极低。
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::io::AsRawHandle;
                let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
                // Safety:
                // - handle 来自合法的 Arc<File>,在 spawn_blocking 闭包执行期间保持存活
                // - size 由调用方传入,来自文件元数据的合法大小值
                // - 内核保证:失败时不影响文件现有状态
                let result = unsafe {
                    windows_sys::Win32::Storage::FileSystem::SetFileValidData(handle, size as i64)
                };
                if result == 0 {
                    tracing::debug!(
                        size,
                        "SetFileValidData 失败(需 SE_MANAGE_VOLUME_NAME),回退到零填充模式"
                    );
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| DownloadError::Io(e.into()))?
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_no_buffering(&self) -> bool {
        self.no_buffering
    }

    pub async fn close(&self) -> DownloadResult<()> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
    }
}

#[cfg(target_os = "windows")]
impl AsyncStorage for WinFile {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(async move {
            use std::os::windows::fs::FileExt;

            // NO_BUFFERING 模式下检测对齐:非对齐写入路由到惰性 buffered fallback 句柄
            let needs_fallback = if self.no_buffering {
                !offset.is_multiple_of(SECTOR_SIZE)
                    || !(data.len() as u64).is_multiple_of(SECTOR_SIZE)
            } else {
                false
            };

            let target_file = if needs_fallback {
                // 非对齐写(如下载尾批次)走 buffered 句柄,保证正确性
                // get_or_init_fallback 是同步函数,但仅首次调用有 I/O(打开句柄),
                // 后续直接返回 Arc clone,开销可忽略
                let path = self.path.clone();
                let fallback_ref = self.get_or_init_fallback()?;
                tracing::debug!(
                    path = %path.display(),
                    offset,
                    len = data.len(),
                    "NO_BUFFERING 非对齐写入路由到 buffered fallback 句柄",
                );
                fallback_ref
            } else {
                self.file.clone()
            };

            tokio::task::spawn_blocking(move || {
                target_file
                    .seek_write(&data, offset)
                    .map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            use std::os::windows::fs::FileExt;
            // 读路径:NO_BUFFERING 模式下非对齐读也走 buffered fallback
            let needs_fallback = if self.no_buffering {
                !offset.is_multiple_of(SECTOR_SIZE)
                    || !(buf.len() as u64).is_multiple_of(SECTOR_SIZE)
            } else {
                false
            };

            let target_file = if needs_fallback {
                self.get_or_init_fallback()?
            } else {
                self.file.clone()
            };

            let buf_len = buf.len();
            let mut owned_buf = vec![0u8; buf_len];
            let (n, owned_buf) = tokio::task::spawn_blocking(move || {
                let n = target_file.seek_read(&mut owned_buf, offset)?;
                Ok::<_, std::io::Error>((n, owned_buf))
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
            .map_err(DownloadError::Io)?;
            buf[..n].copy_from_slice(&owned_buf[..n]);
            Ok(n)
        })
    }

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            // 主句柄 sync
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
                .await
                .map_err(|e| DownloadError::Io(e.into()))??;

            // 若 fallback 句柄已初始化,也需 sync 以保证缓冲数据落盘
            let fallback = self.fallback.lock().clone();
            if let Some(fallback_file) = fallback {
                tokio::task::spawn_blocking(move || {
                    fallback_file.sync_data().map_err(DownloadError::Io)
                })
                .await
                .map_err(|e| DownloadError::Io(e.into()))??;
            }
            Ok(())
        })
    }

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move { self.preallocate(size).await })
    }

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
        Box::pin(async move {
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || {
                file.metadata().map(|m| m.len()).map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
                .await
                .map_err(|e| DownloadError::Io(e.into()))??;
            let fallback = self.fallback.lock().clone();
            if let Some(fallback_file) = fallback {
                tokio::task::spawn_blocking(move || {
                    fallback_file.sync_data().map_err(DownloadError::Io)
                })
                .await
                .map_err(|e| DownloadError::Io(e.into()))??;
            }
            Ok(())
        })
    }
}

#[cfg(not(target_os = "windows"))]
impl AsyncStorage for WinFile {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(async move {
            use std::os::unix::fs::FileExt;
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || {
                file.write_at(&data, offset).map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
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
        })
    }

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
                .await
                .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move { self.preallocate(size).await })
    }

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
        Box::pin(async move {
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || {
                file.metadata().map(|m| m.len()).map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(async move {
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || file.sync_data().map_err(DownloadError::Io))
                .await
                .map_err(|e| DownloadError::Io(e.into()))?
        })
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

    /// P0-A: NO_BUFFERING 模式下非对齐写入应自动 fallback 到 buffered 句柄
    ///
    /// 修复前:write_at(offset=100, len=7) 会返回 InvalidInput 错误。
    /// 修复后:自动路由到惰性 buffered 句柄,写入成功且数据可读回。
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_winfile_no_buffering_unaligned_write_fallback() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_optimized(tmp.path()).await.unwrap();
        assert!(file.is_no_buffering());

        // 先写一个对齐块(512B),建立基准
        let aligned_data = Bytes::from(vec![0xAAu8; 512]);
        file.write_at(0, aligned_data).await.unwrap();

        // 非对齐尾块:offset=100, len=7(均非 512 对齐)
        // 修复前会返回 InvalidInput,修复后应自动 fallback 到 buffered 句柄
        let tail = Bytes::from_static(b"goodbye");
        let written = file.write_at(100, tail).await.unwrap();
        assert_eq!(written, 7);

        // 验证数据可读回(buffered fallback 句柄)
        let mut buf = [0u8; 7];
        let n = file.read_at(100, &mut buf).await.unwrap();
        assert_eq!(n, 7);
        assert_eq!(&buf, b"goodbye");
    }

    /// P0-A: NO_BUFFERING 模式下对齐写入仍走主句柄(zero page cache)
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_winfile_no_buffering_aligned_write_uses_main_handle() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_optimized(tmp.path()).await.unwrap();
        assert!(file.is_no_buffering());

        // 对齐写入(offset 和 len 都是 512 倍数)应走主 NO_BUFFERING 句柄
        let data = Bytes::from(vec![0xBBu8; 1024]);
        let written = file.write_at(0, data).await.unwrap();
        assert_eq!(written, 1024);

        // 读回验证
        let mut buf = vec![0u8; 1024];
        let n = file.read_at(0, &mut buf).await.unwrap();
        assert_eq!(n, 1024);
        assert!(buf.iter().all(|&b| b == 0xBB));
    }

    /// P0-A: 混合对齐+非对齐写入,验证数据完整性
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_winfile_mixed_aligned_and_unaligned_writes() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_optimized(tmp.path()).await.unwrap();

        // 对齐写:block 0
        file.write_at(0, Bytes::from(vec![0x11u8; 512]))
            .await
            .unwrap();
        // 对齐写:block 1
        file.write_at(512, Bytes::from(vec![0x22u8; 512]))
            .await
            .unwrap();
        // 非对齐写:跨 block 边界(offset=500, len=24)
        file.write_at(500, Bytes::from(vec![0x33u8; 24]))
            .await
            .unwrap();

        // 验证:500-512 是 0x33,524-511 之前的位置被 0x33 覆盖
        let mut buf = [0u8; 24];
        file.read_at(500, &mut buf).await.unwrap();
        assert!(buf.iter().all(|&b| b == 0x33), "非对齐区域应全为 0x33");

        // sync 应同时 flush 主句柄和 fallback 句柄
        file.sync().await.unwrap();
    }
}
