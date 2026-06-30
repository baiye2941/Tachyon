//! 磁力下载进度桥接
//!
//! librqbit 的 [`ManagedTorrent::stats`] 返回 `progress_bytes / total_bytes`,
//! 但下载管线下游(chunk reader / UI)依赖 [`FragmentProgress`] 字节流模型。
//! 本模块封装三件纯逻辑,以便单元测试覆盖,不接触 librqbit/tokio runtime:
//!
//! 1. [`bt_stats_to_progress`] — 把 `(progress_bytes, total_bytes, finished)` 三元组
//!    映射为单分片 [`FragmentProgress`](`tachyon_core::FragmentProgress`),
//!    BT 协议对外只暴露 1 个虚拟"分片"(`fragment_index = 0`)。
//!
//! 2. [`StallMonitor`] — 跟踪连续无进度增长的时长。当 `stall_timeout_secs > 0`
//!    且距离上一次 progress 增长超过该秒数,`check()` 返回 `true` 触发任务失败。
//!    `stall_timeout_secs == 0` 永远返回 `false`(显式禁用看门狗)。
//!
//! 3. [`ProgressEmitter`] — 进度去重发射器。在 BT 轮询循环中,只有当
//!    `progress_bytes` 严格大于上次发射值时才返回 `Some(FragmentProgress)`。
//!    避免下游 chunk_reader 在"500ms 周期内字节未变"的窗口里把速度计算成 0,
//!    从而导致 UI speed 在真实值与 0 之间抖动。

use std::time::{Duration, Instant};

use tachyon_core::FragmentProgress;

/// 把 librqbit 的 (progress_bytes, total_bytes, finished) 映射为 FragmentProgress
///
/// BT 协议对外只暴露 1 个虚拟分片 (`fragment_index = 0`),
/// 由 chunk reader 直接把 `fragment_downloaded` 视作总下载字节。
///
/// `completed` 仅在 `finished == true` 时为 true。`progress_bytes > total_bytes` 时
/// (理论上 librqbit 不会出现,但防御性裁剪)按 `total_bytes` 截断。
pub fn bt_stats_to_progress(
    progress_bytes: u64,
    total_bytes: u64,
    finished: bool,
) -> FragmentProgress {
    let bounded = if total_bytes > 0 {
        progress_bytes.min(total_bytes)
    } else {
        progress_bytes
    };
    FragmentProgress {
        fragment_index: 0,
        completed: finished,
        fragment_downloaded: bounded,
    }
}

/// 无进度增长看门狗
///
/// 每次 `observe(progress_bytes)` 被调用时更新内部状态:
/// - `progress_bytes > last_progress` → 重置"无增长"起点为当前时刻
/// - 否则维持上一次的"无增长"起点
///
/// `check(now)` 在 `stall_timeout_secs > 0` 且距离最近一次进度增长超过
/// `stall_timeout_secs` 时返回 `true`。`stall_timeout_secs == 0` 永远返回 `false`。
///
/// 不依赖 tokio,内部仅记录 `Instant`,由调用方注入 `now` 以保持可测试性。
pub struct StallMonitor {
    last_progress: u64,
    last_growth_at: Instant,
    timeout: Duration,
}

impl StallMonitor {
    /// 构造看门狗
    ///
    /// `stall_timeout_secs == 0` 表示禁用(`check` 永远返回 false)。
    pub fn new(initial_progress: u64, stall_timeout_secs: u64, now: Instant) -> Self {
        Self {
            last_progress: initial_progress,
            last_growth_at: now,
            timeout: Duration::from_secs(stall_timeout_secs),
        }
    }

    /// 更新观测值,如有新字节则刷新"无增长"起点
    pub fn observe(&mut self, progress_bytes: u64, now: Instant) {
        if progress_bytes > self.last_progress {
            self.last_progress = progress_bytes;
            self.last_growth_at = now;
        }
    }

    /// 检查是否已触发 stall 超时
    pub fn is_stalled(&self, now: Instant) -> bool {
        if self.timeout.is_zero() {
            return false;
        }
        now.saturating_duration_since(self.last_growth_at) >= self.timeout
    }
}

/// 进度去重发射器
///
/// BT 轮询每 N 毫秒拉一次 `ManagedTorrent::stats()`,但下载速度并非每个窗口
/// 都有 chunk 落盘:slow seeders / 校验 piece 阶段可能整窗为零增长。
/// 若 worker 把"字节数相同的 FragmentProgress"也推送给 chunk reader,
/// 后者在该窗口看到 delta=0 → `speed = 0/Δt`,UI 显示速度归零;
/// 下一窗口又看到 delta=正常 → speed 跳回真实值。
/// 表现为速度在 0 与真实速率之间锯齿状抖动。
///
/// `ProgressEmitter::next_event` 严格只在 `progress_bytes > last_emitted` 或
/// `finished == true` 时返回 `Some(FragmentProgress)`,把"无变化轮询"在源头
/// 抑制掉,使下游永远看不到零增长事件,速度自然平滑。
pub struct ProgressEmitter {
    last_emitted: u64,
    /// 是否已经在 finished 路径上发出过完成事件,避免重复发送 completed=true
    finished_emitted: bool,
}

