//! 可升降的闭环并发控制器
//!
//! 解决 tokio::Semaphore `add_permits` 只能增不能减的限制。
//!
//! ## 背景(FastBioDL 论文 arXiv 2508.05511)
//! 闭环并发控制:测吞吐 → 算效用梯度 → 步进并发度。
//! 当前 `execute_fragmented_download` 的 `reschedule_timer` 只能 add_permits 提升
//! (permits 不可回收),降并发靠 task 自然完成不 spawn 新 task——但这依赖
//! semaphore 自然耗尽,无法精确控制目标并发度。
//!
//! ## 方案
//! `ConcurrencyController` 维护 `active`(当前在途 task 数)和 `target`(目标并发度)。
//! - `should_spawn()`: active < target 时允许 spawn
//! - `set_target()`: 可升可降,立即生效(新 task 受新 target 约束,已有 task 不中断)
//! - `record_spawn()` / `record_complete()`: 更新 active 计数
//!
//! 与 tokio::Semaphore 配合:Semaphore 作为硬上限(防 OOM),Controller 作为软目标
//! (动态调优)。spawn 前先检查 `should_spawn()`,再 acquire permit。

use std::sync::atomic::{AtomicU32, Ordering};

/// 可升降的并发控制器
///
/// 线程安全:内部用 AtomicU32,无锁。
/// active/target 均为 u32(并发度不会超过 u32 范围)。
pub struct ConcurrencyController {
    /// 当前在途 task 数
    active: AtomicU32,
    /// 目标并发度(软上限)
    target: AtomicU32,
    /// 硬上限(Semaphore 容量,防 OOM)
    hard_limit: u32,
}

impl ConcurrencyController {
    /// 创建控制器
    ///
    /// # 参数
    /// - `initial`: 初始目标并发度(将 clamp 到 [1, hard_limit],与 `set_target` 语义一致)
    /// - `hard_limit`: 硬上限(不可超过,通常 = max_concurrent_fragments)
    ///
    /// # 不变式
    /// - `hard_limit` 必须 >= 1(release 下也用 `assert!` 检查,hard_limit=0 会导致
    ///   Semaphore::new(0) 无 permit、`set_target` 的 clamp(1, 0) panic 或 target=0 卡死,
    ///   把静默卡死转为启动期可定位的 panic)
    /// - `initial` clamp 到 [1, hard_limit]:保证 target 永不为 0(否则 `should_spawn`
    ///   永远 false → 不 spawn 任何分片 → 下载永久卡死且无错误信号)
    pub fn new(initial: u32, hard_limit: u32) -> Self {
        // release 下也检查:hard_limit=0 无法工作(Semaphore 无 permit、clamp panic、target=0 卡死)
        assert!(hard_limit >= 1, "硬上限 hard_limit 必须 >= 1");
        // 与 set_target 的 clamp 语义对齐,保证 target ∈ [1, hard_limit],永不为 0
        let clamped_initial = initial.clamp(1, hard_limit);
        Self {
            active: AtomicU32::new(0),
            target: AtomicU32::new(clamped_initial),
            hard_limit,
        }
    }

    /// 当前在途 task 数
    pub fn active(&self) -> u32 {
        self.active.load(Ordering::Acquire)
    }

    /// 当前目标并发度
    pub fn target(&self) -> u32 {
        self.target.load(Ordering::Acquire)
    }

    /// 硬上限
    pub fn hard_limit(&self) -> u32 {
        self.hard_limit
    }

    /// 是否应该 spawn 新 task(active < target)
    pub fn should_spawn(&self) -> bool {
        self.active() < self.target()
    }

    /// 设置新的目标并发度(可升可降)
    ///
    /// 自动 clamp 到 [1, hard_limit]。
    /// 已有在途 task 不受影响(不会中断),仅影响后续 spawn 决策。
    pub fn set_target(&self, new_target: u32) {
        let clamped = new_target.clamp(1, self.hard_limit);
        self.target.store(clamped, Ordering::Release);
    }

    /// 记录一个 task 已 spawn(active +1)
    ///
    /// 调用方应在 `should_spawn()` 返回 true 后、实际 spawn 前调用。
    pub fn record_spawn(&self) {
        self.active.fetch_add(1, Ordering::AcqRel);
    }

