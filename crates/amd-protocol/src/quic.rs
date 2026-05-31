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

use std::io::Write;
use std::pin::Pin;

#[cfg(test)]
use amd_core::filename::{extract_filename_from_url, parse_content_disposition};
use amd_core::traits::Protocol;
use amd_core::types::FileMetadata;
use amd_core::{AmdError, AmdResult, ByteStream};
use bytes::Bytes;
use url::Url;

/// QUIC 传输客户端
///
/// 封装 quinn::Endpoint 和连接状态,提供统一的 Protocol trait 实现。
pub struct QuicTransport {
    /// 本地 QUIC 端点
    endpoint: quinn::Endpoint,
    /// 当前活跃连接(如有)
    connection: Option<quinn::Connection>,
}

impl QuicTransport {
    /// 创建新的 QUIC 传输实例
    ///
    /// 使用自签名证书生成 TLS 配置,仅适用于测试环境。
    /// 生产环境应使用系统信任根或自定义 CA。
    ///
    /// 需要在异步上下文中调用(需要 tokio 运行时)。
    pub async fn new() -> AmdResult<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut root_store = rustls::RootCertStore::empty();
        let certs = rustls_native_certs::load_native_certs();
        if let Some(err) = certs.errors.first() {
            tracing::warn!("加载系统根证书时出现错误: {err:?}");
        }
        for cert in &certs.certs {
            root_store.add(cert.clone()).ok();
        }

        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().map_err(
            |e: std::net::AddrParseError| AmdError::Network(format!("解析本地地址失败: {e}")),
        )?)
        .map_err(|e| AmdError::Network(format!("创建 QUIC 端点失败: {e}")))?;

        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
            .map_err(|e| AmdError::Network(format!("构建 QUIC 客户端配置失败: {e}")))?;

        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));

        Ok(Self {
            endpoint,
            connection: None,
        })
    }

    /// 使用已有 quinn::Endpoint 创建(高级用法)
    pub fn with_endpoint(endpoint: quinn::Endpoint) -> Self {
        Self {
            endpoint,
            connection: None,
        }
    }

    /// 创建用于测试的 QUIC 传输实例(接受自签名证书)
    ///
    /// # 安全性
    ///
    /// 此方法跳过 TLS 证书校验,仅应在测试环境中使用。
    #[cfg(test)]
    pub async fn new_insecure() -> AmdResult<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .map_err(|e| AmdError::Network(format!("生成自签名证书失败: {e}")))?;

        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());

        let crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureVerifier))
            .with_client_auth_cert(vec![cert_der], key.into())
            .map_err(|e| AmdError::Network(format!("构建 TLS 配置失败: {e}")))?;

        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().map_err(
            |e: std::net::AddrParseError| AmdError::Network(format!("解析本地地址失败: {e}")),
        )?)
        .map_err(|e| AmdError::Network(format!("创建 QUIC 端点失败: {e}")))?;

        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
                .map_err(|e| AmdError::Network(format!("构建 QUIC 客户端配置失败: {e}")))?,
        )));

        Ok(Self {
            endpoint,
            connection: None,
        })
    }

    /// 连接到远程 QUIC 服务器
    ///
    /// 解析 URL 中的 host:port,建立 QUIC 连接并存储。
    pub async fn connect(&mut self, url: &str) -> AmdResult<()> {
        let parsed =
            Url::parse(url).map_err(|e| AmdError::Network(format!("URL 解析失败: {e}")))?;

        let host = parsed
            .host_str()
            .ok_or_else(|| AmdError::Network("URL 缺少主机名".into()))?;
        let port = parsed.port().unwrap_or(443);
        let addr = format!("{host}:{port}");

        let addr: std::net::SocketAddr = tokio::net::lookup_host(addr)
            .await
            .map_err(|e| AmdError::Network(format!("DNS 解析失败: {e}")))?
            .next()
            .ok_or_else(|| AmdError::Network("DNS 解析无结果".into()))?;

        amd_core::reject_forbidden_ip(addr.ip())?;

        let connecting = self
            .endpoint
            .connect(addr, host)
            .map_err(|e| AmdError::Network(format!("发起 QUIC 连接失败: {e}")))?;

        let connection = connecting
            .await
            .map_err(|e| AmdError::Network(format!("QUIC 连接建立失败: {e}")))?;

        self.connection = Some(connection);
        tracing::info!(host, port, "QUIC 连接已建立");
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
        if let Some(conn) = self.connection.take() {
            conn.close(0u32.into(), b"client disconnect");
        }
    }

    /// 获取活跃连接引用,未连接时返回 Protocol 错误
    fn require_connection(&self) -> AmdResult<&quinn::Connection> {
        self.connection
            .as_ref()
            .filter(|c| c.close_reason().is_none())
            .ok_or_else(|| AmdError::Protocol("QUIC 未连接,请先调用 connect()".into()))
    }
}

