//! 下载任务执行器
//!
//! 将协议层、I/O 层、校验层串联为完整的下载编排流程:
//! 1. `probe`  -- 探测文件元数据
//! 2. `plan`   -- 规划分片
//! 3. `prepare_storage` -- 预分配文件空间
//! 4. `execute` -- 并发下载全部分片
//! 5. `verify`  -- 校验完整性
//!
//! `run()` 方法一键执行上述全部步骤。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use tachyon_core::config::DownloadConfig;
use tachyon_core::traits::{DownloadScheduler, Protocol, Verifier};
use tachyon_core::types::{DownloadState, FileMetadata, FragmentInfo, TaskId};
use tachyon_core::{ByteStream, DownloadError, DownloadResult};
use tachyon_crypto::CpuVerifier;
use tachyon_io::TokioFile;
use tachyon_io::storage::AsyncStorage;
use tachyon_protocol::http::HttpClient;
use tachyon_scheduler::AdaptiveDownloadScheduler;

/// 类型擦除的校验器,通过 Arc<dyn Verifier> 实现动态分发。
/// 添加新校验后端只需实现 Verifier trait,无需修改引擎层枚举。
pub type VerifierKind = Arc<dyn Verifier>;

/// 创建默认的 blake3 CPU 校验器
pub fn default_blake3_verifier() -> VerifierKind {
    Arc::new(CpuVerifier::blake3())
}

pub type StorageKind = DynStorage;

use crate::connection::ConnectionPool;
use crate::fragment::FragmentRecord;
use crate::orchestrator::DownloadOrchestrator;

#[cfg(test)]
use tachyon_core::test_harness::harness::MemoryStorage as MemStorage;
#[cfg(test)]
use tachyon_core::test_harness::harness::MockProtocol as MockProto;

// ---------------------------------------------------------------------------
// DynStorage: 类型擦除存储包装器
// ---------------------------------------------------------------------------

trait ErasedStorage: Send + Sync {
    fn write_at_erased(
        &self,
        offset: u64,
        data: Bytes,
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
}

impl<S: AsyncStorage + 'static> ErasedStorage for S {
    fn write_at_erased(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(self.write_at(offset, data))
    }

    fn read_at_erased<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(self.read_at(offset, buf))
    }

    fn allocate_erased(
        &self,
        size: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(self.allocate(size))
    }

    fn sync_erased(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(self.sync())
    }

    fn file_size_erased(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
        Box::pin(self.file_size())
    }
}

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

    /// 从 Arc 包装的 AsyncStorage 创建
    pub fn from_arc<S: AsyncStorage + 'static>(storage: Arc<S>) -> Self {
        Self(storage)
    }

    /// 打开或创建 TokioFile 存储
    async fn open(path: &std::path::Path) -> DownloadResult<Self> {
        let storage = TokioFile::open(path).await?;
        Ok(Self::new(storage))
    }

    /// 写入数据到指定偏移
    pub async fn write_at(&self, offset: u64, data: Bytes) -> DownloadResult<usize> {
        self.0.write_at_erased(offset, data).await
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

#[cfg(test)]
struct AsyncMemWrapper(MemStorage);

#[cfg(test)]
impl AsyncStorage for AsyncMemWrapper {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> impl std::future::Future<Output = DownloadResult<usize>> + Send {
        use tachyon_core::traits::Storage;
        self.0.write_at(offset, data)
    }

    fn read_at(
        &self,
        offset: u64,
        buf: &mut [u8],
    ) -> impl std::future::Future<Output = DownloadResult<usize>> + Send {
        use tachyon_core::traits::Storage;
        self.0.read_at(offset, buf)
    }

    fn sync(&self) -> impl std::future::Future<Output = DownloadResult<()>> + Send {
        use tachyon_core::traits::Storage;
        self.0.sync()
    }

    fn allocate(&self, size: u64) -> impl std::future::Future<Output = DownloadResult<()>> + Send {
        use tachyon_core::traits::Storage;
        self.0.allocate(size)
    }

    fn file_size(&self) -> impl std::future::Future<Output = DownloadResult<u64>> + Send {
        use tachyon_core::traits::Storage;
        self.0.file_size()
    }

    fn close(&self) -> impl std::future::Future<Output = DownloadResult<()>> + Send {
        use tachyon_core::traits::Storage;
        self.0.close()
    }
}

#[cfg(test)]
impl DynStorage {
    fn memory() -> Self {
        Self::new(AsyncMemWrapper(MemStorage::new()))
    }

    fn memory_with_capacity(cap: usize) -> Self {
        Self::new(AsyncMemWrapper(MemStorage::with_capacity(cap)))
    }
}

// ---------------------------------------------------------------------------
// FragmentProgress: 分片进度回调消息
// ---------------------------------------------------------------------------

/// 分片进度回调消息
///
/// 通过 `progress_tx` 通道发送给上层(tachyon-app),用于:
/// - `completed == false`:增量进度更新(每写一个 chunk 发一次)
/// - `completed == true`:分片整体下载完成,触发上层 checkpoint 落盘(断点续传)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentProgress {
    /// 分片索引
    pub fragment_index: u32,
    /// 该分片是否已整体完成
    pub completed: bool,
    /// 该分片当前已下载字节数
    pub fragment_downloaded: u64,
}

// ---------------------------------------------------------------------------
// MirrorProtocol: 多源下载适配器
// ---------------------------------------------------------------------------

/// 多镜像源 Protocol 适配器
///
/// 包装主源和备用源列表,在分片下载失败时自动 fallback 到下一个可用源。
/// 每个源独立重试,所有源均不可用时上报原始错误。
struct MirrorProtocol {
    /// 主下载源
    primary: Arc<dyn Protocol>,
    /// 备用镜像源列表 (url, protocol)
    mirrors: Vec<(String, Arc<dyn Protocol>)>,
}

impl MirrorProtocol {
    fn new(primary: Arc<dyn Protocol>, mirrors: Vec<(String, Arc<dyn Protocol>)>) -> Self {
        Self { primary, mirrors }
    }
}