    /// 记录一个 task 已完成(active -1)
    ///
    /// 不会低于 0(saturating)。
    pub fn record_complete(&self) {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                Some(v.saturating_sub(1))
            })
            .ok();
    }

    /// 强制把 active 置 0。
    ///
    /// 用于用户暂停后 `JoinSet::abort_all`:被 abort 的 task 可能来不及
    /// `record_complete`,导致 active 卡死、`should_spawn` 永远 false。
    pub fn reset_active(&self) {
        self.active.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let ctrl = ConcurrencyController::new(4, 16);
        assert_eq!(ctrl.active(), 0);
        assert_eq!(ctrl.target(), 4);
        assert_eq!(ctrl.hard_limit(), 16);
        assert!(ctrl.should_spawn(), "初始 active=0 < target=4 应可 spawn");
    }

    #[test]
    fn test_spawn_increments_active() {
        let ctrl = ConcurrencyController::new(4, 16);
        assert!(ctrl.should_spawn());
        ctrl.record_spawn();
        assert_eq!(ctrl.active(), 1);
        assert!(ctrl.should_spawn(), "active=1 < target=4 仍可 spawn");
    }

    #[test]
    fn test_spawn_blocked_when_active_reaches_target() {
        let ctrl = ConcurrencyController::new(2, 16);
        ctrl.record_spawn();
        ctrl.record_spawn();
        assert_eq!(ctrl.active(), 2);
        assert!(!ctrl.should_spawn(), "active=2 >= target=2 应阻止 spawn");
    }

    #[test]
    fn test_reset_active_clears_inflight_count() {
        let ctrl = ConcurrencyController::new(2, 16);
        ctrl.record_spawn();
        ctrl.record_spawn();
        assert!(!ctrl.should_spawn());
        ctrl.reset_active();
        assert_eq!(ctrl.active(), 0);
        assert!(ctrl.should_spawn(), "reset_active 后应可重新 spawn");
    }

    #[test]
    fn test_complete_decrements_active() {
        let ctrl = ConcurrencyController::new(2, 16);
        ctrl.record_spawn();
        ctrl.record_spawn();
        assert!(!ctrl.should_spawn());
        ctrl.record_complete();
        assert_eq!(ctrl.active(), 1);
        assert!(
            ctrl.should_spawn(),
            "完成一个后 active=1 < target=2 应可 spawn"
        );
    }

    #[test]
    fn test_set_target_up() {
        let ctrl = ConcurrencyController::new(2, 16);
        ctrl.record_spawn();
        ctrl.record_spawn();
        assert!(!ctrl.should_spawn());
        // 提升目标
        ctrl.set_target(4);
        assert_eq!(ctrl.target(), 4);
        assert!(ctrl.should_spawn(), "提升 target=4 后 active=2 应可 spawn");
    }

    #[test]
    fn test_set_target_down() {
        let ctrl = ConcurrencyController::new(4, 16);
        ctrl.record_spawn();
        ctrl.record_spawn();
        // 降低目标(不中断已有 task)
        ctrl.set_target(1);
        assert_eq!(ctrl.target(), 1);
        assert!(
            !ctrl.should_spawn(),
            "降低 target=1 后 active=2 应阻止 spawn"
        );
        // 完成一个后仍不应 spawn(active=1 >= target=1)
        ctrl.record_complete();
        assert!(!ctrl.should_spawn());
        // 再完成一个后 active=0 < target=1 应可 spawn
        ctrl.record_complete();
        assert!(ctrl.should_spawn());
    }

    #[test]
    fn test_set_target_clamps_to_hard_limit() {
        let ctrl = ConcurrencyController::new(4, 16);
        ctrl.set_target(100);
        assert_eq!(ctrl.target(), 16, "target 不应超过 hard_limit");
    }

    #[test]
    fn test_set_target_clamps_to_min_1() {
        let ctrl = ConcurrencyController::new(4, 16);
        ctrl.set_target(0);
        assert_eq!(ctrl.target(), 1, "target 不应低于 1");
    }

    #[test]
    fn test_complete_does_not_go_negative() {
        let ctrl = ConcurrencyController::new(4, 16);
        ctrl.record_complete();
        ctrl.record_complete();
        assert_eq!(ctrl.active(), 0, "active 不应低于 0");
    }

    #[test]
    fn test_concurrent_spawn_and_complete() {
        use std::sync::Arc;
        use std::thread;
        let ctrl = Arc::new(ConcurrencyController::new(8, 32));
        let mut handles = Vec::new();
        // 8 线程并发 spawn + complete
        for _ in 0..8 {
            let c = Arc::clone(&ctrl);
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    if c.should_spawn() {
                        c.record_spawn();
                        c.record_complete();
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // 所有线程完成后 active 应回到 0
        assert_eq!(ctrl.active(), 0, "并发 spawn+complete 后 active 应为 0");
    }

    #[test]
    fn test_down_then_up_cycle() {
        let ctrl = ConcurrencyController::new(4, 16);
        // 模拟带宽波动:升到 8,降到 2,再升到 6
        ctrl.set_target(8);
        assert_eq!(ctrl.target(), 8);
        ctrl.record_spawn();
        ctrl.record_spawn();
        ctrl.record_spawn();
        ctrl.record_spawn();
        assert_eq!(ctrl.active(), 4);
        assert!(ctrl.should_spawn(), "active=4 < target=8 应可 spawn");

        ctrl.set_target(2);
        assert!(!ctrl.should_spawn(), "active=4 >= target=2 应阻止 spawn");
        // 完成 3 个,active=1 < target=2
        ctrl.record_complete();
        ctrl.record_complete();
        ctrl.record_complete();
        assert_eq!(ctrl.active(), 1);
        assert!(ctrl.should_spawn());

        ctrl.set_target(6);
        assert!(ctrl.should_spawn(), "active=1 < target=6 应可 spawn");
    }

    /// FIX-05 回归:动态并发上调不被固定信号量容量阻断。
    ///
    /// 旧实现 Semaphore 以初始建议值(可为 1)构造,即便 set_target(4) 后 should_spawn()
    /// 放行,新任务仍 acquire 不到 permit,在途永不超过 1。修复后 Semaphore 以 hard_limit
    /// (= max_concurrent_fragments) 构造,should_spawn() 作为唯一软目标门禁。
    ///
    /// 本测试验证控制器语义:初始 target=1,hard_limit=4;set_target(4) 后应允许 4 个
    /// 并发 spawn(active 0→4 全部 should_spawn=true),且不触达硬上限。
    #[test]
    fn test_fix05_scale_up_not_blocked_by_initial_target() {
        // 模拟初始建议=1,配置硬上限=4
        let ctrl = ConcurrencyController::new(1, 4);
        assert_eq!(ctrl.target(), 1);
        assert!(ctrl.should_spawn(), "active=0 < target=1 可 spawn");
        // 初始 target=1 下只能 spawn 1 个
        ctrl.record_spawn();
        assert_eq!(ctrl.active(), 1);
        assert!(!ctrl.should_spawn(), "active=1 >= target=1 应阻止 spawn");

        // 调度器重采样后建议上调到 4
        ctrl.set_target(4);
        assert_eq!(ctrl.target(), 4);
        // 现在应能继续 spawn 到 4 个(active 1→4 全部放行)
        assert!(ctrl.should_spawn(), "active=1 < target=4 应可 spawn");
        ctrl.record_spawn();
        ctrl.record_spawn();
        ctrl.record_spawn();
        assert_eq!(ctrl.active(), 4);
        assert!(!ctrl.should_spawn(), "active=4 >= target=4 应阻止 spawn");
        // 未触达硬上限 4(若信号量以 hard_limit 构造,4 个 permit 恰好满足)
        assert_eq!(ctrl.hard_limit(), 4);
    }

    /// RED-TDD:`new` 必须把 initial=0 clamp 到 1,保证 target 永不为 0。
    ///
    /// 背景:release 模式下旧 `debug_assert!(initial > 0)` 被移除,若调用方传入
    /// initial=0(如 bt_cold_start 配置覆盖路径未做 .max(1)),target 直接存 0,
    /// `should_spawn()` 永远返回 false → 不 spawn 任何分片 → 下载永久卡死且无错误
    /// 信号。修复:与 `set_target` 一致在 `new` 内 clamp 到 [1, hard_limit]。
    #[test]
    fn test_new_clamps_initial_zero_to_one() {
        // initial=0 不应产生 target=0 的卡死状态
        let ctrl = ConcurrencyController::new(0, 10);
        assert_eq!(
            ctrl.target(),
            1,
            "initial=0 必须 clamp 到 1,target=0 会导致 should_spawn 永远 false → 卡死"
        );
        assert_eq!(ctrl.hard_limit(), 10);
        assert!(
            ctrl.should_spawn(),
            "active=0 < target=1 应可 spawn(target=0 会让此条件永远 false)"
        );
    }

    /// RED-TDD:`new` 当 initial > hard_limit 时应 clamp 到 hard_limit(与 set_target 对齐)。
    ///
    /// 旧实现 release 下不 clamp,target 直接存入越界值(initial > hard_limit),
    /// 与 `set_target` 的 [1, hard_limit] clamp 语义不一致,且 hard_limit 失去上限意义。
    #[test]
    fn test_new_clamps_initial_above_hard_limit() {
        let ctrl = ConcurrencyController::new(20, 10);
        assert_eq!(
            ctrl.target(),
            10,
            "initial=20 > hard_limit=10 应 clamp 到 10,target 不应超过硬上限"
        );
        assert_eq!(ctrl.hard_limit(), 10);
    }

    /// RED-TDD:`new` 拒绝 hard_limit=0(并发度硬上限为 0 无意义)。
    ///
    /// hard_limit=0 时:Semaphore::new(0) 无 permit、`set_target` 的 clamp(1, 0) 因
    /// min > max 而 panic、或 target=0 卡死。三种路径均无法正常工作,故在构造时
    /// 用 `assert!`(release 也检查)显式 panic,把静默卡死转化为可定位的启动期错误。
    #[test]
    #[should_panic(expected = "硬上限 hard_limit 必须 >= 1")]
    fn test_new_rejects_zero_hard_limit() {
        let _ = ConcurrencyController::new(1, 0);
    }
}
