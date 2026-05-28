use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::{Duration, Instant};

use chrono::Local;
use dashmap::DashMap;
use qf_core::config::{AppConfig, ConnectionConfig, DownloadConfig, USER_AGENT};
use qf_core::filename::extract_filename_from_url;
use qf_core::types::DownloadState;
use qf_engine::DownloadTask;
use qf_engine::connection::{ConnectionPool, PoolConfig};
use qf_sniffer::capture::{ResourceType, identify_resource};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::sync::watch;
use url::Url;
use uuid::Uuid;

#[derive(Debug, thiserror::Error, serde::Serialize)]
pub enum AppError {
    #[error("任务不存在: {0}")]
    TaskNotFound(String),
    #[error("任务已存在: {0}")]
    TaskAlreadyExists(String),
    #[error("网络错误: {0}")]
    Network(String),
    #[error("配置错误: {0}")]
    Config(String),
}

fn validate_download_url(url_str: &str) -> Result<(), AppError> {
    let url = Url::parse(url_str).map_err(|e| AppError::Network(format!("URL 格式无效: {e}")))?;

    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(AppError::Network(format!(
                "不支持的协议: {scheme}，仅允许 http/https"
            )));
        }
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::Network("URL 中不允许包含用户名或密码".into()));
    }

    if let Some(host) = url.host_str() {
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if ip.is_loopback() {
                return Err(AppError::Network("不允许访问环回地址".into()));
            }
            if ip.is_unspecified() {
                return Err(AppError::Network("不允许访问未指定地址".into()));
            }
            match ip {
                std::net::IpAddr::V4(v4) => {
                    if v4.is_private() || v4.is_link_local() {
                        return Err(AppError::Network("不允许访问内网地址".into()));
                    }
                }
                std::net::IpAddr::V6(v6) => {
                    if v6.is_loopback() || v6.is_unspecified() {
                        return Err(AppError::Network("不允许访问 IPv6 环回/未指定地址".into()));
                    }
                }
            }
        }
        if host == "localhost" {
            return Err(AppError::Network("不允许访问 localhost".into()));
        }
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

use qf_sniffer::SnifferResource;

pub struct AppState {
    pub tasks: Arc<Mutex<HashMap<String, TaskInfo>>>,
    pub config: Arc<Mutex<AppConfig>>,
    pub handles: Arc<DashMap<String, tokio::task::JoinHandle<()>>>,
    pub active_permits: Arc<AtomicU32>,
    pub sniffer: Arc<Mutex<Vec<SnifferResource>>>,
    pub sniffer_filters: Arc<Mutex<Vec<String>>>,
    pub progress_tx: watch::Sender<ProgressEvent>,
    pub connection_pool: Arc<ConnectionPool>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let connection_pool = ConnectionPool::new(PoolConfig {
            max_per_host: 16,
            max_global: 256,
        });
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(Mutex::new(AppConfig {
                max_concurrent_tasks: 5,
                download: DownloadConfig::default(),
                connection: ConnectionConfig::default(),
                scheduler: Default::default(),
            })),
            handles: Arc::new(DashMap::new()),
            active_permits: Arc::new(AtomicU32::new(0)),
            sniffer: Arc::new(Mutex::new(Vec::new())),
            sniffer_filters: Arc::new(Mutex::new(Vec::new())),
            progress_tx: watch::Sender::new(HashMap::new()),
            connection_pool: Arc::new(connection_pool),
        }
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
    Ok(())
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

fn update_task_status(
    store: &mut HashMap<String, TaskInfo>,
    task_id: &str,
    new_status: DownloadState,
) {
    if let Some(task) = store.get_mut(task_id) {
        task.status = new_status;
        if new_status == DownloadState::Completed
            || new_status == DownloadState::Failed
            || new_status == DownloadState::Cancelled
        {
            task.speed = 0;
        }
    }
}

fn build_download_config(app_config: &AppConfig, download_dir: &str) -> DownloadConfig {
    DownloadConfig {
        download_dir: download_dir.to_string(),
        max_concurrent_fragments: app_config.download.max_concurrent_fragments,
        max_retries: 3,
        request_timeout_secs: 30,
        verify_checksum: app_config.download.verify_checksum,
        user_agent: USER_AGENT.to_string(),
        headers: std::collections::HashMap::new(),
    }
}

