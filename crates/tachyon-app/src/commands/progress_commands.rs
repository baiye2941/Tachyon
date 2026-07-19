use super::{AppError, AppState, DownloadProgress, ProgressEvent, TaskProgress};
use std::collections::HashMap;

use tokio::sync::broadcast;

use crate::service::try_claim_subscription;

// ---------------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_download_progress(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<DownloadProgress, AppError> {
    get_download_progress_inner(&state, task_id).await
}

#[tauri::command]
pub async fn subscribe_progress(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    use tauri::Emitter;

    // 幂等去重:仅首次调用 spawn broker 任务,防止反复调用累积后台任务
    if !try_claim_subscription(&state.runtime.progress_subscribed) {
        tracing::debug!("进度订阅已存在,跳过重复订阅(幂等)");
        return Ok(());
    }

    let mut rx = state.runtime.progress_broker.subscribe();
    let task_repository = state.domain.task_repository.clone();
    let fragment_state_store = state.fragment_state_store.clone();

    tokio::spawn(async move {
        // 首次广播全量快照，保证前端初始状态正确
        let mut last_snapshot: ProgressEvent = build_initial_progress_event(&task_repository);
        let _ = app_handle.emit("progress-update", &last_snapshot);

        // 审计 M-03:broadcast 顺序投递,不因 watch 覆盖丢失 completed/started delta
        loop {
            match rx.recv().await {
                Ok(snapshot) => {
                    let delta = compute_progress_delta(&snapshot, &last_snapshot);
                    if !delta.is_empty() {
                        for tp in delta.values() {
                            if tp.downloaded > 0 || tp.speed > 0 {
                                tracing::info!(
                                    tid = tp.id,
                                    downloaded = tp.downloaded,
                                    speed = tp.speed,
                                    "emit progress-update"
                                );
                            }
                        }
                        let _ = app_handle.emit("progress-update", &delta);
                        last_snapshot = snapshot;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        skipped = n,
                        "进度 broadcast 订阅滞后,合成权威 resync 后继续接收"
                    );
                    // Lagged 后中间 delta 已丢；用 task_repository + fragment_state_store
                    // 合成权威全量 resync，保证前端分片集合最终一致。
                    let snap = build_lagged_resync_event(&task_repository, &fragment_state_store);
                    if !snap.is_empty() {
                        let _ = app_handle.emit("progress-update", &snap);
                        last_snapshot = snap;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Inner implementations
// ---------------------------------------------------------------------------

async fn get_download_progress_inner(
    state: &AppState,
    task_id: String,
) -> Result<DownloadProgress, AppError> {
    let task = state
        .domain
        .task_repository
        .get(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    Ok(DownloadProgress {
        task_id: task.id.clone(),
        status: task.status,
        progress: task.progress,
        downloaded: task.downloaded,
        file_size: task.file_size,
        speed: task.speed,
        fragments_total: task.fragments_total,
        fragments_done: task.fragments_done,
        active_concurrency: task.active_concurrency,
    })
}

/// 根据当前任务列表构建首次订阅时的全量进度快照。
fn build_initial_progress_event(
    task_repository: &crate::repository::TaskRepository,
) -> ProgressEvent {
    task_repository
        .iter()
        .map(|r| {
            let id = r.key();
            let t = r.value();
            (
                id.clone(),
                TaskProgress {
                    id: id.clone(),
                    progress: t.progress,
                    speed: t.speed,
                    downloaded: t.downloaded,
                    status: t.status,
                    fragments_done: t.fragments_done,
                    fragments_total: t.fragments_total,
                    active_concurrency: t.active_concurrency,
                    file_size: t.file_size,
                    completed_delta: vec![],
                    started_delta: vec![],
                    // 与 broker 路径(build_progress_event)对齐带真实值:
                    // 否则 G 修复(wire 三态)后初始快照会以 null 清掉前端已展示的错误文案,
                    // 启动时 Failed 任务的错误提示会闪一下才被 delta 修正
                    error_reason: t.error_reason.clone(),
                    fragment_bytes: vec![],
                },
            )
        })
        .collect()
}

/// 将新快照与上次广播值比较，只返回真正发生变化的任务。
fn compute_progress_delta(
    new: &ProgressEvent,
    last: &HashMap<String, TaskProgress>,
) -> ProgressEvent {
    new.iter()
        .filter_map(|(id, tp)| {
            if last.get(id) != Some(tp) {
                Some((id.clone(), tp.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Lagged 恢复：用 task_repository + fragment_state_store 合成权威全量 resync。
///
/// 语义：
/// - 标量字段来自 TaskInfo（对齐 `build_initial_progress_event`）
/// - 有 frag state 时：`completed_delta` = done_set 全量有序，
///   `started_delta` = downloading_set 全量有序
/// - 无 frag state 时：delta 为空，仅推送任务标量
/// - `fragment_bytes` 本路径可空（权威字节快照仍由后续正常 tick 补齐）
pub(crate) fn build_lagged_resync_event(
    task_repository: &crate::repository::TaskRepository,
    fragment_state_store: &crate::projection::FragmentStateStore,
) -> ProgressEvent {
    task_repository
        .iter()
        .map(|r| {
            let id = r.key();
            let t = r.value();
            let (completed_delta, started_delta) = if let Some(frag) = fragment_state_store.get(id)
            {
                (
                    frag.done_set.iter().copied().collect(),
                    frag.downloading_set.iter().copied().collect(),
                )
            } else {
                (vec![], vec![])
            };
            (
                id.clone(),
                TaskProgress {
                    id: id.clone(),
                    progress: t.progress,
                    speed: t.speed,
                    downloaded: t.downloaded,
                    status: t.status,
                    fragments_done: t.fragments_done,
                    fragments_total: t.fragments_total,
                    active_concurrency: t.active_concurrency,
                    file_size: t.file_size,
                    completed_delta,
                    started_delta,
                    error_reason: t.error_reason.clone(),
                    fragment_bytes: vec![],
                },
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::TaskInfo;
    use super::super::task_commands::create_task_inner;
    use super::super::tests::test_state;
    use super::*;
    use crate::projection::{FragmentStateStore, TaskFragmentState};
    use crate::repository::TaskRepository;
    use crate::service::try_claim_subscription;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tachyon_core::types::DownloadState;

    fn make_downloading_task(
        id: &str,
        downloaded: u64,
        fragments_done: u32,
        fragments_total: u32,
    ) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            url: format!("https://example.com/{id}.bin"),
            file_name: format!("{id}.bin"),
            file_size: Some(1000),
            downloaded,
            speed: 0,
            status: DownloadState::Downloading,
            progress: if fragments_total > 0 {
                f64::from(fragments_done) / f64::from(fragments_total)
            } else {
                0.0
            },
            fragments_total,
            fragments_done,
            active_concurrency: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            save_path: "/tmp".to_string(),
            error_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        }
    }

    #[test]
    fn test_try_claim_subscription_first_call_returns_true() {
        // 首次声明:flag 从 false 切换到 true,返回 true(调用方应执行订阅)
        let flag = AtomicBool::new(false);
        assert!(try_claim_subscription(&flag));
        assert!(flag.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn test_try_claim_subscription_second_call_returns_false() {
        // 第二次声明:flag 已是 true,返回 false(调用方应跳过,幂等)
        let flag = AtomicBool::new(true);
        assert!(!try_claim_subscription(&flag));
        assert!(flag.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn test_try_claim_subscription_idempotent_under_concurrency() {
        // 模拟并发:多个线程同时声明,只有恰好一个返回 true
        let flag = Arc::new(AtomicBool::new(false));
        let claimed: Vec<_> = (0..16)
            .map(|_| {
                let flag = flag.clone();
                std::thread::spawn(move || try_claim_subscription(&flag))
            })
            .collect();
        let results: Vec<bool> = claimed.into_iter().map(|h| h.join().unwrap()).collect();
        let success_count = results.iter().filter(|&&r| r).count();
        assert_eq!(success_count, 1, "并发声明应恰好只有一个成功");
        assert!(flag.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn test_get_download_progress() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/progress.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let progress = get_download_progress_inner(&state, id.clone())
            .await
            .unwrap();
        assert_eq!(progress.task_id, id);
        assert_eq!(progress.status, DownloadState::Pending);
        assert!((progress.progress - 0.0).abs() < f64::EPSILON);
        assert_eq!(progress.downloaded, 0);
        assert_eq!(progress.speed, 0);
        assert_eq!(progress.fragments_total, 0);
        assert_eq!(progress.fragments_done, 0);
    }

    #[tokio::test]
    async fn test_get_download_progress_not_found() {
        let state = test_state();
        let result = get_download_progress_inner(&state, "nonexistent".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("任务不存在"));
    }

    #[test]
    fn test_download_progress_serialization() {
        let progress = DownloadProgress {
            task_id: "test-id".to_string(),
            status: DownloadState::Downloading,
            progress: 0.5,
            downloaded: 512,
            file_size: Some(1024),
            speed: 100,
            fragments_total: 4,
            fragments_done: 2,
            active_concurrency: 0,
        };
        let json = serde_json::to_string(&progress).unwrap();
        assert!(json.contains("taskId"));
        assert!(json.contains("fileSize"));
        assert!(json.contains("fragmentsTotal"));
    }

    #[tokio::test]
    async fn test_subscribe_progress_initial_full_snapshot() {
        let state = test_state();
        let id1 = create_task_inner(
            &state,
            "https://example.com/1.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let id2 = create_task_inner(
            &state,
            "https://example.com/2.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();

        let event = build_initial_progress_event(&state.domain.task_repository);
        assert_eq!(event.len(), 2);
        assert!(event.contains_key(&id1));
        assert!(event.contains_key(&id2));
    }

    #[test]
    fn test_compute_progress_delta_no_changes() {
        let tp = TaskProgress {
            id: "t1".to_string(),
            progress: 0.5,
            speed: 100,
            downloaded: 512,
            status: DownloadState::Downloading,
            fragments_done: 2,
            fragments_total: 0,
            active_concurrency: 0,
            file_size: None,
            completed_delta: vec![],
            started_delta: vec![],
            error_reason: None,
            fragment_bytes: vec![],
        };
        let mut last = HashMap::new();
        last.insert("t1".to_string(), tp.clone());

        let new = last.clone();
        let delta = compute_progress_delta(&new, &last);
        assert!(delta.is_empty());
    }

    #[test]
    fn test_compute_progress_delta_only_changed_tasks() {
        let mut last = HashMap::new();
        last.insert(
            "t1".to_string(),
            TaskProgress {
                id: "t1".to_string(),
                progress: 0.1,
                speed: 100,
                downloaded: 100,
                status: DownloadState::Downloading,
                fragments_done: 1,
                fragments_total: 0,
                active_concurrency: 0,
                file_size: None,
                completed_delta: vec![],
                started_delta: vec![],
                error_reason: None,
                fragment_bytes: vec![],
            },
        );
        last.insert(
            "t2".to_string(),
            TaskProgress {
                id: "t2".to_string(),
                progress: 0.2,
                speed: 200,
                downloaded: 200,
                status: DownloadState::Downloading,
                fragments_done: 2,
                fragments_total: 0,
                active_concurrency: 0,
                file_size: None,
                completed_delta: vec![],
                started_delta: vec![],
                error_reason: None,
                fragment_bytes: vec![],
            },
        );

        let mut new = last.clone();
        new.get_mut("t1").unwrap().progress = 0.5;
        new.get_mut("t1").unwrap().speed = 150;

        let delta = compute_progress_delta(&new, &last);
        assert_eq!(delta.len(), 1);
        assert!(delta.contains_key("t1"));
        assert!(!delta.contains_key("t2"));

        let changed = delta.get("t1").unwrap();
        assert!((changed.progress - 0.5).abs() < f64::EPSILON);
        assert_eq!(changed.speed, 150);
        assert_eq!(changed.downloaded, 100);
    }

    #[test]
    fn test_compute_progress_delta_new_task_appears() {
        let last: HashMap<String, TaskProgress> = HashMap::new();
        let mut new = ProgressEvent::new();
        new.insert(
            "t1".to_string(),
            TaskProgress {
                id: "t1".to_string(),
                progress: 0.0,
                speed: 0,
                downloaded: 0,
                status: DownloadState::Pending,
                fragments_done: 0,
                fragments_total: 0,
                active_concurrency: 0,
                file_size: None,
                completed_delta: vec![],
                started_delta: vec![],
                error_reason: None,
                fragment_bytes: vec![],
            },
        );

        let delta = compute_progress_delta(&new, &last);
        assert_eq!(delta.len(), 1);
        assert!(delta.contains_key("t1"));
    }

    /// 回归测试:file_size 从 None 变 Some 时必须触发 delta 推送。
    ///
    /// 场景:任务创建时 file_size 未知(None),后端探测完成后写入 Some(size)。
    /// 若 TaskProgress 不含 file_size 字段或 delta 比较忽略该字段,
    /// 前端详情页会一直显示 0B,直到用户手动刷新任务列表。
    #[test]
    fn test_compute_progress_delta_file_size_change_triggers_delta() {
        let mut last = HashMap::new();
        last.insert(
            "t1".to_string(),
            TaskProgress {
                id: "t1".to_string(),
                progress: 0.0,
                speed: 0,
                downloaded: 0,
                status: DownloadState::Connecting,
                fragments_done: 0,
                fragments_total: 0,
                active_concurrency: 0,
                file_size: None,
                completed_delta: vec![],
                started_delta: vec![],
                error_reason: None,
                fragment_bytes: vec![],
            },
        );

        let mut new = last.clone();
        new.get_mut("t1").unwrap().file_size = Some(1024);

        let delta = compute_progress_delta(&new, &last);
        assert_eq!(delta.len(), 1, "file_size 变化应触发 delta 推送");
        let changed = delta.get("t1").unwrap();
        assert_eq!(changed.file_size, Some(1024));
    }

    /// 初始全量快照不应携带活跃分片字节快照(订阅前无下载活动)
    #[tokio::test]
    async fn test_build_initial_progress_event_has_empty_fragment_bytes() {
        let state = test_state();
        let _id = create_task_inner(
            &state,
            "https://example.com/init.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let event = build_initial_progress_event(&state.domain.task_repository);
        let tp = event.values().next().expect("应至少一个任务");
        assert!(tp.fragment_bytes.is_empty());
    }

    /// 初始全量快照必须携带真实 error_reason:订阅前已 Failed 的任务,
    /// 若快照硬编码 None,wire 三态(null=清除)会把前端已展示的错误文案清掉
    #[tokio::test]
    async fn test_build_initial_progress_event_carries_error_reason() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/failed.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        // 模拟订阅前已失败:直接在仓库中标记 Failed + 失败原因
        let mut task = state
            .domain
            .task_repository
            .get_mut(&id)
            .expect("任务应在仓库中");
        task.status = DownloadState::Failed;
        task.error_reason = Some("连接超时".to_string());
        drop(task);

        let event = build_initial_progress_event(&state.domain.task_repository);
        let tp = event.get(&id).expect("任务应在初始快照中");
        assert_eq!(tp.error_reason.as_deref(), Some("连接超时"));
    }

    /// Lagged 恢复必须把 fragment_state_store 的 done/downloading 全量灌入 delta。
    ///
    /// 场景: broadcast 订阅滞后丢弃了中间 Started/Chunk 事件后，
    /// 前端仅凭后续增量无法还原已完成/进行中的分片集合；
    /// resync 必须用权威全量 done_set / downloading_set 补齐。
    #[test]
    fn lagged_resync_includes_all_done_indices() {
        let repo = TaskRepository::new();
        repo.insert("t1".to_string(), make_downloading_task("t1", 500, 2, 4));

        let store = FragmentStateStore::new();
        store.init("t1", TaskFragmentState::from_plan(4, vec![0, 2]));
        store.mark_downloading("t1", 1);

        let event = build_lagged_resync_event(&repo, &store);
        let tp = event.get("t1").expect("resync 应包含 task t1");

        let mut completed = tp.completed_delta.clone();
        completed.sort_unstable();
        assert_eq!(
            completed,
            vec![0, 2],
            "completed_delta 应为 done_set 全量有序"
        );
        assert!(
            tp.started_delta.contains(&1),
            "started_delta 应包含 downloading 分片 1, got {:?}",
            tp.started_delta
        );
        assert_eq!(tp.downloaded, 500, "标量 downloaded 应来自 TaskInfo");
        assert_eq!(tp.fragments_done, 2, "标量 fragments_done 应来自 TaskInfo");
        assert_eq!(tp.fragments_total, 4);
        assert_eq!(tp.status, DownloadState::Downloading);
    }

    /// 有任务但无 fragment state 时: delta 为空，标量仍来自 TaskInfo。
    #[test]
    fn lagged_resync_empty_delta_when_no_frag_state() {
        let repo = TaskRepository::new();
        repo.insert("t1".to_string(), make_downloading_task("t1", 100, 0, 4));
        let store = FragmentStateStore::new();

        let event = build_lagged_resync_event(&repo, &store);
        let tp = event
            .get("t1")
            .expect("即使无 frag state, resync 也应包含 task 标量");

        assert!(
            tp.completed_delta.is_empty(),
            "无 frag state 时 completed_delta 应为空"
        );
        assert!(
            tp.started_delta.is_empty(),
            "无 frag state 时 started_delta 应为空"
        );
        assert_eq!(tp.downloaded, 100);
        assert_eq!(tp.fragments_done, 0);
        assert_eq!(tp.fragments_total, 4);
        assert_eq!(tp.status, DownloadState::Downloading);
    }
}
