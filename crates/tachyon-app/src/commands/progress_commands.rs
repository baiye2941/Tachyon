use super::{AppError, AppState, DownloadProgress, ProgressEvent, TaskProgress};

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

    let mut rx = state.progress_tx.subscribe();
    let tasks = state.tasks.clone();

    tokio::spawn(async move {
        {
            let event: ProgressEvent = tasks
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
                        },
                    )
                })
                .collect();
            let _ = app_handle.emit("progress-update", &event);
        }

        while rx.changed().await.is_ok() {
            let snapshot = (*rx.borrow_and_update()).clone();
            for (tid, tp) in &snapshot {
                if tp.downloaded > 0 || tp.speed > 0 {
                    tracing::info!(
                        tid,
                        downloaded = tp.downloaded,
                        speed = tp.speed,
                        "emit progress-update"
                    );
                }
            }
            let _ = app_handle.emit("progress-update", &snapshot);
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
        .tasks
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
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::test_state;
    use super::super::task_commands::create_task_inner;
    use tachyon_core::types::DownloadState;

    #[tokio::test]
    async fn test_get_download_progress() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/progress.bin".to_string(),
            None,
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
        };
        let json = serde_json::to_string(&progress).unwrap();
        assert!(json.contains("taskId"));
        assert!(json.contains("fileSize"));
        assert!(json.contains("fragmentsTotal"));
    }
}
