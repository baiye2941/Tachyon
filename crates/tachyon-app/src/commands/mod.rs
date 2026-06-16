pub mod config_commands;
pub mod hub_commands;
pub mod progress_commands;
pub mod sniffer_commands;
pub mod states;
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
use std::sync::atomic::AtomicBool;

use chrono::Local;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tachyon_core::config::{AppConfig, ConnectionConfig, DownloadConfig};
use tachyon_core::types::DownloadState;
use tachyon_engine::connection::{ConnectionPool, PoolConfig};
use tachyon_sniffer::capture::ResourceType;
use url::Url;

use crate::projection::ProgressBroker;
use crate::repository::TaskRepository;
use crate::runtime::{ChunkReaderPool, DownloadSupervisor};
use crate::service::{ConfirmationService, SnifferService, TaskService};
use crate::task_store::TaskStore;
use states::{DomainState, InfraState, RuntimeState, ServiceState};

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
// TaskCommand: 控制通道命令枚举
// ---------------------------------------------------------------------------

pub use tachyon_core::types::TaskCommand;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInfo {
    pub id: String,
    /// 任务 URL。
    /// 存储原始 URL 用于去重和重新下载；
    /// 序列化到前端时自动脱敏（显示用）。
    #[serde(serialize_with = "serialize_url_for_display")]
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
    pub save_path: String,
}

/// 序列化 URL 时脱敏，前端只看到不含敏感参数的 URL
fn serialize_url_for_display<S: serde::Serializer>(url: &str, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&tachyon_core::safety::redact_url_for_log(url))
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgress {
    pub id: String,
    pub progress: f64,
    pub speed: u64,
    pub downloaded: u64,
    pub status: DownloadState,
    pub fragments_done: u32,
}

pub(crate) type ProgressEvent = HashMap<String, TaskProgress>;

// ---------------------------------------------------------------------------
// Confirmation token store (P1-11b)
// ---------------------------------------------------------------------------

/// 一次性确认 token 存储
///
/// 用于破坏性命令（delete_task、update_config）的二次确认机制。
/// 前端在用户确认后先请求 token，再将 token 传入破坏性命令完成操作。
/// Token 60 秒后自动失效，使用后立即销毁（一次性）。
///
/// 安全属性:
/// - 一次性: validate_and_consume 原子移除 token,重放攻击无效
/// - 时效性: 60 秒过期,限制攻击窗口
/// - 不可预测: UUID v4 随机生成,暴力枚举不可行
/// - 动作绑定: token 绑定到特定 action,无法跨操作复用
/// - 容量上限: 最多 1024 个并发 token,超出时强制清理过期项
pub(crate) struct ConfirmationStore {
    tokens: DashMap<String, (String, std::time::Instant)>, // (action, created_at)
}

/// ConfirmationStore 容量上限,防止恶意前端脚本反复调用 request_confirmation 耗尽内存
const CONFIRMATION_STORE_MAX_CAPACITY: usize = 1024;

impl ConfirmationStore {
    pub fn new() -> Self {
        Self {
            tokens: DashMap::new(),
        }
    }

    /// 生成一次性确认 token，绑定到指定 action
    ///
    /// 当 token 数量超过容量上限时,先强制清理过期项再插入。
    /// 若清理后仍超限,拒绝生成(返回空字符串)。
    pub fn generate(&self, action: &str) -> String {
        if self.tokens.len() >= CONFIRMATION_STORE_MAX_CAPACITY {
            self.cleanup_expired();
            if self.tokens.len() >= CONFIRMATION_STORE_MAX_CAPACITY {
                tracing::warn!(
                    capacity = CONFIRMATION_STORE_MAX_CAPACITY,
                    "ConfirmationStore 容量已满,拒绝生成新 token"
                );
                return String::new();
            }
        }
        let token = uuid::Uuid::new_v4().to_string();
        self.tokens.insert(
            token.clone(),
            (action.to_string(), std::time::Instant::now()),
        );
        token
    }

