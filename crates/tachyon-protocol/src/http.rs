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
            .pool_idle_timeout(std::time::Duration::from_secs(keep_alive_secs))
            .tcp_keepalive(std::time::Duration::from_secs(keep_alive_secs))
            .tcp_nodelay(true) // 禁用 Nagle 算法:减少小包延迟
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
            builder = builder
                .http2_adaptive_window(true)
                // 初始流窗口 1MB:高 BDP 网络下避免流级饥饿
                // (默认 64KB 在 100Mbps×50ms RTT 下成为瓶颈)
                .http2_initial_stream_window_size(1024 * 1024)
                // 初始连接窗口 16MB:聚合多流吞吐
                .http2_initial_connection_window_size(16 * 1024 * 1024)
                // 最大帧 1MB:减少大载荷的帧切分开销 (默认 16KB)
                .http2_max_frame_size(1 << 20)
                // HTTP/2 PING 保活:检测 NAT/代理超时的死连接
                .http2_keep_alive_interval(std::time::Duration::from_secs(30))
                .http2_keep_alive_timeout(std::time::Duration::from_secs(10));
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

/// 解析 `Content-Range` 头中的文件总字节数
///
/// 支持 RFC 7233 规范格式 `bytes 0-0/<total>` 与未定大小 `bytes 0-0/*`。
/// 仅在 `<total>` 为具体数值时返回 `Some`;`*` 或解析失败返回 `None`。
fn parse_content_range_total(value: &str) -> Option<u64> {
    let value = value.trim();
    let after = value.strip_prefix("bytes")?.trim_start();
    let after = after.strip_prefix("0-0")?.trim_start();
    let total = after.strip_prefix('/')?.trim();
    (total != "*").then(|| total.parse::<u64>().ok()).flatten()
}

/// 判断 HEAD 失败状态码是否应回退到 GET 探测
///
/// 判断响应是否为 HTML 页面(而非可下载文件)
///
/// 用户经常误把网页 URL(如管理后台、商品详情页)当作下载链接粘贴。
/// 此时服务端返回 `Content-Type: text/html` 或 `application/xhtml+xml`,
/// 应在探测阶段直接拒绝,避免后续把 HTML 当文件下载浪费带宽和存储。
///
/// 没有 Content-Type 时返回 `false`(无法判定即放行,不在此处误伤)。
fn is_html_response(headers: &reqwest::header::HeaderMap) -> bool {
    let Some(value) = headers.get("content-type").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    // 只比较 MIME 主类型,忽略 charset 等参数;大小写不敏感
    let mime = value
        .split(';')
        .next()
        .map(|s| s.trim())
        .unwrap_or("")
        .to_ascii_lowercase();
    mime == "text/html" || mime == "application/xhtml+xml"
}

/// 格式化 probe 阶段的 URL 上下文,用于错误消息和日志
///
/// 当请求 URL 与最终 URL 不同(发生重定向)时,以 `原始 -> 最终` 形式展示;
/// 否则只显示一份。两端均经 `redact_url_for_log` 脱敏,避免签名/Token 泄漏到日志。
///
/// 用户场景:粘贴管理后台 URL 后被服务端 301 到登录页 / 错误页。错误消息
/// 仅显示最终 404 时,用户无法判断"为什么我贴的是 A 却变成 B"。
fn format_probe_url_context(request_url: &str, final_url: &str) -> String {
    let req = redact_url_keep_path(request_url);
    if request_url == final_url {
        return req;
    }
    let dest = redact_url_keep_path(final_url);
    if req == dest {
        // 脱敏后相同(如仅 query 不同),退化为单 URL,避免冗余
        return req;
    }
    format!("{req} -> {dest}")
}

/// 脱敏 URL 用于错误消息,保留完整路径(query/fragment/userinfo 被丢弃)
///
/// 与 `tachyon_core::redact_url_for_log` 的区别:后者只保留 basename,适合
/// 简短日志;此函数保留完整路径,适合错误诊断,让用户看到重定向前后的路径差异。
fn redact_url_keep_path(url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return "<invalid-url>".to_string();
    };
    let Some(host) = parsed.host_str() else {
        return "<invalid-url>".to_string();
    };
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    format!("{}://{host}{port}{}", parsed.scheme(), parsed.path())
}

