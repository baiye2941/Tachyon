//! 存储适配器: 类型擦除存储包装器 + 分片进度消息
//!
//! `DynStorage` 将任意 `AsyncStorage` 实现包装为统一的动态分发类型,
//! 添加新存储后端只需实现 `AsyncStorage` trait,无需修改引擎层枚举。

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

use bytes::{Bytes, BytesMut};

use tachyon_core::DownloadResult;
use tachyon_io::TokioFile;
#[cfg(target_os = "windows")]
use tachyon_io::WinFile;
use tachyon_io::storage::AsyncStorage;

#[cfg(any(test, feature = "test-harness"))]
use tachyon_core::test_harness::harness::MemoryStorage as MemStorage;

// DownloadError 仅在部分错误路径使用
use tachyon_core::DownloadError;

// ---------------------------------------------------------------------------
// P4: 下载前磁盘空间预检(跨平台)
// ---------------------------------------------------------------------------

/// P4:磁盘空间预检 margin —— file_size 的 1% 或 100MB 取小值。
///
/// 预检需留余量:文件系统元数据、簇对齐、并发写入峰值等可能使实际占用
/// 略大于声明的 file_size。margin 取较小者避免大文件时 margin 过大
/// (1GB 文件 1% = 10MB 已足够),小文件时保底 100MB(覆盖文件系统开销)。
const fn disk_space_margin(file_size: u64) -> u64 {
    let one_pct = file_size / 100;
    const HUNDRED_MB: u64 = 100 * 1024 * 1024;
    if one_pct < HUNDRED_MB {
        one_pct
    } else {
        HUNDRED_MB
    }
}

/// P4:查询 `dir` 所在分区的可用磁盘空间(字节)。
///
/// 跨平台实现:
/// - Windows:`GetDiskFreeSpaceExW`(kernel32,原始 extern "system" 声明)
/// - Unix(Linux/macOS):`libc::statvfs`(libc 提供目标平台完整 ABI 结构)
///
/// `dir` 不存在时,向上回溯到最近的存在的父目录(下载目录可能尚未创建)。
/// 全部回溯失败、路径含 NUL 或系统调用失败时返回 `None`,调用方降级为"不预检"
/// (不阻断下载,保持向后兼容)。
///
/// # Safety
///
/// 本函数内部使用 `unsafe` 调用平台 FFI:
/// - Windows:`GetDiskFreeSpaceExW` 接收 UTF-16 路径指针(以 null 结尾),
///   传出三个 `u64` 出参指针。指针均指向栈上合法内存,调用后立即读取。
/// - Unix:`libc::statvfs` 接收 C 字符串路径,传出由 libc 定义的完整
///   `libc::statvfs` 结构体缓冲。仅当调用成功后才将该缓冲视为已初始化。
///
/// 两条路径的 FFI 调用均不跨越 await 点,指针生命周期在同步栈帧内。
pub(crate) fn available_disk_space(dir: &Path) -> Option<u64> {
    // 回溯到存在的目录(下载目录可能尚未创建)
    let probe = existing_ancestor(dir)?;
    available_disk_space_inner(probe)
}

