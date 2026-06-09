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
        // 禁止系统根目录和 Unix 系统顶层目录
        let is_root = canonical.parent().is_none();
        let first_normal_component = canonical.components().find_map(|component| {
            if let std::path::Component::Normal(name) = component {
                name.to_str()
            } else {
                None
            }
        });
        let is_unix_system_top_dir =
            matches!(first_normal_component, Some("usr" | "etc" | "System"));
        let forbidden = is_root || is_unix_system_top_dir;
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

pub(crate) fn authorize_download_dir(config: &AppConfig, requested_dir: &str) -> Result<String, AppError> {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::test_state;
    use super::super::{build_download_config, persist_task_snapshot};
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
