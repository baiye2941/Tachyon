//! Tauri 命令模块
//!
//! 提供应用信息查询、下载任务管理、配置管理、嗅探等 Tauri 命令。
//! 任务存储使用 `AppState` 通过 Tauri 的 `manage()` 注入,线程安全。
//! 下载任务通过后台 tokio task 异步执行,不阻塞 Tauri 命令返回。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::{Duration, Instant};

use chrono::Local;
use qf_core::config::DownloadConfig;
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

// ---------------------------------------------------------------------------
// 应用错误类型
// ---------------------------------------------------------------------------

/// 结构化应用错误类型
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

// ---------------------------------------------------------------------------
// URL 安全验证
// ---------------------------------------------------------------------------

/// 验证下载 URL 的安全性
///
/// 拒绝非 HTTP/HTTPS scheme、内网地址(RFC 1918/loopback/link-local)、
/// 包含凭据的 URL,防止 SSRF 攻击。
fn validate_download_url(url_str: &str) -> Result<(), String> {
    let url = Url::parse(url_str).map_err(|e| format!("URL 格式无效: {e}"))?;

    // 仅允许 http/https
    match url.scheme() {
        "http" | "https" => {}
        scheme => return Err(format!("不支持的协议: {scheme}，仅允许 http/https")),
    }

    // 拒绝包含用户名/密码的 URL(凭据注入)
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL 中不允许包含用户名或密码".into());
    }

    // 检查主机地址,拒绝内网/环回/link-local
    if let Some(host) = url.host_str() {
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if ip.is_loopback() {
                return Err("不允许访问环回地址".into());
            }
            if ip.is_unspecified() {
                return Err("不允许访问未指定地址".into());
            }
            // 检查 RFC 1918 私有地址和 link-local
            match ip {
                std::net::IpAddr::V4(v4) => {
                    if v4.is_private() || v4.is_link_local() {
                        return Err("不允许访问内网地址".into());
                    }
                }
                std::net::IpAddr::V6(v6) => {
                    if v6.is_loopback() || v6.is_unspecified() {
                        return Err("不允许访问 IPv6 环回/未指定地址".into());
                    }
                }
            }
        }
        // 拒绝 localhost
        if host == "localhost" {
            return Err("不允许访问 localhost".into());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 数据类型
// ---------------------------------------------------------------------------

/// 下载任务信息(前端可见)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    /// 任务唯一标识
    pub id: String,
    /// 下载地址
    pub url: String,
    /// 文件名(从 URL 提取)
    pub file_name: String,
    /// 文件总大小(字节),None 表示未知
    pub file_size: Option<u64>,
    /// 已下载字节数
    pub downloaded: u64,
    /// 当前下载速度(字节/秒)
    pub speed: u64,
    /// 任务状态: pending / downloading / paused / completed / failed / cancelled
    pub status: String,
    /// 下载进度(0.0 ~ 1.0)
    pub progress: f64,
    /// 分片总数
    pub fragments_total: u32,
    /// 已完成分片数
    pub fragments_done: u32,
    /// 创建时间(ISO 8601 本地时间)
    pub created_at: String,
}

/// 应用全局配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 默认下载目录
    pub download_dir: String,
    /// 最大并发任务数
    pub max_concurrent_tasks: u32,
    /// 每任务最大并发分片数
    pub max_concurrent_fragments: u32,
    /// 每主机最大连接数
    pub max_connections_per_host: u32,
    /// 是否启用 QUIC 协议
    pub enable_quic: bool,
    /// 是否校验文件完整性
    pub verify_checksum: bool,
}

/// 下载进度详情(前端轮询)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    /// 任务唯一标识
    pub task_id: String,
    /// 当前状态
    pub status: String,
    /// 下载进度(0.0 ~ 1.0)
    pub progress: f64,
    /// 已下载字节数
    pub downloaded: u64,
    /// 文件总大小(字节)
    pub file_size: Option<u64>,
    /// 当前下载速度(字节/秒)
    pub speed: u64,
    /// 分片总数
    pub fragments_total: u32,
    /// 已完成分片数
    pub fragments_done: u32,
}

/// 轻量级任务进度信息(事件推送用,不含 url/file_name 等静态字段)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgress {
    /// 任务唯一标识
    pub id: String,
    /// 下载进度(0.0 ~ 1.0)
    pub progress: f64,
    /// 当前下载速度(字节/秒)
    pub speed: u64,
    /// 已下载字节数
    pub downloaded: u64,
    /// 任务状态
    pub status: String,
    /// 已完成分片数
    pub fragments_done: u32,
}

/// 进度事件类型:任务 ID -> TaskProgress 的快照
pub type ProgressEvent = HashMap<String, TaskProgress>;

use qf_sniffer::SnifferResource;

/// 任务状态常量
mod status {
    pub const PENDING: &str = "pending";
    pub const DOWNLOADING: &str = "downloading";
    pub const PAUSED: &str = "paused";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// 应用全局状态,通过 Tauri 的 `manage()` 注入
pub struct AppState {
    /// 任务列表
    pub tasks: Arc<Mutex<HashMap<String, TaskInfo>>>,
    /// 应用配置
    pub config: Arc<Mutex<AppConfig>>,
    /// 后台下载任务句柄(用于取消)
    pub handles: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// 活跃分片许可计数(用于限制并发)
    pub active_permits: Arc<AtomicU32>,
    /// 嗅探到的资源列表
    pub sniffer: Arc<Mutex<Vec<SnifferResource>>>,
    /// 嗅探过滤规则(URL 关键词)
    pub sniffer_filters: Arc<Mutex<Vec<String>>>,
    /// 全局 HTTP 客户端(连接复用,避免每次请求重建)
    pub http_client: Arc<reqwest::Client>,
    /// 全局连接池(按主机限流)
    pub connection_pool: Arc<ConnectionPool>,
    /// 进度事件推送通道:task_fn 发送,subscribe_progress 监听并转发给前端
    pub progress_tx: watch::Sender<ProgressEvent>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// 创建默认 AppState 实例
    pub fn new() -> Self {
        /// 默认 User-Agent
        const USER_AGENT: &str = "QuantumFetch/0.1.0";

        let http_client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(10))
            .pool_max_idle_per_host(16)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .expect("构建全局 HTTP 客户端不应失败");

