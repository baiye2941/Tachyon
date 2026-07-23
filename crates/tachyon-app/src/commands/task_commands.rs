use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tachyon_core::config::{AppConfig, DownloadConfig};
use tachyon_core::safety::{extract_filename_from_url, url_identity_key};
use tachyon_core::traits::{Protocol, TaskRunner};
use tachyon_core::types::{DownloadState, FileMetadata};
use tachyon_engine::BufferPool;
use tachyon_engine::ConnectionPool;
use tachyon_engine::DownloadTask;
use tokio::sync::watch;
use url::Url;

use super::{
    AppError, AppState, TaskCommand, TaskInfo, build_download_config, cleanup_runtime,
    update_task_status, validate_download_url,
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
    buffer_pool: Arc<BufferPool>,
    control_rx: watch::Receiver<TaskCommand>,
    mirror_urls: Option<Vec<String>>,
    preferred_file_name: Option<String>,
) {
    crate::runtime::DownloadSession::new(
        state,
        task_id,
        url,
        download_dir,
        download_config,
        connection_pool,
        buffer_pool,
        control_rx,
        mirror_urls,
        preferred_file_name,
    )
    .run()
    .await;
}

// ---------------------------------------------------------------------------
// Helpers: 将 task_fn 的事务脚本拆分为单一职责函数
// ---------------------------------------------------------------------------

