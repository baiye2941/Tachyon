//! 分片查询命令

use super::{AppError, AppState};
use serde::Serialize;
use tachyon_core::types::DownloadState;

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
    Ok(get_task_fragments_inner(&state, &task_id))
}

/// get_task_fragments 核心逻辑(与 Tauri State 解耦,便于单测)
pub(crate) fn get_task_fragments_inner(state: &AppState, task_id: &str) -> TaskFragmentsView {
    // 主路径:运行时分片状态(活跃任务)
    if let Some(frag_state) = state.fragment_state_store.get(task_id) {
        return TaskFragmentsView {
            total: frag_state.total,
            done_indices: frag_state.done_set.iter().copied().collect(),
            downloading_indices: frag_state.downloading_set.iter().copied().collect(),
        };
    }
    synthesize_terminal_view(state, task_id)
}

/// 终态任务合成分片视图
///
/// 任务进终态后 cleanup_runtime 会移除 fragment_state_store 中的运行时状态,
/// 事后打开详情页时 store 查无。此时从任务仓库/持久化快照合成静态视图:
/// - Completed: 所有分片均已完成(done = 0..total)
/// - Failed/Cancelled: 从快照还原已完成分片;无快照则退回空视图(现状)
/// - 其他状态(活跃态但 store 缺失):空视图,与历史行为一致
fn synthesize_terminal_view(state: &AppState, task_id: &str) -> TaskFragmentsView {
    fn empty_view() -> TaskFragmentsView {
        TaskFragmentsView {
            total: 0,
            done_indices: vec![],
            downloading_indices: vec![],
        }
    }

    let Some(task) = state.domain.task_repository.get(task_id) else {
        return empty_view();
    };
    let status = task.status;
    let memory_total = task.fragments_total;
    // 提前释放 DashMap 读引用,避免跨后续 IO 持有
    drop(task);

    match status {
        DownloadState::Completed => {
            // 内存 fragments_total 缺失(如重启后未恢复该字段)时从快照兜底。
            // 注:当前前端对 fragmentsTotal===0 的任务不发本命令(DetailPanel 早退),
            // 该兜底是防御性路径,为将来的调用方/其他客户端保留
            let total = if memory_total > 0 {
                memory_total
            } else {
                load_snapshot_total(state, task_id)
            };
            TaskFragmentsView {
                total,
                done_indices: (0..total).collect(),
                downloading_indices: vec![],
            }
        }
        DownloadState::Failed | DownloadState::Cancelled => {
            // load_snapshot 为 read_to_string 同步 IO(无 fsync),阻塞极小,
            // 与 persist_task_snapshot 的既有取舍一致,保持同步调用。
            let Some(snapshot) = state.infra.task_store.load_snapshot(task_id).ok().flatten()
            else {
                return empty_view();
            };
            if snapshot.total_fragments == 0 {
                return empty_view();
            }
            // 防御:过滤越界索引,排序去重保证输出确定性
            let mut done_indices: Vec<u32> = snapshot
                .completed_fragments
                .into_iter()
                .filter(|&i| i < snapshot.total_fragments)
                .collect();
            done_indices.sort_unstable();
            done_indices.dedup();
            TaskFragmentsView {
                total: snapshot.total_fragments,
                done_indices,
                downloading_indices: vec![],
            }
        }
        _ => empty_view(),
    }
}

