use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tachyon_core::config::DownloadConfig;
use tachyon_core::filename::extract_filename_from_url;
use tachyon_core::types::DownloadState;
use tachyon_engine::DownloadTask;
use tachyon_engine::connection::ConnectionPool;
use tokio::sync::watch;
use url::Url;
use uuid::Uuid;

use super::config_commands::authorize_download_dir;
use super::{
    AppError, AppState, ProgressEvent, TaskInfo, TaskProgress, build_download_config,
    cleanup_runtime, now_iso8601, persist_task_snapshot, rewrite_hf_url, update_task_status,
    validate_download_url,
};

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
    control_rx: watch::Receiver<DownloadState>,
    mirror_urls: Option<Vec<String>>,
) {
    // HF 镜像: 自动将 huggingface.co 替换为 HF_ENDPOINT 或 hf-mirror.com
    let url = rewrite_hf_url(&url);

    let download_url = match Url::parse(&url) {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "URL 解析失败");
            update_task_status(&state.tasks, &task_id, DownloadState::Failed);
            cleanup_runtime(&state, &task_id);
            return;
        }
    };

    let host = match download_url.host_str() {
        Some(h) => h.to_string(),
        None => {
            tracing::error!(task_id = %task_id, "URL 主机为空");
            update_task_status(&state.tasks, &task_id, DownloadState::Failed);
            cleanup_runtime(&state, &task_id);
            return;
        }
    };

    {
        if let Some(task) = state.tasks.get(&task_id) {
            if task.status == DownloadState::Cancelled {
                tracing::info!(task_id = %task_id, "任务已取消,跳过下载");
                cleanup_runtime(&state, &task_id);
                return;
            }
            if task.status == DownloadState::Paused {
                tracing::info!(task_id = %task_id, "任务已暂停,等待恢复...");
            }
        }
    }

    tracing::info!(
        task_id = %task_id,
        host = %host,
        download_dir = %download_dir,
        "开始真实下载"
    );

    update_task_status(&state.tasks, &task_id, DownloadState::Downloading);

    if let Err(e) = std::fs::create_dir_all(&download_dir) {
        tracing::error!(task_id = %task_id, error = %e, "创建下载目录失败");
        update_task_status(&state.tasks, &task_id, DownloadState::Failed);
        cleanup_runtime(&state, &task_id);
        return;
    }

    let mut download_task = match mirror_urls {
        Some(mirrors) if !mirrors.is_empty() => {
            tracing::info!(task_id = %task_id, mirrors = mirrors.len(), "使用镜像源下载");
            match DownloadTask::with_mirrors(
                url.clone(),
                mirrors,
                download_config,
                Some(connection_pool),
            )
            .await
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(task_id = %task_id, error = %e, "创建镜像 DownloadTask 失败");
                    update_task_status(&state.tasks, &task_id, DownloadState::Failed);
                    return;
                }
            }
        }
        _ => {
            match DownloadTask::with_pool(url.clone(), download_config, Some(connection_pool)).await
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(task_id = %task_id, error = %e, "创建 DownloadTask 失败");
                    update_task_status(&state.tasks, &task_id, DownloadState::Failed);
                    return;
                }
            }
        }
    };
    download_task.set_control_rx(control_rx.clone());

    // 断点续传:若存在已保存快照,注入已完成分片索引,plan() 后将跳过这些分片
    if let Ok(Some(snapshot)) = state.task_store.load_snapshot(&task_id)
        && !snapshot.completed_fragments.is_empty()
    {
        tracing::info!(
            task_id = %task_id,
            completed = snapshot.completed_fragments.len(),
            "断点续传:注入已完成分片"
        );
        download_task.set_completed_fragments(snapshot.completed_fragments);
    }

    if *control_rx.borrow() == DownloadState::Cancelled {
        cleanup_runtime(&state, &task_id);
        return;
    }

    let mut probe_cancel_rx = control_rx.clone();
    match tokio::select! {
        result = download_task.probe() => result,
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
                if let Some(mut task) = state.tasks.get_mut(&task_id) {
                    task.file_size = meta.file_size;
                    task.fragments_total = total_frags;
                }
            }

            let snapshot_task = { state.tasks.get(&task_id).map(|r| r.value().clone()) };
            if let Some(task) = snapshot_task {
                let save_path = std::path::Path::new(&download_dir)
                    .join(&meta.file_name)
                    .to_string_lossy()
                    .to_string();
                let snapshot = crate::task_store::task_info_to_snapshot(
                    &task,
                    save_path,
                    0,
                    vec![],
                    meta.etag.clone(),
                    meta.last_modified.clone(),
                );
                if let Err(e) = state.task_store.save_snapshot(&snapshot) {
                    tracing::warn!(task_id = %task_id, error = %e, "保存元数据快照失败");
                }
            }
        }
        Err(tachyon_core::DownloadError::Cancelled) => {
            cleanup_runtime(&state, &task_id);
            return;
        }
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "元数据探测失败");
            update_task_status(&state.tasks, &task_id, DownloadState::Failed);
            cleanup_runtime(&state, &task_id);
            return;
        }
    }

    let (chunk_progress_tx, mut chunk_progress_rx) =
        tokio::sync::mpsc::channel::<tachyon_engine::FragmentProgress>(256);
    download_task.set_progress_sender(chunk_progress_tx);

    let download_task = Arc::new(tokio::sync::Mutex::new(download_task));

    if *control_rx.borrow() == DownloadState::Cancelled {
        cleanup_runtime(&state, &task_id);
        return;
    }

    let chunk_state = state.clone();
    let chunk_tid = task_id.clone();
    tokio::spawn(async move {
        // 已完成分片集合,用于断点续传 checkpoint
        let mut completed: BTreeSet<u32> = BTreeSet::new();
        // 从 state.tasks 读取 probe 阶段已写入的 total_frags (零锁)
        let total_frags = chunk_state
            .tasks
            .get(&chunk_tid)
            .map(|t| t.fragments_total)
            .unwrap_or(0);
        tracing::info!(task_id = %chunk_tid, total_frags, "chunk reader 启动,等待进度事件");
        // 跟踪每个分片的已下载字节数
        let mut frag_bytes: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        let mut total_downloaded: u64 = 0;
        let mut event_count: u64 = 0;
        while let Some(progress) = chunk_progress_rx.recv().await {
            event_count += 1;
            if progress.completed {
                completed.insert(progress.fragment_index);
            }
            // 增量更新: 替换旧值,差值累加到总数
            let old = frag_bytes
                .insert(progress.fragment_index, progress.fragment_downloaded)
                .unwrap_or(0);
            total_downloaded =
                total_downloaded.saturating_add(progress.fragment_downloaded.saturating_sub(old));
            if event_count == 1 || event_count.is_multiple_of(50) {
                tracing::info!(
                    event = event_count,
                    idx = progress.fragment_index,
                    done = completed.len(),
                    total_frags,
                    total_downloaded,
                    "chunk reader 进度更新"
                );
            }
            let frags_done = completed.len() as u32;
            {
                if let Some(mut task) = chunk_state.tasks.get_mut(&chunk_tid) {
                    task.downloaded = total_downloaded;
                    task.fragments_done = frags_done;
                    task.fragments_total = total_frags;
                    if total_frags > 0 {
                        task.progress = frags_done as f64 / total_frags as f64;
                    }
                }
            }

            // 分片整体完成:更新 completed_fragments 并 checkpoint 落盘(断点续传)
            if progress.completed {
                completed.insert(progress.fragment_index);
                if let Ok(Some(mut snapshot)) = chunk_state.task_store.load_snapshot(&chunk_tid) {
                    snapshot.completed_fragments = completed.iter().copied().collect();
                    snapshot.downloaded = total_downloaded;
                    if let Err(e) = chunk_state.task_store.save_snapshot(&snapshot) {
                        tracing::warn!(task_id = %chunk_tid, error = %e, "checkpoint 落盘失败");
                    }
                }
            }
        }
    });

    let monitor_ps = state.clone();
    let monitor_tid = task_id.clone();
    let mut progress_control_rx = control_rx.clone();
    let progress_handle = tokio::spawn(async move {
        let start = Instant::now();
        let mut last_downloaded: u64 = 0;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                changed = progress_control_rx.changed() => {
                    if changed.is_err() {
                        return 0;
                    }
                    let control_state = {
                        let borrowed = progress_control_rx.borrow_and_update();
                        *borrowed
                    };
                    match control_state {
                        DownloadState::Cancelled => return 0,
                        DownloadState::Paused => {
                            if let Some(mut task) = monitor_ps.tasks.get_mut(&monitor_tid) {
                                task.speed = 0;
                            }
                            continue;
                        }
                        _ => continue,
                    }
                }
            }
            // 从 state.tasks 读取进度(chunk reader 已写入),不锁 download_task
            let (downloaded, ds) = {
                if let Some(task) = monitor_ps.tasks.get(&monitor_tid) {
                    (task.downloaded, task.status)
                } else {
                    continue;
                }
            };

            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                ((downloaded as f64 - last_downloaded as f64) / 0.5) as u64
            } else {
                0
            };
            last_downloaded = downloaded;

            {
                if let Some(mut task) = monitor_ps.tasks.get_mut(&monitor_tid) {
                    task.speed = speed;
                }
            }

            if ds == DownloadState::Completed || ds == DownloadState::Failed {
                return speed;
            }

            {
                let t = match monitor_ps.tasks.get(&monitor_tid) {
                    Some(t) => t,
                    None => continue,
                };
                let event: ProgressEvent = std::iter::once((
                    monitor_tid.clone(),
                    TaskProgress {
                        id: monitor_tid.clone(),
                        progress: t.progress,
                        speed,
                        downloaded,
                        status: t.status,
                        fragments_done: t.fragments_done,
                    },
                ))
                .collect();
                tracing::debug!(
                    tid = %monitor_tid,
                    downloaded,
                    speed,
                    progress = t.progress,
                    frags = t.fragments_done,
                    "广播进度事件"
                );
                if monitor_ps.progress_tx.send(event).is_err() {
                    tracing::warn!("broadcast send 失败(无接收者)");
                }
            }

            if ds == DownloadState::Completed || ds == DownloadState::Failed {
                return speed;
            }
        }
    });

    let (download_result, _final_speed) = tokio::join!(
        async {
            let mut dt = download_task.lock().await;
            dt.run().await
        },
        progress_handle
    );
    let result = download_result;

    cleanup_runtime(&state, &task_id);

    let current_status = state.tasks.get(&task_id).map(|t| t.status);

    match result {
        Ok(()) => {
            if current_status == Some(DownloadState::Cancelled) {
                tracing::info!(task_id = %task_id, "下载完成但任务已被取消");
            } else if let Some(mut task) = state.tasks.get_mut(&task_id) {
                task.progress = 1.0;
                let dt = download_task.lock().await;
                let final_size = dt.metadata().and_then(|m| m.file_size).unwrap_or(0);
                task.downloaded = final_size;
                task.speed = 0;
                drop(dt);
                drop(task);
                update_task_status(&state.tasks, &task_id, DownloadState::Completed);
                tracing::info!(task_id = %task_id, file_size = final_size, "下载任务完成");
            }
        }
        Err(e) => {
            if current_status == Some(DownloadState::Cancelled) {
                tracing::info!(task_id = %task_id, "下载失败但任务已被取消,保留取消状态");
            } else {
                update_task_status(&state.tasks, &task_id, DownloadState::Failed);
                tracing::error!(task_id = %task_id, error = %e, "下载任务失败");
            }
        }
    }

    {
        let event: ProgressEvent = state
            .tasks
            .iter()
            .map(|r| {
                let id = r.key();
                let t = r.value();
                (
                    id.clone(),
                    TaskProgress {
                        id: id.clone(),
                        progress: t.progress,
                        speed: t.speed,
                        downloaded: t.downloaded,
                        status: t.status,
                        fragments_done: t.fragments_done,
                    },
                )
            })
            .collect();
        let _ = state.progress_tx.send(event);
    }

    persist_task_snapshot(&state, &task_id, None).await;
}

