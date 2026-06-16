use std::sync::Arc;
use std::time::Duration;

use tachyon_core::config::DownloadConfig;
use tachyon_core::traits::TaskRunner;
use tachyon_core::types::{DownloadState, FileMetadata};
use tachyon_engine::DownloadTask;
use tachyon_engine::connection::ConnectionPool;
use tokio::sync::watch;
use url::Url;

use super::{AppError, AppState, TaskCommand, TaskInfo, cleanup_runtime, update_task_status};

// ---------------------------------------------------------------------------
// Core download task function
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(crate) async fn task_fn(
    state: Arc<AppState>,
    task_id: String,
    url: String,
    download_dir: String,
    download_config: DownloadConfig,
    connection_pool: Arc<ConnectionPool>,
    control_rx: watch::Receiver<TaskCommand>,
    mirror_urls: Option<Vec<String>>,
) {
    crate::runtime::DownloadSession::new(
        state,
        task_id,
        url,
        download_dir,
        download_config,
        connection_pool,
        control_rx,
        mirror_urls,
    )
    .run()
    .await;
}

// ---------------------------------------------------------------------------
// Helpers: 将 task_fn 的事务脚本拆分为单一职责函数
// ---------------------------------------------------------------------------

/// URL 解析、host 提取、启动前取消/暂停检查,并设置 Downloading 状态
pub(crate) async fn validate_and_prepare_url(
    url: &str,
    state: &AppState,
    task_id: &str,
    control_rx: &watch::Receiver<TaskCommand>,
) -> Option<String> {
    let download_url = match Url::parse(url) {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "URL 解析失败");
            mark_task_failed_and_cleanup(state, task_id).await;
            return None;
        }
    };

    let host = match download_url.host_str() {
        Some(h) => h.to_string(),
        None => {
            tracing::error!(task_id = %task_id, "URL 主机为空");
            mark_task_failed_and_cleanup(state, task_id).await;
            return None;
        }
    };

    {
        if let Some(task) = state.domain.task_repository.get(task_id) {
            if task.status == DownloadState::Cancelled {
                tracing::info!(task_id = %task_id, "任务已取消,跳过下载");
                cleanup_runtime(state, task_id);
                return None;
            }
            if task.status == DownloadState::Paused {
                tracing::info!(task_id = %task_id, "任务已暂停,等待恢复...");
            }
        }
    }

    // 设置 Downloading 前检查是否已被取消/暂停,防止 cancel 竞态覆盖状态
    let cmd = *control_rx.borrow();
    if cmd == TaskCommand::Cancel {
        tracing::info!(task_id = %task_id, "下载启动前检测到取消信号,终止下载");
        update_task_status(
            &state.domain.task_repository,
            task_id,
            DownloadState::Cancelled,
        );
        cleanup_runtime(state, task_id);
        return None;
    }
    if cmd == TaskCommand::Pause {
        tracing::info!(task_id = %task_id, "下载启动前检测到暂停信号,等待恢复");
    }

    update_task_status(
        &state.domain.task_repository,
        task_id,
        DownloadState::Downloading,
    );
    Some(host)
}

pub(crate) async fn ensure_download_dir(
    download_dir: &str,
    state: &AppState,
    task_id: &str,
) -> Result<(), ()> {
    if let Err(e) = tokio::fs::create_dir_all(download_dir).await {
        tracing::error!(task_id = %task_id, error = %e, "创建下载目录失败");
        mark_task_failed_and_cleanup(state, task_id).await;
        Err(())
    } else {
        Ok(())
    }
}