/// 判断 HEAD 失败状态码是否应回退到 GET 探测
///
/// 部分签名 CDN（如字节跳动 bytetos）仅对 GET 方法签名,HEAD 被拒成 403/405,
/// 401 同理。429 限流时回退也无害(可能 GET 不被限流)。
/// 5xx 一律回退:CDN 对 HEAD 返回非标准 5xx(如 wo.cn 的 519、Cloudflare 的
/// 520/521 等)本质是方法被拒,GET 请求有望成功;标准 5xx(500/502/503)同理,
/// 服务端可能对 HEAD 和 GET 有不同处理逻辑。
/// 其余 4xx(400/404/410 等)表示客户端请求有误,回退无意义,不回退。
fn head_status_should_fallback(status: reqwest::StatusCode) -> bool {
    let code = status.as_u16();
    // 5xx 一律回退:CDN 对 HEAD 返回非标准 5xx(519/520/521 等)本质是方法被拒
    if code >= 500 {
        return true;
    }
    // 4xx 中仅对 HEAD 方法被拒或限流的码回退
    matches!(code, 401 | 403 | 405 | 429)
}

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

/// 分类 HTTP 错误,并附带请求 URL 上下文,用于诊断
///
/// 与 `classify_http_error` 行为一致:
/// - 429/503 仍归类为 `Throttled`,URL 上下文不影响重试语义(避免破坏外层重试逻辑)
/// - 401/403 仍归类为 `Forbidden`,URL 不进入消息体(类型字段足够区分)
/// - 其他状态码进入 `Protocol`,消息中带上 URL 上下文,帮助用户定位问题
fn classify_http_error_with_context(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    url_context: &str,
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
        _ => DownloadError::Protocol(format!("{url_context} 返回 HTTP {status}")),
    }
}

/// `evaluate_head_response` 的非成功结果
///
/// 拆出 `NeedFallback` 让调用方决定是否启动 GET Range 回退;`Final` 表示
/// 终态错误,直接向上传播。这种结构允许把判定逻辑(纯)与 IO(异步)分离。
#[derive(Debug)]
enum HeadEvalError {
    /// HEAD 被签名 CDN 拒绝等场景,需要回退到 GET Range:0-0 探测
    NeedFallback,
    /// 终态错误,直接向上传播
    Final(DownloadError),
}

/// 把 HEAD 响应判定为元数据 / 回退信号 / 终态错误
///
/// 这是 probe() 的核心语义,从 reqwest IO 中抽出后可纯函数测试。
///
/// 决策顺序:
/// 1. 状态码非 2xx → `head_status_should_fallback` 决定是回退还是分类错误
/// 2. 状态码 2xx 但 `Content-Type` 是 HTML → 终态错误,告知用户该链接不是文件
/// 3. 否则 → 提取元数据
///
/// 所有错误消息均附带 URL 上下文(若发生过重定向,以 `原始 -> 最终` 形式展示)。
fn evaluate_head_response(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    request_url: &str,
    final_url: &str,
) -> Result<FileMetadata, HeadEvalError> {
    let url_context = format_probe_url_context(request_url, final_url);

    if !status.is_success() {
        return if head_status_should_fallback(status) {
            Err(HeadEvalError::NeedFallback)
        } else {
            Err(HeadEvalError::Final(classify_http_error_with_context(
                status,
                headers,
                &url_context,
            )))
        };
    }

    // 2xx 成功但响应是 HTML 页面 → 用户粘错链接,直接终态拒绝
    if is_html_response(headers) {
        return Err(HeadEvalError::Final(DownloadError::Protocol(format!(
            "{url_context} 返回 HTML 页面而非可下载文件,请确认链接是否正确"
        ))));
    }

    // 提取元数据时使用 final_url(重定向后的真实地址),与文件名解析保持一致
    Ok(metadata_from_headers(final_url, headers, false))
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

/// 从响应头提取文件元数据
///
/// `range_response = true` 表示响应来自 `GET Range: bytes=0-0`(206 Partial Content):
/// 文件大小取自 `Content-Range: bytes 0-0/<total>`,`supports_range` 恒为 true(服务端
/// 已确认支持 Range)。`range_response = false` 表示 HEAD/200:`file_size` 取自
/// `Content-Length`,`supports_range` 取自 `Accept-Ranges` 头。
fn metadata_from_headers(
    url: &str,
    headers: &reqwest::header::HeaderMap,
    range_response: bool,
) -> FileMetadata {
    let content_disposition = headers
        .get("content-disposition")
        .and_then(|v| v.to_str().ok());
    let file_name = extract_filename(url, content_disposition);
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    let etag = headers
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    let last_modified = headers
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());

    let (file_size, supports_range) = if range_response {
        let total = headers
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_total);
        (total, true)
    } else {
        let size = headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let supports = headers
            .get("accept-ranges")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("bytes"))
            .unwrap_or(false);
        (size, supports)
    };

    FileMetadata {
        file_name,
        file_size,
        content_type,
        supports_range,
        etag,
        last_modified,
        file_layout: None,
    }
}

