//! Kademlia DHT 实现
//!
//! 基于 Kademlia 协议的分布式哈希表,用于 Peer 发现。
//! 包含 XOR 距离度量、k-bucket 路由表、迭代查找算法。
//!
//! # 模块结构
//!
//! - [`node`] — `NodeId`、`DhtNode` 类型和 XOR 距离计算
//! - [`message`] — `KademliaMessage` 枚举和 BLAKE3 完整性校验
//! - [`kbucket`] — `KBucket` 路由表结构、Sybil 防护和活动时间追踪
//! - [`kademlia`] — `KademliaDht` 核心逻辑、本地存储和 Bucket Refresh
//! - [`transport`] — UDP 传输层、迭代查找和周期性刷新循环
//!
//! # TODO: 架构优化路线图
//!
//! DHT 模块拆分 (A-09)、Bucket Refresh (A-10)、二进制序列化 (A-11) 均已完成。
//! 后续可考虑的优化方向:
//! - Ed25519 非对称签名替代 BLAKE3 keyed hash
//! - COBS 帧编码支持流式传输
//! - 增量路由表持久化

pub(crate) mod kademlia;
pub(crate) mod kbucket;
pub(crate) mod message;
pub(crate) mod node;
pub(crate) mod transport;

// 重新导出所有公共 API,保持向后兼容
pub use kademlia::KademliaDht;
pub use kbucket::{KBucket, RoutingTable};
pub use message::{KademliaMessage, sign_message, verify_message_signature};
pub use node::{
    ALPHA, DhtNode, K_BUCKET_SIZE, NodeId, generate_node_id, leading_zeros, xor_distance,
};
pub use transport::{DhtTransport, TransportError};
