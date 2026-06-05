use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::{Duration, Instant};

use chrono::Local;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tachyon_core::config::{AppConfig, ConnectionConfig, DownloadConfig};
use tachyon_core::filename::extract_filename_from_url;
use tachyon_core::types::DownloadState;
use tachyon_engine::DownloadTask;
use tachyon_engine::connection::{ConnectionPool, PoolConfig};
use tachyon_sniffer::capture::{ResourceType, identify_resource};
use tokio::sync::watch;
use url::Url;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("任务不存在: {0}")]
    TaskNotFound(String),
    #[error("任务已存在: {0}")]
    TaskAlreadyExists(String),
    #[error("网络错误: {0}")]
    Network(String),
    #[error("配置错误: {0}")]
    Config(String),
    #[error("不支持的协议: {0}")]
    UnsupportedProtocol(String),
    #[error("核心错误: {0}")]
    Core(#[from] tachyon_core::DownloadError),
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            AppError::TaskNotFound(msg) => {
                map.serialize_entry("type", "TaskNotFound")?;
                map.serialize_entry("message", msg)?;
            }
            AppError::TaskAlreadyExists(msg) => {
                map.serialize_entry("type", "TaskAlreadyExists")?;
                map.serialize_entry("message", msg)?;
            }
            AppError::Network(msg) => {
                map.serialize_entry("type", "Network")?;
                map.serialize_entry("message", msg)?;
            }
            AppError::Config(msg) => {
                map.serialize_entry("type", "Config")?;
                map.serialize_entry("message", msg)?;
            }
            AppError::UnsupportedProtocol(msg) => {
                map.serialize_entry("type", "UnsupportedProtocol")?;
                map.serialize_entry("message", msg)?;
            }
            AppError::Core(err) => {
                map.serialize_entry("type", "Core")?;
                map.serialize_entry("message", &err.to_string())?;
            }
        }
        map.end()
    }
}

fn validate_download_url(url_str: &str) -> Result<(), AppError> {
    let url = Url::parse(url_str).map_err(|e| AppError::Network(format!("URL 格式无效: {e}")))?;
    tachyon_core::validate_public_http_url(&url).map_err(|e| AppError::Network(e.to_string()))?;

    let scheme = url.scheme().to_uppercase();
    let supported = supported_protocols();
    if !supported.iter().any(|p| *p == scheme) {
        return Err(AppError::UnsupportedProtocol(scheme));
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInfo {
    pub id: String,
    pub url: String,
    pub file_name: String,
    pub file_size: Option<u64>,
    pub downloaded: u64,
    pub speed: u64,
    pub status: DownloadState,
    pub progress: f64,
    pub fragments_total: u32,
    pub fragments_done: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub task_id: String,
    pub status: DownloadState,
    pub progress: f64,
    pub downloaded: u64,
    pub file_size: Option<u64>,
    pub speed: u64,
    pub fragments_total: u32,
    pub fragments_done: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgress {
    pub id: String,
    pub progress: f64,
    pub speed: u64,
    pub downloaded: u64,
    pub status: DownloadState,
    pub fragments_done: u32,
}

pub type ProgressEvent = HashMap<String, TaskProgress>;

use tachyon_sniffer::SnifferResource;

use crate::task_store::TaskStore;

pub struct AppState {
    pub tasks: DashMap<String, TaskInfo>,
    pub config: Arc<tokio::sync::Mutex<AppConfig>>,
    pub handles: Arc<DashMap<String, tokio::task::JoinHandle<()>>>,
    pub active_permits: Arc<AtomicU32>,
    pub sniffer: Arc<tokio::sync::Mutex<Vec<SnifferResource>>>,
    pub sniffer_filters: Arc<tokio::sync::Mutex<Vec<String>>>,
    pub progress_tx: watch::Sender<ProgressEvent>,
    pub connection_pool: Arc<ConnectionPool>,
    pub controls: Arc<DashMap<String, watch::Sender<DownloadState>>>,
    pub task_store: Arc<TaskStore>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let config = AppConfig {
            max_concurrent_tasks: 5,
            download: DownloadConfig::default(),
            connection: ConnectionConfig::default(),
            scheduler: Default::default(),
        };
        let connection_pool = ConnectionPool::new(PoolConfig::from(config.connection.clone()));
        let store_dir = tachyon_core::config::dirs()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".aimd")
            .join("store");
        let task_store = Arc::new(TaskStore::open(&store_dir).expect("任务存储初始化失败"));
        Self {
            tasks: DashMap::new(),
            config: Arc::new(tokio::sync::Mutex::new(config)),
            handles: Arc::new(DashMap::new()),
            active_permits: Arc::new(AtomicU32::new(0)),
            sniffer: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            sniffer_filters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            progress_tx: watch::Sender::new(HashMap::new()),
            connection_pool: Arc::new(connection_pool),
            controls: Arc::new(DashMap::new()),
            task_store,
        }
    }

    pub async fn load_recovered_tasks(&self) -> Result<(), AppError> {
        let snapshots = self.task_store.load_recoverable()?;
        for snapshot in snapshots {
            let task = crate::task_store::snapshot_to_task_info(&snapshot);
            self.tasks.insert(task.id.clone(), task);
        }
        Ok(())
    }
}

fn now_iso8601() -> String {
    Local::now().to_rfc3339()
}

fn validate_config(config: &AppConfig) -> Result<(), AppError> {
    if config.max_concurrent_tasks == 0 || config.max_concurrent_tasks > 64 {
        return Err(AppError::Config(format!(
            "max_concurrent_tasks 必须在 1..=64 范围内,当前值: {}",
            config.max_concurrent_tasks
        )));
    }
    if config.download.max_concurrent_fragments == 0
        || config.download.max_concurrent_fragments > 32
    {
        return Err(AppError::Config(format!(
            "max_concurrent_fragments 必须在 1..=32 范围内,当前值: {}",
            config.download.max_concurrent_fragments
        )));
    }
    if config.download.download_dir.is_empty() {
        return Err(AppError::Config("download_dir 不能为空".to_string()));
    }

    // 校验 authorized_dirs:每个目录必须存在且不能是系统根目录
    for dir in &config.download.authorized_dirs {
        let path = std::path::Path::new(dir);
        if !path.exists() {
            return Err(AppError::Config(format!(
                "authorized_dirs 路径不存在: {dir}"
            )));
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| AppError::Config(format!("authorized_dirs 路径无法解析: {dir}")))?;
        // 禁止系统根目录
        let canonical_str = canonical.to_string_lossy();
        let forbidden = canonical_str == "/"
            || canonical_str == "C:\\"
            || canonical_str == "C:/"
            || canonical.starts_with("/usr")
            || canonical.starts_with("/etc")
            || canonical.starts_with("/System");
        if forbidden {
            return Err(AppError::Config(format!(
                "authorized_dirs 不允许包含系统根目录: {dir}"
            )));
        }
    }

    // 校验 headers:禁止设置敏感请求头
    for key in config.download.headers.keys() {
        let lower = key.to_lowercase();
        if ["authorization", "cookie", "proxy-authorization"].contains(&lower.as_str()) {
            return Err(AppError::Config(format!("headers 不允许设置敏感头: {key}")));
        }
    }

    Ok(())
}

fn authorize_download_dir(config: &AppConfig, requested_dir: &str) -> Result<String, AppError> {
    let requested = std::path::Path::new(requested_dir);
    // 兼容不存在目录:canonicalize 失败时回退到原始路径(用于测试和目录创建前)
    let requested_canonical = requested
        .canonicalize()
        .unwrap_or_else(|_| requested.to_path_buf());

    let authorized = config.download.authorized_dirs.iter().any(|dir| {
        let path = std::path::Path::new(dir);
        path.canonicalize()
            .ok()
            .map(|canonical| requested_canonical.starts_with(&canonical))
            .unwrap_or(false)
    });

    if !authorized {
        return Err(AppError::Config(format!(
            "下载目录未授权: {}",
            requested.display()
        )));
    }
    // 返回原始请求路径而非 canonical 路径，避免 Windows 上 \\?\ 前缀不一致
    Ok(requested_dir.to_string())
}

fn resource_type_to_string(rt: ResourceType) -> &'static str {
    match rt {
        ResourceType::Video => "video",
        ResourceType::Audio => "audio",
        ResourceType::Document => "document",
        ResourceType::Archive => "archive",
        ResourceType::Executable => "executable",
        ResourceType::Image => "image",
        ResourceType::Other => "other",
    }
}

