//! QuantumFetch P2SP 混合下载、DHT、Peer 发现
//!
//! 实现 P2SP 混合下载能力:
//! - Kademlia DHT 协议
//! - Peer 发现与管理
//! - 多源选择算法(CDN + P2P)

pub mod dht;
pub mod peer;
pub mod source;

pub use dht::KademliaDht;
pub use peer::{PeerInfo, PeerScore};
pub use source::{DownloadSource, SourceSelector};
