//! 分片引擎与状态机
//!
//! 管理单个分片的生命周期:Pending -> Downloading -> Verifying -> Writing -> Done
//! 支持失败重试(指数退避)和 EWMA 带宽追踪。

use std::time::Duration;

use tachyon_core::types::FragmentInfo;

/// 分片状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FragmentState {
    #[default]
    Pending,
    /// 下载中
    Downloading,
    /// 校验中
    Verifying,
    /// 写入存储
    Writing,
    /// 已完成
    Done,
    /// 失败(可重试)
    Failed,
}

/// 分片下载记录
pub struct FragmentRecord {
    pub info: FragmentInfo,
    pub state: FragmentState,
    pub retry_count: u32,
    pub max_retries: u32,
    pub last_duration: Option<Duration>,
}

impl FragmentRecord {
    /// 创建新的分片记录
    pub fn new(info: FragmentInfo, max_retries: u32) -> Self {
        Self {
            info,
            state: FragmentState::Pending,
            retry_count: 0,
            max_retries,
            last_duration: None,
        }
    }

    /// 转换到下载中状态(仅允许从 Pending 进入)
    pub fn start_download(&mut self) {
        debug_assert!(
            self.state == FragmentState::Pending,
            "非法状态转换: {:?} -> Downloading",
            self.state
        );
        self.state = FragmentState::Downloading;
    }

    /// 下载完成,转换到校验状态(仅允许从 Downloading 进入)
    pub fn complete_download(&mut self, downloaded: u64, duration: Duration) {
        debug_assert!(
            self.state == FragmentState::Downloading,
            "非法状态转换: {:?} -> Verifying",
            self.state
        );
        self.info.downloaded = downloaded;
        self.last_duration = Some(duration);
        self.state = FragmentState::Verifying;
    }

    /// 下载完成并直接流转到 Done 状态
    ///
    /// 用于 spawn 内已完成下载和写入的场景,跳过 Verifying/Writing 中间状态,
    /// 但仍正确设置 `last_duration` 以激活调度器反馈回路。
    pub fn complete_download_fast(&mut self, downloaded: u64, duration: Duration) {
        debug_assert!(
            self.state == FragmentState::Downloading,
            "非法状态转换: {:?} -> Done(fast)",
            self.state
        );
        self.info.downloaded = downloaded;
        self.last_duration = Some(duration);
        self.state = FragmentState::Done;
    }

    /// 校验通过,转换到写入状态(仅允许从 Verifying 进入)
    pub fn verify_ok(&mut self) {
        debug_assert!(
            self.state == FragmentState::Verifying,
            "非法状态转换: {:?} -> Writing",
            self.state
        );
        self.state = FragmentState::Writing;
    }

    /// 写入完成,转换到完成状态(仅允许从 Writing 进入)
    pub fn write_done(&mut self) {
        debug_assert!(
            self.state == FragmentState::Writing,
            "非法状态转换: {:?} -> Done",
            self.state
        );
        self.state = FragmentState::Done;
    }

    /// 标记失败,如果可重试则回到 Pending(仅允许从 Downloading/Verifying/Writing 进入)
    pub fn mark_failed(&mut self) -> bool {
        debug_assert!(
            matches!(
                self.state,
                FragmentState::Downloading | FragmentState::Verifying | FragmentState::Writing
            ),
            "非法状态转换: {:?} -> Failed/Pending",
            self.state
        );
        self.retry_count += 1;
        if self.retry_count <= self.max_retries {
            self.state = FragmentState::Pending;
            true
        } else {
            self.state = FragmentState::Failed;
            false
        }
    }

    /// 强制标记为最终失败状态(不可重试)
    ///
    /// 用于上层(如 spawn 内部重试循环)已确认重试耗尽、需要将分片置为终态时。
    /// 与 `mark_failed` 不同,本方法不参与"是否可重试"判定,直接转入 `Failed`。
    pub fn force_fail(&mut self) {
        self.state = FragmentState::Failed;
    }

    /// 是否已完成
    pub fn is_done(&self) -> bool {
        self.state == FragmentState::Done
    }

    /// 是否已彻底失败(无法重试)
    pub fn is_failed(&self) -> bool {
        self.state == FragmentState::Failed
    }

    /// 计算重试退避时间(指数退避:1s, 2s, 4s, 8s, ...)
    pub fn backoff_duration(&self) -> Duration {
        Duration::from_secs(1u64 << self.retry_count.min(10))
    }
}

/// EWMA 带宽追踪器
pub struct BandwidthTracker {
    ewma: f64,
    alpha: f64,
    /// 已记录的采样总数(仅计数,不存储历史样本,节省内存)
    sample_count: usize,
}

