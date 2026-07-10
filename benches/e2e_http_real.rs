//! 真实 HTTP 下载基准测试
//!
//! 用 `ThrottledServer`(hyper streaming + chunk/sleep 节流)替代 MockProtocol,
//! 测真实 HTTP 下载路径(reqwest 连接建立 + H2 协商 + bytes_stream 分块)。
//! 验证此前 mock+memory bench 无法覆盖的优化候选:
//! - 动态 RTT 探测(probe 阶段测真实 RTT,plan 据此算并发度)
//! - 带宽采样与分片规划(BandwidthTracker 在节流下的采样行为)
//! - 多源聚合(MirrorProtocol 快源多干)
//! - 磁盘 IO 反压(真实磁盘 vs MemoryStorage)

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use support::bench_server::ThrottledServer;
use tachyon_core::test_harness::harness::test_config;
use tachyon_core::traits::Protocol;
use tachyon_engine::{ConnectionPool, DownloadTask, PoolConfig};
use tachyon_protocol::HttpClient;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

/// 真实 HTTP loopback 下载(无节流)
///
/// 测 reqwest 连接建立 + HTTP 解析 + bytes_stream 分块的真实 CPU 开销
/// (4 × 256KiB 串行 download_range)。server 在 bench 前启动一次,所有迭代复用。
fn bench_http_range_real_loopback(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("http_range_real");
    support::configure_group(&mut group, 10);

    // server 在 bench 前启动,所有迭代复用(OS 分配端口,零冲突)
    let mut server = rt.block_on(async { ThrottledServer::start(1024 * 1024, 0, 0).await });
    let client = HttpClient::with_timeouts(5, 10, None).unwrap();
    let url = format!("{}/bench.bin", server.uri());

    group.bench_function("1MiB_loopback", |b| {
        b.to_async(&rt).iter(|| async {
            // 4 个 256KiB 分片,模拟分片下载
            for i in 0..4u64 {
                let start = i * 256 * 1024;
                let end = start + 256 * 1024 - 1;
                let bytes = client.download_range(&url, start, end).await.unwrap();
                assert_eq!(bytes.len(), 256 * 1024);
            }
        });
    });

    server.shutdown();
    group.finish();
}

/// 节流 HTTP 下载,走 DownloadTask 完整路径验证 BandwidthTracker 采样
///
/// CI: 2MiB @ 10MB/s;完整: 8MiB @ 1MB/s。
/// 用 DownloadTask::run()(probe->plan->execute)替代 download_full,
/// 真正验证 BandwidthTracker 采样和分片规划在节流下的行为。
/// chunk_size 用 256KiB(4 chunk/1MiB)替代默认 64KiB(16 chunk/1MiB),
/// 减少 sleep 唤醒抖动(16->4 次,抖动降为 1/4),消除节流精度假象。
fn bench_http_range_throttled(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("http_range_throttled");
    support::configure_group(&mut group, 10);

    // CI 模式用高带宽(10MB/s)避免超时;完整模式用 1MB/s 测带宽采样
    // 文件 >1MB 触发分片(min_fragment_size=1MB),走 execute_fragmented_download
    let (file_size, bytes_per_sec) = if support::smoke_mode() {
        (2 * 1024 * 1024, 10 * 1024 * 1024) // CI: 2MiB @ 10MB/s ≈ 0.2s
    } else {
        (8 * 1024 * 1024, 1024 * 1024) // 完整: 8MiB @ 1MB/s ≈ 8s
    };
    let chunk_size = 256 * 1024; // 256KiB,减少节流 sleep 次数

    let mut server = rt.block_on(async {
        ThrottledServer::start_with_chunk(file_size, bytes_per_sec, 0, chunk_size).await
    });
    let url = format!("{}/bench.bin", server.uri());
    let dir = tempfile::TempDir::new().unwrap();

    group.bench_function("throttled_download", |b| {
        b.to_async(&rt).iter(|| async {
            let protocol: Arc<dyn Protocol> =
                Arc::new(HttpClient::with_timeouts(5, 30, None).unwrap());
            let mut config = test_config();
            config.download_dir = dir.path().to_string_lossy().to_string();
            config.authorized_dirs = vec![config.download_dir.clone()];
            let mut task = DownloadTask::new_for_test_no_storage(url.clone(), config, protocol);
            let start = Instant::now();
            task.run().await.expect("下载失败");
            let elapsed = start.elapsed();
            assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
            // 节流验证:DownloadTask 分片并发会突破单连接节流限制。
            // 2MiB 文件 >1MB(min_fragment_size)触发分片,N 个分片各自建独立
            // HTTP 连接,每个连接受 bytes_per_sec 节流,但 N 个连接聚合带宽
            // = N * bytes_per_sec。因此实际耗时可能远低于 file_size/bytes_per_sec
            // (单连接理论值)。这是正确行为——分片并行的收益。
            // 不设严格下限断言(调度器动态决定并发度),只记录耗时供分析。
            let single_conn_min = Duration::from_secs_f64(file_size as f64 / bytes_per_sec as f64);
            // 至少应完成下载(不卡死),且耗时不应超过单连接的 2 倍(容错)
            assert!(
                elapsed <= single_conn_min * 3,
                "节流下载耗时 {elapsed:?} 过长(单连接理论 {single_conn_min:?} 的 3 倍)"
            );
        });
    });

    server.shutdown();
    group.finish();
}

