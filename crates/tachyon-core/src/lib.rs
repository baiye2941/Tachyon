//! Tachyon 核心类型、trait 定义与错误体系
//!
//! 本 crate 定义所有模块共享的公共接口,包括:
//! - 下载任务、分片、协议、存储、校验的 trait 抽象
//! - 统一错误类型
//! - 配置类型

pub mod config;
pub mod download_source;
pub mod error;
pub mod safety;
#[cfg(any(test, feature = "test-harness"))]
pub mod test_harness;
pub mod traits;
pub mod types;
pub mod utils;

// 重新导出核心类型
pub use config::AppConfig;
pub use config::ConfigPatch;
pub use config::ConnectionPatch;
pub use config::DownloadPatch;
pub use config::IoStrategy;
pub use config::SchedulerConfig;
pub use config::USER_AGENT;
pub use download_source::{
    DownloadSource, DownloadSourceKind, classify_download_url, looks_like_hls_url,
    looks_like_magnet_url, parse_download_source,
};
pub use error::{DownloadError, DownloadResult};
pub use safety::{
    extract_filename, extract_filename_from_url, is_restricted_peer_ip, magnet_info_hash,
    parse_content_disposition, redact_url_for_log, reject_forbidden_ip,
    reject_symlink_or_reparse_components, sanitize_filename, url_for_display, url_identity_key,
    validate_multi_save_paths, validate_public_http_url, validate_redirect, validate_resolved_ip,
    validate_save_path,
};
pub use traits::{AsyncStorage, ByteStream, Protocol, TaskRunner, Verifier};
pub use types::{
    DownloadState, DownloadStateChange, FileLayout, FileMetadata, FileSpan, FragmentInfo,
    FragmentProgress, LayoutError, ObjectIdentity, TaskCommand, TaskId, TaskProgress,
};
pub use utils::{Metrics, hex_encode};

/// 验证统一配置类型存在且序列化往返正确
#[cfg(test)]
#[test]
#[allow(deprecated)]
fn app_config() {
    let cfg = config::DownloadConfig {
        download_dir: "/tmp/test".to_string(),
        max_concurrent_fragments: 8,
        max_retries: 5,
        request_timeout_secs: 60,
        connect_timeout_secs: 10,
        verify_checksum: false,
        verify_strategy: config::VerifyStrategy::BestEffort,
        user_agent: "Tachyon/Test".to_string(),
        headers: std::collections::HashMap::new(),
        auth_bearer: None,
        pause_timeout_secs: 300,
        rate_limit_bytes_per_sec: None,
        max_full_stream_bytes: config::default_max_full_stream_bytes(),
        authorized_dirs: vec!["/tmp/test".to_string()],
        io_strategy: config::IoStrategy::default(),
        proxy: None,
        enable_work_stealing: false,
        crash_consistency_mode: config::CrashConsistencyMode::default(),
    };
    assert_eq!(cfg.download_dir, "/tmp/test");
    assert_eq!(cfg.max_concurrent_fragments, 8);
    assert_eq!(cfg.max_retries, 5);
    assert_eq!(cfg.request_timeout_secs, 60);
    assert!(!cfg.verify_checksum);
    assert_eq!(cfg.user_agent, "Tachyon/Test");
    assert!(cfg.headers.is_empty());

    // 序列化往返
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(json.contains("downloadDir"), "JSON 应包含字段名: {json}");
    let restored: config::DownloadConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.download_dir, cfg.download_dir);
    assert_eq!(
        restored.max_concurrent_fragments,
        cfg.max_concurrent_fragments
    );
    assert_eq!(restored.max_retries, cfg.max_retries);
    assert_eq!(restored.request_timeout_secs, cfg.request_timeout_secs);
    assert_eq!(restored.verify_checksum, cfg.verify_checksum);
    assert_eq!(restored.user_agent, cfg.user_agent);
}