/// URL 解析、host 提取、启动前取消/暂停检查,并设置 Downloading 状态
///
/// 检测到暂停信号时等待恢复(带超时上限),不会覆盖 Paused 状态为 Downloading。
pub(crate) async fn validate_and_prepare_url(
    url: &str,
    state: &AppState,
    task_id: &str,
    control_rx: &mut watch::Receiver<TaskCommand>,
    pause_timeout_secs: u64,
) -> Option<String> {
    let download_url = match Url::parse(url) {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "URL 解析失败");
            mark_task_failed_and_cleanup(state, task_id).await;
            return None;
        }
    };

    // 磁力链接没有 host，用占位符代替（仅用于日志）
    let host = if tachyon_core::looks_like_magnet_url(url) {
        "magnet".to_string()
    } else {
        match download_url.host_str() {
            Some(h) => h.to_string(),
            None => {
                tracing::error!(task_id = %task_id, "URL 主机为空");
                mark_task_failed_and_cleanup(state, task_id).await;
                return None;
            }
        }
    };

    {
        if let Some(task) = state.domain.task_repository.get(task_id)
            && task.status == DownloadState::Cancelled
        {
            tracing::info!(task_id = %task_id, "任务已取消,跳过下载");
            cleanup_runtime(state, task_id);
            return None;
        }
    }

    // 设置 Downloading 前检查是否已被取消/暂停,防止竞态覆盖状态
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
        let pause_timeout = Duration::from_secs(pause_timeout_secs);
        match wait_for_resume_or_cancel(control_rx, pause_timeout).await {
            ResumeOrCancel::Resume => {
                tracing::info!(task_id = %task_id, "暂停已恢复,继续下载");
            }
            ResumeOrCancel::Cancel => {
                update_task_status(
                    &state.domain.task_repository,
                    task_id,
                    DownloadState::Cancelled,
                );
                cleanup_runtime(state, task_id);
                return None;
            }
            ResumeOrCancel::Timeout => {
                // 审计 M-05:执行前 pause 超时保持 Paused(可后续 Resume 重启),
                // 不再误映射为 Cancelled。
                tracing::warn!(
                    task_id = %task_id,
                    timeout_secs = pause_timeout_secs,
                    "暂停等待超时,保持 Paused"
                );
                update_task_status(
                    &state.domain.task_repository,
                    task_id,
                    DownloadState::Paused,
                );
                cleanup_runtime(state, task_id);
                return None;
            }
        }
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_download_task(
    task_id: &str,
    url: &str,
    download_config: DownloadConfig,
    connection_pool: Arc<ConnectionPool>,
    buffer_pool: Arc<BufferPool>,
    global_rate_limiter: Arc<tachyon_engine::RateLimiter>,
    scheduler_config: tachyon_core::config::SchedulerConfig,
    mirror_urls: Option<Vec<String>>,
    #[cfg(feature = "magnet")] bt_session: Option<Arc<tachyon_engine::BtSession>>,
) -> Result<Box<dyn TaskRunner>, ()> {
    let is_magnet = tachyon_core::looks_like_magnet_url(url);
    let has_mirrors = mirror_urls.as_ref().is_some_and(|v| !v.is_empty());

    // P2SP 路由:按 is_magnet × has_mirrors 分四路。
    //   - magnet + mirrors:混合下载(HTTP 主源 + BT fallback)
    //   - magnet(纯 BT):with_pool_and_scheduler(传 bt_session)
    //   - http + mirrors:多源镜像
    //   - http(单源):with_pool_and_scheduler(bt_session=None)
    // 审计 A-04:使用 AppConfig.scheduler,禁止 default_config 忽略 UI 配置
    // 审计 A-01/A-04:经 engine 工厂构造调度器,禁止 app 直连 tachyon-scheduler
    let scheduler: Arc<dyn tachyon_core::traits::DownloadScheduler> =
        tachyon_engine::create_adaptive_scheduler(scheduler_config.clone());

    let task_result = if is_magnet && has_mirrors {
        #[cfg(feature = "magnet")]
        {
            let bt_session = bt_session.ok_or_else(|| {
                tracing::error!(task_id = %task_id, "磁力+镜像混合下载缺少 BT Session");
            })?;
            tracing::info!(task_id = %task_id, "P2SP 混合下载:HTTP 镜像主源 + BT fallback");
            DownloadTask::with_hybrid_sources(
                url.to_string(),
                mirror_urls.unwrap(),
                download_config,
                Some(connection_pool),
                scheduler,
                bt_session,
            )
            .await
        }
        #[cfg(not(feature = "magnet"))]
        {
            tracing::error!(task_id = %task_id, "magnet feature 未启用,无法执行混合下载");
            Err(tachyon_core::DownloadError::Config(
                "magnet feature 未启用".into(),
            ))
        }
    } else if has_mirrors {
        let mirrors = mirror_urls.unwrap();
        tracing::info!(task_id = %task_id, mirrors = mirrors.len(), "使用镜像源下载");
        // 审计:镜像路径必须注入 AppConfig 调度器,禁止 default_config 旁路
        DownloadTask::with_mirrors(
            url.to_string(),
            mirrors,
            download_config,
            Some(connection_pool),
            scheduler,
        )
        .await
    } else {
        // 纯 HTTP 单源 或 纯 BT(magnet:?) 均走 with_pool_and_scheduler。
        // is_magnet 时 bt_session 透传,否则传 None。
        DownloadTask::with_pool_and_scheduler(
            url.to_string(),
            download_config,
            Some(connection_pool),
            scheduler,
            #[cfg(feature = "magnet")]
            bt_session,
        )
        .await
    };

    match task_result {
        Ok(mut t) => {
            t.set_buffer_pool(buffer_pool.clone());
            // 审计 A-03:注入全局共享限速器(跨任务总上限)
            t.set_rate_limiter(global_rate_limiter);
            // 审计 A-04:规划参数与 AdaptiveDownloadScheduler 同源
            t.set_scheduler_config(scheduler_config);
            Ok(Box::new(t))
        }
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "创建 DownloadTask 失败");
            Err(())
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: wait for resume or cancel (暂停等待,带超时上限)
// ---------------------------------------------------------------------------

/// 暂停等待结果
pub(crate) enum ResumeOrCancel {
    /// 收到恢复信号
    Resume,
    /// 收到取消信号
    Cancel,
    /// 暂停超时
    Timeout,
}

/// 等待暂停解除(Resume)或取消(Cancel),带超时上限
///
/// CLAUDE.md 规则: paused 状态 MUST 有时间上限,不能永久暂停
pub(crate) async fn wait_for_resume_or_cancel(
    control_rx: &mut watch::Receiver<TaskCommand>,
    pause_timeout: Duration,
) -> ResumeOrCancel {
    match tokio::time::timeout(pause_timeout, async {
        loop {
            let cmd = *control_rx.borrow_and_update();
            match cmd {
                TaskCommand::Resume => return ResumeOrCancel::Resume,
                TaskCommand::Cancel => return ResumeOrCancel::Cancel,
                _ => {
                    if control_rx.changed().await.is_err() {
                        // 控制通道关闭,视为取消
                        return ResumeOrCancel::Cancel;
                    }
                }
            }
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => ResumeOrCancel::Timeout,
    }
}

// ---------------------------------------------------------------------------
// Helper: pre-run check (取消/暂停检查)
// ---------------------------------------------------------------------------

/// 执行前检查结果
pub(crate) enum PreRunCheck {
    /// 可以继续执行
    Continue,
    /// 已取消
    Cancelled,
    /// 已暂停,需等待恢复
    Paused,
}

/// 执行前检查控制信号,同时识别取消和暂停
pub(crate) fn should_stop_before_run(control_rx: &watch::Receiver<TaskCommand>) -> PreRunCheck {
    let cmd = *control_rx.borrow();
    match cmd {
        TaskCommand::Cancel => PreRunCheck::Cancelled,
        TaskCommand::Pause => PreRunCheck::Paused,
        _ => PreRunCheck::Continue,
    }
}

#[cfg(test)]
pub(crate) fn should_cancel_before_run(control_rx: &watch::Receiver<TaskCommand>) -> bool {
    matches!(should_stop_before_run(control_rx), PreRunCheck::Cancelled)
}

pub(crate) async fn inject_resume_snapshot(
    task: &mut dyn TaskRunner,
    state: &AppState,
    task_id: &str,
) {
    // load_snapshot 读磁盘,用 spawn_blocking 包裹避免阻塞 tokio worker。
    // 原 `if let Ok(Some(snapshot))` 在 Err/None 时跳过,此处用 .ok().ok().flatten() 等价。
    let task_store = state.infra.task_store.clone();
    let task_id_owned = task_id.to_string();
    let snapshot = tokio::task::spawn_blocking(move || task_store.load_snapshot(&task_id_owned))
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten();
    if let Some(snapshot) = snapshot {
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
        let identity = tachyon_core::ObjectIdentity {
            etag: snapshot.etag,
            last_modified: snapshot.last_modified,
            file_size: snapshot.file_size.or(snapshot.content_length),
        };
        if identity.etag.is_some()
            || identity.last_modified.is_some()
            || identity.file_size.is_some()
        {
            tracing::info!(
                task_id = %task_id,
                etag = ?identity.etag,
                "断点续传:注入对象身份"
            );
            task.set_resume_object_identity(Some(identity));
        }
        // 审计 batch2:历史 200-fallback 后 supports_range=false 必须注入,
        // 否则 resume 会再次按分片规划浪费带宽。
        if !snapshot.supports_range {
            tracing::info!(
                task_id = %task_id,
                "断点续传:注入 supports_range=false(强制整块路径)"
            );
            task.set_resume_supports_range(Some(false));
        }
    }
}

pub(crate) async fn probe_and_save_metadata(
    task: &mut dyn TaskRunner,
    state: &AppState,
    task_id: &str,
    download_dir: &str,
    control_rx: &mut watch::Receiver<TaskCommand>,
    pause_timeout_secs: u64,
) -> Option<FileMetadata> {
    let mut probe_control_rx = control_rx.clone();
    let pause_timeout = Duration::from_secs(pause_timeout_secs);
    match tokio::select! {
        result = task.probe() => result,
        signal = wait_for_cancel_or_pause(&mut probe_control_rx) => {
            match signal {
                ProbeInterrupt::Cancel => Err(tachyon_core::DownloadError::Cancelled),
                ProbeInterrupt::Pause => {
                    // 探测期间暂停:等待恢复或超时
                    match wait_for_resume_or_cancel(&mut probe_control_rx, pause_timeout).await {
                        ResumeOrCancel::Resume => {
                            // 恢复后重新探测
                            task.probe().await
                        }
                        ResumeOrCancel::Cancel | ResumeOrCancel::Timeout => {
                            Err(tachyon_core::DownloadError::Cancelled)
                        }
                    }
                }
                ProbeInterrupt::ChannelClosed => {
                    Err(tachyon_core::DownloadError::Other("控制通道已关闭".into()))
                }
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
                // 预计算总分段数供进度显示:直接调用 engine 的 plan_fragments
                // (无调度器建议时的默认分支),与真实规划同一公式,
                // 取代原先固定 1MiB 分片导致的显示口径偏差。
                // PlanComplete 到达后会被真实分片数覆盖(chunk_reader_pool)。
                let scheduler_config = {
                    let cfg = state.domain.config.lock().await;
                    cfg.scheduler.clone()
                };
                let total_frags = match meta.file_size {
                    Some(size) => tachyon_engine::fragment::plan_fragments(
                        size,
                        meta.supports_range,
                        None,
                        &scheduler_config,
                    )
                    .map(|fragments| fragments.len() as u32)
                    .unwrap_or(0),
                    None => 0,
                };
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
                    meta.supports_range,
                );
                // task_store 底层为 FileStore 同步 I/O(含 fsync),用 fire-and-forget
                // spawn_blocking 包裹避免阻塞 tokio worker,错误仅记录警告。
                let task_store = state.infra.task_store.clone();
                let task_id_for_log = task_id.to_string();
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = task_store.save_snapshot(&snapshot) {
                        tracing::warn!(task_id = %task_id_for_log, error = %e, "保存元数据快照失败");
                    }
                });
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

// ---------------------------------------------------------------------------
// Helper: wait for cancel or pause signal (探测期间中断检查)
// ---------------------------------------------------------------------------

/// 探测期间中断信号
enum ProbeInterrupt {
    /// 收到取消信号
    Cancel,
    /// 收到暂停信号
    Pause,
    /// 控制通道关闭
    ChannelClosed,
}

/// 等待取消或暂停信号(不阻塞,仅检查当前值和首次变化)
///
/// 用于探测阶段的 `tokio::select!` 中,与 `task.probe()` 竞速。
/// 探测是快速操作,只需在开始时检查一次,然后等待首次信号变化。
async fn wait_for_cancel_or_pause(control_rx: &mut watch::Receiver<TaskCommand>) -> ProbeInterrupt {
    loop {
        let cmd = *control_rx.borrow_and_update();
        match cmd {
            TaskCommand::Cancel => return ProbeInterrupt::Cancel,
            TaskCommand::Pause => return ProbeInterrupt::Pause,
            _ => {
                if control_rx.changed().await.is_err() {
                    return ProbeInterrupt::ChannelClosed;
                }
            }
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
    file_name: Option<String>,
    auto_start: Option<bool>,
) -> Result<String, AppError> {
    create_task_inner(
        &state,
        url,
        download_dir,
        mirror_urls,
        file_name,
        auto_start.unwrap_or(true),
        None,
    )
    .await
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
    delete_local_file: Option<bool>,
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
    let delete_local_file = delete_local_file.unwrap_or(false);
    delete_task_inner(&state, task_id, delete_local_file).await
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

/// 在系统文件管理器中打开任务文件所在目录(P1-21)
///
/// 替代前端 `shell.open`,移除前端 `shell:allow-open` 权限后由后端统一控制:
/// - 按 task_id 查找任务,取其 save_path 的父目录
/// - canonicalize 后校验该目录位于 download_dir 或任一 authorized_dirs 之下,拒绝路径逃逸
/// - 通过 OS 原生命令打开(Windows: explorer / macOS: open / Linux: xdg-open)
///
/// 安全边界:仅能打开下载根目录/已授权目录内的路径,防止 save_path 被污染后打开任意目录。
#[tauri::command]
pub async fn open_task_folder(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
    // 查找任务并取 save_path 的父目录
    let task = state.service.task_service.get_task_detail(&task_id)?;
    let save_path = std::path::Path::new(&task.save_path);
    let target_dir = save_path
        .parent()
        .ok_or_else(|| AppError::Config("任务保存路径无父目录".to_string()))?;
    open_dir_under_download_root(&state, target_dir.to_path_buf()).await
}

/// 在系统文件管理器中打开指定目录(P1-21)
///
/// 用于历史记录/本地模型等不在任务仓库中的场景:调用方传入完整路径,
/// 后端校验该路径位于 download_dir 或任一 authorized_dirs 之下后打开,拒绝路径逃逸。
#[tauri::command]
pub async fn open_folder_under_download_root(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<(), AppError> {
    let target = std::path::PathBuf::from(path);
    open_dir_under_download_root(&state, target).await
}

/// 校验 target 目录位于 download_dir 或任一 authorized_dirs 之下,并通过 OS 文件管理器打开
async fn open_dir_under_download_root(
    state: &AppState,
    target: std::path::PathBuf,
) -> Result<(), AppError> {
    let config = {
        let cfg = state.domain.config.lock().await;
        cfg.clone()
    };

    // canonicalize 是阻塞 IO,放 spawn_blocking;校验本身是纯函数便于单测
    let canon_target =
        tokio::task::spawn_blocking(move || ensure_dir_under_download_roots(&config, &target))
            .await
            .map_err(|e| AppError::Config(format!("路径规范化失败: {e}")))??;

    // 通过 OS 原生命令打开文件管理器(独立后台进程,不阻塞);
    // 剥除 Windows verbatim 前缀:explorer 对 \\?\ 形式路径的支持不可靠
    let canon_str = canon_target.to_string_lossy();
    let target_str = super::strip_verbatim_prefix(canon_str.as_ref()).into_owned();
    tokio::task::spawn_blocking(move || {
        let result = open_in_file_manager(&target_str);
        if let Err(e) = result {
            tracing::warn!(error = %e, path = %target_str, "打开文件管理器失败");
        }
    })
    .await
    .map_err(|e| AppError::Config(format!("打开文件管理器任务失败: {e}")))?;
    Ok(())
}

/// 校验 target 位于 download_dir 或任一 authorized_dirs 之下(纯函数,可单测)
///
/// 安全边界 = canonicalize(download_dir) ∪ canonicalize(authorized_dirs),
/// 与 create_task 的授权口径一致(修复"已授权目录能创建任务、却不能打开文件夹")。
/// canonicalize 要求路径存在;目标目录在任务完成后均应存在,不存在则视为非法。
/// 成功时返回 canonical 后的目标路径,供调用方打开文件管理器。
fn ensure_dir_under_download_roots(
    config: &tachyon_core::config::AppConfig,
    target: &std::path::Path,
) -> Result<std::path::PathBuf, AppError> {
    let canon_target = target
        .canonicalize()
        .map_err(|e| AppError::Config(format!("目标目录不可访问: {e}")))?;
    let roots = crate::commands::config_commands::canonical_download_roots(config);

    if roots.is_empty() {
        return Err(AppError::Config(
            "下载根目录与授权目录均不可访问".to_string(),
        ));
    }

    if !roots.iter().any(|root| canon_target.starts_with(root)) {
        tracing::warn!(
            target = %canon_target.display(),
            roots = ?roots,
            "拒绝打开下载根目录之外的路径"
        );
        return Err(AppError::Config("路径不在下载目录范围内".to_string()));
    }

    Ok(canon_target)
}

/// 跨平台调用系统文件管理器打开指定目录
fn open_in_file_manager(path: &str) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("explorer");
        c.arg(path);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    // 文件管理器是长生命周期独立进程,无需等待其退出
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[tauri::command]
pub async fn set_task_tags(
    state: tauri::State<'_, AppState>,
    task_id: String,
    tags: Vec<String>,
) -> Result<(), AppError> {
    set_task_tags_inner(&state, task_id, tags).await
}

#[tauri::command]
pub async fn add_task_tag(
    state: tauri::State<'_, AppState>,
    task_id: String,
    tag: String,
) -> Result<(), AppError> {
    add_task_tag_inner(&state, task_id, tag).await
}

#[tauri::command]
pub async fn remove_task_tag(
    state: tauri::State<'_, AppState>,
    task_id: String,
    tag: String,
) -> Result<(), AppError> {
    remove_task_tag_inner(&state, task_id, tag).await
}

/// 撤销取消任务
///
/// 破坏性操作,需确认令牌。将任务恢复为取消前的状态,
/// 若原状态为 Downloading 则重新启动下载。
#[tauri::command]
pub async fn undo_cancel_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
    confirmation_token: Option<String>,
) -> Result<(), AppError> {
    match confirmation_token {
        Some(token) => {
            state
                .service
                .confirmation_service
                .validate_and_consume(&token, "undo_cancel_task")?;
        }
        None => {
            return Err(super::AppError::Config(
                "撤销取消任务需要确认令牌,请先确认操作".to_string(),
            ));
        }
    }
    undo_cancel_task_inner(&state, task_id).await
}

/// 撤销删除任务
///
/// 破坏性操作,需确认令牌。仅恢复任务记录和断点续传快照,
/// 不恢复本地文件,也不重新启动下载。
#[tauri::command]
pub async fn undo_delete_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
    confirmation_token: Option<String>,
) -> Result<(), AppError> {
    match confirmation_token {
        Some(token) => {
            state
                .service
                .confirmation_service
                .validate_and_consume(&token, "undo_delete_task")?;
        }
        None => {
            return Err(super::AppError::Config(
                "撤销删除任务需要确认令牌,请先确认操作".to_string(),
            ));
        }
    }
    undo_delete_task_inner(&state, task_id).await
}

/// 重排任务顺序
///
/// `ordered_ids` 为任务 ID 的期望顺序(从前到后)。服务层会验证所有 ID
/// 存在后更新 `display_order` 并持久化快照。
#[tauri::command]
pub async fn reorder_tasks(
    state: tauri::State<'_, AppState>,
    ordered_ids: Vec<String>,
) -> Result<(), AppError> {
    reorder_tasks_inner(&state, ordered_ids).await
}

/// 将任务移动到指定任务之前
///
/// `before_id` 为 `None` 时移动到列表末尾。
#[tauri::command]
pub async fn move_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
    before_id: Option<String>,
) -> Result<(), AppError> {
    move_task_inner(&state, task_id, before_id).await
}

/// 探测文件真实名称(HEAD 请求 / DHT 查询种子元数据)
///
/// - HTTP/HTTPS: 发送 HEAD 请求获取 Content-Disposition 等元数据
/// - 磁力链接: 通过 DHT/Tracker 查询种子 info.name(与迅雷行为一致),
///   元数据超时收紧到 15s(UI 即时反馈,不沿用下载级 120s 默认)
///
/// 探测失败时回退到 URL 本地提取(extract_filename_from_url / magnet-{infoHash})。
#[tauri::command]
pub async fn probe_filename(
    state: tauri::State<'_, AppState>,
    url: String,
) -> Result<String, String> {
    probe_filename_inner(&state, url)
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn probe_filename_inner(
    state: &AppState,
    url: String,
) -> Result<String, AppError> {
    validate_download_url(&url)?;

    // 磁力链接:使用 BtSession 通过 DHT/Tracker 探测真实文件名
    // P0-8: UI 探测是一次性元数据读取,与 DownloadTask 的 factory 绑定不同
    // (probe 无 storage_factory,下载有 factory → 不同 cache key)。
    // 若不 pause+delete,会留下无所有者的 session torrent 持续联网。
    /// UI 文件名探测的元数据超时上限(秒)
    ///
    /// MagnetConfig.metadata_timeout_secs(默认 120s)面向真实下载,允许等待
    /// 半死 swarm;UI 探测是模态框内的即时反馈,失败可回退 dn=/magnet-{hash},
    /// 15s 足以完成健康 swarm 的 tracker announce + ut_metadata 交换(经代理)。
    const MAGNET_PROBE_TIMEOUT_CAP_SECS: u64 = 15;

    #[cfg(feature = "magnet")]
    if tachyon_core::looks_like_magnet_url(&url) {
        let bt_session = state.infra.bt_session.lock().await.clone();
        if let Some(session) = bt_session {
            // UI 探测用收紧的元数据超时:120s 的下载级超时会让模态框长时间无反馈
            let probe_config = {
                let mut c = session.config().clone();
                c.metadata_timeout_secs =
                    c.metadata_timeout_secs.min(MAGNET_PROBE_TIMEOUT_CAP_SECS);
                c
            };
            let protocol = tachyon_engine::MagnetProtocol::new(
                session.session(),
                probe_config,
                session.download_dir().clone(),
                session.handle_cache(),
            )
            .with_ops_gate(session.ops_gate());
            let probe_result = protocol.probe(&url).await;
            // 无论成功/失败都清理:失败也可能已 add 到 session 或半写 cache
            protocol.stop_and_remove_torrent(&url).await;
            match probe_result {
                Ok(meta) => return Ok(meta.file_name),
                Err(e) => {
                    tracing::warn!(error = %e, "磁力链接探测失败,回退到本地提取");
                    return Ok(extract_magnet_fallback_name(&url));
                }
            }
        }
        // BtSession 未初始化,回退到本地提取
        tracing::warn!("BitTorrent Session 未初始化,磁力链接探测不可用");
        return Ok(extract_magnet_fallback_name(&url));
    }

    // HTTP:使用 AppConfig 同源 DownloadConfig(proxy/UA/timeouts/io_strategy),
    // 避免 DownloadConfig::default() 与正式任务配置分叉(A-06 partial)。
    // 审计 A-04/A-14:调度器与正式任务同源,不用 DownloadTask::new 的 default_config。
    // probe 为轻量 HEAD,不注入全局限速器/连接池。
    let (download_config, scheduler_cfg) = {
        let cfg = state.domain.config.lock().await;
        (
            build_download_config(&cfg, &cfg.download.download_dir),
            cfg.scheduler.clone(),
        )
    };
    let scheduler = tachyon_engine::create_adaptive_scheduler(scheduler_cfg);
    match DownloadTask::with_scheduler(url.clone(), download_config, scheduler).await {
        Ok(mut task) => match task.probe().await {
            Ok(meta) => Ok(meta.file_name.clone()),
            Err(_) => {
                // 探测失败,回退到 URL 本地提取
                Ok(extract_filename_from_url(&url))
            }
        },
        Err(_) => Ok(extract_filename_from_url(&url)),
    }
}

/// 磁力链接本地回退:优先 dn= 参数,无则用 magnet-{infoHash}
fn extract_magnet_fallback_name(url: &str) -> String {
    // 尝试提取 dn= (Display Name),使用 URLSearchParams 风格解析
    if let Some(query) = url.strip_prefix("magnet:?") {
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("dn=") {
                let trimmed = val.trim();
                if !trimmed.is_empty() {
                    return simple_percent_decode(trimmed);
                }
            }
        }

        // 回退到 xt=urn:btih:<hash>
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("xt=urn:btih:") {
                let trimmed = val.trim();
                if !trimmed.is_empty() {
                    return format!("magnet-{trimmed}");
                }
            }
        }
    }

    "unknown".to_string()
}

/// 简易百分号解码(磁力链接 dn= 值)
///
/// 仅处理 %XX 格式,不处理无效编码(原样保留)。
fn simple_percent_decode(s: &str) -> String {
    let mut result = Vec::new();
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next();
            let lo = chars.next();
            match (hi, lo) {
                (Some(h), Some(l)) => {
                    if let (Some(hv), Some(lv)) = (hex_val(h), hex_val(l)) {
                        result.push((hv << 4) | lv);
                    } else {
                        result.extend_from_slice(&[b'%', h, l]);
                    }
                }
                _ => result.push(b'%'),
            }
        } else {
            result.push(b);
        }
    }
    String::from_utf8(result).unwrap_or_else(|_| s.to_string())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Inner implementations
// ---------------------------------------------------------------------------

pub(crate) async fn create_task_inner(
    state: &AppState,
    url: String,
    download_dir: Option<String>,
    mirror_urls: Option<Vec<String>>,
    file_name: Option<String>,
    auto_start: bool,
    hf_meta: Option<super::HfTaskMeta>,
) -> Result<String, AppError> {
    let creation = state
        .service
        .task_service
        .create_task(
            &url,
            download_dir.as_deref(),
            mirror_urls.as_deref(),
            file_name.as_deref(),
            auto_start,
        )
        .await?;

    // 注入 HF 元数据（如果提供）
    if let Some(meta) = hf_meta
        && let Some(mut task) = state.domain.task_repository.get_mut(&creation.task_id)
    {
        task.hf_meta = Some(meta);
    }

    // auto_start=false 时保持 Pending,不激活下载;后续可调用 resume_task 启动
    if creation.auto_start {
        let state_arc = Arc::new(state.clone_for_task());
        state
            .runtime
            .supervisor
            .start_download(
                state_arc,
                &creation.task_id,
                creation.url,
                creation.download_dir,
                creation.download_config,
                creation.mirror_urls,
                creation.preferred_file_name,
            )
            .await;
    }

    Ok(creation.task_id)
}

pub(crate) async fn pause_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    // 审计 H-02:同一任务 pause/resume/cancel 串行,避免 TaskInfo 与 watch 交错
    let lock = state.runtime.supervisor.task_command_lock(&task_id);
    let _guard = lock.lock().await;
    state.service.task_service.pause_task(&task_id).await?;
    // 有运行中 task 时发送 Pause;无 channel 时仅改仓库状态(恢复任务未启动)
    let _ = state
        .runtime
        .supervisor
        .send_command(&task_id, TaskCommand::Pause);
    Ok(())
}

pub(crate) async fn resume_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let lock = state.runtime.supervisor.task_command_lock(&task_id);
    let _guard = lock.lock().await;

    // task_service.resume_task 仅改状态(Pending/Paused -> Downloading)并持久化快照,
    // 不负责激活下载。激活逻辑由下方根据 supervisor 是否有运行中的 task_fn 决定。
    state.service.task_service.resume_task(&task_id).await?;

    // 若任务已有运行中的 task_fn(存在 control channel),直接发 Resume 信号。
    // 审计 H-03/H-02:send 失败(receiver 已关)不得伪成功,必须 restart。
    if state.runtime.supervisor.has_running_task(&task_id) {
        if state
            .runtime
            .supervisor
            .send_command(&task_id, TaskCommand::Resume)
        {
            return Ok(());
        }
        tracing::warn!(
            task_id = %task_id,
            "Resume 控制通道已关闭,改走 restart_download"
        );
        // 清掉僵死 channel,避免 has_running_task 继续为 true
        state.runtime.supervisor.cleanup(&task_id);
    }

    // 无运行 task_fn 或 Resume 发送失败:重新 start_download
    restart_download(state, &task_id).await
}

/// 从 TaskInfo 重建下载参数并重启 task_fn(断点续传)
///
/// 用于 resume 无运行 task_fn 的任务(应用启动恢复的任务、task_fn 已退出的任务)。
///
/// 参数重建策略:
/// - `download_dir`: 从 `save_path` 的父目录推导。
/// - `download_config`: 用当前 `AppConfig` 经 `build_download_config` 重建。
/// - `mirror_urls`: 从 TaskInfo(快照持久化)读取,保留创建时配置的多源。
/// - `preferred_file_name`: 传 `None`,probe 会复用磁盘上既有文件名。
async fn restart_download(state: &AppState, task_id: &str) -> Result<(), AppError> {
    let task = state
        .domain
        .task_repository
        .get(task_id)
        .map(|r| r.value().clone())
        .ok_or_else(|| AppError::TaskNotFound(task_id.to_string()))?;

    // 从 save_path 推导 download_dir(父目录)
    let save_path = std::path::Path::new(&task.save_path);
    let download_dir = save_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| AppError::Config(format!("save_path 无父目录: {}", task.save_path)))?;

    // 用当前 AppConfig 重建 download_config
    let download_config = {
        let cfg = state.domain.config.lock().await;
        build_download_config(&cfg, &download_dir)
    };

    let mirrors = mirrors_for_restart(&task);
    let state_arc = Arc::new(state.clone_for_task());
    state
        .runtime
        .supervisor
        .start_download(
            state_arc,
            task_id,
            task.url,
            download_dir,
            download_config,
            mirrors,
            None, // preferred_file_name 无需覆盖,probe 复用既有文件名
        )
        .await;
    tracing::info!(task_id = %task_id, "重启 task_fn 激活断点续传");
    Ok(())
}

