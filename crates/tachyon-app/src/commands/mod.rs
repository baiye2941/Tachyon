pub mod config_commands;
pub mod fragment_commands;
pub mod hub_commands;
pub mod progress_commands;
pub mod sniffer_commands;
pub mod states;
pub mod task_commands;

// Re-exports: Tauri commands and public types
#[cfg(feature = "magnet")]
pub use self::config_commands::get_bt_proxy_coverage;
pub use self::config_commands::{authorize_download_directory, get_config, update_config};
pub use self::fragment_commands::{TaskFragmentsView, get_task_fragments};
pub use self::hub_commands::{
    add_model_favorite, batch_create_hf_tasks, get_hf_download_url, get_model_info,
    list_model_favorites, list_repo_files, remove_model_favorite, scan_local_models, search_models,
    verify_model,
};
pub use self::progress_commands::{get_download_progress, subscribe_progress};
pub use self::sniffer_commands::{
    add_sniffer_filter, add_sniffer_resource, clear_sniffer_resources, create_task_from_sniffer,
    get_sniffer_capture_config, get_sniffer_resources, set_sniffer_capture_config,
};
pub use self::task_commands::{
    add_task_tag, cancel_task, create_task, delete_task, export_backup, get_task_detail,
    get_task_list, import_backup, move_task, open_folder_under_download_root, open_task_folder,
    pause_task, probe_filename, remove_task_tag, reorder_tasks, resume_task, set_task_tags,
    undo_cancel_task, undo_delete_task,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono::Local;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tachyon_core::config::{AppConfig, DownloadConfig};
use tachyon_core::types::DownloadState;
use tachyon_engine::BufferPool;
use tachyon_engine::{ConnectionPool, PoolConfig};
use tachyon_sniffer::capture::ResourceType;

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
    /// 快照 schema 由更高版本程序写入，需升级客户端后才能恢复。
    #[error("需要升级应用以恢复快照: found={found_version}, supported={supported_version}")]
    UpgradeRequired {
        found_version: u32,
        supported_version: u32,
    },
    /// 单个快照内容无效（损坏 JSON 等），携带可定位 key。
    #[error("无效快照: {key}")]
    InvalidSnapshot { key: String },
    /// 存储层 I/O 失败。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
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
                // 嵌套序列化 DownloadError(保留 type/message/retryable 及变体特有字段),
                // 替代旧 `to_string()` 压平:前端可读取 inner.retryable 决定 toast 严重度,
                // 读取 inner.retryAfterSecs/status 等做精确提示。
                // message 仍保留(err.to_string())供前端兜底展示。
                map.serialize_entry("type", "Core")?;
                map.serialize_entry("message", &err.to_string())?;
                map.serialize_entry("inner", err)?;
            }
            AppError::UpgradeRequired {
                found_version,
                supported_version,
            } => {
                map.serialize_entry("type", "UpgradeRequired")?;
                map.serialize_entry(
                    "message",
                    &format!(
                        "需要升级应用以恢复快照: found={found_version}, supported={supported_version}"
                    ),
                )?;
                map.serialize_entry("foundVersion", found_version)?;
                map.serialize_entry("supportedVersion", supported_version)?;
            }
            AppError::InvalidSnapshot { key } => {
                map.serialize_entry("type", "InvalidSnapshot")?;
                map.serialize_entry("message", &format!("无效快照: {key}"))?;
                map.serialize_entry("key", key)?;
            }
            AppError::Io(err) => {
                map.serialize_entry("type", "Io")?;
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
    /// 当前下载并发度,前端推算 downloading 带宽用
    /// 由 PlanComplete 初始化,运行中不更新(静态初始值)
    #[serde(default)]
    pub active_concurrency: u32,
    pub created_at: String,
    /// 任务保存路径。
    /// 存储 canonical 原值;序列化到前端时剥除 Windows `\\?\` verbatim 前缀(显示用)。
    #[serde(serialize_with = "serialize_path_for_display")]
    pub save_path: String,
    /// 失败原因原文（仅 status=Failed 时有值）。
    /// 前端诊断面板据此展示真实错误，无需启发式推断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// 任务级重试计数。
    ///
    /// 累计引擎分片/整块路径的可重试失败次数(每次 `FragmentProgress::Retry` +1)。
    /// 经快照持久化与 IPC 下发；前端诊断「已重试 N 次」在 N>0 时展示。
    #[serde(default)]
    pub retry_count: u32,
    /// 用户自定义任务标签,用于前端分组/过滤。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// HF 任务元数据（仅 HF 来源的下载任务有值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hf_meta: Option<HfTaskMeta>,
    /// 任务在列表中的显示顺序，越小越靠前。
    #[serde(default)]
    pub display_order: i64,
    /// 创建任务时配置的镜像 URL 列表。
    /// 旧任务/旧快照无此字段时默认 None;restart_download 据此恢复多源。
    /// 内存/快照保留原始 URL;序列化到前端时与主 url 对齐脱敏(SEC-008)。
    #[serde(
        default,
        skip_serializing_if = "mirror_urls_is_empty",
        serialize_with = "serialize_mirror_urls_for_display"
    )]
    pub mirror_urls: Option<Vec<String>>,
}

/// 序列化 URL 时转换为显示形式(tachyon_core::url_for_display):
/// magnet/ed2k 等无 host 的内容寻址 scheme 原文放行(前端可复制真实链接、
/// 命令面板可正常判断 startsWith('magnet:'));http(s) 剥 query/凭据(SEC-008);
/// 无法解析时原文返回,不伪造占位符污染剪贴板。
fn serialize_url_for_display<S: serde::Serializer>(url: &str, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&tachyon_core::url_for_display(url))
}

/// `mirror_urls` 为空或 None 时不写入前端 JSON。
fn mirror_urls_is_empty(urls: &Option<Vec<String>>) -> bool {
    urls.as_ref().is_none_or(Vec::is_empty)
}

/// 序列化镜像 URL 列表时逐项 `url_for_display`(SEC-008):
/// 内存与快照仍为原始 URL,仅 IPC/展示边界脱敏。
fn serialize_mirror_urls_for_display<S: serde::Serializer>(
    urls: &Option<Vec<String>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match urls {
        None => s.serialize_none(),
        Some(list) => {
            use serde::ser::SerializeSeq;
            let mut seq = s.serialize_seq(Some(list.len()))?;
            for url in list {
                seq.serialize_element(&tachyon_core::url_for_display(url))?;
            }
            seq.end()
        }
    }
}

/// 序列化保存路径时剥除 Windows verbatim 前缀,前端复制/显示得到常规路径
fn serialize_path_for_display<S: serde::Serializer>(path: &str, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(strip_verbatim_prefix(path).as_ref())
}

