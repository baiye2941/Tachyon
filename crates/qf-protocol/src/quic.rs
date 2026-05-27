//! QUIC 传输实现
//!
//! 基于 quinn 的 QUIC 客户端,支持:
//! - 0-RTT 连接建立
//! - 自签名证书(测试环境)
//! - 多路径传输预留接口
//!
//! 当前为骨架实现,probe/download 方法返回 Protocol 错误,
//! 后续接入真实 QUIC 服务端后完成端到端逻辑。

use std::sync::Arc;

use bytes::Bytes;
use qf_core::traits::Protocol;
use qf_core::types::FileMetadata;
use qf_core::{QfError, QfResult};
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
    pub async fn new() -> QfResult<Self> {
        // 安装 ring 加密提供器(幂等操作)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // 生成自签名证书
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .map_err(|e| QfError::Network(format!("生成自签名证书失败: {e}")))?;

        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());

        // 构建客户端 TLS 配置,允许自签名证书(测试用)
        let mut crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureVerifier))
            .with_client_auth_cert(vec![cert_der], key.into())
            .map_err(|e| QfError::Network(format!("构建 TLS 配置失败: {e}")))?;

        crypto.alpn_protocols = vec![b"h3".to_vec()];

        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
            .map_err(|e| QfError::Network(format!("创建 QUIC 端点失败: {e}")))?;

        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
                .map_err(|e| QfError::Network(format!("构建 QUIC 客户端配置失败: {e}")))?,
        )));

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

    /// 连接到远程 QUIC 服务器
    ///
    /// 解析 URL 中的 host:port,建立 QUIC 连接并存储。
    pub async fn connect(&mut self, url: &str) -> QfResult<()> {
        let parsed = Url::parse(url).map_err(|e| QfError::Network(format!("URL 解析失败: {e}")))?;

        let host = parsed
            .host_str()
            .ok_or_else(|| QfError::Network("URL 缺少主机名".into()))?;
        let port = parsed.port().unwrap_or(443);
        let addr = format!("{host}:{port}");

        let addr: std::net::SocketAddr = tokio::net::lookup_host(addr)
            .await
            .map_err(|e| QfError::Network(format!("DNS 解析失败: {e}")))?
            .next()
            .ok_or_else(|| QfError::Network("DNS 解析无结果".into()))?;

        let connecting = self
            .endpoint
            .connect(addr, host)
            .map_err(|e| QfError::Network(format!("发起 QUIC 连接失败: {e}")))?;

        let connection = connecting
            .await
            .map_err(|e| QfError::Network(format!("QUIC 连接建立失败: {e}")))?;

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
}

impl Drop for QuicTransport {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// 不安全的证书验证器 -- 仅用于测试,接受任何证书
#[derive(Debug)]
struct InsecureVerifier;

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

impl Protocol for QuicTransport {
    async fn probe(&self, _url: &str) -> QfResult<FileMetadata> {
        // QUIC 传输尚未完全实现 -- 后续需实现 HTTP/3 语义的流式请求
        Err(QfError::Protocol("QUIC 传输尚未完全实现".into()))
    }

    async fn download_range(&self, _url: &str, _start: u64, _end: u64) -> QfResult<Bytes> {
        Err(QfError::Protocol("QUIC 传输尚未完全实现".into()))
    }

    async fn download_full(&self, _url: &str) -> QfResult<Bytes> {
        Err(QfError::Protocol("QUIC 传输尚未完全实现".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_quic_transport_creation() {
        let transport = QuicTransport::new().await;
        assert!(transport.is_ok(), "QuicTransport::new() 应成功创建");
    }

    #[tokio::test]
    async fn test_quic_transport_initially_disconnected() {
        let transport = QuicTransport::new().await.unwrap();
        assert!(!transport.is_connected(), "新创建的传输不应处于已连接状态");
    }

    #[tokio::test]
    async fn test_quic_transport_no_initial_connection() {
        let transport = QuicTransport::new().await.unwrap();
        assert!(
            transport.connection().is_none(),
            "新创建的传输不应有活跃连接"
        );
    }

    #[tokio::test]
    async fn test_quic_probe_returns_not_implemented() {
        let transport = QuicTransport::new().await.unwrap();
        let result = transport.probe("https://example.com/file.bin").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("QUIC 传输尚未完全实现"),
            "错误信息应包含 'QUIC 传输尚未完全实现',实际: {err}"
        );
    }

    #[tokio::test]
    async fn test_quic_download_range_returns_not_implemented() {
        let transport = QuicTransport::new().await.unwrap();
        let result = transport
            .download_range("https://example.com/file.bin", 0, 1023)
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("QUIC 传输尚未完全实现")
        );
    }

    #[tokio::test]
    async fn test_quic_download_full_returns_not_implemented() {
        let transport = QuicTransport::new().await.unwrap();
        let result = transport
            .download_full("https://example.com/file.bin")
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("QUIC 传输尚未完全实现")
        );
    }

    #[tokio::test]
    async fn test_quic_transport_disconnect_on_drop() {
        let transport = QuicTransport::new().await.unwrap();
        // 确保 Drop 不会 panic
        drop(transport);
    }

    #[tokio::test]
    async fn test_quic_with_endpoint() {
        let endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
        let transport = QuicTransport::with_endpoint(endpoint);
        assert!(!transport.is_connected());
    }
}
