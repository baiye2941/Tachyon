//! 任务应用服务测试
//!
//! 覆盖 task_service.rs 中的纯函数(delete_save_path 解析、删除候选生成、
//! 路径校验、符号链接防御)与 TaskService 的状态机方法(pause/resume/cancel/delete)。
//!
//! TaskService 依赖 TaskRepository(内存 DashMap)+ TaskStore(临时目录)+ AppConfig,
//! 不启动 Tauri runtime,纯 async 测试。

#![cfg(test)]

use std::path::PathBuf;
use std::sync::Arc;

use tachyon_core::config::AppConfig;
use tachyon_core::types::DownloadState;
use tempfile::TempDir;
use tokio::sync::Mutex;

use super::*;
use crate::commands::TaskInfo;
use crate::repository::TaskRepository;
use crate::task_store::TaskStore;

// ── 纯函数:resolve_delete_save_path ─────────────────────────────────────────

#[test]
fn test_resolve_delete_save_path_prefers_task_save_path() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("model.bin");
    std::fs::write(&file_path, b"data").unwrap();

    let task = make_task(
        "task-1",
        "model.bin",
        file_path.to_string_lossy().to_string(),
    );
    let resolved = resolve_delete_save_path(&task, None);
    assert_eq!(
        resolved.as_deref(),
        Some(file_path.to_string_lossy().as_ref())
    );
}

#[test]
fn test_resolve_delete_save_path_falls_back_to_snapshot() {
    // task.save_path 为空,snapshot 有 save_path
    let task = make_task("task-2", "data.bin", String::new());
    let snapshot = TaskSnapshot {
        save_path: "/tmp/data.bin".to_string(),
        ..empty_snapshot()
    };
    let resolved = resolve_delete_save_path(&task, Some(&snapshot));
    assert_eq!(resolved.as_deref(), Some("/tmp/data.bin"));
}

#[test]
fn test_resolve_delete_save_path_uses_task_path_join_when_no_snapshot() {
    // task.save_path 是目录,file_name 拼接
    let task = make_task("task-3", "weights.safetensors", "/downloads".to_string());
    let resolved = resolve_delete_save_path(&task, None);
    // save_path 是目录(不是文件)且 file_name 匹配 → 走 join 分支
    assert!(resolved.is_some());
    let path = resolved.unwrap();
    assert!(path.ends_with("weights.safetensors"));
}

// ── 纯函数:local_delete_candidates ────────────────────────────────────────────

#[test]
fn test_local_delete_candidates_includes_suffixes() {
    let base = PathBuf::from("dl").join("model.bin");
    let candidates = local_delete_candidates("task-1", &base);
    // 用 PathBuf 比较避免跨平台路径分隔符差异
    let has = |expected: PathBuf| candidates.contains(&expected);

    // 主文件
    assert!(has(base.clone()), "应包含主文件");
    // .part/.tmp/.download 后缀
    assert!(
        has({
            let mut p = base.clone();
            p.set_extension("bin.part");
            p
        }) || has(PathBuf::from(format!("{}.part", base.to_string_lossy()))),
        "应包含 .part 后缀"
    );
    // task_id 前缀的临时文件(父目录下)
    let parent = base.parent().unwrap();
    assert!(
        has(parent.join(".tachyon-task-1.part")),
        "应包含 .tachyon-task-1.part,candidates: {candidates:?}"
    );
    assert!(has(parent.join("task-1.part")), "应包含 task-1.part");
}

#[test]
fn test_local_delete_candidates_no_duplicates() {
    let candidates = local_delete_candidates("task-1", &PathBuf::from("/dl/model.bin"));
    let mut seen = std::collections::HashSet::new();
    for c in &candidates {
        assert!(seen.insert(c.clone()), "发现重复候选: {}", c.display());
    }
}

#[test]
fn test_push_unique_path_deduplicates() {
    let mut candidates = Vec::new();
    let p = PathBuf::from("/a/b.bin");
    push_unique_path(&mut candidates, p.clone());
    push_unique_path(&mut candidates, p.clone());
    assert_eq!(candidates.len(), 1, "重复路径应被去重");
    push_unique_path(&mut candidates, PathBuf::from("/a/c.bin"));
    assert_eq!(candidates.len(), 2);
}

