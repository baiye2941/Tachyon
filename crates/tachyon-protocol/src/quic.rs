//! QUIC 传输实现
//!
//! 基于 quinn 的 QUIC 客户端,通过 HTTP/1.1-over-QUIC 简化方案实现
//! Protocol trait 的三个核心方法:probe、download_range、download_full。
//!
//! 注意:真实 HTTP/3 使用 QPACK 头压缩,此处为简化实现,
//! 在 QUIC 双向流上发送 HTTP/1.1 格式请求。
//!
//! 支持:
//! - 0-RTT 连接建立
//! - 自签名证书(测试环境)
//! - 多路径传输预留接口
//! - HEAD 探测(文件元数据)
//! - Range 请求(分片下载)
//! - 全量下载

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Buf, Bytes};
#[cfg(test)]
use tachyon_core::filename::{extract_filename_from_url, parse_content_disposition};
use tachyon_core::traits::Protocol;
use tachyon_core::types::FileMetadata;
use tachyon_core::{ByteStream, DownloadError, DownloadResult};
use url::Url;

/// HTTP/3 客户端请求句柄类型(h3 + h3-quinn)
///
/// 使用 `Arc<tokio::sync::Mutex<>>` 包装,因为 `SendRequest::send_request()` 需要
/// `&mut self`(内部维护 stream ID 计数器)。Mutex 仅在发送请求瞬间持有,
/// 响应读取通过独立的 `RequestStream` 进行,不影响并发。
type H3SendRequest = Arc<tokio::sync::Mutex<h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>>>;

/// HTTP/3 连接管理句柄(保持控制流活跃)
type H3Connection = h3::client::Connection<h3_quinn::Connection, Bytes>;

/// QUIC 传输客户端
///
/// 封装 quinn::Endpoint 和连接状态,提供统一的 Protocol trait 实现。
/// 支持 0-RTT 连接建立:首次连接后缓存 TLS 会话状态,后续连接可跳过完整握手。
pub struct QuicTransport {
    /// 本地 QUIC 端点
    endpoint: quinn::Endpoint,
    /// 当前活跃连接(如有)
    connection: Option<quinn::Connection>,
    /// 存储的 QUIC 客户端加密配置,用于构造支持 0-RTT 的自定义 ClientConfig
    stored_crypto: Option<Arc<dyn quinn::crypto::ClientConfig>>,
    /// TLS 会话缓存:按 host 记录已建立过连接的服务器证书 DER 编码及缓存时间。
    /// 存在条目表示该 host 的 TLS session 已由 rustls 内部缓存,可尝试 0-RTT。
    /// 条目附带 `Instant` 时间戳用于 TTL 过期清理。
    session_tokens: HashMap<String, (Vec<u8>, Instant)>,
    /// HTTP/3 客户端请求句柄(连接建立后创建, Mutex 保护并发请求发送)
    h3_send: Option<H3SendRequest>,
    /// HTTP/3 连接管理句柄(保持控制流活跃,防止服务端 GOAWAY)
    h3_conn: Option<H3Connection>,
}

/// TLS 会话缓存 TTL:超过此时间的缓存条目将被视为过期,不再尝试 0-RTT
const SESSION_TOKEN_TTL: Duration = Duration::from_secs(3600);

/// 会话缓存容量上限:防止长期运行时内存无限增长
const MAX_SESSION_TOKENS: usize = 256;

impl QuicTransport {
    /// 创建新的 QUIC 传输实例
    ///
    /// 使用自签名证书生成 TLS 配置,仅适用于测试环境。
    /// 生产环境应使用系统信任根或自定义 CA。
    ///
    /// 需要在异步上下文中调用(需要 tokio 运行时)。
    pub async fn new() -> DownloadResult<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut root_store = rustls::RootCertStore::empty();
        let certs = rustls_native_certs::load_native_certs();
        if let Some(err) = certs.errors.first() {
            tracing::warn!("加载系统根证书时出现错误: {err:?}");
        }
        for cert in &certs.certs {
            root_store.add(cert.clone()).ok();
        }

