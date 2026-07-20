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
use std::time::{Duration, Instant};

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
    #[cfg(not(unix))]
    {}
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
    // Windows 上路径可能同时出现:
    // - canonicalize 的 \\?\ 前缀
    // - 8.3 短路径(C:\Users\RUNNER~1\...)
    // 字符串 ends_with 对不上。两边都 canonicalize 后再比 PathBuf。
    let expected = std::fs::canonicalize(dir.path()).expect("临时目录应可 canonicalize");
    let actual = std::fs::canonicalize(&creation.download_dir)
        .unwrap_or_else(|_| PathBuf::from(&creation.download_dir));
    assert_eq!(
        actual,
        expected,
        "download_dir 应匹配临时目录,实际: {} vs {}",
        creation.download_dir,
        dir.path().display()
    );
    // 任务应在仓库中
    assert!(service.task_repository.contains_key(&creation.task_id));
    let task = service.get_task_detail(&creation.task_id).unwrap();
    assert_eq!(task.status, DownloadState::Pending);
    assert_eq!(task.file_name, "file.bin");
}

#[tokio::test]
async fn test_create_task_stores_mirror_urls_on_task_and_snapshot() {
    // create 带 mirrors 时 TaskInfo 与落盘快照都必须保留,供 restart 多源续传
    let (service, _dir) = make_service();
    let url = "https://primary.example.com/file.bin";
    let mirrors = vec![
        "https://m1.example.com/file.bin".to_string(),
        "https://m2.example.com/file.bin".to_string(),
    ];
    let creation = service
        .create_task(url, None, Some(&mirrors), None, false)
        .await
        .expect("create_task 应成功");
    assert_eq!(creation.mirror_urls.as_ref(), Some(&mirrors));

    let task = service.get_task_detail(&creation.task_id).unwrap();
    assert_eq!(task.mirror_urls.as_ref(), Some(&mirrors));

    let snapshot = service
        .task_store
        .load_snapshot(&creation.task_id)
        .unwrap()
        .expect("初始快照应已落盘");
    assert_eq!(snapshot.mirror_urls.as_ref(), Some(&mirrors));
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

// ── 任务标签管理 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_set_task_tags_normalizes_and_persists() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));

    service
        .set_task_tags(
            "t1",
            vec![
                "  Important ".to_string(),
                "MODEL".to_string(),
                "important".to_string(),
                "a".repeat(40),
            ],
        )
        .await
        .unwrap();

    let task = service.get_task_detail("t1").unwrap();
    assert_eq!(
        task.tags,
        vec!["important".to_string(), "model".to_string(), "a".repeat(32)]
    );

    // 持久化快照应包含标签(persist_snapshot 内部为 spawn_blocking fire-and-forget,
    // 轮询等待快照落盘,避免竞态)。
    let mut snapshot = None;
    for _ in 0..50 {
        if let Ok(Some(s)) = service.task_store.load_snapshot("t1") {
            snapshot = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(snapshot.is_some(), "快照应在 1 秒内落盘");
    assert_eq!(snapshot.unwrap().tags, task.tags);
}

#[tokio::test]
async fn test_set_task_tags_limits_count() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));

    let tags: Vec<String> = (0..15).map(|i| format!("tag-{i}")).collect();
    service.set_task_tags("t1", tags).await.unwrap();

    let task = service.get_task_detail("t1").unwrap();
    assert_eq!(task.tags.len(), 10);
}

#[tokio::test]
async fn test_add_task_tag_appends_and_deduplicates() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));

    service.add_task_tag("t1", "alpha").await.unwrap();
    service.add_task_tag("t1", "ALPHA ").await.unwrap();
    service.add_task_tag("t1", "beta").await.unwrap();

    let task = service.get_task_detail("t1").unwrap();
    assert_eq!(task.tags, vec!["alpha".to_string(), "beta".to_string()]);
}

#[tokio::test]
async fn test_add_task_tag_rejects_when_limit_reached() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));

    for i in 0..10 {
        service
            .add_task_tag("t1", &format!("tag-{i}"))
            .await
            .unwrap();
    }

    let result = service.add_task_tag("t1", "overflow").await;
    assert!(result.is_err(), "超过 10 个标签时应报错");
    assert!(result.unwrap_err().to_string().contains("上限"));
}