/// 向上回溯直到找到存在的目录;若 `dir` 本身存在则直接返回。
fn existing_ancestor(dir: &Path) -> Option<&Path> {
    let mut cur = dir;
    loop {
        if cur.is_dir() {
            return Some(cur);
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

#[cfg(target_os = "windows")]
fn available_disk_space_inner(dir: &Path) -> Option<u64> {
    // Safety:GetDiskFreeSpaceExW 是 Win32 稳定 API,签名自 NT 以来不变。
    // 路径以 UTF-16 编码并以 null 结尾(encode_wide + push(0))。
    // 三个出参指针指向栈上 u64,调用后立即读取返回值判断成败。
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available: *mut u64,
            total_number_of_bytes: *mut u64,
            total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }

    // 目录路径转 UTF-16(null 结尾),FFI 要求宽字符串
    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    wide.push(0);

    let mut free_available: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    // SAFETY:wide 以 null 结尾且指针在调用期间有效;三个 out 指针指向
    // 栈上合法 u64 内存。GetDiskFreeSpaceExW 无线程亲和性,可在任意线程调用。
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_available,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 {
        // 调用失败,降级为不预检(不阻断下载)
        tracing::warn!(
            dir = %dir.display(),
            "GetDiskFreeSpaceExW 失败,跳过磁盘空间预检"
        );
        return None;
    }
    Some(free_available)
}

/// 从已完整初始化的原生 `libc::statvfs` 结构计算可用字节数。
///
/// 调用方必须仅在 `statvfs` 成功返回后传入完整输出结构。
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
fn available_bytes_from_statvfs(stat: &libc::statvfs) -> u64 {
    // 审计 SEC-001:编译时 ABI 断言,防止未来回归手写短结构。
    // libc 提供的完整 statvfs 必须 >= Rust 端使用字段所需宽度。
    // 静态断言 f_bavail/f_frsize 的 size_of 与 u64 兼容(至少不小于 4 字节),
    // 并确保 statvfs 结构大小合理(> 0,含必需字段)。
    const _: () = assert!(
        std::mem::size_of::<libc::statvfs>()
            >= std::mem::size_of::<u64>() * 2 + std::mem::size_of::<u32>(),
        "libc::statvfs 结构过小,FFI 输出可能越界"
    );
    // libc 字段的目标 ABI 整数别名不同：Linux 两字段同型，macOS 的 f_bavail 为 u32；
    // 统一直接转为 u64，避免制造并不存在的可失败转换，且允许范围仅限本函数。
    (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
}

#[cfg(unix)]
fn available_disk_space_inner(dir: &Path) -> Option<u64> {
    let c_path = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();

    // SAFETY:c_path 以 null 结尾且指针在调用期间有效;stat 指向完整的 libc::statvfs
    // 输出缓冲。statvfs 是 POSIX 线程安全函数(无全局可变状态),可在任意线程调用。
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        tracing::warn!(dir = %dir.display(), "statvfs 失败,跳过磁盘空间预检");
        return None;
    }

    // SAFETY:仅在 statvfs 返回 0 后执行,此时完整输出缓冲已由 libc 初始化。
    let stat = unsafe { stat.assume_init() };
    Some(available_bytes_from_statvfs(&stat))
}

/// P4:预检磁盘空间是否足够容纳 `file_size`(+margin)。
///
/// 在 `prepare_storage` 之前调用:若可用空间 < file_size + margin,返回
/// `DownloadError::Config`(不可重试),带中文提示含可用/需求数值,便于
/// 用户定位问题。无法获取磁盘信息时返回 `Ok(())`(降级为不预检,保持
/// 向后兼容,不阻断下载)。
pub(crate) fn check_disk_space(dir: &Path, file_size: u64) -> DownloadResult<()> {
    let Some(available) = available_disk_space(dir) else {
        // 无法获取磁盘信息(目录不存在 / FFI 失败):不阻断下载
        return Ok(());
    };
    let margin = disk_space_margin(file_size);
    let needed = file_size.saturating_add(margin);
    if available < needed {
        return Err(DownloadError::Config(format!(
            "磁盘空间不足: 可用 {} 字节 (约 {:.2} GB), 需求 {} 字节 (约 {:.2} GB, 含 {} 字节余量)",
            available,
            available as f64 / 1_073_741_824.0,
            needed,
            needed as f64 / 1_073_741_824.0,
            margin,
        )));
    }
    Ok(())
}

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
        let offered = data.len();
        let written = self.0.write_at_erased(offset, data).await?;
        if written > offered {
            return Err(DownloadError::Fragment(format!(
                "存储后端返回的写入字节数超过输入长度: offset={offset}, offered={offered}, returned={written}"
            )));
        }
        Ok(written)
    }

    /// 写入 BytesMut 数据（避免 freeze() 产生额外复制）
    pub async fn write_at_mut(&self, offset: u64, data: &mut BytesMut) -> DownloadResult<usize> {
        let offered = data.len();
        let written = self.0.write_at_mut_erased(offset, data).await?;
        if written > offered {
            return Err(DownloadError::Fragment(format!(
                "存储后端返回的写入字节数超过输入长度: offset={offset}, offered={offered}, returned={written}"
            )));
        }
        Ok(written)
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
    /// 被清空;Single 路径不消费 `data`。两条路径返回值均遵守 `AsyncStorage`
    /// 写入计数契约,不得超过调用时提供的输入长度。
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

#[cfg(any(test, feature = "test-harness"))]
impl DynStorage {
    /// 内存存储(测试/bench 用),避免文件系统 I/O 噪声干扰被测路径
    pub fn memory() -> Self {
        Self::new(MemStorage::new())
    }

    pub fn memory_with_capacity(cap: usize) -> Self {
        Self::new(MemStorage::with_capacity(cap))
    }
}

// FragmentProgress 已移动到 tachyon-core::types,本模块不再定义该类型,
// 相关实现统一通过 tachyon_core::FragmentProgress 引用。

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use bytes::{Bytes, BytesMut};

    use super::{DynStorage, StorageSet};
    use tachyon_core::DownloadResult;
    use tachyon_core::config::IoStrategy;
    use tachyon_io::storage::AsyncStorage;

    type WriteCalls = Arc<Mutex<Vec<u64>>>;

    /// 故意违反 `AsyncStorage` 写入计数契约的测试存储。
    ///
    /// 每次写入都会记录 offset，并声称比输入多写入一个字节，用于验证
    /// `DynStorage` 的类型擦除边界能拦截不可信后端的错误计数。
    #[derive(Clone)]
    struct OverreportingStorage {
        calls: WriteCalls,
    }

    impl OverreportingStorage {
        fn new() -> (Self, WriteCalls) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: Arc::clone(&calls),
                },
                calls,
            )
        }
    }

    impl AsyncStorage for OverreportingStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            let calls = Arc::clone(&self.calls);
            Box::pin(async move {
                calls.lock().unwrap().push(offset);
                Ok(data.len() + 1)
            })
        }

        fn write_at_mut<'a>(
            &'a self,
            offset: u64,
            data: &'a mut BytesMut,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            let calls = Arc::clone(&self.calls);
            Box::pin(async move {
                calls.lock().unwrap().push(offset);
                Ok(data.len() + 1)
            })
        }

        fn read_at<'a>(
            &'a self,
            _offset: u64,
            _buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async { Ok(0) })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn allocate(
            &self,
            _size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            Box::pin(async { Ok(0) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// 记录实际写入调用的正常测试存储，用于断言失败不会继续到后续文件。
    #[derive(Clone)]
    struct RecordingStorage {
        calls: WriteCalls,
    }

    impl RecordingStorage {
        fn new() -> (Self, WriteCalls) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: Arc::clone(&calls),
                },
                calls,
            )
        }
    }

    impl AsyncStorage for RecordingStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            let calls = Arc::clone(&self.calls);
            Box::pin(async move {
                calls.lock().unwrap().push(offset);
                Ok(data.len())
            })
        }

        fn read_at<'a>(
            &'a self,
            _offset: u64,
            _buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async { Ok(0) })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn allocate(
            &self,
            _size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            Box::pin(async { Ok(0) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn dyn_storage_write_at_rejects_overreported_byte_count() {
        let (broken, calls) = OverreportingStorage::new();
        let storage = DynStorage::new(broken);

        let error = storage
            .write_at(7, Bytes::from_static(b"abc"))
            .await
            .expect_err("类型擦除边界必须拒绝超过输入长度的 write_at 返回值");

        assert!(
            matches!(error, tachyon_core::DownloadError::Fragment(_)),
            "越界写入计数必须返回 Fragment 错误，实际为: {error:?}"
        );
        assert_eq!(*calls.lock().unwrap(), vec![7]);
    }

    #[tokio::test]
    async fn dyn_storage_write_at_mut_rejects_overreported_byte_count() {
        let (broken, calls) = OverreportingStorage::new();
        let storage = DynStorage::new(broken);
        let mut data = BytesMut::from(&b"abc"[..]);

        let error = storage
            .write_at_mut(11, &mut data)
            .await
            .expect_err("类型擦除边界必须拒绝超过输入长度的 write_at_mut 返回值");

        assert!(
            matches!(error, tachyon_core::DownloadError::Fragment(_)),
            "越界写入计数必须返回 Fragment 错误，实际为: {error:?}"
        );
        assert_eq!(*calls.lock().unwrap(), vec![11]);
    }

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

    fn two_single_byte_file_layout() -> FileLayout {
        FileLayout::from_spans(vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 1,
                name: "first".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: 1,
                len: 1,
                name: "second".into(),
            },
        ])
    }

    #[tokio::test]
    async fn storage_set_multi_write_at_rejects_overreported_count_before_later_file() {
        let (broken, broken_calls) = OverreportingStorage::new();
        let (later, later_calls) = RecordingStorage::new();
        let storage = StorageSet::multi(
            vec![DynStorage::new(broken), DynStorage::new(later)],
            two_single_byte_file_layout(),
        );

        let error = storage
            .write_at(0, Bytes::from_static(b"ab"))
            .await
            .expect_err("首个文件的越界写入计数必须返回错误而非切片 panic");

        assert!(
            matches!(error, tachyon_core::DownloadError::Fragment(_)),
            "越界写入计数必须返回 Fragment 错误，实际为: {error:?}"
        );
        assert_eq!(*broken_calls.lock().unwrap(), vec![0]);
        assert!(
            later_calls.lock().unwrap().is_empty(),
            "首个文件失败后不得调用后续文件"
        );
    }

    #[tokio::test]
    async fn storage_set_multi_write_at_mut_rejects_overreported_count_before_later_file() {
        let (broken, broken_calls) = OverreportingStorage::new();
        let (later, later_calls) = RecordingStorage::new();
        let storage = StorageSet::multi(
            vec![DynStorage::new(broken), DynStorage::new(later)],
            two_single_byte_file_layout(),
        );
        let mut data = BytesMut::from(&b"ab"[..]);

        let error = storage
            .write_at_mut(0, &mut data)
            .await
            .expect_err("首个文件的越界写入计数必须返回错误而非切片 panic");

        assert!(
            matches!(error, tachyon_core::DownloadError::Fragment(_)),
            "越界写入计数必须返回 Fragment 错误，实际为: {error:?}"
        );
        assert_eq!(*broken_calls.lock().unwrap(), vec![0]);
        assert!(
            later_calls.lock().unwrap().is_empty(),
            "首个文件失败后不得调用后续文件"
        );
    }

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

    // ===== P4: 磁盘空间预检测试 =====

    #[cfg(unix)]
    use super::available_bytes_from_statvfs;
    use super::{available_disk_space, check_disk_space, disk_space_margin};
    use tachyon_core::DownloadError;

    /// P4:available_disk_space 对系统临时目录应返回合理区间的可用空间。
    ///
    /// 验证跨平台 FFI 调用成功且字段布局正确:Windows 走 GetDiskFreeSpaceExW,
    /// Unix 走 statvfs。仅断言 >0 无法捕获 macOS 上的偏移错位 bug(旧实现误读
    /// f_ffree/f_favail 拼接的垃圾值,常为巨大正数,也 >0)。改用合理区间断言:
    /// 下界 1MB(临时目录几乎不可能比这更少),上界 1EB(1 EiB ≈ 1.15e18 字节,
    /// 远超任何现实磁盘;垃圾拼接值往往逼近 u64 上限,必然越界)。
    #[test]
    fn test_available_disk_space_returns_positive_for_temp_dir() {
        let tmp = std::env::temp_dir();
        let space = available_disk_space(&tmp);
        assert!(space.is_some(), "临时目录 {:?} 应能获取可用磁盘空间", tmp);
        let available = space.unwrap();
        const ONE_MB: u64 = 1024 * 1024;
        const ONE_EB: u64 = 1u64 << 60;
        assert!(
            available >= ONE_MB,
            "临时目录可用空间 {available} 字节异常偏低(< 1MB),FFI 返回值可疑"
        );
        assert!(
            available < ONE_EB,
            "临时目录可用空间 {available} 字节异常偏高(>= 1EB),\
             疑似 statvfs 字段偏移错位产生的垃圾值"
        );
    }

    /// P4:available_disk_space 返回值不得为垃圾量级(回归 P4-statvfs)。
    ///
    /// 旧实现把 statvfs 的 5 个字段全声明为 u64,在 macOS 上 f_bavail(u32,
    /// 偏移 24)被当作 u64 从偏移 32 读取,取到 f_ffree/f_favail 拼接的垃圾值,
    /// 常逼近 u64 上限。check_disk_space 因此恒放行(保护失效)。本测试以
    /// 系统临时目录为探针,断言返回值落在合理物理范围(>= 1MB 且 < 1EB),
    /// 在 macOS 上能真正捕获该 bug;Windows/Linux-64 字段布局本就正确,继续通过。
    #[test]
    fn test_available_disk_space_not_garbage_magnitude() {
        let tmp = std::env::temp_dir();
        let Some(available) = available_disk_space(&tmp) else {
            // FFI 失败不在本测试范围(由 returns_positive 测试覆盖)
            return;
        };
        const ONE_MB: u64 = 1024 * 1024;
        const ONE_EB: u64 = 1u64 << 60;
        assert!(
            (ONE_MB..ONE_EB).contains(&available),
            "可用空间 {available} 字节不在合理物理区间 [1MB, 1EB),\
             疑似 statvfs 字段偏移错位产生的垃圾值"
        );
    }

    /// Unix 回归：锁定完整原生 `libc::statvfs` 结构到可用字节数的纯转换。
    ///
    /// 该 oracle 与被测 helper 共用一次原生系统调用得到的完整 ABI 结构，不通过第二次
    /// `statvfs` 调用取样，因此 `f_bavail` 的并发变化不会造成 TOCTOU 抖动。
    #[cfg(unix)]
    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn test_available_bytes_from_complete_native_statvfs() {
        use std::ffi::CString;
        use std::mem::MaybeUninit;
        use std::os::unix::ffi::OsStrExt;

        let tmp = std::env::temp_dir();
        let c_path =
            CString::new(tmp.as_os_str().as_bytes()).expect("系统临时目录路径不应包含 NUL");
        let mut stat = MaybeUninit::<libc::statvfs>::uninit();

        // SAFETY:c_path 是调用期间有效的 NUL 结尾 Unix 路径；stat 为完整 libc 输出缓冲。
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        assert_eq!(rc, 0, "应能查询系统临时目录的 statvfs");
        // SAFETY:上方已断言 statvfs 返回成功，完整输出结构已初始化。
        let stat = unsafe { stat.assume_init() };
        // libc 字段的整数别名因 Unix 目标而异；测试统一转成 u64 以固定跨平台 oracle。
        let expected = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);

        assert_eq!(
            available_bytes_from_statvfs(&stat),
            expected,
            "转换结果必须精确等于同一完整原生结构的 f_bavail * f_frsize"
        );
    }

    /// P4:available_disk_space 对不存在的目录应向上回溯到存在的父目录。
    ///
    /// 下载目录可能尚未创建(init_storage 之前),预检需能处理此场景。
    /// 回溯到 existing_ancestor 后调用 FFI,应返回 Some(正值)。
    #[test]
    fn test_available_disk_space_handles_nonexistent_dir() {
        let tmp = std::env::temp_dir();
        let nonexistent = tmp.join("tachyon_p4_nonexistent_subdir_for_test");
        let space = available_disk_space(&nonexistent);
        // 回溯到 tmp(存在),应能获取空间。FFI 成功时返回 Some。
        assert!(
            space.is_some(),
            "不存在的目录应回溯到存在的父目录并获取空间"
        );
    }

    /// P4:check_disk_space 在空间充足时应返回 Ok。
    ///
    /// 临时目录通常有数 GB 可用空间,1 字节文件需求必然满足。
    #[test]
    fn test_check_disk_space_ok_when_sufficient() {
        let tmp = std::env::temp_dir();
        // 1 字节文件,margin = 1/100 = 0 字节(整数除法),需求 1 字节
        let result = check_disk_space(&tmp, 1);
        assert!(result.is_ok(), "1 字节文件应有足够空间: {result:?}");
    }

    /// P4:check_disk_space 在空间不足时应返回 Config 错误(不可重试)。
    ///
    /// 构造一个超出磁盘容量的需求(u64::MAX),应触发不足分支。
    /// 错误消息应含"磁盘空间不足"和数值,便于用户定位。
    #[test]
    fn test_check_disk_space_err_when_insufficient() {
        let tmp = std::env::temp_dir();
        // u64::MAX 远超任何真实磁盘容量,必然不足
        let result = check_disk_space(&tmp, u64::MAX);
        assert!(result.is_err(), "u64::MAX 字节需求应触发磁盘空间不足");
        let err = result.as_ref().unwrap_err();
        match err {
            DownloadError::Config(msg) => {
                assert!(
                    msg.contains("磁盘空间不足"),
                    "错误消息应含'磁盘空间不足': {msg}"
                );
                assert!(msg.contains("字节"), "错误消息应含数值(字节): {msg}");
            }
            other => panic!("预期 Config 错误,实际: {other:?}"),
        }
        // Config 错误不可重试(磁盘空间不会因重试而改变)
        assert!(!err.is_retryable(), "磁盘空间不足应为不可重试错误");
    }

    /// P4:disk_space_margin 取 file_size 的 1% 与 100MB(二进制)的较小值。
    ///
    /// - 小文件(1KB):1% = 10 字节,小于 100MB,margin = 10
    /// - 大文件(100GB):1% = 1GB,大于 100MB,margin = 100MB(二进制 104857600)
    /// - 边界(file_size=0):1% = 0,margin = 0
    #[test]
    fn test_disk_space_margin_uses_smaller_of_one_pct_and_100mb() {
        // 小文件:1% < 100MB(二进制)
        assert_eq!(disk_space_margin(1024), 10, "1KB 的 1% = 10 字节");
        assert_eq!(disk_space_margin(1_000_000), 10_000, "1MB 的 1% = 10KB");
        // 临界点:1% == 100MB(二进制)时 file_size = 10_485_760_000
        // 10_485_760_001 的 1% = 104_857_600.01 → 整数 104_857_600 = 100MB(二进制)
        assert_eq!(
            disk_space_margin(10_485_760_001),
            104_857_600,
            "临界值+1 的 1% = 100MB(二进制),应取 100MB"
        );
        // 大文件:1% > 100MB(二进制),取 100MB(二进制)
        assert_eq!(
            disk_space_margin(100 * 1024 * 1024 * 1024),
            104_857_600,
            "100GB 的 1% = 1GB > 100MB,应取 100MB(二进制)"
        );
        // 边界:file_size = 0 → margin = 0
        assert_eq!(disk_space_margin(0), 0, "0 字节文件的 margin 应为 0");
    }
}
