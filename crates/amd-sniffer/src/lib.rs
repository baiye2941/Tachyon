//! AI Model Downloader 嗅探层:浏览器资源拦截与解析
//!
//! 通过 Playwright MCP 拦截浏览器流量:
//! - 请求拦截与资源类型识别
//! - 下载链接提取
//! - 媒体资源捕获
//! - 嗅探资源管理

pub mod capture;
pub mod filter;
pub mod resources;

pub use capture::{CaptureConfig, ResourceType, identify_resource, should_capture};
pub use resources::{ResourceManager, SnifferResource};

#[cfg(test)]
#[test]
/// 测试 WebView 嗅探:资源识别与捕获过滤逻辑
fn webview_sniff() {
    use capture::{CaptureConfig, ResourceType, identify_resource, should_capture};

    // === 视频资源识别 ===
    assert_eq!(
        identify_resource("https://cdn.example.com/movie.mp4"),
        ResourceType::Video
    );
    assert_eq!(
        identify_resource("https://stream.example.com/live.m3u8"),
        ResourceType::Video
    );
    assert_eq!(
        identify_resource("https://example.com/clip.webm"),
        ResourceType::Video
    );

    // === 压缩包识别 ===
    assert_eq!(
        identify_resource("https://download.example.com/pkg.zip"),
        ResourceType::Archive
    );
    assert_eq!(
        identify_resource("https://mirror.example.com/data.tar.gz"),
        ResourceType::Archive
    );

    // === 音频识别 ===
    assert_eq!(
        identify_resource("https://music.example.com/track.mp3"),
        ResourceType::Audio
    );
    assert_eq!(
        identify_resource("https://podcast.example.com/ep.flac"),
        ResourceType::Audio
    );

    // === 文档识别 ===
    assert_eq!(
        identify_resource("https://docs.example.com/manual.pdf"),
        ResourceType::Document
    );

    // === 带查询参数的 URL 仍能正确识别 ===
    assert_eq!(
        identify_resource("https://cdn.example.com/video.mp4?token=abc&quality=1080p"),
        ResourceType::Video
    );
    assert_eq!(
        identify_resource("https://dl.example.com/file.zip?dl=1&v=3"),
        ResourceType::Archive
    );

    // === HTML 页面不是可下载资源 ===
    assert_eq!(
        identify_resource("https://example.com/page.html"),
        ResourceType::Other
    );

    // === should_capture:默认配置捕获媒体 ===
    let default_config = CaptureConfig::default();
    assert!(
        should_capture("https://cdn.example.com/video.mp4", &default_config),
        "默认配置应捕获视频"
    );
    assert!(
        should_capture("https://example.com/file.zip", &default_config),
        "默认配置应捕获压缩包"
    );

    // === should_capture:禁用类型不捕获 ===
    let mut no_video_config = CaptureConfig::default();
    no_video_config.enabled_types.remove(&ResourceType::Video);
    assert!(
        !should_capture("https://cdn.example.com/video.mp4", &no_video_config),
        "禁用视频类型后不应捕获视频"
    );
    assert!(
        should_capture("https://example.com/file.zip", &no_video_config),
        "禁用视频不影响压缩包捕获"
    );

    // === should_capture:URL 过滤器 ===
    let filtered_config = CaptureConfig {
        url_filters: vec!["cdn.example.com".to_string()],
        ..Default::default()
    };
    assert!(
        should_capture("https://cdn.example.com/video.mp4", &filtered_config),
        "匹配过滤器的 URL 应被捕获"
    );
    assert!(
        !should_capture("https://other.com/video.mp4", &filtered_config),
        "不匹配过滤器的 URL 不应被捕获"
    );
}
