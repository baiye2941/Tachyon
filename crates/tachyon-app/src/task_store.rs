use std::collections::HashMap;
use std::path::Path;

use tachyon_core::types::DownloadState;
use tachyon_store::{KvStore, RecoveryManager, TaskSnapshot};

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

    /// 撤销删除等显式恢复:清 tombstone 后写入
    pub fn restore_snapshot(&self, snapshot: &TaskSnapshot) -> Result<(), AppError> {
        self.manager
            .restore_task_snapshot(snapshot)
            .map_err(|e| AppError::Config(format!("恢复任务快照失败: {e}")))
    }

    pub fn load_snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, AppError> {
        self.manager
            .load_task_snapshot(task_id)
            .map_err(|e| AppError::Config(format!("加载任务快照失败: {e}")))
    }

    pub fn load_recoverable(&self) -> Result<Vec<TaskSnapshot>, AppError> {
        let (tasks, _corrupt) = self.load_recoverable_with_warnings()?;
        Ok(tasks)
    }

    /// 加载可恢复任务,同时返回无法解析的损坏 key 列表
    ///
    /// 与 `load_recoverable` 的区别在于暴露 corrupt_keys,
    /// 供调用方(如 Tauri setup 钩子)向 UI 广播恢复告警。
    /// 单个损坏 JSON 不会阻断其他任务的恢复。
    pub fn load_recoverable_with_warnings(
        &self,
    ) -> Result<(Vec<TaskSnapshot>, Vec<String>), AppError> {
        let result = self
            .manager
            .recover_pending_snapshots()
            .map_err(|e| AppError::Config(format!("加载恢复任务失败: {e}")))?;
        if !result.corrupt_keys.is_empty() {
            tracing::warn!(
                count = result.corrupt_keys.len(),
                keys = ?result.corrupt_keys,
                "部分任务快照损坏,已跳过"
            );
        }
        Ok((result.tasks, result.corrupt_keys))
    }

    /// 加载所有任务快照(含终态任务),用于备份导出
    ///
    /// 返回任务快照列表和损坏 key 列表;损坏记录不阻断正常记录的导出。
    pub fn load_all(&self) -> Result<(Vec<TaskSnapshot>, Vec<String>), AppError> {
        let result = self
            .manager
            .load_all_task_snapshots()
            .map_err(|e| AppError::Config(format!("加载所有任务快照失败: {e}")))?;
        if !result.corrupt_keys.is_empty() {
            tracing::warn!(
                count = result.corrupt_keys.len(),
                keys = ?result.corrupt_keys,
                "备份导出时发现损坏快照"
            );
        }
        Ok((result.tasks, result.corrupt_keys))
    }

    /// 删除任务快照(用于完成/取消/失败后的清理)
    ///
    /// 清理后恢复时不再扫描该任务,减少启动恢复开销。
    pub fn remove_snapshot(&self, task_id: &str) -> Result<bool, AppError> {
        self.manager
            .remove_task(task_id)
            .map_err(|e| AppError::Config(format!("删除任务快照失败: {e}")))
    }

    /// 原子性地读取-修改-写入任务快照
    ///
    /// 内部持有锁确保 load-modify-save 序列的原子性,
    /// 防止并发分片进度更新之间的覆盖竞态。
    pub fn update_snapshot(
        &self,
        task_id: &str,
        patch: impl FnOnce(&mut TaskSnapshot),
    ) -> Result<Option<TaskSnapshot>, AppError> {
        self.manager
            .update_snapshot(task_id, patch)
            .map_err(|e| AppError::Config(format!("更新任务快照失败: {e}")))
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
            // 文件大小未知时使用分片完成比例作为进度回退
            if snapshot.total_fragments > 0 {
                (snapshot.completed_fragments.len() as f64 / snapshot.total_fragments as f64)
                    .clamp(0.0, 1.0)
            } else {
                0.0
            }
        } else {
            (snapshot.downloaded as f64 / snapshot.file_size.unwrap_or(1) as f64).clamp(0.0, 1.0)
        },
        fragments_total: snapshot.total_fragments,
        fragments_done: snapshot.completed_fragments.len() as u32,
        active_concurrency: 0,
        created_at: snapshot.created_at.clone(),
        save_path: snapshot.save_path.clone(),
        // 从快照恢复时保留失败原因与重试计数,前端诊断面板可直接使用后端原文
        error_reason: snapshot.fail_reason.clone(),
        retry_count: snapshot.retry_count,
        tags: snapshot.tags.clone(),
        hf_meta: snapshot
            .hf_meta
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        display_order: snapshot.display_order,
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
    partial_fragments: HashMap<u32, u64>,
    etag: Option<String>,
    last_modified: Option<String>,
) -> TaskSnapshot {
    let now = chrono::Local::now().to_rfc3339();
    TaskSnapshot {
        schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
        revision: 0,
        id: task.id.clone(),
        url: task.url.clone(),
        save_path,
        file_name: task.file_name.clone(),
        file_size: task.file_size,
        downloaded: task.downloaded,
        completed_fragments,
        partial_fragments,
        total_fragments: task.fragments_total,
        fragment_size,
        status: task.status,
        etag,
        last_modified,
        content_length: task.file_size,
        created_at: task.created_at.clone(),
        updated_at: now,
        // 保留 TaskInfo 上的失败原因与重试计数,避免持久化时丢失
        fail_reason: task.error_reason.clone(),
        retry_count: task.retry_count,
        tags: task.tags.clone(),
        hf_meta: task
            .hf_meta
            .as_ref()
            .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
            .filter(|v| !v.is_null()),
        display_order: task.display_order,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_to_task_info_preserves_status() {
        let snapshot = TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "task-1".to_string(),
            url: "https://example.com/file.bin".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1000),
            downloaded: 250,
            completed_fragments: vec![0],
            partial_fragments: HashMap::new(),
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
            tags: vec![],
            hf_meta: None,
            display_order: 0,
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
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "task-1".to_string(),
            url: "https://example.com/file.bin".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1000),
            downloaded: 250,
            completed_fragments: vec![0],
            partial_fragments: HashMap::new(),
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
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        };

        store.save_snapshot(&snapshot).unwrap();
        let loaded = store.load_recoverable().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "task-1");
    }

    #[test]
    fn test_load_recoverable_with_warnings_exposes_corrupt_keys() {
        // P1-06续: 损坏快照的 key 必须暴露给调用方(UI 告警),不能被静默吞掉
        let temp = tempfile::tempdir().unwrap();
        let store = TaskStore::open(temp.path()).unwrap();

        // 写入一个正常快照
        let good = TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "good".to_string(),
            url: "https://example.com/good.bin".to_string(),
            save_path: "/downloads/good.bin".to_string(),
            file_name: "good.bin".to_string(),
            file_size: Some(100),
            downloaded: 0,
            completed_fragments: vec![],
            partial_fragments: HashMap::new(),
            total_fragments: 4,
            fragment_size: 25,
            status: DownloadState::Paused,
            etag: None,
            last_modified: None,
            content_length: Some(100),
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:00Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        };
        store.save_snapshot(&good).unwrap();

        // 直接写入损坏 JSON 文件,模拟磁盘损坏
        let corrupt_path = temp.path().join("task_corrupt.json");
        std::fs::write(&corrupt_path, "{ this is not valid json !!!").unwrap();

        let (tasks, corrupt_keys) = store.load_recoverable_with_warnings().unwrap();
        assert_eq!(tasks.len(), 1, "正常任务应被恢复");
        assert_eq!(tasks[0].id, "good");
        assert_eq!(
            corrupt_keys.len(),
            1,
            "损坏 key 必须暴露给调用方以驱动 UI 告警"
        );
        assert!(
            corrupt_keys[0].contains("corrupt"),
            "损坏 key 应包含任务标识: {}",
            corrupt_keys[0]
        );
    }

    #[test]
    fn test_load_recoverable_drops_corrupt_for_backward_compat() {
        // 旧 API load_recoverable 保持 Vec<TaskSnapshot> 返回签名,
        // 调用方若不关心 corrupt 信息仍可正常使用(向后兼容)
        let temp = tempfile::tempdir().unwrap();
        let store = TaskStore::open(temp.path()).unwrap();
        let corrupt_path = temp.path().join("task_bad.json");
        std::fs::write(&corrupt_path, "not json").unwrap();
        // 仅损坏记录:tasks 为空,不报错
        let loaded = store.load_recoverable().unwrap();
        assert!(loaded.is_empty());
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
            active_concurrency: 0,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            error_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
        };

        let snapshot = task_info_to_snapshot(
            &task,
            "/downloads/file.bin".to_string(),
            256,
            vec![],
            HashMap::new(),
            Some("\"abc\"".to_string()),
            None,
        );

        assert_eq!(snapshot.content_length, Some(1024));
        assert_eq!(snapshot.etag.as_deref(), Some("\"abc\""));
        assert_eq!(snapshot.fragment_size, 256);
    }

    #[test]
    fn test_task_info_to_snapshot_roundtrips_tags() {
        let task = TaskInfo {
            id: "task-tags".to_string(),
            url: "https://example.com/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1024),
            downloaded: 0,
            speed: 0,
            status: DownloadState::Pending,
            progress: 0.0,
            fragments_total: 0,
            fragments_done: 0,
            active_concurrency: 0,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            error_reason: None,
            retry_count: 0,
            tags: vec!["model".to_string(), "important".to_string()],
            hf_meta: None,
            display_order: 0,
        };

        let snapshot = task_info_to_snapshot(
            &task,
            "/downloads/file.bin".to_string(),
            256,
            vec![],
            HashMap::new(),
            None,
            None,
        );
        assert_eq!(
            snapshot.tags,
            vec!["model".to_string(), "important".to_string()]
        );

        let recovered = snapshot_to_task_info(&snapshot);
        assert_eq!(recovered.tags, task.tags);
    }
}
