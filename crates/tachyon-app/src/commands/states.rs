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

use tokio::sync::Mutex;

use tachyon_core::config::AppConfig;
use tachyon_engine::connection::ConnectionPool;
use tachyon_io::BufferPool;

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
}

/// 基础设施状态：连接、存储、I/O 池
#[derive(Clone)]
pub struct InfraState {
    pub connection_pool: Arc<ConnectionPool>,
    pub task_store: Arc<TaskStore>,
    pub chunk_reader_pool: Arc<ChunkReaderPool>,
    /// 全局 buffer 池：供下载 worker 复用写盘 buffer,带 Semaphore 反压。
    /// 容量 = max_concurrent_tasks × max_concurrent_fragments,buffer_size = WRITE_BATCH_BYTES。
    pub buffer_pool: Arc<BufferPool>,
    /// BitTorrent Session 单例（可选，magnet feature 启用时可用）
    #[cfg(feature = "magnet")]
    pub bt_session: Option<Arc<tachyon_engine::BtSession>>,
}

/// 应用服务状态：业务服务层
#[derive(Clone)]
pub struct ServiceState {
    pub task_service: Arc<TaskService>,
    pub sniffer_service: Arc<SnifferService>,
    pub confirmation_service: Arc<ConfirmationService>,
}

/// 运行时时状态：任务生命周期与进度广播
#[derive(Clone)]
pub struct RuntimeState {
    pub supervisor: Arc<DownloadSupervisor>,
    pub progress_broker: Arc<ProgressBroker>,
    pub progress_subscribed: Arc<AtomicBool>,
}
