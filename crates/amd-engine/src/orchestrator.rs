//! 下载编排器
//!
//! 协调分片策略计算、连接池和带宽追踪,为上层提供统一的下载管理接口。
//! 核心职责:
//! - 根据文件大小和带宽估计计算最优分片策略
//! - 跟踪分片完成情况并更新带宽模型
//! - 暴露连接池状态供调度层查询

use std::time::Duration;

use std::sync::Arc;

use amd_core::config::SchedulerConfig;
use amd_core::types::FragmentInfo;

use crate::connection::{ConnectionPool, PoolConfig};
use crate::fragment::{BandwidthTracker, FragmentRecord, compute_fragment_size};

/// 下载编排器,统一管理分片策略、连接池与带宽追踪
pub struct DownloadOrchestrator {
    pool: Arc<ConnectionPool>,
    bandwidth: BandwidthTracker,
    scheduler_config: SchedulerConfig,
    active_fragments: Vec<FragmentRecord>,
}

impl DownloadOrchestrator {
    pub fn new(pool_config: PoolConfig) -> Self {
        Self {
            pool: Arc::new(ConnectionPool::new(pool_config)),
            bandwidth: BandwidthTracker::default(),
            scheduler_config: SchedulerConfig::default(),
            active_fragments: Vec::new(),
        }
    }

    pub fn with_shared_pool(pool: Arc<ConnectionPool>, scheduler_config: SchedulerConfig) -> Self {
        Self {
            pool,
            bandwidth: BandwidthTracker::new(scheduler_config.ewma_alpha),
            scheduler_config,
            active_fragments: Vec::new(),
        }
    }

    pub fn with_scheduler_config(
        pool_config: PoolConfig,
        scheduler_config: SchedulerConfig,
    ) -> Self {
        Self {
            pool: Arc::new(ConnectionPool::new(pool_config)),
            bandwidth: BandwidthTracker::new(scheduler_config.ewma_alpha),
            scheduler_config,
            active_fragments: Vec::new(),
        }
    }