fn update_task_status(store: &DashMap<String, TaskInfo>, task_id: &str, new_status: DownloadState) {
    if let Some(mut task) = store.get_mut(task_id) {
        task.status = new_status;
        if new_status == DownloadState::Completed
            || new_status == DownloadState::Failed
            || new_status == DownloadState::Cancelled
        {
            task.speed = 0;
        }
    }
}

fn cleanup_runtime(state: &AppState, task_id: &str) {
    state.controls.remove(task_id);
    state.handles.remove(task_id);
}

async fn persist_task_snapshot(state: &AppState, task_id: &str, fail_reason: Option<String>) {
    let task = { state.tasks.get(task_id).map(|r| r.value().clone()) };
    if let Some(task) = task {
        let existing = state.task_store.load_snapshot(task_id).ok().flatten();
        let save_path = if let Some(snapshot) = existing.as_ref() {
            snapshot.save_path.clone()
        } else {
            let download_dir = state.config.lock().await.download.download_dir.clone();
            std::path::Path::new(&download_dir)
                .join(&task.file_name)
                .to_string_lossy()
                .to_string()
        };
        let mut snapshot =
            crate::task_store::task_info_to_snapshot(&task, save_path, 0, vec![], None, None);
        if let Some(existing) = existing {
            snapshot.fragment_size = existing.fragment_size;
            snapshot.completed_fragments = existing.completed_fragments;
            snapshot.etag = existing.etag;
            snapshot.last_modified = existing.last_modified;
            snapshot.retry_count = existing.retry_count;
        }
        snapshot.fail_reason = fail_reason;
        if let Err(e) = state.task_store.save_snapshot(&snapshot) {
            tracing::warn!(task_id = %task_id, error = %e, "保存任务状态快照失败");
        }
    }
}

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

/// 自动将 huggingface.co 替换为 HF_ENDPOINT 镜像地址
///
/// 检测逻辑:
/// 1. 如果设置了 HF_ENDPOINT 环境变量,替换 URL 中的 huggingface.co → HF_ENDPOINT
/// 2. 如果未设置,检查是否能连接 huggingface.co,不能则自动使用 hf-mirror.com
fn rewrite_hf_url(url: &str) -> String {
    if !url.contains("huggingface.co") {
        return url.to_string();
    }

    let mirror = std::env::var("HF_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://hf-mirror.com".to_string());

    let rewritten = url.replace("https://huggingface.co", &mirror);
    if rewritten != url {
        tracing::info!(original = %url, rewritten = %rewritten, "HF 下载自动切换至镜像源");
    }
    rewritten
}

fn build_download_config(app_config: &AppConfig, download_dir: &str) -> DownloadConfig {
    let mut download = app_config.download.clone();
    download.download_dir = download_dir.to_string();
    download
}