/// RTT 对下载耗时的影响,验证动态 RTT 探测
///
/// 对比 0ms RTT vs 50ms RTT 的下载耗时。高 RTT 下,probe 阶段的
/// observe_rtt 应反映真实 RTT,recommend 据此调整并发度。
fn bench_http_rtt_effect(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("http_rtt_effect");
    support::configure_group(&mut group, 10);

    let file_size = 1024 * 1024; // 1MiB

    for (rtt_ms, label) in [(0u64, "rtt_0ms"), (50, "rtt_50ms")] {
        // 不限速(只测 RTT 影响),高带宽避免带宽成为瓶颈
        let mut server = rt.block_on(async { ThrottledServer::start(file_size, 0, rtt_ms).await });
        let url = format!("{}/bench.bin", server.uri());
        let client = HttpClient::with_timeouts(5, 30, None).unwrap();

        group.bench_function(label, |b| {
            b.to_async(&rt).iter(|| async {
                let start = Instant::now();
                let bytes = client.download_full(&url).await.unwrap();
                let elapsed = start.elapsed();
                assert_eq!(bytes.len() as u64, file_size);
                // 高 RTT 下耗时应更长(至少多 1 个 RTT 用于首字节延迟)
                if rtt_ms > 0 {
                    assert!(
                        elapsed >= Duration::from_millis(rtt_ms),
                        "RTT={rtt_ms}ms 下载耗时 {elapsed:?} 应 >= RTT"
                    );
                }
            });
        });

        server.shutdown();
    }

    group.finish();
}

/// 多源聚合下载,验证 MirrorProtocol 快源多干(真实分片并发)
///
/// 3 个 ThrottledServer(快/中/慢,不同 RTT + 带宽),DownloadTask::with_mirrors
/// 下载同一文件。快源应承担更多分片(quality 高 -> 选源公式 score 小 -> 多被选)。
///
/// 关键设计:
/// - 文件 >1MB 触发多分片(min_fragment_size=1MB),走 execute_fragmented_download,
///   使 3 源能做分片级并行聚合(而非单分片 fallback)
/// - 闭包外预建 ConnectionPool 并传 Some(pool),避免每迭代重建 3 个 reqwest Client
///   (连接池/DNS缓存/TLS状态复用,隔离调度开销)
fn bench_mirror_aggregation(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("mirror_aggregation");
    support::configure_group(&mut group, 10);

    // CI: 2MiB(>1MB 触发分片);完整: 8MiB(8 分片,充分并发)
    let file_size = if support::smoke_mode() {
        2 * 1024 * 1024 // 2MiB
    } else {
        8 * 1024 * 1024 // 8MiB
    };

    // 3 源:快(5ms RTT, 高带宽) / 中(50ms, 中带宽) / 慢(200ms, 低带宽)
    // CI 用高带宽避免超时;完整用低带宽使分片分配差异更明显
    let bw = if support::smoke_mode() {
        20 * 1024 * 1024 // CI: 20MB/s(2MiB@20MB/s ≈ 100ms)
    } else {
        2 * 1024 * 1024 // 完整: 2MB/s
    };

    let (mut fast, mut mid, mut slow) = rt.block_on(async {
        (
            ThrottledServer::start(file_size, bw, 5).await,
            ThrottledServer::start(file_size, bw / 2, 50).await,
            ThrottledServer::start(file_size, bw / 4, 200).await,
        )
    });

    let mirror_urls = vec![
        format!("{}/bench.bin", mid.uri()),
        format!("{}/bench.bin", slow.uri()),
    ];
    let primary = format!("{}/bench.bin", fast.uri());

    // 预建连接池,所有迭代复用(避免 per-iteration 重建 3 个 reqwest Client)
    let pool = Arc::new(ConnectionPool::new(PoolConfig::default()));

    let dir = tempfile::TempDir::new().unwrap();

    group.bench_function("3sources_mixed", |b| {
        b.to_async(&rt).iter(|| async {
            let mut config = test_config();
            config.max_concurrent_fragments = 8;
            config.download_dir = dir.path().to_string_lossy().to_string();
            config.authorized_dirs = vec![config.download_dir.clone()];

            let task = DownloadTask::with_mirrors(
                primary.clone(),
                mirror_urls.clone(),
                config,
                Some(pool.clone()),
            )
            .await
            .expect("with_mirrors 构造失败");
            let mut task = task;
            task.run().await.expect("下载失败");
            assert_eq!(
                task.state(),
                tachyon_core::DownloadState::Completed,
                "多源下载应完成"
            );
        });
    });

    fast.shutdown();
    mid.shutdown();
    slow.shutdown();
    group.finish();
}

