use tachyon_core::config::{AppConfig, ConfigPatch};

use super::{AppError, AppState};

// ---------------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_config(state: tauri::State<'_, AppState>) -> Result<AppConfig, AppError> {
    get_config_inner(&state).await
}

#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, AppState>,
    patch: ConfigPatch,
    confirmation_token: Option<String>,
) -> Result<(), AppError> {
    // P1-11b: 验证一次性确认令牌，绑定 action 防止跨操作复用
    match confirmation_token {
        Some(token) => {
            state
                .service
                .confirmation_service
                .validate_and_consume(&token, "update_config")?;
        }
        None => {
            return Err(super::AppError::Config(
                "更新配置需要确认令牌,请先确认操作".to_string(),
            ));
        }
    }
    update_config_inner(&state, patch).await
}

// ---------------------------------------------------------------------------
// Inner implementations
// ---------------------------------------------------------------------------

async fn get_config_inner(state: &AppState) -> Result<AppConfig, AppError> {
    let cfg = state.domain.config.lock().await;
    Ok(cfg.clone())
}

async fn update_config_inner(state: &AppState, patch: ConfigPatch) -> Result<(), AppError> {
    let mut cfg = state.domain.config.lock().await;
    let updated = patch.apply_to(&cfg);
    // 校验在锁内执行,避免 drop 后重新获取锁的 TOCTOU 竞态
    // 路径校验(authorized_dirs canonicalize/create_dir)是轻量元数据操作,
    // 在 spawn_blocking 中执行以避免阻塞 Tokio 工作线程,但仍在锁的保护下
    let updated = tokio::task::spawn_blocking(move || -> Result<AppConfig, AppError> {
        validate_config(&updated)?;
        Ok(updated)
    })
    .await
    .map_err(|e| AppError::Config(format!("配置校验任务失败: {e}")))??;

    // 在写入新配置前记录新的 download_dir(避免写后读竞态)
    let new_download_dir = updated.download.download_dir.clone();
    *cfg = updated;
    // 将配置变更持久化到磁盘,避免重启后丢失
    let config_to_save = cfg.clone();
    drop(cfg);
    // 同步更新 TaskService 的缓存 download_dir,确保 persist_snapshot
    // 热路径读到最新值,无需再次获取 config 锁
    state
        .service
        .task_service
        .update_cached_download_dir(new_download_dir)
        .await;
    tokio::task::spawn_blocking(move || {
        crate::commands::config_commands::persist_config(&config_to_save)
    })
    .await
    .map_err(|e| AppError::Config(format!("持久化配置任务失败: {e}")))??;
    tracing::info!("应用配置已更新并持久化(白名单补丁模式)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

pub(crate) fn validate_config(config: &AppConfig) -> Result<(), AppError> {
    // 委托 core 层校验数值范围与其他基础字段,保持上下限一致
    config.validate().map_err(|e| match e {
        tachyon_core::DownloadError::Config(msg) => AppError::Config(msg),
        other => AppError::Core(other),
    })?;

    // 校验 authorized_dirs:每个授权根必须存在、是目录且不能是系统根目录
    for dir in &config.download.authorized_dirs {
        let path = std::path::Path::new(dir);
        if path.as_os_str().is_empty() {
            return Err(AppError::Config("authorized_dirs 不能为空路径".to_string()));
        }
        if !path.is_absolute() {
            return Err(AppError::Config(format!(
                "authorized_dirs 必须是绝对路径: {dir}"
            )));
        }
        if !path.exists() {
            return Err(AppError::Config(format!(
                "authorized_dirs 路径不存在: {dir}"
            )));
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| AppError::Config(format!("authorized_dirs 路径无法解析: {dir}")))?;
        if !canonical.is_dir() {
            return Err(AppError::Config(format!(
                "authorized_dirs 必须是目录: {dir}"
            )));
        }
        // 禁止系统根目录和 Unix 系统顶层目录
        if is_forbidden_authorized_root(&canonical) {
            return Err(AppError::Config(format!(
                "authorized_dirs 不允许包含系统根目录: {dir}"
            )));
        }
    }

    // S-02: 校验 download_dir 必须在 authorized_dirs 之下
    // 防止通过 update_config 将 download_dir 设置为 authorized_dirs 之外的路径,
    // 后续 create_task 会因 authorize_download_dir 校验失败而拒绝所有下载
    if !config.download.authorized_dirs.is_empty() {
        let download_path = std::path::Path::new(&config.download.download_dir);
        if download_path.is_absolute() {
            // download_dir 可能还不存在(新配置),尝试 canonicalize 现有前缀
            if let Ok(canonical_dl) = canonicalize_existing(download_path) {
                let roots = canonical_authorized_roots(config)?;
                if !roots
                    .iter()
                    .any(|root| canonical_dl.starts_with(root.as_path()))
                {
                    return Err(AppError::Config(format!(
                        "download_dir 不在 authorized_dirs 下: {}",
                        config.download.download_dir
                    )));
                }
            }
            // 如果 canonicalize_existing 失败(路径完全不存在),
            // 不阻断配置更新,等实际创建时 authorize_download_dir 再校验
        }
    }

    // 校验 headers:禁止设置敏感请求头,禁止键/值中包含 CRLF 注入字符
    for (key, value) in &config.download.headers {
        let lower = key.to_lowercase();
        if ["authorization", "cookie", "proxy-authorization"].contains(&lower.as_str()) {
            return Err(AppError::Config(format!("headers 不允许设置敏感头: {key}")));
        }
        if key.contains('\r') || key.contains('\n') {
            return Err(AppError::Config(format!(
                "headers 键不能包含换行符(CR/LF): {key}"
            )));
        }
        if value.contains('\r') || value.contains('\n') {
            return Err(AppError::Config(format!(
                "headers 值不能包含换行符(CR/LF): {key}"
            )));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 配置持久化
// ---------------------------------------------------------------------------

const CONFIG_FILE_NAME: &str = "config.json";

/// 获取持久化配置文件路径
///
/// 配置文件位于 `tachyon_core::config::dirs()/.tachyon/config.json`。
fn config_file_path() -> std::path::PathBuf {
    let data_root = tachyon_core::config::dirs().unwrap_or_else(|| std::path::PathBuf::from("."));
    data_root.join(".tachyon").join(CONFIG_FILE_NAME)
}

/// 将当前配置持久化到磁盘
///
/// 使用临时文件+重命名实现原子写入,避免写一半导致配置文件损坏。
/// 调用方应在非阻塞上下文中使用(如在 `spawn_blocking` 中调用)。
pub(crate) fn persist_config(config: &AppConfig) -> Result<(), AppError> {
    let path = config_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Config(format!("创建配置目录失败: {e}")))?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| AppError::Config(format!("序列化配置失败: {e}")))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json)
        .map_err(|e| AppError::Config(format!("写入配置临时文件失败: {e}")))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| AppError::Config(format!("重命名配置文件失败: {e}")))?;
    Ok(())
}

/// 从磁盘加载持久化配置
///
/// 若配置文件不存在则返回默认配置;若解析失败则返回错误,由调用方决定是否回退。
pub(crate) fn load_persisted_config() -> Result<AppConfig, AppError> {
    let path = config_file_path();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let json = std::fs::read_to_string(&path)
        .map_err(|e| AppError::Config(format!("读取配置文件失败: {e}")))?;
    let config: AppConfig = serde_json::from_str(&json)
        .map_err(|e| AppError::Config(format!("解析配置文件失败: {e}")))?;
    Ok(config)
}

pub(crate) fn authorize_download_dir(
    config: &AppConfig,
    requested_dir: &str,
) -> Result<String, AppError> {
    let requested = std::path::Path::new(requested_dir);
    if requested.as_os_str().is_empty() {
        return Err(AppError::Config("下载目录未授权: 空路径".to_string()));
    }
    if !requested.is_absolute() {
        return Err(AppError::Config(format!(
            "下载目录未授权: {}",
            requested.display()
        )));
    }
    if requested
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(AppError::Config(format!(
            "下载目录未授权: {}",
            requested.display()
        )));
    }

    let authorized_roots = canonical_authorized_roots(config)?;
    let Some(existing_ancestor) = deepest_existing_ancestor(requested) else {
        return Err(AppError::Config(format!(
            "下载目录未授权: {}",
            requested.display()
        )));
    };
    ensure_not_symlink_or_reparse(existing_ancestor, requested)?;
    if !existing_ancestor.is_dir() {
        return Err(AppError::Config(format!(
            "下载目录已存在但不是目录: {}",
            existing_ancestor.display()
        )));
    }

    let canonical_ancestor = existing_ancestor
        .canonicalize()
        .map_err(|_| AppError::Config(format!("下载目录无法解析: {}", requested.display())))?;
    let authorized_root = authorized_roots
        .iter()
        .find(|root| canonical_ancestor.starts_with(root.as_path()))
        .ok_or_else(|| AppError::Config(format!("下载目录未授权: {}", requested.display())))?;

    let candidate = create_authorized_dir_chain(
        canonical_ancestor,
        missing_components_after(requested, existing_ancestor)?,
        authorized_root,
        requested,
    )?;

    let canonical_requested = candidate
        .canonicalize()
        .map_err(|_| AppError::Config(format!("下载目录无法解析: {}", requested.display())))?;
    if !canonical_requested.is_dir() || !canonical_requested.starts_with(authorized_root) {
        return Err(AppError::Config(format!(
            "下载目录未授权: {}",
            requested.display()
        )));
    }

    Ok(canonical_requested.to_string_lossy().to_string())
}

