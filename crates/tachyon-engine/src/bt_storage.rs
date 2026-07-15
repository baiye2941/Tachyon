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
                let n = storage.read_at(offset, tmp_slice).await?;
                if n < len {
                    return Err(tachyon_core::DownloadError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("pread_exact: 读取不足 {n}/{len}"),
                    )));
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
            let written = storage.write_at(offset, data).await?;
            if written < len {
                return Err(tachyon_core::DownloadError::Io(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    format!("pwrite_all: 写入不足 {written}/{len}"),
                )));
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
        }
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
                // 与单文件分支一致使用 open_sync。
                let file = tachyon_io::TokioFile::open_sync(path)
                    .map_err(|e| anyhow::anyhow!("打开文件 {} 失败: {e}", path.display()))?;
                storages.push(Arc::new(file) as Arc<dyn AsyncStorage>);
            }
        } else {
            // 单文件:download_dir/<name>,与 init_storage 的单文件路径一致
            let final_path = self.download_dir.join(torrent_name);
            let canonical_path = tachyon_core::validate_save_path(&final_path, &self.download_dir)
                .map_err(|e| anyhow::anyhow!("单文件路径校验失败: {e}"))?;
            let file = tachyon_io::TokioFile::open_sync(&canonical_path)
                .map_err(|e| anyhow::anyhow!("打开文件 {} 失败: {e}", canonical_path.display()))?;
            storages.push(Arc::new(file) as Arc<dyn AsyncStorage>);
        }
        Ok(storages)
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
        // 静态不变量: open_storages_from_metadata 多文件分支使用 open_sync
        // (源码契约;避免 async runtime 内嵌套 block_on)。
        let src = include_str!("bt_storage.rs");
        assert!(
            src.contains(
                "// 多文件 factory 在 librqbit async 上下文同步 create;禁止嵌套 Handle::block_on。"
            ),
            "多文件打开路径应保留禁止嵌套 block_on 的契约注释"
        );
        // 在 open_storages_from_metadata 函数体内不应再出现 block_on(TokioFile::open
        let start = src
            .find("fn open_storages_from_metadata")
            .expect("open_storages_from_metadata");
        let body = &src[start..start + 2500];
        assert!(
            !body.contains("block_on(tachyon_io::TokioFile::open"),
            "open_storages_from_metadata 不得嵌套 block_on 打开文件"
        );
        assert!(
            body.contains("TokioFile::open_sync"),
            "多文件/单文件应使用 open_sync"
        );
    }
}