pub(crate) async fn build_download_task(
    task_id: &str,
    url: &str,
    download_config: DownloadConfig,
    connection_pool: Arc<ConnectionPool>,
    mirror_urls: Option<Vec<String>>,
) -> Result<Box<dyn TaskRunner>, ()> {
    match mirror_urls {
        Some(mirrors) if !mirrors.is_empty() => {
            tracing::info!(task_id = %task_id, mirrors = mirrors.len(), "使用镜像源下载");
            match DownloadTask::with_mirrors(
                url.to_string(),
                mirrors,
                download_config,
                Some(connection_pool),
            )
            .await
            {
                Ok(t) => Ok(Box::new(t)),
                Err(e) => {
                    tracing::error!(task_id = %task_id, error = %e, "创建镜像 DownloadTask 失败");
                    Err(())
                }
            }
        }
        _ => {
            match DownloadTask::with_pool(url.to_string(), download_config, Some(connection_pool))
                .await
            {
                Ok(t) => Ok(Box::new(t)),
                Err(e) => {
                    tracing::error!(task_id = %task_id, error = %e, "创建 DownloadTask 失败");
                    Err(())
                }
            }
        }
    }
}

pub(crate) fn should_cancel_before_run(control_rx: &watch::Receiver<TaskCommand>) -> bool {
    *control_rx.borrow() == TaskCommand::Cancel
}

pub(crate) async fn inject_resume_snapshot(
    task: &mut dyn TaskRunner,
    state: &AppState,
    task_id: &str,
) {
    if let Ok(Some(snapshot)) = state.infra.task_store.load_snapshot(task_id) {
        if !snapshot.completed_fragments.is_empty() {
            tracing::info!(
                task_id = %task_id,
                completed = snapshot.completed_fragments.len(),
                "断点续传:注入已完成分片"
            );
            task.set_completed_fragments(snapshot.completed_fragments);
        }
        if !snapshot.partial_fragments.is_empty() {
            tracing::info!(
                task_id = %task_id,
                partial = snapshot.partial_fragments.len(),
                "断点续传:注入字节级未完整分片"
            );
            task.set_partial_fragments(snapshot.partial_fragments);
        }
    }
}

pub(crate) async fn probe_and_save_metadata(
    task: &mut dyn TaskRunner,
    state: &AppState,
    task_id: &str,
    download_dir: &str,
    control_rx: &watch::Receiver<TaskCommand>,
) -> Option<FileMetadata> {
    let mut probe_cancel_rx = control_rx.clone();
    match tokio::select! {
        result = task.probe() => result,
        cancel = wait_for_cancel_signal(&mut probe_cancel_rx) => {
            match cancel {
                Err(e) => Err(e),
                Ok(()) => Err(tachyon_core::DownloadError::Other("控制信号异常结束".into())),
            }
        }
    } {
        Ok(meta) => {
            tracing::info!(
                task_id = %task_id,
                file_name = %meta.file_name,
                file_size = ?meta.file_size,
                supports_range = meta.supports_range,
                "元数据探测成功"
            );

            {
                // 预计算总分段数(基于最小分片1MB),供进度显示使用
                let total_frags = meta
                    .file_size
                    .map(|s| s.max(1).div_ceil(1024 * 1024))
                    .unwrap_or(0) as u32;
                if let Some(mut task) = state.domain.task_repository.get_mut(task_id) {
                    task.file_size = meta.file_size;
                    task.fragments_total = total_frags;
                }
            }

            let snapshot_task = {
                state
                    .domain
                    .task_repository
                    .get(task_id)
                    .map(|r| r.value().clone())
            };
            if let Some(task) = snapshot_task {
                let save_path = std::path::Path::new(download_dir)
                    .join(&meta.file_name)
                    .to_string_lossy()
                    .to_string();
                let snapshot = crate::task_store::task_info_to_snapshot(
                    &task,
                    save_path,
                    0,
                    vec![],
                    std::collections::HashMap::new(),
                    meta.etag.clone(),
                    meta.last_modified.clone(),
                );
                if let Err(e) = state.infra.task_store.save_snapshot(&snapshot) {
                    tracing::warn!(task_id = %task_id, error = %e, "保存元数据快照失败");
                }
            }

            Some(meta.clone())
        }
        Err(tachyon_core::DownloadError::Cancelled) => {
            cleanup_runtime(state, task_id);
            None
        }
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "元数据探测失败");
            mark_task_failed_and_cleanup(state, task_id).await;
            None
        }
    }
}