async fn task_fn(
    state: Arc<AppState>,
    task_id: String,
    url: String,
    download_dir: String,
    download_config: DownloadConfig,
    connection_pool: Arc<ConnectionPool>,
    control_rx: watch::Receiver<DownloadState>,
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

    let mut download_task =
        match DownloadTask::with_pool(url.clone(), download_config, Some(connection_pool)).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(task_id = %task_id, error = %e, "创建 DownloadTask 失败");
                update_task_status(&state.tasks, &task_id, DownloadState::Failed);
                return;
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
                    .map(|s| (s.max(1) + 1024 * 1024 - 1) / (1024 * 1024))
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
        let mut completed: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
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
            if event_count == 1 || event_count % 50 == 0 {
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

#[derive(Serialize)]
pub struct AppInfo {
    pub version: &'static str,
    pub name: &'static str,
}

#[tauri::command]
pub fn get_app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        name: "Tachyon",
    }
}

#[tauri::command]
pub fn supported_protocols() -> Vec<&'static str> {
    vec!["HTTP", "HTTPS"]
}

#[tauri::command]
pub async fn create_task(
    state: tauri::State<'_, AppState>,
    url: String,
    download_dir: Option<String>,
    _mirror_urls: Option<Vec<String>>,
) -> Result<String, AppError> {
    validate_download_url(&url)?;
    let task_id = Uuid::new_v4().to_string();
    let file_name = extract_filename_from_url(&url);
    let created_at = now_iso8601();

    {
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
        let max_tasks = state.config.lock().await.max_concurrent_tasks as usize;
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
    }

    let download_dir_str = {
        let cfg = state.config.lock().await;
        let requested = download_dir.unwrap_or_else(|| cfg.download.download_dir.clone());
        authorize_download_dir(&cfg, &requested)?
    };

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

    {
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
    });

    let (control_tx, control_rx) = watch::channel(DownloadState::Downloading);
    state.controls.insert(task_id.clone(), control_tx);

    let tid = task_id.clone();
    let url_clone = url.clone();
    let pool_clone = state_arc.connection_pool.clone();
    let handle = tokio::spawn(async move {
        task_fn(
            state_arc,
            tid,
            url_clone,
            download_dir_str,
            download_config,
            pool_clone,
            control_rx,
        )
        .await;
    });

    state.handles.insert(task_id.clone(), handle);

    tracing::info!(task_id = %task_id, "创建下载任务并启动后台下载");
    Ok(task_id)
}

#[tauri::command]
pub async fn pause_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
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
    persist_task_snapshot(&state, &task_id, None).await;
    Ok(())
}

#[tauri::command]
pub async fn resume_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
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
    persist_task_snapshot(&state, &task_id, None).await;
    Ok(())
}

#[tauri::command]
pub async fn cancel_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
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
    persist_task_snapshot(&state, &task_id, None).await;
    Ok(())
}

#[tauri::command]
pub async fn delete_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
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

#[tauri::command]
pub async fn get_task_list(state: tauri::State<'_, AppState>) -> Result<Vec<TaskInfo>, AppError> {
    Ok(state.tasks.iter().map(|r| r.value().clone()).collect())
}

#[tauri::command]
pub async fn get_task_detail(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<TaskInfo, AppError> {
    state
        .tasks
        .get(&task_id)
        .map(|r| r.value().clone())
        .ok_or(AppError::TaskNotFound(task_id))
}

#[tauri::command]
pub async fn get_download_progress(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<DownloadProgress, AppError> {
    let task = state
        .tasks
        .get(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;

    Ok(DownloadProgress {
        task_id: task.id.clone(),
        status: task.status,
        progress: task.progress,
        downloaded: task.downloaded,
        file_size: task.file_size,
        speed: task.speed,
        fragments_total: task.fragments_total,
        fragments_done: task.fragments_done,
    })
}

#[tauri::command]
pub async fn get_sniffer_resources(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SnifferResource>, AppError> {
    let store = state.sniffer.lock().await;
    Ok(store.iter().rev().cloned().collect())
}

#[tauri::command]
pub async fn add_sniffer_filter(
    state: tauri::State<'_, AppState>,
    filter: String,
) -> Result<(), AppError> {
    if filter.is_empty() {
        return Err(AppError::Config("过滤规则不能为空".to_string()));
    }
    let mut filters = state.sniffer_filters.lock().await;
    if filters.contains(&filter) {
        return Err(AppError::Config("过滤规则已存在".to_string()));
    }
    tracing::info!(filter = %filter, "添加嗅探过滤规则");
    filters.push(filter);
    Ok(())
}

pub async fn add_sniffer_resource(state: &AppState, url: String) {
    let filters = state.sniffer_filters.lock().await;
    if !filters.is_empty() && !filters.iter().any(|f| url.contains(f.as_str())) {
        return;
    }
    drop(filters);

    let resource_type = identify_resource(&url);
    let file_name = extract_filename_from_url(&url);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let resource = SnifferResource {
        id: Uuid::new_v4().to_string(),
        url: url.clone(),
        file_name,
        resource_type: resource_type_to_string(resource_type).to_string(),
        file_size: None,
        content_type: None,
        discovered_at: now,
        source_page: None,
    };

    let mut store = state.sniffer.lock().await;

    if store.iter().any(|r| r.url == url) {
        return;
    }

    const MAX_SNIFFER_RESOURCES: usize = 1000;
    if store.len() >= MAX_SNIFFER_RESOURCES {
        store.remove(0);
    }

    tracing::info!(url = %tachyon_core::redact_url_for_log(&url), resource_type = %resource.resource_type, "捕获新资源");
    store.push(resource);
}

#[tauri::command]
pub async fn get_config(state: tauri::State<'_, AppState>) -> Result<AppConfig, AppError> {
    let cfg = state.config.lock().await;
    Ok(cfg.clone())
}

#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, AppState>,
    config: AppConfig,
) -> Result<(), AppError> {
    validate_config(&config)?;
    let mut cfg = state.config.lock().await;
    *cfg = config;
    tracing::info!("应用配置已更新");
    Ok(())
}

#[tauri::command]
pub async fn subscribe_progress(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    use tauri::Emitter;

    let mut rx = state.progress_tx.subscribe();
    let tasks = state.tasks.clone();

    tokio::spawn(async move {
        {
            let event: ProgressEvent = tasks
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
            let _ = app_handle.emit("progress-update", &event);
        }

        while rx.changed().await.is_ok() {
            let snapshot = (*rx.borrow_and_update()).clone();
            for (tid, tp) in &snapshot {
                if tp.downloaded > 0 || tp.speed > 0 {
                    tracing::info!(
                        tid,
                        downloaded = tp.downloaded,
                        speed = tp.speed,
                        "emit progress-update"
                    );
                }
            }
            let _ = app_handle.emit("progress-update", &snapshot);
        }
    });

    Ok(())
}

#[cfg(test)]
async fn create_task_inner(
    state: &AppState,
    url: String,
    download_dir: Option<String>,
) -> Result<String, AppError> {
    let task_id = Uuid::new_v4().to_string();
    let file_name = extract_filename_from_url(&url);
    let created_at = now_iso8601();

    {
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
        let max_tasks = state.config.lock().await.max_concurrent_tasks as usize;
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
    }

    let download_dir_str = {
        let cfg = state.config.lock().await;
        let requested = download_dir.unwrap_or_else(|| cfg.download.download_dir.clone());
        authorize_download_dir(&cfg, &requested)?
    };

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

    {
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
    });

    let (control_tx, control_rx) = watch::channel(DownloadState::Downloading);
    state.controls.insert(task_id.clone(), control_tx);

    let tid = task_id.clone();
    let url_clone = url.clone();
    let pool_clone = state_arc.connection_pool.clone();
    let handle = tokio::spawn(async move {
        task_fn(
            state_arc,
            tid,
            url_clone,
            download_dir_str,
            download_config,
            pool_clone,
            control_rx,
        )
        .await;
    });

    state.handles.insert(task_id.clone(), handle);

    tracing::info!(task_id = %task_id, "创建下载任务并启动后台下载");
    Ok(task_id)
}

#[cfg(test)]
async fn pause_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
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
            Ok(())
        }
        other => Err(AppError::Config(format!("当前状态 '{}' 不允许暂停", other))),
    }
}