/// 对用户明确选择的下载目录执行基本安全验证
///
/// 当用户通过对话框选择目录时调用,无需 authorized_dirs 白名单,
/// 但仍执行纵深防御:绝对路径、无路径遍历、非符号链接、非系统根目录。
/// 若目录不存在则自动创建。
/// 返回 canonicalize 后的路径,可直接加入 authorized_dirs。
pub(crate) fn validate_explicit_download_dir(requested_dir: &str) -> Result<String, AppError> {
    let requested = std::path::Path::new(requested_dir);
    if requested.as_os_str().is_empty() {
        return Err(AppError::Config("下载目录不能为空路径".to_string()));
    }
    if !requested.is_absolute() {
        return Err(AppError::Config(format!(
            "下载目录必须是绝对路径: {}",
            requested.display()
        )));
    }
    if requested
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(AppError::Config(format!(
            "下载目录不能包含路径遍历: {}",
            requested.display()
        )));
    }

    // 若目录不存在则创建
    if !requested.exists() {
        std::fs::create_dir_all(requested).map_err(|e| {
            AppError::Config(format!("创建下载目录失败: {}: {e}", requested.display()))
        })?;
    }

    let existing_ancestor = deepest_existing_ancestor(requested)
        .ok_or_else(|| AppError::Config(format!("下载目录路径无效: {}", requested.display())))?;

    // 纵深防御:拒绝符号链接/reparse point
    ensure_not_symlink_or_reparse(existing_ancestor, requested)?;
    if !existing_ancestor.is_dir() {
        return Err(AppError::Config(format!(
            "路径已存在但不是目录: {}",
            existing_ancestor.display()
        )));
    }

    let canonical = existing_ancestor
        .canonicalize()
        .map_err(|_| AppError::Config(format!("下载目录无法解析: {}", requested.display())))?;

    // 禁止系统根目录
    if is_forbidden_authorized_root(&canonical) {
        return Err(AppError::Config(format!(
            "不允许使用系统根目录: {}",
            requested.display()
        )));
    }

    // 若请求路径比 existing_ancestor 更深,沿路径创建缺失目录并 canonicalize
    if requested != existing_ancestor {
        std::fs::create_dir_all(requested).map_err(|e| {
            AppError::Config(format!("创建下载目录失败: {}: {e}", requested.display()))
        })?;
        let canonical_requested = requested
            .canonicalize()
            .map_err(|_| AppError::Config(format!("下载目录无法解析: {}", requested.display())))?;
        return Ok(canonical_requested.to_string_lossy().to_string());
    }

    Ok(canonical.to_string_lossy().to_string())
}

