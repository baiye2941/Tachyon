use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use tachyon_core::{DownloadError, DownloadResult};

use crate::storage::AsyncStorage;

pub struct TokioFile {
    path: PathBuf,
    file: Arc<std::fs::File>,
    /// Windows: seek_write 由 SetFilePointerEx + WriteFile 构成,非原子操作。
    /// 多线程并发 seek_write 可能导致写入位置错乱。Mutex 串行化保护。
    /// 非 Windows: seek_write 是原子的(基于 pread/pwrite),无需锁。
    #[cfg(target_os = "windows")]
    write_lock: Arc<std::sync::Mutex<()>>,
}

/// 审计 S-05:Unix 句柄化 openat 链,中间目录 O_NOFOLLOW|O_DIRECTORY,最终组件 O_NOFOLLOW|O_CREAT。
#[cfg(unix)]
pub(crate) fn open_path_nofollow_create(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let mut components: Vec<&std::ffi::OsStr> = Vec::new();
    let mut root: Option<PathBuf> = None;
    for c in path.components() {
        match c {
            std::path::Component::Prefix(p) => {
                root = Some(PathBuf::from(p.as_os_str()));
            }
            std::path::Component::RootDir => {
                root = Some(
                    root.unwrap_or_default()
                        .join(std::path::Component::RootDir.as_os_str()),
                );
            }
            std::path::Component::Normal(name) => components.push(name),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "路径含 .. 组件,拒绝 openat 链",
                ));
            }
        }
    }
    if components.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "路径无文件名组件",
        ));
    }

    // 打开根/起始目录(绝对路径以 / 起;相对路径以 .)
    let start = root.unwrap_or_else(|| PathBuf::from("."));
    let mut dir_fd = open_dir_nofollow(&start)?;

    let last = components.len() - 1;
    for (i, name) in components.iter().enumerate() {
        // MSRV 1.85:用 OsStrExt::as_bytes,不用 as_encoded_bytes(1.87+)
        let c_name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "路径组件含内部 NUL")
        })?;
        if i < last {
            // 中间目录:不存在则创建,打开时 O_NOFOLLOW|O_DIRECTORY
            let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW;
            // SAFETY: dir_fd 为有效目录 fd;c_name 为 NUL 结尾 C 字符串;flags 合法。
            // openat 失败返回 -1,调用方检查后不把非法 fd 传给 OwnedFd。
            let mut fd = unsafe { libc::openat(dir_fd.as_raw_fd(), c_name.as_ptr(), flags) };
            if fd < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::NotFound {
                    // 创建中间目录后重开
                    // SAFETY: 同上,mkdirat 仅用有效 dir_fd 与 C 字符串路径组件。
                    let mk = unsafe { libc::mkdirat(dir_fd.as_raw_fd(), c_name.as_ptr(), 0o755) };
                    if mk < 0 {
                        let e = std::io::Error::last_os_error();
                        // EEXIST 竞态:另一进程已创建,继续 openat
                        if e.raw_os_error() != Some(libc::EEXIST) {
                            return Err(e);
                        }
                    }
                    // SAFETY: 同上 openat 合约。
                    fd = unsafe { libc::openat(dir_fd.as_raw_fd(), c_name.as_ptr(), flags) };
                    if fd < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                } else {
                    return Err(err);
                }
            }
            // SAFETY: fd >= 0 为 openat 返回的合法 fd,所有权转入 OwnedFd。
            dir_fd = unsafe { OwnedFd::from_raw_fd(fd) };
        } else {
            // 最终组件:O_NOFOLLOW|O_CREAT|O_RDWR
            let flags = libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW;
            // SAFETY: dir_fd 有效;c_name NUL 结尾;mode 0o644 合法。失败时不消费 fd。
            let fd = unsafe { libc::openat(dir_fd.as_raw_fd(), c_name.as_ptr(), flags, 0o644) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: fd >= 0,File 取得所有权。
            return Ok(unsafe { std::fs::File::from_raw_fd(fd) });
        }
    }
    unreachable!()
}

