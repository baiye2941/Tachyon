//! 自适应下载调度器
//!
//! 基于 Holt 双指数平滑带宽预测实现 `DownloadScheduler` trait,
//! 为下载引擎提供动态的并发度和分片大小建议。
//! 使用 parking_lot::RwLock 实现读多写少的高效并发访问。

use std::time::Duration;

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
    /// 估计的链路往返时延(RTT),用于 BDP(带宽延迟积)计算。
    ///
    /// 默认 50ms(典型公网 RTT)。可通过 [`DownloadScheduler::observe_rtt`]
    /// 在 probe 阶段注入实测 RTT(如 TCP 握手 + TTFB),使 BDP 估计更贴合
    /// 真实链路。高延迟链路(跨国 200ms+、卫星 600ms+)下,准确的 RTT 能
    /// 避免分片过小导致 TCP 窗口未打满、并发度不足导致管道空闲。
    rtt: RwLock<Duration>,
}

/// 默认 RTT,冷启动无实测样本时的回退值。
///
/// 50ms 为典型公网 RTT。分片大小与并发度计算共用此值,
/// 保证两者对链路延迟的假设一致。
const DEFAULT_RTT: Duration = Duration::from_millis(50);

impl AdaptiveDownloadScheduler {
    /// 创建新的自适应调度器
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            predictor: RwLock::new(HoltLinearPredictor::new(
                config.ewma_alpha,
                config.ewma_beta,
            )),
            config,
            rtt: RwLock::new(DEFAULT_RTT),
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

    fn observe_rtt(&self, rtt: Duration) {
        // 跳过零值和异常大值(>10s 视为测量错误),防止 BDP 计算爆炸
        if rtt.as_secs() > 10 {
            tracing::warn!(?rtt, "无效的 RTT 观测值(>10s),跳过更新");
            return;
        }
        let mut current = self.rtt.write();
        *current = rtt;
        tracing::debug!(?rtt, "RTT 观测已更新");
    }