fn create_authorized_dir_chain(
    mut candidate: std::path::PathBuf,
    missing_components: Vec<std::ffi::OsString>,
    authorized_root: &std::path::Path,
    requested: &std::path::Path,
) -> Result<std::path::PathBuf, AppError> {
    ensure_authorized_directory(&candidate, authorized_root, requested)?;

    for component in missing_components {
        candidate.push(component);
        if candidate.exists() {
            ensure_authorized_directory(&candidate, authorized_root, requested)?;
            continue;
        }

        // 目录创建是快速元数据操作(<1ms),但在 async 上下文中仍应避免
        // 直接阻塞 Tokio 工作线程。使用 std::fs 而非 tokio::fs 因为
        // 此函数是同步的,且调用频率极低(仅用户修改配置时触发)。
        std::fs::create_dir(&candidate).map_err(|e| {
            AppError::Config(format!("创建下载目录失败: {}: {e}", requested.display()))
        })?;
        ensure_authorized_directory(&candidate, authorized_root, requested)?;
    }

    Ok(candidate)
}

fn ensure_authorized_directory(
    path: &std::path::Path,
    authorized_root: &std::path::Path,
    requested: &std::path::Path,
) -> Result<(), AppError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| AppError::Config(format!("下载目录无法解析: {}", requested.display())))?;
    if is_symlink_or_reparse(&metadata) {
        return Err(AppError::Config(format!(
            "下载目录未授权: {}",
            requested.display()
        )));
    }

    let canonical = path
        .canonicalize()
        .map_err(|_| AppError::Config(format!("下载目录无法解析: {}", requested.display())))?;
    if !canonical.is_dir() || !canonical.starts_with(authorized_root) {
        return Err(AppError::Config(format!(
            "下载目录未授权: {}",
            requested.display()
        )));
    }

    Ok(())
}

