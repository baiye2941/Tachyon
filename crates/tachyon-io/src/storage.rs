//! 异步存储抽象
//!
//! `AsyncStorage` trait 定义在 `tachyon-core::traits`(与 `Protocol`/`Verifier` 同层),
//! 此处通过重导出保持 `tachyon_io::storage::AsyncStorage` 路径向后兼容。
//! 这样 `tachyon-core` 的测试 harness `MemoryStorage` 可直接实现本 trait,
//! 无需 `tachyon-io` 向 `tachyon-core` 反向依赖。

pub use tachyon_core::traits::AsyncStorage;

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use bytes::{Bytes, BytesMut};

    use super::AsyncStorage;
    use tachyon_core::{DownloadError, DownloadResult};

    /// Mock 存储 write_at 调用日志。
    type WriteLog = Arc<Mutex<Vec<(u64, Bytes)>>>;

    /// 记录所有 write_at 调用的 Mock 存储。
    #[derive(Clone)]
    struct MockStorage {
        writes: WriteLog,
    }

    impl MockStorage {
        fn new() -> (Self, WriteLog) {
            let writes = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    writes: writes.clone(),
                },
                writes,
            )
        }
    }

    impl AsyncStorage for MockStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            let writes = self.writes.clone();
            Box::pin(async move {
                let len = data.len();
                writes.lock().unwrap().push((offset, data));
                Ok(len)
            })
        }

        fn read_at<'a>(
            &'a self,
            _offset: u64,
            _buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move { Ok(0) })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn allocate(
            &self,
            _size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            Box::pin(async move { Ok(0) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    #[tokio::test]
    async fn test_write_at_aligned_aligned_data() {
        let (storage, writes) = MockStorage::new();
        let data = [1, 2, 3, 4];
        let result = storage.write_at_aligned(0, &data, 4).await.unwrap();
        assert_eq!(result, 4);

        let captured = writes.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, 0);
        assert_eq!(&captured[0].1[..], &data);
    }

    #[tokio::test]
    async fn test_write_at_aligned_non_aligned_data() {
        let (storage, writes) = MockStorage::new();
        let data = [2, 3];
        let result = storage.write_at_aligned(1, &data, 4).await.unwrap();
        assert_eq!(result, 2);

        let captured = writes.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, 0);
        // offset=1 向下对齐到 0，front_pad=1，后端再填充 1 字节到 4 字节边界
        assert_eq!(&captured[0].1[..], &[0, 2, 3, 0]);
    }

    #[tokio::test]
    async fn test_write_at_aligned_zero_alignment() {
        let (storage, _writes) = MockStorage::new();
        let err = storage.write_at_aligned(0, b"data", 0).await.unwrap_err();
        match err {
            DownloadError::Config(msg) => assert!(msg.contains("alignment")),
            other => panic!("期望 Config 错误，实际: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_write_at_aligned_non_power_of_two_alignment() {
        let (storage, _writes) = MockStorage::new();
        let err = storage.write_at_aligned(0, b"data", 3).await.unwrap_err();
        match err {
            DownloadError::Config(msg) => assert!(msg.contains("alignment")),
            other => panic!("期望 Config 错误，实际: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_write_at_aligned_empty_data() {
        let (storage, writes) = MockStorage::new();
        let result = storage.write_at_aligned(0, b"", 512).await.unwrap();
        assert_eq!(result, 0);
        assert!(writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_write_at_aligned_return_value_semantics() {
        let (storage, _writes) = MockStorage::new();

        // 已对齐：offset 和 length 都是 alignment 的倍数，返回 data.len()
        let result = storage
            .write_at_aligned(512, &[1, 2, 3, 4], 512)
            .await
            .unwrap();
        assert_eq!(result, 4);

        // 非对齐 offset：内部有 front_pad，write_at 返回完整填充长度，
        // 最终应返回用户数据长度
        let result = storage.write_at_aligned(511, &[5, 6], 512).await.unwrap();
        assert_eq!(result, 2);
    }

    #[tokio::test]
    async fn test_write_at_mut_delegates_to_write_at() {
        let (storage, writes) = MockStorage::new();
        let mut data = BytesMut::from(&b"hello"[..]);
        let result = storage.write_at_mut(10, &mut data).await.unwrap();
        assert_eq!(result, 5);
        assert_eq!(&data[..], b"hello", "默认实现不应修改原始 BytesMut");

        let captured = writes.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, 10);
        assert_eq!(&captured[0].1[..], b"hello");
    }

    #[tokio::test]
    async fn test_async_storage_other_methods() {
        let (storage, _writes) = MockStorage::new();
        let mut buf = [0u8; 4];
        assert_eq!(storage.read_at(0, &mut buf).await.unwrap(), 0);
        assert!(storage.sync().await.is_ok());
        assert!(storage.allocate(1024).await.is_ok());
        assert_eq!(storage.file_size().await.unwrap(), 0);
        assert!(storage.close().await.is_ok());
    }
}