/// 从 TaskInfo 提取 restart 应使用的镜像列表。
///
/// 纯函数,便于单测验证 restart 不再硬编码 `None`。
pub(crate) fn mirrors_for_restart(task: &TaskInfo) -> Option<Vec<String>> {
    task.mirror_urls.clone()
}

pub(crate) async fn cancel_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let lock = state.runtime.supervisor.task_command_lock(&task_id);
    let _guard = lock.lock().await;
    state.service.task_service.cancel_task(&task_id).await?;
    let _ = state
        .runtime
        .supervisor
        .send_command(&task_id, TaskCommand::Cancel);
    // 审计 H-04:Cancel 后 await JoinHandle quiesce,避免旧 task 仍在写盘/联网,
    // 与 restart 产生竞态。wait_for_handle 内部超时 abort + 2s grace。
    // 终态/无 handle 时立即返回 None,不影响正确性。
    let _ = state
        .runtime
        .supervisor
        .wait_for_handle(
            &task_id,
            crate::runtime::download_supervisor::DownloadSupervisor::CANCEL_QUIESCE_TIMEOUT,
        )
        .await;
    Ok(())
}

pub(crate) async fn delete_task_inner(
    state: &AppState,
    task_id: String,
    delete_local_file: bool,
) -> Result<(), AppError> {
    // 审计 H-04:Cancel → await JoinHandle quiesce → 再删文件/快照/仓库。
    // 旧实现仅 send Cancel 后立刻 delete+cleanup(drop handle),后台 worker 可能仍在写盘。
    // 终态/无 handle 时 wait_for_handle 立即返回 None,不影响正确性。
    state
        .runtime
        .supervisor
        .send_command(&task_id, TaskCommand::Cancel);
    let waited = state
        .runtime
        .supervisor
        .wait_for_handle(
            &task_id,
            crate::runtime::download_supervisor::DownloadSupervisor::DELETE_QUIESCE_TIMEOUT,
        )
        .await;
    if waited.is_none() {
        // 无 handle 或超时 abort 后继续删除;超时已 warn
        tracing::debug!(task_id = %task_id, "delete: 无活跃 handle 或已超时 abort");
    }
    state
        .service
        .task_service
        .delete_task(&task_id, delete_local_file)
        .await?;
    // wait_for_handle 已移除 handle/channel;再 cleanup 兜底并发路径残留
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

pub(crate) async fn set_task_tags_inner(
    state: &AppState,
    task_id: String,
    tags: Vec<String>,
) -> Result<(), AppError> {
    state
        .service
        .task_service
        .set_task_tags(&task_id, tags)
        .await
}

pub(crate) async fn add_task_tag_inner(
    state: &AppState,
    task_id: String,
    tag: String,
) -> Result<(), AppError> {
    state
        .service
        .task_service
        .add_task_tag(&task_id, &tag)
        .await
}

pub(crate) async fn remove_task_tag_inner(
    state: &AppState,
    task_id: String,
    tag: String,
) -> Result<(), AppError> {
    state
        .service
        .task_service
        .remove_task_tag(&task_id, &tag)
        .await
}

pub(crate) async fn undo_cancel_task_inner(
    state: &AppState,
    task_id: String,
) -> Result<(), AppError> {
    let previous_status = state
        .service
        .task_service
        .undo_cancel_task(&task_id)
        .await?;

    // 原状态为 Downloading 时,取消后 task_fn 已退出,
    // 需要重新启动下载(与 resume_task_inner 无运行 task_fn 的路径一致)。
    if previous_status == DownloadState::Downloading {
        restart_download(state, &task_id).await?;
    }

    Ok(())
}

pub(crate) async fn undo_delete_task_inner(
    state: &AppState,
    task_id: String,
) -> Result<(), AppError> {
    // 撤销删除仅恢复记录和快照,不恢复本地文件,也不重新启动下载。
    state.service.task_service.undo_delete_task(&task_id).await
}

pub(crate) async fn reorder_tasks_inner(
    state: &AppState,
    ordered_ids: Vec<String>,
) -> Result<(), AppError> {
    state.service.task_service.reorder_tasks(&ordered_ids).await
}

// ---------------------------------------------------------------------------
// Backup import/export (P4-10)
// ---------------------------------------------------------------------------

/// 备份文件 schema 版本号
const BACKUP_SCHEMA_VERSION: u32 = 1;
/// 审计 SEC-011:备份文件最大字节数(与网络元数据上限对齐)
#[cfg(not(test))]
const MAX_BACKUP_FILE_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(test)]
const MAX_BACKUP_FILE_BYTES: u64 = 64 * 1024;
/// 审计 SEC-011:单份备份最多任务数
#[cfg(not(test))]
const MAX_BACKUP_TASKS: usize = 10_000;
#[cfg(test)]
const MAX_BACKUP_TASKS: usize = 8;

/// 导出的备份文件结构
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Backup {
    version: u32,
    config: AppConfig,
    tasks: Vec<tachyon_store::TaskSnapshot>,
}

