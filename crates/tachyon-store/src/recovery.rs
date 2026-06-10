//! 断点续传恢复管理
//!
//! 负责在应用启动时从持久化存储中恢复未完成的下载任务。
//! 提供 `TaskRecord` / `TaskSnapshot` 类型和 `RecoveryManager` 管理器。

use serde::{Deserialize, Serialize};

use crate::kv::KvStore;

/// 下载任务快照（用于断点续传）
///
/// 记录任务的完整状态，可在应用重启后恢复。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub id: String,
    pub url: String,
    pub save_path: String,
    pub file_name: String,
    pub file_size: Option<u64>,
    pub downloaded: u64,
    pub completed_fragments: Vec<u32>,
    pub total_fragments: u32,
    pub fragment_size: u64,
    pub status: tachyon_core::DownloadState,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_length: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
    pub fail_reason: Option<String>,
    pub retry_count: u32,
}

/// 下载任务持久化记录（旧接口，保持向后兼容）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// 任务 ID
    pub task_id: String,
    /// 下载 URL
    pub url: String,
    /// 保存路径
    pub save_path: String,
    /// 文件总大小（字节）
    pub file_size: Option<u64>,
    /// 已下载字节数
    pub downloaded: u64,
    /// 已完成的分片索引列表
    pub completed_fragments: Vec<u32>,
    /// 分片总数
    pub total_fragments: u32,
    /// 任务状态
    pub status: String,
}

impl From<TaskSnapshot> for TaskRecord {
    fn from(s: TaskSnapshot) -> Self {
        Self {
            task_id: s.id,
            url: s.url,
            save_path: s.save_path,
            file_size: s.file_size,
            downloaded: s.downloaded,
            completed_fragments: s.completed_fragments,
            total_fragments: s.total_fragments,
            status: format!("{:?}", s.status).to_lowercase(),
        }
    }
}

impl From<TaskRecord> for TaskSnapshot {
    fn from(r: TaskRecord) -> Self {
        Self {
            id: r.task_id,
            url: r.url,
            save_path: r.save_path.clone(),
            file_name: std::path::Path::new(&r.save_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_string(),
            file_size: r.file_size,
            downloaded: r.downloaded,
            completed_fragments: r.completed_fragments,
            total_fragments: r.total_fragments,
            fragment_size: 0,
            status: parse_legacy_status(&r.status),
            etag: None,
            last_modified: None,
            content_length: r.file_size,
            created_at: String::new(),
            updated_at: String::new(),
            fail_reason: None,
            retry_count: 0,
        }
    }
}

fn parse_legacy_status(status: &str) -> tachyon_core::DownloadState {
    // A-02: 利用 strum::EnumString 自动派生的 FromStr，
    // 未知状态字符串回退到 Failed（兼容旧数据）。
    use std::str::FromStr;
    tachyon_core::DownloadState::from_str(status).unwrap_or(tachyon_core::DownloadState::Failed)
}

/// 恢复管理器
pub struct RecoveryManager {
    store: KvStore,
    /// 序列化 read-modify-write 操作,防止并发分片进度更新丢失
    progress_lock: std::sync::Mutex<()>,
}

impl RecoveryManager {
    /// 创建恢复管理器
    pub fn new(store: KvStore) -> Self {
        Self {
            store,
            progress_lock: std::sync::Mutex::new(()),
        }
    }

    /// 保存任务快照
    pub fn save_task_snapshot(&self, snapshot: &TaskSnapshot) -> std::io::Result<()> {
        self.store.put(&format!("task_{}", snapshot.id), snapshot)
    }

    /// 加载任务快照
    pub fn load_task_snapshot(&self, task_id: &str) -> std::io::Result<Option<TaskSnapshot>> {
        self.load_task_snapshot_by_key(&format!("task_{task_id}"))
    }

