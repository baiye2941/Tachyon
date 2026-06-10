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
                config.ewma_alpha * 0.3, // beta 通常小于 alpha
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
        // 经验公式:并发度 = min(带宽 / 单分片带宽需求, 文件分片数, 最大并发)
        let suggested_concurrency = if predicted_bw > 0.0 && suggested_frag_size > 0 {
            // 估算可同时下载的分片数
            let fragments_for_file = file_size.div_ceil(suggested_frag_size);
            let bandwidth_based =
                (predicted_bw / (suggested_frag_size as f64 / target_download_secs)) as u32;
            bandwidth_based
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
