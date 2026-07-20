//! 应用状态分组
//!
//! 将原本扁平的 [`AppState`] 按职责拆分为四个独立的状态组：
//! - [`DomainState`][]: 领域数据（任务仓库、配置）
//! - [`InfraState`][]: 基础设施（连接池、持久化、I/O 池）
//! - [`ServiceState`][]: 应用服务（任务、嗅探、确认）
//! - [`RuntimeState`][]: 运行时管理（调度器、进度代理、订阅标志）
//!
//! 每个状态组可独立克隆并在 Tauri 中作为独立 `State` 管理；
//! 当前阶段先作为 [`AppState`] 的聚合字段完成代码层面的解耦。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::{Mutex, RwLock};

use tachyon_core::config::AppConfig;
use tachyon_engine::BufferPool;
use tachyon_engine::ConnectionPool;

use crate::projection::ProgressBroker;
use crate::repository::TaskRepository;
use crate::runtime::{ChunkReaderPool, DownloadSupervisor};
use crate::service::{ConfirmationService, SnifferService, TaskService};
use crate::task_store::TaskStore;

/// 领域状态：任务仓库与应用配置
#[derive(Clone)]
pub struct DomainState {
    pub task_repository: TaskRepository,
    pub config: Arc<Mutex<AppConfig>>,
    /// 配置文件持久化路径(测试隔离注入点):
    /// 生产为 `dirs()/.tachyon/config.json`,测试指向临时目录下的独立文件,
    /// 避免 update_config / authorize_download_directory 等 persist 路径写穿真实用户配置。
    pub config_path: std::path::PathBuf,
}

/// 基础设施状态：连接、存储、I/O 池
#[derive(Clone)]
pub struct InfraState {
    /// 全局并发许可器(历史名 ConnectionPool;审计 A-02 语义为 ConcurrencyLimiter)
    ///
    /// **不**持有 TCP 连接。TCP/TLS/H2 复用由 reqwest Client + HttpClientRegistry 负责。
    /// 本字段只限制 per-host / 全局并发请求许可。
    ///
    /// 外层 `Arc<RwLock<...>>` 用于在 `update_config` 时热重建:
    /// - 写路径(update_config):写锁内替换内层 `Arc<ConnectionPool>`,
    ///   重建出携带新配置的新许可器;
    /// - 读路径(task_fn 启动 / supervisor.start_download):读锁内 clone
    ///   出当前 `Arc` 传入任务。
    ///
    /// 运行中的任务持有旧 Arc,自然存活至引用释放;新任务拿新许可器。
    /// 注意:许可器热替换与 HttpClient 配置生命周期独立(见 HTTP-15 注册表)。
    pub connection_pool: Arc<RwLock<Arc<ConnectionPool>>>,
    pub task_store: Arc<TaskStore>,
    /// 收藏 KV 存储（独立目录，与任务存储分离）
    pub favorites_store: Arc<tachyon_store::KvStore>,
    pub chunk_reader_pool: Arc<ChunkReaderPool>,
    /// 全局 buffer 池：供下载 worker 复用写盘 buffer,带 Semaphore 反压。
    /// 容量 = max_concurrent_tasks × max_concurrent_fragments,buffer_size = WRITE_BATCH_BYTES。
    ///
    /// 审计 A-14:外层 `Arc<RwLock<...>>` 支持 `update_config` 热重建容量;
    /// 运行中任务持有旧 `Arc<BufferPool>`,新任务读锁 clone 当前池。
    pub buffer_pool: Arc<RwLock<Arc<BufferPool>>>,
    /// 审计 A-03:跨任务共享的全局令牌桶限速器。
    ///
    /// 所有 `build_download_task` 注入同一 `Arc`;`update_config` 调用 `update_rate`
    /// 即时生效。`rate_limit_bytes_per_sec=None` 时速率为 0(不限速)。
    pub global_rate_limiter: Arc<tachyon_engine::RateLimiter>,
    /// BitTorrent Session (magnet:? 链接下载)
    ///
    /// 使用 `Arc<Mutex<Option<...>>>` 包装,原因:
    /// - `BtSession::new()` 是 async,无法在 `AppState::try_new()`(sync) 中初始化
    /// - 需在 Tauri setup 的异步块中延迟初始化
    /// - Mutex 保证初始化与读取的互斥安全
    #[cfg(feature = "magnet")]
    pub bt_session: Arc<Mutex<Option<Arc<tachyon_engine::BtSession>>>>,
}

/// 应用服务状态：业务服务层
#[derive(Clone)]
pub struct ServiceState {
    pub task_service: Arc<TaskService>,
    pub sniffer_service: Arc<SnifferService>,
    pub confirmation_service: Arc<ConfirmationService>,
}

/// 运行时时状态:任务生命周期与进度广播
#[derive(Clone)]
pub struct RuntimeState {
    pub supervisor: Arc<DownloadSupervisor>,
    pub progress_broker: Arc<ProgressBroker>,
    pub progress_subscribed: Arc<AtomicBool>,
    /// 启动恢复告警(损坏快照)。setup 阶段在 Tauri 事件监听器注册前就绪,
    /// 直接 emit 会被前端漏接,故暂存于此供 `get_recovery_warning` 命令拉取,
    /// 前端 mount 时主动查询以补全遗漏事件(P1-22-3)。
    pub recovery_warning: Arc<Mutex<Option<crate::commands::RecoveryWarning>>>,
}
