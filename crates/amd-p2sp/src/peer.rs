//! Peer 发现与管理
//!
//! 管理下载任务的 Peer 列表,实现多源选择算法。

/// Peer 评分因子
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerScore {
    /// 延迟权重(30%)
    pub latency_ms: u32,
    /// 带宽权重(40%)
    pub bandwidth_bps: u64,
    /// 稳定性权重(20%):成功下载次数 / 总尝试次数
    pub stability: f64,
    /// 距离权重(10%):DHT 距离
    pub distance: u32,
}

impl PeerScore {
    /// 计算综合评分(越高越好)
    pub fn weighted_score(&self) -> f64 {
        // 延迟分:越低越好,归一化到 0~1
        let latency_score = 1.0 / (1.0 + self.latency_ms as f64 / 1000.0);
        // 带宽分:越高越好,归一化
        let bandwidth_score = (self.bandwidth_bps as f64 / (100.0 * 1024.0 * 1024.0)).min(1.0);
        // 稳定性分:直接使用 0~1
        let stability_score = self.stability.clamp(0.0, 1.0);
        // 距离分:越近越好
        let distance_score = 1.0 / (1.0 + self.distance as f64 / 256.0);

        latency_score * 0.3 + bandwidth_score * 0.4 + stability_score * 0.2 + distance_score * 0.1
    }
}

impl PartialEq for PeerScore {
    fn eq(&self, other: &Self) -> bool {
        self.latency_ms == other.latency_ms
            && self.bandwidth_bps == other.bandwidth_bps
            && (self.stability - other.stability).abs() < 1e-10
            && self.distance == other.distance
    }
}

impl Eq for PeerScore {}

impl Default for PeerScore {
    fn default() -> Self {
        Self {
            latency_ms: 100,
            bandwidth_bps: 1024 * 1024,
            stability: 0.5,
            distance: 128,
        }
    }
}

/// Peer 信息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Default)]
pub struct PeerInfo {
    pub addr: String,
    pub score: PeerScore,
    pub available: bool,
}

impl PeerInfo {
    pub fn new(addr: String) -> Self {
        tracing::debug!(peer = %addr, "Peer 连接建立");
        Self {
            addr,
            score: PeerScore::default(),
            available: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_score() {
        let score = PeerScore::default();
        assert!(score.weighted_score() > 0.0);
    }

    #[test]
    fn test_low_latency_better() {
        let low_latency = PeerScore {
            latency_ms: 10,
            ..PeerScore::default()
        };
        let high_latency = PeerScore {
            latency_ms: 1000,
            ..PeerScore::default()
        };
        assert!(low_latency.weighted_score() > high_latency.weighted_score());
    }

    #[test]
    fn test_high_bandwidth_better() {
        let high_bw = PeerScore {
            bandwidth_bps: 100 * 1024 * 1024,
            ..PeerScore::default()
        };
        let low_bw = PeerScore {
            bandwidth_bps: 1024 * 1024,
            ..PeerScore::default()
        };
        assert!(high_bw.weighted_score() > low_bw.weighted_score());
    }

    #[test]
    fn test_peer_creation() {
        let peer = PeerInfo::new("192.168.1.1:8080".to_string());
        assert!(peer.available);
        assert_eq!(peer.addr, "192.168.1.1:8080");
    }
}
