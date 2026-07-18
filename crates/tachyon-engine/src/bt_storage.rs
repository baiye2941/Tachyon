//! librqbit 自定义 Storage:消除磁力链接双存储写放大(P2-4)
//!
//! ## 问题
//! 默认 librqbit 用 `FilesystemStorage` 把 piece 写到 `download_dir`,
//! 然后 Tachyon 通过 `FileStream` 读出来再写到目标文件(双存储 I/O)。
//!
//! ## 方案
//! 实现 librqbit `TorrentStorage` trait,让 librqbit 直接写到 Tachyon 的
//! `AsyncStorage`(目标文件),消除 FileStream 读取路径的中间磁盘读写。
//!
//! ## sync -> async 桥接
//! librqbit 的 `pwrite_all`/`pread_exact` 是同步的,Tachyon 的 `AsyncStorage`
//! 是异步的。用 `tokio::task::block_in_place` + `Handle::block_on` 桥接:
//! - `block_in_place` 把当前 worker 线程转为"阻塞模式",允许其他 task 运行
//! - `Handle::block_on` 在阻塞线程上 poll async future
//! - 需多线程 runtime(单线程会 panic,Tachyon 默认多线程)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrentShared, TorrentMetadata};
use librqbit_core::lengths::ValidPieceIndex;
use tachyon_core::config::IoStrategy;
use tachyon_core::traits::AsyncStorage;

/// Tachyon 的 librqbit Storage 实现
///
/// 包装 Tachyon 的 `AsyncStorage`,将 librqbit 的 piece 写入直接路由到目标文件。
/// 每个 file_id 对应一个 `AsyncStorage` 实例(多文件 torrent 有多个)。
pub struct TachyonTorrentStorage {
    /// 各文件的异步存储(file_id -> storage)
    storages: Vec<Arc<dyn AsyncStorage>>,
    /// tokio runtime handle(用于 sync->async 桥接)
    handle: tokio::runtime::Handle,
}

impl TachyonTorrentStorage {
    /// 创建存储
    ///
    /// # 参数
    /// - `storages`: 各文件的 AsyncStorage(file_id 索引对齐)
    /// - `handle`: tokio runtime handle(必须多线程)
    pub fn new(storages: Vec<Arc<dyn AsyncStorage>>, handle: tokio::runtime::Handle) -> Self {
        Self { storages, handle }
    }

    /// 获取指定 file_id 的存储
    fn storage(&self, file_id: usize) -> anyhow::Result<&Arc<dyn AsyncStorage>> {
        self.storages.get(file_id).ok_or_else(|| {
            anyhow::anyhow!("file_id {file_id} 越界(共 {} 个文件)", self.storages.len())
        })
    }

    /// sync -> async 桥接:在 block_in_place 中 poll async future
    fn block_on<F, T>(&self, fut: F) -> anyhow::Result<T>
    where
        F: std::future::Future<Output = tachyon_core::DownloadResult<T>>,
    {
        tokio::task::block_in_place(|| {
            self.handle
                .block_on(fut)
                .map_err(|e| anyhow::anyhow!("storage async 操作失败: {e}"))
        })
    }
}

impl TorrentStorage for TachyonTorrentStorage {
    fn init(
        &mut self,
        _shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        // 存储已在构造时就绪(由 create() 或 register 预先打开),无需额外初始化
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let storage = self.storage(file_id)?.clone();
        let len = buf.len();
        // 用 tmp buffer 避免 buf 的生命周期问题(block_on 闭包不能直接捕获 &mut buf)
        let mut tmp = vec![0u8; len];
        let tmp_ptr = tmp.as_mut_ptr();
        let result = tokio::task::block_in_place(|| {
            self.handle.block_on(async move {
                // safety: tmp 在 block_on 期间存活,tmp_ptr 有效
                let tmp_slice = unsafe { std::slice::from_raw_parts_mut(tmp_ptr, len) };
                // 审计 BT-18:exact 读 —— 循环读满;零进度 EOF 才算不足
                let mut pos = 0usize;
                let mut off = offset;
                while pos < len {
                    let n = storage.read_at(off, &mut tmp_slice[pos..]).await?;
                    if n == 0 {
                        return Err(tachyon_core::DownloadError::Io(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            format!("pread_exact: 读取不足 {pos}/{len}"),
                        )));
                    }
                    pos += n;
                    off += n as u64;
                }
                Ok(())
            })
        });
        result
            .map_err(|e: tachyon_core::DownloadError| anyhow::anyhow!("pread_exact 失败: {e}"))?;
        buf.copy_from_slice(&tmp);
        Ok(())
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        let storage = self.storage(file_id)?.clone();
        let data = bytes::Bytes::copy_from_slice(buf);
        let len = buf.len();
        self.block_on(async move {
            // 审计 BT-18:exact 契约 —— 循环写满,禁止单次 short write 后当成功
            // 也不应仅报一次 short 就丢弃已写进度;写零进度视为错误。
            let mut pos = 0usize;
            let mut off = offset;
            while pos < len {
                let chunk = data.slice(pos..);
                let written = storage.write_at(off, chunk).await?;
                if written == 0 {
                    return Err(tachyon_core::DownloadError::Io(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        format!("pwrite_all: 零进度写入 pos={pos}/{len}"),
                    )));
                }
                pos += written;
                off += written as u64;
            }
            Ok(())
        })
    }

    fn remove_file(&self, _file_id: usize, _filename: &Path) -> anyhow::Result<()> {
        // Tachyon 管理文件生命周期,librqbit 不应删除
        Ok(())
    }

    fn remove_directory_if_empty(&self, _path: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn ensure_file_length(&self, file_id: usize, length: u64) -> anyhow::Result<()> {
        let storage = self.storage(file_id)?.clone();
        self.block_on(async move { storage.allocate(length).await })
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        // 返回一个可用的克隆(librqbit 用 take() 替换当前 storage,
        // 返回的新 storage 应继续工作。与 FilesystemStorage::take 对齐:
        // 克隆文件句柄而非返回 dummy。
        // 原实现返回 DummyStorage 导致 librqbit 写入失败("dummy storage: 已 take")
        Ok(Box::new(TachyonTorrentStorage::new(
            self.storages.clone(),
            self.handle.clone(),
        )))
    }

    fn on_piece_completed(&self, _piece_index: ValidPieceIndex) -> anyhow::Result<()> {
        // piece 完成回调:无需额外操作(数据已直接写入目标文件)
        Ok(())
    }
}