// ── 纯函数:validate_local_delete_path ─────────────────────────────────────────

#[test]
fn test_validate_local_delete_path_rejects_relative() {
    let config = AppConfig::default();
    let result = validate_local_delete_path(&config, &PathBuf::from("relative/path.bin"));
    assert!(result.is_err(), "相对路径应被拒绝");
}

#[test]
fn test_validate_local_delete_path_rejects_parent_dir() {
    let config = AppConfig::default();
    let result = validate_local_delete_path(&config, &PathBuf::from("/dl/../etc/passwd"));
    assert!(result.is_err(), "含 .. 的路径应被拒绝(路径遍历防御)");
}

#[test]
fn test_validate_local_delete_path_rejects_empty() {
    let config = AppConfig::default();
    let result = validate_local_delete_path(&config, &PathBuf::from(""));
    assert!(result.is_err(), "空路径应被拒绝");
}

#[test]
fn test_validate_local_delete_path_rejects_unauthorized_dir() {
    let mut config = AppConfig::default();
    config.download.authorized_dirs = vec!["/authorized".to_string()];
    let result = validate_local_delete_path(&config, &PathBuf::from("/unauthorized/file.bin"));
    assert!(result.is_err(), "未授权目录应被拒绝");
}

// ── 纯函数:validate_delete_candidate(符号链接防御) ──────────────────────────

#[test]
fn test_validate_delete_candidate_rejects_symlink() {
    // Windows 创建符号链接需管理员权限,测试仅 Unix 验证
    #[cfg(unix)]
    {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.bin");
        std::fs::write(&target, b"data").unwrap();
        let link = dir.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let result = validate_delete_candidate(&link);
        assert!(result.is_err(), "符号链接应被拒绝");
    }
}

#[test]
fn test_validate_delete_candidate_rejects_directory() {
    let dir = TempDir::new().unwrap();
    let result = validate_delete_candidate(dir.path());
    assert!(result.is_err(), "目录应被拒绝(仅允许删除文件)");
}

#[test]
fn test_validate_delete_candidate_accepts_regular_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("normal.bin");
    std::fs::write(&file, b"data").unwrap();
    let result = validate_delete_candidate(&file);
    assert!(result.is_ok(), "普通文件应被接受");
}

// ── 纯函数:delete_local_file_candidates(集成) ──────────────────────────────

#[test]
fn test_delete_local_file_candidates_removes_existing_files() {
    let dir = TempDir::new().unwrap();
    let main_file = dir.path().join("model.bin");
    let part_file = dir.path().join("model.bin.part");
    std::fs::write(&main_file, b"main").unwrap();
    std::fs::write(&part_file, b"part").unwrap();

    let mut config = AppConfig::default();
    config.download.authorized_dirs = vec![dir.path().to_string_lossy().to_string()];

    let result = delete_local_file_candidates(&config, "task-1", &main_file.to_string_lossy());
    assert!(result.is_ok(), "删除应成功");
    assert!(!main_file.exists(), "主文件应被删除");
    assert!(!part_file.exists(), ".part 文件应被删除");
}

#[test]
fn test_delete_local_file_candidates_skips_nonexistent() {
    let dir = TempDir::new().unwrap();
    let main_file = dir.path().join("nonexistent.bin");

    let mut config = AppConfig::default();
    config.download.authorized_dirs = vec![dir.path().to_string_lossy().to_string()];

    let result = delete_local_file_candidates(&config, "task-1", &main_file.to_string_lossy());
    assert!(result.is_ok(), "文件不存在时不应报错(跳过即可)");
}

// ── TaskService 状态机方法 ──────────────────────────────────────────────────

