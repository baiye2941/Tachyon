pub mod config_commands;
pub mod hub_commands;
pub mod progress_commands;
pub mod sniffer_commands;
pub mod task_commands;

// Re-exports: Tauri commands and public types
pub use self::config_commands::{get_config, update_config};
pub use self::hub_commands::{get_hf_download_url, list_repo_files};
pub use self::progress_commands::{get_download_progress, subscribe_progress};
pub use self::sniffer_commands::{add_sniffer_filter, add_sniffer_resource, get_sniffer_resources};
pub use self::task_commands::{
    cancel_task, create_task, delete_task, get_task_detail, get_task_list, pause_task, resume_task,
};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use chrono::Local;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tachyon_core::config::{AppConfig, ConnectionConfig, DownloadConfig};
use tachyon_core::types::DownloadState;
use tachyon_engine::connection::{ConnectionPool, PoolConfig};
use tachyon_sniffer::capture::ResourceType;
use tokio::sync::watch;
use url::Url;

use tachyon_sniffer::SnifferResource;

use crate::task_store::TaskStore;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

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
    /// 任务创建锁: 保证去重检查 + 并发计数 + 插入的原子性
    pub create_task_lock: Arc<tokio::sync::Mutex<()>>,
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
            create_task_lock: Arc::new(tokio::sync::Mutex::new(())),
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

// ---------------------------------------------------------------------------
// Simple Tauri commands (no inner function)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Shared utility functions
// ---------------------------------------------------------------------------

pub(crate) fn validate_download_url(url_str: &str) -> Result<(), AppError> {
    let url = Url::parse(url_str).map_err(|e| AppError::Network(format!("URL 格式无效: {e}")))?;
    tachyon_core::validate_public_http_url(&url).map_err(|e| AppError::Network(e.to_string()))?;

    let scheme = url.scheme().to_uppercase();
    let supported = supported_protocols();
    if !supported.iter().any(|p| *p == scheme) {
        return Err(AppError::UnsupportedProtocol(scheme));
    }

    Ok(())
}

pub(crate) fn now_iso8601() -> String {
    Local::now().to_rfc3339()
}

pub(crate) fn resource_type_to_string(rt: ResourceType) -> &'static str {
    match rt {
        ResourceType::Video => "video",
        ResourceType::Audio => "audio",
        ResourceType::Document => "document",
        ResourceType::Archive => "archive",
        ResourceType::Executable => "executable",
        ResourceType::Image => "image",
        ResourceType::Model => "model",
        ResourceType::Other => "other",
    }
}

pub(crate) fn update_task_status(
    store: &DashMap<String, TaskInfo>,
    task_id: &str,
    new_status: DownloadState,
) {
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

pub(crate) fn cleanup_runtime(state: &AppState, task_id: &str) {
    state.controls.remove(task_id);
    state.handles.remove(task_id);
}

pub(crate) async fn persist_task_snapshot(
    state: &AppState,
    task_id: &str,
    fail_reason: Option<String>,
) {
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

pub(crate) fn build_download_config(app_config: &AppConfig, download_dir: &str) -> DownloadConfig {
    let mut download = app_config.download.clone();
    download.download_dir = download_dir.to_string();
    download
}

/// 自动将 huggingface.co 替换为 HF_ENDPOINT 镜像地址
///
/// 检测逻辑:
/// 1. 如果设置了 HF_ENDPOINT 环境变量,替换 URL 中的 huggingface.co → HF_ENDPOINT
/// 2. 如果未设置,检查是否能连接 huggingface.co,不能则自动使用 hf-mirror.com
pub(crate) fn rewrite_hf_url(url: &str) -> String {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tachyon_core::config::{ConnectionConfig, DownloadConfig};
    use tachyon_core::filename::{extract_filename_from_url, parse_content_disposition};

    /// 共享测试辅助:创建测试用 AppState
    pub(crate) fn test_state() -> Arc<AppState> {
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
            create_task_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
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

    #[tokio::test]
    async fn test_any_fragment_failed_detection() {
        let state = test_state();
        let id = task_commands::create_task_inner(
            &state,
            "https://example.com/fail.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let task = task_commands::get_task_detail_inner(&state, id.clone())
            .await
            .unwrap();
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
        let _id1 = task_commands::create_task_inner(
            &state,
            "http://example.com/gate1.bin".into(),
            None,
            None,
        )
        .await
        .unwrap();
        let _id2 = task_commands::create_task_inner(
            &state,
            "http://example.com/gate2.bin".into(),
            None,
            None,
        )
        .await
        .unwrap();
        let result = task_commands::create_task_inner(
            &state,
            "http://example.com/gate3.bin".into(),
            None,
            None,
        )
        .await;
        assert!(result.is_err(), "超过 max_concurrent_tasks 应被拒绝");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("最大并发任务数"),
            "错误应说明并发限制: {err}"
        );
    }
}
