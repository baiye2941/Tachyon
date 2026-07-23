use std::collections::HashMap;
use std::path::Path;

use tachyon_core::types::DownloadState;
use tachyon_store::{
    KvStore, ProtectedSnapshot, RecoveryError, RecoveryManager, TaskSnapshot,
};

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
            .map_err(map_recovery_error)
    }

    /// 撤销删除等显式恢复:清 tombstone 后写入
    pub fn restore_snapshot(&self, snapshot: &TaskSnapshot) -> Result<(), AppError> {
        self.manager
            .restore_task_snapshot(snapshot)
            .map_err(map_recovery_error)
    }

    pub fn load_snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, AppError> {
        self.manager
            .load_task_snapshot(task_id)
            .map_err(map_recovery_error)
    }

    pub fn load_recoverable(&self) -> Result<Vec<TaskSnapshot>, AppError> {
        let (tasks, _corrupt, _unsupported) = self.load_recoverable_with_warnings()?;
        Ok(tasks)
    }

    /// 加载可恢复任务,同时返回损坏 key 与 future schema 保护项。
    ///
    /// 与 `load_recoverable` 的区别在于暴露 corrupt_keys / unsupported_schema,
    /// 供调用方(如 Tauri setup 钩子)向 UI 广播恢复告警。
    /// 单个损坏 JSON 或 future schema 不会阻断其他任务的恢复。
    pub fn load_recoverable_with_warnings(
        &self,
    ) -> Result<(Vec<TaskSnapshot>, Vec<String>, Vec<ProtectedSnapshot>), AppError> {
        let result = self
            .manager
            .recover_pending_snapshots()
            .map_err(map_recovery_error)?;
        if !result.corrupt_keys.is_empty() {
            tracing::warn!(
                count = result.corrupt_keys.len(),
                keys = ?result.corrupt_keys,
                "部分任务快照损坏,已跳过"
            );
        }
        if !result.unsupported_schema.is_empty() {
            tracing::warn!(
                count = result.unsupported_schema.len(),
                items = ?result.unsupported_schema,
                "检测到需要升级客户端的 future schema 快照"
            );
        }
        Ok((
            result.tasks,
            result.corrupt_keys,
            result.unsupported_schema,
        ))
    }

    /// 加载所有任务快照(含终态任务),用于备份导出
    ///
    /// 返回任务快照列表、损坏 key 列表与 future schema 保护项;
    /// 损坏/future 记录不阻断正常记录的导出。
    pub fn load_all(
        &self,
    ) -> Result<(Vec<TaskSnapshot>, Vec<String>, Vec<ProtectedSnapshot>), AppError> {
        let result = self
            .manager
            .load_all_task_snapshots()
            .map_err(map_recovery_error)?;
        if !result.corrupt_keys.is_empty() {
            tracing::warn!(
                count = result.corrupt_keys.len(),
                keys = ?result.corrupt_keys,
                "备份导出时发现损坏快照"
            );
        }
        if !result.unsupported_schema.is_empty() {
            tracing::warn!(
                count = result.unsupported_schema.len(),
                items = ?result.unsupported_schema,
                "备份导出时发现 future schema 快照"
            );
        }
        Ok((
            result.tasks,
            result.corrupt_keys,
            result.unsupported_schema,
        ))
    }

    /// 删除任务快照(用于完成/取消/失败后的清理)
    ///
    /// 清理后恢复时不再扫描该任务,减少启动恢复开销。
    pub fn remove_snapshot(&self, task_id: &str) -> Result<bool, AppError> {
        self.manager
            .remove_task(task_id)
            .map_err(map_recovery_error)
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
            .map_err(map_recovery_error)
    }
}

/// 将 store 层 fail-closed 错误映射为可模式匹配的 AppError 变体。
fn map_recovery_error(error: RecoveryError) -> AppError {
    match error {
        RecoveryError::Unsupported(protected) => AppError::UpgradeRequired {
            found_version: protected.found_version,
            supported_version: protected.supported_version,
        },
        RecoveryError::InvalidData { key } => AppError::InvalidSnapshot { key },
        RecoveryError::Io(error) => AppError::Io(error),
        // S-02b reservation 是 store 内进程保护,app 层映射为可操作 Io/Config 类错误。
        RecoveryError::ReservationActive => AppError::Config(
            "任务命名空间当前被独占操作占用,请稍后重试".into(),
        ),
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
        mirror_urls: snapshot.mirror_urls.clone(),
    }
}

