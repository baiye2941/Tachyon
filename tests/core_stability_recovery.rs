mod common;

use std::path::PathBuf;
use tachyon_core::DownloadState;
use tachyon_store::{KvStore, RecoveryManager, TaskSnapshot};
use tempfile::TempDir;

fn temp_store_dir() -> PathBuf {
    let dir = TempDir::new().unwrap();
    dir.keep()
}

fn make_snapshot(id: &str, url: &str, status: DownloadState) -> TaskSnapshot {
    TaskSnapshot {
        id: id.to_string(),
        url: url.to_string(),
        save_path: "/tmp/test.bin".to_string(),
        file_name: "test.bin".to_string(),
        file_size: None,
        downloaded: 0,
        completed_fragments: vec![],
        total_fragments: 0,
        fragment_size: 0,
        status,
        etag: None,
        last_modified: None,
        content_length: None,
        created_at: "2026-05-29T00:00:00Z".to_string(),
        updated_at: "2026-05-29T00:00:00Z".to_string(),
        fail_reason: None,
        retry_count: 0,
    }
}

fn normalize_recovered_status(status: DownloadState) -> DownloadState {
    match status {
        DownloadState::Downloading | DownloadState::Verifying => DownloadState::Pending,
        other => other,
    }
}

#[test]
fn recovery_roundtrip_downloading_recoverable() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    let snapshot = make_snapshot(
        "task-1",
        "https://example.com/file.bin",
        DownloadState::Downloading,
    );
    manager.save_task_snapshot(&snapshot).unwrap();

    let loaded = manager.recover_pending_snapshots().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, "task-1");
    assert_eq!(loaded[0].status, DownloadState::Downloading);

    let normalized = normalize_recovered_status(loaded[0].status);
    assert_eq!(normalized, DownloadState::Pending);
}

#[test]
fn recovery_roundtrip_paused_recoverable() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    let snapshot = make_snapshot(
        "task-2",
        "https://example.com/file2.bin",
        DownloadState::Paused,
    );
    manager.save_task_snapshot(&snapshot).unwrap();

    let loaded = manager.recover_pending_snapshots().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].status, DownloadState::Paused);
}

#[test]
fn recovery_roundtrip_failed_not_recoverable() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    let mut snapshot = make_snapshot(
        "task-3",
        "https://example.com/file3.bin",
        DownloadState::Failed,
    );
    snapshot.fail_reason = Some("网络超时".to_string());
    manager.save_task_snapshot(&snapshot).unwrap();

    let loaded = manager.recover_pending_snapshots().unwrap();
    assert_eq!(loaded.len(), 0, "失败任务不应被自动恢复");
}

#[test]
fn recovery_roundtrip_completed_not_recoverable() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    let snapshot = make_snapshot(
        "task-4",
        "https://example.com/file4.bin",
        DownloadState::Completed,
    );
    manager.save_task_snapshot(&snapshot).unwrap();

    let loaded = manager.recover_pending_snapshots().unwrap();
    assert_eq!(loaded.len(), 0, "已完成任务不应被恢复");
}

#[test]
fn recovery_roundtrip_cancelled_not_recoverable() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    let snapshot = make_snapshot(
        "task-5",
        "https://example.com/file5.bin",
        DownloadState::Cancelled,
    );
    manager.save_task_snapshot(&snapshot).unwrap();

    let loaded = manager.recover_pending_snapshots().unwrap();
    assert_eq!(loaded.len(), 0, "已取消任务不应被恢复");
}

#[test]
fn recovery_multiple_snapshots() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    for i in 0..5 {
        let snapshot = make_snapshot(
            &format!("task-{}", i),
            &format!("https://example.com/file{}.bin", i),
            DownloadState::Downloading,
        );
        manager.save_task_snapshot(&snapshot).unwrap();
    }

    let loaded = manager.recover_pending_snapshots().unwrap();
    assert_eq!(loaded.len(), 5);
}

