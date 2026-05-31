use std::path::Path;

use amd_core::types::DownloadState;
use amd_store::{KvStore, RecoveryManager, TaskSnapshot};

use crate::{AppError, TaskInfo};

pub struct TaskStore {
    manager: RecoveryManager,
}

impl TaskStore {
    pub fn open(dir: &Path) -> Result<Self, AppError> {
        let kv =
            KvStore::open(dir).map_err(|e| AppError::Config(format!("打开任务存储失败: {e}")))?;
        Ok(Self {
            manager: RecoveryManager::new(kv),
        })
    }

    pub fn save_snapshot(&self, snapshot: &TaskSnapshot) -> Result<(), AppError> {
        self.manager
            .save_task_snapshot(snapshot)
            .map_err(|e| AppError::Config(format!("保存任务快照失败: {e}")))
    }

    pub fn load_snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, AppError> {
        self.manager
            .load_task_snapshot(task_id)
            .map_err(|e| AppError::Config(format!("加载任务快照失败: {e}")))
    }

    pub fn load_recoverable(&self) -> Result<Vec<TaskSnapshot>, AppError> {
        self.manager
            .recover_pending_snapshots()
            .map_err(|e| AppError::Config(format!("加载恢复任务失败: {e}")))
    }
}

pub fn snapshot_to_task_info(snapshot: &TaskSnapshot) -> TaskInfo {
    TaskInfo {
        id: snapshot.id.clone(),
        url: snapshot.url.clone(),
        file_name: snapshot.file_name.clone(),
        file_size: snapshot.file_size,
        downloaded: snapshot.downloaded,
        speed: 0,
        status: normalize_recovered_status(snapshot.status),
        progress: if snapshot.file_size.unwrap_or(0) == 0 {
            0.0
        } else {
            snapshot.downloaded as f64 / snapshot.file_size.unwrap_or(1) as f64
        },
        fragments_total: snapshot.total_fragments,
        fragments_done: snapshot.completed_fragments.len() as u32,
        created_at: snapshot.created_at.clone(),
    }
}

pub fn normalize_recovered_status(status: DownloadState) -> DownloadState {
    match status {
        DownloadState::Downloading | DownloadState::Verifying => DownloadState::Pending,
        other => other,
    }
}

pub fn task_info_to_snapshot(
    task: &TaskInfo,
    save_path: String,
    fragment_size: u64,
    completed_fragments: Vec<u32>,
    etag: Option<String>,
    last_modified: Option<String>,
) -> TaskSnapshot {
    let now = chrono::Local::now().to_rfc3339();
    TaskSnapshot {
        id: task.id.clone(),
        url: task.url.clone(),
        save_path,
        file_name: task.file_name.clone(),
        file_size: task.file_size,
        downloaded: task.downloaded,
        completed_fragments,
        total_fragments: task.fragments_total,
        fragment_size,
        status: task.status,
        etag,
        last_modified,
        content_length: task.file_size,
        created_at: task.created_at.clone(),
        updated_at: now,
        fail_reason: None,
        retry_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_to_task_info_preserves_status() {
        let snapshot = TaskSnapshot {
            id: "task-1".to_string(),
            url: "https://example.com/file.bin".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1000),
            downloaded: 250,
            completed_fragments: vec![0],
            total_fragments: 4,
            fragment_size: 250,
            status: DownloadState::Paused,
            etag: None,
            last_modified: None,
            content_length: Some(1000),
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 0,
        };

        let task = snapshot_to_task_info(&snapshot);
        assert_eq!(task.status, DownloadState::Paused);
        assert_eq!(task.progress, 0.25);
        assert_eq!(task.fragments_done, 1);
    }

    #[test]
    fn test_task_store_round_trip_recoverable_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let store = TaskStore::open(temp.path()).unwrap();
        let snapshot = TaskSnapshot {
            id: "task-1".to_string(),
            url: "https://example.com/file.bin".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1000),
            downloaded: 250,
            completed_fragments: vec![0],
            total_fragments: 4,
            fragment_size: 250,
            status: DownloadState::Paused,
            etag: None,
            last_modified: None,
            content_length: Some(1000),
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 0,
        };

        store.save_snapshot(&snapshot).unwrap();
        let loaded = store.load_recoverable().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "task-1");
    }

    #[test]
    fn test_downloading_recovers_as_pending() {
        assert_eq!(
            normalize_recovered_status(DownloadState::Downloading),
            DownloadState::Pending
        );
    }

    #[test]
    fn test_paused_recovers_as_paused() {
        assert_eq!(
            normalize_recovered_status(DownloadState::Paused),
            DownloadState::Paused
        );
    }

    #[test]
    fn test_task_info_to_snapshot_sets_content_length() {
        let task = TaskInfo {
            id: "task-1".to_string(),
            url: "https://example.com/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1024),
            downloaded: 0,
            speed: 0,
            status: DownloadState::Pending,
            progress: 0.0,
            fragments_total: 0,
            fragments_done: 0,
            created_at: "2026-05-29T00:00:00Z".to_string(),
        };

        let snapshot = task_info_to_snapshot(
            &task,
            "/downloads/file.bin".to_string(),
            256,
            vec![],
            Some("\"abc\"".to_string()),
            None,
        );

        assert_eq!(snapshot.content_length, Some(1024));
        assert_eq!(snapshot.etag.as_deref(), Some("\"abc\""));
        assert_eq!(snapshot.fragment_size, 256);
    }
}
