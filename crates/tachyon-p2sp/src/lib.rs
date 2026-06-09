//! Tachyon P2SP 混合下载、DHT、Peer 发现
//!
//! 实现 P2SP 混合下载能力:
//! - Kademlia DHT 协议
//! - Peer 发现与管理
//! - 多源选择算法(CDN + P2P)

pub mod dht;
pub mod peer;
pub mod source;

pub use dht::DhtNode;
pub use dht::{
    ALPHA, K_BUCKET_SIZE, KBucket, KademliaDht, KademliaMessage, NodeId, RoutingTable,
    DhtTransport, TransportError, generate_node_id, xor_distance,
};
pub use peer::{PeerInfo, PeerScore};
pub use source::{DownloadSource, SourceSelector};

#[cfg(test)]
#[test]
/// 测试 Kademlia DHT: XOR 距离度量与 k-bucket 节点管理
fn kademlia() {
    use dht::{DhtNode, KademliaDht, NodeId, xor_distance};

    // === XOR 距离度量 ===
    // 相同节点 XOR 距离为零
    let a: NodeId = [0xAA; 20];
    assert_eq!(xor_distance(&a, &a), [0u8; 20]);

    // 不同节点 XOR 距离非零
    let b: NodeId = [0x55; 20];
    let dist = xor_distance(&a, &b);
    assert_eq!(dist, [0xFF; 20]); // 0xAA XOR 0x55 = 0xFF

    // XOR 距离对称性: d(a,b) == d(b,a)
    assert_eq!(xor_distance(&a, &b), xor_distance(&b, &a));

    // 部分不同
    let mut c = [0u8; 20];
    c[0] = 0x01;
    let dist_ac = xor_distance(&a, &c);
    // 首字节 0xAA XOR 0x01 = 0xAB,其余 0xAA XOR 0x00 = 0xAA
    assert_eq!(dist_ac[0], 0xAB);
    assert_eq!(dist_ac[1], 0xAA);

    // === KademliaDht 节点管理 ===
    let mut dht = KademliaDht::new([0u8; 20], 100);
    assert_eq!(dht.node_count(), 0);
    assert_eq!(dht.local_id(), &[0u8; 20]);

    // 添加节点
    let node_a = DhtNode::new([1u8; 20], "10.0.0.1:6881".to_string());
    dht.add_node(node_a);
    assert_eq!(dht.node_count(), 1);

    let node_b = DhtNode::new([2u8; 20], "10.0.0.2:6881".to_string());
    dht.add_node(node_b);
    assert_eq!(dht.node_count(), 2);

    // 活跃节点检查(刚添加的不应过期)
    assert_eq!(dht.active_nodes().len(), 2);

    // 清理过期节点(刚添加的不应被清理)
    dht.cleanup_stale();
    assert_eq!(dht.node_count(), 2);

    // === max_nodes 驱逐 ===
    let mut small_dht = KademliaDht::new([0u8; 20], 2);
    small_dht.add_node(DhtNode::new([1u8; 20], "10.0.0.1:6881".to_string()));
    small_dht.add_node(DhtNode::new([2u8; 20], "10.0.0.2:6881".to_string()));
    small_dht.add_node(DhtNode::new([3u8; 20], "10.0.0.3:6881".to_string()));
    assert!(small_dht.node_count() <= 2, "超过 max_nodes 时应驱逐旧节点");

    // 手动构造过期节点验证清理
    let mut dht2 = KademliaDht::new([0u8; 20], 100);
    dht2.add_node(DhtNode::new([1u8; 20], "10.0.0.1:6881".to_string()));
    // 插入一个过期节点(手动设置 last_seen 为很久以前)
    {
        let mut stale_node = DhtNode::new([9u8; 20], "10.0.0.9:6881".to_string());
        // 构造过期节点(手动设置 last_seen 为很久以前)
        stale_node.last_seen = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        dht2.add_node(stale_node);
    }
    assert_eq!(dht2.node_count(), 2);
    dht2.cleanup_stale();
    assert_eq!(dht2.node_count(), 1, "过期节点应被清理");
}

#[cfg(test)]
#[test]
/// 测试 P2P 网络类型:Peer 评分、下载源、源选择器基本操作
fn p2p_network() {
    use peer::{PeerInfo, PeerScore};
    use source::{DownloadSource, SourceSelector};

    // === Peer 评分 ===
    let default_score = PeerScore::default();
    assert!(default_score.weighted_score() > 0.0, "默认评分应为正");

    // 高带宽 CDN 评分优于普通 Peer
    let fast_score = PeerScore {
        latency_ms: 10,
        bandwidth_bps: 100 * 1024 * 1024,
        stability: 0.95,
        distance: 5,
    };
    let slow_score = PeerScore {
        latency_ms: 500,
        bandwidth_bps: 512 * 1024,
        stability: 0.3,
        distance: 200,
    };
    assert!(
        fast_score.weighted_score() > slow_score.weighted_score(),
        "快速 Peer 评分应高于慢速 Peer"
    );

    // === PeerInfo 基本操作 ===
    let peer = PeerInfo::new("10.0.0.5:6881".to_string());
    assert!(peer.available);
    assert_eq!(peer.addr, "10.0.0.5:6881");

    // === DownloadSource 变体 ===
    let cdn = DownloadSource::Cdn {
        url: "https://cdn.example.com/file.bin".to_string(),
    };
    let peer_src = DownloadSource::Peer {
        addr: "192.168.1.50:6881".to_string(),
    };
    assert_eq!(cdn.key(), "https://cdn.example.com/file.bin");
    assert_eq!(peer_src.key(), "192.168.1.50:6881");

    // === SourceSelector 基本操作 ===
    let mut selector = SourceSelector::new();
    assert_eq!(selector.source_count(), 0);

    selector.add_source(
        DownloadSource::Cdn {
            url: "https://fast-cdn.com/big.iso".to_string(),
        },
        PeerScore {
            latency_ms: 15,
            bandwidth_bps: 80 * 1024 * 1024,
            stability: 0.95,
            distance: 5,
        },
    );
    selector.add_source(
        DownloadSource::Peer {
            addr: "10.0.0.99:6881".to_string(),
        },
        PeerScore {
            latency_ms: 300,
            bandwidth_bps: 2 * 1024 * 1024,
            stability: 0.5,
            distance: 150,
        },
    );
    assert_eq!(selector.source_count(), 2);

    // select_source 应返回某个源
    let selected = selector.select_source();
    assert!(selected.is_some(), "有源时不应返回 None");

    // 移除 CDN 源
    selector.remove_source("https://fast-cdn.com/big.iso");
    assert_eq!(selector.source_count(), 1);
    let remaining = selector.select_source().unwrap();
    assert_eq!(remaining.key(), "10.0.0.99:6881");

    // 移除最后一个源
    selector.remove_source("10.0.0.99:6881");
    assert!(selector.select_source().is_none(), "无源时应返回 None");
}
