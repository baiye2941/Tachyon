//! Tachyon P2SP 混合下载、Peer 发现
//!
//! 实现 P2SP 混合下载能力:
//! - Peer 发现与管理
//! - 多源选择算法(CDN + P2P)

pub mod peer;
pub mod source;

pub use peer::{PeerInfo, PeerScore};
pub use source::{DownloadSource, SourceSelector};

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