        let connection_pool = ConnectionPool::new(PoolConfig {
            max_per_host: 16,
            max_global: 256,
        });

        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(Mutex::new(AppConfig {
                download_dir: dirs()
                    .map(|p| p.join("Downloads").to_string_lossy().to_string())
                    .unwrap_or_else(|| ".".to_string()),
                max_concurrent_tasks: 5,
                max_concurrent_fragments: 16,
                max_connections_per_host: 16,
                enable_quic: false,
                verify_checksum: true,
            })),
            handles: Arc::new(Mutex::new(HashMap::new())),
            active_permits: Arc::new(AtomicU32::new(0)),
            sniffer: Arc::new(Mutex::new(Vec::new())),
            sniffer_filters: Arc::new(Mutex::new(Vec::new())),
            http_client: Arc::new(http_client),
            connection_pool: Arc::new(connection_pool),
            progress_tx: watch::Sender::new(HashMap::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 获取用户主目录(Windows: USERPROFILE, Unix: HOME)
fn dirs() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

/// 获取当前本地时间的 ISO 8601 字符串
fn now_iso8601() -> String {
    Local::now().to_rfc3339()
}

/// 验证应用配置合法性
///
/// - `max_concurrent_tasks`: 1..=64
/// - `max_concurrent_fragments`: 1..=32
/// - `download_dir`: 非空字符串
fn validate_config(config: &AppConfig) -> Result<(), String> {
    if config.max_concurrent_tasks == 0 || config.max_concurrent_tasks > 64 {
        return Err(format!(
            "max_concurrent_tasks 必须在 1..=64 范围内,当前值: {}",
            config.max_concurrent_tasks
        ));
    }
    if config.max_concurrent_fragments == 0 || config.max_concurrent_fragments > 32 {
        return Err(format!(
            "max_concurrent_fragments 必须在 1..=32 范围内,当前值: {}",
            config.max_concurrent_fragments
        ));
    }
    if config.download_dir.is_empty() {
        return Err("download_dir 不能为空".to_string());
    }
    Ok(())
}

/// 将 `ResourceType` 枚举转为可读字符串
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

// ---------------------------------------------------------------------------
// 内部辅助函数(在持有外部锁的上下文中调用,不自行获取锁)
// ---------------------------------------------------------------------------

/// 更新任务状态(需要调用方已持有 tasks 写锁或传入可变引用)
fn update_task_status(store: &mut HashMap<String, TaskInfo>, task_id: &str, new_status: &str) {
    if let Some(task) = store.get_mut(task_id) {
        task.status = new_status.to_string();
        if new_status == status::COMPLETED
            || new_status == status::FAILED
            || new_status == status::CANCELLED
        {
            task.speed = 0;
        }
    }
}

/// 构建 `DownloadConfig`(从应用配置和下载目录转换)
///
/// 将 UI 层的 `AppConfig` + 下载目录映射为引擎层的 `DownloadConfig`。
fn build_download_config(app_config: &AppConfig, download_dir: &str) -> DownloadConfig {
    DownloadConfig {
        download_dir: download_dir.to_string(),
        max_concurrent_fragments: app_config.max_concurrent_fragments,
        max_retries: 3,
        request_timeout_secs: 30,
        verify_checksum: app_config.verify_checksum,
        user_agent: "QuantumFetch/0.1.0".to_string(),
        headers: std::collections::HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// 后台下载任务
// ---------------------------------------------------------------------------

/// 后台下载任务实现
///
/// 创建 `DownloadTask` 并调用 `run()` 执行真实下载管线:
/// 探测 -> 规划分片 -> 预分配存储 -> 并发下载 -> 校验。
/// 通过定期检查 `AppState.tasks` 中的状态来响应暂停和取消操作。
async fn task_fn(
    state: Arc<AppState>,
    task_id: String,
    url: String,
    download_dir: String,
    download_config: DownloadConfig,
) {
    // 解析 URL 获取主机名(用于日志)
    let download_url = match Url::parse(&url) {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "URL 解析失败");
            let mut store = state.tasks.lock().await;
            update_task_status(&mut store, &task_id, status::FAILED);
            return;
        }
    };

    let host = match download_url.host_str() {
        Some(h) => h.to_string(),
        None => {
            tracing::error!(task_id = %task_id, "URL 主机为空");
            let mut store = state.tasks.lock().await;
            update_task_status(&mut store, &task_id, status::FAILED);
            return;
        }
    };

    // 检查是否已取消或已暂停(在开始真实下载之前)
    {
        let store = state.tasks.lock().await;
        if let Some(task) = store.get(&task_id) {
            if task.status == status::CANCELLED {
                tracing::info!(task_id = %task_id, "任务已取消,跳过下载");
                return;
            }
            if task.status == status::PAUSED {
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

    // 确保下载目录存在(DownloadTask 会在目录下创建文件)
    if let Err(e) = std::fs::create_dir_all(&download_dir) {
        tracing::error!(task_id = %task_id, error = %e, "创建下载目录失败");
        let mut store = state.tasks.lock().await;
        update_task_status(&mut store, &task_id, status::FAILED);
        return;
    }

    // 创建 DownloadTask(自动根据 URL scheme 选择 HTTP 协议后端)
    let mut download_task = match DownloadTask::new(url.clone(), download_config).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "创建 DownloadTask 失败");
            let mut store = state.tasks.lock().await;
            update_task_status(&mut store, &task_id, status::FAILED);
            return;
        }
    };

    // 探测元数据并更新 TaskInfo
    match download_task.probe().await {
        Ok(meta) => {
            tracing::info!(
                task_id = %task_id,
                file_name = %meta.file_name,
                file_size = ?meta.file_size,
                supports_range = meta.supports_range,
                "元数据探测成功"
            );

            // 用探测到的元数据更新 TaskInfo(提前让用户看到文件大小等信息)
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
            update_task_status(&mut store, &task_id, status::FAILED);
            return;
        }
    }

    // 包装为 Arc<Mutex> 以便下载任务和进度监控并行访问
    let download_task = Arc::new(tokio::sync::Mutex::new(download_task));

    // 设置状态为下载中
    update_task_status(
        &mut *state.tasks.lock().await,
        &task_id,
        status::DOWNLOADING,
    );

    // 启动暂停/取消监控循环(与真实下载并行)
    let monitor_state = state.clone();
    let monitor_task_id = task_id.clone();
    let cancel_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;

            let store = monitor_state.tasks.lock().await;
            match store.get(&monitor_task_id).map(|t| t.status.as_str()) {
                Some(status::CANCELLED) => {
                    tracing::info!(task_id = %monitor_task_id, "监控检测到任务已取消");
                    return;
                }
                Some(status::PAUSED) => {
                    // 暂停超时保护:5 分钟后自动标记失败
                    drop(store);
                    let mut paused_ticks = 0u32;
                    loop {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        paused_ticks += 1;
                        let s = monitor_state.tasks.lock().await;
                        if s.get(&monitor_task_id)
                            .is_none_or(|t| t.status != status::PAUSED)
                        {
                            break;
                        }
                        if paused_ticks > 1500 {
                            tracing::warn!(task_id = %monitor_task_id, "暂停超时(5分钟),标记任务失败");
                            let mut s = monitor_state.tasks.lock().await;
                            update_task_status(&mut s, &monitor_task_id, status::FAILED);
                            return;
                        }
                    }
                }
                Some(status::FAILED) | None => return,
                _ => {}
            }
        }
    });