        let mut tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        tls_config.resumption = rustls::client::Resumption::in_memory_sessions(256);

        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().map_err(
            |e: std::net::AddrParseError| DownloadError::Network(format!("解析本地地址失败: {e}")),
        )?)
        .map_err(|e| DownloadError::Network(format!("创建 QUIC 端点失败: {e}")))?;

        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
            .map_err(|e| DownloadError::Network(format!("构建 QUIC 客户端配置失败: {e}")))?;

        // 保存加密配置的 Arc 引用,用于后续构造支持 0-RTT 的自定义 ClientConfig
        let stored_crypto: Arc<dyn quinn::crypto::ClientConfig> = Arc::new(crypto);
        endpoint.set_default_client_config(quinn::ClientConfig::new(stored_crypto.clone()));

        Ok(Self {
            endpoint,
            connection: None,
            stored_crypto: Some(stored_crypto),
            session_tokens: HashMap::new(),
            h3_send: None,
            h3_conn: None,
        })
    }

    /// 使用已有 quinn::Endpoint 创建(高级用法)
    ///
    /// 注意:通过此方法创建的实例不包含加密配置副本,
    /// 因此不支持 0-RTT 连接建立。如需 0-RTT,请使用 `new()` 方法。
    pub fn with_endpoint(endpoint: quinn::Endpoint) -> Self {
        Self {
            endpoint,
            connection: None,
            stored_crypto: None,
            session_tokens: HashMap::new(),
            h3_send: None,
            h3_conn: None,
        }
    }

    /// 创建用于测试的 QUIC 传输实例(接受自签名证书)
    ///
    /// # 安全性
    ///
    /// 此方法跳过 TLS 证书校验,仅应在测试环境中使用。
    #[cfg(test)]
    pub async fn new_insecure() -> DownloadResult<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .map_err(|e| DownloadError::Network(format!("生成自签名证书失败: {e}")))?;

        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());

        let mut crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureVerifier))
            .with_client_auth_cert(vec![cert_der], key.into())
            .map_err(|e| DownloadError::Network(format!("构建 TLS 配置失败: {e}")))?;
        crypto.resumption = rustls::client::Resumption::in_memory_sessions(64);

        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().map_err(
            |e: std::net::AddrParseError| DownloadError::Network(format!("解析本地地址失败: {e}")),
        )?)
        .map_err(|e| DownloadError::Network(format!("创建 QUIC 端点失败: {e}")))?;

        let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| DownloadError::Network(format!("构建 QUIC 客户端配置失败: {e}")))?;

        let stored_crypto: Arc<dyn quinn::crypto::ClientConfig> = Arc::new(quic_crypto);
        endpoint.set_default_client_config(quinn::ClientConfig::new(stored_crypto.clone()));

        Ok(Self {
            endpoint,
            connection: None,
            stored_crypto: Some(stored_crypto),
            session_tokens: HashMap::new(),
            h3_send: None,
            h3_conn: None,
        })
    }

    /// 连接到远程 QUIC 服务器
    ///
    /// 解析 URL 中的 host:port,建立 QUIC 连接并存储。
    /// 如果目标 host 存在缓存的 TLS 会话,将尝试 0-RTT 连接以跳过完整握手;
    /// 0-RTT 失败时自动回退到标准 1-RTT 连接。
    pub async fn connect(&mut self, url: &str) -> DownloadResult<()> {
        let parsed =
            Url::parse(url).map_err(|e| DownloadError::Network(format!("URL 解析失败: {e}")))?;

        let host = parsed
            .host_str()
            .ok_or_else(|| DownloadError::Network("URL 缺少主机名".into()))?;
        let port = parsed.port().unwrap_or(443);
        let addr = format!("{host}:{port}");

        let addr: std::net::SocketAddr = tokio::net::lookup_host(addr)
            .await
            .map_err(|e| DownloadError::Network(format!("DNS 解析失败: {e}")))?
            .next()
            .ok_or_else(|| DownloadError::Network("DNS 解析无结果".into()))?;

        tachyon_core::reject_forbidden_ip(addr.ip())?;

        let connection = if let Some(ref crypto) = self.stored_crypto {
            // 检查 session token 是否存在且未过期
            let has_valid_session = self
                .session_tokens
                .get(host)
                .is_some_and(|(_, cached_at)| cached_at.elapsed() < SESSION_TOKEN_TTL);

            if has_valid_session {
                // 尝试 0-RTT: 使用缓存的 TLS 会话跳过完整握手
                // rustls 的 Resumption 配置会自动提供缓存的 session ticket
                let client_config = quinn::ClientConfig::new(Arc::clone(crypto));

                let connecting = self
                    .endpoint
                    .connect_with(client_config, addr, host)
                    .map_err(|e| {
                        DownloadError::Network(format!("发起 QUIC 0-RTT 连接失败: {e}"))
                    })?;

                match connecting.into_0rtt() {
                    Ok((conn, accepted)) => {
                        if accepted.await {
                            tracing::info!(host, port, "QUIC 0-RTT 连接成功");
                            conn
                        } else {
                            // 0-RTT 被服务端拒绝: 安全起见关闭此连接并重新建立 1-RTT 连接。
                            // 0-RTT 被拒绝意味着服务端不认可缓存的 session ticket
                            // (可能已过期或被撤销),此时 conn 可能处于不确定状态。
                            tracing::info!(host, "服务端拒绝 0-RTT, 清除缓存并重新建立 1-RTT 连接");
                            conn.close(0u32.into(), b"0-rtt rejected, reconnecting");
                            self.session_tokens.remove(host);

                            let connecting = self.endpoint.connect(addr, host).map_err(|e| {
                                DownloadError::Network(format!("QUIC 1-RTT 重连失败: {e}"))
                            })?;
                            connecting.await.map_err(|e| {
                                DownloadError::Network(format!("QUIC 1-RTT 连接建立失败: {e}"))
                            })?
                        }
                    }
                    Err(connecting) => {
                        // 0-RTT 不可用,回退到标准 1-RTT 连接
                        tracing::debug!(host, "0-RTT 不可用,回退到 1-RTT 连接");
                        connecting.await.map_err(|e| {
                            DownloadError::Network(format!("QUIC 连接建立失败: {e}"))
                        })?
                    }
                }
            } else {
                // 过期条目延迟清理(避免在热路径上遍历 HashMap)
                self.session_tokens.remove(host);

                // 首次连接该 host 或缓存已过期,执行标准 1-RTT 握手
                let connecting = self
                    .endpoint
                    .connect(addr, host)
                    .map_err(|e| DownloadError::Network(format!("发起 QUIC 连接失败: {e}")))?;
                connecting
                    .await
                    .map_err(|e| DownloadError::Network(format!("QUIC 连接建立失败: {e}")))?
            }
        } else {
            // 无存储的加密配置(如 with_endpoint 创建),使用默认配置
            let connecting = self
                .endpoint
                .connect(addr, host)
                .map_err(|e| DownloadError::Network(format!("发起 QUIC 连接失败: {e}")))?;
            connecting
                .await
                .map_err(|e| DownloadError::Network(format!("QUIC 连接建立失败: {e}")))?
        };

        self.connection = Some(connection.clone());
        tracing::info!(host, port, "QUIC 连接已建立");

        // S-10: 初始化 HTTP/3 客户端
        let h3_conn = h3_quinn::Connection::new(connection);
        match h3::client::new(h3_conn).await {
            Ok((h3_client_conn, send_request)) => {
                self.h3_send = Some(Arc::new(tokio::sync::Mutex::new(send_request)));
                self.h3_conn = Some(h3_client_conn);
                tracing::info!("HTTP/3 客户端已就绪");
            }
            Err(e) => {
                tracing::warn!("HTTP/3 客户端创建失败: {e}");
                self.h3_send = None;
            }
        }

        // 缓存该 host 的服务器证书,标记为已知 host 以便后续尝试 0-RTT。
        // 实际的 TLS session ticket 由 rustls 内部会话缓存自动管理。
        if !self.session_tokens.contains_key(host)
            && let Some(conn) = &self.connection
            && let Some(identity) = conn.peer_identity()
            && let Some(chain) =
                identity.downcast_ref::<Vec<rustls::pki_types::CertificateDer<'static>>>()
            && let Some(cert) = chain.first()
        {
            // 容量上限保护: 淘汰最旧条目以防止长期运行时内存泄漏
            if self.session_tokens.len() >= MAX_SESSION_TOKENS {
                self.cleanup_expired_sessions();
                // 清理后仍满则淘汰最旧条目
                if self.session_tokens.len() >= MAX_SESSION_TOKENS
                    && let Some(oldest_key) = self
                        .session_tokens
                        .iter()
                        .min_by_key(|(_, (_, t))| *t)
                        .map(|(k, _)| k.clone())
                {
                    self.session_tokens.remove(&oldest_key);
                }
            }
            self.session_tokens
                .insert(host.to_string(), (cert.to_vec(), Instant::now()));
        }

        Ok(())
    }

    /// 是否已连接到远程服务器
    pub fn is_connected(&self) -> bool {
        self.connection
            .as_ref()
            .is_some_and(|c| c.close_reason().is_none())
    }

    /// 获取当前连接的引用(如有)
    pub fn connection(&self) -> Option<&quinn::Connection> {
        self.connection.as_ref()
    }

    /// 关闭当前连接
    pub fn disconnect(&mut self) {
        // S-10: 先丢弃 h3 客户端句柄(Arc drop)
        self.h3_send.take();
        self.h3_conn.take();
        if let Some(conn) = self.connection.take() {
            conn.close(0u32.into(), b"client disconnect");
        }
    }

    /// 清理过期的 TLS 会话缓存条目
    ///
    /// 移除所有超过 `SESSION_TOKEN_TTL`（1 小时）的缓存条目,
    /// 防止长期运行时内存无限增长和使用过期的 session ticket 尝试 0-RTT。
    pub fn cleanup_expired_sessions(&mut self) {
        self.session_tokens
            .retain(|_, (_, cached_at)| cached_at.elapsed() < SESSION_TOKEN_TTL);
    }

    /// 获取活跃连接引用,未连接时返回 Protocol 错误
    fn require_connection(&self) -> DownloadResult<&quinn::Connection> {
        self.connection
            .as_ref()
            .filter(|c| c.close_reason().is_none())
            .ok_or_else(|| DownloadError::Protocol("QUIC 未连接,请先调用 connect()".into()))
    }

    /// 获取 h3 客户端句柄的 Arc 克隆,未初始化时返回 Protocol 错误
    fn require_h3_send(&self) -> DownloadResult<H3SendRequest> {
        self.h3_send
            .clone()
            .ok_or_else(|| DownloadError::Protocol("HTTP/3 客户端未初始化".into()))
    }
}