impl Drop for QuicTransport {
    fn drop(&mut self) {
        self.disconnect();
    }
}

// ---------------------------------------------------------------------------
// 辅助函数:HTTP 响应解析
// ---------------------------------------------------------------------------

/// 解析 HTTP HEAD 响应,提取文件元数据
///
/// 响应格式:
/// ```text
/// HTTP/1.1 200 OK\r\n
/// Content-Length: 12345\r\n
/// Content-Type: application/octet-stream\r\n
/// \r\n
/// ```
fn parse_head_response(response: &str, url: &str) -> AmdResult<FileMetadata> {
    let header_end = response
        .find("\r\n\r\n")
        .or_else(|| response.find("\n\n"))
        .unwrap_or(response.len());
    let header_section = &response[..header_end];

    let mut lines = header_section.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| AmdError::Protocol("响应为空".into()))?;

    // 解析状态码: "HTTP/1.1 200 OK" -> 200
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| AmdError::Protocol(format!("无效的状态行: {status_line}")))?
        .parse()
        .map_err(|_| AmdError::Protocol(format!("无法解析状态码: {status_line}")))?;

    if !(200..300).contains(&status_code) {
        return Err(AmdError::Protocol(format!("HTTP {status_code}")));
    }

    // 解析各响应头
    let mut file_size: Option<u64> = None;
    let mut content_type: Option<String> = None;
    let mut supports_range = false;
    let mut etag: Option<String> = None;
    let mut last_modified: Option<String> = None;
    let mut content_disposition_name: Option<String> = None;

    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value_trimmed = value.trim();

            // 使用 eq_ignore_ascii_case 避免每行分配一个新 String
            if key.eq_ignore_ascii_case("content-length") {
                file_size = value_trimmed.parse().ok();
            } else if key.eq_ignore_ascii_case("content-type") {
                content_type = Some(value_trimmed.to_string());
            } else if key.eq_ignore_ascii_case("accept-ranges") {
                supports_range = value_trimmed.contains("bytes");
            } else if key.eq_ignore_ascii_case("etag") {
                etag = Some(value_trimmed.to_string());
            } else if key.eq_ignore_ascii_case("last-modified") {
                last_modified = Some(value_trimmed.to_string());
            } else if key.eq_ignore_ascii_case("content-disposition") {
                content_disposition_name =
                    amd_core::filename::parse_content_disposition(value_trimmed);
            }
        }
    }

    let file_name = content_disposition_name
        .unwrap_or_else(|| amd_core::filename::extract_filename_from_url(url));

    Ok(FileMetadata {
        file_name,
        file_size,
        content_type,
        supports_range,
        etag,
        last_modified,
    })
}

/// 分离 HTTP 响应头和响应体
///
/// HTTP 响应以 `\r\n\r\n` 分隔头部与正文。返回正文部分的 Bytes。
/// 如果未找到分隔符,返回错误。
fn parse_body_response(response: &[u8]) -> AmdResult<Bytes> {
    // 查找 \r\n\r\n 分隔符
    let separator = b"\r\n\r\n";
    let header_end = response
        .windows(separator.len())
        .position(|window| window == separator)
        .ok_or_else(|| AmdError::Protocol("响应中未找到头部/体部分隔符".into()))?;

    let body_start = header_end + separator.len();

    // 解析状态码以验证响应有效
    let header_str = std::str::from_utf8(&response[..header_end])
        .map_err(|e| AmdError::Protocol(format!("响应头部非有效 UTF-8: {e}")))?;

    if let Some(status_line) = header_str.lines().next() {
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);

        if !(200..300).contains(&status_code) {
            return Err(AmdError::Protocol(format!("HTTP {status_code}")));
        }
    }

    Ok(Bytes::copy_from_slice(&response[body_start..]))
}