/// 构造测试用 TaskService(内存仓库 + 临时 TaskStore)
fn make_service() -> (TaskService, TempDir) {
    let dir = TempDir::new().unwrap();
    let repo = TaskRepository::new();
    let config = Arc::new(Mutex::new(AppConfig {
        download: tachyon_core::config::DownloadConfig {
            download_dir: dir.path().to_string_lossy().to_string(),
            max_concurrent_fragments: 4,
            authorized_dirs: vec![dir.path().to_string_lossy().to_string()],
            ..Default::default()
        },
        ..Default::default()
    }));
    let task_store = Arc::new(TaskStore::open(dir.path()).unwrap());
    let create_lock = Arc::new(Mutex::new(()));
    let service = TaskService::new(repo, config, task_store, create_lock);
    (service, dir)
}

#[tokio::test]
async fn test_pause_task_downloading_to_paused() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Downloading);

    service.pause_task("t1").await.unwrap();

    let task = service.get_task_detail("t1").unwrap();
    assert_eq!(task.status, DownloadState::Paused);
    assert_eq!(task.speed, 0, "暂停后 speed 应归零");
}

#[tokio::test]
async fn test_pause_task_rejects_terminal_state() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Completed);

    let result = service.pause_task("t1").await;
    assert!(result.is_err(), "Completed 状态不允许暂停");
}

#[tokio::test]
async fn test_resume_task_paused_to_downloading() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Paused);

    service.resume_task("t1").await.unwrap();

    let task = service.get_task_detail("t1").unwrap();
    assert_eq!(task.status, DownloadState::Downloading);
}

#[tokio::test]
async fn test_resume_task_rejects_downloading() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Downloading);

    let result = service.resume_task("t1").await;
    assert!(result.is_err(), "Downloading 状态不允许恢复");
}

#[tokio::test]
async fn test_cancel_task_from_downloading() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Downloading);

    service.cancel_task("t1").await.unwrap();

    let task = service.get_task_detail("t1").unwrap();
    assert_eq!(task.status, DownloadState::Cancelled);
    assert_eq!(task.speed, 0);
}

#[tokio::test]
async fn test_cancel_task_rejects_completed() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Completed);

    let result = service.cancel_task("t1").await;
    assert!(result.is_err(), "Completed 状态不允许取消");
}

#[tokio::test]
async fn test_delete_task_removes_from_repository() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Completed);

    service.delete_task("t1", false).await.unwrap();

    assert!(
        !service.task_repository.contains_key("t1"),
        "任务应已从仓库删除"
    );
}

#[tokio::test]
async fn test_delete_nonexistent_task_returns_error() {
    let (service, _dir) = make_service();
    let result = service.delete_task("nonexistent", false).await;
    assert!(result.is_err(), "删除不存在的任务应返回错误");
}

#[tokio::test]
async fn test_pause_nonexistent_task_returns_error() {
    let (service, _dir) = make_service();
    let result = service.pause_task("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_task_detail_not_found() {
    let (service, _dir) = make_service();
    let result = service.get_task_detail("nonexistent");
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_task_list_returns_all() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "a.bin", "/dl".into()));
    service
        .task_repository
        .insert("t2".to_string(), make_task("t2", "b.bin", "/dl".into()));

    let list = service.get_task_list();
    assert_eq!(list.len(), 2, "应返回 2 个任务");
}

#[tokio::test]
async fn test_update_task_status_sets_terminal_speed_zero() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    // 先设置非零 speed
    {
        let mut task = service.task_repository.get_mut("t1").unwrap();
        task.speed = 1000;
    }

    service.update_task_status("t1", DownloadState::Completed);

    let task = service.get_task_detail("t1").unwrap();
    assert_eq!(task.status, DownloadState::Completed);
    assert_eq!(task.speed, 0, "终态 speed 应归零");
}

#[tokio::test]
async fn test_update_cached_download_dir() {
    let (service, _dir) = make_service();
    service
        .update_cached_download_dir("/new/path".to_string())
        .await;
    // 缓存更新后,后续 persist_snapshot 应使用新路径(间接验证)
    // 直接读取 RwLock 验证
    let cached = service.cached_download_dir.read().await.clone();
    assert_eq!(cached, "/new/path");
}

