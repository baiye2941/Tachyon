//! 测试辅助工具
//!
//! 提供 TestHarness 结构体,封装 mock 依赖和 fixture

#[cfg(any(test, feature = "test-harness"))]
pub mod harness {
    use bytes::{Bytes, BytesMut};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use std::future::Future;
    use std::pin::Pin;

    use crate::config::{DownloadConfig, IoStrategy};
    use crate::error::{DownloadError, DownloadResult};
    use crate::traits::{AsyncStorage, Protocol};
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
        /// 模拟"死 swarm"的区间:命中这些 (start,end) 的 download_range_stream
        /// 返回永不产出项的 pending 流(等价于 librqbit FileStream.read() 在无 peer
        /// 时永久 Pending),用于验证引擎流读取循环的取消信号穿透能力。
        stalling_ranges: Arc<Mutex<HashMap<(u64, u64), ()>>>,
        /// 分块流模式:Some(n) 时 download_range_stream 将 range 数据按 n 字节拆成
        /// 多个 chunk 流式产出(模拟 HTTP chunked / BT FileStream 多次 read),
        /// 覆盖引擎流读取循环的逐块哈希、批量刷写、取消信号穿透路径。
        /// None(默认)时整个 range 一次性产出(stream::once)。
        chunk_size: Option<usize>,
        /// 每个 chunk 产出前的延迟,模拟固定低带宽源(慢源)。
        /// 与 chunk_size 配合:chunk 越小 + delay 越大 = 带宽越低。
        /// 用于验证 MirrorProtocol 的质量加权选源、BT 回退的带宽阈值等错误注入场景。
        chunk_delay: Option<std::time::Duration>,
        /// 间歇失败注入:每次 download_range_stream 调用递增计数器,
        /// 命中指定次数时返回错误,模拟源不稳定(间歇 5xx/连接重置)。
        /// 配合重试逻辑验证 worker 的退避与切换。
        fail_on_attempt: Arc<Mutex<Option<(usize, PreservedError)>>>,
        attempt_counter: Arc<std::sync::atomic::AtomicUsize>,
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
                stalling_ranges: Arc::new(Mutex::new(HashMap::new())),
                chunk_size: None,
                chunk_delay: None,
                fail_on_attempt: Arc::new(Mutex::new(None)),
                attempt_counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        pub fn with_range_data(self, start: u64, end: u64, data: Bytes) -> Self {
            self.range_data.lock().unwrap().insert((start, end), data);
            self
        }

        /// 标记某区间为"死 swarm"区间:对该区间的 download_range_stream 返回
        /// 永不产出项的 pending 流,模拟磁力链接无 peer 时 FileStream.read() 永久挂起。
        /// 用于验证引擎流读取循环在死 swarm 下能否被取消信号穿透。
        pub fn with_stalling_range(self, start: u64, end: u64) -> Self {
            self.stalling_ranges
                .lock()
                .unwrap()
                .insert((start, end), ());
            self
        }

        /// 设置全量下载数据(不支持 Range 时使用)
        pub fn with_default_data(self, data: Bytes) -> Self {
            Self {
                default_data: Some(data),
                ..self
            }
        }

        /// 启用分块流模式:download_range_stream 将 range 数据按 `size` 字节拆成
        /// 多个 chunk 流式产出(模拟 HTTP chunked transfer / BT FileStream 多次 read)。
        ///
        /// 默认(None)时整个 range 一次性产出(stream::once)。设为 Some(n) 后,
        /// 引擎流读取循环会按 n 字节逐块接收,覆盖逐块哈希、批量刷写、取消信号穿透路径。
        pub fn with_chunk_size(self, size: usize) -> Self {
            assert!(size > 0, "chunk_size 必须大于 0");
            Self {
                chunk_size: Some(size),
                ..self
            }
        }

        /// 设置每个 chunk 产出前的延迟,模拟固定低带宽源(慢源)。
        ///
        /// 与 `with_chunk_size` 配合使用:chunk 越小 + delay 越大 = 带宽越低。
        /// 用于错误注入场景:验证 MirrorProtocol 的质量加权选源能否识别慢源、
        /// BT 回退的带宽阈值能否触发等。
        pub fn with_chunk_delay(self, delay: std::time::Duration) -> Self {
            Self {
                chunk_delay: Some(delay),
                ..self
            }
        }

