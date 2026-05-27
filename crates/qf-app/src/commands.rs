//! Tauri 命令模块
//!
//! 提供应用信息查询、下载任务管理、配置管理等 Tauri 命令。
//! 任务存储使用全局 `Mutex<HashMap>` 实现,线程安全。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use chrono::Local;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
// 全局存储
// ---------------------------------------------------------------------------

/// 任务列表全局存储
static TASK_STORE: LazyLock<Mutex<HashMap<String, TaskInfo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 应用配置全局存储
static CONFIG_STORE: LazyLock<Mutex<AppConfig>> = LazyLock::new(|| {
    Mutex::new(AppConfig {
        download_dir: dirs()
            .map(|p| p.join("Downloads").to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string()),
        max_concurrent_tasks: 5,
        max_concurrent_fragments: 16,
        max_connections_per_host: 16,
        enable_quic: false,
        verify_checksum: true,
    })
});

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 获取用户主目录(Windows: USERPROFILE, Unix: HOME)
fn dirs() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

/// 从 URL 中提取文件名,提取失败时返回 `"unknown"`
fn extract_filename(url_str: &str) -> String {
    url::Url::parse(url_str)
        .ok()
        .and_then(|u| {
            let path = u.path();
            let segment = path.rsplit('/').next().unwrap_or("");
            if segment.is_empty() {
                None
            } else {
                // URL 解码百分号编码
                percent_decode(segment)
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// 简易百分号解码(不依赖外部 crate)
fn percent_decode(input: &str) -> Option<String> {
    let mut output = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            output.push(byte);
            i += 3;
        } else {
            output.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(output).ok()
}

/// 获取当前本地时间的 ISO 8601 字符串
fn now_iso8601() -> String {
    Local::now().to_rfc3339()
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
/// 返回新任务的 UUID。
#[tauri::command]
pub fn create_task(url: String, download_dir: Option<String>) -> Result<String, String> {
    let task_id = Uuid::new_v4().to_string();
    let file_name = extract_filename(&url);
    let created_at = now_iso8601();

    // 若指定了下载目录则保存到配置(仅影响本次任务的前端展示,实际 I/O 由引擎层处理)
    if let Some(ref dir) = download_dir
        && let Ok(mut cfg) = CONFIG_STORE.lock()
    {
        cfg.download_dir = dir.clone();
    }

    let task = TaskInfo {
        id: task_id.clone(),
        url,
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

    let mut store = TASK_STORE
        .lock()
        .map_err(|e| format!("获取任务锁失败: {e}"))?;
    store.insert(task_id.clone(), task);

    tracing::info!(task_id = %task_id, "创建下载任务");
    Ok(task_id)
}

/// 暂停下载任务
///
/// 仅 `pending` 或 `downloading` 状态的任务可以暂停。
#[tauri::command]
pub fn pause_task(task_id: String) -> Result<(), String> {
    let mut store = TASK_STORE
        .lock()
        .map_err(|e| format!("获取任务锁失败: {e}"))?;

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
#[tauri::command]
pub fn resume_task(task_id: String) -> Result<(), String> {
    let mut store = TASK_STORE
        .lock()
        .map_err(|e| format!("获取任务锁失败: {e}"))?;

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
#[tauri::command]
pub fn cancel_task(task_id: String) -> Result<(), String> {
    let mut store = TASK_STORE
        .lock()
        .map_err(|e| format!("获取任务锁失败: {e}"))?;

    let task = store
        .get_mut(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;

    match task.status.as_str() {
        status::COMPLETED | status::CANCELLED => Err(format!("任务已{},无法取消", task.status)),
        _ => {
            task.status = status::CANCELLED.to_string();
            task.speed = 0;
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
pub fn delete_task(task_id: String) -> Result<(), String> {
    let mut store = TASK_STORE
        .lock()
        .map_err(|e| format!("获取任务锁失败: {e}"))?;

    let task = store
        .get(&task_id)
        .ok_or_else(|| format!("任务不存在: {task_id}"))?;

    match task.status.as_str() {
        status::COMPLETED | status::CANCELLED | status::FAILED => {
            store.remove(&task_id);
            tracing::info!(task_id = %task_id, "删除任务");
            Ok(())
        }
        other => Err(format!("当前状态 '{other}' 不允许删除,请先取消任务")),
    }
}

/// 获取所有任务列表
#[tauri::command]
pub fn get_task_list() -> Vec<TaskInfo> {
    TASK_STORE
        .lock()
        .map(|store| store.values().cloned().collect())
        .unwrap_or_default()
}

/// 获取单个任务详情
#[tauri::command]
pub fn get_task_detail(task_id: String) -> Result<TaskInfo, String> {
    let store = TASK_STORE
        .lock()
        .map_err(|e| format!("获取任务锁失败: {e}"))?;

    store
        .get(&task_id)
        .cloned()
        .ok_or_else(|| format!("任务不存在: {task_id}"))
}

// ---------------------------------------------------------------------------
// 配置管理命令
// ---------------------------------------------------------------------------

/// 获取当前应用配置
#[tauri::command]
pub fn get_config() -> AppConfig {
    CONFIG_STORE
        .lock()
        .map(|cfg| cfg.clone())
        .unwrap_or_else(|_| AppConfig {
            download_dir: ".".to_string(),
            max_concurrent_tasks: 5,
            max_concurrent_fragments: 16,
            max_connections_per_host: 16,
            enable_quic: false,
            verify_checksum: true,
        })
}

/// 更新应用配置
///
/// 前端传入完整的新配置,整体替换旧配置。
#[tauri::command]
pub fn update_config(config: AppConfig) -> Result<(), String> {
    let mut cfg = CONFIG_STORE
        .lock()
        .map_err(|e| format!("获取配置锁失败: {e}"))?;
    *cfg = config;
    tracing::info!("应用配置已更新");
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 重置全局配置为默认值(含中毒恢复)
    fn reset_config() {
        let mut cfg = CONFIG_STORE.lock().unwrap_or_else(|e| e.into_inner());
        *cfg = AppConfig {
            download_dir: "/default".to_string(),
            max_concurrent_tasks: 5,
            max_concurrent_fragments: 16,
            max_connections_per_host: 16,
            enable_quic: false,
            verify_checksum: true,
        };
    }

    /// 清空全局任务存储(含中毒恢复)
    fn clear_tasks() {
        let mut store = TASK_STORE.lock().unwrap_or_else(|e| e.into_inner());
        store.clear();
    }

    // -- create_task 测试 --

    #[test]
    fn test_create_task_returns_valid_uuid() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        // 验证返回值是合法 UUID
        assert!(Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_create_task_extracts_filename() {
        clear_tasks();
        let id = create_task(
            "https://cdn.example.org/releases/app-v2.0.tar.gz".to_string(),
            None,
        )
        .unwrap();
        let task = get_task_detail(id).unwrap();
        assert_eq!(task.file_name, "app-v2.0.tar.gz");
    }

    #[test]
    fn test_create_task_default_status_is_pending() {
        clear_tasks();
        let id = create_task("https://example.com/data.bin".to_string(), None).unwrap();
        let task = get_task_detail(id).unwrap();
        assert_eq!(task.status, "pending");
        assert_eq!(task.downloaded, 0);
        assert_eq!(task.speed, 0);
        assert!((task.progress - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_create_task_with_download_dir() {
        clear_tasks();
        reset_config();
        let id = create_task(
            "https://example.com/file.zip".to_string(),
            Some("/tmp/custom".to_string()),
        )
        .unwrap();
        let task = get_task_detail(id).unwrap();
        // 验证任务已创建且配置被更新
        assert_eq!(task.url, "https://example.com/file.zip");
        let cfg = get_config();
        assert_eq!(cfg.download_dir, "/tmp/custom");
    }

    // -- pause / resume 测试 --

    #[test]
    fn test_pause_pending_task() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        pause_task(id.clone()).unwrap();
        let task = get_task_detail(id).unwrap();
        assert_eq!(task.status, "paused");
        assert_eq!(task.speed, 0);
    }

    #[test]
    fn test_resume_paused_task() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        pause_task(id.clone()).unwrap();
        resume_task(id.clone()).unwrap();
        let task = get_task_detail(id).unwrap();
        assert_eq!(task.status, "downloading");
    }

    #[test]
    fn test_pause_already_paused_task_fails() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        pause_task(id.clone()).unwrap();
        let result = pause_task(id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("不允许暂停"));
    }

    #[test]
    fn test_resume_non_paused_task_fails() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        // pending 状态不可恢复
        let result = resume_task(id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("仅暂停状态可恢复"));
    }

    // -- cancel 测试 --

    #[test]
    fn test_cancel_pending_task() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        cancel_task(id.clone()).unwrap();
        let task = get_task_detail(id).unwrap();
        assert_eq!(task.status, "cancelled");
    }

    #[test]
    fn test_cancel_already_cancelled_task_fails() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        cancel_task(id.clone()).unwrap();
        let result = cancel_task(id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("无法取消"));
    }

    // -- delete 测试 --

    #[test]
    fn test_delete_cancelled_task() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        cancel_task(id.clone()).unwrap();
        delete_task(id.clone()).unwrap();
        assert!(get_task_detail(id).is_err());
    }

    #[test]
    fn test_delete_pending_task_fails() {
        clear_tasks();
        let id = create_task("https://example.com/file.zip".to_string(), None).unwrap();
        let result = delete_task(id.clone());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("不允许删除"));
    }

    // -- get_task_list 测试 --

    #[test]
    fn test_get_task_list_returns_all_tasks() {
        clear_tasks();
        let id1 = create_task("https://example.com/a.zip".to_string(), None).unwrap();
        let id2 = create_task("https://example.com/b.zip".to_string(), None).unwrap();
        let list = get_task_list();
        let ids: Vec<&String> = list.iter().map(|t| &t.id).collect();
        assert!(ids.contains(&&id1));
        assert!(ids.contains(&&id2));
    }

    #[test]
    fn test_get_task_list_empty() {
        clear_tasks();
        let list = get_task_list();
        assert!(list.is_empty());
    }

    // -- get_task_detail 测试 --

    #[test]
    fn test_get_task_detail_not_found() {
        clear_tasks();
        let result = get_task_detail("nonexistent-id".to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("任务不存在"));
    }

    // -- 配置测试 --

    #[test]
    fn test_get_config_returns_defaults() {
        reset_config();
        let cfg = get_config();
        assert_eq!(cfg.max_concurrent_tasks, 5);
        assert_eq!(cfg.max_concurrent_fragments, 16);
        assert_eq!(cfg.max_connections_per_host, 16);
        assert!(!cfg.enable_quic);
        assert!(cfg.verify_checksum);
    }

    #[test]
    fn test_update_config_roundtrip() {
        // 保存旧配置并在测试结束后恢复,避免污染其他测试
        let old_cfg = get_config();
        let new_cfg = AppConfig {
            download_dir: "/data/downloads".to_string(),
            max_concurrent_tasks: 10,
            max_concurrent_fragments: 32,
            max_connections_per_host: 8,
            enable_quic: true,
            verify_checksum: false,
        };
        update_config(new_cfg).unwrap();
        let cfg = get_config();
        assert_eq!(cfg.download_dir, "/data/downloads");
        assert_eq!(cfg.max_concurrent_tasks, 10);
        assert_eq!(cfg.max_concurrent_fragments, 32);
        assert_eq!(cfg.max_connections_per_host, 8);
        assert!(cfg.enable_quic);
        assert!(!cfg.verify_checksum);
        // 恢复旧配置
        let _ = update_config(old_cfg);
    }

    // -- 辅助函数测试 --

    #[test]
    fn test_extract_filename_basic() {
        assert_eq!(
            extract_filename("https://example.com/path/to/file.zip"),
            "file.zip"
        );
    }

    #[test]
    fn test_extract_filename_with_query() {
        assert_eq!(
            extract_filename("https://example.com/download?file=test.bin"),
            "download"
        );
    }

    #[test]
    fn test_extract_filename_empty_path() {
        assert_eq!(extract_filename("https://example.com/"), "unknown");
    }

    #[test]
    fn test_extract_filename_encoded() {
        assert_eq!(
            extract_filename("https://example.com/my%20file.txt"),
            "my file.txt"
        );
    }

    #[test]
    fn test_extract_filename_invalid_url() {
        assert_eq!(extract_filename("not a url"), "unknown");
    }

    #[test]
    fn test_percent_decode_basic() {
        assert_eq!(
            percent_decode("hello%20world"),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn test_percent_decode_no_encoding() {
        assert_eq!(
            percent_decode("filename.zip"),
            Some("filename.zip".to_string())
        );
    }

    // -- 任务状态流转完整性测试 --

    #[test]
    fn test_full_task_lifecycle() {
        clear_tasks();
        // 创建 -> 暂停 -> 恢复 -> 取消 -> 删除
        let id = create_task("https://example.com/lifecycle.bin".to_string(), None).unwrap();
        assert_eq!(get_task_detail(id.clone()).unwrap().status, "pending");

        pause_task(id.clone()).unwrap();
        assert_eq!(get_task_detail(id.clone()).unwrap().status, "paused");

        resume_task(id.clone()).unwrap();
        assert_eq!(get_task_detail(id.clone()).unwrap().status, "downloading");

        cancel_task(id.clone()).unwrap();
        assert_eq!(get_task_detail(id.clone()).unwrap().status, "cancelled");

        delete_task(id.clone()).unwrap();
        assert!(get_task_detail(id).is_err());
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
}
