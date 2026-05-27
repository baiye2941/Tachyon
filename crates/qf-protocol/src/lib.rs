//! QuantumFetch 协议层:HTTP/HTTPS/QUIC/FTP
//!
//! 实现各协议的统一传输抽象:
//! - HTTP/HTTPS 客户端(基于 reqwest)
//! - QUIC 传输(基于 quinn)
//! - FTP 客户端
//! - 统一 Protocol trait

pub mod ftp;
pub mod http;
pub mod quic;

pub use ftp::FtpClient;
pub use http::HttpClient;
pub use quic::QuicTransport;

/// 协议模块统一测试:验证三种协议的 Protocol trait 实现一致性
#[cfg(test)]
mod protocol_tests {
    use super::*;
    use qf_core::traits::Protocol;

    /// 辅助泛型函数:验证 Protocol trait 在所有协议上的一致行为
    async fn verify_protocol_stub<P: Protocol>(proto: &P, url: &str) {
        // probe 应返回错误
        let result = proto.probe(url).await;
        assert!(result.is_err(), "probe 应返回错误");

        // download_range 应返回错误
        let result = proto.download_range(url, 0, 1023).await;
        assert!(result.is_err(), "download_range 应返回错误");

        // download_full 应返回错误
        let result = proto.download_full(url).await;
        assert!(result.is_err(), "download_full 应返回错误");
    }

    #[tokio::test]
    async fn test_ftp_protocol_trait_consistency() {
        let ftp = FtpClient::new();
        verify_protocol_stub(&ftp, "ftp://example.com/file.bin").await;
    }

    #[tokio::test]
    async fn test_quic_protocol_trait_consistency() {
        let quic = QuicTransport::new().await.unwrap();
        verify_protocol_stub(&quic, "https://example.com/file.bin").await;
    }

    #[tokio::test]
    async fn test_all_protocols_consistent_error_messages() {
        let ftp = FtpClient::new();
        let quic = QuicTransport::new().await.unwrap();

        // 所有 stub 协议的 probe 都应返回 Protocol 错误
        let ftp_err = ftp.probe("ftp://example.com/test").await.unwrap_err();
        let quic_err = quic.probe("https://example.com/test").await.unwrap_err();

        assert!(ftp_err.to_string().contains("FTP 传输尚未完全实现"));
        assert!(quic_err.to_string().contains("QUIC 传输尚未完全实现"));
    }

    #[tokio::test]
    async fn test_all_protocols_return_protocol_error_variant() {
        use qf_core::QfError;

        let ftp = FtpClient::new();
        let quic = QuicTransport::new().await.unwrap();

        // 验证返回的都是 Protocol 变体
        let ftp_err = ftp.probe("ftp://example.com/test").await.unwrap_err();
        let quic_err = quic.probe("https://example.com/test").await.unwrap_err();

        assert!(matches!(ftp_err, QfError::Protocol(_)));
        assert!(matches!(quic_err, QfError::Protocol(_)));
    }
}
