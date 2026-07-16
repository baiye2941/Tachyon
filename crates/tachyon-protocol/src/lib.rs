//! Tachyon 协议层:HTTP/HTTPS/BitTorrent
//!
//! 实现各协议的统一传输抽象:
//! - HTTP/HTTPS 客户端(基于 reqwest)
//! - BitTorrent 磁力链接(基于 librqbit)
//! - 统一 Protocol trait

pub mod hls;
pub mod http;
#[cfg(feature = "magnet")]
pub mod magnet;

pub use http::{HttpClient, effective_quic_enabled, http3_compiled};
#[cfg(feature = "magnet")]
pub use magnet::BtPeerStats;
#[cfg(feature = "magnet")]
pub use magnet::MagnetProtocol;

// 验证测试:放在 crate 根级别,以便 `--exact` 匹配

/// 验证 Protocol trait 的 download_range_stream 方法
#[cfg(test)]
#[tokio::test]
async fn download_range_stream() {
    use bytes::Bytes;
    use futures::StreamExt;
    use tachyon_core::error::DownloadResult;
    use tachyon_core::traits::{ByteStream, Protocol};
    use tachyon_core::types::FileMetadata;

    // 本地 mock:不依赖 tachyon-core 的 test-harness feature
    #[derive(Clone)]
    struct LocalMock {
        data: Bytes,
    }

    impl Protocol for LocalMock {
        fn probe(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>>
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
                    file_layout: None,
                    protocol_managed_storage: false,
                    resolved_host: None,
                })
            })
        }
        fn download_range(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
            _identity: Option<tachyon_core::ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            let data = self.data.clone();
            Box::pin(async move { Ok(data) })
        }
        fn download_range_stream(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
            _identity: Option<tachyon_core::ObjectIdentity>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
        {
            let data = self.data.clone();
            Box::pin(async move {
                Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
            })
        }
        fn download_full(
            &self,
            _url: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>>
        {
            let data = self.data.clone();
            Box::pin(async move { Ok(data) })
        }
    }

    let data = Bytes::from_static(b"stream test data for download_range_stream verification");
    let mock = LocalMock { data: data.clone() };

    let stream = mock
        .download_range_stream(
            "http://example.com/stream.bin",
            0,
            data.len() as u64 - 1,
            None,
        )
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
