//! 文件名提取、Content-Disposition 解析与路径穿越防护
//!
//! 子模块:
//! - [`filename`][]: 文件名提取、清洗与保存路径校验
//! - [`url_safety`][]: 公网 URL 校验、SSRF 防护、DNS Rebinding 防御

pub mod filename;
pub mod url_safety;

// 重新导出公开 API,保持与旧 `pub mod safety` 一致的调用路径
pub use filename::{
    extract_filename, extract_filename_from_url, parse_content_disposition, sanitize_filename,
    validate_save_path,
};
pub use url_safety::{
    redact_url_for_log, reject_forbidden_ip, validate_public_http_url, validate_redirect,
    validate_resolved_ip,
};