#[cfg(test)]
async fn resume_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let mut task = state
        .tasks
        .get_mut(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    if task.status == DownloadState::Paused {
        task.status = DownloadState::Downloading;
        if let Some(control) = state.controls.get(&task_id) {
            let _ = control.send(DownloadState::Downloading);
        }
        Ok(())
    } else {
        Err(AppError::Config(format!(
            "仅暂停状态可恢复,当前状态: '{}'",
            task.status
        )))
    }
}

#[cfg(test)]
async fn cancel_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let mut task = state
        .tasks
        .get_mut(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    match task.status {
        DownloadState::Completed | DownloadState::Cancelled => {
            Err(AppError::Config(format!("任务已{},无法取消", task.status)))
        }
        _ => {
            if let Some(control) = state.controls.get(&task_id) {
                let _ = control.send(DownloadState::Cancelled);
            }
            task.status = DownloadState::Cancelled;
            task.speed = 0;
            Ok(())
        }
    }
}

#[cfg(test)]
async fn delete_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let task = state
        .tasks
        .get(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    match task.status {
        DownloadState::Completed | DownloadState::Cancelled | DownloadState::Failed => {
            drop(task);
            state.tasks.remove(&task_id);
            state.handles.remove(&task_id);
            Ok(())
        }
        other => Err(AppError::Config(format!(
            "当前状态 '{}' 不允许删除,请先取消任务",
            other
        ))),
    }
}

#[cfg(test)]
async fn get_task_list_inner(state: &AppState) -> Result<Vec<TaskInfo>, AppError> {
    Ok(state.tasks.iter().map(|r| r.value().clone()).collect())
}

#[cfg(test)]
async fn get_task_detail_inner(state: &AppState, task_id: String) -> Result<TaskInfo, AppError> {
    state
        .tasks
        .get(&task_id)
        .map(|r| r.value().clone())
        .ok_or(AppError::TaskNotFound(task_id))
}

