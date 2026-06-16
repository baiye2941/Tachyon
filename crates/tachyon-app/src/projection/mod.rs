//! 投影层
//!
//! 负责将后端状态投影到前端可见的进度事件。
//! 不包含业务逻辑，只做状态聚合与事件广播。

pub mod progress_broker;

pub use progress_broker::ProgressBroker;
