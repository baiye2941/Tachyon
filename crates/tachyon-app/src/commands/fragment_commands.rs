//! 分片查询命令

use super::{AppError, AppState};
use serde::Serialize;

/// get_task_fragments 返回视图
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFragmentsView {
    /// 真实分片总数(来自 plan_fragments,非 probe 估算)
    pub total: u32,
    /// 已完成分片索引列表
    pub done_indices: Vec<u32>,
    /// 正在下载的分片索引列表(首拉恢复用,与 done_indices 互斥)
    pub downloading_indices: Vec<u32>,
}

/// 查询任务分片状态(DetailPanel 打开时调用)
#[tauri::command]
pub async fn get_task_fragments(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<TaskFragmentsView, AppError> {
    let Some(frag_state) = state.fragment_state_store.get(&task_id) else {
        return Ok(TaskFragmentsView {
            total: 0,
            done_indices: vec![],
            downloading_indices: vec![],
        });
    };
    Ok(TaskFragmentsView {
        total: frag_state.total,
        done_indices: frag_state.done_set.iter().copied().collect(),
        downloading_indices: frag_state.downloading_set.iter().copied().collect(),
    })
}

#[cfg(test)]
mod tests {
    use crate::projection::{FragmentStateStore, TaskFragmentState};

    #[test]
    fn test_task_fragments_view_from_empty_store() {
        let store = FragmentStateStore::new();
        // 模拟 command 逻辑(无 AppState 时直接测 store)
        let result = store.get("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_task_fragments_view_from_initialized_store() {
        let store = FragmentStateStore::new();
        store.init("t1", TaskFragmentState::from_plan(8, vec![0, 1, 2]));
        let frag_state = store.get("t1").expect("应存在");
        assert_eq!(frag_state.total, 8);
        let done_indices: Vec<u32> = frag_state.done_set.iter().copied().collect();
        assert_eq!(done_indices, vec![0, 1, 2]);
    }

    #[test]
    fn test_task_fragments_view_with_downloading() {
        // 验证 downloading_indices 被正确填充且与 done_indices 互斥
        let store = FragmentStateStore::new();
        store.init("t1", TaskFragmentState::from_plan(8, vec![0, 1]));
        store.mark_downloading("t1", 3);
        store.mark_downloading("t1", 5);
        let frag_state = store.get("t1").expect("应存在");
        let done_indices: Vec<u32> = frag_state.done_set.iter().copied().collect();
        let downloading_indices: Vec<u32> = frag_state.downloading_set.iter().copied().collect();
        assert_eq!(done_indices, vec![0, 1]);
        assert_eq!(downloading_indices, vec![3, 5]);
        // 互斥验证:无交集
        let done_set: std::collections::HashSet<u32> = done_indices.iter().copied().collect();
        for idx in &downloading_indices {
            assert!(!done_set.contains(idx), "downloading 不应与 done 交集");
        }
    }
}