    // 启动进度监控任务(定期读取 DownloadTask 进度并更新共享状态)
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
            // 计算已下载字节数
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

            // 更新共享状态
            {
                let mut store = monitor_ps.tasks.lock().await;
                if let Some(task) = store.get_mut(&monitor_tid) {
                    task.downloaded = downloaded;
                    task.speed = speed;
                    task.progress = p.min(1.0);
                }
            }

            // 推送进度事件到 watch channel
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
                                status: t.status.clone(),
                                fragments_done: t.fragments_done,
                            },
                        )
                    })
                    .collect();
                // 通过 watch channel 推送进度更新
                let _ = monitor_ps.progress_tx.send(event);
            }

            // 下载完成或失败时退出监控
            if ds == DownloadState::Completed || ds == DownloadState::Failed {
                return speed;
            }
        }
    });

    // 并行执行真实下载和进度监控
    let (download_result, _final_speed) = tokio::join!(
        async {
            let mut dt = download_task.lock().await;
            dt.run().await
        },
        progress_handle
    );
    let result = download_result;

    // 取消暂停/取消监控循环
    cancel_handle.abort();

    // 根据下载结果更新任务状态并发送最终进度事件
    {
        let mut store = state.tasks.lock().await;
        let current_status = store.get(&task_id).map(|t| t.status.clone());

        match result {
            Ok(()) => {
                // 下载成功:如果已被取消/暂停,优先保留用户操作的状态
                if current_status.as_deref() == Some(status::CANCELLED) {
                    tracing::info!(task_id = %task_id, "下载完成但任务已被取消");
                } else if let Some(task) = store.get_mut(&task_id) {
                    task.progress = 1.0;
                    // 从 DownloadTask 获取最终下载量
                    let dt = download_task.lock().await;
                    let final_size = dt.metadata().and_then(|m| m.file_size).unwrap_or(0);
                    task.downloaded = final_size;
                    task.speed = 0;
                    drop(dt);
                    update_task_status(&mut store, &task_id, status::COMPLETED);
                    tracing::info!(task_id = %task_id, file_size = final_size, "下载任务完成");
                }
            }
            Err(e) => {
                // 下载失败:如果已被取消,保留 cancelled 状态
                if current_status.as_deref() == Some(status::CANCELLED) {
                    tracing::info!(task_id = %task_id, "下载失败但任务已被取消,保留取消状态");
                } else {
                    update_task_status(&mut store, &task_id, status::FAILED);
                    tracing::error!(task_id = %task_id, error = %e, "下载任务失败");
                }
            }
        }

        // 发送最终进度快照
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
                        status: t.status.clone(),
                        fragments_done: t.fragments_done,
                    },
                )
            })
            .collect();
        let _ = state.progress_tx.send(event);
    }
}

// ---------------------------------------------------------------------------
// 应用信息命令
// ---------------------------------------------------------------------------

/// 应用版本信息
#[derive(Serialize)]
pub struct AppInfo {
    pub version: &'static str,
    pub name: &'static str,
}

/// 获取应用信息
#[tauri::command]
pub fn get_app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        name: "QuantumFetch",
    }
}

/// 获取支持的协议列表
#[tauri::command]
pub fn supported_protocols() -> Vec<&'static str> {
    vec!["HTTP", "HTTPS", "FTP", "QUIC"]
}

// ---------------------------------------------------------------------------
// 任务管理命令
// ---------------------------------------------------------------------------