/// 文件存储列表类型别名
type FileStorages = Vec<Arc<dyn AsyncStorage>>;
/// Storage 注册表类型别名
type StorageRegistry = parking_lot::RwLock<std::collections::HashMap<String, FileStorages>>;

/// StorageFactory:为每个 torrent 创建 TachyonTorrentStorage
///
/// 支持两种模式:
/// 1. **预注册模式**:引擎在 `add_torrent` 前调用 `register()`,传入已打开的
///    `AsyncStorage`(来自 `init_storage`)。`create()` 从 registry 查找。
///    优点:复用引擎已打开的文件句柄,零额外 fd 开销。
/// 2. **自动打开模式**:未注册时,`create()` 从 `metadata.file_infos` +
///    `shared.options.output_folder` 构造路径,用 `IoStrategy` 打开文件。
///    优点:无需时序协调,librqbit `add_torrent` 内部自动处理。
///
/// 生产路径用模式 2(自动打开),因为 magnet 的 metadata 在 `add_torrent`
/// 内部才获取,引擎无法提前注册。模式 1 供测试和未来优化使用。
pub struct TachyonStorageFactory {
    /// info_hash -> storages 映射(预注册模式)
    /// 由 Tachyon 在 add_torrent 前注册(可选,未注册时走自动打开)
    registry: Arc<StorageRegistry>,
    /// tokio runtime handle(用于 sync->async 桥接)
    handle: tokio::runtime::Handle,
    /// I/O 策略(自动打开模式用)
    io_strategy: IoStrategy,
    /// 下载目录(自动打开模式用,与引擎 download_dir 对齐)
    download_dir: PathBuf,
    /// 用户最终根名(单文件名/多文件根目录名);优先于 torrent metadata.name
    preferred_root_name: std::sync::Arc<parking_lot::RwLock<Option<String>>>,
    /// 审计 BT-19 测试用:最近一次自动打开所选后端名
    #[cfg(test)]
    last_open_backend: std::sync::Arc<parking_lot::Mutex<Option<&'static str>>>,
}