/// 剥除 Windows canonicalize 产出的 `\\?\` verbatim 前缀:
/// - `\\?\D:\foo` → `D:\foo`
/// - `\\?\UNC\server\share` → `\\server\share`
/// - 非 Windows / 无前缀路径原样返回
///
/// 仅用于展示边界(UI 显示/复制、explorer 等外部进程参数);
/// 安全校验层(authorize/validate/open 边界检查)仍使用 canonical 原值做 starts_with 比较。
pub(crate) fn strip_verbatim_prefix(path: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        std::borrow::Cow::Owned(format!(r"\\{rest}"))
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        std::borrow::Cow::Borrowed(rest)
    } else {
        std::borrow::Cow::Borrowed(path)
    }
}

/// HF 任务元数据（可选，仅 HF 来源的下载任务有值）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HfTaskMeta {
    pub repo_id: String,
    pub revision: String,
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lfs_oid: Option<String>,
}

/// 本地模型记录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    pub repo_id: String,
    pub revision: String,
    pub local_path: String,
    pub files: Vec<LocalModelFile>,
    pub total_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<tachyon_hub::api::HfModelInfo>,
}

/// 本地模型文件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelFile {
    pub path: String,
    pub local_path: String,
    pub size: u64,
    pub category: tachyon_hub::FileCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lfs_oid: Option<String>,
    pub verify_status: VerifyStatus,
    pub exists: bool,
}

/// 校验状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerifyStatus {
    Unverified,
    Verified,
    Failed(String),
}

/// 文件校验结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileVerifyResult {
    pub path: String,
    pub status: VerifyStatus,
    pub elapsed_ms: u64,
}

/// 收藏记录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFavorite {
    pub repo_id: String,
    pub added_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_info: Option<tachyon_hub::api::HfModelInfo>,
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
    #[serde(default)]
    pub active_concurrency: u32,
}

/// 单个活跃分片的字节级进度快照(仅含 downloading_set 中的分片)
///
/// 每 250ms aggregator tick 随 progress-update 发送。已完成分片不在其中
/// (它们进 completed_delta / doneSet),未开始分片进度为 0 也不在其中。
/// 数量 = 当前活跃并发数 N(通常 ≤ 16),非分片总数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FragmentByteProgress {
    pub index: u32,
    pub downloaded: u64,
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
    #[serde(default)]
    pub fragments_total: u32,
    #[serde(default)]
    pub active_concurrency: u32,
    /// 文件总大小。探测完成后由后端写入,通过进度事件同步到前端,
    /// 避免前端在探测完成前显示 0B(只能靠 get_task_list 全量刷新)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_delta: Vec<u32>,
    /// 本周期新开始下载的分片索引增量(Started 事件累积)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub started_delta: Vec<u32>,
    /// 任务失败原因。Failed 终态时由后端写入,通过进度事件同步到前端,
    /// 避免 UI 依赖 get_task_list 全量刷新才能展示错误详情(P1-22-4)。
    ///
    /// 三态 wire 编码:None 显式序列化为 "errorReason":null(不加 skip)。
    /// 前端 FT-04 分支依赖显式 null 清空残留错误文案;若字段缺失,
    /// 前端会保持旧值,失败任务 cancel 后错误文案将永远清不掉。
    #[serde(default)]
    pub error_reason: Option<String>,
    /// 活跃分片字节级进度快照(仅 downloading_set 中的分片)。
    /// 快照式:前端无状态、幂等、丢包自愈。空时 skip 以省带宽。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fragment_bytes: Vec<FragmentByteProgress>,
    /// 任务级重试计数(与 TaskInfo.retry_count 同源)。
    /// 进度事件路径同步,避免前端仅靠 get_task_list 才能看到「已重试 N 次」。
    #[serde(default)]
    pub retry_count: u32,
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
    pub(crate) fragment_state_store: crate::projection::FragmentStateStore,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn try_new() -> Result<Self, AppError> {
        // 配置文件路径:生产为 dirs()/.tachyon/config.json;
        // 测试经 test_state() 注入临时路径,避免 persist 写穿真实用户配置
        let config_path = crate::commands::config_commands::config_file_path();
        let config = match crate::commands::config_commands::load_persisted_config(&config_path) {
            Ok(cfg) => {
                // 校验持久化配置,失败则回退默认配置并记录警告
                if let Err(e) = crate::commands::config_commands::validate_config(&cfg) {
                    tracing::warn!(error = %e, "持久化配置校验失败,使用默认配置");
                    AppConfig::default()
                } else {
                    cfg
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "加载持久化配置失败,使用默认配置");
                AppConfig::default()
            }
        };
        let connection_pool = ConnectionPool::new(PoolConfig::from(config.connection.clone()));
        // 连接池热替换句柄:外层 Arc<RwLock<Arc<ConnectionPool>>>,
        // update_config 时在写锁内替换内层 Arc,新任务读锁 clone 拿到新 pool。
        let connection_pool = Arc::new(tokio::sync::RwLock::new(Arc::new(connection_pool)));
        let data_root =
            tachyon_core::config::dirs().unwrap_or_else(|| std::path::PathBuf::from("."));
        // 兼容旧版 .aimd 数据目录:若 .aimd 存在但 .tachyon 不存在,自动重命名
        let legacy_dir = data_root.join(".aimd");
        let new_dir = data_root.join(".tachyon");
        if legacy_dir.exists() && !new_dir.exists() {
            std::fs::rename(&legacy_dir, &new_dir).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "旧数据目录 .aimd 迁移到 .tachyon 失败");
            });
        }
        let store_dir = new_dir.join("store");
        let _ = std::fs::create_dir_all(&store_dir);
        let task_store = Arc::new(
            TaskStore::open(&store_dir)
                .map_err(|e| AppError::Config(format!("任务存储初始化失败: {e}")))?,
        );
        let favorites_dir = new_dir.join("favorites");
        let _ = std::fs::create_dir_all(&favorites_dir);
        let favorites_store = Arc::new(
            tachyon_store::KvStore::open(&favorites_dir)
                .map_err(|e| AppError::Config(format!("收藏存储初始化失败: {e}")))?,
        );
        let task_repository = TaskRepository::new();
        let max_concurrent_tasks = config.max_concurrent_tasks;
        // 审计 A-03:全局共享限速器;None 配置 → 0(不限速)
        let initial_rate = config.download.rate_limit_bytes_per_sec.unwrap_or(0);
        let global_rate_limiter = Arc::new(tachyon_engine::RateLimiter::new(initial_rate));
        // 审计 E-04:容量经纯函数 buffer_pool_capacity_for_config 派生(config 在下行 move)
        let buffer_pool_capacity = Self::buffer_pool_capacity_for_config(&config);
        let config_arc = Arc::new(tokio::sync::Mutex::new(config));
        let create_task_lock = Arc::new(tokio::sync::Mutex::new(()));
        // 全局 buffer 池:容量 = 任务并发 × 分片并发,buffer_size = WRITE_BATCH_BYTES。
        // 惰性分配(用 new 而非 with_prefill),首次 alloc 才创建 buffer,降低启动内存开销。
        let buffer_pool = Arc::new(tokio::sync::RwLock::new(Arc::new(BufferPool::new(
            tachyon_core::config::WRITE_BATCH_BYTES,
            buffer_pool_capacity,
        ))));

        let task_service = Arc::new(TaskService::new(
            task_repository.clone(),
            config_arc.clone(),
            task_store.clone(),
            create_task_lock,
        ));
        let supervisor = Arc::new(DownloadSupervisor::new(connection_pool.clone()));
        let progress_broker = Arc::new(ProgressBroker::start(task_repository.clone()));
        let confirmation_service = Arc::new(ConfirmationService::new());
        let sniffer_service = Arc::new(SnifferService::new());
        let chunk_reader_pool = Arc::new(ChunkReaderPool::new(max_concurrent_tasks as usize));

        Ok(Self {
            domain: DomainState {
                task_repository,
                config: config_arc,
                config_path,
            },
            infra: InfraState {
                connection_pool,
                task_store,
                favorites_store,
                chunk_reader_pool,
                buffer_pool,
                global_rate_limiter,
                #[cfg(feature = "magnet")]
                bt_session: Arc::new(tokio::sync::Mutex::new(None)),
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
                recovery_warning: Arc::new(tokio::sync::Mutex::new(None)),
            },
            fragment_state_store: crate::projection::FragmentStateStore::new(),
        })
    }

    pub fn new() -> Self {
        // CLI/测试路径仍用 expect;GUI run() 必须走 try_new 以免白屏 panic
        Self::try_new().expect("AppState 初始化失败")
    }
    /// 审计 E-04: 从 AppConfig 派生 buffer_pool 容量(纯函数,无 AppState/IO)。
    /// capacity = max_concurrent_tasks × max_concurrent_fragments。
    /// 提取自 try_new 的 buffer_pool 构造,使测试无需 AppState::new()/dirs()/env 改动。
    pub fn buffer_pool_capacity_for_config(config: &AppConfig) -> usize {
        (config.max_concurrent_tasks as usize) * (config.download.max_concurrent_fragments as usize)
    }

    /// 加载恢复的任务，返回结构化 outcome（corrupt + unsupported_schema）。
    ///
    /// - 仅插入可恢复的合法任务
    /// - future schema 进入 `unsupported_schema`，不得混入 corrupt 或静默丢弃
    /// - 损坏 JSON 仍进入 `corrupt_keys`
    pub async fn load_recovered_tasks(&self) -> Result<StartupRecovery, AppError> {
        let (snapshots, corrupt_keys, unsupported_schema) =
            self.infra.task_store.load_recoverable_with_warnings()?;
        for snapshot in snapshots {
            let task = crate::task_store::snapshot_to_task_info(&snapshot);
            self.domain.task_repository.insert(task.id.clone(), task);
        }
        Ok(StartupRecovery {
            corrupt_keys,
            unsupported_schema,
        })
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
            fragment_state_store: self.fragment_state_store.clone(),
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
/// 启动恢复结果：合法任务已插入 repository 后的告警摘要。
#[derive(Debug, Clone)]
pub struct StartupRecovery {
    /// 无法解析的损坏 key 列表
    pub corrupt_keys: Vec<String>,
    /// 需要升级客户端才能处理的 future schema 快照
    pub unsupported_schema: Vec<tachyon_store::ProtectedSnapshot>,
}

#[tauri::command]
pub fn get_app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        name: "Tachyon",
    }
}