    fn load_task_snapshot_by_key(&self, key: &str) -> std::io::Result<Option<TaskSnapshot>> {
        let Some(json) = self.store.get_raw(key)? else {
            return Ok(None);
        };
        serde_json::from_str::<TaskSnapshot>(&json)
            .or_else(|_| serde_json::from_str::<TaskRecord>(&json).map(TaskSnapshot::from))
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// 加载所有任务快照
    pub fn load_all_task_snapshots(&self) -> std::io::Result<Vec<TaskSnapshot>> {
        let mut tasks = Vec::new();
        for key in self.store.keys()? {
            if key.starts_with("task_")
                && let Some(snapshot) = self.load_task_snapshot_by_key(&key)?
            {
                tasks.push(snapshot);
            }
        }
        Ok(tasks)
    }

    /// 保存任务记录（旧接口）
    pub fn save_task(&self, record: &TaskRecord) -> std::io::Result<()> {
        let snapshot: TaskSnapshot = TaskSnapshot::from(record.clone());
        self.save_task_snapshot(&snapshot)
    }

    /// 加载任务记录（旧接口）
    pub fn load_task(&self, task_id: &str) -> std::io::Result<Option<TaskRecord>> {
        Ok(self.load_task_snapshot(task_id)?.map(TaskRecord::from))
    }

    /// 删除任务记录
    pub fn remove_task(&self, task_id: &str) -> std::io::Result<bool> {
        self.store.delete(&format!("task_{task_id}"))
    }

    /// 恢复所有未完成的任务
    pub fn recover_pending_tasks(&self) -> std::io::Result<Vec<TaskRecord>> {
        let mut pending = Vec::new();
        for key in self.store.keys()? {
            if let Some(task_id) = key.strip_prefix("task_")
                && let Some(record) = self.load_task(task_id)?
                && (record.status == "downloading" || record.status == "paused")
            {
                tracing::info!(task_id = %record.task_id, "恢复下载任务");
                pending.push(record);
            }
        }
        Ok(pending)
    }

    /// 恢复所有未完成的任务（新接口）
    pub fn recover_pending_snapshots(&self) -> std::io::Result<Vec<TaskSnapshot>> {
        let mut pending = Vec::new();
        for snapshot in self.load_all_task_snapshots()? {
            if matches!(
                snapshot.status,
                tachyon_core::DownloadState::Downloading | tachyon_core::DownloadState::Paused
            ) {
                tracing::info!(task_id = %snapshot.id, "恢复下载任务");
                pending.push(snapshot);
            }
        }
        Ok(pending)
    }

    /// 更新分片进度
    ///
    /// 使用内部锁序列化 read-modify-write,防止并发分片完成时丢失更新。
    pub fn update_fragment_progress(
        &self,
        task_id: &str,
        fragment_index: u32,
        downloaded_bytes: u64,
    ) -> std::io::Result<()> {
        let _guard = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut record) = self.load_task(task_id)? {
            if !record.completed_fragments.contains(&fragment_index) {
                record.completed_fragments.push(fragment_index);
            }
            record.downloaded = downloaded_bytes;
            self.save_task(&record)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(task_id: &str, status: &str) -> TaskRecord {
        TaskRecord {
            task_id: task_id.to_string(),
            url: format!("https://example.com/{task_id}.zip"),
            save_path: format!("/downloads/{task_id}.zip"),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            total_fragments: 4,
            status: status.to_string(),
        }
    }

    fn make_snapshot(id: &str, status: tachyon_core::DownloadState) -> TaskSnapshot {
        TaskSnapshot {
            id: id.to_string(),
            url: format!("https://example.com/{id}.zip"),
            save_path: format!("/downloads/{id}.zip"),
            file_name: format!("{id}.zip"),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            total_fragments: 4,
            fragment_size: 256,
            status,
            etag: None,
            last_modified: None,
            content_length: Some(1024),
            created_at: String::new(),
            updated_at: String::new(),
            fail_reason: None,
            retry_count: 0,
        }
    }

    // ── TaskRecord 旧接口测试 ──

    #[test]
    fn test_save_and_load_task() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let record = make_record("task-1", "downloading");
        mgr.save_task(&record).unwrap();
        let loaded = mgr.load_task("task-1").unwrap().unwrap();
        assert_eq!(loaded.task_id, "task-1");
        assert_eq!(loaded.downloaded, 512);
    }

    #[test]
    fn test_recover_pending_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task(&make_record("t1", "downloading")).unwrap();
        mgr.save_task(&make_record("t2", "completed")).unwrap();
        mgr.save_task(&make_record("t3", "paused")).unwrap();
        mgr.save_task(&make_record("t4", "failed")).unwrap();
        let pending = mgr.recover_pending_tasks().unwrap();
        assert_eq!(pending.len(), 2);
        let ids: Vec<&str> = pending.iter().map(|r| r.task_id.as_str()).collect();
        assert!(ids.contains(&"t1"));
        assert!(ids.contains(&"t3"));
    }

