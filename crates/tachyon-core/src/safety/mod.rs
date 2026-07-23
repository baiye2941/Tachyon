//! 文件名提取、Content-Disposition 解析与路径穿越防护
//!
//! 子模块:
//! - [`filename`][]: 文件名提取、清洗与保存路径校验
//! - [`url_safety`][]: 公网 URL 校验、SSRF 防护、DNS Rebinding 防御

pub mod filename;
pub mod url_safety;

// 重新导出公开 API,保持与旧 `pub mod safety` 一致的调用路径
pub use filename::{
    extract_filename, extract_filename_from_url, parse_content_disposition,
    reject_symlink_or_reparse_components, sanitize_filename, validate_multi_save_paths,
    validate_save_path,
};
pub use url_safety::{
    is_restricted_peer_ip, magnet_info_hash, redact_url_for_log, reject_forbidden_ip,
    url_for_display, url_identity_key, validate_public_http_url, validate_redirect,
    validate_resolved_ip,
};