/// 审计 HTTP-10:QUIC 能力可见性。
///
/// `enable_quic` 是配置意图,`effective_quic` 是 want-and-compile 交集(实际能否生效)。
/// 前端据此提示用户「enable_quic=true 但当前构建未编译 http3,已降级 HTTP/2」。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QuicCapability {
    /// 配置意图(用户设置)
    pub enable_quic: bool,
    /// 实际生效(want && compiled)
    pub effective_quic: bool,
    /// http3 是否编译期可用
    pub http3_compiled: bool,
}

#[tauri::command]
pub async fn get_quic_capability(
    state: tauri::State<'_, AppState>,
) -> Result<QuicCapability, AppError> {
    let cfg = state.domain.config.lock().await;
    let enable_quic = cfg.connection.enable_quic;
    Ok(QuicCapability {
        enable_quic,
        effective_quic: tachyon_engine::effective_quic_enabled(enable_quic),
        http3_compiled: tachyon_engine::http3_compiled(),
    })
}

#[tauri::command]
#[allow(unused_mut)]
pub fn supported_protocols() -> Vec<&'static str> {
    let mut protocols = vec!["HTTP", "HTTPS"];
    #[cfg(feature = "magnet")]
    protocols.push("BitTorrent");
    protocols
}

/// 拉取启动恢复告警(P1-22-3)
///
/// setup 阶段在 Tauri 事件监听器注册前即检测到损坏快照并 emit `recovery-warning`,
/// 前端会漏接该事件。此命令返回暂存的告警(消费后清空),前端 mount 时主动拉取,
/// 双保险确保告警不丢失。返回 `None` 表示无损坏快照或已被前端消费过。
#[tauri::command]
pub async fn get_recovery_warning(
    state: tauri::State<'_, AppState>,
) -> Result<Option<RecoveryWarning>, AppError> {
    Ok(state.runtime.recovery_warning.lock().await.take())
}