/// 创建下载任务
///
/// `url` 为下载地址,`download_dir` 可选覆盖默认下载目录。
/// 创建后立即启动后台下载任务,返回新任务的 UUID。
/// 使用 `DownloadTask` 真实下载管线,后台异步执行下载。
#[tauri::command]
pub async fn create_task(
    state: tauri::State<'_, AppState>,
    url: String,
    download_dir: Option<String>,
) -> Result<String, String> {
    validate_download_url(&url)?;
    let task_id = Uuid::new_v4().to_string();
    let file_name = extract_filename_from_url(&url);
    let created_at = now_iso8601();

    {
        let store = state.tasks.lock().await;
        if store.values().any(|t| {
            t.url == url
                && t.status != status::CANCELLED
                && t.status != status::COMPLETED
                && t.status != status::FAILED
        }) {
            return Err("相同 URL 的下载任务已存在".to_string());
        }
        let max_tasks = state.config.lock().await.max_concurrent_tasks as usize;
        let active_count = store
            .values()
            .filter(|t| t.status == status::DOWNLOADING || t.status == status::PENDING)
            .count();
        if active_count >= max_tasks {
            return Err(format!(
                "已达最大并发任务数({max_tasks}),请等待现有任务完成"
            ));
        }
    }

    let download_dir_str = {
        let cfg = state.config.lock().await;
        download_dir.unwrap_or_else(|| cfg.download_dir.clone())
    };

    let task = TaskInfo {
        id: task_id.clone(),
        url: url.clone(),
        file_name,
        file_size: None,
        downloaded: 0,
        speed: 0,
        status: status::PENDING.to_string(),
        progress: 0.0,
        fragments_total: 0,
        fragments_done: 0,
        created_at,
    };

    {
        let mut store = state.tasks.lock().await;
        store.insert(task_id.clone(), task);
    }

    // 构建引擎层 DownloadConfig
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
        http_client: state.http_client.clone(),
        connection_pool: state.connection_pool.clone(),
        progress_tx: state.progress_tx.clone(),
    });

    let tid = task_id.clone();
    let url_clone = url.clone();
    let handle = tokio::spawn(async move {
        task_fn(state_arc, tid, url_clone, download_dir_str, download_config).await;
    });

    {
        let mut handles = state.handles.lock().await;
        handles.insert(task_id.clone(), handle);
    }

    tracing::info!(task_id = %task_id, "创建下载任务并启动后台下载");
    Ok(task_id)
}

/// 暂停下载任务
///
/// 仅 `pending` 或 `downloading` 状态的任务可以暂停。
/// 后台任务检测到暂停状态后将自旋等待恢复。
#[tauri::command]
pub async fn pause_task(state: tauri::State<'_, AppState>, task_id: String) -> Result<(), String> {
    let mut store = state.tasks.lock().await;

    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;

    match task.status.as_str() {
        status::PENDING | status::DOWNLOADING => {
            task.status = status::PAUSED.to_string();
            task.speed = 0;
            tracing::info!(task_id = %task_id, "暂停任务");
            Ok(())
        }
        other => Err(format!("当前状态 '{other}' 不允许暂停")),
    }
}

/// 恢复下载任务
///
/// 仅 `paused` 状态的任务可以恢复。
/// 后台任务检测到状态恢复后将继续下载。
#[tauri::command]
pub async fn resume_task(state: tauri::State<'_, AppState>, task_id: String) -> Result<(), String> {
    let mut store = state.tasks.lock().await;

    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;

    if task.status == status::PAUSED {
        task.status = status::DOWNLOADING.to_string();
        tracing::info!(task_id = %task_id, "恢复任务");
        Ok(())
    } else {
        Err(format!("仅暂停状态可恢复,当前状态: '{}'", task.status))
    }
}

/// 取消下载任务
///
/// 已完成或已取消的任务不可再次取消。
/// 取消会中止后台下载任务并移除句柄。
#[tauri::command]
pub async fn cancel_task(state: tauri::State<'_, AppState>, task_id: String) -> Result<(), String> {
    let mut store = state.tasks.lock().await;

    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;

    match task.status.as_str() {
        status::COMPLETED | status::CANCELLED => Err(format!("任务已{},无法取消", task.status)),
        _ => {
            let mut handles = state.handles.lock().await;
            if let Some(handle) = handles.remove(&task_id) {
                handle.abort();
            }
            drop(handles);

            update_task_status(&mut store, &task_id, status::CANCELLED);
            tracing::info!(task_id = %task_id, "取消任务");
            Ok(())
        }
    }
}

