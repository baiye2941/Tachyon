//! 混沌测试框架(走 DownloadTask::run() 真实路径)
//!
//! 在不修改业务逻辑代码的前提下,使用 `tachyon-core::test_harness` 中的 Mock 实现
//! 构建一个可注入故障的下载流程,通过 `DownloadTask::new_for_test` + `task.run()`
//! 驱动真实下载路径(调度器、并发控制、流式哈希、取消检查点、重试、限速),
//! 验证:
//! - 随机网络抖动(延迟/超时/失败)下系统不 panic
//! - 随机任务取消下系统不 panic
//! - 随机存储写入延迟下数据最终完整
//! - 重试机制具备自恢复能力
//!
//! # 与旧实现的区别
//!
//! 旧实现自建 `download_fragment_with_chaos` 下载循环(裸 `protocol.download_range` +
//! `storage.write_at`),绕过了 `DownloadTask::run()` 的调度器/并发控制/流式哈希/
//! 取消检查点/限速,验证的是"平行宇宙"的下载逻辑。本实现改为注入 `ChaoticProtocol`
//! 和 `ChaoticStorage` 到 `DownloadTask`,让 chaos 覆盖真实生产路径。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::stream::StreamExt;
use rand::SeedableRng;
use rand::prelude::*;
use rand::rngs::StdRng;
use tokio::sync::{Mutex, watch};

use tachyon_core::config::{DownloadConfig, SchedulerConfig};
use tachyon_core::test_harness::harness::{MemoryStorage, MockProtocol, test_metadata};
use tachyon_core::traits::{AsyncStorage, ByteStream, Protocol};
use tachyon_core::types::{FileMetadata, TaskCommand};
use tachyon_core::{DownloadError, DownloadResult};
use tachyon_engine::{DownloadTask, StorageKind};

/// 混沌注入配置
#[derive(Clone, Debug)]
struct ChaosConfig {
    /// 单次网络请求失败的概率(流建立 + chunk 产出)
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
///
/// 在 `probe`/`download_range_stream` 建立阶段注入故障(连接失败/延迟),
/// 在流产出 chunk 阶段也注入故障(中途断流),覆盖引擎流读取循环的
/// 错误处理与分片级重试路径。
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