/// 请求破坏性操作的确认令牌(P1-11b)
///
/// 前端在各操作自带的显式用户手势(应用内 ConfirmDialog/原生文件选择框/
/// 保存按钮/撤销按钮)之后调用此命令获取一次性 token,再将 token 传入破坏性
/// 命令(delete_task/update_config 等)完成操作。
///
/// 安全属性:
/// - token 一次性使用,验证后立即销毁,重放攻击无效
/// - 60 秒过期,限制攻击窗口
/// - token 绑定 action,无法跨操作复用
/// - 此命令本身是 safe 的,不执行任何破坏性操作
/// - 容量满时返回明确错误,而非静默返回空字符串(S-04)
///
/// UX 审计(2026-07-20):签发 token 前曾弹 OS 原生确认框(SEC-003),
/// 但它对所有 destructive 操作都是叠加在已有用户手势之上的第二次确认,
/// 用户反馈强烈。已移除;已知取舍:XSS/注入脚本可与前端同权请求 token
/// (SEC-003 在 SEC-002 spec 中本即为 non-goal),token 的一次性/时效/
/// action 绑定属性不变。
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
    // 审计 A-06:统一分类入口,再叠加 magnet 细节校验 / 协议支持列表
    let source = tachyon_core::parse_download_source(url_str).map_err(|e| {
        // parse 失败:可能是格式/SSRF/不支持 scheme
        match e {
            tachyon_core::DownloadError::Config(msg) if msg.contains("不支持的协议") => {
                let scheme = url_str
                    .split(':')
                    .next()
                    .unwrap_or("unknown")
                    .to_uppercase();
                AppError::UnsupportedProtocol(scheme)
            }
            other => AppError::Network(other.to_string()),
        }
    })?;

    match source.kind {
        tachyon_core::DownloadSourceKind::Magnet => {
            #[cfg(feature = "magnet")]
            {
                tachyon_engine::validate_magnet_uri(url_str)
                    .map_err(|e| AppError::Config(e.to_string()))
            }
            #[cfg(not(feature = "magnet"))]
            {
                Err(AppError::UnsupportedProtocol("magnet".to_string()))
            }
        }
        tachyon_core::DownloadSourceKind::Http | tachyon_core::DownloadSourceKind::Hls => {
            // parse_download_source 已做 validate_public_http_url;再对齐 supported_protocols
            let scheme = url::Url::parse(url_str)
                .map(|u| u.scheme().to_uppercase())
                .unwrap_or_else(|_| "HTTP".into());
            let supported = supported_protocols();
            if !supported.iter().any(|p| *p == scheme) {
                return Err(AppError::UnsupportedProtocol(scheme));
            }
            Ok(())
        }
    }
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
    state.fragment_state_store.remove(task_id);
    // 终态广播:确保所有终态路径(Completed/Cancelled/Failed)都触发一次
    // ProgressEvent 推送,让前端收到终态 status 以清理 downloadingSet
    state.runtime.progress_broker.broadcast_all();
}

pub(crate) async fn persist_task_snapshot(
    state: &AppState,
    task_id: &str,
    fail_reason: Option<String>,
) {
    // 1. 同步更新内存中 TaskInfo 的 error_reason,前端查询时立即可见
    if let Some(mut task) = state.domain.task_repository.get_mut(task_id) {
        task.error_reason = fail_reason.clone();
    }

    let task = {
        state
            .domain
            .task_repository
            .get(task_id)
            .map(|r| r.value().clone())
    };
    if let Some(task) = task {
        // load 仅 read_to_string(无 fsync),阻塞远小于 save 的 fsync,
        // 保持同步调用以维持原有控制流时序(与 task_service.rs:persist_snapshot 一致)。
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
            true,
        );
        if let Some(existing) = existing {
            // 内存 TaskInfo.retry_count 权威；不合并磁盘 retry_count
            crate::task_store::merge_disk_progress_into_snapshot(&mut snapshot, &existing);
        }
        snapshot.fail_reason = fail_reason;
        // task_store 底层为 FileStore 同步 I/O(含 fsync),用 fire-and-forget
        // spawn_blocking 包裹避免阻塞 tokio worker,错误仅记录警告。
        let task_store = state.infra.task_store.clone();
        let task_id_for_log = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = task_store.save_snapshot(&snapshot) {
                tracing::warn!(task_id = %task_id_for_log, error = %e, "保存任务状态快照失败");
            }
        });
    }
}

pub(crate) fn build_download_config(app_config: &AppConfig, download_dir: &str) -> DownloadConfig {
    let mut download = app_config.download.clone();
    download.download_dir = download_dir.to_string();
    download
}

/// 按配置的源模式处理 HuggingFace 下载 URL
///
/// 行为随 `HfSourceMode` 变化:
/// - `Official`: 不改写,直连 huggingface.co
/// - `Mirror`/`Race`: 将 huggingface.co 替换为 hf-mirror.com(国内可达)
///   Race 模式下官方源由调用方作为 mirror_urls 竞速源注入(见 hf_race_counterpart_url)
///
/// 安全约束: 仅当 URL 含 `huggingface.co` 时才考虑改写;目标
/// 固定为 hf-mirror.com(硬编码常量,不读环境变量,与 SSRF 全局策略一致)。
pub(crate) fn rewrite_hf_url(url: &str, mode: tachyon_core::config::HfSourceMode) -> String {
    if !url.contains("huggingface.co") {
        return url.to_string();
    }
    match mode {
        tachyon_core::config::HfSourceMode::Official => url.to_string(),
        tachyon_core::config::HfSourceMode::Mirror | tachyon_core::config::HfSourceMode::Race => {
            let rewritten = url.replace("https://huggingface.co", "https://hf-mirror.com");
            if rewritten != url {
                tracing::info!(
                    original = %tachyon_core::safety::redact_url_for_log(url),
                    rewritten = %tachyon_core::safety::redact_url_for_log(&rewritten),
                    mode = ?mode,
                    "HF 下载切换至镜像源"
                );
            }
            rewritten
        }
    }
}

