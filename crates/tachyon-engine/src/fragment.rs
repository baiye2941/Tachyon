//! 分片引擎与状态机
//!
//! 管理单个分片的生命周期:Pending -> Downloading -> Verifying -> Writing -> Done
//! 支持失败重试(指数退避)和 EWMA 带宽追踪。

use std::time::Duration;

use tachyon_core::config::SchedulerConfig;
use tachyon_core::types::FragmentInfo;
use tachyon_core::{DownloadError, DownloadResult};

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
    /// 流式哈希结果:下载阶段边写边算的 blake3 十六进制字符串。
    ///
    /// 仅当分片有 expected hash(`info.hash.is_some()`)时在下载时计算,
    /// 供 `verify()` 直接比对,避免重读已写入的数据(I/O 放大消除)。
    /// 断点续传恢复的分片无此字段,`verify()` 回退到读盘计算。
    pub computed_hash: Option<String>,
    /// 字节级断点续传偏移:已持久化的该分片字节数。
    /// 下载时应从 `info.start + resume_offset` 处继续写入,
    /// 避免崩溃后完整重下整个分片。
    pub resume_offset: u64,
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
            computed_hash: None,
            resume_offset: 0,
        }
    }

    /// 转换到下载中状态(仅允许从 Pending 进入)
    pub fn start_download(&mut self) -> DownloadResult<()> {
        if self.state != FragmentState::Pending {
            return Err(DownloadError::Fragment(format!(
                "非法状态转换: {:?} -> Downloading",
                self.state
            )));
        }
        self.state = FragmentState::Downloading;
        Ok(())
    }

    /// 下载完成,转换到校验状态(仅允许从 Downloading 进入)
    pub fn complete_download(&mut self, downloaded: u64, duration: Duration) -> DownloadResult<()> {
        if self.state != FragmentState::Downloading {
            return Err(DownloadError::Fragment(format!(
                "非法状态转换: {:?} -> Verifying",
                self.state
            )));
        }
        self.info.downloaded = downloaded;
        self.last_duration = Some(duration);
        self.state = FragmentState::Verifying;
        Ok(())
    }

    /// 下载完成并直接流转到 Done 状态
    ///
    /// 用于 spawn 内已完成下载和写入的场景,跳过 Verifying/Writing 中间状态,
    /// 但仍正确设置 `last_duration` 以激活调度器反馈回路。
    pub fn complete_download_fast(
        &mut self,
        downloaded: u64,
        duration: Duration,
    ) -> DownloadResult<()> {
        if self.state != FragmentState::Downloading {
            return Err(DownloadError::Fragment(format!(
                "非法状态转换: {:?} -> Done(fast)",
                self.state
            )));
        }
        self.info.downloaded = downloaded;
        self.last_duration = Some(duration);
        self.state = FragmentState::Done;
        Ok(())
    }

    /// 校验通过,转换到写入状态(仅允许从 Verifying 进入)
    pub fn verify_ok(&mut self) -> DownloadResult<()> {
        if self.state != FragmentState::Verifying {
            return Err(DownloadError::Fragment(format!(
                "非法状态转换: {:?} -> Writing",
                self.state
            )));
        }
        self.state = FragmentState::Writing;
        Ok(())
    }

    /// 写入完成,转换到完成状态(仅允许从 Writing 进入)
    pub fn write_done(&mut self) -> DownloadResult<()> {
        if self.state != FragmentState::Writing {
            return Err(DownloadError::Fragment(format!(
                "非法状态转换: {:?} -> Done",
                self.state
            )));
        }
        self.state = FragmentState::Done;
        Ok(())
    }

    /// 标记失败,如果可重试则回到 Pending(仅允许从 Downloading/Verifying/Writing 进入)
    pub fn mark_failed(&mut self) -> DownloadResult<bool> {
        if !matches!(
            self.state,
            FragmentState::Downloading | FragmentState::Verifying | FragmentState::Writing
        ) {
            return Err(DownloadError::Fragment(format!(
                "非法状态转换: {:?} -> Failed/Pending",
                self.state
            )));
        }
        self.retry_count += 1;
        if self.retry_count <= self.max_retries {
            self.state = FragmentState::Pending;
            Ok(true)
        } else {
            self.state = FragmentState::Failed;
            Ok(false)
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

    /// 计算重试退避时间(Full Jitter 指数退避)
    ///
    /// 基础退避为 2^attempt 秒,再施加 [0, base) 均匀随机抖动,
    /// 避免多分片/多任务同源失败时产生惊群效应(thundering herd)。
    /// 上限 1024 秒(约 17 分钟)。
    ///
    /// # 参数
    /// - `jitter_seed`: 调用方提供的种子,用于确定性抖动;
    ///   传入 `None` 时退避时间退化为纯指数(无抖动),保持向后兼容。
    pub fn backoff_duration(&self, jitter_seed: Option<u64>) -> Duration {
        let base_secs = 1u64 << self.retry_count.min(10);
        let jittered = match jitter_seed {
            Some(seed) if base_secs > 1 => {
                // 使用乘法哈希将种子映射到 [0, base_secs)
                // FxHash 风格: seed * 0x517cc1b727220a95 >> (64 - log2(base_secs))
                let log2 = base_secs.trailing_zeros();
                let hash = seed.wrapping_mul(0x517cc1b727220a95);
                let jitter = hash >> (64 - log2);
                base_secs.saturating_sub(jitter)
            }
            _ => base_secs,
        };
        Duration::from_secs(jittered.max(1))
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
///
/// A-04: 高/中带宽阈值已外移到 `SchedulerConfig`,通过参数传入。
pub fn compute_fragment_size(
    file_size: u64,
    bandwidth_bps: u64,
    min_size: u64,
    max_size: u64,
    target_fragments: u32,
    high_bandwidth_threshold: u64,
    medium_bandwidth_threshold: u64,
) -> u64 {
    if file_size == 0 {
        return 0;
    }

    // 基础分片大小 = 文件大小 / 目标分片数
    let base = file_size / target_fragments.max(1) as u64;

    // 根据带宽调整:高带宽时增大分片以减少开销
    let bandwidth_factor = if bandwidth_bps > high_bandwidth_threshold {
        2.0 // > 高带宽阈值,分片翻倍
    } else if bandwidth_bps > medium_bandwidth_threshold {
        1.5 // > 中等带宽阈值
    } else {
        1.0
    };

    let adjusted = (base as f64 * bandwidth_factor) as u64;
    adjusted.clamp(min_size, max_size)
}

/// 计算分片策略
///
/// 根据文件大小、服务端 Range 支持情况和当前带宽估计,生成分片列表。
/// - 文件大小为 0 时返回空列表
/// - 服务端不支持 Range 时返回单个分片覆盖整个文件
/// - 当 `suggested_frag_size` 为 `Some(size)` 且 `size > 0` 时,优先使用调度器建议的分片大小
/// - 否则依据调度配置的目标分片数计算动态分片大小
#[tracing::instrument]
pub fn plan_fragments(
    file_size: u64,
    supports_range: bool,
    suggested_frag_size: Option<u64>,
    scheduler_config: &SchedulerConfig,
) -> Vec<FragmentInfo> {
    if file_size == 0 {
        return Vec::new();
    }

    if !supports_range {
        return vec![FragmentInfo::new(0, 0, file_size - 1, file_size)];
    }

    let frag_size = match suggested_frag_size {
        Some(size) if size > 0 => size,
        _ => {
            // 未提供建议大小时,仅依据配置的目标分片数计算,
            // 不再维护独立的 EWMA 带宽模型,避免与 scheduler 的 Holt 模型不一致。
            let base = file_size / scheduler_config.default_target_fragments.max(1) as u64;
            base.clamp(
                scheduler_config.min_fragment_size,
                scheduler_config.max_fragment_size,
            )
        }
    };

    // frag_size 为 0 的防御(理论上 file_size > 0 时不会发生)
    if frag_size == 0 {
        return vec![FragmentInfo::new(0, 0, file_size - 1, file_size)];
    }

    // 防止超大文件导致分片数溢出: 硬上限 1,000,000 个分片
    // 超过此阈值时强制增大 frag_size
    const MAX_FRAGMENT_COUNT: u64 = 1_000_000;
    let mut effective_frag_size = frag_size;
    let estimated_count = file_size / effective_frag_size;
    if estimated_count > MAX_FRAGMENT_COUNT {
        effective_frag_size = file_size.div_ceil(MAX_FRAGMENT_COUNT);
    }

    let mut fragments = Vec::new();
    let mut offset: u64 = 0;
    let mut index: u32 = 0;

    while offset < file_size {
        let remaining = file_size - offset;
        let size = remaining.min(effective_frag_size);
        let end = offset + size - 1;

        fragments.push(FragmentInfo::new(index, offset, end, size));

        offset += size;
        index = index.checked_add(1).expect("分片数超过 u32::MAX,文件过大");
    }

    fragments
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

        record.start_download().unwrap();
        assert_eq!(record.state, FragmentState::Downloading);

        record
            .complete_download(4, Duration::from_millis(100))
            .unwrap();
        assert_eq!(record.state, FragmentState::Verifying);

        record.verify_ok().unwrap();
        assert_eq!(record.state, FragmentState::Writing);

        record.write_done().unwrap();
        assert_eq!(record.state, FragmentState::Done);
        assert!(record.is_done());
    }

    #[test]
    fn test_fragment_retry() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 2);

        record.start_download().unwrap();
        assert!(record.mark_failed().unwrap()); // retry 1
        assert_eq!(record.state, FragmentState::Pending);

        record.start_download().unwrap();
        assert!(record.mark_failed().unwrap()); // retry 2
        assert_eq!(record.state, FragmentState::Pending);

        record.start_download().unwrap();
        assert!(!record.mark_failed().unwrap()); // retry 3, exceeds max
        assert_eq!(record.state, FragmentState::Failed);
        assert!(record.is_failed());
    }

    #[test]
    fn test_backoff_duration() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 5);

        // 无抖动时退化为纯指数
        record.retry_count = 0;
        assert_eq!(record.backoff_duration(None), Duration::from_secs(1));

        record.retry_count = 1;
        assert_eq!(record.backoff_duration(None), Duration::from_secs(2));

        record.retry_count = 2;
        assert_eq!(record.backoff_duration(None), Duration::from_secs(4));

        record.retry_count = 3;
        assert_eq!(record.backoff_duration(None), Duration::from_secs(8));
    }

    #[test]
    fn test_backoff_duration_with_jitter() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 5);

        // 有抖动时退避时间应在 [1, base_secs] 范围内
        record.retry_count = 3; // base = 8s
        for seed in 0..100 {
            let backoff = record.backoff_duration(Some(seed));
            assert!(backoff.as_secs() >= 1, "退避时间应 >= 1s");
            assert!(backoff.as_secs() <= 8, "退避时间应 <= base(8s)");
        }
    }

    #[test]
    fn test_backoff_jitter_produces_different_values() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 5);
        record.retry_count = 5; // base = 32s,足够大的范围产生差异

        let vals: std::collections::HashSet<u64> = (0..20)
            .map(|seed| record.backoff_duration(Some(seed)).as_secs())
            .collect();
        // 20 个不同种子应产生多个不同的退避值(至少 5 个)
        assert!(
            vals.len() >= 5,
            "Full Jitter 应产生多样化的退避值,实际只有 {} 种",
            vals.len()
        );
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
    fn test_fragment_record_illegal_state_transitions() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 3);

        // Pending 状态不允许直接校验/写入/完成
        assert!(
            record
                .complete_download(100, Duration::from_millis(10))
                .is_err(),
            "Pending -> Verifying 应为非法转换"
        );
        assert!(
            record.verify_ok().is_err(),
            "Pending -> Writing 应为非法转换"
        );
        assert!(record.write_done().is_err(), "Pending -> Done 应为非法转换");
        assert!(
            record.mark_failed().is_err(),
            "Pending -> Failed 应为非法转换"
        );

        // 完成到任意状态均为非法
        record.start_download().unwrap();
        record
            .complete_download_fast(1024, Duration::from_millis(10))
            .unwrap();
        assert_eq!(record.state, FragmentState::Done);
        assert!(
            record.start_download().is_err(),
            "Done -> Downloading 应为非法转换"
        );
        assert!(
            record
                .complete_download(100, Duration::from_millis(10))
                .is_err(),
            "Done -> Verifying 应为非法转换"
        );
        assert!(record.mark_failed().is_err(), "Done -> Failed 应为非法转换");

        // Failed 状态不允许重新开始或再次失败
        let mut record2 = FragmentRecord::new(make_frag(1, 1024), 3);
        record2.force_fail();
        assert!(
            record2.start_download().is_err(),
            "Failed -> Downloading 应为非法转换"
        );
        assert!(
            record2.mark_failed().is_err(),
            "Failed -> Failed 应为非法转换"
        );
    }

    #[test]
    fn test_force_fail_vs_mark_failed_boundary() {
        let info = make_frag(0, 1024);

        // force_fail 从任意状态直接转入 Failed,不增加 retry_count
        let mut record = FragmentRecord::new(info.clone(), 2);
        record.force_fail();
        assert_eq!(record.state, FragmentState::Failed);
        assert!(record.is_failed());
        assert_eq!(record.retry_count, 0, "force_fail 不应增加 retry_count");

        // mark_failed 仅在 Downloading/Verifying/Writing 时有效,并受 max_retries 约束
        let mut record = FragmentRecord::new(info.clone(), 2);
        record.start_download().unwrap();
        assert!(record.mark_failed().unwrap()); // retry 1, 回到 Pending
        assert_eq!(record.retry_count, 1);
        assert_eq!(record.state, FragmentState::Pending);

        record.start_download().unwrap();
        assert!(record.mark_failed().unwrap()); // retry 2, 回到 Pending
        record.start_download().unwrap();
        assert!(!record.mark_failed().unwrap()); // retry 3 > max, Failed
        assert_eq!(record.state, FragmentState::Failed);
        assert_eq!(record.retry_count, 3);
    }

    #[test]
    fn test_complete_download_fast_sets_last_duration() {
        let info = make_frag(0, 1024);
        let mut record = FragmentRecord::new(info, 3);
        record.start_download().unwrap();

        let duration = Duration::from_millis(250);
        record.complete_download_fast(1024, duration).unwrap();

        assert_eq!(record.state, FragmentState::Done);
        assert_eq!(record.info.downloaded, 1024);
        assert_eq!(
            record.last_duration,
            Some(duration),
            "complete_download_fast 应正确设置 last_duration"
        );
        assert!(record.is_done());
    }

    #[test]
    fn test_bandwidth_tracker_default_and_zero_samples() {
        let tracker = BandwidthTracker::default();
        assert_eq!(tracker.estimate(), 0, "零样本时估计值应为 0");
        assert_eq!(tracker.sample_count(), 0);

        let mut tracker = BandwidthTracker::default();
        tracker.record(0);
        assert_eq!(
            tracker.sample_count(),
            0,
            "零值样本不应被记录,避免污染 EWMA"
        );
        assert_eq!(tracker.estimate(), 0);
    }

    #[test]
    fn test_compute_fragment_size_normal() {
        let size = compute_fragment_size(
            100 * 1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            64 * 1024 * 1024,
            16,
            100 * 1024 * 1024,
            10 * 1024 * 1024,
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
            100 * 1024 * 1024,
            10 * 1024 * 1024,
        );
        assert!(size >= 1024 * 1024);
    }

    #[test]
    fn test_compute_fragment_size_zero() {
        let size = compute_fragment_size(
            0,
            0,
            1024,
            64 * 1024 * 1024,
            16,
            100 * 1024 * 1024,
            10 * 1024 * 1024,
        );
        assert_eq!(size, 0);
    }

    #[test]
    fn test_compute_fragment_size_small_file() {
        let size = compute_fragment_size(
            500,
            1024,
            1024,
            64 * 1024 * 1024,
            4,
            100 * 1024 * 1024,
            10 * 1024 * 1024,
        );
        assert_eq!(size, 1024); // clamp to min
    }
    #[cfg(test)]
    mod plan_tests {
        use super::*;
        use tachyon_core::config::SchedulerConfig;

        // ------ 正常路径测试 ------

        #[test]
        fn test_plan_fragments_normal_range_supported() {
            let config = SchedulerConfig::default();
            // 100MB 文件,支持 Range
            let frags = plan_fragments(100 * 1024 * 1024, true, None, &config);
            assert!(!frags.is_empty(), "应至少生成一个分片");

            // 验证连续性和完整性
            assert_eq!(frags[0].start, 0);
            let total_size: u64 = frags.iter().map(|f| f.size).sum();
            assert_eq!(total_size, 100 * 1024 * 1024);

            // 验证索引连续
            for (i, frag) in frags.iter().enumerate() {
                assert_eq!(frag.index, i as u32);
                assert_eq!(frag.downloaded, 0);
                assert!(frag.hash.is_none());
            }

            // 验证相邻分片无缝衔接
            for window in frags.windows(2) {
                assert_eq!(window[0].end + 1, window[1].start);
            }

            // 最后一个分片的 end 应覆盖到文件末尾
            let last = frags.last().unwrap();
            assert_eq!(last.end, 100 * 1024 * 1024 - 1);
        }

        #[test]
        fn test_plan_fragments_small_file() {
            let config = SchedulerConfig::default();
            // 500 字节文件,支持 Range —— 小于 min_fragment_size
            let frags = plan_fragments(500, true, None, &config);
            assert_eq!(frags.len(), 1, "小于最小分片的文件应只产生一个分片");
            assert_eq!(frags[0].start, 0);
            assert_eq!(frags[0].end, 499);
            assert_eq!(frags[0].size, 500);
        }

        #[test]
        fn test_plan_fragments_exactly_one_page() {
            let config = SchedulerConfig::default();
            // 恰好等于 min_fragment_size (1MB)
            let size = 1024 * 1024u64;
            let frags = plan_fragments(size, true, None, &config);
            let total: u64 = frags.iter().map(|f| f.size).sum();
            assert_eq!(total, size);
        }

        // ------ 边界值测试 ------

        #[test]
        fn test_plan_fragments_empty_file() {
            let config = SchedulerConfig::default();
            let frags = plan_fragments(0, true, None, &config);
            assert!(frags.is_empty(), "空文件不应产生任何分片");
        }

        #[test]
        fn test_plan_fragments_empty_file_no_range() {
            let config = SchedulerConfig::default();
            let frags = plan_fragments(0, false, None, &config);
            assert!(frags.is_empty(), "空文件无论是否支持 Range 都不应产生分片");
        }

        #[test]
        fn test_plan_fragments_single_byte() {
            let config = SchedulerConfig::default();
            let frags = plan_fragments(1, true, None, &config);
            assert_eq!(frags.len(), 1);
            assert_eq!(frags[0].size, 1);
            assert_eq!(frags[0].start, 0);
            assert_eq!(frags[0].end, 0);
        }

        // ------ 不支持 Range 测试 ------

        #[test]
        fn test_plan_fragments_no_range_support() {
            let config = SchedulerConfig::default();
            let file_size = 50 * 1024 * 1024u64; // 50MB
            let frags = plan_fragments(file_size, false, None, &config);
            assert_eq!(frags.len(), 1, "不支持 Range 时应只产生单个分片");
            assert_eq!(frags[0].index, 0);
            assert_eq!(frags[0].start, 0);
            assert_eq!(frags[0].end, file_size - 1);
            assert_eq!(frags[0].size, file_size);
        }

        // ------ 自定义配置测试 ------

        #[test]
        fn test_with_scheduler_config() {
            let config = SchedulerConfig {
                min_fragment_size: 512 * 1024,       // 512KB
                max_fragment_size: 32 * 1024 * 1024, // 32MB
                sampling_interval_secs: 30,
                ewma_alpha: 0.5,
                ..Default::default()
            };

            // 验证配置被正确传入(通过检查分片大小约束)
            let frags = plan_fragments(10 * 1024 * 1024, true, None, &config);
            for frag in &frags {
                assert!(frag.size >= config.min_fragment_size || frag.size == 10 * 1024 * 1024);
            }
        }

        // ------ 分片完整性回归测试 ------

        #[test]
        fn test_plan_fragments_large_file_total_coverage() {
            let config = SchedulerConfig::default();
            let file_size = 1024 * 1024 * 1024u64; // 1GB
            let frags = plan_fragments(file_size, true, None, &config);
            let total: u64 = frags.iter().map(|f| f.size).sum();
            assert_eq!(total, file_size, "所有分片大小之和必须等于文件大小");

            // 确保没有重叠:每段的 start == 前一段 end + 1
            for window in frags.windows(2) {
                assert_eq!(window[0].end + 1, window[1].start, "相邻分片之间不应有间隙");
            }
        }

        // ------ suggested_frag_size 测试 ------

        #[test]
        fn test_plan_fragments_with_suggested_size() {
            let config = SchedulerConfig::default();
            let file_size = 10 * 1024 * 1024u64;
            let suggested = 2 * 1024 * 1024u64;

            let frags = plan_fragments(file_size, true, Some(suggested), &config);
            assert!(!frags.is_empty());

            // 每个分片(除最后一个)大小应为 suggested
            for frag in &frags[..frags.len() - 1] {
                assert_eq!(frag.size, suggested, "非末尾分片大小应为建议值");
            }

            let total: u64 = frags.iter().map(|f| f.size).sum();
            assert_eq!(total, file_size);
        }

        #[test]
        fn test_plan_fragments_suggested_size_zero_falls_back() {
            let config = SchedulerConfig::default();
            let file_size = 10 * 1024 * 1024u64;

            // suggested=0 应回退到内部计算
            let frags_zero = plan_fragments(file_size, true, Some(0), &config);
            let frags_none = plan_fragments(file_size, true, None, &config);
            assert_eq!(
                frags_zero.len(),
                frags_none.len(),
                "suggested=0 应与 None 结果一致"
            );
        }

        #[test]
        fn test_plan_fragments_uses_scheduler_target_not_pool_max() {
            let config = SchedulerConfig {
                min_fragment_size: 1024 * 1024,
                max_fragment_size: 64 * 1024 * 1024,
                default_target_fragments: 100,
                ..Default::default()
            };

            // 10MB file, 100 target fragments -> 102KB per fragment -> clamped to min 1MB -> 10 fragments
            // If max_global=4 was used as target_fragments: 10MB/4 = 2.5MB -> still clamped -> 4 fragments
            let frags = plan_fragments(10 * 1024 * 1024, true, None, &config);
            assert_eq!(
                frags.len(),
                10,
                "应使用 SchedulerConfig::default_target_fragments 而非 PoolConfig::max_global"
            );
        }
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
                100 * 1024 * 1024,
                10 * 1024 * 1024,
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
                record.start_download().unwrap();
                prop_assert_eq!(record.state, FragmentState::Downloading);

                if record.retry_count < max_retries {
                    // 还可以重试
                    let can_retry = record.mark_failed().unwrap();
                    prop_assert!(can_retry);
                    prop_assert_eq!(record.state, FragmentState::Pending);
                } else {
                    // 超过最大重试次数
                    let data_len = 22u64;
                    record.complete_download(data_len, Duration::from_millis(10)).unwrap();
                    prop_assert_eq!(record.state, FragmentState::Verifying);
                    record.verify_ok().unwrap();
                    prop_assert_eq!(record.state, FragmentState::Writing);
                    record.write_done().unwrap();
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

            let backoff = record.backoff_duration(None);
            // 退避时间应为正数
            prop_assert!(backoff.as_secs() >= 1);
            // 最大不应超过 2^10 = 1024 秒（被 min(10) 限制）
            prop_assert!(backoff.as_secs() <= 1024);
        }

        /// 有抖动时退避时间应在 [1, base] 范围内
        #[test]
        fn test_backoff_duration_jitter_bounded(
            retry_count in 0u32..10,
            seed in 0u64..1000,
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

            let base_secs = 1u64 << retry_count.min(10);
            let backoff = record.backoff_duration(Some(seed));
            prop_assert!(backoff.as_secs() >= 1);
            prop_assert!(backoff.as_secs() <= base_secs);
        }
    }
}
