//! 混沌测试框架
//!
//! 在不修改业务逻辑代码的前提下,使用 `tachyon-core::test_harness` 中的 Mock 实现
//! 构建一个可注入故障的简化下载流程,验证:
//! - 随机网络抖动(延迟/超时/失败)下系统不 panic
//! - 随机任务取消下系统不 panic
//! - 随机存储写入延迟下数据最终完整
//! - 重试机制具备自恢复能力

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::future::join_all;
use rand::SeedableRng;
use rand::prelude::*;
use rand::rngs::StdRng;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

use tachyon_core::config::SchedulerConfig;
use tachyon_core::test_harness::harness::{MemoryStorage, MockProtocol};
use tachyon_core::traits::{ByteStream, Protocol, Storage};
use tachyon_core::types::{FileMetadata, FragmentInfo, TaskCommand};
use tachyon_core::{DownloadError, DownloadResult};
use tachyon_engine::fragment::plan_fragments;

/// 混沌注入配置
#[derive(Clone, Debug)]
struct ChaosConfig {
    /// 单次网络请求失败的概率
    network_failure_prob: f64,
    /// 单次网络请求注入延迟的概率
    network_delay_prob: f64,
    /// 网络延迟最大时长
    max_network_delay_ms: u64,
    /// 单次存储写入注入延迟的概率
    storage_delay_prob: f64,
    /// 存储延迟最大时长
    max_storage_delay_ms: u64,
    /// 任务被取消的概率(每轮检查)
    cancel_prob: f64,
    /// 随机数种子
    seed: u64,
}

impl ChaosConfig {
    /// 温和故障: mostly 成功,偶发延迟
    fn mild(seed: u64) -> Self {
        Self {
            network_failure_prob: 0.05,
            network_delay_prob: 0.2,
            max_network_delay_ms: 50,
            storage_delay_prob: 0.1,
            max_storage_delay_ms: 20,
            cancel_prob: 0.0,
            seed,
        }
    }

    /// 高故障但最终可恢复
    fn harsh_but_recoverable(seed: u64) -> Self {
        Self {
            network_failure_prob: 0.3,
            network_delay_prob: 0.4,
            max_network_delay_ms: 100,
            storage_delay_prob: 0.2,
            max_storage_delay_ms: 50,
            cancel_prob: 0.0,
            seed,
        }
    }

    /// 包含随机取消
    fn with_cancellation(seed: u64) -> Self {
        Self {
            network_failure_prob: 0.1,
            network_delay_prob: 0.2,
            max_network_delay_ms: 50,
            storage_delay_prob: 0.1,
            max_storage_delay_ms: 20,
            cancel_prob: 0.05,
            seed,
        }
    }
}

/// 注入网络混沌的 Protocol 包装器
#[derive(Clone)]
struct ChaoticProtocol {
    inner: MockProtocol,
    config: ChaosConfig,
    rng: Arc<Mutex<StdRng>>,
}

impl ChaoticProtocol {
    fn new(inner: MockProtocol, config: ChaosConfig) -> Self {
        let rng = StdRng::seed_from_u64(config.seed);
        Self {
            inner,
            config,
            rng: Arc::new(Mutex::new(rng)),
        }
    }

    async fn maybe_inject_network_chaos(&self) -> DownloadResult<()> {
        let mut rng = self.rng.lock().await;

        if rng.r#gen::<f64>() < self.config.network_failure_prob {
            return Err(DownloadError::Network("chaos: 网络故障注入".into()));
        }

        if rng.r#gen::<f64>() < self.config.network_delay_prob {
            let delay_ms = rng.gen_range(0..=self.config.max_network_delay_ms);
            drop(rng);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        Ok(())
    }
}

