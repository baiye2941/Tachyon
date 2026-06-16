//! 存储适配器: 类型擦除存储包装器 + 分片进度消息
//!
//! `DynStorage` 将任意 `AsyncStorage` 实现包装为统一的动态分发类型,
//! 添加新存储后端只需实现 `AsyncStorage` trait,无需修改引擎层枚举。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::{Bytes, BytesMut};

use tachyon_core::DownloadResult;
use tachyon_io::TokioFile;
#[cfg(target_os = "windows")]
use tachyon_io::WinFile;
use tachyon_io::storage::AsyncStorage;

#[cfg(test)]
use tachyon_core::test_harness::harness::MemoryStorage as MemStorage;

// ---------------------------------------------------------------------------
// ErasedStorage: 内部 trait
// ---------------------------------------------------------------------------

pub(crate) trait ErasedStorage: Send + Sync {
    fn write_at_erased(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>>;
    fn write_at_mut_erased(
        &self,
        offset: u64,
        data: BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>>;
    fn read_at_erased<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>>;
    fn allocate_erased(
        &self,
        size: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;
    fn sync_erased(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;
    fn file_size_erased(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>>;
    fn close_erased(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;
}

impl<S: AsyncStorage + 'static> ErasedStorage for S {
    fn write_at_erased(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        self.write_at(offset, data)
    }

    fn write_at_mut_erased(
        &self,
        offset: u64,
        data: BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        self.write_at_mut(offset, data)
    }

    fn read_at_erased<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        self.read_at(offset, buf)
    }

    fn allocate_erased(
        &self,
        size: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        self.allocate(size)
    }

    fn sync_erased(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        self.sync()
    }

    fn file_size_erased(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
        self.file_size()
    }

    fn close_erased(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        self.close()
    }
}

// ---------------------------------------------------------------------------
// DynStorage: 类型擦除存储包装器
// ---------------------------------------------------------------------------

/// 类型擦除存储包装器
///
/// 通过 `Arc<dyn ErasedStorage>` 实现动态分发,添加新存储后端只需
/// 实现 `AsyncStorage` trait,无需修改引擎层枚举定义和 match 分支。
#[derive(Clone)]
pub struct DynStorage(Arc<dyn ErasedStorage>);

impl DynStorage {
    /// 从任意 AsyncStorage 实现创建
    pub fn new<S: AsyncStorage + 'static>(storage: S) -> Self {
        Self(Arc::new(storage))
    }

    /// 显式关闭存储后端
    ///
    /// 确保数据 fsync 和资源释放（轮询线程退出、pending I/O 排空）
    /// 在调用方确定的时机执行,而非依赖 Arc drop 的不确定时机。
    pub async fn close(&self) -> DownloadResult<()> {
        self.0.close_erased().await
    }

    /// 从 Arc 包装的 AsyncStorage 创建
    pub fn from_arc<S: AsyncStorage + 'static>(storage: Arc<S>) -> Self {
        Self(storage)
    }

    /// 打开或创建 TokioFile 存储
    async fn open(path: &std::path::Path) -> DownloadResult<Self> {
        let storage = TokioFile::open(path).await?;
        Ok(Self::new(storage))
    }

    /// 根据 I/O 策略打开存储后端
    ///
    /// - `Standard`: TokioFile（跨平台稳定路径）
    /// - `WinAligned`: WinFile NO_BUFFERING（仅 Windows；其他平台回退到 Standard）
    /// - `Iocp`: IOCP 异步后端（仅 Windows；其他平台回退到 Standard）
    /// - `IoUring`: io_uring 零拷贝后端（仅 Linux 5.4+；其他平台回退到 Standard）
    pub(crate) async fn open_with_strategy(
        path: &std::path::Path,
        strategy: tachyon_core::config::IoStrategy,
    ) -> DownloadResult<Self> {
        match strategy {
            tachyon_core::config::IoStrategy::Standard => Self::open(path).await,
            tachyon_core::config::IoStrategy::WinAligned => {
                #[cfg(target_os = "windows")]
                {
                    tracing::info!(path = %path.display(), "使用 WinFile NO_BUFFERING 后端");
                    let storage = WinFile::open_optimized(path).await?;
                    Ok(Self::new(storage))
                }
                #[cfg(not(target_os = "windows"))]
                {
                    tracing::warn!(
                        path = %path.display(),
                        "WinAligned 策略在非 Windows 平台不可用,回退到 Standard"
                    );
                    Self::open(path).await
                }
            }
            tachyon_core::config::IoStrategy::Iocp => {
                #[cfg(target_os = "windows")]
                {
                    tracing::info!(path = %path.display(), "使用 IOCP 后端");
                    let mut storage = tachyon_io::IoCpStorage::new(path);
                    match storage.init() {
                        Ok(()) => Ok(Self::new(storage)),
                        Err(error) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %error,
                                "IOCP 后端初始化失败,回退到 Standard"
                            );
                            Self::open(path).await
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    tracing::warn!(
                        path = %path.display(),
                        "Iocp 策略在非 Windows 平台不可用,回退到 Standard"
                    );
                    Self::open(path).await
                }
            }
            tachyon_core::config::IoStrategy::IoUring => {
                tracing::info!(path = %path.display(), "使用 io_uring 零拷贝后端");
                let mut storage =
                    tachyon_io::IoUringStorage::new(path, tachyon_io::IoUringConfig::default());
                match storage.init() {
                    Ok(()) => Ok(Self::new(storage)),
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %error,
                            "io_uring 后端初始化失败,回退到 Standard"
                        );
                        Self::open(path).await
                    }
                }
            }
        }
    }