// ---------------------------------------------------------------------------
// Helper: wait for cancel signal
// ---------------------------------------------------------------------------

async fn wait_for_cancel_signal(
    control_rx: &mut watch::Receiver<DownloadState>,
) -> Result<(), tachyon_core::DownloadError> {
    loop {
        let state = *control_rx.borrow_and_update();
        match state {
            DownloadState::Cancelled => return Err(tachyon_core::DownloadError::Cancelled),
            DownloadState::Failed => {
                return Err(tachyon_core::DownloadError::Other("任务已失败".into()));
            }
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
) -> Result<(), AppError> {
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
    validate_download_url(&url)?;

    // A-14: 对每个镜像 URL 执行与主 URL 相同的 SSRF 防护验证
    if let Some(ref mirrors) = mirror_urls {
        for mirror in mirrors {
            validate_download_url(mirror)
                .map_err(|e| AppError::Config(format!("镜像 URL 验证失败: {e}")))?;
        }
    }

    // 提前获取配置和下载目录,避免在检查-插入间隙中 await(消除 TOCTOU 竞态)
    let (max_tasks, download_dir_str) = {
        let cfg = state.config.lock().await;
        let max_tasks = cfg.max_concurrent_tasks as usize;
        let requested = download_dir.unwrap_or_else(|| cfg.download.download_dir.clone());
        let authorized = authorize_download_dir(&cfg, &requested)?;
        (max_tasks, authorized)
    };

    let task_id = Uuid::new_v4().to_string();
    let file_name = extract_filename_from_url(&url);
    let created_at = now_iso8601();

    let task = TaskInfo {
        id: task_id.clone(),
        url: url.clone(),
        file_name,
        file_size: None,
        downloaded: 0,
        speed: 0,
        status: DownloadState::Pending,
        progress: 0.0,
        fragments_total: 0,
        fragments_done: 0,
        created_at,
    };

    // 使用互斥锁保证 check-and-insert 的原子性
    // 防止并发 create_task 请求导致去重检查和并发计数被绕过
    {
        let _create_guard = state.create_task_lock.lock().await;

        if state.tasks.iter().any(|r| {
            let t = r.value();
            t.url == url
                && t.status != DownloadState::Cancelled
                && t.status != DownloadState::Completed
                && t.status != DownloadState::Failed
        }) {
            return Err(AppError::TaskAlreadyExists(
                "相同 URL 的下载任务已存在".to_string(),
            ));
        }
        let active_count = state
            .tasks
            .iter()
            .filter(|r| {
                let t = r.value();
                t.status == DownloadState::Downloading || t.status == DownloadState::Pending
            })
            .count();
        if active_count >= max_tasks {
            return Err(AppError::Config(format!(
                "已达最大并发任务数({max_tasks}),请等待现有任务完成"
            )));
        }
        // 在锁保护下立即插入,消除竞态窗口
        state.tasks.insert(task_id.clone(), task);
    }

    if let Some(task) = state.tasks.get(&task_id).map(|r| r.value().clone()) {
        let save_path = std::path::Path::new(&download_dir_str)
            .join(&task.file_name)
            .to_string_lossy()
            .to_string();
        let snapshot =
            crate::task_store::task_info_to_snapshot(&task, save_path, 0, vec![], None, None);
        if let Err(e) = state.task_store.save_snapshot(&snapshot) {
            tracing::warn!(task_id = %task_id, error = %e, "保存初始快照失败");
        }
    }

    let download_config = {
        let cfg = state.config.lock().await;
        if cfg.download.max_concurrent_fragments == 0 {
            state.tasks.remove(&task_id);
            return Err(AppError::Config(
                "max_concurrent_fragments 不能为 0".to_string(),
            ));
        }
        build_download_config(&cfg, &download_dir_str)
    };

    let state_arc = Arc::new(AppState {
        tasks: state.tasks.clone(),
        config: state.config.clone(),
        handles: state.handles.clone(),
        active_permits: state.active_permits.clone(),
        sniffer: state.sniffer.clone(),
        sniffer_filters: state.sniffer_filters.clone(),
        progress_tx: state.progress_tx.clone(),
        connection_pool: state.connection_pool.clone(),
        controls: state.controls.clone(),
        task_store: state.task_store.clone(),
        create_task_lock: state.create_task_lock.clone(),
    });

    let (control_tx, control_rx) = watch::channel(DownloadState::Downloading);
    state.controls.insert(task_id.clone(), control_tx);

    let tid = task_id.clone();
    let url_clone = url.clone();
    let pool_clone = state_arc.connection_pool.clone();
    let mirrors = mirror_urls.filter(|v| !v.is_empty());
    let handle = tokio::spawn(async move {
        task_fn(
            state_arc,
            tid,
            url_clone,
            download_dir_str,
            download_config,
            pool_clone,
            control_rx,
            mirrors,
        )
        .await;
    });

    state.handles.insert(task_id.clone(), handle);

    tracing::info!(task_id = %task_id, "创建下载任务并启动后台下载");
    Ok(task_id)
}

pub(crate) async fn pause_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    {
        let mut task = state
            .tasks
            .get_mut(&task_id)
            .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
        match task.status {
            DownloadState::Pending | DownloadState::Downloading => {
                task.status = DownloadState::Paused;
                task.speed = 0;
                if let Some(control) = state.controls.get(&task_id) {
                    let _ = control.send(DownloadState::Paused);
                }
                tracing::info!(task_id = %task_id, "暂停任务");
            }
            other => return Err(AppError::Config(format!("当前状态 '{}' 不允许暂停", other))),
        }
    }
    persist_task_snapshot(state, &task_id, None).await;
    Ok(())
}

pub(crate) async fn resume_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    {
        let mut task = state
            .tasks
            .get_mut(&task_id)
            .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
        if task.status == DownloadState::Paused {
            task.status = DownloadState::Downloading;
            if let Some(control) = state.controls.get(&task_id) {
                let _ = control.send(DownloadState::Downloading);
            }
            tracing::info!(task_id = %task_id, "恢复任务");
        } else {
            return Err(AppError::Config(format!(
                "仅暂停状态可恢复,当前状态: '{}'",
                task.status
            )));
        }
    }
    persist_task_snapshot(state, &task_id, None).await;
    Ok(())
}

