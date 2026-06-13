//! 测试辅助工具
//!
//! 提供 TestHarness 结构体,封装 mock 依赖和 fixture

#[cfg(any(test, feature = "test-harness"))]
pub mod harness {
    use bytes::Bytes;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use std::future::Future;
    use std::pin::Pin;

    use crate::config::{DownloadConfig, IoStrategy};
    use crate::error::{DownloadError, DownloadResult};
    use crate::traits::{Protocol, Storage};
    use crate::types::{FileMetadata, FragmentInfo, TaskId};

    /// Mock 协议实现,用于测试
    #[derive(Clone)]
    pub struct MockProtocol {
        metadata: Option<FileMetadata>,
        /// L-16: 保留原始 DownloadError 的关键信息。
        /// 对可 Clone 的变体(Network/Protocol/Fragment/Config/Cancelled 等)直接保留;
        /// 对不可 Clone 的变体(Io/Other)转为 Network(error.to_string()),
        /// 保留错误描述但不保留原始类型(因 DownloadError 未 derive Clone)。
        preserved_error: Option<PreservedError>,
        pub range_data: Arc<Mutex<HashMap<(u64, u64), Bytes>>>,
        /// 全量下载数据(download_full 的返回值)
        default_data: Option<Bytes>,
    }

    /// L-16: 保留 MockProtocol 中原始 DownloadError 的可 Clone 部分。
    /// 可 Clone 变体完整保留(包括 ChecksumMismatch 的 expected/actual 字段);
    /// 不可 Clone 变体(Io/Other)降级为 Network(string)。
    ///
    /// TODO: 考虑给 DownloadError 自定义 Clone 实现(对 Io/Other/UrlParse/Serialization
    /// 做降级 clone),替代此镜像枚举。当前方案的优势是穷尽 match 保证:当 DownloadError
    /// 新增变体时,`from_download_error` 编译报错,强制同步更新。
    #[derive(Clone, Debug)]
    enum PreservedError {
        Network(String),
        Protocol(String),
        Fragment(String),
        ChecksumMismatch {
            expected: String,
            actual: String,
        },
        NoExpectedChecksum,
        Config(String),
        Cancelled,
        TaskNotFound(String),
        ConnectionPoolExhausted,
        Timeout(String),
        Throttled {
            retry_after_secs: Option<u64>,
        },
        Forbidden {
            status: u16,
        },
        Http {
            status: u16,
            reason: String,
        },
        /// Io/Other 等不可 Clone 变体的降级表示
        DowngradedNetwork(String),
    }

    impl PreservedError {
        fn from_download_error(err: &DownloadError) -> Self {
            match err {
                DownloadError::Network(s) => PreservedError::Network(s.clone()),
                DownloadError::Protocol(s) => PreservedError::Protocol(s.clone()),
                DownloadError::Fragment(s) => PreservedError::Fragment(s.clone()),
                DownloadError::ChecksumMismatch { expected, actual } => {
                    PreservedError::ChecksumMismatch {
                        expected: expected.clone(),
                        actual: actual.clone(),
                    }
                }
                DownloadError::NoExpectedChecksum => PreservedError::NoExpectedChecksum,
                DownloadError::Config(s) => PreservedError::Config(s.clone()),
                DownloadError::Cancelled => PreservedError::Cancelled,
                DownloadError::TaskNotFound(s) => PreservedError::TaskNotFound(s.clone()),
                DownloadError::ConnectionPoolExhausted => PreservedError::ConnectionPoolExhausted,
                DownloadError::Timeout(s) => PreservedError::Timeout(s.clone()),
                DownloadError::Throttled { retry_after_secs } => PreservedError::Throttled {
                    retry_after_secs: *retry_after_secs,
                },
                DownloadError::Forbidden { status } => {
                    PreservedError::Forbidden { status: *status }
                }
                DownloadError::Http { status, reason } => PreservedError::Http {
                    status: *status,
                    reason: reason.clone(),
                },
                // 不可 Clone 的变体降级为 Network
                DownloadError::Io(_)
                | DownloadError::Other(_)
                | DownloadError::UrlParse(_)
                | DownloadError::Serialization(_) => {
                    PreservedError::DowngradedNetwork(err.to_string())
                }
            }
        }

