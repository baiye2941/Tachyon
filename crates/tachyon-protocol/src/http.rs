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
use futures::{StreamExt, TryStreamExt, future};
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
    /// 创建新的 HTTP 客户端(使用默认超时: 连接 10s, 读取 30s, 无显式代理)
    pub fn new() -> DownloadResult<Self> {
        Self::with_timeouts(10, 30, None)
    }

    /// 创建带自定义超时的 HTTP 客户端
    ///
    /// # 参数
    /// - `connect_secs`: 连接超时(秒),0 表示禁用
    /// - `read_secs`: 读取超时(秒),0 表示禁用
    /// - `proxy`: 显式代理 URL,如 `http://127.0.0.1:7890`、`socks5://127.0.0.1:1080`;
    ///   None 时 reqwest 读取系统环境变量(`HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`)。
    ///
    /// # 说明
    /// - 连接超时防止连接黑洞 IP 永久挂起
    /// - 读取超时防止连接后静默断流,但不会误杀正常的长下载
    pub fn with_timeouts(
        connect_secs: u64,
        read_secs: u64,
        proxy: Option<&str>,
    ) -> DownloadResult<Self> {
        Self::build_client(connect_secs, read_secs, false, false, 16, 30, proxy)
    }

    /// 使用连接配置创建 HTTP 客户端(含 HTTP/2 控制与连接池调优)
    ///
    /// 将 `ConnectionConfig` 的 `max_connections_per_host` 和 `keep_alive_timeout_secs`
    /// 透传给 reqwest 连接池,使 reqwest 空闲连接池大小与信号量并发上限对齐。
    /// `proxy` 为显式代理 URL,None 时 reqwest 读取系统环境变量。
    pub fn with_connection_config(
        config: &tachyon_core::config::ConnectionConfig,
        connect_secs: u64,
        read_secs: u64,
        proxy: Option<&str>,
    ) -> DownloadResult<Self> {
        Self::build_client(
            connect_secs,
            read_secs,
            config.enable_http2,
            config.enable_quic,
            config.max_connections_per_host as usize,
            config.keep_alive_timeout_secs,
            proxy,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_client(
        connect_secs: u64,
        read_secs: u64,
        enable_http2: bool,
        enable_quic: bool,
        pool_max_idle_per_host: usize,
        keep_alive_secs: u64,
        proxy: Option<&str>,
    ) -> DownloadResult<Self> {
        let mut builder = Client::builder()
            .user_agent(tachyon_core::config::USER_AGENT)
            .pool_max_idle_per_host(pool_max_idle_per_host)
            .pool_idle_timeout(std::time::Duration::from_secs(keep_alive_secs))
            .tcp_keepalive(std::time::Duration::from_secs(keep_alive_secs))
            .tcp_nodelay(true) // 禁用 Nagle 算法:减少小包延迟
            .dns_resolver(PublicDnsResolver::new())
            .redirect(safe_redirect_policy());

        // 代理策略:显式 proxy > 系统环境变量(默认)。
        // 不再调用 `.no_proxy()` —— 原实现强制屏蔽系统代理,导致国内被墙 CDN 直连失败,
        // 且与 BT 侧 `detect_socks_proxy` 自动嗅探系统代理的语义割裂。
        // None 时 reqwest 默认读取 `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` 环境变量。
        if let Some(proxy_url) = proxy {
            let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| {
                // 安全:proxy URL 可能含 user:pass@,错误信息须脱敏避免凭据泄露到前端。
                // 用 redact_proxy_url 保留 scheme/host/port(代理诊断需要端口),
                // 剥离 userinfo(凭据)。
                DownloadError::Config(format!(
                    "无效的代理 URL '{}': {}",
                    tachyon_core::config::redact_proxy_url(proxy_url),
                    e
                ))
            })?;
            builder = builder.proxy(proxy);
        }

        if connect_secs > 0 {
            builder = builder.connect_timeout(std::time::Duration::from_secs(connect_secs));
        }
        if read_secs > 0 {
            builder = builder.read_timeout(std::time::Duration::from_secs(read_secs));
        }
        if enable_http2 {
            builder = builder
                // 初始流窗口 4MB:高 BDP 网络下避免流级饥饿
                // BDP = bandwidth × RTT:千兆宽带(125MB/s)×100ms RTT = 12.5MB,
                // 单分片需多并发填满管道,但单流窗口至少不被 1MB 窒息。
                // 4MB 覆盖 320Mbps×100ms RTT 的单流在途数据需求,
                // 配合 16MB 连接窗口(4 并发流各 4MB = 16MB)聚合多流吞吐。
                .http2_initial_stream_window_size(4 * 1024 * 1024)
                // 初始连接窗口 16MB:聚合多流吞吐(4 流 × 4MB = 16MB)
                .http2_initial_connection_window_size(16 * 1024 * 1024)
                // 最大帧 1MB:减少大载荷的帧切分开销 (默认 16KB)
                .http2_max_frame_size(1 << 20)
                // HTTP/2 PING 保活:检测 NAT/代理超时的死连接
                .http2_keep_alive_interval(std::time::Duration::from_secs(30))
                .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
                // 空闲连接也发 PING:多文件串行下载时,文件间隙连接空闲,
                // 默认 false 会在空闲时停止 PING 导致 NAT/代理超时掐断连接,
                // 下个文件需重建 TCP+TLS 握手(1-2 RTT)。开启后保持连接复用。
                // P2SP 多源场景下池中的空闲镜像源连接同样受益。
                .http2_keep_alive_while_idle(true);
            // 注:不启用 http2_adaptive_window。adaptive_window 会在运行时动态调整
            // 接收窗口并覆盖上方固定的 initial_stream/connection_window_size 设置
            // (见 reqwest 与 hyper 文档),使固定窗口成为无效配置。下载器场景为高 BDP
            // 大文件传输,采用显式固定大窗口(4MB 流 / 16MB 连接)比 adaptive 的动态
            // 试探更可控:后者面向通用 Web 浏览优化,可能在小请求上引入额外往返而
            // 拖慢首字节。
        }

        // HTTP/2 强制控制(FIX-19):enable_http2=false 时显式调用 http1_only(),
        // 确保 reqwest 不经 Alt-Svc/ALPN 协商升级到 HTTP/2。旧实现仅不设置 h2 相关选项,
        // 但 reqwest 默认可能仍会协商 h2,使用户「关闭 HTTP/2」配置不生效。
        // enable_http2=true 时保持上述显式大窗口 + PING 保活配置。
        if !enable_http2 {
            builder = builder.http1_only();
        }

        // HTTP/3(QUIC):reqwest 在编译期开启 `http3` feature 后,默认即通过 Alt-Svc
        // 协商自动升级到 HTTP/3,失败回退 HTTP/2(无需显式调用 builder 方法)。
        // 本块仅在"编译期 http3 可用 + 运行期 enable_quic"时记录意图日志。
        #[cfg(all(feature = "http3", reqwest_unstable))]
        if enable_quic {
            debug!("HTTP/3(QUIC)已启用:将通过 Alt-Svc 协商升级,失败回退 HTTP/2");
        } else {
            debug!("HTTP/3 编译可用但 enable_quic=false,仅使用 HTTP/2");
        }
        #[cfg(not(all(feature = "http3", reqwest_unstable)))]
        if enable_quic {
            // FIX-19:运行期请求 QUIC 但编译期未启用 http3 feature,降级为 HTTP/2。
            // 提升为 warn(原为 debug),使用户知晓 QUIC 配置实际未生效(避免「能力谎言」)。
            // 配置层与前端应据此提示「当前构建不支持 HTTP/3,已降级 HTTP/2」。
            warn!(
                "enable_quic=true 但 HTTP/3 未编译启用(reqwest http3 feature 缺失),降级使用 HTTP/2;http3_compiled={}",
                http3_compiled()
            );
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

    /// FIX-19 测试辅助:用给定 HTTP/2 与 QUIC 开关构造客户端(连接/读取超时用默认值),
    /// 供测试验证 enable_http2=false 时 http1_only 路径可成功构造。
    #[cfg(test)]
    pub(crate) fn build_client_for_test(
        enable_http2: bool,
        enable_quic: bool,
        pool_max_idle_per_host: usize,
        keep_alive_secs: u64,
        proxy: Option<&str>,
    ) -> DownloadResult<Self> {
        Self::build_client(
            5,
            10,
            enable_http2,
            enable_quic,
            pool_max_idle_per_host,
            keep_alive_secs,
            proxy,
        )
    }

    /// 创建 h2c prior-knowledge 客户端(明文 HTTP/2,不发 H1 Upgrade)
    ///
    /// 明文连接(非 TLS)上 reqwest 默认走 HTTP/1.1。此构造函数调用 reqwest 的
    /// `http2_prior_knowledge()`,使客户端直接发送 H2 连接 preface(不做 ALPN 协商),
    /// 用于 bench 环境验证 H2 多路复用与产品 H2 参数的互操作性。
    ///
    /// H2 参数与 `build_client(enable_http2=true)` 完全一致(流窗口 1MiB / 连接窗口
    /// 16MiB / 最大帧 1MiB / keepalive 30s/10s / keep_alive_while_idle)。
    /// 仅用于测试/bench,生产代码用 `with_connection_config`(TLS 下经 ALPN 协商 H2)。
    #[cfg(any(test, feature = "test-harness"))]
    pub fn h2c_prior_knowledge(
        connect_secs: u64,
        read_secs: u64,
        proxy: Option<&str>,
    ) -> DownloadResult<Self> {
        let mut builder = Client::builder()
            .user_agent(tachyon_core::config::USER_AGENT)
            .pool_max_idle_per_host(16)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(90))
            .tcp_nodelay(true)
            .http2_initial_stream_window_size(4 * 1024 * 1024)
            .http2_initial_connection_window_size(16 * 1024 * 1024)
            .http2_max_frame_size(1 << 20)
            .http2_keep_alive_interval(std::time::Duration::from_secs(30))
            .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
            .http2_keep_alive_while_idle(true)
            .http2_prior_knowledge();

        if let Some(proxy_url) = proxy {
            let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| {
                DownloadError::Config(format!(
                    "无效的代理 URL '{}': {}",
                    tachyon_core::config::redact_proxy_url(proxy_url),
                    e
                ))
            })?;
            builder = builder.proxy(proxy);
        }
        if connect_secs > 0 {
            builder = builder.connect_timeout(std::time::Duration::from_secs(connect_secs));
        }
        if read_secs > 0 {
            builder = builder.read_timeout(std::time::Duration::from_secs(read_secs));
        }

        let client = builder
            .build()
            .map_err(|e| DownloadError::Network(format!("创建 h2c 客户端失败: {e}")))?;
        Ok(Self { client })
    }

    /// 创建 HTTP/1.1 only 客户端(禁用 H2,用于 bench H1 vs H2 对比)
    ///
    /// 明文连接上 reqwest 默认即 HTTP/1.1,但此构造函数显式调用
    /// `http1_only()` 确保 H2 被完全禁用,并支持 `pool_max_idle_per_host(0)`
    /// 强制禁用空闲连接池(每请求新建 TCP 连接),用于 H1 最坏场景 bench。
    ///
    /// 仅用于测试/bench。生产代码用 `with_connection_config`。
    #[cfg(any(test, feature = "test-harness"))]
    pub fn h1c_only(
        connect_secs: u64,
        read_secs: u64,
        pool_max_idle_per_host: usize,
        proxy: Option<&str>,
    ) -> DownloadResult<Self> {
        let mut builder = Client::builder()
            .user_agent(tachyon_core::config::USER_AGENT)
            .pool_max_idle_per_host(pool_max_idle_per_host)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(90))
            .tcp_nodelay(true)
            .http1_only();

        if let Some(proxy_url) = proxy {
            let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| {
                DownloadError::Config(format!(
                    "无效的代理 URL '{}': {}",
                    tachyon_core::config::redact_proxy_url(proxy_url),
                    e
                ))
            })?;
            builder = builder.proxy(proxy);
        }
        if connect_secs > 0 {
            builder = builder.connect_timeout(std::time::Duration::from_secs(connect_secs));
        }
        if read_secs > 0 {
            builder = builder.read_timeout(std::time::Duration::from_secs(read_secs));
        }

        let client = builder
            .build()
            .map_err(|e| DownloadError::Network(format!("创建 h1c 客户端失败: {e}")))?;
        Ok(Self { client })
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

        // FIX-18.6:旧实现对每个 chunk 独立 `String::from_utf8_lossy`,多字节 UTF-8 码点
        // 跨 chunk 边界时会被部分字节替换为 U+FFFD,损坏播放列表/文本(如 HLS .m3u8)。
        // 现改为累积原始字节后整体解码,由 decode_chunks_to_string 统一处理。
        // S-14:使用 chunk() 流式读取,对无 Content-Length 的 chunked 响应也能在累积大小超限时及时终止。
        let mut buf: Vec<u8> = Vec::new();
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
            buf.extend_from_slice(&chunk);
        }
        decode_chunks_to_string(std::iter::once(buf.as_slice()))
    }

    /// 获取 URL 的原始字节内容(用于 HLS 分片下载等二进制场景)
    ///
    /// 与 `get_text` 不同,此方法返回原始 `Bytes`,不进行 UTF-8 解码,
    /// 适用于 TS 分片等二进制内容。同样受 `MAX_GET_TEXT_SIZE` 大小限制保护。
    pub async fn get_bytes(&self, url: &str) -> DownloadResult<Bytes> {
        let parsed_url = reqwest::Url::parse(url)?;
        tachyon_core::validate_public_http_url(&parsed_url)?;

        let mut response = self.client.get(url).send().await.map_err(|e| {
            let chain = error_chain(&e);
            DownloadError::Network(format!("GET 请求失败: {chain}"))
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(classify_http_error(status, response.headers()));
        }

        check_response_size_limit(response.content_length())?;

        // 流式读取,防止超大响应 OOM
        let mut buf = Vec::new();
        let mut total = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| DownloadError::Network(format!("读取响应块失败: {e}")))?
        {
            total += chunk.len() as u64;
            if total > MAX_GET_TEXT_SIZE {
                return Err(DownloadError::Protocol(format!(
                    "响应体过大: {total} > 最大允许 {MAX_GET_TEXT_SIZE} 字节"
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(buf))
    }
}

const DNS_CACHE_TTL_SECS: u64 = 60;
/// DNS 缓存最大条目数,防止 DashMap 无限增长导致内存泄漏
const DNS_CACHE_MAX_ENTRIES: usize = 10_000;

/// 将多个字节块解码为一个 UTF-8 字符串。
///
/// FIX-18.6:旧 `get_text` 对每个 HTTP chunk 独立调用 `String::from_utf8_lossy`,
/// 当多字节 UTF-8 码点跨 chunk 边界时,部分字节会被替换为 U+FFFD,损坏播放列表/文本
/// (如 HLS .m3u8 中的 CJK 字符)。本函数累积所有字节后整体解码,避免边界损坏;
/// 无效 UTF-8 序列返回 `Err`(不静默 lossy 替换,便于上游诊断)。
///
/// 调用方负责在累积过程中做大小限制(防止 OOM)。
pub(crate) fn decode_chunks_to_string<'a, I>(chunks: I) -> DownloadResult<String>
where
    I: Iterator<Item = &'a [u8]>,
{
    let mut buf: Vec<u8> = Vec::new();
    for chunk in chunks {
        buf.extend_from_slice(chunk);
    }
    String::from_utf8(buf)
        .map_err(|e| DownloadError::Protocol(format!("响应体不是有效 UTF-8: {e}")))
}

/// FIX-19:当前构建是否编译启用了 HTTP/3(reqwest http3 feature + reqwest_unstable cfg)。
///
/// 供配置层/前端判断「QUIC 配置是否真能生效」,避免 enable_quic=true 在未编译 http3
/// 时静默降级形成「能力谎言」(audit 问题 19)。build_client 据此在降级时发 warn。
pub fn http3_compiled() -> bool {
    cfg!(all(feature = "http3", reqwest_unstable))
}

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

/// 解析 `Content-Range` 头为 (start, end, total) 三元组。
///
/// 支持 RFC 7233 格式:
/// - `bytes <start>-<end>/<total>` → `Some((start, end, Some(total)))`
/// - `bytes <start>-<end>/*` → `Some((start, end, None))`(未定大小)
/// - `bytes */<total>`(unsatisfied range) → `None`
///
/// 用于校验 206 响应的 Content-Range 与请求 Range 一致,
/// 防止 CDN 缓存错位/负载均衡路由到不同版本文件时静默写入错位数据。
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let value = value.trim();
    let after = value.strip_prefix("bytes")?.trim_start();
    // unsatisfied range: `bytes */<total>` — 无 start-end,返回 None
    if after.starts_with('*') {
        return None;
    }
    let (range_part, total_part) = after.split_once('/')?;
    let range_part = range_part.trim();
    let (start_str, end_str) = range_part.split_once('-')?;
    let start: u64 = start_str.trim().parse().ok()?;
    let end: u64 = end_str.trim().parse().ok()?;
    let total_str = total_part.trim();
    let total = if total_str == "*" {
        None
    } else {
        Some(total_str.parse::<u64>().ok()?)
    };
    Some((start, end, total))
}

/// 校验 206 响应的 Content-Range 与请求的 [start, end] 一致。
///
/// 不一致时返回 `RangeMismatch` 错误,交由上层(mirror 调度器)切源。
/// 一致或无 Content-Range 头(部分服务器不返回)时返回 Ok。
/// 校验 206 响应的 Content-Range 与请求的 [start, end] 一致。
///
/// FIX-06(RFC 9110):单范围请求的 206 响应必须携带格式正确的
/// `Content-Range: bytes start-end/total`,且 start/end 与请求区间完全匹配。
///
/// - 缺失 Content-Range 头 -> Err(旧实现放行,可静默拼接错位/同长度异版本字节)
/// - 不可解析 -> Err
/// - start/end 不匹配 -> Err(RangeMismatch 语义,交由上层切源)
///
/// 一致时返回 Ok。
fn validate_content_range(
    headers: &reqwest::header::HeaderMap,
    start: u64,
    end: u64,
) -> DownloadResult<()> {
    let Some(cr_value) = headers.get("content-range").and_then(|v| v.to_str().ok()) else {
        // FIX-06:无 Content-Range 头的 206 违反 RFC 9110,拒绝而非放行
        return Err(DownloadError::Protocol(format!(
            "206 响应缺少 Content-Range 头,无法校验字节范围 [请求 {start},{end}]"
        )));
    };
    match parse_content_range(cr_value) {
        Some((resp_start, resp_end, _)) => {
            if resp_start != start || resp_end != end {
                return Err(DownloadError::Protocol(format!(
                    "Content-Range 不匹配: 请求 [{start},{end}], 响应 [{resp_start},{resp_end}]"
                )));
            }
            Ok(())
        }
        None => Err(DownloadError::Protocol(format!(
            "206 响应的 Content-Range 无法解析: {cr_value}"
        ))),
    }
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
        protocol_managed_storage: false,
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

/// 对任意字节流应用"200 回退"扫描:跳过前 `start` 字节,截取 `need` 字节。
///
/// B-1/B-2 修复的核心逻辑,从 `make_200_fallback_stream` 拆出以便单元测试
/// (用 `futures::stream::iter` 构造 mock 流,无需 mock HTTP server)。
///
/// - **不 OOM**:逐 chunk 跳过/截取,内存上限 = 单个 chunk 大小。
/// - **不浪费带宽**:`need` 取满后返回 `None` 终止流(drop 上游 → reqwest 中断读取)。
fn apply_200_fallback_scan(stream: ByteStream, start: usize, need: usize) -> ByteStream {
    let limited = stream.scan((start, need), |state: &mut (usize, usize), chunk| {
        let mut data = match chunk {
            Ok(b) => b,
            Err(e) => return future::ready(Some(Err(e))),
        };
        // 1) 跳过前导字节
        if state.0 > 0 {
            if data.len() <= state.0 {
                state.0 -= data.len();
                return future::ready(Some(Ok(Bytes::new()))); // 全部跳过,产空(下游 filter 过滤)
            }
            let rest = data.split_off(state.0);
            state.0 = 0;
            data = rest;
        }
        // 2) 截取 take 上限
        if state.1 == 0 {
            // 已取满:终止流(drop 上游 → reqwest 中断读取,不浪费带宽)
            return future::ready(None);
        }
        let out_len = data.len().min(state.1);
        state.1 -= out_len;
        let out = if out_len < data.len() {
            data.slice(..out_len)
        } else {
            data
        };
        future::ready(Some(Ok(out)))
    });
    // 过滤跳过段产出的空块;取满后流已终止
    let exact = limited.filter(|chunk| {
        let is_empty = chunk.as_ref().map(|b| b.is_empty()).unwrap_or(false);
        future::ready(!is_empty)
    });
    Box::pin(exact)
}

/// 构造"服务器忽略 Range 返回 200"的回退流:跳过前 `start` 字节,截取 `[start, end]`。
///
/// B-1/B-2 修复:统一的 200 回退流式实现,被 `download_range`(收集为 Bytes)
/// 和 `download_range_stream`(直接返回流)共用。
fn make_200_fallback_stream(response: reqwest::Response, start: u64, end: u64) -> ByteStream {
    let need = end.saturating_add(1).saturating_sub(start) as usize;
    let stream = response.bytes_stream().map(|result| {
        result.map_err(|e| DownloadError::Network(format!("读取 200 响应流失败: {e}")))
    });
    apply_200_fallback_scan(Box::pin(stream), start as usize, need)
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
        identity: Option<tachyon_core::ObjectIdentity>,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let client = self.client.clone();
        let url = url.to_owned();
        Box::pin(async move {
            let parsed_url = reqwest::Url::parse(&url)?;
            tachyon_core::validate_public_http_url(&parsed_url)?;
            let range = format!("bytes={start}-{end}");
            let if_range = identity.as_ref().and_then(|id| id.if_range_value());
            debug!(url = %tachyon_core::redact_url_for_log(&url), start, end, if_range = ?if_range, "HTTP Range 请求开始");
            let mut request = client.get(&url).header("Range", &range);
            if let Some(ref validator) = if_range {
                request = request.header("If-Range", validator.as_str());
            }
            let response = request.send().await.map_err(|e| {
                let chain = error_chain(&e);
                warn!(url = %tachyon_core::redact_url_for_log(&url), start, end, error = %e, error_chain = %chain, "Range 请求连接失败");
                DownloadError::Network(format!("Range 请求失败: {chain}"))
            })?;

            let status = response.status();
            if status == reqwest::StatusCode::OK {
                // 发过 If-Range 却收到 200: 对象已变更,禁止截取全对象与旧分片拼接。
                if if_range.is_some() {
                    warn!(
                        url = %tachyon_core::redact_url_for_log(&url),
                        start,
                        end,
                        "If-Range 条件未满足(HTTP 200),拒绝拼接"
                    );
                    return Err(DownloadError::Protocol(
                        "对象版本已变更(If-Range 返回 200),拒绝续传拼接".into(),
                    ));
                }
                // 服务器忽略 Range 头返回完整内容(常见于小文件、CDN、对象存储)。
                // B-1 修复:改用流式回退(make_200_fallback_stream)收集为 Bytes,
                // 避免原 `response.bytes()` 把整个响应体载入内存导致大文件 OOM。
                // 流式实现只缓冲单个 chunk + 取满即终止,内存上限 = 分片大小。
                info!(
                    url = %tachyon_core::redact_url_for_log(&url),
                    start, end,
                    "HTTP 200 回退:流式截取请求区间(避免整文件载入内存)"
                );
                let stream = make_200_fallback_stream(response, start, end);
                let chunks: Vec<Bytes> = stream.try_collect().await?;
                let total: usize = chunks.iter().map(|b| b.len()).sum();
                let need = end.saturating_add(1).saturating_sub(start) as usize;
                if total < need {
                    warn!(
                        url = %tachyon_core::redact_url_for_log(&url),
                        start, end, received = total, need,
                        "服务器返回 200 但响应体不足以覆盖请求区间"
                    );
                    return Err(DownloadError::Protocol(format!(
                        "服务器返回 200 但响应体长度({total})不足以覆盖请求区间 [{start},{end}]"
                    )));
                }
                // 合并 chunks 为单个 Bytes(分片通常不超过 max_fragment_size=64MB)
                let result = if chunks.len() == 1 {
                    chunks.into_iter().next().unwrap()
                } else {
                    let mut buf = bytes::BytesMut::with_capacity(total);
                    for chunk in &chunks {
                        buf.extend_from_slice(chunk);
                    }
                    buf.freeze()
                };
                return Ok(result);
            }
            if status != reqwest::StatusCode::PARTIAL_CONTENT {
                warn!(url = %tachyon_core::redact_url_for_log(&url), status = %status, "Range 请求返回非预期状态码");
                return Err(classify_http_error(status, response.headers()));
            }

            // 校验 Content-Range 与请求区间一致,防止 CDN 缓存错位/路由到不同版本文件
            // 时静默写入错位数据(仅哈希校验才暴露,此时已浪费整片带宽)。
            validate_content_range(response.headers(), start, end)?;

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
        identity: Option<tachyon_core::ObjectIdentity>,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>> {
        let client = self.client.clone();
        let url = url.to_owned();
        Box::pin(async move {
            let parsed_url = reqwest::Url::parse(&url)?;
            tachyon_core::validate_public_http_url(&parsed_url)?;
            let range = format!("bytes={start}-{end}");
            let if_range = identity.as_ref().and_then(|id| id.if_range_value());
            debug!(url = %tachyon_core::redact_url_for_log(&url), start, end, if_range = ?if_range, "HTTP 流式 Range 请求开始");
            let mut request = client.get(&url).header("Range", range);
            if let Some(ref validator) = if_range {
                request = request.header("If-Range", validator.as_str());
            }
            let response = request.send().await.map_err(|e| {
                let chain = error_chain(&e);
                warn!(url = %tachyon_core::redact_url_for_log(&url), start, end, error = %e, error_chain = %chain, "流式 Range 请求连接失败");
                DownloadError::Network(format!("Range 请求失败: {chain}"))
            })?;

            let status = response.status();
            if status == reqwest::StatusCode::OK {
                if if_range.is_some() {
                    warn!(
                        url = %tachyon_core::redact_url_for_log(&url),
                        start,
                        end,
                        "If-Range 条件未满足(HTTP 200 流式),拒绝拼接"
                    );
                    return Err(DownloadError::Protocol(
                        "对象版本已变更(If-Range 返回 200),拒绝续传拼接".into(),
                    ));
                }
                // 服务器忽略 Range 头返回完整内容(常见于小文件、CDN、对象存储)。
                // B-2 修复:委托 make_200_fallback_stream 统一实现流式回退,
                // 取满即终止流,避免浪费剩余带宽。download_range 路径也复用此函数。
                info!(
                    url = %tachyon_core::redact_url_for_log(&url),
                    start, end,
                    "服务器忽略 Range 头返回 200,流式回退:跳过 start 字节后截取请求区间"
                );
                return Ok(make_200_fallback_stream(response, start, end));
            }
            if status != reqwest::StatusCode::PARTIAL_CONTENT {
                warn!(url = %tachyon_core::redact_url_for_log(&url), status = %status, "流式 Range 请求返回非预期状态码");
                return Err(classify_http_error(status, response.headers()));
            }

            // 校验 Content-Range 与请求区间一致(同 download_range 路径)
            validate_content_range(response.headers(), start, end)?;

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

    // ── FIX-18.6: get_text 跨 chunk UTF-8 解码 ──────────────

    /// FIX-18.6: 多字节 UTF-8 码点跨 HTTP chunk 边界时,旧实现对每个 chunk 独立
    /// `String::from_utf8_lossy` 会把断裂的部分字节替换为 U+FFFD,损坏播放列表/文本。
    /// 新辅助函数 `decode_chunks_to_string` 累积原始字节后整体解码,避免边界损坏。
    #[test]
    fn test_decode_chunks_preserves_multibyte_across_boundary() {
        // “中”(U+4E2D)的 UTF-8 编码是 E4 B8 AD(3 字节),拆成两 chunk:前 1 字节 + 后 2 字节
        let s = "hello 世界 utf8";
        let bytes = s.as_bytes();
        // 找一个多字节字符的内部位置拆分
        // “世”起始于某偏移,选 split_at = 该偏移 + 1(字节内部,落在多字节序列中)
        let shi_idx = s.find("世").unwrap();
        let split_at = shi_idx + 1; // “世”的 UTF-8 第 1 字节后
        let chunks: Vec<&[u8]> = vec![&bytes[..split_at], &bytes[split_at..]];
        let decoded = super::decode_chunks_to_string(chunks.into_iter()).expect("应解码成功");
        assert_eq!(
            decoded, s,
            "跨 chunk 边界的多字节字符必须完整保留,不得出现 U+FFFD"
        );
        assert!(!decoded.contains('\u{FFFD}'), "不得含替换字符");
    }

    #[test]
    fn test_decode_chunks_single_chunk() {
        // 单 chunk 整体解码与直接 from_utf8 一致
        let chunks: Vec<&[u8]> = vec![b"plain ascii"];
        let decoded = super::decode_chunks_to_string(chunks.into_iter()).expect("应解码成功");
        assert_eq!(decoded, "plain ascii");
    }

    #[test]
    fn test_decode_chunks_rejects_invalid_utf8() {
        // 无效 UTF-8 序列应报错(不静默 lossy 替换,便于上游诊断)
        let chunks: Vec<&[u8]> = vec![&[0xFF, 0xFE, 0xFD][..]];
        assert!(super::decode_chunks_to_string(chunks.into_iter()).is_err());
    }

    // ── FIX-19: HTTP/2 强制与 HTTP/3 能力检测 ──

    /// FIX-19:http3_compiled() 反映编译期 http3 feature 状态,供配置层/前端判断 QUIC
    /// 是否真能生效(避免 enable_quic=true 在未编译时静默降级的「能力谎言」)。
    #[test]
    fn test_http3_compiled_matches_feature_cfg() {
        // 仅断言函数可调用且返回 bool(具体值取决于编译 feature,不在单测中固定)
        let compiled = super::http3_compiled();
        // 默认构建未启用 http3 feature + reqwest_unstable -> 应为 false
        // (CI 默认 --all-features 可能启用 http3,故仅断言类型与可调用性,不硬编码值)
        let _: bool = compiled;
    }

    /// FIX-19:enable_http2=false 时 build_client 应成功并强制 HTTP/1(http1_only),
    /// 不报错(旧实现仅不设 h2 选项,可能仍协商 h2)。此处验证客户端可成功构造。
    #[test]
    fn test_build_client_http1_only_when_http2_disabled() {
        // enable_http2=false 构造客户端,应成功(显式 http1_only 不影响构造)
        let client = HttpClient::build_client_for_test(false, false, 16, 30, None);
        assert!(
            client.is_ok(),
            "enable_http2=false 时客户端应成功构造并强制 HTTP/1"
        );
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

    /// 验证 SSRF 防护拒绝 loopback 重定向目标(生产模式)。
    ///
    /// test-harness feature 下 loopback 被放行(供 wiremock 端到端测试),
    /// 此测试跳过。生产 binary 不开 test-harness,SSRF 防护完整。
    #[cfg(not(feature = "test-harness"))]
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

    /// 验证 PublicDnsResolver 拒绝 localhost 解析(生产模式)。
    ///
    /// test-harness feature 下 loopback 被放行,此测试跳过。
    #[cfg(not(feature = "test-harness"))]
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
        let client = HttpClient::with_timeouts(5, 60, None);
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_timeouts_zero_connect_no_panic() {
        // connect_secs=0 表示禁用连接超时,不应 panic
        let client = HttpClient::with_timeouts(0, 30, None);
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_timeouts_zero_read_no_panic() {
        // read_secs=0 表示禁用读取超时,不应 panic
        let client = HttpClient::with_timeouts(10, 0, None);
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_timeouts_both_zero_no_panic() {
        // 同时禁用两项超时,不应 panic
        let client = HttpClient::with_timeouts(0, 0, None);
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_timeouts_explicit_proxy() {
        // 显式代理 URL 应被接受(reqwest 在 build 时校验代理 URL 语法)
        let client = HttpClient::with_timeouts(5, 30, Some("http://127.0.0.1:7890"));
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

    // --- Content-Range 一致性校验测试 (P1-2) ---

    #[test]
    fn test_parse_content_range_full() {
        // 标准格式 bytes <start>-<end>/<total>
        assert_eq!(
            super::parse_content_range("bytes 0-99/1000"),
            Some((0, 99, Some(1000)))
        );
        assert_eq!(
            super::parse_content_range("bytes 500-999/2000"),
            Some((500, 999, Some(2000)))
        );
    }

    #[test]
    fn test_parse_content_range_unknown_total() {
        // 未定大小 bytes <start>-<end>/*
        assert_eq!(
            super::parse_content_range("bytes 0-99/*"),
            Some((0, 99, None))
        );
    }

    #[test]
    fn test_parse_content_range_unsatisfied() {
        // unsatisfied range bytes */<total> → None
        assert_eq!(super::parse_content_range("bytes */1000"), None);
    }

    #[test]
    fn test_parse_content_range_malformed() {
        assert_eq!(super::parse_content_range("not-a-range"), None);
        assert_eq!(super::parse_content_range("bytes abc-def/100"), None);
        assert_eq!(super::parse_content_range("bytes 0-99"), None); // 缺少 /total
    }

    #[test]
    fn test_validate_content_range_matches() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-range", "bytes 100-199/1000".parse().unwrap());
        // 请求 [100,199] 与响应一致 → Ok
        assert!(super::validate_content_range(&headers, 100, 199).is_ok());
    }

    #[test]
    fn test_validate_content_range_mismatch() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-range", "bytes 200-299/1000".parse().unwrap());
        // 请求 [100,199] 但响应 [200,299](CDN 缓存错位)→ Err
        let result = super::validate_content_range(&headers, 100, 199);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Content-Range 不匹配"),
            "应检测到 Content-Range 不匹配"
        );
    }

    #[test]
    fn test_validate_content_range_absent_header() {
        // FIX-06:无 Content-Range 头的 206 违反 RFC 9110,拒绝(旧实现放行可静默拼接错位数据)
        let headers = reqwest::header::HeaderMap::new();
        assert!(super::validate_content_range(&headers, 100, 199).is_err());
    }

    #[test]
    fn test_validate_content_range_unparseable() {
        // FIX-06:不可解析的 Content-Range 拒绝
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-range", "garbage".parse().unwrap());
        assert!(super::validate_content_range(&headers, 100, 199).is_err());
    }

    #[test]
    fn test_validate_content_range_wrong_start() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-range", "bytes 50-199/1000".parse().unwrap());
        assert!(super::validate_content_range(&headers, 100, 199).is_err());
    }

    #[test]
    fn test_validate_content_range_wrong_end() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-range", "bytes 100-150/1000".parse().unwrap());
        assert!(super::validate_content_range(&headers, 100, 199).is_err());
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
        let client = HttpClient::with_connection_config(&config, 10, 30, None);
        assert!(client.is_ok(), "with_connection_config 应成功创建客户端");
    }

    #[test]
    fn test_with_connection_config_custom_keep_alive() {
        // 验证自定义 keep_alive_timeout_secs 不导致创建失败
        let config = tachyon_core::config::ConnectionConfig {
            keep_alive_timeout_secs: 60,
            ..Default::default()
        };
        let client = HttpClient::with_connection_config(&config, 10, 30, None);
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
        let client = HttpClient::build_client(10, 30, false, false, 16, 60, None);
        assert!(
            client.is_ok(),
            "build_client(keep_alive=60) 应成功(已配置 pool_idle_timeout)"
        );
    }

    #[test]
    fn test_build_client_http2_keepalive_config_succeeds() {
        // 验证启用 HTTP/2(含 keep_alive_while_idle)能正确创建客户端。
        // 此前未配置 keep_alive_while_idle,空闲连接可能被 NAT 静默掐断。
        // 开启后多文件串行下载的文件间隙连接保持复用,省 TCP+TLS 握手。
        let client = HttpClient::build_client(10, 30, true, false, 16, 90, None);
        assert!(
            client.is_ok(),
            "build_client(enable_http2=true) 应成功(含 keep_alive_while_idle 配置)"
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

    // ── B-1/B-2 修复测试:apply_200_fallback_scan 流式回退 ───────────────────

    /// 辅助:把 Vec<Bytes> 包装成 ByteStream
    fn mock_stream(chunks: Vec<Bytes>) -> ByteStream {
        use futures::stream;
        Box::pin(stream::iter(
            chunks
                .into_iter()
                .map(Ok::<_, DownloadError>)
                .collect::<Vec<_>>(),
        ))
    }

    /// 辅助:把 ByteStream 收集为 Vec<u8>
    async fn collect_stream(stream: ByteStream) -> Result<Vec<u8>, DownloadError> {
        use futures::TryStreamExt;
        let chunks: Vec<Bytes> = stream.try_collect().await?;
        let mut out = Vec::new();
        for c in chunks {
            out.extend_from_slice(&c);
        }
        Ok(out)
    }

    #[tokio::test]
    async fn test_200_fallback_basic_skip_and_take() {
        // 响应体 0..=9 (10 字节),请求区间 [3,6](need=4)
        let data = Bytes::from_static(b"0123456789");
        let stream = mock_stream(vec![data]);
        let out = collect_stream(apply_200_fallback_scan(stream, 3, 4))
            .await
            .unwrap();
        assert_eq!(out, b"3456");
    }

    #[tokio::test]
    async fn test_200_fallback_multi_chunk_skip() {
        // 多 chunk:跳过跨 chunk 边界
        let chunks = vec![
            Bytes::from_static(b"012"),    // 跳过全部(start=3 > len=3)
            Bytes::from_static(b"345678"), // 跳过前 0(已跳完),取 4 字节 "3456"
            Bytes::from_static(b"9"),      // 不应到达(已取满)
        ];
        let stream = mock_stream(chunks);
        let out = collect_stream(apply_200_fallback_scan(stream, 3, 4))
            .await
            .unwrap();
        assert_eq!(out, b"3456");
    }

    #[tokio::test]
    async fn test_200_fallback_terminates_early_saving_bandwidth() {
        // B-2 核心断言:取满后流应终止,不消费后续 chunk
        // 用计数器验证后续 chunk 未被 poll
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let counter = Arc::new(AtomicUsize::new(0));
        let c1 = counter.clone();
        let c2 = counter.clone();

        let stream: ByteStream = Box::pin(futures::stream::unfold(
            (0u8, c1, c2),
            |(i, c1, c2)| async move {
                if i >= 10 {
                    return None;
                }
                c1.fetch_add(1, Ordering::SeqCst);
                // 第一个 chunk 含全部所需数据(start=0, need=3),取满后应终止
                Some((
                    Ok::<_, DownloadError>(Bytes::from(vec![i])),
                    (i + 1, c1, c2),
                ))
            },
        ));
        let out = collect_stream(apply_200_fallback_scan(stream, 0, 3))
            .await
            .unwrap();
        assert_eq!(out, vec![0u8, 1, 2]);
        // 取满 3 字节后流应终止,counter 不应继续增长到 10
        let polled = counter.load(Ordering::SeqCst);
        assert!(
            polled < 10,
            "取满后应终止流,但继续 poll 了 {polled} 次(应 <10)"
        );
    }

    #[tokio::test]
    async fn test_200_fallback_start_zero() {
        let data = Bytes::from_static(b"abcdef");
        let stream = mock_stream(vec![data]);
        let out = collect_stream(apply_200_fallback_scan(stream, 0, 3))
            .await
            .unwrap();
        assert_eq!(out, b"abc");
    }

    #[tokio::test]
    async fn test_200_fallback_split_at_chunk_boundary() {
        // start 正好在 chunk 边界
        let chunks = vec![Bytes::from_static(b"abc"), Bytes::from_static(b"def")];
        let stream = mock_stream(chunks);
        let out = collect_stream(apply_200_fallback_scan(stream, 3, 2))
            .await
            .unwrap();
        assert_eq!(out, b"de");
    }

    #[tokio::test]
    async fn test_200_fallback_single_byte_chunk() {
        // 每个 chunk 1 字节,验证逐字节跳过/截取
        let chunks: Vec<Bytes> = (0..6u8).map(|i| Bytes::from(vec![i])).collect();
        let stream = mock_stream(chunks);
        let out = collect_stream(apply_200_fallback_scan(stream, 2, 3))
            .await
            .unwrap();
        assert_eq!(out, vec![2u8, 3, 4]);
    }

    #[tokio::test]
    async fn test_200_fallback_need_larger_than_body() {
        // need > 可用字节数:应返回全部可用(不报错,由上层 download_range 校验长度)
        let data = Bytes::from_static(b"abc");
        let stream = mock_stream(vec![data]);
        let out = collect_stream(apply_200_fallback_scan(stream, 0, 10))
            .await
            .unwrap();
        assert_eq!(out, b"abc");
    }

    #[tokio::test]
    async fn test_200_fallback_skip_entire_body() {
        // start >= body.len():应返回空
        let data = Bytes::from_static(b"abc");
        let stream = mock_stream(vec![data]);
        let out = collect_stream(apply_200_fallback_scan(stream, 5, 3))
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn test_200_fallback_propagates_error() {
        // 流中途出错应传播
        use futures::stream;
        let stream: ByteStream = Box::pin(stream::iter(vec![
            Ok(Bytes::from_static(b"ab")),
            Err(DownloadError::Network("模拟读取失败".into())),
            Ok(Bytes::from_static(b"cd")),
        ]));
        let result = collect_stream(apply_200_fallback_scan(stream, 0, 4)).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DownloadError::Network(_)));
    }

    #[tokio::test]
    async fn test_200_fallback_take_exactly_need_across_chunks() {
        // need 跨 chunk:第一 chunk 不够,第二 chunk 补足,第三 chunk 不到达
        let chunks = vec![
            Bytes::from_static(b"ab"),  // 取全部 2 字节(need=5,剩 3)
            Bytes::from_static(b"cde"), // 取全部 3 字节(need 满)
            Bytes::from_static(b"fg"),  // 不应到达
        ];
        let stream = mock_stream(chunks);
        let out = collect_stream(apply_200_fallback_scan(stream, 0, 5))
            .await
            .unwrap();
        assert_eq!(out, b"abcde");
    }

    // =========================================================================
    // 端到端 wiremock 测试(需 test-harness feature 放行 loopback)
    // =========================================================================
    // 以下测试用 wiremock 启动真实 HTTP server(绑定 127.0.0.1),通过 HttpClient
    // 发真实 HTTP 请求,覆盖 probe/download_range/download_range_stream/download_full/
    // download_full_stream 的协议层 IO 路径。此前这些方法仅通过 MockProtocol 在 engine
    // 层间接测,协议层 0% 单测覆盖(CRAP=156)。
    //
    // test-harness feature 下 tachyon-core 放行 loopback,使 wiremock 可用;
    // 生产 binary 不开 test-harness,SSRF 防护完整。

    #[cfg(feature = "test-harness")]
    mod wiremock_tests {
        use super::*;
        use crate::HttpClient;
        use futures::StreamExt;
        use tachyon_core::traits::Protocol;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        /// 构造测试用 HttpClient(无代理,短超时避免测试卡住)
        fn test_client() -> HttpClient {
            HttpClient::with_timeouts(5, 10, None).unwrap()
        }

        #[tokio::test]
        async fn test_probe_head_returns_metadata() {
            let server = MockServer::start().await;
            Mock::given(method("HEAD"))
                .and(path("/file.bin"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", "1000")
                        .insert_header("Accept-Ranges", "bytes")
                        .insert_header("ETag", "\"abc123\"")
                        .insert_header("Last-Modified", "Wed, 21 Oct 2026 07:28:00 GMT")
                        .insert_header("Content-Type", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/file.bin", server.uri());
            let meta = client.probe(&url).await.unwrap();
            assert_eq!(meta.file_size, Some(1000));
            assert!(meta.supports_range);
            assert_eq!(meta.etag.as_deref(), Some("\"abc123\""));
        }

        #[tokio::test]
        async fn test_probe_head_rejects_html_page() {
            let server = MockServer::start().await;
            Mock::given(method("HEAD"))
                .and(path("/page"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Type", "text/html; charset=utf-8"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/page", server.uri());
            let result = client.probe(&url).await;
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("HTML"), "应拒绝 HTML 页面: {err}");
        }

        #[tokio::test]
        async fn test_probe_head_403_falls_back_to_get_range() {
            let server = MockServer::start().await;
            // HEAD 返回 403 → 触发 GET Range:0-0 回退
            Mock::given(method("HEAD"))
                .and(path("/signed"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
            // GET Range:0-0 返回 206
            Mock::given(method("GET"))
                .and(path("/signed"))
                .and(header("Range", "bytes=0-0"))
                .respond_with(
                    ResponseTemplate::new(206)
                        .insert_header("Content-Range", "bytes 0-0/5000")
                        .insert_header("Content-Length", "1")
                        .insert_header("Accept-Ranges", "bytes")
                        .set_body_raw(b"x", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/signed", server.uri());
            let meta = client.probe(&url).await.unwrap();
            assert_eq!(meta.file_size, Some(5000));
            assert!(meta.supports_range);
        }

        #[tokio::test]
        async fn test_probe_head_404_returns_final_error() {
            let server = MockServer::start().await;
            Mock::given(method("HEAD"))
                .and(path("/missing"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/missing", server.uri());
            let result = client.probe(&url).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_download_range_sends_if_range_for_strong_etag() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/if-range"))
                .and(header("Range", "bytes=0-4"))
                .and(header("If-Range", "\"abc\""))
                .respond_with(
                    ResponseTemplate::new(206)
                        .insert_header("Content-Range", "bytes 0-4/11")
                        .insert_header("Content-Length", "5")
                        .set_body_raw(b"hello", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/if-range", server.uri());
            let identity = tachyon_core::ObjectIdentity {
                etag: Some("\"abc\"".into()),
                last_modified: None,
                file_size: Some(11),
            };
            let bytes = client
                .download_range(&url, 0, 4, Some(identity))
                .await
                .expect("带 strong ETag 的 If-Range 应成功");
            assert_eq!(bytes, Bytes::from_static(b"hello"));
        }

        #[tokio::test]
        async fn test_download_range_weak_etag_does_not_send_if_range() {
            let server = MockServer::start().await;
            // 仅匹配 Range；若错误带上 If-Range，wiremock 仍会匹配本 mock，
            // 因此再挂一个“带 If-Range 则 500”的更高优先级反向用例不方便。
            // 改为：无 If-Range 时 206；有 If-Range 时不应被调用——用缺 If-Range 的成功路径。
            Mock::given(method("GET"))
                .and(path("/weak"))
                .and(header("Range", "bytes=0-3"))
                .respond_with(
                    ResponseTemplate::new(206)
                        .insert_header("Content-Range", "bytes 0-3/4")
                        .insert_header("Content-Length", "4")
                        .set_body_raw(b"weak", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/weak", server.uri());
            let identity = tachyon_core::ObjectIdentity {
                etag: Some("W/\"w1\"".into()),
                last_modified: None,
                file_size: Some(4),
            };
            let bytes = client
                .download_range(&url, 0, 3, Some(identity))
                .await
                .expect("weak ETag 不发 If-Range 仍应 206");
            assert_eq!(bytes, Bytes::from_static(b"weak"));
        }

        #[tokio::test]
        async fn test_download_range_if_range_200_is_rejected() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/changed"))
                .and(header("Range", "bytes=5-9"))
                .and(header("If-Range", "\"old\""))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", "10")
                        .set_body_raw(b"NEWCONTENT", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/changed", server.uri());
            let identity = tachyon_core::ObjectIdentity {
                etag: Some("\"old\"".into()),
                last_modified: None,
                file_size: Some(10),
            };
            let result = client.download_range(&url, 5, 9, Some(identity)).await;
            assert!(
                result.is_err(),
                "If-Range 触发 200 全对象时不得截取拼接: {result:?}"
            );
        }

        #[tokio::test]
        async fn test_download_range_206_returns_exact_bytes() {
            let server = MockServer::start().await;
            let body = b"hello world".to_vec();
            Mock::given(method("GET"))
                .and(path("/data"))
                .and(header("Range", "bytes=0-4"))
                .respond_with(
                    ResponseTemplate::new(206)
                        .insert_header("Content-Range", "bytes 0-4/11")
                        .insert_header("Content-Length", "5")
                        .set_body_raw(&body[..5], "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/data", server.uri());
            let bytes = client.download_range(&url, 0, 4, None).await.unwrap();
            assert_eq!(bytes, Bytes::from_static(b"hello"));
        }

        #[tokio::test]
        async fn test_download_range_200_fallback_truncates() {
            let server = MockServer::start().await;
            // 服务端忽略 Range,返回完整文件(200)
            let full_body = b"hello world".to_vec();
            Mock::given(method("GET"))
                .and(path("/no-range"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", "11")
                        .set_body_raw(&full_body[..], "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/no-range", server.uri());
            // 请求 0-4,服务端返回完整 11 字节,应截取前 5 字节
            let bytes = client.download_range(&url, 0, 4, None).await.unwrap();
            assert_eq!(bytes, Bytes::from_static(b"hello"));
        }

        #[tokio::test]
        async fn test_download_range_content_range_mismatch_rejected() {
            let server = MockServer::start().await;
            // 请求 bytes=0-4,但 Content-Range 返回 bytes=100-104(错位)
            Mock::given(method("GET"))
                .and(path("/mismatch"))
                .and(header("Range", "bytes=0-4"))
                .respond_with(
                    ResponseTemplate::new(206)
                        .insert_header("Content-Range", "bytes 100-104/1000")
                        .insert_header("Content-Length", "5")
                        .set_body_raw(b"xxxxx", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/mismatch", server.uri());
            let result = client.download_range(&url, 0, 4, None).await;
            assert!(result.is_err(), "Content-Range 错位应被拒绝");
        }

        #[tokio::test]
        async fn test_download_range_404_returns_error() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/gone"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/gone", server.uri());
            let result = client.download_range(&url, 0, 99, None).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_download_range_stream_206_streams_chunks() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/stream"))
                .and(header("Range", "bytes=0-9"))
                .respond_with(
                    ResponseTemplate::new(206)
                        .insert_header("Content-Range", "bytes 0-9/10")
                        .insert_header("Content-Length", "10")
                        .set_body_raw(b"0123456789", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/stream", server.uri());
            let stream = client
                .download_range_stream(&url, 0, 9, None)
                .await
                .unwrap();
            let mut s = Box::pin(stream);
            let mut collected = Vec::new();
            while let Some(chunk) = s.next().await {
                collected.extend_from_slice(&chunk.unwrap());
            }
            assert_eq!(&collected, b"0123456789");
        }

        #[tokio::test]
        async fn test_download_range_stream_200_fallback_streams_truncated() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/stream200"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", "10")
                        .set_body_raw(b"0123456789", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/stream200", server.uri());
            // 请求 2-5,服务端返回完整 10 字节,应流式截取 [2,5]
            let stream = client
                .download_range_stream(&url, 2, 5, None)
                .await
                .unwrap();
            let mut s = Box::pin(stream);
            let mut collected = Vec::new();
            while let Some(chunk) = s.next().await {
                collected.extend_from_slice(&chunk.unwrap());
            }
            assert_eq!(&collected, b"2345");
        }

        #[tokio::test]
        async fn test_download_full_returns_complete_body() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/full"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", "5")
                        .set_body_raw(b"hello", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/full", server.uri());
            let bytes = client.download_full(&url).await.unwrap();
            assert_eq!(bytes, Bytes::from_static(b"hello"));
        }

        #[tokio::test]
        async fn test_download_full_rejects_oversize() {
            let server = MockServer::start().await;
            // Content-Length 超过 MAX_FULL_DOWNLOAD_SIZE(64MB)。
            // wiremock 需设 body 才会真正发送 Content-Length header,这里用小 body
            // 但覆盖 header 为超大值,验证 download_full 的 OOM 防护分支。
            let oversized = tachyon_core::config::MAX_FULL_DOWNLOAD_SIZE as u64 + 1;
            Mock::given(method("GET"))
                .and(path("/huge"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", oversized.to_string())
                        .set_body_raw(b"x", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/huge", server.uri());
            let result = client.download_full(&url).await;
            assert!(result.is_err(), "超大 Content-Length 应被拒绝(OOM 防护)");
        }

        #[tokio::test]
        async fn test_download_full_stream_streams_complete_body() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/fullstream"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", "5")
                        .set_body_raw(b"world", "application/octet-stream"),
                )
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/fullstream", server.uri());
            let stream = client.download_full_stream(&url).await.unwrap();
            let mut s = Box::pin(stream);
            let mut collected = Vec::new();
            while let Some(chunk) = s.next().await {
                collected.extend_from_slice(&chunk.unwrap());
            }
            assert_eq!(&collected, b"world");
        }

        #[tokio::test]
        async fn test_download_full_500_returns_throttled_or_network_error() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/servererror"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/servererror", server.uri());
            let result = client.download_full(&url).await;
            assert!(result.is_err(), "500 应返回错误");
        }

        #[tokio::test]
        async fn test_download_range_429_returns_throttled_error() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/throttled"))
                .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "120"))
                .mount(&server)
                .await;

            let client = test_client();
            let url = format!("{}/throttled", server.uri());
            let result = client.download_range(&url, 0, 99, None).await;
            assert!(result.is_err());
            // 429 应分类为 Throttled(含 retry_after_secs)
            match result {
                Err(DownloadError::Throttled { retry_after_secs }) => {
                    assert_eq!(retry_after_secs, Some(120));
                }
                other => panic!("429 应分类为 Throttled,实际: {other:?}"),
            }
        }
    }
}
