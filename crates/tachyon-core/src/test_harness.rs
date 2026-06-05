//! 测试辅助工具
//!
//! 提供 TestHarness 结构体,封装 mock 依赖和 fixture

#[cfg(any(test, feature = "test-harness"))]
pub mod harness {
    use bytes::Bytes;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use std::pin::Pin;

    use crate::config::DownloadConfig;
    use crate::error::{DownloadError, DownloadResult};
    use crate::traits::{Protocol, Storage};
    use crate::types::{FileMetadata, FragmentInfo, TaskId};

    /// Mock 协议实现,用于测试
    #[derive(Clone)]
    pub struct MockProtocol {
        metadata: Option<FileMetadata>,
        error_msg: Option<String>,
        pub range_data: Arc<Mutex<HashMap<(u64, u64), Bytes>>>,
        /// 全量下载数据(download_full 的返回值)
        default_data: Option<Bytes>,
    }

    impl MockProtocol {
        pub fn new(metadata: FileMetadata) -> Self {
            Self {
                metadata: Some(metadata),
                error_msg: None,
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

        pub fn failing(error: DownloadError) -> Self {
            Self {
                metadata: None,
                error_msg: Some(error.to_string()),
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
                } else {
                    Err(DownloadError::Network(
                        this.error_msg.clone().unwrap_or_default(),
                    ))
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
        async fn write_at(&self, offset: u64, data: Bytes) -> DownloadResult<usize> {
            let mut buf = self.data.lock().unwrap();
            let start = offset as usize;
            let end = start + data.len();
            if end > buf.len() {
                buf.resize(end, 0);
            }
            buf[start..end].copy_from_slice(&data);
            Ok(data.len())
        }

        async fn read_at(&self, offset: u64, buf: &mut [u8]) -> DownloadResult<usize> {
            let data = self.data.lock().unwrap();
            let start = offset as usize;
            let available = data.len().saturating_sub(start);
            let to_read = buf.len().min(available);
            if to_read == 0 {
                return Ok(0);
            }
            buf[..to_read].copy_from_slice(&data[start..start + to_read]);
            Ok(to_read)
        }

        async fn sync(&self) -> DownloadResult<()> {
            Ok(())
        }

        async fn allocate(&self, size: u64) -> DownloadResult<()> {
            let mut data = self.data.lock().unwrap();
            data.resize(size as usize, 0);
            Ok(())
        }

        async fn file_size(&self) -> DownloadResult<u64> {
            let data = self.data.lock().unwrap();
            Ok(data.len() as u64)
        }

        async fn close(&self) -> DownloadResult<()> {
            Ok(())
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
            authorized_dirs: vec![std::env::temp_dir().to_string_lossy().to_string()],
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
