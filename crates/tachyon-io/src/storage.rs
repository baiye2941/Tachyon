//! 异步存储抽象

use std::future::Future;
use std::pin::Pin;

use bytes::{Bytes, BytesMut};

use tachyon_core::{DownloadError, DownloadResult};

pub trait AsyncStorage: Send + Sync {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>>;

    /// 写入 BytesMut 数据（避免 freeze() 产生额外复制）
    ///
    /// P1-05: 默认实现复制一份 `Bytes` 后调用 `write_at`，后端应覆盖此方法
    /// 以直接从 `BytesMut` 内部缓冲区写入，避免复制/原子引用计数开销。
    ///
    /// 语义约定：
    /// - 方法返回实际写入的字节数 `n`，`data` 本身不会被修改。
    /// - 调用方需根据返回值自行 `data.advance(n)`，以处理短写循环。
    fn write_at_mut<'a>(
        &'a self,
        offset: u64,
        data: &'a mut BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        // 默认实现通过复制构造 Bytes，避免将 &mut BytesMut 的借用带入 async 块。
        // 后端覆盖时应直接读取 data 的连续内存，且不消费 data。
        let frozen = Bytes::copy_from_slice(data);
        Box::pin(async move { self.write_at(offset, frozen).await })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>>;

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>>;

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    /// 对齐写入：自动处理 offset 和 data 的对齐填充。
    ///
    /// 4.1: 为 WinFile NO_BUFFERING 和 IoUringStorage O_DIRECT 等需要对齐的后端
    /// 提供统一的对齐写入 API。默认实现通过填充零字节将 offset 向下对齐、
    /// data 向上对齐到 `alignment` 边界，然后委托给 `write_at`。
    ///
    /// - `alignment` 必须为 2 的幂（典型值：512 扇区 / 4096 页）
    /// - 返回实际写入的用户数据字节数（等于 `data.len()`）
    fn write_at_aligned<'a>(
        &'a self,
        offset: u64,
        data: &'a [u8],
        alignment: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            if alignment == 0 || !alignment.is_power_of_two() {
                return Err(DownloadError::Config(format!(
                    "alignment 必须为 2 的正整数幂, 实际值: {alignment}"
                )));
            }

            if data.is_empty() {
                return Ok(0);
            }

            let align_mask = alignment - 1;

            // 1. 将 offset 向下对齐到 alignment 边界
            let aligned_offset = offset & !align_mask;
            let front_pad = (offset - aligned_offset) as usize;

            // 2. 计算总填充大小（前端 + 数据 + 后端对齐）
            let total_len = front_pad + data.len();
            let padded_len = ((total_len as u64 + align_mask) & !align_mask) as usize;

            // 3. 构造对齐的写入缓冲区
            let mut padded = vec![0u8; padded_len];
            padded[front_pad..front_pad + data.len()].copy_from_slice(data);

            // 4. 委托给 write_at
            let written = self.write_at(aligned_offset, Bytes::from(padded)).await?;

            // 5. 返回用户数据的实际长度（而非填充后的长度）
            let user_written = written.saturating_sub(front_pad).min(data.len());
            Ok(user_written)
        })
    }
}

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
