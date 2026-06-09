//! HTTP/HTTPS 协议实现
//!
//! 基于 reqwest 的 HTTP 客户端,支持:
//! - Range 请求(分片下载)
//! - HEAD 探测(文件元数据)
//! - Keep-Alive 连接复用

use std::net::ToSocketAddrs;
use std::pin::Pin;
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;
use futures::StreamExt;
use reqwest::Client;
use tachyon_core::filename::extract_filename;
use tachyon_core::traits::Protocol;
use tachyon_core::types::FileMetadata;
use tachyon_core::{ByteStream, DownloadError, DownloadResult};
use tracing::{debug, info, warn};

/// HTTP/HTTPS 协议客户端
pub struct HttpClient {
    client: Client,
}

impl HttpClient {
    /// 创建新的 HTTP 客户端(使用默认超时: 连接 10s, 读取 30s)
    pub fn new() -> DownloadResult<Self> {
        Self::with_timeouts(10, 30)
    }

    /// 创建带自定义超时的 HTTP 客户端
    ///
    /// # 参数
    /// - `connect_secs`: 连接超时(秒),0 表示禁用
    /// - `read_secs`: 读取超时(秒),0 表示禁用
    ///
    /// # 说明
    /// - 连接超时防止连接黑洞 IP 永久挂起
    /// - 读取超时防止连接后静默断流,但不会误杀正常的长下载
    pub fn with_timeouts(connect_secs: u64, read_secs: u64) -> DownloadResult<Self> {
        Self::build_client(connect_secs, read_secs, false)
    }

    /// 使用连接配置创建 HTTP 客户端(含 HTTP/2 控制)
    pub fn with_connection_config(
        config: &tachyon_core::config::ConnectionConfig,
        connect_secs: u64,
        read_secs: u64,
    ) -> DownloadResult<Self> {
        Self::build_client(connect_secs, read_secs, config.enable_http2)
    }

    fn build_client(connect_secs: u64, read_secs: u64, enable_http2: bool) -> DownloadResult<Self> {
        let mut builder = Client::builder()
            .user_agent(tachyon_core::config::USER_AGENT)
            .pool_max_idle_per_host(16)
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .no_proxy()
            .dns_resolver(PublicDnsResolver::new())
            .redirect(safe_redirect_policy());

        if connect_secs > 0 {
            builder = builder.connect_timeout(std::time::Duration::from_secs(connect_secs));
        }
        if read_secs > 0 {
            builder = builder.read_timeout(std::time::Duration::from_secs(read_secs));
        }
        if enable_http2 {
            builder = builder.http2_adaptive_window(true);
        }

        let client = builder
            .build()
            .map_err(|e| DownloadError::Network(format!("创建 HTTP 客户端失败: {e}")))?;
        Ok(Self { client })
    }

    /// 使用自定义 reqwest Client 创建
    pub fn with_client(client: Client) -> Self {
        Self { client }
    }

    /// 获取内部 reqwest Client 引用
    pub fn inner(&self) -> &Client {
        &self.client
    }
}

// Default 实现已移除 — TLS 初始化可能失败,请使用 HttpClient::new()

const DNS_CACHE_TTL_SECS: u64 = 60;

#[derive(Debug, Clone)]
struct PublicDnsResolver {
    cache: DashMap<String, (Vec<std::net::SocketAddr>, Instant)>,
}

impl PublicDnsResolver {
    fn new() -> Self {
        Self {
            cache: DashMap::new(),
        }
    }
}

impl reqwest::dns::Resolve for PublicDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let cache = self.cache.clone();

        if let Some(entry) = cache.get(&host)
            && entry.value().1.elapsed() < Duration::from_secs(DNS_CACHE_TTL_SECS)
        {
            let addrs = entry.value().0.clone();
            return Box::pin(async move { Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs) });
        }

        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = (host.as_str(), 0).to_socket_addrs()?.collect();
            for addr in &addrs {
                tachyon_core::reject_forbidden_ip(addr.ip())
                    .map_err(|err| -> Box<dyn std::error::Error + Send + Sync> { Box::new(err) })?;
            }
            cache.insert(host, (addrs.clone(), Instant::now()));
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn validate_redirect_target(url: &reqwest::Url) -> DownloadResult<()> {
    tachyon_core::validate_public_http_url(url)
}