impl Drop for QuicTransport {
    fn drop(&mut self) {
        self.disconnect();
    }
}

// ---------------------------------------------------------------------------
// HTTP/3 辅助函数
// ---------------------------------------------------------------------------

/// 构造 HTTP/3 请求
///
/// 使用 `http::Request::builder()` 构建标准 HTTP/3 请求。
/// h3 自动通过 QPACK 二进制编码头部,从根本上消除 HTTP 头注入风险 (S-3)。
fn build_h3_request(
    method: http::Method,
    url: &str,
    extra_headers: &[(&str, &str)],
) -> DownloadResult<http::Request<()>> {
    let parsed =
        Url::parse(url).map_err(|e| DownloadError::Network(format!("URL 解析失败: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| DownloadError::Network("URL 缺少主机名".into()))?;
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let full_path = match parsed.query() {
        Some(q) => format!("{path}?{q}"),
        None => path.to_string(),
    };
    let user_agent = tachyon_core::config::USER_AGENT;

    let mut builder = http::Request::builder()
        .method(method)
        .uri(&full_path)
        .header("host", host)
        .header("user-agent", user_agent)
        .header("accept-encoding", "identity");

    for (key, value) in extra_headers {
        builder = builder.header(*key, *value);
    }

    builder
        .body(())
        .map_err(|e| DownloadError::Protocol(format!("构造 HTTP/3 请求失败: {e}")))
}

/// 从 HTTP/3 响应头中提取文件元数据
///
/// 使用 `http::HeaderMap` 直接访问解码后的头部,无需文本解析。
fn parse_h3_metadata(response: &http::Response<()>, url: &str) -> DownloadResult<FileMetadata> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(DownloadError::Protocol(format!("HTTP {status}")));
    }

    let headers = response.headers();

    let file_size = headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let content_type = headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let supports_range = headers
        .get(http::header::ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.contains("bytes"));

    let etag = headers
        .get(http::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let last_modified = headers
        .get(http::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let content_disposition_name = headers
        .get(http::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(tachyon_core::filename::parse_content_disposition);

    let file_name = content_disposition_name
        .unwrap_or_else(|| tachyon_core::filename::extract_filename_from_url(url));

    Ok(FileMetadata {
        file_name,
        file_size,
        content_type,
        supports_range,
        etag,
        last_modified,
    })
}

/// 将 h3 RequestStream 包装为 ByteStream
///
/// 逐块读取 HTTP/3 DATA 帧,产出 Bytes。无需手动解析 HTTP 响应头:
/// h3 已通过 QPACK 解码头部,响应状态码和头部通过 `http::Response` 直接访问。
fn h3_recv_streaming(
    stream: h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
) -> ByteStream {
    Box::pin(futures::stream::unfold(stream, move |mut s| async move {
        match s.recv_data().await {
            Ok(Some(mut buf)) => {
                let len = buf.chunk().len();
                let data = Bytes::copy_from_slice(buf.chunk());
                buf.advance(len);
                if data.is_empty() {
                    None
                } else {
                    Some((Ok(data), s))
                }
            }
            Ok(None) => None,
            Err(e) => Some((
                Err(DownloadError::Network(format!("HTTP/3 流读取失败: {e}"))),
                s,
            )),
        }
    }))
}

/// 将 h3 错误转换为 DownloadError
fn h3_error(e: impl std::fmt::Display) -> DownloadError {
    DownloadError::Network(format!("HTTP/3 协议错误: {e}"))
}

// ---------------------------------------------------------------------------
// 不安全的证书验证器 -- 仅用于测试
// ---------------------------------------------------------------------------

/// 不安全的证书验证器 -- 仅用于测试,接受任何证书
///
/// # 安全性
///
/// 此验证器跳过所有 TLS 证书校验,仅应在测试环境中使用。
/// 在生产代码中使用将导致中间人攻击(MITM)风险。
#[cfg(test)]
#[derive(Debug)]
struct InsecureVerifier;

#[cfg(test)]
impl rustls::client::danger::ServerCertVerifier for InsecureVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// Protocol trait 实现
// ---------------------------------------------------------------------------

impl Protocol for QuicTransport {
    fn probe(
        &self,
        url: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>> {
        if let Err(e) = self.require_connection() {
            return Box::pin(async move { Err(e) });
        }
        let h3_send = match self.require_h3_send() {
            Ok(s) => s,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let url = url.to_owned();
        Box::pin(async move {
            // S-2: 校验 URL 防 SSRF
            let parsed = Url::parse(&url)
                .map_err(|e| DownloadError::Network(format!("URL 解析失败: {e}")))?;
            tachyon_core::url_safety::validate_public_http_url(&parsed)?;

            let request = build_h3_request(http::Method::HEAD, &url, &[])?;

            let mut send = h3_send.lock().await;
            let mut stream = send.send_request(request).await.map_err(h3_error)?;
            let response = stream.recv_response().await.map_err(h3_error)?;

            parse_h3_metadata(&response, &url)
        })
    }

    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        if let Err(e) = self.require_connection() {
            return Box::pin(async move { Err(e) });
        }
        let h3_send = match self.require_h3_send() {
            Ok(s) => s,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let url = url.to_owned();
        Box::pin(async move {
            // S-2: 校验 URL 防 SSRF
            let parsed = Url::parse(&url)
                .map_err(|e| DownloadError::Network(format!("URL 解析失败: {e}")))?;
            tachyon_core::url_safety::validate_public_http_url(&parsed)?;

            let range_value = format!("bytes={start}-{end}");
            let request = build_h3_request(http::Method::GET, &url, &[("range", &range_value)])?;

            let mut send = h3_send.lock().await;
            let mut stream = send.send_request(request).await.map_err(h3_error)?;
            let response = stream.recv_response().await.map_err(h3_error)?;

            // 严格验证 Range 请求必须返回 206
            let status = response.status().as_u16();
            if status != 206 {
                return Err(DownloadError::Protocol(format!(
                    "服务器忽略 Range 头, 返回 HTTP {status} (期望 206 Partial Content)"
                )));
            }
            drop(send); // 释放 Mutex 以便并发请求

            // 读取响应体 DATA 帧
            use futures::StreamExt;
            let mut byte_stream = h3_recv_streaming(stream);
            let mut chunks = Vec::new();
            while let Some(chunk) = byte_stream.next().await {
                chunks.push(chunk?);
            }
            let total_len = chunks.iter().map(|b| b.len()).sum();
            let mut result = Vec::with_capacity(total_len);
            for chunk in chunks {
                result.extend_from_slice(&chunk);
            }
            Ok(Bytes::from(result))
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>> {
        if let Err(e) = self.require_connection() {
            return Box::pin(async move { Err(e) });
        }
        let h3_send = match self.require_h3_send() {
            Ok(s) => s,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let url = url.to_owned();
        Box::pin(async move {
            // S-2: 校验 URL 防 SSRF
            let parsed = Url::parse(&url)
                .map_err(|e| DownloadError::Network(format!("URL 解析失败: {e}")))?;
            tachyon_core::url_safety::validate_public_http_url(&parsed)?;

            let range_value = format!("bytes={start}-{end}");
            let request = build_h3_request(http::Method::GET, &url, &[("range", &range_value)])?;

            let mut send = h3_send.lock().await;
            let mut stream = send.send_request(request).await.map_err(h3_error)?;
            let response = stream.recv_response().await.map_err(h3_error)?;

            // 严格验证 Range 请求必须返回 206
            let status = response.status().as_u16();
            if status != 206 {
                return Err(DownloadError::Protocol(format!(
                    "服务器忽略 Range 头, 返回 HTTP {status} (期望 206 Partial Content)"
                )));
            }
            drop(send); // 释放 Mutex

            Ok(h3_recv_streaming(stream))
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        if let Err(e) = self.require_connection() {
            return Box::pin(async move { Err(e) });
        }
        let h3_send = match self.require_h3_send() {
            Ok(s) => s,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let url = url.to_owned();
        Box::pin(async move {
            // S-2: 校验 URL 防 SSRF
            let parsed = Url::parse(&url)
                .map_err(|e| DownloadError::Network(format!("URL 解析失败: {e}")))?;
            tachyon_core::url_safety::validate_public_http_url(&parsed)?;

            let request = build_h3_request(http::Method::GET, &url, &[])?;

            let mut send = h3_send.lock().await;
            let mut stream = send.send_request(request).await.map_err(h3_error)?;
            let _response = stream.recv_response().await.map_err(h3_error)?;
            drop(send); // 释放 Mutex

            // 读取全部响应体 DATA 帧
            use futures::StreamExt;
            let mut byte_stream = h3_recv_streaming(stream);
            let mut chunks = Vec::new();
            let mut total_size: usize = 0;
            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk?;
                total_size = total_size.saturating_add(chunk.len());
                // W-12: 统一使用共享大小限制,防止 OOM
                if total_size > tachyon_core::config::MAX_FULL_DOWNLOAD_SIZE {
                    return Err(DownloadError::Protocol(format!(
                        "QUIC 文件过大: 超过单次全量下载上限 {} MB, 请使用分片下载",
                        tachyon_core::config::MAX_FULL_DOWNLOAD_SIZE / (1024 * 1024)
                    )));
                }
                chunks.push(chunk);
            }
            let mut result = Vec::with_capacity(total_size);
            for chunk in chunks {
                result.extend_from_slice(&chunk);
            }
            Ok(Bytes::from(result))
        })
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- 连接管理测试 ---

    #[tokio::test]
    async fn test_quic_transport_creation() {
        let transport = QuicTransport::new_insecure().await;
        assert!(
            transport.is_ok(),
            "QuicTransport::new_insecure() 应成功创建"
        );
    }

    #[tokio::test]
    async fn test_quic_transport_initially_disconnected() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        assert!(!transport.is_connected(), "新创建的传输不应处于已连接状态");
    }

    #[tokio::test]
    async fn test_quic_transport_no_initial_connection() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        assert!(
            transport.connection().is_none(),
            "新创建的传输不应有活跃连接"
        );
    }

    #[tokio::test]
    async fn test_quic_transport_disconnect_on_drop() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        // 确保 Drop 不会 panic
        drop(transport);
    }

    #[tokio::test]
    async fn test_quic_with_endpoint() {
        let endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
        let transport = QuicTransport::with_endpoint(endpoint);
        assert!(!transport.is_connected());
    }

    // --- 未连接时 Protocol 方法应返回错误 ---

    #[tokio::test]
    async fn test_probe_returns_error_when_not_connected() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        let result = transport.probe("https://example.com/file.bin").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("未连接"),
            "错误应提示未连接,实际: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_download_range_returns_error_when_not_connected() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        let result = transport
            .download_range("https://example.com/file.bin", 0, 1023)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("未连接"));
    }

    #[tokio::test]
    async fn test_download_full_returns_error_when_not_connected() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        let result = transport
            .download_full("https://example.com/file.bin")
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("未连接"));
    }

    // --- URL 解析辅助测试 ---

    #[test]
    fn test_extract_filename_from_url_basic() {
        assert_eq!(
            extract_filename_from_url("https://example.com/path/file.zip"),
            "file.zip"
        );
    }

    #[test]
    fn test_extract_filename_from_url_root() {
        assert_eq!(extract_filename_from_url("https://example.com/"), "unknown");
    }

    #[test]
    fn test_extract_filename_from_url_no_path() {
        assert_eq!(extract_filename_from_url("https://example.com"), "unknown");
    }

    #[test]
    fn test_extract_filename_from_url_with_query() {
        assert_eq!(
            extract_filename_from_url("https://example.com/file.zip?v=2"),
            "file.zip"
        );
    }

    #[test]
    fn test_parse_content_disposition_with_quotes() {
        assert_eq!(
            parse_content_disposition("attachment; filename=\"test.zip\""),
            Some("test.zip".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_without_quotes() {
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
    fn test_parse_content_disposition_no_filename() {
        assert_eq!(parse_content_disposition("attachment"), None);
    }

    // --- Protocol trait 一致性测试(未连接状态) ---

    #[tokio::test]
    async fn test_protocol_trait_probe_error_variant() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        let err = transport
            .probe("https://example.com/test")
            .await
            .unwrap_err();
        assert!(
            matches!(err, DownloadError::Protocol(_)),
            "未连接时 probe 应返回 Protocol 错误变体,实际: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_protocol_trait_download_range_error_variant() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        let err = transport
            .download_range("https://example.com/test", 0, 100)
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::Protocol(_)));
    }

    #[tokio::test]
    async fn test_protocol_trait_download_full_error_variant() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        let err = transport
            .download_full("https://example.com/test")
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::Protocol(_)));
    }

    // --- download_range_stream 未连接错误测试 ---

    #[tokio::test]
    async fn test_download_range_stream_returns_error_when_not_connected() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        let result = transport
            .download_range_stream("https://example.com/file.bin", 0, 1023)
            .await;
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(err_msg.contains("未连接"), "应提示未连接,实际: {err_msg}");
    }

    // --- h3 请求构造测试 ---

    #[test]
    fn test_build_h3_request_head_basic() {
        let req =
            build_h3_request(http::Method::HEAD, "https://example.com/file.bin", &[]).unwrap();
        assert_eq!(req.method(), http::Method::HEAD);
        assert_eq!(req.uri().path(), "/file.bin");
        assert_eq!(req.headers().get("host").unwrap(), "example.com");
        assert!(req.headers().contains_key("user-agent"));
    }

    #[test]
    fn test_build_h3_request_root_path() {
        let req = build_h3_request(http::Method::HEAD, "https://example.com/", &[]).unwrap();
        assert_eq!(req.uri().path(), "/");
    }

    #[test]
    fn test_build_h3_request_with_query() {
        let req =
            build_h3_request(http::Method::GET, "https://example.com/path?key=value", &[]).unwrap();
        assert_eq!(req.uri().path(), "/path");
        assert_eq!(req.uri().query(), Some("key=value"));
    }

    #[test]
    fn test_build_h3_request_with_range_header() {
        let req = build_h3_request(
            http::Method::GET,
            "https://example.com/big.bin",
            &[("range", "bytes=0-999")],
        )
        .unwrap();
        assert_eq!(req.method(), http::Method::GET);
        assert_eq!(req.headers().get("range").unwrap(), "bytes=0-999");
    }

    #[test]
    fn test_build_h3_request_custom_port() {
        let req =
            build_h3_request(http::Method::HEAD, "https://example.com:8443/file", &[]).unwrap();
        assert_eq!(req.headers().get("host").unwrap(), "example.com");
    }

    #[test]
    fn test_build_h3_request_invalid_url() {
        let result = build_h3_request(http::Method::GET, "not a url", &[]);
        assert!(result.is_err());
    }

    // --- h3 响应元数据解析测试 ---

    fn make_h3_response(status: u16, headers: Vec<(&str, &str)>) -> http::Response<()> {
        let mut builder = http::Response::builder().status(status);
        for (k, v) in headers {
            builder = builder.header(k, v);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn test_parse_h3_metadata_success() {
        let resp = make_h3_response(
            200,
            vec![
                ("content-length", "1048576"),
                ("content-type", "application/octet-stream"),
                ("accept-ranges", "bytes"),
                ("etag", "\"abc123\""),
                ("last-modified", "Mon, 01 Jan 2024 00:00:00 GMT"),
            ],
        );
        let meta = parse_h3_metadata(&resp, "https://example.com/data.bin").unwrap();
        assert_eq!(meta.file_name, "data.bin");
        assert_eq!(meta.file_size, Some(1_048_576));
        assert_eq!(
            meta.content_type,
            Some("application/octet-stream".to_string())
        );
        assert!(meta.supports_range);
        assert_eq!(meta.etag, Some("\"abc123\"".to_string()));
        assert_eq!(
            meta.last_modified,
            Some("Mon, 01 Jan 2024 00:00:00 GMT".to_string())
        );
    }

    #[test]
    fn test_parse_h3_metadata_no_content_length() {
        let resp = make_h3_response(200, vec![("content-type", "text/html")]);
        let meta = parse_h3_metadata(&resp, "https://example.com/page").unwrap();
        assert_eq!(meta.file_name, "page");
        assert_eq!(meta.file_size, None);
        assert!(!meta.supports_range);
    }

    #[test]
    fn test_parse_h3_metadata_404() {
        let resp = make_h3_response(404, vec![]);
        let result = parse_h3_metadata(&resp, "https://example.com/missing");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("404"));
    }

    #[test]
    fn test_parse_h3_metadata_500() {
        let resp = make_h3_response(500, vec![]);
        let result = parse_h3_metadata(&resp, "https://example.com/");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }

    #[test]
    fn test_parse_h3_metadata_302_redirect() {
        let resp = make_h3_response(302, vec![("location", "https://example.com/new")]);
        let result = parse_h3_metadata(&resp, "https://example.com/old");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("302"));
    }

    #[test]
    fn test_parse_h3_metadata_content_disposition() {
        let resp = make_h3_response(
            200,
            vec![
                ("content-disposition", "attachment; filename=\"report.pdf\""),
                ("content-length", "2048"),
            ],
        );
        let meta = parse_h3_metadata(&resp, "https://example.com/download?id=42").unwrap();
        assert_eq!(meta.file_name, "report.pdf");
        assert_eq!(meta.file_size, Some(2048));
    }

    #[test]
    fn test_parse_h3_metadata_no_range_support() {
        let resp = make_h3_response(200, vec![("content-length", "100")]);
        let meta = parse_h3_metadata(&resp, "https://example.com/file").unwrap();
        assert!(!meta.supports_range, "无 Accept-Ranges 头应为 false");
    }
}
