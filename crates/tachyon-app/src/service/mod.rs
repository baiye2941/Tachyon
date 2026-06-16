//! 应用服务层
//!
//! 从 Tauri command 层提取的业务逻辑，负责：
//! - 任务创建的业务规则（URL 校验、并发门控、去重、目录授权）
//! - 任务状态变更的业务规则（暂停/恢复/取消/删除的前置条件校验）
//! - TaskInfo 与持久化 snapshot 的协调
//! - 嗅探器资源管理和过滤规则校验

pub mod confirmation_service;
pub mod sniffer_service;
pub mod task_service;

pub use confirmation_service::{ConfirmationService, try_claim_subscription};
pub use sniffer_service::SnifferService;
pub use task_service::{TaskCreation, TaskService};