pub fn normalize_recovered_status(status: DownloadState) -> DownloadState {
    match status {
        DownloadState::Downloading | DownloadState::Verifying => DownloadState::Pending,
        other => other,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn task_info_to_snapshot(
    task: &TaskInfo,
    save_path: String,
    fragment_size: u64,
    completed_fragments: Vec<u32>,
    partial_fragments: HashMap<u32, u64>,
    etag: Option<String>,
    last_modified: Option<String>,
    supports_range: bool,
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
        supports_range,
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
        mirror_urls: task.mirror_urls.clone(),
    }
}

/// 将磁盘快照中的续传/校验字段合并进内存 TaskInfo 构建的 snapshot。
///
/// **不覆盖** `retry_count`：内存 `TaskInfo.retry_count` 是权威源
///（由 chunk_reader 消费 `FragmentProgress::Retry` 累加，并可能已 checkpoint）；
/// 若用磁盘旧值覆盖，会在 `persist_task_snapshot` / `persist_task_state` 时
/// 把运行期累加的重试次数抹回落盘前的值。
///
/// 合并字段：fragment_size、completed/partial_fragments、etag、last_modified、
/// supports_range、revision。`fail_reason` 由调用方单独处理（两条路径语义不同）。
pub fn merge_disk_progress_into_snapshot(snapshot: &mut TaskSnapshot, existing: &TaskSnapshot) {
    snapshot.fragment_size = existing.fragment_size;
    snapshot.completed_fragments = existing.completed_fragments.clone();
    snapshot.partial_fragments = existing.partial_fragments.clone();
    snapshot.etag = existing.etag.clone();
    snapshot.last_modified = existing.last_modified.clone();
    snapshot.supports_range = existing.supports_range;
    // 审计 H-05: full-save 必须携带磁盘 revision,否则 CAS 会把 0 当旧写拒绝/错序
    snapshot.revision = existing.revision;
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
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
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
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };

        store.save_snapshot(&snapshot).unwrap();
        let loaded = store.load_recoverable().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "task-1");
    }

    /// 内存 TaskInfo.retry_count 在与磁盘快照合并时保持权威，不被 existing 覆盖。
    #[test]
    fn test_merge_disk_progress_preserves_memory_retry_count() {
        let mut from_memory = TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "merge-retry".to_string(),
            url: "https://example.com/m.bin".to_string(),
            save_path: "/downloads/m.bin".to_string(),
            file_name: "m.bin".to_string(),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![],
            partial_fragments: HashMap::new(),
            total_fragments: 2,
            fragment_size: 0,
            status: DownloadState::Downloading,
            etag: None,
            last_modified: None,
            content_length: Some(1024),
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 3, // 内存权威：运行期已累加
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };
        let existing = TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 5,
            id: "merge-retry".to_string(),
            url: "https://example.com/m.bin".to_string(),
            save_path: "/downloads/m.bin".to_string(),
            file_name: "m.bin".to_string(),
            file_size: Some(1024),
            downloaded: 256,
            completed_fragments: vec![0],
            partial_fragments: HashMap::from([(1, 128)]),
            total_fragments: 2,
            fragment_size: 512,
            status: DownloadState::Paused,
            etag: Some("\"abc\"".to_string()),
            last_modified: Some("Wed, 01 Jan 2020 00:00:00 GMT".to_string()),
            content_length: Some(1024),
            supports_range: false,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:00Z".to_string(),
            fail_reason: Some("old".to_string()),
            retry_count: 1, // 磁盘旧值，不得覆盖内存 3
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };

        merge_disk_progress_into_snapshot(&mut from_memory, &existing);

        assert_eq!(
            from_memory.retry_count, 3,
            "合并后 retry_count 必须保留内存权威值 3，不能被磁盘 1 覆盖"
        );
        assert_eq!(from_memory.fragment_size, 512);
        assert_eq!(from_memory.completed_fragments, vec![0]);
        assert_eq!(from_memory.partial_fragments.get(&1), Some(&128));
        assert_eq!(from_memory.etag.as_deref(), Some("\"abc\""));
        assert_eq!(
            from_memory.last_modified.as_deref(),
            Some("Wed, 01 Jan 2020 00:00:00 GMT")
        );
        assert!(!from_memory.supports_range);
        assert_eq!(from_memory.revision, 5);
        // fail_reason 不由 merge helper 处理
        assert_eq!(from_memory.fail_reason, None);
    }

    /// 非零 retry_count 经 snapshot ↔ TaskInfo 往返保持(A-13 聚合后真实语义)。
    #[test]
    fn test_snapshot_round_trip_preserves_nonzero_retry_count() {
        let temp = tempfile::tempdir().unwrap();
        let store = TaskStore::open(temp.path()).unwrap();
        let snapshot = TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "retry-task".to_string(),
            url: "https://example.com/retry.bin".to_string(),
            save_path: "/downloads/retry.bin".to_string(),
            file_name: "retry.bin".to_string(),
            file_size: Some(2048),
            downloaded: 512,
            completed_fragments: vec![0],
            partial_fragments: HashMap::new(),
            total_fragments: 4,
            fragment_size: 512,
            status: DownloadState::Paused,
            etag: None,
            last_modified: None,
            content_length: Some(2048),
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 7,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };

        store.save_snapshot(&snapshot).unwrap();
        let loaded = store
            .load_snapshot("retry-task")
            .unwrap()
            .expect("快照应存在");
        assert_eq!(loaded.retry_count, 7, "快照往返应保留非零 retry_count");

        let task = snapshot_to_task_info(&loaded);
        assert_eq!(
            task.retry_count, 7,
            "snapshot→TaskInfo 应保留非零 retry_count"
        );

        let back = task_info_to_snapshot(
            &task,
            task.save_path.clone(),
            loaded.fragment_size,
            loaded.completed_fragments.clone(),
            loaded.partial_fragments.clone(),
            loaded.etag.clone(),
            loaded.last_modified.clone(),
            loaded.supports_range,
        );
        assert_eq!(
            back.retry_count, 7,
            "TaskInfo→snapshot 应保留非零 retry_count"
        );
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
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:00Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };
        store.save_snapshot(&good).unwrap();

        // 直接写入损坏 JSON 文件,模拟磁盘损坏
        let corrupt_path = temp.path().join("task_corrupt.json");
        std::fs::write(&corrupt_path, "{ this is not valid json !!!").unwrap();

        let (tasks, corrupt_keys, _unsupported) = store.load_recoverable_with_warnings().unwrap();
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
            mirror_urls: None,
        };

        let snapshot = task_info_to_snapshot(
            &task,
            "/downloads/file.bin".to_string(),
            256,
            vec![],
            HashMap::new(),
            Some("\"abc\"".to_string()),
            None,
            true,
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
            mirror_urls: None,
        };

        let snapshot = task_info_to_snapshot(
            &task,
            "/downloads/file.bin".to_string(),
            256,
            vec![],
            HashMap::new(),
            None,
            None,
            true,
        );
        assert_eq!(
            snapshot.tags,
            vec!["model".to_string(), "important".to_string()]
        );

        let recovered = snapshot_to_task_info(&snapshot);
        assert_eq!(recovered.tags, task.tags);
    }

    #[test]
    fn test_snapshot_to_task_info_preserves_mirror_urls() {
        let mut snapshot = TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "task-mirrors".to_string(),
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
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: Some(vec![
                "https://m1.example.com/file.bin".to_string(),
                "https://m2.example.com/file.bin".to_string(),
            ]),
        };

        let task = snapshot_to_task_info(&snapshot);
        assert_eq!(
            task.mirror_urls,
            Some(vec![
                "https://m1.example.com/file.bin".to_string(),
                "https://m2.example.com/file.bin".to_string(),
            ])
        );

        // 往返:TaskInfo -> Snapshot 同样保留
        let back = task_info_to_snapshot(
            &task,
            snapshot.save_path.clone(),
            snapshot.fragment_size,
            snapshot.completed_fragments.clone(),
            snapshot.partial_fragments.clone(),
            None,
            None,
            true,
        );
        assert_eq!(back.mirror_urls, task.mirror_urls);

        // 缺字段默认 None
        snapshot.mirror_urls = None;
        let task_none = snapshot_to_task_info(&snapshot);
        assert!(task_none.mirror_urls.is_none());
    }

    /// S-02a2: store `Unsupported` 必须映射为可模式匹配的 `AppError::UpgradeRequired`。
    #[test]
    fn unsupported_store_error_maps_to_upgrade_required() {
        let temp = tempfile::tempdir().unwrap();
        let found_version = tachyon_store::SNAPSHOT_SCHEMA_VERSION + 1;
        let future_path = temp.path().join("task_future.json");
        let future_raw = format!(
            r#"{{"schemaVersion":{found_version},"id":"future","url":"https://example.com/f.bin","fileName":"f.bin","downloaded":0,"status":"downloading","createdAt":"2026-05-29T00:00:00Z","updatedAt":"2026-05-29T00:00:00Z"}}"#
        );
        std::fs::write(&future_path, future_raw.as_bytes()).unwrap();

        let store = TaskStore::open(temp.path()).unwrap();
        let err = store
            .load_snapshot("future")
            .expect_err("future schema 必须 fail-closed 为 UpgradeRequired");

        match err {
            AppError::UpgradeRequired {
                found_version: found,
                supported_version: supported,
            } => {
                assert_eq!(found, found_version);
                assert_eq!(supported, tachyon_store::SNAPSHOT_SCHEMA_VERSION);
            }
            other => panic!("expected AppError::UpgradeRequired, got {other:?}"),
        }
    }

    /// S-02a2: `InvalidData` 必须映射为可区分的 `InvalidSnapshot`，不得伪装成 UpgradeRequired 或 Io。
    #[test]
    fn invalid_data_maps_to_invalid_snapshot_not_upgrade_or_io() {
        let temp = tempfile::tempdir().unwrap();
        let bad_path = temp.path().join("task_bad.json");
        std::fs::write(&bad_path, "{ this is not valid json !!!").unwrap();

        let store = TaskStore::open(temp.path()).unwrap();
        let err = store
            .load_snapshot("bad")
            .expect_err("invalid JSON 必须 fail-closed 为 InvalidSnapshot");

        match &err {
            AppError::InvalidSnapshot { key } => {
                assert!(
                    key.contains("bad"),
                    "InvalidSnapshot 应携带可定位 key, got {key}"
                );
            }
            AppError::UpgradeRequired { .. } => {
                panic!("invalid data 不得映射为 UpgradeRequired")
            }
            AppError::Io(_) => panic!("invalid data 不得映射为 Io"),
            other => panic!("expected AppError::InvalidSnapshot, got {other:?}"),
        }

        // 与 UpgradeRequired 保持可区分（同路径不同输入产生不同变体）
        let found_version = tachyon_store::SNAPSHOT_SCHEMA_VERSION + 1;
        let future_path = temp.path().join("task_future.json");
        let future_raw = format!(
            r#"{{"schemaVersion":{found_version},"id":"future","url":"https://example.com/f.bin","fileName":"f.bin","downloaded":0,"status":"downloading","createdAt":"2026-05-29T00:00:00Z","updatedAt":"2026-05-29T00:00:00Z"}}"#
        );
        std::fs::write(&future_path, future_raw.as_bytes()).unwrap();
        let upgrade_err = store.load_snapshot("future").expect_err("future → UpgradeRequired");
        assert!(
            matches!(upgrade_err, AppError::UpgradeRequired { .. }),
            "future schema 必须仍为 UpgradeRequired: {upgrade_err:?}"
        );
        assert!(
            !matches!(err, AppError::UpgradeRequired { .. }),
            "InvalidSnapshot 与 UpgradeRequired 必须可区分"
        );
    }

    /// S-02a2: startup facade 恢复合法任务，future 进入 upgrade notice，不得混入 corrupt 或静默丢弃。
    #[test]
    fn startup_recovery_surfaces_future_as_upgrade_notice_not_corrupt() {
        let temp = tempfile::tempdir().unwrap();
        let store = TaskStore::open(temp.path()).unwrap();

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
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:00Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };
        store.save_snapshot(&good).unwrap();

        let found_version = tachyon_store::SNAPSHOT_SCHEMA_VERSION + 1;
        let future_path = temp.path().join("task_future.json");
        let future_raw = format!(
            r#"{{"schemaVersion":{found_version},"id":"future","url":"https://example.com/f.bin","fileName":"f.bin","downloaded":0,"status":"downloading","createdAt":"2026-05-29T00:00:00Z","updatedAt":"2026-05-29T00:00:00Z"}}"#
        );
        std::fs::write(&future_path, future_raw.as_bytes()).unwrap();
        let future_raw_before = std::fs::read(&future_path).unwrap();

        let corrupt_path = temp.path().join("task_corrupt.json");
        std::fs::write(&corrupt_path, "{ this is not valid json !!!").unwrap();

        let (tasks, corrupt_keys, unsupported_schema) = store
            .load_recoverable_with_warnings()
            .expect("batch recovery 不得因 future/corrupt 整批失败");

        assert_eq!(tasks.len(), 1, "合法任务必须继续恢复");
        assert_eq!(tasks[0].id, "good");

        assert_eq!(corrupt_keys.len(), 1, "损坏 key 仍单独暴露");
        assert!(
            corrupt_keys.iter().any(|k| k.contains("corrupt")),
            "corrupt_keys 应包含损坏标识: {corrupt_keys:?}"
        );
        assert!(
            !corrupt_keys.iter().any(|k| k.contains("future")),
            "future 不得混入 corrupt_keys: {corrupt_keys:?}"
        );

        assert_eq!(
            unsupported_schema.len(),
            1,
            "future 必须作为显式 upgrade notice 上报"
        );
        assert_eq!(unsupported_schema[0].key, "task_future");
        assert_eq!(unsupported_schema[0].found_version, found_version);
        assert_eq!(
            unsupported_schema[0].supported_version,
            tachyon_store::SNAPSHOT_SCHEMA_VERSION
        );
        assert_eq!(
            std::fs::read(&future_path).unwrap(),
            future_raw_before,
            "future raw bytes 必须保持不变"
        );
    }
}