pub(crate) async fn mark_task_failed_and_cleanup(state: &AppState, task_id: &str) {
    update_task_status(
        &state.domain.task_repository,
        task_id,
        DownloadState::Failed,
    );
    cleanup_runtime(state, task_id);
}

pub(crate) async fn finalize_task_state(
    state: &AppState,
    task_id: &str,
    result: Result<&(), &tachyon_core::DownloadError>,
    final_file_size: Option<u64>,
) {
    match result {
        Ok(()) => {
            let final_size = final_file_size
                .or_else(|| {
                    state
                        .domain
                        .task_repository
                        .get(task_id)
                        .and_then(|task| task.file_size)
                })
                .unwrap_or(0);
            if let Some(mut task) = state.domain.task_repository.get_mut(task_id) {
                if task.status == DownloadState::Cancelled {
                    tracing::info!(task_id = %task_id, "下载完成但任务已被取消");
                } else {
                    task.progress = 1.0;
                    task.file_size = Some(final_size);
                    task.downloaded = final_size;
                    task.speed = 0;
                    task.status = DownloadState::Completed;
                    tracing::info!(task_id = %task_id, file_size = final_size, "下载任务完成");
                }
            }
        }
        Err(e) => {
            if let Some(mut task) = state.domain.task_repository.get_mut(task_id) {
                if task.status == DownloadState::Cancelled {
                    tracing::info!(task_id = %task_id, "下载失败但任务已被取消,保留取消状态");
                } else {
                    task.status = DownloadState::Failed;
                    task.speed = 0;
                    tracing::error!(task_id = %task_id, error = %e, "下载任务失败");
                }
            }
        }
    }
}

pub(crate) async fn wait_chunk_reader_done(
    done_rx: tokio::sync::oneshot::Receiver<()>,
    task_id: &str,
) -> Result<(), ()> {
    match tokio::time::timeout(Duration::from_secs(3), done_rx).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            tracing::warn!(task_id = %task_id, error = %e, "chunk reader 任务异常退出");
            Err(())
        }
        Err(_) => {
            tracing::warn!(task_id = %task_id, "chunk reader 退出超时,强制中止");
            Err(())
        }
    }
}

