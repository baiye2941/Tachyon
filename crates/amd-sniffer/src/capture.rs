//! 浏览器请求捕获
//!
//! 通过 Playwright MCP 拦截浏览器网络请求,识别可下载资源。

use std::collections::HashSet;

/// 可下载资源类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceType {
    Video,
    Audio,
    Document,
    Archive,
    Executable,
    Image,
    Other,
}

impl ResourceType {
    /// 转为小写字符串表示,用于 JSON 序列化
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceType::Video => "video",
            ResourceType::Audio => "audio",
            ResourceType::Document => "document",
            ResourceType::Archive => "archive",
            ResourceType::Executable => "executable",
            ResourceType::Image => "image",
            ResourceType::Other => "other",
        }
    }
}

/// 捕获规则配置
pub struct CaptureConfig {
    /// 启用的资源类型
    pub enabled_types: HashSet<ResourceType>,
    /// 最小文件大小(字节),低于此值不捕获
    pub min_size: u64,
    /// URL 过滤关键词
    pub url_filters: Vec<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        let mut enabled_types = HashSet::new();
        enabled_types.insert(ResourceType::Video);
        enabled_types.insert(ResourceType::Audio);
        enabled_types.insert(ResourceType::Document);
        enabled_types.insert(ResourceType::Archive);
        enabled_types.insert(ResourceType::Executable);
        Self {
            enabled_types,
            min_size: 1024, // 1KB
            url_filters: Vec::new(),
        }
    }
}

/// 根据文件扩展名识别资源类型
pub fn identify_resource(url: &str) -> ResourceType {
    // 剥离 query 参数和 fragment,只取路径部分
    let path = url::Url::parse(url)
        .ok()
        .map(|u| u.path().to_string())
        .unwrap_or_else(|| url.split('?').next().unwrap_or(url).to_string());
    let lower = path.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "mp4" | "webm" | "m3u8" | "flv" | "avi" | "mkv" | "mov" | "ts" => ResourceType::Video,
        "mp3" | "aac" | "flac" | "ogg" | "wav" | "wma" | "m4a" => ResourceType::Audio,
        "pdf" | "doc" | "docx" | "ppt" | "pptx" | "xls" | "xlsx" => ResourceType::Document,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" => ResourceType::Archive,
        "exe" | "msi" | "dmg" | "appimage" | "deb" | "rpm" => ResourceType::Executable,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" => ResourceType::Image,
        _ => ResourceType::Other,
    }
}

/// 检查 URL 是否匹配捕获规则
pub fn should_capture(url: &str, config: &CaptureConfig) -> bool {
    let resource_type = identify_resource(url);
    if !config.enabled_types.contains(&resource_type) {
        tracing::debug!(url, resource_type = ?resource_type, accepted = false, "资源被过滤");
        return false;
    }
    if !config.url_filters.is_empty() {
        let has_match = config.url_filters.iter().any(|f| url.contains(f.as_str()));
        if !has_match {
            tracing::debug!(url, accepted = false, "资源被过滤");
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identify_video() {
        assert_eq!(
            identify_resource("http://example.com/video.mp4"),
            ResourceType::Video
        );
        assert_eq!(
            identify_resource("http://example.com/stream.m3u8"),
            ResourceType::Video
        );
    }

    #[test]
    fn test_identify_audio() {
        assert_eq!(
            identify_resource("http://example.com/song.mp3"),
            ResourceType::Audio
        );
    }

    #[test]
    fn test_identify_document() {
        assert_eq!(
            identify_resource("http://example.com/report.pdf"),
            ResourceType::Document
        );
    }

    #[test]
    fn test_identify_archive() {
        assert_eq!(
            identify_resource("http://example.com/package.zip"),
            ResourceType::Archive
        );
    }

    #[test]
    fn test_identify_executable() {
        assert_eq!(
            identify_resource("http://example.com/setup.exe"),
            ResourceType::Executable
        );
    }

    #[test]
    fn test_identify_other() {
        assert_eq!(
            identify_resource("http://example.com/page"),
            ResourceType::Other
        );
    }

    #[test]
    fn test_should_capture_default() {
        let config = CaptureConfig::default();
        assert!(should_capture("http://example.com/video.mp4", &config));
        assert!(should_capture("http://example.com/file.zip", &config));
    }

    #[test]
    fn test_should_capture_filtered() {
        let config = CaptureConfig {
            url_filters: vec!["cdn.example.com".to_string()],
            ..Default::default()
        };
        assert!(should_capture("http://cdn.example.com/video.mp4", &config));
        assert!(!should_capture("http://other.com/video.mp4", &config));
    }

    #[test]
    fn test_should_capture_disabled_type() {
        let mut config = CaptureConfig::default();
        config.enabled_types.remove(&ResourceType::Image);
        assert!(!should_capture("http://example.com/photo.jpg", &config));
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(
            identify_resource("http://example.com/VIDEO.MP4"),
            ResourceType::Video
        );
    }

    #[test]
    fn test_url_with_query_params() {
        assert_eq!(
            identify_resource("http://example.com/file.zip?token=abc&v=2"),
            ResourceType::Archive
        );
        assert_eq!(
            identify_resource("http://cdn.example.com/video.mp4?dl=1"),
            ResourceType::Video
        );
    }

    #[test]
    fn test_url_with_fragment() {
        assert_eq!(
            identify_resource("http://example.com/doc.pdf#page=5"),
            ResourceType::Document
        );
    }
}