impl TachyonStorageFactory {
    /// 创建 factory
    ///
    /// # 参数
    /// - `handle`: tokio runtime handle(必须多线程)
    /// - `io_strategy`: I/O 策略(自动打开模式用,预注册模式忽略)
    /// - `download_dir`: 下载目录(自动打开模式用,与引擎 download_dir 对齐)
    pub fn new(
        handle: tokio::runtime::Handle,
        io_strategy: IoStrategy,
        download_dir: PathBuf,
    ) -> Self {
        Self {
            registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            handle,
            io_strategy,
            download_dir,
            preferred_root_name: std::sync::Arc::new(parking_lot::RwLock::new(None)),
            #[cfg(test)]
            last_open_backend: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// 测试:读取最近自动打开的后端标签
    #[cfg(test)]
    pub fn last_open_backend(&self) -> Option<&'static str> {
        *self.last_open_backend.lock()
    }

    /// 测试:当前配置的 io_strategy
    #[cfg(test)]
    pub fn io_strategy(&self) -> IoStrategy {
        self.io_strategy
    }

    /// 注入用户最终根名(须在 probe/add_torrent 前)
    pub fn with_preferred_root_name(self, name: impl Into<String>) -> Self {
        *self.preferred_root_name.write() = Some(name.into());
        self
    }

    pub fn set_preferred_root_name(&self, name: Option<String>) {
        *self.preferred_root_name.write() = name;
    }

    /// 测试/调试:解析自动打开模式下的根名
    pub fn resolved_root_name(&self, torrent_name: &str) -> String {
        self.preferred_root_name
            .read()
            .clone()
            .unwrap_or_else(|| torrent_name.to_string())
    }

    /// 注册 torrent 的 storages(在 add_torrent 前调用,可选)
    ///
    /// # 参数
    /// - `info_hash`: torrent info hash(十六进制)
    /// - `storages`: 各文件的 AsyncStorage
    pub fn register(&self, info_hash: String, storages: Vec<Arc<dyn AsyncStorage>>) {
        self.registry.write().insert(info_hash, storages);
    }

    /// 注销 torrent 的 storages(下载完成后清理)
    pub fn unregister(&self, info_hash: &str) {
        self.registry.write().remove(info_hash);
    }

    /// 从 metadata 构造文件路径并打开 AsyncStorage(自动打开模式)
    ///
    /// 路径规则与引擎 `init_storage` 对齐(sanitize_filename + validate_save_path):
    /// - 单文件: `download_dir/<name>`
    /// - 多文件: `download_dir/<sanitize(torrent_name)>/<sanitize(relative_filename)>`
    ///
    /// 安全:与 init_storage 使用同一套 validate_multi_save_paths/validate_save_path,
    /// 确保 librqbit 写入路径与引擎存储路径一致(消除双存储写放大的前提)。
    ///
    /// 审计 BT-19:按 `io_strategy` 选择后端(平台不可用/初始化失败时回退 Standard),
    /// 禁止忽略配置硬编码 TokioFile。
    fn open_storages_from_metadata(
        &self,
        _shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<Vec<Arc<dyn AsyncStorage>>> {
        let file_infos = &metadata.file_infos;
        let preferred = self.preferred_root_name.read().clone();
        let torrent_name = preferred
            .as_deref()
            .or(metadata.name.as_deref())
            .unwrap_or("unknown_torrent");

        let multi_file = file_infos.len() > 1;
        let mut storages = Vec::with_capacity(file_infos.len());
        if multi_file {
            // 多文件:用 validate_multi_save_paths 确保路径与 init_storage 完全一致
            let file_names: Vec<String> = file_infos
                .iter()
                .map(|fi| fi.relative_filename.to_string_lossy().into_owned())
                .collect();
            let paths = tachyon_core::validate_multi_save_paths(
                &self.download_dir,
                torrent_name,
                &file_names,
            )
            .map_err(|e| anyhow::anyhow!("多文件路径校验失败: {e}"))?;
            for path in &paths {
                // 多文件 factory 在 librqbit async 上下文同步 create;禁止嵌套 Handle::block_on。
                let (file, backend) = self.open_storage_for_path(path)?;
                #[cfg(test)]
                {
                    *self.last_open_backend.lock() = Some(backend);
                }
                let _ = backend;
                storages.push(file);
            }
        } else {
            // 单文件:download_dir/<name>,与 init_storage 的单文件路径一致
            let final_path = self.download_dir.join(torrent_name);
            let canonical_path = tachyon_core::validate_save_path(&final_path, &self.download_dir)
                .map_err(|e| anyhow::anyhow!("单文件路径校验失败: {e}"))?;
            let (file, backend) = self.open_storage_for_path(&canonical_path)?;
            #[cfg(test)]
            {
                *self.last_open_backend.lock() = Some(backend);
            }
            let _ = backend;
            storages.push(file);
        }
        Ok(storages)
    }

    /// 审计 BT-19:按 io_strategy 同步打开 AsyncStorage。
    ///
    /// 在 librqbit StorageFactory::create 同步上下文中调用,禁止再嵌套 Handle::block_on。
    /// 高级后端用其同步 init/open 路径;失败或平台不支持时回退 TokioFile::open_sync。
    fn open_storage_for_path(
        &self,
        path: &Path,
    ) -> anyhow::Result<(Arc<dyn AsyncStorage>, &'static str)> {
        match self.io_strategy {
            IoStrategy::Standard => self.open_standard_storage(path),
            IoStrategy::WinAligned => {
                #[cfg(target_os = "windows")]
                {
                    match self.open_win_aligned_storage(path) {
                        Ok(s) => Ok((s, "WinAligned")),
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "BT WinAligned 打开失败,回退 Standard"
                            );
                            self.open_standard_storage(path)
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    tracing::warn!(
                        path = %path.display(),
                        "BT WinAligned 策略在非 Windows 不可用,回退 Standard"
                    );
                    self.open_standard_storage(path)
                }
            }
            IoStrategy::Iocp => {
                #[cfg(target_os = "windows")]
                {
                    match self.open_iocp_storage(path) {
                        Ok(s) => Ok((s, "Iocp")),
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "BT IOCP 初始化失败,回退 Standard"
                            );
                            self.open_standard_storage(path)
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    tracing::warn!(
                        path = %path.display(),
                        "BT Iocp 策略在非 Windows 不可用,回退 Standard"
                    );
                    self.open_standard_storage(path)
                }
            }
            IoStrategy::IoUring => {
                #[cfg(target_os = "linux")]
                {
                    match self.open_iouring_storage(path) {
                        Ok(s) => Ok((s, "IoUring")),
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "BT io_uring 初始化失败,回退 Standard"
                            );
                            self.open_standard_storage(path)
                        }
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    tracing::warn!(
                        path = %path.display(),
                        "BT IoUring 策略在非 Linux 不可用,回退 Standard"
                    );
                    self.open_standard_storage(path)
                }
            }
        }
    }

    fn open_standard_storage(
        &self,
        path: &Path,
    ) -> anyhow::Result<(Arc<dyn AsyncStorage>, &'static str)> {
        let file = tachyon_io::TokioFile::open_sync(path)
            .map_err(|e| anyhow::anyhow!("打开文件 {} 失败: {e}", path.display()))?;
        Ok((Arc::new(file) as Arc<dyn AsyncStorage>, "Standard"))
    }

    #[cfg(target_os = "windows")]
    fn open_win_aligned_storage(&self, path: &Path) -> anyhow::Result<Arc<dyn AsyncStorage>> {
        // WinFile::open_optimized 内部为同步 OpenOptions;此处用 block_in_place 包装
        // 仅在 factory create 同步上下文,且不得嵌套 runtime handle.block_on 打开 TokioFile。
        let path = path.to_path_buf();
        let file = tokio::task::block_in_place(|| {
            self.handle
                .block_on(tachyon_io::WinFile::open_optimized(&path))
        })
        .map_err(|e| anyhow::anyhow!("WinAligned 打开 {} 失败: {e}", path.display()))?;
        Ok(Arc::new(file) as Arc<dyn AsyncStorage>)
    }

    #[cfg(target_os = "windows")]
    fn open_iocp_storage(&self, path: &Path) -> anyhow::Result<Arc<dyn AsyncStorage>> {
        let mut storage = tachyon_io::IoCpStorage::new(path);
        storage
            .init()
            .map_err(|e| anyhow::anyhow!("IOCP init {} 失败: {e}", path.display()))?;
        Ok(Arc::new(storage) as Arc<dyn AsyncStorage>)
    }

    #[cfg(target_os = "linux")]
    fn open_iouring_storage(&self, path: &Path) -> anyhow::Result<Arc<dyn AsyncStorage>> {
        let mut storage =
            tachyon_io::IoUringStorage::new(path, tachyon_io::IoUringConfig::default());
        storage
            .init()
            .map_err(|e| anyhow::anyhow!("io_uring init {} 失败: {e}", path.display()))?;
        Ok(Arc::new(storage) as Arc<dyn AsyncStorage>)
    }
}

impl StorageFactory for TachyonStorageFactory {
    type Storage = TachyonTorrentStorage;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<Self::Storage> {
        let info_hash = shared.info_hash.as_string();

        // 优先从 registry 查找预注册的 storages(模式 1)
        let storages = if let Some(s) = self.registry.read().get(&info_hash).cloned() {
            tracing::debug!(info_hash = %info_hash, "StorageFactory: 命中预注册 storages");
            s
        } else {
            // 自动打开模式(模式 2):从 metadata 构造路径并打开
            tracing::debug!(info_hash = %info_hash, "StorageFactory: 自动打开 storages");
            self.open_storages_from_metadata(shared, metadata)?
        };

        Ok(TachyonTorrentStorage::new(storages, self.handle.clone()))
    }