    /// 写入数据到指定偏移
    pub async fn write_at(&self, offset: u64, data: Bytes) -> DownloadResult<usize> {
        self.0.write_at_erased(offset, data).await
    }

    /// 写入 BytesMut 数据（避免 freeze() 产生额外复制）
    pub async fn write_at_mut(&self, offset: u64, data: BytesMut) -> DownloadResult<usize> {
        self.0.write_at_mut_erased(offset, data).await
    }

    /// 从指定偏移读取数据
    pub async fn read_at(&self, offset: u64, buf: &mut [u8]) -> DownloadResult<usize> {
        self.0.read_at_erased(offset, buf).await
    }

    /// 预分配文件空间
    pub async fn allocate(&self, size: u64) -> DownloadResult<()> {
        self.0.allocate_erased(size).await
    }

    /// 同步数据到磁盘
    pub async fn sync(&self) -> DownloadResult<()> {
        self.0.sync_erased().await
    }

    pub async fn file_size(&self) -> DownloadResult<u64> {
        self.0.file_size_erased().await
    }
}

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) struct AsyncMemWrapper(pub(crate) MemStorage);

#[cfg(test)]
impl AsyncStorage for AsyncMemWrapper {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        use tachyon_core::traits::Storage;
        Box::pin(self.0.write_at(offset, data))
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        use tachyon_core::traits::Storage;
        Box::pin(self.0.read_at(offset, buf))
    }

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        use tachyon_core::traits::Storage;
        Box::pin(self.0.sync())
    }

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        use tachyon_core::traits::Storage;
        Box::pin(self.0.allocate(size))
    }

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
        use tachyon_core::traits::Storage;
        Box::pin(self.0.file_size())
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        use tachyon_core::traits::Storage;
        Box::pin(self.0.close())
    }
}

#[cfg(test)]
impl DynStorage {
    pub(crate) fn memory() -> Self {
        Self::new(AsyncMemWrapper(MemStorage::new()))
    }

    pub(crate) fn memory_with_capacity(cap: usize) -> Self {
        Self::new(AsyncMemWrapper(MemStorage::with_capacity(cap)))
    }
}

// FragmentProgress 已移动到 tachyon-core::types,本模块不再定义该类型,
// 相关实现统一通过 tachyon_core::FragmentProgress 引用。