    #[test]
    fn test_update_fragment_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task(&make_record("t1", "downloading")).unwrap();
        mgr.update_fragment_progress("t1", 2, 768).unwrap();
        let record = mgr.load_task("t1").unwrap().unwrap();
        assert!(record.completed_fragments.contains(&2));
        assert_eq!(record.downloaded, 768);
    }

    #[test]
    fn test_remove_task() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task(&make_record("t1", "completed")).unwrap();
        assert!(mgr.remove_task("t1").unwrap());
        assert!(mgr.load_task("t1").unwrap().is_none());
    }

    #[test]
    fn test_load_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        assert!(mgr.load_task("no-such-task").unwrap().is_none());
    }

    // ── TaskSnapshot 新接口测试 ──

    #[test]
    fn snapshot_save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("s1", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();
        let loaded = mgr.load_task_snapshot("s1").unwrap().unwrap();
        assert_eq!(loaded, snap);
    }

    #[test]
    fn snapshot_load_all() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task_snapshot(&make_snapshot(
            "a",
            tachyon_core::DownloadState::Downloading,
        ))
        .unwrap();
        mgr.save_task_snapshot(&make_snapshot("b", tachyon_core::DownloadState::Completed))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("c", tachyon_core::DownloadState::Paused))
            .unwrap();

        let all = mgr.load_all_task_snapshots().unwrap();
        assert_eq!(all.len(), 3);
        let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"c"));
    }

    #[test]
    fn snapshot_recover_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task_snapshot(&make_snapshot(
            "p1",
            tachyon_core::DownloadState::Downloading,
        ))
        .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p2", tachyon_core::DownloadState::Completed))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p3", tachyon_core::DownloadState::Paused))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p4", tachyon_core::DownloadState::Failed))
            .unwrap();

        let pending = mgr.recover_pending_snapshots().unwrap();
        assert_eq!(pending.len(), 2);
        let ids: Vec<&str> = pending.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"p1"));
        assert!(ids.contains(&"p3"));
    }

    #[test]
    fn snapshot_load_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        assert!(mgr.load_task_snapshot("ghost").unwrap().is_none());
    }

    #[test]
    fn snapshot_to_record_conversion() {
        let snap = make_snapshot("conv", tachyon_core::DownloadState::Downloading);
        let record: TaskRecord = snap.clone().into();
        assert_eq!(record.task_id, "conv");
        assert_eq!(record.completed_fragments, vec![0, 1]);
        assert_eq!(record.status, "downloading");
    }

    // ── 边界条件 ──

    #[test]
    fn snapshot_empty_fragments() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("empty", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();
        let loaded = mgr.load_task_snapshot("empty").unwrap().unwrap();
        assert_eq!(loaded, snap);
    }

    #[test]
    fn snapshot_recovers_legacy_task_record_json() {
        let tmp = tempfile::tempdir().unwrap();
        let raw_json = r#"{
            "task_id":"legacy-1",
            "url":"https://example.com/legacy.bin",
            "save_path":"/downloads/legacy.bin",
            "file_size":1024,
            "downloaded":512,
            "completed_fragments":[0,1],
            "total_fragments":4,
            "status":"paused"
        }"#;
        std::fs::write(tmp.path().join("task_legacy_1.json"), raw_json).unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let pending = mgr.recover_pending_snapshots().unwrap();

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "legacy-1");
        assert_eq!(pending[0].file_name, "legacy.bin");
        assert_eq!(pending[0].status, tachyon_core::DownloadState::Paused);
    }

    #[test]
    fn test_task_snapshot_serializes_typed_status_and_metadata() {
        let snapshot = TaskSnapshot {
            id: "task-1".to_string(),
            url: "https://example.com/file.bin".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            total_fragments: 4,
            fragment_size: 256,
            status: tachyon_core::DownloadState::Paused,
            etag: Some("\"abc\"".to_string()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
            content_length: Some(1024),
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 0,
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("paused"));
        let loaded: TaskSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.status, tachyon_core::DownloadState::Paused);
        assert_eq!(loaded.completed_fragments, vec![0, 1]);
        assert_eq!(loaded.etag.as_deref(), Some("\"abc\""));
    }
}
