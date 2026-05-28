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
pub struct TaskSnapshot {
    /// 任务 ID
    pub id: String,
    /// 下载 URL
    pub url: String,
    /// 保存路径
    pub save_path: String,
    /// 文件总大小（字节）
    pub file_size: Option<u64>,
    /// 已下载字节数
    pub downloaded: u64,
    /// 已完成的分片索引列表
    pub fragments: Vec<u32>,
    /// 分片总数
    pub total_fragments: u32,
    /// 任务状态：downloading / paused / completed / failed
    pub status: String,
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
            completed_fragments: s.fragments,
            total_fragments: s.total_fragments,
            status: s.status,
        }
    }
}

impl From<TaskRecord> for TaskSnapshot {
    fn from(r: TaskRecord) -> Self {
        Self {
            id: r.task_id,
            url: r.url,
            save_path: r.save_path,
            file_size: r.file_size,
            downloaded: r.downloaded,
            fragments: r.completed_fragments,
            total_fragments: r.total_fragments,
            status: r.status,
        }
    }
}

/// 恢复管理器
pub struct RecoveryManager {
    store: KvStore,
}

impl RecoveryManager {
    /// 创建恢复管理器
    pub fn new(store: KvStore) -> Self {
        Self { store }
    }

    /// 保存任务快照
    pub fn save_task_snapshot(&self, snapshot: &TaskSnapshot) -> std::io::Result<()> {
        self.store.put(&format!("task_{}", snapshot.id), snapshot)
    }

    /// 加载任务快照
    pub fn load_task_snapshot(&self, task_id: &str) -> std::io::Result<Option<TaskSnapshot>> {
        self.store.get(&format!("task_{task_id}"))
    }

    /// 加载所有任务快照
    pub fn load_all_task_snapshots(&self) -> std::io::Result<Vec<TaskSnapshot>> {
        let mut tasks = Vec::new();
        for key in self.store.keys()? {
            if key.starts_with("task_") {
                if let Some(snapshot) = self.store.get::<TaskSnapshot>(&key)? {
                    tasks.push(snapshot);
                }
            }
        }
        Ok(tasks)
    }

    /// 保存任务记录（旧接口）
    pub fn save_task(&self, record: &TaskRecord) -> std::io::Result<()> {
        let snapshot: TaskSnapshot = TaskSnapshot {
            id: record.task_id.clone(),
            url: record.url.clone(),
            save_path: record.save_path.clone(),
            file_size: record.file_size,
            downloaded: record.downloaded,
            fragments: record.completed_fragments.clone(),
            total_fragments: record.total_fragments,
            status: record.status.clone(),
        };
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
            if let Some(task_id) = key.strip_prefix("task_") {
                if let Some(record) = self.load_task(task_id)? {
                    if record.status == "downloading" || record.status == "paused" {
                        pending.push(record);
                    }
                }
            }
        }
        Ok(pending)
    }

    /// 恢复所有未完成的任务（新接口）
    pub fn recover_pending_snapshots(&self) -> std::io::Result<Vec<TaskSnapshot>> {
        let mut pending = Vec::new();
        for snapshot in self.load_all_task_snapshots()? {
            if snapshot.status == "downloading" || snapshot.status == "paused" {
                pending.push(snapshot);
            }
        }
        Ok(pending)
    }

    /// 更新分片进度
    pub fn update_fragment_progress(
        &self,
        task_id: &str,
        fragment_index: u32,
        downloaded_bytes: u64,
    ) -> std::io::Result<()> {
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

    fn make_snapshot(id: &str, status: &str) -> TaskSnapshot {
        TaskSnapshot {
            id: id.to_string(),
            url: format!("https://example.com/{id}.zip"),
            save_path: format!("/downloads/{id}.zip"),
            file_size: Some(1024),
            downloaded: 512,
            fragments: vec![0, 1],
            total_fragments: 4,
            status: status.to_string(),
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
        let snap = make_snapshot("s1", "downloading");
        mgr.save_task_snapshot(&snap).unwrap();
        let loaded = mgr.load_task_snapshot("s1").unwrap().unwrap();
        assert_eq!(loaded, snap);
    }

    #[test]
    fn snapshot_load_all() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task_snapshot(&make_snapshot("a", "downloading"))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("b", "completed"))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("c", "paused"))
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
        mgr.save_task_snapshot(&make_snapshot("p1", "downloading"))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p2", "completed"))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p3", "paused"))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p4", "failed"))
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
        let snap = make_snapshot("conv", "downloading");
        let record: TaskRecord = snap.clone().into();
        assert_eq!(record.task_id, "conv");
        assert_eq!(record.completed_fragments, vec![0, 1]);

        let back: TaskSnapshot = record.into();
        assert_eq!(back, snap);
    }

    // ── 边界条件 ──

    #[test]
    fn snapshot_empty_fragments() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = TaskSnapshot {
            id: "empty".into(),
            url: "https://example.com/empty".into(),
            save_path: "/tmp/empty".into(),
            file_size: None,
            downloaded: 0,
            fragments: vec![],
            total_fragments: 0,
            status: "downloading".into(),
        };
        mgr.save_task_snapshot(&snap).unwrap();
        let loaded = mgr.load_task_snapshot("empty").unwrap().unwrap();
        assert_eq!(loaded, snap);
        assert!(loaded.fragments.is_empty());
        assert!(loaded.file_size.is_none());
    }
}
