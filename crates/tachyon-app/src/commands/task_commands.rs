use std::sync::Arc;
use std::time::Duration;

use tachyon_core::config::DownloadConfig;
use tachyon_core::safety::extract_filename_from_url;
use tachyon_core::traits::{Protocol, TaskRunner};
use tachyon_core::types::{DownloadState, FileMetadata};
use tachyon_engine::DownloadTask;
use tachyon_engine::connection::ConnectionPool;
use tachyon_io::BufferPool;
use tokio::sync::watch;
use url::Url;

use super::{
    AppError, AppState, TaskCommand, TaskInfo, cleanup_runtime, update_task_status,
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
    let host = if url.starts_with("magnet:?") {
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
                tracing::warn!(
                    task_id = %task_id,
                    timeout_secs = pause_timeout_secs,
                    "暂停等待超时,取消任务"
                );
                update_task_status(
                    &state.domain.task_repository,
                    task_id,
                    DownloadState::Cancelled,
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
    mirror_urls: Option<Vec<String>>,
    #[cfg(feature = "magnet")] bt_session: Option<Arc<tachyon_engine::BtSession>>,
) -> Result<Box<dyn TaskRunner>, ()> {
    let is_magnet = url.starts_with("magnet:?");
    let has_mirrors = mirror_urls.as_ref().is_some_and(|v| !v.is_empty());

    // P2SP 路由:按 is_magnet × has_mirrors 分四路。
    //   - magnet + mirrors:混合下载(HTTP 主源 + BT fallback)
    //   - magnet(纯 BT):with_pool_and_scheduler(传 bt_session)
    //   - http + mirrors:多源镜像
    //   - http(单源):with_pool_and_scheduler(bt_session=None)
    use tachyon_scheduler::AdaptiveDownloadScheduler;
    let scheduler: Arc<dyn tachyon_core::traits::DownloadScheduler> =
        Arc::new(AdaptiveDownloadScheduler::default_config());

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
        DownloadTask::with_mirrors(
            url.to_string(),
            mirrors,
            download_config,
            Some(connection_pool),
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
) -> Result<String, AppError> {
    create_task_inner(&state, url, download_dir, mirror_urls, file_name, None).await
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

/// 探测文件真实名称(HEAD 请求 / DHT 查询种子元数据)
///
/// - HTTP/HTTPS: 发送 HEAD 请求获取 Content-Disposition 等元数据
/// - 磁力链接: 通过 DHT/Tracker 查询种子 info.name(与迅雷行为一致)
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
    #[cfg(feature = "magnet")]
    if url.starts_with("magnet:?") {
        let bt_session = state.infra.bt_session.lock().await.clone();
        if let Some(session) = bt_session {
            let protocol = tachyon_engine::MagnetProtocol::new(
                session.session(),
                session.config().clone(),
                session.download_dir().clone(),
                session.handle_cache(),
            );
            match protocol.probe(&url).await {
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

    // HTTP/FTP:构造 DownloadTask 做 HEAD 探测
    let download_config = DownloadConfig::default();
    match DownloadTask::new(url.clone(), download_config).await {
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
        )
        .await?;

    // 注入 HF 元数据（如果提供）
    if let Some(meta) = hf_meta
        && let Some(mut task) = state.domain.task_repository.get_mut(&creation.task_id)
    {
        task.hf_meta = Some(meta);
    }

    let state_arc = Arc::new(state.clone_for_task());
    state.runtime.supervisor.start_download(
        state_arc,
        &creation.task_id,
        creation.url,
        creation.download_dir,
        creation.download_config,
        creation.mirror_urls,
        creation.preferred_file_name,
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

pub(crate) async fn delete_task_inner(
    state: &AppState,
    task_id: String,
    delete_local_file: bool,
) -> Result<(), AppError> {
    // 先发送 Cancel 命令停止活跃下载,再从仓库删除记录。
    // 对于终态任务 Cancel 命令会被忽略(无活跃 handle),不影响正确性。
    state
        .runtime
        .supervisor
        .send_command(&task_id, TaskCommand::Cancel);
    state
        .service
        .task_service
        .delete_task(&task_id, delete_local_file)
        .await?;
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
                hf_meta: None,
            },
        );

        let (start_tx, start_rx) = oneshot::channel();
        let handle = tokio::spawn({
            let state = state.clone();
            let connection_pool = state.infra.connection_pool.clone();
            // 切片2 夹具修复:task_fn 新签名增加 buffer_pool 参数,
            // 从 AppState.infra.buffer_pool 取池注入,使 worker 用池化 buffer。
            let buffer_pool = state.infra.buffer_pool.clone();
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
            None,
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
                create_task_inner(&state, url.to_string(), None, None, None, None).await
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
                create_task_inner(&state, url, None, None, None, None).await
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
            None,
            None,
        )
        .await
        .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        delete_task_inner(&state, id.clone(), false).await.unwrap();
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
        let _id1 = create_task_inner(
            &state,
            "http://example.com/file1.bin".into(),
            None,
            None,
            None,
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
    async fn test_concurrent_pause_resume_no_deadlock() {
        let state = test_state();

        let id = create_task_inner(
            &state,
            "http://example.com/pause-resume-test.bin".to_string(),
            None,
            None,
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

    #[tokio::test]
    async fn test_create_task_with_custom_filename() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/long-auto-name.tar.gz".to_string(),
            None,
            None,
            Some("my_model.bin".to_string()),
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
                hf_meta: None,
            },
        );
        task_id
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
        assert_eq!(task.status, DownloadState::Cancelled);
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
        let capacity = state.infra.buffer_pool.capacity();
        let download_config = {
            let cfg = state.domain.config.lock().await;
            build_download_config(&cfg, "/tmp/tachyon-slice2-unused")
        };
        let (_control_tx, control_rx) = watch::channel(TaskCommand::Start);

        // 调用新签名:传入 state.infra.buffer_pool.clone() 作为 buffer_pool 参数。
        // 仅构造会话,不调用 run()(run 会触发真实下载)。
        let _session = crate::runtime::DownloadSession::new(
            state.clone(),
            "slice2-signature-contract".to_string(),
            "https://example.com/slice2-signature.bin".to_string(),
            "/tmp/tachyon-slice2-unused".to_string(),
            download_config,
            state.infra.connection_pool.clone(),
            state.infra.buffer_pool.clone(),
            control_rx,
            None,
            None,
        );

        // 构造只是存储 Arc<BufferPool>,不应消费任何许可。
        assert_eq!(
            state.infra.buffer_pool.available(),
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
        let capacity = state.infra.buffer_pool.capacity();
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
            state.infra.buffer_pool.available(),
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
        let capacity = state.infra.buffer_pool.capacity();
        let download_config = {
            let cfg = state.domain.config.lock().await;
            build_download_config(&cfg, "/tmp/tachyon-slice2-bdt-unused")
        };

        // 调用新签名:传入 state.infra.buffer_pool.clone() 作为 buffer_pool 参数。
        // ftp 协议使 with_pool 返回 Err,build_download_task 应返回 Err。
        let result = build_download_task(
            "slice2-bdt-contract",
            "ftp://example.com/slice2-bdt.bin",
            download_config,
            state.infra.connection_pool.clone(),
            state.infra.buffer_pool.clone(),
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
            state.infra.buffer_pool.available(),
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
        let capacity = state.infra.buffer_pool.capacity();
        let download_config = {
            let cfg = state.domain.config.lock().await;
            build_download_config(&cfg, "/tmp/tachyon-task8-p2sp-unused")
        };

        // magnet + 镜像 + bt_session=None -> P2SP 分支的 ok_or_else 错误路径
        let result = build_download_task(
            "task8-p2sp-contract",
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
            download_config,
            state.infra.connection_pool.clone(),
            state.infra.buffer_pool.clone(),
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
            state.infra.buffer_pool.available(),
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
}