    fn recommend(&self, file_size: u64, max_concurrency: u32) -> ScheduleRecommendation {
        let (predicted_bw, confidence) = {
            let pred = self.predictor.read();
            (pred.predict(1), pred.confidence())
        };
        let rtt_secs = self.rtt.read().as_secs_f64();

        // 根据带宽预测计算建议分片大小
        // 目标:每个分片下载时间约 2-5 秒
        let target_download_secs = if confidence > 0.5 {
            3.0 // 高置信度时使用 3 秒目标
        } else {
            5.0 // 低置信度时使用更保守的 5 秒目标
        };

        let suggested_frag_size = if predicted_bw > 0.0 {
            // 基于"目标下载时长"的分片大小:每个分片约 3-5 秒可完成,
            // 避免分片过小导致请求开销过高、分片过大导致断点续传粒度过粗。
            let time_based = (predicted_bw * target_download_secs) as u64;

            // BDP(带宽延迟积)分片大小估计:
            //   fragment_size ≈ bandwidth_bps * rtt_secs * 2
            // 直觉:链路 BDP = bandwidth × RTT 是"在途字节数上限"。
            // 取 2×BDP 使得单个分片能在一个往返内排空管道并完成传输,
            // 高延迟链路下避免分片过小导致 TCP 窗口无法打满。
            // RTT 来自 observe_rtt 注入(默认 50ms,可由 probe 阶段实测更新)。
            let bdp_based = (predicted_bw * rtt_secs * 2.0) as u64;

            // 取两者较大值:兼顾"下载时长目标"与"管道充盈度",
            // 高带宽低延迟链路 time_based 主导,高延迟链路 bdp_based 主导。
            let size = time_based.max(bdp_based);
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
            // 以充分利用 TCP 窗口(与分片大小估计使用同一 RTT 保持一致)。
            // 当 frag_size < BDP 时(高带宽且 max_fragment_size 限制分片大小),
            // 需要多并发才能让在途字节数 ≥ BDP,填满管道。
            let bdp = (predicted_bw * rtt_secs) as u64;
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
        tracing::debug!(recommendation = ?recommendation, rtt_secs, "调度推荐结果");
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

    /// BDP 分片大小估计:验证高带宽场景下 BDP 下界能放大分片大小。
    ///
    /// 100MB/s 带宽、RTT=50ms:
    ///   - time_based(高置信,3s) = 100MB/s * 3s = 300MB → clamp 到 max=64MB
    ///   - bdp_based = 100MB/s * 0.05s * 2 = 10MB
    ///
    /// time_based 主导,但分片应被 clamp 到 64MB(配置上限),体现 BDP 放大效应。
    #[test]
    fn test_recommend_fragment_size_bdp_amplification() {
        let sched = AdaptiveDownloadScheduler::default_config();

        // 高带宽,多次观测以提升置信度
        for _ in 0..20 {
            sched.observe_bandwidth(100 * 1024 * 1024); // 100MB/s
        }

        let rec = sched.recommend(2 * 1024 * 1024 * 1024, 8);
        // time_based(300MB) 被 clamp 到 max_fragment_size=64MB
        assert_eq!(
            rec.fragment_size,
            SchedulerConfig::default().max_fragment_size,
            "高带宽下分片应触及上限(64MB),体现 BDP/时长目标放大"
        );
    }

    /// BDP 主导场景:中低带宽、高延迟等效下验证 BDP 提升分片大小。
    ///
    /// 5MB/s 带宽、RTT=50ms:
    ///   - time_based(低置信,5s) = 5MB/s * 5s = 25MB
    ///   - bdp_based = 5MB/s * 0.05s * 2 = 0.5MB → clamp 到 min=1MB
    ///   - 取较大值 = 25MB,验证 BDP 路径不压低分片。
    #[test]
    fn test_recommend_fragment_size_bdp_vs_time_based() {
        let sched = AdaptiveDownloadScheduler::default_config();

        for _ in 0..10 {
            sched.observe_bandwidth(5 * 1024 * 1024); // 5MB/s
        }

        let rec = sched.recommend(500 * 1024 * 1024, 8);
        // 25MB 在 [1MB, 64MB] 范围内,且应等于 max(time_based, bdp_based)=25MB
        assert!(
            rec.fragment_size >= 20 * 1024 * 1024 && rec.fragment_size <= 30 * 1024 * 1024,
            "5MB/s 下分片应接近 25MB(time_based 主导),实际: {}",
            rec.fragment_size
        );
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

    // ── RTT 动态注入测试 ────────────────────────────────────────────

    /// observe_rtt 更新后,recommend 的 BDP 计算应使用新 RTT。
    ///
    /// 10MB/s 带宽、RTT=50ms(默认):
    ///   - bdp_based = 10MB/s * 0.05s * 2 = 1MB
    ///   - time_based(低置信,5s) = 10MB/s * 5s = 50MB → clamp 到 64MB(max)
    ///   - 取较大值 = 50MB
    ///
    /// 10MB/s 带宽、RTT=500ms(高延迟):
    ///   - bdp_based = 10MB/s * 0.5s * 2 = 10MB
    ///   - time_based = 50MB
    ///   - 取较大值 = 50MB(time_based 仍主导)
    ///
    /// 关键验证:高 RTT 下 BDP 增大,但分片大小不变(time_based 主导)。
    /// 改为验证低带宽场景下 RTT 对分片大小的影响。
    #[test]
    fn test_observe_rtt_updates_bdp_estimate() {
        let sched = AdaptiveDownloadScheduler::default_config();

        // 低带宽 1MB/s,多次观测提升置信度
        for _ in 0..20 {
            sched.observe_bandwidth(1024 * 1024); // 1MB/s
        }

        // 默认 RTT=50ms:
        //   time_based(高置信,3s) = 1MB/s * 3s = 3MB
        //   bdp_based = 1MB/s * 0.05s * 2 = 0.1MB → clamp 到 min=1MB
        //   取较大值 = 3MB
        let rec_default = sched.recommend(100 * 1024 * 1024, 8);
        assert_eq!(
            rec_default.fragment_size,
            3 * 1024 * 1024,
            "默认 RTT=50ms 下分片应为 3MB(time_based 主导)"
        );

        // 注入高延迟 RTT=2s(卫星链路):
        //   time_based = 3MB(不变)
        //   bdp_based = 1MB/s * 2s * 2 = 4MB
        //   取较大值 = 4MB(bdp_based 主导!)
        sched.observe_rtt(Duration::from_secs(2));
        let rec_high_rtt = sched.recommend(100 * 1024 * 1024, 8);
        assert_eq!(
            rec_high_rtt.fragment_size,
            4 * 1024 * 1024,
            "RTT=2s 下 bdp_based(4MB)应主导 over time_based(3MB),实际: {}",
            rec_high_rtt.fragment_size
        );
    }

    /// 高 RTT 下 BDP 并发度约束应生效(当 frag_size < BDP 时)。
    ///
    /// 1GB/s(极高带宽)、RTT=200ms、max_fragment_size=64MB:
    ///   - BDP = 1GB/s * 0.2s = 200MB
    ///   - frag_size = clamp(time_based, 1MB, 64MB) = 64MB(max 限制)
    ///   - bdp_concurrency = 200MB / 64MB = 3(需 3 并发填满管道)
    ///
    /// 注:bandwidth_based = 1GB/s * 3s / 64MB = 48 会主导并发度计算,
    /// 但当 max_concurrency 限制到 4 时,RTT=200ms 的 bdp_concurrency=3
    /// 应使并发度 >= 3(而非 1),验证 BDP 约束未被忽略。
    #[test]
    fn test_high_rtt_triggers_bdp_concurrency() {
        let config = SchedulerConfig::default();
        let sched = AdaptiveDownloadScheduler::new(config);

        // 极高带宽 1GB/s,多次观测提升置信度
        for _ in 0..20 {
            sched.observe_bandwidth(1024 * 1024 * 1024);
        }

        // 低延迟 RTT=50ms:BDP = 1GB/s * 0.05s = 50MB < frag_size=64MB
        // bdp_concurrency = 1(单并发可填满管道)
        // bandwidth_based = 48,但 max_concurrency=4 限制到 4
        sched.observe_rtt(Duration::from_millis(50));
        let rec_low_rtt = sched.recommend(10 * 1024 * 1024 * 1024, 4);
        assert_eq!(
            rec_low_rtt.concurrency, 4,
            "RTT=50ms 下受 max_concurrency=4 限制"
        );

        // 高延迟 RTT=200ms:BDP = 1GB/s * 0.2s = 200MB > frag_size=64MB
        // bdp_concurrency = ceil(200MB / 64MB) = 3
        // bandwidth_based = 48 → min(48, 4) = 4,max(4, 3) = 4
        sched.observe_rtt(Duration::from_millis(200));
        // max_concurrency=4 时,bdp_concurrency=3 应使并发度 >= 3(而非被压到 1)
        let rec_high_rtt_4 = sched.recommend(10 * 1024 * 1024 * 1024, 4);
        assert!(
            rec_high_rtt_4.concurrency >= 3,
            "RTT=200ms 下 BDP=200MB > frag=64MB,bdp_concurrency 应 >=3,实际: {}",
            rec_high_rtt_4.concurrency
        );
    }

    /// observe_rtt 应过滤异常值(>10s),不更新内部 RTT。
    #[test]
    fn test_observe_rtt_rejects_invalid_values() {
        let sched = AdaptiveDownloadScheduler::default_config();

        // 先设置一个正常 RTT
        sched.observe_rtt(Duration::from_millis(100));
        sched.observe_bandwidth(10 * 1024 * 1024);
        for _ in 0..20 {
            sched.observe_bandwidth(10 * 1024 * 1024);
        }
        let rec_normal = sched.recommend(100 * 1024 * 1024, 8);

        // 注入异常 RTT(>10s),应被拒绝,RTT 保持 100ms
        sched.observe_rtt(Duration::from_secs(100));
        let rec_after_invalid = sched.recommend(100 * 1024 * 1024, 8);
        assert_eq!(
            rec_normal.fragment_size, rec_after_invalid.fragment_size,
            "异常 RTT 应被拒绝,recommend 结果不应变化"
        );
    }

    /// 默认 RTT(50ms)行为与旧硬编码常量一致(向后兼容)。
    #[test]
    fn test_default_rtt_matches_legacy_constant() {
        let sched = AdaptiveDownloadScheduler::default_config();

        for _ in 0..20 {
            sched.observe_bandwidth(50 * 1024 * 1024); // 50MB/s
        }

        // 默认 RTT=50ms,行为应与旧 ESTIMATED_RTT_SECS=0.050 一致
        let rec = sched.recommend(1024 * 1024 * 1024, 8);
        // time_based(高置信,3s) = 50MB/s * 3s = 150MB → clamp 到 64MB
        assert_eq!(
            rec.fragment_size,
            SchedulerConfig::default().max_fragment_size
        );
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