        /// 间歇失败注入:第 `attempt` 次(从 1 开始)调用 download_range_stream 时
        /// 返回指定错误,模拟源不稳定(间歇 5xx/连接重置)。
        ///
        /// 配合 worker 的重试退避逻辑验证:首次失败 → 退避 → 重试成功。
        /// `attempt` 从 1 开始计数,设为 1 表示首次调用即失败。
        pub fn fail_on_attempt(self, attempt: usize, error: DownloadError) -> Self {
            *self.fail_on_attempt.lock().unwrap() =
                Some((attempt, PreservedError::from_download_error(&error)));
            self
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
                stalling_ranges: Arc::new(Mutex::new(HashMap::new())),
                chunk_size: None,
                chunk_delay: None,
                fail_on_attempt: Arc::new(Mutex::new(None)),
                attempt_counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
                // 间歇失败注入:递增计数器,命中指定次数时返回错误。
                // 用于验证 worker 的退避重试与源切换逻辑。
                if let Some((fail_at, ref preserved)) = *this.fail_on_attempt.lock().unwrap() {
                    let current = this
                        .attempt_counter
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        + 1;
                    if current == fail_at {
                        return Err(preserved.to_download_error());
                    }
                }
                // 命中"死 swarm"区间:返回永不产出项的 pending 流,
                // 模拟磁力链接无 peer 时 FileStream.read() 永久 Pending。
                if this
                    .stalling_ranges
                    .lock()
                    .unwrap()
                    .contains_key(&(start, end))
                {
                    return Ok(
                        Box::pin(futures::stream::pending::<DownloadResult<Bytes>>())
                            as crate::traits::ByteStream,
                    );
                }
                let data = this.download_range(&url, start, end).await?;
                // 分块流模式:按 chunk_size 拆成多个 chunk 流式产出,
                // 模拟 HTTP chunked / BT FileStream 多次 read,覆盖引擎流读取循环
                // 的逐块哈希、批量刷写、取消信号穿透路径。
                if let Some(chunk_sz) = this.chunk_size {
                    let chunks: Vec<Bytes> = (0..data.len())
                        .step_by(chunk_sz)
                        .map(|offset| {
                            let end = (offset + chunk_sz).min(data.len());
                            data.slice(offset..end)
                        })
                        .collect();
                    // 慢源注入:每个 chunk 产出前 sleep,模拟固定低带宽。
                    // 用 then 在每个 chunk 前 sleep,延迟放在流内部让消费方的 select!
                    // 能在等待期间被取消信号穿透,而非阻塞整个 future。
                    if let Some(delay) = this.chunk_delay {
                        use futures::StreamExt;
                        Ok(
                            Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)).then(
                                move |result| async move {
                                    tokio::time::sleep(delay).await;
                                    result
                                },
                            )) as crate::traits::ByteStream,
                        )
                    } else {
                        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
                            as crate::traits::ByteStream)
                    }
                } else {
                    Ok(Box::pin(futures::stream::once(async move { Ok(data) }))
                        as crate::traits::ByteStream)
                }
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

    impl AsyncStorage for MemoryStorage {
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

    /// 无操作存储实现,用于隔离测量存储适配层(如 `StorageSet::Multi`)开销
    ///
    /// `write_at`/`write_at_mut` 立即返回成功,不拷贝数据也不做真实 I/O,
    /// 因此计时测试可隔离出 `StorageSet::Multi::write_at_mut` 的分段拷贝/拆分成本
    /// (而非被底层后端 I/O 或全量拷贝掩盖)。
    #[derive(Clone, Default)]
    pub struct NoopStorage;

    impl AsyncStorage for NoopStorage {
        fn write_at(
            &self,
            _offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            Box::pin(async move { Ok(data.len()) })
        }

        // write_at_mut 不覆盖:默认实现会 Bytes::copy_from_slice 全量拷贝,
        // 这正是我们想隔离测量 Multi 分段拷贝时不想被干扰的因素,故覆盖为零拷贝直读。
        fn write_at_mut<'a>(
            &'a self,
            _offset: u64,
            data: &'a mut BytesMut,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move { Ok(data.len()) })
        }

        fn read_at<'a>(
            &'a self,
            _offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move { Ok(buf.len()) })
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
            Box::pin(async move { Ok(u64::MAX) })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    /// 可故障存储,用于磁盘边界/IO 错误注入测试。
    ///
    /// 配置 `fail_after` 后,前 N 次 write_at 成功,第 N+1 次起返回 `DownloadError::Io`。
    /// 用于验证 downloader 在磁盘满/IO 错误时优雅降级(返回错误而非 panic/无限重试)。
    /// read_at/sync/allocate 等正常工作,仅 write_at 注入故障。
    #[derive(Clone, Default)]
    pub struct FailingStorage {
        inner: MemoryStorage,
        /// write_at 成功次数上限;超过后返回错误。None = 永不失败。
        fail_after: Arc<std::sync::atomic::AtomicUsize>,
        write_count: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FailingStorage {
        pub fn new() -> Self {
            Self::default()
        }

        /// 设置 write_at 成功 N 次后开始失败(模拟磁盘剩余空间耗尽)。
        /// N=0 表示首次 write_at 即失败。
        pub fn fail_write_after(self, n: usize) -> Self {
            self.fail_after
                .store(n, std::sync::atomic::Ordering::SeqCst);
            self
        }

        /// 当前 write_at 被调用次数(含失败次数),用于测试断言。
        pub fn write_call_count(&self) -> usize {
            self.write_count.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// write_count 的 Arc 引用(供 storage 被 move 后测试仍能读取计数)。
        pub fn write_call_count_arc(&self) -> Arc<std::sync::atomic::AtomicUsize> {
            self.write_count.clone()
        }
    }

    impl AsyncStorage for FailingStorage {
        fn write_at(
            &self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
            let count = self
                .write_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let limit = self.fail_after.load(std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if count >= limit {
                    return Err(DownloadError::Io(std::io::Error::new(
                        std::io::ErrorKind::StorageFull,
                        "模拟磁盘空间不足(ENOSPC)",
                    )));
                }
                self.inner.write_at(offset, data).await
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            self.inner.read_at(offset, buf)
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            self.inner.sync()
        }

        fn allocate(
            &self,
            size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            self.inner.allocate(size)
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            self.inner.file_size()
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            self.inner.close()
        }
    }
    pub fn test_metadata(file_name: &str, file_size: u64) -> FileMetadata {
        FileMetadata {
            file_name: file_name.to_string(),
            file_size: Some(file_size),
            content_type: Some("application/octet-stream".into()),
            supports_range: true,
            etag: Some("\"abc123\"".into()),
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
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
            verify_strategy: crate::config::VerifyStrategy::BestEffort,
            user_agent: "Tachyon-Test/0.1.0".into(),
            headers: HashMap::new(),
            pause_timeout_secs: 300,
            rate_limit_bytes_per_sec: None,
            max_full_stream_bytes: crate::config::default_max_full_stream_bytes(),
            authorized_dirs: vec![std::env::temp_dir().to_string_lossy().to_string()],
            // 测试统一用 Standard(TokioFile),消除"Windows 跑 Standard、Linux 跑 IoUring"
            // 的平台隐式分歧。IoUring 的 O_DIRECT 慢速路径有平台特定行为,
            // 端到端落盘测试不应隐式依赖 IoStrategy::default()(Linux 上回退 IoUring)。
            // IoUring 后端有独立的单元测试覆盖(crates/tachyon-io/src/iouring.rs)。
            io_strategy: IoStrategy::Standard,
            proxy: None,
            enable_work_stealing: false,
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
    use crate::traits::{AsyncStorage, Protocol};

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
    async fn test_mock_protocol_chunk_delay_injects_slow_source() {
        // 慢源注入:chunk_delay 让每个 chunk 产出前 sleep,
        // 验证流确实延迟产出且数据完整。
        use futures::StreamExt;
        let meta = test_metadata("slow.bin", 1024);
        let data = Bytes::from(vec![0xABu8; 1024]);
        let protocol = MockProtocol::new(meta)
            .with_range_data(0, 1023, data.clone())
            .with_chunk_size(256)
            .with_chunk_delay(std::time::Duration::from_millis(20));
        let stream = protocol
            .download_range_stream("http://example.com/slow.bin", 0, 1023)
            .await
            .unwrap();
        let start = std::time::Instant::now();
        let collected: Vec<_> = stream.collect::<Vec<_>>().await;
        let elapsed = start.elapsed();
        // 4 chunk × 20ms = 至少 80ms(允许调度抖动,断言下限 60ms)
        assert!(
            elapsed >= std::time::Duration::from_millis(60),
            "慢源延迟未生效: elapsed={elapsed:?}"
        );
        // 数据完整性:4 chunk 拼回 1024 字节
        let total: usize = collected.iter().map(|c| c.as_ref().unwrap().len()).sum();
        assert_eq!(total, 1024);
    }

    #[tokio::test]
    async fn test_mock_protocol_fail_on_attempt_intermittent_failure() {
        // 间歇失败注入:第 1 次调用失败,第 2 次成功,
        // 验证计数器递增与错误返回语义。
        let meta = test_metadata("flaky.bin", 64);
        let data = Bytes::from(vec![0xCDu8; 64]);
        let protocol = MockProtocol::new(meta)
            .with_range_data(0, 63, data.clone())
            .fail_on_attempt(1, DownloadError::Network("间歇连接重置".into()));
        // 第 1 次:失败
        let result = protocol
            .download_range_stream("http://example.com/flaky.bin", 0, 63)
            .await;
        assert!(result.is_err(), "第 1 次调用应失败");
        // 第 2 次:成功
        let stream = protocol
            .download_range_stream("http://example.com/flaky.bin", 0, 63)
            .await
            .unwrap();
        use futures::StreamExt;
        let collected: Vec<_> = stream.collect::<Vec<_>>().await;
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].as_ref().unwrap(), &data);
    }

    #[tokio::test]
    async fn test_noop_storage_smoke() {
        // NoopStorage 主要用于 bench(隔离存储适配层开销),
        // 此冒烟测试覆盖其所有方法,避免 bench-only 代码拉低覆盖率。
        let storage = NoopStorage;
        let w = storage
            .write_at(0, Bytes::from_static(b"abc"))
            .await
            .unwrap();
        assert_eq!(w, 3);
        let mut buf = bytes::BytesMut::zeroed(4);
        let wm = storage.write_at_mut(0, &mut buf).await.unwrap();
        assert_eq!(wm, 4);
        let mut rbuf = [0u8; 4];
        let r = storage.read_at(0, &mut rbuf).await.unwrap();
        assert_eq!(r, 4);
        storage.sync().await.unwrap();
        storage.allocate(1024).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), u64::MAX);
        storage.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_preserved_error_roundtrip_all_variants() {
        // 通过 MockProtocol::failing + probe 间接覆盖 PreservedError 的
        // from_download_error / to_download_error 全变体分支。
        // PreservedError 是 harness 模块私有类型,测试经由公共 API(failing/probe)触达。
        let cases: Vec<DownloadError> = vec![
            DownloadError::Network("net".into()),
            DownloadError::Protocol("proto".into()),
            DownloadError::Fragment("frag".into()),
            DownloadError::ChecksumMismatch {
                expected: "abc".into(),
                actual: "def".into(),
            },
            DownloadError::NoExpectedChecksum,
            DownloadError::Config("cfg".into()),
            DownloadError::Cancelled,
            DownloadError::TaskNotFound("t1".into()),
            DownloadError::ConnectionPoolExhausted,
            DownloadError::Timeout("t".into()),
            DownloadError::Throttled {
                retry_after_secs: Some(30),
            },
            DownloadError::Throttled {
                retry_after_secs: None,
            },
            DownloadError::Forbidden { status: 403 },
            DownloadError::Http {
                status: 500,
                reason: "err".into(),
            },
            // 不可 Clone 变体:降级为 Network
            DownloadError::Io(std::io::Error::other("io")),
            DownloadError::Other("other".into()),
            DownloadError::UrlParse(url::ParseError::EmptyHost),
            DownloadError::Serialization(serde_json::from_str::<()>("bad").unwrap_err()),
        ];
        for err in cases {
            // err 将被 move 进 failing,先记录判据
            let is_downgraded = matches!(
                err,
                DownloadError::Io(_)
                    | DownloadError::Other(_)
                    | DownloadError::UrlParse(_)
                    | DownloadError::Serialization(_)
            );
            let orig_discriminant = std::mem::discriminant(&err);
            let proto = MockProtocol::failing(err);
            let result = proto.probe("http://test.example/file").await;
            assert!(result.is_err(), "failing protocol probe 应返回错误");
            // 降级变体应还原为 Network;可 Clone 变体应保留类型
            let restored = result.unwrap_err();
            if is_downgraded {
                assert!(
                    matches!(restored, DownloadError::Network(_)),
                    "降级变体应还原为 Network,实际: {restored:?}"
                );
            } else {
                assert!(
                    orig_discriminant == std::mem::discriminant(&restored),
                    "可 Clone 变体应保留类型,还原: {restored:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_failing_storage_write_succeeds_then_fails() {
        // 前 2 次 write_at 成功,第 3 次起失败
        let storage = FailingStorage::new().fail_write_after(2);
        // 第 1 次:成功
        let w1 = storage
            .write_at(0, Bytes::from_static(b"aa"))
            .await
            .unwrap();
        assert_eq!(w1, 2);
        // 第 2 次:成功
        let w2 = storage
            .write_at(2, Bytes::from_static(b"bb"))
            .await
            .unwrap();
        assert_eq!(w2, 2);
        // 第 3 次:失败(StorageFull)
        let err = storage
            .write_at(4, Bytes::from_static(b"cc"))
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::Io(ref e)
            if e.kind() == std::io::ErrorKind::StorageFull));
        assert_eq!(storage.write_call_count(), 3);
    }

    #[tokio::test]
    async fn test_failing_storage_non_write_methods_delegate_to_inner() {
        // FailingStorage 的 read_at/sync/allocate/file_size/close 委托给 MemoryStorage,
        // 验证它们正常工作(不注入故障)。
        let storage = FailingStorage::new().fail_write_after(0);
        // allocate 不受故障注入影响
        storage.allocate(64).await.unwrap();
        // write_at 失败,但数据未写入(inner 为空)
        let _ = storage.write_at(0, Bytes::from_static(b"xxx")).await;
        // file_size 应反映 allocate 的 64
        let size = storage.file_size().await.unwrap();
        assert_eq!(size, 64);
        // read_at 读取 allocate 预分配的零字节
        let mut buf = [0u8; 4];
        let n = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, &[0u8; 4]);
        // sync 和 close 不报错
        storage.sync().await.unwrap();
        storage.close().await.unwrap();
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

    // -----------------------------------------------------------------------
    // P1: MockProtocol 全量下载 / 流式 / clear_selected / failing 覆盖
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_mock_protocol_with_default_data_success() {
        let data = Bytes::from_static(b"full content");
        let protocol =
            MockProtocol::new(test_metadata("file.bin", 100)).with_default_data(data.clone());
        let result = protocol.download_full("http://example.com/file.bin").await;
        assert_eq!(result.unwrap(), data);
    }

    #[tokio::test]
    async fn test_mock_protocol_download_full_failure_when_missing_data() {
        let protocol = MockProtocol::new(test_metadata("file.bin", 100));
        let result = protocol.download_full("http://example.com/file.bin").await;
        assert!(
            matches!(result.unwrap_err(), DownloadError::Protocol(_)),
            "未配置 default_data 时应返回 Protocol 错误"
        );
    }

    #[tokio::test]
    async fn test_mock_protocol_download_full_stream() {
        use futures::StreamExt;

        let data = Bytes::from_static(b"streamed data");
        let protocol =
            MockProtocol::new(test_metadata("file.bin", 100)).with_default_data(data.clone());
        let mut stream = protocol
            .download_full_stream("http://example.com/file.bin")
            .await
            .unwrap();

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk, data);
        assert!(stream.next().await.is_none(), "单块流应仅有一个元素");
    }

    #[tokio::test]
    async fn test_mock_protocol_clear_selected() {
        let protocol = MockProtocol::new(test_metadata("file.bin", 100));
        // 默认实现为空操作,不应 panic
        protocol.clear_selected().await;
    }

    #[tokio::test]
    #[allow(clippy::type_complexity)]
    async fn test_mock_protocol_failing_preserves_cloneable_errors() {
        let cases: Vec<(DownloadError, Box<dyn Fn(&DownloadError)>)> = vec![
            (
                DownloadError::Network("timeout".into()),
                Box::new(|e| assert!(matches!(e, DownloadError::Network(s) if s == "timeout"))),
            ),
            (
                DownloadError::Protocol("bad".into()),
                Box::new(|e| assert!(matches!(e, DownloadError::Protocol(s) if s == "bad"))),
            ),
            (
                DownloadError::Fragment("short".into()),
                Box::new(|e| assert!(matches!(e, DownloadError::Fragment(s) if s == "short"))),
            ),
            (
                DownloadError::ChecksumMismatch {
                    expected: "abc".into(),
                    actual: "def".into(),
                },
                Box::new(|e| {
                    assert!(matches!(
                        e,
                        DownloadError::ChecksumMismatch { expected, actual }
                        if expected == "abc" && actual == "def"
                    ))
                }),
            ),
            (
                DownloadError::NoExpectedChecksum,
                Box::new(|e| assert!(matches!(e, DownloadError::NoExpectedChecksum))),
            ),
            (
                DownloadError::Config("bad".into()),
                Box::new(|e| assert!(matches!(e, DownloadError::Config(s) if s == "bad"))),
            ),
            (
                DownloadError::Cancelled,
                Box::new(|e| assert!(matches!(e, DownloadError::Cancelled))),
            ),
            (
                DownloadError::TaskNotFound("t1".into()),
                Box::new(|e| assert!(matches!(e, DownloadError::TaskNotFound(s) if s == "t1"))),
            ),
            (
                DownloadError::ConnectionPoolExhausted,
                Box::new(|e| assert!(matches!(e, DownloadError::ConnectionPoolExhausted))),
            ),
            (
                DownloadError::Timeout("30s".into()),
                Box::new(|e| assert!(matches!(e, DownloadError::Timeout(s) if s == "30s"))),
            ),
            (
                DownloadError::Throttled {
                    retry_after_secs: Some(60),
                },
                Box::new(|e| {
                    assert!(matches!(
                        e,
                        DownloadError::Throttled {
                            retry_after_secs: Some(60)
                        }
                    ))
                }),
            ),
            (
                DownloadError::Forbidden { status: 403 },
                Box::new(|e| assert!(matches!(e, DownloadError::Forbidden { status: 403 }))),
            ),
            (
                DownloadError::Http {
                    status: 500,
                    reason: "err".into(),
                },
                Box::new(|e| {
                    assert!(matches!(
                        e,
                        DownloadError::Http { status: 500, reason }
                        if reason == "err"
                    ))
                }),
            ),
        ];

        for (err, check) in cases {
            let protocol = MockProtocol::failing(err);
            let result = protocol.probe("http://example.com/file.bin").await;
            check(&result.unwrap_err());
        }
    }

    #[tokio::test]
    async fn test_mock_protocol_failing_downgrades_non_cloneable_errors() {
        let io_err = DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file gone",
        ));
        let other_err = DownloadError::Other(Box::new(std::io::Error::other("other")));
        let url_err = DownloadError::UrlParse(url::ParseError::EmptyHost);
        let serde_err = DownloadError::Serialization(
            serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
        );

        for err in [io_err, other_err, url_err, serde_err] {
            let original_msg = err.to_string();
            let protocol = MockProtocol::failing(err);
            let result = protocol.probe("http://example.com/file.bin").await;
            match result.unwrap_err() {
                DownloadError::Network(s) => {
                    assert!(
                        s.contains(&original_msg),
                        "降级后的 Network 错误应保留原始描述: {s}"
                    );
                }
                other => panic!("预期降级为 Network 错误,实际: {other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // P1: MemoryStorage 扩展测试
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_memory_storage_with_capacity() {
        let storage = MemoryStorage::with_capacity(8);
        assert_eq!(storage.get_data(), vec![0u8; 8]);

        storage
            .write_at(2, Bytes::from_static(b"ab"))
            .await
            .unwrap();
        let data = storage.get_data();
        assert_eq!(data, vec![0, 0, b'a', b'b', 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn test_memory_storage_get_data_isolation() {
        let storage = MemoryStorage::new();
        storage.write_at(0, Bytes::from_static(b"x")).await.unwrap();
        let snapshot = storage.get_data();
        storage.write_at(1, Bytes::from_static(b"y")).await.unwrap();
        assert_eq!(snapshot, vec![b'x']);
    }

    #[tokio::test]
    async fn test_memory_storage_close() {
        let storage = MemoryStorage::new();
        assert!(storage.close().await.is_ok());
    }

    #[tokio::test]
    async fn test_memory_storage_large_offset_write() {
        let storage = MemoryStorage::new();
        storage
            .write_at(1024, Bytes::from_static(b"end"))
            .await
            .unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 1027);

        let data = storage.get_data();
        assert_eq!(&data[1024..], b"end");
        // 未写入的中间区域应为 0
        assert!(data[..1024].iter().all(|&b| b == 0));
    }
}