#[tokio::test]
async fn test_remove_task_tag_removes_normalized_match() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));

    service
        .set_task_tags("t1", vec!["keep".to_string(), "remove".to_string()])
        .await
        .unwrap();
    service.remove_task_tag("t1", "REMOVE ").await.unwrap();

    let task = service.get_task_detail("t1").unwrap();
    assert_eq!(task.tags, vec!["keep".to_string()]);
}

#[tokio::test]
async fn test_tag_operations_require_existing_task() {
    let (service, _dir) = make_service();

    assert!(service.set_task_tags("missing", vec![]).await.is_err());
    assert!(service.add_task_tag("missing", "x").await.is_err());
    assert!(service.remove_task_tag("missing", "x").await.is_err());
}

// ── 任务排序与拖拽 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_task_list_sorts_by_display_order_asc() {
    let (service, _dir) = make_service();
    service.task_repository.insert(
        "t1".to_string(),
        make_task_with_order("t1", "a.bin", "/dl".into(), 100, "2026-01-01T00:00:00Z"),
    );
    service.task_repository.insert(
        "t2".to_string(),
        make_task_with_order("t2", "b.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );
    service.task_repository.insert(
        "t3".to_string(),
        make_task_with_order("t3", "c.bin", "/dl".into(), 50, "2026-01-01T00:00:00Z"),
    );

    let list = service.get_task_list();
    let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["t2", "t3", "t1"]);
}

