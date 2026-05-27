//! 多源选择算法与下载源管理
//!
//! 为每个分片选择最优下载源(CDN 或 P2P Peer),
//! 使用加权随机算法确保高评分源被优先选中。

use crate::peer::PeerScore;
use std::sync::atomic::{AtomicU64, Ordering};

/// 下载源类型
#[derive(Debug, Clone)]
pub enum DownloadSource {
    /// CDN 源
    Cdn { url: String },
    /// P2P Peer 源
    Peer { addr: String },
}

impl DownloadSource {
    /// 获取源的标识符(URL 或地址)
    pub fn key(&self) -> &str {
        match self {
            DownloadSource::Cdn { url } => url,
            DownloadSource::Peer { addr } => addr,
        }
    }
}

/// 源选择器,为每个分片选择最优下载源
///
/// 使用加权随机算法:评分越高的源被选中的概率越大。
/// 权重 = max(0, weighted_score),确保非负。
#[derive(Debug)]
pub struct SourceSelector {
    sources: Vec<(DownloadSource, PeerScore)>,
}

impl SourceSelector {
    /// 创建空的源选择器
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// 添加下载源
    pub fn add_source(&mut self, source: DownloadSource, score: PeerScore) {
        self.sources.push((source, score));
    }

    /// 移除下载源
    pub fn remove_source(&mut self, url_or_addr: &str) {
        self.sources.retain(|(src, _)| src.key() != url_or_addr);
    }

    /// 为分片选择最优源(加权随机)
    ///
    /// 权重为综合评分,评分越高的源被选中概率越大。
    /// 如果所有源评分为零或负数,则均匀随机选择。
    pub fn select_source(&self) -> Option<&DownloadSource> {
        if self.sources.is_empty() {
            return None;
        }

        // 计算每个源的权重(确保非负)
        let weights: Vec<f64> = self
            .sources
            .iter()
            .map(|(_, score)| score.weighted_score().max(0.0))
            .collect();

        let total: f64 = weights.iter().sum();

        // 所有权重为零时,均匀随机选择
        if total <= 0.0 {
            let idx = random_index(self.sources.len());
            return self.sources.get(idx).map(|(src, _)| src);
        }

        // 生成 [0, total) 范围内的随机值
        let roll = random_f64() * total;

        let mut cumulative = 0.0;
        for (i, w) in weights.iter().enumerate() {
            cumulative += *w;
            if roll < cumulative {
                return self.sources.get(i).map(|(src, _)| src);
            }
        }

        // 浮点精度回退,选最后一个
        self.sources.last().map(|(src, _)| src)
    }

    /// 获取源数量
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// 根据性能反馈更新源评分
    pub fn update_score(&mut self, url_or_addr: &str, latency_ms: u32, bandwidth_bps: u64) {
        if let Some((_, score)) = self
            .sources
            .iter_mut()
            .find(|(src, _)| src.key() == url_or_addr)
        {
            score.latency_ms = latency_ms;
            score.bandwidth_bps = bandwidth_bps;
        }
    }

    /// 获取所有源的只读引用
    pub fn sources(&self) -> &[(DownloadSource, PeerScore)] {
        &self.sources
    }
}