        fn to_download_error(&self) -> DownloadError {
            match self {
                PreservedError::Network(s) => DownloadError::Network(s.clone()),
                PreservedError::Protocol(s) => DownloadError::Protocol(s.clone()),
                PreservedError::Fragment(s) => DownloadError::Fragment(s.clone()),
                PreservedError::ChecksumMismatch { expected, actual } => {
                    DownloadError::ChecksumMismatch {
                        expected: expected.clone(),
                        actual: actual.clone(),
                    }
                }
                PreservedError::NoExpectedChecksum => DownloadError::NoExpectedChecksum,
                PreservedError::Config(s) => DownloadError::Config(s.clone()),
                PreservedError::Cancelled => DownloadError::Cancelled,
                PreservedError::TaskNotFound(s) => DownloadError::TaskNotFound(s.clone()),
                PreservedError::ConnectionPoolExhausted => DownloadError::ConnectionPoolExhausted,
                PreservedError::Timeout(s) => DownloadError::Timeout(s.clone()),
                PreservedError::Throttled { retry_after_secs } => DownloadError::Throttled {
                    retry_after_secs: *retry_after_secs,
                },
                PreservedError::Forbidden { status } => {
                    DownloadError::Forbidden { status: *status }
                }
                PreservedError::Http { status, reason } => DownloadError::Http {
                    status: *status,
                    reason: reason.clone(),
                },
                PreservedError::DowngradedNetwork(s) => DownloadError::Network(s.clone()),
            }
        }
    }

    impl MockProtocol {
        pub fn new(metadata: FileMetadata) -> Self {
            Self {
                metadata: Some(metadata),
                preserved_error: None,
                range_data: Arc::new(Mutex::new(HashMap::new())),
                default_data: None,
            }
        }

        pub fn with_range_data(self, start: u64, end: u64, data: Bytes) -> Self {
            self.range_data.lock().unwrap().insert((start, end), data);
            self
        }

        /// 设置全量下载数据(不支持 Range 时使用)
        pub fn with_default_data(self, data: Bytes) -> Self {
            Self {
                default_data: Some(data),
                ..self
            }
        }

        /// 创建一个总是失败的 MockProtocol。
        ///
        /// L-16: 保留原始 DownloadError 的关键信息(变体类型 + 附加字段)。
        /// 对可 Clone 的变体(如 ChecksumMismatch)完整保留 expected/actual 字段;
        /// 对不可 Clone 的变体(Io/Other)降级为 Network(string)但保留描述。
        pub fn failing(error: DownloadError) -> Self {
            Self {
                metadata: None,
                preserved_error: Some(PreservedError::from_download_error(&error)),
                range_data: Arc::new(Mutex::new(HashMap::new())),
                default_data: None,
            }
        }
    }

    impl Protocol for MockProtocol {
        fn probe(
            &self,
            _url: &str,
        ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>>
        {
            let this = self.clone();
            Box::pin(async move {
                if let Some(ref meta) = this.metadata {
                    Ok(meta.clone())
                } else if let Some(ref preserved) = this.preserved_error {
                    // L-16: 从保留的错误信息重建 DownloadError,保留原始变体类型
                    Err(preserved.to_download_error())
                } else {
                    Err(DownloadError::Network("mock 协议未配置".into()))
                }
            })
        }

        fn download_range(
            &self,
            _url: &str,
            start: u64,
            end: u64,
        ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
            let this = self.clone();
            Box::pin(async move {
                let data = this.range_data.lock().unwrap();
                data.get(&(start, end))
                    .cloned()
                    .ok_or_else(|| DownloadError::Network(format!("未找到范围数据: {start}-{end}")))
            })
        }

        fn download_range_stream(
            &self,
            url: &str,
            start: u64,
            end: u64,
        ) -> Pin<
            Box<dyn std::future::Future<Output = DownloadResult<crate::traits::ByteStream>> + Send>,
        > {
            let this = self.clone();
            let url = url.to_owned();
            Box::pin(async move {
                let data = this.download_range(&url, start, end).await?;
                Ok(Box::pin(futures::stream::once(async move { Ok(data) }))
                    as crate::traits::ByteStream)
            })
        }

        fn download_full(
            &self,
            _url: &str,
        ) -> Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
            let this = self.clone();
            Box::pin(async move {
                this.default_data
                    .clone()
                    .ok_or_else(|| DownloadError::Protocol("不支持全量下载".into()))
            })
        }
    }