/// 从持久化快照读取分片总数,无快照时返回 0
fn load_snapshot_total(state: &AppState, task_id: &str) -> u32 {
    state
        .infra
        .task_store
        .load_snapshot(task_id)
        .ok()
        .flatten()
        .map(|s| s.total_fragments)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::TaskInfo;
    use crate::commands::tests::test_state;
    use crate::projection::{FragmentStateStore, TaskFragmentState};
    use crate::task_store::{TaskStore, task_info_to_snapshot};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tachyon_core::types::DownloadState;

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

    // ── BUG H:终态任务合成分片视图 ──────────────────────────────────────────
    //
    // 任务进终态后 cleanup_runtime 移除 fragment_state_store,事后打开详情页
    // store 查无。合成视图应从任务仓库/持久化快照还原分片矩阵。

    fn make_task(id: &str, status: DownloadState, fragments_total: u32) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            url: format!("https://example.com/{id}.bin"),
            file_name: format!("{id}.bin"),
            file_size: None,
            downloaded: 0,
            speed: 0,
            status,
            progress: 0.0,
            fragments_total,
            fragments_done: 0,
            active_concurrency: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            save_path: "/dl".to_string(),
            error_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        }
    }

    /// test_state() 的 TaskStore 挂在已 drop 的 TempDir 上,无法真实读写快照;
    /// 换绑到独立 TempDir 的 store,供快照还原用例使用。
    fn state_with_real_store() -> (Arc<AppState>, tempfile::TempDir) {
        let mut state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        Arc::get_mut(&mut state)
            .expect("test_state 返回的 Arc 应独占")
            .infra
            .task_store = store;
        (state, dir)
    }

    #[tokio::test]
    async fn test_completed_task_without_store_synthesizes_full_done_view() {
        let (state, _dir) = state_with_real_store();
        let mut task = make_task("t1", DownloadState::Completed, 8);
        task.fragments_done = 8;
        state.domain.task_repository.insert("t1".to_string(), task);
        // 不经 fragment_state_store(终态后已被 cleanup_runtime 移除)

        let view = get_task_fragments_inner(&state, "t1");
        assert_eq!(view.total, 8, "Completed 任务应合成分片总数");
        assert_eq!(
            view.done_indices,
            (0..8).collect::<Vec<u32>>(),
            "Completed 任务所有分片应为 done"
        );
        assert!(view.downloading_indices.is_empty());
    }

    #[tokio::test]
    async fn test_completed_task_falls_back_to_snapshot_total_when_memory_zero() {
        let (state, _dir) = state_with_real_store();
        let task = make_task("t1", DownloadState::Completed, 0);
        state
            .domain
            .task_repository
            .insert("t1".to_string(), task.clone());
        // 内存 fragments_total 缺失(0)时,从快照 total_fragments 兜底
        let mut snapshot = task_info_to_snapshot(
            &task,
            "/dl/t1.bin".into(),
            0,
            (0..4).collect(),
            HashMap::new(),
            None,
            None,
            true,
        );
        snapshot.total_fragments = 4;
        state.infra.task_store.save_snapshot(&snapshot).unwrap();

        let view = get_task_fragments_inner(&state, "t1");
        assert_eq!(view.total, 4);
        assert_eq!(view.done_indices, vec![0, 1, 2, 3]);
        assert!(view.downloading_indices.is_empty());
    }

    #[tokio::test]
    async fn test_failed_task_restores_done_indices_from_snapshot() {
        let (state, _dir) = state_with_real_store();
        let task = make_task("t1", DownloadState::Failed, 8);
        state
            .domain
            .task_repository
            .insert("t1".to_string(), task.clone());
        // 快照记录已完成分片 [5,0,2,1](乱序),合成视图应排序输出
        let snapshot = task_info_to_snapshot(
            &task,
            "/dl/t1.bin".into(),
            0,
            vec![5, 0, 2, 1],
            HashMap::new(),
            None,
            None,
            true,
        );
        state.infra.task_store.save_snapshot(&snapshot).unwrap();

        let view = get_task_fragments_inner(&state, "t1");
        assert_eq!(view.total, 8);
        assert_eq!(
            view.done_indices,
            vec![0, 1, 2, 5],
            "Failed 任务应从快照还原已完成分片"
        );
        assert!(view.downloading_indices.is_empty());
    }

    #[tokio::test]
    async fn test_cancelled_task_restores_done_indices_from_snapshot() {
        let (state, _dir) = state_with_real_store();
        let task = make_task("t1", DownloadState::Cancelled, 4);
        state
            .domain
            .task_repository
            .insert("t1".to_string(), task.clone());
        let snapshot = task_info_to_snapshot(
            &task,
            "/dl/t1.bin".into(),
            0,
            vec![0, 3],
            HashMap::new(),
            None,
            None,
            true,
        );
        state.infra.task_store.save_snapshot(&snapshot).unwrap();

        let view = get_task_fragments_inner(&state, "t1");
        assert_eq!(view.total, 4);
        assert_eq!(view.done_indices, vec![0, 3]);
        assert!(view.downloading_indices.is_empty());
    }

    #[tokio::test]
    async fn test_failed_task_without_snapshot_returns_empty_view() {
        let (state, _dir) = state_with_real_store();
        state
            .domain
            .task_repository
            .insert("t1".to_string(), make_task("t1", DownloadState::Failed, 8));
        // 无快照可还原:退回空视图(现状)
        let view = get_task_fragments_inner(&state, "t1");
        assert_eq!(view.total, 0);
        assert!(view.done_indices.is_empty());
        assert!(view.downloading_indices.is_empty());
    }

    #[tokio::test]
    async fn test_active_task_without_store_returns_empty_view() {
        // 活跃态(Downloading)但 store 缺失:保持现状,不合成
        let (state, _dir) = state_with_real_store();
        state.domain.task_repository.insert(
            "t1".to_string(),
            make_task("t1", DownloadState::Downloading, 8),
        );
        let view = get_task_fragments_inner(&state, "t1");
        assert_eq!(view.total, 0);
        assert!(view.done_indices.is_empty());
    }

    #[tokio::test]
    async fn test_store_view_takes_precedence_over_synthesized() {
        // store 存在时以 store 为准(既有行为不变),即使任务已是终态
        let (state, _dir) = state_with_real_store();
        state.domain.task_repository.insert(
            "t1".to_string(),
            make_task("t1", DownloadState::Completed, 8),
        );
        state
            .fragment_state_store
            .init("t1", TaskFragmentState::from_plan(8, vec![0, 1, 2]));

        let view = get_task_fragments_inner(&state, "t1");
        assert_eq!(view.total, 8);
        let mut done = view.done_indices.clone();
        done.sort_unstable();
        assert_eq!(done, vec![0, 1, 2], "store 的 done_set 优先于合成视图");
    }
}
