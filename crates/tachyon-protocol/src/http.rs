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

// ---------------------------------------------------------------------------
// Error chain utility
// ---------------------------------------------------------------------------

/// 构造错误链字符串: `err -> source1 -> source2 -> ...`
///
/// T-04: 提取重复的错误链拼接逻辑,消除 http.rs 中 6 处相同代码。
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut chain = String::new();
    let mut current: Option<&dyn std::error::Error> = Some(e);
    while let Some(err) = current {
        if !chain.is_empty() {
            chain.push_str(" -> ");
        }
        chain.push_str(&err.to_string());
        current = err.source();
    }
    chain
}
use tachyon_core::safety::extract_filename;
use tachyon_core::traits::Protocol;
use tachyon_core::types::FileMetadata;
use tachyon_core::{ByteStream, DownloadError, DownloadResult};
use tracing::{debug, info, warn};

/// `get_text` 响应体最大允许字节数
///
/// 防止恶意或异常服务器返回超大响应导致 OOM。64MB 足以覆盖
/// 大型模型仓库(HuggingFace)的文件树 JSON 列表。
/// 与 [`tachyon_core::config::MAX_FULL_DOWNLOAD_SIZE`] 保持一致,
/// 统一三协议(Hub 元数据 API 同样使用 64MB)的 OOM 防护上限。
pub const MAX_GET_TEXT_SIZE: u64 = 64 * 1024 * 1024;

/// 校验响应体 Content-Length 是否超出上限
///
/// `content_length` 为 `None` 表示服务器未声明(如 chunked 流式),
/// 此时无法预检,返回 Ok 由 `.text()` 自然读取并受读超时约束。
pub fn check_response_size_limit(content_length: Option<u64>) -> DownloadResult<()> {
    if let Some(cl) = content_length
        && cl > MAX_GET_TEXT_SIZE
    {
        return Err(DownloadError::Protocol(format!(
            "响应体过大: {cl} > 最大允许 {MAX_GET_TEXT_SIZE} 字节"
        )));
    }
    Ok(())
}

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
        Self::build_client(connect_secs, read_secs, false, 16, 30)
    }

    /// 使用连接配置创建 HTTP 客户端(含 HTTP/2 控制与连接池调优)
    ///
    /// 将 `ConnectionConfig` 的 `max_connections_per_host` 和 `keep_alive_timeout_secs`
    /// 透传给 reqwest 连接池,使 reqwest 空闲连接池大小与信号量并发上限对齐。
    pub fn with_connection_config(
        config: &tachyon_core::config::ConnectionConfig,
        connect_secs: u64,
        read_secs: u64,
    ) -> DownloadResult<Self> {
        Self::build_client(
            connect_secs,
            read_secs,
            config.enable_http2,
            config.max_connections_per_host as usize,
            config.keep_alive_timeout_secs,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_client(
        connect_secs: u64,
        read_secs: u64,
        enable_http2: bool,
        pool_max_idle_per_host: usize,
        keep_alive_secs: u64,
    ) -> DownloadResult<Self> {
        let mut builder = Client::builder()
            .user_agent(tachyon_core::config::USER_AGENT)
            .pool_max_idle_per_host(pool_max_idle_per_host)
            .tcp_keepalive(std::time::Duration::from_secs(keep_alive_secs))
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

    /// 发送 GET 请求并返回响应文本
    ///
    /// # 参数
    /// - `url`: 请求 URL
    /// - `headers`: 自定义请求头(key-value 对),可为空
    ///
    /// # 返回
    /// 响应文本,或网络错误
    pub async fn get_text(&self, url: &str, headers: &[(&str, &str)]) -> DownloadResult<String> {
        let parsed_url = reqwest::Url::parse(url)?;
        tachyon_core::validate_public_http_url(&parsed_url)?;

        let mut req = self.client.get(url);
        for (key, value) in headers {
            req = req.header(*key, *value);
        }

        let mut response = req.send().await.map_err(|e| {
            let chain = error_chain(&e);
            DownloadError::Network(format!("GET 请求失败: {chain}"))
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(classify_http_error(status, response.headers()));
        }

        // 校验响应体大小,防止超大响应导致 OOM(复用统一的字节上限逻辑)
        check_response_size_limit(response.content_length())?;

        // S-14: 使用 chunk() 流式读取,对无 Content-Length 的 chunked 响应
        // 也能在累积大小超限时及时终止,而非等到读超时或 OOM
        let mut body = String::new();
        let mut total_size = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| DownloadError::Network(format!("读取响应块失败: {e}")))?
        {
            total_size += chunk.len() as u64;
            if total_size > MAX_GET_TEXT_SIZE {
                return Err(DownloadError::Protocol(format!(
                    "响应体过大: {total_size} > 最大允许 {MAX_GET_TEXT_SIZE} 字节"
                )));
            }
            // chunk 是 Bytes (UTF-8 边界安全),直接 extend
            let chunk_str = String::from_utf8_lossy(&chunk);
            body.push_str(&chunk_str);
        }
        Ok(body)
    }
}

// Default 实现已移除 — TLS 初始化可能失败,请使用 HttpClient::new()

const DNS_CACHE_TTL_SECS: u64 = 60;
/// DNS 缓存最大条目数,防止 DashMap 无限增长导致内存泄漏
const DNS_CACHE_MAX_ENTRIES: usize = 10_000;

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

    /// 清理过期的 DNS 缓存条目
    /// 清理过期 DNS 缓存条目
    ///
    /// N-03: 在每次 DNS 解析前调用,防止缓存无限增长。
    /// 使用概率式清理(每 100 次解析触发一次),避免每次都遍历全表。
    fn maybe_evict_expired(&self) {
        // 每约 100 次解析触发一次清理,避免频繁遍历
        if self.cache.len().is_multiple_of(100) {
            let ttl = Duration::from_secs(DNS_CACHE_TTL_SECS);
            self.cache.retain(|_, (_, ts)| ts.elapsed() < ttl);
        }
    }
}

impl reqwest::dns::Resolve for PublicDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let cache = self.cache.clone();

        // N-03: 定期清理过期 DNS 缓存条目
        self.maybe_evict_expired();

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

            // 容量检查: 达到上限时先清理过期条目,仍满则拒绝缓存(仍返回解析结果)
            if cache.len() >= DNS_CACHE_MAX_ENTRIES {
                // 借用 self 不可用(已 move 进闭包),通过 cache 引用操作
                let ttl = Duration::from_secs(DNS_CACHE_TTL_SECS);
                cache.retain(|_, (_, ts)| ts.elapsed() < ttl);
            }
            if cache.len() < DNS_CACHE_MAX_ENTRIES {
                cache.insert(host, (addrs.clone(), Instant::now()));
            }
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
/// - 429/503: 返回 Throttled,尝试解析 Retry-After 头中的秒数(整数或 HTTP-date)
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
                .and_then(parse_retry_after);
            DownloadError::Throttled { retry_after_secs }
        }
        401 | 403 => DownloadError::Forbidden { status: code },
        _ => DownloadError::Protocol(format!("HTTP {status}")),
    }
}