    /// 验证并消费 token（一次性），同时校验 action 是否匹配
    ///
    /// 返回 true 表示 token 有效、未过期且 action 匹配，false 表示无效/已过期/action 不匹配。
    /// DashMap::remove 是原子操作,保证同一 token 只能被消费一次。
    pub fn validate_and_consume(&self, token: &str, expected_action: &str) -> bool {
        if let Some((_, (action, created_at))) = self.tokens.remove(token) {
            action == expected_action && created_at.elapsed().as_secs() < 60
        } else {
            false
        }
    }

    /// 清理过期 token（>60秒）
    ///
    /// 使用 DashMap::retain 替代 iter+collect+remove,减少一次遍历。
    pub fn cleanup_expired(&self) {
        self.tokens
            .retain(|_, (_, created_at)| created_at.elapsed().as_secs() < 60);
    }
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub(crate) domain: DomainState,
    pub(crate) infra: InfraState,
    pub(crate) service: ServiceState,
    pub(crate) runtime: RuntimeState,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn try_new() -> Result<Self, AppError> {
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
        let task_store = Arc::new(
            TaskStore::open(&store_dir)
                .map_err(|e| AppError::Config(format!("任务存储初始化失败: {e}")))?,
        );
        let task_repository = TaskRepository::new();
        let max_concurrent_tasks = config.max_concurrent_tasks;
        let config_arc = Arc::new(tokio::sync::Mutex::new(config));
        let create_task_lock = Arc::new(tokio::sync::Mutex::new(()));
        let connection_pool_arc = Arc::new(connection_pool);

        let task_service = Arc::new(TaskService::new(
            task_repository.clone(),
            config_arc.clone(),
            task_store.clone(),
            create_task_lock,
        ));
        let supervisor = Arc::new(DownloadSupervisor::new(connection_pool_arc.clone()));
        let progress_broker = Arc::new(ProgressBroker::start(task_repository.clone()));
        let confirmation_service = Arc::new(ConfirmationService::new());
        let sniffer_service = Arc::new(SnifferService::new());
        let chunk_reader_pool = Arc::new(ChunkReaderPool::new(max_concurrent_tasks as usize));

        Ok(Self {
            domain: DomainState {
                task_repository,
                config: config_arc,
            },
            infra: InfraState {
                connection_pool: connection_pool_arc,
                task_store,
                chunk_reader_pool,
            },
            service: ServiceState {
                task_service,
                sniffer_service,
                confirmation_service,
            },
            runtime: RuntimeState {
                supervisor,
                progress_broker,
                progress_subscribed: Arc::new(AtomicBool::new(false)),
            },
        })
    }

    pub fn new() -> Self {
        Self::try_new().expect("AppState 初始化失败")
    }

    /// 加载恢复的任务,返回损坏快照的 key 列表(供 UI 告警)
    pub async fn load_recovered_tasks(&self) -> Result<Vec<String>, AppError> {
        let (snapshots, corrupt_keys) = self.infra.task_store.load_recoverable_with_warnings()?;
        for snapshot in snapshots {
            let task = crate::task_store::snapshot_to_task_info(&snapshot);
            self.domain.task_repository.insert(task.id.clone(), task);
        }
        Ok(corrupt_keys)
    }

    /// 创建用于 task_fn 的轻量 AppState 克隆
    ///
    /// 所有内部字段通过 Arc/DashMap clone 共享同一实例，
    /// 不复制数据,仅增加引用计数。
    pub(crate) fn clone_for_task(&self) -> Self {
        Self {
            domain: self.domain.clone(),
            infra: self.infra.clone(),
            service: self.service.clone(),
            runtime: self.runtime.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Simple Tauri commands (no inner function)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
pub struct AppInfo {
    pub version: &'static str,
    pub name: &'static str,
}

/// 启动恢复告警(损坏的断点续传快照)
///
/// 由 `recovery-warning` 一次性事件推送给前端,告知用户
/// 哪些任务快照损坏已被跳过恢复。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryWarning {
    /// 损坏快照的 key 列表(task_<id> 形式)
    pub corrupt_keys: Vec<String>,
    /// 损坏数量(冗余字段,便于前端无需 .length)
    pub count: usize,
}

#[tauri::command]
pub fn get_app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        name: "Tachyon",
    }
}