fn safe_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("重定向次数超过 10 次");
        }
        if let Err(err) = validate_redirect_target(attempt.url()) {
            return attempt.error(err.to_string());
        }
        attempt.follow()
    })
}

/// 根据 HTTP 状态码和响应头对错误进行精确分类
///
/// - 429/503: 返回 Throttled,尝试解析 Retry-After 头中的秒数
/// - 401/403: 返回 Forbidden
/// - 其他: 返回通用 Protocol 错误
fn classify_http_error(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> DownloadError {
    let code = status.as_u16();
    match code {
        429 | 503 => {
            let retry_after_secs = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            DownloadError::Throttled { retry_after_secs }
        }
        401 | 403 => DownloadError::Forbidden { status: code },
        _ => DownloadError::Protocol(format!("HTTP {status}")),
    }
}

impl Protocol for HttpClient {
    fn probe(
        &self,
        url: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>> {
        let client = self.client.clone();
        let url = url.to_owned();
        Box::pin(async move {
            let parsed_url = reqwest::Url::parse(&url)?;
            tachyon_core::validate_public_http_url(&parsed_url)?;
            debug!(url = %tachyon_core::redact_url_for_log(&url), "HTTP HEAD 探测开始");
            let response = client.head(&url).send().await.map_err(|e| {
                let mut chain = String::new();
                let mut current: Option<&dyn std::error::Error> = Some(&e);
                while let Some(err) = current {
                    if !chain.is_empty() { chain.push_str(" -> "); }
                    chain.push_str(&err.to_string());
                    current = err.source();
                }
                warn!(url = %tachyon_core::redact_url_for_log(&url), error = %e, error_chain = %chain, "HEAD 请求连接失败");
                DownloadError::Network(format!("HEAD 请求失败: {chain}"))
            })?;

            let status = response.status();
            if !status.is_success() {
                warn!(url = %tachyon_core::redact_url_for_log(&url), status = %status, "HEAD 请求返回非成功状态码");
                return Err(classify_http_error(status, response.headers()));
            }

            let headers = response.headers();
            let content_disposition = headers
                .get("content-disposition")
                .and_then(|v| v.to_str().ok());
            let file_name = extract_filename(&url, content_disposition);
            let file_size = headers
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let content_type = headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string());
            let supports_range = headers
                .get("accept-ranges")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.contains("bytes"))
                .unwrap_or(false);
            let etag = headers
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string());
            let last_modified = headers
                .get("last-modified")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string());

            info!(
                url = %tachyon_core::redact_url_for_log(&url),
                file_size = ?file_size,
                supports_range = supports_range,
                content_type = ?content_type,
                "HTTP HEAD 探测完成"
            );

            Ok(FileMetadata {
                file_name,
                file_size,
                content_type,
                supports_range,
                etag,
                last_modified,
            })
        })
    }

    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let client = self.client.clone();
        let url = url.to_owned();
        Box::pin(async move {
            let parsed_url = reqwest::Url::parse(&url)?;
            tachyon_core::validate_public_http_url(&parsed_url)?;
            let range = format!("bytes={start}-{end}");
            debug!(url = %tachyon_core::redact_url_for_log(&url), start, end, "HTTP Range 请求开始");
            let response = client
                .get(&url)
                .header("Range", &range)
                .send()
                .await
                .map_err(|e| {
                    let mut chain = String::new();
                    let mut current: Option<&dyn std::error::Error> = Some(&e);
                    while let Some(err) = current {
                        if !chain.is_empty() { chain.push_str(" -> "); }
                        chain.push_str(&err.to_string());
                        current = err.source();
                    }
                    warn!(url = %tachyon_core::redact_url_for_log(&url), start, end, error = %e, error_chain = %chain, "Range 请求连接失败");
                    DownloadError::Network(format!("Range 请求失败: {chain}"))
                })?;

            let status = response.status();
            if status == reqwest::StatusCode::OK {
                warn!(url = %tachyon_core::redact_url_for_log(&url), "服务器忽略 Range 头,返回 HTTP 200");
                return Err(DownloadError::Protocol(
                    "服务器忽略 Range 头,返回 HTTP 200(不支持分片下载)".into(),
                ));
            }
            if status != reqwest::StatusCode::PARTIAL_CONTENT {
                warn!(url = %tachyon_core::redact_url_for_log(&url), status = %status, "Range 请求返回非预期状态码");
                return Err(classify_http_error(status, response.headers()));
            }

            let bytes = response
                .bytes()
                .await
                .map_err(|e| DownloadError::Network(format!("读取响应体失败: {e}")))?;

            info!(
                url = %tachyon_core::redact_url_for_log(&url),
                start,
                end,
                bytes = bytes.len(),
                "HTTP Range 下载完成"
            );
            Ok(bytes)
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>> {
        let client = self.client.clone();
        let url = url.to_owned();
        Box::pin(async move {
            let parsed_url = reqwest::Url::parse(&url)?;
            tachyon_core::validate_public_http_url(&parsed_url)?;
            let range = format!("bytes={start}-{end}");
            debug!(url = %tachyon_core::redact_url_for_log(&url), start, end, "HTTP 流式 Range 请求开始");
            let response = client
                .get(&url)
                .header("Range", range)
                .send()
                .await
                .map_err(|e| {
                    let mut chain = String::new();
                    let mut current: Option<&dyn std::error::Error> = Some(&e);
                    while let Some(err) = current {
                        if !chain.is_empty() { chain.push_str(" -> "); }
                        chain.push_str(&err.to_string());
                        current = err.source();
                    }
                    warn!(url = %tachyon_core::redact_url_for_log(&url), start, end, error = %e, error_chain = %chain, "流式 Range 请求连接失败");
                    DownloadError::Network(format!("Range 请求失败: {chain}"))
                })?;

            let status = response.status();
            if status == reqwest::StatusCode::OK {
                warn!(url = %tachyon_core::redact_url_for_log(&url), "服务器忽略 Range 头,返回 HTTP 200");
                return Err(DownloadError::Protocol(
                    "服务器忽略 Range 头,返回 HTTP 200(不支持分片下载)".into(),
                ));
            }
            if status != reqwest::StatusCode::PARTIAL_CONTENT {
                warn!(url = %tachyon_core::redact_url_for_log(&url), status = %status, "流式 Range 请求返回非预期状态码");
                return Err(classify_http_error(status, response.headers()));
            }

            info!(url = %tachyon_core::redact_url_for_log(&url), start, end, "HTTP 流式 Range 响应头已接收,开始流式传输");

            // 使用 bytes_stream() 获取真正的数据流,
            // 调用方通过 StreamExt::next() 逐块消费,峰值内存仅包含单个 chunk
            let stream = response.bytes_stream().map(|result| {
                result.map_err(|e| DownloadError::Network(format!("读取响应流数据失败: {e}")))
            });

            Ok(Box::pin(stream) as ByteStream)
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let client = self.client.clone();
        let url = url.to_owned();
        Box::pin(async move {
            let parsed_url = reqwest::Url::parse(&url)?;
            tachyon_core::validate_public_http_url(&parsed_url)?;
            let response = client
                .get(&url)
                .send()
                .await
                .map_err(|e| {
                    let mut chain = String::new();
                    let mut current: Option<&dyn std::error::Error> = Some(&e);
                    while let Some(err) = current {
                        if !chain.is_empty() { chain.push_str(" -> "); }
                        chain.push_str(&err.to_string());
                        current = err.source();
                    }
                    warn!(url = %tachyon_core::redact_url_for_log(&url), error = %e, error_chain = %chain, "整块下载请求连接失败");
                    DownloadError::Network(format!("下载请求失败: {chain}"))
                })?;

            let status = response.status();
            if !status.is_success() {
                return Err(classify_http_error(status, response.headers()));
            }

            // 限制非流式响应大小，防止 OOM
            const MAX_DOWNLOAD_SIZE: u64 = 10 * 1024 * 1024; // 10MB
            if let Some(content_length) = response.content_length()
                && content_length > MAX_DOWNLOAD_SIZE
            {
                return Err(DownloadError::Protocol(format!(
                    "响应体过大: {} > 最大允许 {} 字节",
                    content_length, MAX_DOWNLOAD_SIZE
                )));
            }

            response
                .bytes()
                .await
                .map_err(|e| DownloadError::Network(format!("读取响应体失败: {e}")))
        })
    }

    fn download_full_stream(
        &self,
        url: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>> {
        let client = self.client.clone();
        let url = url.to_owned();
        Box::pin(async move {
            let parsed_url = reqwest::Url::parse(&url)?;
            tachyon_core::validate_public_http_url(&parsed_url)?;
            debug!(url = %tachyon_core::redact_url_for_log(&url), "HTTP 整块流式请求开始");
            let response = client
                .get(&url)
                .send()
                .await
                .map_err(|e| {
                    let mut chain = String::new();
                    let mut current: Option<&dyn std::error::Error> = Some(&e);
                    while let Some(err) = current {
                        if !chain.is_empty() { chain.push_str(" -> "); }
                        chain.push_str(&err.to_string());
                        current = err.source();
                    }
                    warn!(url = %tachyon_core::redact_url_for_log(&url), error = %e, error_chain = %chain, "整块下载请求连接失败");
                    DownloadError::Network(format!("下载请求失败: {chain}"))
                })?;

            let status = response.status();
            if !status.is_success() {
                return Err(classify_http_error(status, response.headers()));
            }

            info!(url = %tachyon_core::redact_url_for_log(&url), "HTTP 整块流式响应头已接收,开始流式传输");

            // 使用 bytes_stream() 逐块产出,峰值内存仅含单个 chunk,
            // 避免大文件整块进内存
            let stream = response.bytes_stream().map(|result| {
                result.map_err(|e| DownloadError::Network(format!("读取响应流数据失败: {e}")))
            });

            Ok(Box::pin(stream) as ByteStream)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachyon_core::filename::parse_content_disposition;

    #[test]
    fn test_extract_filename_from_url() {
        assert_eq!(
            extract_filename("http://example.com/path/file.zip", None),
            "file.zip"
        );
    }

    #[test]
    fn test_extract_filename_from_url_root() {
        assert_eq!(extract_filename("http://example.com/", None), "unknown");
    }

    #[test]
    fn test_parse_content_disposition_filename() {
        assert_eq!(
            parse_content_disposition("attachment; filename=\"test.zip\""),
            Some("test.zip".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_no_quotes() {
        assert_eq!(
            parse_content_disposition("attachment; filename=test.zip"),
            Some("test.zip".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_empty() {
        assert_eq!(parse_content_disposition(""), None);
    }

    #[test]
    fn test_http_client_creation() {
        let client = HttpClient::new();
        assert!(client.is_ok());
    }

    #[test]
    fn test_http_client_new() {
        let _client = HttpClient::new().unwrap();
    }

    #[test]
    fn test_redirect_target_validation_rejects_loopback() {
        let target = reqwest::Url::parse("http://127.0.0.1/admin").unwrap();
        assert!(super::validate_redirect_target(&target).is_err());
    }

    #[test]
    fn test_redirect_target_validation_accepts_public() {
        let target = reqwest::Url::parse("https://example.com/file.bin").unwrap();
        assert!(super::validate_redirect_target(&target).is_ok());
    }

    #[tokio::test]
    async fn test_public_dns_resolver_rejects_localhost() {
        let resolver = super::PublicDnsResolver::new();
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        let result = reqwest::dns::Resolve::resolve(&resolver, name).await;
        assert!(result.is_err());
    }

    // --- 任务 1: with_timeouts 测试 ---

    #[test]
    fn test_with_timeouts_default_values() {
        // 默认构造(10s 连接, 30s 读取)应成功
        let client = HttpClient::new();
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_timeouts_custom_values() {
        let client = HttpClient::with_timeouts(5, 60);
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_timeouts_zero_connect_no_panic() {
        // connect_secs=0 表示禁用连接超时,不应 panic
        let client = HttpClient::with_timeouts(0, 30);
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_timeouts_zero_read_no_panic() {
        // read_secs=0 表示禁用读取超时,不应 panic
        let client = HttpClient::with_timeouts(10, 0);
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_timeouts_both_zero_no_panic() {
        // 同时禁用两项超时,不应 panic
        let client = HttpClient::with_timeouts(0, 0);
        assert!(client.is_ok());
    }

    // --- 任务 2: classify_http_error 测试 ---

    #[test]
    fn test_classify_429_with_retry_after() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "120".parse().unwrap());
        let err = super::classify_http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, &headers);
        match err {
            DownloadError::Throttled { retry_after_secs } => {
                assert_eq!(retry_after_secs, Some(120));
            }
            other => panic!("预期 Throttled,实际: {other:?}"),
        }
    }

    #[test]
    fn test_classify_429_without_retry_after() {
        let headers = reqwest::header::HeaderMap::new();
        let err = super::classify_http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, &headers);
        match err {
            DownloadError::Throttled { retry_after_secs } => {
                assert_eq!(retry_after_secs, None);
            }
            other => panic!("预期 Throttled,实际: {other:?}"),
        }
    }

    #[test]
    fn test_classify_429_with_invalid_retry_after() {
        // Retry-After 为 HTTP 日期格式(当前只解析纯秒数),应返回 None
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        let err = super::classify_http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, &headers);
        match err {
            DownloadError::Throttled { retry_after_secs } => {
                assert_eq!(retry_after_secs, None);
            }
            other => panic!("预期 Throttled,实际: {other:?}"),
        }
    }

    #[test]
    fn test_classify_503_with_retry_after() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "60".parse().unwrap());
        let err = super::classify_http_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, &headers);
        match err {
            DownloadError::Throttled { retry_after_secs } => {
                assert_eq!(retry_after_secs, Some(60));
            }
            other => panic!("预期 Throttled,实际: {other:?}"),
        }
    }

    #[test]
    fn test_classify_401_forbidden() {
        let headers = reqwest::header::HeaderMap::new();
        let err = super::classify_http_error(reqwest::StatusCode::UNAUTHORIZED, &headers);
        match err {
            DownloadError::Forbidden { status } => {
                assert_eq!(status, 401);
            }
            other => panic!("预期 Forbidden,实际: {other:?}"),
        }
    }

    #[test]
    fn test_classify_403_forbidden() {
        let headers = reqwest::header::HeaderMap::new();
        let err = super::classify_http_error(reqwest::StatusCode::FORBIDDEN, &headers);
        match err {
            DownloadError::Forbidden { status } => {
                assert_eq!(status, 403);
            }
            other => panic!("预期 Forbidden,实际: {other:?}"),
        }
    }

    #[test]
    fn test_classify_404_protocol_error() {
        let headers = reqwest::header::HeaderMap::new();
        let err = super::classify_http_error(reqwest::StatusCode::NOT_FOUND, &headers);
        match err {
            DownloadError::Protocol(msg) => {
                assert!(msg.contains("404"));
            }
            other => panic!("预期 Protocol,实际: {other:?}"),
        }
    }
}
