//! 自适应下载调度器
//!
//! 基于 Holt 双指数平滑带宽预测实现 `DownloadScheduler` trait,
//! 为下载引擎提供动态的并发度和分片大小建议。
//! 使用 parking_lot::RwLock 实现读多写少的高效并发访问。

use parking_lot::RwLock;

use tachyon_core::config::SchedulerConfig;
use tachyon_core::traits::{DownloadScheduler, ScheduleRecommendation};

use crate::predictor::HoltLinearPredictor;

/// 自适应下载调度器
///
/// 使用 Holt 双指数平滑模型预测带宽,
/// 并根据预测结果动态调整并发度和分片大小。
pub struct AdaptiveDownloadScheduler {
    predictor: RwLock<HoltLinearPredictor>,
    config: SchedulerConfig,
}

impl AdaptiveDownloadScheduler {
    /// 创建新的自适应调度器
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            predictor: RwLock::new(HoltLinearPredictor::new(
                config.ewma_alpha,
                config.ewma_beta,
            )),
            config,
        }
    }

    /// 使用默认配置创建调度器
    pub fn default_config() -> Self {
        Self::new(SchedulerConfig::default())
    }
}

impl DownloadScheduler for AdaptiveDownloadScheduler {
    fn observe_bandwidth(&self, bytes_per_sec: u64) {
        tracing::info!(bandwidth = bytes_per_sec, "带宽分配更新");
        let mut pred = self.predictor.write();
        pred.observe(bytes_per_sec as f64);
    }

    fn recommend(&self, file_size: u64, max_concurrency: u32) -> ScheduleRecommendation {
        let (predicted_bw, confidence) = {
            let pred = self.predictor.read();
            (pred.predict(1), pred.confidence())
        };

        // 根据带宽预测计算建议分片大小
        // 目标:每个分片下载时间约 2-5 秒
        let target_download_secs = if confidence > 0.5 {
            3.0 // 高置信度时使用 3 秒目标
        } else {
            5.0 // 低置信度时使用更保守的 5 秒目标
        };

        let suggested_frag_size = if predicted_bw > 0.0 {
            let size = (predicted_bw * target_download_secs) as u64;
            // 限制在配置范围内
            size.clamp(self.config.min_fragment_size, self.config.max_fragment_size)
        } else {
            // 无带宽数据时使用默认值
            self.config.min_fragment_size
        };

        // 根据带宽和文件大小计算建议并发度
        // 公式:并发度 = predicted_bw * target_secs / frag_size
        //   即"需要多少个并行分片才能占满预测带宽"
        //   当 frag_size 被 clamp 到 max_fragment_size 时,并发度 > 1;
        //   当 frag_size = predicted_bw * target_secs (未 clamp) 时,并发度 = 1 (单分片即可占满)。
        //
        // 旧公式 `predicted_bw / (frag_size / target_secs)` 在 frag_size 未 clamp 时
        // 简化为 `predicted_bw / predicted_bw = 1`,数学上正确但语义不清且易误读。
        // 新公式等价但更直观,并增加 BDP 约束确保高延迟链路下并发度足够。
        let suggested_concurrency = if predicted_bw > 0.0 && suggested_frag_size > 0 {
            let fragments_for_file = file_size.div_ceil(suggested_frag_size);
            let bandwidth_based =
                (predicted_bw * target_download_secs / suggested_frag_size as f64) as u32;
            // BDP 约束:高延迟链路下确保至少 ceil(BDP / frag_size) 个并发
            // 以充分利用 TCP 窗口(假设 RTT ≈ 50ms 作为典型值)
            let estimated_rtt = 0.050; // 50ms 典型 RTT
            let bdp = (predicted_bw * estimated_rtt) as u64;
            let bdp_concurrency = if bdp > suggested_frag_size {
                (bdp / suggested_frag_size).max(1) as u32
            } else {
                1
            };
            bandwidth_based
                .max(bdp_concurrency)
                .min(fragments_for_file as u32)
                .min(max_concurrency)
                .max(1) // 至少 1 个并发
        } else {
            // 冷启动(无带宽样本):回退到调用方传入的 max_concurrency,
            // 代表用户配置意图;下游 downloader 仍会 min(config.max_concurrent_fragments),
            // 且实际 spawn 的分片数受 fragment_specs 长度限制,不会过度并发。
            max_concurrency.max(1)
        };

        let recommendation = ScheduleRecommendation {
            concurrency: suggested_concurrency,
            fragment_size: suggested_frag_size,
            confidence,
        };
        tracing::debug!(recommendation = ?recommendation, "调度推荐结果");
        recommendation
    }