async fn task_fn(
    state: Arc<AppState>,
    task_id: String,
    url: String,
    download_dir: String,
    download_config: DownloadConfig,
    connection_pool: Arc<ConnectionPool>,
) {
    let download_url = match Url::parse(&url) {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "URL 解析失败");
            let mut store = state.tasks.lock().await;
            update_task_status(&mut store, &task_id, DownloadState::Failed);
            return;
        }
    };

    let host = match download_url.host_str() {
        Some(h) => h.to_string(),
        None => {
            tracing::error!(task_id = %task_id, "URL 主机为空");
            let mut store = state.tasks.lock().await;
            update_task_status(&mut store, &task_id, DownloadState::Failed);
            return;
        }
    };

    {
        let store = state.tasks.lock().await;
        if let Some(task) = store.get(&task_id) {
            if task.status == DownloadState::Cancelled {
                tracing::info!(task_id = %task_id, "任务已取消,跳过下载");
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

    if let Err(e) = std::fs::create_dir_all(&download_dir) {
        tracing::error!(task_id = %task_id, error = %e, "创建下载目录失败");
        let mut store = state.tasks.lock().await;
        update_task_status(&mut store, &task_id, DownloadState::Failed);
        return;
    }

    let mut download_task =
        match DownloadTask::with_pool(url.clone(), download_config, Some(connection_pool)).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(task_id = %task_id, error = %e, "创建 DownloadTask 失败");
                let mut store = state.tasks.lock().await;
                update_task_status(&mut store, &task_id, DownloadState::Failed);
                return;
            }
        };

    match download_task.probe().await {
        Ok(meta) => {
            tracing::info!(
                task_id = %task_id,
                file_name = %meta.file_name,
                file_size = ?meta.file_size,
                supports_range = meta.supports_range,
                "元数据探测成功"
            );

            {
                let mut store = state.tasks.lock().await;
                if let Some(task) = store.get_mut(&task_id) {
                    task.file_size = meta.file_size;
                }
            }
        }
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "元数据探测失败");
            let mut store = state.tasks.lock().await;
            update_task_status(&mut store, &task_id, DownloadState::Failed);
            return;
        }
    }

    let download_task = Arc::new(tokio::sync::Mutex::new(download_task));

    update_task_status(
        &mut *state.tasks.lock().await,
        &task_id,
        DownloadState::Downloading,
    );

    let monitor_state = state.clone();
    let monitor_task_id = task_id.clone();
    let cancel_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;

            let store = monitor_state.tasks.lock().await;
            match store.get(&monitor_task_id).map(|t| t.status) {
                Some(DownloadState::Cancelled) => {
                    tracing::info!(task_id = %monitor_task_id, "监控检测到任务已取消");
                    return;
                }
                Some(DownloadState::Paused) => {
                    drop(store);
                    let mut paused_ticks = 0u32;
                    loop {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        paused_ticks += 1;
                        let s = monitor_state.tasks.lock().await;
                        if s.get(&monitor_task_id)
                            .is_none_or(|t| t.status != DownloadState::Paused)
                        {
                            break;
                        }
                        if paused_ticks > 1500 {
                            tracing::warn!(task_id = %monitor_task_id, "暂停超时(5分钟),标记任务失败");
                            let mut s = monitor_state.tasks.lock().await;
                            update_task_status(&mut s, &monitor_task_id, DownloadState::Failed);
                            return;
                        }
                    }
                }
                Some(DownloadState::Failed) | None => return,
                _ => {}
            }
        }
    });

    let monitor_dt = download_task.clone();
    let monitor_ps = state.clone();
    let monitor_tid = task_id.clone();
    let progress_handle = tokio::spawn(async move {
        let start = Instant::now();
        let mut last_downloaded: u64 = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let dt = monitor_dt.lock().await;
            let p = dt.progress();
            let ds = dt.state();
            let downloaded = dt
                .fragment_infos()
                .iter()
                .map(|f| f.downloaded)
                .sum::<u64>();
            drop(dt);

            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                ((downloaded as f64 - last_downloaded as f64) / 0.5) as u64
            } else {
                0
            };
            last_downloaded = downloaded;

            {
                let mut store = monitor_ps.tasks.lock().await;
                if let Some(task) = store.get_mut(&monitor_tid) {
                    task.downloaded = downloaded;
                    task.speed = speed;
                    task.progress = p.min(1.0);
                }
            }

            {
                let store = monitor_ps.tasks.lock().await;
                let event: ProgressEvent = store
                    .iter()
                    .map(|(id, t)| {
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
                let _ = monitor_ps.progress_tx.send(event);
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

    cancel_handle.abort();

    {
        let mut store = state.tasks.lock().await;
        let current_status = store.get(&task_id).map(|t| t.status);

        match result {
            Ok(()) => {
                if current_status == Some(DownloadState::Cancelled) {
                    tracing::info!(task_id = %task_id, "下载完成但任务已被取消");
                } else if let Some(task) = store.get_mut(&task_id) {
                    task.progress = 1.0;
                    let dt = download_task.lock().await;
                    let final_size = dt.metadata().and_then(|m| m.file_size).unwrap_or(0);
                    task.downloaded = final_size;
                    task.speed = 0;
                    drop(dt);
                    update_task_status(&mut store, &task_id, DownloadState::Completed);
                    tracing::info!(task_id = %task_id, file_size = final_size, "下载任务完成");
                }
            }
            Err(e) => {
                if current_status == Some(DownloadState::Cancelled) {
                    tracing::info!(task_id = %task_id, "下载失败但任务已被取消,保留取消状态");
                } else {
                    update_task_status(&mut store, &task_id, DownloadState::Failed);
                    tracing::error!(task_id = %task_id, error = %e, "下载任务失败");
                }
            }
        }

        let event: ProgressEvent = store
            .iter()
            .map(|(id, t)| {
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
        name: "QuantumFetch",
    }
}

#[tauri::command]
pub fn supported_protocols() -> Vec<&'static str> {
    vec!["HTTP", "HTTPS", "FTP", "QUIC"]
}

#[tauri::command]
pub async fn create_task(
    state: tauri::State<'_, AppState>,
    url: String,
    download_dir: Option<String>,
) -> Result<String, AppError> {
    validate_download_url(&url)?;
    let task_id = Uuid::new_v4().to_string();
    let file_name = extract_filename_from_url(&url);
    let created_at = now_iso8601();

    {
        let store = state.tasks.lock().await;
        if store.values().any(|t| {
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
        let active_count = store
            .values()
            .filter(|t| {
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
        download_dir.unwrap_or_else(|| cfg.download.download_dir.clone())
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
        let mut store = state.tasks.lock().await;
        store.insert(task_id.clone(), task);
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
    });

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
    let mut store = state.tasks.lock().await;

    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;

    match task.status {
        DownloadState::Pending | DownloadState::Downloading => {
            task.status = DownloadState::Paused;
            task.speed = 0;
            tracing::info!(task_id = %task_id, "暂停任务");
            Ok(())
        }
        other => Err(AppError::Config(format!("当前状态 '{}' 不允许暂停", other))),
    }
}

#[tauri::command]
pub async fn resume_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
    let mut store = state.tasks.lock().await;

    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;

    if task.status == DownloadState::Paused {
        task.status = DownloadState::Downloading;
        tracing::info!(task_id = %task_id, "恢复任务");
        Ok(())
    } else {
        Err(AppError::Config(format!(
            "仅暂停状态可恢复,当前状态: '{}'",
            task.status
        )))
    }
}

#[tauri::command]
pub async fn cancel_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
    let mut store = state.tasks.lock().await;

    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;

    match task.status {
        DownloadState::Completed | DownloadState::Cancelled => {
            Err(AppError::Config(format!("任务已{},无法取消", task.status)))
        }
        _ => {
            if let Some((_, handle)) = state.handles.remove(&task_id) {
                handle.abort();
            }

            update_task_status(&mut store, &task_id, DownloadState::Cancelled);
            tracing::info!(task_id = %task_id, "取消任务");
            Ok(())
        }
    }
}

#[tauri::command]
pub async fn delete_task(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), AppError> {
    let mut store = state.tasks.lock().await;

    let task = store
        .get(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;

    match task.status {
        DownloadState::Completed | DownloadState::Cancelled | DownloadState::Failed => {
            store.remove(&task_id);
            state.handles.remove(&task_id);
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
    let store = state.tasks.lock().await;
    Ok(store.values().cloned().collect())
}

#[tauri::command]
pub async fn get_task_detail(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<TaskInfo, AppError> {
    let store = state.tasks.lock().await;

    store
        .get(&task_id)
        .cloned()
        .ok_or(AppError::TaskNotFound(task_id))
}

#[tauri::command]
pub async fn get_download_progress(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<DownloadProgress, AppError> {
    let store = state.tasks.lock().await;

    let task = store
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

    tracing::info!(url = %url, resource_type = %resource.resource_type, "捕获新资源");
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
    let mut cfg = state.config.lock().await;
    if !config.download.download_dir.is_empty() {
        cfg.download.download_dir = config.download.download_dir;
    }
    if config.max_concurrent_tasks > 0 {
        cfg.max_concurrent_tasks = config.max_concurrent_tasks;
    }
    if config.download.max_concurrent_fragments > 0 {
        cfg.download.max_concurrent_fragments = config.download.max_concurrent_fragments;
    }
    if config.connection.max_connections_per_host > 0 {
        cfg.connection.max_connections_per_host = config.connection.max_connections_per_host;
    }
    cfg.connection.enable_quic = config.connection.enable_quic;
    cfg.download.verify_checksum = config.download.verify_checksum;
    validate_config(&cfg)?;
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
            let store = tasks.lock().await;
            let event: ProgressEvent = store
                .iter()
                .map(|(id, t)| {
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
        let store = state.tasks.lock().await;
        if store.values().any(|t| {
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
        let active_count = store
            .values()
            .filter(|t| {
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
        download_dir.unwrap_or_else(|| cfg.download.download_dir.clone())
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
        let mut store = state.tasks.lock().await;
        store.insert(task_id.clone(), task);
    }

    let download_config = {
        let cfg = state.config.lock().await;
        if cfg.download.max_concurrent_fragments == 0 {
            let mut store = state.tasks.lock().await;
            store.remove(&task_id);
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
    });

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
        )
        .await;
    });

    state.handles.insert(task_id.clone(), handle);

    tracing::info!(task_id = %task_id, "创建下载任务并启动后台下载");
    Ok(task_id)
}

#[cfg(test)]
async fn pause_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let mut store = state.tasks.lock().await;
    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    match task.status {
        DownloadState::Pending | DownloadState::Downloading => {
            task.status = DownloadState::Paused;
            task.speed = 0;
            Ok(())
        }
        other => Err(AppError::Config(format!("当前状态 '{}' 不允许暂停", other))),
    }
}

#[cfg(test)]
async fn resume_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let mut store = state.tasks.lock().await;
    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    if task.status == DownloadState::Paused {
        task.status = DownloadState::Downloading;
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
    let mut store = state.tasks.lock().await;
    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    match task.status {
        DownloadState::Completed | DownloadState::Cancelled => {
            Err(AppError::Config(format!("任务已{},无法取消", task.status)))
        }
        _ => {
            if let Some((_, handle)) = state.handles.remove(&task_id) {
                handle.abort();
            }
            update_task_status(&mut store, &task_id, DownloadState::Cancelled);
            Ok(())
        }
    }
}

#[cfg(test)]
async fn delete_task_inner(state: &AppState, task_id: String) -> Result<(), AppError> {
    let mut store = state.tasks.lock().await;
    let task = store
        .get(&task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.clone()))?;
    match task.status {
        DownloadState::Completed | DownloadState::Cancelled | DownloadState::Failed => {
            store.remove(&task_id);
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
    let store = state.tasks.lock().await;
    Ok(store.values().cloned().collect())
}

#[cfg(test)]
async fn get_task_detail_inner(state: &AppState, task_id: String) -> Result<TaskInfo, AppError> {
    let store = state.tasks.lock().await;
    store
        .get(&task_id)
        .cloned()
        .ok_or(AppError::TaskNotFound(task_id))
}

#[cfg(test)]
async fn get_download_progress_inner(
    state: &AppState,
    task_id: String,
) -> Result<DownloadProgress, AppError> {
    let store = state.tasks.lock().await;
    let task = store
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
    use qf_core::filename::parse_content_disposition;

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
            download: qf_core::config::DownloadConfig {
                download_dir: download_dir.to_string(),
                max_concurrent_fragments,
                max_retries: 3,
                request_timeout_secs: 30,
                verify_checksum,
                user_agent: USER_AGENT.to_string(),
                headers: std::collections::HashMap::new(),
            },
            connection: qf_core::config::ConnectionConfig {
                max_connections_per_host,
                max_global_connections: 256,
                keep_alive_timeout_secs: 30,
                connect_timeout_secs: 10,
                enable_http2: true,
                enable_quic,
            },
            scheduler: qf_core::config::SchedulerConfig::default(),
        }
    }

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(Mutex::new(AppConfig {
                max_concurrent_tasks: 5,
                download: DownloadConfig {
                    download_dir: "/default".to_string(),
                    ..DownloadConfig::default()
                },
                connection: ConnectionConfig::default(),
                scheduler: Default::default(),
            })),
            handles: Arc::new(DashMap::new()),
            active_permits: Arc::new(AtomicU32::new(0)),
            sniffer: Arc::new(Mutex::new(Vec::new())),
            sniffer_filters: Arc::new(Mutex::new(Vec::new())),
            progress_tx: watch::Sender::new(HashMap::new()),
            connection_pool: Arc::new(ConnectionPool::new(PoolConfig {
                max_per_host: 16,
                max_global: 256,
            })),
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
        let id = create_task_inner(
            &state,
            "https://example.com/file.zip".to_string(),
            Some("/tmp/custom".to_string()),
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
        let new_cfg = AppConfig {
            max_concurrent_tasks: 10,
            download: qf_core::config::DownloadConfig {
                download_dir: "/data/downloads".to_string(),
                max_concurrent_fragments: 32,
                max_retries: 3,
                request_timeout_secs: 30,
                verify_checksum: false,
                user_agent: USER_AGENT.to_string(),
                headers: std::collections::HashMap::new(),
            },
            connection: qf_core::config::ConnectionConfig {
                max_connections_per_host: 8,
                max_global_connections: 256,
                keep_alive_timeout_secs: 30,
                connect_timeout_secs: 10,
                enable_http2: true,
                enable_quic: true,
            },
            scheduler: qf_core::config::SchedulerConfig::default(),
        };
        update_config_inner(&state, new_cfg).await.unwrap();
        let cfg = get_config_inner(&state).await.unwrap();
        assert_eq!(cfg.download.download_dir, "/data/downloads");
        assert_eq!(cfg.max_concurrent_tasks, 10);
        assert_eq!(cfg.download.max_concurrent_fragments, 32);
        assert_eq!(cfg.connection.max_connections_per_host, 8);
        assert!(cfg.connection.enable_quic);
        assert!(!cfg.download.verify_checksum);
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
            download: qf_core::config::DownloadConfig {
                download_dir: "/tmp".to_string(),
                max_concurrent_fragments: 8,
                max_retries: 3,
                request_timeout_secs: 30,
                verify_checksum: false,
                user_agent: USER_AGENT.to_string(),
                headers: std::collections::HashMap::new(),
            },
            connection: qf_core::config::ConnectionConfig {
                max_connections_per_host: 4,
                max_global_connections: 256,
                keep_alive_timeout_secs: 30,
                connect_timeout_secs: 10,
                enable_http2: true,
                enable_quic: true,
            },
            scheduler: qf_core::config::SchedulerConfig::default(),
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
        let result =
            update_config_inner(&state, make_test_app_config(0, "/tmp", 16, 16, false, true)).await;
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
        let result =
            update_config_inner(&state, make_test_app_config(5, "/tmp", 0, 16, false, true)).await;
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
            make_test_app_config(65, "/tmp", 16, 16, false, true),
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
        let result =
            update_config_inner(&state, make_test_app_config(5, "/tmp", 33, 16, false, true)).await;
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
        let result =
            update_config_inner(&state, make_test_app_config(1, "/tmp", 1, 1, false, true)).await;
        assert!(result.is_ok());

        let result = update_config_inner(
            &state,
            make_test_app_config(64, "/tmp", 32, 16, false, true),
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

        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert!(
            task.status == DownloadState::Paused || task.status == DownloadState::Downloading,
            "最终状态应为 Paused 或 Downloading,实际: {}",
            task.status
        );
    }
}