impl Protocol for ChaoticProtocol {
    fn probe(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>> {
        let this = self.clone();
        let url = url.to_owned();
        Box::pin(async move {
            this.maybe_inject_network_chaos().await?;
            this.inner.probe(&url).await
        })
    }

    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
        let this = self.clone();
        let url = url.to_owned();
        Box::pin(async move {
            this.maybe_inject_network_chaos().await?;
            this.inner.download_range(&url, start, end).await
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        let this = self.clone();
        let url = url.to_owned();
        Box::pin(async move {
            this.maybe_inject_network_chaos().await?;
            this.inner.download_range_stream(&url, start, end).await
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
        let this = self.clone();
        let url = url.to_owned();
        Box::pin(async move {
            this.maybe_inject_network_chaos().await?;
            this.inner.download_full(&url).await
        })
    }
}

/// 注入存储混沌的 Storage 包装器
#[derive(Clone)]
struct ChaoticStorage {
    inner: MemoryStorage,
    config: ChaosConfig,
    rng: Arc<Mutex<StdRng>>,
}

impl ChaoticStorage {
    fn new(inner: MemoryStorage, config: ChaosConfig) -> Self {
        let rng = StdRng::seed_from_u64(config.seed.wrapping_add(0x9E3779B97F4A7C15));
        Self {
            inner,
            config,
            rng: Arc::new(Mutex::new(rng)),
        }
    }

    async fn maybe_inject_storage_delay(&self) {
        let mut rng = self.rng.lock().await;
        if rng.r#gen::<f64>() < self.config.storage_delay_prob {
            let delay_ms = rng.gen_range(0..=self.config.max_storage_delay_ms);
            drop(rng);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }
}

impl Storage for ChaoticStorage {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>> {
        Box::pin(async move {
            self.maybe_inject_storage_delay().await;
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

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        self.inner.allocate(size)
    }

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
        self.inner.file_size()
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        self.inner.close()
    }
}

/// 创建带 range 数据的 MockProtocol
///
/// 使用与真实流程一致的 `plan_fragments` 生成分片,确保 MockProtocol 的 range_data
/// 键与下载请求完全匹配。
fn build_mock_protocol(total_size: u64) -> (MockProtocol, Vec<u8>, Vec<FragmentInfo>) {
    let mut data = vec![0u8; total_size as usize];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }

    let meta = FileMetadata {
        file_name: "chaos.bin".into(),
        file_size: Some(total_size),
        content_type: Some("application/octet-stream".into()),
        supports_range: true,
        etag: None,
        last_modified: None,
    };

    let scheduler_config = SchedulerConfig::default();
    let fragments = plan_fragments(total_size, meta.supports_range, None, &scheduler_config);

    let mut protocol = MockProtocol::new(meta);
    for frag in &fragments {
        let start = frag.start as usize;
        let end = frag.end as usize;
        let chunk = Bytes::copy_from_slice(&data[start..=end]);
        protocol = protocol.with_range_data(frag.start, frag.end, chunk);
    }

    (protocol, data, fragments)
}

/// 单分片下载任务(带重试与取消检查)
async fn download_fragment_with_chaos(
    protocol: Arc<dyn Protocol>,
    storage: Arc<dyn Storage>,
    frag: FragmentInfo,
    max_retries: u32,
    cancel_flag: Arc<AtomicBool>,
) -> DownloadResult<()> {
    let mut attempt = 0u32;
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }

        match protocol
            .download_range("http://example.com/chaos.bin", frag.start, frag.end)
            .await
        {
            Ok(bytes) => {
                storage.write_at(frag.start, bytes).await?;
                return Ok(());
            }
            Err(e) => {
                if cancel_flag.load(Ordering::Relaxed) {
                    return Err(DownloadError::Cancelled);
                }
                if !e.is_retryable() || attempt >= max_retries {
                    return Err(e);
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

/// 执行一次混沌下载流程
///
/// 返回 (是否因取消而结束, 错误结果)
async fn run_chaos_download(
    config: ChaosConfig,
    total_size: u64,
    max_retries: u32,
) -> (bool, DownloadResult<MemoryStorage>) {
    let (mock_protocol, expected_data, fragments) = build_mock_protocol(total_size);
    let protocol = Arc::new(ChaoticProtocol::new(mock_protocol, config.clone()));
    let storage = Arc::new(ChaoticStorage::new(
        MemoryStorage::with_capacity(total_size as usize),
        config.clone(),
    ));

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (cancel_tx, _cancel_rx) = watch::channel(TaskCommand::Start);

    // 取消注入任务
    // 当 cancel_prob 为 0 时跳过注入器,避免无限循环(rng.gen::<f64>() < 0.0 永远为 false)
    let cancel_flag_clone = cancel_flag.clone();
    let cancel_injector: Option<JoinHandle<()>> = if config.cancel_prob > 0.0 {
        Some(tokio::spawn(async move {
            let mut rng = StdRng::seed_from_u64(config.seed.wrapping_add(0x12345678));
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if rng.r#gen::<f64>() < config.cancel_prob {
                    let _ = cancel_tx.send(TaskCommand::Cancel);
                    cancel_flag_clone.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }))
    } else {
        None
    };

    // 并发下载分片
    let mut handles: Vec<JoinHandle<DownloadResult<()>>> = Vec::new();
    for frag in fragments.clone() {
        let protocol = protocol.clone();
        let storage = storage.clone();
        let flag = cancel_flag.clone();
        handles.push(tokio::spawn(async move {
            download_fragment_with_chaos(protocol, storage, frag, max_retries, flag).await
        }));
    }

    let results = join_all(handles).await;
    if let Some(handle) = cancel_injector {
        let _ = handle.await;
    }

    // 检查是否有取消或失败
    let mut cancelled = false;
    let mut first_err: Option<DownloadError> = None;
    for res in results {
        match res {
            Ok(Ok(())) => {}
            Ok(Err(DownloadError::Cancelled)) => cancelled = true,
            Ok(Err(e)) if first_err.is_none() => first_err = Some(e),
            Err(_) if first_err.is_none() => {
                first_err = Some(DownloadError::Other("分片任务 panic".into()));
            }
            Err(_) => {}
            _ => {}
        }
    }

    // 读取最终存储数据
    let inner = match Arc::try_unwrap(storage) {
        Ok(s) => s.inner,
        Err(arc) => arc.inner.clone(),
    };

    // 只要未取消且无错误,就验证数据完整性
    if cancelled {
        (true, Ok(inner))
    } else if let Some(e) = first_err {
        (false, Err(e))
    } else {
        let actual = inner.get_data();
        assert_eq!(
            actual.len(),
            expected_data.len(),
            "混沌下载完成后数据长度应一致"
        );
        assert_eq!(actual, expected_data, "混沌下载完成后数据内容应完整一致");
        (false, Ok(inner))
    }
}

#[tokio::test]
async fn chaos_mild_network_and_storage_delays_succeeds() {
    let (_, result) = run_chaos_download(ChaosConfig::mild(42), 128 * 1024, 5).await;
    assert!(result.is_ok(), "温和故障下应成功完成: {:?}", result.err());
}

#[tokio::test]
async fn chaos_harsh_network_recoverable() {
    let (_, result) =
        run_chaos_download(ChaosConfig::harsh_but_recoverable(12345), 64 * 1024, 10).await;
    assert!(
        result.is_ok(),
        "高故障率下应通过重试恢复: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn chaos_random_cancellation_does_not_panic() {
    let (cancelled, result) =
        run_chaos_download(ChaosConfig::with_cancellation(99999), 64 * 1024, 3).await;
    if !cancelled {
        assert!(result.is_ok(), "未取消时应成功完成: {:?}", result.err());
    }
}

#[tokio::test]
async fn chaos_empty_file_no_panic() {
    let (_, result) = run_chaos_download(ChaosConfig::mild(777), 0, 3).await;
    assert!(result.is_ok(), "空文件应直接成功: {:?}", result.err());
}

#[tokio::test]
async fn chaos_single_fragment_with_delays() {
    let (_, result) = run_chaos_download(ChaosConfig::mild(555), 32 * 1024, 3).await;
    assert!(result.is_ok(), "单分片延迟场景应成功: {:?}", result.err());
}
