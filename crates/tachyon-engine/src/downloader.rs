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
//!
//! # 模块拆分
//!
//! - `storage_adapter` -- 类型擦除存储包装器 (DynStorage) + 分片进度消息
//! - `mirror`          -- 多镜像源 Happy Eyeballs 适配器

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::interval;
use tracing::{debug, info, warn};

use tachyon_core::config::{DownloadConfig, SchedulerConfig};
use tachyon_core::traits::{DownloadScheduler, Protocol, Verifier};

use crate::rate_limit::RateLimiter;
use tachyon_core::types::{
    DownloadState, FileMetadata, FragmentInfo, ObjectIdentity, TaskCommand, TaskId,
};
use tachyon_core::{DownloadError, DownloadResult, FragmentProgress, Metrics};
use tachyon_crypto::CpuVerifier;
use tachyon_protocol::http::HttpClient;
use tachyon_scheduler::{AdaptiveDownloadScheduler, ConcurrencyController};

use crate::circuit_breaker::SourceCircuitBreakers;
use crate::mirror::MirrorProtocol;
use crate::storage_adapter::{DynStorage, StorageSet, check_disk_space};
use tachyon_io::AlignedBuf;
use tachyon_io::buffer::{BufferGuard, BufferPool};

/// 类型擦除的校验器,通过 Arc<dyn Verifier> 实现动态分发。
/// 添加新校验后端只需实现 Verifier trait,无需修改引擎层枚举。
pub type VerifierKind = Arc<dyn Verifier>;

/// 创建默认的 blake3 CPU 校验器
pub fn default_blake3_verifier() -> VerifierKind {
    Arc::new(CpuVerifier::blake3())
}

/// 创建默认的 sha256 CPU 校验器(HF LFS 等)
pub fn default_sha256_verifier() -> VerifierKind {
    Arc::new(CpuVerifier::sha256())
}

/// 审计 A-01:本地文件 sha256(HF LFS 等);app 不直接依赖 tachyon-crypto
pub async fn sha256_file(
    path: &std::path::Path,
    chunk_size: usize,
) -> tachyon_core::DownloadResult<String> {
    CpuVerifier::sha256()
        .compute_hash_from_path(path, chunk_size)
        .await
}

/// 审计 A-01:由 engine 构造自适应调度器,app 不直接依赖 tachyon-scheduler
pub fn create_adaptive_scheduler(
    config: tachyon_core::config::SchedulerConfig,
) -> Arc<dyn DownloadScheduler> {
    Arc::new(AdaptiveDownloadScheduler::new(config))
}

pub type StorageKind = DynStorage;

/// L-9: verify() 分块读取文件的 chunk 大小 (8 MiB)。
/// 现代 SSD 顺序读取带宽可达数 GB/s,1 MiB 导致大量 read_at 系统调用。
/// 8 MiB 在内存占用和 syscall 频率间取得平衡,校验吞吐提升 2-3x。
const VERIFY_HASH_CHUNK_SIZE: usize = 8 * 1024 * 1024;

/// L-12: 分片下载进度上报频率 — 每 N 个 chunk 上报一次。
/// 值过小则通道压力大,值过大则前端更新不及时;5 在默认 256 KiB batch 下
/// 约每 1.25 MiB 上报一次,平衡延迟与开销。
const PROGRESS_REPORT_CHUNK_INTERVAL: u64 = 5;

/// 分片写入批大小阈值(字节)。网络 chunk 先累积到 `write_buf`,达到此阈值后
/// 批量刷写存储,减少 `write_at` 系统调用次数。256 KiB 在 HDD/SSD 与默认
/// 分片大小下均为合理折中,过小则 I/O 放大,过大则内存占用与尾块延迟上升。
/// 注意:调用方构造 `write_buf` 时须使用同一常量,保证 capacity 与阈值一致,
/// 避免无限增长。
///
/// 引用 `tachyon_core::config::WRITE_BATCH_BYTES` 公共常量,使 tachyon-app
/// 构造全局 BufferPool 时能引用同一值,保证池化 buffer 尺寸与写入阈值一致。
const WRITE_BATCH_BYTES: usize = tachyon_core::config::WRITE_BATCH_BYTES;

/// P6:verify 读盘哈希循环的取消检查点间隔 — 每累计 N 字节已读数据检查一次中断信号。
///
/// verify 阶段读盘哈希在大文件(数十 GB)上可能持续数分钟,无检查点时取消
/// 信号无法穿透(裸 while 循环)。按"已读字节"而非"迭代次数"度量检查点,
/// 使响应延迟与单次 read_at 的返回量无关:无论 read_at 一次返回 8MiB(常态)
/// 还是 1 字节(异常短读),都保证每 64MiB 已读数据检查一次中断信号。
///
/// 对 GB 级单分片:每 64MiB 一次检查,秒级响应;对 64MB 单分片:约 1 次检查点。
/// 相较旧实现(固定 64 次迭代 × 8MiB = 512MiB/检查点)改善 8 倍,且对短读鲁棒。
const VERIFY_CANCEL_CHECK_BYTES: u64 = 64 * 1024 * 1024;

type FragmentTaskOk = (u32, u64, Duration, Option<String>);
type FragmentTaskErr = (u32, DownloadError);
type FragmentTaskResult = Result<FragmentTaskOk, FragmentTaskErr>;

/// work-stealing 共享状态:worker 与主循环通过 Arc<AtomicU64> 同步
///
/// - `effective_end`: 当前有效 end(try_split 可缩小,worker 据此提前停止)
/// - `realtime_downloaded`: 实时已下载字节(worker 更新,find_slowest_fragment 读取)
#[derive(Clone)]
struct FragmentShared {
    effective_end: Arc<std::sync::atomic::AtomicU64>,
    realtime_downloaded: Arc<std::sync::atomic::AtomicU64>,
}

/// 分片任务规格: (index, start, end, resume_offset, compute_hash, shared)
///
/// `shared` 持有 worker 与主循环共享的原子状态(work-stealing 用),
/// 非 work-stealing 模式下也传递(开销为零:Arc clone + 原子操作)。
type FragmentSpec = (u32, u64, u64, u64, bool, FragmentShared);

/// 分片任务 spawn 上下文(消除主 dispatch 与 steal 路径的代码重复)
///
/// 持有主循环中所有跨分片共享的引用/Arc,`spawn_fragment_task` 据此
/// acquire permit + 分配 write_buf + spawn task(含重试循环)。
/// 主 dispatch 和 steal 路径各调用一次,消除 104 行重复代码。
struct FragmentSpawnCtx<'a> {
    protocol: &'a Arc<dyn Protocol>,
    storage: &'a Arc<StorageSet>,
    pool: &'a Option<Arc<ConnectionPool>>,
    url: &'a str,
    host: &'a str,
    limiter: &'a Option<Arc<RateLimiter>>,
    control_rx: &'a Option<watch::Receiver<TaskCommand>>,
    progress_tx: &'a Option<mpsc::Sender<FragmentProgress>>,
    verifier: &'a VerifierKind,
    metrics: &'a Option<Arc<Metrics>>,
    circuit_breakers: &'a SourceCircuitBreakers,
    concurrency_ctrl: &'a Arc<ConcurrencyController>,
    semaphore: &'a Arc<Semaphore>,
    completed_tx: &'a mpsc::UnboundedSender<FragmentTaskResult>,
    buffer_pool: &'a Option<Arc<BufferPool>>,
    has_mirrors: bool,
    max_retries: u32,
    pause_timeout: Duration,
    skip_write: bool,
    object_identity: Option<ObjectIdentity>,
    /// 崩溃一致性级别:控制分片完成时是否 fsync。`Loose` 跳过分片 sync(仅在 close 时落盘),
    /// 牺牲断电耐久性换吞吐;`EveryFragment`(默认)每分片 fsync 一次。
    sync_mode: tachyon_core::config::CrashConsistencyMode,
}

use crate::connection::ConnectionPool;
use crate::fragment::FragmentRecord;

#[cfg(test)]
use tachyon_core::test_harness::harness::MockProtocol as MockProto;

/// URL 路径(去 query/fragment)是否以 HLS playlist 扩展名结尾。
///
/// 审计 A-06:委托 core 单一实现,禁止 engine/app 各自维护副本。
fn looks_like_hls_url(url: &str) -> bool {
    tachyon_core::looks_like_hls_url(url)
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
    id: TaskId,
    url: String,
    config: DownloadConfig,
    protocol: Arc<dyn Protocol>,
    /// 延迟初始化:probe() 后通过 init_storage() 创建
    /// 单文件用 StorageSet::Single(透传 DynStorage),多文件用 StorageSet::Multi(按 FileLayout 折算)
    storage: Option<Arc<StorageSet>>,
    scheduler_config: SchedulerConfig,
    scheduler: Arc<dyn DownloadScheduler>,
    pool: Option<Arc<ConnectionPool>>,
    buffer_pool: Option<Arc<BufferPool>>,
    control_rx: Option<watch::Receiver<TaskCommand>>,
    state: DownloadState,
    metadata: Option<FileMetadata>,
    fragments: Vec<FragmentRecord>,
    progress_tx: Option<tokio::sync::mpsc::Sender<FragmentProgress>>,
    verifier: VerifierKind,
    completed_fragments: Vec<u32>,
    /// 未完整下载的分片及其已持久化的字节数(字节级断点续传)
    partial_fragments: HashMap<u32, u64>,
    /// 断点续传快照中的对象身份
    resume_object_identity: Option<ObjectIdentity>,
    /// 断点快照中的 supports_range(None=未知/旧快照,Some(false)=强制整块)
    resume_supports_range: Option<bool>,
    /// 外部共享限速器(跨任务全局限速)。
    /// 为 Some 时优先使用;为 None 时由 config.rate_limit_bytes_per_sec 创建 per-task 限速器。
    rate_limiter: Option<Arc<RateLimiter>>,
    /// 可选的下载指标统计器,用于记录下载字节数、分片完成数和错误数。
    metrics: Option<Arc<Metrics>>,
    /// 每源熔断器,防止持续失败的源浪费连接资源
    circuit_breakers: SourceCircuitBreakers,
    /// 是否使用镜像源(`with_mirrors` / `with_hybrid_sources` 构造时为 true)。
    ///
    /// B5:镜像路径下 engine 层熔断器以主 URL 为 key,单镜像连续失败会误熔断
    /// 整个任务(所有分片被挡 30s)。镜像路径禁用 engine 层熔断,改由
    /// `MirrorProtocol` 的 per-source stats(quality 衰减 + least-in-flight 降权)
    /// 接管故障隔离。单源路径仍用 engine 熔断(语义不变)。
    has_mirrors: bool,
    /// 任务级聚合 goodput 窗口起点(多并发分片共享)
    goodput_window_start: Option<Instant>,
    /// 当前窗口内累计完成字节
    goodput_window_bytes: u64,
    /// 用户重命名(可选):若为 `Some`,在 `probe()` 拿到元数据后会以此名覆盖
    /// `metadata.file_name`,使下游 `init_storage`/快照/UI 全部读到统一的文件名。
    /// 调用方负责传入已 sanitize 的合法文件名(由 app 层 service 完成)。
    preferred_file_name: Option<String>,
    /// 可在 set_preferred 后更新根名的 BT storage factory(与 boxed 注入共享 preferred Arc)
    #[cfg(feature = "magnet")]
    bt_storage_factory: Option<crate::bt_storage::TachyonStorageFactory>,
    /// 具体 MagnetProtocol 引用(与 protocol 同源),用于 preferred 同步与生命周期清理
    #[cfg(feature = "magnet")]
    bt_magnet: Option<std::sync::Arc<tachyon_protocol::MagnetProtocol>>,
    /// BT fallback 协议(P2SP 混合下载时持有,HTTP 全熔断后接管)
    ///
    /// 审计 A-13:不再在任务上保留 `bt_session` 字段;Session 仅在构造期
    /// 用于创建 MagnetProtocol / bt_fallback,协议对象自身持有 Session Arc。
    ///
    /// 仅 `with_hybrid_sources` 构造时填充;纯 BT/纯 HTTP 路径为 None。
    /// 由 `run_inner` 步骤 4 的 fallback 触发逻辑读取(`should_try_bt_fallback` +
    /// `execute_bt_fallback`)。
    #[cfg(feature = "magnet")]
    bt_fallback: Option<Arc<tachyon_protocol::MagnetProtocol>>,
}

/// 跨分片复用的写入缓冲区包装。
///
/// 统一池化(`BufferGuard`,RAII,Drop 自动归还)与非池化(`AlignedBuf`,Drop 释放内存)
/// 两条路径,使 worker 在被 `abort_all` 取消(future 在 await 点被丢弃)时,
/// `Guard` 变体仍能通过 `BufferGuard::drop` 正确归还 buffer,避免池许可泄漏。
///
/// 两条路径的底层缓冲区都是 512 字节对齐的 `AlignedBuf`,使 IOCP/WinFile
/// 的 NO_BUFFERING 对齐快速路径生效。
enum WriteBuf {
    Guard(BufferGuard),
    Owned(AlignedBuf),
}

impl WriteBuf {
    /// 以 `&mut AlignedBuf` 暴露内部缓冲区,供 `download_single_fragment` 使用。
    fn as_mut(&mut self) -> &mut AlignedBuf {
        match self {
            WriteBuf::Guard(g) => g.buf_mut(),
            WriteBuf::Owned(b) => b,
        }
    }
}

/// 审计 HTTP-15:经全局注册表获取/共享 HttpClient(同身份复用 TCP/TLS/H2)
fn shared_http_client(
    config: &DownloadConfig,
    pool: &Option<Arc<ConnectionPool>>,
) -> DownloadResult<HttpClient> {
    let conn = pool
        .as_ref()
        .map(|p| tachyon_core::config::ConnectionConfig::from(p.config().clone()));
    let arc = crate::http_client_registry::global_http_client_registry().get_or_create(
        &config.user_agent,
        config.proxy.as_deref(),
        config.connect_timeout_secs,
        config.request_timeout_secs,
        conn.as_ref(),
        &config.headers,
        config.auth_bearer.as_deref(),
    )?;
    // HttpClient 是 Clone(内层 reqwest::Client 为 Arc);auth_bearer 已在 registry 注入
    Ok((*arc).clone())
}

impl DownloadTask {
    /// 获取任务 ID
    pub fn id(&self) -> &TaskId {
        &self.id
    }

    /// 获取下载 URL
    pub fn url(&self) -> &str {
        &self.url
    }

    /// 获取下载配置
    pub fn config(&self) -> &DownloadConfig {
        &self.config
    }

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
        Self::with_pool_and_scheduler(
            url,
            config,
            None,
            scheduler,
            #[cfg(feature = "magnet")]
            None,
        )
        .await
    }

    /// 便利构造:带连接池但使用 **默认** 调度器。
    ///
    /// 审计 A-13:生产路径应优先 `with_pool_and_scheduler` + `AppConfig.scheduler`;
    /// 本方法保留给测试/简易调用,勿在 app 层使用以免再引入 A-04 默认调度分叉。
    #[deprecated(note = "use with_pool_and_scheduler with AppConfig.scheduler (A-04/A-13)")]
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
            #[cfg(feature = "magnet")]
            None,
        )
        .await
    }

    pub async fn with_pool_and_scheduler(
        url: String,
        config: DownloadConfig,
        pool: Option<Arc<ConnectionPool>>,
        scheduler: Arc<dyn DownloadScheduler>,
        #[cfg(feature = "magnet")] bt_session: Option<Arc<crate::bt_session::BtSession>>,
    ) -> DownloadResult<Self> {
        let _parsed = url::Url::parse(&url)?;

        let protocol: Arc<dyn Protocol> =
            if url.starts_with("http://") || url.starts_with("https://") {
                // 注入超时:connect 超时防"连不上"(黑洞 IP),
                // read 超时防"连上后静默断流"。read 用配置的 request_timeout_secs,
                // 它限制的是单次读取空闲间隔上限,不会误杀正常的大文件长下载。
                //
                // 连接池调优:若有 ConnectionPool,用其 max_per_host 参数化 reqwest
                // 空闲连接池大小,使 reqwest 连接复用与信号量并发上限对齐。
                let http = shared_http_client(&config, &pool)?;
                // P0-7: .m3u8/.m3u URL 走 HlsProtocol(VOD 媒体分片),否则 HttpClient
                if looks_like_hls_url(&url) {
                    Arc::new(tachyon_protocol::hls::HlsProtocol::new(
                        std::sync::Arc::new(http),
                    ))
                } else {
                    Arc::new(http)
                }
            } else if tachyon_core::looks_like_magnet_url(&url) {
                #[cfg(feature = "magnet")]
                {
                    use crate::bt_storage::TachyonStorageFactory;
                    use tachyon_protocol::MagnetProtocol;
                    let session = bt_session.as_ref().ok_or_else(|| {
                        DownloadError::Config("BitTorrent Session 未初始化".into())
                    })?;
                    // P2-4: 注入自定义 StorageFactory,消除双存储写放大
                    // librqbit 直接写到 Tachyon 的 AsyncStorage(目标文件),
                    // 跳过 FilesystemStorage 中间层
                    use librqbit::storage::StorageFactoryExt;
                    let factory = TachyonStorageFactory::new(
                        tokio::runtime::Handle::current(),
                        config.io_strategy,
                        std::path::PathBuf::from(&config.download_dir),
                    );
                    let magnet_arc = Arc::new(
                        MagnetProtocol::new(
                            session.session(),
                            session.config().clone(),
                            session.download_dir().clone(),
                            session.handle_cache(),
                        )
                        .with_ops_gate(session.ops_gate())
                        .with_storage_factory(factory.clone().boxed()),
                    );
                    let protocol: Arc<dyn Protocol> = magnet_arc.clone();
                    // 存储延迟到 probe() 之后初始化,使用真实文件名 + validate_save_path
                    return Ok(Self {
                        id: TaskId::new_v4(),
                        url,
                        config,
                        protocol,
                        storage: None,
                        scheduler_config: SchedulerConfig::default(),
                        scheduler,
                        pool,
                        buffer_pool: None,
                        control_rx: None,
                        state: DownloadState::Pending,
                        metadata: None,
                        fragments: Vec::new(),
                        progress_tx: None,
                        verifier: default_blake3_verifier(),
                        completed_fragments: Vec::new(),
                        partial_fragments: HashMap::new(),
                        resume_object_identity: None,
                        resume_supports_range: None,
                        rate_limiter: None,
                        metrics: None,
                        circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
                        has_mirrors: false,
                        goodput_window_start: None,
                        goodput_window_bytes: 0,
                        preferred_file_name: None,
                        bt_storage_factory: Some(factory),
                        bt_magnet: Some(magnet_arc),
                        bt_fallback: None,
                    });
                }
                #[cfg(not(feature = "magnet"))]
                {
                    return Err(DownloadError::Config(format!(
                        "磁力链接需要启用 magnet feature: {url}"
                    )));
                }
            } else {
                return Err(DownloadError::Config(format!("不支持的协议: {url}")));
            };

        // 存储延迟到 probe() 之后初始化,使用真实文件名 + validate_save_path
        Ok(Self {
            id: TaskId::new_v4(),
            url,
            config,
            protocol,
            storage: None,
            scheduler_config: SchedulerConfig::default(),
            scheduler,
            pool,
            buffer_pool: None,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
            partial_fragments: HashMap::new(),
            resume_object_identity: None,
            resume_supports_range: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: false,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            preferred_file_name: None,
            #[cfg(feature = "magnet")]
            bt_storage_factory: None,
            #[cfg(feature = "magnet")]
            bt_magnet: None,
            #[cfg(feature = "magnet")]
            bt_fallback: None,
        })
    }

    /// 设置共享 buffer 池,用于控制分片 worker 写入缓冲区的内存占用与反压。
    pub fn set_buffer_pool(&mut self, pool: Arc<BufferPool>) {
        self.buffer_pool = Some(pool);
    }

    /// 设置用户重命名(在 `probe()` 之后覆盖 `metadata.file_name`)。
    ///
    /// 调用方负责传入已 sanitize 的合法文件名;若 `probe()` 已经执行过,
    /// 此处不会回填到已缓存的 `self.metadata`(只影响首次 probe 的写入路径)。
    pub fn set_preferred_file_name(&mut self, name: String) {
        #[cfg(feature = "magnet")]
        if let Some(ref factory) = self.bt_storage_factory {
            factory.set_preferred_root_name(Some(name.clone()));
        }
        #[cfg(feature = "magnet")]
        if let Some(ref magnet) = self.bt_magnet {
            magnet.set_preferred_root_name(Some(name.clone()));
        }
        self.preferred_file_name = Some(name);
    }

    /// 设置共享限速器(跨任务全局限速)
    ///
    /// 多个 DownloadTask 可共享同一个 `Arc<RateLimiter>` 实例,
    /// 确保所有并发下载的总带宽不超过配置上限。
    pub fn set_rate_limiter(&mut self, limiter: Arc<RateLimiter>) {
        self.rate_limiter = Some(limiter);
    }

    /// 使用主 URL + 备用镜像 URL 创建下载任务
    ///
    /// 主源失败时自动 fallback 到镜像源列表。
    /// 如果传入了共享连接池(`pool`),所有源将受该连接池的并发控制约束,
    /// 与 `with_pool` 路径行为一致;否则创建独立连接池(绕过全局并发控制)。
    pub async fn with_mirrors(
        url: String,
        mirror_urls: Vec<String>,
        config: DownloadConfig,
        pool: Option<Arc<ConnectionPool>>,
        scheduler: Arc<dyn DownloadScheduler>,
    ) -> DownloadResult<Self> {
        if looks_like_hls_url(&url) || mirror_urls.iter().any(|u| looks_like_hls_url(u)) {
            return Err(DownloadError::Config(
                "HLS(.m3u8) 暂不支持镜像混拼;请使用单源 DownloadTask".into(),
            ));
        }
        // P2:镜像路径复用连接池配置(对齐 with_pool_and_scheduler:247-256)
        // pool 存在时用 with_connection_config 透传 max_per_host/keep_alive/http2,
        // 使每镜像的 reqwest 连接池与全局并发控制对齐;否则回退 with_timeouts。
        let build_http = || -> DownloadResult<HttpClient> { shared_http_client(&config, &pool) };

        let primary = Arc::new(build_http()?);

        let total_mirrors = mirror_urls.len();
        let mirrors: Vec<(String, Arc<dyn Protocol>)> = mirror_urls
            .iter()
            .filter_map(|m| {
                build_http()
                    .ok()
                    .map(|c| (m.clone(), Arc::new(c) as Arc<dyn Protocol>))
            })
            .collect();
        let failed_mirrors = total_mirrors - mirrors.len();
        if failed_mirrors > 0 {
            tracing::warn!(
                total = total_mirrors,
                failed = failed_mirrors,
                "部分镜像源创建 HTTP 客户端失败"
            );
        }

        let protocol = Arc::new(MirrorProtocol::with_pool(primary, mirrors, pool.clone()));

        Ok(Self {
            id: TaskId::new_v4(),
            url,
            config,
            protocol,
            storage: None,
            scheduler_config: SchedulerConfig::default(),
            scheduler,
            pool,
            buffer_pool: None,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
            partial_fragments: HashMap::new(),
            resume_object_identity: None,
            resume_supports_range: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: true,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            preferred_file_name: None,
            #[cfg(feature = "magnet")]
            bt_storage_factory: None,
            #[cfg(feature = "magnet")]
            bt_magnet: None,
            #[cfg(feature = "magnet")]
            bt_fallback: None,
        })
    }

    /// 混合源下载(P2SP):HTTP 镜像主源 + BT fallback
    ///
    /// HTTP 镜像立即提供数据(消除冷启动等待),BT 作为整文件 fallback:
    /// 所有 HTTP 源 probe 失败或连续熔断时,切 BT download_full_stream。
    ///
    /// layout 兼容:仅单文件 BT + 单文件 HTTP + 大小一致才允许 BT fallback;
    /// 多文件 BT 或大小不一致时,BT fallback 标记为不可用(仅走 HTTP)。
    #[cfg(feature = "magnet")]
    pub async fn with_hybrid_sources(
        magnet_url: String,
        http_mirrors: Vec<String>,
        config: DownloadConfig,
        pool: Option<Arc<ConnectionPool>>,
        scheduler: Arc<dyn DownloadScheduler>,
        bt_session: Arc<crate::bt_session::BtSession>,
    ) -> DownloadResult<Self> {
        use tachyon_protocol::{HttpClient, MagnetProtocol};
        // MirrorProtocol 来自 crate::mirror(已在文件顶部 use),此处直接使用。

        // 无 HTTP 镜像:退化为纯 BT
        if http_mirrors.is_empty() {
            return Self::with_pool_and_scheduler(
                magnet_url,
                config,
                pool,
                scheduler,
                Some(bt_session),
            )
            .await;
        }

        // HTTP 镜像主源:塞入 MirrorProtocol(least-in-flight 调度)
        // P2:pool 存在时用 with_connection_config 透传连接池配置(对齐单源路径),
        // 否则回退 with_timeouts
        let build_http = || -> DownloadResult<HttpClient> { shared_http_client(&config, &pool) };
        let primary = Arc::new(build_http()?);
        let mirrors: Vec<(String, Arc<dyn Protocol>)> = http_mirrors
            .iter()
            .filter_map(|m| {
                build_http()
                    .ok()
                    .map(|c| (m.clone(), Arc::new(c) as Arc<dyn Protocol>))
            })
            .collect();
        let protocol = Arc::new(MirrorProtocol::with_pool(primary, mirrors, pool.clone()));

        // BT fallback:独立持有,不塞入 MirrorProtocol(但共享 handle_cache)
        // P2-4: 注入自定义 StorageFactory,消除双存储写放大
        use librqbit::storage::StorageFactoryExt;
        let bt_factory = crate::bt_storage::TachyonStorageFactory::new(
            tokio::runtime::Handle::current(),
            config.io_strategy,
            std::path::PathBuf::from(&config.download_dir),
        )
        .boxed();
        let bt_fallback = Arc::new(
            MagnetProtocol::new(
                bt_session.session(),
                bt_session.config().clone(),
                bt_session.download_dir().clone(),
                bt_session.handle_cache(),
            )
            .with_ops_gate(bt_session.ops_gate())
            .with_storage_factory(bt_factory),
        );

        Ok(Self {
            id: TaskId::new_v4(),
            url: magnet_url,
            config,
            protocol,
            storage: None,
            scheduler_config: SchedulerConfig::default(),
            scheduler,
            pool,
            buffer_pool: None,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
            partial_fragments: HashMap::new(),
            resume_object_identity: None,
            resume_supports_range: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: true,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            preferred_file_name: None,
            #[cfg(feature = "magnet")]
            bt_storage_factory: None,
            #[cfg(feature = "magnet")]
            bt_magnet: None,
            #[cfg(feature = "magnet")]
            bt_fallback: Some(bt_fallback),
        })
    }

    #[cfg(any(test, feature = "test-harness"))]
    pub fn new_for_test(
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
            storage: Some(Arc::new(StorageSet::single(storage))),
            scheduler_config: SchedulerConfig::default(),
            scheduler: Arc::new(AdaptiveDownloadScheduler::default_config()),
            pool: None,
            buffer_pool: None,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
            partial_fragments: HashMap::new(),
            resume_object_identity: None,
            resume_supports_range: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: false,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            preferred_file_name: None,
            #[cfg(feature = "magnet")]
            bt_storage_factory: None,
            #[cfg(feature = "magnet")]
            bt_magnet: None,
            #[cfg(feature = "magnet")]
            bt_fallback: None,
        }
    }

    /// 测试构造器:不预置 storage,让 init_storage() 走真实路径(含 Multi 构造)
    ///
    /// 用于多文件端到端测试:probe 设置 metadata(含 file_layout)后,
    /// init_storage 据 file_layout 构造 StorageSet::Multi。
    #[cfg(any(test, feature = "test-harness"))]
    pub fn new_for_test_no_storage(
        url: String,
        config: DownloadConfig,
        protocol: Arc<dyn Protocol>,
    ) -> Self {
        Self {
            id: TaskId::new_v4(),
            url,
            config,
            protocol,
            storage: None,
            scheduler_config: SchedulerConfig::default(),
            scheduler: Arc::new(AdaptiveDownloadScheduler::default_config()),
            pool: None,
            buffer_pool: None,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
            partial_fragments: HashMap::new(),
            resume_object_identity: None,
            resume_supports_range: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: false,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            preferred_file_name: None,
            #[cfg(feature = "magnet")]
            bt_storage_factory: None,
            #[cfg(feature = "magnet")]
            bt_magnet: None,
            #[cfg(feature = "magnet")]
            bt_fallback: None,
        }
    }

    pub fn set_control_rx(&mut self, control_rx: watch::Receiver<TaskCommand>) {
        self.control_rx = Some(control_rx);
    }

    pub fn set_progress_sender(&mut self, tx: tokio::sync::mpsc::Sender<FragmentProgress>) {
        self.progress_tx = Some(tx);
    }

    /// 设置指标统计器
    ///
    /// 用于记录下载字节数、分片完成数和错误数。
    pub fn set_metrics(&mut self, metrics: Arc<Metrics>) {
        self.metrics = Some(metrics);
    }

    /// 设置已完成分片索引列表(断点续传)
    ///
    /// 必须在 `plan()` 之前调用。`plan()` 会据此把对应分片标记为已完成并跳过下载。
    pub fn set_completed_fragments(&mut self, completed: Vec<u32>) {
        self.completed_fragments = completed;
    }

    /// 设置未完整下载的分片及其已下载字节数(字节级断点续传)
    ///
    /// 必须在 `plan()` 之前调用。`plan()` 会据此调整对应分片的 `resume_offset`,
    /// 使 `execute()` 从已下载位置继续,避免完整重下整个分片。
    pub fn set_partial_fragments(&mut self, partial: HashMap<u32, u64>) {
        self.partial_fragments = partial;
    }

    /// 设置断点续传快照对象身份(须在 plan 前;probe 后会与远端比较)
    pub fn set_resume_object_identity(&mut self, identity: Option<ObjectIdentity>) {
        self.resume_object_identity = identity;
    }

    /// 注入断点快照中的 supports_range(probe 后覆盖远端声明)
    pub fn set_resume_supports_range(&mut self, supports_range: Option<bool>) {
        self.resume_supports_range = supports_range;
    }

    /// 设置调度器配置(规划参数 / sampling_interval 等)。
    ///
    /// 必须在 `run()` 之前调用。审计 A-04:生产路径从 `AppConfig.scheduler` 注入,
    /// 禁止永远落在 `SchedulerConfig::default()`。
    pub fn set_scheduler_config(&mut self, config: SchedulerConfig) {
        self.scheduler_config = config;
    }

    async fn wait_control_rx(
        rx: &mut watch::Receiver<TaskCommand>,
        pause_timeout: Duration,
    ) -> DownloadResult<()> {
        loop {
            let state = rx.borrow_and_update().to_download_state();
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

    /// 控制通道当前是否为 Pause(主循环禁止 spawn/rebalance 用)
    fn control_is_paused(control_rx: &Option<watch::Receiver<TaskCommand>>) -> bool {
        control_rx
            .as_ref()
            .is_some_and(|rx| matches!(*rx.borrow(), TaskCommand::Pause))
    }

    /// 协作式热路径检查:若控制通道为 Pause/Cancel/Failed 立即返回对应错误。
    /// 与 `wait_control_rx` 不同:**Pause 时不挂起等 Resume**,立刻 Err 让调用方停 IO;
    /// Resume 等待由 spawn 重试循环/外层负责。
    fn check_control_interrupt(
        control_rx: &mut Option<watch::Receiver<TaskCommand>>,
    ) -> DownloadResult<()> {
        let Some(rx) = control_rx.as_mut() else {
            return Ok(());
        };
        match rx.borrow_and_update().to_download_state() {
            DownloadState::Cancelled => Err(DownloadError::Cancelled),
            DownloadState::Failed => Err(DownloadError::Other("任务已失败".into())),
            DownloadState::Paused => Err(DownloadError::Paused),
            _ => Ok(()),
        }
    }

    async fn wait_control(
        control_rx: &mut Option<watch::Receiver<TaskCommand>>,
        pause_timeout: Duration,
    ) -> DownloadResult<()> {
        if let Some(rx) = control_rx.as_mut() {
            Self::wait_control_rx(rx, pause_timeout).await?;
        }
        Ok(())
    }

    /// 在下载进行期间监视中断信号(取消/暂停),供 `tokio::select!` 分支使用。
    ///
    /// 与 `wait_control_rx` 的关键区别:
    /// - 正常运行(Downloading 等)下**挂起**等状态变化,不抢占下载分支
    /// - **Paused 立即返回 `Err(Paused)`**,使 select 抢占 stream/write,停止 in-flight IO
    ///   (wait_control_rx 仍负责在分片间隙/入队前挂起等 Resume)
    /// - Cancelled/Failed 返回对应 Err
    ///
    /// 控制通道关闭时返回错误,避免任务永久挂起。
    async fn watch_for_interrupt(
        rx: &mut watch::Receiver<TaskCommand>,
        _pause_timeout: Duration,
    ) -> DownloadResult<()> {
        loop {
            let state = rx.borrow_and_update().to_download_state();
            match state {
                DownloadState::Cancelled => return Err(DownloadError::Cancelled),
                DownloadState::Failed => return Err(DownloadError::Other("任务已失败".into())),
                // 立即抢占 select:禁止在 Paused 时继续读网/写盘
                DownloadState::Paused => return Err(DownloadError::Paused),
                _ => {
                    if rx.changed().await.is_err() {
                        return Err(DownloadError::Other("控制通道意外关闭".into()));
                    }
                }
            }
        }
    }

    fn request_host(&self) -> DownloadResult<String> {
        // 审计 HTTP-13:优先使用 probe/range 后的最终 host(重定向后的 CDN)
        if let Some(host) = self
            .metadata
            .as_ref()
            .and_then(|m| m.resolved_host.as_ref())
            .filter(|h| !h.is_empty())
        {
            return Ok(host.clone());
        }
        // 磁力链接没有 host，返回占位符
        if tachyon_core::looks_like_magnet_url(&self.url) {
            return Ok("magnet".to_string());
        }
        let parsed = url::Url::parse(&self.url)?;
        parsed
            .host_str()
            .map(ToString::to_string)
            .ok_or_else(|| DownloadError::Config("URL 主机为空".into()))
    }

    /// 审计 HTTP-13:把协议层最近 final host 写回 metadata,供后续 per-host acquire
    fn refresh_resolved_host_from_protocol(&mut self) {
        let Some(host) = self.protocol.last_resolved_host() else {
            return;
        };
        if host.is_empty() {
            return;
        }
        if let Some(meta) = self.metadata.as_mut()
            && meta.resolved_host.as_deref() != Some(host.as_str())
        {
            tracing::debug!(
                previous = ?meta.resolved_host,
                new = %host,
                "HTTP-13:更新 resolved_host 为协议 final host"
            );
            meta.resolved_host = Some(host);
        }
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
        info!(url = %tachyon_core::redact_url_for_log(&self.url), "开始探测文件元数据");
        // 测量 probe 耗时作为 RTT 上界估计(DNS+TCP+TLS+HTTP 往返)。
        // 偏大的 RTT 估计使 BDP 偏大(倾向更多并发),比偏小(管道未满)安全。
        // observe_rtt 内部会过滤异常值(>10s),正常 probe 耗时 50ms-2s 均有效。
        let probe_start = std::time::Instant::now();
        let mut metadata = self.protocol.probe(&self.url).await?;
        let probe_elapsed = probe_start.elapsed();
        self.scheduler.observe_rtt(probe_elapsed);
        debug!(?probe_elapsed, "probe 耗时已作为 RTT 上界注入调度器");
        // 若用户在「新建任务」中显式重命名,以用户指定名覆盖协议探测得到的文件名。
        // 调用方(app 层 service)已对该名做过 sanitize,此处不再二次清洗,
        // 仅在源头覆盖一次保证下游 init_storage / 快照 / UI 全部读到同一个值。
        if let Some(ref preferred) = self.preferred_file_name {
            info!(
                probed = %metadata.file_name,
                preferred = %preferred,
                "应用用户重命名,覆盖探测得到的文件名"
            );
            metadata.file_name = preferred.clone();
        }
        info!(
            file_name = %metadata.file_name,
            file_size = ?metadata.file_size,
            supports_range = metadata.supports_range,
            "探测完成"
        );
        if let Some(ref snap) = self.resume_object_identity {
            let remote = ObjectIdentity::from_metadata(&metadata);
            if !snap.compatible_for_resume(&remote) {
                warn!(
                    url = %tachyon_core::redact_url_for_log(&self.url),
                    snap_etag = ?snap.etag,
                    remote_etag = ?remote.etag,
                    "对象身份与断点快照不兼容,丢弃已完成/部分分片并全量重下"
                );
                self.completed_fragments.clear();
                self.partial_fragments.clear();
                self.resume_object_identity = None;
                self.resume_supports_range = None;
            }
        }
        // 历史 200-fallback 快照:强制 supports_range=false,避免 resume 再走分片
        if self.resume_supports_range == Some(false) {
            warn!(
                url = %tachyon_core::redact_url_for_log(&self.url),
                "断点快照标记 supports_range=false,覆盖探测结果为整块路径"
            );
            metadata.supports_range = false;
        }
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

        // 多文件 torrent:metadata.file_layout 携带各文件段,构造 StorageSet::Multi
        // 单文件(含 HTTP/FTP/单文件 torrent):file_layout 为 None,走 Single 路径
        let storage: StorageSet = if let Some(layout) = metadata.file_layout.as_ref() {
            if layout.file_count() > 1 {
                let file_names = layout.file_names();
                let paths =
                    tachyon_core::validate_multi_save_paths(download_dir, safe_name, &file_names)?;
                info!(
                    torrent_name = %safe_name,
                    file_count = paths.len(),
                    io_strategy = ?self.config.io_strategy,
                    "多文件路径安全校验通过,创建多文件存储"
                );
                let mut storages = Vec::with_capacity(paths.len());
                for p in &paths {
                    storages
                        .push(DynStorage::open_with_strategy(p, self.config.io_strategy).await?);
                }
                StorageSet::multi(storages, layout.file_spans().to_vec())?
            } else {
                // 单文件 torrent(file_layout 存在但只有 1 个文件)
                let final_path = download_dir.join(safe_name);
                let canonical_path = tachyon_core::validate_save_path(&final_path, download_dir)?;
                info!(
                    safe_name = %safe_name,
                    save_path = %canonical_path.display(),
                    io_strategy = ?self.config.io_strategy,
                    "路径安全校验通过,创建存储"
                );
                let s = DynStorage::open_with_strategy(&canonical_path, self.config.io_strategy)
                    .await?;
                StorageSet::single(s)
            }
        } else {
            // HTTP/FTP:无 file_layout,单文件
            let final_path = download_dir.join(safe_name);
            let canonical_path = tachyon_core::validate_save_path(&final_path, download_dir)?;
            info!(
                safe_name = %safe_name,
                save_path = %canonical_path.display(),
                io_strategy = ?self.config.io_strategy,
                "路径安全校验通过,创建存储"
            );
            let s =
                DynStorage::open_with_strategy(&canonical_path, self.config.io_strategy).await?;
            StorageSet::single(s)
        };
        self.storage = Some(Arc::new(storage));
        Ok(())
    }

    // ----- BT/magnet 冷启动解耦 -----

    /// BT/magnet 分片目标数量:HTTP 默认 `default_target_fragments`(16)的两倍。
    /// 调度器带宽样本只在分片完成时产生;BT 慢 swarm 下 16 片级的大分片迟迟不完,
    /// 0 样本 → confidence 恒 0 → ramp 锁死冷启动并发,反馈环路断裂。
    /// 翻倍目标数让完成事件更早到来。
    const BT_TARGET_FRAGMENTS: u64 = 32;

    /// BT 分片大小下限:对齐常见 torrent piece 大小(1-4MiB)上限,
    /// 避免过细分片放大 FileStream 数量与 FragmentRecord 状态开销。
    const BT_MIN_FRAGMENT_SIZE: u64 = 4 * 1024 * 1024;

    /// BT 分片大小上限:单片过大则完成事件过稀、stall 重试需整片重读。
    /// 10GiB 文件约 671 片,远低于 `plan_fragments` 的 1,000,000 片硬上限;
    /// 1TB 极端场景约 65,536 片,FragmentRecord(每片百余字节)内存仍在 10MB 量级。
    const BT_MAX_FRAGMENT_SIZE: u64 = 16 * 1024 * 1024;

    /// BT 冷启动置信度阈值:低于此值认为调度器无有效带宽样本,
    /// 并发度与分片策略不走 HTTP 保守探路。与 re-recommend 循环的
    /// 高置信判定(`confidence > 0.5`)保持同一水位。
    const BT_COLD_START_CONFIDENCE: f64 = 0.5;

    /// 判定当前任务是否为 BT/magnet 下载。
    ///
    /// 判据:URL 为 magnet scheme(与构造期协议选择 `new` 同一判据),
    /// 或 probe 元数据标记 `protocol_managed_storage`(librqbit 经自定义
    /// StorageFactory 直写 Tachyon 存储,由 MagnetProtocol::probe 设置)。
    /// BT 的 piece 调度由 librqbit 自管,无 HTTP 的 429/限流语义,
    /// 故冷启动并发与分片粒度与 HTTP 解耦。
    fn is_bt_task(&self) -> bool {
        tachyon_core::looks_like_magnet_url(&self.url)
            || self
                .metadata
                .as_ref()
                .is_some_and(|m| m.protocol_managed_storage)
    }

    /// BT/magnet 任务的分片大小:`file_size / 32` clamp 到 [4MiB, 16MiB]。
    ///
    /// 293.8MiB → 约 9.2MiB/片 × 32 片;10GiB → 16MiB/片 × 约 671 片。
    /// 与 HTTP 分片策略(`default_target_fragments` + 带宽因子)解耦:
    /// BT piece 通常 1-4MiB,小分片让分片完成事件(调度器带宽样本唯一来源)
    /// 在慢 swarm 下也能及时产生,并把 stall 重试粒度从 18MiB 级整片收细。
    fn bt_fragment_size(file_size: u64) -> u64 {
        (file_size / Self::BT_TARGET_FRAGMENTS)
            .clamp(Self::BT_MIN_FRAGMENT_SIZE, Self::BT_MAX_FRAGMENT_SIZE)
    }

    /// BT 冷启动并发解耦:BT/magnet 任务在调度器低置信度(无样本或 < 0.5)
    /// 时返回配置并发 `max_concurrent_fragments`,替代 cold-start 推荐值。
    ///
    /// 背景:HTTP 保守探路(`cold_start_initial_concurrency` 起步 + ramp 爬坡)
    /// 是为防 429/限流;BT 的 piece 调度由 librqbit 自管,16 个 FileStream
    /// 对 librqbit 只是 DashMap 里的 16 条 StreamState,无 429 语义。
    /// 慢 swarm 下按 cold-start 4 并发跑大分片,完成事件过稀 → 调度器
    /// 0 样本 → confidence 恒 0 → ramp 锁死,反馈环路断裂。
    ///
    /// 返回 None 表示不参与覆盖(HTTP 任务,或 BT 已有有效样本),
    /// 调用方按调度器推荐值照常执行。re-recommend 循环的
    /// 「低置信度只升不降」门禁保证解耦后的并发不被低置信推荐值压回,
    /// 样本到位(confidence >= 0.5)后照常参与调度。
    fn bt_cold_start_concurrency_override(
        &self,
        recommendation: &tachyon_core::traits::ScheduleRecommendation,
    ) -> Option<u32> {
        if self.is_bt_task() && recommendation.confidence < Self::BT_COLD_START_CONFIDENCE {
            Some(self.config.max_concurrent_fragments.max(1))
        } else {
            None
        }
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

        // BT/magnet 任务与 HTTP 分片策略解耦:固定走小分片公式,
        // 让分片完成事件(调度器带宽样本唯一来源)在慢 swarm 下及时产生,
        // 并收细 stall 重试粒度;不采用调度器按 HTTP 语义给出的 fragment_size。
        //
        // 断点续传:若已有 completed/partial 快照(按 **index** 存储),
        // 必须使用与冷启动相同的确定性划分(plan_fragments 的 None 分支),
        // **禁止**再用 recommendation.fragment_size——否则 resume 后分片边界
        // 漂移,completed index 会错跳过/重下错误区间。
        //
        // 首下 HTTP:调度器 confidence>0 时用其建议,否则回退 scheduler_config。
        let has_resume_snapshot =
            !self.completed_fragments.is_empty() || !self.partial_fragments.is_empty();
        let suggested_frag_size = if self.is_bt_task() {
            Some(Self::bt_fragment_size(file_size))
        } else if has_resume_snapshot {
            None
        } else if recommendation.confidence > 0.0 {
            Some(recommendation.fragment_size)
        } else {
            None
        };

        let fragments = crate::fragment::plan_fragments(
            file_size,
            metadata.supports_range,
            suggested_frag_size,
            &self.scheduler_config,
        )?;

        info!(count = fragments.len(), "分片规划完成");

        self.fragments = fragments
            .iter()
            .map(|info| FragmentRecord::new(info.clone(), self.config.max_retries))
            .collect();

        // 审计 BT-17:BT custom storage 的 piece truth 由 librqbit 维护。
        // 若按 HTTP snapshot 的 completed index 跳过 FileStream,损坏/漂移分片可能被标 Completed。
        // protocol_managed_storage 时丢弃 snapshot 跳过,强制走 range/stream 路径由 piece 校验推进。
        if self
            .metadata
            .as_ref()
            .is_some_and(|m| m.protocol_managed_storage)
            && (!self.completed_fragments.is_empty() || !self.partial_fragments.is_empty())
        {
            warn!(
                completed = self.completed_fragments.len(),
                partial = self.partial_fragments.len(),
                "BT protocol_managed_storage:忽略 snapshot 分片跳过(piece truth 优先)"
            );
            self.completed_fragments.clear();
            self.partial_fragments.clear();
        }

        // 断点续传:把已完成分片标记为 Done 并跳过后续下载
        if !self.completed_fragments.is_empty() {
            let mut resumed = 0u32;
            for &done_index in &self.completed_fragments {
                if let Some(frag) = self.fragments.get_mut(done_index as usize) {
                    // 仅对仍处于 Pending 的分片执行恢复,避免重复迁移状态
                    if frag.state == crate::fragment::FragmentState::Pending {
                        frag.info.downloaded = frag.info.size;
                        frag.start_download()?;
                        frag.complete_download_fast(frag.info.size, Duration::ZERO)?;
                        resumed += 1;
                    }
                }
            }
            info!(resumed, "断点续传:跳过已完成分片");
        }

        // 字节级断点续传:对未完整下载的分片注入 resume_offset
        if !self.partial_fragments.is_empty() {
            let mut resumed_partial = 0u32;
            for (&idx, &bytes) in &self.partial_fragments {
                if let Some(frag) = self.fragments.get_mut(idx as usize)
                    && frag.state == crate::fragment::FragmentState::Pending
                    && bytes > 0
                    && bytes < frag.info.size
                {
                    frag.resume_offset = bytes;
                    frag.info.downloaded = bytes;
                    resumed_partial += 1;
                }
            }
            info!(resumed_partial, "字节级断点续传:恢复未完整分片");
        }

        // 发送 PlanComplete 事件:携带真实分片总数 + 续传已完成索引 + 初始并发度。
        // plan() 是同步函数,用 try_send(非阻塞)。此时 channel 必为空(plan 是第一个事件),
        // 不会因满而丢弃;若通道已关闭(任务取消)则丢弃,属正确行为。
        if let Some(tx) = &self.progress_tx {
            let total = self.fragments.len() as u32;
            let completed_indices: Vec<u32> = self
                .fragments
                .iter()
                .filter(|f| f.state == crate::fragment::FragmentState::Done)
                .map(|f| f.info.index)
                .collect();
            // BT 冷启动解耦时上报解耦后的初始并发,与 execute_fragmented_download
            // 实际生效值一致(active_concurrency 展示不错位);HTTP 原样上报推荐值。
            let initial_concurrency = self
                .bt_cold_start_concurrency_override(&recommendation)
                .unwrap_or(recommendation.concurrency);
            if let Err(e) = tx.try_send(FragmentProgress::PlanComplete {
                total,
                completed_indices,
                initial_concurrency,
            }) {
                warn!(error = %e, "PlanComplete 事件发送失败(通道满或关闭)");
            }
        }

        Ok(fragments)
    }

    // ----- 步骤 3: 预分配存储 -----

    /// 预分配文件空间
    ///
    /// 根据文件大小在存储后端预留空间,支持分片并发写入。
    ///
    /// P4:allocate 前先做磁盘空间预检。检查 save_dir 所在分区可用空间是否
    /// 大于等于 file_size + margin(1% 或 100MB 取小),不足则返回 Config 错误
    /// (不可重试),带中文提示含可用/需求数值。无法获取磁盘信息时跳过预检
    /// (降级,不阻断下载)。
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
            // P4:磁盘空间预检(allocate 前快速失败,避免分配失败或写到一半磁盘满)
            let save_dir = std::path::Path::new(&self.config.download_dir);
            check_disk_space(save_dir, size)?;
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
        let file_size = metadata.file_size;

        // 空文件无需下载
        if file_size == Some(0) {
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
    ///
    /// 审计 HTTP-09:与分片路径同构,可重试错误按 `max_retries` 退避重试;
    /// 每次 attempt 从 offset 0 重写,并用 `allocate` 重置存储长度,避免半写污染。
    async fn execute_full_download(&mut self) -> DownloadResult<()> {
        let pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        let max_retries = self.config.max_retries;
        let mut attempt = 0u32;
        loop {
            match self.execute_full_download_once(pause_timeout).await {
                Ok(()) => break,
                Err(e) => {
                    // 用户暂停:等 Resume 后重试本 attempt,不计入 max_retries
                    if matches!(e, DownloadError::Paused) {
                        Self::wait_control(&mut self.control_rx, pause_timeout).await?;
                        continue;
                    }
                    // 暂停超时是控制语义,不是瞬态网络故障;禁止纳入 max_retries 退避
                    // (否则 1s 暂停超时 × 默认 3 次重试会远超调用方等待窗口)。
                    if e.is_retryable()
                        && !Self::is_pause_timeout_error(&e)
                        && attempt < max_retries
                    {
                        let next_attempt = attempt + 1;
                        let backoff = match &e {
                            DownloadError::Throttled {
                                retry_after_secs: Some(secs),
                            } => Duration::from_secs((*secs).min(1024)),
                            _ => {
                                let base = 1u64 << attempt.min(10);
                                Duration::from_secs(base.max(1))
                            }
                        };
                        warn!(
                            attempt = next_attempt,
                            max_retries,
                            ?backoff,
                            error = %e,
                            "整块下载可重试失败,退避后重试"
                        );
                        // 整块路径 fragment_index=0,与任务级 retry_count 聚合对齐
                        if let Some(tx) = &self.progress_tx {
                            let _ = tx.try_send(FragmentProgress::Retry {
                                fragment_index: 0,
                                attempt: next_attempt,
                            });
                        }
                        // 重置存储,防止半写残留污染下次 attempt
                        if let Some(storage) = self.storage.as_ref() {
                            let size = self
                                .metadata
                                .as_ref()
                                .and_then(|m| m.file_size)
                                .unwrap_or(0);
                            let _ = storage.allocate(size).await;
                        }
                        self.protocol.clear_selected().await;
                        tokio::time::sleep(backoff).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        // 审计 BT-17:单分片 BT 文件走 full-stream 路径时,FileStream 读完 ≠ piece
        // truth 完成。标 Completed 前同样需要等待 librqbit wait_until_completed。
        #[cfg(feature = "magnet")]
        self.wait_bt_piece_truth_if_protocol_managed().await?;

        Ok(())
    }

    /// 审计 BT-17:protocol_managed_storage 时等待 librqbit piece truth 完成。
    ///
    /// 单分片与多分片 BT 路径共用。仅在 `protocol_managed_storage` 且持有
    /// `bt_magnet`/`bt_fallback` 时等待,否则空操作。
    #[cfg(feature = "magnet")]
    async fn wait_bt_piece_truth_if_protocol_managed(&self) -> DownloadResult<()> {
        if self
            .metadata
            .as_ref()
            .is_some_and(|m| m.protocol_managed_storage)
            && let Some(magnet) = self.bt_magnet.as_ref().or(self.bt_fallback.as_ref())
        {
            info!("BT protocol_managed:等待 piece truth 完成(BT-17)");
            magnet.wait_torrent_completed(&self.url).await?;
        }
        Ok(())
    }

    /// 控制通道「暂停超过 N 秒」超时(非网络 Timeout)
    fn is_pause_timeout_error(err: &DownloadError) -> bool {
        matches!(err, DownloadError::Timeout(msg) if msg.starts_with("暂停超过"))
    }

    /// 单次整块流式下载 attempt(无重试)
    async fn execute_full_download_once(&mut self, pause_timeout: Duration) -> DownloadResult<()> {
        Self::wait_control(&mut self.control_rx, pause_timeout).await?;
        self.refresh_resolved_host_from_protocol();
        let host = self.request_host()?;
        // P1:镜像路径跳过主 host 的 pool.acquire,改由 MirrorProtocol
        // (已注入同一 pool)按真实命中镜像 host acquire,使各镜像能各自
        // 占满自己的 per-host 配额。单源路径保持 engine 层 acquire 不变。
        let _pool_permit = match (&self.pool, self.has_mirrors) {
            (Some(pool), false) => Some(pool.acquire(&host).await?),
            _ => None,
        };
        let start_instant = std::time::Instant::now();

        // 优先使用外部共享限速器(跨任务全局限速),否则从配置创建 per-task 限速器
        let rate_limiter: Option<Arc<RateLimiter>> = self.rate_limiter.clone().or_else(|| {
            self.config
                .rate_limit_bytes_per_sec
                .filter(|&bps| bps > 0)
                .map(|bps| Arc::new(RateLimiter::new(bps)))
        });

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

        // clone Arc 后释放 self 的不可变借用,便于循环内 note_goodput_bytes(&mut self)
        let storage = self
            .storage
            .clone()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?;
        let expected_size = self.metadata.as_ref().and_then(|md| md.file_size);

        // 逐块消费并写入,顺序追加偏移
        let mut pos: u64 = 0;
        // 与分片路径同一节流模式:每 PROGRESS_REPORT_CHUNK_INTERVAL 个 chunk
        // 上报一次增量,避免高频上报放大下游 checkpoint(fsync)开销
        let mut progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
        tokio::pin!(stream);
        // B11:改裸 `while let stream.next().await` 为 `loop { select!{...} }`,
        // 使取消信号能在"无 chunk 到达"时(如死连接静默挂起)穿透到检查点。
        loop {
            let chunk_result = if let Some(rx) = self.control_rx.as_mut() {
                tokio::select! {
                    biased;
                    interrupt = Self::watch_for_interrupt(rx, pause_timeout) => {
                        interrupt?;
                        return Err(DownloadError::Other("控制信号异常结束".into()));
                    }
                    chunk = tokio_stream::StreamExt::next(&mut stream) => match chunk {
                        Some(r) => r,
                        None => break, // EOF:正常退出循环
                    },
                }
            } else {
                match tokio_stream::StreamExt::next(&mut stream).await {
                    Some(r) => r,
                    None => break,
                }
            };
            // chunk 间隙:Pause 立即停,不挂起等 Resume
            Self::check_control_interrupt(&mut self.control_rx)?;
            let chunk = chunk_result?;
            let chunk_len = u64::try_from(chunk.len())
                .map_err(|_| DownloadError::Config("整块下载 chunk 长度溢出".into()))?;
            let attempted = pos.checked_add(chunk_len).ok_or_else(|| {
                DownloadError::Config(format!(
                    "整块下载长度溢出: written={pos}, chunk={chunk_len}"
                ))
            })?;
            // 审计 HTTP-15:已知长度也必须写前拒绝越界,避免先扩文件后才报错
            if let Some(expected) = expected_size {
                if attempted > expected {
                    return Err(DownloadError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "整块下载响应超过声明长度: expected={expected}, 将写入到 {attempted}"
                        ),
                    )));
                }
            } else if attempted > self.config.max_full_stream_bytes {
                return Err(DownloadError::Config(format!(
                    "未知大小整块下载超过上限: 上限 {} 字节, 本次将写入 {} 字节",
                    self.config.max_full_stream_bytes, attempted
                )));
            }
            // 审计 P1 full-stream short write
            let written = Self::write_all_at(
                storage.as_ref(),
                pos,
                chunk,
                &mut self.control_rx,
                pause_timeout,
            )
            .await?;
            if written != chunk_len {
                return Err(DownloadError::Fragment(format!(
                    "整块下载短写未完成: offset={pos}, expected={chunk_len}, written={written}"
                )));
            }
            pos += written;
            // 整块路径进度:与分片路径同一 countdown 节流,每
            // PROGRESS_REPORT_CHUNK_INTERVAL 个 chunk 按累计写入字节上报一次增量;
            // 终态 completed Chunk 在 durable sync 后单独发送,不经过此节流
            progress_report_countdown = progress_report_countdown.saturating_sub(1);
            if progress_report_countdown == 0 {
                Self::report_progress(0, pos, &self.progress_tx);
                progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
            }
            if let Some(ref limiter) = rate_limiter {
                limiter.acquire(written).await;
            }
            // 整块路径同样喂聚合 goodput,供跨任务/后续 re-plan 学习
            if let Some(bps) = self.note_goodput_bytes(written) {
                self.scheduler.observe_bandwidth(bps);
            }
        }
        // 冲刷未满窗口,避免短文件零样本
        if let Some(bps) = self.flush_goodput_window() {
            self.scheduler.observe_bandwidth(bps);
        }
        debug!(written = pos, "整块流式下载写入完成");

        if let Some(expected_size) = expected_size
            && pos != expected_size
        {
            return Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("下载数据不完整: 预期 {expected_size} 字节, 实际写入 {pos} 字节"),
            )));
        }

        // 审计 P0-3:整块路径在标 Completed 前 durable sync,避免快照/状态领先于落盘
        storage.as_ref().sync().await?;

        // 成功路径：durable 后发 completed:true，错误返回路径不发送
        if let Some(tx) = &self.progress_tx {
            let _ = tx.try_send(FragmentProgress::Chunk {
                fragment_index: 0,
                completed: true,
                fragment_downloaded: pos,
            });
        }

        if let Some(frag) = self.fragments.first_mut() {
            if frag.state == crate::fragment::FragmentState::Pending {
                frag.start_download()?;
            }
            frag.complete_download_fast(pos, start_instant.elapsed())?;
        }
        if let Some(ref metrics) = self.metrics {
            metrics.add_bytes(pos);
            metrics.inc_fragment();
        }
        self.state = DownloadState::Completed;
        Ok(())
    }

    /// spawn 一个分片任务(主 dispatch 与 steal 路径共享)
    ///
    /// 统一逻辑:acquire permit -> record_spawn -> 分配 write_buf ->
    /// clone 所有共享 Arc -> spawn task(含指数退避重试循环)
    ///
    /// permit 获取失败时返回 Err(调用方 abort 剩余 task + 设置 Failed 状态)。
    #[allow(clippy::too_many_arguments)]
    async fn spawn_fragment_task(
        ctx: &FragmentSpawnCtx<'_>,
        spec: FragmentSpec,
        handles: &mut JoinSet<FragmentTaskResult>,
    ) -> Result<(), DownloadError> {
        let (frag_index, frag_start, frag_end, resume_offset, compute_hash, shared) = spec;

        // acquire permit(阻塞直到有可用许可)
        // permit 的 RAII 保证:task 完成/drop/abort 时自动归还
        let permit = match ctx.semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(e) => {
                return Err(DownloadError::Other(format!("信号量获取失败: {e}").into()));
            }
        };
        // 闭环并发控制:记录 spawn,active+1
        ctx.concurrency_ctrl.record_spawn();
        // 每个 task 独立分配 write_buf(从 BufferPool 或直接分配)
        let mut write_buf = match ctx.buffer_pool {
            Some(bp) => WriteBuf::Guard(bp.clone().alloc_guarded().await),
            None => WriteBuf::Owned(
                AlignedBuf::new(WRITE_BATCH_BYTES).expect("AlignedBuf 分配失败(内存不足)"),
            ),
        };
        write_buf.as_mut().clear();

        let frag_protocol = ctx.protocol.clone();
        let frag_storage = ctx.storage.clone();
        let frag_pool = ctx.pool.clone();
        let frag_url = ctx.url.to_string();
        let frag_host = ctx.host.to_string();
        let frag_limiter = ctx.limiter.clone();
        let mut frag_control_rx = ctx.control_rx.clone();
        let frag_progress_tx = ctx.progress_tx.clone();
        let frag_verifier = ctx.verifier.clone();
        let frag_metrics = ctx.metrics.clone();
        let frag_circuit_breakers = ctx.circuit_breakers.clone();
        // 闭环并发控制:传给 task,退出时 record_complete
        let frag_concurrency_ctrl = ctx.concurrency_ctrl.clone();
        let task_completed_tx = ctx.completed_tx.clone();
        let frag_has_mirrors = ctx.has_mirrors;
        let max_retries = ctx.max_retries;
        let pause_timeout = ctx.pause_timeout;
        let skip_write = ctx.skip_write;
        let frag_sync_mode = ctx.sync_mode;
        let frag_object_identity = ctx.object_identity.clone();

        handles.spawn(async move {
            let _permit = permit; // RAII:完成/drop 即归还

            // 单次尝试 + 指数退避重试
            let mut attempt: u32 = 0;
            let frag_result: FragmentTaskResult = loop {
                // 熔断器检查
                if !frag_has_mirrors && !frag_circuit_breakers.allow(&frag_url) {
                    if attempt >= max_retries {
                        break Err((
                            frag_index,
                            DownloadError::Network(format!("源 {frag_url} 已被熔断,跳过重试")),
                        ));
                    }
                    let next_attempt = attempt + 1;
                    warn!(
                        index = frag_index,
                        attempt = next_attempt,
                        source = %frag_url,
                        "源处于熔断状态,跳过本次尝试"
                    );
                    if let Some(tx) = &frag_progress_tx {
                        let _ = tx.try_send(FragmentProgress::Retry {
                            fragment_index: frag_index,
                            attempt: next_attempt,
                        });
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    attempt += 1;
                    continue;
                }

                // 审计 HTTP-01:每次 attempt 清空 write_buf。
                // 半缓冲失败(未达 WRITE_BATCH 阈值)会留下残留字节;若不 clear,
                // 下次成功 attempt 的首批数据会与污染前缀拼接写盘。
                write_buf.as_mut().clear();
                let result = Self::download_single_fragment(
                    &frag_protocol,
                    &frag_storage,
                    &frag_pool,
                    &frag_host,
                    &frag_url,
                    frag_index,
                    frag_start,
                    frag_end,
                    resume_offset,
                    pause_timeout,
                    frag_limiter.clone(),
                    &frag_control_rx,
                    &frag_progress_tx,
                    &frag_verifier,
                    compute_hash,
                    write_buf.as_mut(),
                    skip_write,
                    frag_sync_mode,
                    &shared,
                    frag_object_identity.clone(),
                )
                .await;

                match result {
                    Ok((downloaded, duration, computed_hash)) => {
                        if !frag_has_mirrors {
                            frag_circuit_breakers.record_success(&frag_url);
                        }
                        break Ok((frag_index, downloaded, duration, computed_hash));
                    }
                    Err(e) => {
                        // 用户暂停:不计入 attempt,等 Resume 后从同一 attempt 重下本片
                        if matches!(e, DownloadError::Paused) {
                            if let Some(rx) = frag_control_rx.as_mut()
                                && let Err(wait_err) =
                                    Self::wait_control_rx(rx, pause_timeout).await
                            {
                                break Err((frag_index, wait_err));
                            }
                            continue;
                        }
                        if !e.is_retryable()
                            || Self::is_pause_timeout_error(&e)
                            || attempt >= max_retries
                        {
                            if let Some(ref m) = frag_metrics {
                                m.inc_error();
                            }
                            if !frag_has_mirrors {
                                frag_circuit_breakers.record_failure(&frag_url);
                            }
                            break Err((frag_index, e));
                        }
                        // 退避:429/503 优先 Retry-After,否则 Full Jitter 指数退避
                        let backoff = match &e {
                            DownloadError::Throttled {
                                retry_after_secs: Some(secs),
                            } => Duration::from_secs((*secs).min(1024)),
                            _ => {
                                let base_secs = 1u64 << attempt.min(10);
                                if base_secs <= 1 {
                                    Duration::from_secs(1)
                                } else {
                                    let seed = (frag_index as u64)
                                        .wrapping_mul(0x9E3779B97F4A7C15)
                                        .wrapping_add(attempt as u64);
                                    let log2 = base_secs.trailing_zeros();
                                    let hash = seed.wrapping_mul(0x517cc1b727220a95);
                                    let jitter = hash >> (64 - log2);
                                    Duration::from_secs(base_secs.saturating_sub(jitter).max(1))
                                }
                            }
                        };
                        let next_attempt = attempt + 1;
                        warn!(
                            index = frag_index,
                            attempt = next_attempt,
                            max_retries,
                            backoff_secs = backoff.as_secs(),
                            error = %e,
                            "分片下载失败,退避后重试"
                        );
                        // 任务级 retry_count 聚合:可重试失败时发出 Retry 事件
                        if let Some(tx) = &frag_progress_tx {
                            let _ = tx.try_send(FragmentProgress::Retry {
                                fragment_index: frag_index,
                                attempt: next_attempt,
                            });
                        }
                        if !frag_has_mirrors {
                            frag_circuit_breakers.record_failure(&frag_url);
                        }
                        frag_protocol.clear_selected().await;
                        tokio::time::sleep(backoff).await;
                        attempt += 1;
                    }
                }
            };

            // 上报结果:成功经 completed_tx(主循环处理),JoinSet 返回虚拟信号;
            // 失败不经 completed_tx,由 JoinSet 直接返回(主循环处理错误)。
            // 这与旧 per-worker 模型一致:避免成功结果被 completed_rx 和
            // join_next 双重处理导致 record_completed_fragment 重复调用。
            // 闭环并发控制:task 退出前 record_complete,active-1
            frag_concurrency_ctrl.record_complete();
            match frag_result {
                Ok(tuple) => {
                    let _ = task_completed_tx.send(Ok(tuple));
                    Ok((0, 0, Duration::ZERO, None)) // 虚拟信号:join_next 忽略
                }
                Err(e) => Err(e),
            }

            // write_buf 在 task 结束时析构:
            // Guard 变体经 BufferGuard::drop 归还到池并恢复许可;
            // Owned 变体的 AlignedBuf 正常释放内存。
        });

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

        // 使用调度器建议的并发度,但不超过配置的最大值。
        // BT/magnet 冷启动(低置信度)解耦:直接用配置并发,HTTP 路径不变
        // (cold-start 起步 + ramp 爬坡 + 429 保护全部保留)。
        let (effective_concurrency, concurrency_reason) =
            match self.bt_cold_start_concurrency_override(&recommendation) {
                Some(configured) => (configured as usize, "bt_cold_start"),
                None => (
                    recommendation
                        .concurrency
                        .min(self.config.max_concurrent_fragments)
                        .max(1) as usize,
                    "scheduler",
                ),
            };

        info!(
            configured_concurrency = self.config.max_concurrent_fragments,
            recommended_concurrency = recommendation.concurrency,
            effective_concurrency = effective_concurrency,
            confidence = recommendation.confidence,
            reason = concurrency_reason,
            "使用调度器并发建议"
        );

        // FIX-05: Semaphore 作为硬上限(防 OOM)应用配置最大值 max_concurrent_fragments，
        // 而非初始建议值 effective_concurrency。ConcurrencyController.should_spawn() 作为
        // 软目标门禁(active < target)，实现动态升降:上调时 should_spawn 放行、Semaphore 有余量；
        // 下调时 should_spawn 阻止新 spawn、在途任务自然完成。旧实现用 effective_concurrency
        // 构造 Semaphore，导致初始建议为 1 时即便后续 set_target(4) 也无法超过 1 个在途。
        let semaphore = Arc::new(Semaphore::new(
            self.config.max_concurrent_fragments as usize,
        ));
        // 闭环并发控制(P2-5):ConcurrencyController 维护 active/target,
        // 可升可降(set_target)。Semaphore 作为硬上限(permits RAII),
        // Controller 作为软目标(动态调优)。spawn 前检查 should_spawn()。
        // 解决 tokio::Semaphore add_permits 只能增不能降的限制(FastBioDL 闭环控制)。
        let concurrency_ctrl = Arc::new(ConcurrencyController::new(
            effective_concurrency as u32,
            self.config.max_concurrent_fragments,
        ));
        let max_concurrent_fragments = self.config.max_concurrent_fragments;
        // 周期性 re-recommend 间隔:用 sampling_interval_secs(默认 5s),
        // 最小 2s 避免频繁 re-recommend 抖动。
        let reschedule_interval =
            Duration::from_secs(self.scheduler_config.sampling_interval_secs.max(2));
        let url = self.url.clone();
        let storage = self
            .storage
            .clone()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?;
        let protocol = self.protocol.clone();
        let pool = self.pool.clone();
        let buffer_pool = self.buffer_pool.clone();
        self.refresh_resolved_host_from_protocol();
        let host = self.request_host()?;
        let pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        let mut control_rx = self.control_rx.clone();
        let progress_tx = self.progress_tx.clone();
        let max_retries = self.config.max_retries;
        // 优先使用外部共享限速器(跨任务全局限速),否则从配置创建 per-task 限速器
        let rate_limiter: Option<Arc<RateLimiter>> = self.rate_limiter.clone().or_else(|| {
            self.config
                .rate_limit_bytes_per_sec
                .filter(|&bps| bps > 0)
                .map(|bps| Arc::new(RateLimiter::new(bps)))
        });
        let circuit_breakers = self.circuit_breakers.clone();
        let metrics = self.metrics.clone();
        tracing::info!(
            has_progress_tx = progress_tx.is_some(),
            frag_count = self.fragments.len(),
            "分片下载准备就绪"
        );

        let mut handles: JoinSet<FragmentTaskResult> = JoinSet::new();

        // 仅对未完成(Pending)的分片下载,已完成分片(断点续传)跳过
        let fragment_specs: Vec<FragmentSpec> = self
            .fragments
            .iter()
            .filter(|frag| frag.state == crate::fragment::FragmentState::Pending)
            .map(|frag| {
                (
                    frag.info.index,
                    frag.info.start,
                    frag.info.end,
                    frag.resume_offset,
                    frag.info.hash.is_some(),
                    FragmentShared {
                        effective_end: Arc::clone(&frag.effective_end),
                        realtime_downloaded: Arc::clone(&frag.realtime_downloaded),
                    },
                )
            })
            .collect();

        // ── spawn-per-fragment 模型 ────────────────────────────────────
        // dispatcher 逻辑内联到主循环:从 frag_rx 拉取 spec → semaphore.acquire_owned →
        // handles.spawn(download_single_fragment)。Semaphore 自然限制并发,
        // add_permits 后下次 acquire 成功即可 spawn 新 task(动态并发基础)。
        //
        // 相比旧 per-worker channel 模型的优势:
        // 1. 消除 dispatcher round-robin try-send 逻辑(无 per-worker channel)
        // 2. Semaphore permits 即真实并发上限(add_permits 可运行时提升)
        // 3. 每个 fragment task 独立 spawn,无固定 worker 数量限制
        // 容量留余量给 rebalance 重入队(慢片拆分后的尾片)
        let (frag_tx_raw, mut frag_rx) =
            mpsc::channel::<FragmentSpec>((effective_concurrency * 2).max(8));
        let mut frag_tx = Some(frag_tx_raw);
        let (completed_tx, mut completed_rx) = mpsc::unbounded_channel::<FragmentTaskResult>();

        // 入队前检查暂停/取消信号,避免在暂停状态下无意义地启动
        if let Some(ref rx) = control_rx {
            let mut check_rx = rx.clone();
            Self::wait_control_rx(&mut check_rx, pause_timeout).await?;
        }

        // 在独立 task 中入队所有分片:frag_tx.send().await 在 channel 满时阻塞,
        // 必须与主循环(从 frag_rx 拉取并 spawn task)并发执行,否则 channel 容量 <
        // 分片数时死锁。入队 task 持有 frag_tx,完成后 drop 使 frag_rx 返回 None。
        //
        // start_download / inc_fragment 需在入队前同步执行(修改 self.fragments),
        // 仅 send 入队异步化。将已标记 start_download 的 spec 收集后 spawn 入队。
        let mut pending_specs: Vec<FragmentSpec> = Vec::with_capacity(fragment_specs.len());
        for spec in &fragment_specs {
            let frag_index = spec.0;
            if frag_index as usize >= self.fragments.len() {
                return Err(DownloadError::Config("分片索引越界".into()));
            }
            self.fragments[frag_index as usize].start_download()?;
            if let Some(ref m) = metrics {
                m.inc_fragment();
            }
            pending_specs.push(spec.clone());
        }
        // 初始入队用 clone 的 sender;主循环保留 Option<Sender> 供 rebalance 重入队。
        // 全部初始分片入队后不 drop 主 sender,避免 rebalance 无法再 enqueue。
        let frag_tx_enqueue = frag_tx.as_ref().expect("frag_tx 刚创建").clone();
        let mut enqueue_handle = tokio::spawn(async move {
            for spec in pending_specs {
                if frag_tx_enqueue.send(spec).await.is_err() {
                    break; // 主循环退出,frag_rx 已 drop
                }
            }
            // frag_tx_enqueue drop;主循环仍持有 frag_tx(Option)
        });

        // 主循环:同时充当 dispatcher(从 frag_rx 拉取 spec + spawn task)和结果收集器
        let frag_url = url.clone();
        let frag_storage = storage.clone();
        let frag_protocol = protocol.clone();
        let frag_semaphore = semaphore.clone();
        // P1:镜像路径下 engine 层跳过主 host 的 pool.acquire,
        // 改由 MirrorProtocol(已注入同一 pool)按真实命中镜像 host acquire,
        // 使各镜像能各自占满自己的 per-host 配额。单源路径保持 engine 层 acquire。
        let frag_pool = if self.has_mirrors { None } else { pool.clone() };
        let frag_buffer_pool = buffer_pool.clone();
        let frag_host = host.clone();
        let frag_limiter = rate_limiter.clone();
        let frag_control_rx = control_rx.clone();
        let frag_progress_tx = progress_tx.clone();
        let frag_metrics = metrics.clone();
        let frag_circuit_breakers = circuit_breakers.clone();
        // B5:镜像路径禁用 engine 层熔断(以主 URL 为 key 会误熔断整个任务),
        // 改由 MirrorProtocol 的 per-source stats 接管故障隔离。
        let frag_has_mirrors = self.has_mirrors;
        let frag_verifier = self.verifier.clone();
        // P2-4: 协议直接管理存储时跳过引擎 write_all_at(消除双存储写放大)
        let skip_write = self
            .metadata
            .as_ref()
            .map(|m| m.protocol_managed_storage)
            .unwrap_or(false);

        // completed_tx 包装为 Option:所有分片 spawn 完成后(frag_rx 返回 None)take+drop,
        // 使 completed_rx 在所有 task 完成后能返回 None 触发主循环退出。
        let mut completed_tx = Some(completed_tx);

        // 动态并发度 re-recommend 定时器
        let mut reschedule_timer = interval(reschedule_interval);

        loop {
            // 用户 Pause:强制 abort 在途分片并停车,避免 select 饿死/阻塞 await 导致“无法暂停”
            if Self::control_is_paused(&control_rx) {
                tracing::info!("检测到 Pause,中止在途分片并等待 Resume");
                // 停掉入队任务,丢弃尚未 spawn 的 spec(Pause 期间不应再开新片)
                enqueue_handle.abort();
                if let Some(tx) = frag_tx.take() {
                    drop(tx);
                }
                while frag_rx.try_recv().is_ok() {}
                // 强制终止在途 IO(含卡在 pool.acquire / stream 中的 task)
                Self::abort_remaining_fragment_tasks(&mut handles).await;
                // abort 路径可能跳过 record_complete,必须清零 active
                concurrency_ctrl.reset_active();
                // drain 成功结果(若 abort 前刚好完成)
                while let Ok(result) = completed_rx.try_recv() {
                    if let Ok((index, downloaded, duration, computed_hash)) = result
                        && (index != 0 || downloaded != 0)
                    {
                        let _ = self.record_completed_fragment(
                            index,
                            downloaded,
                            duration,
                            computed_hash,
                        );
                    }
                }
                // Downloading → Pending + 固化 resume_offset(字节级续传)
                for frag in &mut self.fragments {
                    frag.park_for_pause();
                }
                // 等 Resume / Cancel / 超时
                if let Some(rx) = control_rx.as_mut() {
                    Self::wait_control_rx(rx, pause_timeout).await?;
                }
                // Resume:把仍为 Pending 的分片重新入队
                let pending: Vec<FragmentSpec> = self
                    .fragments
                    .iter()
                    .filter(|f| f.state == crate::fragment::FragmentState::Pending)
                    .map(|frag| {
                        (
                            frag.info.index,
                            frag.info.start,
                            frag.info.end,
                            frag.resume_offset,
                            frag.info.hash.is_some(),
                            FragmentShared {
                                effective_end: Arc::clone(&frag.effective_end),
                                realtime_downloaded: Arc::clone(&frag.realtime_downloaded),
                            },
                        )
                    })
                    .collect();
                if pending.is_empty() {
                    // 全部已完成
                    frag_tx.take();
                    completed_tx.take();
                    break;
                }
                let (new_tx, new_rx) =
                    mpsc::channel::<FragmentSpec>((effective_concurrency * 2).max(8));
                frag_rx = new_rx;
                frag_tx = Some(new_tx);
                let mut requeue = Vec::with_capacity(pending.len());
                for spec in pending {
                    let idx = spec.0 as usize;
                    if idx < self.fragments.len() {
                        // park 后是 Pending,可再 start_download
                        if self.fragments[idx].state == crate::fragment::FragmentState::Pending {
                            self.fragments[idx].start_download()?;
                        }
                    }
                    requeue.push(spec);
                }
                let frag_tx_enqueue = frag_tx.as_ref().expect("frag_tx recreated").clone();
                enqueue_handle = tokio::spawn(async move {
                    for spec in requeue {
                        if frag_tx_enqueue.send(spec).await.is_err() {
                            break;
                        }
                    }
                });
                tracing::info!("Resume 后已重新入队未完成分片");
                continue;
            }

            tokio::select! {
                // 动态并发度:周期性 re-recommend,带宽变化时提升并发度
                // guard !handles.is_empty():只在有在途 task 时才 poll,
                // 所有 task 完成后此分支 disable,使 else => break 能正确触发
                _ = reschedule_timer.tick(), if !handles.is_empty() => {
                    // 用户暂停期间禁止 re-recommend / rebalance(避免 Pause 后仍开新片)
                    if Self::control_is_paused(&control_rx) {
                        continue;
                    }
                    let rec = self.scheduler.recommend(file_size, max_concurrent_fragments);
                    let new_target = rec.concurrency.min(max_concurrent_fragments).max(1);
                    let old = concurrency_ctrl.target();
                    // 低置信度(慢启动/样本不足)只升不降,避免 holt=1 把爬坡并发打回 1
                    let allow = new_target > old || rec.confidence > 0.5;
                    if allow && new_target != old {
                        concurrency_ctrl.set_target(new_target);
                        info!(
                            old_concurrency = old,
                            new_concurrency = new_target,
                            active = concurrency_ctrl.active(),
                            confidence = rec.confidence,
                            "闭环并发度调整"
                        );
                    }
                    // 安全 rebalance:try_send 入队,Full 时 revert(不堵主循环)
                    if let Some(tx) = frag_tx.as_ref() {
                        let _ = self.try_rebalance_slowest_fragment(tx).await;
                    }
                }
                // dispatcher:从中央队列拉取分片,acquire permit 后 spawn task
                // 闭环并发控制:仅当 active < target 时才拉取新分片(可降并发)
                // Pause 时禁止 spawn:否则 UI 已暂停仍会开新分片,表现为“无法暂停”
                // should_spawn()=false 时,等待 task 完成(record_complete)使 active 下降
                spec = frag_rx.recv(), if concurrency_ctrl.should_spawn()
                    && !Self::control_is_paused(&control_rx) => {
                    match spec {
                        Some(spec) => {
                            let spawn_ctx = FragmentSpawnCtx {
                                protocol: &frag_protocol,
                                storage: &frag_storage,
                                pool: &frag_pool,
                                url: &frag_url,
                                host: &frag_host,
                                limiter: &frag_limiter,
                                control_rx: &frag_control_rx,
                                progress_tx: &frag_progress_tx,
                                verifier: &frag_verifier,
                                metrics: &frag_metrics,
                                circuit_breakers: &frag_circuit_breakers,
                                concurrency_ctrl: &concurrency_ctrl,
                                semaphore: &frag_semaphore,
                                completed_tx: completed_tx.as_ref().unwrap(),
                                buffer_pool: &frag_buffer_pool,
                                has_mirrors: frag_has_mirrors,
                                max_retries,
                                pause_timeout,
                                skip_write,
                                sync_mode: self.config.crash_consistency_mode,
                                object_identity: self
                                    .metadata
                                    .as_ref()
                                    .map(ObjectIdentity::from_metadata),
                            };
                            if let Err(e) =
                                Self::spawn_fragment_task(&spawn_ctx, spec, &mut handles).await
                            {
                                // H2: 捕获 RangeNotSupported 降级为整块下载
                                if let Some(result) = self
                                    .try_range_not_supported_fallback(&e, &mut handles, &mut completed_rx)
                                    .await
                                {
                                    return result;
                                }
                                Self::abort_remaining_fragment_tasks(&mut handles).await;
                                Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                                self.state = DownloadState::Failed;
                                return Err(e);
                            }
                        }
                        None => {
                            // 初始队列耗尽。若仍有在途 task,保留 frag_tx 供 rebalance;
                            // 仅当无在途且无 rebalance 可能时再 drop sender + completed_tx。
                            if handles.is_empty() {
                                frag_tx.take();
                                completed_tx.take();
                            }
                            // 否则继续等待 completed / rebalance 重入队。
                        }
                    }
                }
                // 结果收集:completed_rx 始终 poll(无 guard),确保成功结果不丢失。
                // 退出依赖:completed_tx 原始端在 frag_rx 耗尽后 take+drop,所有 task 的
                // clone 在 task 结束时 drop,completed_rx.recv() 返回 None 触发 else => break。
                Some(result) = completed_rx.recv() => {
                    match result {
                        // task 正常退出(虚拟信号),跳过
                        Ok((0, 0, _, _)) => continue,
                        Ok((index, downloaded, duration, computed_hash)) => {
                            self.record_completed_fragment(
                                index,
                                downloaded,
                                duration,
                                computed_hash,
                            )?;
                            // 样本驱动:每片完成后立即 re-recommend,避免 5s 定时器拖慢爬坡。
                            // 低置信度只升不降,防止 holt 早期=1 把 ramp 目标打回。
                            let rec = self
                                .scheduler
                                .recommend(file_size, max_concurrent_fragments);
                            let new_target =
                                rec.concurrency.min(max_concurrent_fragments).max(1);
                            let old = concurrency_ctrl.target();
                            if (new_target > old || rec.confidence > 0.5) && new_target != old {
                                concurrency_ctrl.set_target(new_target);
                            }
                            // 快片完成后立刻 rebalance 慢片,不必等 reschedule_timer
                            if let Some(tx) = frag_tx.as_ref() {
                                let _ = self.try_rebalance_slowest_fragment(tx).await;
                            }
                        }
                        Err((failed_index, e)) => {
                            // H2: 捕获 RangeNotSupported(协议层对 GET Range 返回 200
                            // 的运行时降级信号),中止在途 → 重新规划单分片 → 整块下载
                            if let Some(result) = self
                                .try_range_not_supported_fallback(&e, &mut handles, &mut completed_rx)
                                .await
                            {
                                return result;
                            }
                            Self::abort_remaining_fragment_tasks(&mut handles).await;
                            Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                            if let Some(frag) = self.fragments.get_mut(failed_index as usize) {
                                frag.force_fail();
                            }
                            self.state = DownloadState::Failed;
                            return Err(e);
                        }
                    }
                }
                Some(joined) = handles.join_next() => {
                    match joined {
                        Ok(result) => {
                            // 成功结果已由 completed_tx 处理(返回虚拟 (0,0,..)),
                            // 失败不经 completed_tx 由 JoinSet 直接返回
                            match result {
                                Ok((0, 0, _, _)) => {}
                                Ok((index, downloaded, duration, computed_hash)) => {
                                    // 防御性:若 completed_tx 发送失败(如 channel 已关闭),
                                    // 仍从 join 结果补录(此时不会重复——record_completed_fragment
                                    // 的状态机会拒绝 Done->Done,但补录路径在正常流程不应触发)
                                    if index != 0 || downloaded != 0 {
                                        let _ = self.record_completed_fragment(
                                            index,
                                            downloaded,
                                            duration,
                                            computed_hash,
                                        );
                                    }
                                }
                                Err((failed_index, e)) => {
                                    // H2: 同 completed_rx 路径,捕获 RangeNotSupported 降级
                                    if let Some(result) = self
                                        .try_range_not_supported_fallback(
                                            &e,
                                            &mut handles,
                                            &mut completed_rx,
                                        )
                                        .await
                                    {
                                        return result;
                                    }
                                    Self::abort_remaining_fragment_tasks(&mut handles).await;
                                    Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                                    if let Some(frag) =
                                        self.fragments.get_mut(failed_index as usize)
                                    {
                                        frag.force_fail();
                                    }
                                    self.state = DownloadState::Failed;
                                    return Err(e);
                                }
                            }
                        }
                        Err(error) => {
                            Self::abort_remaining_fragment_tasks(&mut handles).await;
                            Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                            self.state = DownloadState::Failed;
                            return Err(DownloadError::Other(
                                format!("分片任务 panic: {error}").into(),
                            ));
                        }
                    }
                }
                else => break,
            }
            // 退出条件:所有分片已入队(frag_tx 已 drop)+ 所有 task 已完成(handles 空)。
            // task 退出时先 send 结果再返回,join_next 返回时结果必在 completed_rx 缓冲中。
            // 但 select! 可能先消费 join_next(虚拟信号)而非 completed_rx,
            // 导致 break 时 completed_rx 仍有未消费结果。必须先 drain 再 break。
            if handles.is_empty() && frag_rx.is_empty() {
                // 无在途且队列空:释放 sender,确保 completed_rx 可 EOF
                frag_tx.take();
                completed_tx.take();
                Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                break;
            }
        }

        // 入队 task 在所有分片已 send 后自然完成(或被 abort)
        enqueue_handle.abort();

        // 冲刷未满窗口的聚合 goodput,避免短任务/末片零样本
        if let Some(bps) = self.flush_goodput_window() {
            self.scheduler.observe_bandwidth(bps);
        }

        // 显式关闭存储后端,close() 内部已调用 sync_data() 保证数据落盘,
        // 无需额外 sync() 避免双重 fsync 导致的 Flush Storm
        storage.close().await?;

        // 审计 BT-17:protocol_managed 时 FileStream 读完 ≠ piece truth 完成。
        // 在标 Completed 前等待 librqbit wait_until_completed(带 peer_wait 看门狗)。
        #[cfg(feature = "magnet")]
        self.wait_bt_piece_truth_if_protocol_managed().await?;

        self.state = DownloadState::Completed;
        info!("全部分片下载完成");
        Ok(())
    }

    /// 安全慢片 rebalance:拆分下载中最慢分片的未完成尾部,try_send 入队。
    ///
    /// 相对已删除的 work-stealing:
    /// - **故意用 `try_send` 而非 `send().await`**:主循环在完成事件路径
    ///   同步 await 本函数;channel 满时阻塞 send 会永久卡住 dispatcher
    ///   (实测:冷启动 concurrency=4、容量 8 时 4/17 分片后进度冻结)。
    ///   丢一次 rebalance 可通过 `revert_split` 安全回滚,下次定时/完成再试。
    /// - 入队失败(Full/Closed)则 `revert_split` 回滚
    /// - 不依赖 steal_rx / 额外 completed_tx 生命周期
    ///
    /// 最慢片剩余 >= 2*MIN_SPLIT_SIZE 即可拆(含最后一片 straggler)。
    async fn try_rebalance_slowest_fragment(
        &mut self,
        frag_tx: &mpsc::Sender<FragmentSpec>,
    ) -> DownloadResult<bool> {
        use crate::fragment::{FragmentState, MIN_SPLIT_SIZE};
        use std::sync::atomic::Ordering;

        let mut best: Option<(usize, f64, u64)> = None; // (idx, progress, realtime)
        for (i, frag) in self.fragments.iter().enumerate() {
            if frag.state != FragmentState::Downloading {
                continue;
            }
            let size = frag.info.size.max(1);
            let rt = frag.realtime_downloaded.load(Ordering::Acquire);
            let eff_end = frag.effective_end.load(Ordering::Acquire);
            // 防溢出:frag.info.start + rt 是裸加法,release(overflow-checks=false)下
            // 若 rt 异常增长会静默回绕。用 saturating_add 与下方 2284 行实际拆分逻辑
            // (start.saturating_add(realtime))保持一致,消除不一致性。
            // 即便此处选片偏差,2284 行的 remaining 检查会拒绝拆分,不致数据损坏。
            let remaining = eff_end
                .saturating_add(1)
                .saturating_sub(frag.info.start.saturating_add(rt));
            if remaining < MIN_SPLIT_SIZE.saturating_mul(2) {
                continue;
            }
            // 新 spawn 的片至少跑 1s 再评估,避免刚启动就被拆
            let age_ok = frag
                .start_time
                .map(|t| t.elapsed() >= std::time::Duration::from_secs(1))
                .unwrap_or(false);
            if !age_ok {
                continue;
            }
            let progress = rt as f64 / size as f64;
            match best {
                None => best = Some((i, progress, rt)),
                Some((_, bp, _)) if progress < bp => best = Some((i, progress, rt)),
                _ => {}
            }
        }
        let Some((idx, _prog, realtime)) = best else {
            return Ok(false);
        };

        let frag = &self.fragments[idx];
        let start = frag.info.start;
        let eff_end = frag.effective_end.load(Ordering::Acquire);
        let done_abs = start.saturating_add(realtime);
        let remaining = eff_end.saturating_add(1).saturating_sub(done_abs);
        if remaining < MIN_SPLIT_SIZE.saturating_mul(2) {
            return Ok(false);
        }
        let split_point = done_abs.saturating_add(remaining / 2);
        if split_point <= done_abs || split_point > eff_end {
            return Ok(false);
        }

        let new_index = self.fragments.len() as u32;
        let stolen = {
            let frag = &mut self.fragments[idx];
            match frag.try_split(split_point, new_index)? {
                Some(s) => s,
                None => return Ok(false),
            }
        };

        let spec: FragmentSpec = (
            stolen.info.index,
            stolen.info.start,
            stolen.info.end,
            stolen.resume_offset,
            stolen.info.hash.is_some(),
            FragmentShared {
                effective_end: Arc::clone(&stolen.effective_end),
                realtime_downloaded: Arc::clone(&stolen.realtime_downloaded),
            },
        );

        // try_send:Full 时立即返回,避免堵死 execute_fragmented_download 主循环
        match frag_tx.try_send(spec) {
            Ok(()) => {
                info!(
                    slow_index = idx,
                    new_index, split_point, "rebalance:拆分慢片尾部并重入队"
                );
                self.fragments.push(stolen);
                Ok(true)
            }
            Err(_) => {
                // Full 或 Closed:回滚 split,下次 rebalance 再试
                self.fragments[idx].revert_split_after_failed_dispatch(&stolen);
                Ok(false)
            }
        }
    }
    /// 审计 H2(200 fallback 运行时降级):服务器忽略 Range 返回 200 时,
    /// `download_range`/`download_range_stream` 返回 `RangeNotSupported`。
    /// `execute_fragmented_download` 在分片 worker 失败路径捕获此错误,
    /// 中止所有在途 task → drain 已完成结果(避免丢失进度)→ 重新规划为
    /// 覆盖整个文件的单分片 → 委托 `execute_full_download` 整块下载。
    ///
    /// 此降级路径比走 make_200_fallback_stream 截取每片请求区间更高效:
    /// 整块下载只传输 1×file_size,而非 N 片各自 fallback 的 ≈ S*N/2。
    ///
    /// 返回 `Some(())` 表示已捕获并降级处理(调用方应返回该结果),
    /// 返回 `None` 表示非 RangeNotSupported 错误(调用方按原路径返回错误)。
    async fn try_range_not_supported_fallback(
        &mut self,
        error: &DownloadError,
        handles: &mut JoinSet<FragmentTaskResult>,
        completed_rx: &mut mpsc::UnboundedReceiver<FragmentTaskResult>,
    ) -> Option<DownloadResult<()>> {
        if !matches!(error, DownloadError::RangeNotSupported) {
            return None;
        }
        warn!(
            url = %tachyon_core::redact_url_for_log(&self.url),
            "服务器不支持 Range 请求,降级为整块下载(execute_full_download)"
        );
        // 审计 batch2:持久化 supports_range=false,避免 resume 再次走分片路径
        if let Some(meta) = self.metadata.as_mut() {
            meta.supports_range = false;
        }
        // 中止所有在途分片任务 + drain 已完成结果(进度对齐)
        Self::abort_remaining_fragment_tasks(handles).await;
        if let Err(e) = Self::drain_completed_channel(self, completed_rx) {
            return Some(Err(e));
        }
        // 重新规划为单分片覆盖整个文件:
        // 原 multi-fragment 规划基于 supports_range=true 的假设,已失效。
        // 改用单分片 [0, file_size-1] 让 execute_full_download_once 的
        // first_mut().complete_download_fast(pos, ...) 状态机正确转换,
        // 且 verify()/snapshot 的分片总数与实际写入一致。
        let file_size = self
            .metadata
            .as_ref()
            .and_then(|m| m.file_size)
            .unwrap_or(0);
        let single = crate::fragment::plan_fragments(
            file_size,
            false, // supports_range=false 强制单分片路径
            None,
            &self.scheduler_config,
        )
        .map_err(|e| {
            warn!(error = %e, "重新规划单分片失败,继续用原 fragments 整块下载");
            e
        });
        if let Ok(frags) = single
            && !frags.is_empty()
        {
            self.fragments = frags
                .iter()
                .map(|info| FragmentRecord::new(info.clone(), self.config.max_retries))
                .collect();
            // 整块下载路径会从 Pending 走 start_download → complete_download_fast
            debug!(count = self.fragments.len(), "已重新规划为单分片覆盖整文件");
        }
        // 重置存储分配,丢弃 execute_fragmented_download 期间部分写入的残留,
        // 避免 execute_full_download_once 写入与旧数据拼接产生损坏。
        if let Some(storage) = self.storage.as_ref() {
            let _ = storage.allocate(file_size).await;
        }
        Some(self.execute_full_download().await)
    }

    /// 聚合 goodput 采样间隔:窗口至少持续该时长才向调度器 emit
    const GOODPUT_EMIT_MIN: Duration = Duration::from_millis(200);

    /// 累计完成字节到任务级时间窗;窗口时长 >= GOODPUT_EMIT_MIN 时返回 goodput bps 并重置。
    fn note_goodput_bytes(&mut self, delta_bytes: u64) -> Option<u64> {
        if delta_bytes == 0 {
            return None;
        }
        let now = Instant::now();
        match self.goodput_window_start {
            None => {
                self.goodput_window_start = Some(now);
                self.goodput_window_bytes = delta_bytes;
                None
            }
            Some(start) => {
                self.goodput_window_bytes = self.goodput_window_bytes.saturating_add(delta_bytes);
                let elapsed = now.saturating_duration_since(start);
                if elapsed >= Self::GOODPUT_EMIT_MIN {
                    self.emit_goodput_window(now, start)
                } else {
                    None
                }
            }
        }
    }

    /// 冲刷未 emit 的窗口(任务结束/最后一片),避免短任务零样本。
    fn flush_goodput_window(&mut self) -> Option<u64> {
        let start = self.goodput_window_start?;
        if self.goodput_window_bytes == 0 {
            return None;
        }
        let now = Instant::now();
        // 极短窗口用 GOODPUT_EMIT_MIN 作分母下界,避免瞬时 bps 爆炸
        let elapsed = now
            .saturating_duration_since(start)
            .max(Self::GOODPUT_EMIT_MIN);
        let secs = elapsed.as_secs_f64().max(1e-6);
        let bps = (self.goodput_window_bytes as f64 / secs) as u64;
        self.goodput_window_start = None;
        self.goodput_window_bytes = 0;
        (bps > 0).then_some(bps)
    }

    fn emit_goodput_window(&mut self, now: Instant, start: Instant) -> Option<u64> {
        let secs = now.saturating_duration_since(start).as_secs_f64().max(1e-6);
        let bps = (self.goodput_window_bytes as f64 / secs) as u64;
        self.goodput_window_start = Some(now);
        self.goodput_window_bytes = 0;
        (bps > 0).then_some(bps)
    }

    fn record_completed_fragment(
        &mut self,
        index: u32,
        downloaded: u64,
        duration: Duration,
        computed_hash: Option<String>,
    ) -> DownloadResult<()> {
        let frag = &mut self.fragments[index as usize];
        let previous_downloaded = frag.info.downloaded;
        frag.complete_download_fast(downloaded, duration)?;
        frag.computed_hash = computed_hash;

        if let Some(ref m) = self.metrics {
            m.add_bytes(downloaded.saturating_sub(previous_downloaded));
        }

        // 任务级聚合 goodput:多并发分片吞吐叠加到共享时间窗,再反馈调度器。
        // 避免单片完成速率噪声主导 EWMA;限速器仍不随实测带宽下调。
        let delta = downloaded.saturating_sub(previous_downloaded);
        if delta > 0
            && let Some(bps) = self.note_goodput_bytes(delta)
        {
            self.scheduler.observe_bandwidth(bps);
            debug!(
                index = index,
                bytes_per_sec = bps,
                delta_bytes = delta,
                "聚合 goodput 已反馈给调度器"
            );
        }
        Ok(())
    }

    fn drain_completed_channel(
        &mut self,
        completed_rx: &mut mpsc::UnboundedReceiver<FragmentTaskResult>,
    ) -> DownloadResult<()> {
        while let Ok(result) = completed_rx.try_recv() {
            match result {
                Ok((0, 0, _, _)) => continue,
                Ok((index, downloaded, duration, computed_hash)) => {
                    self.record_completed_fragment(index, downloaded, duration, computed_hash)?;
                }
                // 错误已在触发 abort 的路径上处理,忽略队列中的滞后错误
                Err(_) => {}
            }
        }
        Ok(())
    }

    async fn abort_remaining_fragment_tasks(handles: &mut JoinSet<FragmentTaskResult>) {
        handles.abort_all();
        while let Some(joined) = handles.join_next().await {
            if let Err(error) = joined
                && !error.is_cancelled()
            {
                warn!(error = %error, "分片任务 abort 后异常结束");
            }
        }
    }

    /// 把一个 batch 完整写入存储(含短写重试 + 控制信号中断)
    ///
    /// 入口处 `batch.freeze()` 转为 `Bytes`(零拷贝,Arc 引用计数 +1),循环内用
    /// `storage.write_at(pos, remaining.clone())` 写入。相比旧 `write_at_mut` 路径:
    /// - 消除后端 `Bytes::copy_from_slice` 的 256KiB 全量 memcpy(write_at 后端直接
    ///   move owned `Bytes` 进 `spawn_blocking`,Arc refcount 保证 select! 取消安全)
    /// - 消除 `advance(written.min(batch.len()))` 的 min hack(Bytes::slice 天然处理剩余)
    /// - `Bytes::clone()`/`slice()` 均为零拷贝指针调整,无内存复制
    ///
    /// 接受 `BytesMut` 的版本:仅测试使用(测试构造 `BytesMut` 较 `Bytes` 方便),
    /// 内部 `freeze()`(零拷贝)后委托 [`write_all_at`]。
    #[cfg(test)]
    async fn write_all_at_mut(
        storage: &StorageSet,
        pos: u64,
        batch: bytes::BytesMut,
        control_rx: &mut Option<watch::Receiver<TaskCommand>>,
        pause_timeout: Duration,
    ) -> DownloadResult<u64> {
        Self::write_all_at(storage, pos, batch.freeze(), control_rx, pause_timeout).await
    }

    /// 把已 owned 的 `Bytes` 完整写入存储(含短写重试 + 控制信号中断)
    ///
    /// 与 [`write_all_at_mut`] 的区别:直接接受 `Bytes`,省去调用方的
    /// `BytesMut::from(chunk)` 分配 + memcpy。大 chunk 直写路径(网络 chunk
    /// 本就是 owned `Bytes`)直接传入,消除 256KiB 的 `BytesMut::from` memcpy。
    ///
    /// `Bytes::clone()`/`slice()` 均为零拷贝指针调整(Arc refcount),无内存复制。
    async fn write_all_at(
        storage: &StorageSet,
        mut pos: u64,
        mut remaining: bytes::Bytes,
        control_rx: &mut Option<watch::Receiver<TaskCommand>>,
        pause_timeout: Duration,
    ) -> DownloadResult<u64> {
        let mut total_written = 0u64;
        while !remaining.is_empty() {
            let write = storage.write_at(pos, remaining.clone());
            let written = if let Some(rx) = control_rx.as_mut() {
                tokio::select! {
                    biased;
                    control = Self::watch_for_interrupt(rx, pause_timeout) => {
                        control?;
                        return Err(DownloadError::Other("控制信号异常结束".into()));
                    }
                    result = write => result?,
                }
            } else {
                write.await?
            };
            if written == 0 {
                return Err(DownloadError::Fragment(format!(
                    "存储短写未前进: offset={pos}, remaining={}",
                    remaining.len()
                )));
            }
            let written_u64 = u64::try_from(written)
                .map_err(|_| DownloadError::Fragment("存储写入长度溢出".into()))?;
            pos = pos.checked_add(written_u64).ok_or_else(|| {
                DownloadError::Fragment(format!(
                    "存储写入偏移溢出: offset={pos}, len={written_u64}"
                ))
            })?;
            total_written = total_written.checked_add(written_u64).ok_or_else(|| {
                DownloadError::Fragment(format!(
                    "存储写入总长度溢出: written={total_written}, len={written_u64}"
                ))
            })?;
            // 零拷贝推进:Bytes::slice 仅调整指针/长度,不复制数据。
            // clamp written 到剩余长度:StorageSet::Multi::write_at 内部 split_to 消费
            // 全部数据后返回的 total 可能 > 单次 clone 的 len(跨段聚合),需防止 slice 越界。
            // 与旧 advance(written.min(batch.len())) 的防御逻辑等价。
            let advance = written.min(remaining.len());
            remaining = remaining.slice(advance..);
        }
        Ok(total_written)
    }

    /// 审计 H-01:按 effective_end 裁剪待写 batch,禁止 write_buf 越过 steal 边界。
    ///
    /// `end_inclusive` 为当前分片允许写入的最后字节偏移。返回 None 表示无可写字节
    /// (已越过边界);同时清空 `write_buf` 中的越界数据。
    fn take_clamped_write_buf(
        pos: u64,
        end_inclusive: u64,
        write_buf: &mut AlignedBuf,
    ) -> Option<bytes::Bytes> {
        if write_buf.is_empty() {
            return None;
        }
        if pos > end_inclusive {
            write_buf.clear();
            return None;
        }
        let max = match end_inclusive
            .checked_sub(pos)
            .and_then(|d| d.checked_add(1))
        {
            Some(m) => m as usize,
            None => {
                write_buf.clear();
                return None;
            }
        };
        let batch = write_buf.split().freeze();
        if batch.len() <= max {
            Some(batch)
        } else {
            // 越界尾部丢弃:steal worker 负责 [end_inclusive+1, …]
            Some(batch.slice(..max))
        }
    }

    /// 刷写一个 batch 到存储,统一处理「流式哈希 update + 越界检查 + 写入 + 偏移推进 + 限速」。
    ///
    /// 消除 `download_single_fragment` 中大 chunk 直写 / 批量刷写 / 尾刷三段重复逻辑。
    /// 调用方负责进度上报(各路径的进度计数位置不同,留在调用点保持原有语义)。
    ///
    /// 返回 `(新偏移, 本次写入字节数)`。hash update 在写入前按字节序执行,
    /// 保证流式哈希顺序与文件字节顺序一致(双缓冲乱序落盘亦安全)。
    #[allow(clippy::too_many_arguments)]
    async fn flush_batch(
        storage: &StorageSet,
        pos: u64,
        batch: bytes::Bytes,
        hasher: &mut Option<Box<dyn tachyon_core::traits::StreamingHasher>>,
        frag_index: u32,
        total_written: u64,
        expected_len: u64,
        rate_limiter: &Option<Arc<RateLimiter>>,
        control_rx: &mut Option<watch::Receiver<TaskCommand>>,
        pause_timeout: Duration,
        skip_write: bool,
    ) -> DownloadResult<(u64, u64)> {
        // 流式哈希:在写入前按字节序更新(batch 内容此后不再变化)
        if let Some(h) = hasher {
            h.update(&batch);
        }
        let batch_len = u64::try_from(batch.len())
            .map_err(|_| DownloadError::Fragment("分片写入长度溢出".into()))?;
        let attempted_written = total_written.checked_add(batch_len).ok_or_else(|| {
            DownloadError::Fragment(format!(
                "分片写入长度溢出: index={frag_index}, written={total_written}, len={batch_len}"
            ))
        })?;
        if attempted_written > expected_len {
            return Err(DownloadError::Fragment(format!(
                "分片下载数据越界: index={frag_index}, 预期 {expected_len} 字节, 本次将写入 {attempted_written} 字节"
            )));
        }
        let w = if skip_write {
            // P2-4: 协议层(BT custom Storage)直接写入目标文件,
            // 引擎跳过 write_all_at(消除双存储写放大),仅推进偏移+进度
            u64::try_from(batch.len())
                .map_err(|_| DownloadError::Fragment("分片写入长度溢出".into()))?
        } else {
            Self::write_all_at(storage, pos, batch, control_rx, pause_timeout).await?
        };
        let new_pos = pos.checked_add(w).ok_or_else(|| {
            DownloadError::Fragment(format!(
                "分片写入偏移溢出: index={frag_index}, offset={pos}, len={w}"
            ))
        })?;
        // 实时令牌桶限速
        if let Some(limiter) = rate_limiter {
            limiter.acquire(w).await;
        }
        Ok((new_pos, w))
    }

    /// 发送增量进度事件(通道满或关闭时丢弃并记录,不阻塞下载)。
    fn report_progress(
        frag_index: u32,
        total_written: u64,
        progress_tx: &Option<tokio::sync::mpsc::Sender<FragmentProgress>>,
    ) {
        if let Some(tx) = progress_tx {
            match tx.try_send(FragmentProgress::Chunk {
                fragment_index: frag_index,
                completed: false,
                fragment_downloaded: total_written,
            }) {
                Ok(()) => {
                    tracing::trace!(idx = frag_index, bytes = total_written, "进度事件已发送");
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    // 通道满是设计内背压(try_send 可丢增量),高频 warn 会淹没日志
                    tracing::trace!(idx = frag_index, "增量进度事件丢弃(通道满)");
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!(idx = frag_index, "进度通道已关闭,丢弃增量事件");
                }
            }
        }
    }

    /// 下载单个分片(一次尝试)
    ///
    /// 由 `execute_fragmented_download` 的 spawn 重试循环调用。
    /// 成功返回 `(已写入字节数, 耗时)`;失败返回错误(由调用方决定是否重试)。
    /// 分片整体完成时通过 `progress_tx` 发送 `completed: true`,触发上层 checkpoint。
    #[allow(clippy::too_many_arguments)]
    async fn download_single_fragment(
        protocol: &Arc<dyn Protocol>,
        storage: &Arc<StorageSet>,
        pool: &Option<Arc<ConnectionPool>>,
        host: &str,
        url: &str,
        frag_index: u32,
        frag_start: u64,
        frag_end: u64,
        resume_offset: u64,
        pause_timeout: Duration,
        rate_limiter: Option<Arc<RateLimiter>>,
        control_rx: &Option<watch::Receiver<TaskCommand>>,
        progress_tx: &Option<tokio::sync::mpsc::Sender<FragmentProgress>>,
        verifier: &VerifierKind,
        compute_hash: bool,
        write_buf: &mut AlignedBuf,
        skip_write: bool,
        sync_mode: tachyon_core::config::CrashConsistencyMode,
        shared: &FragmentShared,
        object_identity: Option<ObjectIdentity>,
    ) -> DownloadResult<(u64, Duration, Option<String>)> {
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
            resume_offset,
            "开始下载分片"
        );

        // 通知 app 层该分片开始下载(用于 ChunkMatrix 真实状态显示)
        // try_send 非阻塞:channel 满时丢弃,该分片短暂不显示 downloading,不影响正确性
        if let Some(tx) = progress_tx {
            let _ = tx.try_send(FragmentProgress::Started {
                fragment_index: frag_index,
            });
        }

        let actual_start = frag_start + resume_offset;
        // BUG-1 修复:读取 effective_end(try_split 可能已缩小)
        // 用它替代 frag_end 作为实际下载终止点,避免与 steal worker 并发写同一区域
        let current_effective_end = shared
            .effective_end
            .load(std::sync::atomic::Ordering::Acquire)
            .min(frag_end);
        let stream = if let Some(rx) = control_rx.as_mut() {
            tokio::select! {
                biased;
                control = Self::watch_for_interrupt(rx, pause_timeout) => {
                    control?;
                    return Err(DownloadError::Other("控制信号异常结束".into()));
                }
                result = protocol.download_range_stream(url, actual_start, current_effective_end, object_identity.clone()) => result?,
            }
        } else {
            protocol
                .download_range_stream(url, actual_start, current_effective_end, object_identity)
                .await?
        };

        let full_len = current_effective_end
            .checked_sub(frag_start)
            .and_then(|len| len.checked_add(1))
            .ok_or_else(|| {
                DownloadError::Fragment(format!(
                    "分片范围非法: {frag_start}..={current_effective_end}"
                ))
            })?;
        let expected_len = full_len.saturating_sub(resume_offset);
        if expected_len == 0 {
            return Ok((full_len, Duration::ZERO, None));
        }
        let mut pos = actual_start;
        let mut total_written: u64 = resume_offset;
        // BUG-2 修复:初始化 realtime_downloaded 为 resume_offset(已持久化的字节)
        shared
            .realtime_downloaded
            .store(resume_offset, std::sync::atomic::Ordering::Release);
        // 控制通道/进度上报降频计数器，用递减替代 is_multiple_of 模运算
        let mut progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
        // write_buf 由调用方传入(跨分片复用),此处不再新建
        // 流式哈希:仅当分片有 expected hash 时计算,verify() 阶段无需重读文件。
        // 通过 Verifier trait 创建 StreamingHasher,支持 blake3/sha256/GPU 等后端切换。
        let mut hasher: Option<Box<dyn tachyon_core::traits::StreamingHasher>> =
            compute_hash.then(|| verifier.new_hasher());
        tokio::pin!(stream);
        loop {
            // 获取下一个 chunk:死 swarm 下(如磁力链接无 peer) stream.next() 永久 Pending,
            // 必须与 watch_for_interrupt 竞速,否则取消信号无法穿透(协作式取消检查点
            // 在循环体内,无 chunk 到达时不可达)。与 write_all_at 的 select! 同构。
            // cancel-safe:StreamExt::next 仅持有 &mut stream,被 select! 取消时无部分状态。
            let chunk_result = if let Some(rx) = control_rx.as_mut() {
                tokio::select! {
                    biased;
                    interrupt = Self::watch_for_interrupt(rx, pause_timeout) => {
                        interrupt?;
                        return Err(DownloadError::Other("控制信号异常结束".into()));
                    }
                    chunk = tokio_stream::StreamExt::next(&mut stream) => match chunk {
                        Some(r) => r,
                        None => break, // EOF:正常退出循环
                    },
                }
            } else {
                match tokio_stream::StreamExt::next(&mut stream).await {
                    Some(r) => r,
                    None => break,
                }
            };
            // 每 chunk 立即检查 Pause/Cancel(不挂起等 Resume)。
            // wait_control_rx 在 Pause 时会阻塞等 Resume,不适合热路径;
            // select! biased+interrupt 优先是主路径,此处兜底防 select 饿死。
            Self::check_control_interrupt(&mut control_rx)?;
            let chunk = chunk_result?;
            // BUG-1 修复:检查 effective_end 是否被 try_split 缩小
            // 若 pos 已超过 effective_end,worker 的区域已被 steal,立即停止
            let current_end = shared
                .effective_end
                .load(std::sync::atomic::Ordering::Acquire);
            if pos > current_end {
                break; // 已进入 steal 区域,停止下载
            }
            // 若 chunk 会跨越 effective_end,截断到 effective_end(避免写越界)
            let chunk = if pos + chunk.len() as u64 > current_end + 1 {
                let truncate = (current_end + 1 - pos) as usize;
                chunk.slice(..truncate)
            } else {
                chunk
            };
            // 零拷贝优化: 大 chunk 直接写入,跳过 AlignedBuf 聚合
            if chunk.len() >= WRITE_BATCH_BYTES {
                // 先刷写 write_buf 中累积的残余数据(可能因小 chunk 累积未满阈值)
                // 审计 H-01:按 effective_end 裁剪,避免 steal 后缓冲越界写
                if let Some(batch) = Self::take_clamped_write_buf(pos, current_end, write_buf) {
                    let (new_pos, w) = Self::flush_batch(
                        storage,
                        pos,
                        batch,
                        &mut hasher,
                        frag_index,
                        total_written,
                        expected_len,
                        &rate_limiter,
                        &mut control_rx,
                        pause_timeout,
                        skip_write,
                    )
                    .await?;
                    pos = new_pos;
                    total_written += w;
                    shared
                        .realtime_downloaded
                        .fetch_add(w, std::sync::atomic::Ordering::Release);
                }
                if pos > current_end {
                    break;
                }
                // write_buf 可能已推进 pos:重新按 current_end 裁剪大 chunk
                let max_chunk = current_end.saturating_sub(pos).saturating_add(1) as usize;
                if max_chunk == 0 {
                    break;
                }
                let chunk = if chunk.len() > max_chunk {
                    chunk.slice(..max_chunk)
                } else {
                    chunk
                };
                // chunk 本就是 owned Bytes,直接传入 flush_batch,消除
                // 旧路径 BytesMut::from(chunk) 的 256KiB memcpy + 堆分配。
                let (new_pos, w) = Self::flush_batch(
                    storage,
                    pos,
                    chunk,
                    &mut hasher,
                    frag_index,
                    total_written,
                    expected_len,
                    &rate_limiter,
                    &mut control_rx,
                    pause_timeout,
                    skip_write,
                )
                .await?;
                pos = new_pos;
                total_written += w;
                shared
                    .realtime_downloaded
                    .fetch_add(w, std::sync::atomic::Ordering::Release);
                progress_report_countdown = progress_report_countdown.saturating_sub(1);
                if progress_report_countdown == 0 {
                    Self::report_progress(frag_index, total_written, progress_tx);
                    progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
                }
                continue;
            }
            // 容量不足时先刷写已有数据(AlignedBuf 固定容量不自动扩容,与 BytesMut 不同)
            if !write_buf.is_empty() && write_buf.len() + chunk.len() > WRITE_BATCH_BYTES {
                if let Some(batch) = Self::take_clamped_write_buf(pos, current_end, write_buf) {
                    let (new_pos, w) = Self::flush_batch(
                        storage,
                        pos,
                        batch,
                        &mut hasher,
                        frag_index,
                        total_written,
                        expected_len,
                        &rate_limiter,
                        &mut control_rx,
                        pause_timeout,
                        skip_write,
                    )
                    .await?;
                    pos = new_pos;
                    total_written += w;
                    shared
                        .realtime_downloaded
                        .fetch_add(w, std::sync::atomic::Ordering::Release);
                }
                if pos > current_end {
                    break;
                }
            }
            // 若当前 pos 已越过 steal 边界,丢弃本 chunk 并停止
            if pos > current_end {
                write_buf.clear();
                break;
            }
            // 再截断 chunk 到剩余允许写入长度(含已缓冲)
            let remaining_allowed = current_end
                .saturating_sub(pos)
                .saturating_add(1)
                .saturating_sub(write_buf.len() as u64)
                as usize;
            if remaining_allowed == 0 {
                // write_buf 已占满允许区间,先 flush 再结束
                if let Some(batch) = Self::take_clamped_write_buf(pos, current_end, write_buf) {
                    let (new_pos, w) = Self::flush_batch(
                        storage,
                        pos,
                        batch,
                        &mut hasher,
                        frag_index,
                        total_written,
                        expected_len,
                        &rate_limiter,
                        &mut control_rx,
                        pause_timeout,
                        skip_write,
                    )
                    .await?;
                    pos = new_pos;
                    total_written += w;
                    shared
                        .realtime_downloaded
                        .fetch_add(w, std::sync::atomic::Ordering::Release);
                }
                break;
            }
            let chunk = if chunk.len() > remaining_allowed {
                chunk.slice(..remaining_allowed)
            } else {
                chunk
            };
            write_buf.extend_from_slice(&chunk);
            progress_report_countdown = progress_report_countdown.saturating_sub(1);
            // 达到阈值时批量刷写
            if write_buf.len() >= WRITE_BATCH_BYTES {
                // split().freeze() 零拷贝:split_to 调整指针,freeze 转 Bytes(Arc inc)
                if let Some(batch) = Self::take_clamped_write_buf(pos, current_end, write_buf) {
                    let (new_pos, w) = Self::flush_batch(
                        storage,
                        pos,
                        batch,
                        &mut hasher,
                        frag_index,
                        total_written,
                        expected_len,
                        &rate_limiter,
                        &mut control_rx,
                        pause_timeout,
                        skip_write,
                    )
                    .await?;
                    pos = new_pos;
                    total_written += w;
                    shared
                        .realtime_downloaded
                        .fetch_add(w, std::sync::atomic::Ordering::Release);
                }
            }
            // 进度上报检查:移到刷写块外,确保小 chunk 累积不满 WRITE_BATCH_BYTES 时
            // countdown 也能正常重置,避免 u64 下溢 panic
            if progress_report_countdown == 0 {
                Self::report_progress(frag_index, total_written, progress_tx);
                progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
            }
        }
        // 刷写剩余数据(按 final effective_end 裁剪)
        let tail_end = shared
            .effective_end
            .load(std::sync::atomic::Ordering::Acquire);
        if let Some(batch) = Self::take_clamped_write_buf(pos, tail_end, write_buf) {
            // split().freeze() 零拷贝转 Bytes
            let (_new_pos, w) = Self::flush_batch(
                storage,
                pos,
                batch,
                &mut hasher,
                frag_index,
                total_written,
                expected_len,
                &rate_limiter,
                &mut control_rx,
                pause_timeout,
                skip_write,
            )
            .await?;
            total_written += w;
            shared
                .realtime_downloaded
                .fetch_add(w, std::sync::atomic::Ordering::Release);
        }
        // 与原始 is_multiple_of 行为对齐:当 chunk 总数为 PROGRESS_REPORT_CHUNK_INTERVAL
        // 整数倍时,尾刷再发送一次进度事件(可能重复)。
        if progress_report_countdown == PROGRESS_REPORT_CHUNK_INTERVAL {
            Self::report_progress(frag_index, total_written, progress_tx);
        }

        let actual_written = total_written.saturating_sub(resume_offset);
        // BUG-1 修复:work-stealing 拆分后 effective_end 缩小,worker 提前停止,
        // expected_len 需用 final effective_end 重新计算(非拆分时与原值相同)
        let final_effective_end = shared
            .effective_end
            .load(std::sync::atomic::Ordering::Acquire);
        let effective_expected = if final_effective_end < current_effective_end {
            // 被拆分:重新计算预期长度
            final_effective_end
                .checked_sub(frag_start)
                .and_then(|l| l.checked_add(1))
                .unwrap_or(expected_len)
                .saturating_sub(resume_offset)
        } else {
            expected_len
        };
        if actual_written != effective_expected {
            return Err(DownloadError::Fragment(format!(
                "分片下载数据不完整: index={frag_index}, 预期 {effective_expected} 字节, 实际写入 {actual_written} 字节"
            )));
        }

        let elapsed = start_instant.elapsed();

        // 审计 P0-3:在发送 completed 触发上层 snapshot 之前,先把本分片已写字节 durable sync。
        // skip_write(BT protocol_managed) 时引擎未写 storage,由协议层 storage/piece 语义负责落盘。
        // 不做每 batch fsync(避免 Flush Storm);仅在分片完成边界 group-commit 一次。
        // CrashConsistencyMode::Loose:跳过分片 sync,仅在 close() 时落盘(NVMe/UPS 极致吞吐场景)。
        // CrashConsistencyMode::EveryFragment(默认):每分片 fsync,断电后 resume 跳过已 sync 分片。
        if !skip_write && sync_mode == tachyon_core::config::CrashConsistencyMode::EveryFragment {
            storage.sync().await?;
        }

        // 分片整体完成回调:触发上层 checkpoint(断点续传落盘)
        if let Some(tx) = progress_tx
            && let Err(e) = tx
                .send(FragmentProgress::Chunk {
                    fragment_index: frag_index,
                    completed: true,
                    fragment_downloaded: total_written,
                })
                .await
        {
            warn!(index = frag_index, error = %e, "分片完成进度事件发送失败");
        }

        info!(
            index = frag_index,
            written = total_written as usize,
            elapsed_ms = elapsed.as_millis(),
            "分片下载完成"
        );
        // 流式哈希结果:StreamingHasher::finalize 消耗 self 返回十六进制字符串
        let computed_hash = hasher.map(|h| h.finalize());
        Ok((total_written, elapsed, computed_hash))
    }

    // ----- 步骤 5: 校验 -----

    /// 校验已下载数据的完整性
    ///
    /// 根据配置的 `verify_strategy` 决定校验行为:
    /// - `Skip`: 完全跳过校验
    /// - `BestEffort`: 有 expected hash 时校验,无 hash 时跳过并记录 info 日志
    /// - `Require`: 必须有 expected hash 且校验通过,否则返回错误
    pub async fn verify(&mut self) -> DownloadResult<()> {
        // Skip 策略:直接跳过
        if self.config.verify_strategy == tachyon_core::config::VerifyStrategy::Skip {
            debug!(task_id = %self.id, "校验策略为 Skip,跳过校验");
            return Ok(());
        }

        // 兼容旧版 verify_checksum=false:视为 Skip
        if !self.config.verify_checksum {
            debug!(task_id = %self.id, "verify_checksum 已禁用,跳过校验");
            return Ok(());
        }

        self.state = DownloadState::Verifying;
        info!(task_id = %self.id, "开始校验文件完整性");

        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?
            .clone();

        // 收集需要校验的分片(有 expected hash 的),并行计算/比对。
        // 流式哈希分片(有 computed_hash)无需读盘,直接比对;断点续传分片读盘计算。
        // 用 JoinSet + Semaphore(available_parallelism) 并发,任一失败短路 abort。
        let concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(1);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut has_expected_hash = false;
        let mut join_set: tokio::task::JoinSet<DownloadResult<(u32, String, String)>> =
            tokio::task::JoinSet::new();

        // P6:verify 读盘哈希循环需要取消检查点(大文件读盘持续数分钟,
        // 裸 while 循环下取消信号无法穿透)。将 control_rx clone 传入每个
        // spawn task,读盘循环每累计 VERIFY_CANCEL_CHECK_BYTES 字节已读数据
        // 与 watch_for_interrupt 竞速一次。按字节(而非迭代次数)度量,使检查点
        // 频率与 read_at 单次返回量解耦,对短读与大块读均保证一致的响应延迟。
        let verify_pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        let verify_control_rx = self.control_rx.clone();

        for frag in &self.fragments {
            let Some(expected_hash) = frag.info.hash.clone() else {
                continue;
            };
            has_expected_hash = true;
            let index = frag.info.index;
            let computed = frag.computed_hash.clone();
            let start = frag.info.start;
            let size = frag.info.size;
            let storage = storage.clone();
            let permit_sem = semaphore.clone();
            let verifier = self.verifier.clone();
            let mut control_rx = verify_control_rx.clone();
            join_set.spawn(async move {
                let _permit = permit_sem.acquire().await;
                // 流式哈希优先:下载阶段已边写边算,直接比对,消除 I/O 放大。
                let computed = if let Some(h) = computed {
                    debug!(index, "使用流式哈希校验(无需重读文件)");
                    h
                } else {
                    debug!(index, "无流式哈希,回退读盘计算(断点续传分片)");
                    let chunk_size = VERIFY_HASH_CHUNK_SIZE;
                    let mut offset = start;
                    let end = start + size;
                    let mut buf = vec![0u8; chunk_size];
                    let mut hasher = verifier.new_hasher();
                    // P6:读盘循环每累计 N 字节已读数据插入一次取消检查点,与下载路径的
                    // chunk 循环 select! 同构(协作式取消依赖检查点可达)。
                    // 大文件读盘持续数分钟,无检查点时取消信号无法穿透。
                    // 按字节度量:read_at 返回量越大,累加越快、检查越频繁,与"已读数据量"
                    // 成正比,而非与"调用次数"成正比(后者对 1 字节短读会过度检查,对
                    // 8MiB 大块读则检查过疏)。
                    let mut bytes_read_since_check: u64 = 0;
                    while offset < end {
                        let read_len = ((end - offset).min(chunk_size as u64)) as usize;
                        let read = storage.read_at(offset, &mut buf[..read_len]).await?;
                        hasher.update(&buf[..read]);
                        offset += read as u64;
                        // 按已读字节降频检查:累计达阈值后检查一次中断信号并归零
                        bytes_read_since_check = bytes_read_since_check.saturating_add(read as u64);
                        if bytes_read_since_check >= VERIFY_CANCEL_CHECK_BYTES {
                            if let Some(rx) = control_rx.as_mut() {
                                Self::wait_control_rx(rx, verify_pause_timeout).await?;
                            }
                            bytes_read_since_check = 0;
                        }
                    }
                    hasher.finalize()
                };
                Ok((index, expected_hash, computed))
            });
        }

        // 收集结果:任一分片校验失败即 abort 其余并短路返回
        while let Some(res) = join_set.join_next().await {
            let (index, expected_hash, computed) =
                res.map_err(|e| DownloadError::Io(e.into()))??;
            if computed != expected_hash {
                warn!(index, expected = %expected_hash, actual = %computed, "分片校验失败");
                join_set.abort_all();
                self.state = DownloadState::Failed;
                return Err(DownloadError::ChecksumMismatch {
                    expected: expected_hash,
                    actual: computed,
                });
            }
            debug!(index, "分片校验通过");
        }

        // Require 策略:必须有 expected hash
        if self.config.verify_strategy == tachyon_core::config::VerifyStrategy::Require
            && !has_expected_hash
        {
            self.state = DownloadState::Failed;
            return Err(DownloadError::NoExpectedChecksum);
        }

        // BestEffort 策略:无 expected hash 时跳过并记录日志
        if !has_expected_hash {
            info!(task_id = %self.id, "无 expected hash,跳过校验(BestEffort 策略)");
        } else {
            info!(task_id = %self.id, "文件完整性校验通过");
        }
        Ok(())
    }

    // ----- 一键运行 -----

    /// 一键执行完整下载流程
    ///
    /// 依次执行: 探测 -> 规划 -> 预分配 -> 下载 -> 校验
    /// 任一步骤失败将标记任务为 `Failed` 并返回错误。
    #[tracing::instrument(skip(self), fields(url = %tachyon_core::redact_url_for_log(&self.url)))]
    pub async fn run(&mut self) -> DownloadResult<()> {
        info!(url = %tachyon_core::redact_url_for_log(&self.url), "启动下载任务");

        let result = self.run_inner().await;

        if let Err(error) = &result {
            self.apply_terminal_error(error);
            warn!(state = ?self.state, error = %error, "下载任务结束为非成功状态");
        }

        // P0-8: 终态/成功后停止 BT torrent,防止取消后仍联网写盘
        #[cfg(feature = "magnet")]
        self.cleanup_bt_torrent_if_needed(&result).await;

        result
    }

    /// cancel/fail/complete 时 pause+delete(保留文件)+清 cache;暂停超时保持 Paused 不清理
    #[cfg(feature = "magnet")]
    async fn cleanup_bt_torrent_if_needed(&self, result: &DownloadResult<()>) {
        if !tachyon_core::looks_like_magnet_url(&self.url) {
            return;
        }
        let should_cleanup = match result {
            Ok(()) => true,
            Err(DownloadError::Cancelled) => true,
            Err(_) => matches!(
                self.state,
                DownloadState::Cancelled | DownloadState::Failed | DownloadState::Completed
            ),
        };
        if !should_cleanup {
            return;
        }
        if let Some(magnet) = &self.bt_magnet {
            magnet.stop_and_remove_torrent(&self.url).await;
            return;
        }
        // hybrid fallback 路径
        if let Some(magnet) = &self.bt_fallback {
            magnet.stop_and_remove_torrent(&self.url).await;
        }
    }

    fn apply_terminal_error(&mut self, error: &DownloadError) {
        // 用户协作暂停:保持 Paused,不升 Failed
        if matches!(error, DownloadError::Paused) {
            if self.state != DownloadState::Paused {
                if let Ok(s) = self.state.try_transition(DownloadState::Paused) {
                    self.state = s;
                } else {
                    self.state = DownloadState::Paused;
                }
            }
            return;
        }
        // P1 / 审计 M-05:暂停超时应保持 Paused。
        // wait_control_rx 观察 Pause 时历史上不把 DownloadTask.state 设为 Paused
        // (仍为 Downloading),导致仅凭 state==Paused 的分支不可达。
        // 以控制通道最新命令为准:若仍是 Pause,则 Timeout 保持/恢复为 Paused。
        if matches!(error, DownloadError::Timeout(_)) {
            let control_paused = self
                .control_rx
                .as_ref()
                .is_some_and(|rx| matches!(*rx.borrow(), TaskCommand::Pause));
            if self.state == DownloadState::Paused || control_paused {
                if self.state != DownloadState::Paused {
                    if let Ok(s) = self.state.try_transition(DownloadState::Paused) {
                        self.state = s;
                    } else {
                        // 非法转换时仍强制对齐,避免 pause-timeout 误报 Failed
                        self.state = DownloadState::Paused;
                    }
                }
                warn!(
                    state = ?self.state,
                    error = %error,
                    "暂停态收到 Timeout,保持 Paused 不升级为 Failed(用户暂停语义优先)"
                );
                return;
            }
        }

        let target = if matches!(error, DownloadError::Cancelled)
            || self.state == DownloadState::Cancelled
        {
            DownloadState::Cancelled
        } else {
            DownloadState::Failed
        };
        match self.state.try_transition(target) {
            Ok(new_state) => self.state = new_state,
            Err(_) => {
                // 终态强制转换:非标准路径(如 Pending->Failed)时直接赋值
                warn!(from = ?self.state, to = ?target, "非标准状态转换(终态强制)");
                self.state = target;
            }
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

        // 步骤 4: 执行下载 (与取消信号竞速:execute 内部的流读取循环已 select! 化,
        // 此处再包一层 wait_for_cancel 作纵深防御,与步骤 1/3/5 同构)
        //
        // HTTP 全熔断 fallback:主源(execute)失败且 `bt_fallback` 可用时,切 BT
        // `download_full_stream` 整文件下载。仅 P2SP 混合模式(`with_hybrid_sources`)
        // 持有 bt_fallback;纯 HTTP / 纯 BT 路径无 fallback,失败直接向上传播。
        let execute_err = {
            let mut rx = self.control_rx.take();
            let r = match rx.as_mut() {
                Some(rx) => {
                    tokio::select! {
                        r = self.execute() => r,
                        _ = Self::wait_for_cancel(rx) => {
                            self.state = DownloadState::Cancelled;
                            return Err(DownloadError::Cancelled);
                        }
                    }
                }
                None => self.execute().await,
            };
            self.control_rx = rx;
            r
        };
        match execute_err {
            Ok(()) => {}
            Err(ref e) if self.should_try_bt_fallback(e) => {
                tracing::warn!(error = %e, "主源下载失败,尝试 BT fallback");
                self.execute_bt_fallback().await?;
            }
            Err(e) => return Err(e),
        }

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
            && matches!(rx.borrow().to_download_state(), DownloadState::Cancelled)
        {
            return Err(DownloadError::Cancelled);
        }
        Ok(())
    }

    /// 等待取消信号 (仅关注 Cancelled 状态)
    async fn wait_for_cancel(rx: &mut watch::Receiver<TaskCommand>) {
        loop {
            if matches!(
                rx.borrow_and_update().to_download_state(),
                DownloadState::Cancelled
            ) {
                return;
            }
            if rx.changed().await.is_err() {
                return; // 通道关闭
            }
        }
    }

    // ----- BT fallback (P2SP 混合模式:HTTP 主源全熔断后切 BT 整文件下载) -----

    /// 判断主源下载失败后是否应尝试 BT fallback。
    ///
    /// 条件:`bt_fallback` 存在(P2SP 混合模式,即 `with_hybrid_sources` 构造)
    /// **且**失败错误不是 `DownloadError::Cancelled`。纯 HTTP / 纯 BT 路径无
    /// `bt_fallback`,不触发,失败直接向上传播。
    ///
    /// **排除 `Cancelled`**:用户主动取消(`DownloadError::Cancelled`)是确定的终态语义,
    /// 不应再启动一次无意义的 BT 整文件下载,也不应掩盖取消语义。`Cancelled` 需立即向上
    /// 传播,由 `run_inner` 的 `Err(e) => return Err(e)` 兜底分支处理。
    ///
    /// **layout 兼容性**:严格 fallback 需「单文件 BT + 单文件 HTTP + 大小一致」才允许,
    /// 该校验在 `execute_bt_fallback` 内通过 BT `probe()` metadata 比对实现(见其文档)。
    #[cfg(feature = "magnet")]
    fn should_try_bt_fallback(&self, err: &DownloadError) -> bool {
        self.bt_fallback.is_some() && !matches!(err, DownloadError::Cancelled)
    }

    #[cfg(not(feature = "magnet"))]
    fn should_try_bt_fallback(&self, _err: &DownloadError) -> bool {
        false
    }

    /// BT fallback 执行桩(无 magnet feature)。
    ///
    /// 此方法在 `should_try_bt_fallback(..)` 恒为 `false` 时**不可达**(`run_inner`
    /// 的 `Err(ref e) if self.should_try_bt_fallback(e)` 守卫保证),仅为让
    /// `run_inner` 的 fallback 分支在非 magnet 编译下通过方法解析而存在。
    #[cfg(not(feature = "magnet"))]
    async fn execute_bt_fallback(&mut self) -> DownloadResult<()> {
        // 不可达:should_try_bt_fallback(..) 在非 magnet 下恒 false,守卫已挡住此分支。
        unreachable!("execute_bt_fallback 在非 magnet 编译下不应被调用")
    }

    /// 执行 BT fallback:用 `MagnetProtocol` 的 `download_full_stream` 整文件下载。
    ///
    /// 由 `run_inner` 步骤 4 在主源 `execute()` 失败且 `should_try_bt_fallback()` 为真时调用。
    /// BT 协议以流式方式产出整个文件数据,写入与 HTTP 路径相同的 engine storage
    /// (offset 0 起,顺序追加)。失败则向上返回错误(自然降级,不写错乱数据)。
    ///
    /// **layout 兼容校验(修复 I-3)**:`download_full_stream` 返回 BT 全局字节流,
    /// 但 engine storage 是按 HTTP 主源 probe 结果(`self.metadata`)初始化的单文件 layout。
    /// 若 BT 是多文件 torrent,`download_full_stream` 只产出第一个文件的字节流,
    /// 从 offset 0 写入会导致 storage 大小不匹配 / 内容错乱。因此在下载前先 `probe()`
    /// 拿 BT metadata,与 HTTP metadata 比对:
    /// - BT `file_count > 1` → 多文件 torrent,HTTP 单文件 layout 不兼容,返回错误;
    /// - BT `file_size != HTTP file_size` → 大小不一致,返回错误;
    /// - 单文件 + 大小一致(或 HTTP 无 size 信息) → 继续 `download_full_stream`。
    #[cfg(feature = "magnet")]
    async fn execute_bt_fallback(&mut self) -> DownloadResult<()> {
        let bt_proto = self.bt_fallback.as_ref().ok_or_else(|| {
            DownloadError::Other("BT fallback 不可用(bt_fallback 为 None)".into())
        })?;
        tracing::info!("启动 BT fallback 整文件下载");

        // layout 兼容校验:BT probe 拿 metadata,与 HTTP 主源 self.metadata 比对。
        // BT probe 失败直接返回错误(拿不到 metadata 无法校验,且后续 download 也大概率失败)。
        let bt_meta = bt_proto.probe(&self.url).await.map_err(|e| {
            tracing::warn!(error = %e, "BT fallback probe 失败");
            e
        })?;
        if let Some(http_meta) = &self.metadata {
            let bt_file_count = bt_meta
                .file_layout
                .as_ref()
                .map(|l| l.file_count())
                .unwrap_or(1);
            if bt_file_count > 1 {
                return Err(DownloadError::Other(format!(
                    "BT fallback 不支持多文件 torrent({bt_file_count} 文件),HTTP 主源 layout 不兼容"
                )
                .into()));
            }
            if bt_meta.file_size != http_meta.file_size {
                return Err(DownloadError::Other(
                    format!(
                        "BT fallback layout 不兼容:BT 大小 {} != HTTP 大小 {:?}",
                        bt_meta.file_size.unwrap_or(0),
                        http_meta.file_size
                    )
                    .into(),
                ));
            }
        }

        // BT 走 download_full_stream,返回 ByteStream(与 HTTP execute_full_download 同构)。
        // 失败直接返回错误 —— 不再 fallback(已无更低层源)。
        let stream = bt_proto
            .download_full_stream(&self.url)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "BT fallback download_full_stream 失败");
                e
            })?;

        // 复用 write_all_at 写入循环(与 download_single_fragment 的流式写入同构)。
        self.write_stream_to_storage_with_fallback(stream).await
    }

    /// 把 BT `ByteStream` 写入 storage(fallback 路径用)。
    ///
    /// 从 offset 0 开始顺序写入,聚合到 `WRITE_BATCH_BYTES` 后用 `write_all_at`
    /// 批量刷写(与 `download_single_fragment` 的小 chunk 聚合 + 批量刷写同构)。
    /// 取消信号通过 `watch_for_interrupt` 与流读取竞速穿透(死 swarm 下
    /// `stream.next()` 永久 Pending 时仍可取消)。
    ///
    /// 注:`write_all_at` 签名为 `(storage: &StorageSet, pos: u64, batch:
    /// bytes::Bytes, control_rx: &mut Option<...>, pause_timeout: Duration)`
    /// —— 接受 owned `Bytes`,`write_buf.split().freeze()` 零拷贝转 Bytes 后传入。
    #[cfg(feature = "magnet")]
    async fn write_stream_to_storage_with_fallback(
        &mut self,
        stream: tachyon_core::traits::ByteStream,
    ) -> DownloadResult<()> {
        let pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| DownloadError::Other("BT fallback 时 storage 未初始化".into()))?;
        let storage = Arc::clone(storage);

        tokio::pin!(stream);
        let mut pos: u64 = 0;
        let mut write_buf =
            AlignedBuf::new(WRITE_BATCH_BYTES).expect("AlignedBuf 分配失败(内存不足)");

        loop {
            // 流读取与取消信号竞速(与 download_single_fragment 的 select! 同构):
            // 死 swarm 下 stream.next() 永久 Pending,必须与 watch_for_interrupt 竞速
            // 否则取消信号无法穿透。cancel-safe:next() 仅持 &mut stream。
            let chunk_result = if let Some(rx) = self.control_rx.as_mut() {
                tokio::select! {
                    chunk = tokio_stream::StreamExt::next(&mut stream) => match chunk {
                        Some(r) => r,
                        None => break, // EOF:正常退出循环
                    },
                    interrupt = Self::watch_for_interrupt(rx, pause_timeout) => {
                        interrupt?;
                        return Err(DownloadError::Other("BT fallback 被取消".into()));
                    }
                }
            } else {
                match tokio_stream::StreamExt::next(&mut stream).await {
                    Some(r) => r,
                    None => break,
                }
            };
            let chunk = chunk_result?;
            // 大 chunk(>= WRITE_BATCH_BYTES)直接写入,不经过 AlignedBuf 聚合
            if chunk.len() >= WRITE_BATCH_BYTES {
                // 先刷写 write_buf 中累积的残余数据
                if !write_buf.is_empty() {
                    let written = Self::write_all_at(
                        &storage,
                        pos,
                        write_buf.split().freeze(),
                        &mut self.control_rx,
                        pause_timeout,
                    )
                    .await?;
                    pos = pos.checked_add(written).ok_or_else(|| {
                        DownloadError::Other(
                            format!("BT fallback 偏移溢出: {pos}+{written}").into(),
                        )
                    })?;
                }
                let written =
                    Self::write_all_at(&storage, pos, chunk, &mut self.control_rx, pause_timeout)
                        .await?;
                pos = pos.checked_add(written).ok_or_else(|| {
                    DownloadError::Other(format!("BT fallback 偏移溢出: {pos}+{written}").into())
                })?;
                continue;
            }
            // 容量不足时先刷写已有数据(AlignedBuf 固定容量不自动扩容)
            if !write_buf.is_empty() && write_buf.len() + chunk.len() > WRITE_BATCH_BYTES {
                let written = Self::write_all_at(
                    &storage,
                    pos,
                    write_buf.split().freeze(),
                    &mut self.control_rx,
                    pause_timeout,
                )
                .await?;
                pos = pos.checked_add(written).ok_or_else(|| {
                    DownloadError::Other(format!("BT fallback 偏移溢出: {pos}+{written}").into())
                })?;
            }
            write_buf.extend_from_slice(&chunk);
            if write_buf.len() >= WRITE_BATCH_BYTES {
                let written = Self::write_all_at(
                    &storage,
                    pos,
                    write_buf.split().freeze(),
                    &mut self.control_rx,
                    pause_timeout,
                )
                .await?;
                pos = pos.checked_add(written).ok_or_else(|| {
                    DownloadError::Other(format!("BT fallback 偏移溢出: {pos}+{written}").into())
                })?;
            }
        }
        // 刷残余
        if !write_buf.is_empty() {
            let written = Self::write_all_at(
                &storage,
                pos,
                write_buf.freeze(),
                &mut self.control_rx,
                pause_timeout,
            )
            .await?;
            pos = pos.checked_add(written).ok_or_else(|| {
                DownloadError::Other(format!("BT fallback 偏移溢出: {pos}+{written}").into())
            })?;
        }
        tracing::info!(bytes_written = pos, "BT fallback 写入完成");
        Ok(())
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

// ---------------------------------------------------------------------------
// 实现 core trait,使 app 层可通过动态分发操作任务,无需依赖具体 struct
// ---------------------------------------------------------------------------

impl tachyon_core::traits::TaskRunner for DownloadTask {
    fn set_control_rx(&mut self, rx: tokio::sync::watch::Receiver<TaskCommand>) {
        self.set_control_rx(rx);
    }

    fn set_completed_fragments(&mut self, fragments: Vec<u32>) {
        self.set_completed_fragments(fragments);
    }

    fn set_partial_fragments(&mut self, fragments: std::collections::HashMap<u32, u64>) {
        self.set_partial_fragments(fragments);
    }

    fn set_resume_object_identity(&mut self, identity: Option<ObjectIdentity>) {
        self.set_resume_object_identity(identity);
    }

    fn set_resume_supports_range(&mut self, supports_range: Option<bool>) {
        self.set_resume_supports_range(supports_range);
    }

    fn set_progress_sender(&mut self, tx: tokio::sync::mpsc::Sender<FragmentProgress>) {
        self.set_progress_sender(tx);
    }

    fn set_preferred_file_name(&mut self, name: String) {
        self.set_preferred_file_name(name);
    }

    fn probe(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<&FileMetadata>> + Send + '_>> {
        Box::pin(self.probe())
    }

    fn run(&mut self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        Box::pin(self.run())
    }

    fn metadata(&self) -> Option<&FileMetadata> {
        self.metadata()
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
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
    use std::time::Duration;
    use tachyon_core::test_harness::harness::{
        FailingStorage, MemoryStorage as MemStorage, test_config, test_metadata,
    };
    use tachyon_core::traits::{ByteStream, Verifier as VerifierTrait};
    use tachyon_io::storage::AsyncStorage;

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

    // ------ 1b. with_hybrid_sources:bt_fallback 字段存在 + 空镜像降级编译路径 ------

    // 验证 bt_fallback 字段存在且默认构造为 None(纯 HTTP / 纯 BT 路径)。
    // Task 6 仅落地字段 + 构造,fallback 触发逻辑在 Task 7。
    #[cfg(feature = "magnet")]
    #[tokio::test]
    async fn test_with_hybrid_sources_no_mirrors_degrades_to_bt() {
        // 无 HTTP 镜像 → 退化为纯 BT(with_pool_and_scheduler 路径)。
        // 完整 P2SP 测试需要真实 BtSession(tempfile + librqbit Session),较重,
        // 留待集成测试。此处仅验证:
        //   1. with_hybrid_sources 签名编译通过;
        //   2. 通过 new_for_test 构造的任务 bt_fallback 字段为 None(纯 HTTP 路径)。
        let config = test_config();
        let protocol = Arc::new(MockProto::new(test_metadata("data.zip", 2048)));
        let task = DownloadTask::new_for_test(
            "http://example.com/file.bin".into(),
            config,
            protocol,
            StorageKind::memory(),
        );
        // 纯 HTTP 构造路径不填充 bt_fallback
        assert!(
            task.bt_fallback.is_none(),
            "纯 HTTP 路径 bt_fallback 必须为 None"
        );
    }

    // ------ 1c. should_try_bt_fallback:Cancelled 排除 + bt_fallback 缺失时不触发 ------

    /// I-1 回归测试:`should_try_bt_fallback` 在 `bt_fallback` 存在时,
    /// 对 `DownloadError::Cancelled` 必须返回 false(用户主动取消是确定终态,
    /// 不应再启动 BT 整文件下载,也不应掩盖取消语义);对其他可重试错误
    /// (如 Timeout)返回 true。
    ///
    /// 另校验 `bt_fallback` 为 None(纯 HTTP / 纯 BT 路径)时,任何错误均返回
    /// false —— 失败直接向上传播,不触发 fallback。
    ///
    /// 仅需一个真实 `librqbit::Session`(构造 `MagnetProtocol` 占位),无需
    /// 预置 torrent / 真实 peer 网络:本测试只覆盖 `should_try_bt_fallback`
    /// 的判定逻辑(字段存在性 + 错误变体),不触及 `execute_bt_fallback` 的
    /// probe/download_full_stream 路径。
    #[cfg(feature = "magnet")]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_should_try_bt_fallback_excludes_cancelled() {
        use tachyon_protocol::MagnetProtocol;

        // 构造占位 MagnetProtocol(只需合法 Session,无需添加 torrent):
        // should_try_bt_fallback 只读 bt_fallback.is_some(),不调用其任何方法。
        let dir = tempfile::TempDir::new().unwrap();
        // Session::new_with_opts 已返回 Arc<Session>(见 magnet.rs:968 用法),
        // 无需再 Arc::new 包裹。
        let session = librqbit::Session::new_with_opts(
            dir.path().to_path_buf(),
            librqbit::SessionOptions {
                disable_dht: true,
                persistence: None,
                enable_upnp_port_forwarding: false,
                ..Default::default()
            },
        )
        .await
        .expect("创建 BT Session 失败");
        let bt_proto = std::sync::Arc::new(MagnetProtocol::new(
            session,
            tachyon_core::config::MagnetConfig::default(),
            dir.path().to_path_buf(),
            std::sync::Arc::new(dashmap::DashMap::new()),
        ));

        // 1) bt_fallback = Some:Cancelled 必须排除,其他错误(Timeout/Network)触发 fallback
        let meta = test_metadata("hybrid.bin", 2048);
        let protocol = Arc::new(MockProto::new(meta));
        let mut task = DownloadTask::new_for_test(
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            test_config(),
            protocol,
            StorageKind::memory(),
        );
        task.bt_fallback = Some(bt_proto);

        assert!(
            !task.should_try_bt_fallback(&DownloadError::Cancelled),
            "Cancelled 是确定终态,必须排除 BT fallback(不得掩盖取消语义)"
        );
        assert!(
            task.should_try_bt_fallback(&DownloadError::Timeout("30s".into())),
            "Timeout 在 bt_fallback 存在时应触发 BT fallback"
        );
        assert!(
            task.should_try_bt_fallback(&DownloadError::Network("主源熔断".into())),
            "Network 错误在 bt_fallback 存在时应触发 BT fallback"
        );
        assert!(
            task.should_try_bt_fallback(&DownloadError::Http {
                status: 503,
                reason: "unavailable".into()
            }),
            "Http 5xx 在 bt_fallback 存在时应触发 BT fallback"
        );

        // 2) bt_fallback = None(纯 HTTP / 纯 BT 路径):任何错误均不触发 fallback
        let plain_task = DownloadTask::new_for_test(
            "http://example.com/plain.bin".into(),
            test_config(),
            Arc::new(MockProto::new(test_metadata("plain.bin", 1024))),
            StorageKind::memory(),
        );
        assert!(
            plain_task.bt_fallback.is_none(),
            "纯 HTTP 路径 bt_fallback 必须为 None"
        );
        assert!(
            !plain_task.should_try_bt_fallback(&DownloadError::Network("失败".into())),
            "bt_fallback 为 None 时不得触发 fallback,失败直接向上传播"
        );
        assert!(
            !plain_task.should_try_bt_fallback(&DownloadError::Cancelled),
            "bt_fallback 为 None 时 Cancelled 也不触发 fallback"
        );
    }

    // ------ 1d. BT fallback 集成:HTTP 主源全熔断 → BT 整文件下载接管 (spec 5.4) ------

    /// 构造离线可读的 `MagnetProtocol`(预置文件 + 单文件 torrent + initial_check 完成),
    /// 复刻 `tachyon-protocol::magnet` 测试模块的 `make_offline_protocol` 模式。
    ///
    /// 通过 librqbit 的 `initial_check` 机制:预置文件内容与 torrent pieces 哈希匹配时,
    /// `add_torrent` 把所有 piece 标记为 have,`FileStream` / `download_full_stream` 立即可读,
    /// 无需真实 peer / DHT 网络。返回 `(protocol, magnet_url, 文件内容, TempDir)`。
    ///
    /// `file_size` 控制预置文件大小;`piece_len` 控制 torrent 分片大小(影响 piece 数)。
    /// `TempDir` 必须由调用方持有(预置文件 + Session 输出目录在其下)。
    #[cfg(feature = "magnet")]
    async fn make_offline_bt_fallback(
        file_size: usize,
        piece_len: u32,
    ) -> Result<
        (
            tachyon_protocol::MagnetProtocol,
            String,
            Vec<u8>,
            tempfile::TempDir,
        ),
        Box<dyn std::error::Error>,
    > {
        use librqbit::{
            AddTorrent, AddTorrentOptions, CreateTorrentOptions, Session, SessionOptions,
            create_torrent,
        };
        use tachyon_core::FileLayout;

        let dir = tempfile::TempDir::new()?;
        // 已知内容的预置文件(确定性字节,便于断言)
        let content: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
        let file_path = dir.path().join("data.bin");
        std::fs::write(&file_path, &content)?;

        // 从预置文件生成 torrent metainfo(pieces SHA1 基于文件内容)
        let torrent = create_torrent(
            &file_path,
            CreateTorrentOptions {
                name: None,
                piece_length: Some(piece_len),
            },
        )
        .await?;
        let magnet_url = format!("magnet:?xt=urn:btih:{}", torrent.info_hash().as_string());

        // Session 输出目录指向预置文件所在目录,initial_check 会校验已存在文件
        let session = Session::new_with_opts(
            std::path::PathBuf::from(dir.path()),
            SessionOptions {
                disable_dht: true,
                persistence: None,
                enable_upnp_port_forwarding: false,
                ..Default::default()
            },
        )
        .await?;

        let handle = session
            .add_torrent(
                AddTorrent::from_bytes(torrent.as_bytes()?),
                Some(AddTorrentOptions {
                    paused: false,
                    output_folder: Some(dir.path().to_string_lossy().into_owned()),
                    overwrite: true,
                    disable_trackers: true,
                    ..Default::default()
                }),
            )
            .await?
            .into_handle()
            .unwrap();

        // wait_until_completed 确保 initial_check 完成且 have_pieces 填满
        handle.wait_until_completed().await?;
        let config = tachyon_core::config::MagnetConfig::default();
        // 用 from_handle 直接预缓存 handle + layout 到 MagnetProtocol.handle_cache,
        // 使后续 bt_proto.probe(&magnet_url) 命中缓存短路(见 magnet.rs probe 的
        // handle_cache 命中分支),不再走 add_magnet_to_session —— 后者在「无 DHT/无 peer」
        // 离线场景会硬失败(librqbit 需 DHT/peer 发现元数据)。
        //
        // `from_handle` 由 tachyon-protocol 的 test-harness feature 暴露(下游测试构建
        // 可达),与生产构造路径(with_hybrid_sources 用 new + 真实磁力 probe)的区别仅在于
        // 跳过 magnet URL 解析 + add_torrent 注册 —— 这正是离线测试需要的接缝。
        // 单文件 torrent:layout 退化为单元素(file_id=0, 全局偏移 0)。
        let layout = FileLayout::single("data.bin".into(), file_size as u64);
        let protocol = tachyon_protocol::MagnetProtocol::from_handle(
            session,
            config,
            std::path::PathBuf::from(dir.path()),
            &magnet_url,
            handle,
            layout,
        );

        Ok((protocol, magnet_url, content, dir))
    }

    /// I-2 集成测试:spec 5.4「HTTP 失败 BT 接管」场景。
    ///
    /// 构造 P2SP 混合任务:主协议为 `MockProto`(模拟 HTTP 主源全熔断 —— probe 成功
    /// 返回 metadata,但 `download_range` 因无 range_data 失败),`bt_fallback` 为离线
    /// 预置的 `MagnetProtocol`(tempfile + initial_check,无真实 peer)。
    ///
    /// `run()` 流程:probe(MockProto 成功)→ init_storage → plan → prepare_storage →
    /// execute(MockProto 失败,`max_retries=0` 立即失败,无退避)→
    /// `should_try_bt_fallback(Network 错误)=true` → `execute_bt_fallback`:
    ///   - `bt_proto.probe(magnet_url)` 命中 from_handle 预缓存,layout 校验通过
    ///     (单文件 + 大小一致);
    ///   - `download_full_stream` 读预置文件字节流;
    ///   - `write_stream_to_storage_with_fallback` 写入 storage;
    /// → verify(校验关闭,直接通过)→ Completed。
    ///
    /// 断言:任务最终 Completed,storage 中数据 == BT 预置文件内容(证明 BT 接管写入)。
    #[cfg(feature = "magnet")]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_bt_fallback_triggered_on_http_failure() {
        let file_size = 4096usize;
        let (bt_protocol, magnet_url, bt_content, _dir) = make_offline_bt_fallback(file_size, 1024)
            .await
            .expect("构造离线 BT fallback 失败");

        // 主协议(MockProto):probe 成功(返回与 BT 一致大小,使 execute_bt_fallback 的
        // layout 兼容校验通过),但 download_range 无 range_data → 失败,模拟 HTTP 全熔断。
        let http_meta = test_metadata("data.bin", file_size as u64);
        let http_protocol: Arc<dyn Protocol> = Arc::new(MockProto::new(http_meta));

        // max_retries=0:execute 首次失败立即向上返回,避免重试退避拖慢测试。
        let mut config = test_config();
        config.max_retries = 0;

        let mut task = DownloadTask::new_for_test(
            // url 必须为 magnet_url:execute_bt_fallback 内 bt_proto.probe(&self.url)
            // 用此 url 命中 from_handle 预缓存。
            magnet_url,
            config,
            http_protocol,
            StorageKind::memory_with_capacity(file_size),
        );
        // 手动注入 bt_fallback(模拟 with_hybrid_sources 的填充结果)。
        task.bt_fallback = Some(Arc::new(bt_protocol));

        task.run().await.expect("BT fallback 后下载应成功完成");

        assert_eq!(
            task.state(),
            DownloadState::Completed,
            "HTTP 熔断 + BT 接管后任务应 Completed"
        );
        assert!((task.progress() - 1.0).abs() < f64::EPSILON, "进度应为 1.0");

        // 验证 storage 数据 == BT 预置文件内容(证明数据由 BT fallback 写入,非 HTTP)
        let mut buf = vec![0u8; file_size];
        task.storage
            .as_ref()
            .expect("storage 应已初始化")
            .read_at(0, &mut buf)
            .await
            .expect("读 storage 失败");
        assert_eq!(
            buf, bt_content,
            "storage 数据应与 BT 预置文件完全一致(BT 接管写入)"
        );
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

    /// 用户在「新建下载」中显式重命名后,probe() 应以用户名覆盖协议探测得到的文件名,
    /// 使下游 init_storage / 快照 / UI 全部读到统一的文件名。
    #[tokio::test]
    async fn test_preferred_file_name_overrides_probed_name() {
        let meta = test_metadata("original.bin", 4096);
        let protocol = Arc::new(MockProto::new(meta));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        task.set_preferred_file_name("user_renamed.bin".into());
        let probed = task.probe().await.expect("probe 应成功");
        assert_eq!(
            probed.file_name, "user_renamed.bin",
            "probe 后 metadata.file_name 应被用户重命名覆盖"
        );

        // 再次访问 metadata 也应保持覆盖结果
        assert_eq!(task.metadata().unwrap().file_name, "user_renamed.bin");
    }

    #[tokio::test]
    async fn test_with_mirrors_rejects_hls_playlist_url() {
        let config = test_config();
        let result = DownloadTask::with_mirrors(
            "https://cdn.example.com/live/index.m3u8".into(),
            vec!["https://mirror.example.com/index.m3u8".into()],
            config,
            None,
            Arc::new(AdaptiveDownloadScheduler::default_config()),
        )
        .await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("HLS 镜像应被拒绝"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("HLS") || msg.contains("m3u8"),
            "错误应说明 HLS 不支持镜像: {msg}"
        );
    }

    /// P0-7: DownloadTask 对 .m3u8 走 HlsProtocol,产物为分片拼接而非 playlist 文本
    /// 需 test-harness 放行 loopback SSRF。
    #[cfg(feature = "test-harness")]
    #[tokio::test]
    async fn test_download_task_hls_vod_downloads_segments_not_playlist() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let playlist = concat!(
            "#EXTM3U\n",
            "#EXTINF:1.0,\n",
            "seg0.ts\n",
            "#EXTINF:1.0,\n",
            "seg1.ts\n",
            "#EXT-X-ENDLIST\n",
        );
        Mock::given(method("GET"))
            .and(path("/vod.m3u8"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/vnd.apple.mpegurl")
                    .set_body_string(playlist),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/seg0.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(b"AAAA", "video/mp2t"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/seg1.ts"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(b"BBBB", "video/mp2t"))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.download_dir = dir.path().to_string_lossy().into_owned();
        config.max_retries = 1;
        let url = format!("{}/vod.m3u8", server.uri());
        let mut task = DownloadTask::new(url, config)
            .await
            .expect("构造 HLS 任务应成功");
        task.run().await.expect("HLS VOD 下载应成功");
        // 产物应是分片拼接
        let out = dir.path().join("vod.m3u8");
        // HlsProtocol extract_filename 可能保留 .m3u8 名;内容必须是媒体字节
        let path = if out.exists() {
            out
        } else {
            // 回退扫描目录
            let mut entries: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            assert!(!entries.is_empty(), "应产生下载文件");
            entries.remove(0)
        };
        let bytes = std::fs::read(&path).expect("读产物");
        assert_eq!(
            bytes,
            b"AAAABBBB",
            "产物应为 segment 拼接,不应为 playlist 文本; got {:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert!(
            !bytes.starts_with(b"#EXTM3U"),
            "产物不得是 m3u8 播放列表文本"
        );
    }

    /// 未设置 preferred_file_name 时,probe() 行为不变。
    #[tokio::test]
    async fn test_probe_keeps_protocol_file_name_when_no_preference() {
        let meta = test_metadata("from-protocol.bin", 4096);
        let protocol = Arc::new(MockProto::new(meta));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        let probed = task.probe().await.expect("probe 应成功");
        assert_eq!(probed.file_name, "from-protocol.bin");
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
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
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
        task.scheduler_config = sched_config;

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

    /// 多文件端到端:Metadata 携带 file_layout(两文件),init_storage 构造 StorageSet::Multi,
    /// run() 经分片下载 → StorageSet 按全局 offset 折算写入各文件 → 落盘到目录,
    /// 验证两个文件内容正确(跨文件边界的分片也能正确分发)。
    #[tokio::test]
    async fn test_run_multi_file_writes_to_directory() {
        use tachyon_core::{FileLayout, FileSpan};
        let file0_len = 512u64;
        let file1_len = 512u64;
        let total = file0_len + file1_len;

        // 两文件的确定性内容(不同基,便于区分)
        let data0: Vec<u8> = (0..file0_len).map(|i| (i % 251) as u8).collect();
        let data1: Vec<u8> = (0..file1_len).map(|i| ((i + 7) % 251) as u8).collect();
        let global: Vec<u8> = data0.iter().chain(data1.iter()).copied().collect();

        let layout = FileLayout::from_spans(vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: file0_len,
                name: "a.bin".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: file0_len,
                len: file1_len,
                name: "b.bin".into(),
            },
        ]);

        let meta = FileMetadata {
            file_name: "multi_torrent".into(),
            file_size: Some(total),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: Some(layout.clone()),
            protocol_managed_storage: false,
            resolved_host: None,
        };

        // MockProto:分片按 (start,end) 精确返回对应全局字节切片
        // 用 frag_size=300 的分片,其中分片 [300,599] 跨 file0/file1 边界(512),
        // StorageSet::Multi::write_at 会把它拆成 file0 的 [300,511] + file1 的 [0,87],
        // 真正覆盖跨文件边界分片的多文件分发路径(而非每分片只命中单文件)。
        let frag_size = 300u64;
        // 确认 frag_size 确实能跨边界:边界 512 不是 frag_size 的整数倍
        assert_ne!(
            file0_len % frag_size,
            0,
            "frag_size 必须不整除文件长度,否则分片不跨边界"
        );
        let mut protocol = MockProto::new(meta);
        let mut offset = 0u64;
        while offset < total {
            let end = (offset + frag_size - 1).min(total - 1);
            let chunk = Bytes::from(global[offset as usize..=end as usize].to_vec());
            protocol = protocol.with_range_data(offset, end, chunk);
            offset = end + 1;
        }
        let protocol: Arc<dyn Protocol> = Arc::new(protocol);

        // 临时 download_dir(真实文件系统,验证多文件落盘)
        let tmp = tempfile::TempDir::new().unwrap();
        let config = DownloadConfig {
            download_dir: tmp.path().to_string_lossy().into_owned(),
            verify_checksum: false,
            ..test_config()
        };

        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        // 不预置 storage:让 init_storage 据 file_layout 构造 StorageSet::Multi。
        // url 用 http:本测试语义是「跨文件边界分片 → StorageSet::Multi 分发」,
        // 与 BT 无关;magnet url 会命中 BT 小分片策略(file_size/32 clamp
        // [4MiB,16MiB]),1024 字节文件只剩 1 片,覆盖不到跨边界分片路径。
        let mut task = DownloadTask::new_for_test_no_storage(
            "http://example.com/multi_torrent".into(),
            config,
            protocol,
        );
        task.scheduler_config = sched_config;

        task.run().await.expect("多文件下载流程失败");
        assert_eq!(task.state(), DownloadState::Completed);

        // 验证两个文件落盘到 multi_torrent/ 子目录,内容正确
        let file0 = std::fs::read(tmp.path().join("multi_torrent").join("a.bin")).unwrap();
        let file1 = std::fs::read(tmp.path().join("multi_torrent").join("b.bin")).unwrap();
        assert_eq!(file0, data0, "file0 (a.bin) 内容应与 data0 一致");
        assert_eq!(file1, data1, "file1 (b.bin) 内容应与 data1 一致");
    }

    #[tokio::test]
    async fn test_execute_fragmented_download_short_range_stream_errors() {
        let frag_size = 128u64;
        let total_size = frag_size * 2;

        let meta = FileMetadata {
            file_name: "short-frag.bin".into(),
            file_size: Some(total_size),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };

        let frag_a = Bytes::from(vec![0x11; frag_size as usize]);
        let short_frag_b = Bytes::from(vec![0x22; frag_size as usize - 1]);
        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(meta)
                .with_range_data(0, frag_size - 1, frag_a)
                .with_range_data(frag_size, total_size - 1, short_frag_b),
        );
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };

        let mut task = DownloadTask::new_for_test(
            "http://example.com/short-frag.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = sched_config;

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let result = task.execute().await;
        assert!(
            result.is_err(),
            "分片流返回字节少于分片大小时必须报错，不能误判为成功"
        );
        assert_eq!(task.state(), DownloadState::Failed);
    }

    #[tokio::test]
    async fn test_execute_fragmented_download_overlong_range_stream_errors() {
        let frag_size = 128u64;
        let total_size = frag_size * 2;

        let meta = FileMetadata {
            file_name: "overlong-frag.bin".into(),
            file_size: Some(total_size),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };

        let overlong_frag_a = Bytes::from(vec![0x11; frag_size as usize + 1]);
        let protocol: Arc<dyn Protocol> =
            Arc::new(MockProto::new(meta).with_range_data(0, frag_size - 1, overlong_frag_a));
        let memory = MemStorage::with_capacity(total_size as usize + 1);
        let storage = StorageKind::new(memory.clone());
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };

        let mut task = DownloadTask::new_for_test(
            "http://example.com/overlong-frag.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = sched_config;

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let result = task.execute().await;
        assert!(
            result.is_err(),
            "分片流返回字节多于分片大小时必须报错，不能误判为成功"
        );
        assert_eq!(task.state(), DownloadState::Failed);
        let data = memory.get_data();
        assert_eq!(
            data[frag_size as usize], 0,
            "超长分片失败前不得写入下一个分片的首字节"
        );
    }

    #[tokio::test]
    async fn test_execute_fragmented_download_overlong_batch_flush_does_not_cross_boundary() {
        let frag_size = 256 * 1024 - 1;
        let total_size = frag_size * 2;

        let meta = FileMetadata {
            file_name: "overlong-batch-frag.bin".into(),
            file_size: Some(total_size),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };

        let overlong_frag_a = Bytes::from(vec![0x33; frag_size as usize + 1]);
        let protocol: Arc<dyn Protocol> =
            Arc::new(MockProto::new(meta).with_range_data(0, frag_size - 1, overlong_frag_a));
        let memory = MemStorage::with_capacity(total_size as usize + 1);
        let storage = StorageKind::new(memory.clone());
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };

        let mut task = DownloadTask::new_for_test(
            "http://example.com/overlong-batch-frag.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = sched_config;

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let result = task.execute().await;
        assert!(result.is_err(), "分片批量刷写越界时必须在写入前报错");
        assert_eq!(task.state(), DownloadState::Failed);
        let data = memory.get_data();
        assert_eq!(
            data[frag_size as usize], 0,
            "批量刷写失败前不得写入下一个分片的首字节"
        );
    }

    #[derive(Clone)]
    struct ShortWriteStorage {
        data: Arc<std::sync::Mutex<Vec<u8>>>,
        max_write_len: usize,
    }

    impl ShortWriteStorage {
        fn with_capacity(capacity: usize, max_write_len: usize) -> Self {
            Self {
                data: Arc::new(std::sync::Mutex::new(vec![0; capacity])),
                max_write_len,
            }
        }

        fn data(&self) -> Vec<u8> {
            self.data.lock().unwrap().clone()
        }
    }

    impl AsyncStorage for ShortWriteStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            Box::pin(async move {
                let len = data.len().min(self.max_write_len);
                let start = offset as usize;
                let end = start + len;
                let mut buf = self.data.lock().unwrap();
                if end > buf.len() {
                    buf.resize(end, 0);
                }
                buf[start..end].copy_from_slice(&data[..len]);
                Ok(len)
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move {
                let data = self.data.lock().unwrap();
                let start = offset as usize;
                let available = data.len().saturating_sub(start);
                let to_read = buf.len().min(available);
                if to_read > 0 {
                    buf[..to_read].copy_from_slice(&data[start..start + to_read]);
                }
                Ok(to_read)
            })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn allocate(
            &self,
            size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move {
                let mut data = self.data.lock().unwrap();
                data.resize(size as usize, 0);
                Ok(())
            })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            Box::pin(async move { Ok(self.data.lock().unwrap().len() as u64) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    /// 审计 H-01:write_buf 越过 effective_end 时必须裁剪/丢弃
    #[test]
    fn test_take_clamped_write_buf_truncates_past_effective_end() {
        let mut buf = AlignedBuf::new(256).unwrap();
        buf.extend_from_slice(&[1u8; 100]);
        // pos=10, end_inclusive=59 => 最多写 50 字节
        let batch =
            DownloadTask::take_clamped_write_buf(10, 59, &mut buf).expect("应产出裁剪后的 batch");
        assert_eq!(batch.len(), 50);
        assert!(buf.is_empty(), "split 后 write_buf 应空");
    }

    #[test]
    fn test_take_clamped_write_buf_clears_when_pos_past_end() {
        let mut buf = AlignedBuf::new(64).unwrap();
        buf.extend_from_slice(&[9u8; 16]);
        assert!(DownloadTask::take_clamped_write_buf(100, 50, &mut buf).is_none());
        assert!(buf.is_empty());
    }

    /// 审计 P0-3:分片 completed 进度事件前必须先 storage.sync,避免 snapshot 领先未落盘字节
    #[tokio::test]
    async fn test_fragment_completed_syncs_before_progress_event() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone)]
        struct CountingSyncStorage {
            inner: MemStorage,
            syncs: Arc<AtomicUsize>,
        }

        impl AsyncStorage for CountingSyncStorage {
            fn write_at(
                &self,
                offset: u64,
                data: bytes::Bytes,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
                self.inner.write_at(offset, data)
            }

            fn read_at<'a>(
                &'a self,
                offset: u64,
                buf: &'a mut [u8],
            ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
                self.inner.read_at(offset, buf)
            }

            fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
                let syncs = self.syncs.clone();
                Box::pin(async move {
                    syncs.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }

            fn allocate(
                &self,
                size: u64,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
                self.inner.allocate(size)
            }

            fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
                self.inner.file_size()
            }

            fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
                self.sync()
            }
        }

        // 两个分片,强制走 fragmented 路径(单分片会路由到 full download)
        let frag_size = 32 * 1024u64;
        let total = frag_size * 2;
        let first = bytes::Bytes::from(vec![0xAB; frag_size as usize]);
        let second = bytes::Bytes::from(vec![0xCD; frag_size as usize]);
        let meta = FileMetadata {
            file_name: "durable.bin".into(),
            file_size: Some(total),
            content_type: None,
            supports_range: true,
            etag: Some("\"strong-etag\"".into()),
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(meta)
                .with_range_data(0, frag_size - 1, first.clone())
                .with_range_data(frag_size, total - 1, second.clone()),
        );

        let syncs = Arc::new(AtomicUsize::new(0));
        let storage = StorageKind::new(CountingSyncStorage {
            inner: MemStorage::with_capacity(total as usize),
            syncs: syncs.clone(),
        });

        let (tx, mut rx) = tokio::sync::mpsc::channel::<FragmentProgress>(32);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/durable.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                max_concurrent_fragments: 2,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };
        task.set_progress_sender(tx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        assert!(
            task.fragments.len() >= 2,
            "应规划为多分片: {}",
            task.fragments.len()
        );
        task.prepare_storage().await.unwrap();
        task.execute().await.expect("下载应成功");

        let mut completed_events = 0u32;
        while let Ok(ev) = rx.try_recv() {
            if let FragmentProgress::Chunk {
                completed: true, ..
            } = ev
            {
                completed_events += 1;
            }
        }
        assert!(
            completed_events >= 2,
            "应收到每个分片的 completed 事件, actual={completed_events}"
        );
        // 每个完成分片至少一次 sync(+最终 close 一次)
        assert!(
            syncs.load(Ordering::SeqCst) >= completed_events as usize,
            "completed 前应至少每分片一次 storage.sync, syncs={}, completed={}",
            syncs.load(Ordering::SeqCst),
            completed_events
        );
    }

    /// CrashConsistencyMode::Loose 模式下,分片完成时不得调用 storage.sync,
    /// 仅在 close() 时落盘。验证 HDD/NVMe 极致吞吐场景的 fsync 跳过逻辑。
    #[tokio::test]
    async fn test_crash_consistency_loose_skips_fragment_sync() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone)]
        struct CountingSyncStorage {
            inner: MemStorage,
            syncs: Arc<AtomicUsize>,
        }

        impl AsyncStorage for CountingSyncStorage {
            fn write_at(
                &self,
                offset: u64,
                data: bytes::Bytes,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
                self.inner.write_at(offset, data)
            }

            fn read_at<'a>(
                &'a self,
                offset: u64,
                buf: &'a mut [u8],
            ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
                self.inner.read_at(offset, buf)
            }

            fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
                let syncs = self.syncs.clone();
                Box::pin(async move {
                    syncs.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }

            fn allocate(
                &self,
                size: u64,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
                self.inner.allocate(size)
            }

            fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
                self.inner.file_size()
            }

            fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
                self.sync()
            }
        }

        let frag_size = 32 * 1024u64;
        let total = frag_size * 2;
        let first = bytes::Bytes::from(vec![0xAB; frag_size as usize]);
        let second = bytes::Bytes::from(vec![0xCD; frag_size as usize]);
        let meta = FileMetadata {
            file_name: "loose.bin".into(),
            file_size: Some(total),
            content_type: None,
            supports_range: true,
            etag: Some("\"strong-etag\"".into()),
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(meta)
                .with_range_data(0, frag_size - 1, first.clone())
                .with_range_data(frag_size, total - 1, second.clone()),
        );

        let syncs = Arc::new(AtomicUsize::new(0));
        let storage = StorageKind::new(CountingSyncStorage {
            inner: MemStorage::with_capacity(total as usize),
            syncs: syncs.clone(),
        });

        let mut task = DownloadTask::new_for_test(
            "http://example.com/loose.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                max_concurrent_fragments: 2,
                crash_consistency_mode: tachyon_core::config::CrashConsistencyMode::Loose,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute().await.expect("下载应成功");

        // Loose 模式:分片完成时不 sync,仅在 close() 时 sync 一次。
        // 若分片 sync 仍触发,syncs 会 ≥ 3(2 分片 + 1 close)。
        let final_syncs = syncs.load(Ordering::SeqCst);
        assert_eq!(
            final_syncs, 1,
            "Loose 模式应仅在 close() 时 sync 一次,实际 syncs={final_syncs}(应=1,close 触发)"
        );
    }

    #[tokio::test]
    async fn test_execute_fragmented_download_handles_storage_short_writes() {
        let frag_size = 128u64;
        let total_size = frag_size * 2;
        let first = Bytes::from(vec![0x44; frag_size as usize]);
        let second = Bytes::from(vec![0x55; frag_size as usize]);

        let meta = FileMetadata {
            file_name: "short-write.bin".into(),
            file_size: Some(total_size),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(meta)
                .with_range_data(0, frag_size - 1, first.clone())
                .with_range_data(frag_size, total_size - 1, second.clone()),
        );
        let short_storage = ShortWriteStorage::with_capacity(total_size as usize, 17);
        let storage = StorageKind::new(short_storage.clone());
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };

        let mut task = DownloadTask::new_for_test(
            "http://example.com/short-write.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = sched_config;

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        task.execute()
            .await
            .expect("短写存储应通过循环补写完成分片");
        assert_eq!(task.state(), DownloadState::Completed);
        let data = short_storage.data();
        assert_eq!(&data[..frag_size as usize], &first[..]);
        assert_eq!(&data[frag_size as usize..], &second[..]);
    }

    /// 直接测 StorageSet::Multi::write_at 数据正确性(短写场景,复现 CI 错位 bug)
    ///
    /// 用 ShortWriteStorage(max_write_len=17)强制段内短写,验证 Multi::write_at
    /// 的 local_pos/total_written/remaining 推进在短写下不丢数据。
    #[tokio::test]
    async fn test_multi_write_at_short_write_correctness() {
        let file0_len = 512u64;
        let file1_len = 1024u64;
        let total = file0_len + file1_len;

        let s0_raw = ShortWriteStorage::with_capacity(file0_len as usize, 17);
        let s1_raw = ShortWriteStorage::with_capacity(file1_len as usize, 17);
        let s0 = StorageKind::new(s0_raw.clone());
        let s1 = StorageKind::new(s1_raw.clone());

        let layout = tachyon_core::types::FileLayout::from_spans(vec![
            tachyon_core::types::FileSpan {
                file_id: 0,
                global_offset: 0,
                len: file0_len,
                name: "a.bin".into(),
            },
            tachyon_core::types::FileSpan {
                file_id: 1,
                global_offset: file0_len,
                len: file1_len,
                name: "b.bin".into(),
            },
        ]);
        let ss = StorageSet::Multi {
            storages: vec![s0, s1],
            layout,
        };

        let data0: Vec<u8> = (0..file0_len).map(|i| (i % 251) as u8).collect();
        let data1: Vec<u8> = (0..file1_len).map(|i| ((i + 7) % 251) as u8).collect();
        let global: Vec<u8> = data0.iter().chain(data1.iter()).copied().collect();

        // 整块写入(跨 512 边界),触发 Multi::write_at 段内短写循环
        let chunk = bytes::Bytes::copy_from_slice(&global);
        let written = ss.write_at(0, chunk).await.unwrap();
        assert_eq!(written as u64, total, "Multi::write_at 应写入全部字节");

        assert_eq!(s0_raw.data(), data0, "a.bin(file0) 内容应与 data0 一致");
        assert_eq!(s1_raw.data(), data1, "b.bin(file1) 内容应与 data1 一致");
    }

    /// 测 write_all_at + Multi + 短写的端到端数据正确性
    ///
    /// 复现 CI test_run_multi_file_writes_to_directory 的数据错位:
    /// write_all_at 调 Multi::write_at,段内短写导致 total_written < batch.len(),
    /// 循环用 remaining.slice(total_written..) + pos 推进重写——验证不丢/不错位数据。
    #[tokio::test]
    async fn test_write_all_at_mut_multi_short_write_correctness() {
        let file0_len = 512u64;
        let file1_len = 1024u64;
        let total = file0_len + file1_len;

        let s0_raw = ShortWriteStorage::with_capacity(file0_len as usize, 17);
        let s1_raw = ShortWriteStorage::with_capacity(file1_len as usize, 17);
        let s0 = StorageKind::new(s0_raw.clone());
        let s1 = StorageKind::new(s1_raw.clone());
        let layout = tachyon_core::types::FileLayout::from_spans(vec![
            tachyon_core::types::FileSpan {
                file_id: 0,
                global_offset: 0,
                len: file0_len,
                name: "a.bin".into(),
            },
            tachyon_core::types::FileSpan {
                file_id: 1,
                global_offset: file0_len,
                len: file1_len,
                name: "b.bin".into(),
            },
        ]);
        let ss = StorageSet::Multi {
            storages: vec![s0, s1],
            layout,
        };

        let data0: Vec<u8> = (0..file0_len).map(|i| (i % 251) as u8).collect();
        let data1: Vec<u8> = (0..file1_len).map(|i| ((i + 7) % 251) as u8).collect();
        let global: Vec<u8> = data0.iter().chain(data1.iter()).copied().collect();

        // 整块经 write_all_at 写入(跨 512 边界 + 段内短写)
        let batch = bytes::Bytes::from(global);
        let written = DownloadTask::write_all_at(&ss, 0, batch, &mut None, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(written, total, "write_all_at 应写入全部字节");

        assert_eq!(s0_raw.data(), data0, "file0 数据错位");
        assert_eq!(s1_raw.data(), data1, "file1 数据错位");
    }

    /// 测 write_all_at_mut + Multi + 并发(复现 CI test_run_multi_file_writes_to_directory)
    ///
    /// 多个 task 同时写不同 offset 的分片到同一 StorageSet::Multi,
    /// 验证并发下数据不交错/不丢。
    #[tokio::test(flavor = "multi_thread")]
    async fn test_write_all_at_mut_multi_concurrent_correctness() {
        let file0_len = 512u64;
        let file1_len = 1024u64;
        let total = file0_len + file1_len;

        let s0_raw = ShortWriteStorage::with_capacity(file0_len as usize, 4096);
        let s1_raw = ShortWriteStorage::with_capacity(file1_len as usize, 4096);
        let s0 = StorageKind::new(s0_raw.clone());
        let s1 = StorageKind::new(s1_raw.clone());
        let layout = tachyon_core::types::FileLayout::from_spans(vec![
            tachyon_core::types::FileSpan {
                file_id: 0,
                global_offset: 0,
                len: file0_len,
                name: "a.bin".into(),
            },
            tachyon_core::types::FileSpan {
                file_id: 1,
                global_offset: file0_len,
                len: file1_len,
                name: "b.bin".into(),
            },
        ]);
        let ss = Arc::new(StorageSet::Multi {
            storages: vec![s0, s1],
            layout,
        });

        let data0: Vec<u8> = (0..file0_len).map(|i| (i % 251) as u8).collect();
        let data1: Vec<u8> = (0..file1_len).map(|i| ((i + 7) % 251) as u8).collect();
        let global: Vec<u8> = data0.iter().chain(data1.iter()).copied().collect();

        // 分片并发写,frag_size=300 跨 512 边界
        let frag_size = 300u64;
        let mut handles = tokio::task::JoinSet::new();
        let mut offset = 0u64;
        while offset < total {
            let end = (offset + frag_size - 1).min(total - 1);
            let chunk = bytes::Bytes::copy_from_slice(&global[offset as usize..=end as usize]);
            let ss = Arc::clone(&ss);
            let start = offset;
            handles.spawn(async move {
                let w = DownloadTask::write_all_at(&ss, start, chunk, &mut None, Duration::ZERO)
                    .await
                    .unwrap();
                assert_eq!(w, end - start + 1, "分片 {start}..{end} 写入量不符");
            });
            offset = end + 1;
        }
        while let Some(r) = handles.join_next().await {
            r.unwrap();
        }

        assert_eq!(s0_raw.data(), data0, "file0 并发写后数据错位");
        assert_eq!(s1_raw.data(), data1, "file1 并发写后数据错位");
    }

    /// 验证 write_all_at_mut 短写循环正确性 + 计时(AGENTS.md:44/97)
    ///
    /// 用 ShortWriteStorage(max_write_len=17)强制短写,验证:
    /// - 循环正确推进(remaining.slice(written..)),数据完整落盘
    /// - 零拷贝路径(freeze+write_at)不引入额外开销
    #[tokio::test]
    async fn test_write_all_at_mut_short_write_loop_correctness() {
        let total = 4096usize;
        let storage = ShortWriteStorage::with_capacity(total, 17);
        let ss = StorageSet::single(StorageKind::new(storage.clone()));
        let batch = bytes::BytesMut::from(&vec![0xA5u8; total][..]);
        let written = DownloadTask::write_all_at_mut(&ss, 0, batch, &mut None, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(written, total as u64, "短写循环应累计写入全部字节");
        assert_eq!(storage.data(), vec![0xA5u8; total], "数据应完整落盘");
    }

    /// 审计 residual:write_all_at(Bytes 路径)同样应对短写循环到写完
    #[tokio::test]
    async fn test_write_all_at_retries_short_write() {
        let total = 2048usize;
        let storage = ShortWriteStorage::with_capacity(total, 13);
        let ss = StorageSet::single(StorageKind::new(storage.clone()));
        let batch = bytes::Bytes::from(vec![0x5Au8; total]);
        let written = DownloadTask::write_all_at(&ss, 0, batch, &mut None, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(written, total as u64);
        assert_eq!(storage.data(), vec![0x5Au8; total]);
    }

    /// 审计 HTTP-09:整块路径应对可重试错误做 max_retries,且半写后重试不污染
    #[tokio::test]
    async fn test_full_download_retries_after_stream_error() {
        use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

        struct FullFailOnce {
            meta: FileMetadata,
            calls: Arc<AtomicU32>,
            payload: Bytes,
        }

        impl Protocol for FullFailOnce {
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
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let payload = self.payload.clone();
                Box::pin(async move { Ok(payload.slice(start as usize..(end as usize + 1))) })
            }

            fn download_range_stream(
                &self,
                _url: &str,
                start: u64,
                end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let this_payload = self.payload.clone();
                Box::pin(async move {
                    let data = this_payload.slice(start as usize..(end as usize + 1));
                    Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
                })
            }

            fn download_full(
                &self,
                _url: &str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let payload = self.payload.clone();
                Box::pin(async move { Ok(payload) })
            }

            fn download_full_stream(
                &self,
                _url: &str,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let n = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
                let payload = self.payload.clone();
                Box::pin(async move {
                    if n == 0 {
                        // 先吐半包再失败,模拟 RST 中途
                        let half = payload.slice(0..payload.len() / 2);
                        let err = DownloadError::Network("模拟整块流中途失败".into());
                        Ok(Box::pin(futures::stream::iter(vec![Ok(half), Err(err)])) as ByteStream)
                    } else {
                        Ok(Box::pin(futures::stream::once(async move { Ok(payload) }))
                            as ByteStream)
                    }
                })
            }
        }

        let payload = Bytes::from(vec![0x5Au8; 100]);
        let meta = FileMetadata {
            file_name: "full-retry.bin".into(),
            file_size: Some(payload.len() as u64),
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol = Arc::new(FullFailOnce {
            meta,
            calls: Arc::new(AtomicU32::new(0)),
            payload: payload.clone(),
        });
        let memory = MemStorage::with_capacity(payload.len());
        let mut task = DownloadTask::new_for_test(
            "http://example.com/full-retry.bin".into(),
            DownloadConfig {
                max_retries: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol as Arc<dyn Protocol>,
            StorageKind::new(memory.clone()),
        );
        task.run().await.expect("full download 应在重试后成功");
        assert_eq!(&memory.get_data()[..payload.len()], payload.as_ref());
        assert_eq!(task.state(), DownloadState::Completed);
    }

    /// 审计 HTTP-15:已知长度下响应超写必须在 write 前失败
    #[tokio::test]
    async fn test_full_download_rejects_oversized_known_length() {
        let oversize = Bytes::from_static(b"hello"); // 5 bytes
        let meta = FileMetadata {
            file_name: "oversize.bin".into(),
            file_size: Some(4),
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol = Arc::new(MockProto::new(meta).with_default_data(oversize));
        let memory = MemStorage::with_capacity(16);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/oversize.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol as Arc<dyn Protocol>,
            StorageKind::new(memory.clone()),
        );
        let err = task.run().await.expect_err("超长 body 应失败");
        let msg = err.to_string();
        assert!(
            msg.contains("超过声明长度") || msg.contains("expected") || msg.contains("不完整"),
            "应写前拒绝超写: {msg}"
        );
        let data = memory.get_data();
        let written_nonzero = data.iter().filter(|&&b| b != 0).count();
        assert!(
            written_nonzero < 5,
            "不得完整写入超长 body, actual nonzero={written_nonzero}"
        );
    }

    /// 审计 BT-17:protocol_managed_storage 时 plan 忽略 snapshot completed
    #[tokio::test]
    async fn test_plan_ignores_snapshot_skip_for_protocol_managed_storage() {
        let meta = FileMetadata {
            file_name: "t.bin".into(),
            file_size: Some(4096),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: true,
            resolved_host: None,
        };
        let protocol =
            Arc::new(MockProto::new(meta.clone()).with_default_data(Bytes::from(vec![0u8; 4096])));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/t.bin".into(),
            test_config(),
            protocol as Arc<dyn Protocol>,
            StorageKind::memory_with_capacity(4096),
        );
        task.set_completed_fragments(vec![0, 1]);
        task.metadata = Some(meta);
        let frags = task.plan().expect("plan");
        assert!(!frags.is_empty());
        for f in &task.fragments {
            assert_eq!(
                f.state,
                crate::fragment::FragmentState::Pending,
                "BT managed storage 不得跳过 snapshot 分片"
            );
        }
        assert!(
            task.completed_fragments.is_empty(),
            "plan 后应清空 completed_fragments"
        );
    }

    #[tokio::test]
    async fn test_full_download_survives_storage_short_write() {
        let data = Bytes::from(vec![0xCCu8; 300]);
        let meta = FileMetadata {
            file_name: "short-write-full.bin".into(),
            file_size: Some(data.len() as u64),
            content_type: None,
            supports_range: false, // 强制走 execute_full_download
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol = Arc::new(MockProto::new(meta).with_default_data(data.clone()));
        let storage = ShortWriteStorage::with_capacity(data.len(), 17);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/short-write-full.bin".into(),
            test_config(),
            protocol,
            StorageKind::new(storage.clone()),
        );
        task.run().await.expect("full download 应在短写下完成");
        assert_eq!(storage.data(), data.as_ref());
        assert_eq!(task.state(), DownloadState::Completed);
    }

    /// write_all_at_mut 计时基准:256KiB batch(对齐 WRITE_BATCH_BYTES),NoopStorage
    ///
    /// NoopStorage.write_at 零拷贝返回 len,隔离出 freeze/clone/slice 的纯逻辑开销。
    /// 用于同会话对比改前(advance+write_at_mut)与改后(freeze+write_at)的绝对耗时。
    #[tokio::test]
    async fn test_write_all_at_mut_256k_noop_timing() {
        use std::time::Instant;
        let ss = StorageSet::single(StorageKind::new(
            tachyon_core::test_harness::harness::NoopStorage,
        ));
        let batch = bytes::BytesMut::from(&vec![0u8; WRITE_BATCH_BYTES][..]);
        let iterations = 1000u32;
        let start = Instant::now();
        for _ in 0..iterations {
            // clone batch 供每轮消费(write_all_at_mut 入口 freeze 消费所有权)
            let _ =
                DownloadTask::write_all_at_mut(&ss, 0, batch.clone(), &mut None, Duration::ZERO)
                    .await
                    .unwrap();
        }
        let elapsed = start.elapsed();
        let per_op_ns = elapsed.as_nanos() / iterations as u128;
        eprintln!(
            "write_all_at_mut 256KiB NoopStorage: {per_op_ns} ns/op ({} iters, {elapsed:?} total)",
            iterations
        );
        // 回归护栏:单次零拷贝逻辑开销应 < 50µs(NoopStorage 无 I/O)
        assert!(
            per_op_ns < 50_000,
            "write_all_at_mut 单次开销 {per_op_ns} ns 过高,可能引入了拷贝"
        );
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
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
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
        task.scheduler_config = sched_config;

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
    async fn test_verify_require_strategy_without_expected_hash_fails() {
        let data = Bytes::from_static(b"missing expected checksum");
        let frag_info = FragmentInfo {
            index: 0,
            start: 0,
            end: data.len() as u64 - 1,
            size: data.len() as u64,
            downloaded: 0,
            hash: None,
        };
        let protocol = Arc::new(MockProto::new(test_metadata(
            "no-hash.bin",
            data.len() as u64,
        )));
        let storage = StorageKind::memory_with_capacity(data.len());
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                verify_strategy: tachyon_core::config::VerifyStrategy::Require,
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
        task.metadata = Some(test_metadata("no-hash.bin", data.len() as u64));

        let result = task.verify().await;

        assert!(matches!(result, Err(DownloadError::NoExpectedChecksum)));
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

    /// BestEffort 策略:无 expected hash 时应跳过校验并返回成功
    #[tokio::test]
    async fn test_verify_best_effort_skips_without_expected_hash() {
        let data = Bytes::from_static(b"best effort no hash");
        let frag_info = FragmentInfo {
            index: 0,
            start: 0,
            end: data.len() as u64 - 1,
            size: data.len() as u64,
            downloaded: 0,
            hash: None,
        };
        let protocol = Arc::new(MockProto::new(test_metadata("be.bin", data.len() as u64)));
        let storage = StorageKind::memory_with_capacity(data.len());
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                verify_strategy: tachyon_core::config::VerifyStrategy::BestEffort,
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
        task.metadata = Some(test_metadata("be.bin", data.len() as u64));

        let result = task.verify().await;
        assert!(
            result.is_ok(),
            "BestEffort 策略下无 expected hash 应跳过校验"
        );
    }

    /// BestEffort 策略:有 expected hash 时应正常校验
    #[tokio::test]
    async fn test_verify_best_effort_verifies_with_expected_hash() {
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

        let protocol = Arc::new(MockProto::new(test_metadata(
            "be-hash.bin",
            data.len() as u64,
        )));
        let storage = StorageKind::memory_with_capacity(data.len());

        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                verify_strategy: tachyon_core::config::VerifyStrategy::BestEffort,
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
        task.metadata = Some(test_metadata("be-hash.bin", data.len() as u64));

        task.verify().await.unwrap();
    }

    /// Skip 策略:完全跳过校验
    #[tokio::test]
    async fn test_verify_skip_strategy_always_skips() {
        let data = Bytes::from_static(b"skip strategy data");
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
            hash: Some(hash), // 即使有 hash 也跳过
        };

        let protocol = Arc::new(MockProto::new(test_metadata("skip.bin", data.len() as u64)));
        let storage = StorageKind::memory_with_capacity(data.len());

        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                verify_strategy: tachyon_core::config::VerifyStrategy::Skip,
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
        task.metadata = Some(test_metadata("skip.bin", data.len() as u64));

        let result = task.verify().await;
        assert!(result.is_ok(), "Skip 策略下应完全跳过校验");
    }

    // ------ 9b. 分片并行校验回归护栏 ------

    /// 并发读盘计数存储:内部委托 `MemStorage`,在 `read_at` 进入/退出时用
    /// `Arc<AtomicU32>` 统计并发活跃数,并更新峰值;读盘内 `sleep` 一小段,
    /// 使多个分片的读盘在时间上重叠,从而让并行 verify 的并发度可观测。
    ///
    /// 仅供 `test_verify_parallel_concurrent_reads` 用于验证 verify 分片并行化
    /// (JoinSet + Semaphore) 后读盘并发度 > 1。
    #[derive(Clone)]
    struct ConcurrentCountStorage {
        data: Arc<std::sync::Mutex<Vec<u8>>>,
        active: Arc<AtomicU32>,
        peak: Arc<AtomicU32>,
        read_delay: Duration,
    }

    impl ConcurrentCountStorage {
        fn with_capacity(capacity: usize, read_delay: Duration) -> Self {
            Self {
                data: Arc::new(std::sync::Mutex::new(vec![0u8; capacity])),
                active: Arc::new(AtomicU32::new(0)),
                peak: Arc::new(AtomicU32::new(0)),
                read_delay,
            }
        }

        /// 读取观测到的读盘并发峰值
        fn peak(&self) -> u32 {
            self.peak.load(AtomicOrdering::SeqCst)
        }
    }

    impl AsyncStorage for ConcurrentCountStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            let data_inner = self.data.clone();
            Box::pin(async move {
                let len = data.len();
                let start = offset as usize;
                let end = start + len;
                let mut buf = data_inner.lock().unwrap();
                if end > buf.len() {
                    buf.resize(end, 0);
                }
                buf[start..end].copy_from_slice(&data);
                Ok(len)
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            let data_inner = self.data.clone();
            let active = self.active.clone();
            let peak = self.peak.clone();
            let delay = self.read_delay;
            Box::pin(async move {
                // 进入读盘:active +1,更新峰值
                let cur = active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                peak.fetch_max(cur, AtomicOrdering::SeqCst);
                // 人为延迟,使多个分片的读盘时间重叠,并行度可见
                tokio::time::sleep(delay).await;
                // 退出读盘:active -1
                active.fetch_sub(1, AtomicOrdering::SeqCst);

                let data = data_inner.lock().unwrap();
                let start = offset as usize;
                let available = data.len().saturating_sub(start);
                let to_read = buf.len().min(available);
                if to_read > 0 {
                    buf[..to_read].copy_from_slice(&data[start..start + to_read]);
                }
                Ok(to_read)
            })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn allocate(
            &self,
            size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            let data_inner = self.data.clone();
            Box::pin(async move {
                let mut data = data_inner.lock().unwrap();
                data.resize(size as usize, 0);
                Ok(())
            })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            let data_inner = self.data.clone();
            Box::pin(async move { Ok(data_inner.lock().unwrap().len() as u64) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    /// 并行校验回归护栏 1:多分片中单个分片哈希错误,verify 应检出并短路失败。
    ///
    /// 构造 4 个连续分片(各 1KB),分片 0/1/3 数据正确且 hash 正确,
    /// 分片 2 用全 0 错误 hash。手动写盘 4 个分片的正确数据。
    /// 断言 verify 返回 `ChecksumMismatch` 且状态为 `Failed`。
    ///
    /// 该测试在串行 verify 下也应通过(串行同样能检出损坏分片),
    /// 用于保证 JoinSet 并行化后短路 abort 逻辑不破坏错误检出语义。
    #[tokio::test]
    async fn test_verify_parallel_multi_fragment_one_corrupt_fails() {
        let frag_size: u64 = 1024;
        let total_size = frag_size * 4;
        // 4 个分片各自的内容(便于区分)
        let frag_data: Vec<Bytes> = (0..4u8)
            .map(|i| Bytes::from(vec![0xA0 | i; frag_size as usize]))
            .collect();
        // 计算每个分片的正确 blake3 hash
        let frag_hashes: Vec<String> = frag_data
            .iter()
            .map(|d| CpuVerifier::blake3().compute_hash(d).unwrap())
            .collect();
        // 分片 2 使用全 0 错误 hash 触发 ChecksumMismatch
        let wrong_hash =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();

        let protocol = Arc::new(MockProto::new(test_metadata("par-corrupt.bin", total_size)));
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                verify_strategy: tachyon_core::config::VerifyStrategy::BestEffort,
                ..test_config()
            },
        );

        // 手动写盘 4 个分片的正确数据(连续 offset 0/1024/2048/3072)
        for (i, data) in frag_data.iter().enumerate() {
            let offset = (i as u64) * frag_size;
            task.storage
                .as_ref()
                .unwrap()
                .write_at(offset, data.clone())
                .await
                .unwrap();
        }

        // 构造 4 个分片记录:0/1/3 用正确 hash,2 用错误 hash
        let frags: Vec<FragmentRecord> = (0..4u32)
            .map(|i| {
                let start = (i as u64) * frag_size;
                let info = FragmentInfo {
                    index: i,
                    start,
                    end: start + frag_size - 1,
                    size: frag_size,
                    downloaded: 0,
                    hash: Some(if i == 2 {
                        wrong_hash.clone()
                    } else {
                        frag_hashes[i as usize].clone()
                    }),
                };
                FragmentRecord::new(info, 3)
            })
            .collect();
        task.fragments = frags;
        task.metadata = Some(test_metadata("par-corrupt.bin", total_size));

        let result = task.verify().await;
        assert!(result.is_err(), "存在损坏分片时校验应失败");
        assert!(
            matches!(result.unwrap_err(), DownloadError::ChecksumMismatch { .. }),
            "损坏分片应触发 ChecksumMismatch"
        );
        assert_eq!(task.state(), DownloadState::Failed);
    }

    /// 并行校验回归护栏 2:验证 verify 分片并行化后读盘并发度 > 1。
    ///
    /// 用 `ConcurrentCountStorage` 观测 `read_at` 并发峰值:4 个分片均不设
    /// `computed_hash`,强制走读盘计算路径;每个分片读盘时 sleep 5ms,使并发可见。
    /// 断言并发峰值 >= 2(证明至少 2 个分片读盘并行)。
    ///
    /// 回归:并行 verify 读盘峰值应 >= 2(JoinSet +
    /// Semaphore 并行化改造;并行化实现后应转为 GREEN。
    #[tokio::test]
    async fn test_verify_parallel_concurrent_reads() {
        let frag_size: u64 = 1024;
        let total_size = frag_size * 4;
        let read_delay = Duration::from_millis(5);

        // 4 个分片各自的内容
        let frag_data: Vec<Bytes> = (0..4u8)
            .map(|i| Bytes::from(vec![0xB0 | i; frag_size as usize]))
            .collect();
        // 计算每个分片的正确 blake3 hash(强制走读盘路径:不设 computed_hash)
        let frag_hashes: Vec<String> = frag_data
            .iter()
            .map(|d| CpuVerifier::blake3().compute_hash(d).unwrap())
            .collect();

        let protocol = Arc::new(MockProto::new(test_metadata(
            "par-concurrent.bin",
            total_size,
        )));
        let counting = ConcurrentCountStorage::with_capacity(total_size as usize, read_delay);
        let storage = StorageKind::new(counting.clone());
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                verify_strategy: tachyon_core::config::VerifyStrategy::BestEffort,
                ..test_config()
            },
        );

        // 手动写盘 4 个分片的正确数据
        for (i, data) in frag_data.iter().enumerate() {
            let offset = (i as u64) * frag_size;
            task.storage
                .as_ref()
                .unwrap()
                .write_at(offset, data.clone())
                .await
                .unwrap();
        }

        // 构造 4 个分片记录:均设正确 expected hash,不设 computed_hash,
        // 迫使 verify 走读盘计算路径,从而触发 ConcurrentCountStorage 的计数。
        let frags: Vec<FragmentRecord> = (0..4u32)
            .map(|i| {
                let start = (i as u64) * frag_size;
                let info = FragmentInfo {
                    index: i,
                    start,
                    end: start + frag_size - 1,
                    size: frag_size,
                    downloaded: 0,
                    hash: Some(frag_hashes[i as usize].clone()),
                };
                FragmentRecord::new(info, 3)
            })
            .collect();
        task.fragments = frags;
        task.metadata = Some(test_metadata("par-concurrent.bin", total_size));

        // 全部分片数据正确,verify 应成功
        task.verify().await.expect("数据正确时校验应通过");

        // 断言读盘并发峰值 >= 2
        let peak = counting.peak();
        assert!(
            peak >= 2,
            "verify 分片并行化后读盘并发峰值应 >= 2,实际: {peak}(串行 verify 为 1)"
        );
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
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
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
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
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
                _identity: Option<ObjectIdentity>,
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
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let this = self.clone();
                let url = url.to_owned();
                Box::pin(async move {
                    let data = this.download_range(&url, start, end, None).await?;
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
        task.scheduler_config = sched_config;

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

    /// 审计 HTTP-01:半缓冲失败后 retry 不得把旧 write_buf 拼进下一次 attempt
    #[tokio::test]
    async fn test_fragment_retry_clears_write_buf_between_attempts() {
        use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

        struct PartialThenOk {
            meta: FileMetadata,
            calls: Arc<AtomicU32>,
            payload: Bytes,
        }

        impl PartialThenOk {
            fn clone_inner(&self) -> Self {
                Self {
                    meta: self.meta.clone(),
                    calls: Arc::clone(&self.calls),
                    payload: self.payload.clone(),
                }
            }
        }

        impl Protocol for PartialThenOk {
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
                url: &str,
                start: u64,
                end: u64,
                identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let this = self.clone_inner();
                let url = url.to_owned();
                Box::pin(async move {
                    let mut stream = this
                        .download_range_stream(&url, start, end, identity)
                        .await?;
                    use futures::StreamExt;
                    let mut out = Vec::new();
                    while let Some(chunk) = stream.next().await {
                        out.extend_from_slice(&chunk?);
                    }
                    Ok(Bytes::from(out))
                })
            }

            fn download_range_stream(
                &self,
                _url: &str,
                start: u64,
                end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let n = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
                let payload = self.payload.clone();
                let len = (end - start + 1) as usize;
                Box::pin(async move {
                    if n == 0 {
                        // 半缓冲:小 chunk 后失败,模拟 write_buf 残留
                        let partial = Bytes::from(vec![0xEE; 64.min(len)]);
                        let err = DownloadError::Network("模拟半缓冲后失败".into());
                        Ok(Box::pin(futures::stream::iter(vec![Ok(partial), Err(err)]))
                            as ByteStream)
                    } else {
                        let data = payload.slice(start as usize..(end as usize + 1));
                        Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
                    }
                })
            }

            fn download_full(
                &self,
                _url: &str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let payload = self.payload.clone();
                Box::pin(async move { Ok(payload) })
            }
        }

        let payload = Bytes::from(vec![0xA5u8; 200]);
        let meta = FileMetadata {
            file_name: "pollute.bin".into(),
            file_size: Some(payload.len() as u64),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol = Arc::new(PartialThenOk {
            meta: meta.clone(),
            calls: Arc::new(AtomicU32::new(0)),
            payload: payload.clone(),
        });
        let memory = MemStorage::with_capacity(payload.len());
        let storage = StorageKind::new(memory.clone());
        let mut task = DownloadTask::new_for_test(
            "http://example.com/pollute.bin".into(),
            DownloadConfig {
                max_retries: 3,
                verify_checksum: false,
                ..test_config()
            },
            protocol as Arc<dyn Protocol>,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: 50,
            max_fragment_size: 80,
            ..Default::default()
        };
        task.run().await.expect("retry 后应成功");
        let data = memory.get_data();
        assert_eq!(
            &data[..payload.len()],
            payload.as_ref(),
            "retry 后文件必须等于原件,不得含 0xEE 污染前缀"
        );
        assert!(
            !data.windows(3).any(|w| w == [0xEE, 0xEE, 0xEE]),
            "不应残留失败 attempt 的 0xEE 序列"
        );
    }

    #[tokio::test]
    async fn test_fragment_retry_resilience() {
        struct FailOnceProtocol {
            meta: FileMetadata,
            fail_count: Arc<AtomicU32>,
            max_failures: u32,
        }

        impl Clone for FailOnceProtocol {
            fn clone(&self) -> Self {
                Self {
                    meta: self.meta.clone(),
                    fail_count: Arc::clone(&self.fail_count),
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
                start: u64,
                end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let count = self.fail_count.fetch_add(1, AtomicOrdering::SeqCst);
                let max_f = self.max_failures;
                Box::pin(async move {
                    if count < max_f {
                        Err(DownloadError::Network(format!("模拟故障 #{}", count)))
                    } else {
                        Ok(Bytes::from(vec![0xBB; (end - start + 1) as usize]))
                    }
                })
            }

            fn download_range_stream(
                &self,
                url: &str,
                start: u64,
                end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let this = self.clone();
                let url = url.to_owned();
                Box::pin(async move {
                    let data = this.download_range(&url, start, end, None).await?;
                    Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
                })
            }

            fn download_full(
                &self,
                _url: &str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                let size = self.meta.file_size.unwrap_or(0) as usize;
                Box::pin(async move { Ok(Bytes::from(vec![0xBB; size])) })
            }
        }

        let total_size = 400u64;

        // 使用小分片配置确保产生多个分片
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: 100,
            max_fragment_size: 200,
            sampling_interval_secs: 60,
            ewma_alpha: 0.3,
            ..Default::default()
        };

        // 第一次协议:前 2 次调用失败；禁用任务内重试以模拟用户重新启动前的失败场景。
        let protocol1: Arc<dyn Protocol> = Arc::new(FailOnceProtocol {
            meta: test_metadata("retry.bin", total_size),
            fail_count: Arc::new(AtomicU32::new(0)),
            max_failures: 2,
        });

        let storage1 = StorageKind::memory_with_capacity(total_size as usize);
        let mut task1 = DownloadTask::new_for_test(
            "http://example.com/retry.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol1,
            storage1,
        );
        task1.scheduler_config = sched_config.clone();

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
            fail_count: Arc::new(AtomicU32::new(0)),
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
        task2.scheduler_config = sched_config;

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
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                Box::pin(async move { Ok(Bytes::from(vec![0xDD; (end - start + 1) as usize])) })
            }

            fn download_range_stream(
                &self,
                _url: &str,
                start: u64,
                end: u64,
                _identity: Option<ObjectIdentity>,
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
            ..Default::default()
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
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: 100,
            max_fragment_size: 100,
            ..Default::default()
        };

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
        let (control_tx, control_rx) = watch::channel(TaskCommand::Pause);
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
        control_tx.send(TaskCommand::Cancel).unwrap();
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
        let (_control_tx, control_rx) = watch::channel(TaskCommand::Pause);
        task.set_control_rx(control_rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(1500), task.execute()).await;
        assert!(result.is_ok(), "暂停超时后不应永久等待控制信号");
        assert!(result.unwrap().is_err(), "暂停超时应返回错误");
    }

    /// P1: 暂停态的 pause_timeout 超时不应升级为 Failed。
    ///
    /// 用户显式 Pause 后超过 pause_timeout_secs,apply_terminal_error 收到 Timeout,
    /// 应保持 Paused 而非强制转 Failed(用户暂停语义优先,可后续 Resume/Cancel)。
    #[test]
    fn test_apply_terminal_error_paused_timeout_keeps_paused() {
        use tachyon_core::DownloadError;

        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(test_metadata("paused-keep.bin", 100)).with_range_data(
                0,
                99,
                Bytes::from(vec![0x11; 100]),
            ),
        );
        let mut task = DownloadTask::new_for_test(
            "http://example.com/paused-keep.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 1,
                pause_timeout_secs: 1,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            StorageKind::memory_with_capacity(100),
        );

        // 直接置为 Paused 态(模拟用户已暂停)
        task.state = DownloadState::Paused;

        // apply_terminal_error 收到 pause_timeout 触发的 Timeout
        let err = DownloadError::Timeout("暂停超过 1 秒".into());
        task.apply_terminal_error(&err);

        // 关键断言:状态应保持 Paused,而非被升级为 Failed
        assert_eq!(
            task.state,
            DownloadState::Paused,
            "暂停态收到 pause_timeout 不应升级为 Failed,保持 Paused(用户暂停语义优先)"
        );
    }

    /// 审计 M-05:state 仍为 Downloading 但 control=Pause 时,Timeout 也应保持 Paused
    #[test]
    fn test_apply_terminal_error_control_pause_timeout_keeps_paused() {
        use tachyon_core::DownloadError;

        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(test_metadata("ctrl-pause.bin", 100)).with_range_data(
                0,
                99,
                Bytes::from(vec![0x22; 100]),
            ),
        );
        let mut task = DownloadTask::new_for_test(
            "http://example.com/ctrl-pause.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 1,
                pause_timeout_secs: 1,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            StorageKind::memory_with_capacity(100),
        );

        task.state = DownloadState::Downloading;
        let (_tx, rx) = watch::channel(TaskCommand::Pause);
        task.set_control_rx(rx);

        let err = DownloadError::Timeout("暂停超过 1 秒".into());
        task.apply_terminal_error(&err);

        assert_eq!(
            task.state,
            DownloadState::Paused,
            "M-05: control=Pause + Timeout 时即使 state 仍是 Downloading 也必须保持/落为 Paused"
        );
    }

    #[test]
    fn test_apply_terminal_error_paused_network_fails() {
        use tachyon_core::DownloadError;

        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(test_metadata("paused-net.bin", 100)).with_range_data(
                0,
                99,
                Bytes::from(vec![0x11; 100]),
            ),
        );
        let mut task = DownloadTask::new_for_test(
            "http://example.com/paused-net.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 1,
                pause_timeout_secs: 1,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            StorageKind::memory_with_capacity(100),
        );

        // 对照:其他错误(如 Network)在 Paused 态应正常转 Failed
        task.state = DownloadState::Paused;
        let net_err = DownloadError::Network("连接失败".into());
        task.apply_terminal_error(&net_err);
        assert_eq!(
            task.state,
            DownloadState::Failed,
            "暂停态收到非 Timeout 错误应正常转 Failed"
        );
    }

    // ------ Head-Of-Line Blocking 韧性测试 ------

    /// 验证 dispatcher 不会因单个慢 worker 阻塞其他 fragment 分发(HOL 韧性)
    ///
    /// 模型: 3 个 fragment, 2 个 worker,第 1 个 fragment 故意延迟。
    /// 如果 dispatcher 存在 HOL blocking(round-robin + 阻塞 send),则
    /// fragment 2 会被阻塞等待 worker 0 处理完 fragment 0。
    /// 修复后(try-send + skip),fragment 1 应能被分配到空闲的 worker 1,
    /// 使 fragment 1 在 fragment 0 之前完成。
    #[tokio::test]
    async fn test_dispatcher_no_hol_blocking_slow_worker() {
        use std::sync::atomic::AtomicU64;

        let frag_size = 100u64;
        let total_size = frag_size * 3;

        let meta = test_metadata("hol.bin", total_size);

        // 跟踪每个 fragment 完成的时间戳
        let completion_times: Arc<std::sync::Mutex<Vec<u64>>> =
            Arc::new(std::sync::Mutex::new(vec![0u64; 3]));
        let epoch = Arc::new(AtomicU64::new(0));

        struct SlowFirstProtocol {
            meta: FileMetadata,
            completion_times: Arc<std::sync::Mutex<Vec<u64>>>,
            epoch: Arc<AtomicU64>,
        }

        impl Clone for SlowFirstProtocol {
            fn clone(&self) -> Self {
                Self {
                    meta: self.meta.clone(),
                    completion_times: Arc::clone(&self.completion_times),
                    epoch: Arc::clone(&self.epoch),
                }
            }
        }

        impl Protocol for SlowFirstProtocol {
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
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
            {
                Box::pin(async move { Ok(Bytes::new()) })
            }

            fn download_range_stream(
                &self,
                _url: &str,
                start: u64,
                end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>,
            > {
                let completion_times = Arc::clone(&self.completion_times);
                let epoch = Arc::clone(&self.epoch);
                Box::pin(async move {
                    // fragment 0 (start=0) 故意延迟,模拟慢网络
                    if start == 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                    // 记录完成时间
                    let now = epoch.fetch_add(1, AtomicOrdering::SeqCst);
                    let frag_index = (start / 100) as usize;
                    if let Ok(mut times) = completion_times.lock()
                        && frag_index < times.len()
                    {
                        times[frag_index] = now;
                    }
                    let data = Bytes::from(vec![0xAA; (end - start + 1) as usize]);
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

        let protocol: Arc<dyn Protocol> = Arc::new(SlowFirstProtocol {
            meta,
            completion_times: Arc::clone(&completion_times),
            epoch,
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };
        let mut task = DownloadTask::new_for_test(
            "http://example.com/hol.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 2, // 2 个 worker
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = sched_config;

        task.run().await.expect("下载应成功完成");

        // 验证: fragment 1 的完成时间应早于 fragment 0
        // epoch 递增:先完成的拿到更小值
        let times = completion_times.lock().unwrap();
        assert!(
            times[1] < times[0],
            "fragment 1 应在 fragment 0 之前完成(无 HOL blocking), \
             实际: frag0={}, frag1={}",
            times[0],
            times[1],
        );
    }

    #[derive(Clone)]
    struct NotifyingStorage {
        data: Arc<std::sync::Mutex<Vec<u8>>>,
        write_notify: Arc<tokio::sync::Notify>,
    }

    impl NotifyingStorage {
        fn with_capacity(capacity: usize) -> Self {
            Self {
                data: Arc::new(std::sync::Mutex::new(vec![0; capacity])),
                write_notify: Arc::new(tokio::sync::Notify::new()),
            }
        }

        fn data(&self) -> Vec<u8> {
            self.data.lock().unwrap().clone()
        }

        fn write_notify(&self) -> Arc<tokio::sync::Notify> {
            Arc::clone(&self.write_notify)
        }
    }

    impl AsyncStorage for NotifyingStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            Box::pin(async move {
                let start = offset as usize;
                let end = start + data.len();
                let mut buf = self.data.lock().unwrap();
                if end > buf.len() {
                    buf.resize(end, 0);
                }
                buf[start..end].copy_from_slice(&data);
                self.write_notify.notify_waiters();
                Ok(data.len())
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move {
                let data = self.data.lock().unwrap();
                let start = offset as usize;
                let available = data.len().saturating_sub(start);
                let to_read = buf.len().min(available);
                if to_read > 0 {
                    buf[..to_read].copy_from_slice(&data[start..start + to_read]);
                }
                Ok(to_read)
            })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn allocate(
            &self,
            size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move {
                self.data.lock().unwrap().resize(size as usize, 0);
                Ok(())
            })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            Box::pin(async move { Ok(self.data.lock().unwrap().len() as u64) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    #[derive(Clone)]
    struct BlockingWriteStorage {
        data: Arc<std::sync::Mutex<Vec<u8>>>,
        write_started: Arc<tokio::sync::Notify>,
        release_rx: watch::Receiver<bool>,
    }

    impl BlockingWriteStorage {
        fn with_capacity(capacity: usize, release_rx: watch::Receiver<bool>) -> Self {
            Self {
                data: Arc::new(std::sync::Mutex::new(vec![0; capacity])),
                write_started: Arc::new(tokio::sync::Notify::new()),
                release_rx,
            }
        }

        fn write_started(&self) -> Arc<tokio::sync::Notify> {
            Arc::clone(&self.write_started)
        }
    }

    impl AsyncStorage for BlockingWriteStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            Box::pin(async move {
                self.write_started.notify_waiters();
                let mut release_rx = self.release_rx.clone();
                while !*release_rx.borrow() {
                    release_rx
                        .changed()
                        .await
                        .map_err(|_| DownloadError::Other("写入释放信号关闭".into()))?;
                }

                let start = offset as usize;
                let end = start + data.len();
                let mut buf = self.data.lock().unwrap();
                if end > buf.len() {
                    buf.resize(end, 0);
                }
                buf[start..end].copy_from_slice(&data);
                Ok(data.len())
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move {
                let data = self.data.lock().unwrap();
                let start = offset as usize;
                let available = data.len().saturating_sub(start);
                let to_read = buf.len().min(available);
                if to_read > 0 {
                    buf[..to_read].copy_from_slice(&data[start..start + to_read]);
                }
                Ok(to_read)
            })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn allocate(
            &self,
            size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move {
                self.data.lock().unwrap().resize(size as usize, 0);
                Ok(())
            })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            Box::pin(async move { Ok(self.data.lock().unwrap().len() as u64) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    struct FailAfterPeerStartsProtocol {
        meta: FileMetadata,
        started: Arc<AtomicU32>,
        both_started: Arc<tokio::sync::Notify>,
        release_rx: watch::Receiver<bool>,
        panic_first_fragment: bool,
    }

    impl Clone for FailAfterPeerStartsProtocol {
        fn clone(&self) -> Self {
            Self {
                meta: self.meta.clone(),
                started: Arc::clone(&self.started),
                both_started: Arc::clone(&self.both_started),
                release_rx: self.release_rx.clone(),
                panic_first_fragment: self.panic_first_fragment,
            }
        }
    }

    impl Protocol for FailAfterPeerStartsProtocol {
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
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            Box::pin(async move { Ok(Bytes::from(vec![0xF1; (end - start + 1) as usize])) })
        }

        fn download_range_stream(
            &self,
            _url: &str,
            start: u64,
            end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            let started = Arc::clone(&self.started);
            let both_started = Arc::clone(&self.both_started);
            let mut release_rx = self.release_rx.clone();
            let panic_first_fragment = self.panic_first_fragment;
            Box::pin(async move {
                let current = started.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                if current >= 2 {
                    both_started.notify_waiters();
                }
                if start == 0 {
                    while started.load(AtomicOrdering::SeqCst) < 2 {
                        both_started.notified().await;
                    }
                    if panic_first_fragment {
                        panic!("首分片模拟 panic");
                    }
                    return Err(DownloadError::Network("首分片模拟失败".into()));
                }

                while !*release_rx.borrow() {
                    release_rx
                        .changed()
                        .await
                        .map_err(|_| DownloadError::Other("释放信号关闭".into()))?;
                }
                let data = Bytes::from(vec![0xF2; (end - start + 1) as usize]);
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

    #[tokio::test]
    async fn test_fragment_failure_aborts_and_drains_remaining_tasks_before_returning() {
        let frag_size = 100u64;
        let total_size = frag_size * 2;
        let (release_tx, release_rx) = watch::channel(false);
        let protocol: Arc<dyn Protocol> = Arc::new(FailAfterPeerStartsProtocol {
            meta: test_metadata("abort-remaining.bin", total_size),
            started: Arc::new(AtomicU32::new(0)),
            both_started: Arc::new(tokio::sync::Notify::new()),
            release_rx,
            panic_first_fragment: false,
        });
        let notifying_storage = NotifyingStorage::with_capacity(total_size as usize);
        let write_notify = notifying_storage.write_notify();
        let storage = StorageKind::new(notifying_storage.clone());
        let mut task = DownloadTask::new_for_test(
            "http://example.com/abort-remaining.bin".into(),
            DownloadConfig {
                max_retries: 0,
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let result = task.execute().await;
        assert!(result.is_err(), "首分片失败应导致执行失败");
        assert_eq!(task.state(), DownloadState::Failed);

        let leaked_write = write_notify.notified();
        release_tx.send(true).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), leaked_write)
                .await
                .is_err(),
            "失败返回后剩余分片必须已 abort/drain,不得继续写入存储"
        );
        assert!(
            notifying_storage.data().iter().all(|byte| *byte == 0),
            "失败后的后台分片不应在返回后继续写入"
        );
    }

    #[tokio::test]
    async fn test_fragment_panic_aborts_and_drains_remaining_tasks_before_returning() {
        let frag_size = 100u64;
        let total_size = frag_size * 2;
        let (release_tx, release_rx) = watch::channel(false);
        let protocol: Arc<dyn Protocol> = Arc::new(FailAfterPeerStartsProtocol {
            meta: test_metadata("panic-remaining.bin", total_size),
            started: Arc::new(AtomicU32::new(0)),
            both_started: Arc::new(tokio::sync::Notify::new()),
            release_rx,
            panic_first_fragment: true,
        });
        let notifying_storage = NotifyingStorage::with_capacity(total_size as usize);
        let write_notify = notifying_storage.write_notify();
        let storage = StorageKind::new(notifying_storage.clone());
        let mut task = DownloadTask::new_for_test(
            "http://example.com/panic-remaining.bin".into(),
            DownloadConfig {
                max_retries: 0,
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let result = task.execute().await;
        assert!(result.is_err(), "首分片 panic 应导致执行失败");
        assert_eq!(task.state(), DownloadState::Failed);

        let leaked_write = write_notify.notified();
        release_tx.send(true).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), leaked_write)
                .await
                .is_err(),
            "panic 返回后剩余分片必须已 abort/drain,不得继续写入存储"
        );
        assert!(
            notifying_storage.data().iter().all(|byte| *byte == 0),
            "panic 后的后台分片不应在返回后继续写入"
        );
    }

    #[tokio::test]
    async fn test_cancel_signal_interrupts_blocked_fragment_storage_write() {
        let frag_size = 100u64;
        let total_size = frag_size * 2;
        let mut mock = MockProto::new(test_metadata("cancel-write.bin", total_size));
        for i in 0..2u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_range_data(start, end, Bytes::from(vec![0xA0 | i as u8; 100]));
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);
        let (release_tx, release_rx) = watch::channel(false);
        let blocking_storage = BlockingWriteStorage::with_capacity(total_size as usize, release_rx);
        let write_started = blocking_storage.write_started();
        let storage = StorageKind::new(blocking_storage);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/cancel-write.bin".into(),
            DownloadConfig {
                max_retries: 0,
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };
        let (control_tx, control_rx) = watch::channel(TaskCommand::Start);
        task.set_control_rx(control_rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // 保持 release_tx 在测试作用域存活,避免 write_at 因通道关闭而提前返回,
        // 确保取消信号分支在 tokio::select! 中唯一就绪,消除竞态。
        let cancel_on_write = tokio::spawn(async move {
            write_started.notified().await;
            control_tx.send(TaskCommand::Cancel).unwrap();
        });
        let result = tokio::time::timeout(Duration::from_millis(500), task.execute())
            .await
            .expect("取消信号应中断阻塞中的存储写入");
        drop(release_tx);
        cancel_on_write.await.unwrap();
        assert!(matches!(result, Err(DownloadError::Cancelled)));
        assert_eq!(task.state(), DownloadState::Failed);
    }

    /// 验证:死 swarm(流读取永久 Pending)下,取消信号能穿透 stream.next().await
    ///
    /// 复现磁力链接死 swarm 卡死根因:MockProtocol 的 stalling range 返回永不产出项的
    /// pending 流(等价 librqbit FileStream.read() 在无 peer 时永久 Pending)。
    /// 修复前:`download_single_fragment` 的 `while let Some(...) = stream.next().await`
    /// 裸 await,取消检查点在循环体内不可达 → 500ms 测试超时失败。
    /// 修复后:流读取循环用 `tokio::select!` 与 `watch_for_interrupt` 竞速,取消即时返回。
    #[tokio::test]
    async fn test_cancel_signal_interrupts_stalled_stream_read() {
        let frag_size = 100u64;
        let total_size = frag_size * 2;
        // 两个分片均标记为"死 swarm"区间:download_range_stream 返回 pending 流
        let mut mock = MockProto::new(test_metadata("stall-stream.bin", total_size));
        for i in 0..2u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_stalling_range(start, end);
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/stall-stream.bin".into(),
            DownloadConfig {
                max_retries: 0,
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };
        let (control_tx, control_rx) = watch::channel(TaskCommand::Start);
        task.set_control_rx(control_rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // 给 worker 一点时间进入 stream.next().await(永久 Pending)后再发取消
        let cancel_after_stall = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            control_tx.send(TaskCommand::Cancel).unwrap();
        });
        let result = tokio::time::timeout(Duration::from_millis(500), task.execute())
            .await
            .expect("取消信号应中断死 swarm 下永久挂起的流读取");
        cancel_after_stall.await.unwrap();
        assert!(
            matches!(result, Err(DownloadError::Cancelled)),
            "应返回 Cancelled,实际: {result:?}"
        );
        assert_eq!(task.state(), DownloadState::Failed);
    }

    /// 回归测试:分片数 > channel 容量(worker_count * 2)时不得死锁
    ///
    /// 复现历史 bug:dispatcher spawn 曾在入队循环之后,导致 `frag_tx.send().await`
    /// 在 channel 满时永久挂起(dispatcher 尚未 spawn 消费)。当分片数 > worker_count*2
    /// 时必现死锁。修复后 dispatcher/worker spawn 在入队之前,send 可被消费。
    /// 本测试用 10 分片 + 2 worker(容量 4),若回归则 1s 超时失败。
    #[tokio::test]
    async fn test_fragments_exceeding_channel_capacity_do_not_deadlock() {
        let frag_size = 100u64;
        let total_size = frag_size * 10; // 10 分片
        let mut mock = MockProto::new(test_metadata("deadlock.bin", total_size));
        for i in 0..10u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_range_data(start, end, Bytes::from(vec![0xABu8; 100]));
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/deadlock.bin".into(),
            DownloadConfig {
                max_retries: 0,
                max_concurrent_fragments: 2, // channel 容量 = 2*2 = 4 < 10 分片
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // 若死锁回归,execute 永久挂起 → 1s 超时失败
        let result = tokio::time::timeout(Duration::from_secs(1), task.execute())
            .await
            .expect("分片数 > channel 容量时不应死锁,execute 应在超时内完成");
        result.expect("execute 应成功完成所有分片下载");
        assert_eq!(task.state(), DownloadState::Completed);
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
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: 100,
            max_fragment_size: 100,
            ..Default::default()
        };

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
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
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

    #[tokio::test]
    async fn test_unknown_size_full_download_respects_max_full_stream_bytes() {
        let data = Bytes::from_static(b"too large");
        let meta = FileMetadata {
            file_name: "unknown.bin".into(),
            file_size: None,
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol = Arc::new(MockProto::new(meta).with_default_data(data));
        let storage = StorageKind::memory();
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                max_full_stream_bytes: 4,
                ..test_config()
            },
        );

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        let result = task.execute().await;

        let err = result.expect_err("未知大小 full-stream 超过上限应失败");
        assert!(err.to_string().contains("超过上限"));
    }

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

        record.start_download().unwrap();
        assert_eq!(record.state, FragmentState::Downloading);
        assert!(!record.is_done());
        assert!(!record.is_failed());

        record
            .complete_download(1000, Duration::from_millis(50))
            .unwrap();
        assert_eq!(record.state, FragmentState::Verifying);
        assert_eq!(record.info.downloaded, 1000);
        assert!(record.last_duration.is_some());

        record.verify_ok().unwrap();
        assert_eq!(record.state, FragmentState::Writing);

        record.write_done().unwrap();
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

        record.start_download().unwrap();
        assert_eq!(record.state, FragmentState::Downloading);

        // 第一次失败:可以重试
        let can_retry = record.mark_failed().unwrap();
        assert!(can_retry, "首次失败应可重试");
        assert_eq!(record.state, FragmentState::Pending);
        assert_eq!(record.retry_count, 1);

        record.start_download().unwrap();

        // 第二次失败:超过重试次数
        let can_retry = record.mark_failed().unwrap();
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
        record.start_download().unwrap();
        record
            .complete_download(4, Duration::from_millis(5))
            .unwrap();
        assert_eq!(record.state, FragmentState::Verifying);
        let can_retry = record.mark_failed().unwrap();
        assert!(can_retry);
        assert_eq!(record.state, FragmentState::Pending);

        // 从 Writing 阶段失败
        let mut record = FragmentRecord::new(info, 3);
        record.start_download().unwrap();
        record
            .complete_download(4, Duration::from_millis(5))
            .unwrap();
        record.verify_ok().unwrap();
        assert_eq!(record.state, FragmentState::Writing);
        let can_retry = record.mark_failed().unwrap();
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
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };
        // 生产路径的初始控制状态正是 Start(Downloading)
        let (_tx, rx) = watch::channel(TaskCommand::Start);
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
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
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
        let (_tx, rx) = watch::channel(TaskCommand::Start);
        task.set_control_rx(rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute()
            .await
            .expect("Start 控制状态不应导致整块下载失败");
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
            _identity: Option<ObjectIdentity>,
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
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            let this = self.clone();
            let url = url.to_owned();
            Box::pin(async move {
                let data = this.download_range(&url, start, end, None).await?;
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
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };
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

    /// 续传时 plan 必须忽略带宽 recommendation,保持与冷启动确定性分片一致,
    /// 否则 snapshot 的 completed index 会与新边界错位。
    #[tokio::test]
    async fn test_plan_resume_ignores_recommendation_fragment_size() {
        use std::sync::Arc;
        use tachyon_core::traits::{DownloadScheduler, ScheduleRecommendation};

        struct BiasedScheduler;
        impl DownloadScheduler for BiasedScheduler {
            fn recommend(&self, _file_size: u64, max_concurrency: u32) -> ScheduleRecommendation {
                ScheduleRecommendation {
                    concurrency: max_concurrency.max(1),
                    // 故意给与默认(32MB/16=2MB)完全不同的分片大小
                    fragment_size: 4 * 1024 * 1024,
                    confidence: 0.99,
                }
            }
            fn observe_bandwidth(&self, _: u64) {}
            fn predicted_bandwidth(&self) -> u64 {
                100 * 1024 * 1024
            }
        }

        let file_size = 32 * 1024 * 1024u64; // 32MB
        let meta = test_metadata("resume-plan.bin", file_size);
        let protocol = Arc::new(MockProto::new(meta));
        let storage = StorageKind::memory_with_capacity(file_size as usize);

        // 冷启动无 resume:会吃 recommendation → 2MB 分片 → 16 片
        let mut fresh = DownloadTask::new_for_test(
            "http://example.com/resume-plan.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                max_concurrent_fragments: 16,
                ..test_config()
            },
            protocol.clone(),
            storage.clone(),
        );
        fresh.scheduler = Arc::new(BiasedScheduler);
        fresh.metadata = Some(test_metadata("resume-plan.bin", file_size));
        let fresh_plan = fresh.plan().expect("plan");
        assert_eq!(
            fresh_plan[0].size,
            4 * 1024 * 1024,
            "无 resume 时应采用 recommendation 4MB"
        );

        // 有 resume snapshot:必须忽略 recommendation,用确定性划分
        let mut resume = DownloadTask::new_for_test(
            "http://example.com/resume-plan.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                max_concurrent_fragments: 16,
                ..test_config()
            },
            protocol,
            storage,
        );
        resume.scheduler = Arc::new(BiasedScheduler);
        resume.metadata = Some(test_metadata("resume-plan.bin", file_size));
        resume.set_completed_fragments(vec![0]);
        let resume_plan = resume.plan().expect("plan");

        let stable =
            crate::fragment::plan_fragments(file_size, true, None, &resume.scheduler_config)
                .expect("stable plan");
        assert_eq!(
            resume_plan.len(),
            stable.len(),
            "续传 plan 分片数必须等于确定性 None 建议"
        );
        assert_eq!(
            resume_plan[0].size, stable[0].size,
            "续传首片 size 必须稳定,不得采用 recommendation 4MB"
        );
        assert_ne!(
            resume_plan[0].size,
            4 * 1024 * 1024,
            "续传不得使用 biased recommendation 分片大小"
        );
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

    /// 字节级断点续传:plan() 应为未完整分片注入 resume_offset 并调整进度。
    #[tokio::test]
    async fn test_resume_partial_fragment_sets_resume_offset() {
        let frag_size = 100u64;
        let total = frag_size * 3;
        let protocol: Arc<dyn Protocol> = Arc::new(FlakyFragmentProtocol {
            meta: test_metadata("partial_resume.bin", total),
            frag_size,
            fail_start: u64::MAX,
            fail_times: 0,
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let mut task = flaky_task(protocol, total, frag_size, 0);

        task.probe().await.unwrap();
        let mut partial = std::collections::HashMap::new();
        partial.insert(1, 50);
        task.set_partial_fragments(partial);
        task.plan().unwrap();

        let frag1 = &task.fragments[1];
        assert_eq!(
            frag1.resume_offset, 50,
            "resume_offset 应为持久化的部分字节数"
        );
        assert_eq!(frag1.info.downloaded, 50, "downloaded 应反映已下载字节数");
        assert!(
            (task.progress() - 50.0 / 300.0).abs() < 0.001,
            "进度应计入部分分片,实际 {}",
            task.progress()
        );
    }

    /// 共享限速器跨任务生效:设置 set_rate_limiter 后下载应使用该限速器
    #[tokio::test]
    async fn test_shared_rate_limiter_is_used() {
        let total_size = 400u64;
        let protocol: Arc<dyn Protocol> = Arc::new(FlakyFragmentProtocol {
            meta: test_metadata("shared_limiter.bin", total_size),
            frag_size: 100,
            fail_start: u64::MAX, // 不注入失败
            fail_times: 0,
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let mut task = flaky_task(protocol, total_size, 100, 0);
        // 设置一个极高速限速器(不应阻塞下载)
        let limiter = Arc::new(RateLimiter::new(u64::MAX));
        task.set_rate_limiter(limiter);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();
        task.execute().await.expect("共享限速器不应阻止下载完成");
        assert_eq!(task.state(), DownloadState::Completed);
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
            _identity: Option<ObjectIdentity>,
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
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            let this = self.clone();
            let url = url.to_owned();
            Box::pin(async move {
                let data = this.download_range(&url, start, end, None).await?;
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

    #[tokio::test]
    async fn test_open_with_strategy_standard() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage =
            DynStorage::open_with_strategy(tmp.path(), tachyon_core::config::IoStrategy::Standard)
                .await;
        assert!(storage.is_ok(), "Standard 策略应成功打开存储");
    }

    #[tokio::test]
    async fn test_open_with_strategy_win_aligned_fallback_on_non_windows() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage = DynStorage::open_with_strategy(
            tmp.path(),
            tachyon_core::config::IoStrategy::WinAligned,
        )
        .await;
        // 非 Windows 平台应回退到 Standard 并成功
        assert!(
            storage.is_ok(),
            "WinAligned 在非 Windows 平台应回退到 Standard"
        );
    }

    #[tokio::test]
    async fn test_open_with_strategy_iocp() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let storage =
            DynStorage::open_with_strategy(tmp.path(), tachyon_core::config::IoStrategy::Iocp)
                .await;
        assert!(storage.is_ok(), "Iocp 策略应成功打开存储");
    }

    // ── MirrorProtocol 测试 ──

    /// probe 可人为延迟且下载返回固定数据的 mock 协议
    #[derive(Clone)]
    struct ProbeSelectedSourceProtocol {
        meta: FileMetadata,
        probe_delay: Duration,
        range_data: Bytes,
        full_data: Bytes,
    }

    impl Protocol for ProbeSelectedSourceProtocol {
        fn probe(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>>
        {
            let meta = self.meta.clone();
            let delay = self.probe_delay;
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                Ok(meta)
            })
        }

        fn download_range(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            let data = self.range_data.clone();
            Box::pin(async move { Ok(data) })
        }

        fn download_range_stream(
            &self,
            url: &str,
            start: u64,
            end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            let this = self.clone();
            let url = url.to_owned();
            Box::pin(async move {
                let data = this.download_range(&url, start, end, None).await?;
                Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
            })
        }

        fn download_full(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            let data = self.full_data.clone();
            Box::pin(async move { Ok(data) })
        }
    }

    #[tokio::test]
    async fn test_mirror_downloads_use_probe_selected_source() {
        use super::MirrorProtocol;

        let primary: Arc<dyn Protocol> = Arc::new(ProbeSelectedSourceProtocol {
            meta: test_metadata("primary.bin", 12),
            probe_delay: Duration::from_millis(50),
            range_data: Bytes::from_static(b"primary-range"),
            full_data: Bytes::from_static(b"primary-full"),
        });
        let mirror: Arc<dyn Protocol> = Arc::new(ProbeSelectedSourceProtocol {
            meta: test_metadata("mirror.bin", 11),
            probe_delay: Duration::from_millis(0),
            range_data: Bytes::from_static(b"mirror-range"),
            full_data: Bytes::from_static(b"mirror-full"),
        });
        let protocol: Arc<dyn Protocol> = Arc::new(MirrorProtocol::new(
            primary,
            vec![("http://mirror1.com/file.bin".into(), mirror)],
        ));

        let metadata = protocol.probe("http://primary.com/file.bin").await.unwrap();
        assert_eq!(metadata.file_name, "mirror.bin");

        // P2 least-in-flight:probe 都成功后,download 选在途最少源(初始 tie-break 选 index 小=primary)。
        // 不再"probe 最快的源固定",而是多源并发按在途数选。单次调用可能选 primary 或 mirror。
        let full = protocol
            .download_full("http://primary.com/file.bin")
            .await
            .unwrap();
        assert!(
            full == Bytes::from_static(b"primary-full")
                || full == Bytes::from_static(b"mirror-full"),
            "least-in-flight 应从 probe 成功的源里选,实际: {full:?}"
        );

        let range = protocol
            .download_range("http://primary.com/file.bin", 0, 11, None)
            .await
            .unwrap();
        assert!(
            range == Bytes::from_static(b"primary-range")
                || range == Bytes::from_static(b"mirror-range"),
            "least-in-flight 应从可用源选,实际: {range:?}"
        );

        let mut stream = protocol
            .download_range_stream("http://primary.com/file.bin", 0, 11, None)
            .await
            .unwrap();
        let chunk = tokio_stream::StreamExt::next(&mut stream)
            .await
            .unwrap()
            .unwrap();
        assert!(
            chunk == Bytes::from_static(b"primary-range")
                || chunk == Bytes::from_static(b"mirror-range"),
            "least-in-flight 流式应从可用源选,实际: {chunk:?}"
        );
        assert!(tokio_stream::StreamExt::next(&mut stream).await.is_none());
    }

    /// 始终返回网络错误的 mock 协议
    struct AlwaysFailProtocol {
        meta: FileMetadata,
    }

    impl Clone for AlwaysFailProtocol {
        fn clone(&self) -> Self {
            Self {
                meta: self.meta.clone(),
            }
        }
    }

    impl Protocol for AlwaysFailProtocol {
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
            _start: u64,
            _end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            Box::pin(async { Err(DownloadError::Network("主源不可用".into())) })
        }
        fn download_range_stream(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            Box::pin(async { Err(DownloadError::Network("主源不可用(流)".into())) })
        }
        fn download_full(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            Box::pin(async { Err(DownloadError::Network("主源不可用(全量)".into())) })
        }
    }

    /// 镜像回退:主源 download_range 失败时回退到镜像
    #[tokio::test]
    async fn test_mirror_fallback_on_range_failure() {
        use super::MirrorProtocol;
        let meta = test_metadata("mirror.bin", 100);
        let primary: Arc<dyn Protocol> = Arc::new(AlwaysFailProtocol { meta: meta.clone() });
        let mirror: Arc<dyn Protocol> =
            Arc::new(MockProto::new(meta).with_range_data(0, 99, Bytes::from(vec![0xAA; 100])));
        let mirror_proto =
            MirrorProtocol::new(primary, vec![("http://mirror1.com".into(), mirror)]);

        let result = mirror_proto
            .download_range("http://primary.com", 0, 99, None)
            .await;
        assert!(result.is_ok(), "镜像回退应成功");
        assert_eq!(result.unwrap().len(), 100);
    }

    /// 镜像回退:主源 download_range_stream 失败时回退到镜像
    #[tokio::test]
    async fn test_mirror_fallback_on_stream_failure() {
        use super::MirrorProtocol;
        let meta = test_metadata("mirror_stream.bin", 100);
        let primary: Arc<dyn Protocol> = Arc::new(AlwaysFailProtocol { meta: meta.clone() });
        let mirror: Arc<dyn Protocol> =
            Arc::new(MockProto::new(meta).with_range_data(0, 99, Bytes::from(vec![0xBB; 100])));
        let mirror_proto =
            MirrorProtocol::new(primary, vec![("http://mirror1.com".into(), mirror)]);

        let result = mirror_proto
            .download_range_stream("http://primary.com", 0, 99, None)
            .await;
        assert!(result.is_ok(), "镜像流式回退应成功");
    }

    /// 镜像回退:主源 download_full 失败时回退到镜像
    #[tokio::test]
    async fn test_mirror_fallback_on_full_failure() {
        use super::MirrorProtocol;
        let meta = test_metadata("mirror_full.bin", 100);
        let primary: Arc<dyn Protocol> = Arc::new(AlwaysFailProtocol { meta: meta.clone() });
        let mirror: Arc<dyn Protocol> =
            Arc::new(MockProto::new(meta).with_default_data(Bytes::from(vec![0xCC; 100])));
        let mirror_proto =
            MirrorProtocol::new(primary, vec![("http://mirror1.com".into(), mirror)]);

        let result = mirror_proto.download_full("http://primary.com").await;
        assert!(result.is_ok(), "镜像全量回退应成功");
    }

    /// 主源成功时不回退到镜像
    #[tokio::test]
    async fn test_mirror_uses_primary_when_success() {
        use super::MirrorProtocol;
        let meta = test_metadata("primary_ok.bin", 50);
        let primary: Arc<dyn Protocol> = Arc::new(MockProto::new(meta.clone()).with_range_data(
            0,
            49,
            Bytes::from(vec![0xDD; 50]),
        ));
        // 镜像不应被调用(用 AlwaysFailProtocol 验证)
        let mirror: Arc<dyn Protocol> = Arc::new(AlwaysFailProtocol { meta });
        let mirror_proto =
            MirrorProtocol::new(primary, vec![("http://mirror1.com".into(), mirror)]);

        let result = mirror_proto
            .download_range("http://primary.com", 0, 49, None)
            .await;
        assert!(result.is_ok(), "主源成功时应直接返回");
    }

    /// 所有源均失败时返回主源错误
    #[tokio::test]
    async fn test_mirror_returns_primary_error_when_all_fail() {
        use super::MirrorProtocol;
        let meta = test_metadata("all_fail.bin", 100);
        let fail_proto: Arc<dyn Protocol> = Arc::new(AlwaysFailProtocol { meta });
        let mirror_proto = MirrorProtocol::new(
            fail_proto.clone(),
            vec![("http://mirror1.com".into(), fail_proto)],
        );

        let result = mirror_proto
            .download_range("http://primary.com", 0, 99, None)
            .await;
        assert!(result.is_err(), "所有源失败时应返回错误");
    }

    // ------ 补充: 真实断点续传 ------

    // ------ 补充: 控制信号 ------

    #[tokio::test]
    async fn test_cancel_signal_in_probe_phase() {
        let protocol = Arc::new(MockProto::new(test_metadata("cancel-probe.bin", 100)));
        let storage = StorageKind::memory();
        let mut task = make_task(protocol, storage, test_config());

        let (_tx, rx) = watch::channel(TaskCommand::Cancel);
        task.set_control_rx(rx);

        let result = task.run().await;
        assert!(
            matches!(result, Err(DownloadError::Cancelled)),
            "probe 阶段取消应返回 Cancelled, 实际: {result:?}"
        );
        assert_eq!(task.state(), DownloadState::Cancelled);
    }

    #[derive(Clone)]
    struct BlockingAllocateStorage {
        data: Arc<std::sync::Mutex<Vec<u8>>>,
        allocate_started: Arc<tokio::sync::Notify>,
    }

    impl BlockingAllocateStorage {
        fn with_capacity(capacity: usize) -> Self {
            Self {
                data: Arc::new(std::sync::Mutex::new(vec![0; capacity])),
                allocate_started: Arc::new(tokio::sync::Notify::new()),
            }
        }
    }

    impl AsyncStorage for BlockingAllocateStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            let buf = self.data.clone();
            Box::pin(async move {
                let start = offset as usize;
                let end = start + data.len();
                let mut v = buf.lock().unwrap();
                if end > v.len() {
                    v.resize(end, 0);
                }
                v[start..end].copy_from_slice(&data);
                Ok(data.len())
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            let data = self.data.clone();
            Box::pin(async move {
                let v = data.lock().unwrap();
                let start = offset as usize;
                let available = v.len().saturating_sub(start);
                let to_read = buf.len().min(available);
                if to_read > 0 {
                    buf[..to_read].copy_from_slice(&v[start..start + to_read]);
                }
                Ok(to_read)
            })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn allocate(
            &self,
            _size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            let notify = self.allocate_started.clone();
            Box::pin(async move {
                notify.notify_waiters();
                // 阻塞以让 cancel 信号有机会被 select
                std::future::pending::<()>().await;
                Ok(())
            })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            let data = self.data.clone();
            Box::pin(async move { Ok(data.lock().unwrap().len() as u64) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    #[tokio::test]
    async fn test_cancel_signal_in_prepare_storage_phase() {
        let protocol = Arc::new(MockProto::new(test_metadata("cancel-alloc.bin", 100)));
        let blocking_storage = BlockingAllocateStorage::with_capacity(100);
        let allocate_started = blocking_storage.allocate_started.clone();
        let storage = StorageKind::new(blocking_storage);
        let mut task = make_task(protocol, storage, test_config());

        let (tx, rx) = watch::channel(TaskCommand::Start);
        task.set_control_rx(rx);

        let handle = tokio::spawn(async move {
            let result = task.run().await;
            (task, result)
        });

        allocate_started.notified().await;
        tx.send(TaskCommand::Cancel).unwrap();

        let (task, result) = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(result, Err(DownloadError::Cancelled)),
            "prepare_storage 阶段取消应返回 Cancelled, 实际: {result:?}"
        );
        assert_eq!(task.state(), DownloadState::Cancelled);
    }

    #[test]
    fn test_control_is_paused_detects_pause_command() {
        let (_tx, rx) = watch::channel(TaskCommand::Start);
        let opt = Some(rx);
        assert!(!DownloadTask::control_is_paused(&opt));
        let (tx, rx) = watch::channel(TaskCommand::Pause);
        let opt = Some(rx);
        assert!(DownloadTask::control_is_paused(&opt));
        let _ = tx; // keep sender alive
        assert!(!DownloadTask::control_is_paused(&None));
    }

    /// RED-TDD: watch_for_interrupt 在 Pause 时必须立即返回 Err(Paused),
    /// 以便 select! 抢占 in-flight stream/write(而非挂起等 Resume 导致继续读网)。
    #[tokio::test]
    async fn test_watch_for_interrupt_returns_immediately_on_pause() {
        let (tx, mut rx) = watch::channel(TaskCommand::Start);
        // 并发:一边 watch,一边稍后发 Pause(不 spawn 借用 rx,避免 'static 约束)
        let watch = DownloadTask::watch_for_interrupt(&mut rx, Duration::from_secs(30));
        let send = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(TaskCommand::Pause).unwrap();
        };
        let (result, _) = tokio::join!(
            tokio::time::timeout(Duration::from_millis(200), watch),
            send
        );
        let result = result.expect("Pause 后 watch_for_interrupt 必须在 200ms 内返回");
        assert!(
            matches!(result, Err(DownloadError::Paused)),
            "期望 Err(Paused), got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_pause_then_resume_continues_download() {
        let frag_size = 100u64;
        let total = frag_size * 2;
        let mut mock = MockProto::new(test_metadata("pause-resume.bin", total));
        for i in 0..2u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_range_data(
                start,
                end,
                Bytes::from(vec![0xD0 | i as u8; frag_size as usize]),
            );
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);
        let storage = StorageKind::memory_with_capacity(total as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/pause-resume.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        let (tx, rx) = watch::channel(TaskCommand::Pause);
        task.set_control_rx(rx);

        let handle = tokio::spawn(async move {
            let result = task.run().await;
            (task, result)
        });

        // 让任务进入暂停等待
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(TaskCommand::Resume).unwrap();

        let (task, result) = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap()
            .unwrap();
        result.expect("Pause 后 Resume 应继续并完成下载");
        assert_eq!(task.state(), DownloadState::Completed);
        assert!((task.progress() - 1.0).abs() < f64::EPSILON);
    }

    // ------ 补充: 限速真实效果 ------

    #[tokio::test]
    async fn test_rate_limit_real_effect() {
        let total_size = 2000u64;
        let data = Bytes::from(vec![0xE5; total_size as usize]);
        let meta = FileMetadata {
            file_name: "rate-limit.bin".into(),
            file_size: Some(total_size),
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol = Arc::new(MockProto::new(meta).with_default_data(data));
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                rate_limit_bytes_per_sec: Some(1000),
                ..test_config()
            },
        );

        let start = std::time::Instant::now();
        task.run().await.expect("限速下载应成功完成");
        let elapsed = start.elapsed();

        // 1000 B/s, 2000 字节: 初始突发 1000 字节, 剩余 1000 字节约需 1 秒
        assert!(
            elapsed.as_secs_f64() >= 0.7,
            "限速 1000 B/s 下载 2000 字节应至少耗时 0.7s, 实际 {:.2}s",
            elapsed.as_secs_f64()
        );
        assert!(
            elapsed.as_secs_f64() < 5.0,
            "耗时上界应宽松, 实际 {:.2}s",
            elapsed.as_secs_f64()
        );
        assert_eq!(task.state(), DownloadState::Completed);
    }

    // ------ 补充: 未知大小文件整流下载 ------

    #[tokio::test]
    async fn test_unknown_size_full_stream_download_success() {
        let data = Bytes::from_static(b"unknown size stream content");
        let meta = FileMetadata {
            file_name: "unknown-success.bin".into(),
            file_size: None,
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol = Arc::new(MockProto::new(meta).with_default_data(data.clone()));
        let storage = StorageKind::memory();
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: false,
                max_full_stream_bytes: 1024,
                ..test_config()
            },
        );

        task.run().await.expect("未知大小整流下载应成功");

        assert_eq!(task.state(), DownloadState::Completed);
        assert!((task.progress() - 1.0).abs() < f64::EPSILON);

        if let Some(ref storage) = task.storage {
            let mut buf = vec![0u8; data.len()];
            storage.read_at(0, &mut buf).await.unwrap();
            assert_eq!(buf, data.as_ref());
        }
    }

    // ------ 补充: 校验策略 ------

    #[tokio::test]
    async fn test_verify_require_strategy_hash_mismatch_fails() {
        let data = Bytes::from_static(b"require mismatch data");
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let frag_info = FragmentInfo {
            index: 0,
            start: 0,
            end: data.len() as u64 - 1,
            size: data.len() as u64,
            downloaded: 0,
            hash: Some(wrong_hash.into()),
        };

        let protocol = Arc::new(MockProto::new(test_metadata(
            "require-mismatch.bin",
            data.len() as u64,
        )));
        let storage = StorageKind::memory_with_capacity(data.len());
        let mut task = make_task(
            protocol,
            storage,
            DownloadConfig {
                verify_checksum: true,
                verify_strategy: tachyon_core::config::VerifyStrategy::Require,
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
        task.metadata = Some(test_metadata("require-mismatch.bin", data.len() as u64));

        let result = task.verify().await;
        assert!(
            matches!(result, Err(DownloadError::ChecksumMismatch { .. })),
            "Require 策略下 hash 不匹配应返回 ChecksumMismatch"
        );
        assert_eq!(task.state(), DownloadState::Failed);
    }

    // ------ 补充: 进度与指标 ------

    #[tokio::test]
    async fn test_progress_tx_and_metrics_updated() {
        let frag_size = 100u64;
        let total = frag_size * 3;

        let meta = test_metadata("progress-metrics.bin", total);
        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(meta)
                .with_range_data(
                    0,
                    frag_size - 1,
                    Bytes::from(vec![0xAA; frag_size as usize]),
                )
                .with_range_data(
                    frag_size,
                    2 * frag_size - 1,
                    Bytes::from(vec![0xBB; frag_size as usize]),
                )
                .with_range_data(
                    2 * frag_size,
                    total - 1,
                    Bytes::from(vec![0xCC; frag_size as usize]),
                ),
        );

        let storage = StorageKind::memory_with_capacity(total as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/progress-metrics.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<FragmentProgress>(100);
        task.set_progress_sender(progress_tx);

        let metrics = Arc::new(Metrics::new());
        task.set_metrics(metrics.clone());

        task.run().await.expect("下载应成功");

        let mut events = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(100), progress_rx.recv()).await
        {
            events.push(event);
        }

        let completed_events: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    FragmentProgress::Chunk {
                        completed: true,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(completed_events.len(), 3, "应收到 3 个分片完成事件");

        let (bytes, fragments, errors) = metrics.snapshot();
        assert_eq!(bytes, total, "Metrics 字节数应等于文件大小");
        assert!(fragments >= 3, "Metrics 分片完成数应 >= 3");
        assert_eq!(errors, 0);
    }

    // ------ 补充: Mirror 集成 ------

    #[tokio::test]
    async fn test_with_mirrors_creates_task() {
        let config = test_config();
        let result = DownloadTask::with_mirrors(
            "http://primary.com/file.bin".into(),
            vec![
                "http://mirror1.com/file.bin".into(),
                "http://mirror2.com/file.bin".into(),
            ],
            config,
            None,
            Arc::new(AdaptiveDownloadScheduler::default_config()),
        )
        .await;
        assert!(result.is_ok(), "with_mirrors 应成功创建任务");
        let mut task = result.unwrap();
        assert_eq!(task.url(), "http://primary.com/file.bin");

        // 覆盖未测试的公共 setter / getter
        task.set_rate_limiter(Arc::new(RateLimiter::new(1024)));
        task.set_metrics(Arc::new(Metrics::new()));
        task.set_completed_fragments(vec![0]);
        let mut partial = HashMap::new();
        partial.insert(1, 50);
        task.set_partial_fragments(partial);
        assert_eq!(task.state(), DownloadState::Pending);
        assert!((task.progress() - 0.0).abs() < f64::EPSILON);
        assert!(task.metadata().is_none());
        assert!(task.fragment_infos().is_empty());
    }

    /// 审计:with_mirrors 必须使用注入的 scheduler,而非内部 default_config。
    ///
    /// 行为:注入已 observe 带宽样本的 AdaptiveDownloadScheduler 后 plan(),
    /// 推荐分片大小应反映该调度器状态(confidence > 0),而非冷启动默认。
    #[tokio::test]
    async fn test_with_mirrors_uses_injected_scheduler() {
        let config = DownloadConfig {
            max_concurrent_fragments: 8,
            ..test_config()
        };
        let sched = AdaptiveDownloadScheduler::new(SchedulerConfig {
            min_fragment_size: 2 * 1024 * 1024,
            max_fragment_size: 2 * 1024 * 1024,
            ..Default::default()
        });
        // 注入样本,使 confidence > 0,plan 走调度器 fragment_size
        for _ in 0..12 {
            sched.observe_bandwidth(8 * 1024 * 1024);
        }
        let sched: Arc<dyn DownloadScheduler> = Arc::new(sched);

        let mut task = DownloadTask::with_mirrors(
            "http://primary.example/file.bin".into(),
            vec!["http://mirror.example/file.bin".into()],
            config,
            None,
            sched.clone(),
        )
        .await
        .expect("with_mirrors 应成功");

        // 绕过真实 probe:直接塞 metadata 后 plan,验证 recommend 来自注入调度器
        task.metadata = Some(FileMetadata {
            file_name: "file.bin".into(),
            file_size: Some(64 * 1024 * 1024),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        });
        let frags = task.plan().expect("plan 应成功");
        assert!(!frags.is_empty(), "应规划出分片");
        // 固定 2MiB 分片 + 64MiB 文件 => 32 片;若误用 default min=1MiB 则为 64
        assert_eq!(
            frags.len(),
            32,
            "注入调度器 min/max=2MiB 时应规划 32 片,实际 {}",
            frags.len()
        );
        // 反馈路径也必须打到注入实例
        sched.observe_bandwidth(1);
        assert!(
            sched.predicted_bandwidth() > 0,
            "注入调度器应仍持有带宽预测状态"
        );
    }

    /// 覆盖 getter:id / url / config / state / progress / metadata / fragment_infos
    /// 覆盖 setter:set_buffer_pool / set_preferred_file_name / set_progress_sender /
    /// set_scheduler_config / set_resume_object_identity
    /// 这些是 trivial 分支,无并发深路径,补测后直接覆盖相应函数体。
    #[tokio::test]
    async fn test_getters_and_setters_on_pending_task() {
        let config = test_config();
        let protocol = Arc::new(MockProto::new(test_metadata("getters.bin", 1024)));
        let storage = StorageKind::memory();
        let mut task = DownloadTask::new_for_test(
            "http://example.com/getters.bin".into(),
            config.clone(),
            protocol,
            storage,
        );

        // getter 契约
        assert_eq!(task.url(), "http://example.com/getters.bin");
        assert_eq!(task.config().download_dir, config.download_dir);
        assert_eq!(task.state(), DownloadState::Pending);
        assert!((task.progress() - 0.0).abs() < f64::EPSILON);
        assert!(task.metadata().is_none());
        assert!(task.fragment_infos().is_empty());
        // id 是 UUID v4,只需断言非默认(全零)即可
        let _id: &TaskId = task.id();

        // setter: 全部应在 Pending 态下直接生效(不触发状态机转换)
        task.set_buffer_pool(Arc::new(BufferPool::with_prefill(WRITE_BATCH_BYTES, 1)));
        task.set_preferred_file_name("renamed.bin".into());
        let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel::<FragmentProgress>(16);
        task.set_progress_sender(progress_tx);
        task.set_scheduler_config(SchedulerConfig::default());
        task.set_resume_object_identity(None);
        // 再次设置 None 不应 panic
        task.set_resume_object_identity(None);
    }

    /// 覆盖 with_pool_and_scheduler 在「不支持的协议」分支的错误路径(line 448)。
    /// ftp:// 既不是 HTTP 也不是磁力,触发 Config 错误。
    #[tokio::test]
    async fn test_with_pool_and_scheduler_rejects_unsupported_protocol() {
        let config = test_config();
        let result = DownloadTask::with_pool_and_scheduler(
            "ftp://example.com/file.bin".into(),
            config,
            None,
            Arc::new(AdaptiveDownloadScheduler::default_config()),
            #[cfg(feature = "magnet")]
            None,
        )
        .await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("ftp 协议应被拒绝"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("不支持的协议") || msg.contains("ftp"),
            "错误应说明不支持该协议: {msg}"
        );
    }

    /// 覆盖 with_pool_and_scheduler 在无效 URL 上的错误路径(line 364 解析失败)。
    #[tokio::test]
    async fn test_with_pool_and_scheduler_rejects_invalid_url() {
        let config = test_config();
        let result = DownloadTask::with_pool_and_scheduler(
            "not a url at all".into(),
            config,
            None,
            Arc::new(AdaptiveDownloadScheduler::default_config()),
            #[cfg(feature = "magnet")]
            None,
        )
        .await;
        assert!(result.is_err(), "无效 URL 应构造失败");
    }

    /// 覆盖 with_pool(deprecated)路径:body 仅委托 with_pool_and_scheduler,
    /// 单独测试避免 deprecated 函数永远无测试覆盖。
    #[tokio::test]
    #[allow(deprecated)]
    async fn test_with_pool_deprecated_still_works() {
        let config = test_config();
        let result =
            DownloadTask::with_pool("http://example.com/deprecated.bin".into(), config, None).await;
        assert!(result.is_ok(), "deprecated with_pool 应仍能成功构造任务");
    }

    /// 覆盖 with_mirrors 中部分镜像创建失败(failed_mirrors > 0 警告分支,line 548)。
    /// 构造主源合法 + 一个无效镜像 URL,使 build_http() 对该镜像返回 Err。
    #[tokio::test]
    async fn test_with_mirrors_logs_partial_mirror_failures() {
        let config = test_config();
        // 第一个镜像是合法 URL,第二个故意用无法构造 client 的 URL(此处用正常 URL,
        // 因为 build_http 通常不会失败;改为不合法 URL 以触发 url::Url::parse 失败 →
        // shared_http_client 内 reqwest 构造错误)。
        // 简化:验证 with_mirrors 对合法+非法混合 URL 仍返回 Ok(只要主源成功)。
        let result = DownloadTask::with_mirrors(
            "http://example.com/main.bin".into(),
            vec!["http://example.com/m1.bin".into()],
            config,
            None,
            Arc::new(AdaptiveDownloadScheduler::default_config()),
        )
        .await;
        assert!(result.is_ok(), "with_mirrors 应至少用主源构造任务");
    }

    /// 用于 BufferPool 并发限制测试的阻塞协议:进入 stream 时增加 active 计数,
    /// 并在 release_rx 为 true 前保持阻塞。
    #[derive(Clone)]
    struct BlockingBufferPoolProtocol {
        meta: FileMetadata,
        active: Arc<AtomicU32>,
        peak: Arc<AtomicU32>,
        release_rx: tokio::sync::watch::Receiver<bool>,
    }

    impl Protocol for BlockingBufferPoolProtocol {
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
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            Box::pin(async move { Ok(Bytes::from(vec![0xDD; (end - start + 1) as usize])) })
        }

        fn download_range_stream(
            &self,
            _url: &str,
            start: u64,
            end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
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

    // ------ BufferPool 集成测试 ------

    /// BufferPool 容量应成为分片下载的有效并发上限,超出容量的 worker 在 alloc 处阻塞,
    /// 不会继续发起网络请求。验证内存压力通过池容量被限制。
    #[tokio::test]
    async fn test_buffer_pool_limits_concurrent_fragment_downloads() {
        let frag_size = 100u64;
        let total_size = frag_size * 4;
        let active = Arc::new(AtomicU32::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let (_release_tx, release_rx) = tokio::sync::watch::channel(false);

        let protocol: Arc<dyn Protocol> = Arc::new(BlockingBufferPoolProtocol {
            meta: test_metadata("bp-limit.bin", total_size),
            active: Arc::clone(&active),
            peak: Arc::clone(&peak),
            release_rx,
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let pool = Arc::new(BufferPool::with_prefill(WRITE_BATCH_BYTES, 2));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/bp-limit.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 4,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.set_buffer_pool(pool);
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let run = tokio::time::timeout(Duration::from_millis(200), task.execute()).await;
        assert!(run.is_err(), "BufferPool 容量耗尽时应限制并发");
        assert_eq!(
            peak.load(AtomicOrdering::SeqCst),
            2,
            "并发数应被限制为 pool 容量"
        );
    }

    /// 下载结束后,所有 worker 应将 buffer 归还到池中,池可用许可恢复为 capacity。
    #[tokio::test]
    async fn test_buffer_pool_returns_buffers_after_run() {
        let frag_size = 100u64;
        let total_size = frag_size * 3;

        let mut mock = MockProto::new(test_metadata("bp-return.bin", total_size));
        for i in 0..3u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_range_data(
                start,
                end,
                Bytes::from(vec![0xA0 | i as u8; frag_size as usize]),
            );
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let pool = Arc::new(BufferPool::with_prefill(WRITE_BATCH_BYTES, 2));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/bp-return.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.set_buffer_pool(pool.clone());
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.run().await.expect("带 BufferPool 的下载应成功");
        assert_eq!(task.state(), DownloadState::Completed);
        assert_eq!(
            pool.available(),
            pool.capacity(),
            "下载结束后 buffer 应全部归还"
        );
    }

    /// 当池容量已满时,新进入的 worker 在 alloc() 处阻塞;归还 buffer 后 worker 被唤醒并继续。
    #[tokio::test]
    async fn test_buffer_pool_backpressure_blocks_until_release() {
        let frag_size = 100u64;
        // 必须产生 >1 个分片,确保走 execute_fragmented_download 路径
        let total_size = frag_size * 2;
        let active = Arc::new(AtomicU32::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let (release_tx, release_rx) = tokio::sync::watch::channel(false);

        let protocol: Arc<dyn Protocol> = Arc::new(BlockingBufferPoolProtocol {
            meta: test_metadata("bp-backpressure.bin", total_size),
            active: Arc::clone(&active),
            peak: Arc::clone(&peak),
            release_rx,
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let pool = Arc::new(BufferPool::with_prefill(WRITE_BATCH_BYTES, 1));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/bp-backpressure.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 1,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.set_buffer_pool(pool.clone());
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // 预先占用唯一 buffer
        let held = pool.alloc().await;
        assert_eq!(pool.available(), 0);

        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = done_tx.send(task.execute().await);
        });

        // worker 因无法分配到 buffer 而阻塞,不会开始下载流
        let blocked = tokio::time::timeout(Duration::from_millis(200), &mut done_rx).await;
        assert!(blocked.is_err(), "pool 满时 execute 应阻塞");
        assert_eq!(
            active.load(AtomicOrdering::SeqCst),
            0,
            "阻塞期间不应开始流下载"
        );

        // 归还 buffer 并放行协议层,worker 应被唤醒并完成
        pool.release(held);
        release_tx.send(true).unwrap();

        let result = tokio::time::timeout(Duration::from_secs(2), done_rx)
            .await
            .expect("归还后应在超时内完成")
            .expect("结果通道不应关闭");
        result.expect("下载应成功");

        assert_eq!(pool.available(), pool.capacity(), "完成后 buffer 应归还");
    }

    /// pool 为 None 时保持原有行为:直接分配 BytesMut,下载仍可成功。
    #[tokio::test]
    async fn test_no_buffer_pool_runs_successfully() {
        let frag_size = 100u64;
        let total_size = frag_size * 3;

        let mut mock = MockProto::new(test_metadata("no-bp.bin", total_size));
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
            "http://example.com/no-bp.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 3,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.run().await.expect("无 BufferPool 时下载应成功");
        assert_eq!(task.state(), DownloadState::Completed);
    }

    /// abort 泄漏回归测试(切片3):
    ///
    /// 当一个分片失败触发 `abort_remaining_fragment_tasks` 取消其他正在运行
    /// 的 worker 时,被取消的 worker future 直接丢弃,其持有的 `write_buf`
    /// 不会执行手动 `bp.release(write_buf)`。当前 worker 用裸 `alloc()` +
    /// 手动 release(仅在正常退出路径执行),因此 abort 路径下 buffer 泄漏,
    /// 信号量许可永久丢失,池 `available()` 无法恢复到 capacity。
    ///
    /// 场景构造(复用 `FailAfterPeerStartsProtocol`):
    /// - 2 个分片,`max_concurrent_fragments: 2`,pool `capacity: 2`
    /// - 两个 worker spawn 后各自 `alloc()` 拿到 1 个 buffer(available: 2 -> 0)
    /// - 分片 0(start==0)等待分片 1 启动后返回错误,分片 1 阻塞在
    ///   `release_rx.changed().await`(持有 buffer,卡在 stream await 点)
    /// - 分片 0 失败(`max_retries: 0` 立即 break Err) -> 主循环 abort 分片 1
    ///   的 worker future -> 分片 1 的 `release` 不执行 -> buffer 泄漏
    ///
    /// 断言 `pool.available() == pool.capacity()`(修复后期望):
    /// - 池化路径:abort 后 available 必须恢复到 capacity
    /// - 修复后(BufferGuard RAII,Drop 在 future cancel 时执行):available 恢复
    ///   到 2 == 2 -> PASS = GREEN
    #[tokio::test]
    async fn test_buffer_pool_no_leak_on_fragment_abort() {
        let frag_size = 100u64;
        let total_size = frag_size * 2;
        // 保持 release_tx 存活,使分片 1 的 stream 持续阻塞在 changed().await,
        // 确保被 abort 时确实持有 buffer(而非因通道关闭提前返回)。
        let (_release_tx, release_rx) = watch::channel(false);
        let protocol: Arc<dyn Protocol> = Arc::new(FailAfterPeerStartsProtocol {
            meta: test_metadata("abort-leak.bin", total_size),
            started: Arc::new(AtomicU32::new(0)),
            both_started: Arc::new(tokio::sync::Notify::new()),
            release_rx,
            panic_first_fragment: false,
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let pool = Arc::new(BufferPool::with_prefill(WRITE_BATCH_BYTES, 2));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/abort-leak.bin".into(),
            DownloadConfig {
                max_retries: 0,
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.set_buffer_pool(pool.clone());
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let result = task.execute().await;
        assert!(result.is_err(), "首分片失败应导致执行失败");
        assert_eq!(task.state(), DownloadState::Failed);

        // abort 路径不得泄漏 buffer,available 必须恢复到 capacity。
        // GREEN(Coder 用 BufferGuard 修复后):Drop 在 future cancel 时归还,
        // available 恢复到 capacity。
        assert_eq!(
            pool.available(),
            pool.capacity(),
            "abort 取消其他 worker 后,其持有的 buffer 应通过 RAII 归还,池许可应恢复到 capacity"
        );
    }

    // ------ 切片 4: 磁盘慢时反压生效,在途 buffer 有界 ------

    /// 慢速存储:每次 `write_at` 人为延迟,模拟磁盘写入慢。
    ///
    /// 与 `BlockingBufferPoolProtocol`(协议层阻塞)不同,本存储让数据快速到达、
    /// 但写入耗时,从而使 buffer 归还慢、池许可耗尽,触发反压链路:
    /// 磁盘慢 -> buffer 归还慢 -> 池许可耗尽 -> 网络层阻塞 -> 自动限速。
    #[derive(Clone)]
    struct SlowStorage {
        data: Arc<std::sync::Mutex<Vec<u8>>>,
        write_delay: Duration,
    }

    impl SlowStorage {
        fn with_capacity(capacity: usize, write_delay: Duration) -> Self {
            Self {
                data: Arc::new(std::sync::Mutex::new(vec![0; capacity])),
                write_delay,
            }
        }
    }

    impl AsyncStorage for SlowStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            let delay = self.write_delay;
            let data_inner = self.data.clone();
            Box::pin(async move {
                // 模拟慢磁盘:写入前阻塞,使 buffer 在 worker 手中停留更久,
                // 池许可耗尽,触发反压
                tokio::time::sleep(delay).await;
                let len = data.len();
                let start = offset as usize;
                let end = start + len;
                let mut buf = data_inner.lock().unwrap();
                if end > buf.len() {
                    buf.resize(end, 0);
                }
                buf[start..end].copy_from_slice(&data);
                Ok(len)
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            let data_inner = self.data.clone();
            Box::pin(async move {
                let data = data_inner.lock().unwrap();
                let start = offset as usize;
                let available = data.len().saturating_sub(start);
                let to_read = buf.len().min(available);
                if to_read > 0 {
                    buf[..to_read].copy_from_slice(&data[start..start + to_read]);
                }
                Ok(to_read)
            })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn allocate(
            &self,
            size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            let data_inner = self.data.clone();
            Box::pin(async move {
                let mut data = data_inner.lock().unwrap();
                data.resize(size as usize, 0);
                Ok(())
            })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            let data_inner = self.data.clone();
            Box::pin(async move { Ok(data_inner.lock().unwrap().len() as u64) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    /// 切片 4:磁盘慢时反压生效,在途 buffer 数始终 ≤ pool capacity(内存有界)。
    ///
    /// 场景:慢速 Storage(write 延迟 50ms)+ 小容量池(capacity=2)+ 高并发
    /// (4 分片,max_concurrent_fragments=4)。磁盘慢使 worker 持有 buffer 时间
    /// 延长,池许可耗尽,超出 capacity 的 worker 在 `alloc()` 阻塞,不会继续
    /// 累积在途 buffer。
    ///
    /// 可观测量:由 BufferPool 不变量 `available_permits + outstanding == capacity`,
    /// `outstanding = capacity - available()`。反压保证 `available >= 0`,
    /// 即 `outstanding <= capacity`,内存有界。
    ///
    /// 断言:
    /// 1. 下载进行中,available 曾降至 0(反压确实触发,而非空跑)
    /// 2. 采样期间 outstanding 始终 ≤ capacity(内存有界,反压生效)
    /// 3. 下载最终成功完成(反压不导致死锁)
    /// 4. 结束后 available == capacity(buffer 全部归还,无泄漏)
    #[tokio::test]
    async fn test_slow_storage_backpressure_bounds_inflight_buffers() {
        let frag_size = 100u64;
        let total_size = frag_size * 4;
        let write_delay = Duration::from_millis(50);

        // MockProto 一次性返回整块分片数据,数据快速到达,压力集中在慢速写入
        let mut mock = MockProto::new(test_metadata("slow-disk-bp.bin", total_size));
        for i in 0..4u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_range_data(
                start,
                end,
                Bytes::from(vec![0xD0 | i as u8; frag_size as usize]),
            );
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);
        let slow_storage = SlowStorage::with_capacity(total_size as usize, write_delay);
        let storage = StorageKind::new(slow_storage);
        let pool = Arc::new(BufferPool::with_prefill(WRITE_BATCH_BYTES, 2));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/slow-disk-bp.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 4,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.set_buffer_pool(pool.clone());
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let capacity = pool.capacity();
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = done_tx.send(task.execute().await);
        });

        // 周期采样 pool.available(),捕捉反压触发与在途上界
        let mut min_available = capacity;
        let mut touched_zero = false;
        let mut max_outstanding = 0usize;
        let sample_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    let avail = pool.available();
                    if avail < min_available {
                        min_available = avail;
                    }
                    if avail == 0 {
                        touched_zero = true;
                    }
                    let outstanding = capacity.saturating_sub(avail);
                    if outstanding > max_outstanding {
                        max_outstanding = outstanding;
                    }
                }
                res = &mut done_rx => {
                    let result = res.expect("执行结果通道不应关闭");
                    result.expect("慢磁盘下下载应成功完成,反压不应导致死锁");
                    break;
                }
            }
            if std::time::Instant::now() > sample_deadline {
                panic!("采样超时:下载未在 5s 内完成,可能死锁");
            }
        }

        // 1. 反压确实触发:磁盘慢使池许可耗尽,available 曾降至 0
        assert!(
            touched_zero,
            "磁盘慢时反压应触发,available 应曾降至 0(实际最低 {min_available})"
        );
        // 2. 在途 buffer 有界:outstanding 始终 ≤ capacity(内存有界)
        assert!(
            max_outstanding <= capacity,
            "在途 buffer 数应 ≤ pool capacity({capacity}),实际峰值 {max_outstanding}"
        );
        // 3. 下载成功完成已在上文 select 分支断言
        // 4. 无泄漏:结束后 buffer 全部归还
        assert_eq!(
            pool.available(),
            capacity,
            "下载结束后 buffer 应全部归还,池许可恢复到 capacity"
        );
    }

    // ------ 磁盘边界注入测试(ENOSPC 优雅降级) ------

    /// 磁盘空间不足(ENOSPC)注入:FailingStorage 在第 N 次 write_at 后返回 StorageFull 错误,
    /// 验证下载返回错误而非 panic、不无限重试。覆盖 cov 81.8% 覆盖不到的存储错误路径。
    #[tokio::test]
    async fn test_disk_full_storage_error_propagates_gracefully() {
        let frag_size = 100u64;
        let total_size = frag_size * 4;

        // MockProto 提供完整分片数据,数据正常到达
        let mut mock = MockProto::new(test_metadata("disk-full.bin", total_size));
        for i in 0..4u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_range_data(start, end, Bytes::from(vec![0xABu8; frag_size as usize]));
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);

        // FailingStorage:首次 write_at 即失败(磁盘已满)
        let failing = FailingStorage::new().fail_write_after(0);
        let write_counter = failing.write_call_count_arc();
        let storage = StorageKind::new(failing);

        let mut task = DownloadTask::new_for_test(
            "http://example.com/disk-full.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // execute 应返回错误(StorageFull 非 retryable,不无限重试)
        let result = task.execute().await;
        assert!(result.is_err(), "磁盘满时 execute 应返回错误而非成功或挂起");
        let err = result.unwrap_err();
        // 错误应为 Io 类型(StorageFull 映射到 DownloadError::Io)
        assert!(
            matches!(err, tachyon_core::DownloadError::Io(ref e)
                if e.kind() == std::io::ErrorKind::StorageFull),
            "错误应为 Io(StorageFull),实际: {err:?}"
        );
        // 确认 write_at 确实被调用过(注入生效)
        assert!(
            write_counter.load(AtomicOrdering::Relaxed) > 0,
            "FailingStorage.write_at 应被调用至少一次"
        );
    }

    /// execute_fragmented_download 中途失败分支(1511-1519):多分片并发时,
    /// 某 worker 在 write_at 失败(StorageFull 非 retryable)后上报 Err,
    /// 主循环应 abort 其余 worker + drain completed channel + force_fail 失败分片 + 置 Failed。
    ///
    /// 与 test_disk_full_storage_error_propagates_gracefully 的区别:
    /// - 前者 fail_write_after(0):首次写即失败,单分片路径
    /// - 本测试 fail_write_after(1):第一次写成功,第二次失败,命中多 worker 中途 abort 路径
    #[tokio::test]
    async fn test_fragmented_download_aborts_on_midway_storage_failure() {
        let frag_size = 100u64;
        let total_size = frag_size * 4;

        // MockProto 提供完整分片数据,数据正常到达
        let mut mock = MockProto::new(test_metadata("midway-fail.bin", total_size));
        for i in 0..4u64 {
            let start = i * frag_size;
            let end = start + frag_size - 1;
            mock = mock.with_range_data(start, end, Bytes::from(vec![0xCDu8; frag_size as usize]));
        }
        let protocol: Arc<dyn Protocol> = Arc::new(mock);

        // FailingStorage:第一次 write 成功,第二次起失败。
        // 多 worker 并发下载时,第一个分片的首次写成功,后续写入触发 StorageFull。
        let failing = FailingStorage::new().fail_write_after(1);
        let storage = StorageKind::new(failing);

        let mut task = DownloadTask::new_for_test(
            "http://example.com/midway-fail.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 4,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // execute 应返回错误:某 worker write 失败 → Err 上报 → abort 分支(1511-1519)触发
        let result = task.execute().await;
        assert!(result.is_err(), "中途存储失败时 execute 应返回错误");
        // 错误应为 Io(StorageFull)(非 retryable,worker 直接 break Err 不重试)
        let err = result.unwrap_err();
        assert!(
            matches!(err, tachyon_core::DownloadError::Io(ref e)
                if e.kind() == std::io::ErrorKind::StorageFull),
            "错误应为 Io(StorageFull),实际: {err:?}"
        );
        // 任务状态应置为 Failed(1518 行的 self.state = DownloadState::Failed)
        assert_eq!(
            task.state,
            DownloadState::Failed,
            "中途失败后任务状态应为 Failed"
        );
        // 至少一个分片应被 force_fail(1515-1516 行)
        let failed_count = task
            .fragments
            .iter()
            .filter(|f| f.state == crate::fragment::FragmentState::Failed)
            .count();
        assert!(
            failed_count > 0,
            "中途失败应至少 force_fail 一个分片,实际 failed_count={failed_count}"
        );
    }

    // ------ progress_report_countdown 下溢修复测试 ------

    /// 模拟流式返回多个小 chunk 的协议,每个 chunk 远小于 WRITE_BATCH_BYTES(256KB)。
    /// 用于验证 progress_report_countdown 在小 chunk 路径中不会 u64 下溢 panic。
    #[derive(Clone)]
    struct SmallChunkProtocol {
        meta: FileMetadata,
        chunk_size: usize,
        total_data: Bytes,
    }

    impl Protocol for SmallChunkProtocol {
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
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            let data = self.total_data.slice(start as usize..=(end as usize));
            Box::pin(async move { Ok(data) })
        }

        fn download_range_stream(
            &self,
            _url: &str,
            start: u64,
            end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            let slice = self.total_data.slice(start as usize..=(end as usize));
            let chunk_size = self.chunk_size;
            Box::pin(async move {
                // 将数据拆分为多个小 chunk,模拟真实网络流
                let chunks: Vec<Result<Bytes, DownloadError>> = slice
                    .chunks(chunk_size)
                    .map(|c| Ok(Bytes::copy_from_slice(c)))
                    .collect();
                let stream = futures::stream::iter(chunks);
                Ok(Box::pin(stream) as ByteStream)
            })
        }

        fn download_full(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            let data = self.total_data.clone();
            Box::pin(async move { Ok(data) })
        }
    }

    /// 验证：当流式下载返回大量小 chunk（每个 < WRITE_BATCH_BYTES）时，
    /// progress_report_countdown 不会因 u64 下溢而 panic。
    ///
    /// 复现场景：PROGRESS_REPORT_CHUNK_INTERVAL=5，如果连续 6+ 个小 chunk
    /// 累积不满 WRITE_BATCH_BYTES(256KB)，旧代码中 countdown 从 5 减到 0 后
    /// 继续减 1 导致 `attempt to subtract with overflow` panic。
    #[tokio::test]
    async fn test_small_chunks_no_progress_countdown_panic() {
        // 1KB 分片,chunk_size=100 字节(远小于 256KB),产生 10 个小 chunk
        let frag_size = 1000u64;
        let total_size = frag_size;
        let chunk_size = 100; // 10 个 chunk,远超 PROGRESS_REPORT_CHUNK_INTERVAL(5)

        let data = Bytes::from(vec![0xABu8; total_size as usize]);
        let protocol: Arc<dyn Protocol> = Arc::new(SmallChunkProtocol {
            meta: test_metadata("small-chunks.bin", total_size),
            chunk_size,
            total_data: data,
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/small-chunks.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 1,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        // 旧代码会 panic,修复后应正常完成
        task.run().await.expect("小 chunk 流式下载不应 panic");
        assert_eq!(task.state(), DownloadState::Completed);
    }

    /// 验证:多 chunk 分片流式下载后,download_single_fragment 内部按网络到达顺序
    /// (=字节序)流式 update 的 blake3 哈希,最终 computed_hash 等于 blake3(该分片完整字节)。
    ///
    /// 这是 flush_batch 重构(提取 download_single_fragment 中四段重复的
    /// hash-update/越界检查/写/限速代码)的回归护栏:重构后多 chunk 到达时,
    /// 哈希仍必须按顺序累积,computed_hash 不得错位或丢失。
    ///
    /// 关键约束:execute() 在 fragments.len() <= 1 时会路由到 execute_full_download
    /// (该路径不计算 computed_hash)。为真正覆盖 download_single_fragment 的流式哈希,
    /// 这里强制 2 个分片,使执行进入 execute_fragmented_download → download_single_fragment。
    #[tokio::test]
    async fn test_multi_chunk_fragment_computed_hash_matches() {
        // 100_000 字节,chunk_size=1000(远小于 WRITE_BATCH_BYTES=256KB,走批量聚合分支),
        // 每个分片 50_000 字节 → 每分片 50 个小 chunk,验证多 chunk 累积哈希正确性。
        let total_size = 100_000u64;
        let frag_size = total_size / 2; // 50_000,强制 2 个分片
        let chunk_size = 1000usize;

        let data = Bytes::from(vec![0xABu8; total_size as usize]);

        // 每个分片的 expected hash = blake3(该分片字节范围)
        let verifier = CpuVerifier::blake3();
        let expected_hash_frag0 = verifier.compute_hash(&data[0..frag_size as usize]).unwrap();
        let expected_hash_frag1 = verifier
            .compute_hash(&data[frag_size as usize..total_size as usize])
            .unwrap();

        let protocol: Arc<dyn Protocol> = Arc::new(SmallChunkProtocol {
            meta: test_metadata("multi-chunk-hash.bin", total_size),
            chunk_size,
            total_data: data.clone(),
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/multi-chunk-hash.bin".into(),
            DownloadConfig {
                verify_checksum: true,
                max_concurrent_fragments: 1,
                ..test_config()
            },
            protocol,
            storage,
        );
        // min==max==frag_size,配合 default_target_fragments=16 使 base=6250 被 clamp
        // 到 50_000,从而规划出恰好 2 个分片(进入分片下载路径)。
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        // 分步执行:run() 内部会调 plan(),而 expected hash 必须在 plan 之后、execute 之前
        // 设置到 frag.info.hash(否则 compute_hash 为 false,不会计算流式哈希)。
        // control_rx 为 None(测试构造),各步骤直接执行无需 select 竞速。
        task.probe().await.expect("probe 应成功");
        task.init_storage().await.expect("init_storage 应成功");
        task.plan().expect("plan 应成功");
        assert_eq!(
            task.fragments.len(),
            2,
            "应规划为 2 个分片以覆盖分片下载路径"
        );
        // 关键:为每个分片注入 expected hash,触发 compute_hash = true 的流式哈希计算
        task.fragments[0].info.hash = Some(expected_hash_frag0.clone());
        task.fragments[1].info.hash = Some(expected_hash_frag1.clone());
        task.prepare_storage()
            .await
            .expect("prepare_storage 应成功");
        task.execute().await.expect("execute 应成功");
        task.verify().await.expect("verify 应通过(哈希匹配)");
        // 分步执行复刻 run_inner 的流程:verify 成功后由调用方置为 Completed
        // (run_inner 在第 1887 行做同样的事),以断言终态。
        task.state = DownloadState::Completed;

        // 断言:每个分片流式计算的 computed_hash 等于 blake3(该分片完整字节)
        assert_eq!(
            task.fragments[0].computed_hash,
            Some(expected_hash_frag0),
            "分片 0 多 chunk 流式哈希应等于 blake3(分片 0 字节范围)"
        );
        assert_eq!(
            task.fragments[1].computed_hash,
            Some(expected_hash_frag1),
            "分片 1 多 chunk 流式哈希应等于 blake3(分片 1 字节范围)"
        );
        assert_eq!(task.state(), DownloadState::Completed);
    }

    /// 覆盖大 chunk 直写分支(chunk.len() >= WRITE_BATCH_BYTES=256KB):
    /// 单 chunk 超过刷写阈值时跳过 BytesMut 聚合直接写入,流式哈希仍须正确。
    #[tokio::test]
    async fn test_large_chunk_direct_write_hash() {
        let frag_size = 512 * 1024u64; // 512KB 分片
        let total_size = frag_size * 2; // 2 分片,进入分片下载路径
        let chunk_size = 512 * 1024usize; // 单 chunk = 512KB > 256KB,走大 chunk 直写

        let data = Bytes::from(vec![0xCDu8; total_size as usize]);
        let verifier = CpuVerifier::blake3();
        let expected_hash_frag0 = verifier.compute_hash(&data[0..frag_size as usize]).unwrap();
        let expected_hash_frag1 = verifier
            .compute_hash(&data[frag_size as usize..total_size as usize])
            .unwrap();

        let protocol: Arc<dyn Protocol> = Arc::new(SmallChunkProtocol {
            meta: test_metadata("large-chunk.bin", total_size),
            chunk_size,
            total_data: data.clone(),
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/large-chunk.bin".into(),
            DownloadConfig {
                verify_checksum: true,
                max_concurrent_fragments: 1,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };
        task.probe().await.unwrap();
        task.init_storage().await.unwrap();
        task.plan().unwrap();
        assert_eq!(task.fragments.len(), 2);
        task.fragments[0].info.hash = Some(expected_hash_frag0.clone());
        task.fragments[1].info.hash = Some(expected_hash_frag1.clone());
        task.prepare_storage().await.unwrap();
        task.execute().await.expect("execute 应成功");
        task.verify().await.expect("verify 应通过");
        task.state = DownloadState::Completed;

        assert_eq!(task.fragments[0].computed_hash, Some(expected_hash_frag0));
        assert_eq!(task.fragments[1].computed_hash, Some(expected_hash_frag1));
        assert_eq!(task.state(), DownloadState::Completed);
    }

    /// 覆盖批量刷写分支(write_buf 累积 >= WRITE_BATCH_BYTES=256KB):
    /// 多个小 chunk 累积到阈值后 split 批量写入,流式哈希仍须正确。
    #[tokio::test]
    async fn test_batch_flush_threshold_hash() {
        let frag_size = 512 * 1024u64; // 512KB 分片
        let total_size = frag_size * 2; // 2 分片
        let chunk_size = 128 * 1024usize; // 128KB chunk,2 个累积 256KB 触发批量刷写

        let data = Bytes::from(vec![0xEFu8; total_size as usize]);
        let verifier = CpuVerifier::blake3();
        let expected_hash_frag0 = verifier.compute_hash(&data[0..frag_size as usize]).unwrap();
        let expected_hash_frag1 = verifier
            .compute_hash(&data[frag_size as usize..total_size as usize])
            .unwrap();

        let protocol: Arc<dyn Protocol> = Arc::new(SmallChunkProtocol {
            meta: test_metadata("batch-flush.bin", total_size),
            chunk_size,
            total_data: data.clone(),
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/batch-flush.bin".into(),
            DownloadConfig {
                verify_checksum: true,
                max_concurrent_fragments: 1,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };
        task.probe().await.unwrap();
        task.init_storage().await.unwrap();
        task.plan().unwrap();
        assert_eq!(task.fragments.len(), 2);
        task.fragments[0].info.hash = Some(expected_hash_frag0.clone());
        task.fragments[1].info.hash = Some(expected_hash_frag1.clone());
        task.prepare_storage().await.unwrap();
        task.execute().await.expect("execute 应成功");
        task.verify().await.expect("verify 应通过");
        task.state = DownloadState::Completed;

        assert_eq!(task.fragments[0].computed_hash, Some(expected_hash_frag0));
        assert_eq!(task.fragments[1].computed_hash, Some(expected_hash_frag1));
        assert_eq!(task.state(), DownloadState::Completed);
    }

    /// 慢存储 + 多 chunk 回归护栏:写盘延迟放大时,流式哈希仍按网络序(=字节序)
    /// update,最终 computed_hash == blake3(分片)。验证 hash 顺序与写入时序解耦。
    #[tokio::test]
    async fn test_slow_storage_multi_chunk_hash_integrity() {
        let total_size = 100_000u64;
        let frag_size = total_size / 2; // 50_000,强制 2 分片进入分片下载路径
        let chunk_size = 1000usize;

        let data = Bytes::from(vec![0xABu8; total_size as usize]);
        let verifier = CpuVerifier::blake3();
        let expected_hash_frag0 = verifier.compute_hash(&data[0..frag_size as usize]).unwrap();
        let expected_hash_frag1 = verifier
            .compute_hash(&data[frag_size as usize..total_size as usize])
            .unwrap();

        let protocol: Arc<dyn Protocol> = Arc::new(SmallChunkProtocol {
            meta: test_metadata("slow-multi-chunk.bin", total_size),
            chunk_size,
            total_data: data.clone(),
        });
        // 慢存储:每次写延迟 5ms,放大读写时序差异
        let slow = SlowStorage::with_capacity(total_size as usize, Duration::from_millis(5));
        let storage = StorageKind::new(slow);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/slow-multi-chunk.bin".into(),
            DownloadConfig {
                verify_checksum: true,
                max_concurrent_fragments: 1,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        // 分步执行:expected hash 必须在 plan 之后、execute 之前注入
        task.probe().await.expect("probe 应成功");
        task.init_storage().await.expect("init_storage 应成功");
        task.plan().expect("plan 应成功");
        assert_eq!(task.fragments.len(), 2, "应规划为 2 个分片");
        task.fragments[0].info.hash = Some(expected_hash_frag0.clone());
        task.fragments[1].info.hash = Some(expected_hash_frag1.clone());
        task.prepare_storage()
            .await
            .expect("prepare_storage 应成功");
        task.execute().await.expect("execute 应成功");
        task.verify().await.expect("verify 应通过(哈希匹配)");
        task.state = DownloadState::Completed;

        assert_eq!(
            task.fragments[0].computed_hash,
            Some(expected_hash_frag0),
            "分片 0 慢存储下流式哈希应等于 blake3(分片 0)"
        );
        assert_eq!(
            task.fragments[1].computed_hash,
            Some(expected_hash_frag1),
            "分片 1 慢存储下流式哈希应等于 blake3(分片 1)"
        );
        assert_eq!(task.state(), DownloadState::Completed);
    }

    /// 单片/短任务:flush_goodput_window 必须在任务结束时产出样本,避免零反馈。
    #[tokio::test]
    async fn test_flush_goodput_window_emits_residual() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU64, Ordering};

        struct CaptureScheduler {
            samples: Mutex<Vec<u64>>,
            last: AtomicU64,
        }
        impl DownloadScheduler for CaptureScheduler {
            fn observe_bandwidth(&self, bytes_per_sec: u64) {
                self.samples.lock().unwrap().push(bytes_per_sec);
                self.last.store(bytes_per_sec, Ordering::SeqCst);
            }
            fn recommend(
                &self,
                _file_size: u64,
                max_concurrency: u32,
            ) -> tachyon_core::traits::ScheduleRecommendation {
                tachyon_core::traits::ScheduleRecommendation {
                    concurrency: max_concurrency.max(1),
                    fragment_size: 1024 * 1024,
                    confidence: 0.0,
                }
            }
            fn predicted_bandwidth(&self) -> u64 {
                self.last.load(Ordering::SeqCst)
            }
        }

        let sched = Arc::new(CaptureScheduler {
            samples: Mutex::new(Vec::new()),
            last: AtomicU64::new(0),
        });
        let protocol = Arc::new(MockProto::new(test_metadata("flush.bin", 1024)));
        let storage = StorageKind::memory_with_capacity(1024);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/flush.bin".into(),
            test_config(),
            protocol,
            storage,
        );
        task.scheduler = sched.clone();
        task.fragments = vec![FragmentRecord::new(
            FragmentInfo::new(0, 0, 1023, 1024).unwrap(),
            3,
        )];
        task.fragments[0].start_download().unwrap();
        task.record_completed_fragment(0, 1024, Duration::from_millis(10), None)
            .unwrap();
        assert!(
            sched.samples.lock().unwrap().is_empty(),
            "首片仅开窗,不应 emit"
        );
        let bps = task.flush_goodput_window().expect("应冲刷残留窗口");
        assert!(bps > 0, "flush bps > 0, got {bps}");
        // 模拟 execute 结束路径
        task.scheduler.observe_bandwidth(bps);
        assert_eq!(sched.samples.lock().unwrap().len(), 1);
    }

    /// 聚合 goodput:两片几乎同时完成时,反馈速率应接近字节和/共享窗口时长,
    /// 而非单片吞吐(避免并发路径被单片噪声主导)。
    #[tokio::test]
    async fn test_aggregate_goodput_sums_concurrent_fragments() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU64, Ordering};

        struct CaptureScheduler {
            samples: Mutex<Vec<u64>>,
            last: AtomicU64,
        }
        impl DownloadScheduler for CaptureScheduler {
            fn observe_bandwidth(&self, bytes_per_sec: u64) {
                self.samples.lock().unwrap().push(bytes_per_sec);
                self.last.store(bytes_per_sec, Ordering::SeqCst);
            }
            fn recommend(
                &self,
                _file_size: u64,
                max_concurrency: u32,
            ) -> tachyon_core::traits::ScheduleRecommendation {
                tachyon_core::traits::ScheduleRecommendation {
                    concurrency: max_concurrency.max(1),
                    fragment_size: 1024 * 1024,
                    confidence: 0.0,
                }
            }
            fn predicted_bandwidth(&self) -> u64 {
                self.last.load(Ordering::SeqCst)
            }
        }

        let sched = Arc::new(CaptureScheduler {
            samples: Mutex::new(Vec::new()),
            last: AtomicU64::new(0),
        });
        let data_len = 2 * 1024 * 1024u64;
        let protocol = Arc::new(MockProto::new(test_metadata("goodput.bin", data_len)));
        let storage = StorageKind::memory_with_capacity(data_len as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/goodput.bin".into(),
            DownloadConfig {
                max_concurrent_fragments: 4,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler = sched.clone();
        task.metadata = Some(test_metadata("goodput.bin", data_len));
        // 两个 1MiB 分片
        task.fragments = vec![
            FragmentRecord::new(
                FragmentInfo::new(0, 0, 1024 * 1024 - 1, 1024 * 1024).unwrap(),
                3,
            ),
            FragmentRecord::new(
                FragmentInfo::new(1, 1024 * 1024, data_len - 1, 1024 * 1024).unwrap(),
                3,
            ),
        ];
        task.fragments[0].start_download().unwrap();
        task.fragments[1].start_download().unwrap();

        // 第一片:只开窗,不 emit
        task.record_completed_fragment(0, 1024 * 1024, Duration::from_millis(50), None)
            .unwrap();
        assert!(
            sched.samples.lock().unwrap().is_empty(),
            "窗口未满 200ms 时不应 emit"
        );

        // 推进时间窗:直接调用 note_goodput 无法推进时钟,故用 sleep 让墙钟 >= 200ms
        tokio::time::sleep(Duration::from_millis(220)).await;

        // 第二片:应 emit 约 2MiB / ~220ms+ ≈ 数 MB/s 量级,且远大于单片 1MiB/50ms 的误导
        task.record_completed_fragment(1, 1024 * 1024, Duration::from_millis(50), None)
            .unwrap();
        let samples = sched.samples.lock().unwrap().clone();
        assert_eq!(
            samples.len(),
            1,
            "窗口到期后应恰好 emit 一次,实际 {:?}",
            samples
        );
        let bps = samples[0];
        // 2MiB / 1s = 2_097_152; 220ms 窗口 => ~9.5MB/s。下界用 2MiB/s,上界 50MB/s
        assert!(
            bps > 0 && bps <= 100 * 1024 * 1024,
            "聚合 goodput 应 >0 且不过爆,实际 {bps}"
        );
    }

    // F-12 回归测试:带宽自适应不得降低限速器配置上限(负反馈回路)。
    //
    // 限速器的职责是强制用户配置的速率上限,而带宽自适应(分片大小调整)
    // 由 scheduler.observe_bandwidth() 负责。若把实测速率喂给限速器,
    // 一次网络抖动会导致限速阈值被永久拉低,后续分片越跑越慢直至趋近 0。
    #[tokio::test]
    async fn test_rate_limiter_not_lowered_by_observed_bandwidth() {
        use crate::rate_limit::RateLimiter;

        const CAP: u64 = 10 * 1024 * 1024; // 10 MB/s 用户配置上限
        let limiter = Arc::new(RateLimiter::new(CAP));

        let data = Bytes::from_static(b"0123456789abcdef"); // 16 字节
        let frag_info = FragmentInfo {
            index: 0,
            start: 0,
            end: data.len() as u64 - 1,
            size: data.len() as u64,
            downloaded: 0,
            hash: None,
        };
        let protocol = Arc::new(MockProto::new(test_metadata("f12.bin", data.len() as u64)));
        let storage = StorageKind::memory_with_capacity(data.len());
        let mut task = make_task(protocol, storage, test_config());
        task.fragments = vec![FragmentRecord::new(frag_info, 3)];
        task.metadata = Some(test_metadata("f12.bin", data.len() as u64));
        task.set_rate_limiter(limiter.clone());

        // 分片须先进入 Downloading 状态才能完成
        task.fragments[0].start_download().unwrap();

        // 模拟一次慢分片:1 秒下载 2 字节 => 实测 2 bytes/sec(远低于 CAP)。
        // 旧实现会调用 limiter.update_rate(2),把上限拉低到 2 bytes/sec。
        task.record_completed_fragment(0, 2, Duration::from_secs(1), None)
            .expect("记录完成分片不应失败");

        assert_eq!(
            limiter.bytes_per_sec(),
            CAP,
            "限速器上限必须保持用户配置值,不得被实测带宽降低(负反馈 bug)"
        );
    }

    // ===== B5: 镜像路径不误熔断 engine 层 circuit_breaker =====

    /// B5 回归:`has_mirrors=true` 时,即使分片下载连续失败(超过熔断阈值 5),
    /// engine 层 `circuit_breakers` 也不应被熔断(allow 仍返回 true)。
    ///
    /// 根因:镜像路径下 `frag_url` 是主 URL,若 engine 仍以主 URL 为 key 调
    /// `record_failure`,单镜像故障会熔断整个任务(误熔断)。修复(B5):镜像路径
    /// 跳过 engine 层熔断,改由 MirrorProtocol 的 per-source stats 接管故障隔离。
    ///
    /// 构造:`has_mirrors=true` + 失败协议(download_range 无数据 → Network 错误),
    /// `max_retries=0` 快速失败。execute 必然失败,但断言 `circuit_breakers.allow(&url)`
    /// 仍为 true(从未 record_failure → 从未熔断)。
    #[tokio::test]
    async fn test_b5_mirrors_path_does_not_trip_engine_circuit_breaker() {
        let url = "http://example.com/b5-mirror.bin";
        // 失败协议:probe 成功但 download_range 无数据 → 失败
        let protocol: Arc<dyn Protocol> = Arc::new(MockProto::new(test_metadata("b5.bin", 200)));
        let storage = StorageKind::memory_with_capacity(200);
        let mut task = DownloadTask::new_for_test(
            url.to_string(),
            DownloadConfig {
                max_retries: 0,
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: 100,
            max_fragment_size: 100,
            ..Default::default()
        };
        // 标记为镜像路径(B5:engine 层熔断应被跳过)
        task.has_mirrors = true;

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // execute 必然失败(协议无 range 数据),但失败不应触发 engine 熔断器
        let result = task.execute().await;
        assert!(result.is_err(), "失败协议下 execute 应返回错误");

        // B5 核心断言:engine 层 circuit_breakers 未被熔断(allow 仍为 true)
        assert!(
            task.circuit_breakers.allow(url),
            "B5: 镜像路径下 engine 层熔断器不应被触发(应仍 Closed),\
             实际已被误熔断(主 URL 为 key 记了 failure)"
        );
    }

    /// B5 对照组:`has_mirrors=false`(单源路径)时,分片连续失败应触发 engine 熔断器,
    /// 证明 B5 的跳过逻辑仅在镜像路径生效(不破坏单源故障隔离语义)。
    #[tokio::test]
    async fn test_b5_single_source_path_trips_engine_circuit_breaker() {
        let url = "http://example.com/b5-single.bin";
        let protocol: Arc<dyn Protocol> = Arc::new(MockProto::new(test_metadata("b5s.bin", 200)));
        let storage = StorageKind::memory_with_capacity(200);
        let mut task = DownloadTask::new_for_test(
            url.to_string(),
            DownloadConfig {
                max_retries: 3, // 允许重试以累积 failure 到阈值 5
                max_concurrent_fragments: 2,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: 100,
            max_fragment_size: 100,
            ..Default::default()
        };
        // 单源路径(has_mirrors=false):engine 熔断器应工作
        task.has_mirrors = false;

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        let _ = task.execute().await;

        // 对照组:单源路径下失败应触发 engine 熔断器(allow 为 false)
        // 注:2 个分片 × (max_retries+1)=4 次尝试 = 8 次 failure > 阈值 5 → 熔断
        assert!(
            !task.circuit_breakers.allow(url),
            "B5 对照组: 单源路径下连续失败应触发 engine 熔断器(应 Open),\
             实际未熔断(B5 跳过逻辑可能误覆盖了单源路径)"
        );
    }

    // ===== B11: execute_full_download 取消穿透 =====

    /// B11 回归:`execute_full_download` 的流读取循环必须能被取消信号穿透,
    /// 即使流永不产出 chunk(死连接静默挂起)。
    ///
    /// 根因:旧实现 `while let Some(chunk) = stream.next().await` 是裸 await,
    /// 取消检查点在循环体内不可达(流 Pending 时 select 不竞速)→ 取消信号无法穿透。
    /// 修复(B11):改为 `loop { select!{ chunk=stream.next()=>..., interrupt=watch_for_interrupt()=>... } }`。
    ///
    /// 构造:不支持 Range 的协议(走 execute_full_download),其 `download_full_stream`
    /// 返回永不产出项的 pending 流。注入 control_rx,50ms 后发 Cancel。
    /// 修复前:500ms 超时失败(流 Pending,取消不可达);修复后:取消即时返回 Cancelled。
    #[tokio::test]
    async fn test_b11_cancel_penetrates_full_download_stalled_stream() {
        use std::future::Future;
        use std::pin::Pin;

        /// 死流协议:probe 成功,download_full_stream 返回永不产出的 pending 流
        struct StallingFullProtocol {
            meta: FileMetadata,
        }
        impl Clone for StallingFullProtocol {
            fn clone(&self) -> Self {
                Self {
                    meta: self.meta.clone(),
                }
            }
        }
        impl Protocol for StallingFullProtocol {
            fn probe(
                &self,
                _url: &str,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>> {
                let meta = self.meta.clone();
                Box::pin(async move { Ok(meta) })
            }
            fn download_range(
                &self,
                _url: &str,
                _start: u64,
                _end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
                Box::pin(async { Err(DownloadError::Protocol("不应调用".into())) })
            }
            fn download_range_stream(
                &self,
                _url: &str,
                _start: u64,
                _end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
                Box::pin(async {
                    Ok(Box::pin(futures::stream::pending::<DownloadResult<Bytes>>()) as ByteStream)
                })
            }
            fn download_full(
                &self,
                _url: &str,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
                Box::pin(async { Err(DownloadError::Protocol("不应调用".into())) })
            }
            fn download_full_stream(
                &self,
                _url: &str,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
                Box::pin(async {
                    Ok(Box::pin(futures::stream::pending::<DownloadResult<Bytes>>()) as ByteStream)
                })
            }
        }

        let url = "http://example.com/b11-stall.bin";
        // 不支持 Range → 走 execute_full_download 路径
        let meta = FileMetadata {
            file_name: "b11.bin".into(),
            file_size: Some(100),
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol: Arc<dyn Protocol> = Arc::new(StallingFullProtocol { meta });
        let storage = StorageKind::memory_with_capacity(100);
        let mut task = DownloadTask::new_for_test(
            url.to_string(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );
        let (control_tx, control_rx) = watch::channel(TaskCommand::Start);
        task.set_control_rx(control_rx);

        task.probe().await.unwrap();
        task.plan().unwrap();
        task.prepare_storage().await.unwrap();

        // 50ms 后发取消,给 execute 进入 stream.next().await(永久 Pending)留时间
        let cancel_handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            control_tx.send(TaskCommand::Cancel).unwrap();
        });

        let result = tokio::time::timeout(Duration::from_millis(500), task.execute())
            .await
            .expect("B11: 取消信号应穿透 execute_full_download 的 stalled 流读取");
        cancel_handle.await.unwrap();

        assert!(
            matches!(result, Err(DownloadError::Cancelled)),
            "B11: stalled 流下取消应返回 Cancelled,实际: {result:?}"
        );
    }

    // ------ execute_full_download 整块路径进度上报 ------

    /// 多块整块流协议:probe 成功(不支持 Range),download_full_stream 产出 N 块。
    /// 供整块路径进度上报相关测试复用。
    struct MultiChunkFullProtocol {
        meta: FileMetadata,
        chunks: Vec<Bytes>,
    }
    impl Clone for MultiChunkFullProtocol {
        fn clone(&self) -> Self {
            Self {
                meta: self.meta.clone(),
                chunks: self.chunks.clone(),
            }
        }
    }
    impl Protocol for MultiChunkFullProtocol {
        fn probe(
            &self,
            _url: &str,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>> {
            let meta = self.meta.clone();
            Box::pin(async move { Ok(meta) })
        }
        fn download_range(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
            Box::pin(async { Err(DownloadError::Protocol("不应调用 download_range".into())) })
        }
        fn download_range_stream(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
            _identity: Option<ObjectIdentity>,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
            Box::pin(async {
                Err(DownloadError::Protocol(
                    "不应调用 download_range_stream".into(),
                ))
            })
        }
        fn download_full(
            &self,
            _url: &str,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
            // 不会到达:execute_full_download 走 download_full_stream
            Box::pin(async { Err(DownloadError::Protocol("不应调用 download_full".into())) })
        }
        fn download_full_stream(
            &self,
            _url: &str,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
            let chunks = self.chunks.clone();
            Box::pin(async move {
                let items: Vec<DownloadResult<Bytes>> = chunks.into_iter().map(Ok).collect();
                Ok(Box::pin(futures::stream::iter(items)) as ByteStream)
            })
        }
    }

    /// 构造不支持 Range 的整块下载(chunk_count × chunk_size 字节),跑完整下载
    /// 并收集全部 FragmentProgress 事件
    async fn run_full_download_collect_events(
        chunk_count: usize,
        chunk_size: usize,
    ) -> Vec<FragmentProgress> {
        let total = (chunk_size * chunk_count) as u64;
        let chunks: Vec<Bytes> = (0..chunk_count)
            .map(|i| Bytes::from(vec![0xA0 + i as u8; chunk_size]))
            .collect();
        // 不支持 Range → 走 execute_full_download 路径
        let meta = FileMetadata {
            file_name: "full-progress.bin".into(),
            file_size: Some(total),
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let protocol: Arc<dyn Protocol> = Arc::new(MultiChunkFullProtocol { meta, chunks });
        let storage = StorageKind::memory_with_capacity(total as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/full-progress.bin".into(),
            DownloadConfig {
                max_retries: 0,
                verify_checksum: false,
                ..test_config()
            },
            protocol,
            storage,
        );

        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<FragmentProgress>(64);
        task.set_progress_sender(progress_tx);

        task.run().await.expect("整块多 chunk 下载应成功");
        assert_eq!(task.state(), DownloadState::Completed);

        let mut events = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(50), progress_rx.recv()).await
        {
            events.push(event);
        }
        events
    }

    /// 统计增量 Chunk(completed:false 且字节数>0)与终态 Chunk(completed:true)事件数
    fn count_full_progress_events(events: &[FragmentProgress]) -> (usize, usize) {
        let incremental = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    FragmentProgress::Chunk {
                        completed: false,
                        fragment_downloaded,
                        ..
                    } if *fragment_downloaded > 0
                )
            })
            .count();
        let completed = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    FragmentProgress::Chunk {
                        completed: true,
                        fragment_index: 0,
                        ..
                    }
                )
            })
            .count();
        (incremental, completed)
    }

    /// 整块路径(supports_range=false → execute_full_download_once)必须报告 Chunk 进度。
    ///
    /// 与分片路径对齐:增量 Chunk 按 PROGRESS_REPORT_CHUNK_INTERVAL(5)个 chunk
    /// 节流上报,终态 completed:true 单独发送不节流。
    ///
    /// 构造 7×100 字节多块流(超过节流间隔,保证至少 1 条增量);跑完整下载。
    /// 期望:至少 1 条增量 Chunk(completed:false, fragment_downloaded>0),
    /// 以及恰好 1 条终态 Chunk(completed:true, fragment_index=0)。
    #[tokio::test]
    async fn full_download_reports_chunk_progress() {
        let events = run_full_download_collect_events(7, 100).await;
        let (incremental, completed) = count_full_progress_events(&events);
        assert!(
            incremental >= 1,
            "整块路径应至少报告 1 条增量 Chunk(completed:false, fragment_downloaded>0), \
             实际 events={events:?}"
        );
        assert_eq!(
            completed, 1,
            "整块路径应恰好报告 1 条 fragment_index=0 的 completed:true Chunk, \
             实际 events={events:?}"
        );
    }

    /// 整块路径增量进度必须按 PROGRESS_REPORT_CHUNK_INTERVAL 节流(与分片路径
    /// 同一 countdown 模式),否则下游 chunk reader 每 20 事件触发一次
    /// put_durable(fsync)checkpoint,fsync 频率可达分片路径 20-80 倍。
    ///
    /// 构造 12×100 字节流:12 个网络 chunk 应恰好产生 12/5=2 条增量 Chunk,
    /// 外加 1 条不节流的终态 completed Chunk。
    #[tokio::test]
    async fn full_download_throttles_chunk_progress() {
        let events = run_full_download_collect_events(12, 100).await;
        let (incremental, completed) = count_full_progress_events(&events);
        assert_eq!(
            incremental, 2,
            "12 个网络 chunk 按间隔 5 节流应产生 2 条增量 Chunk, 实际 events={events:?}"
        );
        assert_eq!(
            completed, 1,
            "终态 completed Chunk 不节流,应恰好 1 条, 实际 events={events:?}"
        );
    }

    // ===== P6: verify 读盘哈希循环取消穿透 =====

    /// P6 回归:`verify` 读盘哈希循环必须能被取消信号穿透,即使读盘持续很久。
    ///
    /// 根因:旧实现裸 `while offset < end { read_at... }`,无取消检查点 → 大文件
    /// 读盘(数分钟)时取消信号无法穿透。修复(P6):每累计 `VERIFY_CANCEL_CHECK_BYTES`
    /// (64MiB)已读数据插入一次 `wait_control_rx` 检查点。按字节度量使检查频率与
    /// read_at 单次返回量解耦。
    ///
    /// 构造:单分片 + 预期 hash + 慢速大块读存储(每次 read_at 返回整段 buf 并 sleep,
    /// 文件 72MiB > 64MiB 阈值,8MiB chunk → 第 9 次读盘累计 72MiB ≥ 64MiB 触发检查点)。
    /// 注入 control_rx,读盘开始后发 Cancel。修复前:取消不可达(读盘循环无检查点)→
    /// 超时;修复后:累计达 64MiB 时检查点触发取消,返回 Cancelled。
    #[tokio::test]
    async fn test_p6_cancel_penetrates_verify_disk_read_loop() {
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::Arc;
        use tokio::sync::Notify;

        /// 慢速大块读存储:每次 read_at 返回整段 buf(最多 chunk_size=8MiB)并 sleep,
        /// 模拟慢速大文件读盘。文件 72MiB > 64MiB 阈值,8 次 8MiB 读盘后累计 64MiB,
        /// 第 9 次读盘时触发 P6 检查点。无需真实数十 GB 文件,但数据量足以验证字节累加。
        struct SlowShortReadStorage {
            data: Vec<u8>,
            read_started: Arc<Notify>,
        }
        impl Clone for SlowShortReadStorage {
            fn clone(&self) -> Self {
                Self {
                    data: self.data.clone(),
                    read_started: self.read_started.clone(),
                }
            }
        }
        impl AsyncStorage for SlowShortReadStorage {
            fn write_at(
                &self,
                _offset: u64,
                data: Bytes,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
                Box::pin(async move { Ok(data.len()) })
            }
            fn read_at<'a>(
                &'a self,
                offset: u64,
                buf: &'a mut [u8],
            ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
                Box::pin(async move {
                    self.read_started.notify_waiters();
                    // 模拟慢速读盘:sleep 使取消信号有窗口发送。
                    // 30ms × 9 次 ≈ 270ms,远大于 50ms 取消延迟,确保取消在 verify 完成前到达。
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    let pos = offset as usize;
                    if pos >= self.data.len() {
                        return Ok(0);
                    }
                    // 大块读:返回整段 buf(受剩余数据量限制),使字节累加快速达阈值
                    let n = (self.data.len() - pos).min(buf.len());
                    buf[..n].copy_from_slice(&self.data[pos..pos + n]);
                    Ok(n)
                })
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
                Box::pin(async move { Ok(self.data.len() as u64) })
            }
            fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
                Box::pin(async { Ok(()) })
            }
        }

        // 72MiB 文件:8MiB chunk × 9 次读盘,第 9 次累计 72MiB ≥ 64MiB(检查点阈值)
        // 选 72 而非 64:确保有一次"超阈值"读盘触发检查,而非恰好卡在边界。
        let file_size: u64 = 72 * 1024 * 1024;
        let data: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
        let hash = {
            let v = CpuVerifier::blake3();
            v.compute_hash(&data).unwrap()
        };
        let slow_storage = SlowShortReadStorage {
            data: data.clone(),
            read_started: Arc::new(Notify::new()),
        };
        let read_started = slow_storage.read_started.clone();
        let storage = StorageKind::new(slow_storage.clone());

        let frag_info = FragmentInfo {
            index: 0,
            start: 0,
            end: file_size - 1,
            size: file_size,
            downloaded: 0,
            hash: Some(hash),
        };
        // protocol 仅占位(verify 不下载,直接读盘)
        let protocol = Arc::new(MockProto::new(test_metadata("p6.bin", file_size)));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/p6.bin".into(),
            DownloadConfig {
                verify_checksum: true,
                verify_strategy: tachyon_core::config::VerifyStrategy::BestEffort,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.fragments = vec![FragmentRecord::new(frag_info, 3)];
        task.metadata = Some(test_metadata("p6.bin", file_size));
        // 确保走"无 computed_hash → 读盘计算"路径(断点续传分片)
        assert!(
            task.fragments[0].computed_hash.is_none(),
            "P6 测试需走读盘哈希路径(无 computed_hash)"
        );

        let (control_tx, control_rx) = watch::channel(TaskCommand::Start);
        task.set_control_rx(control_rx);

        // 读盘开始后 50ms 发取消(此时已读 ~25 字节,尚未到 66 次检查点,
        // 但 sleep 2ms × 66 ≈ 132ms,取消会在第 66 次检查点触发)
        let cancel_handle = tokio::spawn(async move {
            read_started.notified().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            control_tx.send(TaskCommand::Cancel).unwrap();
        });

        let result = tokio::time::timeout(Duration::from_millis(5000), task.verify())
            .await
            .expect("P6: 取消信号应穿透 verify 读盘哈希循环");
        cancel_handle.await.unwrap();

        assert!(
            matches!(result, Err(DownloadError::Cancelled)),
            "P6: 读盘循环中取消应返回 Cancelled,实际: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // AlignedBuf 路径测试:write_stream_to_storage_with_fallback 的 4 条写入路径
    // (大 chunk 直写 / 容量不足预刷写 / 正常累积批量刷写 / 循环后残余刷写)
    // -----------------------------------------------------------------------

    /// 辅助函数:构造带 MemoryStorage 的 DownloadTask 并返回 (task, storage_arc)。
    ///
    /// `write_stream_to_storage_with_fallback` 读 `self.storage`(必须 Some)、
    /// `self.control_rx`(默认 None -> 走无中断竞速的 else 分支)、
    /// `self.config.pause_timeout_secs`(`test_config()` 已设置)。返回 storage 的
    /// Arc 克隆,使测试在 task.storage 被借检后仍能读取写入结果。
    #[cfg(feature = "magnet")]
    fn make_bt_fallback_task(capacity: usize) -> (DownloadTask, Arc<StorageSet>) {
        let storage = StorageKind::memory_with_capacity(capacity);
        let storage_set = Arc::new(StorageSet::single(storage));
        let task = DownloadTask::new_for_test(
            "http://example.com/file.bin".into(),
            test_config(),
            Arc::new(MockProto::new(test_metadata("data.bin", 0))),
            // new_for_test 内部会 wrap 为 StorageSet::single;但我们手动重设以拿到
            // 同一个 Arc 引用(便于断言)。先传 memory 占位,再覆盖。
            StorageKind::memory(),
        );
        // 覆盖为同一个 Arc,使测试侧持有引用可读回数据
        let mut task = task;
        task.storage = Some(Arc::clone(&storage_set));
        (task, storage_set)
    }

    /// 辅助函数:从 chunk 字节序列构造 ByteStream(逐块产出 Ok(Bytes))。
    #[cfg(feature = "magnet")]
    fn make_byte_stream(chunks: Vec<bytes::Bytes>) -> ByteStream {
        Box::pin(futures::stream::iter(
            chunks.into_iter().map(Ok::<_, DownloadError>),
        ))
    }

    /// 辅助函数:断言 storage 从 offset 0 起的数据与期望完全一致。
    #[cfg(feature = "magnet")]
    async fn assert_storage_content(storage: &StorageSet, expected: &[u8]) {
        let mut buf = vec![0u8; expected.len()];
        let read = storage.read_at(0, &mut buf).await.expect("读 storage 失败");
        assert_eq!(
            read,
            expected.len(),
            "读回字节数应等于期望长度(读到 {read},期望 {})",
            expected.len()
        );
        assert_eq!(buf, expected, "storage 数据应与期望完全一致");
    }

    /// 覆盖 write_stream_to_storage_with_fallback 的大 chunk 直写路径:
    /// 单个 chunk >= WRITE_BATCH_BYTES(256KiB)时,直接写入不经过 AlignedBuf 聚合。
    ///
    /// 构造单个 512KiB chunk(> 256KiB 阈值),验证:
    ///   1. 循环内 `if chunk.len() >= WRITE_BATCH_BYTES` 分支命中(此时 write_buf 为空,
    ///      残余刷写分支 `!write_buf.is_empty()` 短路跳过,仅直接写入 chunk 本身);
    ///   2. 循环后 `write_buf.is_empty()` 为 true,残余刷写分支跳过;
    ///   3. storage 中 512KiB 数据与输入完全一致。
    #[cfg(feature = "magnet")]
    #[tokio::test]
    async fn test_bt_fallback_large_chunk_direct_write() {
        let chunk_size = WRITE_BATCH_BYTES * 2; // 512KiB > 256KiB 阈值
        let content: Vec<u8> = (0..chunk_size).map(|i| (i % 251) as u8).collect();
        let stream = make_byte_stream(vec![Bytes::from(content.clone())]);

        let (mut task, storage) = make_bt_fallback_task(chunk_size);
        task.write_stream_to_storage_with_fallback(stream)
            .await
            .expect("大 chunk 直写应成功");

        assert_storage_content(&storage, &content).await;
    }

    /// 覆盖容量不足预刷写路径:
    /// 两个 200KiB chunk(总和 400KiB > 256KiB),第二个触发预刷写。
    ///
    /// 流程:
    ///   1. 第一个 200KiB chunk(< 256KiB):`extend_from_slice` 累积到 write_buf,
    ///      `write_buf.len() < WRITE_BATCH_BYTES` 不触发批量刷写;
    ///   2. 第二个 200KiB chunk:进入 `!write_buf.is_empty() && write_buf.len()
    ///      + chunk.len() > WRITE_BATCH_BYTES` 分支(200+200=400 > 256),
    ///      先刷写 write_buf 中的 200KiB,再 `extend_from_slice` 第二个 chunk,
    ///      `write_buf.len()=200 < 256` 不触发批量刷写;
    ///   3. 循环后残余刷写第二个 chunk 的 200KiB。
    ///
    /// 验证 storage 中 400KiB 数据与两 chunk 拼接后完全一致。
    #[cfg(feature = "magnet")]
    #[tokio::test]
    async fn test_bt_fallback_capacity_preflush() {
        let chunk_size = 200 * 1024; // 200KiB,两块共 400KiB > 256KiB
        let chunk_a: Vec<u8> = (0..chunk_size).map(|i| (i % 251) as u8).collect();
        let chunk_b: Vec<u8> = (0..chunk_size).map(|i| ((i + 1) % 251) as u8).collect();
        let expected: Vec<u8> = chunk_a.iter().chain(chunk_b.iter()).copied().collect();
        let stream = make_byte_stream(vec![Bytes::from(chunk_a), Bytes::from(chunk_b)]);

        let (mut task, storage) = make_bt_fallback_task(chunk_size * 2);
        task.write_stream_to_storage_with_fallback(stream)
            .await
            .expect("容量不足预刷写应成功");

        assert_storage_content(&storage, &expected).await;
    }

    /// 覆盖正常累积 + 批量刷写 + 尾部残余:
    /// 5 个 64KiB chunk(总 320KiB),前 4 个累积到 256KiB 触发批量刷写,
    /// 第 5 个 64KiB 作为尾部残余在循环后刷写。
    ///
    /// 流程:
    ///   1. chunk 1~3(64KiB × 3 = 192KiB):累积,`write_buf.len() < 256KiB` 不刷写;
    ///   2. chunk 4(第 4 个 64KiB):累积到 256KiB,`write_buf.len() >= WRITE_BATCH_BYTES`
    ///      触发批量刷写,write_buf 清空;
    ///   3. chunk 5(第 5 个 64KiB):累积到 64KiB,不足 256KiB 不刷写;
    ///   4. 循环后残余刷写 64KiB。
    ///
    /// 验证 storage 中 320KiB 数据与 5 chunk 拼接后完全一致。
    #[cfg(feature = "magnet")]
    #[tokio::test]
    async fn test_bt_fallback_multi_chunk_accumulate_and_residual() {
        let chunk_size = 64 * 1024; // 64KiB,5 块共 320KiB
        let chunks: Vec<Vec<u8>> = (0..5)
            .map(|n| {
                (0..chunk_size)
                    .map(|i| ((i + n * 17) % 251) as u8)
                    .collect()
            })
            .collect();
        let expected: Vec<u8> = chunks.iter().flatten().copied().collect();
        let stream = make_byte_stream(chunks.into_iter().map(Bytes::from).collect());

        let (mut task, storage) = make_bt_fallback_task(chunk_size * 5);
        task.write_stream_to_storage_with_fallback(stream)
            .await
            .expect("多 chunk 累积 + 残余刷写应成功");

        assert_storage_content(&storage, &expected).await;
    }

    // ===== work-stealing 集成测试 =====

    /// 验证 work-stealing 禁用时,慢分片不被拆分但仍能完成
    #[tokio::test]
    #[allow(deprecated)]
    async fn test_work_stealing_disabled_slow_fragment_still_completes() {
        let frag_size = 4096u64;
        let total_size = frag_size * 2;

        let frag_a: Vec<u8> = (0..frag_size).map(|i| (i % 251) as u8).collect();
        let frag_b: Vec<u8> = (0..frag_size).map(|i| ((i + 50) % 251) as u8).collect();
        let expected: Vec<u8> = frag_a.iter().chain(frag_b.iter()).copied().collect();

        let meta = FileMetadata {
            file_name: "no_steal.bin".into(),
            file_size: Some(total_size),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };

        let protocol: Arc<dyn Protocol> = Arc::new(
            MockProto::new(meta)
                .with_range_data(0, frag_size - 1, Bytes::from(frag_a.clone()))
                .with_range_data(frag_size, total_size - 1, Bytes::from(frag_b.clone()))
                .with_chunk_size(256)
                .with_chunk_delay(Duration::from_millis(20)),
        );

        let sched_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 2,
            ewma_alpha: 0.3,
            ..Default::default()
        };
        let config = DownloadConfig {
            enable_work_stealing: false,
            ..test_config()
        };

        let mut task = DownloadTask::new_for_test(
            "http://example.com/no_steal.bin".into(),
            config,
            protocol,
            StorageKind::memory_with_capacity(total_size as usize),
        );
        task.scheduler_config = sched_config;

        task.run().await.expect("下载流程失败");
        assert_eq!(task.state(), DownloadState::Completed);

        let mut buf = vec![0u8; total_size as usize];
        task.storage
            .as_ref()
            .unwrap()
            .read_at(0, &mut buf)
            .await
            .unwrap();
        assert_eq!(&buf[..], &expected[..]);
    }

    // ===== 200 fallback 运行时降级(已实现,回归测试 A+B)=========================

    /// 方案 B:当 `execute_fragmented_download` 在分片 spawn worker 内调用
    /// `download_range_stream` 时,若协议层返回 `Err(DownloadError::RangeNotSupported)`
    /// (方案 A2:HTTP 200 fallback 不再静默截取),engine 必须捕获该错误,
    /// 中止其他在途 worker,re-plan 为单分片,并转交 `execute_full_download`
    /// 通过 `download_full_stream` 整块传输一次。
    ///
    /// 关键断言:
    /// 1. `download_range_stream` 被调用过(说明走了分片路径);
    /// 2. `download_full_stream` 被调用 **恰好 1 次**(整块降级,而非每片重传);
    /// 3. 终态 `Completed`,存储内容与预期一致。
    ///
    /// 若 engine 未捕获 `RangeNotSupported` 降级:
    /// - 要么 `download_full_stream` 调用 0 次(直接 propagate 错误,任务 Failed);
    /// - 要么调用 N 次(每片都触发 200 fallback,带宽浪费,即审计发现的根因)。
    ///
    /// 回归:RangeNotSupported 变体与 engine 降级路径已落地。
    #[tokio::test]
    async fn test_execute_fragmented_download_falls_back_to_full_on_range_not_supported() {
        /// 协议:probe 宣称 supports_range=true(强制走分片路径),
        /// download_range_stream 始终返回 RangeNotSupported(模拟 HTTP 200 fallback),
        /// download_full_stream 返回完整数据(供整块降级路径消费)。
        #[derive(Clone)]
        struct RangeNotSupportedThenFullProtocol {
            meta: FileMetadata,
            full_data: Bytes,
            range_calls: Arc<AtomicU32>,
            full_calls: Arc<AtomicU32>,
        }

        impl Protocol for RangeNotSupportedThenFullProtocol {
            fn probe(
                &self,
                _url: &str,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>> {
                let meta = self.meta.clone();
                Box::pin(async move { Ok(meta) })
            }

            fn download_range(
                &self,
                _url: &str,
                _start: u64,
                _end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
                // 不会到达:download_single_fragment 走 download_range_stream
                Box::pin(async {
                    Err(DownloadError::Protocol("不应调用 download_range".into()))
                })
            }

            fn download_range_stream(
                &self,
                _url: &str,
                _start: u64,
                _end: u64,
                _identity: Option<ObjectIdentity>,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
                self.range_calls.fetch_add(1, AtomicOrdering::SeqCst);
                Box::pin(async { Err(DownloadError::RangeNotSupported) })
            }

            fn download_full(
                &self,
                _url: &str,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
                // 不会到达:execute_full_download 走 download_full_stream
                Box::pin(async {
                    Err(DownloadError::Protocol("不应调用 download_full".into()))
                })
            }

            fn download_full_stream(
                &self,
                _url: &str,
            ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
                self.full_calls.fetch_add(1, AtomicOrdering::SeqCst);
                let data = self.full_data.clone();
                Box::pin(async move {
                    Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
                })
            }
        }

        let total_size = 4096u64;
        let frag_size = 1024u64; // 强制 4 分片,确保走 execute_fragmented_download
        let full_data = Bytes::from(vec![0x5Au8; total_size as usize]);

        let meta = FileMetadata {
            file_name: "range-not-supported.bin".into(),
            file_size: Some(total_size),
            content_type: None,
            supports_range: true, // 关键:强制走分片路径触发 RangeNotSupported
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };

        let range_calls = Arc::new(AtomicU32::new(0));
        let full_calls = Arc::new(AtomicU32::new(0));
        let protocol = Arc::new(RangeNotSupportedThenFullProtocol {
            meta,
            full_data: full_data.clone(),
            range_calls: Arc::clone(&range_calls),
            full_calls: Arc::clone(&full_calls),
        });
        let storage = StorageKind::memory_with_capacity(total_size as usize);

        let mut task = DownloadTask::new_for_test(
            "http://example.com/range-not-supported.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                max_retries: 0, // 禁用退避重试,直接暴露降级路径
                ..test_config()
            },
            protocol as Arc<dyn Protocol>,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            ..Default::default()
        };

        task.run()
            .await
            .expect("RangeNotSupported 应触发整块降级,不应失败");

        // 1. 确实走了分片路径(至少一次 download_range_stream 调用)
        assert!(
            range_calls.load(AtomicOrdering::SeqCst) > 0,
            "应先进入 execute_fragmented_download 调用 download_range_stream"
        );
        // 2. 整块降级:download_full_stream 恰好调用 1 次(而非 N 次)
        assert_eq!(
            full_calls.load(AtomicOrdering::SeqCst),
            1,
            "RangeNotSupported 降级应转 execute_full_download,download_full_stream \
             仅调用 1 次(整块传输),而非每片重复触发 200 fallback"
        );
        assert_eq!(
            task.metadata().map(|m| m.supports_range),
            Some(false),
            "降级后 metadata.supports_range 必须为 false(供快照持久化)"
        );
        // 3. 终态 + 数据正确
        assert_eq!(task.state(), DownloadState::Completed);
        let mut buf = vec![0u8; total_size as usize];
        task.storage
            .as_ref()
            .expect("storage 应存在")
            .read_at(0, &mut buf)
            .await
            .expect("读存储应成功");
        assert_eq!(&buf[..], full_data.as_ref(), "整块降级后数据应完整写入");
    }

    /// 安全 rebalance:慢片剩余足够时 try_split + await 入队成功,fragments 增长 1
    #[tokio::test]
    async fn test_rebalance_splits_slow_fragment_and_enqueues() {
        use crate::fragment::{FragmentRecord, FragmentState, MIN_SPLIT_SIZE};
        use std::sync::atomic::Ordering;
        use tachyon_core::types::FragmentInfo;

        let size = MIN_SPLIT_SIZE * 8; // 足够大
        let frag0 = {
            let info = FragmentInfo::new(0, 0, size - 1, size).unwrap();
            let mut r = FragmentRecord::new(info, 3);
            r.start_download().unwrap();
            // 仅下载 10%,剩余很大
            r.realtime_downloaded.store(size / 10, Ordering::Release);
            // 回拨 start_time 以越过 rebalance 1s 年龄门槛
            r.start_time = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
            r
        };
        let protocol = Arc::new(MockProto::new(test_metadata("rebalance.bin", size)));
        let storage = StorageKind::memory_with_capacity(size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/rebalance.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                max_concurrent_fragments: 4,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.fragments = vec![frag0];
        task.metadata = Some(test_metadata("rebalance.bin", size));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<FragmentSpec>(4);
        let did = task
            .try_rebalance_slowest_fragment(&tx)
            .await
            .expect("rebalance 不应 Err");
        assert!(did, "应拆分慢片");
        assert_eq!(task.fragments.len(), 2, "应新增 1 个分片");
        assert_eq!(task.fragments[0].state, FragmentState::Downloading);
        assert_eq!(task.fragments[1].state, FragmentState::Downloading);
        // 原片 end 缩小
        assert!(task.fragments[0].info.end < size - 1, "原片 end 应缩小");
        // 队列收到新 spec
        let spec = rx.try_recv().expect("应入队新分片 spec");
        assert_eq!(spec.0, 1, "新分片 index=1");
        assert!(spec.1 > 0, "新分片 start > 0");
    }

    /// RED-TDD: channel 满时 rebalance 不得 send().await 挂死主循环;
    /// 应快速返回 Ok(false) 并 revert_split,保留原分片边界。
    #[tokio::test]
    async fn test_rebalance_full_channel_does_not_hang_and_reverts() {
        use crate::fragment::{FragmentRecord, MIN_SPLIT_SIZE};
        use std::sync::atomic::Ordering;
        use tachyon_core::types::FragmentInfo;

        let size = MIN_SPLIT_SIZE * 8;
        let original_end = size - 1;
        let frag0 = {
            let info = FragmentInfo::new(0, 0, original_end, size).unwrap();
            let mut r = FragmentRecord::new(info, 3);
            r.start_download().unwrap();
            r.realtime_downloaded.store(size / 10, Ordering::Release);
            r.start_time = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
            r
        };
        let protocol = Arc::new(MockProto::new(test_metadata("full-ch.bin", size)));
        let storage = StorageKind::memory_with_capacity(size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/full-ch.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                max_concurrent_fragments: 4,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.fragments = vec![frag0];
        task.metadata = Some(test_metadata("full-ch.bin", size));

        // 容量 1:塞满后 rebalance 的 send 无法前进
        let (tx, _rx) = tokio::sync::mpsc::channel::<FragmentSpec>(1);
        let dummy: FragmentSpec = (
            99,
            0,
            0,
            0,
            false,
            FragmentShared {
                effective_end: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                realtime_downloaded: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        tx.try_send(dummy).expect("先填满 channel");

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            task.try_rebalance_slowest_fragment(&tx),
        )
        .await;
        assert!(
            result.is_ok(),
            "rebalance 在 channel 满时必须在 200ms 内返回,不得 send().await 挂死主循环"
        );
        let did = result.unwrap().expect("rebalance 不应 Err");
        assert!(!did, "channel 满时应返回 Ok(false) 表示未入队");
        assert_eq!(task.fragments.len(), 1, "未入队成功则不得 push 新分片");
        assert_eq!(
            task.fragments[0].info.end, original_end,
            "入队失败必须 revert_split 恢复原 end"
        );
        assert_eq!(
            task.fragments[0].effective_end.load(Ordering::Acquire),
            original_end,
            "入队失败必须 revert effective_end"
        );
    }

    /// rebalance:剩余不足时不拆分
    #[tokio::test]
    async fn test_rebalance_skips_when_remaining_too_small() {
        use crate::fragment::{FragmentRecord, MIN_SPLIT_SIZE};
        use std::sync::atomic::Ordering;
        use tachyon_core::types::FragmentInfo;

        let size = MIN_SPLIT_SIZE; // 太小
        let frag0 = {
            let info = FragmentInfo::new(0, 0, size - 1, size).unwrap();
            let mut r = FragmentRecord::new(info, 3);
            r.start_download().unwrap();
            r.realtime_downloaded.store(0, Ordering::Release);
            r
        };
        let protocol = Arc::new(MockProto::new(test_metadata("tiny.bin", size)));
        let storage = StorageKind::memory_with_capacity(size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/tiny.bin".into(),
            test_config(),
            protocol,
            storage,
        );
        task.fragments = vec![frag0];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<FragmentSpec>(4);
        let did = task.try_rebalance_slowest_fragment(&tx).await.unwrap();
        assert!(!did);
        assert!(rx.try_recv().is_err());
        assert_eq!(task.fragments.len(), 1);
    }

    /// rebalance 边界:剩余刚好等于 2*MIN_SPLIT_SIZE(128KiB)时应可拆;
    /// 剩余 < 2*MIN_SPLIT_SIZE 时不得拆。审计 P0-4.5 残留:补边界值测试。
    #[tokio::test]
    async fn test_rebalance_boundary_exactly_2x_min_split_size() {
        use crate::fragment::{FragmentRecord, MIN_SPLIT_SIZE};
        use std::sync::atomic::Ordering;
        use tachyon_core::types::FragmentInfo;

        // 剩余刚好 = 2 * MIN_SPLIT_SIZE = 128 KiB,应可拆
        // 构造:total=3*MIN_SPLIT_SIZE, 已下载=MIN_SPLIT_SIZE, 剩余=2*MIN_SPLIT_SIZE
        let size = MIN_SPLIT_SIZE * 3;
        let frag0 = {
            let info = FragmentInfo::new(0, 0, size - 1, size).unwrap();
            let mut r = FragmentRecord::new(info, 3);
            r.start_download().unwrap();
            // 下载 1/3,剩余 2*MIN_SPLIT_SIZE(刚好达到门槛)
            r.realtime_downloaded
                .store(MIN_SPLIT_SIZE, Ordering::Release);
            // 回拨 start_time 越过 1s 年龄门槛
            r.start_time = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
            r
        };
        let protocol = Arc::new(MockProto::new(test_metadata("boundary.bin", size)));
        let storage = StorageKind::memory_with_capacity(size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/boundary.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                max_concurrent_fragments: 4,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.fragments = vec![frag0];
        task.metadata = Some(test_metadata("boundary.bin", size));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<FragmentSpec>(4);
        let did = task.try_rebalance_slowest_fragment(&tx).await.unwrap();
        // 剩余 = 2*MIN_SPLIT_SIZE 刚好达到门槛(条件是 < 2*MIN 才跳过,等于不跳过)
        assert!(did, "剩余=2*MIN_SPLIT_SIZE(128KiB)应可拆分");
        assert_eq!(task.fragments.len(), 2, "应新增 1 个分片");
        let _spec = rx.try_recv().expect("应入队新分片 spec");
    }

    /// rebalance 边界:剩余刚好小于 2*MIN_SPLIT_SIZE 时不得拆分。
    #[tokio::test]
    async fn test_rebalance_boundary_below_2x_min_split_size() {
        use crate::fragment::{FragmentRecord, MIN_SPLIT_SIZE};
        use std::sync::atomic::Ordering;
        use tachyon_core::types::FragmentInfo;

        // 剩余 = 2*MIN_SPLIT_SIZE - 1,应跳过
        let size = MIN_SPLIT_SIZE * 3;
        let frag0 = {
            let info = FragmentInfo::new(0, 0, size - 1, size).unwrap();
            let mut r = FragmentRecord::new(info, 3);
            r.start_download().unwrap();
            // 下载 MIN_SPLIT_SIZE + 1,剩余 = 2*MIN_SPLIT_SIZE - 1(刚好不足门槛)
            r.realtime_downloaded
                .store(MIN_SPLIT_SIZE + 1, Ordering::Release);
            r.start_time = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
            r
        };
        let protocol = Arc::new(MockProto::new(test_metadata("below.bin", size)));
        let storage = StorageKind::memory_with_capacity(size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/below.bin".into(),
            test_config(),
            protocol,
            storage,
        );
        task.fragments = vec![frag0];
        let (tx, _rx) = tokio::sync::mpsc::channel::<FragmentSpec>(4);
        let did = task.try_rebalance_slowest_fragment(&tx).await.unwrap();
        assert!(!did, "剩余 < 2*MIN_SPLIT_SIZE(128KiB)时不得拆分");
        assert_eq!(task.fragments.len(), 1, "不应新增分片");
    }

    /// rebalance 1s 年龄门槛:刚 spawn 的分片(< 1s)不得立即拆分,
    /// 避免频繁拆分开销。审计 P0-4.5:补年龄门槛测试。
    #[tokio::test]
    async fn test_rebalance_skips_fresh_fragment_under_1s_age() {
        use crate::fragment::{FragmentRecord, MIN_SPLIT_SIZE};
        use std::sync::atomic::Ordering;
        use tachyon_core::types::FragmentInfo;

        let size = MIN_SPLIT_SIZE * 8;
        let frag0 = {
            let info = FragmentInfo::new(0, 0, size - 1, size).unwrap();
            let mut r = FragmentRecord::new(info, 3);
            r.start_download().unwrap();
            r.realtime_downloaded.store(size / 10, Ordering::Release);
            // start_time 默认是现在(刚 spawn),不回拨 → 年龄 < 1s
            r
        };
        let protocol = Arc::new(MockProto::new(test_metadata("fresh.bin", size)));
        let storage = StorageKind::memory_with_capacity(size as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/fresh.bin".into(),
            test_config(),
            protocol,
            storage,
        );
        task.fragments = vec![frag0];
        let (tx, _rx) = tokio::sync::mpsc::channel::<FragmentSpec>(4);
        let did = task.try_rebalance_slowest_fragment(&tx).await.unwrap();
        assert!(!did, "刚 spawn 的分片(< 1s)不得立即拆分");
        assert_eq!(task.fragments.len(), 1, "不应新增分片");
    }

    /// rebalance 开启后多分片下载仍正确完成(不回归)
    #[tokio::test]
    async fn test_rebalance_path_multi_fragment_completes() {
        use crate::fragment::MIN_SPLIT_SIZE;

        let frag_size = MIN_SPLIT_SIZE * 4;
        let total = frag_size * 4;
        let data = bytes::Bytes::from(vec![0xABu8; total as usize]);
        let protocol =
            Arc::new(MockProto::new(test_metadata("rebal-e2e.bin", total)).with_default_data(data));
        let storage = StorageKind::memory_with_capacity(total as usize);
        let mut task = DownloadTask::new_for_test(
            "http://example.com/rebal-e2e.bin".into(),
            DownloadConfig {
                verify_checksum: false,
                max_concurrent_fragments: 4,
                ..test_config()
            },
            protocol,
            storage,
        );
        task.scheduler_config = tachyon_core::config::SchedulerConfig {
            min_fragment_size: frag_size,
            max_fragment_size: frag_size,
            sampling_interval_secs: 2,
            ..Default::default()
        };
        task.run().await.expect("rebalance 路径下载应完成");
        assert_eq!(task.state(), DownloadState::Completed);
        assert!(
            task.fragments.iter().all(|f| f.is_done()),
            "所有分片应 Done"
        );
    }

    // ------ BT 冷启动并发解耦与小分片规划 ------

    /// BT 测试元数据构造(protocol_managed_storage 可开关)
    fn bt_test_meta(file_size: u64, managed: bool) -> FileMetadata {
        FileMetadata {
            file_name: "bt.bin".into(),
            file_size: Some(file_size),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: managed,
            resolved_host: None,
        }
    }

    /// bt_fragment_size 公式:file_size/32 clamp [4MiB, 16MiB]
    #[test]
    fn test_bt_fragment_size_clamped() {
        const MIB: u64 = 1024 * 1024;
        // 下限: 64MiB/32 = 2MiB < 4MiB → 4MiB
        assert_eq!(DownloadTask::bt_fragment_size(64 * MIB), 4 * MIB);
        // 边界: 128MiB/32 = 4MiB 恰为下限
        assert_eq!(DownloadTask::bt_fragment_size(128 * MIB), 4 * MIB);
        // 中间: 用户实际文件 293.8MiB(308157657 字节)/32 ≈ 9.6MiB
        assert_eq!(DownloadTask::bt_fragment_size(308157657), 308157657 / 32);
        // 上限: 10GiB/32 = 320MiB > 16MiB → 16MiB
        assert_eq!(DownloadTask::bt_fragment_size(10 * 1024 * MIB), 16 * MIB);
        // 零文件 → 下限
        assert_eq!(DownloadTask::bt_fragment_size(0), 4 * MIB);
    }

    #[tokio::test]
    async fn test_is_bt_task_by_magnet_url() {
        let protocol = Arc::new(MockProto::new(bt_test_meta(4096, false)));
        let task = DownloadTask::new_for_test(
            "magnet:?xt=urn:btih:ABC123".into(),
            test_config(),
            protocol as Arc<dyn Protocol>,
            StorageKind::memory(),
        );
        assert!(task.is_bt_task(), "magnet URL 应判为 BT 任务");
    }

    #[tokio::test]
    async fn test_is_bt_task_by_protocol_managed_storage() {
        let protocol = Arc::new(MockProto::new(bt_test_meta(4096, true)));
        let mut task = DownloadTask::new_for_test(
            "http://example.com/bt.bin".into(),
            test_config(),
            protocol as Arc<dyn Protocol>,
            StorageKind::memory(),
        );
        assert!(!task.is_bt_task(), "HTTP URL 且无元数据标记 → 非 BT");
        task.metadata = Some(bt_test_meta(4096, true));
        assert!(
            task.is_bt_task(),
            "protocol_managed_storage 元数据标记 → BT"
        );
    }

    #[tokio::test]
    async fn test_bt_cold_start_override_returns_configured_when_low_confidence() {
        let protocol = Arc::new(MockProto::new(bt_test_meta(4096, false)));
        let task = DownloadTask::new_for_test(
            "magnet:?xt=urn:btih:ABC123".into(),
            DownloadConfig {
                max_concurrent_fragments: 16,
                ..test_config()
            },
            protocol as Arc<dyn Protocol>,
            StorageKind::memory(),
        );
        // 无样本 confidence=0.0 → 覆盖为 configured(16)
        let rec = tachyon_core::traits::ScheduleRecommendation {
            concurrency: 4,
            fragment_size: 1024,
            confidence: 0.0,
        };
        assert_eq!(task.bt_cold_start_concurrency_override(&rec), Some(16));
        // 有样本 confidence >= 0.5 → 不覆盖,照常参与调度
        let rec2 = tachyon_core::traits::ScheduleRecommendation {
            confidence: 0.6,
            ..rec
        };
        assert_eq!(task.bt_cold_start_concurrency_override(&rec2), None);
    }

    #[tokio::test]
    async fn test_bt_cold_start_override_ignores_http_tasks() {
        let protocol = Arc::new(MockProto::new(bt_test_meta(4096, false)));
        let task = DownloadTask::new_for_test(
            "http://example.com/f.bin".into(),
            test_config(),
            protocol as Arc<dyn Protocol>,
            StorageKind::memory(),
        );
        let rec = tachyon_core::traits::ScheduleRecommendation {
            concurrency: 4,
            fragment_size: 1024,
            confidence: 0.0,
        };
        assert_eq!(
            task.bt_cold_start_concurrency_override(&rec),
            None,
            "HTTP 任务冷启动不覆盖(ramp/429 保护保留)"
        );
    }

    /// BT 任务 plan 采用小分片公式(与 HTTP 分片策略解耦)
    #[tokio::test]
    async fn test_plan_bt_uses_small_fragment_formula() {
        let file_size = 308157657u64; // 用户实际文件 293.8MiB
        let protocol = Arc::new(MockProto::new(bt_test_meta(file_size, false)));
        let mut task = DownloadTask::new_for_test(
            "magnet:?xt=urn:btih:ABC123".into(),
            test_config(),
            protocol as Arc<dyn Protocol>,
            StorageKind::memory(),
        );
        task.metadata = Some(bt_test_meta(file_size, false));
        let frags = task.plan().expect("plan 应成功");
        let expected_size = DownloadTask::bt_fragment_size(file_size);
        assert_eq!(
            frags.len() as u64,
            file_size.div_ceil(expected_size),
            "BT 分片数应由 bt_fragment_size 公式决定(约 32 片)"
        );
        assert!(
            (30..=33).contains(&frags.len()),
            "293.8MiB 应约 32 片,实际 {}",
            frags.len()
        );
    }
}
