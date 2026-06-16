//! 通用工具函数与可观测性类型
//!
//! 子模块:
//! - [`metrics`][]: 无锁下载指标计数器 (Metrics)
//! - [`hex`][]: 高性能十六进制编码 (hex_encode)

pub mod hex;
pub mod metrics;

pub use hex::hex_encode;
pub use metrics::Metrics;