#[test]
fn recovery_overwrite_same_task_id() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    let mut snapshot = make_snapshot(
        "task-1",
        "https://example.com/file.bin",
        DownloadState::Downloading,
    );
    manager.save_task_snapshot(&snapshot).unwrap();

    snapshot.downloaded = 1024;
    snapshot.status = DownloadState::Paused;
    manager.save_task_snapshot(&snapshot).unwrap();

    let loaded = manager.recover_pending_snapshots().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].downloaded, 1024);
    assert_eq!(loaded[0].status, DownloadState::Paused);
}

#[test]
fn snapshot_preserves_metadata() {
    let snapshot = TaskSnapshot {
        id: "task-1".to_string(),
        url: "https://example.com/file.bin".to_string(),
        save_path: "/tmp/test.bin".to_string(),
        file_name: "test.bin".to_string(),
        file_size: Some(1024),
        downloaded: 512,
        completed_fragments: vec![0, 1],
        total_fragments: 4,
        fragment_size: 256,
        status: DownloadState::Paused,
        etag: Some("abc123".to_string()),
        last_modified: Some("2026-01-01T00:00:00Z".to_string()),
        content_length: Some(1024),
        created_at: "2026-05-29T00:00:00Z".to_string(),
        updated_at: "2026-05-29T00:00:01Z".to_string(),
        fail_reason: None,
        retry_count: 0,
    };

    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);
    manager.save_task_snapshot(&snapshot).unwrap();

    let loaded = manager.recover_pending_snapshots().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, "task-1");
    assert_eq!(loaded[0].file_size, Some(1024));
    assert_eq!(loaded[0].downloaded, 512);
    assert_eq!(loaded[0].total_fragments, 4);
    assert_eq!(loaded[0].completed_fragments.len(), 2);
    assert_eq!(loaded[0].etag.as_deref(), Some("abc123"));
}

#[test]
fn normalize_status_mapping() {
    assert_eq!(
        normalize_recovered_status(DownloadState::Pending),
        DownloadState::Pending
    );
    assert_eq!(
        normalize_recovered_status(DownloadState::Downloading),
        DownloadState::Pending
    );
    assert_eq!(
        normalize_recovered_status(DownloadState::Verifying),
        DownloadState::Pending
    );
    assert_eq!(
        normalize_recovered_status(DownloadState::Paused),
        DownloadState::Paused
    );
    assert_eq!(
        normalize_recovered_status(DownloadState::Completed),
        DownloadState::Completed
    );
    assert_eq!(
        normalize_recovered_status(DownloadState::Failed),
        DownloadState::Failed
    );
    assert_eq!(
        normalize_recovered_status(DownloadState::Cancelled),
        DownloadState::Cancelled
    );
}

#[test]
fn all_snapshots_loadable_via_all() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    for (i, status) in [
        DownloadState::Downloading,
        DownloadState::Paused,
        DownloadState::Failed,
        DownloadState::Completed,
        DownloadState::Cancelled,
    ]
    .iter()
    .enumerate()
    {
        let snapshot = make_snapshot(
            &format!("task-{}", i),
            &format!("https://example.com/file{}.bin", i),
            *status,
        );
        manager.save_task_snapshot(&snapshot).unwrap();
    }

    let all = manager.load_all_task_snapshots().unwrap();
    assert_eq!(all.len(), 5, "所有快照都应可通过 load_all 加载");

    let recoverable = manager.recover_pending_snapshots().unwrap();
    assert_eq!(recoverable.len(), 2, "只有 Downloading 和 Paused 可恢复");
}

#[test]
fn failed_snapshot_preserves_fail_reason() {
    let store_dir = temp_store_dir();
    let kv = KvStore::open(&store_dir).unwrap();
    let manager = RecoveryManager::new(kv);

    let mut snapshot = make_snapshot(
        "task-1",
        "https://example.com/file.bin",
        DownloadState::Failed,
    );
    snapshot.fail_reason = Some("网络错误".to_string());
    manager.save_task_snapshot(&snapshot).unwrap();

    let loaded = manager.load_task_snapshot("task-1").unwrap().unwrap();
    assert_eq!(loaded.status, DownloadState::Failed);
    assert_eq!(loaded.fail_reason, Some("网络错误".to_string()));
}