#[tokio::test]
async fn test_get_task_list_stable_by_created_at_desc_when_order_equal() {
    let (service, _dir) = make_service();
    service.task_repository.insert(
        "t1".to_string(),
        make_task_with_order("t1", "a.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );
    service.task_repository.insert(
        "t2".to_string(),
        make_task_with_order("t2", "b.bin", "/dl".into(), 0, "2026-01-03T00:00:00Z"),
    );
    service.task_repository.insert(
        "t3".to_string(),
        make_task_with_order("t3", "c.bin", "/dl".into(), 0, "2026-01-02T00:00:00Z"),
    );

    let list = service.get_task_list();
    let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
    // 相同 display_order 时按 created_at 降序,新创建在前
    assert_eq!(ids, vec!["t2", "t3", "t1"]);
}

#[tokio::test]
async fn test_reorder_tasks_assigns_orders_by_index() {
    let (service, _dir) = make_service();
    service.task_repository.insert(
        "t1".to_string(),
        make_task_with_order("t1", "a.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );
    service.task_repository.insert(
        "t2".to_string(),
        make_task_with_order("t2", "b.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );
    service.task_repository.insert(
        "t3".to_string(),
        make_task_with_order("t3", "c.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );

    service
        .reorder_tasks(&["t3".to_string(), "t1".to_string(), "t2".to_string()])
        .await
        .unwrap();

    assert_eq!(service.get_task_detail("t3").unwrap().display_order, 0);
    assert_eq!(service.get_task_detail("t1").unwrap().display_order, 1000);
    assert_eq!(service.get_task_detail("t2").unwrap().display_order, 2000);

    let list = service.get_task_list();
    let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["t3", "t1", "t2"]);
}

#[tokio::test]
async fn test_reorder_tasks_persists_display_order() {
    let (service, _dir) = make_service();
    service.task_repository.insert(
        "t1".to_string(),
        make_task_with_order("t1", "a.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );
    // 初始持久化一个快照,使后续 load 能合并字段
    service.persist_display_order("t1").await;

    service.reorder_tasks(&["t1".to_string()]).await.unwrap();

    // 等待 fire-and-forget 持久化完成
    let mut snapshot = None;
    for _ in 0..50 {
        if let Ok(Some(s)) = service.task_store.load_snapshot("t1") {
            snapshot = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(snapshot.is_some(), "快照应在 1 秒内落盘");
    assert_eq!(snapshot.unwrap().display_order, 0);
}

#[tokio::test]
async fn test_reorder_tasks_rejects_missing_id() {
    let (service, _dir) = make_service();
    service.task_repository.insert(
        "t1".to_string(),
        make_task_with_order("t1", "a.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );

    let result = service
        .reorder_tasks(&["t1".to_string(), "missing".to_string()])
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("任务不存在"));
}

#[tokio::test]
async fn test_move_task_before_another_task() {
    let (service, _dir) = make_service();
    service.task_repository.insert(
        "t1".to_string(),
        make_task_with_order("t1", "a.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );
    service.task_repository.insert(
        "t2".to_string(),
        make_task_with_order("t2", "b.bin", "/dl".into(), 1000, "2026-01-01T00:00:00Z"),
    );
    service.task_repository.insert(
        "t3".to_string(),
        make_task_with_order("t3", "c.bin", "/dl".into(), 2000, "2026-01-01T00:00:00Z"),
    );

    service
        .move_task("t3".to_string(), Some("t1".to_string()))
        .await
        .unwrap();

    let list = service.get_task_list();
    let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["t3", "t1", "t2"]);
}

#[tokio::test]
async fn test_move_task_to_end() {
    let (service, _dir) = make_service();
    service.task_repository.insert(
        "t1".to_string(),
        make_task_with_order("t1", "a.bin", "/dl".into(), 0, "2026-01-01T00:00:00Z"),
    );
    service.task_repository.insert(
        "t2".to_string(),
        make_task_with_order("t2", "b.bin", "/dl".into(), 1000, "2026-01-01T00:00:00Z"),
    );

    service.move_task("t1".to_string(), None).await.unwrap();

    let list = service.get_task_list();
    let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["t2", "t1"]);
}

// ── 撤销操作测试 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_undo_cancel_restores_previous_status() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Downloading);

    service.cancel_task("t1").await.unwrap();
    assert_eq!(
        service.get_task_detail("t1").unwrap().status,
        DownloadState::Cancelled
    );

    let restored = service.undo_cancel_task("t1").await.unwrap();
    assert_eq!(restored, DownloadState::Downloading);
    assert_eq!(
        service.get_task_detail("t1").unwrap().status,
        DownloadState::Downloading
    );
}

#[tokio::test]
async fn test_undo_cancel_from_failed_restores_error_reason() {
    // BUG G:失败任务 cancel 会双写清除 error_reason(内存 + 快照),
    // undo_cancel 恢复 Failed 状态时必须一并恢复原始失败原因,
    // 否则前端诊断面板只剩 Failed 状态而无错误详情。
    let (service, _dir) = make_service();
    let mut task = make_task("t1", "f.bin", "/dl".into());
    task.status = DownloadState::Failed;
    task.error_reason = Some("HTTP 404".to_string());
    service.task_repository.insert("t1".to_string(), task);

    service.cancel_task("t1").await.unwrap();
    // cancel 清除 error_reason 是既有行为(双写),保持不变
    assert_eq!(service.get_task_detail("t1").unwrap().error_reason, None);

    // 等待 cancel 快照落盘(persist_snapshot 为 spawn_blocking fire-and-forget,
    // 且存储层有 revision CAS:若 undo 写入携带的 revision 落后于磁盘会被拒)。
    // 真实场景中 undo 必发生在 cancel 落盘之后,此处模拟该时序。
    let mut cancelled = false;
    for _ in 0..50 {
        if let Ok(Some(s)) = service.task_store.load_snapshot("t1")
            && s.status == DownloadState::Cancelled
        {
            cancelled = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(cancelled, "cancel 快照应在 1 秒内落盘");

    let restored = service.undo_cancel_task("t1").await.unwrap();
    assert_eq!(restored, DownloadState::Failed);
    assert_eq!(
        service
            .get_task_detail("t1")
            .unwrap()
            .error_reason
            .as_deref(),
        Some("HTTP 404"),
        "undo_cancel 应恢复 cancel 前的 error_reason"
    );

    // 持久化快照也应恢复 fail_reason(persist_snapshot 内部为 spawn_blocking
    // fire-and-forget,轮询等待落盘,避免竞态)
    let mut persisted = None;
    for _ in 0..50 {
        if let Ok(Some(s)) = service.task_store.load_snapshot("t1")
            && s.status == DownloadState::Failed
        {
            persisted = Some(s.fail_reason.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    match persisted {
        Some(reason) => assert_eq!(
            reason.as_deref(),
            Some("HTTP 404"),
            "快照应恢复 fail_reason"
        ),
        None => panic!("快照应在 1 秒内恢复为 Failed 状态"),
    }
}

#[tokio::test]
async fn test_undo_cancel_from_paused_restores_paused() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Paused);

    service.cancel_task("t1").await.unwrap();
    let restored = service.undo_cancel_task("t1").await.unwrap();
    assert_eq!(restored, DownloadState::Paused);
    assert_eq!(
        service.get_task_detail("t1").unwrap().status,
        DownloadState::Paused
    );
}

#[tokio::test]
async fn test_undo_cancel_timeout_fails() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Downloading);

    service.cancel_task("t1").await.unwrap();
    // 直接插入过期记录模拟超时
    service.undo_records.insert(
        "t1".to_string(),
        super::UndoRecord::Cancel {
            previous_status: DownloadState::Downloading,
            previous_error_reason: None,
            timestamp: Instant::now() - Duration::from_secs(31),
        },
    );

    let result = service.undo_cancel_task("t1").await;
    assert!(result.is_err(), "撤销窗口超时后应返回错误");
    assert!(
        result.unwrap_err().to_string().contains("撤销窗口已过期"),
        "错误信息应提示撤销窗口过期"
    );
}

#[tokio::test]
async fn test_undo_cancel_repeat_fails() {
    let (service, _dir) = make_service();
    service
        .task_repository
        .insert("t1".to_string(), make_task("t1", "f.bin", "/dl".into()));
    service
        .task_repository
        .update_status("t1", DownloadState::Downloading);

    service.cancel_task("t1").await.unwrap();
    service.undo_cancel_task("t1").await.unwrap();

    let result = service.undo_cancel_task("t1").await;
    assert!(result.is_err(), "重复撤销应失败");
    assert!(
        result.unwrap_err().to_string().contains("无可用撤销记录"),
        "错误信息应提示无撤销记录"
    );
}

#[tokio::test]
async fn test_undo_delete_restores_task_and_snapshot() {
    let (service, dir) = make_service();
    let task = make_task("t1", "f.bin", dir.path().to_string_lossy().to_string());
    service
        .task_repository
        .insert("t1".to_string(), task.clone());

    // 预存快照
    let snapshot = task_info_to_snapshot(
        &task,
        dir.path().join("f.bin").to_string_lossy().to_string(),
        256,
        vec![],
        std::collections::HashMap::new(),
        None,
        None,
        true,
    );
    service.task_store.save_snapshot(&snapshot).unwrap();
    assert!(service.task_store.load_snapshot("t1").unwrap().is_some());

    service.delete_task("t1", false).await.unwrap();
    assert!(!service.task_repository.contains_key("t1"));
    assert!(
        service.task_store.load_snapshot("t1").unwrap().is_none(),
        "删除任务时应清理快照"
    );

    service.undo_delete_task("t1").await.unwrap();
    assert!(service.task_repository.contains_key("t1"));
    assert_eq!(service.get_task_detail("t1").unwrap().file_name, "f.bin");
    assert!(
        service.task_store.load_snapshot("t1").unwrap().is_some(),
        "撤销删除时应恢复快照"
    );
}

#[tokio::test]
async fn test_undo_delete_timeout_fails() {
    let (service, dir) = make_service();
    let task = make_task("t1", "f.bin", dir.path().to_string_lossy().to_string());
    service
        .task_repository
        .insert("t1".to_string(), task.clone());
    service
        .task_store
        .save_snapshot(&task_info_to_snapshot(
            &task,
            dir.path().join("f.bin").to_string_lossy().to_string(),
            256,
            vec![],
            std::collections::HashMap::new(),
            None,
            None,
            true,
        ))
        .unwrap();

    service.delete_task("t1", false).await.unwrap();
    // 直接插入过期记录模拟超时
    service.undo_records.insert(
        "t1".to_string(),
        super::UndoRecord::Delete {
            task: Box::new(task),
            snapshot: Box::new(None),
            timestamp: Instant::now() - Duration::from_secs(31),
        },
    );

    let result = service.undo_delete_task("t1").await;
    assert!(result.is_err(), "撤销窗口超时后应返回错误");
    assert!(
        result.unwrap_err().to_string().contains("撤销窗口已过期"),
        "错误信息应提示撤销窗口过期"
    );
}

#[tokio::test]
async fn test_undo_delete_repeat_fails() {
    let (service, dir) = make_service();
    let task = make_task("t1", "f.bin", dir.path().to_string_lossy().to_string());
    service.task_repository.insert("t1".to_string(), task);

    service.delete_task("t1", false).await.unwrap();
    service.undo_delete_task("t1").await.unwrap();

    let result = service.undo_delete_task("t1").await;
    assert!(result.is_err(), "重复撤销删除应失败");
    assert!(
        result.unwrap_err().to_string().contains("无可用撤销记录"),
        "错误信息应提示无撤销记录"
    );
}

#[tokio::test]
async fn test_undo_delete_ignores_missing_local_file() {
    let (service, dir) = make_service();
    let task = make_task("t1", "f.bin", dir.path().to_string_lossy().to_string());
    service.task_repository.insert("t1".to_string(), task);

    // 创建本地文件,删除任务但不删文件,再外部删除文件
    let file_path = dir.path().join("f.bin");
    std::fs::write(&file_path, b"data").unwrap();

    service.delete_task("t1", false).await.unwrap();
    std::fs::remove_file(&file_path).unwrap();
    assert!(!file_path.exists(), "前置条件:外部已删除文件");

    // 撤销删除应成功,仅恢复记录
    service.undo_delete_task("t1").await.unwrap();
    assert!(service.task_repository.contains_key("t1"));
    assert!(!file_path.exists(), "撤销删除不应恢复本地文件");
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────────

fn make_task(id: &str, file_name: &str, save_path: String) -> TaskInfo {
    make_task_with_order(id, file_name, save_path, 0, "2026-01-01T00:00:00Z")
}

fn make_task_with_order(
    id: &str,
    file_name: &str,
    save_path: String,
    display_order: i64,
    created_at: &str,
) -> TaskInfo {
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
        created_at: created_at.to_string(),
        save_path,
        error_reason: None,
        retry_count: 0,
        tags: vec![],
        hf_meta: None,
        display_order,
        mirror_urls: None,
    }
}

fn empty_snapshot() -> TaskSnapshot {
    TaskSnapshot {
        schema_version: 0,
        revision: 0,
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
        supports_range: true,
        created_at: String::new(),
        updated_at: String::new(),
        fail_reason: None,
        retry_count: 0,
        tags: vec![],
        hf_meta: None,
        display_order: 0,
        mirror_urls: None,
    }
}

// ── create_task 去重:url_identity_key(magnet 按 info hash)─────────────────

/// 不同 info hash 的 magnet 是不同资源,不得误判重复
/// (回归:旧实现把任意 magnet 统一脱敏为 <invalid-url>,第二个 magnet 任务必被误拒)
#[tokio::test]
async fn test_create_task_allows_two_different_magnets() {
    let (service, _dir) = make_service();
    let m1 = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=one&tr=udp://t1";
    let m2 = "magnet:?xt=urn:btih:6GJXEQ5SGFF7BWMQL74VTOAXZC36XSJG&dn=two&tr=udp://t2";
    service
        .create_task(m1, None, None, None, false)
        .await
        .expect("第一个 magnet 应创建成功");
    service
        .create_task(m2, None, None, None, false)
        .await
        .expect("不同 info hash 的 magnet 不得误判重复");
}

/// 同一资源(info hash 相同,大小写/tracker/dn 不同)判重
#[tokio::test]
async fn test_create_task_rejects_same_info_hash_magnet() {
    let (service, _dir) = make_service();
    let m1 = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=one&tr=udp://t1";
    // 同 hash 大写形式 + 不同 dn/tr 参数 → 同一资源
    let m2 = "magnet:?dn=other&tr=https://t2/announce&xt=urn:btih:0123456789ABCDEF0123456789ABCDEF01234567";
    service
        .create_task(m1, None, None, None, false)
        .await
        .unwrap();
    let result = service.create_task(m2, None, None, None, false).await;
    assert!(
        matches!(result, Err(AppError::TaskAlreadyExists(_))),
        "同 info hash 应判重: {result:?}"
    );
}

/// http(s) 去重口径不变:同路径不同签名 query 判同
#[tokio::test]
async fn test_create_task_dedup_http_ignores_query() {
    let (service, _dir) = make_service();
    service
        .create_task("https://example.com/f.bin?sig=1", None, None, None, false)
        .await
        .unwrap();
    let result = service
        .create_task("https://example.com/f.bin?sig=2", None, None, None, false)
        .await;
    assert!(
        matches!(result, Err(AppError::TaskAlreadyExists(_))),
        "同路径不同 query 应判重: {result:?}"
    );
}
