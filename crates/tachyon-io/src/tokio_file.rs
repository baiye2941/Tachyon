use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
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
    /// Windows: seek_write 由 SetFilePointerEx + WriteFile 构成,非原子操作。
    /// 多线程并发 seek_write 可能导致写入位置错乱。Mutex 串行化保护。
    /// 非 Windows: seek_write 是原子的(基于 pread/pwrite),无需锁。
    #[cfg(target_os = "windows")]
    write_lock: Arc<std::sync::Mutex<()>>,
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
            #[cfg(target_os = "windows")]
            write_lock: Arc::new(std::sync::Mutex::new(())),
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
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(async move {
            use std::os::windows::fs::FileExt;
            let file = self.file.clone();
            let write_lock = self.write_lock.clone();
            tokio::task::spawn_blocking(move || {
                let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                file.seek_write(&data, offset).map_err(DownloadError::Io)
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
            let file = self.file.clone();
            let write_lock = self.write_lock.clone();
            // CRITICAL 修复:复制成 owned Bytes move 进 spawn_blocking。
            // 旧实现用 data.as_mut_ptr() as usize + from_raw_parts 裸指针跨
            // spawn_blocking,当 future 被 tokio::select! 取消时(如用户暂停下载),
            // batch(BytesMut)drop 但阻塞任务仍持裸指针 → use-after-free。
            // owned Bytes(Arc 引用计数)在 future drop 时闭包仍持所有权,安全。
            // 复制代价可接受:spawn_blocking 本就有阻塞线程切换开销,memcpy 带宽高。
            let data_bytes = bytes::Bytes::copy_from_slice(&data[..]);
            tokio::task::spawn_blocking(move || {
                let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                file.seek_write(&data_bytes, offset)
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
            let file = self.file.clone();
            // CRITICAL 修复(C-01 UAF):旧实现把 buf 的裸指针(as_mut_ptr as usize)
            // move 进 spawn_blocking,再 from_raw_parts_mut 重建切片。当 future 被
            // JoinSet::abort_all 取消时(downloader.rs verify 阶段某分片哈希不匹配),
            // buf 随 future drop 释放,但 spawn_blocking 任务继续运行持悬垂指针 → UAF。
            //
            // 修复(与 write_at_mut 对称):闭包内分配 owned 本地缓冲读盘,await 成功
            // 后再 copy_from_slice 写回调用方 buf。future 被取消时本地缓冲所有权随
            // 闭包 drop,无外部裸指针持有 → 取消安全。复制代价可接受:spawn_blocking
            // 本就有阻塞线程切换开销,且 read 后必须 memcpy 到调用方 buf。
            let buf_len = buf.len();
            tokio::task::spawn_blocking(move || {
                let mut local = vec![0u8; buf_len];
                file.seek_read(&mut local, offset)
                    .map(|n| (n, local))
                    .map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
            .map(|(n, local)| {
                buf[..n].copy_from_slice(&local[..n]);
                n
            })
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
        Box::pin(async move {
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || {
                // 先设置文件逻辑大小(EOF)
                file.set_len(size).map_err(DownloadError::Io)?;
                // 使用 SetFileInformationByHandle(FileAllocationInfo) 真正预分配物理磁盘块,
                // 避免稀疏文件仅扩展逻辑大小而不分配空间。
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
                {
                    use std::os::windows::io::AsRawHandle;
                    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
                    // Safety:
                    // - handle 来自合法的 Arc<File>,在 spawn_blocking 闭包执行期间保持存活
                    // - size 由调用方传入,来自文件元数据的合法大小值
                    // - 内核保证:失败时不影响文件已有状态
                    let result = unsafe {
                        windows_sys::Win32::Storage::FileSystem::SetFileValidData(
                            handle,
                            size as i64,
                        )
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
        })
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

#[cfg(target_os = "linux")]
impl AsyncStorage for TokioFile {
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
            // (详见 Windows impl 注释:future 被 select! 取消时 batch drop 但任务仍跑)
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
            // CRITICAL 修复(C-01 UAF):旧实现把 buf 裸指针 move 进 spawn_blocking,
            // JoinSet::abort_all 取消 future 时 buf 释放但任务仍持悬垂指针 → UAF。
            // 修复(与 write_at_mut 对称):闭包内 owned 本地缓冲读盘,await 后写回。
            let buf_len = buf.len();
            tokio::task::spawn_blocking(move || {
                let mut local = vec![0u8; buf_len];
                file.read_at(&mut local, offset)
                    .map(|n| (n, local))
                    .map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
            .map(|(n, local)| {
                buf[..n].copy_from_slice(&local[..n]);
                n
            })
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
        Box::pin(async move {
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || {
                use std::os::fd::AsRawFd;
                // Safety:
                // - file 是合法打开的 Arc<File>,在 spawn_blocking 闭包执行期间保持存活
                // - as_raw_fd() 返回的文件描述符在该期间有效
                // - mode=0、offset=0、len=size 均为合法的 fallocate 参数
                let ret = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, size as libc::off_t) };
                if ret != 0 {
                    return Err(DownloadError::Io(std::io::Error::last_os_error()));
                }
                Ok(())
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
        })
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

#[cfg(all(unix, not(target_os = "linux")))]
impl AsyncStorage for TokioFile {
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
            // (详见 Windows impl 注释:future 被 select! 取消时 batch drop 但任务仍跑)
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
            // CRITICAL 修复(C-01 UAF):旧实现把 buf 裸指针 move 进 spawn_blocking,
            // JoinSet::abort_all 取消 future 时 buf 释放但任务仍持悬垂指针 → UAF。
            // 修复(与 write_at_mut 对称):闭包内 owned 本地缓冲读盘,await 后写回。
            let buf_len = buf.len();
            tokio::task::spawn_blocking(move || {
                let mut local = vec![0u8; buf_len];
                file.read_at(&mut local, offset)
                    .map(|n| (n, local))
                    .map_err(DownloadError::Io)
            })
            .await
            .map_err(|e| DownloadError::Io(e.into()))?
            .map(|(n, local)| {
                buf[..n].copy_from_slice(&local[..n]);
                n
            })
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
        Box::pin(async move {
            let file = self.file.clone();
            tokio::task::spawn_blocking(move || file.set_len(size).map_err(DownloadError::Io))
                .await
                .map_err(|e| DownloadError::Io(e.into()))?
        })
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

    /// Windows:预分配后文件物理分配大小应达到请求大小
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_allocate_physical_size_windows() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(1024).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 1024);
        let alloc = file_allocation_size(tmp.path());
        assert!(
            alloc >= 1024,
            "预分配后文件物理分配大小 {} 小于请求大小 1024",
            alloc
        );
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

    #[tokio::test]
    async fn test_path_returns_correct_path() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        assert_eq!(storage.path(), tmp.path());
    }

    #[tokio::test]
    async fn test_close_calls_sync_data() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage
            .write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
        storage.close().await.unwrap();

        // 关闭后重新打开，验证数据已通过 sync_data 落盘
        let storage2 = TokioFile::open(tmp.path()).await.unwrap();
        let mut buf = [0u8; 5];
        let read = storage2.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 5);
        assert_eq!(&buf, b"hello");
    }

    /// Windows 路径下 write_at 使用 write_lock 串行化 seek_write，
    /// 并发写入同一文件不同偏移不应出现数据交错。
    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_windows_concurrent_write_at_no_interleave() {
        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        storage.allocate(4096).await.unwrap();
        let storage = std::sync::Arc::new(storage);

        let mut handles = Vec::new();
        for i in 0u8..16 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let offset = (i as u64) * 256;
                let data = Bytes::from(vec![i; 256]);
                s.write_at(offset, data).await.unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        for i in 0u8..16 {
            let offset = (i as u64) * 256;
            let mut buf = [0u8; 256];
            let read = storage.read_at(offset, &mut buf).await.unwrap();
            assert_eq!(read, 256);
            assert!(
                buf.iter().all(|&b| b == i),
                "区域 {offset} 数据不一致，期望全部为 {i}"
            );
        }
    }

    /// C-01 回归测试:`read_at` 跨 `spawn_blocking` 的 Use-After-Free。
    ///
    /// 复现 downloader.rs verify 阶段的取消路径:多个分片在 JoinSet 中并发
    /// `read_at().await`,任一完成即 `abort_all()` 取消其余。旧实现把调用方
    /// `&mut [u8]` 裸指针(as_mut_ptr as usize)move 进 spawn_blocking,被取消时
    /// buf 随 future drop 释放,但阻塞任务仍持悬垂指针 → UAF / panic。
    ///
    /// 修复后:闭包内分配 owned `Vec<u8>` 本地缓冲,await 成功后写回调用方 buf,
    /// future 被取消时本地缓冲所有权随闭包 drop,无外部裸指针持有 → 取消安全。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_read_at_joinset_abort_all_no_uaf() {
        use tokio::task::JoinSet;

        let tmp = NamedTempFile::new().unwrap();
        let storage = TokioFile::open(tmp.path()).await.unwrap();
        // 预置足够大的文件(256 KiB),使 read_at 在 spawn_blocking 线程上有
        // 可观的执行窗口,提高 abort 命中正在执行的阻塞任务的概率
        storage.allocate(256 * 1024).await.unwrap();
        let storage = std::sync::Arc::new(storage);

        // 多轮迭代提高竞态检出概率
        const ROUNDS: usize = 30;
        for round in 0..ROUNDS {
            let mut join_set: JoinSet<DownloadResult<usize>> = JoinSet::new();
            // 32 个并发读任务,每个读 4 KiB
            for i in 0u32..32 {
                let s = storage.clone();
                join_set.spawn(async move {
                    let offset = (i as u64) * 4096;
                    let mut buf = vec![0u8; 4096];
                    s.read_at(offset, &mut buf).await
                });
            }

            // 收第一个完成的结果后立即 abort_all,取消其余正在 await 的任务
            let first = join_set.join_next().await;
            join_set.abort_all();
            // 排空被取消任务(JoinError::Cancelled 是正常的,不是 panic/UAF)
            while let Some(res) = join_set.join_next().await {
                if let Ok(Ok(_)) = res {
                    // 个别任务可能在 abort 前正常完成,允许
                }
            }
            // 第一个完成的任务应成功(无 panic / 无 UAF 报错)
            let first = first.expect("至少一个任务应完成");
            match first {
                Ok(Ok(_n)) => {}
                Ok(Err(e)) => panic!("round {round}: read_at 返回错误: {e:?}"),
                Err(join_err) => {
                    // 第一个任务不应是 cancelled(它是被 join_next 取出的)
                    panic!("round {round}: 第一个任务 join 错误: {join_err}");
                }
            }
        }
    }
}
