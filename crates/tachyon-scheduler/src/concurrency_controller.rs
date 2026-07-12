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
    /// - `initial`: 初始目标并发度
    /// - `hard_limit`: 硬上限(不可超过,通常 = max_concurrent_fragments)
    pub fn new(initial: u32, hard_limit: u32) -> Self {
        debug_assert!(initial > 0, "初始并发度必须 > 0");
        debug_assert!(hard_limit >= initial, "硬上限必须 >= 初始并发度");
        Self {
            active: AtomicU32::new(0),
            target: AtomicU32::new(initial),
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
}