fn ensure_not_symlink_or_reparse(
    path: &std::path::Path,
    requested: &std::path::Path,
) -> Result<(), AppError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| AppError::Config(format!("下载目录无法解析: {}", requested.display())))?;
    if is_symlink_or_reparse(&metadata) {
        return Err(AppError::Config(format!(
            "下载目录未授权: {}",
            requested.display()
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn is_symlink_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_symlink_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::{FileTypeExt, MetadataExt};
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let file_type = metadata.file_type();
    file_type.is_symlink_dir()
        || file_type.is_symlink_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn canonical_authorized_roots(config: &AppConfig) -> Result<Vec<std::path::PathBuf>, AppError> {
    if config.download.authorized_dirs.is_empty() {
        return Err(AppError::Config("authorized_dirs 不能为空".to_string()));
    }

    config
        .download
        .authorized_dirs
        .iter()
        .map(|dir| {
            let path = std::path::Path::new(dir);
            if path.as_os_str().is_empty() || !path.is_absolute() || !path.exists() {
                return Err(AppError::Config(format!("authorized_dirs 路径无效: {dir}")));
            }
            let canonical = path
                .canonicalize()
                .map_err(|_| AppError::Config(format!("authorized_dirs 路径无法解析: {dir}")))?;
            if !canonical.is_dir() || is_forbidden_authorized_root(&canonical) {
                return Err(AppError::Config(format!("authorized_dirs 路径无效: {dir}")));
            }
            Ok(canonical)
        })
        .collect()
}

fn is_forbidden_authorized_root(canonical: &std::path::Path) -> bool {
    let is_root = canonical.parent().is_none();
    let first_normal_component = canonical.components().find_map(|component| {
        if let std::path::Component::Normal(name) = component {
            name.to_str()
        } else {
            None
        }
    });
    let is_unix_system_top_dir = matches!(first_normal_component, Some("usr" | "etc" | "System"));
    // Windows 系统目录保护
    #[cfg(target_os = "windows")]
    let is_windows_system_top_dir = matches!(
        first_normal_component,
        Some("Windows" | "Program Files" | "Program Files (x86)" | "ProgramData")
    );
    #[cfg(not(target_os = "windows"))]
    let is_windows_system_top_dir = false;
    is_root || is_unix_system_top_dir || is_windows_system_top_dir
}

/// 对路径的已存在前缀执行 canonicalize
///
/// 如果路径本身存在,直接 canonicalize;否则找到最深已存在祖先并 canonicalize。
/// 用于校验 download_dir 与 authorized_dirs 的归属关系,
/// 即使 download_dir 尚未创建(新配置场景),只要已存在部分在授权根下即可。
fn canonicalize_existing(path: &std::path::Path) -> Result<std::path::PathBuf, AppError> {
    if path.exists() {
        path.canonicalize()
            .map_err(|_| AppError::Config(format!("路径无法解析: {}", path.display())))
    } else if let Some(ancestor) = deepest_existing_ancestor(path) {
        ancestor
            .canonicalize()
            .map_err(|_| AppError::Config(format!("路径前缀无法解析: {}", ancestor.display())))
    } else {
        Err(AppError::Config(format!(
            "路径及其父目录均不存在: {}",
            path.display()
        )))
    }
}

fn deepest_existing_ancestor(path: &std::path::Path) -> Option<&std::path::Path> {
    path.ancestors().find(|ancestor| ancestor.exists())
}

fn missing_components_after(
    requested: &std::path::Path,
    existing_ancestor: &std::path::Path,
) -> Result<Vec<std::ffi::OsString>, AppError> {
    let relative = requested
        .strip_prefix(existing_ancestor)
        .map_err(|_| AppError::Config(format!("下载目录无法解析: {}", requested.display())))?;
    Ok(relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_os_string()),
            _ => None,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::tests::test_state;
    use super::super::{build_download_config, persist_task_snapshot};
    use super::*;
    use tachyon_core::config::{
        ConfigPatch, ConnectionPatch, DownloadPatch, IoStrategy, USER_AGENT,
    };

    /// 创建临时测试路径,确保在 authorized_dirs (tachyon-test-downloads) 下
    /// S-02: validate_config 要求 download_dir 在 authorized_dirs 下
    fn test_tmp_path(name: &str) -> String {
        let dir = std::env::temp_dir()
            .join("tachyon-test-downloads")
            .join(format!("sub-{name}"));
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
                verify_strategy: tachyon_core::config::VerifyStrategy::BestEffort,
                user_agent: USER_AGENT.to_string(),
                headers: std::collections::HashMap::new(),
                pause_timeout_secs: 300,
                rate_limit_bytes_per_sec: None,
                max_full_stream_bytes: tachyon_core::config::default_max_full_stream_bytes(),
                authorized_dirs: vec![download_dir.to_string()],
                io_strategy: IoStrategy::default(),
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

    /// 构建测试用 ConfigPatch,设置关键 patchable 字段
    fn make_test_patch(
        max_concurrent_tasks: Option<u32>,
        download_dir: Option<String>,
        max_concurrent_fragments: Option<u32>,
        max_connections_per_host: Option<u32>,
        enable_quic: Option<bool>,
        verify_checksum: Option<bool>,
    ) -> ConfigPatch {
        ConfigPatch {
            max_concurrent_tasks,
            download: Some(DownloadPatch {
                download_dir,
                max_concurrent_fragments,
                max_retries: None,
                request_timeout_secs: None,
                connect_timeout_secs: None,
                verify_checksum,
                pause_timeout_secs: None,
                rate_limit_bytes_per_sec: None,
                io_strategy: None,
            }),
            connection: Some(ConnectionPatch {
                max_connections_per_host,
                max_global_connections: None,
                keep_alive_timeout_secs: None,
                connect_timeout_secs: None,
                enable_http2: None,
                enable_quic,
            }),
        }
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
    async fn test_update_config_patch_roundtrip() {
        let state = test_state();
        // S-02: download_dir 必须在 authorized_dirs 下,test_state 的 authorized_dirs
        // 为 tachyon-test-downloads,所以新 download_dir 必须在其子目录下
        let existing_auth_dir = std::env::temp_dir().join("tachyon-test-downloads");
        let _ = std::fs::create_dir_all(&existing_auth_dir);
        let dl_dir = existing_auth_dir.join("sub-downloads");
        std::fs::create_dir_all(&dl_dir).unwrap();
        let dl_dir_str = dl_dir.to_string_lossy().to_string();

        let patch = ConfigPatch {
            max_concurrent_tasks: Some(10),
            download: Some(DownloadPatch {
                download_dir: Some(dl_dir_str.clone()),
                max_concurrent_fragments: Some(32),
                max_retries: None,
                request_timeout_secs: None,
                connect_timeout_secs: None,
                verify_checksum: Some(false),
                pause_timeout_secs: None,
                rate_limit_bytes_per_sec: None,
                io_strategy: None,
            }),
            connection: Some(ConnectionPatch {
                max_connections_per_host: Some(8),
                max_global_connections: None,
                keep_alive_timeout_secs: None,
                connect_timeout_secs: None,
                enable_http2: None,
                enable_quic: Some(true),
            }),
        };
        update_config_inner(&state, patch).await.unwrap();
        let cfg = get_config_inner(&state).await.unwrap();
        assert_eq!(cfg.download.download_dir, dl_dir_str);
        assert_eq!(cfg.max_concurrent_tasks, 10);
        assert_eq!(cfg.download.max_concurrent_fragments, 32);
        assert_eq!(cfg.connection.max_connections_per_host, 8);
        assert!(cfg.connection.enable_quic);
        assert!(!cfg.download.verify_checksum);
    }

    #[tokio::test]
    async fn test_update_config_patch_preserves_unpatched_fields() {
        let state = test_state();

        // 先设置一些非默认值
        let setup_patch = ConfigPatch {
            max_concurrent_tasks: Some(7),
            download: Some(DownloadPatch {
                download_dir: None,
                max_concurrent_fragments: Some(24),
                max_retries: None,
                request_timeout_secs: None,
                connect_timeout_secs: None,
                verify_checksum: None,
                pause_timeout_secs: None,
                rate_limit_bytes_per_sec: None,
                io_strategy: None,
            }),
            connection: None,
        };
        update_config_inner(&state, setup_patch).await.unwrap();

        // 只 patch connection,不传 download
        let partial_patch = ConfigPatch {
            max_concurrent_tasks: None,
            download: None,
            connection: Some(ConnectionPatch {
                max_connections_per_host: Some(4),
                max_global_connections: None,
                keep_alive_timeout_secs: None,
                connect_timeout_secs: None,
                enable_http2: None,
                enable_quic: Some(true),
            }),
        };
        update_config_inner(&state, partial_patch).await.unwrap();
        let cfg = get_config_inner(&state).await.unwrap();

        // download 字段应保持之前 patch 的值
        assert_eq!(cfg.max_concurrent_tasks, 7);
        assert_eq!(cfg.download.max_concurrent_fragments, 24);
        // connection 中 patch 的字段应更新
        assert_eq!(cfg.connection.max_connections_per_host, 4);
        assert!(cfg.connection.enable_quic);
        // 安全字段 user_agent/headers/authorized_dirs 应保持不变
        assert_eq!(cfg.download.user_agent, USER_AGENT);
        assert!(cfg.download.headers.is_empty());
    }

    #[tokio::test]
    async fn test_update_config_rejects_invalid_without_mutating_current_config() {
        let state = test_state();
        let before = get_config_inner(&state).await.unwrap();

        // 使用 patch 设置超范围的 max_concurrent_fragments
        let invalid_patch = make_test_patch(None, None, Some(257), None, None, None);
        let result = update_config_inner(&state, invalid_patch).await;

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
        use super::super::TaskInfo;
        use tachyon_core::types::DownloadState;

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
            save_path: "/custom/file.bin".to_string(),
            error_reason: None,
            retry_count: 0,
        };
        state
            .domain
            .task_repository
            .insert(task.id.clone(), task.clone());
        let original_snapshot = crate::task_store::task_info_to_snapshot(
            &task,
            "/custom/file.bin".to_string(),
            256,
            vec![0],
            std::collections::HashMap::new(),
            None,
            None,
        );
        state
            .infra
            .task_store
            .save_snapshot(&original_snapshot)
            .unwrap();

        persist_task_snapshot(&state, &task.id, None).await;

        let loaded = state.infra.task_store.load_recoverable().unwrap();
        let snapshot = loaded
            .iter()
            .find(|snapshot| snapshot.id == task.id)
            .unwrap();
        assert_eq!(snapshot.save_path, "/custom/file.bin");
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
                verify_strategy: tachyon_core::config::VerifyStrategy::BestEffort,
                user_agent: USER_AGENT.to_string(),
                headers: std::collections::HashMap::new(),
                pause_timeout_secs: 300,
                rate_limit_bytes_per_sec: None,
                max_full_stream_bytes: tachyon_core::config::default_max_full_stream_bytes(),
                authorized_dirs: vec!["/tmp".to_string()],
                io_strategy: IoStrategy::default(),
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

    #[tokio::test]
    async fn test_update_config_rejects_zero_max_concurrent_tasks() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            make_test_patch(Some(0), None, None, None, None, None),
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
            make_test_patch(None, None, Some(0), None, None, None),
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
            make_test_patch(Some(101), None, None, None, None, None),
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
            make_test_patch(None, None, Some(257), None, None, None),
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
        let result = update_config_inner(
            &state,
            make_test_patch(None, Some("".to_string()), None, None, None, None),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("download_dir"));
    }

    #[tokio::test]
    async fn test_update_config_accepts_valid_boundary_values() {
        let state = test_state();
        let result = update_config_inner(
            &state,
            make_test_patch(
                Some(1),
                Some(test_tmp_path("d")),
                Some(1),
                Some(1),
                None,
                Some(true),
            ),
        )
        .await;
        assert!(result.is_ok());

        let result = update_config_inner(
            &state,
            make_test_patch(
                Some(64),
                Some(test_tmp_path("e")),
                Some(32),
                Some(16),
                None,
                None,
            ),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_update_config_empty_patch_preserves_all_fields() {
        let state = test_state();
        let before = get_config_inner(&state).await.unwrap();

        let empty_patch = ConfigPatch {
            max_concurrent_tasks: None,
            download: None,
            connection: None,
        };
        update_config_inner(&state, empty_patch).await.unwrap();
        let after = get_config_inner(&state).await.unwrap();

        assert_eq!(after.max_concurrent_tasks, before.max_concurrent_tasks);
        assert_eq!(after.download.download_dir, before.download.download_dir);
        assert_eq!(
            after.download.max_concurrent_fragments,
            before.download.max_concurrent_fragments
        );
        assert_eq!(after.download.user_agent, before.download.user_agent);
        assert_eq!(
            after.download.authorized_dirs,
            before.download.authorized_dirs
        );
    }

    #[test]
    fn test_config_patch_serialization_roundtrip() {
        let patch = ConfigPatch {
            max_concurrent_tasks: Some(10),
            download: Some(DownloadPatch {
                download_dir: Some("/new/dir".to_string()),
                max_concurrent_fragments: Some(32),
                max_retries: Some(5),
                request_timeout_secs: Some(60),
                connect_timeout_secs: Some(15),
                verify_checksum: Some(false),
                pause_timeout_secs: Some(600),
                rate_limit_bytes_per_sec: Some(Some(1_048_576)),
                io_strategy: Some(IoStrategy::WinAligned),
            }),
            connection: Some(ConnectionPatch {
                max_connections_per_host: Some(8),
                max_global_connections: Some(512),
                keep_alive_timeout_secs: Some(60),
                connect_timeout_secs: Some(15),
                enable_http2: Some(false),
                enable_quic: Some(true),
            }),
        };
        let json = serde_json::to_string(&patch).unwrap();
        let deserialized: ConfigPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_concurrent_tasks, Some(10));
        assert_eq!(
            deserialized.download.unwrap().download_dir,
            Some("/new/dir".to_string())
        );
    }

    #[test]
    fn test_config_patch_deserializes_partial() {
        let json = r#"{"maxConcurrentTasks":7}"#;
        let patch: ConfigPatch = serde_json::from_str(json).unwrap();
        assert_eq!(patch.max_concurrent_tasks, Some(7));
        assert!(patch.download.is_none());
        assert!(patch.connection.is_none());
    }

    #[test]
    fn test_validate_config_rejects_sensitive_headers() {
        let download_dir = test_tmp_path("sensitive-headers");
        let mut config = make_test_app_config(5, &download_dir, 16, 16, false, true);
        config
            .download
            .headers
            .insert("Authorization".to_string(), "secret".to_string());

        let result = validate_config(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("敏感头"));
    }

    #[test]
    fn test_validate_config_rejects_crlf_in_header_value() {
        let download_dir = test_tmp_path("crlf-headers");
        let mut config = make_test_app_config(5, &download_dir, 16, 16, false, true);
        config.download.headers.insert(
            "X-Custom".to_string(),
            "value\r\nInjected: true".to_string(),
        );

        let result = validate_config(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("换行符"));
    }

    #[test]
    fn test_validate_config_rejects_crlf_in_header_key() {
        let download_dir = test_tmp_path("crlf-key-headers");
        let mut config = make_test_app_config(5, &download_dir, 16, 16, false, true);
        config.download.headers.insert(
            "X-Custom\r\nInjected: true".to_string(),
            "value".to_string(),
        );

        let result = validate_config(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("换行符"));
    }

    #[test]
    fn test_validate_config_rejects_empty_authorized_dirs() {
        let download_dir = test_tmp_path("empty-authorized-dirs");
        let mut config = make_test_app_config(5, &download_dir, 16, 16, false, true);
        config.download.authorized_dirs.clear();

        let result = validate_config(&config);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("authorized_dirs"));
    }

    #[test]
    fn test_validate_config_rejects_nonexistent_authorized_dir() {
        let download_dir = test_tmp_path("missing-authorized-base");
        let mut config = make_test_app_config(5, &download_dir, 16, 16, false, true);
        config.download.authorized_dirs = vec![
            std::env::temp_dir()
                .join("tachyon-missing-authorized-dir")
                .join(uuid::Uuid::new_v4().to_string())
                .to_string_lossy()
                .to_string(),
        ];

        let result = validate_config(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("路径不存在"));
    }

    #[test]
    fn test_validate_config_rejects_root_authorized_dir() {
        let download_dir = test_tmp_path("root-authorized-base");
        let mut config = make_test_app_config(5, &download_dir, 16, 16, false, true);
        let root = std::env::temp_dir()
            .ancestors()
            .last()
            .expect("temp dir should have a root")
            .to_string_lossy()
            .to_string();
        config.download.authorized_dirs = vec![root];

        let result = validate_config(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("系统根目录"));
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

        let authorized = authorize_download_dir(&config, &safe_path).unwrap();
        assert_eq!(
            std::path::Path::new(&authorized),
            safe_dir.path().canonicalize().unwrap()
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

        let authorized = authorize_download_dir(&config, &sub_path).unwrap();
        assert_eq!(
            std::path::Path::new(&authorized),
            std::path::Path::new(&sub_path).canonicalize().unwrap()
        );
    }

    #[test]
    fn test_authorize_download_dir_creates_missing_authorized_subdir_and_returns_canonical_path() {
        let safe_dir = tempfile::tempdir().unwrap();
        let safe_path = safe_dir.path().to_string_lossy().to_string();
        let requested = safe_dir.path().join("downloads").join("models");
        let requested_path = requested.to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.download.download_dir = safe_path.clone();
        config.download.authorized_dirs = vec![safe_path];

        let authorized = authorize_download_dir(&config, &requested_path).unwrap();

        assert!(requested.is_dir());
        assert_eq!(
            std::path::Path::new(&authorized),
            requested.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_authorize_download_dir_rejects_existing_symlink_component_without_creating_target() {
        let safe_dir = tempfile::tempdir().unwrap();
        let target_dir = safe_dir.path().join("real");
        std::fs::create_dir(&target_dir).unwrap();
        let safe_path = safe_dir.path().to_string_lossy().to_string();
        let link_path = safe_dir.path().join("link");
        let target_created = target_dir.join("created-by-authorize");
        let requested = link_path.join("created-by-authorize");
        let requested_path = requested.to_string_lossy().to_string();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target_dir, &link_path).unwrap();

        #[cfg(windows)]
        {
            if let Err(e) = std::os::windows::fs::symlink_dir(&target_dir, &link_path) {
                eprintln!("跳过 symlink 逃逸测试: 当前 Windows 权限不允许创建目录符号链接: {e}");
                return;
            }
        }

        let mut config = AppConfig::default();
        config.download.download_dir = safe_path.clone();
        config.download.authorized_dirs = vec![safe_path];

        let err = authorize_download_dir(&config, &requested_path).unwrap_err();

        assert!(err.to_string().contains("未授权"));
        assert!(
            !target_created.exists(),
            "拒绝 symlink/reparse 组件时不得在链接目标下创建子目录"
        );
    }

    #[test]
    fn test_authorize_download_dir_rejects_missing_subdir_that_escapes_authorized_root() {
        let safe_dir = tempfile::tempdir().unwrap();
        let safe_path = safe_dir.path().to_string_lossy().to_string();
        let escaped_name = format!("escaped-downloads-{}", uuid::Uuid::new_v4());
        let escaped = safe_dir.path().parent().unwrap().join(&escaped_name);
        let requested = safe_dir.path().join("..").join(&escaped_name);
        let requested_path = requested.to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.download.download_dir = safe_path.clone();
        config.download.authorized_dirs = vec![safe_path];

        let err = authorize_download_dir(&config, &requested_path).unwrap_err();

        assert!(err.to_string().contains("未授权"));
        assert!(!escaped.exists());
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

    #[test]
    fn test_is_forbidden_authorized_root_rejects_unix_system_dirs() {
        assert!(is_forbidden_authorized_root(std::path::Path::new("/usr")));
        assert!(is_forbidden_authorized_root(std::path::Path::new("/etc")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_is_forbidden_authorized_root_rejects_windows_system_dirs() {
        assert!(is_forbidden_authorized_root(std::path::Path::new(
            "C:\\Windows"
        )));
        assert!(is_forbidden_authorized_root(std::path::Path::new(
            "C:\\Program Files"
        )));
        assert!(is_forbidden_authorized_root(std::path::Path::new(
            "C:\\Program Files (x86)"
        )));
        assert!(is_forbidden_authorized_root(std::path::Path::new(
            "C:\\ProgramData"
        )));
    }

    #[test]
    fn test_is_forbidden_authorized_root_allows_user_dirs() {
        assert!(!is_forbidden_authorized_root(std::path::Path::new(
            "/home/user/downloads"
        )));
    }
}