    fn predicted_bandwidth(&self) -> u64 {
        let pred = self.predictor.read();
        pred.predict(1) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_scheduler_creation() {
        let sched = AdaptiveDownloadScheduler::default_config();
        assert_eq!(sched.predicted_bandwidth(), 0);
    }

    #[test]
    fn test_observe_and_predict() {
        let sched = AdaptiveDownloadScheduler::default_config();
        sched.observe_bandwidth(1024 * 1024); // 1MB/s
        assert!(sched.predicted_bandwidth() > 0);
    }

    #[test]
    fn test_recommend_with_no_data() {
        let sched = AdaptiveDownloadScheduler::default_config();
        let rec = sched.recommend(100 * 1024 * 1024, 8);
        // 冷启动(无带宽样本)时应回退到 max_concurrency,充分利用用户配置的并发上限
        assert_eq!(rec.concurrency, 8);
        assert_eq!(
            rec.fragment_size,
            SchedulerConfig::default().min_fragment_size
        );
        assert!((rec.confidence - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_recommend_cold_start_respects_max_concurrency() {
        let sched = AdaptiveDownloadScheduler::default_config();
        // 冷启动时并发度应等于传入的 max_concurrency
        let rec_4 = sched.recommend(100 * 1024 * 1024, 4);
        assert_eq!(rec_4.concurrency, 4);

        let rec_16 = sched.recommend(100 * 1024 * 1024, 16);
        assert_eq!(rec_16.concurrency, 16);

        // max_concurrency 为 0 时应至少保证 1 并发
        let rec_0 = sched.recommend(100 * 1024 * 1024, 0);
        assert_eq!(rec_0.concurrency, 1);
    }

    #[test]
    fn test_recommend_with_bandwidth_data() {
        let sched = AdaptiveDownloadScheduler::default_config();

        // 模拟多次带宽观测
        for _ in 0..10 {
            sched.observe_bandwidth(10 * 1024 * 1024); // 10MB/s
        }

        let rec = sched.recommend(100 * 1024 * 1024, 8);
        // 有带宽数据时应有更高的并发度和更大的分片
        assert!(rec.concurrency >= 1);
        assert!(rec.fragment_size >= SchedulerConfig::default().min_fragment_size);
        assert!(rec.confidence > 0.0);
    }

    #[test]
    fn test_recommend_respects_max_concurrency() {
        let sched = AdaptiveDownloadScheduler::default_config();

        // 高带宽场景
        for _ in 0..20 {
            sched.observe_bandwidth(100 * 1024 * 1024); // 100MB/s
        }

        let rec = sched.recommend(1024 * 1024 * 1024, 4); // 限制最大并发为 4
        assert!(rec.concurrency <= 4, "并发度不应超过 max_concurrency");
    }

    #[test]
    fn test_recommend_fragment_size_in_range() {
        let config = SchedulerConfig {
            min_fragment_size: 512 * 1024,       // 512KB
            max_fragment_size: 32 * 1024 * 1024, // 32MB
            ..Default::default()
        };
        let sched = AdaptiveDownloadScheduler::new(config.clone());

        // 中等带宽
        for _ in 0..10 {
            sched.observe_bandwidth(5 * 1024 * 1024); // 5MB/s
        }

        let rec = sched.recommend(500 * 1024 * 1024, 8);
        assert!(
            rec.fragment_size >= config.min_fragment_size,
            "分片大小不应小于最小值"
        );
        assert!(
            rec.fragment_size <= config.max_fragment_size,
            "分片大小不应超过最大值"
        );
    }

    #[test]
    fn test_recommend_small_file() {
        let sched = AdaptiveDownloadScheduler::default_config();

        for _ in 0..10 {
            sched.observe_bandwidth(10 * 1024 * 1024);
        }

        // 小文件
        let rec = sched.recommend(1024, 8);
        // 小文件应只有 1 个分片,并发度应为 1
        assert_eq!(rec.concurrency, 1);
    }

    #[test]
    fn test_confidence_increases_with_observations() {
        let sched = AdaptiveDownloadScheduler::default_config();

        let rec1 = sched.recommend(100 * 1024 * 1024, 8);
        let conf1 = rec1.confidence;

        sched.observe_bandwidth(10 * 1024 * 1024);
        let rec2 = sched.recommend(100 * 1024 * 1024, 8);
        let conf2 = rec2.confidence;

        assert!(conf2 >= conf1, "置信度应随观测次数增加");
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let sched = Arc::new(AdaptiveDownloadScheduler::default_config());
        let mut handles = vec![];

        // 多线程并发访问
        for i in 0..4 {
            let sched_clone = Arc::clone(&sched);
            handles.push(thread::spawn(move || {
                for j in 0..100 {
                    sched_clone.observe_bandwidth((i * 100 + j) * 1024);
                    let _rec = sched_clone.recommend(100 * 1024 * 1024, 8);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // 随机带宽与文件大小下,recommend 返回的并发度与分片大小均满足约束
    proptest! {
        #[test]
        fn test_recommend_invariants(
            file_size in 0u64..2 * 1024 * 1024 * 1024u64,
            max_concurrency in 0u32..64,
            bandwidths in prop::collection::vec(1u64..200 * 1024 * 1024u64, 0..30),
        ) {
            let config = SchedulerConfig::default();
            let sched = AdaptiveDownloadScheduler::new(config.clone());

            for bw in bandwidths {
                sched.observe_bandwidth(bw);
            }

            let rec = sched.recommend(file_size, max_concurrency);

            // 并发度至少为 1,不超过 max_concurrency(若 max_concurrency=0 仍应 >=1)
            prop_assert!(rec.concurrency >= 1);
            prop_assert!(
                rec.concurrency <= max_concurrency.max(1),
                "并发度 {} 超过 max_concurrency {}",
                rec.concurrency,
                max_concurrency
            );

            // 分片大小在配置边界内
            prop_assert!(
                rec.fragment_size >= config.min_fragment_size,
                "分片大小 {} 小于最小值 {}",
                rec.fragment_size,
                config.min_fragment_size
            );
            prop_assert!(
                rec.fragment_size <= config.max_fragment_size,
                "分片大小 {} 超过最大值 {}",
                rec.fragment_size,
                config.max_fragment_size
            );

            // 置信度在 [0.0, 1.0] 内
            prop_assert!(rec.confidence >= 0.0 && rec.confidence <= 1.0);
        }

        // 冷启动(无带宽样本)时,分片大小为最小值,并发度为 max_concurrency
        #[test]
        fn test_cold_start_recommend(
            file_size in 1u64..2 * 1024 * 1024 * 1024u64,
            max_concurrency in 1u32..64,
        ) {
            let config = SchedulerConfig::default();
            let sched = AdaptiveDownloadScheduler::new(config);
            let rec = sched.recommend(file_size, max_concurrency);

            prop_assert_eq!(rec.concurrency, max_concurrency);
            prop_assert_eq!(rec.fragment_size, SchedulerConfig::default().min_fragment_size);
            prop_assert!((rec.confidence - 0.0).abs() < f64::EPSILON);
        }
    }
}