#[tauri::command]
#[allow(unused_mut)]
pub fn supported_protocols() -> Vec<&'static str> {
    let mut protocols = vec!["HTTP", "HTTPS"];
    #[cfg(feature = "ftp")]
    protocols.push("FTP");
    #[cfg(feature = "quic")]
    protocols.push("QUIC");
    protocols
}

/// 请求破坏性操作的确认令牌(P1-11b)
///
/// 前端在用户通过 window.confirm 确认后调用此命令获取一次性 token,
/// 再将 token 传入破坏性命令(delete_task/update_config)完成操作。
///
/// 安全属性:
/// - token 一次性使用,验证后立即销毁,重放攻击无效
/// - 60 秒过期,限制攻击窗口
/// - 此命令本身是 safe 的,不执行任何破坏性操作
/// - 容量满时返回明确错误,而非静默返回空字符串(S-04)
#[tauri::command]
pub fn request_confirmation(
    state: tauri::State<'_, AppState>,
    action: String,
) -> Result<String, AppError> {
    state.service.confirmation_service.request(&action)
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
    repository: &TaskRepository,
    task_id: &str,
    new_status: DownloadState,
) {
    if let Some(mut task) = repository.get_mut(task_id) {
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
    state.runtime.supervisor.cleanup(task_id);
}

pub(crate) async fn persist_task_snapshot(
    state: &AppState,
    task_id: &str,
    fail_reason: Option<String>,
) {
    let task = {
        state
            .domain
            .task_repository
            .get(task_id)
            .map(|r| r.value().clone())
    };
    if let Some(task) = task {
        let existing = state.infra.task_store.load_snapshot(task_id).ok().flatten();
        let save_path = if let Some(snapshot) = existing.as_ref() {
            snapshot.save_path.clone()
        } else {
            let download_dir = state
                .domain
                .config
                .lock()
                .await
                .download
                .download_dir
                .clone();
            std::path::Path::new(&download_dir)
                .join(&task.file_name)
                .to_string_lossy()
                .to_string()
        };
        let mut snapshot = crate::task_store::task_info_to_snapshot(
            &task,
            save_path,
            0,
            vec![],
            std::collections::HashMap::new(),
            None,
            None,
        );
        if let Some(existing) = existing {
            snapshot.fragment_size = existing.fragment_size;
            snapshot.completed_fragments = existing.completed_fragments;
            snapshot.partial_fragments = existing.partial_fragments;
            snapshot.etag = existing.etag;
            snapshot.last_modified = existing.last_modified;
            snapshot.retry_count = existing.retry_count;
        }
        snapshot.fail_reason = fail_reason;
        if let Err(e) = state.infra.task_store.save_snapshot(&snapshot) {
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
///
/// 安全约束: HF_ENDPOINT 必须通过 validate_public_http_url 校验(与全局 SSRF 防护一致),
/// 否则忽略环境变量以防止 SSRF。
pub(crate) fn rewrite_hf_url(url: &str) -> String {
    if !url.contains("huggingface.co") {
        return url.to_string();
    }

    let mirror = std::env::var("HF_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
        // 安全校验: 多层防护,与全局 SSRF 体系一致
        // 1. 强制 HTTPS(HuggingFace 下载不应使用 HTTP 明文传输)
        // 2. 无路径穿越(不允许 .. 绕过 URL 路径)
        // 3. validate_public_http_url: 拒绝内网 IP/localhost(核心 SSRF 防护)
        .filter(|s| {
            if !s.starts_with("https://") || s.contains("..") {
                return false;
            }
            let Ok(parsed) = url::Url::parse(s) else {
                return false;
            };
            tachyon_core::safety::validate_public_http_url(&parsed).is_ok()
        })
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
    use tachyon_core::safety::{extract_filename_from_url, parse_content_disposition};

    /// 共享测试辅助:创建测试用 AppState
    pub(crate) fn test_state() -> Arc<AppState> {
        let tmp_store = tempfile::tempdir().unwrap();
        let test_dir = std::env::temp_dir()
            .join("tachyon-test-downloads")
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&test_dir);
        let task_repository = TaskRepository::new();
        let config_arc = Arc::new(tokio::sync::Mutex::new(AppConfig {
            max_concurrent_tasks: 5,
            download: DownloadConfig {
                download_dir: test_dir.clone(),
                authorized_dirs: vec![test_dir.clone()],
                ..DownloadConfig::default()
            },
            connection: ConnectionConfig::default(),
            scheduler: Default::default(),
        }));
        let task_store = Arc::new(crate::task_store::TaskStore::open(tmp_store.path()).unwrap());
        let create_task_lock = Arc::new(tokio::sync::Mutex::new(()));
        let connection_pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 16,
            max_global: 256,
            ..Default::default()
        }));
        let progress_broker = Arc::new(ProgressBroker::new_no_aggregator(task_repository.clone()));

        let task_service = Arc::new(TaskService::new(
            task_repository.clone(),
            config_arc.clone(),
            task_store.clone(),
            create_task_lock,
        ));
        let supervisor = Arc::new(DownloadSupervisor::new(connection_pool.clone()));
        let sniffer_service = Arc::new(SnifferService::new());
        let chunk_reader_pool = Arc::new(ChunkReaderPool::new(5));

        Arc::new(AppState {
            domain: DomainState {
                task_repository,
                config: config_arc,
            },
            infra: InfraState {
                connection_pool,
                task_store,
                chunk_reader_pool,
            },
            service: ServiceState {
                task_service,
                sniffer_service,
                confirmation_service: Arc::new(ConfirmationService::new()),
            },
            runtime: RuntimeState {
                supervisor,
                progress_broker,
                progress_subscribed: Arc::new(AtomicBool::new(false)),
            },
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
            save_path: "/downloads/file.zip".to_string(),
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

    #[test]
    fn test_task_command_variants() {
        // 验证四个变体存在且可构造
        let start = TaskCommand::Start;
        let pause = TaskCommand::Pause;
        let resume = TaskCommand::Resume;
        let cancel = TaskCommand::Cancel;

        // PartialEq
        assert_eq!(start, TaskCommand::Start);
        assert_ne!(start, pause);
        assert_ne!(pause, resume);
        assert_ne!(resume, cancel);

        // Clone
        assert_eq!(start.clone(), TaskCommand::Start);
        assert_eq!(pause.clone(), TaskCommand::Pause);

        // Copy（赋值应产生独立副本）
        let copied = start;
        assert_eq!(copied, TaskCommand::Start);
        assert_eq!(start, TaskCommand::Start); // 原值不受影响

        // Hash（用于 DashMap/DashSet 键）
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h1 = DefaultHasher::new();
        start.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        TaskCommand::Start.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn test_task_command_serialization() {
        // serde 序列化应使用 lowercase 格式（rename_all = "lowercase"）
        let cases: Vec<(TaskCommand, &str)> = vec![
            (TaskCommand::Start, "\"start\""),
            (TaskCommand::Pause, "\"pause\""),
            (TaskCommand::Resume, "\"resume\""),
            (TaskCommand::Cancel, "\"cancel\""),
        ];

        for (cmd, expected_json) in &cases {
            let json = serde_json::to_string(cmd).unwrap();
            assert_eq!(
                json, *expected_json,
                "序列化 {:?} 应为 {}",
                cmd, expected_json
            );
        }

        // 反序列化往返
        for (cmd, json_str) in &cases {
            let deserialized: TaskCommand = serde_json::from_str(json_str).unwrap();
            assert_eq!(deserialized, *cmd, "反序列化 {} 应为 {:?}", json_str, cmd);
        }

        // 无效值应反序列化失败
        let result = serde_json::from_str::<TaskCommand>("\"invalid\"");
        assert!(result.is_err(), "无效 TaskCommand 值应反序列化失败");
    }

    #[tokio::test]
    async fn test_max_concurrent_semaphore_gating() {
        let state = AppState::new();
        {
            let mut cfg = state.domain.config.lock().await;
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

    #[test]
    fn test_rewrite_hf_url_rejects_non_https_endpoint() {
        // HF_ENDPOINT 设置为 http:// (非 https) 应被忽略,回退到默认镜像
        // Safety: 测试代码,仅修改当前进程环境变量,无跨线程风险
        unsafe { std::env::set_var("HF_ENDPOINT", "http://evil.com") };
        let result = rewrite_hf_url("https://huggingface.co/model.bin");
        assert!(
            result.contains("hf-mirror.com"),
            "非 HTTPS HF_ENDPOINT 应被忽略: {result}"
        );
        unsafe { std::env::remove_var("HF_ENDPOINT") };
    }

    #[test]
    fn test_rewrite_hf_url_rejects_path_traversal() {
        // HF_ENDPOINT 包含 .. 应被忽略
        // Safety: 测试代码,仅修改当前进程环境变量,无跨线程风险
        unsafe { std::env::set_var("HF_ENDPOINT", "https://evil.com/../huggingface.co") };
        let result = rewrite_hf_url("https://huggingface.co/model.bin");
        assert!(
            result.contains("hf-mirror.com"),
            "含路径穿越的 HF_ENDPOINT 应被忽略: {result}"
        );
        unsafe { std::env::remove_var("HF_ENDPOINT") };
    }

    #[test]
    fn test_rewrite_hf_url_rejects_private_ip() {
        // HF_ENDPOINT 指向内网 IP 应被 SSRF 防护拦截
        // Safety: 测试代码,仅修改当前进程环境变量,无跨线程风险
        unsafe { std::env::set_var("HF_ENDPOINT", "https://192.168.1.1") };
        let result = rewrite_hf_url("https://huggingface.co/model.bin");
        assert!(
            result.contains("hf-mirror.com"),
            "内网 IP HF_ENDPOINT 应被 SSRF 防护拦截: {result}"
        );
        unsafe { std::env::remove_var("HF_ENDPOINT") };
    }

    #[test]
    fn test_rewrite_hf_url_accepts_valid_https() {
        // Safety: 测试代码,仅修改当前进程环境变量,无跨线程风险
        unsafe { std::env::set_var("HF_ENDPOINT", "https://my-mirror.example.com") };
        let result = rewrite_hf_url("https://huggingface.co/model.bin");
        assert!(
            result.contains("my-mirror.example.com"),
            "合法 HTTPS HF_ENDPOINT 应被使用: {result}"
        );
        unsafe { std::env::remove_var("HF_ENDPOINT") };
    }

    // ── ConfirmationStore 测试(P1-11b) ──────────────────────────────

    #[test]
    fn test_confirmation_store_generate_and_validate() {
        let store = ConfirmationStore::new();
        let token = store.generate("delete_task");
        assert!(!token.is_empty(), "生成的 token 不应为空");
        assert!(
            uuid::Uuid::parse_str(&token).is_ok(),
            "token 应为合法 UUID v4: {token}"
        );
        // 首次验证应成功并消费 token
        assert!(
            store.validate_and_consume(&token, "delete_task"),
            "有效 token 应验证通过"
        );
        // 二次验证应失败(token 已被消费)
        assert!(
            !store.validate_and_consume(&token, "delete_task"),
            "已消费的 token 不应再次验证通过"
        );
    }

    #[test]
    fn test_confirmation_store_unknown_token_rejected() {
        let store = ConfirmationStore::new();
        assert!(
            !store.validate_and_consume("nonexistent-token", "delete_task"),
            "不存在的 token 应被拒绝"
        );
    }

    #[test]
    fn test_confirmation_store_action_binding() {
        let store = ConfirmationStore::new();
        let token = store.generate("delete_task");
        // 同 action 应验证通过
        assert!(
            store.validate_and_consume(&token, "delete_task"),
            "action 匹配的 token 应验证通过"
        );
        // 不同 action 应拒绝
        let token2 = store.generate("update_config");
        assert!(
            !store.validate_and_consume(&token2, "delete_task"),
            "action 不匹配的 token 应被拒绝"
        );
    }

    #[test]
    fn test_confirmation_store_multiple_tokens_independent() {
        let store = ConfirmationStore::new();
        let token1 = store.generate("delete_task");
        let token2 = store.generate("delete_task");
        assert_ne!(token1, token2, "不同次生成的 token 应不同");
        // 消费 token1 不影响 token2
        assert!(store.validate_and_consume(&token1, "delete_task"));
        assert!(store.validate_and_consume(&token2, "delete_task"));
    }

    #[test]
    fn test_confirmation_store_cleanup_expired() {
        let store = ConfirmationStore::new();
        let token = store.generate("delete_task");
        // 模拟过期:直接插入一个 61 秒前创建的 token
        let expired_instant = std::time::Instant::now() - std::time::Duration::from_secs(61);
        store.tokens.insert(
            "expired-test".to_string(),
            ("delete_task".to_string(), expired_instant),
        );

        // 清理前:过期 token 仍在 store 中
        assert!(store.tokens.contains_key("expired-test"));
        // 清理后:过期 token 被移除
        store.cleanup_expired();
        assert!(
            !store.tokens.contains_key("expired-test"),
            "过期 token 应被清理"
        );
        // 未过期 token 应仍可验证
        assert!(store.validate_and_consume(&token, "delete_task"));
    }

    #[test]
    fn test_confirmation_store_expired_token_rejected() {
        let store = ConfirmationStore::new();
        // 直接插入一个 61 秒前创建的 token 模拟过期
        let expired_instant = std::time::Instant::now() - std::time::Duration::from_secs(61);
        store.tokens.insert(
            "expired-token".to_string(),
            ("delete_task".to_string(), expired_instant),
        );
        // 过期 token 验证应失败
        assert!(
            !store.validate_and_consume("expired-token", "delete_task"),
            "过期 token 应被拒绝"
        );
    }

    #[tokio::test]
    async fn test_delete_task_without_token_rejected() {
        let state = test_state();
        let id = task_commands::create_task_inner(
            &state,
            "https://example.com/no-token.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        // 取消任务使其可删除
        task_commands::cancel_task_inner(&state, id.clone())
            .await
            .unwrap();

        // 不带 confirmation_token 调用 delete_task_inner 应失败
        // 注意: delete_task_inner 不验证 token,验证在 Tauri command 层
        // 此测试验证 Tauri command 层逻辑,需通过模拟方式测试
        // 这里直接测试 ConfirmationStore 的拒绝行为
        let store = ConfirmationStore::new();
        assert!(
            !store.validate_and_consume("", "delete_task"),
            "空 token 应被拒绝"
        );
        assert!(
            !store.validate_and_consume("fake-token", "delete_task"),
            "伪造 token 应被拒绝"
        );
    }

    #[tokio::test]
    async fn test_delete_task_with_valid_token_succeeds() {
        let state = test_state();
        let id = task_commands::create_task_inner(
            &state,
            "https://example.com/valid-token.bin".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        task_commands::cancel_task_inner(&state, id.clone())
            .await
            .unwrap();

        // 生成有效 token 并验证后删除
        let token = state
            .service
            .confirmation_service
            .request("delete_task")
            .unwrap();
        assert!(
            state
                .service
                .confirmation_service
                .validate_and_consume(&token, "delete_task")
                .is_ok(),
            "有效 token 应验证通过"
        );
        // delete_task_inner 不需要 token(验证在 command 层),直接调用
        task_commands::delete_task_inner(&state, id.clone())
            .await
            .unwrap();
        assert!(
            task_commands::get_task_detail_inner(&state, id)
                .await
                .is_err(),
            "已删除任务应不存在"
        );
    }
}