pub(crate) async fn cancel_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    {
        let mut task = state
            .tasks
            .get_mut(&task_id)
            .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
        match task.status {
            DownloadState::Completed | DownloadState::Cancelled => {
                return Err(AppError::Config(format!("任务已{},无法取消", task.status)));
            }
            _ => {
                if let Some(control) = state.controls.get(&task_id) {
                    let _ = control.send(DownloadState::Cancelled);
                }
                task.status = DownloadState::Cancelled;
                task.speed = 0;
                tracing::info!(task_id = %task_id, "取消任务");
            }
        }
    }
    persist_task_snapshot(state, &task_id, None).await;
    Ok(())
}

pub(crate) async fn delete_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let task = state
        .tasks
        .get(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    match task.status {
        DownloadState::Completed | DownloadState::Cancelled | DownloadState::Failed => {
            drop(task);
            state.tasks.remove(&task_id);
            state.handles.remove(&task_id);
            state.controls.remove(&task_id);
            tracing::info!(task_id = %task_id, "删除任务");
            Ok(())
        }
        other => Err(AppError::Config(format!(
            "当前状态 '{}' 不允许删除,请先取消任务",
            other
        ))),
    }
}

pub(crate) async fn get_task_list_inner(state: &AppState) -> Result<Vec<TaskInfo>, AppError> {
    Ok(state.tasks.iter().map(|r| r.value().clone()).collect())
}