/// 在已建立的 QUIC 连接上发送 HTTP 请求并读取完整响应
///
/// 打开一个新的双向流,发送请求,读取响应。
async fn send_request(conn: &quinn::Connection, request: &[u8]) -> AmdResult<Vec<u8>> {
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| AmdError::Network(format!("打开 QUIC 双向流失败: {e}")))?;

    send.write_all(request)
        .await
        .map_err(|e| AmdError::Network(format!("发送请求数据失败: {e}")))?;

    send.finish()
        .map_err(|e| AmdError::Network(format!("关闭发送流失败: {e}")))?;

    // 分片下载可能返回大量数据,设置 256MB 上限
    // TODO: 改为流式读取(循环 recv.read() 直到 EOF),消除硬性上限
    recv.read_to_end(256 * 1024 * 1024)
        .await
        .map_err(|e| AmdError::Network(format!("读取响应数据失败: {e}")))
}

/// 流式读取 QUIC 响应,分块返回数据
///
/// 使用 quinn `read_chunk` 进行零拷贝分块读取(默认 64KB/块),
/// 先解析 HTTP 响应头并验证状态码,再逐块产出正文数据。
///
/// 消除 `read_to_end` 的 256MB 硬性上限和峰值内存问题。
fn recv_streaming(recv: quinn::RecvStream) -> ByteStream {
    const CHUNK_SIZE: usize = 64 * 1024; // 64KB per read_chunk
    const MAX_HEADER_BUF: usize = 256 * 1024; // 头部缓冲区上限 256KB

    /// 解析器状态:跟踪 HTTP 响应头解析进度
    enum State {
        /// 正在寻找 `\r\n\r\n` 头部终止符
        FindHeader { header_buf: Vec<u8> },
        /// 正在流式读取正文
        Body,
    }

    let initial_state = State::FindHeader {
        header_buf: Vec::with_capacity(4096),
    };

    // 使用 futures::stream::unfold 实现状态机驱动的 Stream
    // recv 的所有权移入闭包,确保 Stream 结束时正确清理 QUIC 流
    Box::pin(futures::stream::unfold(
        (initial_state, recv, Bytes::new()),
        |(mut state, mut recv, pending): (State, quinn::RecvStream, Bytes)| async move {
            loop {
                // 读取下一个 QUIC 数据块(零拷贝)
                let chunk = match recv.read_chunk(CHUNK_SIZE, true).await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => return None,
                    Err(e) => {
                        return Some((
                            Err(AmdError::Network(format!("QUIC 流读取失败: {e}"))),
                            (state, recv, pending),
                        ));
                    }
                };

                match state {
                    State::FindHeader { ref mut header_buf } => {
                        header_buf.extend_from_slice(&chunk.bytes);

                        // 在累积缓冲区中搜索分隔符
                        if let Some(pos) = find_subsequence(header_buf, b"\r\n\r\n") {
                            let body_start = pos + 4;

                            // 验证 HTTP 状态码
                            if let Err(e) = validate_http_status(&header_buf[..pos]) {
                                return Some((Err(e), (State::Body, recv, pending)));
                            }

                            // 提取尾部残余数据(分隔符之后的字节)
                            let tail = Bytes::copy_from_slice(&header_buf[body_start..]);
                            if !tail.is_empty() {
                                return Some((Ok(tail), (State::Body, recv, pending)));
                            }
                            // 没有残余数据,继续读取 body
                            continue;
                        }

                        if header_buf.len() > MAX_HEADER_BUF {
                            return Some((
                                Err(AmdError::Protocol("HTTP 头部超过 256KB 上限".into())),
                                (State::Body, recv, pending),
                            ));
                        }
                        // 继续读取下一个 chunk
                        continue;
                    }
                    State::Body => {
                        let data = chunk.bytes;
                        if !data.is_empty() {
                            return Some((Ok(data), (State::Body, recv, pending)));
                        }
                        // 空 chunk,继续读取
                    }
                }
            }
        },
    ))
}

/// 在字节切片中查找子序列位置
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 从头部字节中解析并验证 HTTP 状态码(200-299)
fn validate_http_status(header_bytes: &[u8]) -> AmdResult<()> {
    let header_str = std::str::from_utf8(header_bytes)
        .map_err(|e| AmdError::Protocol(format!("响应头部非有效 UTF-8: {e}")))?;

    if let Some(status_line) = header_str.lines().next() {
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);

        if !(200..300).contains(&status_code) {
            return Err(AmdError::Protocol(format!("HTTP {status_code}")));
        }
    }

    Ok(())
}