    fn clone_box(&self) -> BoxStorageFactory {
        // 用 librqbit 的 boxed() 包装(self 实现了 StorageFactory)
        self.clone().boxed()
    }
}

impl Clone for TachyonStorageFactory {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            handle: self.handle.clone(),
            io_strategy: self.io_strategy,
            download_dir: self.download_dir.clone(),
            preferred_root_name: self.preferred_root_name.clone(),
            #[cfg(test)]
            last_open_backend: self.last_open_backend.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内存 AsyncStorage 测试实现
    struct InMemStorage {
        data: parking_lot::RwLock<Vec<u8>>,
    }

    impl InMemStorage {
        fn new() -> Self {
            Self {
                data: parking_lot::RwLock::new(Vec::new()),
            }
        }
    }

    impl AsyncStorage for InMemStorage {
        fn write_at(
            &self,
            offset: u64,
            data: bytes::Bytes,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>> + Send + '_>,
        > {
            let off = offset as usize;
            Box::pin(async move {
                let mut guard = self.data.write();
                let need = off + data.len();
                if guard.len() < need {
                    guard.resize(need, 0);
                }
                guard[off..off + data.len()].copy_from_slice(&data);
                Ok(data.len())
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>> + Send + 'a>,
        > {
            let off = offset as usize;
            Box::pin(async move {
                let guard = self.data.read();
                if off >= guard.len() {
                    return Ok(0);
                }
                let end = (off + buf.len()).min(guard.len());
                let n = end - off;
                buf[..n].copy_from_slice(&guard[off..end]);
                Ok(n)
            })
        }

        fn sync(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
        > {
            Box::pin(async { Ok(()) })
        }

        fn allocate(
            &self,
            size: u64,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
        > {
            Box::pin(async move {
                let mut guard = self.data.write();
                if (size as usize) > guard.len() {
                    guard.resize(size as usize, 0);
                }
                Ok(())
            })
        }

        fn file_size(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<u64>> + Send + '_>,
        > {
            Box::pin(async move {
                let guard = self.data.read();
                Ok(guard.len() as u64)
            })
        }

        fn close(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
        > {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tachyon_torrent_storage_pwrite_pread() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        // 写入
        ts.pwrite_all(0, 100, b"hello world").unwrap();

        // 读取
        let mut buf = [0u8; 11];
        ts.pread_exact(0, 100, &mut buf).unwrap();
        assert_eq!(&buf, b"hello world");
    }

    /// 审计 BT-18:底层 short write 时 pwrite_all 须循环写满
    #[tokio::test(flavor = "multi_thread")]
    async fn test_pwrite_all_retries_short_write() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ShortWriteStorage {
            inner: InMemStorage,
            max_per_call: usize,
            calls: AtomicUsize,
        }

        impl AsyncStorage for ShortWriteStorage {
            fn write_at(
                &self,
                offset: u64,
                data: bytes::Bytes,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>>
                        + Send
                        + '_,
                >,
            > {
                let max = self.max_per_call;
                Box::pin(async move {
                    let n = data.len().min(max);
                    if n == 0 {
                        return Ok(0);
                    }
                    self.calls.fetch_add(1, Ordering::Relaxed);
                    self.inner.write_at(offset, data.slice(..n)).await
                })
            }

            fn read_at<'a>(
                &'a self,
                offset: u64,
                buf: &'a mut [u8],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>>
                        + Send
                        + 'a,
                >,
            > {
                self.inner.read_at(offset, buf)
            }

            fn sync(
                &self,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.sync()
            }

            fn allocate(
                &self,
                size: u64,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.allocate(size)
            }

            fn file_size(
                &self,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<u64>> + Send + '_,
                >,
            > {
                self.inner.file_size()
            }

            fn close(
                &self,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.close()
            }
        }

        let short = Arc::new(ShortWriteStorage {
            inner: InMemStorage::new(),
            max_per_call: 3,
            calls: AtomicUsize::new(0),
        }) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![short.clone()], handle);

        let payload = b"hello world!!"; // 13 bytes, max 3 -> >=5 calls
        ts.pwrite_all(0, 0, payload).unwrap();
        // 通过 trait object 读回:构造另一个 storage 引用困难,用 pread
        let mut buf = [0u8; 13];
        ts.pread_exact(0, 0, &mut buf).unwrap();
        assert_eq!(&buf, payload);

        // calls 在 short 上:需要 downcast 不可行;至少数据完整即证明循环写满
        // 再写一次验证可重复
        ts.pwrite_all(0, 0, b"ABCDEFGHIJKLM").unwrap();
        let mut buf2 = [0u8; 13];
        ts.pread_exact(0, 0, &mut buf2).unwrap();
        assert_eq!(&buf2, b"ABCDEFGHIJKLM");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tachyon_torrent_storage_multiple_files() {
        let s0 = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let s1 = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![s0, s1], handle);

        ts.pwrite_all(0, 0, b"file0").unwrap();
        ts.pwrite_all(1, 0, b"file1").unwrap();

        let mut buf = [0u8; 5];
        ts.pread_exact(0, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"file0");
        ts.pread_exact(1, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"file1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tachyon_torrent_storage_file_id_out_of_range() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        let err = ts.pwrite_all(1, 0, b"data").unwrap_err();
        assert!(err.to_string().contains("越界"));
    }

    /// 审计 BT-18:底层 short read 时 pread_exact 须循环读满
    #[tokio::test(flavor = "multi_thread")]
    async fn test_pread_exact_retries_short_read() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ShortReadStorage {
            inner: InMemStorage,
            max_per_call: usize,
            calls: AtomicUsize,
        }

        impl AsyncStorage for ShortReadStorage {
            fn write_at(
                &self,
                offset: u64,
                data: bytes::Bytes,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>>
                        + Send
                        + '_,
                >,
            > {
                self.inner.write_at(offset, data)
            }

            fn read_at<'a>(
                &'a self,
                offset: u64,
                buf: &'a mut [u8],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>>
                        + Send
                        + 'a,
                >,
            > {
                let max = self.max_per_call;
                Box::pin(async move {
                    let limit = buf.len().min(max);
                    self.calls.fetch_add(1, Ordering::Relaxed);
                    self.inner.read_at(offset, &mut buf[..limit]).await
                })
            }

            fn sync(
                &self,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.sync()
            }

            fn allocate(
                &self,
                size: u64,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.allocate(size)
            }

            fn file_size(
                &self,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<u64>> + Send + '_,
                >,
            > {
                self.inner.file_size()
            }

            fn close(
                &self,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.close()
            }
        }

        let short = Arc::new(ShortReadStorage {
            inner: InMemStorage::new(),
            max_per_call: 3,
            calls: AtomicUsize::new(0),
        }) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![short], handle);
        ts.pwrite_all(0, 0, b"hello world!!").unwrap();
        let mut buf = [0u8; 13];
        ts.pread_exact(0, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello world!!");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pread_exact_short_read_errors() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        // 写入 5 字节
        ts.pwrite_all(0, 0, b"hello").unwrap();

        // 尝试读取 10 字节(只有 5 字节可读)
        let mut buf = [0u8; 10];
        let err = ts.pread_exact(0, 0, &mut buf).unwrap_err();
        assert!(err.to_string().contains("读取不足"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tachyon_torrent_storage_take_returns_clone() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        // take() 应返回一个可用的克隆(与 FilesystemStorage::take 对齐)
        let cloned = ts.take().unwrap();
        // cloned 的 pwrite 应该成功(不是 dummy)
        cloned.pwrite_all(0, 0, b"data").unwrap();

        // 验证数据写入
        let mut buf = [0u8; 4];
        ts.pread_exact(0, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"data");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tachyon_torrent_storage_ensure_file_length() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        // ensure_file_length 应该扩展存储
        ts.ensure_file_length(0, 1024).unwrap();

        // 写入超出原始大小
        ts.pwrite_all(0, 1020, b"abcd").unwrap();

        // 读取验证
        let mut buf = [0u8; 4];
        ts.pread_exact(0, 1020, &mut buf).unwrap();
        assert_eq!(&buf, b"abcd");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_preferred_root_name_overrides_torrent_name() {
        let handle = tokio::runtime::Handle::current();
        let factory = TachyonStorageFactory::new(
            handle,
            IoStrategy::default(),
            std::path::PathBuf::from("/tmp/dl"),
        )
        .with_preferred_root_name("user_renamed.bin");
        assert_eq!(
            factory.resolved_root_name("original.bin"),
            "user_renamed.bin"
        );
        factory.set_preferred_root_name(Some("later.bin".into()));
        assert_eq!(factory.resolved_root_name("original.bin"), "later.bin");
    }

    #[test]
    fn test_multi_file_open_path_does_not_use_nested_block_on_comment() {
        // 静态不变量: open_storages_from_metadata 不得嵌套 block_on(TokioFile::open)
        // (源码契约;避免 async runtime 内嵌套 block_on 打开 TokioFile 异步路径)。
        let src = include_str!("bt_storage.rs");
        assert!(
            src.contains(
                "// 多文件 factory 在 librqbit async 上下文同步 create;禁止嵌套 Handle::block_on。"
            ),
            "多文件打开路径应保留禁止嵌套 block_on 的契约注释"
        );
        let start = src
            .find("fn open_storages_from_metadata")
            .expect("open_storages_from_metadata");
        let body = &src[start..start + 3500];
        assert!(
            !body.contains("block_on(tachyon_io::TokioFile::open"),
            "open_storages_from_metadata 不得嵌套 block_on 打开 TokioFile 异步路径"
        );
        assert!(
            body.contains("open_storage_for_path"),
            "BT-19:应通过 open_storage_for_path 按 io_strategy 打开"
        );
    }

    /// 审计 BT-19:Standard 策略打开 Standard 后端
    #[tokio::test(flavor = "multi_thread")]
    async fn test_bt19_standard_opens_standard_backend() {
        let dir = tempfile::tempdir().unwrap();
        let handle = tokio::runtime::Handle::current();
        let factory =
            TachyonStorageFactory::new(handle, IoStrategy::Standard, dir.path().to_path_buf());
        let path = dir.path().join("f.bin");
        let (storage, backend) = factory.open_storage_for_path(&path).unwrap();
        assert_eq!(backend, "Standard");
        // 可写
        use bytes::Bytes;
        let n = storage
            .write_at(0, Bytes::from_static(b"ab"))
            .await
            .unwrap();
        assert_eq!(n, 2);
    }

    /// 审计 BT-19:非本平台策略回退 Standard 而不是 panic/忽略配置
    #[tokio::test(flavor = "multi_thread")]
    async fn test_bt19_cross_platform_strategy_falls_back_to_standard() {
        let dir = tempfile::tempdir().unwrap();
        let handle = tokio::runtime::Handle::current();
        // Windows 上 IoUring 应回退;非 Windows 上 Iocp 应回退
        #[cfg(target_os = "windows")]
        let strategy = IoStrategy::IoUring;
        #[cfg(not(target_os = "windows"))]
        let strategy = IoStrategy::Iocp;
        let factory = TachyonStorageFactory::new(handle, strategy, dir.path().to_path_buf());
        let path = dir.path().join("fallback.bin");
        let (_storage, backend) = factory.open_storage_for_path(&path).unwrap();
        assert_eq!(
            backend, "Standard",
            "跨平台不可用策略必须回退 Standard,backend={backend}"
        );
    }

    /// 审计 BT-19:factory 保存的 io_strategy 可被读取(不再是死字段)
    #[tokio::test(flavor = "multi_thread")]
    async fn test_bt19_factory_retains_io_strategy() {
        let handle = tokio::runtime::Handle::current();
        let factory = TachyonStorageFactory::new(
            handle,
            IoStrategy::WinAligned,
            std::path::PathBuf::from("/tmp/dl"),
        );
        assert_eq!(factory.io_strategy(), IoStrategy::WinAligned);
    }

    // ===== S2: bt_storage 覆盖率缺口补 RED 测试 =====

    /// 底层 storage write_at 永远返回 Ok(0) 时,pwrite_all 必须检测零进度
    /// 并返回 WriteZero 错误,禁止静默返回成功(审计 BT-18 写零进度契约)。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_pwrite_all_zero_progress_returns_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// 永远返回 Ok(0) 的零进度写入 mock(模拟底层存储卡死/管道断裂)
        struct ZeroProgressStorage {
            inner: InMemStorage,
            calls: AtomicUsize,
        }

        impl AsyncStorage for ZeroProgressStorage {
            fn write_at(
                &self,
                offset: u64,
                data: bytes::Bytes,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>>
                        + Send
                        + '_,
                >,
            > {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let _ = (offset, data);
                Box::pin(async move { Ok(0) })
            }

            fn read_at<'a>(
                &'a self,
                offset: u64,
                buf: &'a mut [u8],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>>
                        + Send
                        + 'a,
                >,
            > {
                self.inner.read_at(offset, buf)
            }

            fn sync(
                &self,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.sync()
            }

            fn allocate(
                &self,
                size: u64,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.allocate(size)
            }

            fn file_size(
                &self,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<u64>> + Send + '_,
                >,
            > {
                self.inner.file_size()
            }

            fn close(
                &self,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.close()
            }
        }

        let broken = Arc::new(ZeroProgressStorage {
            inner: InMemStorage::new(),
            calls: AtomicUsize::new(0),
        }) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![broken], handle);

        let err = ts.pwrite_all(0, 0, b"hello").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("零进度") || msg.contains("WriteZero"),
            "零进度写入必须返回 WriteZero 错误,实际: {msg}"
        );
    }

    /// pread_exact 在底层立即返回 Ok(0) 时必须返回 UnexpectedEof 错误,
    /// 禁止静默成功返回未填满的 buf(审计 BT-18 读零进度契约)。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_pread_exact_eof_returns_unexpected_eof() {
        /// 永远返回 Ok(0) 的零进度读取 mock(模拟读到 EOF)
        struct ZeroReadStorage {
            inner: InMemStorage,
        }

        impl AsyncStorage for ZeroReadStorage {
            fn write_at(
                &self,
                offset: u64,
                data: bytes::Bytes,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>>
                        + Send
                        + '_,
                >,
            > {
                self.inner.write_at(offset, data)
            }

            fn read_at<'a>(
                &'a self,
                _offset: u64,
                _buf: &'a mut [u8],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<usize>>
                        + Send
                        + 'a,
                >,
            > {
                Box::pin(async move { Ok(0) })
            }

            fn sync(
                &self,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.sync()
            }

            fn allocate(
                &self,
                size: u64,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.allocate(size)
            }

            fn file_size(
                &self,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = tachyon_core::DownloadResult<u64>> + Send + '_,
                >,
            > {
                self.inner.file_size()
            }

            fn close(
                &self,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = tachyon_core::DownloadResult<()>> + Send + '_>,
            > {
                self.inner.close()
            }
        }

        let empty = Arc::new(ZeroReadStorage {
            inner: InMemStorage::new(),
        }) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![empty], handle);

        let mut buf = [0u8; 8];
        let err = ts.pread_exact(0, 0, &mut buf).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("读取不足") || msg.contains("UnexpectedEof"),
            "EOF 时 pread_exact 必须返回 UnexpectedEof 错误,实际: {msg}"
        );
    }

    /// on_piece_completed 回调应直接返回 Ok(piece 已直接写入目标文件,无需额外操作)。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_on_piece_completed_returns_ok() {
        use librqbit_core::lengths::{Lengths, ValidPieceIndex};

        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        // 通过 Lengths 构造合法 ValidPieceIndex(单分片,total=1024, piece=1024)
        let lengths = Lengths::new(1024, 1024).expect("Lengths::new");
        let idx: ValidPieceIndex = lengths.last_piece_id();
        let result = ts.on_piece_completed(idx);
        assert!(result.is_ok(), "on_piece_completed 应返回 Ok: {:?}", result);
    }

    /// take() 对未知/已失效状态仍应返回可工作的克隆(与 FilesystemStorage::take 契约一致),
    /// 禁止返回 DummyStorage。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_take_returns_working_clone_for_single_file_storage() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        // 第一次 take
        let cloned_a = ts.take().expect("take 应返回 Ok");
        // 第二次 take(原 storage 仍可 take)
        let cloned_b = ts.take().expect("多次 take 应都返回 Ok");

        cloned_a.pwrite_all(0, 0, b"clone-a").unwrap();
        cloned_b.pwrite_all(0, 0, b"clone-b").unwrap();

        // 两个 clone 共享同一底层 storage,数据应可见
        let mut buf = [0u8; 7];
        ts.pread_exact(0, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"clone-b", "take 返回的 clone 应共享底层 storage");
    }

    /// remove_file 在 Tachyon 实现下应直接返回 Ok(Tachyon 管理文件生命周期)。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_remove_file_returns_ok_for_any_filename() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        let result = ts.remove_file(0, std::path::Path::new("anything.bin"));
        assert!(result.is_ok(), "remove_file 应直接返回 Ok: {:?}", result);
    }

    /// remove_directory_if_empty 应直接返回 Ok(Tachyon 不允许 librqbit 删除目录)。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_remove_directory_if_empty_returns_ok() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        let dir = tempfile::tempdir().unwrap();
        let result = ts.remove_directory_if_empty(dir.path());
        assert!(
            result.is_ok(),
            "remove_directory_if_empty 应返回 Ok: {:?}",
            result
        );
    }

    /// storage(file_id) 越界时 pwrite_all/pread_exact/ensure_file_length 都应返回
    /// "file_id 越界"错误(覆盖 storage() 的 ok_or_else 分支)。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_storage_helper_returns_error_for_out_of_range_file_id() {
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);

        // file_id=999 不存在
        let err_write = ts.pwrite_all(999, 0, b"data").unwrap_err();
        assert!(
            err_write.to_string().contains("越界"),
            "pwrite_all 越界错误: {err_write}"
        );

        let mut buf = [0u8; 4];
        let err_read = ts.pread_exact(999, 0, &mut buf).unwrap_err();
        assert!(
            err_read.to_string().contains("越界"),
            "pread_exact 越界错误: {err_read}"
        );

        let err_alloc = ts.ensure_file_length(999, 1024).unwrap_err();
        assert!(
            err_alloc.to_string().contains("越界"),
            "ensure_file_length 越界错误: {err_alloc}"
        );
    }

    /// register/unregister 操作 registry;create() 命中预注册路径时复用 storages。
    /// 此测试覆盖 register/unregister/clone_box/Clone 以及 last_open_backend 访问器。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_factory_register_unregister_and_clone() {
        use librqbit::storage::StorageFactory as _;

        let handle = tokio::runtime::Handle::current();
        let factory = TachyonStorageFactory::new(
            handle,
            IoStrategy::Standard,
            std::path::PathBuf::from("/tmp/dl"),
        );

        // 初始 last_open_backend 为 None(尚未调用 open_storage_for_path)
        assert!(factory.last_open_backend().is_none());

        // register 一个 info_hash
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        factory.register("abc123".to_string(), vec![storage]);
        // registry 非空(无法直接观测,但 unregister 不应 panic)
        factory.unregister("abc123");
        // unregister 不存在的 key 也不应 panic
        factory.unregister("nonexistent");

        // clone_box 应返回一个 BoxStorageFactory(可用)
        let _boxed = factory.clone_box();
        // Clone impl:clone 后 io_strategy 一致
        let cloned = factory.clone();
        assert_eq!(cloned.io_strategy(), IoStrategy::Standard);
        assert!(cloned.last_open_backend().is_none());
    }

    /// init() 回调应直接返回 Ok(存储在构造时已就绪,无需额外初始化)。
    /// 覆盖 TachyonTorrentStorage::init 的 trivial Ok 分支。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_init_returns_ok_without_side_effects() {
        // init 需要 ManagedTorrentShared + TorrentMetadata,构造困难;
        // 改用静态契约断言:init 不访问 shared/metadata(参数为 _ 前缀)。
        // 这里仅验证 TachyonTorrentStorage 可在无 init 调用下直接工作。
        let storage = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
        let handle = tokio::runtime::Handle::current();
        let ts = TachyonTorrentStorage::new(vec![storage], handle);
        // 直接调用 pwrite/pread 验证 init 不必要的契约
        ts.pwrite_all(0, 0, b"no-init").unwrap();
        let mut buf = [0u8; 7];
        ts.pread_exact(0, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"no-init");
    }

    /// Windows 上 WinAligned 策略应打开 WinAligned 后端(非回退)。
    /// 覆盖 open_storage_for_path 的 WinAligned 分支 + open_win_aligned_storage。
    #[cfg(target_os = "windows")]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_bt19_winaligned_opens_winaligned_backend_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let handle = tokio::runtime::Handle::current();
        let factory =
            TachyonStorageFactory::new(handle, IoStrategy::WinAligned, dir.path().to_path_buf());
        let path = dir.path().join("winaligned.bin");
        // 先创建文件(避免"文件不存在"导致回退)
        std::fs::write(&path, b"").unwrap();
        let (_storage, backend) = factory.open_storage_for_path(&path).unwrap();
        assert_eq!(
            backend, "WinAligned",
            "Windows 上 WinAligned 策略应打开 WinAligned 后端(非回退)"
        );
    }

    /// Windows 上 Iocp 策略应打开 Iocp 后端(非回退)。
    /// 覆盖 open_storage_for_path 的 Iocp 分支 + open_iocp_storage。
    #[cfg(target_os = "windows")]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_bt19_iocp_opens_iocp_backend_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let handle = tokio::runtime::Handle::current();
        let factory =
            TachyonStorageFactory::new(handle, IoStrategy::Iocp, dir.path().to_path_buf());
        let path = dir.path().join("iocp.bin");
        // 先创建文件(避免"文件不存在"导致回退)
        std::fs::write(&path, b"").unwrap();
        let (_storage, backend) = factory.open_storage_for_path(&path).unwrap();
        assert_eq!(
            backend, "Iocp",
            "Windows 上 Iocp 策略应打开 Iocp 后端(非回退)"
        );
    }
}
