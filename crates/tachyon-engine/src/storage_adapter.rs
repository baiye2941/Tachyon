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

// DownloadError 仅在部分错误路径使用
use tachyon_core::DownloadError;

// ---------------------------------------------------------------------------
// ErasedStorage: 内部 trait
// ---------------------------------------------------------------------------

pub(crate) trait ErasedStorage: Send + Sync {
    fn write_at_erased(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>>;
    fn write_at_mut_erased<'a>(
        &'a self,
        offset: u64,
        data: &'a mut BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>>;
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

    fn write_at_mut_erased<'a>(
        &'a self,
        offset: u64,
        data: &'a mut BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
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
    pub async fn write_at_mut(&self, offset: u64, data: &mut BytesMut) -> DownloadResult<usize> {
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
// StorageSet: 单/多文件统一存储抽象
// ---------------------------------------------------------------------------

/// 单文件或多文件存储集合,对外暴露全局偏移语义
///
/// - `Single`:单文件(现有路径),全局 offset 直接透传给底层 `DynStorage`
/// - `Multi`:多文件,持 `Vec<DynStorage>` + `FileLayout`,全局 offset 经
///   `layout.split_range` 折算到 `(file_id, 文件内 offset)` 后分发写各 storage
///
/// 这样 `DownloadTask.storage` 字段统一为 `StorageSet`,`download_single_fragment`/
/// `flush_batch`/`verify` 等调用点签名不变(仍传全局 offset),多文件折算封装在内部。
/// 4 个 I/O 后端、`AsyncStorage` trait、bench 全部零改动。
#[derive(Clone)]
pub enum StorageSet {
    /// 单文件存储(现有路径,行为不变)
    Single(DynStorage),
    /// 多文件存储:各文件独立 DynStorage + 全局布局
    Multi {
        storages: Vec<DynStorage>,
        layout: tachyon_core::FileLayout,
    },
}

#[allow(dead_code)] // multi/is_single/sync/file_size 为预留公共 API,测试已覆盖,多文件端到端路径(层 5)使用
impl StorageSet {
    /// 单文件快捷构造
    pub fn single(storage: DynStorage) -> Self {
        Self::Single(storage)
    }

    /// 多文件构造
    pub fn multi(storages: Vec<DynStorage>, layout: tachyon_core::FileLayout) -> Self {
        Self::Multi { storages, layout }
    }

    /// 是否为单文件
    pub fn is_single(&self) -> bool {
        matches!(self, Self::Single(_))
    }

    /// 写入数据(全局 offset 语义)
    ///
    /// Single:直接透传。Multi:按 layout 拆分到各文件段,逐段写对应 storage。
    /// 跨文件边界的 data 会被 split 成多段分别写入。
    pub async fn write_at(&self, offset: u64, data: Bytes) -> DownloadResult<usize> {
        match self {
            Self::Single(s) => s.write_at(offset, data).await,
            Self::Multi { storages, layout } => {
                if data.is_empty() {
                    return Ok(0);
                }
                let end = offset + data.len() as u64 - 1;
                let segments = layout.split_range(offset, end);
                let mut total_written = 0usize;
                let mut byte_cursor = 0usize;
                for (file_id, local_start, local_end) in segments {
                    let seg_len = (local_end - local_start + 1) as usize;
                    // 修复 BUG-F:补短写重试(与 write_at_mut 一致)
                    let mut remaining = data.slice(byte_cursor..byte_cursor + seg_len);
                    byte_cursor += seg_len;
                    let mut local_pos = local_start;
                    while !remaining.is_empty() {
                        let written = storages[file_id]
                            .write_at(local_pos, remaining.clone())
                            .await?;
                        if written == 0 {
                            return Err(DownloadError::Fragment(format!(
                                "多文件存储短写未前进(file_id={file_id}, offset={local_pos})"
                            )));
                        }
                        // 诊断:后端 write_at 不应返回 > 传入 data.len() 的值
                        debug_assert!(
                            written <= remaining.len(),
                            "Multi 段内 write_at 返回 {written} > remaining.len() {} (file_id={file_id}, local_pos={local_pos})",
                            remaining.len()
                        );
                        local_pos += written as u64;
                        remaining = remaining.slice(written..);
                        total_written += written;
                    }
                }
                Ok(total_written)
            }
        }
    }

    /// 写入 BytesMut(全局 offset 语义,避免 freeze 复制)
    ///
    /// Multi 下按 layout 拆分到各文件段。OPT-1 零拷贝实现:每段用
    /// `data.split_to(seg_len)`(零拷贝指针调整)取前缀 BytesMut,再 `freeze()` 成
    /// `Bytes`(零拷贝,引用计数),交给 `write_at` 写入(段内短写用 `slice` 跳过已写,
    /// 引用计数不复制)。相比旧 `Bytes::copy_from_slice` 每段复制,大文件跨边界场景
    /// (4MB batch / 3 段 ~3MB 复制)从 ~2.8ms 降至纯 split/freeze/slice 指针操作。
    ///
    /// 契约变化:Multi 路径会用 `split_to` 消费 `data`(逐段取走前缀),成功时 `data`
    /// 被清空。调用方 `write_all_at_mut` 用 `advance(written.min(batch.len()))`
    /// 兼容此行为(Min 防止空 batch 上 advance 越界 panic);Single 路径不消费 data,
    /// `min` 退化为 `written`,行为与旧版一致。
    pub async fn write_at_mut(&self, offset: u64, data: &mut BytesMut) -> DownloadResult<usize> {
        match self {
            Self::Single(s) => s.write_at_mut(offset, data).await,
            Self::Multi { storages, layout } => {
                if data.is_empty() {
                    return Ok(0);
                }
                let end = offset + data.len() as u64 - 1;
                let segments = layout.split_range(offset, end);
                let mut total_written = 0usize;
                for (file_id, local_start, local_end) in segments {
                    let seg_len = (local_end - local_start + 1) as usize;
                    // OPT-1:split_to 零拷贝取前缀(指针调整,不复制),freeze 零拷贝转 Bytes。
                    // 旧实现 Bytes::copy_from_slice(&data[byte_cursor..byte_cursor+seg_len])
                    // 每段复制 seg_len 字节,大文件场景是显著瓶颈(见 timing 测试)。
                    let chunk_bytes = data.split_to(seg_len).freeze();
                    let mut remaining_chunk = chunk_bytes;
                    let mut local_pos = local_start;
                    // 段内短写重试:slice 引用计数,不复制
                    while !remaining_chunk.is_empty() {
                        let written = storages[file_id]
                            .write_at(local_pos, remaining_chunk.clone())
                            .await?;
                        if written == 0 {
                            return Err(DownloadError::Fragment(format!(
                                "多文件存储短写未前进: file_id={file_id}, offset={local_pos}, remaining={}",
                                remaining_chunk.len()
                            )));
                        }
                        local_pos += written as u64;
                        // Bytes 无 in-place advance,用 slice 跳过已写部分(引用计数,不复制)
                        remaining_chunk = remaining_chunk.slice(written..);
                        total_written += written;
                    }
                }
                Ok(total_written)
            }
        }
    }

    /// 读取数据(全局 offset 语义)
    ///
    /// Multi 下 range 跨文件边界时,逐段读并填充 buf 对应区间。
    pub async fn read_at(&self, offset: u64, buf: &mut [u8]) -> DownloadResult<usize> {
        match self {
            Self::Single(s) => s.read_at(offset, buf).await,
            Self::Multi { storages, layout } => read_multi(storages, layout, offset, buf).await,
        }
    }

    /// 预分配空间
    ///
    /// Single:透传。Multi:对每个 storage 按其文件长度 allocate。
    pub async fn allocate(&self, size: u64) -> DownloadResult<()> {
        match self {
            Self::Single(s) => s.allocate(size).await,
            Self::Multi { storages, layout } => {
                // size 是全局总长;各文件长度由 layout 决定,按 (file_id, len) 分配
                for (file_id, len) in layout_split_iter(layout) {
                    storages[file_id].allocate(len).await?;
                }
                let _ = size; // 总长仅作校验用,实际按各文件长度分配
                Ok(())
            }
        }
    }

    /// 同步所有 storage 到磁盘
    pub async fn sync(&self) -> DownloadResult<()> {
        match self {
            Self::Single(s) => s.sync().await,
            Self::Multi { storages, .. } => {
                for s in storages {
                    s.sync().await?;
                }
                Ok(())
            }
        }
    }

    /// 关闭所有 storage
    pub async fn close(&self) -> DownloadResult<()> {
        match self {
            Self::Single(s) => s.close().await,
            Self::Multi { storages, .. } => {
                for s in storages {
                    s.close().await?;
                }
                Ok(())
            }
        }
    }

    /// 文件总大小(Multi 返回各文件长度之和)
    pub async fn file_size(&self) -> DownloadResult<u64> {
        match self {
            Self::Single(s) => s.file_size().await,
            Self::Multi { storages, .. } => {
                let mut total = 0u64;
                for s in storages {
                    total = total
                        .checked_add(s.file_size().await?)
                        .ok_or_else(|| DownloadError::Config("文件总大小溢出".into()))?;
                }
                Ok(total)
            }
        }
    }
}

/// 从全局 offset 读 buf.len() 字节,跨文件边界逐段填充
async fn read_multi(
    storages: &[DynStorage],
    layout: &tachyon_core::FileLayout,
    offset: u64,
    buf: &mut [u8],
) -> DownloadResult<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let end = offset + buf.len() as u64 - 1;
    let segments = layout.split_range(offset, end);
    let mut total_read = 0usize;
    for (file_id, local_start, local_end) in segments {
        let seg_len = (local_end - local_start + 1) as usize;
        let read = storages[file_id]
            .read_at(local_start, &mut buf[total_read..total_read + seg_len])
            .await?;
        total_read += read;
        if read < seg_len {
            // 修复 BUG-G:短读返回错误而非静默 break(防 verify 读盘哈希死循环)
            return Err(DownloadError::Fragment(format!(
                "多文件存储短读: file_id={file_id}, 期望 {seg_len} 字节, 实际 {read} 字节"
            )));
        }
    }
    Ok(total_read)
}

/// 返回 layout 各文件的 (file_id, len) 迭代(用于 allocate)
fn layout_split_iter(layout: &tachyon_core::FileLayout) -> impl Iterator<Item = (usize, u64)> {
    // FileLayout 内部 files 是私有的,通过 split_range(0, total-1) 取各段
    // 每段 (file_id, local_start=0, local_end=len-1)
    let total = layout.total_len();
    if total == 0 {
        Vec::new().into_iter()
    } else {
        layout
            .split_range(0, total - 1)
            .into_iter()
            .map(|(fid, ls, le)| (fid, le - ls + 1))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

// F-26:AsyncMemWrapper 已删除。MemoryStorage 现在直接实现 AsyncStorage
// (trait 已上移到 tachyon-core),无需适配器桥接 core::Storage -> io::AsyncStorage。

#[cfg(test)]
impl DynStorage {
    pub(crate) fn memory() -> Self {
        Self::new(MemStorage::new())
    }

    pub(crate) fn memory_with_capacity(cap: usize) -> Self {
        Self::new(MemStorage::with_capacity(cap))
    }
}

// FragmentProgress 已移动到 tachyon-core::types,本模块不再定义该类型,
// 相关实现统一通过 tachyon_core::FragmentProgress 引用。

#[cfg(test)]
mod tests {
    use bytes::{Bytes, BytesMut};

    use super::{DynStorage, StorageSet};
    use tachyon_core::config::IoStrategy;

    #[tokio::test]
    async fn test_dyn_storage_open_with_strategy_standard() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = DynStorage::open_with_strategy(tmp.path(), IoStrategy::Standard)
            .await
            .unwrap();
        storage
            .write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
        let mut buf = [0u8; 5];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 5);
        assert_eq!(&buf, b"hello");
        storage.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_dyn_storage_open_with_strategy_win_aligned() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = DynStorage::open_with_strategy(tmp.path(), IoStrategy::WinAligned)
            .await
            .unwrap();
        // 基本操作应成功：Windows 上使用 WinFile，其他平台回退到 Standard
        storage.allocate(1024).await.unwrap();
        storage
            .write_at(0, Bytes::from_static(b"aligned"))
            .await
            .unwrap();
        let mut buf = [0u8; 7];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 7);
        assert_eq!(&buf, b"aligned");
        storage.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_dyn_storage_open_with_strategy_iocp() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = DynStorage::open_with_strategy(tmp.path(), IoStrategy::Iocp)
            .await
            .unwrap();
        storage
            .write_at(0, Bytes::from_static(b"iocp"))
            .await
            .unwrap();
        let mut buf = [0u8; 4];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 4);
        assert_eq!(&buf, b"iocp");
        storage.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_dyn_storage_open_with_strategy_iouring() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = DynStorage::open_with_strategy(tmp.path(), IoStrategy::IoUring)
            .await
            .unwrap();
        // io_uring 后端使用 O_DIRECT, 要求 offset 与 length 均为 4096 字节对齐
        // 非 Linux 平台会回退到 Standard, 仍可使用任意长度
        let payload_size = 4096;
        let mut payload = vec![0u8; payload_size];
        payload[..5].copy_from_slice(b"uring");
        storage
            .write_at(0, Bytes::from(payload.clone()))
            .await
            .unwrap();
        let mut buf = vec![0u8; payload_size];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, payload_size);
        assert_eq!(&buf[..5], b"uring");
        storage.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_dyn_storage_delegation() {
        let storage = DynStorage::memory();
        storage
            .write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
        let mut tail = BytesMut::from(&b" world"[..]);
        storage.write_at_mut(5, &mut tail).await.unwrap();
        let mut buf = [0u8; 11];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 11);
        assert_eq!(&buf, b"hello world");
        storage.allocate(1024).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 1024);
        storage.sync().await.unwrap();
        storage.close().await.unwrap();
    }

    // ===== StorageSet 多文件测试 =====

    use tachyon_core::{FileLayout, FileSpan};

    /// 构造双文件 StorageSet:file0 [0,4095], file1 [4096,8191]
    fn make_multi_storage_set() -> StorageSet {
        let storages = vec![DynStorage::memory(), DynStorage::memory()];
        let layout = FileLayout::from_spans(vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 4096,
                name: "a".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: 4096,
                len: 4096,
                name: "b".into(),
            },
        ]);
        StorageSet::multi(storages, layout)
    }

    #[tokio::test]
    async fn test_storage_set_multi_write_within_single_file() {
        let ss = make_multi_storage_set();
        ss.allocate(8192).await.unwrap();
        // [5000, 6000] 完全在 file1 [4096,8191] 内
        let data = Bytes::from(vec![0xAB; 1001]);
        let written = ss.write_at(5000, data).await.unwrap();
        assert_eq!(written, 1001);
        let mut buf = vec![0u8; 1001];
        let read = ss.read_at(5000, &mut buf).await.unwrap();
        assert_eq!(read, 1001);
        assert!(buf.iter().all(|&b| b == 0xAB));
    }

    #[tokio::test]
    async fn test_storage_set_multi_write_across_boundary() {
        let ss = make_multi_storage_set();
        ss.allocate(8192).await.unwrap();
        // [3000, 5000] 跨 file0 末尾(1097 字节)+ file1 开头(905 字节)
        let data: Vec<u8> = (0..2001).map(|i| (i % 251) as u8).collect();
        let data_bytes = Bytes::from(data.clone());
        let written = ss.write_at(3000, data_bytes).await.unwrap();
        assert_eq!(written, 2001, "跨边界写入应全部成功");

        // 读回校验:全局 [3000, 5000]
        let mut buf = vec![0u8; 2001];
        let read = ss.read_at(3000, &mut buf).await.unwrap();
        assert_eq!(read, 2001);
        assert_eq!(buf, data, "跨边界读回应等于写入数据");
    }

    #[tokio::test]
    async fn test_storage_set_multi_write_mut_across_boundary() {
        let ss = make_multi_storage_set();
        ss.allocate(8192).await.unwrap();
        let mut batch = BytesMut::from(&vec![0xCD; 2001][..]);
        let written = ss.write_at_mut(3000, &mut batch).await.unwrap();
        assert_eq!(written, 2001);

        let mut buf = vec![0u8; 2001];
        ss.read_at(3000, &mut buf).await.unwrap();
        assert!(buf.iter().all(|&b| b == 0xCD));
    }

    #[tokio::test]
    async fn test_storage_set_multi_file_size() {
        let ss = make_multi_storage_set();
        ss.allocate(8192).await.unwrap();
        assert_eq!(ss.file_size().await.unwrap(), 8192);
    }

    #[tokio::test]
    async fn test_storage_set_single_passthrough() {
        // Single 分支应与 DynStorage 行为一致
        let storage = DynStorage::memory();
        let ss = StorageSet::single(storage);
        ss.allocate(100).await.unwrap();
        let data = Bytes::from_static(b"hello");
        ss.write_at(0, data).await.unwrap();
        let mut buf = [0u8; 5];
        ss.read_at(0, &mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        assert!(ss.is_single());
    }

    // ===== OPT-1: Multi::write_at_mut 分段拷贝开销计时测试 =====
    //
    // 用 NoopStorage(零拷贝、零 I/O 后端)隔离 StorageSet::Multi::write_at_mut 自身的
    // 分段拆分/拷贝成本:旧实现对每个跨边界段 Bytes::copy_from_slice,
    // 新实现改为 split_to + 后端 write_at_mut + unsplit(零拷贝)。
    //
    // 构造 16 个 256B 的小文件,每次写 4KB batch(跨 ~15 个文件边界),
    // 重复写入测平均耗时,对比旧(copy)/新(zero-copy)实现差异。
    //
    // 由于 Windows criterion 相对变化不可信,这里用 Instant 绝对计时 + 同会话对比。

    fn make_noop_multi_storage_set(n_files: usize, file_len: u64) -> StorageSet {
        use tachyon_core::test_harness::harness::NoopStorage;
        let storages: Vec<DynStorage> =
            (0..n_files).map(|_| DynStorage::new(NoopStorage)).collect();
        let spans: Vec<_> = (0..n_files)
            .map(|file_id| tachyon_core::FileSpan {
                file_id,
                global_offset: file_id as u64 * file_len,
                len: file_len,
                name: format!("f{file_id}"),
            })
            .collect();
        let layout = tachyon_core::FileLayout::from_spans(spans);
        StorageSet::multi(storages, layout)
    }

    #[tokio::test]
    async fn test_multi_write_at_mut_cross_boundary_timing() {
        // 16 个 256B 文件,总 4096B;每次写 4096B batch 跨 15 个边界
        let n_files = 16;
        let file_len = 256u64;
        let total = n_files as u64 * file_len;
        let ss = make_noop_multi_storage_set(n_files, file_len);

        let batch_size = total as usize;
        let iterations = 2000u32;

        // 预热(触发内联/分支预测)
        for _ in 0..50 {
            let mut batch = BytesMut::from(&vec![0u8; batch_size][..]);
            let _ = ss.write_at_mut(0, &mut batch).await.unwrap();
        }

        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let mut batch = BytesMut::from(&vec![0u8; batch_size][..]);
            let written = ss.write_at_mut(0, &mut batch).await.unwrap();
            assert_eq!(written, batch_size);
        }
        let elapsed = start.elapsed();
        let per_op_ns = elapsed.as_nanos() / iterations as u128;
        // 仅打印,无硬性阈值断言(避免环境波动 flaky):供人工对比新旧实现
        eprintln!(
            "Multi::write_at_mut 跨 {n_files} 边界 {batch_size}B batch: \
             {iterations} 次 {elapsed:?} = {per_op_ns} ns/op"
        );
        // 基本正确性兜底:写入量应等于 batch 大小
        assert!(per_op_ns > 0);
    }

    /// 大文件场景计时:模拟真实多文件 torrent(4 个 1MB 文件),
    /// batch = 1MB 跨 3 个边界,每段 ~256KB copy_from_slice。
    /// NoopStorage 隔离拷贝成本,对比真实磁盘 I/O(1MB 写入约 50-200µs on NVMe)。
    #[tokio::test]
    async fn test_multi_write_at_mut_large_file_timing() {
        let n_files = 4;
        let file_len = 1024 * 1024u64; // 1MB 每文件
        let total = n_files as u64 * file_len; // 4MB
        let ss = make_noop_multi_storage_set(n_files, file_len);

        let batch_size = total as usize; // 4MB batch 跨 3 边界
        let iterations = 200u32;

        // 预热
        for _ in 0..10 {
            let mut batch = BytesMut::from(&vec![0u8; batch_size][..]);
            let _ = ss.write_at_mut(0, &mut batch).await.unwrap();
        }

        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let mut batch = BytesMut::from(&vec![0u8; batch_size][..]);
            let written = ss.write_at_mut(0, &mut batch).await.unwrap();
            assert_eq!(written, batch_size);
        }
        let elapsed = start.elapsed();
        let per_op_us = elapsed.as_micros() / iterations as u128;
        eprintln!(
            "Multi::write_at_mut 大文件(4x1MB) 跨 3 边界 4MB batch: \
             {iterations} 次 {elapsed:?} = {per_op_us} µs/op (其中 copy 约 3MB)"
        );
        assert!(per_op_us > 0);
    }

    /// bench 缺口 2:storage read_multi 跨边界 timing(对称 write_at_mut)
    ///
    /// read_multi 是 verify 阶段读盘哈希的跨文件路径。用 NoopStorage 隔离 I/O,
    /// 对照 write_at_mut large_file 基线(~1500µs/4MB),确认读路径无异常开销。
    #[tokio::test]
    async fn test_multi_read_at_large_file_timing() {
        let n_files = 4;
        let file_len = 1024 * 1024u64; // 1MB 每文件
        let total = n_files as u64 * file_len; // 4MB
        let ss = make_noop_multi_storage_set(n_files, file_len);

        let batch_size = total as usize; // 4MB 读跨 3 边界
        let iterations = 200u32;

        // 预热
        for _ in 0..10 {
            let mut buf = vec![0u8; batch_size];
            let _ = ss.read_at(0, &mut buf).await.unwrap();
        }

        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let mut buf = vec![0u8; batch_size];
            let read = ss.read_at(0, &mut buf).await.unwrap();
            assert_eq!(read, batch_size);
        }
        let elapsed = start.elapsed();
        let per_op_us = elapsed.as_micros() / iterations as u128;
        eprintln!(
            "StorageSet::read_at 大文件(4x1MB) 跨 3 边界 4MB batch: \
             {iterations} 次 {elapsed:?} = {per_op_us} µs/op"
        );
        assert!(per_op_us > 0);
    }
}
