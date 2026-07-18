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
                    tracing::warn!(skipped = n, "进度 broadcast 订阅滞后,继续接收后续事件");
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
                    error_reason: None,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::task_commands::create_task_inner;
    use super::super::tests::test_state;
    use super::*;
    use crate::service::try_claim_subscription;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tachyon_core::types::DownloadState;

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
}
