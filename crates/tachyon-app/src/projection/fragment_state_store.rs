//! 分片状态存储
//!
//! 维护每个任务的真实分片总数、已完成分片索引集合和正在下载分片索引集合,
//! 供 get_task_fragments command 查询和 ChunkMatrix 渲染使用。
//! 由 PlanComplete 事件初始化,Started 事件标记 downloading,
//! Chunk::completed 事件标记 done,任务终态时由 cleanup_runtime 移除。

use std::collections::BTreeSet;
use std::sync::Arc;

use dashmap::DashMap;

/// 单个任务的分片运行时状态(内存,随任务生命周期)
pub struct TaskFragmentState {
    /// 真实分片总数(来自 plan_fragments,非 probe 估算)
    pub total: u32,
    /// 已完成分片索引集合
    pub done_set: BTreeSet<u32>,
    /// 正在下载的分片索引集合(Started 事件插入,Chunk{completed:true} 移除)
    pub downloading_set: BTreeSet<u32>,
}

impl TaskFragmentState {
    /// 从 PlanComplete 事件构造
    pub fn from_plan(total: u32, completed_indices: Vec<u32>) -> Self {
        Self {
            total,
            done_set: completed_indices.into_iter().collect(),
            downloading_set: BTreeSet::new(),
        }
    }

    /// 标记分片开始下载(Started 事件触发)
    ///
    /// 若分片已在 done_set 中则跳过(防御终态竞态:已完成分片不应回退到 downloading)。
    /// 幂等:重复调用不产生副作用,重试场景的重复 Started 事件安全吸收。
    pub fn mark_downloading(&mut self, index: u32) {
        if self.done_set.contains(&index) {
            return;
        }
        self.downloading_set.insert(index);
    }

    /// 标记分片完成(Chunk::completed 事件触发)
    ///
    /// 先从 downloading_set 移除再加入 done_set,保证同一分片不会同时存在于两个集合。
    pub fn mark_done(&mut self, index: u32) {
        self.downloading_set.remove(&index);
        self.done_set.insert(index);
    }
}

/// 全局分片状态存储,长存于 AppState
///
/// key = task_id, value = TaskFragmentState。
/// 任务进入 downloading 时由 PlanComplete 初始化,
/// 任务终态时由 cleanup_runtime 移除。
#[derive(Clone, Default)]
pub struct FragmentStateStore(Arc<DashMap<String, TaskFragmentState>>);

impl FragmentStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 初始化任务分片状态(PlanComplete 事件触发)
    /// 若已存在则覆盖(防止重试场景残留旧状态)
    pub fn init(&self, task_id: &str, state: TaskFragmentState) {
        self.0.insert(task_id.to_string(), state);
    }

    /// 标记分片开始下载(Started 事件触发)
    pub fn mark_downloading(&self, task_id: &str, index: u32) {
        if let Some(mut state) = self.0.get_mut(task_id) {
            state.mark_downloading(index);
        }
    }

    /// 标记分片完成(Chunk::completed 事件触发)
    pub fn mark_done(&self, task_id: &str, index: u32) {
        if let Some(mut state) = self.0.get_mut(task_id) {
            state.mark_done(index);
        }
    }

    /// 查询任务分片状态(get_task_fragments command 调用)
    pub fn get(
        &self,
        task_id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, TaskFragmentState>> {
        self.0.get(task_id)
    }

    /// 移除任务分片状态(cleanup_runtime 调用)
    pub fn remove(&self, task_id: &str) {
        self.0.remove(task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_plan_empty() {
        let state = TaskFragmentState::from_plan(16, vec![]);
        assert_eq!(state.total, 16);
        assert!(state.done_set.is_empty());
    }

    #[test]
    fn test_from_plan_with_completed() {
        let state = TaskFragmentState::from_plan(16, vec![0, 1, 2]);
        assert_eq!(state.total, 16);
        assert_eq!(state.done_set.len(), 3);
        assert!(state.done_set.contains(&1));
    }

    #[test]
    fn test_mark_done() {
        let mut state = TaskFragmentState::from_plan(16, vec![]);
        state.mark_done(5);
        assert!(state.done_set.contains(&5));
        // 幂等:重复 mark_done 不增加
        state.mark_done(5);
        assert_eq!(state.done_set.len(), 1);
    }

    #[test]
    fn test_mark_downloading() {
        let mut state = TaskFragmentState::from_plan(16, vec![]);
        state.mark_downloading(3);
        assert!(state.downloading_set.contains(&3));
        // 幂等:重复 mark_downloading 不增加
        state.mark_downloading(3);
        assert_eq!(state.downloading_set.len(), 1);
    }

    #[test]
    fn test_mark_done_clears_downloading() {
        // mark_done 必须从 downloading_set 移除,保证同一分片不同时存在于两个集合
        let mut state = TaskFragmentState::from_plan(16, vec![]);
        state.mark_downloading(5);
        assert!(state.downloading_set.contains(&5));
        state.mark_done(5);
        assert!(state.done_set.contains(&5));
        assert!(
            !state.downloading_set.contains(&5),
            "mark_done 应清除 downloading_set"
        );
    }

    #[test]
    fn test_mark_downloading_skips_done() {
        // 已完成的分片不应回退到 downloading(防御终态竞态)
        let mut state = TaskFragmentState::from_plan(16, vec![]);
        state.mark_done(7);
        state.mark_downloading(7);
        assert!(
            !state.downloading_set.contains(&7),
            "已完成分片不应进入 downloading_set"
        );
    }

    #[test]
    fn test_store_init_and_get() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![0]));
        let state = store.get("task1").expect("应存在");
        assert_eq!(state.total, 8);
        assert_eq!(state.done_set.len(), 1);
    }

    #[test]
    fn test_store_mark_done() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![]));
        store.mark_done("task1", 3);
        let state = store.get("task1").expect("应存在");
        assert!(state.done_set.contains(&3));
    }

    #[test]
    fn test_store_mark_downloading() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![]));
        store.mark_downloading("task1", 2);
        let state = store.get("task1").expect("应存在");
        assert!(state.downloading_set.contains(&2));
    }

    #[test]
    fn test_store_mark_downloading_nonexistent_task() {
        // 对不存在的 task_id 调用 mark_downloading 不应 panic
        let store = FragmentStateStore::new();
        store.mark_downloading("nonexistent", 0);
    }

    #[test]
    fn test_store_remove() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![]));
        store.remove("task1");
        assert!(store.get("task1").is_none());
    }

    #[test]
    fn test_store_overwrite_on_reinit() {
        let store = FragmentStateStore::new();
        store.init("task1", TaskFragmentState::from_plan(8, vec![0, 1]));
        // 先写入 downloading 状态,验证 reinit 后清空
        store.mark_downloading("task1", 5);
        // 覆盖(重试场景)
        store.init("task1", TaskFragmentState::from_plan(16, vec![]));
        let state = store.get("task1").expect("应存在");
        assert_eq!(state.total, 16);
        assert!(state.done_set.is_empty());
        assert!(
            state.downloading_set.is_empty(),
            "reinit 应清空 downloading_set"
        );
    }
}
