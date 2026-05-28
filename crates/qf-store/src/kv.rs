//! 嵌入式 KV 存储（向后兼容模块）
//!
//! `KvStore` 的实际实现已移至 [`super::store`] 模块。
//! 本模块仅为向后兼容提供重导出。

// 重导出，保持 `crate::kv::KvStore` 路径可用
pub use super::store::KvStore;
