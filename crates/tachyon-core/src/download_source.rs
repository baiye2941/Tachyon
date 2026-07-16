//! 下载源类型：HTTP / HLS / Magnet 的单一分类入口
//!
//! 审计 A-06：避免 app 校验、engine 构造、probe 路径各自用字符串前缀
//! 导致语义分叉。本模块只负责 **分类 + 规范化 URL 字符串**；
//! magnet 细节校验（xt=btih）与 SSRF 深度策略仍由调用方组合本结果完成。

use serde::{Deserialize, Serialize};

use crate::error::{DownloadError, DownloadResult};
use crate::safety::validate_public_http_url;

/// 下载源种类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadSourceKind {
    /// 普通 HTTP/HTTPS 对象下载
    Http,
    /// HLS 媒体播放列表（路径以 .m3u8/.m3u 结尾）
    Hls,
    /// BitTorrent 磁力链接
    Magnet,
}

impl DownloadSourceKind {
    pub fn is_magnet(self) -> bool {
        matches!(self, Self::Magnet)
    }

    pub fn is_hls(self) -> bool {
        matches!(self, Self::Hls)
    }

    pub fn is_http_family(self) -> bool {
        matches!(self, Self::Http | Self::Hls)
    }
}

/// 经验证/分类后的下载源
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSource {
    pub kind: DownloadSourceKind,
    /// 原始 URL 字符串（保留用户输入形态；分类基于解析结果）
    pub url: String,
}

impl DownloadSource {
    pub fn kind(&self) -> DownloadSourceKind {
        self.kind
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

/// 磁力链接 scheme 前缀（与历史路径一致，大小写敏感 `magnet:?`）
pub fn looks_like_magnet_url(url: &str) -> bool {
    url.starts_with("magnet:?")
}

/// URL 路径（去 query/fragment）是否以 HLS playlist 扩展名结尾
pub fn looks_like_hls_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let path = parsed.path().to_ascii_lowercase();
    path.ends_with(".m3u8") || path.ends_with(".m3u")
}

/// 仅分类，不做 SSRF / magnet 细节校验（engine 热路径可用）
pub fn classify_download_url(url: &str) -> DownloadResult<DownloadSourceKind> {
    if looks_like_magnet_url(url) {
        return Ok(DownloadSourceKind::Magnet);
    }
    let parsed =
        url::Url::parse(url).map_err(|e| DownloadError::Config(format!("URL 格式无效: {e}")))?;
    match parsed.scheme() {
        "http" | "https" => {
            if looks_like_hls_url(url) {
                Ok(DownloadSourceKind::Hls)
            } else {
                Ok(DownloadSourceKind::Http)
            }
        }
        other => Err(DownloadError::Config(format!(
            "不支持的协议: {}",
            other.to_uppercase()
        ))),
    }
}

/// 解析并校验公网 HTTP(S) 源；magnet 仅做 scheme 分类（细节校验留给 protocol 层）
///
/// HTTP/HLS 路径会调用 `validate_public_http_url`（SSRF 直连边界）。
pub fn parse_download_source(url_str: &str) -> DownloadResult<DownloadSource> {
    if looks_like_magnet_url(url_str) {
        return Ok(DownloadSource {
            kind: DownloadSourceKind::Magnet,
            url: url_str.to_string(),
        });
    }

    // 先分类,再对 HTTP 族做 SSRF;避免 FTP 等先被 validate_public 报错路径不一致
    let kind = classify_download_url(url_str)?;
    match kind {
        DownloadSourceKind::Magnet => Ok(DownloadSource {
            kind,
            url: url_str.to_string(),
        }),
        DownloadSourceKind::Http | DownloadSourceKind::Hls => {
            let parsed = url::Url::parse(url_str)
                .map_err(|e| DownloadError::Config(format!("URL 格式无效: {e}")))?;
            validate_public_http_url(&parsed)?;
            Ok(DownloadSource {
                kind,
                url: url_str.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_http() {
        assert_eq!(
            classify_download_url("https://cdn.example.com/file.bin").unwrap(),
            DownloadSourceKind::Http
        );
    }

    #[test]
    fn test_classify_hls_strips_query() {
        assert_eq!(
            classify_download_url("https://cdn.example.com/live.m3u8?token=1").unwrap(),
            DownloadSourceKind::Hls
        );
        assert_eq!(
            classify_download_url("https://cdn.example.com/list.M3U").unwrap(),
            DownloadSourceKind::Hls
        );
    }

    #[test]
    fn test_classify_magnet() {
        assert_eq!(
            classify_download_url("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567")
                .unwrap(),
            DownloadSourceKind::Magnet
        );
    }

    #[test]
    fn test_classify_rejects_ftp() {
        let err = classify_download_url("ftp://example.com/a.bin").unwrap_err();
        assert!(
            err.to_string().contains("不支持")
                || err.to_string().contains("FTP")
                || err.to_string().contains("ftp")
                || err.to_string().contains("协议")
        );
    }

    #[test]
    fn test_parse_http_public_ok() {
        let src = parse_download_source("https://example.com/a.bin").unwrap();
        assert_eq!(src.kind, DownloadSourceKind::Http);
    }

    #[test]
    fn test_parse_rejects_private_ip() {
        // 用非 loopback 私网:workspace 开 test-harness 时 127.0.0.1 被放行供 wiremock,
        // 但 10.0.0.0/8 仍拒绝(与 reject_forbidden_ip 生产语义对齐)。
        let err = parse_download_source("http://10.0.0.1/a.bin").unwrap_err();
        let s = err.to_string();
        assert!(!s.is_empty());
        assert!(
            s.contains("受限") || s.contains("不允许") || s.contains("私"),
            "应拒绝私网 IP,got: {s}"
        );
    }

    #[test]
    fn test_looks_like_helpers() {
        assert!(looks_like_magnet_url("magnet:?xt=urn:btih:aa"));
        assert!(!looks_like_magnet_url("https://x/magnet:?"));
        assert!(looks_like_hls_url("https://x/a.m3u8#frag"));
        assert!(!looks_like_hls_url("https://x/a.mp4"));
    }
}
