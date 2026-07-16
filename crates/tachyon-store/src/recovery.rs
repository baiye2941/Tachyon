//! 断点续传恢复管理
//!
//! 负责在应用启动时从持久化存储中恢复未完成的下载任务。
//! 提供 `TaskRecord` / `TaskSnapshot` 类型和 `RecoveryManager` 管理器。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::kv::KvStore;

/// 当前快照 schema 版本号
///
/// 每次 TaskSnapshot 结构发生新增/删除/重命名字段时递增。
/// 新增字段必须标注 `#[serde(default)]`，确保旧版本 JSON 可正常反序列化。
/// 删除字段应先改为 `Option<T>` + `#[serde(default)]`，至少保留一个版本周期的兼容。
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 5;

/// 下载任务快照（用于断点续传）
///
/// 记录任务的完整状态，可在应用重启后恢复。
///
/// schema_version 字段用于版本检测和未来迁移:
/// - 旧 JSON(无 schemaVersion 字段)通过 `#[serde(default)]` 自动补为 0
/// - 新 JSON 带有 schemaVersion=1
/// - 迁移策略:若 schema_version < SNAPSHOT_SCHEMA_VERSION,可在加载后补填新字段默认值
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    /// schema 版本号,用于向前兼容检测
    ///
    /// 旧 JSON 不含此字段时默认为 0,加载后可检测并补填。
    #[serde(default)]
    pub schema_version: u32,
    /// 审计 H-05:单调 revision。full-save / patch 成功后 +1;旧 revision 不得覆盖新值。
    #[serde(default)]
    pub revision: u64,
    pub id: String,
    pub url: String,
    pub save_path: String,
    pub file_name: String,
    pub file_size: Option<u64>,
    #[serde(default)]
    pub downloaded: u64,
    #[serde(default)]
    pub completed_fragments: Vec<u32>,
    /// 未完整下载的分片及其已下载字节数(字节级断点续传)
    #[serde(default)]
    pub partial_fragments: HashMap<u32, u64>,
    #[serde(default)]
    pub total_fragments: u32,
    #[serde(default)]
    pub fragment_size: u64,
    pub status: tachyon_core::DownloadState,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub content_length: Option<u64>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub fail_reason: Option<String>,
    #[serde(default)]
    pub retry_count: u32,
    /// 用户自定义任务标签(如 "important"、"model" 等),用于前端分组/过滤。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hf_meta: Option<serde_json::Value>,
    /// 任务在列表中的显示顺序,越小越靠前。
    /// 旧版快照无此字段时默认 0,保持与创建时间降序的兼容排序。
    #[serde(default)]
    pub display_order: i64,
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
            schema_version: 0, // 旧记录无 schema 版本,标记为 0 表示需要迁移
            revision: 0,
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
            partial_fragments: HashMap::new(),
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
            tags: Vec::new(),
            hf_meta: None,
            display_order: 0,
        }
    }
}

fn parse_legacy_status(status: &str) -> tachyon_core::DownloadState {
    // A-02: 利用 strum::EnumString 自动派生的 FromStr，
    // 未知状态字符串回退到 Failed（兼容旧数据）。
    use std::str::FromStr;
    tachyon_core::DownloadState::from_str(status).unwrap_or(tachyon_core::DownloadState::Failed)
}

/// 恢复结果:包含成功恢复的任务和无法解析的损坏 key
///
/// 单个损坏 JSON 不会阻断其他任务的恢复(隔离策略)。
#[derive(Debug)]
pub struct RecoveryResult {
    /// 成功恢复的任务快照
    pub tasks: Vec<TaskSnapshot>,
    /// 无法解析的 key 列表(记录日志供排查,不中断恢复流程)
    pub corrupt_keys: Vec<String>,
}

