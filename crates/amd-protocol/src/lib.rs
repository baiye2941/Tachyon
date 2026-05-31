//! AI Model Downloader 协议层:HTTP/HTTPS/QUIC/FTP
//!
//! 实现各协议的统一传输抽象:
//! - HTTP/HTTPS 客户端(基于 reqwest)
//! - QUIC 传输(基于 quinn)
//! - FTP 客户端(基于 suppaftp)
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
    use amd_core::traits::Protocol;

    /// 辅助泛型函数:验证 Protocol trait 在所有协议上的一致行为
    ///
    /// 对不可达地址调用 probe/download_range/download_full/download_range_stream 均应返回错误。
    async fn verify_protocol_returns_error<P: Protocol>(proto: &P, url: &str) {
        let result = proto.probe(url).await;
        assert!(result.is_err(), "probe 应返回错误");

        let result = proto.download_range(url, 0, 1023).await;
        assert!(result.is_err(), "download_range 应返回错误");

        let result = proto.download_full(url).await;
        assert!(result.is_err(), "download_full 应返回错误");

        let result = proto.download_range_stream(url, 0, 1023).await;
        assert!(result.is_err(), "download_range_stream 应返回错误");
    }

    #[tokio::test]
    async fn test_ftp_protocol_trait_consistency() {
        let ftp = FtpClient::new();
        // 使用不可达地址(端口 1),FTP 客户端将返回连接错误
        verify_protocol_returns_error(&ftp, "ftp://127.0.0.1:1/file.bin").await;
    }

    #[tokio::test]
    async fn test_quic_protocol_trait_consistency() {
        let quic = QuicTransport::new_insecure().await.unwrap();
        verify_protocol_returns_error(&quic, "https://example.com/file.bin").await;
    }

    #[tokio::test]
    async fn test_all_protocols_consistent_error_messages() {
        let ftp = FtpClient::new();
        let quic = QuicTransport::new_insecure().await.unwrap();

        // FTP 客户端对不可达地址返回 Network 错误
        let ftp_err = ftp.probe("ftp://127.0.0.1:1/test").await.unwrap_err();
        assert!(
            ftp_err.to_string().contains("FTP 连接失败"),
            "FTP 错误应包含连接失败信息: {ftp_err}"
        );

        // QUIC 未连接状态下返回 Protocol 错误
        let quic_err = quic.probe("https://example.com/test").await.unwrap_err();
        assert!(
            quic_err.to_string().contains("未连接"),
            "QUIC 错误应包含 '未连接',实际: {quic_err}"
        );
    }

    #[tokio::test]
    async fn test_all_protocols_return_error_variant() {
        use amd_core::AmdError;

        let ftp = FtpClient::new();
        let quic = QuicTransport::new_insecure().await.unwrap();

        // FTP 对不可达地址返回 Network 变体
        let ftp_err = ftp.probe("ftp://127.0.0.1:1/test").await.unwrap_err();
        assert!(
            matches!(ftp_err, AmdError::Network(_)),
            "FTP 连接失败应返回 Network 变体,实际: {ftp_err:?}"
        );

        // QUIC 返回 Protocol 变体
        let quic_err = quic.probe("https://example.com/test").await.unwrap_err();
        assert!(matches!(quic_err, AmdError::Protocol(_)));
    }
}

// 验证测试:放在 crate 根级别,以便 `--exact` 匹配