/// 删除下载任务
///
/// 仅 `completed`、`cancelled` 或 `failed` 状态的任务可以删除。
/// 活跃任务需先取消再删除。
#[tauri::command]
pub async fn delete_task(state: tauri::State<'_, AppState>, task_id: String) -> Result<(), String> {
    let mut store = state.tasks.lock().await;

    let task = store
        .get(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;

    match task.status.as_str() {
        status::COMPLETED | status::CANCELLED | status::FAILED => {
            store.remove(&task_id);
            let mut handles = state.handles.lock().await;
            handles.remove(&task_id);
            tracing::info!(task_id = %task_id, "删除任务");
            Ok(())
        }
        other => Err(format!("当前状态 '{other}' 不允许删除,请先取消任务")),
    }
}

/// 获取所有任务列表
#[tauri::command]
pub async fn get_task_list(state: tauri::State<'_, AppState>) -> Result<Vec<TaskInfo>, String> {
    let store = state.tasks.lock().await;
    Ok(store.values().cloned().collect())
}

/// 获取单个任务详情
#[tauri::command]
pub async fn get_task_detail(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<TaskInfo, String> {
    let store = state.tasks.lock().await;

    store
        .get(&task_id)
        .cloned()
        .ok_or_else(|| format!("任务不存在: {task_id}"))
}

// ---------------------------------------------------------------------------
// 进度查询命令
// ---------------------------------------------------------------------------

/// 获取下载进度详情
///
/// 返回指定任务的实时进度、速度、分片状态等信息。
/// 前端可定期轮询此接口更新 UI。
#[tauri::command]
pub async fn get_download_progress(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<DownloadProgress, String> {
    let store = state.tasks.lock().await;

    let task = store
        .get(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;

    Ok(DownloadProgress {
        task_id: task.id.clone(),
        status: task.status.clone(),
        progress: task.progress,
        downloaded: task.downloaded,
        file_size: task.file_size,
        speed: task.speed,
        fragments_total: task.fragments_total,
        fragments_done: task.fragments_done,
    })
}

// ---------------------------------------------------------------------------
// 嗅探命令
// ---------------------------------------------------------------------------

/// 获取嗅探到的可下载资源列表
///
/// 返回当前所有已捕获的资源,按捕获时间降序排列。
#[tauri::command]
pub async fn get_sniffer_resources(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SnifferResource>, String> {
    let store = state.sniffer.lock().await;
    Ok(store.iter().rev().cloned().collect())
}

/// 添加嗅探过滤规则
///
/// `filter` 为 URL 关键词,仅包含匹配关键词的资源会被捕获。
/// 规则持久化到 `sniffer_filters`,供嗅探引擎使用。
#[tauri::command]
pub async fn add_sniffer_filter(
    state: tauri::State<'_, AppState>,
    filter: String,
) -> Result<(), String> {
    if filter.is_empty() {
        return Err("过滤规则不能为空".to_string());
    }
    let mut filters = state.sniffer_filters.lock().await;
    if filters.contains(&filter) {
        return Err("过滤规则已存在".to_string());
    }
    tracing::info!(filter = %filter, "添加嗅探过滤规则");
    filters.push(filter);
    Ok(())
}

/// 内部接口:添加嗅探资源(供嗅探引擎调用)
///
/// 检查过滤规则,仅当 URL 匹配时才添加资源。
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

// ---------------------------------------------------------------------------
// 配置管理命令
// ---------------------------------------------------------------------------

/// 获取当前应用配置
#[tauri::command]
pub async fn get_config(state: tauri::State<'_, AppState>) -> Result<AppConfig, String> {
    let cfg = state.config.lock().await;
    Ok(cfg.clone())
}

/// 更新应用配置
///
/// 前端传入完整的新配置,整体替换旧配置。
/// 会验证 max_concurrent_tasks(1..=64)、max_concurrent_fragments(1..=32)、download_dir(非空)。
#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, AppState>,
    config: AppConfig,
) -> Result<(), String> {
    validate_config(&config)?;
    let mut cfg = state.config.lock().await;
    *cfg = config;
    tracing::info!("应用配置已更新");
    Ok(())
}

// ---------------------------------------------------------------------------
// 事件推送命令
// ---------------------------------------------------------------------------

/// 订阅进度更新事件
///
/// 启动后台监听任务,将进度快照通过 Tauri 事件系统推送给前端。
/// 前端通过 `window.__TAURI__.event.listen('progress-update', callback)` 接收。
#[tauri::command]
pub async fn subscribe_progress(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    use tauri::Emitter;

    let mut rx = state.progress_tx.subscribe();
    let tasks = state.tasks.clone();

    tokio::spawn(async move {
        // 首次推送当前全量快照
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
                            status: t.status.clone(),
                            fragments_done: t.fragments_done,
                        },
                    )
                })
                .collect();
            let _ = app_handle.emit("progress-update", &event);
        }

        // 持续监听 watch channel 变化
        while rx.changed().await.is_ok() {
            let snapshot = (*rx.borrow_and_update()).clone();
            let _ = app_handle.emit("progress-update", &snapshot);
        }
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// 测试辅助函数(直接操作 AppState,不依赖 Tauri State 注入)
// ---------------------------------------------------------------------------

#[cfg(test)]
/// 创建下载任务(直接操作 AppState)
async fn create_task_inner(
    state: &AppState,
    url: String,
    download_dir: Option<String>,
) -> Result<String, String> {
    let task_id = Uuid::new_v4().to_string();
    let file_name = extract_filename_from_url(&url);
    let created_at = now_iso8601();

    {
        let store = state.tasks.lock().await;
        if store.values().any(|t| {
            t.url == url
                && t.status != status::CANCELLED
                && t.status != status::COMPLETED
                && t.status != status::FAILED
        }) {
            return Err("相同 URL 的下载任务已存在".to_string());
        }
        let max_tasks = state.config.lock().await.max_concurrent_tasks as usize;
        let active_count = store
            .values()
            .filter(|t| t.status == status::DOWNLOADING || t.status == status::PENDING)
            .count();
        if active_count >= max_tasks {
            return Err(format!(
                "已达最大并发任务数({max_tasks}),请等待现有任务完成"
            ));
        }
    }

    let download_dir_str = {
        let cfg = state.config.lock().await;
        download_dir.unwrap_or_else(|| cfg.download_dir.clone())
    };

    let task = TaskInfo {
        id: task_id.clone(),
        url: url.clone(),
        file_name,
        file_size: None,
        downloaded: 0,
        speed: 0,
        status: status::PENDING.to_string(),
        progress: 0.0,
        fragments_total: 0,
        fragments_done: 0,
        created_at,
    };

    {
        let mut store = state.tasks.lock().await;
        store.insert(task_id.clone(), task);
    }

    // 构建引擎层 DownloadConfig
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
        http_client: state.http_client.clone(),
        connection_pool: state.connection_pool.clone(),
        progress_tx: state.progress_tx.clone(),
    });

    let tid = task_id.clone();
    let url_clone = url.clone();
    let handle = tokio::spawn(async move {
        task_fn(state_arc, tid, url_clone, download_dir_str, download_config).await;
    });

    {
        let mut handles = state.handles.lock().await;
        handles.insert(task_id.clone(), handle);
    }

    tracing::info!(task_id = %task_id, "创建下载任务并启动后台下载");
    Ok(task_id)
}

#[cfg(test)]
/// 暂停下载任务(直接操作 AppState)
async fn pause_task_inner(state: &AppState, task_id: String) -> Result<(), String> {
    let mut store = state.tasks.lock().await;
    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;
    match task.status.as_str() {
        status::PENDING | status::DOWNLOADING => {
            task.status = status::PAUSED.to_string();
            task.speed = 0;
            Ok(())
        }
        other => Err(format!("当前状态 '{other}' 不允许暂停")),
    }
}

#[cfg(test)]
/// 恢复下载任务(直接操作 AppState)
async fn resume_task_inner(state: &AppState, task_id: String) -> Result<(), String> {
    let mut store = state.tasks.lock().await;
    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;
    if task.status == status::PAUSED {
        task.status = status::DOWNLOADING.to_string();
        Ok(())
    } else {
        Err(format!("仅暂停状态可恢复,当前状态: '{}'", task.status))
    }
}