    /// 计算分片策略
    ///
    /// 根据文件大小、服务端 Range 支持情况和当前带宽估计,生成分片列表。
    /// - 文件大小为 0 时返回空列表
    /// - 服务端不支持 Range 时返回单个分片覆盖整个文件
    /// - 当 `suggested_frag_size` 为 `Some(size)` 且 `size > 0` 时,优先使用调度器建议的分片大小
    /// - 否则调用 `compute_fragment_size` 计算动态分片大小
    #[tracing::instrument(skip(self))]
    pub fn plan_fragments(
        &self,
        file_size: u64,
        supports_range: bool,
        suggested_frag_size: Option<u64>,
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
                let bandwidth_bps = self.bandwidth.estimate();
                compute_fragment_size(
                    file_size,
                    bandwidth_bps,
                    self.scheduler_config.min_fragment_size,
                    self.scheduler_config.max_fragment_size,
                    self.pool.config().max_global,
                )
            }
        };

        // frag_size 为 0 的防御(理论上 file_size > 0 时不会发生)
        if frag_size == 0 {
            return vec![FragmentInfo::new(0, 0, file_size - 1, file_size)];
        }

        let mut fragments = Vec::new();
        let mut offset: u64 = 0;
        let mut index: u32 = 0;

        while offset < file_size {
            let remaining = file_size - offset;
            let size = remaining.min(frag_size);
            let end = offset + size - 1;

            fragments.push(FragmentInfo::new(index, offset, end, size));

            offset += size;
            index += 1;
        }

        fragments
    }

    /// 记录分片完成,更新带宽追踪
    ///
    /// 根据字节数和耗时计算即时带宽,并更新 EWMA 模型。
    /// 如果 `duration` 为零则跳过(避免除零)。
    pub fn on_fragment_done(&mut self, bytes: u64, duration: Duration) {
        let secs = duration.as_secs_f64();
        if secs <= 0.0 || bytes == 0 {
            return;
        }
        let bytes_per_sec = (bytes as f64 / secs) as u64;
        self.bandwidth.record(bytes_per_sec);
    }

    /// 注册分片到活跃列表(供上层追踪分片状态)
    pub fn register_fragment(&mut self, info: FragmentInfo) {
        self.active_fragments.push(FragmentRecord::new(info, 3));
    }

    /// 标记分片完成并更新带宽追踪(供上层在分片下载完成后调用)
    pub fn on_fragment_complete(&mut self, info: &FragmentInfo, duration: Duration) {
        self.on_fragment_done(info.size, duration);
        if let Some(record) = self.active_fragments.get_mut(info.index as usize) {
            record.write_done();
        }
    }

    /// 获取活跃分片记录的不可变引用(供上层查询分片状态)
    pub fn active_fragments(&self) -> &[FragmentRecord] {
        &self.active_fragments
    }

    /// 获取当前带宽估计(字节/秒)
    pub fn estimated_bandwidth(&self) -> u64 {
        self.bandwidth.estimate()
    }

    pub fn active_connections(&self) -> u32 {
        self.pool.active_connections()
    }

    pub fn pool(&self) -> &Arc<ConnectionPool> {
        &self.pool
    }

    /// 获取带宽追踪器的不可变引用
    pub fn bandwidth_tracker(&self) -> &BandwidthTracker {
        &self.bandwidth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 辅助函数:创建默认配置的编排器
    fn make_orchestrator() -> DownloadOrchestrator {
        DownloadOrchestrator::new(PoolConfig::default())
    }

    /// 辅助函数:创建自定义池配置的编排器
    #[allow(dead_code)]
    fn make_orchestrator_with_pool(max_per_host: u32, max_global: u32) -> DownloadOrchestrator {
        DownloadOrchestrator::new(PoolConfig {
            max_per_host,
            max_global,
        })
    }

    // ------ 正常路径测试 ------

    #[test]
    fn test_plan_fragments_normal_range_supported() {
        let orch = make_orchestrator();
        // 100MB 文件,支持 Range
        let frags = orch.plan_fragments(100 * 1024 * 1024, true, None);
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
        let orch = make_orchestrator();
        // 500 字节文件,支持 Range —— 小于 min_fragment_size
        let frags = orch.plan_fragments(500, true, None);
        assert_eq!(frags.len(), 1, "小于最小分片的文件应只产生一个分片");
        assert_eq!(frags[0].start, 0);
        assert_eq!(frags[0].end, 499);
        assert_eq!(frags[0].size, 500);
    }

    #[test]
    fn test_plan_fragments_exactly_one_page() {
        let orch = make_orchestrator();
        // 恰好等于 min_fragment_size (1MB)
        let size = 1024 * 1024u64;
        let frags = orch.plan_fragments(size, true, None);
        let total: u64 = frags.iter().map(|f| f.size).sum();
        assert_eq!(total, size);
    }

    // ------ 边界值测试 ------

    #[test]
    fn test_plan_fragments_empty_file() {
        let orch = make_orchestrator();
        let frags = orch.plan_fragments(0, true, None);
        assert!(frags.is_empty(), "空文件不应产生任何分片");
    }

    #[test]
    fn test_plan_fragments_empty_file_no_range() {
        let orch = make_orchestrator();
        let frags = orch.plan_fragments(0, false, None);
        assert!(frags.is_empty(), "空文件无论是否支持 Range 都不应产生分片");
    }

    #[test]
    fn test_plan_fragments_single_byte() {
        let orch = make_orchestrator();
        let frags = orch.plan_fragments(1, true, None);
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].size, 1);
        assert_eq!(frags[0].start, 0);
        assert_eq!(frags[0].end, 0);
    }

    // ------ 不支持 Range 测试 ------

    #[test]
    fn test_plan_fragments_no_range_support() {
        let orch = make_orchestrator();
        let file_size = 50 * 1024 * 1024u64; // 50MB
        let frags = orch.plan_fragments(file_size, false, None);
        assert_eq!(frags.len(), 1, "不支持 Range 时应只产生单个分片");
        assert_eq!(frags[0].index, 0);
        assert_eq!(frags[0].start, 0);
        assert_eq!(frags[0].end, file_size - 1);
        assert_eq!(frags[0].size, file_size);
    }

    // ------ 带宽追踪测试 ------

    #[test]
    fn test_on_fragment_done_updates_bandwidth() {
        let mut orch = make_orchestrator();
        assert_eq!(orch.estimated_bandwidth(), 0, "初始带宽应为 0");

        // 1MB 在 1 秒内完成 => 约 1MB/s
        orch.on_fragment_done(1024 * 1024, Duration::from_secs(1));
        assert_eq!(orch.estimated_bandwidth(), 1024 * 1024);

        // 再记录一次更高带宽
        orch.on_fragment_done(2 * 1024 * 1024, Duration::from_secs(1));
        // EWMA: 0.3 * 2MB + 0.7 * 1MB = 0.6MB + 0.7MB = 1.3MB
        let expected = (0.3 * (2.0 * 1024.0 * 1024.0) + 0.7 * (1024.0 * 1024.0)) as u64;
        assert_eq!(orch.estimated_bandwidth(), expected);
    }

    #[test]
    fn test_on_fragment_done_zero_duration_ignored() {
        let mut orch = make_orchestrator();
        orch.on_fragment_done(1024, Duration::ZERO);
        assert_eq!(orch.estimated_bandwidth(), 0, "零耗时应被忽略,带宽不变");
    }

    #[test]
    fn test_on_fragment_done_zero_bytes_ignored() {
        let mut orch = make_orchestrator();
        orch.on_fragment_done(0, Duration::from_secs(1));
        assert_eq!(orch.estimated_bandwidth(), 0, "零字节应被忽略,带宽不变");
    }

    #[test]
    fn test_on_fragment_done_subsecond_duration() {
        let mut orch = make_orchestrator();
        // 512KB 在 500ms 内完成 => 1024KB/s = 1024000 B/s
        orch.on_fragment_done(512 * 1024, Duration::from_millis(500));
        assert_eq!(orch.estimated_bandwidth(), 1024 * 1024);
    }

    // ------ 连接池集成测试 ------

    #[tokio::test]
    async fn test_active_connections_initial() {
        let orch = make_orchestrator();
        assert_eq!(orch.active_connections(), 0);
    }

    #[tokio::test]
    async fn test_active_connections_with_permits() {
        let orch = make_orchestrator();
        let _permit = orch.pool().acquire("example.com").await.unwrap();
        assert_eq!(orch.active_connections(), 1);
    }

    // ------ 自定义配置测试 ------

    #[test]
    fn test_with_scheduler_config() {
        let config = SchedulerConfig {
            min_fragment_size: 512 * 1024,       // 512KB
            max_fragment_size: 32 * 1024 * 1024, // 32MB
            sampling_interval_secs: 30,
            ewma_alpha: 0.5,
        };
        let orch =
            DownloadOrchestrator::with_scheduler_config(PoolConfig::default(), config.clone());

        // 验证配置被正确传入(通过检查分片大小约束)
        let frags = orch.plan_fragments(10 * 1024 * 1024, true, None);
        for frag in &frags {
            assert!(frag.size >= config.min_fragment_size || frag.size == 10 * 1024 * 1024);
        }
    }

    #[test]
    fn test_bandwidth_tracker_accessor() {
        let orch = make_orchestrator();
        assert_eq!(orch.bandwidth_tracker().sample_count(), 0);
    }

    // ------ 分片完整性回归测试 ------

    #[test]
    fn test_plan_fragments_large_file_total_coverage() {
        let orch = make_orchestrator();
        let file_size = 1024 * 1024 * 1024u64; // 1GB
        let frags = orch.plan_fragments(file_size, true, None);
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
        let orch = make_orchestrator();
        let file_size = 10 * 1024 * 1024u64;
        let suggested = 2 * 1024 * 1024u64;

        let frags = orch.plan_fragments(file_size, true, Some(suggested));
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
        let orch = make_orchestrator();
        let file_size = 10 * 1024 * 1024u64;

        // suggested=0 应回退到内部计算
        let frags_zero = orch.plan_fragments(file_size, true, Some(0));
        let frags_none = orch.plan_fragments(file_size, true, None);
        assert_eq!(
            frags_zero.len(),
            frags_none.len(),
            "suggested=0 应与 None 结果一致"
        );
    }
}