#[tokio::test]
async fn test_create_task_success() {
    let (service, dir) = make_service();
    let url = "https://example.com/file.bin";
    let result = service.create_task(url, None, None, None, true).await;
    assert!(result.is_ok(), "创建任务应成功: {:?}", result.err());
    let creation = result.unwrap();
    assert!(!creation.task_id.is_empty());
    assert_eq!(creation.url, url);
    // Windows canonicalize 返回 \\?\ 前缀(UNC 路径),断言用 ends_with 容忍平台差异
    let dir_str = dir.path().to_string_lossy().to_string();
    assert!(
        creation.download_dir.ends_with(&dir_str) || dir_str.ends_with(&creation.download_dir),
        "download_dir 应匹配临时目录,实际: {} vs {}",
        creation.download_dir,
        dir_str
    );
    // 任务应在仓库中
    assert!(service.task_repository.contains_key(&creation.task_id));
    let task = service.get_task_detail(&creation.task_id).unwrap();
    assert_eq!(task.status, DownloadState::Pending);
    assert_eq!(task.file_name, "file.bin");
}

#[tokio::test]
async fn test_create_task_dedup_same_url() {
    let (service, _dir) = make_service();
    let url = "https://example.com/file.bin";
    service
        .create_task(url, None, None, None, true)
        .await
        .unwrap();

    // 同 URL 再次创建应失败(去重)
    let result = service.create_task(url, None, None, None, true).await;
    assert!(result.is_err(), "相同 URL 应去重拒绝");
    let err = result.unwrap_err();
    assert!(
        matches!(err, AppError::TaskAlreadyExists(_)),
        "应为 TaskAlreadyExists 错误"
    );
}

#[tokio::test]
async fn test_create_task_invalid_url() {
    let (service, _dir) = make_service();
    let result = service
        .create_task("not-a-url", None, None, None, true)
        .await;
    assert!(result.is_err(), "非法 URL 应被拒绝");
}

#[tokio::test]
async fn test_create_task_with_preferred_filename() {
    let (service, _dir) = make_service();
    let url = "https://example.com/file.bin";
    let result = service
        .create_task(url, None, None, Some("custom-name.bin"), true)
        .await;
    assert!(result.is_ok());
    let creation = result.unwrap();
    assert_eq!(
        creation.preferred_file_name.as_deref(),
        Some("custom-name.bin"),
        "preferred_file_name 应为 sanitize 后的用户输入"
    );
    let task = service.get_task_detail(&creation.task_id).unwrap();
    assert_eq!(task.file_name, "custom-name.bin");
}

#[tokio::test]
async fn test_create_task_sanitizes_filename() {
    let (service, _dir) = make_service();
    let url = "https://example.com/file.bin";
    // 含路径遍历字符的文件名应被 sanitize
    let result = service
        .create_task(url, None, None, Some("../../etc/passwd"), true)
        .await;
    assert!(result.is_ok());
    let creation = result.unwrap();
    let name = creation.preferred_file_name.unwrap();
    assert!(!name.contains(".."), "sanitize 后不应含路径遍历: {name}");
    assert!(!name.contains('/'), "sanitize 后不应含路径分隔符: {name}");
    assert!(!name.is_empty(), "sanitize 不应产生空名");
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────────

fn make_task(id: &str, file_name: &str, save_path: String) -> TaskInfo {
    TaskInfo {
        id: id.to_string(),
        url: format!("https://example.com/{file_name}"),
        file_name: file_name.to_string(),
        file_size: None,
        downloaded: 0,
        speed: 0,
        status: DownloadState::Pending,
        progress: 0.0,
        fragments_total: 0,
        fragments_done: 0,
        active_concurrency: 0,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        save_path,
        error_reason: None,
        retry_count: 0,
        hf_meta: None,
    }
}

fn empty_snapshot() -> TaskSnapshot {
    TaskSnapshot {
        schema_version: 0,
        id: String::new(),
        url: String::new(),
        save_path: String::new(),
        file_name: String::new(),
        file_size: None,
        downloaded: 0,
        completed_fragments: vec![],
        partial_fragments: std::collections::HashMap::new(),
        total_fragments: 0,
        fragment_size: 0,
        status: DownloadState::Pending,
        etag: None,
        last_modified: None,
        content_length: None,
        created_at: String::new(),
        updated_at: String::new(),
        fail_reason: None,
        retry_count: 0,
        hf_meta: None,
    }
}