impl Default for ProgressEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressEmitter {
    /// 创建发射器,初始 last_emitted=0(允许首次任何 >0 的字节数被推送)
    pub fn new() -> Self {
        Self {
            last_emitted: 0,
            finished_emitted: false,
        }
    }

    /// 以 `progress_bytes` 已下载的初始值构造发射器
    ///
    /// 用于断点续传:任务恢复时已有 `resume_bytes` 字节落盘,
    /// 后续轮询只应在超过该值时上报,避免 chunk_reader 出现"负 delta"。
    pub fn with_initial(initial_bytes: u64) -> Self {
        Self {
            last_emitted: initial_bytes,
            finished_emitted: false,
        }
    }

    /// 根据本轮 stats 决定是否发射进度事件
    ///
    /// 返回 `Some(FragmentProgress)` 当且仅当:
    /// - `progress_bytes > last_emitted` (有新字节落盘),或
    /// - `finished == true` 且此前未发射过 completed(下载完成的最终事件)
    ///
    /// 否则返回 `None`,调用方应跳过本轮上报。
    pub fn next_event(
        &mut self,
        progress_bytes: u64,
        total_bytes: u64,
        finished: bool,
    ) -> Option<FragmentProgress> {
        if finished && !self.finished_emitted {
            self.finished_emitted = true;
            // 完成事件:即便字节数与上次相同,也要让下游知道 completed=true
            self.last_emitted = self.last_emitted.max(progress_bytes);
            return Some(bt_stats_to_progress(progress_bytes, total_bytes, true));
        }
        if progress_bytes > self.last_emitted {
            self.last_emitted = progress_bytes;
            return Some(bt_stats_to_progress(progress_bytes, total_bytes, finished));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bt_stats_to_progress_zero_progress() {
        let p = bt_stats_to_progress(0, 1024, false);
        assert_eq!(p.fragment_index, 0);
        assert_eq!(p.fragment_downloaded, 0);
        assert!(!p.completed);
    }

    #[test]
    fn test_bt_stats_to_progress_partial() {
        let p = bt_stats_to_progress(512, 1024, false);
        assert_eq!(p.fragment_downloaded, 512);
        assert!(!p.completed);
    }

    #[test]
    fn test_bt_stats_to_progress_completed_marks_finished() {
        let p = bt_stats_to_progress(1024, 1024, true);
        assert_eq!(p.fragment_downloaded, 1024);
        assert!(p.completed, "finished=true 应映射为 completed=true");
    }

    #[test]
    fn test_bt_stats_to_progress_clamps_overshoot() {
        // 防御性:librqbit 不应出现 progress > total,但即便出现也不能让 UI 进度条爆掉
        let p = bt_stats_to_progress(2000, 1000, true);
        assert_eq!(p.fragment_downloaded, 1000);
    }

    #[test]
    fn test_bt_stats_to_progress_unknown_total_keeps_progress() {
        // 罕见路径:total_bytes 暂未就绪(metadata 仍在补全),不能因此把 progress 抹零
        let p = bt_stats_to_progress(123, 0, false);
        assert_eq!(p.fragment_downloaded, 123);
    }

    #[test]
    fn test_stall_monitor_disabled_when_timeout_zero() {
        let t0 = Instant::now();
        let mon = StallMonitor::new(0, 0, t0);
        let later = t0 + Duration::from_secs(3600);
        assert!(
            !mon.is_stalled(later),
            "stall_timeout_secs=0 应禁用看门狗,任意时长都不触发"
        );
    }

    #[test]
    fn test_stall_monitor_triggers_after_timeout() {
        let t0 = Instant::now();
        let mon = StallMonitor::new(0, 120, t0);
        // t0 + 119s 未到阈值
        assert!(!mon.is_stalled(t0 + Duration::from_secs(119)));
        // t0 + 120s 触发
        assert!(mon.is_stalled(t0 + Duration::from_secs(120)));
    }

    #[test]
    fn test_stall_monitor_observe_growth_resets_window() {
        let t0 = Instant::now();
        let mut mon = StallMonitor::new(0, 60, t0);
        // t0 + 59s 仍未超时
        assert!(!mon.is_stalled(t0 + Duration::from_secs(59)));
        // t0 + 59s 有新进度,刷新"无增长"起点
        mon.observe(1024, t0 + Duration::from_secs(59));
        // t0 + 59 + 59 = 118s,从重置点起未超 60s 不触发
        assert!(!mon.is_stalled(t0 + Duration::from_secs(118)));
        // t0 + 59 + 60 = 119s,从重置点正好满 60s 才触发
        assert!(mon.is_stalled(t0 + Duration::from_secs(119)));
    }

    #[test]
    fn test_stall_monitor_observe_no_growth_does_not_reset() {
        let t0 = Instant::now();
        let mut mon = StallMonitor::new(100, 60, t0);
        // 同样的 progress 上报多次不应延长窗口
        mon.observe(100, t0 + Duration::from_secs(10));
        mon.observe(100, t0 + Duration::from_secs(30));
        mon.observe(100, t0 + Duration::from_secs(59));
        assert!(mon.is_stalled(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn test_stall_monitor_observe_regression_does_not_reset() {
        // 防御性:若 stats 报告的字节数倒退(理论上不会),不应误判为"有进展"
        let t0 = Instant::now();
        let mut mon = StallMonitor::new(500, 60, t0);
        mon.observe(400, t0 + Duration::from_secs(30));
        // 30s 时没有真正的"增长",60s 时应仍触发
        assert!(mon.is_stalled(t0 + Duration::from_secs(60)));
    }

    // ── ProgressEmitter ─────────────────────────────────────────────

    #[test]
    fn test_emitter_first_growth_emits() {
        let mut emitter = ProgressEmitter::new();
        let evt = emitter.next_event(1024, 10_000, false);
        assert!(evt.is_some(), "首次有字节增长应发射事件");
        assert_eq!(evt.unwrap().fragment_downloaded, 1024);
    }

    #[test]
    fn test_emitter_no_growth_emits_none() {
        // 这是 BT speed 抖动问题的核心:0 增长窗口必须返回 None
        let mut emitter = ProgressEmitter::new();
        emitter.next_event(1024, 10_000, false);
        let evt = emitter.next_event(1024, 10_000, false);
        assert!(evt.is_none(), "字节未变时不应再发射事件,避免 speed 抖动");
    }

    #[test]
    fn test_emitter_alternating_growth_and_no_growth() {
        // 模拟实际场景:slow seeder 下 progress 在某些 500ms 窗口里增长,
        // 某些窗口不变。下游应只看到"增长"事件,使 speed 计算永远基于正 delta。
        let mut emitter = ProgressEmitter::new();
        assert!(emitter.next_event(1_000, 100_000, false).is_some());
        assert!(emitter.next_event(1_000, 100_000, false).is_none());
        assert!(emitter.next_event(1_500, 100_000, false).is_some());
        assert!(emitter.next_event(1_500, 100_000, false).is_none());
        assert!(emitter.next_event(2_000, 100_000, false).is_some());
    }

    #[test]
    fn test_emitter_regression_emits_none() {
        // 防御性:若 stats 因 librqbit 内部异常报告小于上次的字节数,
        // 也不能让 chunk_reader 看到"负 delta"。
        let mut emitter = ProgressEmitter::new();
        emitter.next_event(2_000, 10_000, false);
        let evt = emitter.next_event(1_500, 10_000, false);
        assert!(evt.is_none(), "字节倒退不应发射事件");
    }

    #[test]
    fn test_emitter_finished_emits_even_without_growth() {
        // 完成事件:即便字节没有增长(已是最终大小),也必须发射 completed=true
        // 让下游 chunk_reader 触发最终 checkpoint。
        let mut emitter = ProgressEmitter::new();
        emitter.next_event(10_000, 10_000, false);
        let evt = emitter.next_event(10_000, 10_000, true);
        assert!(
            evt.is_some(),
            "completion 事件即便无字节增长也应发射,触发下游 checkpoint"
        );
        assert!(evt.unwrap().completed);
    }

    #[test]
    fn test_emitter_finished_emitted_only_once() {
        // 同一次下载内,completion 事件只应发射一次,避免下游重复 checkpoint
        let mut emitter = ProgressEmitter::new();
        emitter.next_event(10_000, 10_000, false);
        assert!(emitter.next_event(10_000, 10_000, true).is_some());
        assert!(
            emitter.next_event(10_000, 10_000, true).is_none(),
            "completion 事件只应发射一次"
        );
    }

    #[test]
    fn test_emitter_with_initial_skips_resume_offset() {
        // 断点续传:已下载 5000 字节,后续 stats 仍在 0..=5000 区间内
        // 不应让 chunk_reader 看到负 delta。
        let mut emitter = ProgressEmitter::with_initial(5_000);
        assert!(emitter.next_event(3_000, 10_000, false).is_none());
        assert!(emitter.next_event(5_000, 10_000, false).is_none());
        assert!(
            emitter.next_event(5_001, 10_000, false).is_some(),
            "超过初始值后才开始上报"
        );
    }
}