/// 磁盘 IO 后端对比:真实磁盘(TokioFile) vs MemoryStorage
///
/// 用 ThrottledServer 做源,DownloadTask 写入不同存储后端,测磁盘 IO 对下载的反压。
fn bench_disk_io_backends(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("disk_io_backends");
    support::configure_group(&mut group, 10);

    let file_size = if support::smoke_mode() {
        512 * 1024 // CI: 512KiB
    } else {
        4 * 1024 * 1024 // 完整: 4MiB
    };

    let mut server = rt.block_on(async { ThrottledServer::start(file_size, 0, 0).await });
    let url = format!("{}/bench.bin", server.uri());

    // Memory 后端(基线)
    let protocol: Arc<dyn Protocol> = Arc::new(HttpClient::with_timeouts(5, 30, None).unwrap());
    group.bench_function("memory_storage", |b| {
        b.to_async(&rt).iter(|| async {
            let mut task = DownloadTask::new_for_test(
                url.clone(),
                test_config(),
                protocol.clone(),
                tachyon_engine::StorageKind::memory(),
            );
            task.run().await.expect("下载失败");
            assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
        });
    });

    // 真实磁盘后端(TokioFile)。每次迭代创建新 TempDir,迭代结束时 TempDir drop
    // 自动清理文件(不留垃圾)。protocol 复用同一连接池(测磁盘 IO 非连接建立)。
    group.bench_function("tokio_file_storage", |b| {
        b.to_async(&rt).iter(|| async {
            let dir = tempfile::TempDir::new().unwrap();
            let mut config = test_config();
            config.download_dir = dir.path().to_string_lossy().to_string();
            config.authorized_dirs = vec![config.download_dir.clone()];
            config.io_strategy = tachyon_core::config::IoStrategy::Standard;
            let mut task =
                DownloadTask::new_for_test_no_storage(url.clone(), config, protocol.clone());
            task.run().await.expect("下载失败");
            assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
            // dir 在此 drop,自动删除临时文件
        });
    });

    server.shutdown();
    group.finish();
}

/// 大文件分片下载,暴露分片并发的真实行为
///
/// CI: 4MiB(4 分片);完整: 16MiB(16 分片)。无节流,loopback 全速。
/// 走 DownloadTask::run() 完整路径,对比 memory vs tokio_file 存储,
/// 验证多分片是否真正并行 + 大文件磁盘写入反压。
fn bench_large_file_fragmented(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("large_file_fragmented");
    support::configure_group(&mut group, 10);

    let file_size = if support::smoke_mode() {
        4 * 1024 * 1024 // CI: 4MiB(4 分片,1MB/分片)
    } else {
        16 * 1024 * 1024 // 完整: 16MiB(16 分片)
    };

    let mut server = rt.block_on(async { ThrottledServer::start(file_size, 0, 0).await });
    let url = format!("{}/bench.bin", server.uri());
    let protocol: Arc<dyn Protocol> = Arc::new(HttpClient::with_timeouts(5, 30, None).unwrap());

    // Memory 后端(基线,无磁盘反压)
    group.bench_function("memory_storage", |b| {
        b.to_async(&rt).iter(|| async {
            let mut task = DownloadTask::new_for_test(
                url.clone(),
                test_config(),
                protocol.clone(),
                tachyon_engine::StorageKind::memory(),
            );
            task.run().await.expect("下载失败");
            assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
        });
    });

    // 真实磁盘后端(测磁盘写入反压)
    group.bench_function("tokio_file_storage", |b| {
        b.to_async(&rt).iter(|| async {
            let dir = tempfile::TempDir::new().unwrap();
            let mut config = test_config();
            config.download_dir = dir.path().to_string_lossy().to_string();
            config.authorized_dirs = vec![config.download_dir.clone()];
            config.io_strategy = tachyon_core::config::IoStrategy::Standard;
            let mut task =
                DownloadTask::new_for_test_no_storage(url.clone(), config, protocol.clone());
            task.run().await.expect("下载失败");
            assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
        });
    });

    server.shutdown();
    group.finish();
}

criterion_group! {
    name = benches;
    config = support::bench_config();
    targets =
        bench_http_range_real_loopback,
        bench_http_range_throttled,
        bench_http_rtt_effect,
        bench_mirror_aggregation,
        bench_disk_io_backends,
        bench_large_file_fragmented
}
criterion_main!(benches);
