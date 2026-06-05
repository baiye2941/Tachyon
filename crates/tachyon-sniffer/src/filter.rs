//! 资源类型过滤与识别
//!
//! 基于 URL 和 Content-Type 的资源分类。

/// 从 Content-Type 推断资源类型
pub fn from_content_type(content_type: &str) -> Option<super::capture::ResourceType> {
    let ct = content_type.to_lowercase();
    if ct.starts_with("video/") {
        Some(super::capture::ResourceType::Video)
    } else if ct.starts_with("audio/") {
        Some(super::capture::ResourceType::Audio)
    } else if ct.starts_with("image/") {
        Some(super::capture::ResourceType::Image)
    } else if ct.starts_with("application/pdf")
        || ct.starts_with("application/msword")
        || ct.starts_with("application/vnd.openxmlformats")
    {
        Some(super::capture::ResourceType::Document)
    } else if ct.starts_with("application/zip")
        || ct.starts_with("application/x-rar")
        || ct.starts_with("application/x-7z")
        || ct.starts_with("application/gzip")
    {
        Some(super::capture::ResourceType::Archive)
    } else if ct.starts_with("application/octet-stream") {
        Some(super::capture::ResourceType::Other)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::ResourceType;

    #[test]
    fn test_video_content_type() {
        assert_eq!(from_content_type("video/mp4"), Some(ResourceType::Video));
    }

    #[test]
    fn test_audio_content_type() {
        assert_eq!(from_content_type("audio/mpeg"), Some(ResourceType::Audio));
    }

    #[test]
    fn test_pdf_content_type() {
        assert_eq!(
            from_content_type("application/pdf"),
            Some(ResourceType::Document)
        );
    }

    #[test]
    fn test_unknown_content_type() {
        assert_eq!(from_content_type("text/html"), None);
    }
}