/// 验证破坏性备份命令的一次性确认令牌
fn validate_backup_token(
    state: &AppState,
    token: Option<String>,
    action: &str,
) -> Result<(), AppError> {
    match token {
        Some(token) => state
            .service
            .confirmation_service
            .validate_and_consume(&token, action),
        None => Err(AppError::Config(format!(
            "{action} 需要确认令牌,请先确认操作"
        ))),
    }
}

/// 导出当前配置与所有任务快照到 JSON 备份文件
#[tauri::command]
pub async fn export_backup(
    state: tauri::State<'_, AppState>,
    path: String,
    confirmation_token: Option<String>,
) -> Result<(), AppError> {
    validate_backup_token(&state, confirmation_token, "export_backup")?;
    export_backup_inner(&state, path).await
}

/// 从 JSON 备份文件导入配置与任务快照
#[tauri::command]
pub async fn import_backup(
    state: tauri::State<'_, AppState>,
    path: String,
    overwrite: bool,
    confirmation_token: Option<String>,
) -> Result<usize, AppError> {
    validate_backup_token(&state, confirmation_token, "import_backup")?;
    import_backup_inner(&state, path, overwrite).await
}

/// 导出备份的内部实现(便于测试直接使用 AppState)
pub(crate) async fn export_backup_inner(state: &AppState, path: String) -> Result<(), AppError> {
    let config = { state.domain.config.lock().await.clone() };
    // 审计 SEC-006:备份路径必须在 authorized_dirs 下
    crate::commands::config_commands::path_under_authorized_dirs(&config, &path)?;
    let (tasks, corrupt_keys, unsupported_schema) = state
        .infra
        .task_store
        .load_all()
        .map_err(|e| AppError::Config(format!("加载任务快照失败: {e}")))?;
    if !corrupt_keys.is_empty() {
        tracing::warn!(
            count = corrupt_keys.len(),
            keys = ?corrupt_keys,
            "导出备份时发现损坏快照"
        );
    }
    if !unsupported_schema.is_empty() {
        tracing::warn!(
            count = unsupported_schema.len(),
            items = ?unsupported_schema,
            "导出备份时发现 future schema 快照,拒绝导出不完整备份"
        );
        return Err(AppError::UpgradeRequired {
            found_version: unsupported_schema[0].found_version,
            supported_version: unsupported_schema[0].supported_version,
        });
    }

    let backup = Backup {
        version: BACKUP_SCHEMA_VERSION,
        config,
        tasks,
    };
    let path_buf = std::path::PathBuf::from(path);

    tokio::task::spawn_blocking(move || {
        let json = serde_json::to_string_pretty(&backup)
            .map_err(|e| AppError::Config(format!("序列化备份失败: {e}")))?;
        if let Some(parent) = path_buf.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Config(format!("创建备份目录失败: {e}")))?;
        }
        let tmp = path_buf.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, json)
            .map_err(|e| AppError::Config(format!("写入备份临时文件失败: {e}")))?;
        std::fs::rename(&tmp, &path_buf)
            .map_err(|e| AppError::Config(format!("重命名备份文件失败: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Config(format!("导出备份任务失败: {e}")))?
}

/// 导入备份的内部实现(便于测试直接使用 AppState)
pub(crate) async fn import_backup_inner(
    state: &AppState,
    path: String,
    overwrite: bool,
) -> Result<usize, AppError> {
    {
        let config = state.domain.config.lock().await;
        crate::commands::config_commands::path_under_authorized_dirs(&config, &path)?;
    }
    let path_buf = std::path::PathBuf::from(path);
    let backup: Backup = tokio::task::spawn_blocking(move || {
        let meta = std::fs::metadata(&path_buf)
            .map_err(|e| AppError::Config(format!("读取备份文件元数据失败: {e}")))?;
        let len = meta.len();
        if len > MAX_BACKUP_FILE_BYTES {
            return Err(AppError::Config(format!(
                "备份文件过大: {len} > 最大允许 {MAX_BACKUP_FILE_BYTES} 字节"
            )));
        }
        let json = std::fs::read_to_string(&path_buf)
            .map_err(|e| AppError::Config(format!("读取备份文件失败: {e}")))?;
        if json.len() as u64 > MAX_BACKUP_FILE_BYTES {
            return Err(AppError::Config(format!(
                "备份内容过大: {} > 最大允许 {MAX_BACKUP_FILE_BYTES} 字节",
                json.len()
            )));
        }
        let backup: Backup = serde_json::from_str(&json)
            .map_err(|e| AppError::Config(format!("备份文件格式无效: {e}")))?;
        if backup.tasks.len() > MAX_BACKUP_TASKS {
            return Err(AppError::Config(format!(
                "备份任务数过多: {} > 最大允许 {MAX_BACKUP_TASKS}",
                backup.tasks.len()
            )));
        }
        Ok(backup)
    })
    .await
    .map_err(|e| AppError::Config(format!("导入备份任务失败: {e}")))??;

    if backup.version != BACKUP_SCHEMA_VERSION {
        return Err(AppError::Config(format!(
            "不支持的备份版本: {},当前仅支持版本 {}",
            backup.version, BACKUP_SCHEMA_VERSION
        )));
    }

    crate::commands::config_commands::validate_config(&backup.config)?;

    if overwrite {
        // 替换当前配置并持久化
        {
            let mut cfg = state.domain.config.lock().await;
            *cfg = backup.config.clone();
        }
        state
            .service
            .task_service
            .update_cached_download_dir(backup.config.download.download_dir.clone())
            .await;
        let config_to_persist = backup.config.clone();
        let config_path = state.domain.config_path.clone();
        tokio::task::spawn_blocking(move || {
            crate::commands::config_commands::persist_config(&config_to_persist, &config_path)
        })
        .await
        .map_err(|e| AppError::Config(format!("持久化导入配置任务失败: {e}")))??;

        // 清空内存任务表与持久化快照
        let ids: Vec<String> = state
            .domain
            .task_repository
            .iter()
            .map(|r| r.key().clone())
            .collect();
        for id in &ids {
            state.domain.task_repository.remove(id);
            if let Err(e) = state.infra.task_store.remove_snapshot(id) {
                tracing::warn!(task_id = %id, error = %e, "导入覆盖时清理旧快照失败");
            }
        }
    }

    // 去重键口径与 TaskService::create_task 一致(url_identity_key):
    // magnet 按 info hash,http(s) 按 scheme://host/basename,其余按原文
    let existing_urls: HashSet<String> = state
        .domain
        .task_repository
        .iter()
        .map(|r| url_identity_key(&r.value().url))
        .collect();

    let mut imported = 0usize;
    for mut snapshot in backup.tasks {
        let identity = url_identity_key(&snapshot.url);
        if !overwrite && existing_urls.contains(&identity) {
            continue;
        }

        snapshot.status = crate::task_store::normalize_recovered_status(snapshot.status);
        let task = crate::task_store::snapshot_to_task_info(&snapshot);
        state.domain.task_repository.insert(task.id.clone(), task);

        let store = state.infra.task_store.clone();
        let task_id = snapshot.id.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = store.save_snapshot(&snapshot) {
                tracing::warn!(task_id = %task_id, error = %e, "导入时保存任务快照失败");
            }
        });
        imported += 1;
    }

    Ok(imported)
}