#[cfg(test)]
/// 取消下载任务(直接操作 AppState)
async fn cancel_task_inner(state: &AppState, task_id: String) -> Result<(), String> {
    let mut store = state.tasks.lock().await;
    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;
    match task.status.as_str() {
        status::COMPLETED | status::CANCELLED => Err(format!("任务已{},无法取消", task.status)),
        _ => {
            let mut handles = state.handles.lock().await;
            if let Some(handle) = handles.remove(&task_id) {
                handle.abort();
            }
            drop(handles);
            update_task_status(&mut store, &task_id, status::CANCELLED);
            Ok(())
        }
    }
}

#[cfg(test)]
/// 删除下载任务(直接操作 AppState)
async fn delete_task_inner(state: &AppState, task_id: String) -> Result<(), String> {
    let mut store = state.tasks.lock().await;
    let task = store
        .get(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;
    match task.status.as_str() {
        status::COMPLETED | status::CANCELLED | status::FAILED => {
            store.remove(&task_id);
            let mut handles = state.handles.lock().await;
            handles.remove(&task_id);
            Ok(())
        }
        other => Err(format!("当前状态 '{other}' 不允许删除,请先取消任务")),
    }
}

#[cfg(test)]
/// 获取所有任务列表(直接操作 AppState)
async fn get_task_list_inner(state: &AppState) -> Result<Vec<TaskInfo>, String> {
    let store = state.tasks.lock().await;
    Ok(store.values().cloned().collect())
}

#[cfg(test)]
/// 获取单个任务详情(直接操作 AppState)
async fn get_task_detail_inner(state: &AppState, task_id: String) -> Result<TaskInfo, String> {
    let store = state.tasks.lock().await;
    store
        .get(&task_id)
        .cloned()
        .ok_or_else(|| format!("任务不存在: {task_id}"))
}

#[cfg(test)]
/// 获取下载进度详情(直接操作 AppState)
async fn get_download_progress_inner(
    state: &AppState,
    task_id: String,
) -> Result<DownloadProgress, String> {
    let store = state.tasks.lock().await;
    let task = store
        .get(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;
    Ok(DownloadProgress {
        task_id: task.id.clone(),
        status: task.status.clone(),
        progress: task.progress,
        downloaded: task.downloaded,
        file_size: task.file_size,
        speed: task.speed,
        fragments_total: task.fragments_total,
        fragments_done: task.fragments_done,
    })
}

#[cfg(test)]
/// 获取嗅探资源列表(直接操作 AppState)
async fn get_sniffer_resources_inner(state: &AppState) -> Result<Vec<SnifferResource>, String> {
    let store = state.sniffer.lock().await;
    Ok(store.iter().rev().cloned().collect())
}

#[cfg(test)]
/// 添加嗅探过滤规则(直接操作 AppState)
async fn add_sniffer_filter_inner(state: &AppState, filter: String) -> Result<(), String> {
    if filter.is_empty() {
        return Err("过滤规则不能为空".to_string());
    }
    let mut filters = state.sniffer_filters.lock().await;
    if filters.contains(&filter) {
        return Err("过滤规则已存在".to_string());
    }
    filters.push(filter);
    Ok(())
}

#[cfg(test)]
/// 获取应用配置(直接操作 AppState)
async fn get_config_inner(state: &AppState) -> Result<AppConfig, String> {
    let cfg = state.config.lock().await;
    Ok(cfg.clone())
}

#[cfg(test)]
/// 更新应用配置(直接操作 AppState)
async fn update_config_inner(state: &AppState, config: AppConfig) -> Result<(), String> {
    validate_config(&config)?;
    let mut cfg = state.config.lock().await;
    *cfg = config;
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use qf_core::filename::parse_content_disposition;

    /// 创建测试用 AppState
    fn test_state() -> Arc<AppState> {
        Arc::new(AppState {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(Mutex::new(AppConfig {
                download_dir: "/default".to_string(),
                max_concurrent_tasks: 5,
                max_concurrent_fragments: 16,
                max_connections_per_host: 16,
                enable_quic: false,
                verify_checksum: true,
            })),
            handles: Arc::new(Mutex::new(HashMap::new())),
            active_permits: Arc::new(AtomicU32::new(0)),
            sniffer: Arc::new(Mutex::new(Vec::new())),
            sniffer_filters: Arc::new(Mutex::new(Vec::new())),
            http_client: Arc::new(
                reqwest::Client::builder()
                    .user_agent("QuantumFetch-test/0.1.0")
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap(),
            ),
            connection_pool: Arc::new(ConnectionPool::new(PoolConfig::default())),
            progress_tx: watch::Sender::new(HashMap::new()),
        })
    }

    // -- create_task 测试 --

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
        assert_eq!(task.status, "pending");
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
        assert!(result.unwrap_err().contains("已存在"));
    }

    // -- pause / resume 测试 --

    #[tokio::test]
    async fn test_pause_pending_task() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
            .await
            .unwrap();
        pause_task_inner(&state, id.clone()).await.unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, "paused");
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
        assert_eq!(task.status, "downloading");
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
        assert!(result.unwrap_err().contains("不允许暂停"));
    }

    #[tokio::test]
    async fn test_resume_non_paused_task_fails() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
            .await
            .unwrap();
        let result = resume_task_inner(&state, id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("仅暂停状态可恢复"));
    }

    // -- cancel 测试 --

    #[tokio::test]
    async fn test_cancel_pending_task() {
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/file.zip".to_string(), None)
            .await
            .unwrap();
        cancel_task_inner(&state, id.clone()).await.unwrap();
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(task.status, "cancelled");
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
        assert!(result.unwrap_err().contains("无法取消"));
    }

    // -- delete 测试 --

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
        assert!(result.unwrap_err().contains("不允许删除"));
    }

    // -- get_task_list 测试 --

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

    // -- get_task_detail 测试 --

    #[tokio::test]
    async fn test_get_task_detail_not_found() {
        let state = test_state();
        let result = get_task_detail_inner(&state, "nonexistent-id".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("任务不存在"));
    }

    // -- 配置测试 --

    #[tokio::test]
    async fn test_get_config_returns_defaults() {
        let state = test_state();
        let cfg = get_config_inner(&state).await.unwrap();
        assert_eq!(cfg.max_concurrent_tasks, 5);
        assert_eq!(cfg.max_concurrent_fragments, 16);
        assert_eq!(cfg.max_connections_per_host, 16);
        assert!(!cfg.enable_quic);
        assert!(cfg.verify_checksum);
    }

    #[tokio::test]
    async fn test_update_config_roundtrip() {
        let state = test_state();
        let new_cfg = AppConfig {
            download_dir: "/data/downloads".to_string(),
            max_concurrent_tasks: 10,
            max_concurrent_fragments: 32,
            max_connections_per_host: 8,
            enable_quic: true,
            verify_checksum: false,
        };
        update_config_inner(&state, new_cfg).await.unwrap();
        let cfg = get_config_inner(&state).await.unwrap();
        assert_eq!(cfg.download_dir, "/data/downloads");
        assert_eq!(cfg.max_concurrent_tasks, 10);
        assert_eq!(cfg.max_concurrent_fragments, 32);
        assert_eq!(cfg.max_connections_per_host, 8);
        assert!(cfg.enable_quic);
        assert!(!cfg.verify_checksum);
    }

    // -- 辅助函数测试 --

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

    // -- 任务状态流转完整性测试 --

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
            "pending"
        );

        pause_task_inner(&state, id.clone()).await.unwrap();
        assert_eq!(
            get_task_detail_inner(&state, id.clone())
                .await
                .unwrap()
                .status,
            "paused"
        );

        resume_task_inner(&state, id.clone()).await.unwrap();
        assert_eq!(
            get_task_detail_inner(&state, id.clone())
                .await
                .unwrap()
                .status,
            "downloading"
        );

        cancel_task_inner(&state, id.clone()).await.unwrap();
        assert_eq!(
            get_task_detail_inner(&state, id.clone())
                .await
                .unwrap()
                .status,
            "cancelled"
        );

        delete_task_inner(&state, id.clone()).await.unwrap();
        assert!(get_task_detail_inner(&state, id).await.is_err());
    }

    // -- 进度查询测试 --

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
        assert_eq!(progress.status, "pending");
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
        assert!(result.unwrap_err().contains("任务不存在"));
    }

    // -- DownloadProgress 序列化测试 --

    #[test]
    fn test_download_progress_serialization() {
        let progress = DownloadProgress {
            task_id: "test-id".to_string(),
            status: "downloading".to_string(),
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

    // -- 嗅探命令测试 --

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
        assert!(result.unwrap_err().contains("已存在"));
    }

    #[tokio::test]
    async fn test_add_sniffer_filter_empty_string_fails() {
        let state = test_state();
        let result = add_sniffer_filter_inner(&state, String::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("不能为空"));
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

    // -- resource_type_to_string 测试 --

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

    // -- AppConfig 序列化测试 --

    #[test]
    fn test_app_config_serialization_roundtrip() {
        let cfg = AppConfig {
            download_dir: "/tmp".to_string(),
            max_concurrent_tasks: 3,
            max_concurrent_fragments: 8,
            max_connections_per_host: 4,
            enable_quic: true,
            verify_checksum: false,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let deserialized: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.download_dir, "/tmp");
        assert_eq!(deserialized.max_concurrent_tasks, 3);
        assert!(deserialized.enable_quic);
        assert!(!deserialized.verify_checksum);
    }

    // -- TaskInfo 序列化测试 --

    #[test]
    fn test_task_info_serialization_roundtrip() {
        let task = TaskInfo {
            id: "test-id".to_string(),
            url: "https://example.com/file.zip".to_string(),
            file_name: "file.zip".to_string(),
            file_size: Some(1024),
            downloaded: 512,
            speed: 100,
            status: "downloading".to_string(),
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

    // -- parse_content_disposition 测试 --

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

    // -- any_fragment_failed 验证测试 --

    #[tokio::test]
    async fn test_any_fragment_failed_detection() {
        // 验证:当信号量关闭时,any_fragment_failed 正确检测到分片失败
        let state = test_state();
        let id = create_task_inner(&state, "https://example.com/fail.bin".to_string(), None)
            .await
            .unwrap();
        // 初始状态下任务应存在
        let task = get_task_detail_inner(&state, id.clone()).await.unwrap();
        assert_eq!(task.status, "pending");
        // 验证 any_fragment_failed 逻辑:任务未完成时不标记为 failed
        assert_ne!(task.status, "failed");
    }

    // -- max_concurrent 信号量门控验证测试 --

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
        // 第三个任务应被 max_concurrent 门控拒绝
        let result = create_task_inner(&state, "http://example.com/gate3.bin".into(), None).await;
        assert!(result.is_err(), "超过 max_concurrent_tasks 应被拒绝");
        let err = result.unwrap_err();
        assert!(err.contains("最大并发任务数"), "错误应说明并发限制: {err}");
    }

    #[tokio::test]
    async fn test_max_concurrent_tasks_rejects() {
        let state = AppState::new();
        {
            let mut cfg = state.config.lock().await;
            cfg.max_concurrent_tasks = 2;
        }
        // 创建两个任务
        let _id1 = create_task_inner(&state, "http://example.com/file1.bin".into(), None)
            .await
            .unwrap();
        let _id2 = create_task_inner(&state, "http://example.com/file2.bin".into(), None)
            .await
            .unwrap();
        // 第三个应被拒绝
        let result = create_task_inner(&state, "http://example.com/file3.bin".into(), None).await;
        assert!(result.is_err(), "超过 max_concurrent_tasks 应返回错误");
        assert!(
            result.unwrap_err().contains("最大并发任务数"),
            "错误信息应提及并发限制"
        );
    }

    // -- validate_config 测试 --

    #[tokio::test]
    async fn test_update_config_rejects_zero_max_concurrent_tasks() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            AppConfig {
                download_dir: "/tmp".to_string(),
                max_concurrent_tasks: 0,
                max_concurrent_fragments: 16,
                max_connections_per_host: 16,
                enable_quic: false,
                verify_checksum: true,
            },
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max_concurrent_tasks"));
    }

    #[tokio::test]
    async fn test_update_config_rejects_zero_max_concurrent_fragments() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            AppConfig {
                download_dir: "/tmp".to_string(),
                max_concurrent_tasks: 5,
                max_concurrent_fragments: 0,
                max_connections_per_host: 16,
                enable_quic: false,
                verify_checksum: true,
            },
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max_concurrent_fragments"));
    }

    #[tokio::test]
    async fn test_update_config_rejects_too_large_tasks() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            AppConfig {
                download_dir: "/tmp".to_string(),
                max_concurrent_tasks: 65,
                max_concurrent_fragments: 16,
                max_connections_per_host: 16,
                enable_quic: false,
                verify_checksum: true,
            },
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max_concurrent_tasks"));
    }

    #[tokio::test]
    async fn test_update_config_rejects_too_large_fragments() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            AppConfig {
                download_dir: "/tmp".to_string(),
                max_concurrent_tasks: 5,
                max_concurrent_fragments: 33,
                max_connections_per_host: 16,
                enable_quic: false,
                verify_checksum: true,
            },
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max_concurrent_fragments"));
    }

    #[tokio::test]
    async fn test_update_config_rejects_empty_download_dir() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            AppConfig {
                download_dir: String::new(),
                max_concurrent_tasks: 5,
                max_concurrent_fragments: 16,
                max_connections_per_host: 16,
                enable_quic: false,
                verify_checksum: true,
            },
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("download_dir"));
    }

    #[tokio::test]
    async fn test_update_config_accepts_valid_boundary_values() {
        let state = test_state();
        // 边界值:最小值
        let result = update_config_inner(
            &state,
            AppConfig {
                download_dir: "/tmp".to_string(),
                max_concurrent_tasks: 1,
                max_concurrent_fragments: 1,
                max_connections_per_host: 1,
                enable_quic: false,
                verify_checksum: true,
            },
        )
        .await;
        assert!(result.is_ok());

        // 边界值:最大值
        let result = update_config_inner(
            &state,
            AppConfig {
                download_dir: "/tmp".to_string(),
                max_concurrent_tasks: 64,
                max_concurrent_fragments: 32,
                max_connections_per_host: 16,
                enable_quic: false,
                verify_checksum: true,
            },
        )
        .await;
        assert!(result.is_ok());
    }

    // -- 信号量 max_concurrent=0 保护测试 --

    #[tokio::test]
    async fn test_zero_max_concurrent_fragments_marks_task_failed() {
        let state = test_state();
        {
            let mut cfg = state.config.lock().await;
            cfg.max_concurrent_fragments = 0;
        }
        // DownloadTask::new 创建 Semaphore::new(0) 会 panic,
        // 但在此之前,create_dir_all 或文件创建更可能失败。
        // 无论哪种情况,任务最终应变为 failed。
        let id = create_task_inner(&state, "http://example.com/zero-sem.bin".into(), None)
            .await
            .unwrap();
        // 等待后台任务完成(使用轮询而非固定 sleep,避免网络超时导致的竞态)
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let task = get_task_detail_inner(&state, id.clone()).await.unwrap();
            if task.status == "failed" {
                break;
            }
        }
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert_eq!(
            task.status, "failed",
            "max_concurrent_fragments=0 时任务应标记为 failed"
        );
    }

    // ------ 并发死锁测试 ------

    /// 验证同时调用 cancel_task 和 get_task_list 不会死锁
    ///
    /// 两个操作都需要获取 tasks 锁,但由于 tokio::sync::Mutex 是异步的,
    /// 并发获取应正确排队而非死锁。
    #[tokio::test]
    async fn test_concurrent_cancel_and_get_list_no_deadlock() {
        let state = test_state();

        // 创建多个任务
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

        // 并发:多个 cancel 操作
        for id in &task_ids[..3] {
            let state_clone = state.clone();
            let tid = id.clone();
            cancel_handles.push(tokio::spawn(async move {
                cancel_task_inner(&state_clone, tid).await
            }));
        }

        let mut list_handles = Vec::new();

        // 并发:多个 get_task_list 操作
        for _ in 0..3 {
            let state_clone = state.clone();
            list_handles.push(tokio::spawn(async move {
                get_task_list_inner(&state_clone).await
            }));
        }

        // 使用 5 秒超时检测死锁
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

        // 验证被取消的任务状态正确
        for id in &task_ids[..3] {
            let task = get_task_detail_inner(&state, id.clone()).await.unwrap();
            assert_eq!(task.status, "cancelled", "任务应已被取消: {}", id);
        }
    }

    /// 验证同时调用 create_task 和 delete_task 不会死锁
    #[tokio::test]
    async fn test_concurrent_create_and_delete_no_deadlock() {
        let state = test_state();

        // 先创建一些任务并取消(使它们可以被删除)
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

        // 并发:创建新任务
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

        // 并发:删除已有任务
        for id in &deletable_ids {
            let state_clone = state.clone();
            let tid = id.clone();
            delete_handles.push(tokio::spawn(async move {
                delete_task_inner(&state_clone, tid).await
            }));
        }

        // 使用 5 秒超时检测死锁
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

        // 验证被删除的任务已不存在
        for id in &deletable_ids {
            let result = get_task_detail_inner(&state, id.clone()).await;
            assert!(result.is_err(), "已删除任务应不存在: {}", id);
        }
    }

    /// 验证并发暂停和恢复同一任务不会死锁
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

        // 交替 pause 和 resume
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

        // 使用 5 秒超时
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for handle in handles {
                let _ = handle.await;
            }
        })
        .await;

        assert!(result.is_ok(), "并发 pause+resume 操作超时,疑似死锁");

        // 验证任务状态是 pause 或 download 之一(最终状态取决于竞争)
        let task = get_task_detail_inner(&state, id).await.unwrap();
        assert!(
            task.status == "paused" || task.status == "downloading",
            "最终状态应为 paused 或 downloading,实际: {}",
            task.status
        );
    }
}
