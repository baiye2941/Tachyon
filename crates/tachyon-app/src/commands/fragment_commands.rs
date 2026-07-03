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
        });
    };
    Ok(TaskFragmentsView {
        total: frag_state.total,
        done_indices: frag_state.done_set.iter().copied().collect(),
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
}