    /// 尝试注入网络故障(连接阶段):按概率返回错误或注入延迟
    async fn maybe_inject_network_chaos(&self) -> DownloadResult<()> {
        let mut rng = self.rng.lock().await;

        if rng.random::<f64>() < self.config.network_failure_prob {
            return Err(DownloadError::Network(
                "chaos: 网络故障注入(连接阶段)".into(),
            ));
        }

        if rng.random::<f64>() < self.config.network_delay_prob {
            let delay_ms = rng.random_range(0..=self.config.max_network_delay_ms);
            drop(rng);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        Ok(())
    }

    /// 尝试注入 chunk 级故障(流传输中途):按概率返回错误或注入延迟
    ///
    /// chunk 级故障概率大幅降低(原值的 1/10):一个分片可能产出数十个 chunk,
    /// 若每个 chunk 都有高故障概率,整片成功率极低导致重试风暴。
    /// 1/10 比例使"中途断流"场景偶发触发(覆盖流读取错误处理),
    /// 但不致于让正常重试无法恢复。
    async fn maybe_inject_chunk_chaos(&self) -> DownloadResult<()> {
        let mut rng = self.rng.lock().await;

        let chunk_fail_prob = self.config.network_failure_prob * 0.1;
        if rng.random::<f64>() < chunk_fail_prob {
            return Err(DownloadError::Network(
                "chaos: 网络故障注入(chunk 阶段)".into(),
            ));
        }

        let chunk_delay_prob = self.config.network_delay_prob * 0.3;
        if rng.random::<f64>() < chunk_delay_prob {
            let delay_ms = rng.random_range(0..=self.config.max_network_delay_ms);
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
        _identity: Option<tachyon_core::ObjectIdentity>,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
        let this = self.clone();
        let url = url.to_owned();
        Box::pin(async move {
            this.maybe_inject_network_chaos().await?;
            this.inner.download_range(&url, start, end, None).await
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
        _identity: Option<tachyon_core::ObjectIdentity>,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        let this = self.clone();
        let url = url.to_owned();
        Box::pin(async move {
            // 连接阶段混沌
            this.maybe_inject_network_chaos().await?;
            // 获取底层流
            let inner_stream = this
                .inner
                .download_range_stream(&url, start, end, None)
                .await?;
            // 包装为混沌流:每个 chunk 产出前注入故障
            let chaos = this.clone();
            let chaotic_stream = inner_stream.then(move |result| {
                let chaos = chaos.clone();
                async move {
                    match result {
                        Ok(bytes) => match chaos.maybe_inject_chunk_chaos().await {
                            Ok(()) => Ok(bytes),
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(e),
                    }
                }
            });
            Ok(Box::pin(chaotic_stream) as ByteStream)
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

    /// 覆盖默认实现:走 stream 路径(引擎 execute_full_download 调用此方法)
    fn download_full_stream(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        let this = self.clone();
        let url = url.to_owned();
        Box::pin(async move {
            this.maybe_inject_network_chaos().await?;
            this.inner.download_full_stream(&url).await
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
        if rng.random::<f64>() < self.config.storage_delay_prob {
            let delay_ms = rng.random_range(0..=self.config.max_storage_delay_ms);
            drop(rng);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }
}

impl AsyncStorage for ChaoticStorage {
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
/// 键与下载请求完全匹配。配置 chunk_size 使流分块产出,覆盖引擎流读取循环。
fn build_mock_protocol(total_size: u64) -> (MockProtocol, Vec<u8>) {
    let mut data = vec![0u8; total_size as usize];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }

    let meta = test_metadata("chaos.bin", total_size);

    let mut protocol = MockProtocol::new(meta);
    // 用 MockProtocol 的 with_range_data 逐分片填充,键为 (start, end)
    // 引擎经 plan_fragments 生成分片后,download_range_stream 按 (start, end) 查询
    let scheduler_config = SchedulerConfig::default();
    let fragments = tachyon_engine::fragment::plan_fragments(
        total_size,
        true, // supports_range
        None,
        &scheduler_config,
    )
    .expect("plan_fragments 不应失败");

    for frag in &fragments {
        let start = frag.start as usize;
        let end = frag.end as usize;
        let chunk = Bytes::copy_from_slice(&data[start..=end]);
        protocol = protocol.with_range_data(frag.start, frag.end, chunk);
    }
    // 配置 chunk_size 使流分块产出(模拟 HTTP chunked / BT FileStream 多次 read),
    // 覆盖引擎流读取循环的逐块哈希、批量刷写、取消信号穿透路径
    protocol = protocol.with_chunk_size(4096);
    // 配置 default_data:单分片/空文件场景走 execute_full_download → download_full_stream,
    // MockProtocol 默认实现回退到 download_full,需要 default_data 支持
    protocol = protocol.with_default_data(Bytes::copy_from_slice(&data));

    (protocol, data)
}

/// 执行一次混沌下载流程(走 DownloadTask::run() 真实路径)
///
/// 返回 (是否因取消而结束, 下载结果)
///
/// 通过 `DownloadTask::new_for_test` 构造任务,注入 `ChaoticProtocol` 和
/// `ChaoticStorage`,调用 `task.run()` 驱动完整下载路径(probe → plan →
/// execute_fragmented_download → verify)。取消信号通过 watch 通道注入,
/// 与生产路径的控制通道一致。
async fn run_chaos_download(
    config: ChaosConfig,
    total_size: u64,
    max_retries: u32,
) -> (bool, DownloadResult<()>) {
    let (mock_protocol, expected_data) = build_mock_protocol(total_size);
    let protocol: Arc<dyn Protocol> = Arc::new(ChaoticProtocol::new(mock_protocol, config.clone()));

    // MemoryStorage 共享 Arc<Mutex<Vec<u8>>>,clone 后写入对 clone 可见
    let mem_storage = MemoryStorage::with_capacity(total_size as usize);
    let mem_storage_clone = mem_storage.clone();
    let storage = ChaoticStorage::new(mem_storage, config.clone());
    let dyn_storage = StorageKind::new(storage);

    // 构造下载配置:用混沌 max_retries
    let download_config = DownloadConfig {
        max_retries,
        verify_checksum: false, // chaos 测试不校验哈希(MockProtocol 无 hash)
        ..tachyon_core::test_harness::harness::test_config()
    };

    let mut task = DownloadTask::new_for_test(
        "http://example.com/chaos.bin".into(),
        download_config,
        protocol,
        dyn_storage,
    );

    // 设置控制通道(取消信号注入)
    let (cancel_tx, cancel_rx) = watch::channel(TaskCommand::Start);
    task.set_control_rx(cancel_rx);

    // 取消注入任务:按概率发送 Cancel
    let cancel_injector: Option<tokio::task::JoinHandle<()>> = if config.cancel_prob > 0.0 {
        let cancel_prob = config.cancel_prob;
        let seed = config.seed;
        Some(tokio::spawn(async move {
            let mut rng = StdRng::seed_from_u64(seed.wrapping_add(0x12345678));
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if rng.random::<f64>() < cancel_prob {
                    let _ = cancel_tx.send(TaskCommand::Cancel);
                    break;
                }
            }
        }))
    } else {
        None
    };

    // 执行下载(走真实 run() 路径)
    let result = task.run().await;

    if let Some(handle) = cancel_injector {
        let _ = handle.await;
    }

    // 判断结果
    let cancelled = matches!(&result, Err(DownloadError::Cancelled))
        || task.state() == tachyon_core::types::DownloadState::Cancelled;

    if cancelled {
        return (true, result);
    }

    // 非取消:验证数据完整性
    if result.is_ok() {
        let actual = mem_storage_clone.get_data();
        // 仅验证已写入部分(混沌下可能部分分片未完成,但 run() 成功意味着全部分片完成)
        assert_eq!(
            actual.len(),
            expected_data.len(),
            "混沌下载完成后数据长度应一致"
        );
        assert_eq!(actual, expected_data, "混沌下载完成后数据内容应完整一致");
    }

    (false, result)
}

// ── 测试用例 ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn chaos_mild_network_and_storage_delays_succeeds() {
    let (_, result) = run_chaos_download(ChaosConfig::mild(42), 128 * 1024, 5).await;
    assert!(result.is_ok(), "温和故障下应成功完成: {:?}", result.err());
}

#[tokio::test(flavor = "multi_thread")]
async fn chaos_harsh_network_recoverable() {
    let (_, result) =
        run_chaos_download(ChaosConfig::harsh_but_recoverable(12345), 64 * 1024, 15).await;
    assert!(
        result.is_ok(),
        "高故障率下应通过重试恢复: {:?}",
        result.err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chaos_random_cancellation_does_not_panic() {
    let (cancelled, result) =
        run_chaos_download(ChaosConfig::with_cancellation(99999), 64 * 1024, 5).await;
    // 本测试核心:不 panic。
    // 取消时 result 应为 Err(Cancelled);未取消时,在高混沌下可能因网络故障
    // 耗尽重试而 Err(Network) —— 这也是"不 panic"的合法终态,不算失败。
    if cancelled {
        assert!(
            matches!(result, Err(DownloadError::Cancelled)),
            "取消时应为 Cancelled 错误,实际: {:?}",
            result.err()
        );
    }
    // 未取消时不额外断言 result(混沌注入可能导致重试耗尽后的网络错误,
    // 这是测试的预期行为之一,只要不 panic 即通过)
}

#[tokio::test(flavor = "multi_thread")]
async fn chaos_empty_file_no_panic() {
    let (_, result) = run_chaos_download(ChaosConfig::mild(777), 0, 3).await;
    assert!(result.is_ok(), "空文件应直接成功: {:?}", result.err());
}

#[tokio::test(flavor = "multi_thread")]
async fn chaos_single_fragment_with_delays() {
    let (_, result) = run_chaos_download(ChaosConfig::mild(555), 32 * 1024, 3).await;
    assert!(result.is_ok(), "单分片延迟场景应成功: {:?}", result.err());
}