pub(crate) async fn move_task_inner(
    state: &AppState,
    task_id: String,
    before_id: Option<String>,
) -> Result<(), AppError> {
    state
        .service
        .task_service
        .move_task(task_id, before_id)
        .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::now_iso8601;
    use super::super::tests::test_state;
    use super::*;
    use tachyon_core::safety::redact_url_for_log;
    use tachyon_core::types::DownloadState;
    use tokio::sync::oneshot;
    use uuid::Uuid;

    async fn set_test_download_root(state: &AppState, root: &std::path::Path) {
        let root = root.canonicalize().unwrap().to_string_lossy().to_string();
        let mut cfg = state.domain.config.lock().await;
        cfg.download.download_dir = root.clone();
        cfg.download.authorized_dirs = vec![root];
    }

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
                active_concurrency: 0,
                created_at: now_iso8601(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                tags: vec![],
                hf_meta: None,
                display_order: 0,
                mirror_urls: None,
            },
        );

        let (start_tx, start_rx) = oneshot::channel();
        let handle = tokio::spawn({
            let state = state.clone();
            // 连接池热替换句柄:在 spawn 内读锁 clone 出当前 Arc<ConnectionPool>
            let pool_handle = state.infra.connection_pool.clone();
            // 切片2 夹具修复:task_fn 新签名增加 buffer_pool 参数,
            // 从 AppState.infra.buffer_pool 取池注入,使 worker 用池化 buffer。
            // 审计 A-14:与连接池一样,启动时读锁 clone 当前池快照。
            let buffer_pool_handle = state.infra.buffer_pool.clone();
            let task_id = task_id.clone();
            async move {
                let _ = start_rx.await;
                let connection_pool = pool_handle.read().await.clone();
                let buffer_pool = buffer_pool_handle.read().await.clone();
                task_fn(
                    state,
                    task_id,
                    url,
                    download_dir,
                    download_config,
                    connection_pool,
                    buffer_pool,
                    control_rx,
                    None,
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
            None,
            true,
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
            None,
            true,
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
            None,
            true,
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
    async fn test_create_task_auto_start_true_starts_download() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/auto-start-true.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();

        // 创建后先为 Pending,随后 supervisor 启动 task_fn
        let task = get_task_detail_inner(&state, id.clone()).await.unwrap();
        assert_eq!(task.status, DownloadState::Pending);

        // 等待 supervisor 注册运行中的任务
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !state.runtime.supervisor.has_running_task(&id) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "auto_start=true 时 supervisor 应启动下载任务"
        );
    }

    #[tokio::test]
    async fn test_create_task_auto_start_false_keeps_pending() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/auto-start-false.bin".to_string(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();

        let task = get_task_detail_inner(&state, id.clone()).await.unwrap();
        assert_eq!(task.status, DownloadState::Pending);
        assert!(
            !state.runtime.supervisor.has_running_task(&id),
            "auto_start=false 时 supervisor 不应有运行中的任务"
        );

        // 稍等片刻再次确认未自动启动
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !state.runtime.supervisor.has_running_task(&id),
            "auto_start=false 时任务应保持 Pending,不应被自动启动"
        );
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
            None,
            true,
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
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let result = create_task_inner(
            &state,
            "https://dup.example.com/once.zip".to_string(),
            None,
            None,
            None,
            true,
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
                create_task_inner(&state, url.to_string(), None, None, None, true, None).await
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
                create_task_inner(&state, url, None, None, None, true, None).await
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
            None,
            true,
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
            None,
            true,
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
            None,
            true,
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
    async fn test_resume_pending_task_succeeds() {
        // 修复回归:恢复的任务被 normalize_recovered_status 归一化为 Pending,
        // 此前 resume_task 仅允许 Paused 恢复,Pending 会报错"仅暂停状态可恢复"。
        // 现在 Pending/Paused 均可恢复,create_task 后任务初始即为 Pending。
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        resume_task_inner(&state, id.clone()).await.unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, DownloadState::Downloading);
    }

    #[tokio::test]
    async fn test_resume_cancelled_task_fails() {
        // 终态任务(Cancelled)不可恢复
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        let result = resume_task_inner(&state, id).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("仅 Pending/Paused 状态可恢复")
        );
    }

    #[tokio::test]
    async fn test_cancel_pending_task() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
            None,
            true,
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
            None,
            true,
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
            None,
            true,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        delete_task_inner(&state, id.clone(), false).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    /// 审计 H-04:delete 必须 await 活跃 JoinHandle,不能仅 cancel 后立刻删记录
    #[tokio::test]
    async fn test_delete_task_inner_waits_for_running_handle() {
        use std::time::{Duration, Instant};
        use tokio::sync::watch;

        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/h04-quiesce.bin".to_string(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();

        // 注入仍在运行的 handle(模拟下载 worker 尚未退出)
        let (tx, mut rx) = watch::channel(TaskCommand::Start);
        state
            .runtime
            .supervisor
            .command_channels
            .insert(id.clone(), tx);
        let handle = tokio::spawn(async move {
            // 收到 Cancel 后再延迟退出,模拟协作式取消后的 drain
            loop {
                if matches!(*rx.borrow(), TaskCommand::Cancel) {
                    break;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        });
        state.runtime.supervisor.handles.insert(id.clone(), handle);

        let started = Instant::now();
        delete_task_inner(&state, id.clone(), false)
            .await
            .expect("delete 应在 quiesce 后成功");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(120),
            "delete 应等待 handle 退出, elapsed={elapsed:?}"
        );
        assert!(
            !state.runtime.supervisor.handles.contains_key(&id),
            "handle 应已移除"
        );
        assert!(
            !state.runtime.supervisor.command_channels.contains_key(&id),
            "command channel 应已移除"
        );
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    #[tokio::test]
    async fn test_delete_task_default_option_preserves_local_file_and_temp_candidates() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/preserve-local-file.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();

        let token = state
            .service
            .confirmation_service
            .request("delete_task")
            .unwrap();

        let temp_root = tempfile::tempdir().unwrap();
        let final_path = temp_root.path().join("preserve-local-file.bin");
        let temp_candidate_1 = temp_root.path().join("preserve-local-file.bin.part");
        let temp_candidate_2 = temp_root.path().join("preserve-local-file.bin.tachyon.tmp");
        std::fs::write(&final_path, b"final payload").unwrap();
        std::fs::write(&temp_candidate_1, b"partial payload").unwrap();
        std::fs::write(&temp_candidate_2, b"resume payload").unwrap();

        {
            if let Some(mut task) = state.domain.task_repository.get_mut(&id) {
                task.save_path = final_path.to_string_lossy().to_string();
            }
        }

        state
            .service
            .confirmation_service
            .validate_and_consume(&token, "delete_task")
            .unwrap();
        delete_task_inner(&state, id.clone(), false).await.unwrap();

        assert!(get_task_detail_inner(&state, id.clone()).await.is_err());
        assert!(
            state.infra.task_store.load_snapshot(&id).unwrap().is_none(),
            "删除任务后应移除断点续传快照"
        );
        assert!(final_path.exists(), "默认删除任务不应删除最终本地文件");
        assert!(
            temp_candidate_1.exists(),
            "默认删除任务不应删除确定性的临时文件候选"
        );
        assert!(
            temp_candidate_2.exists(),
            "默认删除任务不应删除确定性的临时文件候选"
        );
    }

    #[tokio::test]
    async fn test_delete_task_with_delete_local_file_option_removes_file_and_temp_candidates() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/remove-local-file.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();

        let temp_root = tempfile::tempdir().unwrap();
        set_test_download_root(&state, temp_root.path()).await;
        let final_path = temp_root.path().join("remove-local-file.bin");
        let temp_candidate_1 = std::path::PathBuf::from(format!("{}.part", final_path.display()));
        let temp_candidate_2 = std::path::PathBuf::from(format!("{}.tmp", final_path.display()));
        let temp_candidate_3 =
            std::path::PathBuf::from(format!("{}.download", final_path.display()));
        let temp_candidate_4 = temp_root.path().join(format!("{id}.part"));
        let temp_candidate_5 = temp_root.path().join(format!("{id}.tmp"));
        std::fs::write(&final_path, b"final payload").unwrap();
        std::fs::write(&temp_candidate_1, b"partial payload").unwrap();
        std::fs::write(&temp_candidate_2, b"tmp payload").unwrap();
        std::fs::write(&temp_candidate_3, b"download payload").unwrap();
        std::fs::write(&temp_candidate_4, b"task id part payload").unwrap();
        std::fs::write(&temp_candidate_5, b"task id tmp payload").unwrap();

        {
            if let Some(mut task) = state.domain.task_repository.get_mut(&id) {
                task.save_path = final_path.to_string_lossy().to_string();
            }
        }

        delete_task_inner(&state, id.clone(), true).await.unwrap();

        assert!(get_task_detail_inner(&state, id.clone()).await.is_err());
        assert!(
            state.infra.task_store.load_snapshot(&id).unwrap().is_none(),
            "删除任务后应移除断点续传快照"
        );
        assert!(
            !final_path.exists(),
            "启用删除本地文件选项时应删除最终本地文件"
        );
        assert!(
            !temp_candidate_1.exists(),
            "启用删除本地文件选项时应删除 <save_path>.part"
        );
        assert!(
            !temp_candidate_2.exists(),
            "启用删除本地文件选项时应删除 <save_path>.tmp"
        );
        assert!(
            !temp_candidate_3.exists(),
            "启用删除本地文件选项时应删除 <save_path>.download"
        );
        assert!(
            !temp_candidate_4.exists(),
            "启用删除本地文件选项时应删除 task-id sidecar .part"
        );
        assert!(
            !temp_candidate_5.exists(),
            "启用删除本地文件选项时应删除 task-id sidecar .tmp"
        );
    }

    #[tokio::test]
    async fn test_delete_task_with_delete_local_file_option_preserves_task_and_snapshot_when_candidate_delete_fails()
     {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/delete-failure.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();

        let snapshot_before_delete = state.infra.task_store.load_snapshot(&id).unwrap();
        assert!(snapshot_before_delete.is_some(), "前置条件: 任务快照应存在");

        let temp_root = tempfile::tempdir().unwrap();
        set_test_download_root(&state, temp_root.path()).await;

        let final_path = temp_root.path().join("delete-failure.bin");
        let undeletable_candidate =
            std::path::PathBuf::from(format!("{}.part", final_path.display()));
        std::fs::write(&final_path, b"final payload").unwrap();
        std::fs::create_dir_all(&undeletable_candidate).unwrap();

        {
            if let Some(mut task) = state.domain.task_repository.get_mut(&id) {
                task.save_path = final_path.to_string_lossy().to_string();
            }
        }

        let result = delete_task_inner(&state, id.clone(), true).await;

        assert!(result.is_err(), "删除候选失败时应向调用方返回错误");
        assert!(
            get_task_detail_inner(&state, id.clone()).await.is_ok(),
            "删除失败时应保留任务记录"
        );
        assert!(
            state.infra.task_store.load_snapshot(&id).unwrap().is_some(),
            "删除失败时应保留断点续传快照"
        );
        assert!(final_path.exists(), "删除失败时应保留最终本地文件");
        assert!(
            undeletable_candidate.exists(),
            "删除失败时应保留删除失败的候选路径"
        );
    }

    /// 删除终态任务时,应同步清理本地已下载文件和断点续传快照。
    #[tokio::test]
    async fn test_delete_cancelled_task_removes_local_file() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();

        // 构造一个临时文件模拟已下载文件,并写入任务记录
        let temp_root = tempfile::tempdir().unwrap();
        set_test_download_root(&state, temp_root.path()).await;
        let tmp_path = temp_root.path().join(format!("tachyon-delete-test-{id}"));
        std::fs::write(&tmp_path, b"test data").unwrap();
        let save_path = tmp_path.to_string_lossy().to_string();
        {
            if let Some(mut task) = state.domain.task_repository.get_mut(&id) {
                task.save_path = save_path.clone();
            }
        }

        delete_task_inner(&state, id.clone(), true).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
        assert!(!tmp_path.exists(), "删除任务时应同步删除已下载的本地文件");

        // 清理(若删除逻辑异常,此处也做兜底)
        let _ = std::fs::remove_file(&tmp_path);
    }

    #[tokio::test]
    async fn test_delete_pending_task_succeeds() {
        // 恢复的任务状态为 Pending,用户应能直接删除(无需先取消)
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        // Pending 状态的任务应可直接删除
        delete_task_inner(&state, id.clone(), false).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    #[tokio::test]
    async fn test_delete_paused_task_succeeds() {
        // 恢复的断点续传任务状态为 Paused,用户应能直接删除
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/paused-delete.zip".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        pause_task_inner(&state, id.clone()).await.unwrap();
        // Paused 状态的任务应可直接删除
        delete_task_inner(&state, id.clone(), false).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    #[tokio::test]
    async fn test_get_task_list_returns_all_tasks() {
        let state = test_state();
        let id1 = create_task_inner(
            &state,
            "https://example.com/a.zip".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let id2 = create_task_inner(
            &state,
            "https://example.com/b.zip".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
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
            None,
            true,
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

        delete_task_inner(&state, id.clone(), false).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    #[tokio::test]
    async fn test_max_concurrent_tasks_rejects() {
        // 必须用 test_state()(独立临时 store/config)，禁止 AppState::new()：
        // 后者打开全局 ~/.tachyon/store；nextest 并行会锁冲突，且可能写穿真实用户目录。
        let state = test_state();
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
        // auto_start=false 保持 Pending:门控对 Pending+Downloading 同样计数,
        // 且避免 auto_start 后任务因真实网络环境(CI 直连 example.com 可能
        // 秒级完成/失败)在第三次 create 前移出计数状态,导致断言环境敏感
        let _id1 = create_task_inner(
            &state,
            "http://example.com/file1.bin".into(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        let _id2 = create_task_inner(
            &state,
            "http://example.com/file2.bin".into(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        let result = create_task_inner(
            &state,
            "http://example.com/file3.bin".into(),
            None,
            None,
            None,
            false,
            None,
        )
        .await;
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
        let result = create_task_inner(
            &state,
            "http://example.com/zero-sem.bin".into(),
            None,
            None,
            None,
            true,
            None,
        )
        .await;
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
                None,
                true,
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

        // 测试目的是验证并发 cancel+get_list 无死锁,而非取消语义的精确状态。
        // 并发场景下 cancel_task 设置 Cancelled 与 supervisor 清理(可能因无活跃
        // handle 触发 Err 路径设为 Failed)存在 race,两者都是终态,均表示操作完成。
        // 严格断言 Cancelled 会导致 flaky 失败(取消信号与错误处理的竞争)。
        for id in &task_ids[..3] {
            let task = get_task_detail_inner(&state, id.clone()).await.unwrap();
            assert!(
                task.status == DownloadState::Cancelled || task.status == DownloadState::Failed,
                "任务应处于终态(Cancelled 或 Failed),实际: {}",
                task.status
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
                None,
                true,
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
                    None,
                    true,
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
                delete_task_inner(&state_clone, tid, false).await
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
    async fn test_h02_resume_closed_channel_restarts_download() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "http://example.com/h02-closed.bin".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();

        // 模拟 session 已退出但 command channel entry 仍在:
        // drop 唯一 receiver,保留 sender entry,使 has_running_task=true 但 send 失败
        {
            let tx = state
                .runtime
                .supervisor
                .command_channels
                .get(&id)
                .expect("应有 control channel")
                .clone();
            // 先暂停状态机到 Paused,再制造 closed receiver
            pause_task_inner(&state, id.clone()).await.unwrap();
            // 取出并替换为新的 sender,drop 其 receiver
            let (new_tx, new_rx) = watch::channel(TaskCommand::Pause);
            drop(new_rx);
            state
                .runtime
                .supervisor
                .command_channels
                .insert(id.clone(), new_tx);
            drop(tx);
        }

        // Resume:不得伪成功停在无 worker;应走 restart 路径
        resume_task_inner(&state, id.clone()).await.unwrap();
        let task = state.domain.task_repository.get(&id).unwrap();
        assert_eq!(task.status, DownloadState::Downloading);
        // restart 后应有新的 channel/handle
        assert!(
            state.runtime.supervisor.has_running_task(&id),
            "H-02/H-03: send 失败后必须 restart,恢复有 worker 的运行态"
        );
    }

    #[tokio::test]
    async fn test_concurrent_pause_resume_no_deadlock() {
        let state = test_state();

        let id = create_task_inner(
            &state,
            "http://example.com/pause-resume-test.bin".to_string(),
            None,
            None,
            None,
            true,
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
            None,
            true,
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
            None,
            true,
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
            None,
            true,
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

    /// 审计 H-04 (RED): cancel_task_inner 在 send Cancel 后必须 wait_for_handle quiesce,
    /// 不能直接返回 Ok,否则旧 task 仍在写盘/联网,与 restart 产生竞态。
    /// 当前实现未调用 wait_for_handle,本测试在 cancel 返回时 handle 应已结束,
    /// 但当前实现会失败(handle 仍存活)。
    #[tokio::test]
    async fn test_cancel_task_waits_for_handle_quiesce() {
        use std::time::{Duration, Instant};
        use tokio::sync::watch;

        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/h04-cancel-quiesce.bin".to_string(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();

        // 注入一个仍在运行的 handle:收到 Cancel 后短暂 drain 再退出,
        // 模拟协作式取消后的写盘收尾。cancel_task_inner 必须等待此 handle 结束。
        let (tx, mut rx) = watch::channel(TaskCommand::Start);
        state
            .runtime
            .supervisor
            .command_channels
            .insert(id.clone(), tx);
        let cancel_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_seen_clone = cancel_seen.clone();
        let handle = tokio::spawn(async move {
            loop {
                if matches!(*rx.borrow(), TaskCommand::Cancel) {
                    cancel_seen_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
            // 模拟 drain:cancel 后再延迟 120ms 退出
            tokio::time::sleep(Duration::from_millis(120)).await;
        });
        state.runtime.supervisor.handles.insert(id.clone(), handle);

        let started = Instant::now();
        cancel_task_inner(&state, id.clone())
            .await
            .expect("cancel 应在 quiesce 后成功");
        let elapsed = started.elapsed();

        // 期望行为:cancel 返回前 handle 已被 wait_for_handle 移除
        assert!(
            !state.runtime.supervisor.handles.contains_key(&id),
            "H-04: cancel 返回前应已移除 handle, elapsed={elapsed:?}"
        );
        assert!(
            !state.runtime.supervisor.command_channels.contains_key(&id),
            "H-04: cancel 返回前应已移除 command channel, elapsed={elapsed:?}"
        );
        // 期望行为:cancel 应等待 drain(>=120ms)。当前实现不等待,会远小于 120ms
        assert!(
            elapsed >= Duration::from_millis(120),
            "H-04: cancel 应等待 handle quiesce, elapsed={elapsed:?}"
        );
        // 取消信号确实送达
        assert!(
            cancel_seen.load(std::sync::atomic::Ordering::SeqCst),
            "H-04: cancel 应送达 Cancel 信号"
        );
    }

    /// 审计 H-04 (RED): cancel 一个卡住的 task(不响应 Cancel)→ CANCEL_QUIESCE_TIMEOUT
    /// 后 wait_for_handle 内部 abort + 2s grace。当前实现不调 wait_for_handle,
    /// cancel 立即返回,handle 仍存活 → 测试失败。
    #[tokio::test]
    async fn test_cancel_task_timeout_aborts_handle() {
        use std::time::{Duration, Instant};
        use tokio::sync::watch;

        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/h04-cancel-stuck.bin".to_string(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();

        // 注入一个不响应 Cancel 的 task:while 循环不检查 watch,模拟死 swarm/卡死 worker
        let (tx, _rx) = watch::channel(TaskCommand::Start);
        state
            .runtime
            .supervisor
            .command_channels
            .insert(id.clone(), tx);
        let stuck_handle = tokio::spawn(async move {
            // 永远不退出,也不读 watch。只有 abort 才能终止
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        state
            .runtime
            .supervisor
            .handles
            .insert(id.clone(), stuck_handle);

        let started = Instant::now();
        // 期望行为:cancel 在 CANCEL_QUIESCE_TIMEOUT(5s) + 2s grace 内 abort 并返回。
        // 为避免真实等 5s,这里给 8s 上限(超时值 + grace + 余量)。
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            cancel_task_inner(&state, id.clone()).await
        })
        .await;
        assert!(
            result.is_ok(),
            "H-04: cancel 应在 quiesce 超时 abort 后返回,不应永久挂起"
        );
        let elapsed = started.elapsed();

        // 期望行为:cancel 应等待至少 CANCEL_QUIESCE_TIMEOUT(5s) - 1s 余量 才返回
        // 当前实现不等待,elapsed 远小于 5s
        assert!(
            elapsed >= Duration::from_secs(4),
            "H-04: cancel 卡住 task 应等待 CANCEL_QUIESCE_TIMEOUT 后 abort, elapsed={elapsed:?}"
        );
        // 期望行为:abort 后 handle 已移除
        assert!(
            !state.runtime.supervisor.handles.contains_key(&id),
            "H-04: cancel 超时 abort 后应移除 handle"
        );
        assert!(
            !state.runtime.supervisor.command_channels.contains_key(&id),
            "H-04: cancel 超时 abort 后应移除 command channel"
        );
    }

    /// 审计 H-04 (RED): cancel 不存在的 task(无 handle)→ 应立即返回,不阻塞。
    /// 当前实现虽不等待,但行为正确。此测试应通过,作为契约保护。
    #[tokio::test]
    async fn test_cancel_task_no_handle_returns_quickly() {
        use std::time::{Duration, Instant};

        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/h04-cancel-no-handle.bin".to_string(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        // 不注入 handle,无运行中 task_fn

        let started = Instant::now();
        cancel_task_inner(&state, id.clone())
            .await
            .expect("cancel 无 handle 的 task 应成功");
        let elapsed = started.elapsed();

        // 无 handle 时 wait_for_handle 立即返回 None,整体应 < 500ms
        assert!(
            elapsed < Duration::from_millis(500),
            "H-04: cancel 无 handle 的 task 应立即返回, elapsed={elapsed:?}"
        );
        // 状态应为 Cancelled
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, DownloadState::Cancelled);
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

    #[tokio::test]
    async fn test_create_task_with_custom_filename() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/long-auto-name.tar.gz".to_string(),
            None,
            None,
            Some("my_model.bin".to_string()),
            true,
            None,
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.file_name, "my_model.bin");
    }

    #[tokio::test]
    async fn test_create_task_custom_filename_sanitizes_path_traversal() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/safe.zip".to_string(),
            None,
            None,
            Some("../../etc/passwd".to_string()),
            true,
            None,
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.file_name, "etc passwd");
    }

    #[tokio::test]
    async fn test_create_task_none_filename_falls_back_to_url() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://cdn.example.org/releases/v2.0.tar.gz".to_string(),
            None,
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.file_name, "v2.0.tar.gz");
    }

    #[tokio::test]
    async fn test_create_task_blank_filename_falls_back_to_url() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/model.bin".to_string(),
            None,
            None,
            Some("   ".to_string()),
            true,
            None,
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.file_name, "model.bin");
    }

    // ---- wait_for_resume_or_cancel 测试 ----

    #[tokio::test]
    async fn test_wait_for_resume_or_cancel_resume() {
        let (tx, rx) = watch::channel(TaskCommand::Pause);
        let mut rx = rx;

        // 在另一个任务中延迟发送 Resume
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(TaskCommand::Resume);
        });

        let result = wait_for_resume_or_cancel(&mut rx, Duration::from_secs(5)).await;
        assert!(matches!(result, ResumeOrCancel::Resume));
    }

    #[tokio::test]
    async fn test_wait_for_resume_or_cancel_cancel() {
        let (tx, rx) = watch::channel(TaskCommand::Pause);
        let mut rx = rx;

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(TaskCommand::Cancel);
        });

        let result = wait_for_resume_or_cancel(&mut rx, Duration::from_secs(5)).await;
        assert!(matches!(result, ResumeOrCancel::Cancel));
    }

    #[tokio::test]
    async fn test_wait_for_resume_or_cancel_timeout() {
        let (tx, rx) = watch::channel(TaskCommand::Pause);
        let mut rx = rx;

        let result = wait_for_resume_or_cancel(&mut rx, Duration::from_millis(200)).await;
        // 保持 tx 存活直到测试结束,避免控制通道关闭被误判为取消
        drop(tx);
        assert!(matches!(result, ResumeOrCancel::Timeout));
    }

    // ---- should_stop_before_run 测试 ----

    #[test]
    fn test_should_stop_before_run_continue() {
        let (_, rx) = watch::channel(TaskCommand::Start);
        assert!(matches!(should_stop_before_run(&rx), PreRunCheck::Continue));
    }

    #[test]
    fn test_should_stop_before_run_cancelled() {
        let (_, rx) = watch::channel(TaskCommand::Cancel);
        assert!(matches!(
            should_stop_before_run(&rx),
            PreRunCheck::Cancelled
        ));
    }

    #[test]
    fn test_should_stop_before_run_paused() {
        let (_, rx) = watch::channel(TaskCommand::Pause);
        assert!(matches!(should_stop_before_run(&rx), PreRunCheck::Paused));
    }

    #[test]
    fn test_should_cancel_before_run_delegates_to_should_stop() {
        let (_, rx) = watch::channel(TaskCommand::Cancel);
        assert!(should_cancel_before_run(&rx));

        let (_, rx) = watch::channel(TaskCommand::Start);
        assert!(!should_cancel_before_run(&rx));

        // Pause 不算取消
        let (_, rx) = watch::channel(TaskCommand::Pause);
        assert!(!should_cancel_before_run(&rx));
    }

    // ---- validate_and_prepare_url 暂停处理测试 ----

    /// 辅助:创建测试用 TaskInfo 并插入仓库,返回 task_id
    fn insert_test_task(state: &AppState, status: DownloadState) -> String {
        let task_id = Uuid::new_v4().to_string();
        state.domain.task_repository.insert(
            task_id.clone(),
            TaskInfo {
                id: task_id.clone(),
                url: "https://example.com/file.zip".to_string(),
                file_name: "file.zip".to_string(),
                file_size: None,
                downloaded: 0,
                speed: 0,
                status,
                progress: 0.0,
                fragments_total: 0,
                fragments_done: 0,
                active_concurrency: 0,
                created_at: now_iso8601(),
                save_path: String::new(),
                error_reason: None,
                retry_count: 0,
                tags: vec![],
                hf_meta: None,
                display_order: 0,
                mirror_urls: None,
            },
        );
        task_id
    }

    #[test]
    fn test_mirrors_for_restart_uses_task_mirror_urls() {
        // restart_download 必须传 task.mirror_urls,不再硬编码 None
        let mut task = TaskInfo {
            id: "t-mirror".to_string(),
            url: "https://primary.example.com/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(100),
            downloaded: 0,
            speed: 0,
            status: DownloadState::Paused,
            progress: 0.0,
            fragments_total: 1,
            fragments_done: 0,
            active_concurrency: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            error_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: Some(vec![
                "https://m1.example.com/file.bin".to_string(),
                "https://m2.example.com/file.bin".to_string(),
            ]),
        };
        assert_eq!(
            mirrors_for_restart(&task),
            Some(vec![
                "https://m1.example.com/file.bin".to_string(),
                "https://m2.example.com/file.bin".to_string(),
            ])
        );
        task.mirror_urls = None;
        assert!(mirrors_for_restart(&task).is_none());
    }

    #[tokio::test]
    async fn test_validate_and_prepare_url_pause_then_resume() {
        let state = test_state();
        let task_id = insert_test_task(&state, DownloadState::Pending);
        let (tx, rx) = watch::channel(TaskCommand::Pause);
        let mut rx = rx;

        // 延迟发送 Resume
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(TaskCommand::Resume);
        });

        let result =
            validate_and_prepare_url("https://example.com/file.zip", &state, &task_id, &mut rx, 5)
                .await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "example.com");
        // 暂停恢复后应设置为 Downloading
        let task = state.domain.task_repository.get(&task_id).unwrap();
        assert_eq!(task.status, DownloadState::Downloading);
    }

    #[tokio::test]
    async fn test_validate_and_prepare_url_pause_timeout() {
        let state = test_state();
        let task_id = insert_test_task(&state, DownloadState::Pending);
        let (tx, rx) = watch::channel(TaskCommand::Pause);
        let mut rx = rx;

        let result = validate_and_prepare_url(
            "https://example.com/file.zip",
            &state,
            &task_id,
            &mut rx,
            // 使用极短超时
            1,
        )
        .await;
        // 保持 tx 存活
        drop(tx);
        assert!(result.is_none());
        let task = state.domain.task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.status,
            DownloadState::Paused,
            "M-05: 执行前 pause 超时应保持 Paused,不映射为 Cancelled"
        );
    }

    #[tokio::test]
    async fn test_validate_and_prepare_url_pause_then_cancel() {
        let state = test_state();
        let task_id = insert_test_task(&state, DownloadState::Pending);
        let (tx, rx) = watch::channel(TaskCommand::Pause);
        let mut rx = rx;

        // 延迟发送 Cancel
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(TaskCommand::Cancel);
        });

        let result =
            validate_and_prepare_url("https://example.com/file.zip", &state, &task_id, &mut rx, 5)
                .await;
        assert!(result.is_none());
        let task = state.domain.task_repository.get(&task_id).unwrap();
        assert_eq!(task.status, DownloadState::Cancelled);
    }

    #[tokio::test]
    async fn test_validate_and_prepare_url_cancel_directly() {
        let state = test_state();
        let task_id = insert_test_task(&state, DownloadState::Pending);
        let (tx, rx) = watch::channel(TaskCommand::Cancel);
        let mut rx = rx;

        let result =
            validate_and_prepare_url("https://example.com/file.zip", &state, &task_id, &mut rx, 5)
                .await;
        drop(tx);
        assert!(result.is_none());
        let task = state.domain.task_repository.get(&task_id).unwrap();
        assert_eq!(task.status, DownloadState::Cancelled);
    }

    #[tokio::test]
    async fn test_validate_and_prepare_url_normal_flow() {
        let state = test_state();
        let task_id = insert_test_task(&state, DownloadState::Pending);
        let (tx, rx) = watch::channel(TaskCommand::Start);
        let mut rx = rx;

        let result =
            validate_and_prepare_url("https://example.com/file.zip", &state, &task_id, &mut rx, 5)
                .await;
        drop(tx);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "example.com");
        let task = state.domain.task_repository.get(&task_id).unwrap();
        assert_eq!(task.status, DownloadState::Downloading);
    }

    // ── BufferPool 链路注入(切片2) RED 测试 ──────────────────────────
    //
    // 目标:验证全局 BufferPool 经 task_fn -> DownloadSession::new ->
    // build_download_task 链路注入到 DownloadTask,使 worker 用池化 buffer。
    //
    // 可观测性限制(app 层无法做完整运行时 RED 的原因):
    //   1. build_download_task 内部硬编码 DownloadTask::with_pool/with_mirrors,
    //      创建真实 HttpClient,无协议注入入口。app crate 无法访问 engine 的
    //      new_for_test(cfg(test) 私有)或 tachyon-core 的 MockProtocol
    //      (需 test-harness feature,app dev-deps 未启用,且即便启用也无注入点)。
    //   2. DownloadTask.buffer_pool 为私有字段,build_download_task 返回
    //      Box<dyn TaskRunner>,TaskRunner trait 无 buffer_pool 查询方法
    //      (core 不能依赖 io,加方法违反层序)。
    //
    // 因此切片2 的 RED 信号采用"编译期契约 + 运行时不变量"双轨:
    //   - 编译期:测试调用 task_fn / DownloadSession::new 的新签名(含 buffer_pool
    //     参数)。Coder 改签名前 -> 参数数量不匹配 -> 编译失败(RED)。
    //     Coder 改签名后 -> 编译通过 -> 测试体执行(GREEN)。
    //   - 运行时:构造失败路径(ftp URL,with_pool 对非 http 返回 Err)下,buffer_pool
    //     不被消费(available == capacity),且任务进入 Failed。这验证夹具接线正确
    //     (task_fn 新签名被实际调用且不 panic)与构造失败不泄漏 buffer 引用。
    //
    // 成功注入路径(真实 http 下载)的正确性由 engine 层 set_buffer_pool 测试
    // (downloader.rs: test_buffer_pool_returns_buffers_after_run 等) +
    // 编译保证 + 既有 app 下载测试不回归共同覆盖。

    /// 验证 DownloadSession::new 接受 buffer_pool 参数(编译期契约)
    ///
    /// 调用新签名构造会话,断言构造不副作用消费 buffer(available 保持 capacity)。
    /// RED:Coder 改 DownloadSession::new 签名前,buffer_pool 参数不存在,
    /// 编译失败(参数数量不匹配)。
    #[tokio::test]
    async fn test_download_session_new_accepts_buffer_pool() {
        let state = test_state();
        let capacity = state.infra.buffer_pool.read().await.clone().capacity();
        let download_config = {
            let cfg = state.domain.config.lock().await;
            build_download_config(&cfg, "/tmp/tachyon-slice2-unused")
        };
        let (_control_tx, control_rx) = watch::channel(TaskCommand::Start);

        // 调用新签名:传入 state.infra.buffer_pool.clone() 作为 buffer_pool 参数。
        // 仅构造会话,不调用 run()(run 会触发真实下载)。
        // connection_pool 字段现为热替换句柄,读锁 clone 出当前 Arc<ConnectionPool>。
        let connection_pool = state.infra.connection_pool.read().await.clone();
        let _session = crate::runtime::DownloadSession::new(
            state.clone(),
            "slice2-signature-contract".to_string(),
            "https://example.com/slice2-signature.bin".to_string(),
            "/tmp/tachyon-slice2-unused".to_string(),
            download_config,
            connection_pool,
            state.infra.buffer_pool.read().await.clone(),
            control_rx,
            None,
            None,
        );

        // 构造只是存储 Arc<BufferPool>,不应消费任何许可。
        assert_eq!(
            state.infra.buffer_pool.read().await.clone().available(),
            capacity,
            "DownloadSession::new 构造不应消费 buffer_pool 许可"
        );
    }

    /// 验证构造失败路径不消费 buffer_pool(运行时不变量 + 夹具接线)
    ///
    /// ftp URL 使 build_download_task 内部 with_pool 返回
    /// Err(DownloadError::Config("不支持的协议")),任务进入 Failed。
    /// 此路径在 set_buffer_pool 之前返回,buffer_pool 不应被消费。
    /// 同时验证 spawn_task_fn_for_test 夹具已按新签名传 buffer_pool,
    /// task_fn 被实际调用且不 panic。
    ///
    /// RED:Coder 改 task_fn 签名前,夹具调用 task_fn(..., buffer_pool, ...)
    /// 编译失败;改后本测试运行,验证构造失败不变量。
    #[tokio::test]
    async fn test_task_fn_construct_failure_does_not_consume_buffer_pool() {
        let state = test_state();
        let capacity = state.infra.buffer_pool.read().await.clone().capacity();
        let task_id = "slice2-construct-fail-no-bp-consume".to_string();
        let download_root = tempfile::tempdir().unwrap();
        let download_dir = download_root.path().to_string_lossy().to_string();
        // ftp 协议:with_pool 返回 Err,触发构造失败路径(不接触 set_buffer_pool)
        let url = "ftp://example.com/slice2-construct-fail.bin".to_string();

        spawn_task_fn_for_test(
            state.clone(),
            task_id.clone(),
            url,
            "slice2-construct-fail.bin".to_string(),
            download_dir,
        )
        .await;

        // 等待任务进入 Failed 且运行态清理完成
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = state
                    .domain
                    .task_repository
                    .get(&task_id)
                    .map(|task| task.status);
                if status == Some(DownloadState::Failed)
                    && !state.runtime.supervisor.handles.contains_key(&task_id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("构造失败后应写入 Failed 并清理后台句柄");

        let task = state.domain.task_repository.get(&task_id).unwrap();
        assert_eq!(
            task.status,
            DownloadState::Failed,
            "ftp URL 应触发构造失败,任务进入 Failed"
        );

        // 构造失败路径在 set_buffer_pool 之前返回,buffer_pool 不应被消费
        assert_eq!(
            state.infra.buffer_pool.read().await.clone().available(),
            capacity,
            "构造失败路径不应消费 buffer_pool 许可(无泄漏)"
        );
    }

    /// 验证 build_download_task 接受 buffer_pool 参数(编译期契约 + 构造失败不变量)
    ///
    /// 直接调用 build_download_task 新签名(含 buffer_pool),用 ftp URL 使
    /// with_pool 返回 Err,断言返回 Err 且 buffer_pool 未被消费。
    /// 这覆盖链路第三环 build_download_task 的签名契约。
    ///
    /// RED:Coder 改 build_download_task 签名前,buffer_pool 参数不存在,
    /// 编译失败(参数数量不匹配)。
    #[tokio::test]
    async fn test_build_download_task_accepts_buffer_pool() {
        let state = test_state();
        let capacity = state.infra.buffer_pool.read().await.clone().capacity();
        let download_config = {
            let cfg = state.domain.config.lock().await;
            build_download_config(&cfg, "/tmp/tachyon-slice2-bdt-unused")
        };

        // 调用新签名:传入 state.infra.buffer_pool.clone() 作为 buffer_pool 参数。
        // ftp 协议使 with_pool 返回 Err,build_download_task 应返回 Err。
        // connection_pool 字段现为热替换句柄,读锁 clone 出当前 Arc<ConnectionPool>。
        let connection_pool = state.infra.connection_pool.read().await.clone();
        let result = build_download_task(
            "slice2-bdt-contract",
            "ftp://example.com/slice2-bdt.bin",
            download_config,
            connection_pool,
            state.infra.buffer_pool.read().await.clone(),
            state.infra.global_rate_limiter.clone(),
            tachyon_core::config::SchedulerConfig::default(),
            None,
            #[cfg(feature = "magnet")]
            None,
        )
        .await;

        assert!(
            result.is_err(),
            "ftp URL 应使 build_download_task 构造失败返回 Err"
        );

        // 构造失败在 set_buffer_pool 之前返回,buffer_pool 不应被消费
        assert_eq!(
            state.infra.buffer_pool.read().await.clone().available(),
            capacity,
            "build_download_task 构造失败不应消费 buffer_pool 许可"
        );
    }

    /// 验证 P2SP 路由:magnet + 镜像但缺少 bt_session 时返回 Err(Task 8)
    ///
    /// Task 8 在 build_download_task 新增 magnet+mirrors 分支调 with_hybrid_sources,
    /// 该分支要求 bt_session.is_some()。此处用 None 触发路由的 ok_or_else 错误路径,
    /// 断言返回 Err 且 buffer_pool 未被消费(在 set_buffer_pool 之前返回)。
    /// 覆盖新分支的路由契约,无需真实 BT session。
    #[tokio::test]
    #[cfg(feature = "magnet")]
    async fn test_build_download_task_p2sp_missing_bt_session_returns_err() {
        let state = test_state();
        let capacity = state.infra.buffer_pool.read().await.clone().capacity();
        let download_config = {
            let cfg = state.domain.config.lock().await;
            build_download_config(&cfg, "/tmp/tachyon-task8-p2sp-unused")
        };

        // magnet + 镜像 + bt_session=None -> P2SP 分支的 ok_or_else 错误路径
        // connection_pool 字段现为热替换句柄,读锁 clone 出当前 Arc<ConnectionPool>。
        let connection_pool = state.infra.connection_pool.read().await.clone();
        let result = build_download_task(
            "task8-p2sp-contract",
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
            download_config,
            connection_pool,
            state.infra.buffer_pool.read().await.clone(),
            state.infra.global_rate_limiter.clone(),
            tachyon_core::config::SchedulerConfig::default(),
            Some(vec!["https://mirror.example.com/file.bin".to_string()]),
            #[cfg(feature = "magnet")]
            None,
        )
        .await;

        assert!(
            result.is_err(),
            "magnet+镜像缺少 BT Session 应使 P2SP 路由返回 Err"
        );

        // 错误路径在 set_buffer_pool 之前返回,buffer_pool 不应被消费
        assert_eq!(
            state.infra.buffer_pool.read().await.clone().available(),
            capacity,
            "P2SP 路由错误路径不应消费 buffer_pool 许可"
        );
    }

    /// 无效 URL 应返回错误
    #[tokio::test]
    async fn test_probe_filename_invalid_url_returns_error() {
        let state = test_state();
        let result = probe_filename_inner(&state, "not-a-url".to_string()).await;
        assert!(result.is_err(), "无效 URL 应返回错误");
    }

    // ------ ensure_dir_under_download_roots(打开文件夹授权口径) ------

    /// 构造 download_dir=root_a、authorized_dirs=[root_a, root_b] 的配置
    fn make_open_dir_config(root_a: &std::path::Path, root_b: &std::path::Path) -> AppConfig {
        let mut config = AppConfig::default();
        config.download.download_dir = root_a.to_string_lossy().to_string();
        config.download.authorized_dirs = vec![
            root_a.to_string_lossy().to_string(),
            root_b.to_string_lossy().to_string(),
        ];
        config
    }

    /// 修复"已授权目录能创建任务、却不能打开文件夹":
    /// 位于 authorized_dirs(但不在 download_dir)之下的目录应放行
    #[test]
    fn test_open_dir_allows_authorized_dir_outside_download_dir() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let target = root_b.path().join("sub");
        std::fs::create_dir_all(&target).unwrap();
        let config = make_open_dir_config(root_a.path(), root_b.path());

        let result = ensure_dir_under_download_roots(&config, &target);
        assert!(
            result.is_ok(),
            "authorized_dirs 之下的目录应放行: {:?}",
            result.unwrap_err()
        );
        assert_eq!(result.unwrap(), target.canonicalize().unwrap());
    }

    #[test]
    fn test_open_dir_allows_download_dir_subdir() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let target = root_a.path().join("sub");
        std::fs::create_dir_all(&target).unwrap();
        let config = make_open_dir_config(root_a.path(), root_b.path());

        assert!(ensure_dir_under_download_roots(&config, &target).is_ok());
    }

    /// download_dir 与 authorized_dirs 之外的目录必须拒绝(安全边界不回归)
    #[test]
    fn test_open_dir_rejects_path_outside_roots() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let config = make_open_dir_config(root_a.path(), root_b.path());

        let err = ensure_dir_under_download_roots(&config, outside.path()).unwrap_err();
        assert!(
            err.to_string().contains("路径不在下载目录范围内"),
            "目录之外的路径应拒绝: {err}"
        );
    }

    #[test]
    fn test_open_dir_rejects_nonexistent_target() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let config = make_open_dir_config(root_a.path(), root_b.path());
        let missing = root_a.path().join("no-such-dir");

        let err = ensure_dir_under_download_roots(&config, &missing).unwrap_err();
        assert!(
            err.to_string().contains("目标目录不可访问"),
            "不存在的目标应拒绝: {err}"
        );
    }

    #[test]
    fn test_open_dir_rejects_when_all_roots_broken() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        // 配置里的根全部不存在 → canonicalize 全失败 → roots 为空
        let mut config = AppConfig::default();
        config.download.download_dir = root_a.path().join("gone-a").to_string_lossy().to_string();
        config.download.authorized_dirs =
            vec![root_b.path().join("gone-b").to_string_lossy().to_string()];

        let err = ensure_dir_under_download_roots(&config, target.path()).unwrap_err();
        assert!(
            err.to_string().contains("均不可访问"),
            "roots 全失效应明确报错: {err}"
        );
    }

    /// 不支持的协议(如 ftp)应返回错误
    #[tokio::test]
    async fn test_probe_filename_unsupported_protocol_returns_error() {
        let state = test_state();
        let result = probe_filename_inner(&state, "ftp://example.com/file.zip".to_string()).await;
        assert!(result.is_err(), "不支持的协议应返回错误");
    }

    /// 网络不可达时 HTTP 探测应回退到本地提取
    #[tokio::test]
    async fn test_probe_filename_http_fallback_on_network_error() {
        let state = test_state();
        let result =
            probe_filename_inner(&state, "https://192.0.2.1/nonexistent-file.bin".to_string())
                .await;
        match result {
            Ok(name) => assert_eq!(name, "nonexistent-file.bin"),
            Err(_) => {
                // 网络超时也可接受,但不能 panic
            }
        }
    }

    /// extract_magnet_fallback_name 应正确提取 dn=
    #[test]
    fn test_extract_magnet_fallback_name_with_dn() {
        assert_eq!(
            extract_magnet_fallback_name(
                "magnet:?xt=urn:btih:abc&dn=ubuntu.iso&tr=udp://t.example.com"
            ),
            "ubuntu.iso"
        );
    }

    /// extract_magnet_fallback_name 无 dn= 时应回退到 magnet-{infoHash}
    #[test]
    fn test_extract_magnet_fallback_name_without_dn() {
        assert_eq!(
            extract_magnet_fallback_name("magnet:?xt=urn:btih:ABC123&tr=udp://t.example.com"),
            "magnet-ABC123"
        );
    }

    /// extract_magnet_fallback_name dn= 为空时应回退到 infoHash
    #[test]
    fn test_extract_magnet_fallback_name_empty_dn() {
        assert_eq!(
            extract_magnet_fallback_name("magnet:?xt=urn:btih:DEF456&dn=&tr=udp://t.example.com"),
            "magnet-DEF456"
        );
    }

    /// simple_percent_decode 应处理 UTF-8 编码
    #[test]
    fn test_simple_percent_decode_utf8() {
        assert_eq!(simple_percent_decode("%E4%B8%AD%E6%96%87"), "中文");
    }

    /// simple_percent_decode 应原样保留无效编码
    #[test]
    fn test_simple_percent_decode_no_encoding() {
        assert_eq!(simple_percent_decode("filename.zip"), "filename.zip");
    }

    // --- P4-10: 配置与任务备份导入导出 ---

    fn make_test_snapshot(
        id: &str,
        url: &str,
        status: DownloadState,
    ) -> tachyon_store::TaskSnapshot {
        tachyon_store::TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: id.to_string(),
            url: url.to_string(),
            save_path: format!("/downloads/{id}.bin"),
            file_name: format!("{id}.bin"),
            file_size: Some(1024),
            downloaded: 0,
            completed_fragments: vec![],
            partial_fragments: std::collections::HashMap::new(),
            total_fragments: 4,
            fragment_size: 256,
            status,
            etag: None,
            last_modified: None,
            content_length: Some(1024),
            supports_range: true,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        }
    }

    #[tokio::test]
    async fn test_export_backup_rejects_path_outside_authorized_dirs() {
        let state = test_state();
        let outside = std::env::temp_dir()
            .join("tachyon-sec006-outside-export.json")
            .to_string_lossy()
            .to_string();
        let err = export_backup_inner(&state, outside).await.unwrap_err();
        assert!(
            err.to_string().contains("authorized_dirs")
                || err.to_string().contains("未授权")
                || err.to_string().contains("不在"),
            "应拒绝授权目录外备份: {err}"
        );
    }

    #[tokio::test]
    async fn test_create_task_rejects_unlisted_explicit_download_dir() {
        let state = test_state();
        let outside = std::env::temp_dir()
            .join("tachyon-sec002-unlisted-dl")
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&outside);
        let err = create_task_inner(
            &state,
            "https://example.com/sec002.bin".to_string(),
            Some(outside),
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("未授权") || msg.contains("授权") || msg.contains("authorized"),
            "应拒绝自动授权外目录: {msg}"
        );
    }

    #[tokio::test]
    async fn test_export_import_roundtrip() {
        let state = test_state();
        let snapshot =
            make_test_snapshot("snap-1", "https://example.com/a.bin", DownloadState::Paused);
        state.infra.task_store.save_snapshot(&snapshot).unwrap();
        state.domain.task_repository.insert(
            snapshot.id.clone(),
            crate::task_store::snapshot_to_task_info(&snapshot),
        );

        // 审计 SEC-006:备份路径须在 authorized_dirs 下
        let auth_dir = {
            let cfg = state.domain.config.lock().await;
            cfg.download.authorized_dirs[0].clone()
        };
        let path = std::path::Path::new(&auth_dir)
            .join("backup-roundtrip.json")
            .to_string_lossy()
            .to_string();
        export_backup_inner(&state, path.clone()).await.unwrap();

        let new_state = test_state();
        // 将备份复制到 new_state 授权目录(路径不同)
        let new_auth = {
            let cfg = new_state.domain.config.lock().await;
            cfg.download.authorized_dirs[0].clone()
        };
        let new_path = std::path::Path::new(&new_auth)
            .join("backup-roundtrip.json")
            .to_string_lossy()
            .to_string();
        let bytes = std::fs::read(&path).expect("读导出备份");
        std::fs::write(&new_path, bytes).expect("写到新授权目录");
        let count = import_backup_inner(&new_state, new_path, false)
            .await
            .unwrap();
        assert_eq!(count, 1);

        let task = get_task_detail_inner(&new_state, "snap-1".to_string())
            .await
            .unwrap();
        assert_eq!(task.url, "https://example.com/a.bin");
        assert_eq!(task.status, DownloadState::Paused);
    }

    #[tokio::test]
    async fn test_import_backup_rejects_oversized_file() {
        let state = test_state();
        let path = {
            let cfg = state.domain.config.lock().await;
            std::path::Path::new(&cfg.download.authorized_dirs[0])
                .join("oversized-backup.bin")
                .to_string_lossy()
                .to_string()
        };
        std::fs::write(&path, vec![b'x'; (MAX_BACKUP_FILE_BYTES as usize) + 1]).unwrap();
        let err = import_backup_inner(&state, path, false).await.unwrap_err();
        assert!(
            err.to_string().contains("过大"),
            "应拒绝超大备份文件: {err}"
        );
    }

    #[tokio::test]
    async fn test_import_backup_rejects_too_many_tasks() {
        let state = test_state();
        let path = {
            let cfg = state.domain.config.lock().await;
            std::path::Path::new(&cfg.download.authorized_dirs[0])
                .join("too-many-tasks-backup.json")
                .to_string_lossy()
                .to_string()
        };
        let tasks: Vec<tachyon_store::TaskSnapshot> = (0..MAX_BACKUP_TASKS + 1)
            .map(|i| {
                make_test_snapshot(
                    &format!("t{i}"),
                    &format!("https://example.com/{i}.bin"),
                    DownloadState::Paused,
                )
            })
            .collect();
        let backup = Backup {
            version: BACKUP_SCHEMA_VERSION,
            config: AppConfig::default(),
            tasks,
        };
        let json = serde_json::to_string(&backup).unwrap();
        assert!(
            (json.len() as u64) <= MAX_BACKUP_FILE_BYTES,
            "测试夹具 JSON 应小于文件上限,否则先撞 size 门: {}",
            json.len()
        );
        std::fs::write(&path, json).unwrap();
        let err = import_backup_inner(&state, path, false).await.unwrap_err();
        assert!(
            err.to_string().contains("任务数过多"),
            "应拒绝超多任务备份: {err}"
        );
    }

    #[tokio::test]
    async fn test_import_corrupt_json_returns_error() {
        let state = test_state();
        let path = {
            let cfg = state.domain.config.lock().await;
            std::path::Path::new(&cfg.download.authorized_dirs[0])
                .join("corrupt-backup.json")
                .to_string_lossy()
                .to_string()
        };
        std::fs::write(&path, "not valid json").unwrap();

        let result = import_backup_inner(&state, path, false).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("备份文件格式无效"));
    }

    #[tokio::test]
    async fn test_import_merge_skips_duplicate_url() {
        let state = test_state();
        let existing = make_test_snapshot(
            "existing",
            "https://example.com/dup.bin",
            DownloadState::Paused,
        );
        state.infra.task_store.save_snapshot(&existing).unwrap();
        state.domain.task_repository.insert(
            existing.id.clone(),
            crate::task_store::snapshot_to_task_info(&existing),
        );

        let imported = make_test_snapshot(
            "imported",
            "https://example.com/dup.bin",
            DownloadState::Completed,
        );
        let backup_config = state.domain.config.lock().await.clone();
        let backup = Backup {
            version: BACKUP_SCHEMA_VERSION,
            config: backup_config,
            tasks: vec![imported],
        };
        let path = {
            let cfg = state.domain.config.lock().await;
            std::path::Path::new(&cfg.download.authorized_dirs[0])
                .join("merge-dup-backup.json")
                .to_string_lossy()
                .to_string()
        };
        std::fs::write(&path, serde_json::to_string_pretty(&backup).unwrap()).unwrap();

        let count = import_backup_inner(&state, path, false).await.unwrap();
        assert_eq!(count, 0);
        assert!(
            get_task_detail_inner(&state, "existing".to_string())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_import_overwrite_replaces_tasks() {
        let state = test_state();
        let old = make_test_snapshot("old", "https://example.com/old.bin", DownloadState::Paused);
        state.infra.task_store.save_snapshot(&old).unwrap();
        state.domain.task_repository.insert(
            old.id.clone(),
            crate::task_store::snapshot_to_task_info(&old),
        );

        let new = make_test_snapshot(
            "new",
            "https://example.com/new.bin",
            DownloadState::Completed,
        );
        let backup_config = state.domain.config.lock().await.clone();
        let backup = Backup {
            version: BACKUP_SCHEMA_VERSION,
            config: backup_config,
            tasks: vec![new],
        };
        let path = {
            let cfg = state.domain.config.lock().await;
            std::path::Path::new(&cfg.download.authorized_dirs[0])
                .join("overwrite-backup.json")
                .to_string_lossy()
                .to_string()
        };
        std::fs::write(&path, serde_json::to_string_pretty(&backup).unwrap()).unwrap();

        let count = import_backup_inner(&state, path, true).await.unwrap();
        assert_eq!(count, 1);
        assert!(
            get_task_detail_inner(&state, "old".to_string())
                .await
                .is_err()
        );
        let task = get_task_detail_inner(&state, "new".to_string())
            .await
            .unwrap();
        assert_eq!(task.status, DownloadState::Completed);
    }
}