impl Protocol for MirrorProtocol {
    fn probe(
        &self,
        url: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>>
    {
        let primary = self.primary.clone();
        let url = url.to_string();
        Box::pin(async move { primary.probe(&url).await })
    }

    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let primary = self.primary.clone();
        let mirrors = self.mirrors.clone();
        let url = url.to_string();
        Box::pin(async move {
            match primary.download_range(&url, start, end).await {
                Ok(data) => Ok(data),
                Err(e) => {
                    for (mirror_url, mirror_proto) in &mirrors {
                        tracing::info!(url = %mirror_url, "主源失败,尝试备用镜像");
                        if let Ok(data) = mirror_proto.download_range(mirror_url, start, end).await
                        {
                            return Ok(data);
                        }
                    }
                    Err(e)
                }
            }
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
    {
        let primary = self.primary.clone();
        let mirrors = self.mirrors.clone();
        let url = url.to_string();
        Box::pin(async move {
            match primary.download_range_stream(&url, start, end).await {
                Ok(stream) => Ok(stream),
                Err(e) => {
                    for (mirror_url, mirror_proto) in &mirrors {
                        tracing::info!(url = %mirror_url, "主源失败,尝试备用镜像(流式)");
                        if let Ok(stream) = mirror_proto
                            .download_range_stream(mirror_url, start, end)
                            .await
                        {
                            return Ok(stream);
                        }
                    }
                    Err(e)
                }
            }
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let primary = self.primary.clone();
        let url = url.to_string();
        Box::pin(async move { primary.download_full(&url).await })
    }
}

// ---------------------------------------------------------------------------
// DownloadTask: 下载任务执行器
// ---------------------------------------------------------------------------

/// 单个下载任务的执行器
///
/// 串联协议层、存储层、校验层,提供完整的下载编排流程。
/// 支持自适应调度器,根据带宽预测动态调整并发度和分片大小。
/// 存储延迟初始化:在 `probe()` 获取真实文件名后,通过 `init_storage()`
/// 配合 `validate_save_path()` 纵深防御创建存储。
pub struct DownloadTask {
    pub id: TaskId,
    pub url: String,
    pub config: DownloadConfig,
    protocol: Arc<dyn Protocol>,
    /// 延迟初始化:probe() 后通过 init_storage() 创建
    storage: Option<Arc<DynStorage>>,
    orchestrator: DownloadOrchestrator,
    scheduler: Arc<dyn DownloadScheduler>,
    pool: Option<Arc<ConnectionPool>>,
    control_rx: Option<watch::Receiver<DownloadState>>,
    state: DownloadState,
    metadata: Option<FileMetadata>,
    fragments: Vec<FragmentRecord>,
    progress_tx: Option<tokio::sync::mpsc::Sender<FragmentProgress>>,
    #[allow(dead_code)]
    verifier: VerifierKind,
    completed_fragments: Vec<u32>,
}

impl DownloadTask {
    /// 创建新的下载任务
    ///
    /// 根据 URL scheme 自动选择协议后端,使用默认 blake3 校验器和自适应调度器。
    /// 存储文件位于 `config.download_dir` 目录下,文件名在 `probe` 阶段确定。
    pub async fn new(url: String, config: DownloadConfig) -> DownloadResult<Self> {
        Self::with_scheduler(
            url,
            config,
            Arc::new(AdaptiveDownloadScheduler::default_config()),
        )
        .await
    }

    /// 使用指定调度器创建下载任务
    pub async fn with_scheduler(
        url: String,
        config: DownloadConfig,
        scheduler: Arc<dyn DownloadScheduler>,
    ) -> DownloadResult<Self> {
        Self::with_pool_and_scheduler(url, config, None, scheduler).await
    }

    pub async fn with_pool(
        url: String,
        config: DownloadConfig,
        pool: Option<Arc<ConnectionPool>>,
    ) -> DownloadResult<Self> {
        Self::with_pool_and_scheduler(
            url,
            config,
            pool,
            Arc::new(AdaptiveDownloadScheduler::default_config()),
        )
        .await
    }

    pub async fn with_pool_and_scheduler(
        url: String,
        config: DownloadConfig,
        pool: Option<Arc<ConnectionPool>>,
        scheduler: Arc<dyn DownloadScheduler>,
    ) -> DownloadResult<Self> {
        let _parsed = url::Url::parse(&url)?;

        let protocol: Arc<dyn Protocol> =
            if url.starts_with("http://") || url.starts_with("https://") {
                // 注入超时:connect 超时防"连不上"(黑洞 IP),
                // read 超时防"连上后静默断流"。read 用配置的 request_timeout_secs,
                // 它限制的是单次读取空闲间隔上限,不会误杀正常的大文件长下载。
                Arc::new(HttpClient::with_timeouts(
                    config.connect_timeout_secs,
                    config.request_timeout_secs,
                )?)
            } else {
                return Err(DownloadError::Config(format!("不支持的协议: {url}")));
            };

        // 存储延迟到 probe() 之后初始化,使用真实文件名 + validate_save_path
        let orchestrator = match &pool {
            Some(p) => DownloadOrchestrator::with_shared_pool(p.clone(), Default::default()),
            None => DownloadOrchestrator::new(Default::default()),
        };

        Ok(Self {
            id: TaskId::new_v4(),
            url,
            config,
            protocol,
            storage: None,
            orchestrator,
            scheduler,
            pool,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
        })
    }

    /// 使用主 URL + 备用镜像 URL 创建下载任务
    ///
    /// 主源失败时自动 fallback 到镜像源列表。
    /// 所有源共享同一个连接池。
    pub async fn with_mirrors(
        url: String,
        mirror_urls: Vec<String>,
        config: DownloadConfig,
    ) -> DownloadResult<Self> {
        let primary = Arc::new(HttpClient::with_timeouts(
            config.connect_timeout_secs,
            config.request_timeout_secs,
        )?);

        let mirrors: Vec<(String, Arc<dyn Protocol>)> = mirror_urls
            .iter()
            .filter_map(|m| {
                HttpClient::with_timeouts(config.connect_timeout_secs, config.request_timeout_secs)
                    .ok()
                    .map(|c| (m.clone(), Arc::new(c) as Arc<dyn Protocol>))
            })
            .collect();

        let protocol = Arc::new(MirrorProtocol::new(primary, mirrors));
        let orchestrator = DownloadOrchestrator::new(Default::default());

        Ok(Self {
            id: TaskId::new_v4(),
            url,
            config,
            protocol,
            storage: None,
            orchestrator,
            scheduler: Arc::new(AdaptiveDownloadScheduler::default_config()),
            pool: None,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
        })
    }

    #[cfg(test)]
    fn new_for_test(
        url: String,
        config: DownloadConfig,
        protocol: Arc<dyn Protocol>,
        storage: StorageKind,
    ) -> Self {
        Self {
            id: TaskId::new_v4(),
            url,
            config,
            protocol,
            storage: Some(Arc::new(storage)),
            orchestrator: DownloadOrchestrator::new(Default::default()),
            scheduler: Arc::new(AdaptiveDownloadScheduler::default_config()),
            pool: None,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
        }
    }

    pub fn set_control_rx(&mut self, control_rx: watch::Receiver<DownloadState>) {
        self.control_rx = Some(control_rx);
    }

    pub fn set_progress_sender(&mut self, tx: tokio::sync::mpsc::Sender<FragmentProgress>) {
        self.progress_tx = Some(tx);
    }

    /// 设置已完成分片索引列表(断点续传)
    ///
    /// 必须在 `plan()` 之前调用。`plan()` 会据此把对应分片标记为已完成并跳过下载。
    pub fn set_completed_fragments(&mut self, completed: Vec<u32>) {
        self.completed_fragments = completed;
    }

    async fn wait_control_rx(
        rx: &mut watch::Receiver<DownloadState>,
        pause_timeout: Duration,
    ) -> DownloadResult<()> {
        loop {
            let state = *rx.borrow_and_update();
            match state {
                DownloadState::Cancelled => return Err(DownloadError::Cancelled),
                DownloadState::Failed => return Err(DownloadError::Other("任务已失败".into())),
                DownloadState::Paused => {
                    tokio::time::timeout(pause_timeout, rx.changed())
                        .await
                        .map_err(|_| {
                            DownloadError::Timeout(format!(
                                "暂停超过 {} 秒",
                                pause_timeout.as_secs()
                            ))
                        })?
                        .map_err(|_| DownloadError::Other("控制通道已关闭".into()))?;
                }
                _ => return Ok(()),
            }
        }
    }

    async fn wait_control(
        control_rx: &mut Option<watch::Receiver<DownloadState>>,
        pause_timeout: Duration,
    ) -> DownloadResult<()> {
        if let Some(rx) = control_rx.as_mut() {
            Self::wait_control_rx(rx, pause_timeout).await?;
        }
        Ok(())
    }

    /// 在下载进行期间监视中断信号(取消/暂停),供 `tokio::select!` 分支使用。
    ///
    /// 与 `wait_control_rx` 的关键区别:正常运行状态(Downloading 等)下**不会立即返回**,
    /// 而是挂起等待状态变化,因此不会在 `select!` 中抢占正在进行的下载分支。
    /// 只有在出现 Cancelled/Failed 时返回 `Err`,出现 Paused 时按暂停语义阻塞/超时。
    /// 控制通道关闭时返回错误,避免任务永久挂起。
    async fn watch_for_interrupt(
        rx: &mut watch::Receiver<DownloadState>,
        pause_timeout: Duration,
    ) -> DownloadResult<()> {
        loop {
            let state = *rx.borrow_and_update();
            match state {
                DownloadState::Cancelled => return Err(DownloadError::Cancelled),
                DownloadState::Failed => return Err(DownloadError::Other("任务已失败".into())),
                DownloadState::Paused => {
                    tokio::time::timeout(pause_timeout, rx.changed())
                        .await
                        .map_err(|_| {
                            DownloadError::Timeout(format!(
                                "暂停超过 {} 秒",
                                pause_timeout.as_secs()
                            ))
                        })?
                        .map_err(|_| DownloadError::Other("控制通道已关闭".into()))?;
                }
                _ => {
                    if rx.changed().await.is_err() {
                        return Err(DownloadError::Other("控制通道意外关闭".into()));
                    }
                }
            }
        }
    }

    fn request_host(&self) -> DownloadResult<String> {
        let parsed = url::Url::parse(&self.url)?;
        parsed
            .host_str()
            .map(ToString::to_string)
            .ok_or_else(|| DownloadError::Config("URL 主机为空".into()))
    }

    // ----- 步骤 1: 探测 -----

    /// 探测文件元数据
    ///
    /// 向服务端发送 HEAD 请求,获取文件名、大小、Range 支持等信息。
    /// 如果元数据已缓存(例如 task_fn 已调用过),直接返回缓存值,避免重复网络请求。
    pub async fn probe(&mut self) -> DownloadResult<&FileMetadata> {
        if let Some(ref meta) = self.metadata {
            return Ok(meta);
        }
        info!(url = %self.url, "开始探测文件元数据");
        let metadata = self.protocol.probe(&self.url).await?;
        info!(
            file_name = %metadata.file_name,
            file_size = ?metadata.file_size,
            supports_range = metadata.supports_range,
            "探测完成"
        );
        self.metadata = Some(metadata);
        self.metadata
            .as_ref()
            .ok_or_else(|| DownloadError::Config("探测完成但元数据未填充".into()))
    }

    /// 初始化存储(延迟到 probe() 之后)
    ///
    /// 使用 metadata 中的真实文件名构造保存路径,
    /// 并通过 `validate_save_path()` 做纵深防御校验。
    async fn init_storage(&mut self) -> DownloadResult<()> {
        if self.storage.is_some() {
            return Ok(());
        }

        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| DownloadError::Config("必须先调用 probe() 获取文件元数据".into()))?;

        let safe_name = &metadata.file_name;
        let download_dir = std::path::Path::new(&self.config.download_dir);
        let final_path = download_dir.join(safe_name);

        // 纵深防御:校验路径不逃逸下载目录
        let canonical_path = tachyon_core::validate_save_path(&final_path, download_dir)?;

        info!(
            safe_name = %safe_name,
            save_path = %canonical_path.display(),
            "路径安全校验通过,创建存储"
        );

        let storage = DynStorage::open(&canonical_path).await?;
        self.storage = Some(Arc::new(storage));
        Ok(())
    }

    // ----- 步骤 2: 规划分片 -----

    /// 根据已探测的文件元数据规划分片
    ///
    /// 调用编排器计算最优分片策略,生成分片列表并存入内部状态。
    /// 使用调度器的带宽预测动态调整分片大小。
    /// 必须在 `probe()` 之后调用。
    pub fn plan(&mut self) -> DownloadResult<Vec<FragmentInfo>> {
        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| DownloadError::Config("必须先调用 probe() 获取文件元数据".into()))?;

        let file_size = metadata.file_size.unwrap_or(0);

        // 使用调度器获取分片大小建议
        let recommendation = self
            .scheduler
            .recommend(file_size, self.config.max_concurrent_fragments);

        debug!(
            predicted_bandwidth = self.scheduler.predicted_bandwidth(),
            recommended_fragment_size = recommendation.fragment_size,
            recommended_concurrency = recommendation.concurrency,
            confidence = recommendation.confidence,
            "调度器建议"
        );

        let suggested_frag_size = if recommendation.confidence > 0.0 {
            Some(recommendation.fragment_size)
        } else {
            None
        };

        let fragments = self.orchestrator.plan_fragments(
            file_size,
            metadata.supports_range,
            suggested_frag_size,
        );

        info!(count = fragments.len(), "分片规划完成");

        self.fragments = fragments
            .iter()
            .map(|info| FragmentRecord::new(info.clone(), self.config.max_retries))
            .collect();

        // 断点续传:把已完成分片标记为 Done 并跳过后续下载
        if !self.completed_fragments.is_empty() {
            let mut resumed = 0u32;
            for &done_index in &self.completed_fragments {
                if let Some(frag) = self.fragments.get_mut(done_index as usize) {
                    // 仅对仍处于 Pending 的分片执行恢复,避免重复迁移状态
                    if frag.state == crate::fragment::FragmentState::Pending {
                        frag.info.downloaded = frag.info.size;
                        frag.start_download();
                        frag.complete_download_fast(frag.info.size, Duration::ZERO);
                        resumed += 1;
                    }
                }
            }
            info!(resumed, "断点续传:跳过已完成分片");
        }

        Ok(fragments)
    }

    // ----- 步骤 3: 预分配存储 -----

    /// 预分配文件空间
    ///
    /// 根据文件大小在存储后端预留空间,支持分片并发写入。
    pub async fn prepare_storage(&self) -> DownloadResult<()> {
        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| DownloadError::Config("必须先调用 probe() 获取文件元数据".into()))?;

        let size = metadata.file_size.unwrap_or(0);
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?;
        if size > 0 {
            storage.allocate(size).await?;
            debug!(size, "存储空间预分配完成");
        }
        Ok(())
    }

    // ----- 步骤 4: 并发执行下载 -----

    /// 执行全部分片下载
    ///
    /// 根据配置的最大并发数使用信号量控制并发,每个分片独立下载并写入存储。
    /// 不支持 Range 请求时退化为整块下载。
    #[tracing::instrument(skip(self), fields(task_id = %self.id))]
    pub async fn execute(&mut self) -> DownloadResult<()> {
        self.state = DownloadState::Downloading;
        info!("开始执行下载任务");

        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| DownloadError::Config("必须先调用 probe()".into()))?;

        let supports_range = metadata.supports_range;
        let file_size = metadata.file_size.unwrap_or(0);

        // 空文件无需下载
        if file_size == 0 {
            self.state = DownloadState::Completed;
            info!("文件大小为 0,跳过下载");
            return Ok(());
        }

        // 不支持 Range:整块下载
        if !supports_range || self.fragments.len() <= 1 {
            return self.execute_full_download().await;
        }

        // 支持 Range:并发分片下载
        self.execute_fragmented_download().await
    }

    /// 整块下载(不支持 Range 或单分片)
    ///
    /// 以流式方式逐块写入存储,峰值内存仅含单个 chunk,避免大文件整块进内存。
    async fn execute_full_download(&mut self) -> DownloadResult<()> {
        let pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        Self::wait_control(&mut self.control_rx, pause_timeout).await?;
        let host = self.request_host()?;
        let _pool_permit = match &self.pool {
            Some(pool) => Some(pool.acquire(&host).await?),
            None => None,
        };
        let start_instant = std::time::Instant::now();

        // 获取流式响应(控制信号可在建立连接阶段中断)
        let stream = if let Some(rx) = self.control_rx.as_mut() {
            tokio::select! {
                result = self.protocol.download_full_stream(&self.url) => result?,
                control = Self::watch_for_interrupt(rx, pause_timeout) => {
                    control?;
                    return Err(DownloadError::Other("控制信号异常结束".into()));
                }
            }
        } else {
            self.protocol.download_full_stream(&self.url).await?
        };

        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?;

        // 逐块消费并写入,顺序追加偏移
        let mut pos: u64 = 0;
        tokio::pin!(stream);
        while let Some(chunk_result) = tokio_stream::StreamExt::next(&mut stream).await {
            if let Some(rx) = self.control_rx.as_mut() {
                Self::wait_control_rx(rx, pause_timeout).await?;
            }
            let chunk = chunk_result?;
            let written = storage.write_at(pos, chunk).await?;
            pos += written as u64;
        }
        debug!(written = pos, "整块流式下载写入完成");

        if let Some(md) = &self.metadata
            && let Some(expected_size) = md.file_size
            && pos != expected_size
        {
            return Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("下载数据不完整: 预期 {expected_size} 字节, 实际写入 {pos} 字节"),
            )));
        }

        if let Some(frag) = self.fragments.first_mut() {
            if frag.state == crate::fragment::FragmentState::Pending {
                frag.start_download();
            }
            frag.complete_download_fast(pos, start_instant.elapsed());
        }
        self.state = DownloadState::Completed;
        Ok(())
    }

    /// 并发分片下载
    ///
    /// 将信号量获取移入 spawn 任务内部,确保分片任务立即启动网络请求,
    /// 仅在实际占用并发槽位时才等待信号量,最大化网络并发。
    /// 使用调度器的带宽预测动态调整并发度。
    ///
    /// 每个分片 spawn 内部自带重试循环:单次尝试失败后按指数退避重试,
    /// 直到 `max_retries` 耗尽才整体失败。已完成的分片(断点续传)直接跳过。
    async fn execute_fragmented_download(&mut self) -> DownloadResult<()> {
        if self.config.max_concurrent_fragments == 0 {
            return Err(DownloadError::Config(
                "max_concurrent_fragments 不能为 0".to_string(),
            ));
        }

        // 使用调度器获取动态并发建议
        let file_size = self
            .metadata
            .as_ref()
            .and_then(|m| m.file_size)
            .unwrap_or(0);
        let recommendation = self
            .scheduler
            .recommend(file_size, self.config.max_concurrent_fragments);

        // 使用调度器建议的并发度,但不超过配置的最大值
        let effective_concurrency = recommendation
            .concurrency
            .min(self.config.max_concurrent_fragments)
            .max(1) as usize;

        info!(
            configured_concurrency = self.config.max_concurrent_fragments,
            recommended_concurrency = recommendation.concurrency,
            effective_concurrency = effective_concurrency,
            confidence = recommendation.confidence,
            "使用调度器并发建议"
        );

        let semaphore = Arc::new(Semaphore::new(effective_concurrency));
        let url = self.url.clone();
        let storage = self
            .storage
            .clone()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?;
        let protocol = self.protocol.clone();
        let pool = self.pool.clone();
        let host = self.request_host()?;
        let pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        let control_rx = self.control_rx.clone();
        let progress_tx = self.progress_tx.clone();
        let max_retries = self.config.max_retries;
        tracing::info!(
            has_progress_tx = progress_tx.is_some(),
            frag_count = self.fragments.len(),
            "分片下载准备就绪"
        );

        // spawn 成功返回 (index, downloaded, duration);失败返回 (index, error)
        type FragOk = (u32, u64, Duration);
        type FragErr = (u32, DownloadError);
        let mut handles: Vec<JoinHandle<Result<FragOk, FragErr>>> = Vec::new();

        // 仅对未完成(Pending)的分片下载,已完成分片(断点续传)跳过
        let fragment_specs: Vec<(u32, u64, u64)> = self
            .fragments
            .iter()
            .filter(|frag| frag.state == crate::fragment::FragmentState::Pending)
            .map(|frag| (frag.info.index, frag.info.start, frag.info.end))
            .collect();

        for (frag_index, frag_start, frag_end) in fragment_specs {
            let frag_url = url.clone();
            let frag_storage = storage.clone();
            let frag_protocol = protocol.clone();
            let frag_semaphore = semaphore.clone();
            let frag_pool = pool.clone();
            let frag_host = host.clone();
            let rate_limit_bps = self.config.rate_limit_bytes_per_sec;
            let frag_control_rx = control_rx.clone();
            let frag_progress_tx = progress_tx.clone();

            if frag_index as usize >= self.fragments.len() {
                return Err(DownloadError::Config("分片索引越界".into()));
            }
            self.fragments[frag_index as usize].start_download();

            let handle = tokio::spawn(async move {
                // 信号量获取移入 spawn 内部:分片任务立即启动,
                // 仅在需要实际占用并发槽位时才等待
                let permit = match frag_semaphore.acquire_owned().await {
                    Ok(p) => p,
                    Err(e) => {
                        return Err((
                            frag_index,
                            DownloadError::Other(format!("信号量获取失败: {e}").into()),
                        ));
                    }
                };

                // spawn 内部重试循环:单次尝试失败后指数退避重试,
                // 最多重试 max_retries 次(总尝试次数 max_retries + 1)。
                let mut attempt: u32 = 0;
                loop {
                    match Self::download_single_fragment(
                        &frag_protocol,
                        &frag_storage,
                        &frag_pool,
                        &frag_host,
                        &frag_url,
                        frag_index,
                        frag_start,
                        frag_end,
                        pause_timeout,
                        rate_limit_bps,
                        &frag_control_rx,
                        &frag_progress_tx,
                    )
                    .await
                    {
                        Ok((downloaded, duration)) => {
                            drop(permit);
                            return Ok((frag_index, downloaded, duration));
                        }
                        Err(e) => {
                            // 取消/暂停超时等控制类错误不重试,直接上报
                            if matches!(e, DownloadError::Cancelled | DownloadError::Timeout(_)) {
                                drop(permit);
                                return Err((frag_index, e));
                            }
                            // 权限错误(401/403)重试无意义,立即终止该分片
                            if matches!(e, DownloadError::Forbidden { .. }) {
                                drop(permit);
                                return Err((frag_index, e));
                            }
                            if attempt >= max_retries {
                                drop(permit);
                                return Err((frag_index, e));
                            }
                            // 退避时间:服务端限流(429/503)若给出 Retry-After 则优先采用,
                            // 否则回退到指数退避 1s, 2s, 4s, ...(上限 1024s)
                            let backoff = match &e {
                                DownloadError::Throttled {
                                    retry_after_secs: Some(secs),
                                } => Duration::from_secs((*secs).min(1024)),
                                _ => Duration::from_secs(1u64 << attempt.min(10)),
                            };
                            warn!(
                                index = frag_index,
                                attempt = attempt + 1,
                                max_retries,
                                backoff_secs = backoff.as_secs(),
                                error = %e,
                                "分片下载失败,退避后重试"
                            );
                            tokio::time::sleep(backoff).await;
                            attempt += 1;
                        }
                    }
                }
            });

            handles.push(handle);
        }

        for handle in handles {
            let result = handle
                .await
                .map_err(|e| DownloadError::Other(format!("分片任务 panic: {e}").into()))?;

            let (index, downloaded, duration) = match result {
                Ok(ok) => ok,
                Err((failed_index, e)) => {
                    // spawn 内部已耗尽重试,这里精确标记真正失败的分片为终态
                    if let Some(frag) = self.fragments.get_mut(failed_index as usize) {
                        frag.force_fail();
                    }
                    self.state = DownloadState::Failed;
                    return Err(e);
                }
            };

            let frag = &mut self.fragments[index as usize];
            frag.complete_download_fast(downloaded, duration);

            // 将带宽数据反馈给调度器
            if let Some(duration) = frag.last_duration {
                let bytes_per_sec = if duration.as_secs_f64() > 0.0 {
                    (downloaded as f64 / duration.as_secs_f64()) as u64
                } else {
                    0
                };
                if bytes_per_sec > 0 {
                    self.scheduler.observe_bandwidth(bytes_per_sec);
                    debug!(
                        index = index,
                        bytes_per_sec = bytes_per_sec,
                        "带宽数据已反馈给调度器"
                    );
                }
            }
        }

        self.storage.as_ref().unwrap().sync().await?;
        self.state = DownloadState::Completed;
        info!("全部分片下载完成");
        Ok(())
    }

    /// 下载单个分片(一次尝试)
    ///
    /// 由 `execute_fragmented_download` 的 spawn 重试循环调用。
    /// 成功返回 `(已写入字节数, 耗时)`;失败返回错误(由调用方决定是否重试)。
    /// 分片整体完成时通过 `progress_tx` 发送 `completed: true`,触发上层 checkpoint。
    #[allow(clippy::too_many_arguments)]
    async fn download_single_fragment(
        protocol: &Arc<dyn Protocol>,
        storage: &Arc<StorageKind>,
        pool: &Option<Arc<ConnectionPool>>,
        host: &str,
        url: &str,
        frag_index: u32,
        frag_start: u64,
        frag_end: u64,
        pause_timeout: Duration,
        rate_limit_bps: Option<u64>,
        control_rx: &Option<watch::Receiver<DownloadState>>,
        progress_tx: &Option<tokio::sync::mpsc::Sender<FragmentProgress>>,
    ) -> DownloadResult<(u64, Duration)> {
        let mut control_rx = control_rx.clone();

        // 真实 I/O 前检查暂停/取消
        if let Some(rx) = control_rx.as_mut() {
            Self::wait_control_rx(rx, pause_timeout).await?;
        }

        // 获取连接许可,持有到本次尝试结束(全局 + 单主机限流真实生效)
        let _pool_permit = match pool {
            Some(pool) => Some(pool.acquire(host).await?),
            None => None,
        };

        let start_instant = std::time::Instant::now();
        debug!(
            index = frag_index,
            start = frag_start,
            end = frag_end,
            "开始下载分片"
        );

        let stream = if let Some(rx) = control_rx.as_mut() {
            tokio::select! {
                result = protocol.download_range_stream(url, frag_start, frag_end) => result?,
                control = Self::watch_for_interrupt(rx, pause_timeout) => {
                    control?;
                    return Err(DownloadError::Other("控制信号异常结束".into()));
                }
            }
        } else {
            protocol
                .download_range_stream(url, frag_start, frag_end)
                .await?
        };

        let mut pos = frag_start;
        let mut total_written: u64 = 0;
        let mut chunk_count: u64 = 0;
        // 批量写入缓冲区:累积碎片,达到阈值后一次性写入
        const WRITE_BATCH_BYTES: usize = 256 * 1024; // 256KB 批量写入阈值
        let mut write_buf = bytes::BytesMut::with_capacity(WRITE_BATCH_BYTES);
        tokio::pin!(stream);
        while let Some(chunk_result) = tokio_stream::StreamExt::next(&mut stream).await {
            if let Some(rx) = control_rx.as_mut() {
                Self::wait_control_rx(rx, pause_timeout).await?;
            }
            let chunk = chunk_result?;
            write_buf.extend_from_slice(&chunk);
            chunk_count += 1;
            // 达到阈值时批量刷写
            if write_buf.len() >= WRITE_BATCH_BYTES {
                let batch = write_buf.split().freeze();
                let written = storage.write_at(pos, batch).await?;
                let w = written as u64;
                pos += w;
                total_written += w;
                if let Some(tx) = progress_tx
                    && chunk_count.is_multiple_of(5)
                {
                    let _ = tx.try_send(FragmentProgress {
                        fragment_index: frag_index,
                        completed: false,
                        fragment_downloaded: total_written,
                    });
                    tracing::debug!(idx = frag_index, bytes = total_written, "进度事件已发送");
                }
            }
        }
        // 刷写剩余数据
        if !write_buf.is_empty() {
            let batch = write_buf.freeze();
            let written = storage.write_at(pos, batch).await?;
            let w = written as u64;
            total_written += w;
        }

        let elapsed = start_instant.elapsed();

        // 限速: 若配置了 rate_limit_bytes_per_sec, 确保实际速率不超过限制
        if let Some(limit) = rate_limit_bps
            && limit > 0
        {
            let expected_secs = total_written as f64 / limit as f64;
            let actual_secs = elapsed.as_secs_f64();
            if actual_secs < expected_secs {
                let sleep_dur = Duration::from_secs_f64(expected_secs - actual_secs);
                tokio::time::sleep(sleep_dur).await;
            }
        }

        // 分片整体完成回调:触发上层 checkpoint(断点续传落盘)
        if let Some(tx) = progress_tx {
            let _ = tx.try_send(FragmentProgress {
                fragment_index: frag_index,
                completed: true,
                fragment_downloaded: total_written,
            });
        }

        info!(
            index = frag_index,
            written = total_written as usize,
            elapsed_ms = elapsed.as_millis(),
            "分片下载完成"
        );
        Ok((total_written, elapsed))
    }

    // ----- 步骤 5: 校验 -----

    /// 校验已下载数据的完整性
    ///
    /// 遍历所有带哈希值的分片,从存储中读取数据并与预期哈希比对。
    /// 任一分片校验失败即返回 `false`。
    pub async fn verify(&mut self) -> DownloadResult<()> {
        if !self.config.verify_checksum {
            debug!("校验已禁用,跳过");
            return Ok(());
        }

        self.state = DownloadState::Verifying;
        info!("开始校验文件完整性");

        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?;

        for frag in &self.fragments {
            if let Some(ref expected_hash) = frag.info.hash {
                let chunk_size = 1024 * 1024;
                let mut offset = frag.info.start;
                let end = frag.info.start + frag.info.size;
                let mut buf = vec![0u8; chunk_size];
                let mut hasher = blake3::Hasher::new();

                while offset < end {
                    let read_len = ((end - offset).min(chunk_size as u64)) as usize;
                    let read = storage.read_at(offset, &mut buf[..read_len]).await?;
                    hasher.update(&buf[..read]);
                    offset += read as u64;
                }

                let computed = hasher.finalize().to_hex().to_string();

                if computed != *expected_hash {
                    warn!(
                        index = frag.info.index,
                        expected = %expected_hash,
                        actual = %computed,
                        "分片校验失败"
                    );
                    self.state = DownloadState::Failed;
                    return Err(DownloadError::ChecksumMismatch {
                        expected: expected_hash.clone(),
                        actual: computed,
                    });
                }
                debug!(index = frag.info.index, "分片校验通过");
            }
        }

        info!("文件完整性校验通过");
        Ok(())
    }

    // ----- 一键运行 -----

    /// 一键执行完整下载流程
    ///
    /// 依次执行: 探测 -> 规划 -> 预分配 -> 下载 -> 校验
    /// 任一步骤失败将标记任务为 `Failed` 并返回错误。
    #[tracing::instrument(skip(self), fields(url = %self.url))]
    pub async fn run(&mut self) -> DownloadResult<()> {
        info!(url = %self.url, "启动下载任务");

        let result = self.run_inner().await;

        if let Err(error) = &result {
            self.apply_terminal_error(error);
            warn!(state = ?self.state, error = %error, "下载任务结束为非成功状态");
        }

        result
    }

    fn apply_terminal_error(&mut self, error: &DownloadError) {
        if matches!(error, DownloadError::Cancelled) || self.state == DownloadState::Cancelled {
            self.state = DownloadState::Cancelled;
        } else {
            self.state = DownloadState::Failed;
        }
    }

    /// 内部执行逻辑,便于 run() 统一处理错误状态
    async fn run_inner(&mut self) -> DownloadResult<()> {
        // 步骤 1: 探测 (与取消信号竞速: HEAD 请求可能长时间挂起)
        {
            let mut rx = self.control_rx.take();
            match rx.as_mut() {
                Some(rx) => {
                    tokio::select! {
                        r = self.probe() => { r?; }
                        _ = Self::wait_for_cancel(rx) => {
                            self.state = DownloadState::Cancelled;
                            return Err(DownloadError::Cancelled);
                        }
                    }
                }
                None => {
                    self.probe().await?;
                }
            }
            self.control_rx = rx;
        }

        // 步骤 1.5: 初始化存储
        self.init_storage().await?;

        // 步骤 2: 规划分片 (纯 CPU, 不阻塞)
        self.check_cancelled()?;
        self.plan()?;

        // 步骤 3: 预分配存储 (与取消信号竞速)
        {
            let mut rx = self.control_rx.take();
            match rx.as_mut() {
                Some(rx) => {
                    tokio::select! {
                        r = self.prepare_storage() => { r?; }
                        _ = Self::wait_for_cancel(rx) => {
                            self.state = DownloadState::Cancelled;
                            return Err(DownloadError::Cancelled);
                        }
                    }
                }
                None => {
                    self.prepare_storage().await?;
                }
            }
            self.control_rx = rx;
        }

        // 步骤 4: 执行下载 (内部已有 cancel/pause 中断处理)
        self.execute().await?;

        // 步骤 5: 校验 (与取消信号竞速)
        {
            let mut rx = self.control_rx.take();
            match rx.as_mut() {
                Some(rx) => {
                    tokio::select! {
                        r = self.verify() => { r?; }
                        _ = Self::wait_for_cancel(rx) => {
                            self.state = DownloadState::Cancelled;
                            return Err(DownloadError::Cancelled);
                        }
                    }
                }
                None => {
                    self.verify().await?;
                }
            }
            self.control_rx = rx;
        }

        self.state = DownloadState::Completed;
        info!("下载任务完成");
        Ok(())
    }

    /// 检查是否已被取消,若已取消则立即返回错误
    fn check_cancelled(&self) -> DownloadResult<()> {
        if let Some(rx) = &self.control_rx
            && matches!(*rx.borrow(), DownloadState::Cancelled)
        {
            return Err(DownloadError::Cancelled);
        }
        Ok(())
    }

    /// 等待取消信号 (仅关注 Cancelled 状态)
    async fn wait_for_cancel(rx: &mut watch::Receiver<DownloadState>) {
        loop {
            if matches!(*rx.borrow_and_update(), DownloadState::Cancelled) {
                return;
            }
            if rx.changed().await.is_err() {
                return; // 通道关闭
            }
        }
    }

    // ----- 状态查询 -----

    /// 获取当前下载进度(0.0 ~ 1.0)
    pub fn progress(&self) -> f64 {
        // 已完成的任务进度为 1.0
        if self.state == DownloadState::Completed {
            return 1.0;
        }
        if self.fragments.is_empty() {
            // 无分片:如果已知文件大小为 0 则视为完成
            if let Some(ref meta) = self.metadata
                && meta.file_size == Some(0)
            {
                return 1.0;
            }
            return 0.0;
        }
        let total: u64 = self.fragments.iter().map(|f| f.info.size).sum();
        if total == 0 {
            return 1.0;
        }
        let downloaded: u64 = self.fragments.iter().map(|f| f.info.downloaded).sum();
        downloaded as f64 / total as f64
    }

    /// 获取当前状态
    pub fn state(&self) -> DownloadState {
        self.state
    }

    /// 获取文件元数据(需先调用 probe)
    pub fn metadata(&self) -> Option<&FileMetadata> {
        self.metadata.as_ref()
    }

    /// 获取分片信息(需先调用 plan)
    pub fn fragment_infos(&self) -> Vec<FragmentInfo> {
        self.fragments.iter().map(|f| f.info.clone()).collect()
    }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::FragmentState;
    use bytes::Bytes;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
    use std::time::Duration;
    use tachyon_core::test_harness::harness::{test_config, test_metadata};
    use tachyon_core::traits::{ByteStream, Verifier as VerifierTrait};

    /// 辅助函数:创建带 mock 协议和存储的测试任务
    fn make_task(
        protocol: Arc<dyn Protocol>,
        storage: StorageKind,
        config: DownloadConfig,
    ) -> DownloadTask {
        DownloadTask::new_for_test(
            "http://example.com/file.bin".into(),
            config,
            protocol,
            storage,
        )
    }

    // ------ 1. DownloadTask::new 正确初始化 -----

    #[tokio::test]
    async fn test_new_initializes_fields() {
        let config = test_config();
        let task = DownloadTask::new("http://example.com/test.bin".into(), config)
            .await
            .expect("创建任务失败");

        assert_eq!(task.state(), DownloadState::Pending);
        assert_eq!(task.url, "http://example.com/test.bin");
        assert!(task.metadata().is_none());
        assert!(task.fragment_infos().is_empty());
        assert!((task.progress() - 0.0).abs() < f64::EPSILON);
    }

    // ------ 2. probe 获取元数据 -----

    #[tokio::test]
    async fn test_probe_fetches_metadata() {
        let meta = test_metadata("data.zip", 2048);
        let protocol = Arc::new(MockProto::new(meta.clone()));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        let result = task.probe().await;
        assert!(result.is_ok());

        let m = result.unwrap();
        assert_eq!(m.file_name, "data.zip");
        assert_eq!(m.file_size, Some(2048));
        assert!(m.supports_range);
    }

    #[tokio::test]
    async fn test_probe_propagates_error() {
        let protocol = Arc::new(MockProto::failing(DownloadError::Network(
            "连接超时".into(),
        )));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        let result = task.probe().await;
        assert!(result.is_err());
    }

    // ------ 3. plan 根据元数据生成分片 -----

    #[tokio::test]
    async fn test_plan_generates_fragments() {
        let meta = test_metadata("large.bin", 10_000);
        let protocol = Arc::new(MockProto::new(meta));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        task.probe().await.unwrap();
        let frags = task.plan().unwrap();

        assert!(!frags.is_empty());
        // 所有分片覆盖完整文件
        let total: u64 = frags.iter().map(|f| f.size).sum();
        assert_eq!(total, 10_000);
        // 内部状态同步
        assert_eq!(task.fragment_infos().len(), frags.len());
    }

    #[test]
    fn test_plan_without_probe_fails() {
        let protocol = Arc::new(MockProto::new(test_metadata("f.bin", 100)));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        // 未调用 probe,直接 plan 应报错
        let result = task.plan();
        assert!(result.is_err());
    }

    // ------ 4. prepare_storage 预分配空间 -----

    #[tokio::test]
    async fn test_prepare_storage_allocates() {
        let file_size = 4096u64;
        let meta = test_metadata("alloc.bin", file_size);
        let protocol = Arc::new(MockProto::new(meta));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        task.probe().await.unwrap();
        task.prepare_storage().await.unwrap();

        // 验证内存存储已分配
        if let Some(ref storage) = task.storage {
            assert_eq!(storage.file_size().await.unwrap(), file_size);
        }
    }

    // ------ 5. 完整 run 流程(使用 mock) -----

    #[tokio::test]
    async fn test_run_full_flow_with_mock() {
        let frag_size = 334u64;
        let total_size = frag_size * 3;

        // 构造分片数据
        let frag_a = Bytes::from(vec![0xAA; frag_size as usize]);
        let frag_b = Bytes::from(vec![0xBB; frag_size as usize]);
        let frag_c = Bytes::from(vec![0xCC; frag_size as usize]);

        let meta = FileMetadata {
            file_name: "test.bin".into(),
            file_size: Some(total_size),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
        };

        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(meta)
                .with_range_data(0, frag_size - 1, frag_a.clone())
                .with_range_data(frag_size, 2 * frag_size - 1, frag_b.clone())
                .with_range_data(2 * frag_size, total_size - 1, frag_c.clone()),
        );

        let storage = StorageKind::memory_with_capacity(total_size as usize);

        // 调度器配置:确保恰好产生 3 个分片
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };
        let config = DownloadConfig {
            verify_checksum: false, // 本测试不校验哈希
            ..test_config()
        };

        let mut task = DownloadTask::new_for_test(
            "http://example.com/test.bin".into(),
            config,
            protocol,
            storage,
        );

        // 使用自定义调度器配置创建编排器
        task.orchestrator =
            DownloadOrchestrator::with_scheduler_config(Default::default(), sched_config);

        task.run().await.expect("下载流程失败");

        assert_eq!(task.state(), DownloadState::Completed);
        assert!((task.progress() - 1.0).abs() < f64::EPSILON);

        // 验证写入数据的正确性
        let mut buf = vec![0u8; total_size as usize];
        task.storage
            .as_ref()
            .unwrap()
            .read_at(0, &mut buf)
            .await
            .unwrap();
        assert_eq!(&buf[..frag_size as usize], &frag_a[..]);
        assert_eq!(
            &buf[frag_size as usize..2 * frag_size as usize],
            &frag_b[..]
        );
        assert_eq!(&buf[2 * frag_size as usize..], &frag_c[..]);
    }

    /// 不支持 Range 请求时使用整块下载
    #[tokio::test]
    async fn test_run_no_range_support() {
        let data = Bytes::from_static(b"hello world no range");
        let meta = FileMetadata {
            file_name: "no_range.bin".into(),
            file_size: Some(data.len() as u64),
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
        };

        let protocol = Arc::new(MockProto::new(meta).with_default_data(data.clone()));

        let storage = StorageKind::memory_with_capacity(data.len());

        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
        );

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute().await.unwrap();

        assert_eq!(task.state(), DownloadState::Completed);
    }

    // ------ 6. 进度追踪正确 -----

    #[test]
    fn test_progress_tracking() {
        let protocol = Arc::new(MockProto::new(test_metadata("p.bin", 100)));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        // 模拟 3 个分片,部分完成
        task.fragments = vec![
            FragmentRecord::new(
                FragmentInfo {
                    index: 0,
                    start: 0,
                    end: 32,
                    size: 33,
                    downloaded: 33,
                    hash: None,
                },
                3,
            ),
            FragmentRecord::new(
                FragmentInfo {
                    index: 1,
                    start: 33,
                    end: 65,
                    size: 33,
                    downloaded: 10,
                    hash: None,
                },
                3,
            ),
            FragmentRecord::new(
                FragmentInfo {
                    index: 2,
                    start: 66,
                    end: 99,
                    size: 34,
                    downloaded: 0,
                    hash: None,
                },
                3,
            ),
        ];

        // 总大小 100,已下载 43
        let progress = task.progress();
        assert!((progress - 0.43).abs() < 0.001);
    }

    #[test]
    fn test_progress_no_fragments_is_zero() {
        let protocol = Arc::new(MockProto::new(test_metadata("e.bin", 100)));
        let storage = StorageKind::memory();
        let task = make_task(protocol, storage, test_config());
        assert!((task.progress() - 0.0).abs() < f64::EPSILON);
    }

    // ------ 7. 状态转换正确 -----

    #[tokio::test]
    async fn test_state_transitions() {
        let meta = test_metadata("state.bin", 100);
        let default_data = Bytes::from(vec![0u8; 100]);
        let protocol = Arc::new(MockProto::new(meta).with_default_data(default_data));
        let storage = StorageKind::memory_with_capacity(100);
        let mut task = make_task(protocol, storage, test_config());

        // 初始状态
        assert_eq!(task.state(), DownloadState::Pending);

        // probe 不改变状态
        task.probe().await.unwrap();
        assert_eq!(task.state(), DownloadState::Pending);

        // plan 不改变状态
        task.plan().unwrap();
        assert_eq!(task.state(), DownloadState::Pending);

        // execute 转为 Downloading,完成后转为 Completed
        task.execute().await.unwrap();
        assert_eq!(task.state(), DownloadState::Completed);
    }

    // ------ 8. 并发分片数限制 -----

    #[tokio::test]
    async fn test_concurrent_fragment_execution() {
        let total_size = 400u64;
        let frag_count = 4;
        let frag_size = total_size / frag_count;

        let meta = test_metadata("conc.bin", total_size);
        let mut protocol_mock = MockProto::new(meta);
        for i in 0..frag_count {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            let data = Bytes::from(vec![(i + 1) as u8; frag_size as usize]);
            protocol_mock = protocol_mock.with_range_data(start, end, data);
        }

        let protocol: Arc<dyn Protocol> = Arc::new(protocol_mock);
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let config = DownloadConfig {
            max_concurrent_fragments: 2, // 限制并发为 2
            verify_checksum: false,
            ..test_config()
        };

        // 使用小分片配置以产生多个分片
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: 100,
            max_fragment_size: 110,
            ..Default::default()
        };

        let mut task = DownloadTask::new_for_test(
            "http://example.com/conc.bin".into(),
            config,
            protocol,
            storage,
        );
        task.orchestrator =
            DownloadOrchestrator::with_scheduler_config(Default::default(), sched_config);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute().await.unwrap();

        assert_eq!(task.state(), DownloadState::Completed);
        assert!((task.progress() - 1.0).abs() < f64::EPSILON);
    }

    // ------ 9. 分片校验 -----

    #[tokio::test]
    async fn test_verify_fragments_with_hash() {
        let data = Bytes::from_static(b"verify this data block");
        let hash = {
            let v = CpuVerifier::blake3();
            v.compute_hash(&data).unwrap()
        };

        let frag_info = FragmentInfo {
            index: 0,
            start: 0,
            end: data.len() as u64 - 1,
            size: data.len() as u64,
            downloaded: 0,
            hash: Some(hash),
        };

        let protocol = Arc::new(MockProto::new(test_metadata("v.bin", data.len() as u64)));
        let storage = StorageKind::memory_with_capacity(data.len());

        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                ..test_config()
            },
        );

        // 手动写入数据到存储
        task.storage
            .as_ref()
            .unwrap()
            .write_at(0, data.clone())
            .await
            .unwrap();

        // 设置分片记录
        task.fragments = vec![FragmentRecord::new(frag_info, 3)];
        task.metadata = Some(test_metadata("v.bin", data.len() as u64));

        task.verify().await.unwrap();
    }

    #[tokio::test]
    async fn test_verify_detects_corruption() {
        let data = Bytes::from_static(b"original data");
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let frag_info = FragmentInfo {
            index: 0,
            start: 0,
            end: data.len() as u64 - 1,
            size: data.len() as u64,
            downloaded: 0,
            hash: Some(wrong_hash.into()),
        };

        let protocol = Arc::new(MockProto::new(test_metadata("c.bin", data.len() as u64)));
        let storage = StorageKind::memory_with_capacity(data.len());

        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                ..test_config()
            },
        );

        task.storage
            .as_ref()
            .unwrap()
            .write_at(0, data.clone())
            .await
            .unwrap();
        task.fragments = vec![FragmentRecord::new(frag_info, 3)];
        task.metadata = Some(test_metadata("c.bin", data.len() as u64));

        let result = task.verify().await;
        assert!(result.is_err(), "哈希不匹配时校验应失败");
        assert!(matches!(
            result.unwrap_err(),
            DownloadError::ChecksumMismatch { .. }
        ));
        assert_eq!(task.state(), DownloadState::Failed);
    }

    #[tokio::test]
    async fn test_verify_skipped_when_disabled() {
        let protocol = Arc::new(MockProto::new(test_metadata("s.bin", 100)));
        let storage = StorageKind::memory();
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
        );

        task.verify().await.unwrap();
    }

    // ------ 10. 空文件处理 -----

    #[tokio::test]
    async fn test_empty_file_handling() {
        let meta = FileMetadata {
            file_name: "empty.txt".into(),
            file_size: Some(0),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
        };
        let protocol = Arc::new(MockProto::new(meta));
        let storage = StorageKind::memory();
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
        );

        task.probe().await.unwrap();
        let frags = task.plan().unwrap();
        assert!(frags.is_empty(), "空文件不应产生分片");

        task.execute().await.unwrap();
        assert_eq!(task.state(), DownloadState::Completed);
        assert!(
            (task.progress() - 1.0).abs() < f64::EPSILON,
            "空文件进度应为 1.0"
        );
    }

    #[tokio::test]
    async fn test_empty_file_unknown_size() {
        let meta = FileMetadata {
            file_name: "stream.dat".into(),
            file_size: None, // 未知大小
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
        };
        let protocol = Arc::new(MockProto::new(meta));
        let storage = StorageKind::memory();
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
        );

        task.probe().await.unwrap();
        let frags = task.plan().unwrap();
        // 未知大小视为 0,不产生分片
        assert!(frags.is_empty());
    }

    // ------ 补充: 零大小文件进度 -----

    #[test]
    fn test_progress_zero_size_fragments() {
        let protocol = Arc::new(MockProto::new(test_metadata("z.bin", 0)));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        // 分片 size 为 0 时进度应为 1.0
        task.fragments = vec![FragmentRecord::new(
            FragmentInfo {
                index: 0,
                start: 0,
                end: 0,
                size: 0,
                downloaded: 0,
                hash: None,
            },
            3,
        )];
        assert!((task.progress() - 1.0).abs() < f64::EPSILON);
    }

    // ------ 补充: VerifierKind clone 验证 -----

    #[test]
    fn test_verifier_kind_clone() {
        let v = default_blake3_verifier();
        let v2 = v.clone();
        let data = b"test data for clone verification";
        let hash = v.compute_hash(data).unwrap();
        let hash2 = v2.compute_hash(data).unwrap();
        assert_eq!(hash, hash2);
    }

    // ------ 补充: URL 解析校验 -----

    #[tokio::test]
    async fn test_invalid_url_fails() {
        let config = test_config();
        let result = DownloadTask::new("not a url".into(), config).await;
        assert!(result.is_err(), "非法 URL 应创建失败");
    }

    // ------ 补充: run 失败时状态标记 -----

    #[tokio::test]
    async fn test_run_failure_marks_state() {
        let protocol = Arc::new(MockProto::failing(DownloadError::Network("断网".into())));
        let storage = StorageKind::memory();
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
        );

        let result = task.run().await;
        assert!(result.is_err());
        assert_eq!(task.state(), DownloadState::Failed);
    }

    // ------ 补充: 并发下载失败场景(mock protocol 返回错误) ------

    /// 验证并发分片下载时,协议层返回错误会正确传播
    #[tokio::test]
    async fn test_concurrent_download_failure() {
        let total_size = 400u64;
        let frag_size = 100u64;

        let meta = test_metadata("fail_conc.bin", total_size);

        // 自定义协议:第 2 次调用返回错误(并发场景中某个分片会失败)
        struct FailOnSecondProtocol {
            meta: FileMetadata,
            call_count: Arc<AtomicU32>,
            frag_data: Bytes,
        }

        impl Clone for FailOnSecondProtocol {
            fn clone(&self) -> Self {
                Self {
                    meta: self.meta.clone(),
                    call_count: Arc::clone(&self.call_count),
                    frag_data: self.frag_data.clone(),
                }
            }
        }

        impl Protocol for FailOnSecondProtocol {
            fn probe(
                &self,
                _url: &str,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>,
            > {
                let meta = self.meta.clone();
                Box::pin(async move { Ok(meta) })
            }

            fn download_range(
                &self,
                _url: &str,
                _start: u64,
                _end: u64,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let count = self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
                let data = self.frag_data.clone();
                Box::pin(async move {
                    if count == 1 {
                        Err(DownloadError::Network("分片 1 下载失败".into()))
                    } else {
                        Ok(data)
                    }
                })
            }

            fn download_range_stream(
                &self,
                url: &str,
                start: u64,
                end: u64,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let this = self.clone();
                let url = url.to_owned();
                Box::pin(async move {
                    let data = this.download_range(&url, start, end).await?;
                    Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
                })
            }

            fn download_full(
                &self,
                _url: &str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let data = self.frag_data.clone();
                Box::pin(async move { Ok(data) })
            }
        }

        let protocol: Arc<dyn Protocol> = Arc::new(FailOnSecondProtocol {
            meta: meta.clone(),
            call_count: Arc::new(AtomicU32::new(0)),
            frag_data: Bytes::from(vec![0xAA; frag_size as usize]),
        });

        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };

        let mut task = DownloadTask::new_for_test(
            "http://example.com/fail.bin".into(),
            DownloadConfig {
                max_retries: 0, // 禁用重试:验证"分片失败即整体失败"的传播契约
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.orchestrator =
            DownloadOrchestrator::with_scheduler_config(Default::default(), sched_config);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // 执行应失败(分片 1 下载错误,max_retries=0 不重试)
        let result = task.execute().await;
        assert!(result.is_err(), "并发分片下载中任一分片失败应导致整体失败");
        // 验证错误信息包含网络故障描述
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("分片") || err_msg.contains("网络") || err_msg.contains("失败"),
            "错误信息应包含故障描述: {err_msg}"
        );
    }

    // ------ 补充: 分片重试韧性(第一次失败,第二次成功) ------

    /// 验证:协议首次调用失败后,重试可以成功
    /// 模拟 DownloadTask 的 run() 失败后,用户重试 run() 成功的场景
    #[tokio::test]
    async fn test_fragment_retry_resilience() {
        struct FailOnceProtocol {
            meta: FileMetadata,
            data: Bytes,
            fail_count: AtomicU32,
            max_failures: u32,
        }

        impl Clone for FailOnceProtocol {
            fn clone(&self) -> Self {
                Self {
                    meta: self.meta.clone(),
                    data: self.data.clone(),
                    fail_count: AtomicU32::new(self.fail_count.load(AtomicOrdering::SeqCst)),
                    max_failures: self.max_failures,
                }
            }
        }

        impl Protocol for FailOnceProtocol {
            fn probe(
                &self,
                _url: &str,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>,
            > {
                let meta = self.meta.clone();
                Box::pin(async move { Ok(meta) })
            }

            fn download_range(
                &self,
                _url: &str,
                _start: u64,
                _end: u64,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let count = self.fail_count.fetch_add(1, AtomicOrdering::SeqCst);
                let data = self.data.clone();
                let max_f = self.max_failures;
                Box::pin(async move {
                    if count < max_f {
                        Err(DownloadError::Network(format!("模拟故障 #{}", count)))
                    } else {
                        Ok(data)
                    }
                })
            }

            fn download_range_stream(
                &self,
                url: &str,
                start: u64,
                end: u64,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let this = self.clone();
                let url = url.to_owned();
                Box::pin(async move {
                    let data = this.download_range(&url, start, end).await?;
                    Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
                })
            }

            fn download_full(
                &self,
                _url: &str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let data = self.data.clone();
                Box::pin(async move { Ok(data) })
            }
        }

        let total_size = 400u64;
        let frag_data = Bytes::from(vec![0xBB; total_size as usize]);

        // 使用小分片配置确保产生多个分片
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: 100,
            max_fragment_size: 200,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };

        // 第一次协议:前 2 次调用失败(模拟并发分片场景中部分分片失败)
        let protocol1: Arc<dyn Protocol> = Arc::new(FailOnceProtocol {
            meta: test_metadata("retry.bin", total_size),
            data: frag_data.clone(),
            fail_count: AtomicU32::new(0),
            max_failures: 2,
        });

        let storage1 = StorageKind::memory_with_capacity(total_size as usize);
        let mut task1 = DownloadTask::new_for_test(
            "http://example.com/retry.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
            protocol1,
            storage1,
        );
        task1.orchestrator =
            DownloadOrchestrator::with_scheduler_config(Default::default(), sched_config.clone());

        task1.probe().await.unwrap();
        task1.plan().unwrap();
        task1.prepare_storage().await.unwrap();
        assert!(
            task1.fragment_infos().len() > 1,
            "应产生多个分片以测试并发失败"
        );

        // 第一次执行:应失败(前 2 次协议调用返回错误)
        let result1 = task1.execute().await;
        assert!(result1.is_err(), "首次执行应因协议故障而失败");

        // 第二次协议:所有调用都成功(模拟重试)
        let protocol2: Arc<dyn Protocol> = Arc::new(FailOnceProtocol {
            meta: test_metadata("retry.bin", total_size),
            data: frag_data.clone(),
            fail_count: AtomicU32::new(0),
            max_failures: 0, // 不失败
        });

        let storage2 = StorageKind::memory_with_capacity(total_size as usize);
        let mut task2 = DownloadTask::new_for_test(
            "http://example.com/retry.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
            protocol2,
            storage2,
        );
        task2.orchestrator =
            DownloadOrchestrator::with_scheduler_config(Default::default(), sched_config);

        task2.probe().await.unwrap();
        task2.plan().unwrap();
        task2.prepare_storage().await.unwrap();

        // 第二次执行:应成功
        task2.execute().await.expect("重试执行应成功");
        assert_eq!(task2.state(), DownloadState::Completed);
        assert!((task2.progress() - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_connection_pool_permit_limits_real_range_requests() {
        struct BlockingProtocol {
            meta: FileMetadata,
            active: Arc<AtomicU32>,
            peak: Arc<AtomicU32>,
            release_rx: tokio::sync::watch::Receiver<bool>,
        }

        impl Clone for BlockingProtocol {
            fn clone(&self) -> Self {
                Self {
                    meta: self.meta.clone(),
                    active: Arc::clone(&self.active),
                    peak: Arc::clone(&self.peak),
                    release_rx: self.release_rx.clone(),
                }
            }
        }

        impl Protocol for BlockingProtocol {
            fn probe(
                &self,
                _url: &str,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>,
            > {
                let meta = self.meta.clone();
                Box::pin(async move { Ok(meta) })
            }

            fn download_range(
                &self,
                _url: &str,
                start: u64,
                end: u64,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                Box::pin(async move { Ok(Bytes::from(vec![0xDD; (end - start + 1) as usize])) })
            }

            fn download_range_stream(
                &self,
                _url: &str,
                start: u64,
                end: u64,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let active = Arc::clone(&self.active);
                let peak = Arc::clone(&self.peak);
                let mut release_rx = self.release_rx.clone();
                Box::pin(async move {
                    let now = active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                    peak.fetch_max(now, AtomicOrdering::SeqCst);
                    while !*release_rx.borrow() {
                        release_rx
                            .changed()
                            .await
                            .map_err(|_| DownloadError::Other("释放信号关闭".into()))?;
                    }
                    active.fetch_sub(1, AtomicOrdering::SeqCst);
                    let data = Bytes::from(vec![0xDD; (end - start + 1) as usize]);
                    Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
                })
            }

            fn download_full(
                &self,
                _url: &str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                Box::pin(async move { Ok(Bytes::new()) })
            }
        }

        let active = Arc::new(AtomicU32::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let (release_tx, release_rx) = tokio::sync::watch::channel(false);
        let protocol: Arc<dyn Protocol> = Arc::new(BlockingProtocol {
            meta: test_metadata("pool.bin", 400),
            active,
            peak: Arc::clone(&peak),
            release_rx,
        });
        let storage = StorageKind::memory_with_capacity(400);
        let pool = Arc::new(ConnectionPool::new(crate::connection::PoolConfig {
            max_per_host: 1,
            max_global: 4,
        }));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/pool.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 4,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.pool = Some(pool);
        task.orchestrator = DownloadOrchestrator::with_scheduler_config(
            Default::default(),
            tachyon_core::config::SchedulerConfig {
                min_fragment_size: 100,
                max_fragment_size: 100,
                ..Default::default()
            },
        );

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        let run = tokio::time::timeout(std::time::Duration::from_millis(200), task.execute()).await;
        assert!(run.is_err(), "无释放信号时应仍有分片等待连接许可");
        assert_eq!(peak.load(AtomicOrdering::SeqCst), 1);
        release_tx.send(true).unwrap();
    }

    #[tokio::test]
    async fn test_paused_control_prevents_fragment_writes() {
        let data = Bytes::from(vec![0xEE; 100]);
        let protocol: Arc<dyn Protocol> =
            Arc::new(MockProto::new(test_metadata("paused.bin", 100)).with_range_data(0, 99, data));
        let storage = StorageKind::memory_with_capacity(100);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/paused.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 1,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        let (control_tx, control_rx) = watch::channel(DownloadState::Paused);
        task.set_control_rx(control_rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let paused_result =
            tokio::time::timeout(std::time::Duration::from_millis(100), task.execute()).await;
        assert!(paused_result.is_err(), "暂停状态下执行应等待控制信号");
        let stored = if let Some(storage) = &task.storage {
            let mut buf = vec![0u8; 100];
            let _ = storage.read_at(0, &mut buf).await;
            buf
        } else {
            Vec::new()
        };
        assert!(stored.iter().all(|byte| *byte == 0), "暂停期间不应写入数据");
        control_tx.send(DownloadState::Cancelled).unwrap();
    }

    #[tokio::test]
    async fn test_paused_control_respects_pause_timeout() {
        let data = Bytes::from(vec![0xEE; 100]);
        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(test_metadata("paused-timeout.bin", 100)).with_range_data(0, 99, data),
        );
        let storage = StorageKind::memory_with_capacity(100);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/paused-timeout.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 1,
                pause_timeout_secs: 1,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        let (_control_tx, control_rx) = watch::channel(DownloadState::Paused);
        task.set_control_rx(control_rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(1500), task.execute()).await;
        assert!(result.is_ok(), "暂停超时后不应永久等待控制信号");
        assert!(result.unwrap().is_err(), "暂停超时应返回错误");
    }

    #[tokio::test]
    async fn test_fragment_failure_records_failed_state_and_run_fails() {
        let protocol: Arc<dyn Protocol> =
            Arc::new(MockProto::new(test_metadata("missing.bin", 200)));
        let storage = StorageKind::memory_with_capacity(200);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/missing.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.orchestrator = DownloadOrchestrator::with_scheduler_config(
            Default::default(),
            tachyon_core::config::SchedulerConfig {
                min_fragment_size: 100,
                max_fragment_size: 100,
                ..Default::default()
            },
        );

        let result = task.run().await;
        assert!(result.is_err(), "缺失分片数据应导致 run 失败");
        assert_eq!(task.state(), DownloadState::Failed);
        assert!(
            task.fragments
                .iter()
                .any(|frag| frag.state == FragmentState::Failed),
            "至少一个失败分片应记录 Failed 状态"
        );
    }

    #[tokio::test]
    async fn test_full_download_uses_fragment_state_machine() {
        let data = Bytes::from_static(b"full state machine");
        let meta = FileMetadata {
            file_name: "full.bin".into(),
            file_size: Some(data.len() as u64),
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
        };
        let protocol = Arc::new(MockProto::new(meta).with_default_data(data.clone()));
        let storage = StorageKind::memory_with_capacity(data.len());
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
        );

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute().await.unwrap();

        let frag = task.fragments.first().expect("整块下载应保留首分片记录");
        assert_eq!(frag.state, FragmentState::Done);
        assert!(frag.last_duration.is_some());
        assert_eq!(frag.info.downloaded, data.len() as u64);
    }

    // ------ 补充: DownloadTask::progress() 正确性(更多场景) ------

    /// 验证 progress() 在多种分片状态下的准确性
    #[test]
    fn test_progress_various_fragment_states() {
        let protocol = Arc::new(MockProto::new(test_metadata("prog.bin", 300)));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        // 场景 1:无分片 -> 0.0
        assert!((task.progress() - 0.0).abs() < f64::EPSILON);

        // 场景 2:单分片,下载一半
        task.fragments = vec![FragmentRecord::new(
            FragmentInfo {
                index: 0,
                start: 0,
                end: 299,
                size: 300,
                downloaded: 150,
                hash: None,
            },
            3,
        )];
        let p = task.progress();
        assert!((p - 0.5).abs() < 0.001, "单分片下载一半应为 0.5,实际: {p}");

        // 场景 3:多分片,不同进度
        task.fragments = vec![
            FragmentRecord::new(
                FragmentInfo {
                    index: 0,
                    start: 0,
                    end: 99,
                    size: 100,
                    downloaded: 100, // 完成
                    hash: None,
                },
                3,
            ),
            FragmentRecord::new(
                FragmentInfo {
                    index: 1,
                    start: 100,
                    end: 199,
                    size: 100,
                    downloaded: 50, // 一半
                    hash: None,
                },
                3,
            ),
            FragmentRecord::new(
                FragmentInfo {
                    index: 2,
                    start: 200,
                    end: 299,
                    size: 100,
                    downloaded: 0, // 未开始
                    hash: None,
                },
                3,
            ),
        ];
        let p = task.progress();
        assert!(
            (p - 0.5).abs() < 0.001,
            "三分片(100+50+0)/300 应为 0.5,实际: {p}"
        );

        // 场景 4:全部完成
        for frag in &mut task.fragments {
            frag.info.downloaded = frag.info.size;
        }
        let p = task.progress();
        assert!((p - 1.0).abs() < f64::EPSILON, "全部完成应为 1.0,实际: {p}");

        // 场景 5:状态为 Completed 时强制返回 1.0
        task.state = DownloadState::Completed;
        task.fragments[1].info.downloaded = 0; // 人为清零
        let p = task.progress();
        assert!(
            (p - 1.0).abs() < f64::EPSILON,
            "Completed 状态应强制返回 1.0"
        );
    }

    // ------ 补充: FragmentRecord 状态转换(更完整的覆盖) ------

    /// 验证 Pending -> Downloading -> Done 完整路径
    #[test]
    fn test_fragment_record_pending_to_done() {
        let info = FragmentInfo {
            index: 0,
            start: 0,
            end: 999,
            size: 1000,
            downloaded: 0,
            hash: None,
        };
        let mut record = FragmentRecord::new(info, 3);
        assert_eq!(record.state, FragmentState::Pending);

        record.start_download();
        assert_eq!(record.state, FragmentState::Downloading);
        assert!(!record.is_done());
        assert!(!record.is_failed());

        record.complete_download(1000, Duration::from_millis(50));
        assert_eq!(record.state, FragmentState::Verifying);
        assert_eq!(record.info.downloaded, 1000);
        assert!(record.last_duration.is_some());

        record.verify_ok();
        assert_eq!(record.state, FragmentState::Writing);

        record.write_done();
        assert_eq!(record.state, FragmentState::Done);
        assert!(record.is_done());
    }

    /// 验证 Downloading -> Failed(超过最大重试)
    #[test]
    fn test_fragment_record_to_failed() {
        let info = FragmentInfo {
            index: 1,
            start: 1000,
            end: 1999,
            size: 1000,
            downloaded: 0,
            hash: None,
        };
        let mut record = FragmentRecord::new(info, 1); // 最多重试 1 次

        record.start_download();
        assert_eq!(record.state, FragmentState::Downloading);

        // 第一次失败:可以重试
        let can_retry = record.mark_failed();
        assert!(can_retry, "首次失败应可重试");
        assert_eq!(record.state, FragmentState::Pending);
        assert_eq!(record.retry_count, 1);

        record.start_download();

        // 第二次失败:超过重试次数
        let can_retry = record.mark_failed();
        assert!(!can_retry, "超过重试次数应不可重试");
        assert_eq!(record.state, FragmentState::Failed);
        assert!(record.is_failed());
        assert_eq!(record.retry_count, 2);
    }

    /// 验证 Verifying 和 Writing 阶段也可以标记失败
    #[test]
    fn test_fragment_fail_from_verifying_and_writing() {
        let info = FragmentInfo {
            index: 0,
            start: 0,
            end: 99,
            size: 100,
            downloaded: 0,
            hash: None,
        };

        // 从 Verifying 阶段失败
        let mut record = FragmentRecord::new(info.clone(), 3);
        record.start_download();
        record.complete_download(4, Duration::from_millis(5));
        assert_eq!(record.state, FragmentState::Verifying);
        let can_retry = record.mark_failed();
        assert!(can_retry);
        assert_eq!(record.state, FragmentState::Pending);

        // 从 Writing 阶段失败
        let mut record = FragmentRecord::new(info, 3);
        record.start_download();
        record.complete_download(4, Duration::from_millis(5));
        record.verify_ok();
        assert_eq!(record.state, FragmentState::Writing);
        let can_retry = record.mark_failed();
        assert!(can_retry);
        assert_eq!(record.state, FragmentState::Pending);
    }

    // ------ 回归: control_rx=Downloading 时下载不应被误判为"控制信号异常结束" ------

    /// 回归测试 P0-1:协作式控制通道初始值为 Downloading(生产路径如此),
    /// 此前 `wait_control_rx` 在 Downloading 下同步立即返回 Ok,
    /// 导致 `tokio::select!` 抢占下载分支并误判失败。
    /// 修复后 `watch_for_interrupt` 在正常状态下挂起,下载应正常完成。
    #[tokio::test]
    async fn test_control_downloading_does_not_abort_fragmented_download() {
        let frag_size = 100u64;
        let total_size = frag_size * 3;
        let meta = test_metadata("ctrl.bin", total_size);
        let mut mock = MockProto::new(meta);
        for i in 0..3u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_range_data(
                start,
                end,
                Bytes::from(vec![0xC0 | i as u8; frag_size as usize]),
            );
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/ctrl.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 3,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.orchestrator = DownloadOrchestrator::with_scheduler_config(
            Default::default(),
            tachyon_core::config::SchedulerConfig {
                min_fragment_size: frag_size,
                max_fragment_size: frag_size,
                ..Default::default()
            },
        );
        // 生产路径的初始控制状态正是 Downloading
        let (_tx, rx) = watch::channel(DownloadState::Downloading);
        task.set_control_rx(rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute()
            .await
            .expect("Downloading 控制状态不应导致下载失败");
        assert_eq!(task.state(), DownloadState::Completed);
        assert!((task.progress() - 1.0).abs() < f64::EPSILON);
    }

    /// 回归测试 P0-1(整块下载路径):不支持 Range + control_rx=Downloading 时应正常完成。
    #[tokio::test]
    async fn test_control_downloading_does_not_abort_full_download() {
        let data = Bytes::from_static(b"control downloading full path");
        let meta = FileMetadata {
            file_name: "ctrl_full.bin".into(),
            file_size: Some(data.len() as u64),
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
        };
        let protocol = Arc::new(MockProto::new(meta).with_default_data(data.clone()));
        let storage = StorageKind::memory_with_capacity(data.len());
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
        );
        let (_tx, rx) = watch::channel(DownloadState::Downloading);
        task.set_control_rx(rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute()
            .await
            .expect("Downloading 控制状态不应导致整块下载失败");
        assert_eq!(task.state(), DownloadState::Completed);
    }

    // ====== P0-2 重试 / P0-3 续传 / P1-6 失败归因 独立验证 ======

    /// 测试协议:指定分片索引的前 N 次 range 请求失败,之后成功。
    /// 用于验证 spawn 内部重试循环。
    struct FlakyFragmentProtocol {
        meta: FileMetadata,
        frag_size: u64,
        /// 对哪个分片(按 start 偏移判定)注入失败
        fail_start: u64,
        /// 该分片失败几次后转为成功
        fail_times: u32,
        attempts: Arc<AtomicU32>,
    }

    impl Clone for FlakyFragmentProtocol {
        fn clone(&self) -> Self {
            Self {
                meta: self.meta.clone(),
                frag_size: self.frag_size,
                fail_start: self.fail_start,
                fail_times: self.fail_times,
                attempts: Arc::clone(&self.attempts),
            }
        }
    }

    impl Protocol for FlakyFragmentProtocol {
        fn probe(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>>
        {
            let meta = self.meta.clone();
            Box::pin(async move { Ok(meta) })
        }

        fn download_range(
            &self,
            _url: &str,
            start: u64,
            end: u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            let fail_start = self.fail_start;
            let fail_times = self.fail_times;
            let attempts = Arc::clone(&self.attempts);
            let size = (end - start + 1) as usize;
            Box::pin(async move {
                if start == fail_start {
                    let n = attempts.fetch_add(1, AtomicOrdering::SeqCst);
                    if n < fail_times {
                        return Err(DownloadError::Network(format!(
                            "分片 {start} 模拟故障 #{n}"
                        )));
                    }
                }
                Ok(Bytes::from(vec![0xAB; size]))
            })
        }

        fn download_range_stream(
            &self,
            url: &str,
            start: u64,
            end: u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            let this = self.clone();
            let url = url.to_owned();
            Box::pin(async move {
                let data = this.download_range(&url, start, end).await?;
                Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
            })
        }

        fn download_full(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            Box::pin(async move { Ok(Bytes::new()) })
        }
    }

    fn flaky_task(
        protocol: Arc<dyn Protocol>,
        total: u64,
        frag_size: u64,
        max_retries: u32,
    ) -> DownloadTask {
        let storage = StorageKind::memory_with_capacity(total as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/flaky.bin".into(),
            DownloadConfig {
                max_retries,
                max_concurrent_fragments: 4,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.orchestrator = DownloadOrchestrator::with_scheduler_config(
            Default::default(),
            tachyon_core::config::SchedulerConfig {
                min_fragment_size: frag_size,
                max_fragment_size: frag_size,
                ..Default::default()
            },
        );
        task
    }

    /// P0-2:单个分片前 2 次失败、第 3 次成功,在 max_retries=3 下应整体成功。
    #[tokio::test]
    async fn test_fragment_auto_retry_succeeds_within_limit() {
        let frag_size = 100u64;
        let total = frag_size * 3;
        let protocol: Arc<dyn Protocol> = Arc::new(FlakyFragmentProtocol {
            meta: test_metadata("flaky.bin", total),
            frag_size,
            fail_start: frag_size, // 第 2 个分片失败
            fail_times: 2,
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let mut task = flaky_task(protocol, total, frag_size, 3);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute().await.expect("重试上限内应自动恢复并成功");
        assert_eq!(task.state(), DownloadState::Completed);
        assert!((task.progress() - 1.0).abs() < f64::EPSILON);
    }

    /// P0-2 + P1-6:失败次数超过 max_retries,应整体失败,
    /// 且被标记 Failed 的恰好是真正失败的那个分片(归因正确)。
    #[tokio::test]
    async fn test_fragment_retry_exhausted_marks_correct_fragment() {
        let frag_size = 100u64;
        let total = frag_size * 3;
        // 第 3 个分片(start=200)始终失败,超过 max_retries=1(共 2 次尝试)
        let protocol: Arc<dyn Protocol> = Arc::new(FlakyFragmentProtocol {
            meta: test_metadata("flaky.bin", total),
            frag_size,
            fail_start: 2 * frag_size,
            fail_times: u32::MAX, // 永远失败
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let mut task = flaky_task(protocol, total, frag_size, 1);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        let result = task.execute().await;
        assert!(result.is_err(), "重试耗尽应整体失败");
        assert_eq!(task.state(), DownloadState::Failed);

        // 失败的应是 index=2 那个分片(start=200),而非张冠李戴到 index 0
        let failed: Vec<u32> = task
            .fragments
            .iter()
            .filter(|f| f.state == FragmentState::Failed)
            .map(|f| f.info.index)
            .collect();
        assert_eq!(failed, vec![2], "应精确标记真正失败的分片 index=2");
    }

    /// P0-3:注入已完成分片后,plan() 应跳过它们的下载,且 progress 反映已完成部分。
    #[tokio::test]
    async fn test_resume_skips_completed_fragments() {
        let frag_size = 100u64;
        let total = frag_size * 3;
        // 协议对"被跳过的分片"若被请求会 panic 计数;这里让 start=0 分片一旦被下载就失败,
        // 用以证明它确实未被下载(已通过续传跳过)。
        let protocol: Arc<dyn Protocol> = Arc::new(FlakyFragmentProtocol {
            meta: test_metadata("flaky.bin", total),
            frag_size,
            fail_start: 0,        // 若 index 0 被真实下载会失败
            fail_times: u32::MAX, // 始终失败
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let mut task = flaky_task(protocol, total, frag_size, 0);

        task.probe().await.unwrap();
        // 注入:index 0 已完成 → 应跳过下载(否则会因 fail_start=0 失败)
        task.set_completed_fragments(vec![0]);
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute()
            .await
            .expect("已完成分片应被跳过,其余分片成功");
        assert_eq!(task.state(), DownloadState::Completed);

        // index 0 应为 Done 且 downloaded == size(续传标记)
        let frag0 = &task.fragments[0];
        assert_eq!(frag0.state, FragmentState::Done);
        assert_eq!(frag0.info.downloaded, frag0.info.size);
    }

    /// P0-3:续传后整体 progress 正确(已完成分片计入)。
    #[tokio::test]
    async fn test_resume_progress_reflects_completed() {
        let frag_size = 100u64;
        let total = frag_size * 4;
        let protocol: Arc<dyn Protocol> = Arc::new(FlakyFragmentProtocol {
            meta: test_metadata("flaky.bin", total),
            frag_size,
            fail_start: u64::MAX, // 不注入失败
            fail_times: 0,
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let mut task = flaky_task(protocol, total, frag_size, 0);

        task.probe().await.unwrap();
        task.set_completed_fragments(vec![0, 1]); // 一半已完成
        task.plan().unwrap();
        // 下载前进度应已反映 2/4 完成
        assert!(
            (task.progress() - 0.5).abs() < 0.001,
            "续传后下载前进度应为 0.5,实际 {}",
            task.progress()
        );

        task.prepare_storage().await.unwrap();
        task.execute().await.expect("其余分片应成功下载");
        assert!((task.progress() - 1.0).abs() < f64::EPSILON);
    }

    /// 测试协议:指定分片的前 N 次请求返回固定分类错误,之后成功。
    /// `attempts` 记录该分片被实际请求的次数。
    struct ClassifiedErrorProtocol {
        meta: FileMetadata,
        fail_start: u64,
        /// 该分片失败几次后转为成功(u32::MAX 表示永远失败)
        fail_times: u32,
        error_factory: Arc<dyn Fn() -> DownloadError + Send + Sync>,
        attempts: Arc<AtomicU32>,
    }

    impl Clone for ClassifiedErrorProtocol {
        fn clone(&self) -> Self {
            Self {
                meta: self.meta.clone(),
                fail_start: self.fail_start,
                fail_times: self.fail_times,
                error_factory: Arc::clone(&self.error_factory),
                attempts: Arc::clone(&self.attempts),
            }
        }
    }

    impl Protocol for ClassifiedErrorProtocol {
        fn probe(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>>
        {
            let meta = self.meta.clone();
            Box::pin(async move { Ok(meta) })
        }

        fn download_range(
            &self,
            _url: &str,
            start: u64,
            end: u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            let fail_start = self.fail_start;
            let fail_times = self.fail_times;
            let factory = Arc::clone(&self.error_factory);
            let attempts = Arc::clone(&self.attempts);
            let size = (end - start + 1) as usize;
            Box::pin(async move {
                if start == fail_start {
                    let n = attempts.fetch_add(1, AtomicOrdering::SeqCst);
                    if n < fail_times {
                        return Err(factory());
                    }
                }
                Ok(Bytes::from(vec![0xCD; size]))
            })
        }

        fn download_range_stream(
            &self,
            url: &str,
            start: u64,
            end: u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            let this = self.clone();
            let url = url.to_owned();
            Box::pin(async move {
                let data = this.download_range(&url, start, end).await?;
                Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
            })
        }

        fn download_full(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            Box::pin(async move { Ok(Bytes::new()) })
        }
    }

    /// P2:权限错误(403)不应重试,应立即终止该分片。
    /// 即使 max_retries=5,被请求次数也应恰好为 1。
    #[tokio::test]
    async fn test_forbidden_error_not_retried() {
        let frag_size = 100u64;
        let total = frag_size * 3;
        let attempts = Arc::new(AtomicU32::new(0));
        let protocol: Arc<dyn Protocol> = Arc::new(ClassifiedErrorProtocol {
            meta: test_metadata("forbidden.bin", total),
            fail_start: frag_size, // 第 2 个分片返回 403
            fail_times: u32::MAX,  // 始终失败(用以验证不重试)
            error_factory: Arc::new(|| DownloadError::Forbidden { status: 403 }),
            attempts: Arc::clone(&attempts),
        });
        let mut task = flaky_task(protocol, total, frag_size, 5);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        let result = task.execute().await;
        assert!(result.is_err(), "403 应导致整体失败");
        assert_eq!(task.state(), DownloadState::Failed);
        assert_eq!(
            attempts.load(AtomicOrdering::SeqCst),
            1,
            "权限错误应只尝试一次,不重试"
        );
    }

    /// P2:服务端限流(429)带 Retry-After 应被重试(用退避后恢复)。
    /// 第 1 次返回 429,之后成功;max_retries=3 下应整体成功。
    #[tokio::test]
    async fn test_throttled_error_is_retried_and_recovers() {
        let frag_size = 100u64;
        let total = frag_size * 3;
        let attempts = Arc::new(AtomicU32::new(0));
        // 第 2 个分片首次返回限流(Retry-After=1s,走 Throttled 退避分支),其后成功
        let protocol: Arc<dyn Protocol> = Arc::new(ClassifiedErrorProtocol {
            meta: test_metadata("throttled.bin", total),
            fail_start: frag_size,
            fail_times: 1, // 仅首次失败,重试即成功
            error_factory: Arc::new(|| DownloadError::Throttled {
                retry_after_secs: Some(1),
            }),
            attempts: Arc::clone(&attempts),
        });
        let mut task = flaky_task(protocol, total, frag_size, 3);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        // 注意:Retry-After=1s 会让该测试至少耗时 1s,属预期
        task.execute().await.expect("限流后退避重试应成功");
        assert_eq!(task.state(), DownloadState::Completed);
        assert_eq!(
            attempts.load(AtomicOrdering::SeqCst),
            2,
            "限流分片应被尝试 2 次(首次限流 + 重试成功)"
        );
    }
}