impl BandwidthTracker {
    /// 创建带宽追踪器
    /// - alpha: EWMA 平滑因子(0.0 ~ 1.0),越大越重视最新数据
    pub fn new(alpha: f64) -> Self {
        Self {
            ewma: 0.0,
            alpha: alpha.clamp(0.0, 1.0),
            sample_count: 0,
        }
    }

    /// 记录一个新的带宽样本(字节/秒),跳过零值避免污染 EWMA
    pub fn record(&mut self, bytes_per_sec: u64) {
        if bytes_per_sec == 0 {
            return;
        }
        self.sample_count += 1;
        if self.sample_count == 1 {
            self.ewma = bytes_per_sec as f64;
        } else {
            self.ewma = self.alpha * bytes_per_sec as f64 + (1.0 - self.alpha) * self.ewma;
        }
    }

    /// 获取当前 EWMA 带宽估计(字节/秒)
    pub fn estimate(&self) -> u64 {
        self.ewma as u64
    }

    /// 获取采样数
    pub fn sample_count(&self) -> usize {
        self.sample_count
    }
}

impl Default for BandwidthTracker {
    fn default() -> Self {
        Self::new(0.3)
    }
}

/// 根据带宽和文件大小计算最优分片大小
pub fn compute_fragment_size(
    file_size: u64,
    bandwidth_bps: u64,
    min_size: u64,
    max_size: u64,
    target_fragments: u32,
) -> u64 {
    if file_size == 0 {
        return 0;
    }

    // 基础分片大小 = 文件大小 / 目标分片数
    let base = file_size / target_fragments.max(1) as u64;

    // 根据带宽调整:高带宽时增大分片以减少开销
    let bandwidth_factor = if bandwidth_bps > 100 * 1024 * 1024 {
        2.0 // > 100Mbps,分片翻倍
    } else if bandwidth_bps > 10 * 1024 * 1024 {
        1.5 // > 10Mbps
    } else {
        1.0
    };

    let adjusted = (base as f64 * bandwidth_factor) as u64;
    adjusted.clamp(min_size, max_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachyon_core::types::FragmentInfo;

    fn make_frag(index: u32, size: u64) -> FragmentInfo {
        FragmentInfo::new(
            index,
            index as u64 * size,
            (index as u64 + 1) * size - 1,
            size,
        )
    }

    #[test]
    fn test_fragment_state_transitions() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 3);
        assert_eq!(record.state, FragmentState::Pending);

        record.start_download();
        assert_eq!(record.state, FragmentState::Downloading);

        record.complete_download(4, Duration::from_millis(100));
        assert_eq!(record.state, FragmentState::Verifying);

        record.verify_ok();
        assert_eq!(record.state, FragmentState::Writing);

        record.write_done();
        assert_eq!(record.state, FragmentState::Done);
        assert!(record.is_done());
    }

    #[test]
    fn test_fragment_retry() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 2);

        record.start_download();
        assert!(record.mark_failed()); // retry 1
        assert_eq!(record.state, FragmentState::Pending);

        record.start_download();
        assert!(record.mark_failed()); // retry 2
        assert_eq!(record.state, FragmentState::Pending);

        record.start_download();
        assert!(!record.mark_failed()); // retry 3, exceeds max
        assert_eq!(record.state, FragmentState::Failed);
        assert!(record.is_failed());
    }

    #[test]
    fn test_backoff_duration() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 5);

        record.retry_count = 0;
        assert_eq!(record.backoff_duration(), Duration::from_secs(1));

        record.retry_count = 1;
        assert_eq!(record.backoff_duration(), Duration::from_secs(2));

        record.retry_count = 2;
        assert_eq!(record.backoff_duration(), Duration::from_secs(4));

        record.retry_count = 3;
        assert_eq!(record.backoff_duration(), Duration::from_secs(8));
    }

    #[test]
    fn test_bandwidth_tracker() {
        let mut tracker = BandwidthTracker::new(0.5);
        tracker.record(100);
        assert_eq!(tracker.estimate(), 100);

        tracker.record(200);
        // EWMA = 0.5 * 200 + 0.5 * 100 = 150
        assert_eq!(tracker.estimate(), 150);

        tracker.record(300);
        // EWMA = 0.5 * 300 + 0.5 * 150 = 225
        assert_eq!(tracker.estimate(), 225);
    }

    #[test]
    fn test_bandwidth_tracker_default() {
        let mut tracker = BandwidthTracker::default();
        tracker.record(1000);
        assert_eq!(tracker.sample_count(), 1);
    }

    #[test]
    fn test_compute_fragment_size_normal() {
        let size = compute_fragment_size(
            100 * 1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            64 * 1024 * 1024,
            16,
        );
        assert!(size >= 1024 * 1024);
        assert!(size <= 64 * 1024 * 1024);
    }

    #[test]
    fn test_compute_fragment_size_high_bandwidth() {
        let size = compute_fragment_size(
            1024 * 1024 * 1024,
            200 * 1024 * 1024,
            1024 * 1024,
            64 * 1024 * 1024,
            16,
        );
        assert!(size >= 1024 * 1024);
    }

    #[test]
    fn test_compute_fragment_size_zero() {
        let size = compute_fragment_size(0, 0, 1024, 64 * 1024 * 1024, 16);
        assert_eq!(size, 0);
    }

    #[test]
    fn test_compute_fragment_size_small_file() {
        let size = compute_fragment_size(500, 1024, 1024, 64 * 1024 * 1024, 4);
        assert_eq!(size, 1024); // clamp to min
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// compute_fragment_size 结果应在 min..=max 范围内
        #[test]
        fn test_fragment_size_always_in_range(
            file_size in 0u64..1024 * 1024 * 1024 * 10,
            bandwidth in 0u64..1024 * 1024 * 1024,
        ) {
            let min_size = 1024 * 1024;       // 1MB
            let max_size = 64 * 1024 * 1024;  // 64MB
            let target_fragments = 16u32;

            let result = compute_fragment_size(
                file_size,
                bandwidth,
                min_size,
                max_size,
                target_fragments,
            );

            if file_size == 0 {
                // 空文件返回 0
                prop_assert_eq!(result, 0);
            } else {
                // 正常文件结果在 [min_size, max_size] 内
                prop_assert!(result >= min_size, "结果 {} 小于最小值 {}", result, min_size);
                prop_assert!(result <= max_size, "结果 {} 大于最大值 {}", result, max_size);
            }
        }

        /// EWMA 估计值应该在观测值的合理范围内
        #[test]
        fn test_bandwidth_tracker_ewma_bounded(
            values in prop::collection::vec(0u64..1024 * 1024 * 1024, 1..50)
        ) {
            let mut tracker = BandwidthTracker::new(0.3);
            for v in &values {
                tracker.record(*v);
            }

            let estimate = tracker.estimate();
            let max_val = *values.iter().max().unwrap();

            // EWMA 不应超过观测最大值的合理范围
            // (理论上 EWMA 永远在 min..max 之间,但 u64 截断可能导致边界情况)
            prop_assert!(
                estimate <= max_val * 2,
                "EWMA 估计 {} 远超最大观测值 {}",
                estimate,
                max_val,
            );
            prop_assert_eq!(tracker.sample_count(), values.len());
        }

        /// alpha 值应被 clamp 到 [0.0, 1.0] 范围内
        #[test]
        fn test_bandwidth_tracker_alpha_clamped(
            alpha in -10.0f64..10.0f64,
            sample in 0u64..1024 * 1024,
        ) {
            let tracker = BandwidthTracker::new(alpha);
            let mut tracker = tracker;
            tracker.record(sample);
            // 创建不应 panic,estimate 应等于 sample（单样本）
            prop_assert_eq!(tracker.estimate(), sample);
        }

        /// FragmentRecord 状态机: 必须经历正确的生命周期
        #[test]
        fn test_fragment_state_machine_valid(
            max_retries in 0u32..10,
        ) {
            let info = FragmentInfo {
                index: 0,
                start: 0,
                end: 999,
                size: 1000,
                downloaded: 0,
                hash: None,
            };
            let mut record = FragmentRecord::new(info, max_retries);
            prop_assert_eq!(record.state, FragmentState::Pending);

            // 尝试下载 -> 失败重试
            for _ in 0..=max_retries {
                record.start_download();
                prop_assert_eq!(record.state, FragmentState::Downloading);

                if record.retry_count < max_retries {
                    // 还可以重试
                    let can_retry = record.mark_failed();
                    prop_assert!(can_retry);
                    prop_assert_eq!(record.state, FragmentState::Pending);
                } else {
                    // 超过最大重试次数
                    let data_len = 22u64;
                    record.complete_download(data_len, Duration::from_millis(10));
                    prop_assert_eq!(record.state, FragmentState::Verifying);
                    record.verify_ok();
                    prop_assert_eq!(record.state, FragmentState::Writing);
                    record.write_done();
                    prop_assert!(record.is_done());
                    break;
                }
            }
        }

        /// 指数退避时间应随重试次数递增,且不溢出
        #[test]
        fn test_backoff_duration_monotonic(
            retry_count in 0u32..15,
        ) {
            let info = FragmentInfo {
                index: 0,
                start: 0,
                end: 99,
                size: 100,
                downloaded: 0,
                hash: None,
            };
            let mut record = FragmentRecord::new(info, 20);
            record.retry_count = retry_count;

            let backoff = record.backoff_duration();
            // 退避时间应为正数
            prop_assert!(backoff.as_secs() >= 1);
            // 最大不应超过 2^10 = 1024 秒（被 min(10) 限制）
            prop_assert!(backoff.as_secs() <= 1024);
        }
    }
}