pub(crate) fn extract_fail_reason(
    state: &AppState,
    task_id: &str,
    result: Result<&(), &tachyon_core::DownloadError>,
) -> Option<String> {
    match result {
        Ok(()) => None,
        Err(e) => {
            if state
                .domain
                .task_repository
                .get(task_id)
                .is_some_and(|t| t.status == DownloadState::Cancelled)
            {
                None
            } else {
                Some(e.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: wait for cancel signal
// ---------------------------------------------------------------------------

async fn wait_for_cancel_signal(
    control_rx: &mut watch::Receiver<TaskCommand>,
) -> Result<(), tachyon_core::DownloadError> {
    loop {
        let cmd = *control_rx.borrow_and_update();
        match cmd {
            TaskCommand::Cancel => return Err(tachyon_core::DownloadError::Cancelled),
            _ => control_rx
                .changed()
                .await
                .map_err(|_| tachyon_core::DownloadError::Other("控制通道已关闭".into()))?,
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn create_task(
    state: tauri::State<'_, AppState>,
    url: String,
    download_dir: Option<String>,
    mirror_urls: Option<Vec<String>>,
) -> Result<String, AppError> {
    create_task_inner(&state, url, download_dir, mirror_urls).await
}

#[tauri::command]
pub async fn pause_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
    pause_task_inner(&state, task_id).await
}

#[tauri::command]
pub async fn resume_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
    resume_task_inner(&state, task_id).await
}

#[tauri::command]
pub async fn cancel_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
    cancel_task_inner(&state, task_id).await
}

#[tauri::command]
pub async fn delete_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
    confirmation_token: Option<String>,
) -> Result<(), AppError> {
    // P1-11b: 验证一次性确认令牌，绑定 action 防止跨操作复用
    match confirmation_token {
        Some(token) => {
            state
                .service
                .confirmation_service
                .validate_and_consume(&token, "delete_task")?;
        }
        None => {
            return Err(super::AppError::Config(
                "删除任务需要确认令牌,请先确认操作".to_string(),
            ));
        }
    }
    delete_task_inner(&state, task_id).await
}

#[tauri::command]
pub async fn get_task_list(state: tauri::State<'_, AppState>) -> Result<Vec<TaskInfo>, AppError> {
    get_task_list_inner(&state).await
}

#[tauri::command]
pub async fn get_task_detail(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<TaskInfo, AppError> {
    get_task_detail_inner(&state, task_id).await
}

// ---------------------------------------------------------------------------
// Inner implementations
// ---------------------------------------------------------------------------

pub(crate) async fn create_task_inner(
    state: &AppState,
    url: String,
    download_dir: Option<String>,
    mirror_urls: Option<Vec<String>>,
) -> Result<String, AppError> {
    let creation = state
        .service
        .task_service
        .create_task(&url, download_dir.as_deref(), mirror_urls.as_deref())
        .await?;

    let state_arc = Arc::new(state.clone_for_task());
    state.runtime.supervisor.start_download(
        state_arc,
        &creation.task_id,
        creation.url,
        creation.download_dir,
        creation.download_config,
        creation.mirror_urls,
    );

    Ok(creation.task_id)
}

pub(crate) async fn pause_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    state.service.task_service.pause_task(&task_id).await?;
    state
        .runtime
        .supervisor
        .send_command(&task_id, TaskCommand::Pause);
    Ok(())
}

pub(crate) async fn resume_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    state.service.task_service.resume_task(&task_id).await?;
    state
        .runtime
        .supervisor
        .send_command(&task_id, TaskCommand::Resume);
    Ok(())
}

pub(crate) async fn cancel_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    state.service.task_service.cancel_task(&task_id).await?;
    state
        .runtime
        .supervisor
        .send_command(&task_id, TaskCommand::Cancel);
    Ok(())
}

pub(crate) async fn delete_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    state.service.task_service.delete_task(&task_id)?;
    state.runtime.supervisor.cleanup(&task_id);
    Ok(())
}

pub(crate) async fn get_task_list_inner(state: &AppState) -> Result<Vec<TaskInfo>, AppError> {
    Ok(state.service.task_service.get_task_list())
}

pub(crate) async fn get_task_detail_inner(
    state: &AppState,
    task_id: String,
) -> Result<TaskInfo, AppError> {
    state.service.task_service.get_task_detail(&task_id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::tests::test_state;
    use super::super::{build_download_config, now_iso8601};
    use super::*;
    use tachyon_core::safety::redact_url_for_log;
    use tachyon_core::types::DownloadState;
    use tokio::sync::oneshot;
    use uuid::Uuid;

    async fn spawn_task_fn_for_test(
        state: Arc<AppState>,
        task_id: String,
        url: String,
        file_name: String,
        download_dir: String,
    ) {
        let download_config = {
            let cfg = state.domain.config.lock().await;
            build_download_config(&cfg, &download_dir)
        };
        let (control_tx, control_rx) = watch::channel(TaskCommand::Start);
        state
            .runtime
            .supervisor
            .command_channels
            .insert(task_id.clone(), control_tx);
        state.domain.task_repository.insert(
            task_id.clone(),
            TaskInfo {
                id: task_id.clone(),
                url: redact_url_for_log(&url),
                file_name,
                file_size: None,
                downloaded: 0,
                speed: 0,
                status: DownloadState::Pending,
                progress: 0.0,
                fragments_total: 0,
                fragments_done: 0,
                created_at: now_iso8601(),
                save_path: String::new(),
            },
        );

        let (start_tx, start_rx) = oneshot::channel();
        let handle = tokio::spawn({
            let state = state.clone();
            let connection_pool = state.infra.connection_pool.clone();
            let task_id = task_id.clone();
            async move {
                let _ = start_rx.await;
                task_fn(
                    state,
                    task_id,
                    url,
                    download_dir,
                    download_config,
                    connection_pool,
                    control_rx,
                    None,
                )
                .await;
            }
        });
        state.runtime.supervisor.handles.insert(task_id, handle);
        let _ = start_tx.send(());
    }

    #[tokio::test]
    async fn test_create_task_returns_valid_uuid() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(Uuid::parse_str(&id).is_ok());
    }

    #[tokio::test]
    async fn test_create_task_extracts_filename() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://cdn.example.org/releases/app-v2.0.tar.gz".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.file_name, "app-v2.0.tar.gz");
    }

    #[tokio::test]
    async fn test_create_task_default_status_is_pending() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/data.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, DownloadState::Pending);
        assert_eq!(task.downloaded, 0);
        assert_eq!(task.speed, 0);
        assert!((task.progress - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_create_task_with_download_dir() {
        let state = test_state();
        // 使用 test_state 中已授权的下载目录的子目录
        let cfg = state.domain.config.lock().await;
        let base_dir = cfg.download.download_dir.clone();
        drop(cfg);
        let sub_dir = std::path::Path::new(&base_dir)
            .join("subdir")
            .to_string_lossy()
            .to_string();
        std::fs::create_dir_all(&sub_dir).unwrap();

        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            Some(sub_dir.clone()),
            None,
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.url, "https://example.com/file.zip");
    }

    #[tokio::test]
    async fn test_create_task_duplicate_url_rejected() {
        let state = test_state();
        let _ = create_task_inner(
            &state,
            "https://dup.example.com/once.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let result = create_task_inner(
            &state,
            "https://dup.example.com/once.zip".to_string(),
            None,
            None,
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("已存在"));
    }

    /// Q-001 修复验证:并发创建相同 URL 的任务,应只有一个成功
    /// 修复前存在 TOCTOU 竞态:检查与插入非原子,两个并发请求可能都通过检查
    #[tokio::test]
    async fn test_concurrent_create_same_url_only_one_succeeds() {
        let state = test_state();
        let url = "https://race.example.com/unique-file.bin";

        // 并发创建 10 个相同 URL 的任务
        let mut handles = Vec::new();
        for _ in 0..10 {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                create_task_inner(&state, url.to_string(), None, None).await
            }));
        }

        let mut successes = 0usize;
        let mut failures = 0usize;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(_) => successes += 1,
                Err(_) => failures += 1,
            }
        }

        // 必须恰好只有 1 个成功
        assert_eq!(
            successes, 1,
            "并发创建相同 URL 应只有 1 个成功,实际成功 {successes} 个"
        );
        // 其余 9 个应返回 TaskAlreadyExists 错误
        assert_eq!(
            failures, 9,
            "并发创建相同 URL 应有 9 个失败,实际失败 {failures} 个"
        );

        // 验证 DashMap 中只有 1 条任务记录
        let task_count = state
            .domain
            .task_repository
            .iter()
            .filter(|r| r.value().url == url)
            .count();
        assert_eq!(task_count, 1, "DashMap 中应只有 1 条相同 URL 的任务");
    }

    /// 压力测试:50 个不同 URL 并发创建,验证并发门控(max_concurrent_tasks=5)
    /// 成功数应恰等于上限 5,其余应返回"已达最大并发任务数"错误。
    #[tokio::test]
    async fn test_concurrent_create_distinct_urls_respects_max_tasks() {
        let state = test_state();
        // test_state 的 max_concurrent_tasks = 5
        const N: usize = 50;

        let mut handles = Vec::new();
        for i in 0..N {
            let state = state.clone();
            let url = format!("https://stress.example.com/file-{i}.bin");
            handles.push(tokio::spawn(async move {
                create_task_inner(&state, url, None, None).await
            }));
        }

        let mut successes = 0usize;
        let mut cap_failures = 0usize;
        let mut other_failures = 0usize;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(_) => successes += 1,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("已达最大并发任务数") {
                        cap_failures += 1;
                    } else {
                        other_failures += 1;
                    }
                }
            }
        }

        // 成功数应恰等于上限 5(Pending/Downloading 任务数)
        assert_eq!(
            successes, 5,
            "并发门控应限制活跃任务数为 5,实际成功 {successes} 个"
        );
        // 其余 45 个应因并发上限失败
        assert_eq!(
            cap_failures,
            N - 5,
            "应有 {} 个任务因并发上限失败,实际 {cap_failures} 个",
            N - 5
        );
        // 不应有其他类型失败(URL 校验/去重等)
        assert_eq!(
            other_failures, 0,
            "不应有非并发上限的失败,实际 other_failures={other_failures}"
        );
        // DashMap 中应恰有 5 条任务
        assert_eq!(
            state.domain.task_repository.len(),
            5,
            "DashMap 应恰有 5 条任务,实际 {} 条",
            state.domain.task_repository.len()
        );
    }

    #[tokio::test]
    async fn test_pause_pending_task() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        pause_task_inner(&state, id.clone()).await.unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, DownloadState::Paused);
        assert_eq!(task.speed, 0);
    }

    #[tokio::test]
    async fn test_resume_paused_task() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        pause_task_inner(&state, id.clone()).await.unwrap();
        resume_task_inner(&state, id.clone()).await.unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, DownloadState::Downloading);
    }

    #[tokio::test]
    async fn test_pause_already_paused_task_fails() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        pause_task_inner(&state, id.clone()).await.unwrap();
        let result = pause_task_inner(&state, id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不允许暂停"));
    }

    #[tokio::test]
    async fn test_resume_non_paused_task_fails() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let result = resume_task_inner(&state, id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("仅暂停状态可恢复"));
    }

    #[tokio::test]
    async fn test_cancel_pending_task() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, DownloadState::Cancelled);
    }

    #[tokio::test]
    async fn test_cancel_already_cancelled_task_fails() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        let result = cancel_task_inner(&state, id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("无法取消"));
    }

    #[tokio::test]
    async fn test_delete_cancelled_task() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        delete_task_inner(&state, id.clone()).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    #[tokio::test]
    async fn test_delete_pending_task_fails() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let result = delete_task_inner(&state, id.clone()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不允许删除"));
    }

    #[tokio::test]
    async fn test_get_task_list_returns_all_tasks() {
        let state = test_state();
        let id1 = create_task_inner(&state, "https://example.com/a.zip".to_string(), None, None)
            .await
            .unwrap();
        let id2 = create_task_inner(&state, "https://example.com/b.zip".to_string(), None, None)
            .await
            .unwrap();
        let list = get_task_list_inner(&state).await.unwrap();
        let ids: Vec<&String> = list.iter().map(|t| &t.id).collect();
        assert!(ids.contains(&&id1));
        assert!(ids.contains(&&id2));
    }

    #[tokio::test]
    async fn test_get_task_list_empty() {
        let state = test_state();
        let list = get_task_list_inner(&state).await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_get_task_detail_not_found() {
        let state = test_state();
        let result = get_task_detail_inner(&state, "nonexistent-id".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("任务不存在"));
    }

    #[tokio::test]
    async fn test_full_task_lifecycle() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/lifecycle.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            get_task_detail_inner(&state, id.clone())
                .await
                .unwrap()
                .status,
            DownloadState::Pending
        );

        pause_task_inner(&state, id.clone()).await.unwrap();
        assert_eq!(
            get_task_detail_inner(&state, id.clone())
                .await
                .unwrap()
                .status,
            DownloadState::Paused
        );

        resume_task_inner(&state, id.clone()).await.unwrap();
        assert_eq!(
            get_task_detail_inner(&state, id.clone())
                .await
                .unwrap()
                .status,
            DownloadState::Downloading
        );

        cancel_task_inner(&state, id.clone()).await.unwrap();
        assert_eq!(
            get_task_detail_inner(&state, id.clone())
                .await
                .unwrap()
                .status,
            DownloadState::Cancelled
        );

        delete_task_inner(&state, id.clone()).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    #[tokio::test]
    async fn test_max_concurrent_tasks_rejects() {
        let state = AppState::new();
        {
            let mut cfg = state.domain.config.lock().await;
            cfg.max_concurrent_tasks = 2;
            // 设置有效下载目录，确保 authorized_dirs 校验通过
            let test_dir = std::env::temp_dir().join("tachyon-test-rejects");
            let test_dir_str = test_dir.to_string_lossy().to_string();
            let _ = std::fs::create_dir_all(&test_dir);
            cfg.download.download_dir = test_dir_str.clone();
            cfg.download.authorized_dirs = vec![test_dir_str];
        }
        let _id1 = create_task_inner(&state, "http://example.com/file1.bin".into(), None, None)
            .await
            .unwrap();
        let _id2 = create_task_inner(&state, "http://example.com/file2.bin".into(), None, None)
            .await
            .unwrap();
        let result =
            create_task_inner(&state, "http://example.com/file3.bin".into(), None, None).await;
        assert!(result.is_err(), "超过 max_concurrent_tasks 应返回错误");
        assert!(
            result.unwrap_err().to_string().contains("最大并发任务数"),
            "错误信息应提及并发限制"
        );
    }

    #[tokio::test]
    async fn test_zero_max_concurrent_fragments_marks_task_failed() {
        let state = test_state();
        {
            let mut cfg = state.domain.config.lock().await;
            cfg.download.max_concurrent_fragments = 0;
        }
        let result =
            create_task_inner(&state, "http://example.com/zero-sem.bin".into(), None, None).await;
        assert!(
            result.is_err(),
            "max_concurrent_fragments=0 时应拒绝创建任务"
        );
        if let Err(e) = result {
            assert!(matches!(e, AppError::Config(_)), "应为 Config 错误: {e}");
        }
    }

    #[tokio::test]
    async fn test_concurrent_cancel_and_get_list_no_deadlock() {
        let state = test_state();

        let mut task_ids = Vec::new();
        for i in 0..5 {
            let id = create_task_inner(
                &state,
                format!("http://example.com/deadlock-test-{i}.bin"),
                None,
                None,
            )
            .await
            .unwrap();
            task_ids.push(id);
        }

        let mut cancel_handles = Vec::new();

        for id in &task_ids[..3] {
            let state_clone = state.clone();
            let tid = id.clone();
            cancel_handles.push(tokio::spawn(async move {
                cancel_task_inner(&state_clone, tid).await
            }));
        }

        let mut list_handles = Vec::new();

        for _ in 0..3 {
            let state_clone = state.clone();
            list_handles.push(tokio::spawn(async move {
                get_task_list_inner(&state_clone).await
            }));
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for handle in cancel_handles {
                let _ = handle.await;
            }
            for handle in list_handles {
                let _ = handle.await;
            }
        })
        .await;

        assert!(result.is_ok(), "并发 cancel+get_list 操作超时,疑似死锁");

        for id in &task_ids[..3] {
            let task = get_task_detail_inner(&state, id.clone()).await.unwrap();
            assert_eq!(
                task.status,
                DownloadState::Cancelled,
                "任务应已被取消: {}",
                id
            );
        }
    }

    #[tokio::test]
    async fn test_concurrent_create_and_delete_no_deadlock() {
        let state = test_state();

        let mut deletable_ids = Vec::new();
        for i in 0..3 {
            let id = create_task_inner(
                &state,
                format!("http://example.com/to-delete-{i}.bin"),
                None,
                None,
            )
            .await
            .unwrap();
            cancel_task_inner(&state, id.clone()).await.unwrap();
            deletable_ids.push(id);
        }

        let mut create_handles = Vec::new();

        for i in 0..3 {
            let state_clone = state.clone();
            create_handles.push(tokio::spawn(async move {
                create_task_inner(
                    &state_clone,
                    format!("http://example.com/new-task-{i}.bin"),
                    None,
                    None,
                )
                .await
            }));
        }

        let mut delete_handles = Vec::new();

        for id in &deletable_ids {
            let state_clone = state.clone();
            let tid = id.clone();
            delete_handles.push(tokio::spawn(async move {
                delete_task_inner(&state_clone, tid).await
            }));
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for handle in create_handles {
                let _ = handle.await;
            }
            for handle in delete_handles {
                let _ = handle.await;
            }
        })
        .await;

        assert!(result.is_ok(), "并发 create+delete 操作超时,疑似死锁");

        for id in &deletable_ids {
            let result = get_task_detail_inner(&state, id.clone()).await;
            assert!(result.is_err(), "已删除任务应不存在: {}", id);
        }
    }

    #[tokio::test]
    async fn test_concurrent_pause_resume_no_deadlock() {
        let state = test_state();

        let id = create_task_inner(
            &state,
            "http://example.com/pause-resume-test.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();

        let mut handles = Vec::new();

        for i in 0..10 {
            let state_clone = state.clone();
            let tid = id.clone();
            if (i as u32).is_multiple_of(2) {
                handles.push(tokio::spawn(async move {
                    pause_task_inner(&state_clone, tid).await
                }));
            } else {
                handles.push(tokio::spawn(async move {
                    resume_task_inner(&state_clone, tid).await
                }));
            }
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for handle in handles {
                let _ = handle.await;
            }
        })
        .await;

        assert!(result.is_ok(), "并发 pause+resume 操作超时,疑似死锁");
    }

    #[tokio::test]
    async fn test_pause_resume_send_cooperative_control_signal() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "http://example.com/control-pause.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let mut rx = state
            .runtime
            .supervisor
            .command_channels
            .get(&id)
            .unwrap()
            .subscribe();

        pause_task_inner(&state, id.clone()).await.unwrap();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TaskCommand::Pause);

        resume_task_inner(&state, id).await.unwrap();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TaskCommand::Resume);
    }

    #[tokio::test]
    async fn test_failed_download_updates_task_info_status_failed() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/status-failed.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();

        {
            update_task_status(&state.domain.task_repository, &id, DownloadState::Failed);
        }

        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, DownloadState::Failed);
        assert_eq!(task.speed, 0);
    }

    #[tokio::test]
    async fn test_cancel_sends_signal_and_background_task_exits() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "http://example.com/control-cancel.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let mut rx = state
            .runtime
            .supervisor
            .command_channels
            .get(&id)
            .unwrap()
            .subscribe();

        cancel_task_inner(&state, id.clone()).await.unwrap();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TaskCommand::Cancel);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while state.runtime.supervisor.handles.contains_key(&id) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("取消后后台任务应有序退出并清理句柄");
    }

    #[tokio::test]
    async fn test_task_fn_construct_failure_cleans_runtime_state() {
        let state = test_state();
        let task_id = "p0-construct-fast-fail".to_string();
        let download_root = tempfile::tempdir().unwrap();
        let download_dir = download_root.path().to_string_lossy().to_string();
        let url = "ftp://example.com/fast-fail.bin".to_string();

        spawn_task_fn_for_test(
            state.clone(),
            task_id.clone(),
            url,
            "fast-fail.bin".to_string(),
            download_dir,
        )
        .await;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = state
                    .domain
                    .task_repository
                    .get(&task_id)
                    .map(|task| task.status);
                if status == Some(DownloadState::Failed)
                    && !state.runtime.supervisor.handles.contains_key(&task_id)
                    && !state
                        .runtime
                        .supervisor
                        .command_channels
                        .contains_key(&task_id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("DownloadTask 构造失败后应写入 Failed 并清理运行态");

        let task = state.domain.task_repository.get(&task_id).unwrap();
        assert_eq!(task.status, DownloadState::Failed);
        assert!(
            !state.runtime.supervisor.handles.contains_key(&task_id),
            "DownloadTask 构造失败后应清理后台 JoinHandle"
        );
        assert!(
            !state
                .runtime
                .supervisor
                .command_channels
                .contains_key(&task_id),
            "DownloadTask 构造失败后应清理控制通道"
        );
    }
}
