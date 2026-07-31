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
//! - `download_proxy`  -- 代理并发推断逻辑(冷启动 cap / 稳态天花板)
//! - `download_verify` -- 校验逻辑(blake3/sha256 + 整文件比对)
//! - `download_executor` -- 并发分片下载执行器(execute / spawn / rebalance)

#[path = "download_executor.rs"]
mod download_executor;
#[path = "download_proxy.rs"]
mod download_proxy;
#[path = "download_verify.rs"]
mod download_verify;

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

/// 已知长度分片下载的终态结构不变式(审计 S-03)。
///
/// 在标 `DownloadState::Completed` 前调用:要求
/// 1. 每个分片状态均为 Done
/// 2. 分片区间连续、不重叠、覆盖 [0, file_size)
/// 3. Σ size == file_size
///
/// 未知长度(`file_size == 0`/`None`)不在此函数职责内,调用方应跳过。
pub(crate) fn assert_known_length_fragment_completion(
    fragments: &[crate::fragment::FragmentRecord],
    file_size: u64,
) -> DownloadResult<()> {
    use crate::fragment::FragmentState;

    if file_size == 0 {
        return Ok(());
    }
    if fragments.is_empty() {
        return Err(DownloadError::Other(
            "已知长度分片完成校验失败: 分片列表为空".into(),
        ));
    }

    let mut ordered: Vec<_> = fragments.iter().collect();
    ordered.sort_by_key(|f| f.info.start);

    let mut cursor = 0u64;
    let mut sum = 0u64;
    for frag in &ordered {
        if frag.state != FragmentState::Done {
            return Err(DownloadError::Other(
                format!(
                    "已知长度分片完成校验失败: 分片 {} 状态为 {:?}, 期望 Done",
                    frag.info.index, frag.state
                )
                .into(),
            ));
        }
        if frag.info.start != cursor {
            return Err(DownloadError::Other(
                format!(
                    "已知长度分片完成校验失败: 分片 {} 起点 {} 与期望连续起点 {} 不一致",
                    frag.info.index, frag.info.start, cursor
                )
                .into(),
            ));
        }
        let end_excl = frag.info.end.saturating_add(1);
        if end_excl <= frag.info.start {
            return Err(DownloadError::Other(
                format!(
                    "已知长度分片完成校验失败: 分片 {} 区间非法 [{}, {}]",
                    frag.info.index, frag.info.start, frag.info.end
                )
                .into(),
            ));
        }
        let size = end_excl - frag.info.start;
        if frag.info.size != size {
            return Err(DownloadError::Other(
                format!(
                    "已知长度分片完成校验失败: 分片 {} size {} 与区间长度 {} 不一致",
                    frag.info.index, frag.info.size, size
                )
                .into(),
            ));
        }
        sum = sum.saturating_add(size);
        cursor = end_excl;
    }

    if cursor != file_size || sum != file_size {
        return Err(DownloadError::Other(format!(
            "已知长度分片完成校验失败: 覆盖终点 {cursor}/累计 {sum} 与 file_size {file_size} 不一致"
        ).into()));
    }
    Ok(())
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

/// Loose 模式分片完成边界 group-commit 批大小:每完成 N 个分片调用一次 `storage.sync()`。
/// 相对 EveryFragment 降低 fsync 频率,同时保证非零 durable 点(16 片场景至少 2 次)。
const LOOSE_GROUP_COMMIT_N: usize = 8;

/// Loose 模式 mid-flight partial 的 group-commit 字节水位。
/// 任务级累计已写入字节每跨过该水位调用一次 `storage.sync()`。
/// 取 `WRITE_BATCH_BYTES`(=256 KiB):与写合并批次对齐,使 sync 由写入量决定、
/// 与网络 chunk 切分无关;小文件 mid-flight 仍有非零 durable 点。
const LOOSE_PARTIAL_GROUP_COMMIT_BYTES: u64 = tachyon_core::config::WRITE_BATCH_BYTES as u64;

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
    /// 崩溃一致性级别:控制分片完成边界的 fsync 频率。
    /// `Loose`(默认)每 `LOOSE_GROUP_COMMIT_N` 个完成分片 group-commit 一次;
    /// `EveryFragment` 每个分片完成时 fsync 一次。
    sync_mode: tachyon_core::config::CrashConsistencyMode,
    /// 任务级 Loose group-commit 计数器(跨分片 worker 共享)。
    /// EveryFragment 路径不读此计数器;仍传入以统一 spawn 签名。
    loose_completed_frags: Arc<std::sync::atomic::AtomicUsize>,
    /// 任务级 Loose partial 进度 group-commit 字节水位(跨分片 worker 共享)。
    /// 仅 Loose + mid-flight partial 路径读取;EveryFragment 不读。
    /// 累计已计入 group-commit 的写入字节。
    loose_partial_bytes: Arc<std::sync::atomic::AtomicU64>,
    /// 代理下片内 Range 窗口(字节)。`None`=整片一次 Range;
    /// `Some(w)`=每次最多请求 w 字节,TLS EOF 只丢当前窗口。
    range_window_bytes: Option<u64>,
    /// 本任务 soft-pressure 冷却截止(与 DownloadTask 共享 Arc)
    soft_pressure_until: &'a Arc<std::sync::atomic::AtomicU64>,
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
    /// 任务级期望校验和(整文件 hex)。LFS oid 等可信来源注入;
    /// 与分片级 FragmentInfo.hash 互补,verify 阶段整文件比对。
    expected_checksum: Option<String>,
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
    /// 测试/观测:MirrorProtocol 的源数量(含 primary + 镜像 + BT)。
    /// 用于断言 P1-P2SP 改造后 BT 是否作为并发源加入。None=非 mirror 路径。
    #[cfg_attr(not(test), allow(dead_code))]
    mirror_source_count: Option<usize>,
    /// 任务级聚合 goodput 窗口起点(多并发分片共享)
    goodput_window_start: Option<Instant>,
    /// 当前窗口内累计完成字节
    goodput_window_bytes: u64,
    /// 上次成功 rebalance 时刻;最小间隔内禁止再拆,避免 soft-pressure 恢复后连环拆片
    last_rebalance_at: Option<Instant>,
    /// rebalance 开关(false = 禁用 `try_rebalance_slowest_fragment`,A/B 量化收益用)。
    ///
    /// 默认 true(生产路径保持当前行为);bench 通过 `set_rebalance_enabled(false)`
    /// 注入,跑 on/off A/B 对照,判定收益是否 >10%(AGENTS.md 性能规则)。
    /// 若 <10% 收益,后续单独 revert PR 删除 rebalance 全套(函数+字段+测试)。
    rebalance_enabled: bool,
    /// 本任务 soft-pressure 冷却截止(epoch 秒)。per-task,避免多任务互串清零/延长。
    soft_pressure_until: Arc<std::sync::atomic::AtomicU64>,
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

/// 审计 HTTP-15:经全局注册表获取/共享 HttpClient(同身份复用 TCP/TLS)
///
/// **强制 HTTP/1.1(多 TCP)**:分片并发的产品语义是「N 片 = N 条独立连接」,
/// 聚合带宽 ≈ N × 单连接限速(见 docs/sdd/perf-research.md)。
/// 默认 `enable_http2=true` 时 reqwest 把多 Range 复用到**同一条 TCP**,
/// CDN/Clash 按连接限流时出现「并发 9 仍 ~8MB/s」(用户实测 wo 网盘)。
/// 下载引擎路径因此覆盖为 `http1_only`,让每片独立握手/独立限速桶。
/// 用户 UI「启用 HTTP/2」仍写入 ConnectionConfig,但引擎下载客户端不沿用
/// 该开关做多路复用(H2 适合 API/小请求,不适合多连接打满带宽)。
fn shared_http_client(
    config: &DownloadConfig,
    pool: &Option<Arc<ConnectionPool>>,
) -> DownloadResult<HttpClient> {
    let mut conn = pool
        .as_ref()
        .map(|p| tachyon_core::config::ConnectionConfig::from(p.config().clone()))
        .unwrap_or_default();
    // 多分片下载必须多 TCP;H2 单连接复用会抵消并发收益
    conn.enable_http2 = false;
    let arc = crate::http_client_registry::global_http_client_registry().get_or_create(
        &config.user_agent,
        config.proxy.as_deref(),
        config.connect_timeout_secs,
        config.request_timeout_secs,
        Some(&conn),
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

    /// 自动检测磁力链接中的 Web Seed (BEP 19) 并创建混合源下载任务。
    ///
    /// 行为:
    /// - 若磁力链接包含 `ws=` 参数(HTTP web seed URL),且 SSRF 校验通过,
    ///   自动调用 [`with_hybrid_sources`] 创建 HTTP 镜像主源 + BT fallback 的 P2SP 下载。
    /// - 若无 web seed 或非磁力链接,回退到 [`with_pool_and_scheduler`] 纯 BT 路径。
    ///
    /// 调用方无需手动解析磁力链接,只需用此方法替代 `with_pool_and_scheduler`。
    ///
    /// Web seed 叠加后,HTTP 镜像立即提供数据(消除 BT 冷启动等待),
    /// BT 作为整文件 fallback(HTTP 全熔断后接管)。
    #[cfg(feature = "magnet")]
    pub async fn with_magnet_auto_web_seeds(
        url: String,
        config: DownloadConfig,
        pool: Option<Arc<ConnectionPool>>,
        scheduler: Arc<dyn DownloadScheduler>,
        bt_session: Arc<crate::bt_session::BtSession>,
    ) -> DownloadResult<Self> {
        let web_seeds = tachyon_core::extract_web_seeds_from_magnet(&url);
        if !web_seeds.is_empty() {
            tracing::info!(
                count = web_seeds.len(),
                "磁力链接检测到 web seed,创建 HTTP+BT 混合源下载(P2SP)"
            );
            Self::with_hybrid_sources(url, web_seeds, config, pool, scheduler, bt_session).await
        } else {
            Self::with_pool_and_scheduler(url, config, pool, scheduler, Some(bt_session)).await
        }
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
                        .with_session_coordinator(session.session_coordinator())
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
                        expected_checksum: None,
                        rate_limiter: None,
                        metrics: None,
                        circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
                        has_mirrors: false,
                        mirror_source_count: None,
                        goodput_window_start: None,
                        goodput_window_bytes: 0,
                        last_rebalance_at: None,
                        rebalance_enabled: true,
                        soft_pressure_until: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
            expected_checksum: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: false,
            mirror_source_count: None,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            last_rebalance_at: None,
            rebalance_enabled: true,
            soft_pressure_until: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            preferred_file_name: None,
            #[cfg(feature = "magnet")]
            bt_storage_factory: None,
            #[cfg(feature = "magnet")]
            bt_magnet: None,
            #[cfg(feature = "magnet")]
            bt_fallback: None,
        })
    }

    /// bench 专用:用调用方预构造的 `protocol: Arc<dyn Protocol>` 创建任务。
    ///
    /// 与 `with_pool_and_scheduler` 的区别:不内部 `shared_http_client`,允许 bench
    /// 注入自定义客户端(如 `HttpClient::with_danger_accept_invalid_certs` 跑 HTTPS
    /// 自签证书 bench server)。仅 test-harness 编译可用,生产路径不可达。
    #[cfg(any(test, feature = "test-harness"))]
    pub async fn with_protocol(
        url: String,
        config: DownloadConfig,
        pool: Option<Arc<ConnectionPool>>,
        scheduler: Arc<dyn DownloadScheduler>,
        protocol: Arc<dyn Protocol>,
    ) -> DownloadResult<Self> {
        let _ = url::Url::parse(&url)?;
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
            expected_checksum: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: false,
            mirror_source_count: None,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            last_rebalance_at: None,
            rebalance_enabled: true,
            soft_pressure_until: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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

    /// 设置 rebalance 开关(bench 量化 A/B 用;生产不调用,默认 true)。
    ///
    /// `false` 时禁用 `try_rebalance_slowest_fragment`,使两个调用点
    /// (reschedule_timer 分支 + completed_rx 分支)直接跳过,保持其他行为不变。
    /// 用于跑 rebalance on/off A/B 对照,量化动态拆片收益是否 >10%。
    pub fn set_rebalance_enabled(&mut self, enabled: bool) {
        self.rebalance_enabled = enabled;
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
        let mirror_source_count = Some(protocol.source_count());

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
            expected_checksum: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: true,
            mirror_source_count,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            last_rebalance_at: None,
            rebalance_enabled: true,
            soft_pressure_until: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
        let mut mirrors: Vec<(String, Arc<dyn Protocol>)> = http_mirrors
            .iter()
            .filter_map(|m| {
                build_http()
                    .ok()
                    .map(|c| (m.clone(), Arc::new(c) as Arc<dyn Protocol>))
            })
            .collect();

        // BT fallback:独立持有用于 cleanup,同时塞入 MirrorProtocol 作并发源
        // P1-P2SP:BT 加入 sources 参与并发竞速(非仅 HTTP 全失败才 fallback)
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
            .with_session_coordinator(bt_session.session_coordinator())
            .with_storage_factory(bt_factory),
        );
        // P1-P2SP:BT 作为 MirrorProtocol 的并发源之一(与 HTTP 镜像在 least-in-flight
        // 层并发竞速分片)。URL 用 magnet 原始链接(host_of 对无 host 降级不限流)。
        mirrors.push((magnet_url.clone(), bt_fallback.clone() as Arc<dyn Protocol>));
        let protocol = Arc::new(MirrorProtocol::with_pool(primary, mirrors, pool.clone()));
        // 捕获源数量(含 BT)供测试断言,在 protocol move 进 Self 前读取
        let mirror_source_count = Some(protocol.source_count());

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
            expected_checksum: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: true,
            mirror_source_count,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            last_rebalance_at: None,
            rebalance_enabled: true,
            soft_pressure_until: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
            expected_checksum: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: false,
            mirror_source_count: None,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            last_rebalance_at: None,
            rebalance_enabled: true,
            soft_pressure_until: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
            expected_checksum: None,
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            has_mirrors: false,
            mirror_source_count: None,
            goodput_window_start: None,
            goodput_window_bytes: 0,
            last_rebalance_at: None,
            rebalance_enabled: true,
            soft_pressure_until: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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

    /// 注入任务级期望校验和(整文件 hex)。
    ///
    /// 须在 `run()`/`verify()` 前调用。有值时 `verify` 读盘计算整文件哈希并比对;
    /// 亦使 `Require` 在无分片 hash 时不再 fail-fast。
    pub fn set_expected_checksum(&mut self, checksum: Option<String>) {
        self.expected_checksum = checksum.filter(|s| !s.is_empty());
    }

    /// 设置调度器配置(规划参数 / sampling_interval 等)。
    ///
    /// 必须在 `run()` 之前调用。审计 A-04:生产路径从 `AppConfig.scheduler` 注入,
    /// 禁止永远落在 `SchedulerConfig::default()`。
    ///
    /// **同步性**:`plan_fragments` 读 `self.scheduler_config`,而 `recommend()` 读
    /// `AdaptiveDownloadScheduler` 内部 config。仅写一侧会导致 max_fragment_size 等
    /// 分叉(bench/测试只 set_scheduler_config 时尤为明显)。此处在调度器 Arc 唯一
    /// (strong_count==1)时重建同源 `AdaptiveDownloadScheduler`;若已被共享则只更新
    /// plan 侧并 warn(生产路径应在构造时 `create_adaptive_scheduler(config)` 同源)。
    pub fn set_scheduler_config(&mut self, config: SchedulerConfig) {
        self.scheduler_config = config.clone();
        if Arc::strong_count(&self.scheduler) == 1 {
            self.scheduler = create_adaptive_scheduler(config);
        } else {
            tracing::warn!(
                strong_count = Arc::strong_count(&self.scheduler),
                "set_scheduler_config:调度器 Arc 已共享,仅更新 plan 侧 scheduler_config;recommend 仍用旧内部配置"
            );
        }
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
        debug!(url = %tachyon_core::redact_url_for_log(&self.url), "开始探测文件元数据");
        // 测量 probe 耗时作为 RTT 上界估计(DNS+TCP+TLS+HTTP 往返)。
        // 偏大的 RTT 估计使 BDP 偏大(倾向更多并发),比偏小(管道未满)安全。
        // observe_rtt 内部会过滤异常值(>10s),正常 probe 耗时 50ms-2s 均有效。
        //
        // 可重试错误(TLS handshake eof / 连接超时 / 5xx 等)按 max_retries 退避:
        // 旧路径 probe 失败直接终态,代理抖动/瞬态 TLS 失败会把整任务打死。
        let max_retries = self.config.max_retries;
        let mut attempt = 0u32;
        // 单次 probe attempt 墙钟上限:取 connect_timeout,钳制到 5..=10s。
        // 代理黑洞时避免 HEAD 挂到 request_timeout(120s)×重试 打满墙钟。
        let probe_attempt_timeout = {
            let c = self.config.connect_timeout_secs.max(1);
            Duration::from_secs(c.clamp(5, 10))
        };
        let (mut metadata, probe_elapsed) = loop {
            let probe_start = std::time::Instant::now();
            let probe_fut = self.protocol.probe(&self.url);
            let timed = tokio::time::timeout(probe_attempt_timeout, probe_fut).await;
            let result = match timed {
                Ok(inner) => inner,
                Err(_) => Err(DownloadError::Timeout(format!(
                    "probe 超过 {}s",
                    probe_attempt_timeout.as_secs()
                ))),
            };
            match result {
                Ok(meta) => break (meta, probe_start.elapsed()),
                Err(e) => {
                    if e.is_retryable() && attempt < max_retries {
                        let next = attempt + 1;
                        let is_403 = matches!(
                            &e,
                            DownloadError::Forbidden { status: 403 }
                                | DownloadError::Http { status: 403, .. }
                        );
                        // 403 常是 WAF/签名永久拒绝:短退避即可,勿走 soft-pressure 2/4/8/16s。
                        // TLS EOF/5xx 才需要长冷却。
                        let base = Duration::from_secs((1u64 << attempt.min(4)).max(1));
                        let is_timeout = matches!(e, DownloadError::Timeout(_));
                        let backoff = if is_403 || is_timeout {
                            // 超时/403:短退避快速换路径(HEAD→Range→GET),勿 2/4/8s 空等
                            Duration::from_millis(
                                200u64.saturating_mul(next as u64).clamp(200, 800),
                            )
                        } else if Self::is_connection_soft_pressure(&e) {
                            Self::soft_pressure_backoff_secs(attempt, base)
                        } else {
                            base
                        };
                        if !is_403 && !is_timeout && Self::is_connection_soft_pressure(&e) {
                            Self::extend_soft_pressure_cooldown(
                                &self.soft_pressure_until,
                                Duration::from_secs(15),
                            );
                        }
                        warn!(
                            attempt = next,
                            max_retries,
                            backoff_ms = backoff.as_millis() as u64,
                            error = %e,
                            "probe 可重试失败,退避后重试"
                        );
                        tokio::time::sleep(backoff).await;
                        attempt = next;
                        continue;
                    }
                    return Err(e);
                }
            }
        };
        // scheduler.observe_rtt 会丢弃 >10s;代理冷启动时 11s probe 若丢弃会落回默认 50ms,
        // 误判为低延迟高并发。钳制到 [1ms, 10s] 保留"高延迟"信号。
        let rtt_for_sched = probe_elapsed
            .min(Duration::from_secs(10))
            .max(Duration::from_millis(1));
        self.scheduler.observe_rtt(rtt_for_sched);
        debug!(
            ?probe_elapsed,
            ?rtt_for_sched,
            "probe 耗时已作为 RTT 上界注入调度器"
        );
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
        debug!(
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
                    storages.push(
                        DynStorage::open_with_strategy_and_concurrency(
                            p,
                            self.config.io_strategy,
                            self.config.max_concurrent_fragments,
                        )
                        .await?,
                    );
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
                let s = DynStorage::open_with_strategy_and_concurrency(
                    &canonical_path,
                    self.config.io_strategy,
                    self.config.max_concurrent_fragments,
                )
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
            let s = DynStorage::open_with_strategy_and_concurrency(
                &canonical_path,
                self.config.io_strategy,
                self.config.max_concurrent_fragments,
            )
            .await?;
            StorageSet::single(s)
        };
        self.storage = Some(Arc::new(storage));
        Ok(())
    }

    // ----- BT/magnet 冷启动解耦 -----

    /// BT/magnet 分片目标数量:独立于 HTTP `default_target_fragments`(现为 64)。
    /// 调度器带宽样本只在分片完成时产生;BT 慢 swarm 下过粗分片迟迟不完,
    /// 0 样本 → confidence 恒 0 → ramp 锁死冷启动并发,反馈环路断裂。
    /// 固定 32 目标数让完成事件更早到来(与 HTTP 目标数解耦,勿随 64 同步翻倍)。
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

    /// 向 progress 通道上报 BT peer 发现快照(可丢)。
    ///
    /// 仅 magnet/BT 任务且持有 `bt_magnet`/`bt_fallback` 时生效;HTTP 空操作。
    /// UI 用于「0 peer / 发现中」提示,不参与正确性。
    #[cfg(feature = "magnet")]
    fn try_emit_peer_stats(&self) {
        let Some(tx) = self.progress_tx.as_ref() else {
            return;
        };
        let Some(magnet) = self.bt_magnet.as_ref().or(self.bt_fallback.as_ref()) else {
            return;
        };
        let Some(stats) = magnet.peer_stats_snapshot(&self.url) else {
            // 尚无 live 统计时也推 0,让 UI 显示「发现中」而非空白
            let _ = tx.try_send(FragmentProgress::PeerStats {
                live: 0,
                connecting: 0,
                queued: 0,
            });
            return;
        };
        let _ = tx.try_send(FragmentProgress::PeerStats {
            live: stats.live_peers as u32,
            connecting: stats.connecting_peers as u32,
            queued: stats.queued_peers as u32,
        });
    }

    #[cfg(not(feature = "magnet"))]
    fn try_emit_peer_stats(&self) {}

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
        // 首下 HTTP:不采用 recommendation.fragment_size。scheduler 为 per-task 实例,
        // plan 阶段尚无本任务带宽样本,confidence 恒为 0;历史上的
        // `confidence > 0` 分支在生产冷启动路径上不可达。若将来要在 plan 阶段
        // 激活跨任务带宽建议,需先做 scheduler 跨任务共享,再恢复该分支。
        // 当前一律回退 plan_fragments + scheduler_config.default_target_fragments。
        let has_resume_snapshot =
            !self.completed_fragments.is_empty() || !self.partial_fragments.is_empty();
        // 保留 is_bt_task / has_resume_snapshot 分支结构:
        // - BT: 固定小分片公式
        // - resume: None → plan_fragments 确定性划分(禁止 recommendation 漂移边界)
        // - 首下 HTTP: None → scheduler_config.default_target_fragments
        //   (不采用 recommendation.fragment_size; 见上方注释)
        #[allow(clippy::if_same_then_else)]
        let suggested_frag_size = if self.is_bt_task() {
            Some(Self::bt_fragment_size(file_size))
        } else if has_resume_snapshot {
            None
        } else {
            None
        };

        let fragments = crate::fragment::plan_fragments(
            file_size,
            metadata.supports_range,
            suggested_frag_size,
            &self.scheduler_config,
        )?;

        debug!(count = fragments.len(), "分片规划完成");

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
            debug!(resumed, "断点续传:跳过已完成分片");
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
            debug!(resumed_partial, "字节级断点续传:恢复未完整分片");
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
            let initial_concurrency = match self.bt_cold_start_concurrency_override(&recommendation)
            {
                Some(c) => c,
                None => {
                    let mut c = recommendation.concurrency.max(1);
                    if let Some(cap) =
                        self.proxy_cold_start_cap_for_config(recommendation.confidence)
                    {
                        c = c.min(cap).max(1);
                    }
                    self.apply_proxy_concurrency_ceiling(c)
                }
            };
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

    // ----- 一键运行 -----

    /// 一键执行完整下载流程
    ///
    /// 依次执行: 探测 -> 规划 -> 预分配 -> 下载 -> 校验
    /// 任一步骤失败将标记任务为 `Failed` 并返回错误。
    #[tracing::instrument(skip(self), fields(url = %tachyon_core::redact_url_for_log(&self.url)))]
    pub async fn run(&mut self) -> DownloadResult<()> {
        debug!(url = %tachyon_core::redact_url_for_log(&self.url), "启动下载任务");

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

        // Require 且 plan 后仍无任何 expected hash(分片级或任务级):在发起字节下载前 fail-fast,
        // 避免完整下载后再在 verify 抛 NoExpectedChecksum 的陷阱。
        let has_any_expected = self.expected_checksum.is_some()
            || self.fragments.iter().any(|f| f.info.hash.is_some());
        if self.config.verify_checksum
            && self.config.verify_strategy == tachyon_core::config::VerifyStrategy::Require
            && !has_any_expected
        {
            self.state = DownloadState::Failed;
            return Err(DownloadError::NoExpectedChecksum);
        }

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

        // 步骤 4: 执行下载
        //
        // **禁止** take 走 control_rx:execute 内部靠 watch_for_interrupt / check_control
        // 协作式处理 Pause 与 Cancel。若此处 take,Pause 信号进不了热路径,
        // UI 显示暂停但 IO 继续(v0.1.3 用户报告)。
        // Cancel 同样由 execute 内部 select 穿透;外层不再重复 wait_for_cancel。
        //
        // P1-P2SP:HTTP 镜像与 BT 在 MirrorProtocol 的 least-in-flight 层并发竞速分片
        // (BT 已作为 sources 之一加入,非 HTTP 全失败才 fallback)。execute 失败即整体
        // 失败(含 BT 路径在内已全部尝试)。纯 BT 路径(bt_magnet)仍走单协议 execute。
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
        debug!("下载任务完成");
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
    /// **且**失败错误不是 `Cancelled`/`Paused`。纯 HTTP / 纯 BT 路径无
    /// `bt_fallback`,不触发,失败直接向上传播。
    ///
    /// **排除 `Cancelled` / `Paused`**:用户主动取消或协作暂停是确定的控制语义,
    /// 不应再启动一次无意义的 BT 整文件下载,也不应掩盖取消/暂停语义。
    ///
    /// **layout 兼容性**:严格 fallback 需「单文件 BT + 单文件 HTTP + 大小一致」才允许,
    /// 该校验在 `execute_bt_fallback` 内通过 BT `probe()` metadata 比对实现(见其文档)。
    #[cfg_attr(not(test), allow(dead_code))] // P1-P2SP 后生产不再调用,仅测试断言判定逻辑
    #[cfg(feature = "magnet")]
    fn should_try_bt_fallback(&self, err: &DownloadError) -> bool {
        self.bt_fallback.is_some()
            && !matches!(err, DownloadError::Cancelled | DownloadError::Paused)
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
    #[allow(dead_code)]
    // P1-P2SP 后 BT 在 MirrorProtocol 内并发,此整文件 fallback 不再被调用,保留供未来退化路径
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
                        self.metrics.as_deref(),
                    )
                    .await?;
                    pos = pos.checked_add(written).ok_or_else(|| {
                        DownloadError::Other(
                            format!("BT fallback 偏移溢出: {pos}+{written}").into(),
                        )
                    })?;
                }
                let written = Self::write_all_at(
                    &storage,
                    pos,
                    chunk,
                    &mut self.control_rx,
                    pause_timeout,
                    self.metrics.as_deref(),
                )
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
                    self.metrics.as_deref(),
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
                    self.metrics.as_deref(),
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
                self.metrics.as_deref(),
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

    fn set_expected_checksum(&mut self, checksum: Option<String>) {
        self.set_expected_checksum(checksum);
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
#[path = "downloader_tests.rs"]
mod tests;