/// HEAD 被签名 CDN 拒绝后,用 GET + Range:0-0 回退探测元数据
///
/// - 206 Partial Content:从 `Content-Range` 取总大小,`supports_range = true`
/// - 200:服务端忽略 Range,返回完整文件;仅取响应头(`Content-Length` 即总大小),
///   不消费响应体,`supports_range` 取自 `Accept-Ranges`(通常为 false)
/// - 其他错误:返回 GET 的真实错误分类(不再回退),错误消息含 URL 上下文
async fn probe_via_get_range(client: &Client, request_url: &str) -> DownloadResult<FileMetadata> {
    debug!(url = %tachyon_core::redact_url_for_log(request_url), "GET Range 探测开始");
    let response = client
        .get(request_url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .map_err(|e| {
            let chain = error_chain(&e);
            warn!(url = %tachyon_core::redact_url_for_log(request_url), error = %e, error_chain = %chain, "GET Range 探测连接失败");
            DownloadError::Network(format!("GET Range 探测失败: {chain}"))
        })?;

    let final_url = response.url().as_str().to_owned();
    let url_context = format_probe_url_context(request_url, &final_url);
    let status = response.status();
    if status == reqwest::StatusCode::PARTIAL_CONTENT {
        let metadata = metadata_from_headers(&final_url, response.headers(), true);
        info!(
            url = %tachyon_core::redact_url_for_log(&final_url),
            file_size = ?metadata.file_size,
            supports_range = metadata.supports_range,
            "GET Range 探测完成(206)"
        );
        return Ok(metadata);
    }
    if status.is_success() {
        // 200:服务端忽略 Range。仅取头,不消费响应体。
        // 同样需要识别 HTML 页面,避免把网页当文件下载。
        if is_html_response(response.headers()) {
            warn!(
                url = %tachyon_core::redact_url_for_log(&final_url),
                "GET Range 探测返回 HTML 页面,拒绝下载"
            );
            return Err(DownloadError::Protocol(format!(
                "{url_context} 返回 HTML 页面而非可下载文件,请确认链接是否正确"
            )));
        }
        let metadata = metadata_from_headers(&final_url, response.headers(), false);
        info!(
            url = %tachyon_core::redact_url_for_log(&final_url),
            file_size = ?metadata.file_size,
            supports_range = metadata.supports_range,
            "GET Range 探测完成(200,服务端忽略 Range)"
        );
        return Ok(metadata);
    }
    warn!(url = %tachyon_core::redact_url_for_log(&final_url), status = %status, "GET Range 探测返回非成功状态码");
    Err(classify_http_error_with_context(
        status,
        response.headers(),
        &url_context,
    ))
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
            // HEAD 请求:签名 CDN(如 bytetos)可能拒绝 HEAD 方法导致 403/405,
            // 也可能直接超时断连(签名 URL 对 HEAD 的连接策略不同于 GET)。
            // 两种场景均回退到 GET + Range:0-0 探测。
            let response = match client.head(&url).send().await {
                Ok(resp) => resp,
                Err(e) => {
                    let chain = error_chain(&e);
                    warn!(
                        url = %tachyon_core::redact_url_for_log(&url),
                        error = %e, error_chain = %chain,
                        "HEAD 请求失败,回退 GET Range 探测"
                    );
                    return probe_via_get_range(&client, &url).await;
                }
            };

            let final_url = response.url().as_str().to_owned();
            let status = response.status();
            // reqwest 自动跟随 301/302 重定向,这里 final_url 可能与原始 url 不同。
            // 把判定逻辑从 IO 中抽离到 `evaluate_head_response`,便于纯函数测试。
            match evaluate_head_response(status, response.headers(), &url, &final_url) {
                Ok(metadata) => {
                    info!(
                        url = %tachyon_core::redact_url_for_log(&final_url),
                        file_size = ?metadata.file_size,
                        supports_range = metadata.supports_range,
                        content_type = ?metadata.content_type,
                        "HTTP HEAD 探测完成"
                    );
                    Ok(metadata)
                }
                Err(HeadEvalError::NeedFallback) => {
                    warn!(
                        url = %tachyon_core::redact_url_for_log(&final_url),
                        status = %status,
                        "HEAD 探测被拒,回退 GET Range 探测"
                    );
                    probe_via_get_range(&client, &url).await
                }
                Err(HeadEvalError::Final(err)) => {
                    warn!(
                        url = %tachyon_core::redact_url_for_log(&final_url),
                        status = %status,
                        redirected = url != final_url,
                        error = %err,
                        "HEAD 探测终态错误"
                    );
                    Err(err)
                }
            }
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
            // 避免大文件整块进内存。流式下载本身不会 OOM,大小上限由引擎层
            // 的 `max_full_stream_bytes` 控制(未知大小时),因此协议层不再额外限制。
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

    // --- HTML 响应识别 ---
    // 行为:用户粘贴管理后台 URL(如 console.volcengine.com/...)时,
    // 服务端返回 200 + text/html。此时不应当成可下载文件继续下载,
    // 而是返回带说明的 Protocol 错误,告知用户该链接不是文件。

    #[test]
    fn test_is_html_response_with_text_html() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "text/html".parse().unwrap());
        assert!(super::is_html_response(&headers));
    }

    #[test]
    fn test_is_html_response_with_text_html_charset() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
        assert!(super::is_html_response(&headers));
    }

    #[test]
    fn test_is_html_response_with_xhtml() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "application/xhtml+xml".parse().unwrap());
        assert!(super::is_html_response(&headers));
    }

    #[test]
    fn test_is_html_response_with_octet_stream_is_false() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "application/octet-stream".parse().unwrap());
        assert!(!super::is_html_response(&headers));
    }

    #[test]
    fn test_is_html_response_with_video_is_false() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "video/mp4".parse().unwrap());
        assert!(!super::is_html_response(&headers));
    }

    #[test]
    fn test_is_html_response_with_no_content_type_is_false() {
        // 没有 Content-Type 头时,无法判定为 HTML,默认放行(纵深防御:
        // 后续步骤仍可能识别问题)
        let headers = reqwest::header::HeaderMap::new();
        assert!(!super::is_html_response(&headers));
    }

    #[test]
    fn test_is_html_response_case_insensitive() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "TEXT/HTML".parse().unwrap());
        assert!(super::is_html_response(&headers));
    }

    // --- URL 上下文格式化(支持重定向链路) ---
    // 行为:错误消息和日志需要同时显示用户提交的原始 URL 与请求最终落点。
    // 当原始 URL == 最终 URL 时,只显示一份;当不同时,以 "原始 -> 最终" 形式展示,
    // 帮助用户判断"我的链接被重定向到哪里"。

    #[test]
    fn test_format_probe_url_context_no_redirect() {
        // 没有重定向:request_url 与 final_url 相同,只显示一份
        let ctx = super::format_probe_url_context(
            "https://cdn.example.com/file.bin",
            "https://cdn.example.com/file.bin",
        );
        assert_eq!(ctx, "https://cdn.example.com/file.bin");
    }

    #[test]
    fn test_format_probe_url_context_with_redirect() {
        // 发生过重定向:显示 "原始 -> 最终",中间用箭头清晰区分
        let ctx = super::format_probe_url_context(
            "https://console.example.com/page?a=1",
            "https://console.example.com/login",
        );
        assert!(
            ctx.contains("console.example.com/page"),
            "应包含原始 URL host+path: {ctx}"
        );
        assert!(
            ctx.contains("console.example.com/login"),
            "应包含最终 URL host+path: {ctx}"
        );
        assert!(ctx.contains("->"), "应使用箭头分隔: {ctx}");
    }

    #[test]
    fn test_format_probe_url_context_redacts_sensitive_query() {
        // 带签名/Token 的 URL 应被脱敏(不让密钥泄进日志/错误链)
        let ctx = super::format_probe_url_context(
            "https://cdn.example.com/file.bin?Signature=abc&Token=xyz",
            "https://cdn.example.com/file.bin?Signature=abc&Token=xyz",
        );
        assert!(
            !ctx.contains("Signature=abc"),
            "Signature 不应明文出现: {ctx}"
        );
        assert!(!ctx.contains("Token=xyz"), "Token 不应明文出现: {ctx}");
    }

    // --- classify_http_error 携带 URL 上下文 ---
    // 行为:用户原本只看到 "协议错误: HTTP 404 Not Found",无法定位是哪个链接、
    // 是否经过重定向。带上下文版本应将 URL 注入错误消息。

    #[test]
    fn test_classify_404_with_context_includes_url() {
        let headers = reqwest::header::HeaderMap::new();
        let err = super::classify_http_error_with_context(
            reqwest::StatusCode::NOT_FOUND,
            &headers,
            "https://console.example.com/page",
        );
        match err {
            DownloadError::Protocol(msg) => {
                assert!(msg.contains("404"), "应保留状态码: {msg}");
                assert!(
                    msg.contains("https://console.example.com/page"),
                    "应包含 URL 上下文: {msg}"
                );
            }
            other => panic!("预期 Protocol,实际: {other:?}"),
        }
    }

    #[test]
    fn test_classify_with_context_redirect_chain_in_message() {
        // 包含 "->" 的上下文(重定向场景)应原样进入错误消息
        let headers = reqwest::header::HeaderMap::new();
        let ctx = super::format_probe_url_context(
            "https://a.example.com/orig",
            "https://b.example.com/dest",
        );
        let err =
            super::classify_http_error_with_context(reqwest::StatusCode::NOT_FOUND, &headers, &ctx);
        match err {
            DownloadError::Protocol(msg) => {
                assert!(msg.contains("->"), "应保留重定向箭头: {msg}");
                assert!(msg.contains("orig"), "应包含原始路径: {msg}");
                assert!(msg.contains("dest"), "应包含最终路径: {msg}");
            }
            other => panic!("预期 Protocol,实际: {other:?}"),
        }
    }

    #[test]
    fn test_classify_with_context_throttled_unaffected() {
        // 429/503 仍走 Throttled 分支,URL 上下文不影响重试语义
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "30".parse().unwrap());
        let err = super::classify_http_error_with_context(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            &headers,
            "https://cdn.example.com/file.bin",
        );
        match err {
            DownloadError::Throttled { retry_after_secs } => {
                assert_eq!(retry_after_secs, Some(30));
            }
            other => panic!("预期 Throttled,实际: {other:?}"),
        }
    }

    // --- HEAD 回退 GET 探测:Content-Range 解析 ---

    #[test]
    fn test_parse_content_range_total_with_size() {
        assert_eq!(
            super::parse_content_range_total("bytes 0-0/12345"),
            Some(12345)
        );
    }

    #[test]
    fn test_parse_content_range_total_unknown() {
        assert_eq!(super::parse_content_range_total("bytes 0-0/*"), None);
    }

    #[test]
    fn test_parse_content_range_total_malformed() {
        assert_eq!(super::parse_content_range_total("not-a-range"), None);
        assert_eq!(super::parse_content_range_total("bytes 0-0/"), None);
    }

    // --- HEAD 回退 GET 探测:回退判定 ---

    #[test]
    fn test_head_should_fallback_on_signed_cdn_codes() {
        use reqwest::StatusCode;
        assert!(super::head_status_should_fallback(StatusCode::FORBIDDEN));
        assert!(super::head_status_should_fallback(StatusCode::UNAUTHORIZED));
        assert!(super::head_status_should_fallback(
            StatusCode::METHOD_NOT_ALLOWED
        ));
    }

    #[test]
    fn test_head_should_not_fallback_on_real_errors() {
        use reqwest::StatusCode;
        // 404 表示资源不存在,回退 GET 也找不到
        assert!(!super::head_status_should_fallback(StatusCode::NOT_FOUND));
    }

    #[test]
    fn test_head_should_fallback_on_non_standard_5xx() {
        use reqwest::StatusCode;
        // wo.cn CDN 对 HEAD 返回 519,Cloudflare 返回 520/521/522 等
        assert!(super::head_status_should_fallback(
            StatusCode::from_u16(519).unwrap()
        ));
        assert!(super::head_status_should_fallback(
            StatusCode::from_u16(520).unwrap()
        ));
        assert!(super::head_status_should_fallback(
            StatusCode::from_u16(521).unwrap()
        ));
        // 标准 5xx 也应回退(如 500/502/503,CDN 可能对 HEAD 返回这些码)
        assert!(super::head_status_should_fallback(
            StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(super::head_status_should_fallback(StatusCode::BAD_GATEWAY));
        assert!(super::head_status_should_fallback(
            StatusCode::SERVICE_UNAVAILABLE
        ));
    }

    #[test]
    fn test_head_should_fallback_on_429_throttled() {
        use reqwest::StatusCode;
        assert!(super::head_status_should_fallback(
            StatusCode::TOO_MANY_REQUESTS
        ));
    }

    #[test]
    fn test_head_should_not_fallback_on_4xx_client_errors() {
        use reqwest::StatusCode;
        assert!(!super::head_status_should_fallback(StatusCode::BAD_REQUEST));
        assert!(!super::head_status_should_fallback(StatusCode::NOT_FOUND));
        assert!(!super::head_status_should_fallback(StatusCode::GONE));
    }

    // --- HEAD 回退 GET 探测:从响应头提取元数据 ---

    #[test]
    fn test_metadata_from_range_response_with_total() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-range", "bytes 0-0/5000".parse().unwrap());
        headers.insert("content-type", "video/mp4".parse().unwrap());
        let meta = super::metadata_from_headers(
            "https://cdn.example.com/path/video.mp4?a=1",
            &headers,
            true,
        );
        assert_eq!(meta.file_size, Some(5000));
        assert!(meta.supports_range);
        assert_eq!(meta.content_type.as_deref(), Some("video/mp4"));
    }

    #[test]
    fn test_metadata_from_range_response_unknown_total() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-range", "bytes 0-0/*".parse().unwrap());
        let meta =
            super::metadata_from_headers("https://cdn.example.com/stream.mp4", &headers, true);
        assert!(meta.file_size.is_none());
        // 206 已确认支持 Range
        assert!(meta.supports_range);
    }

    #[test]
    fn test_metadata_from_head_uses_content_length_and_accept_ranges() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-length", "1024".parse().unwrap());
        headers.insert("accept-ranges", "bytes".parse().unwrap());
        let meta =
            super::metadata_from_headers("https://cdn.example.com/file.bin", &headers, false);
        assert_eq!(meta.file_size, Some(1024));
        assert!(meta.supports_range);
    }

    #[test]
    fn test_metadata_from_head_no_accept_ranges() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-length", "2048".parse().unwrap());
        let meta =
            super::metadata_from_headers("https://cdn.example.com/file.bin", &headers, false);
        assert_eq!(meta.file_size, Some(2048));
        assert!(!meta.supports_range);
    }

    // --- ConnectionPool 与 reqwest 连接池对齐测试 ---

    #[test]
    fn test_with_connection_config_creates_client() {
        // 验证 with_connection_config 能正确创建客户端
        let config = tachyon_core::config::ConnectionConfig::default();
        let client = HttpClient::with_connection_config(&config, 10, 30);
        assert!(client.is_ok(), "with_connection_config 应成功创建客户端");
    }

    #[test]
    fn test_with_connection_config_custom_keep_alive() {
        // 验证自定义 keep_alive_timeout_secs 不导致创建失败
        let config = tachyon_core::config::ConnectionConfig {
            keep_alive_timeout_secs: 60,
            ..Default::default()
        };
        let client = HttpClient::with_connection_config(&config, 10, 30);
        assert!(
            client.is_ok(),
            "自定义 keep_alive 应成功创建客户端(已对齐 pool_idle_timeout)"
        );
    }

    #[test]
    fn test_build_client_pool_idle_timeout_aligned_with_keep_alive() {
        // 验证 build_client 在 keep_alive_secs=60 时能正常创建
        // pool_idle_timeout 应与 keep_alive_secs 对齐,
        // 避免 reqwest 默认 90s idle timeout 与 semaphore 侧不一致
        let client = HttpClient::build_client(10, 30, false, 16, 60);
        assert!(
            client.is_ok(),
            "build_client(keep_alive=60) 应成功(已配置 pool_idle_timeout)"
        );
    }

    // --- evaluate_head_response: HEAD 响应判定的核心纯逻辑 ---
    // 行为:把 probe() 中"接到 HEAD 响应后做什么"的判定逻辑抽成纯函数,
    // 三种结果之一:
    //   1. 成功提取元数据 -> Ok(FileMetadata)
    //   2. 应回退 GET Range -> Err(NeedFallback)
    //   3. 终态错误 -> Err(Final(DownloadError))
    // 测试覆盖原日志案例的修复:HEAD 200 + HTML 应被识别为终态错误。

    #[test]
    fn test_evaluate_head_html_response_is_rejected() {
        // 用户粘贴控制台 URL 后被 200 + HTML 拒绝(原日志场景的回归测试)
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
        let result = super::evaluate_head_response(
            reqwest::StatusCode::OK,
            &headers,
            "https://console.example.com/page",
            "https://console.example.com/login",
        );
        match result {
            Err(super::HeadEvalError::Final(DownloadError::Protocol(msg))) => {
                assert!(
                    msg.contains("HTML") || msg.contains("html"),
                    "应说明这是 HTML 页面: {msg}"
                );
                assert!(
                    msg.contains("console.example.com"),
                    "应包含 URL 上下文: {msg}"
                );
            }
            other => panic!("预期 HTML 终态错误,实际: {other:?}"),
        }
    }

    #[test]
    fn test_evaluate_head_html_redirect_chain_in_message() {
        // 重定向 + HTML 双信号:错误消息应同时包含原始与最终 URL
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "text/html".parse().unwrap());
        let result = super::evaluate_head_response(
            reqwest::StatusCode::OK,
            &headers,
            "https://console.example.com/ark/subscription/coding-plan",
            "https://console.example.com/coding-plan",
        );
        match result {
            Err(super::HeadEvalError::Final(DownloadError::Protocol(msg))) => {
                assert!(msg.contains("->"), "应保留重定向箭头: {msg}");
                assert!(msg.contains("coding-plan"), "应含路径片段: {msg}");
            }
            other => panic!("预期 HTML 终态错误,实际: {other:?}"),
        }
    }

    #[test]
    fn test_evaluate_head_normal_2xx_returns_metadata() {
        // 正常 2xx + 二进制 Content-Type:返回元数据
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-length", "1024".parse().unwrap());
        headers.insert("content-type", "application/octet-stream".parse().unwrap());
        headers.insert("accept-ranges", "bytes".parse().unwrap());
        let result = super::evaluate_head_response(
            reqwest::StatusCode::OK,
            &headers,
            "https://cdn.example.com/file.bin",
            "https://cdn.example.com/file.bin",
        );
        match result {
            Ok(meta) => {
                assert_eq!(meta.file_size, Some(1024));
                assert!(meta.supports_range);
            }
            other => panic!("预期成功,实际: {other:?}"),
        }
    }

    #[test]
    fn test_evaluate_head_403_signals_fallback() {
        // 签名 CDN 的 HEAD 403:应返回 NeedFallback
        let headers = reqwest::header::HeaderMap::new();
        let result = super::evaluate_head_response(
            reqwest::StatusCode::FORBIDDEN,
            &headers,
            "https://cdn.example.com/signed.bin",
            "https://cdn.example.com/signed.bin",
        );
        assert!(matches!(result, Err(super::HeadEvalError::NeedFallback)));
    }

    #[test]
    fn test_evaluate_head_404_includes_url_in_error() {
        // 真 404:应进入终态错误并把 URL 上下文带上
        // 这是日志中报告的修复场景:用户原本只看到 "HTTP 404 Not Found"
        let headers = reqwest::header::HeaderMap::new();
        let result = super::evaluate_head_response(
            reqwest::StatusCode::NOT_FOUND,
            &headers,
            "https://console.example.com/ark/subscription/coding-plan",
            "https://console.example.com/coding-plan",
        );
        match result {
            Err(super::HeadEvalError::Final(DownloadError::Protocol(msg))) => {
                assert!(msg.contains("404"), "应保留状态码: {msg}");
                assert!(msg.contains("console.example.com"), "应包含 host: {msg}");
                assert!(msg.contains("->"), "重定向应在消息中体现: {msg}");
            }
            other => panic!("预期带 URL 的 Protocol 错误,实际: {other:?}"),
        }
    }
}