/// 恢复管理器
///
/// 负责任务快照的持久化与恢复。所有 `task_*` 键的写入均走强制 Durable 路径
/// (fsync 数据文件 + 目录),以满足崩溃恢复承诺(见 [`Self::save_task_snapshot`])。
pub struct RecoveryManager {
    store: KvStore,
    /// 序列化所有快照 mutation(full-save / patch / delete),防止并发覆盖
    progress_lock: std::sync::Mutex<()>,
    /// 审计 H-05:删除 tombstone。key=task_id, value=删除时磁盘 revision。
    /// 之后任何 `revision <= tombstone` 的 save 拒绝,防止旧 full-save 复活已删任务。
    delete_tombstones: std::sync::Mutex<HashMap<String, u64>>,
}

impl RecoveryManager {
    /// 创建恢复管理器
    pub fn new(store: KvStore) -> Self {
        Self {
            store,
            progress_lock: std::sync::Mutex::new(()),
            delete_tombstones: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 保存任务快照(强制持久化)
    ///
    /// 即使底层 `KvStore` 以 `Durability::Fast` 打开,本方法仍通过
    /// [`KvStore::put_durable`] 对本次写入执行 `sync_all`(数据文件 + 目录),
    /// 保证进程崩溃/断电后任务进度不丢失。
    ///
    /// # 为什么 RecoveryManager 必须 Durable
    ///
    /// `RecoveryManager` 的核心职责是崩溃恢复:应用重启后从这里重建未完成下载。
    /// 若快照写走 Fast 模式(仅依赖 OS 页面缓存),进程崩溃或断电时最新进度会丢失,
    /// 恢复时只能读到上一次落盘的旧进度,导致已下载分片被重复下载。
    ///
    /// # 为什么 Durable 在热路径可接受
    ///
    /// 生产热路径(`chunk_reader_pool.rs`)走 [`Self::update_snapshot`],
    /// 该方法已通过 `CHECKPOINT_BATCH_SIZE` 与 `PARTIAL_CHECKPOINT_INTERVAL`
    /// 限频(批量 + 时间间隔双维度节流),Durable 的 fsync 开销被摊薄到可控频率,
    /// 不会成为每分片的热点。
    pub fn save_task_snapshot(&self, snapshot: &TaskSnapshot) -> std::io::Result<()> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.save_task_snapshot_locked(snapshot)
    }

    /// 撤销删除等显式恢复路径:清除 tombstone 后强制写入。
    pub fn restore_task_snapshot(&self, snapshot: &TaskSnapshot) -> std::io::Result<()> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut tombs) = self.delete_tombstones.lock() {
            tombs.remove(&snapshot.id);
        }
        self.save_task_snapshot_locked(snapshot)
    }

    fn save_task_snapshot_locked(&self, snapshot: &TaskSnapshot) -> std::io::Result<()> {
        let key = format!("task_{}", snapshot.id);

        // tombstone:拒绝基于删除前状态的旧写
        if let Ok(tombs) = self.delete_tombstones.lock()
            && let Some(&tomb_rev) = tombs.get(&snapshot.id)
            && snapshot.revision <= tomb_rev
        {
            tracing::warn!(
                task_id = %snapshot.id,
                incoming_revision = snapshot.revision,
                tombstone_revision = tomb_rev,
                "拒绝写入已删除任务快照(H-05 tombstone)"
            );
            return Ok(());
        }

        let existing = self.load_task_snapshot_by_key(&key)?;
        let base_rev = existing.as_ref().map(|s| s.revision).unwrap_or(0);
        if snapshot.revision < base_rev {
            tracing::warn!(
                task_id = %snapshot.id,
                incoming_revision = snapshot.revision,
                disk_revision = base_rev,
                "拒绝过期快照写入(H-05 revision CAS)"
            );
            return Ok(());
        }

        let mut to_write = snapshot.clone();
        to_write.revision = base_rev.saturating_add(1);
        if to_write.schema_version < SNAPSHOT_SCHEMA_VERSION {
            to_write.schema_version = SNAPSHOT_SCHEMA_VERSION;
        }
        self.store.put_durable(&key, &to_write)
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

    /// 加载所有任务快照,隔离损坏记录
    ///
    /// 单个 key 解析失败不会中断其他任务的恢复,而是记录到 `corrupt_keys` 中。
    pub fn load_all_task_snapshots(&self) -> std::io::Result<RecoveryResult> {
        let mut tasks = Vec::new();
        let mut corrupt_keys = Vec::new();
        for key in self.store.keys()? {
            if key.starts_with("task_") {
                match self.load_task_snapshot_by_key(&key) {
                    Ok(Some(snapshot)) => tasks.push(snapshot),
                    Ok(None) => {} // key 存在但无数据,忽略
                    Err(_) => {
                        tracing::warn!(key = %key, "快照 JSON 损坏,跳过恢复");
                        corrupt_keys.push(key);
                    }
                }
            }
        }
        Ok(RecoveryResult {
            tasks,
            corrupt_keys,
        })
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

    /// 删除任务记录(审计 H-05:持锁 + tombstone 防旧 save 复活)
    pub fn remove_task(&self, task_id: &str) -> std::io::Result<bool> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        let key = format!("task_{task_id}");
        let existing_rev = self
            .load_task_snapshot_by_key(&key)?
            .map(|s| s.revision)
            .unwrap_or(0);
        let deleted = self.store.delete(&key)?;
        if let Ok(mut tombs) = self.delete_tombstones.lock() {
            // 即便 key 本就不存在,也记 tombstone,挡住 in-flight 的旧 full-save
            tombs.insert(task_id.to_string(), existing_rev);
        }
        Ok(deleted)
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

    /// 恢复所有未完成的任务（新接口）,隔离损坏记录
    ///
    /// 单个 key 解析失败不会中断恢复,而是记录到 `corrupt_keys` 中。
    pub fn recover_pending_snapshots(&self) -> std::io::Result<RecoveryResult> {
        let mut tasks = Vec::new();
        let mut corrupt_keys = Vec::new();
        for key in self.store.keys()? {
            if key.starts_with("task_") {
                match self.load_task_snapshot_by_key(&key) {
                    Ok(Some(snapshot))
                        if matches!(
                            snapshot.status,
                            tachyon_core::DownloadState::Downloading
                                | tachyon_core::DownloadState::Paused
                        ) =>
                    {
                        tracing::info!(task_id = %snapshot.id, "恢复下载任务");
                        tasks.push(snapshot);
                    }
                    Ok(_) => {} // 完成或空,跳过
                    Err(_) => {
                        tracing::warn!(key = %key, "快照 JSON 损坏,跳过恢复");
                        corrupt_keys.push(key);
                    }
                }
            }
        }
        Ok(RecoveryResult {
            tasks,
            corrupt_keys,
        })
    }

    /// 原子性地读取-修改-写入任务快照
    ///
    /// 内部持有 `progress_lock` 确保 load-modify-save 序列的原子性,
    /// 防止并发分片进度更新之间的覆盖竞态。
    ///
    /// # 参数
    /// - `task_id`: 任务 ID
    /// - `patch`: 闭包,接收可变引用到快照,在锁内执行修改
    ///
    /// # 返回
    /// - `Ok(Some(TaskSnapshot))`: 快照存在且已更新
    /// - `Ok(None)`: 快照不存在
    /// - `Err`: I/O 或序列化错误
    pub fn update_snapshot(
        &self,
        task_id: &str,
        patch: impl FnOnce(&mut TaskSnapshot),
    ) -> std::io::Result<Option<TaskSnapshot>> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        let key = format!("task_{task_id}");
        let mut snapshot = match self.load_task_snapshot_by_key(&key)? {
            Some(s) => s,
            None => return Ok(None),
        };

        patch(&mut snapshot);

        // 确保新写入的快照使用当前 schema 版本
        if snapshot.schema_version < SNAPSHOT_SCHEMA_VERSION {
            snapshot.schema_version = SNAPSHOT_SCHEMA_VERSION;
        }

        // 已持 progress_lock,走 locked save(revision CAS + bump)
        self.save_task_snapshot_locked(&snapshot)?;
        // 返回磁盘最终 revision
        let final_snap = self.load_task_snapshot_by_key(&key)?;
        Ok(final_snap)
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
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: id.to_string(),
            url: format!("https://example.com/{id}.zip"),
            save_path: format!("/downloads/{id}.zip"),
            file_name: format!("{id}.zip"),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            partial_fragments: HashMap::new(),
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
            tags: vec![],
            hf_meta: None,
            display_order: 0,
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
        // save 会 bump revision:0 -> 1
        let mut expected = snap;
        expected.revision = 1;
        assert_eq!(loaded, expected);
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

        let result = mgr.load_all_task_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 3);
        assert!(result.corrupt_keys.is_empty());
        let ids: Vec<&str> = result.tasks.iter().map(|s| s.id.as_str()).collect();
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

        let result = mgr.recover_pending_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 2);
        assert!(result.corrupt_keys.is_empty());
        let ids: Vec<&str> = result.tasks.iter().map(|s| s.id.as_str()).collect();
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
        let mut expected = snap;
        expected.revision = 1;
        assert_eq!(loaded, expected);
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

        let result = mgr.recover_pending_snapshots().unwrap();

        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].id, "legacy-1");
        assert_eq!(result.tasks[0].file_name, "legacy.bin");
        assert_eq!(result.tasks[0].status, tachyon_core::DownloadState::Paused);
    }

    #[test]
    fn test_task_snapshot_serializes_typed_status_and_metadata() {
        let snapshot = TaskSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "task-1".to_string(),
            url: "https://example.com/file.bin".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            partial_fragments: HashMap::new(),
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
            tags: vec!["model".to_string(), "important".to_string()],
            hf_meta: None,
            display_order: 0,
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("paused"));
        let loaded: TaskSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.status, tachyon_core::DownloadState::Paused);
        assert_eq!(loaded.completed_fragments, vec![0, 1]);
        assert_eq!(loaded.etag.as_deref(), Some("\"abc\""));
    }

    // ── schema_version 兼容性测试 ──

    #[test]
    fn snapshot_old_json_without_schema_version_deserializes() {
        // 模拟旧版本 JSON(无 schemaVersion 字段)
        let old_json = r#"{
            "id":"old-task",
            "url":"https://example.com/old.bin",
            "savePath":"/downloads/old.bin",
            "fileName":"old.bin",
            "fileSize":2048,
            "downloaded":512,
            "completedFragments":[0],
            "totalFragments":4,
            "fragmentSize":512,
            "status":"downloading",
            "createdAt":"2026-01-01T00:00:00Z",
            "updatedAt":"2026-01-01T00:00:01Z",
            "retryCount":0
        }"#;
        let snapshot: TaskSnapshot = serde_json::from_str(old_json).unwrap();
        // 旧 JSON 无 schemaVersion,应默认为 0
        assert_eq!(snapshot.schema_version, 0);
        assert_eq!(snapshot.id, "old-task");
        assert_eq!(snapshot.downloaded, 512);
        // 旧 JSON 无 displayOrder,应默认为 0
        assert_eq!(snapshot.display_order, 0);
    }

    #[test]
    fn snapshot_new_json_with_schema_version_deserializes() {
        let new_json = r#"{
            "schemaVersion":1,
            "id":"new-task",
            "url":"https://example.com/new.bin",
            "savePath":"/downloads/new.bin",
            "fileName":"new.bin",
            "fileSize":4096,
            "downloaded":1024,
            "completedFragments":[0,1],
            "totalFragments":8,
            "fragmentSize":512,
            "status":"paused",
            "createdAt":"2026-06-01T00:00:00Z",
            "updatedAt":"2026-06-01T00:00:01Z",
            "retryCount":1
        }"#;
        let snapshot: TaskSnapshot = serde_json::from_str(new_json).unwrap();
        assert_eq!(snapshot.schema_version, 1);
        assert_eq!(snapshot.id, "new-task");
    }

    // ── 坏 JSON 隔离测试 ──

    #[test]
    fn corrupt_json_is_isolated_during_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        // 保存一个正常任务
        mgr.save_task_snapshot(&make_snapshot(
            "good",
            tachyon_core::DownloadState::Downloading,
        ))
        .unwrap();

        // 直接写入一个损坏的 JSON 文件
        let corrupt_path = tmp.path().join("task_corrupt.json");
        std::fs::write(&corrupt_path, "{ this is not valid json !!!").unwrap();

        // 恢复不应失败,应返回正常任务并标记损坏 key
        let result = mgr.recover_pending_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].id, "good");
        assert_eq!(result.corrupt_keys.len(), 1);
        assert!(result.corrupt_keys[0].contains("corrupt"));
    }

    #[test]
    fn corrupt_json_is_isolated_in_load_all() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        mgr.save_task_snapshot(&make_snapshot(
            "ok1",
            tachyon_core::DownloadState::Completed,
        ))
        .unwrap();
        mgr.save_task_snapshot(&make_snapshot("ok2", tachyon_core::DownloadState::Paused))
            .unwrap();

        let corrupt_path = tmp.path().join("task_bad.json");
        std::fs::write(&corrupt_path, "not json at all").unwrap();

        let result = mgr.load_all_task_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 2);
        assert_eq!(result.corrupt_keys.len(), 1);
    }

    // ── B7: RecoveryManager 强制 Durable 测试 ──

    /// B7: RecoveryManager 在 Fast store 上写入快照后,重新打开仍可恢复
    ///
    /// `save_task_snapshot` 必须走 `put_durable`(fsync),保证进程崩溃后进度不丢失。
    /// 此测试用 "关闭实例后重开" 模拟崩溃:若写入仅停留在 OS 页面缓存,
    /// 在真实断电场景会丢失;此处验证至少数据已正确落盘到文件系统可读状态。
    #[test]
    fn recovery_snapshot_survives_reopen_on_fast_store() {
        let tmp = tempfile::tempdir().unwrap();
        let snap = make_snapshot("crash", tachyon_core::DownloadState::Downloading);

        // 写入后关闭实例(模拟进程退出/崩溃)
        {
            let store = KvStore::open(tmp.path()).unwrap();
            assert_eq!(store.durability(), crate::Durability::Fast);
            let mgr = RecoveryManager::new(store);
            mgr.save_task_snapshot(&snap).unwrap();
        }

        // 重新打开,验证快照可恢复
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let loaded = mgr.load_task_snapshot("crash").unwrap().unwrap();
        let mut expected = snap;
        expected.revision = 1;
        assert_eq!(loaded, expected);
    }

    /// B7: `update_snapshot` 同样走 Durable 路径(经 save_task_snapshot),
    /// 重开后 patch 后的进度可恢复
    #[test]
    fn recovery_update_snapshot_survives_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let store = KvStore::open(tmp.path()).unwrap();
            let mgr = RecoveryManager::new(store);
            let snap = make_snapshot("up", tachyon_core::DownloadState::Downloading);
            mgr.save_task_snapshot(&snap).unwrap();
            mgr.update_snapshot("up", |s| {
                s.downloaded = 999;
                s.completed_fragments.push(2);
            })
            .unwrap();
        }

        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let loaded = mgr.load_task_snapshot("up").unwrap().unwrap();
        assert_eq!(loaded.downloaded, 999);
        assert!(loaded.completed_fragments.contains(&2));
    }

    // ── update_snapshot 原子性测试 ──

    #[test]
    fn update_snapshot_applies_patch_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let snap = make_snapshot("atomic", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();

        let updated = mgr
            .update_snapshot("atomic", |s| {
                s.downloaded = 768;
                s.completed_fragments.push(2);
            })
            .unwrap()
            .unwrap();

        assert_eq!(updated.downloaded, 768);
        assert!(updated.completed_fragments.contains(&2));
        assert_eq!(updated.schema_version, SNAPSHOT_SCHEMA_VERSION);

        // 验证持久化
        let loaded = mgr.load_task_snapshot("atomic").unwrap().unwrap();
        assert_eq!(loaded.downloaded, 768);
        assert!(loaded.completed_fragments.contains(&2));
    }

    #[test]
    fn update_snapshot_returns_none_for_missing_task() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let result = mgr.update_snapshot("nonexistent", |_| {}).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn update_snapshot_upgrades_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        // 直接写入旧 schema 版本的 JSON
        let mut snap = make_snapshot("old-schema", tachyon_core::DownloadState::Paused);
        snap.schema_version = 0;
        mgr.save_task_snapshot(&snap).unwrap();

        let updated = mgr.update_snapshot("old-schema", |_| {}).unwrap().unwrap();

        assert_eq!(updated.schema_version, SNAPSHOT_SCHEMA_VERSION);
    }

    #[test]
    fn test_h05_stale_full_save_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let mut paused = make_snapshot("h05", tachyon_core::DownloadState::Paused);
        mgr.save_task_snapshot(&paused).unwrap();
        let on_disk = mgr.load_task_snapshot("h05").unwrap().unwrap();
        assert_eq!(on_disk.revision, 1);
        assert_eq!(on_disk.status, tachyon_core::DownloadState::Paused);

        // 模拟较新 full-save(Downloading)先基于 rev1 写出
        let mut downloading = on_disk.clone();
        downloading.status = tachyon_core::DownloadState::Downloading;
        mgr.save_task_snapshot(&downloading).unwrap();
        let mid = mgr.load_task_snapshot("h05").unwrap().unwrap();
        assert_eq!(mid.revision, 2);
        assert_eq!(mid.status, tachyon_core::DownloadState::Downloading);

        // 旧的 Paused full-save(仍带 rev1)后到,必须拒绝
        paused.revision = 1;
        paused.status = tachyon_core::DownloadState::Paused;
        mgr.save_task_snapshot(&paused).unwrap();
        let final_snap = mgr.load_task_snapshot("h05").unwrap().unwrap();
        assert_eq!(final_snap.revision, 2);
        assert_eq!(
            final_snap.status,
            tachyon_core::DownloadState::Downloading,
            "过期 full-save 不得覆盖较新状态"
        );
    }

    #[test]
    fn test_h05_remove_then_stale_save_does_not_resurrect() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let snap = make_snapshot("gone", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();
        let on_disk = mgr.load_task_snapshot("gone").unwrap().unwrap();
        assert!(mgr.remove_task("gone").unwrap());
        assert!(mgr.load_task_snapshot("gone").unwrap().is_none());

        // 旧 in-flight save 带着删除前 revision,不得复活
        mgr.save_task_snapshot(&on_disk).unwrap();
        assert!(
            mgr.load_task_snapshot("gone").unwrap().is_none(),
            "删除后旧 save 不得复活快照"
        );
    }

    #[test]
    fn test_h05_restore_after_delete_clears_tombstone() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let snap = make_snapshot("undo", tachyon_core::DownloadState::Paused);
        mgr.save_task_snapshot(&snap).unwrap();
        let on_disk = mgr.load_task_snapshot("undo").unwrap().unwrap();
        assert!(mgr.remove_task("undo").unwrap());
        mgr.restore_task_snapshot(&on_disk).unwrap();
        let restored = mgr.load_task_snapshot("undo").unwrap().unwrap();
        assert_eq!(restored.status, tachyon_core::DownloadState::Paused);
        assert!(restored.revision >= 1);
    }
}