    /// 内存存储实现,用于测试
    #[derive(Clone)]
    pub struct MemoryStorage {
        data: Arc<Mutex<Vec<u8>>>,
    }

    impl MemoryStorage {
        pub fn new() -> Self {
            Self {
                data: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub fn with_capacity(capacity: usize) -> Self {
            Self {
                data: Arc::new(Mutex::new(vec![0u8; capacity])),
            }
        }

        pub fn get_data(&self) -> Vec<u8> {
            self.data.lock().unwrap().clone()
        }
    }

    impl Default for MemoryStorage {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Storage for MemoryStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            Box::pin(async move {
                let mut buf = self.data.lock().unwrap();
                let start = offset as usize;
                let end = start + data.len();
                if end > buf.len() {
                    buf.resize(end, 0);
                }
                buf[start..end].copy_from_slice(&data);
                Ok(data.len())
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move {
                let data = self.data.lock().unwrap();
                let start = offset as usize;
                let available = data.len().saturating_sub(start);
                let to_read = buf.len().min(available);
                if to_read == 0 {
                    return Ok(0);
                }
                buf[..to_read].copy_from_slice(&data[start..start + to_read]);
                Ok(to_read)
            })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn allocate(
            &self,
            size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move {
                let mut data = self.data.lock().unwrap();
                data.resize(size as usize, 0);
                Ok(())
            })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            Box::pin(async move {
                let data = self.data.lock().unwrap();
                Ok(data.len() as u64)
            })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    /// 创建测试用的文件元数据
    pub fn test_metadata(file_name: &str, file_size: u64) -> FileMetadata {
        FileMetadata {
            file_name: file_name.to_string(),
            file_size: Some(file_size),
            content_type: Some("application/octet-stream".into()),
            supports_range: true,
            etag: Some("\"abc123\"".into()),
            last_modified: None,
        }
    }

    /// 创建测试用的分片列表
    pub fn test_fragments(total_size: u64, fragment_count: u32) -> Vec<FragmentInfo> {
        if fragment_count == 0 || total_size == 0 {
            return Vec::new();
        }
        // 确保每分片至少 1 字节
        let actual_count = (fragment_count as u64).min(total_size);
        let chunk_size = total_size / actual_count;
        let remainder = total_size % actual_count;
        (0..actual_count as u32)
            .map(|i| {
                let i = i as u64;
                let extra = if i < remainder { 1 } else { 0 };
                let start = i * chunk_size + i.min(remainder);
                let size = chunk_size + extra;
                let end = start + size - 1;
                FragmentInfo {
                    index: i as u32,
                    start,
                    end,
                    size,
                    downloaded: 0,
                    hash: None,
                }
            })
            .collect()
    }

    /// 创建测试用的默认下载配置
    pub fn test_config() -> DownloadConfig {
        DownloadConfig {
            download_dir: std::env::temp_dir().to_string_lossy().to_string(),
            max_concurrent_fragments: 4,
            max_retries: 3,
            request_timeout_secs: 10,
            connect_timeout_secs: 10,
            verify_checksum: false,
            user_agent: "Tachyon-Test/0.1.0".into(),
            headers: HashMap::new(),
            pause_timeout_secs: 300,
            rate_limit_bytes_per_sec: None,
            max_full_stream_bytes: crate::config::default_max_full_stream_bytes(),
            authorized_dirs: vec![std::env::temp_dir().to_string_lossy().to_string()],
            io_strategy: IoStrategy::default(),
        }
    }

    /// 创建测试用的任务 ID
    pub fn test_task_id() -> TaskId {
        use uuid::Uuid;
        Uuid::from_bytes([0u8; 16])
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::harness::*;
    use crate::error::DownloadError;
    use crate::traits::Protocol;
    use crate::traits::Storage;

    #[test]
    fn test_metadata_creation() {
        let meta = test_metadata("test.bin", 1024);
        assert_eq!(meta.file_name, "test.bin");
        assert_eq!(meta.file_size, Some(1024));
        assert!(meta.supports_range);
    }

    #[test]
    fn test_fragments_creation() {
        let frags = test_fragments(100, 4);
        assert_eq!(frags.len(), 4);
        assert_eq!(frags[0].start, 0);
        assert_eq!(frags[0].size, 25);
        assert_eq!(frags[3].end, 99);
    }

    #[test]
    fn test_fragments_single() {
        let frags = test_fragments(500, 1);
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].start, 0);
        assert_eq!(frags[0].end, 499);
        assert_eq!(frags[0].size, 500);
    }

    #[test]
    fn test_fragments_empty() {
        let frags = test_fragments(0, 0);
        assert!(frags.is_empty());
    }

    #[tokio::test]
    async fn test_mock_protocol_probe() {
        let meta = test_metadata("file.zip", 2048);
        let protocol = MockProtocol::new(meta);
        let result = protocol.probe("http://example.com/file.zip").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().file_name, "file.zip");
    }

    #[tokio::test]
    async fn test_mock_protocol_download_range() {
        let meta = test_metadata("file.bin", 100);
        let data = Bytes::from_static(b"hello world");
        let protocol = MockProtocol::new(meta).with_range_data(0, 10, data.clone());
        let result = protocol
            .download_range("http://example.com/file.bin", 0, 10)
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), data);
    }