impl Default for SourceSelector {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局 PRNG 状态(Xorshift64)
static PRNG_STATE: AtomicU64 = AtomicU64::new(0);

/// 初始化 PRNG 种子(仅首次调用时生效)
fn ensure_prng_seeded() {
    if PRNG_STATE.load(Ordering::Relaxed) == 0 {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        // 确保种子非零(零是 Xorshift64 的不动点)
        let seed = if seed == 0 { 1 } else { seed };
        let _ = PRNG_STATE.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// Xorshift64 下一步,返回新的随机状态
fn xorshift64_next(state: u64) -> u64 {
    let mut x = state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

/// 生成 [0.0, 1.0) 范围内的伪随机浮点数(有状态,Xorshift64)
fn random_f64() -> f64 {
    ensure_prng_seeded();
    let old = PRNG_STATE.load(Ordering::Relaxed);
    let new = xorshift64_next(old);
    PRNG_STATE.store(new, Ordering::Relaxed);
    // 取高 32 位归一化,分布更均匀
    ((new >> 32) as f64) / (u32::MAX as f64 + 1.0)
}

/// 生成 [0, upper) 范围内的伪随机索引
fn random_index(upper: usize) -> usize {
    if upper == 0 {
        return 0;
    }
    ensure_prng_seeded();
    let old = PRNG_STATE.load(Ordering::Relaxed);
    let new = xorshift64_next(old);
    PRNG_STATE.store(new, Ordering::Relaxed);
    (new as usize) % upper
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::PeerScore;

    /// 辅助:构建高带宽 CDN 源
    fn cdn_high_bw(url: &str) -> (DownloadSource, PeerScore) {
        (
            DownloadSource::Cdn {
                url: url.to_string(),
            },
            PeerScore {
                latency_ms: 20,
                bandwidth_bps: 50 * 1024 * 1024,
                stability: 0.9,
                distance: 10,
            },
        )
    }

    /// 辅助:构建低带宽 Peer 源
    fn peer_low_bw(addr: &str) -> (DownloadSource, PeerScore) {
        (
            DownloadSource::Peer {
                addr: addr.to_string(),
            },
            PeerScore {
                latency_ms: 200,
                bandwidth_bps: 1 * 1024 * 1024,
                stability: 0.5,
                distance: 100,
            },
        )
    }

    /// 测试 1:创建空选择器
    #[test]
    fn test_empty_selector() {
        let selector = SourceSelector::new();
        assert_eq!(selector.source_count(), 0);
        assert!(selector.select_source().is_none());
    }

    /// 测试 2:添加 CDN 源
    #[test]
    fn test_add_cdn_source() {
        let mut selector = SourceSelector::new();
        let (src, score) = cdn_high_bw("https://cdn.example.com/file.bin");
        selector.add_source(src, score);
        assert_eq!(selector.source_count(), 1);
        match selector.select_source().unwrap() {
            DownloadSource::Cdn { url } => assert_eq!(url, "https://cdn.example.com/file.bin"),
            _ => panic!("期望 CDN 源"),
        }
    }

    /// 测试 3:添加 Peer 源
    #[test]
    fn test_add_peer_source() {
        let mut selector = SourceSelector::new();
        let (src, score) = peer_low_bw("192.168.1.100:6881");
        selector.add_source(src, score);
        assert_eq!(selector.source_count(), 1);
        match selector.select_source().unwrap() {
            DownloadSource::Peer { addr } => assert_eq!(addr, "192.168.1.100:6881"),
            _ => panic!("期望 Peer 源"),
        }
    }

    /// 测试 4:高带宽源应被优先选择
    #[test]
    fn test_high_bandwidth_preferred() {
        let mut selector = SourceSelector::new();
        let (cdn, cdn_score) = cdn_high_bw("https://fast-cdn.com/file.bin");
        let (peer, peer_score) = peer_low_bw("10.0.0.1:6881");

        let cdn_score_val = cdn_score.weighted_score();
        let peer_score_val = peer_score.weighted_score();
        assert!(cdn_score_val > peer_score_val, "CDN 评分应高于 Peer 评分");

        selector.add_source(cdn, cdn_score);
        selector.add_source(peer, peer_score);

        // 大量采样,高带宽源应被选中更多次
        let mut cdn_count = 0;
        let iterations = 1000;
        for _ in 0..iterations {
            if let Some(src) = selector.select_source() {
                if matches!(src, DownloadSource::Cdn { .. }) {
                    cdn_count += 1;
                }
            }
        }
        // CDN 评分显著高于 Peer,被选中比例应超过 50%
        assert!(
            cdn_count > iterations / 2,
            "高带宽 CDN 源被选中次数 {} 应超过半数",
            cdn_count
        );
    }

    /// 测试 5:移除源后选择
    #[test]
    fn test_remove_source() {
        let mut selector = SourceSelector::new();
        let (cdn, cdn_score) = cdn_high_bw("https://cdn.example.com/a.bin");
        let (peer, peer_score) = peer_low_bw("10.0.0.1:6881");
        selector.add_source(cdn, cdn_score);
        selector.add_source(peer, peer_score);
        assert_eq!(selector.source_count(), 2);

        selector.remove_source("https://cdn.example.com/a.bin");
        assert_eq!(selector.source_count(), 1);

        // 移除后只剩 Peer 源
        match selector.select_source().unwrap() {
            DownloadSource::Peer { addr } => assert_eq!(addr, "10.0.0.1:6881"),
            _ => panic!("期望 Peer 源"),
        }
    }

    /// 测试 6:空选择器返回 None
    #[test]
    fn test_empty_returns_none() {
        let selector = SourceSelector::new();
        assert!(selector.select_source().is_none());
    }

    /// 测试 7:更新评分后选择结果变化
    #[test]
    fn test_update_score_changes_selection() {
        let mut selector = SourceSelector::new();
        let (cdn, cdn_score) = cdn_high_bw("https://cdn.example.com/a.bin");
        let (peer, peer_score) = peer_low_bw("10.0.0.1:6881");
        selector.add_source(cdn, cdn_score);
        selector.add_source(peer, peer_score);

        // 将 Peer 源升级为超级源
        selector.update_score("10.0.0.1:6881", 5, 200 * 1024 * 1024);

        // 更新后的 Peer 评分应高于 CDN
        let sources = selector.sources();
        let updated_peer_score = sources
            .iter()
            .find(|(src, _)| src.key() == "10.0.0.1:6881")
            .map(|(_, s)| s.weighted_score())
            .unwrap();
        let cdn_score_val = sources
            .iter()
            .find(|(src, _)| src.key() == "https://cdn.example.com/a.bin")
            .map(|(_, s)| s.weighted_score())
            .unwrap();

        assert!(
            updated_peer_score > cdn_score_val,
            "更新后的 Peer 评分 {} 应高于 CDN 评分 {}",
            updated_peer_score,
            cdn_score_val
        );

        // 大量采样,Peer 应被选中更多次
        let mut peer_count = 0;
        let iterations = 1000;
        for _ in 0..iterations {
            if let Some(src) = selector.select_source() {
                if matches!(src, DownloadSource::Peer { .. }) {
                    peer_count += 1;
                }
            }
        }
        assert!(
            peer_count > iterations / 2,
            "升级后的 Peer 源被选中次数 {} 应超过半数",
            peer_count
        );
    }

    /// 测试 8:多源加权随机分布测试
    #[test]
    fn test_weighted_random_distribution() {
        let mut selector = SourceSelector::new();
        // 添加三个评分相近的源
        selector.add_source(
            DownloadSource::Cdn {
                url: "https://a.com/file".to_string(),
            },
            PeerScore::default(),
        );
        selector.add_source(
            DownloadSource::Cdn {
                url: "https://b.com/file".to_string(),
            },
            PeerScore::default(),
        );
        selector.add_source(
            DownloadSource::Cdn {
                url: "https://c.com/file".to_string(),
            },
            PeerScore::default(),
        );

        let mut counts = [0usize; 3];
        let iterations = 3000;
        for _ in 0..iterations {
            if let Some(src) = selector.select_source() {
                match src.key() {
                    "https://a.com/file" => counts[0] += 1,
                    "https://b.com/file" => counts[1] += 1,
                    "https://c.com/file" => counts[2] += 1,
                    _ => {}
                }
            }
        }

        // 评分相同时应大致均匀分布,每个源占比应在 15%~50% 之间
        for (i, count) in counts.iter().enumerate() {
            let ratio = *count as f64 / iterations as f64;
            assert!(
                ratio > 0.15 && ratio < 0.50,
                "源 {} 选择比例 {:.2}% 不在预期范围内",
                i,
                ratio * 100.0
            );
        }
    }

    /// 测试 9:边界值 -- 只有一个源
    #[test]
    fn test_single_source() {
        let mut selector = SourceSelector::new();
        let (src, score) = cdn_high_bw("https://only-one.com/file.bin");
        selector.add_source(src, score);

        // 100 次选择都应返回同一个源
        for _ in 0..100 {
            let selected = selector.select_source().unwrap();
            assert_eq!(selected.key(), "https://only-one.com/file.bin");
        }
    }

    /// 测试 10:边界值 -- 所有源评分相同
    #[test]
    fn test_all_same_score() {
        let mut selector = SourceSelector::new();
        selector.add_source(
            DownloadSource::Cdn {
                url: "https://a.com".to_string(),
            },
            PeerScore::default(),
        );
        selector.add_source(
            DownloadSource::Peer {
                addr: "10.0.0.1:6881".to_string(),
            },
            PeerScore::default(),
        );

        // 评分相同时不应 panic,且两个源都应被选到
        let mut saw_cdn = false;
        let mut saw_peer = false;
        for _ in 0..1000 {
            match selector.select_source().unwrap() {
                DownloadSource::Cdn { .. } => saw_cdn = true,
                DownloadSource::Peer { .. } => saw_peer = true,
            }
        }
        assert!(saw_cdn, "CDN 源应被选中过");
        assert!(saw_peer, "Peer 源应被选中过");
    }

    /// 测试 11:Default trait 实现
    #[test]
    fn test_default_trait() {
        let selector = SourceSelector::default();
        assert_eq!(selector.source_count(), 0);
    }

    /// 测试 12:移除不存在的源不影响已有源
    #[test]
    fn test_remove_nonexistent() {
        let mut selector = SourceSelector::new();
        let (src, score) = cdn_high_bw("https://cdn.example.com/a.bin");
        selector.add_source(src, score);
        selector.remove_source("https://does-not-exist.com");
        assert_eq!(selector.source_count(), 1);
    }

    /// 测试 13:更新不存在的源评分不影响已有源
    #[test]
    fn test_update_nonexistent_score() {
        let mut selector = SourceSelector::new();
        let (src, score) = cdn_high_bw("https://cdn.example.com/a.bin");
        let original_score = score.weighted_score();
        selector.add_source(src, score);

        selector.update_score("https://does-not-exist.com", 999, 0);

        let actual_score = selector.sources()[0].1.weighted_score();
        assert!(
            (actual_score - original_score).abs() < f64::EPSILON,
            "不存在的源不应影响已有评分"
        );
    }
}
