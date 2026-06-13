use tachyon_core::config::AppConfig;

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
    config: AppConfig,
) -> Result<(), AppError> {
    update_config_inner(&state, config).await
}

// ---------------------------------------------------------------------------
// Inner implementations
// ---------------------------------------------------------------------------

async fn get_config_inner(state: &AppState) -> Result<AppConfig, AppError> {
    let cfg = state.config.lock().await;
    Ok(cfg.clone())
}

async fn update_config_inner(state: &AppState, config: AppConfig) -> Result<(), AppError> {
    validate_config(&config)?;
    let mut cfg = state.config.lock().await;
    *cfg = config;
    tracing::info!("应用配置已更新");
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
    is_root || is_unix_system_top_dir
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
    use tachyon_core::config::{IoStrategy, USER_AGENT};

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
                max_full_stream_bytes: tachyon_core::config::default_max_full_stream_bytes(),
                authorized_dirs: vec![dl_dir_str.clone()],
                io_strategy: IoStrategy::default(),
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
        invalid.download.max_concurrent_fragments = 257;

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
            make_test_app_config(101, &test_tmp_path("b"), 16, 16, false, true),
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
            make_test_app_config(5, &test_tmp_path("c"), 257, 16, false, true),
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
}