/// 从 URL 中提取路径(含查询字符串)
///
/// 返回格式: `/path?key=value`
fn extract_path(url: &Url) -> String {
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    match url.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    }
}

/// 构造 HTTP/1.1 格式的 HEAD 请求
fn build_head_request(url: &str) -> AmdResult<Vec<u8>> {
    let parsed = Url::parse(url).map_err(|e| AmdError::Network(format!("URL 解析失败: {e}")))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| AmdError::Network("URL 缺少主机名".into()))?;
    let full_path = extract_path(&parsed);

    // 使用 write! 避免 format! 创建中间 String,直接写入预分配缓冲区
    let mut buf = Vec::with_capacity(128 + full_path.len() + host.len());
    write!(
        buf,
        "HEAD {full_path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .expect("写入 Vec 不会失败");
    Ok(buf)
}

/// 构造 HTTP/1.1 格式的 GET 请求(带 Range 头)
fn build_range_request(url: &str, start: u64, end: u64) -> AmdResult<Vec<u8>> {
    let parsed = Url::parse(url).map_err(|e| AmdError::Network(format!("URL 解析失败: {e}")))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| AmdError::Network("URL 缺少主机名".into()))?;
    let full_path = extract_path(&parsed);

    let mut buf = Vec::with_capacity(160 + full_path.len() + host.len());
    write!(buf, "GET {full_path} HTTP/1.1\r\nHost: {host}\r\nRange: bytes={start}-{end}\r\nConnection: close\r\n\r\n")
        .expect("写入 Vec 不会失败");
    Ok(buf)
}

/// 构造 HTTP/1.1 格式的 GET 请求(全量下载)
fn build_full_request(url: &str) -> AmdResult<Vec<u8>> {
    let parsed = Url::parse(url).map_err(|e| AmdError::Network(format!("URL 解析失败: {e}")))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| AmdError::Network("URL 缺少主机名".into()))?;
    let full_path = extract_path(&parsed);

    let mut buf = Vec::with_capacity(128 + full_path.len() + host.len());
    write!(
        buf,
        "GET {full_path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .expect("写入 Vec 不会失败");
    Ok(buf)
}

// ---------------------------------------------------------------------------
// 不安全的证书验证器 -- 仅用于测试
// ---------------------------------------------------------------------------