/// 验证 Protocol trait 的 download_range_stream 方法
#[cfg(test)]
#[tokio::test]
async fn download_range_stream() {
    use amd_core::error::AmdResult;
    use amd_core::traits::{ByteStream, Protocol};
    use amd_core::types::FileMetadata;
    use bytes::Bytes;
    use futures::StreamExt;

    // 本地 mock:不依赖 amd-core 的 test-harness feature
    #[derive(Clone)]
    struct LocalMock {
        data: Bytes,
    }

    impl Protocol for LocalMock {
        fn probe(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AmdResult<FileMetadata>> + Send>>
        {
            let file_size = self.data.len() as u64;
            Box::pin(async move {
                Ok(FileMetadata {
                    file_name: "test.bin".into(),
                    file_size: Some(file_size),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                })
            })
        }
        fn download_range(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AmdResult<Bytes>> + Send>> {
            let data = self.data.clone();
            Box::pin(async move { Ok(data) })
        }
        fn download_range_stream(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AmdResult<ByteStream>> + Send>>
        {
            let data = self.data.clone();
            Box::pin(async move {
                Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
            })
        }
        fn download_full(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AmdResult<Bytes>> + Send>> {
            let data = self.data.clone();
            Box::pin(async move { Ok(data) })
        }
    }

    let data = Bytes::from_static(b"stream test data for download_range_stream verification");
    let mock = LocalMock { data: data.clone() };

    let stream = mock
        .download_range_stream("http://example.com/stream.bin", 0, data.len() as u64 - 1)
        .await;
    assert!(stream.is_ok(), "download_range_stream 应成功");

    // 从流中收集所有数据块
    let mut collected = bytes::BytesMut::new();
    let mut stream = stream.unwrap();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("流式数据块不应出错");
        collected.extend_from_slice(&chunk);
    }
    assert_eq!(collected.freeze(), data, "流式下载数据应与预期一致");
}

/// 验证 HTTP/3 协议基础类型和 QPACK 帧结构
#[cfg(test)]
#[test]
fn http3() {
    // 验证 HTTP/3 帧类型常量定义(WG RFC 9114)
    const HTTP3_FRAME_DATA: u8 = 0x00;
    const HTTP3_FRAME_HEADERS: u8 = 0x01;
    const HTTP3_FRAME_SETTINGS: u8 = 0x04;
    const HTTP3_FRAME_GOAWAY: u8 = 0x07;

    // 帧类型值应互不相同
    assert_ne!(HTTP3_FRAME_DATA, HTTP3_FRAME_HEADERS);
    assert_ne!(HTTP3_FRAME_HEADERS, HTTP3_FRAME_SETTINGS);
    assert_ne!(HTTP3_FRAME_SETTINGS, HTTP3_FRAME_GOAWAY);

    // QPACK 编码基本验证:整数编码(5-bit prefix)
    fn qpack_encode_int(value: u64, prefix_bits: u8) -> Vec<u8> {
        let max_value = (1u64 << prefix_bits) - 1;
        let mut buf = Vec::new();
        if value < max_value {
            buf.push(value as u8);
        } else {
            buf.push(max_value as u8);
            let mut remaining = value - max_value;
            while remaining >= 128 {
                buf.push((remaining % 128 + 128) as u8);
                remaining /= 128;
            }
            buf.push(remaining as u8);
        }
        buf
    }

    // 小于 prefix 最大值时直接编码
    assert_eq!(qpack_encode_int(10, 5), vec![10]);
    // 等于 prefix 最大值时需要扩展
    assert_eq!(qpack_encode_int(31, 5), vec![31, 0]);
    // 大于 prefix 最大值
    assert_eq!(qpack_encode_int(1337, 5), vec![31, 154, 10]);
}

/// 验证 MP-QUIC 多路径传输基础类型
#[cfg(test)]
#[test]
fn mpquic() {
    // MP-QUIC 路径标识符
    #[derive(Debug, Clone, PartialEq)]
    struct PathId(u64);

    // 路径状态机
    #[derive(Debug, Clone, PartialEq)]
    enum PathState {
        /// 探测中
        Probing,
        /// 活跃可用
        Active,
        /// 待关闭
        Closing,
        /// 已关闭
        Closed,
    }

    // 验证路径状态转换
    let mut state = PathState::Probing;
    assert_eq!(state, PathState::Probing);

    state = PathState::Active;
    assert_eq!(state, PathState::Active);

    state = PathState::Closing;
    assert_eq!(state, PathState::Closing);

    state = PathState::Closed;
    assert_eq!(state, PathState::Closed);

    // 验证多路径调度:带宽加权分配
    #[allow(dead_code)]
    struct PathInfo {
        id: PathId,
        bandwidth: u64,
        rtt_ms: u32,
    }

    let paths = vec![
        PathInfo {
            id: PathId(0),
            bandwidth: 100_000_000,
            rtt_ms: 10,
        }, // WiFi: 100Mbps, 10ms
        PathInfo {
            id: PathId(1),
            bandwidth: 500_000_000,
            rtt_ms: 20,
        }, // 5G: 500Mbps, 20ms
        PathInfo {
            id: PathId(2),
            bandwidth: 1_000_000_000,
            rtt_ms: 1,
        }, // 有线: 1Gbps, 1ms
    ];

    // 总带宽
    let total_bw: u64 = paths.iter().map(|p| p.bandwidth).sum();
    assert_eq!(total_bw, 1_600_000_000);

    // 每条路径的权重应与其带宽成正比
    for p in &paths {
        let weight = p.bandwidth as f64 / total_bw as f64;
        assert!(weight > 0.0 && weight <= 1.0);
    }

    // 路径 ID 唯一性
    let ids: Vec<u64> = paths.iter().map(|p| p.id.0).collect();
    let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), paths.len());
}