/// 解析 Retry-After 头部值
///
/// 支持两种格式(RFC 7231):
/// 1. delay-seconds: 纯整数秒数(如 "120")
/// 2. HTTP-date: IMF-fixdate 格式(如 "Wed, 21 Oct 2026 07:28:00 GMT")
pub(crate) fn parse_retry_after(value: &str) -> Option<u64> {
    // 优先尝试整数秒格式
    if let Ok(secs) = value.trim().parse::<u64>() {
        return Some(secs);
    }

    // W-10: 解析 HTTP-date (IMF-fixdate) 格式
    // 格式: "Day, DD Mon YYYY HH:MM:SS GMT"
    parse_http_date_to_secs(value.trim())
}

/// 月份名称映射
fn month_num(name: &str) -> Option<u32> {
    match name {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

/// 将 IMF-fixdate 格式的 HTTP-date 转换为距当前时刻的秒数
///
/// 输入: "Wed, 21 Oct 2026 07:28:00 GMT"
/// 输出: 距离当前 UTC 时间的秒数(若已过期则返回 Some(0))
pub(crate) fn parse_http_date_to_secs(date_str: &str) -> Option<u64> {
    // 解析 "Day, DD Mon YYYY HH:MM:SS GMT"
    let parts: Vec<&str> = date_str.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }

    // parts[0] = "Wed," (忽略星期)
    let day: u32 = parts.get(1)?.parse().ok()?;
    let month = month_num(parts.get(2)?)?;
    let year: u32 = parts.get(3)?.parse().ok()?;
    let time_str = parts.get(4)?;
    // parts[5] = "GMT" (忽略时区,假定为 UTC)

    let time_parts: Vec<&str> = time_str.split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let minute: u32 = time_parts[1].parse().ok()?;
    let second: u32 = time_parts[2].parse().ok()?;

    // 计算从 epoch 到目标时间的秒数(简化算法,精确到天级别即可)
    let target_epoch = days_from_epoch(year, month, day) as u64 * 86400
        + hour as u64 * 3600
        + minute as u64 * 60
        + second as u64;

    // 获取当前 UNIX 时间戳
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    Some(target_epoch.saturating_sub(now_epoch))
}

/// 计算从 1970-01-01 到指定日期的天数(简化算法)
fn days_from_epoch(year: u32, month: u32, day: u32) -> i64 {
    // 使用 civil_from_days 的反向算法
    // 参考: http://howardhinnant.github.io/date_algorithms.html
    let y = if month <= 2 {
        year as i64 - 1
    } else {
        year as i64
    };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // year of era [0, 399]
    let doy = (153 * m as i64 + 2) / 5 + day as i64 - 1; // day of year [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day of era [0, 146096]
    era * 146097 + doe - 719468
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
                let chain = error_chain(&e);
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
                    let chain = error_chain(&e);
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
                    let chain = error_chain(&e);
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
                    let chain = error_chain(&e);
                    warn!(url = %tachyon_core::redact_url_for_log(&url), error = %e, error_chain = %chain, "整块下载请求连接失败");
                    DownloadError::Network(format!("下载请求失败: {chain}"))
                })?;

            let status = response.status();
            if !status.is_success() {
                return Err(classify_http_error(status, response.headers()));
            }

            // 限制非流式响应大小，防止 OOM
            if let Some(content_length) = response.content_length()
                && content_length > tachyon_core::config::MAX_FULL_DOWNLOAD_SIZE as u64
            {
                return Err(DownloadError::Protocol(format!(
                    "响应体过大: {} > 最大允许 {} 字节",
                    content_length,
                    tachyon_core::config::MAX_FULL_DOWNLOAD_SIZE
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
                    let chain = error_chain(&e);
                    warn!(url = %tachyon_core::redact_url_for_log(&url), error = %e, error_chain = %chain, "整块下载请求连接失败");
                    DownloadError::Network(format!("下载请求失败: {chain}"))
                })?;

            let status = response.status();
            if !status.is_success() {
                return Err(classify_http_error(status, response.headers()));
            }

            info!(url = %tachyon_core::redact_url_for_log(&url), "HTTP 整块流式响应头已接收,开始流式传输");

            // 使用 bytes_stream() 逐块产出,峰值内存仅含单个 chunk,
            // 避免大文件整块进内存。scan 累计字节数,超过上限时终止流。
            let max_bytes = tachyon_core::config::MAX_FULL_DOWNLOAD_SIZE as u64;
            let stream = response
                .bytes_stream()
                .scan(0u64, move |total, result| match result {
                    Ok(chunk) => {
                        *total += chunk.len() as u64;
                        if *total > max_bytes {
                            futures::future::ready(Some(Err(DownloadError::Protocol(format!(
                                "HTTP 流式下载超过大小上限: {total} > {max_bytes} 字节"
                            )))))
                        } else {
                            futures::future::ready(Some(Ok(chunk)))
                        }
                    }
                    Err(e) => futures::future::ready(Some(Err(DownloadError::Network(format!(
                        "读取响应流数据失败: {e}"
                    ))))),
                });

            Ok(Box::pin(stream) as ByteStream)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachyon_core::safety::parse_content_disposition;

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

    // --- get_text 响应体大小限制测试 (P2-42) ---

    #[test]
    fn test_max_get_text_size_constant_is_64mb() {
        // 64MB 上限足以覆盖 Hub 文件树 JSON,同时防止 OOM
        assert_eq!(MAX_GET_TEXT_SIZE, 64 * 1024 * 1024);
    }

    #[test]
    fn test_check_response_size_limit_allows_none() {
        // Content-Length 缺失(chunked 流式)时无法预检,应放行
        assert!(check_response_size_limit(None).is_ok());
    }

    #[test]
    fn test_check_response_size_limit_allows_under_limit() {
        // 恰好等于上限:边界值应允许
        assert!(check_response_size_limit(Some(MAX_GET_TEXT_SIZE)).is_ok());
        assert!(check_response_size_limit(Some(0)).is_ok());
        assert!(check_response_size_limit(Some(1024)).is_ok());
    }

    #[test]
    fn test_check_response_size_limit_rejects_over_limit() {
        // 超出上限一字节:应返回 Protocol 错误
        let result = check_response_size_limit(Some(MAX_GET_TEXT_SIZE + 1));
        assert!(result.is_err());
        match result.unwrap_err() {
            DownloadError::Protocol(msg) => {
                assert!(
                    msg.contains("响应体过大"),
                    "错误消息应含'响应体过大': {msg}"
                );
                assert!(
                    msg.contains(&MAX_GET_TEXT_SIZE.to_string()),
                    "错误消息应含上限值: {msg}"
                );
            }
            other => panic!("预期 Protocol 错误,实际: {other:?}"),
        }
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
        // W-10 后 HTTP-date 格式已被支持，应返回距当前时刻的正整数秒数
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        let err = super::classify_http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, &headers);
        match err {
            DownloadError::Throttled { retry_after_secs } => {
                assert!(
                    retry_after_secs.is_some_and(|s| s > 0),
                    "HTTP-date 在未来时应返回正秒数, 实际: {retry_after_secs:?}"
                );
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