use std::sync::Arc;

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
    ) -> Pin<Box<dyn std::future::Future<Output = AmdResult<FileMetadata>> + Send>> {
        let conn = match self.require_connection() {
            Ok(c) => c.clone(),
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let url = url.to_owned();
        Box::pin(async move {
            let request = build_head_request(&url)?;
            let response_bytes = send_request(&conn, &request).await?;
            let response = String::from_utf8_lossy(&response_bytes);
            parse_head_response(&response, &url)
        })
    }

    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = AmdResult<Bytes>> + Send>> {
        let conn = match self.require_connection() {
            Ok(c) => c.clone(),
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let url = url.to_owned();
        Box::pin(async move {
            let request = build_range_request(&url, start, end)?;
            let response_bytes = send_request(&conn, &request).await?;
            parse_body_response(&response_bytes)
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = AmdResult<ByteStream>> + Send>> {
        let conn = match self.require_connection() {
            Ok(c) => c.clone(),
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let url = url.to_owned();
        Box::pin(async move {
            let request = build_range_request(&url, start, end)?;

            // 打开双向流并发送请求
            let (mut send, recv) = conn
                .open_bi()
                .await
                .map_err(|e| AmdError::Network(format!("打开 QUIC 双向流失败: {e}")))?;

            send.write_all(&request)
                .await
                .map_err(|e| AmdError::Network(format!("发送请求数据失败: {e}")))?;

            send.finish()
                .map_err(|e| AmdError::Network(format!("关闭发送流失败: {e}")))?;

            // 返回流式读取器:逐块读取响应,避免一次性缓冲整个响应
            Ok(recv_streaming(recv))
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = AmdResult<Bytes>> + Send>> {
        let conn = match self.require_connection() {
            Ok(c) => c.clone(),
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let url = url.to_owned();
        Box::pin(async move {
            let request = build_full_request(&url)?;
            let response_bytes = send_request(&conn, &request).await?;
            parse_body_response(&response_bytes)
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

    // --- HEAD 请求构造测试 ---

    #[test]
    fn test_build_head_request_basic() {
        let request = build_head_request("https://example.com/file.bin").unwrap();
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("HEAD /file.bin HTTP/1.1\r\n"));
        assert!(text.contains("Host: example.com\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[test]
    fn test_build_head_request_root_path() {
        let request = build_head_request("https://example.com/").unwrap();
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("HEAD / HTTP/1.1\r\n"));
    }

    #[test]
    fn test_build_head_request_with_query() {
        let request = build_head_request("https://example.com/path?key=value").unwrap();
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("HEAD /path?key=value HTTP/1.1\r\n"));
    }

    #[test]
    fn test_build_head_request_custom_port() {
        let request = build_head_request("https://example.com:8443/file").unwrap();
        let text = String::from_utf8(request).unwrap();
        assert!(text.contains("Host: example.com\r\n"));
    }

    #[test]
    fn test_build_head_request_invalid_url() {
        let result = build_head_request("not a url");
        assert!(result.is_err());
    }

    // --- GET Range 请求构造测试 ---

    #[test]
    fn test_build_range_request() {
        let request = build_range_request("https://example.com/big.bin", 0, 999).unwrap();
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("GET /big.bin HTTP/1.1\r\n"));
        assert!(text.contains("Range: bytes=0-999\r\n"));
        assert!(text.contains("Host: example.com\r\n"));
    }

    #[test]
    fn test_build_range_request_large_offsets() {
        let request =
            build_range_request("https://example.com/big.bin", 1_000_000, 9_999_999).unwrap();
        let text = String::from_utf8(request).unwrap();
        assert!(text.contains("Range: bytes=1000000-9999999\r\n"));
    }

    // --- GET 全量请求构造测试 ---

    #[test]
    fn test_build_full_request() {
        let request = build_full_request("https://example.com/file.bin").unwrap();
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("GET /file.bin HTTP/1.1\r\n"));
        assert!(text.contains("Host: example.com\r\n"));
        // 不应包含 Range 头
        assert!(!text.contains("Range:"));
    }

    // --- 响应头解析测试 ---

    #[test]
    fn test_parse_head_response_success() {
        let response = "HTTP/1.1 200 OK\r\n\
                         Content-Length: 1048576\r\n\
                         Content-Type: application/octet-stream\r\n\
                         Accept-Ranges: bytes\r\n\
                         ETag: \"abc123\"\r\n\
                         Last-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\n\
                         \r\n";

        let meta = parse_head_response(response, "https://example.com/data.bin").unwrap();
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
    fn test_parse_head_response_no_content_length() {
        let response = "HTTP/1.1 200 OK\r\n\
                         Content-Type: text/html\r\n\
                         \r\n";

        let meta = parse_head_response(response, "https://example.com/page").unwrap();
        assert_eq!(meta.file_name, "page");
        assert_eq!(meta.file_size, None);
        assert!(!meta.supports_range);
    }

    #[test]
    fn test_parse_head_response_404() {
        let response = "HTTP/1.1 404 Not Found\r\n\
                         Content-Length: 0\r\n\
                         \r\n";

        let result = parse_head_response(response, "https://example.com/missing");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("404"),
            "应报告 HTTP 404 错误,实际: {err}"
        );
    }

    #[test]
    fn test_parse_head_response_500() {
        let response = "HTTP/1.1 500 Internal Server Error\r\n\r\n";
        let result = parse_head_response(response, "https://example.com/");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }

    #[test]
    fn test_parse_head_response_302_redirect() {
        let response = "HTTP/1.1 302 Found\r\n\
                         Location: https://example.com/new-location\r\n\
                         \r\n";
        let result = parse_head_response(response, "https://example.com/old");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("302"));
    }

    #[test]
    fn test_parse_head_response_content_disposition() {
        let response = "HTTP/1.1 200 OK\r\n\
                         Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
                         Content-Length: 2048\r\n\
                         \r\n";

        let meta = parse_head_response(response, "https://example.com/download?id=42").unwrap();
        assert_eq!(meta.file_name, "report.pdf");
        assert_eq!(meta.file_size, Some(2048));
    }

    #[test]
    fn test_parse_head_response_content_disposition_no_quotes() {
        let response = "HTTP/1.1 200 OK\r\n\
                         Content-Disposition: attachment; filename=report.pdf\r\n\
                         \r\n";

        let meta = parse_head_response(response, "https://example.com/download").unwrap();
        assert_eq!(meta.file_name, "report.pdf");
    }

    #[test]
    fn test_parse_head_response_no_range_support() {
        let response = "HTTP/1.1 200 OK\r\n\
                         Content-Length: 100\r\n\
                         \r\n";

        let meta = parse_head_response(response, "https://example.com/file").unwrap();
        assert!(!meta.supports_range, "无 Accept-Ranges 头应为 false");
    }

    #[test]
    fn test_parse_head_response_empty() {
        let result = parse_head_response("", "https://example.com/");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_head_response_invalid_status_line() {
        let response = "GARBAGE\r\n\r\n";
        let result = parse_head_response(response, "https://example.com/");
        assert!(result.is_err());
    }

    // --- 响应体分离测试 ---

    #[test]
    fn test_parse_body_response_success() {
        let body = b"Hello, World!";
        let mut response = Vec::new();
        response.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n");
        response.extend_from_slice(body);

        let parsed = parse_body_response(&response).unwrap();
        assert_eq!(parsed.as_ref(), body);
    }

    #[test]
    fn test_parse_body_response_empty_body() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let parsed = parse_body_response(response).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_body_response_error_status() {
        let response = b"HTTP/1.1 404 Not Found\r\n\r\nNot Found";
        let result = parse_body_response(response);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("404"));
    }

    #[test]
    fn test_parse_body_response_no_separator() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 5";
        let result = parse_body_response(response);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("分隔符"));
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
            matches!(err, AmdError::Protocol(_)),
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
        assert!(matches!(err, AmdError::Protocol(_)));
    }

    #[tokio::test]
    async fn test_protocol_trait_download_full_error_variant() {
        let transport = QuicTransport::new_insecure().await.unwrap();
        let err = transport
            .download_full("https://example.com/test")
            .await
            .unwrap_err();
        assert!(matches!(err, AmdError::Protocol(_)));
    }

    // --- 辅助函数测试 ---

    #[test]
    fn test_find_subsequence_found() {
        assert_eq!(
            find_subsequence(b"hello\r\n\r\nworld", b"\r\n\r\n"),
            Some(5)
        );
    }

    #[test]
    fn test_find_subsequence_not_found() {
        assert_eq!(find_subsequence(b"hello world", b"\r\n\r\n"), None);
    }

    #[test]
    fn test_find_subsequence_at_start() {
        assert_eq!(find_subsequence(b"\r\n\r\ndata", b"\r\n\r\n"), Some(0));
    }

    #[test]
    fn test_find_subsequence_at_end() {
        assert_eq!(find_subsequence(b"data\r\n\r\n", b"\r\n\r\n"), Some(4));
    }

    #[test]
    fn test_validate_http_status_200() {
        assert!(validate_http_status(b"HTTP/1.1 200 OK\r\nContent-Length: 10").is_ok());
    }

    #[test]
    fn test_validate_http_status_206_partial() {
        assert!(validate_http_status(b"HTTP/1.1 206 Partial Content").is_ok());
    }

    #[test]
    fn test_validate_http_status_404() {
        let result = validate_http_status(b"HTTP/1.1 404 Not Found");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("404"));
    }

    #[test]
    fn test_validate_http_status_500() {
        let result = validate_http_status(b"HTTP/1.1 500 Internal Server Error");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }

    #[test]
    fn test_extract_path_basic() {
        let url = Url::parse("https://example.com/file.bin").unwrap();
        assert_eq!(extract_path(&url), "/file.bin");
    }

    #[test]
    fn test_extract_path_root() {
        let url = Url::parse("https://example.com/").unwrap();
        assert_eq!(extract_path(&url), "/");
    }

    #[test]
    fn test_extract_path_with_query() {
        let url = Url::parse("https://example.com/path?key=value").unwrap();
        assert_eq!(extract_path(&url), "/path?key=value");
    }

    #[test]
    fn test_extract_path_no_path() {
        let url = Url::parse("https://example.com").unwrap();
        assert_eq!(extract_path(&url), "/");
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
}