#[cfg(unix)]
fn open_dir_nofollow(path: &Path) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "路径含内部 NUL"))?;
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW;
    // 根目录 "/" 上 O_NOFOLLOW 无影响;相对 "." 亦然。
    // 若起始路径本身是 symlink,拒绝(ELOOP),避免从 symlink 基目录起链。
    // SAFETY: c_path 为有效 C 字符串;flags 合法。失败返回 -1 不包装为 OwnedFd。
    let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
    if fd < 0 {
        // 基目录可能是已 canonicalize 的真实目录;O_NOFOLLOW 对非 symlink 无害。
        // 若失败因非目录等,回退不带 O_NOFOLLOW 仅当路径是 "." 或 "/" (启动锚点)。
        let err = std::io::Error::last_os_error();
        if path == Path::new(".") || path == Path::new("/") {
            let flags2 = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY;
            // SAFETY: 同上,仅对 "."/"/" 锚点回退打开。
            let fd2 = unsafe { libc::open(c_path.as_ptr(), flags2) };
            if fd2 < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: fd2 >= 0。
            return Ok(unsafe { OwnedFd::from_raw_fd(fd2) });
        }
        return Err(err);
    }
    // SAFETY: fd >= 0。
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// 审计 S-05:Windows 分组件 no-follow 打开。
/// 中间目录:symlink_metadata 拒绝 reparse 后 create_dir + open;
/// 最终组件:FILE_FLAG_OPEN_REPARSE_POINT 不跟随。
#[cfg(target_os = "windows")]
pub(crate) fn open_path_nofollow_create_windows(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    // 拒绝路径中已存在的中间 reparse 组件
    if let Some(parent) = path.parent() {
        reject_existing_reparse_components(parent)?;
        // 确保父目录存在(create_dir_all 不跟随 junction? 标准库会跟随 —
        // 因此先按组件创建,每步检查 reparse)
        create_dir_all_nofollow(parent)?;
    }

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
    const FILE_SHARE_READ: u32 = 0x00000001;
    const FILE_SHARE_WRITE: u32 = 0x00000002;
    const FILE_SHARE_DELETE: u32 = 0x00000004;

    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
}

#[cfg(target_os = "windows")]
pub(crate) fn reject_existing_reparse_components(path: &Path) -> std::io::Result<()> {
    let mut cursor = PathBuf::new();
    for c in path.components() {
        match c {
            // Prefix/RootDir 是盘符与根,不是可创建的中间目录;直接推进 cursor
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                cursor.push(c.as_os_str());
                continue;
            }
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "路径含 .. 组件,拒绝",
                ));
            }
            std::path::Component::Normal(name) => {
                cursor.push(name);
            }
        }
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) => {
                use std::os::windows::fs::{FileTypeExt, MetadataExt};
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                let ft = meta.file_type();
                if ft.is_symlink_dir()
                    || ft.is_symlink_file()
                    || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                {
                    return Err(std::io::Error::other(format!(
                        "路径组件是重解析点,拒绝: {}",
                        cursor.display()
                    )));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 后续 create_dir 会创建;尚未存在的组件无需检查
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn create_dir_all_nofollow(path: &Path) -> std::io::Result<()> {
    let mut cursor = PathBuf::new();
    for c in path.components() {
        match c {
            // 盘符/根目录不可 create_dir,仅推进 cursor
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                cursor.push(c.as_os_str());
                continue;
            }
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "路径含 .. 组件,拒绝创建",
                ));
            }
            std::path::Component::Normal(name) => {
                cursor.push(name);
            }
        }
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) => {
                use std::os::windows::fs::{FileTypeExt, MetadataExt};
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                let ft = meta.file_type();
                if ft.is_symlink_dir()
                    || ft.is_symlink_file()
                    || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                {
                    return Err(std::io::Error::other(format!(
                        "中间目录是重解析点,拒绝创建: {}",
                        cursor.display()
                    )));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&cursor)?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl TokioFile {
    #[cfg(target_os = "windows")]
    pub async fn open<P: AsRef<Path>>(path: P) -> DownloadResult<Self> {
        Self::open_sync(path).map_err(DownloadError::Io)
    }

    /// 审计 S-05:分组件 no-follow 打开。最终组件带 FILE_FLAG_OPEN_REPARSE_POINT;
    /// 中间目录经 symlink_metadata 拒绝 reparse 后打开,关闭 validate→open 中间目录 TOCTOU。
    #[cfg(target_os = "windows")]
    pub fn open_sync<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = open_path_nofollow_create_windows(&path)?;
        Ok(Self {
            path,
            file: Arc::new(file),
            write_lock: Arc::new(std::sync::Mutex::new(())),
        })
    }

    #[cfg(unix)]
    pub async fn open<P: AsRef<Path>>(path: P) -> DownloadResult<Self> {
        Self::open_sync(path).map_err(DownloadError::Io)
    }

    /// 审计 S-05:句柄化 openat 链打开路径,中间目录与最终组件均 O_NOFOLLOW。
    /// 攻击者在 validate_save_path 与 open 之间把中间目录替换为 symlink 时,
    /// openat(O_NOFOLLOW|O_DIRECTORY) 返回 ELOOP 而非跟随到基目录外。
    #[cfg(unix)]
    pub fn open_sync<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = open_path_nofollow_create(&path)?;
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
                // F-14: 改调统一 helper,集中处理 i64 溢出检查 + rollback。
                // helper 内部:set_len + SetFileInformationByHandle(FileAllocationInfo)
                // + SetFileValidData(静默回退),SetFileInfo 失败时 rollback 到旧大小。
                crate::alloc::allocate_windows(&file, size)
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
            let path = self.path.clone();
            tokio::task::spawn_blocking(move || {
                file.sync_data().map_err(DownloadError::Io)?;
                // F-15:文件 fsync 后 sync 父目录(防断电丢目录项)
                crate::sync_parent_dir(&path).map_err(DownloadError::Io)?;
                Ok(())
            })
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
            // F-14: fallocate 的 len 参数为 i64(off_t)。若 size > i64::MAX,
            // `size as i64` 会静默截断为负数,导致 fallocate 行为未定义或 EINVAL。
            // 入口处显式校验,拒绝溢出的 size(参照 iouring.rs:1685 模式)。
            libc::off_t::try_from(size).map_err(|_| {
                DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "tokio_file allocate size {size} 超过 i64 最大值 {},fallocate 无法处理",
                        i64::MAX
                    ),
                ))
            })?;
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
            let path = self.path.clone();
            tokio::task::spawn_blocking(move || {
                file.sync_data().map_err(DownloadError::Io)?;
                crate::sync_parent_dir(&path).map_err(DownloadError::Io)
            })
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
            let path = self.path.clone();
            tokio::task::spawn_blocking(move || {
                file.sync_data().map_err(DownloadError::Io)?;
                crate::sync_parent_dir(&path).map_err(DownloadError::Io)
            })
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
        // SAFETY: FILE_STANDARD_INFO 是 POD Win32 结构体,全零位模式是有效初始值,
        // 用作 GetFileInformationByHandleEx 输出缓冲区。
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