#[cfg(test)]
async fn get_download_progress_inner(
    state: &AppState,
    task_id: String,
) -> Result<DownloadProgress, AppError> {
    let task = state
        .tasks
        .get(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    Ok(DownloadProgress {
        task_id: task.id.clone(),
        status: task.status,
        progress: task.progress,
        downloaded: task.downloaded,
        file_size: task.file_size,
        speed: task.speed,
        fragments_total: task.fragments_total,
        fragments_done: task.fragments_done,
    })
}

#[cfg(test)]
async fn get_sniffer_resources_inner(state: &AppState) -> Result<Vec<SnifferResource>, AppError> {
    let store = state.sniffer.lock().await;
    Ok(store.iter().rev().cloned().collect())
}

#[cfg(test)]
async fn add_sniffer_filter_inner(state: &AppState, filter: String) -> Result<(), AppError> {
    if filter.is_empty() {
        return Err(AppError::Config("过滤规则不能为空".to_string()));
    }
    let mut filters = state.sniffer_filters.lock().await;
    if filters.contains(&filter) {
        return Err(AppError::Config("过滤规则已存在".to_string()));
    }
    filters.push(filter);
    Ok(())
}

#[cfg(test)]
async fn get_config_inner(state: &AppState) -> Result<AppConfig, AppError> {
    let cfg = state.config.lock().await;
    Ok(cfg.clone())
}

#[cfg(test)]
async fn update_config_inner(state: &AppState, config: AppConfig) -> Result<(), AppError> {
    validate_config(&config)?;
    let mut cfg = state.config.lock().await;
    *cfg = config;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachyon_core::config::USER_AGENT;
    use tachyon_core::filename::parse_content_disposition;

    /// 创建临时测试路径，在所有平台上均有效
    fn test_tmp_path(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("tachyon-test-{name}"));
        let _ = std::fs::create_dir_all(&dir);
        dir.to_string_lossy().to_string()
    }

    fn make_test_app_config(
        max_concurrent_tasks: u32,
        download_dir: &str,
        max_concurrent_fragments: u32,
        max_connections_per_host: u32,
        enable_quic: bool,
        verify_checksum: bool,
    ) -> AppConfig {
        AppConfig {
            max_concurrent_tasks,
            download: tachyon_core::config::DownloadConfig {
                download_dir: download_dir.to_string(),
                max_concurrent_fragments,
                max_retries: 3,
                request_timeout_secs: 30,
                connect_timeout_secs: 10,
                verify_checksum,
                user_agent: USER_AGENT.to_string(),
                headers: std::collections::HashMap::new(),
                pause_timeout_secs: 300,
                rate_limit_bytes_per_sec: None,
                authorized_dirs: vec![download_dir.to_string()],
            },
            connection: tachyon_core::config::ConnectionConfig {
                max_connections_per_host,
                max_global_connections: 256,
                keep_alive_timeout_secs: 30,
                connect_timeout_secs: 10,
                enable_http2: true,
                enable_quic,
            },
            scheduler: tachyon_core::config::SchedulerConfig::default(),
        }
    }

    fn test_state() -> Arc<AppState> {
        let tmp_store = tempfile::tempdir().unwrap();
        let test_dir = std::env::temp_dir()
            .join("tachyon-test-downloads")
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&test_dir);
        Arc::new(AppState {
            tasks: DashMap::new(),
            config: Arc::new(tokio::sync::Mutex::new(AppConfig {
                max_concurrent_tasks: 5,
                download: DownloadConfig {
                    download_dir: test_dir.clone(),
                    authorized_dirs: vec![test_dir.clone()],
                    ..DownloadConfig::default()
                },
                connection: ConnectionConfig::default(),
                scheduler: Default::default(),
            })),
            handles: Arc::new(DashMap::new()),
            active_permits: Arc::new(AtomicU32::new(0)),
            sniffer: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            sniffer_filters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            progress_tx: watch::Sender::new(HashMap::new()),
            connection_pool: Arc::new(ConnectionPool::new(PoolConfig {
                max_per_host: 16,
                max_global: 256,
            })),
            controls: Arc::new(DashMap::new()),
            task_store: Arc::new(crate::task_store::TaskStore::open(tmp_store.path()).unwrap()),
        })
    }

    #[tokio::test]
    async fn test_create_task_returns_valid_uuid() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
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
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.file_name, "app-v2.0.tar.gz");
    }

    #[tokio::test]
    async fn test_create_task_default_status_is_pending() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/data.bin".to_string(), None)
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
        )
        .await
        .unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.url, "https://example.com/file.zip");
    }

    #[tokio::test]
    async fn test_create_task_duplicate_url_rejected() {
        let state = test_state();
        let _ = create_task_inner(&state, "https://dup.example.com/once.zip".to_string(), None)
            .await
            .unwrap();
        let result =
            create_task_inner(&state, "https://dup.example.com/once.zip".to_string(), None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("已存在"));
    }

    #[tokio::test]
    async fn test_pause_pending_task() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
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
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
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
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
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
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
            .await
            .unwrap();
        let result = resume_task_inner(&state, id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("仅暂停状态可恢复"));
    }

    #[tokio::test]
    async fn test_cancel_pending_task() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
            .await
            .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, DownloadState::Cancelled);
    }

    #[tokio::test]
    async fn test_cancel_already_cancelled_task_fails() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
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
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
            .await
            .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        delete_task_inner(&state, id.clone()).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    #[tokio::test]
    async fn test_delete_pending_task_fails() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
            .await
            .unwrap();
        let result = delete_task_inner(&state, id.clone()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不允许删除"));
    }

    #[tokio::test]
    async fn test_get_task_list_returns_all_tasks() {
        let state = test_state();
        let id1 = create_task_inner(&state, "https://example.com/a.zip".to_string(), None)
            .await
            .unwrap();
        let id2 = create_task_inner(&state, "https://example.com/b.zip".to_string(), None)
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
    async fn test_get_config_returns_defaults() {
        let state = test_state();
        let cfg = get_config_inner(&state).await.unwrap();
        assert_eq!(cfg.max_concurrent_tasks, 5);
        assert_eq!(cfg.download.max_concurrent_fragments, 16);
        assert_eq!(cfg.connection.max_connections_per_host, 16);
        assert!(!cfg.connection.enable_quic);
        assert!(cfg.download.verify_checksum);
    }

    #[tokio::test]
    async fn test_update_config_roundtrip() {
        let state = test_state();
        let temp_dir = tempfile::tempdir().unwrap();
        let dl_dir = temp_dir.path().join("downloads");
        std::fs::create_dir_all(&dl_dir).unwrap();
        let dl_dir_str = dl_dir.to_string_lossy().to_string();

        let new_cfg = AppConfig {
            max_concurrent_tasks: 10,
            download: tachyon_core::config::DownloadConfig {
                download_dir: dl_dir_str.clone(),
                max_concurrent_fragments: 32,
                max_retries: 3,
                request_timeout_secs: 30,
                connect_timeout_secs: 10,
                verify_checksum: false,
                user_agent: USER_AGENT.to_string(),
                headers: std::collections::HashMap::new(),
                pause_timeout_secs: 300,
                rate_limit_bytes_per_sec: None,
                authorized_dirs: vec![dl_dir_str.clone()],
            },
            connection: tachyon_core::config::ConnectionConfig {
                max_connections_per_host: 8,
                max_global_connections: 256,
                keep_alive_timeout_secs: 30,
                connect_timeout_secs: 10,
                enable_http2: true,
                enable_quic: true,
            },
            scheduler: tachyon_core::config::SchedulerConfig::default(),
        };
        update_config_inner(&state, new_cfg).await.unwrap();
        let cfg = get_config_inner(&state).await.unwrap();
        assert_eq!(cfg.download.download_dir, dl_dir_str);
        assert_eq!(cfg.max_concurrent_tasks, 10);
        assert_eq!(cfg.download.max_concurrent_fragments, 32);
        assert_eq!(cfg.connection.max_connections_per_host, 8);
        assert!(cfg.connection.enable_quic);
        assert!(!cfg.download.verify_checksum);
    }

    #[tokio::test]
    async fn test_update_config_rejects_invalid_without_mutating_current_config() {
        let state = test_state();
        let before = get_config_inner(&state).await.unwrap();
        let mut invalid = before.clone();
        invalid.download.max_concurrent_fragments = 128;

        let result = update_config_inner(&state, invalid).await;

        assert!(result.is_err());
        let after = get_config_inner(&state).await.unwrap();
        assert_eq!(
            after.download.max_concurrent_fragments,
            before.download.max_concurrent_fragments
        );
        assert_eq!(after.download.download_dir, before.download.download_dir);
    }

    #[test]
    fn test_build_download_config_preserves_download_fields() {
        let mut cfg = AppConfig::default();
        cfg.download.max_retries = 9;
        cfg.download.request_timeout_secs = 120;
        cfg.download.user_agent = "Tachyon/Custom".to_string();
        cfg.download
            .headers
            .insert("Authorization".to_string(), "Bearer token".to_string());
        cfg.download.pause_timeout_secs = 42;
        cfg.download.authorized_dirs = vec!["/allowed".to_string()];

        let download = build_download_config(&cfg, "/chosen");

        assert_eq!(download.download_dir, "/chosen");
        assert_eq!(download.max_retries, 9);
        assert_eq!(download.request_timeout_secs, 120);
        assert_eq!(download.user_agent, "Tachyon/Custom");
        assert_eq!(
            download.headers.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
        assert_eq!(download.pause_timeout_secs, 42);
        assert_eq!(download.authorized_dirs, vec!["/allowed".to_string()]);
    }

    #[tokio::test]
    async fn test_persist_task_snapshot_preserves_existing_save_path() {
        let state = test_state();
        let task = TaskInfo {
            id: "task-custom-path".to_string(),
            url: "https://example.com/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1024),
            downloaded: 128,
            speed: 0,
            status: DownloadState::Paused,
            progress: 0.125,
            fragments_total: 4,
            fragments_done: 1,
            created_at: "2026-05-29T00:00:00Z".to_string(),
        };
        state.tasks.insert(task.id.clone(), task.clone());
        let original_snapshot = crate::task_store::task_info_to_snapshot(
            &task,
            "/custom/file.bin".to_string(),
            256,
            vec![0],
            None,
            None,
        );
        state.task_store.save_snapshot(&original_snapshot).unwrap();

        persist_task_snapshot(&state, &task.id, None).await;

        let loaded = state.task_store.load_recoverable().unwrap();
        let snapshot = loaded
            .iter()
            .find(|snapshot| snapshot.id == task.id)
            .unwrap();
        assert_eq!(snapshot.save_path, "/custom/file.bin");
    }

    #[test]
    fn test_extract_filename_basic() {
        assert_eq!(
            extract_filename_from_url("https://example.com/path/to/file.zip"),
            "file.zip"
        );
    }

    #[test]
    fn test_extract_filename_with_query() {
        assert_eq!(
            extract_filename_from_url("https://example.com/download?file=test.bin"),
            "download"
        );
    }

    #[test]
    fn test_extract_filename_empty_path() {
        assert_eq!(extract_filename_from_url("https://example.com/"), "unknown");
    }

    #[test]
    fn test_extract_filename_encoded() {
        assert_eq!(
            extract_filename_from_url("https://example.com/my%20file.txt"),
            "my file.txt"
        );
    }

    #[test]
    fn test_extract_filename_invalid_url() {
        assert_eq!(extract_filename_from_url("not a url"), "unknown");
    }

    #[test]
    fn test_extract_filename_with_invalid_hex_encoding() {
        assert_eq!(
            extract_filename_from_url("https://example.com/file%GG.txt"),
            "file%GG.txt"
        );
    }

    #[tokio::test]
    async fn test_full_task_lifecycle() {
        let state = test_state();
        let id = create_task_inner(
            &state,
            "https://example.com/lifecycle.bin".to_string(),
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
    async fn test_get_download_progress() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/progress.bin".to_string(), None)
            .await
            .unwrap();
        let progress = get_download_progress_inner(&state, id.clone())
            .await
            .unwrap();
        assert_eq!(progress.task_id, id);
        assert_eq!(progress.status, DownloadState::Pending);
        assert!((progress.progress - 0.0).abs() < f64::EPSILON);
        assert_eq!(progress.downloaded, 0);
        assert_eq!(progress.speed, 0);
        assert_eq!(progress.fragments_total, 0);
        assert_eq!(progress.fragments_done, 0);
    }

    #[tokio::test]
    async fn test_get_download_progress_not_found() {
        let state = test_state();
        let result = get_download_progress_inner(&state, "nonexistent".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("任务不存在"));
    }

    #[test]
    fn test_download_progress_serialization() {
        let progress = DownloadProgress {
            task_id: "test-id".to_string(),
            status: DownloadState::Downloading,
            progress: 0.5,
            downloaded: 512,
            file_size: Some(1024),
            speed: 100,
            fragments_total: 4,
            fragments_done: 2,
        };
        let json = serde_json::to_string(&progress).unwrap();
        assert!(json.contains("taskId"));
        assert!(json.contains("fileSize"));
        assert!(json.contains("fragmentsTotal"));
    }

    #[tokio::test]
    async fn test_get_sniffer_resources_empty() {
        let state = test_state();
        let resources = get_sniffer_resources_inner(&state).await.unwrap();
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn test_add_sniffer_filter() {
        let state = test_state();
        add_sniffer_filter_inner(&state, "cdn.example.com".to_string())
            .await
            .unwrap();
        let result = add_sniffer_filter_inner(&state, "cdn.example.com".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("已存在"));
    }

    #[tokio::test]
    async fn test_add_sniffer_filter_empty_string_fails() {
        let state = test_state();
        let result = add_sniffer_filter_inner(&state, String::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不能为空"));
    }

    #[tokio::test]
    async fn test_add_sniffer_resource() {
        let state = test_state();
        add_sniffer_resource(&state, "http://example.com/video.mp4".to_string()).await;
        let resources = get_sniffer_resources_inner(&state).await.unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].url, "http://example.com/video.mp4");
        assert_eq!(resources[0].resource_type, "video");
        assert_eq!(resources[0].file_name, "video.mp4");
    }

    #[tokio::test]
    async fn test_add_sniffer_resource_duplicate_ignored() {
        let state = test_state();
        add_sniffer_resource(&state, "http://example.com/file.zip".to_string()).await;
        add_sniffer_resource(&state, "http://example.com/file.zip".to_string()).await;
        let resources = get_sniffer_resources_inner(&state).await.unwrap();
        assert_eq!(resources.len(), 1, "重复 URL 应被忽略");
    }

    #[tokio::test]
    async fn test_add_sniffer_resource_with_filter() {
        let state = test_state();
        add_sniffer_filter_inner(&state, "cdn.example.com".to_string())
            .await
            .unwrap();
        add_sniffer_resource(&state, "http://other.com/video.mp4".to_string()).await;
        assert_eq!(get_sniffer_resources_inner(&state).await.unwrap().len(), 0);
        add_sniffer_resource(&state, "http://cdn.example.com/video.mp4".to_string()).await;
        assert_eq!(get_sniffer_resources_inner(&state).await.unwrap().len(), 1);
    }

    #[test]
    fn test_resource_type_to_string_all_variants() {
        assert_eq!(resource_type_to_string(ResourceType::Video), "video");
        assert_eq!(resource_type_to_string(ResourceType::Audio), "audio");
        assert_eq!(resource_type_to_string(ResourceType::Document), "document");
        assert_eq!(resource_type_to_string(ResourceType::Archive), "archive");
        assert_eq!(
            resource_type_to_string(ResourceType::Executable),
            "executable"
        );
        assert_eq!(resource_type_to_string(ResourceType::Image), "image");
        assert_eq!(resource_type_to_string(ResourceType::Other), "other");
    }

    #[test]
    fn test_app_config_serialization_roundtrip() {
        let cfg = AppConfig {
            max_concurrent_tasks: 3,
            download: tachyon_core::config::DownloadConfig {
                download_dir: "/tmp".to_string(),
                max_concurrent_fragments: 8,
                max_retries: 3,
                request_timeout_secs: 30,
                connect_timeout_secs: 10,
                verify_checksum: false,
                user_agent: USER_AGENT.to_string(),
                headers: std::collections::HashMap::new(),
                pause_timeout_secs: 300,
                rate_limit_bytes_per_sec: None,
                authorized_dirs: vec!["/tmp".to_string()],
            },
            connection: tachyon_core::config::ConnectionConfig {
                max_connections_per_host: 4,
                max_global_connections: 256,
                keep_alive_timeout_secs: 30,
                connect_timeout_secs: 10,
                enable_http2: true,
                enable_quic: true,
            },
            scheduler: tachyon_core::config::SchedulerConfig::default(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let deserialized: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.download.download_dir, "/tmp");
        assert_eq!(deserialized.max_concurrent_tasks, 3);
        assert!(deserialized.connection.enable_quic);
        assert!(!deserialized.download.verify_checksum);
    }

    #[test]
    fn test_task_info_serialization_roundtrip() {
        let task = TaskInfo {
            id: "test-id".to_string(),
            url: "https://example.com/file.zip".to_string(),
            file_name: "file.zip".to_string(),
            file_size: Some(1024),
            downloaded: 512,
            speed: 100,
            status: DownloadState::Downloading,
            progress: 0.5,
            fragments_total: 4,
            fragments_done: 2,
            created_at: "2025-01-01T00:00:00+08:00".to_string(),
        };
        let json = serde_json::to_string(&task).unwrap();
        let deserialized: TaskInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "test-id");
        assert_eq!(deserialized.file_size, Some(1024));
        assert!((deserialized.progress - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_disposition_filename_simple() {
        assert_eq!(
            parse_content_disposition(r#"attachment; filename="file.zip""#),
            Some("file.zip".to_string())
        );
    }

    #[test]
    fn test_disposition_filename_encoded() {
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''my%20file.zip"),
            Some("my file.zip".to_string())
        );
    }

    #[test]
    fn test_disposition_filename_none() {
        assert_eq!(parse_content_disposition("inline"), None);
    }

    #[tokio::test]
    async fn test_any_fragment_failed_detection() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/fail.bin".to_string(), None)
            .await
            .unwrap();
        let task = get_task_detail_inner(&state, id.clone()).await.unwrap();
        assert_eq!(task.status, DownloadState::Pending);
        assert_ne!(task.status, DownloadState::Failed);
    }

    #[tokio::test]
    async fn test_max_concurrent_semaphore_gating() {
        let state = AppState::new();
        {
            let mut cfg = state.config.lock().await;
            cfg.max_concurrent_tasks = 2;
            // 设置有效下载目录，确保 authorized_dirs 校验通过
            let test_dir = std::env::temp_dir().join("tachyon-test-concurrent");
            let test_dir_str = test_dir.to_string_lossy().to_string();
            let _ = std::fs::create_dir_all(&test_dir);
            cfg.download.download_dir = test_dir_str.clone();
            cfg.download.authorized_dirs = vec![test_dir_str];
        }
        let _id1 = create_task_inner(&state, "http://example.com/gate1.bin".into(), None)
            .await
            .unwrap();
        let _id2 = create_task_inner(&state, "http://example.com/gate2.bin".into(), None)
            .await
            .unwrap();
        let result = create_task_inner(&state, "http://example.com/gate3.bin".into(), None).await;
        assert!(result.is_err(), "超过 max_concurrent_tasks 应被拒绝");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("最大并发任务数"),
            "错误应说明并发限制: {err}"
        );
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
        let _id1 = create_task_inner(&state, "http://example.com/file1.bin".into(), None)
            .await
            .unwrap();
        let _id2 = create_task_inner(&state, "http://example.com/file2.bin".into(), None)
            .await
            .unwrap();
        let result = create_task_inner(&state, "http://example.com/file3.bin".into(), None).await;
        assert!(result.is_err(), "超过 max_concurrent_tasks 应返回错误");
        assert!(
            result.unwrap_err().to_string().contains("最大并发任务数"),
            "错误信息应提及并发限制"
        );
    }

    #[tokio::test]
    async fn test_update_config_rejects_zero_max_concurrent_tasks() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            make_test_app_config(0, &test_tmp_path("z"), 16, 16, false, true),
        )
        .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("max_concurrent_tasks")
        );
    }

    #[tokio::test]
    async fn test_update_config_rejects_zero_max_concurrent_fragments() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            make_test_app_config(5, &test_tmp_path("a"), 0, 16, false, true),
        )
        .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("max_concurrent_fragments")
        );
    }

    #[tokio::test]
    async fn test_update_config_rejects_too_large_tasks() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            make_test_app_config(65, &test_tmp_path("b"), 16, 16, false, true),
        )
        .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("max_concurrent_tasks")
        );
    }

    #[tokio::test]
    async fn test_update_config_rejects_too_large_fragments() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            make_test_app_config(5, &test_tmp_path("c"), 33, 16, false, true),
        )
        .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("max_concurrent_fragments")
        );
    }

    #[tokio::test]
    async fn test_update_config_rejects_empty_download_dir() {
        let state = test_state();
        let result =
            update_config_inner(&state, make_test_app_config(5, "", 16, 16, false, true)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("download_dir"));
    }

    #[tokio::test]
    async fn test_update_config_accepts_valid_boundary_values() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            make_test_app_config(1, &test_tmp_path("d"), 1, 1, false, true),
        )
        .await;
        assert!(result.is_ok());

        let result = update_config_inner(
            &state,
            make_test_app_config(64, &test_tmp_path("e"), 32, 16, false, true),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_zero_max_concurrent_fragments_marks_task_failed() {
        let state = test_state();
        {
            let mut cfg = state.config.lock().await;
            cfg.download.max_concurrent_fragments = 0;
        }
        let result =
            create_task_inner(&state, "http://example.com/zero-sem.bin".into(), None).await;
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
        )
        .await
        .unwrap();

        let mut handles = Vec::new();

        for i in 0..10 {
            let state_clone = state.clone();
            let tid = id.clone();
            if i % 2 == 0 {
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

    #[test]
    fn test_authorize_download_dir_rejects_unlisted_dir() {
        let safe_dir = tempfile::tempdir().unwrap();
        let evil_dir = tempfile::tempdir().unwrap();
        let evil_path = evil_dir.path().to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.download.download_dir = safe_dir.path().to_string_lossy().to_string();
        config.download.authorized_dirs = vec![safe_dir.path().to_string_lossy().to_string()];

        let err = authorize_download_dir(&config, &evil_path).unwrap_err();
        assert!(err.to_string().contains("未授权"));
    }

    #[test]
    fn test_authorize_download_dir_accepts_default_dir() {
        let safe_dir = tempfile::tempdir().unwrap();
        let safe_path = safe_dir.path().to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.download.download_dir = safe_path.clone();
        config.download.authorized_dirs = vec![safe_path.clone()];

        assert_eq!(
            authorize_download_dir(&config, &safe_path).unwrap(),
            safe_path
        );
    }

    #[test]
    fn test_authorize_download_dir_accepts_subdir() {
        let safe_dir = tempfile::tempdir().unwrap();
        let safe_path = safe_dir.path().to_string_lossy().to_string();
        let sub_path = safe_dir.path().join("sub").to_string_lossy().to_string();
        std::fs::create_dir_all(&sub_path).unwrap();

        let mut config = AppConfig::default();
        config.download.download_dir = safe_path.clone();
        config.download.authorized_dirs = vec![safe_path.clone()];

        assert_eq!(
            authorize_download_dir(&config, &sub_path).unwrap(),
            sub_path
        );
    }

    #[test]
    fn test_authorize_download_dir_rejects_path_traversal() {
        let safe_dir = tempfile::tempdir().unwrap();
        let evil_dir = tempfile::tempdir().unwrap();
        let safe_path = safe_dir.path().to_string_lossy().to_string();
        let evil_path = evil_dir.path().to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.download.download_dir = safe_path.clone();
        config.download.authorized_dirs = vec![safe_path.clone()];

        let err = authorize_download_dir(&config, &evil_path).unwrap_err();
        assert!(err.to_string().contains("未授权"));
    }

    #[test]
    fn test_authorize_download_dir_rejects_nonexistent_dir() {
        let mut config = AppConfig::default();
        config.download.download_dir = "/nonexistent/path".to_string();
        config.download.authorized_dirs = vec![test_tmp_path("nonexist")];

        let err = authorize_download_dir(&config, "/nonexistent/path").unwrap_err();
        // 当请求目录不存在且不在授权列表中时,应拒绝
        assert!(err.to_string().contains("未授权"));
    }
}
