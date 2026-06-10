//! Tachyon 调度层:智能调度、带宽分配、预测
//!
//! 实现下载任务的智能调度:
//! - Holt 双指数平滑带宽预测
//! - 优先级队列
//! - 连接分配策略
//! - 任务生命周期管理

pub mod download_scheduler;
pub mod predictor;
pub mod scheduler;

pub use download_scheduler::AdaptiveDownloadScheduler;
pub use predictor::HoltLinearPredictor;
pub use scheduler::{Priority, ScheduledTask, Scheduler};