#[cfg(test)]
mod s05_nofollow_tests {
    use super::*;
    use std::fs;

    /// 审计 S-05:中间目录被替换为 symlink 时,open 必须失败而非跟随写入基目录外。
    #[test]
    #[cfg(unix)]
    fn test_open_sync_rejects_intermediate_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        fs::create_dir(&base).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let mid = base.join("mid");
        fs::create_dir(&mid).unwrap();
        // 先正常打开一次确保路径可用
        let target = mid.join("file.bin");
        let f = TokioFile::open_sync(&target).expect("正常路径应打开成功");
        drop(f);
        fs::remove_file(&target).ok();
        // 把 mid 换成指向 outside 的 symlink
        fs::remove_dir(&mid).unwrap();
        std::os::unix::fs::symlink(&outside, &mid).unwrap();
        let err = match TokioFile::open_sync(&target) {
            Ok(_) => panic!("中间目录 symlink 应被拒绝"),
            Err(e) => e,
        };
        // openat(O_NOFOLLOW|O_DIRECTORY) 对中间 symlink 的 errno 因平台而异:
        // - 常见 ELOOP(symlink 不跟随)
        // - Linux 部分路径/内核组合也可能 ENOTDIR(把 symlink 当目录打开失败)
        // 安全不变量:open 必须失败,且不得跟随写入 base 外。
        let raw = err.raw_os_error();
        assert!(
            raw == Some(libc::ELOOP)
                || raw == Some(libc::ENOTDIR)
                || err.kind() == std::io::ErrorKind::NotADirectory
                || err.kind() == std::io::ErrorKind::Other
                || err.to_string().contains("symlink")
                || err.to_string().contains("directory")
                || err.to_string().contains("重解析")
                || err.to_string().contains("符号"),
            "期望 ELOOP/ENOTDIR(中间 symlink 拒绝), got kind={:?} raw={:?} msg={err}",
            err.kind(),
            raw
        );
        // 额外:不得在 symlink 目标(outside)下创建 file.bin
        assert!(
            !outside.join("file.bin").exists(),
            "中间目录 symlink 被跟随时会在 outside 下创建文件,属于 TOCTOU 逃逸"
        );
    }

    /// 审计 S-05:正常无 symlink 路径仍应成功打开并写入。
    #[test]
    fn test_open_sync_normal_nested_path() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        fs::create_dir(&base).unwrap();
        let nested = base.join("a").join("b").join("file.bin");
        // 父目录可能不存在:open 应能创建中间目录(unix openat mkdirat / win create_dir_all_nofollow)
        let f = TokioFile::open_sync(&nested).expect("嵌套路径应打开成功");
        // 简单写入校验
        drop(f);
        assert!(nested.exists(), "文件应被创建");
    }

    /// 审计 S-05:Windows 中间目录被替换为 junction/symlink 时,open 必须失败。
    #[test]
    #[cfg(target_os = "windows")]
    fn test_open_sync_rejects_intermediate_reparse() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        fs::create_dir(&base).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let mid = base.join("mid");
        fs::create_dir(&mid).unwrap();
        let target = mid.join("file.bin");
        let f = TokioFile::open_sync(&target).expect("正常路径应打开成功");
        drop(f);
        fs::remove_file(&target).ok();
        // 把 mid 换成指向 outside 的 directory symlink/junction
        fs::remove_dir(&mid).unwrap();
        if let Err(e) = std::os::windows::fs::symlink_dir(&outside, &mid) {
            // 无开发者模式/管理员时跳过(环境限制,非逻辑失败)
            eprintln!("跳过:无法创建 directory symlink: {e}");
            return;
        }
        match TokioFile::open_sync(&target) {
            Ok(_) => panic!("中间目录 reparse 应被拒绝"),
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    msg.contains("重解析")
                        || msg.contains("reparse")
                        || msg.contains("symlink")
                        || err.kind() == std::io::ErrorKind::Other,
                    "期望 reparse 拒绝错误, got kind={:?} msg={msg}",
                    err.kind()
                );
            }
        }
    }
}
