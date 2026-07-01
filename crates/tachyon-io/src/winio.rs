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
    /// Windows: seek_write 由 SetFilePointerEx + WriteFile 构成,非原子操作。
    /// 多任务并发 seek_write 同一 per-handle 文件指针会互相覆盖写入位置。
    /// 双缓冲(网络读与落盘重叠)会使并发写成为常态,故用 Mutex 串行化保护。
    /// 非 Windows: seek_write 原子(基于 pread/pwrite),此字段不使用。
    #[allow(dead_code)]
    write_lock: Arc<std::sync::Mutex<()>>,
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
            write_lock: Arc::new(std::sync::Mutex::new(())),
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
            write_lock: Arc::new(std::sync::Mutex::new(())),
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
            write_lock: Arc::new(std::sync::Mutex::new(())),
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
            // 先设置文件逻辑大小(EOF)
            file.set_len(size).map_err(DownloadError::Io)?;
            // 使用 SetFileInformationByHandle(FileAllocationInfo) 真正预分配物理磁盘块,
            // 避免稀疏文件仅扩展逻辑大小而不分配空间。
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::io::AsRawHandle;
                let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
                let info = windows_sys::Win32::Storage::FileSystem::FILE_ALLOCATION_INFO {
                    AllocationSize: size as i64,
                };
                // Safety:
                // - handle 来自合法的 Arc<File>,在 spawn_blocking 闭包执行期间保持存活
                // - info 指针指向有效的 FILE_ALLOCATION_INFO 结构
                // - FileAllocationInfo 是 Windows 定义的标准信息类
                // - 失败时通过 last_os_error 返回错误,不破坏文件已有状态
                let result = unsafe {
                    windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle(
                        handle,
                        windows_sys::Win32::Storage::FileSystem::FileAllocationInfo,
                        &info as *const _ as *const std::ffi::c_void,
                        std::mem::size_of::<
                            windows_sys::Win32::Storage::FileSystem::FILE_ALLOCATION_INFO,
                        >() as u32,
                    )
                };
                if result == 0 {
                    return Err(DownloadError::Io(std::io::Error::last_os_error()));
                }
            }
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
                // - 内核保证:失败时不影响文件已有状态
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

            // NO_BUFFERING 三重对齐要求(任一不满足都会返回 ERROR_INVALID_PARAMETER):
            // 1. 文件偏移按扇区对齐
            // 2. 写入长度按扇区对齐
            // 3. 缓冲区指针(内存地址)按扇区对齐 — bytes::Bytes 内部 Vec<u8> 通常仅
            //    16B 对齐,需显式校验,否则恰好 offset/len 对齐时会触发内核报错。
            let needs_fallback = if self.no_buffering {
                let buf_addr = data.as_ptr() as usize as u64;
                !offset.is_multiple_of(SECTOR_SIZE)
                    || !(data.len() as u64).is_multiple_of(SECTOR_SIZE)
                    || !buf_addr.is_multiple_of(SECTOR_SIZE)
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

            let write_lock = self.write_lock.clone();
            tokio::task::spawn_blocking(move || {
                // seek_write 非原子,串行化保证并发写不交错文件指针
                let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                target_file
                    .seek_write(&data, offset)
                    .map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
        })
    }

    fn write_at_mut<'a>(
        &'a self,
        offset: u64,
        data: &'a mut bytes::BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            use std::os::windows::fs::FileExt;

            // NO_BUFFERING 三重对齐要求(同 write_at):offset/length/buffer pointer
            let needs_fallback = if self.no_buffering {
                let buf_addr = data.as_ptr() as usize as u64;
                !offset.is_multiple_of(SECTOR_SIZE)
                    || !(data.len() as u64).is_multiple_of(SECTOR_SIZE)
                    || !buf_addr.is_multiple_of(SECTOR_SIZE)
            } else {
                false
            };

            let target_file = if needs_fallback {
                // 非对齐写(如下载尾批次)走 buffered 句柄,保证正确性
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

            let write_lock = self.write_lock.clone();
            // CRITICAL 修复:复制成 owned Bytes move 进 spawn_blocking,消除裸指针 UAF
            // (future 被 select! 取消时 batch drop 但 spawn_blocking 任务仍跑)
            let data_bytes = bytes::Bytes::copy_from_slice(&data[..]);
            tokio::task::spawn_blocking(move || {
                // seek_write 非原子,串行化保证并发写不交错文件指针
                let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                target_file
                    .seek_write(&data_bytes, offset)
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
            // 读路径:NO_BUFFERING 主句柄要求 offset/length/buffer 三者均按扇区对齐。
            // 内部 owned_buf 是 Vec<u8>(堆分配,通常仅 16B 对齐),无法保证扇区对齐,
            // 因此 NO_BUFFERING 模式下统一走 buffered fallback 句柄(无对齐限制)。
            let needs_fallback = self.no_buffering;

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

    fn write_at_mut<'a>(
        &'a self,
        offset: u64,
        data: &'a mut bytes::BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            use std::os::unix::fs::FileExt;
            let file = self.file.clone();
            // CRITICAL 修复:复制成 owned Bytes move 进 spawn_blocking,消除裸指针 UAF
            let data_bytes = bytes::Bytes::copy_from_slice(&data[..]);
            tokio::task::spawn_blocking(move || {
                file.write_at(&data_bytes, offset)
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

    /// 获取 Windows 文件分配大小(物理磁盘分配)
    #[cfg(target_os = "windows")]
    fn file_allocation_size(path: &std::path::Path) -> u64 {
        use std::os::windows::io::AsRawHandle;
        let file = std::fs::File::open(path).unwrap();
        let mut info: windows_sys::Win32::Storage::FileSystem::FILE_STANDARD_INFO =
            unsafe { std::mem::zeroed() };
        // Safety:
        // - file 是合法打开的文件句柄
        // - info 指针指向长度为 size_of::<FILE_STANDARD_INFO>() 的可写内存
        // - FileStandardInfo 是 Windows 定义的标准信息类
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx(
                file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
                windows_sys::Win32::Storage::FileSystem::FileStandardInfo,
                &mut info as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<windows_sys::Win32::Storage::FileSystem::FILE_STANDARD_INFO>()
                    as u32,
            )
        };
        assert!(ok != 0, "GetFileInformationByHandleEx 失败");
        info.AllocationSize as u64
    }

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

    /// Windows:preallocate 后文件物理分配大小应达到请求大小
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_preallocate_physical_size_windows() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        file.preallocate(4096).await.unwrap();
        assert_eq!(file.file_size().await.unwrap(), 4096);
        let alloc = file_allocation_size(tmp.path());
        assert!(
            alloc >= 4096,
            "预分配后文件物理分配大小 {} 小于请求大小 4096",
            alloc
        );
    }

    /// Windows:通过 trait allocate 后文件物理分配大小应达到请求大小
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_allocate_via_trait_physical_size_windows() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_standard(tmp.path()).await.unwrap();
        file.allocate(8192).await.unwrap();
        assert_eq!(file.file_size().await.unwrap(), 8192);
        let alloc = file_allocation_size(tmp.path());
        assert!(
            alloc >= 8192,
            "预分配后文件物理分配大小 {} 小于请求大小 8192",
            alloc
        );
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

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_winfile_close_flushes_data() {
        let tmp = NamedTempFile::new().unwrap();
        let file = WinFile::open_optimized(tmp.path()).await.unwrap();
        file.write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
        file.close().await.unwrap();

        let file2 = WinFile::open_standard(tmp.path()).await.unwrap();
        let mut buf = [0u8; 5];
        let read = file2.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 5);
        assert_eq!(&buf, b"hello");
    }

    /// WinFile 的 write_at 经 spawn_blocking 调用 seek_write
    /// (SetFilePointerEx + WriteFile 两步,非原子),且无 write_lock 保护。
    /// 同一 per-handle 文件指针是多任务共享状态,并发 seek_write 不同 offset
    /// 会互相覆盖文件指针位置,导致写入错位。此测试验证并发写不交错。
    /// 参考实现 tokio_file.rs::test_windows_concurrent_write_at_no_interleave
    /// 因有 write_lock 而通过;WinFile 缺 write_lock,当前预期失败(RED)。
    ///
    /// 用 open_standard(no_buffering=false)使所有写都走主句柄的 seek_write
    /// (无 fallback 分流),并多轮迭代以提高竞态检出概率。
    #[cfg(target_os = "windows")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_concurrent_write_at_no_interleave() {
        const ROUNDS: usize = 20;
        const CONCURRENCY: u8 = 32;
        const REGION: usize = 256;
        let total = (CONCURRENCY as usize) * REGION;

        for round in 0..ROUNDS {
            let tmp = NamedTempFile::new().unwrap();
            let storage = WinFile::open_standard(tmp.path()).await.unwrap();
            storage.allocate(total as u64).await.unwrap();
            let storage = std::sync::Arc::new(storage);

            // 并发写:每个 i 写到 offset=i*REGION,内容全部为 i(REGION 字节)
            let mut handles = Vec::new();
            for i in 0u8..CONCURRENCY {
                let s = storage.clone();
                handles.push(tokio::spawn(async move {
                    let offset = (i as u64) * REGION as u64;
                    let data = Bytes::from(vec![i; REGION]);
                    s.write_at(offset, data).await.unwrap();
                }));
            }
            for handle in handles {
                handle.await.unwrap();
            }

            // 逐区域回读,断言全部等于 i(不应出现交错)
            for i in 0u8..CONCURRENCY {
                let offset = (i as u64) * REGION as u64;
                let mut buf = [0u8; REGION];
                let read = storage.read_at(offset, &mut buf).await.unwrap();
                assert_eq!(read, REGION);
                assert!(
                    buf.iter().all(|&b| b == i),
                    "round {round} 区域 {offset} 数据不一致,期望全部为 {i}"
                );
            }
        }
    }
}