/// 构造 HF 竞速的对立源 URL(Race 模式注入用)
///
/// 主源是官方(`huggingface.co`)则返回镜像(`hf-mirror.com`),反之亦然,
/// 保证 Race 模式下官方与镜像同时参与竞速。仅对 resolve URL(含 huggingface.co
/// 或 hf-mirror.com)生效,CDN 子域不匹配但注入点用的是 download_url() 构造的
/// resolve URL,替换安全。
pub(crate) fn hf_race_counterpart_url(url: &str) -> Option<String> {
    if url.contains("huggingface.co") {
        Some(url.replace("huggingface.co", "hf-mirror.com"))
    } else if url.contains("hf-mirror.com") {
        Some(url.replace("hf-mirror.com", "huggingface.co"))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tachyon_core::config::{ConnectionConfig, DownloadConfig};
    use tachyon_core::safety::{extract_filename_from_url, parse_content_disposition};
    use tachyon_engine::BufferPool;

    /// 共享测试辅助:创建测试用 AppState
    pub(crate) fn test_state() -> Arc<AppState> {
        let tmp_store = tempfile::tempdir().unwrap();
        let test_dir = std::env::temp_dir()
            .join("tachyon-test-downloads")
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&test_dir);
        let task_repository = TaskRepository::new();
        // 测试配置文件路径注入:指向系统临时目录下的独立文件,避免
        // update_config_inner / authorize_download_directory_inner 的 persist
        // 写穿真实用户配置(%USERPROFILE%\.tachyon\config.json)。
        // 同一进程内多次构造用原子计数器区分文件名;nextest 每测试独立进程,pid 天然隔离。
        static TEST_CONFIG_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let test_config_dir = std::env::temp_dir().join("tachyon-test-config");
        let _ = std::fs::create_dir_all(&test_config_dir);
        let config_path = test_config_dir.join(format!(
            "config-{}-{}.json",
            std::process::id(),
            TEST_CONFIG_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let config_arc = Arc::new(tokio::sync::Mutex::new(AppConfig {
            max_concurrent_tasks: 5,
            download: DownloadConfig {
                download_dir: test_dir.clone(),
                authorized_dirs: vec![test_dir.clone()],
                ..DownloadConfig::default()
            },
            connection: ConnectionConfig::default(),
            scheduler: Default::default(),
            magnet: Default::default(),
            hub: Default::default(),
            clipboard: Default::default(),
            notifications: Default::default(),
        }));
        let task_store = Arc::new(crate::task_store::TaskStore::open(tmp_store.path()).unwrap());
        // favorites_store 必须使用独立临时目录,不能与 task_store 共用同一目录:
        // KvStore::open 对每个目录加 OS 级排他锁(fs2::try_lock_exclusive),
        // 同目录上第二次 open 会因锁冲突返回 WouldBlock。
        let tmp_favorites = tempfile::tempdir().unwrap();
        let favorites_store = Arc::new(tachyon_store::KvStore::open(tmp_favorites.path()).unwrap());
        let create_task_lock = Arc::new(tokio::sync::Mutex::new(()));
        let connection_pool = Arc::new(tokio::sync::RwLock::new(Arc::new(ConnectionPool::new(
            PoolConfig {
                max_per_host: 16,
                max_global: 256,
                ..Default::default()
            },
        ))));
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
        // 夹具修复:InfraState 新增 buffer_pool 字段后,字面量构造需同步补字段。
        // 此处用默认规格(WRITE_BATCH_BYTES, 5*16=80)构造池,仅满足结构体契约。
        let buffer_pool = Arc::new(tokio::sync::RwLock::new(Arc::new(BufferPool::new(
            tachyon_core::config::WRITE_BATCH_BYTES,
            5 * 16,
        ))));
        let global_rate_limiter = Arc::new(tachyon_engine::RateLimiter::new(0));

        Arc::new(AppState {
            domain: DomainState {
                task_repository,
                config: config_arc,
                config_path,
            },
            infra: InfraState {
                connection_pool,
                task_store,
                favorites_store,
                chunk_reader_pool,
                buffer_pool,
                global_rate_limiter,
                #[cfg(feature = "magnet")]
                bt_session: Arc::new(tokio::sync::Mutex::new(None)),
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
                recovery_warning: Arc::new(tokio::sync::Mutex::new(None)),
            },
            fragment_state_store: crate::projection::FragmentStateStore::new(),
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
            active_concurrency: 0,
            created_at: "2025-01-01T00:00:00+08:00".to_string(),
            save_path: "/downloads/file.zip".to_string(),
            error_reason: None,
            retry_count: 0,
            tags: vec!["model".to_string()],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };
        let json = serde_json::to_string(&task).unwrap();
        let deserialized: TaskInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "test-id");
        assert_eq!(deserialized.file_size, Some(1024));
        assert!((deserialized.progress - 0.5).abs() < f64::EPSILON);
    }

    // ------ 展示边界:strip_verbatim_prefix / serialize_*_for_display ------

    #[test]
    fn test_strip_verbatim_prefix_drive_path() {
        assert_eq!(
            strip_verbatim_prefix(r"\\?\D:\downloads\f.bin"),
            r"D:\downloads\f.bin"
        );
    }

    #[test]
    fn test_strip_verbatim_prefix_unc_path() {
        // \\?\UNC\server\share 必须转换为 \\server\share,而非简单去前缀
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\server\share\dir"),
            r"\\server\share\dir"
        );
    }

    #[test]
    fn test_strip_verbatim_prefix_passthrough() {
        assert_eq!(strip_verbatim_prefix(r"D:\downloads"), r"D:\downloads");
        assert_eq!(strip_verbatim_prefix("/home/user/dl"), "/home/user/dl");
        assert_eq!(strip_verbatim_prefix(""), "");
    }

    /// 构造带自定义 url/save_path 的 TaskInfo,聚焦序列化边界
    fn make_task_info_for_display(url: &str, save_path: &str) -> TaskInfo {
        TaskInfo {
            id: "t-display".to_string(),
            url: url.to_string(),
            file_name: "f.bin".to_string(),
            file_size: None,
            downloaded: 0,
            speed: 0,
            status: DownloadState::Pending,
            progress: 0.0,
            fragments_total: 0,
            fragments_done: 0,
            active_concurrency: 0,
            created_at: "2025-01-01T00:00:00+08:00".to_string(),
            save_path: save_path.to_string(),
            error_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        }
    }

    #[test]
    fn test_task_info_save_path_serialized_without_verbatim_prefix() {
        let task =
            make_task_info_for_display("https://example.com/f.bin", r"\\?\D:\downloads\f.bin");
        let json = serde_json::to_string(&task).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["savePath"], r"D:\downloads\f.bin");
        assert!(
            !json.contains(r"\\?\\"),
            "verbatim 前缀不得泄漏到前端: {json}"
        );
    }

    #[test]
    fn test_task_info_magnet_url_serialized_verbatim() {
        // magnet 原文放行:前端复制链接/startsWith('magnet:') 判断依赖完整值
        let magnet = "magnet:?xt=urn:btih:ABC123&dn=name&tr=udp://t/announce";
        let task = make_task_info_for_display(magnet, r"D:\dl\f.bin");
        let json = serde_json::to_string(&task).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["url"], magnet);
    }

    #[test]
    fn test_task_info_http_url_serialized_stripped() {
        // http(s) 仍剥 query/凭据(SEC-008)
        let task = make_task_info_for_display(
            "https://user:secret@example.com/path/f.bin?token=abc#frag",
            r"D:\dl\f.bin",
        );
        let json = serde_json::to_string(&task).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["url"], "https://example.com/f.bin");
        assert!(!json.contains("secret"));
        assert!(!json.contains("token=abc"));
    }

    #[test]
    fn test_task_info_mirror_urls_serialized_stripped() {
        // SEC-008: mirror_urls 与主 url 对齐,序列化到前端时剥 query/凭据;
        // 内存与快照仍保留原始值供 restart_download 续传。
        let mut task = make_task_info_for_display(
            "https://cdn.example.com/path/f.bin?token=main-secret",
            r"D:\dl\f.bin",
        );
        task.mirror_urls = Some(vec![
            "https://mirror1.example.com/path/f.bin?token=mirror-secret-1".to_string(),
            "https://user:pass@mirror2.example.com/path/f.bin?sig=xyz#frag".to_string(),
        ]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            !json.contains("mirror-secret-1"),
            "mirror query token 不得泄漏到前端: {json}"
        );
        assert!(
            !json.contains("main-secret"),
            "主 url query 不得泄漏到前端: {json}"
        );
        assert!(
            !json.contains("user:pass") && !json.contains(":pass@"),
            "mirror 凭据不得泄漏到前端: {json}"
        );
        assert!(
            !json.contains("sig=xyz"),
            "mirror query 不得泄漏到前端: {json}"
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mirrors = value["mirrorUrls"]
            .as_array()
            .expect("mirrorUrls 应出现在 JSON 中");
        assert_eq!(mirrors.len(), 2);
        assert_eq!(mirrors[0], "https://mirror1.example.com/f.bin");
        assert_eq!(mirrors[1], "https://mirror2.example.com/f.bin");
    }

    #[test]
    fn test_task_info_empty_mirror_urls_skipped() {
        // 空镜像列表不应出现在前端 JSON(与 skip_serializing_if 一致)
        let mut task = make_task_info_for_display("https://example.com/f.bin", r"D:\dl\f.bin");
        task.mirror_urls = Some(vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            !json.contains("mirrorUrls"),
            "空 mirror_urls 应 skip 序列化: {json}"
        );
    }

    /// 构造测试用 TaskProgress,减少字面量样板
    fn make_progress(error_reason: Option<&str>) -> TaskProgress {
        TaskProgress {
            id: "t1".to_string(),
            progress: 0.0,
            speed: 0,
            downloaded: 0,
            status: DownloadState::Cancelled,
            fragments_done: 0,
            fragments_total: 0,
            active_concurrency: 0,
            file_size: None,
            completed_delta: vec![],
            started_delta: vec![],
            error_reason: error_reason.map(String::from),
            fragment_bytes: vec![],
            retry_count: 0,
        }
    }

    #[test]
    fn test_task_progress_retry_count_serializes_and_defaults() {
        let mut tp = make_progress(None);
        tp.retry_count = 5;
        let json = serde_json::to_string(&tp).unwrap();
        assert!(
            json.contains(r#""retryCount":5"#),
            "retry_count 应以 camelCase 序列化,实际: {json}"
        );
        let decoded: TaskProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.retry_count, 5);
        // 旧版 JSON 无字段 → default 0
        let legacy = r#"{"id":"t1","progress":0.0,"speed":0,"downloaded":0,"status":"cancelled","fragmentsDone":0}"#;
        let decoded: TaskProgress = serde_json::from_str(legacy).unwrap();
        assert_eq!(decoded.retry_count, 0);
    }

    #[test]
    fn test_task_progress_error_reason_none_serializes_explicit_null() {
        // BUG G:三态 wire 编码。error_reason=None 必须序列化为 "errorReason":null,
        // 前端 FT-04 分支(downloads.ts updateProgress)依赖显式 null 清空残留
        // 错误文案;字段缺失(旧 skip 行为)会被前端当作「保持不变」,
        // 失败任务 cancel 后错误文案永远清不掉。
        let json = serde_json::to_string(&make_progress(None)).unwrap();
        assert!(
            json.contains(r#""errorReason":null"#),
            "error_reason=None 应序列化为 \"errorReason\":null,实际: {json}"
        );
    }

    #[test]
    fn test_task_progress_error_reason_some_serializes_value() {
        let json = serde_json::to_string(&make_progress(Some("HTTP 404"))).unwrap();
        assert!(
            json.contains(r#""errorReason":"HTTP 404""#),
            "error_reason=Some 应序列化原始值,实际: {json}"
        );
        // 反序列化兼容:值往返不变
        let decoded: TaskProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.error_reason.as_deref(), Some("HTTP 404"));
        // 旧版 JSON(无 errorReason 字段)经 #[serde(default)] 反序列化为 None
        let legacy = r#"{"id":"t1","progress":0.0,"speed":0,"downloaded":0,"status":"cancelled","fragmentsDone":0}"#;
        let decoded: TaskProgress = serde_json::from_str(legacy).unwrap();
        assert_eq!(decoded.error_reason, None);
    }

    #[tokio::test]
    async fn test_any_fragment_failed_detection() {
        let state = test_state();
        let id = task_commands::create_task_inner(
            &state,
            "https://example.com/fail.bin".to_string(),
            None,
            None,
            None,
            true,
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
        // 必须用 test_state()(独立临时 store/config)，禁止 AppState::new()：
        // 后者打开全局 ~/.tachyon/store；nextest 并行会锁冲突，且可能写穿真实用户目录。
        let state = test_state();
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
            None,
            false,
            None,
        )
        .await
        .unwrap();
        let _id2 = task_commands::create_task_inner(
            &state,
            "http://example.com/gate2.bin".into(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        let result = task_commands::create_task_inner(
            &state,
            "http://example.com/gate3.bin".into(),
            None,
            None,
            None,
            false,
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

    /// 审计 HTTP-10:get_quic_capability 返回配置意图与实际生效交集
    #[tokio::test]
    async fn test_http10_get_quic_capability_reflects_config() {
        let state = test_state();
        // 默认配置 enable_quic=false
        let cfg = state.domain.config.lock().await;
        let enable_quic = cfg.connection.enable_quic;
        assert!(!enable_quic);
        // effective = want && compiled;默认 false 故 effective 必 false
        assert!(!tachyon_engine::effective_quic_enabled(enable_quic));
    }

    #[test]
    fn test_rewrite_hf_url_official_no_rewrite() {
        // Official 模式:不改写,直连官方
        let result = rewrite_hf_url(
            "https://huggingface.co/owner/repo/resolve/main/model.bin",
            tachyon_core::config::HfSourceMode::Official,
        );
        assert_eq!(
            result, "https://huggingface.co/owner/repo/resolve/main/model.bin",
            "Official 模式应保持官方 URL: {result}"
        );
    }

    #[test]
    fn test_rewrite_hf_url_mirror_rewrites() {
        // Mirror 模式:替换为 hf-mirror.com
        let result = rewrite_hf_url(
            "https://huggingface.co/owner/repo/resolve/main/model.bin",
            tachyon_core::config::HfSourceMode::Mirror,
        );
        assert!(
            result.contains("hf-mirror.com"),
            "Mirror 模式应替换为镜像: {result}"
        );
        assert!(
            !result.contains("huggingface.co"),
            "Mirror 模式替换后不应残留官方域名: {result}"
        );
    }

    #[test]
    fn test_rewrite_hf_url_race_rewrites_to_mirror() {
        // Race 模式:主源改写为镜像(国内可达),官方由 hf_race_counterpart_url 注入竞速
        let result = rewrite_hf_url(
            "https://huggingface.co/owner/repo/resolve/main/model.bin",
            tachyon_core::config::HfSourceMode::Race,
        );
        assert!(
            result.contains("hf-mirror.com"),
            "Race 模式主源应改写为镜像: {result}"
        );
        assert!(
            !result.contains("huggingface.co"),
            "Race 模式主源不应残留官方域名: {result}"
        );
    }

    #[test]
    fn test_rewrite_hf_url_non_hf_untouched() {
        // 非 HF URL 任何模式都不改写
        let result = rewrite_hf_url(
            "https://example.com/file.bin",
            tachyon_core::config::HfSourceMode::Mirror,
        );
        assert_eq!(result, "https://example.com/file.bin");
    }

    #[test]
    fn test_hf_race_counterpart_url() {
        // 主源官方 → 对立源镜像
        let mirror =
            hf_race_counterpart_url("https://huggingface.co/owner/repo/resolve/main/f.bin");
        assert_eq!(
            mirror.as_deref(),
            Some("https://hf-mirror.com/owner/repo/resolve/main/f.bin")
        );
        // 主源镜像 → 对立源官方
        let official =
            hf_race_counterpart_url("https://hf-mirror.com/owner/repo/resolve/main/f.bin");
        assert_eq!(
            official.as_deref(),
            Some("https://huggingface.co/owner/repo/resolve/main/f.bin")
        );
        // 非 HF URL 无对立源
        assert!(hf_race_counterpart_url("https://example.com/f.bin").is_none());
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
            None,
            true,
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
            None,
            true,
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
        task_commands::delete_task_inner(&state, id.clone(), false)
            .await
            .unwrap();
        assert!(
            task_commands::get_task_detail_inner(&state, id)
                .await
                .is_err(),
            "已删除任务应不存在"
        );
    }

    // ── BufferPool 全局接入(切片1) ─────────────────────────────────
    //
    // 审计 E-04:原测试调 AppState::new() 并 unsafe 改 USERPROFILE/HOME 来隔离
    // dirs()/.tachyon 配置加载,是全局 nextest retries=2 的根因。改为直接测试
    // 纯函数 buffer_pool_capacity_for_config + BufferPool 构造,零 IO/零 env 改动。

    #[test]
    fn test_buffer_pool_capacity_for_default_config() {
        let cfg = AppConfig::default();
        let capacity = AppState::buffer_pool_capacity_for_config(&cfg);
        assert_eq!(
            capacity, 80,
            "默认配置容量应为 max_concurrent_tasks(5) × max_concurrent_fragments(16) = 80"
        );
    }

    #[test]
    fn test_buffer_pool_capacity_for_custom_config() {
        let mut cfg = AppConfig::default();
        cfg.max_concurrent_tasks = 3;
        cfg.download.max_concurrent_fragments = 8;
        assert_eq!(
            AppState::buffer_pool_capacity_for_config(&cfg),
            24,
            "自定义配置容量应为 3 × 8 = 24"
        );
    }

    #[test]
    fn test_buffer_pool_spec_from_default_config() {
        let cfg = AppConfig::default();
        let pool = BufferPool::new(
            tachyon_core::config::WRITE_BATCH_BYTES,
            AppState::buffer_pool_capacity_for_config(&cfg),
        );
        assert_eq!(
            pool.buffer_size(),
            tachyon_core::config::WRITE_BATCH_BYTES,
            "buffer_pool.buffer_size 应等于 WRITE_BATCH_BYTES"
        );
        assert_eq!(
            pool.capacity(),
            80,
            "buffer_pool.capacity 应等于 max_concurrent_tasks × max_concurrent_fragments"
        );
        assert_eq!(
            pool.available(),
            pool.capacity(),
            "初始状态 available 应等于 capacity"
        );
    }

    /// 验证 buffer_pool 在 clone_for_task 后共享同一底层池实例
    ///
    /// clone_for_task 通过 InfraState::clone 共享 Arc 句柄,
    /// 两个 AppState 应看到相同的信号量状态。
    #[tokio::test]
    async fn test_buffer_pool_shared_across_clone_for_task() {
        // 测试隔离:test_state 注入 temp 数据目录,避免与应用实例争抢真实 store 锁
        let state = test_state();
        let pool = state.infra.buffer_pool.read().await.clone();
        let capacity = pool.capacity();

        // 在原 state 上 alloc 一个 buffer,消耗一个许可
        let _buf = pool.alloc().await;
        assert_eq!(
            pool.available(),
            capacity - 1,
            "alloc 后原 state 可用许可应减 1"
        );

        // clone_for_task 应共享同一热替换句柄,当前池 Arc 一致
        let cloned = state.clone_for_task();
        let cloned_pool = cloned.infra.buffer_pool.read().await.clone();
        assert_eq!(
            Arc::as_ptr(&cloned_pool),
            Arc::as_ptr(&pool),
            "clone_for_task 应共享同一 Arc<BufferPool> 实例"
        );
        assert_eq!(
            cloned_pool.available(),
            capacity - 1,
            "克隆态应看到相同的可用许可数"
        );
    }

    // ── validate_download_url 磁力链接校验测试 ──────────────────────────

    /// 验证 validate_download_url 接受合法磁力链接
    ///
    /// 修复前 BUG:validate_download_url 调用 validate_public_http_url
    /// 只接受 http/https,磁力链接被拒绝。修复后磁力链接走独立校验路径。
    #[test]
    fn test_validate_download_url_accepts_magnet() {
        let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=test";
        let result = validate_download_url(uri);
        assert!(result.is_ok(), "合法磁力链接应被接受: {result:?}");
    }

    /// 验证 validate_download_url 拒绝无效磁力链接(缺少 xt 参数)
    #[test]
    fn test_validate_download_url_rejects_invalid_magnet() {
        let uri = "magnet:?dn=test";
        let result = validate_download_url(uri);
        assert!(result.is_err(), "缺少 xt 参数的磁力链接应被拒绝");
    }

    /// 验证 validate_download_url 仍然拒绝非 magnet 非 http 的 URL
    #[test]
    fn test_validate_download_url_rejects_unsupported_scheme() {
        let result = validate_download_url("ftp://example.com/file.bin");
        assert!(result.is_err(), "FTP URL(未启用 ftp feature 时)应被拒绝");
    }

    /// 审计 A-06:HLS playlist URL 应被接受(与 HTTP 同源 SSRF 校验)
    #[test]
    fn test_validate_download_url_accepts_hls() {
        let result = validate_download_url("https://cdn.example.com/vod/index.m3u8");
        assert!(result.is_ok(), "公网 HLS URL 应被接受: {result:?}");
    }

    /// 审计 A-06:分类与 validate 同源 — HLS query 不影响识别
    #[test]
    fn test_a06_classify_hls_with_query() {
        let kind = tachyon_core::classify_download_url("https://cdn.example.com/list.m3u8?token=x")
            .unwrap();
        assert_eq!(kind, tachyon_core::DownloadSourceKind::Hls);
    }

    /// 审计 A-01:app 不得在 Cargo.toml 直连 io/crypto/scheduler
    #[test]
    fn test_a01_app_cargo_no_direct_io_crypto_scheduler() {
        let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        // 仅检查 [dependencies] 段,避免 dev-dependencies 误伤
        let deps = manifest
            .split("[dev-dependencies]")
            .next()
            .unwrap_or(manifest);
        for forbidden in ["tachyon-io", "tachyon-crypto", "tachyon-scheduler"] {
            assert!(
                !deps.contains(forbidden),
                "A-01:tachyon-app [dependencies] 不得直连 {forbidden}"
            );
        }
        assert!(
            deps.contains("tachyon-engine"),
            "A-01:app 应经 tachyon-engine 门面"
        );
    }

    /// S-02a2: startup consumer 恢复合法任务；future 为 upgrade notice，不得混入 corrupt 或静默丢弃。
    #[tokio::test]
    async fn load_recovered_tasks_surfaces_future_as_upgrade_notice_not_corrupt() {
        let state = test_state();
        let found_version = tachyon_store::SNAPSHOT_SCHEMA_VERSION + 1;

        let good = tachyon_store::TaskSnapshot {
            schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "good".to_string(),
            url: "https://example.com/good.bin".to_string(),
            save_path: "/downloads/good.bin".to_string(),
            file_name: "good.bin".to_string(),
            file_size: Some(100),
            downloaded: 0,
            completed_fragments: vec![],
            partial_fragments: std::collections::HashMap::new(),
            total_fragments: 4,
            fragment_size: 25,
            status: tachyon_core::types::DownloadState::Paused,
            etag: None,
            last_modified: None,
            content_length: Some(100),
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:00Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };

        // 经 TaskStore facade 打开已知临时目录，再写入 future/corrupt raw。
        let tmp = tempfile::tempdir().unwrap();
        let bound_store = crate::task_store::TaskStore::open(tmp.path()).unwrap();
        bound_store.save_snapshot(&good).unwrap();
        let future_raw = format!(
            r#"{{"schemaVersion":{found_version},"id":"future","url":"https://example.com/f.bin","fileName":"f.bin","downloaded":0,"status":"downloading","createdAt":"2026-05-29T00:00:00Z","updatedAt":"2026-05-29T00:00:00Z"}}"#
        );
        std::fs::write(tmp.path().join("task_future.json"), future_raw.as_bytes()).unwrap();
        std::fs::write(
            tmp.path().join("task_corrupt.json"),
            "{ this is not valid json !!!",
        )
        .unwrap();

        let task_repository = TaskRepository::new();
        let config = state.domain.config.clone();
        let config_path = state.domain.config_path.clone();
        let task_store = Arc::new(bound_store);
        let task_service = Arc::new(TaskService::new(
            task_repository.clone(),
            config.clone(),
            task_store.clone(),
            Arc::new(tokio::sync::Mutex::new(())),
        ));
        let specialized = AppState {
            domain: DomainState {
                task_repository: task_repository.clone(),
                config,
                config_path,
            },
            infra: InfraState {
                connection_pool: state.infra.connection_pool.clone(),
                task_store,
                favorites_store: state.infra.favorites_store.clone(),
                chunk_reader_pool: state.infra.chunk_reader_pool.clone(),
                buffer_pool: state.infra.buffer_pool.clone(),
                global_rate_limiter: state.infra.global_rate_limiter.clone(),
                #[cfg(feature = "magnet")]
                bt_session: state.infra.bt_session.clone(),
            },
            service: ServiceState {
                task_service,
                sniffer_service: state.service.sniffer_service.clone(),
                confirmation_service: state.service.confirmation_service.clone(),
            },
            runtime: RuntimeState {
                supervisor: state.runtime.supervisor.clone(),
                progress_broker: Arc::new(ProgressBroker::new_no_aggregator(
                    task_repository.clone(),
                )),
                progress_subscribed: Arc::new(AtomicBool::new(false)),
                recovery_warning: Arc::new(tokio::sync::Mutex::new(None)),
            },
            fragment_state_store: crate::projection::FragmentStateStore::new(),
        };

        // 期望 startup 返回结构化 outcome（corrupt + unsupported_schema），而非仅 Vec<String>。
        let recovery = specialized
            .load_recovered_tasks()
            .await
            .expect("startup recovery 不得因 future/corrupt 整批失败");

        assert!(
            specialized.domain.task_repository.contains_key("good"),
            "合法任务必须进入 repository"
        );
        assert!(
            !specialized.domain.task_repository.contains_key("future"),
            "future 不得当作可恢复任务插入"
        );

        assert!(
            !recovery.corrupt_keys.iter().any(|k| k.contains("future")),
            "future 不得混入 corrupt_keys: {:?}",
            recovery.corrupt_keys
        );
        assert!(
            recovery.corrupt_keys.iter().any(|k| k.contains("corrupt")),
            "损坏 key 仍须暴露: {:?}",
            recovery.corrupt_keys
        );
        assert_eq!(
            recovery.unsupported_schema.len(),
            1,
            "future 必须作为显式 upgrade notice 上报"
        );
        assert_eq!(recovery.unsupported_schema[0].key, "task_future");
        assert_eq!(recovery.unsupported_schema[0].found_version, found_version);
        assert_eq!(
            recovery.unsupported_schema[0].supported_version,
            tachyon_store::SNAPSHOT_SCHEMA_VERSION
        );
    }
}

#[cfg(test)]
mod fragment_bytes_tests {
    use super::*;

    #[test]
    fn fragment_byte_progress_serializes_camel_case() {
        let entry = FragmentByteProgress {
            index: 3,
            downloaded: 524288,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"index\""));
        assert!(json.contains("\"downloaded\""));
        assert!(
            !json.contains("fragment_index"),
            "应是 camelCase index 而非 fragment_index"
        );
    }

    #[test]
    fn task_progress_fragment_bytes_default_empty() {
        let tp = TaskProgress {
            id: "t1".to_string(),
            progress: 0.0,
            speed: 0,
            downloaded: 0,
            status: DownloadState::Pending,
            fragments_done: 0,
            fragments_total: 0,
            active_concurrency: 0,
            file_size: None,
            completed_delta: vec![],
            started_delta: vec![],
            error_reason: None,
            fragment_bytes: vec![],
            retry_count: 0,
        };
        let json = serde_json::to_string(&tp).unwrap();
        // skip_serializing_if = Vec::is_empty,空时不应出现在 JSON
        assert!(
            !json.contains("fragmentBytes"),
            "空 fragment_bytes 应被 skip"
        );
    }

    #[test]
    fn task_progress_fragment_bytes_serialized_when_non_empty() {
        let tp = TaskProgress {
            id: "t1".to_string(),
            progress: 0.5,
            speed: 100,
            downloaded: 512,
            status: DownloadState::Downloading,
            fragments_done: 1,
            fragments_total: 4,
            active_concurrency: 2,
            file_size: Some(1024),
            completed_delta: vec![],
            started_delta: vec![],
            error_reason: None,
            fragment_bytes: vec![FragmentByteProgress {
                index: 1,
                downloaded: 256,
            }],
            retry_count: 0,
        };
        let json = serde_json::to_string(&tp).unwrap();
        assert!(
            json.contains("fragmentBytes"),
            "非空 fragment_bytes 应序列化"
        );
        assert!(json.contains("\"index\":1"));
        assert!(json.contains("\"downloaded\":256"));
    }
}