    #[tokio::test]
    async fn test_mock_protocol_missing_range() {
        let meta = test_metadata("file.bin", 100);
        let protocol = MockProtocol::new(meta);
        let result = protocol
            .download_range("http://example.com/file.bin", 0, 10)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_protocol_failing() {
        let protocol = MockProtocol::failing(DownloadError::Network("连接超时".into()));
        let result = protocol.probe("http://example.com/file.bin").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_memory_storage_write_read() {
        let storage = MemoryStorage::new();
        let written = storage
            .write_at(0, Bytes::from_static(b"hello"))
            .await
            .unwrap();
        assert_eq!(written, 5);

        let mut buf = [0u8; 5];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 5);
        assert_eq!(&buf, b"hello");
    }

    #[tokio::test]
    async fn test_memory_storage_write_offset() {
        let storage = MemoryStorage::new();
        storage
            .write_at(0, Bytes::from_static(b"AAAA"))
            .await
            .unwrap();
        storage
            .write_at(4, Bytes::from_static(b"BBBB"))
            .await
            .unwrap();

        let mut buf = [0u8; 8];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 8);
        assert_eq!(&buf, b"AAAABBBB");
    }

    #[tokio::test]
    async fn test_memory_storage_allocate() {
        let storage = MemoryStorage::new();
        storage.allocate(1024).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 1024);
    }

    #[tokio::test]
    async fn test_memory_storage_sync() {
        let storage = MemoryStorage::new();
        assert!(storage.sync().await.is_ok());
    }

    #[tokio::test]
    async fn test_memory_storage_read_past_end() {
        let storage = MemoryStorage::new();
        storage
            .write_at(0, Bytes::from_static(b"abc"))
            .await
            .unwrap();
        let mut buf = [0u8; 10];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 3);
    }

    #[test]
    fn test_config_defaults() {
        let config = test_config();
        assert_eq!(config.max_concurrent_fragments, 4);
        assert_eq!(config.max_retries, 3);
        assert!(!config.verify_checksum);
    }

    #[test]
    fn test_task_id() {
        use uuid::Uuid;
        let id = Uuid::from_bytes([0u8; 16]);
        assert_eq!(id.as_bytes(), &[0u8; 16]);
    }
}
