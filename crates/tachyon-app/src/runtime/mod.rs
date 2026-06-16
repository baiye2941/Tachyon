//! 运行时管理层
//!
//! 负责下载任务的运行时生命周期：
//! - 启动/停止下载任务
//! - JoinHandle 生命周期管理
//! - 控制命令通道（暂停/恢复/取消）
//! - 运行时资源清理
//! - 共享 ChunkReader 工作池

pub mod chunk_reader_pool;
pub mod download_session;
pub mod download_supervisor;

pub use chunk_reader_pool::{ChunkReaderJob, ChunkReaderPool};
pub use download_session::DownloadSession;
pub use download_supervisor::DownloadSupervisor;