pub(crate) async fn get_task_detail_inner(
    state: &AppState,
    task_id: String,
) -> Result<TaskInfo, AppError> {
    state
        .tasks
        .get(&task_id)
        .map(|r| r.value().clone())
        .ok_or(AppError::TaskNotFound(task_id))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::tests::test_state;
    use super::*;
    use tachyon_core::types::DownloadState;

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
        let cfg = state.config.lock().await;
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
        let task_count = state.tasks.iter().filter(|r| r.value().url == url).count();
        assert_eq!(task_count, 1, "DashMap 中应只有 1 条相同 URL 的任务");
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
            let mut cfg = state.config.lock().await;
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
            let mut cfg = state.config.lock().await;
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
        let mut rx = state.controls.get(&id).unwrap().subscribe();

        pause_task_inner(&state, id.clone()).await.unwrap();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), DownloadState::Paused);

        resume_task_inner(&state, id).await.unwrap();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), DownloadState::Downloading);
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
            update_task_status(&state.tasks, &id, DownloadState::Failed);
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
        let mut rx = state.controls.get(&id).unwrap().subscribe();

        cancel_task_inner(&state, id.clone()).await.unwrap();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), DownloadState::Cancelled);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while state.handles.contains_key(&id) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("取消后后台任务应有序退出并清理句柄");
    }
}
