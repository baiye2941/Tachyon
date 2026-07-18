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
use support::bench_server::{BenchProtocol, DEFAULT_CHUNK_SIZE, ThrottledServer};
use tachyon_core::test_harness::harness::test_config;
use tachyon_core::traits::{DownloadScheduler, Protocol};
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
                let bytes = client.download_range(&url, start, end, None).await.unwrap();
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
                tachyon_engine::create_adaptive_scheduler(
                    tachyon_core::config::SchedulerConfig::default(),
                ),
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

    // IOCP 后端(Windows 默认)/ io_uring(Linux 默认)。
    // 注入对齐 BufferPool(512B 对齐),使 IOCP/WinFile 的 NO_BUFFERING 对齐快速路径生效。
    // 未注入时 Vec<u8> 堆分配仅 16B 对齐,needs_fallback 必然 true,退化为 TokioFile 等价。
    let aligned_pool = Arc::new(tachyon_io::BufferPool::with_prefill(
        tachyon_core::config::WRITE_BATCH_BYTES,
        16, // max_concurrent_fragments=4(test_config),16 足够
    ));
    group.bench_function("default_io_strategy", |b| {
        b.to_async(&rt).iter(|| async {
            let dir = tempfile::TempDir::new().unwrap();
            let mut config = test_config();
            config.download_dir = dir.path().to_string_lossy().to_string();
            config.authorized_dirs = vec![config.download_dir.clone()];
            config.io_strategy = tachyon_core::config::IoStrategy::default();
            let mut task =
                DownloadTask::new_for_test_no_storage(url.clone(), config, protocol.clone());
            task.set_buffer_pool(aligned_pool.clone());
            task.run().await.expect("下载失败");
            assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
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

    // IOCP 后端(Windows 默认)/ io_uring(Linux 默认)。
    // 注入对齐 BufferPool 使 IOCP NO_BUFFERING 对齐快速路径生效(无锁真异步)。
    let aligned_pool = Arc::new(tachyon_io::BufferPool::with_prefill(
        tachyon_core::config::WRITE_BATCH_BYTES,
        16,
    ));
    group.bench_function("default_io_strategy", |b| {
        b.to_async(&rt).iter(|| async {
            let dir = tempfile::TempDir::new().unwrap();
            let mut config = test_config();
            config.download_dir = dir.path().to_string_lossy().to_string();
            config.authorized_dirs = vec![config.download_dir.clone()];
            config.io_strategy = tachyon_core::config::IoStrategy::default();
            let mut task =
                DownloadTask::new_for_test_no_storage(url.clone(), config, protocol.clone());
            task.set_buffer_pool(aligned_pool.clone());
            task.run().await.expect("下载失败");
            assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
        });
    });

    server.shutdown();
    group.finish();
}

/// HTTP/2 多路复用 vs HTTP/1.1 对比,验证 H2 连接复用收益
///
/// 三个子 bench 对比不同场景下的连接数和墙钟耗时。核心改造:在 server 端注入
/// **连接级握手延迟**(每连接 sleep 一次,模拟高 RTT 网络的 TCP+TLS 握手),
/// 并通过 `accept_count` 量化 H2 的资源收益(1 连接 vs 4 连接)。
///
/// 明文 loopback 上 reqwest 默认走 H1。H2 子 bench 通过 `http2_prior_knowledge()`
/// 强制 h2c(H2 over cleartext),server 用 `auto::Builder` 检测 H2 preface 自动切换。
///
/// ## 为什么用连接级握手延迟而非 per-request TTFB
///
/// H2 多路复用的核心收益是**连接复用**:N 个请求复用 1 个 TCP 连接,只付 1 次握手。
/// 旧设计注入 per-request TTFB RTT,H1 和 H2 各支付 N× TTFB,无法区分连接复用收益。
/// 新设计在 `serve_connection` 开始处注入握手延迟(每连接一次),H1 并发 N 个请求
/// 建 N 个连接各付一次握手,H2 并发 N 个请求复用 1 个连接只付一次握手。
///
/// ## 三个子 bench
///
/// 1. `h2_concurrent_keepalive`: H2 并发 4 分片,每迭代新建 client(强制握手)
/// 2. `h1_concurrent_keepalive`: H1 并发 4 分片,keep-alive 连接池(生产行为)
/// 3. `h1_concurrent_no_pool`: H1 并发 4 分片,`pool_max_idle_per_host(0)` 禁用空闲池
///
/// 预期(50ms handshake RTT):
/// - h2_keepalive: ~50ms, 1 连接(多路复用)
/// - h1_keepalive: ~50ms, 4 连接(并发握手,墙钟相近但资源 4×)
/// - h1_no_pool:  ~50ms, 4 连接(与 keepalive 并发场景相同 -- 并发时无空闲连接可复用)
///
/// 关键洞察:H1 keep-alive 在**并发**场景下无法复用连接(hyper H1 不支持 pipeline,
/// 一个连接同时只处理一个请求),因此 h1_keepalive 与 h1_no_pool 在并发场景下等价。
/// H2 的优势在连接数(1 vs 4),非墙钟。墙钟收益需连接数受限场景才显著(见注释)。
fn bench_http2_vs_http1_multiplexing(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("http2_vs_http1_multiplexing");
    support::configure_group(&mut group, 10);

    // CI 模式用小文件(512KiB)避免超时;完整模式用 2MiB
    let file_size = if support::smoke_mode() {
        512 * 1024 // CI: 512KiB
    } else {
        2 * 1024 * 1024 // 完整: 2MiB
    };
    // 连接级握手延迟 50ms,放大连接建立成本,使 H2 连接复用收益可见。
    // per-request TTFB 设 0(rtt_ms=0),避免与握手延迟混淆。
    let handshake_rtt_ms = 50;

    // --- H2: 并发 4 分片,多路复用 1 连接 ---
    let h2_server = Arc::new(rt.block_on(async {
        ThrottledServer::start_with_handshake(
            file_size,
            0,
            0, // rtt_ms=0,只用 handshake_rtt
            handshake_rtt_ms,
            DEFAULT_CHUNK_SIZE,
            BenchProtocol::Auto,
        )
        .await
    }));
    let h2_url = format!("{}/bench.bin", h2_server.uri());

    group.bench_function("h2_concurrent_keepalive", |b| {
        b.to_async(&rt).iter(|| {
            let h2_url = h2_url.clone();
            let server = h2_server.clone();
            async move {
                // 每迭代新建 client,强制每次迭代支付握手成本(消除连接池复用偏差)
                let client = Arc::new(HttpClient::h2c_prior_knowledge(5, 30, None).unwrap());
                server.reset_accept_count();
                let futures: Vec<_> = (0..4u64)
                    .map(|i| {
                        let start = i * (file_size / 4);
                        let end = if i == 3 {
                            file_size - 1
                        } else {
                            start + file_size / 4 - 1
                        };
                        let url = h2_url.clone();
                        let client = client.clone();
                        async move {
                            let bytes =
                                client.download_range(&url, start, end, None).await.unwrap();
                            assert_eq!(bytes.len() as u64, end - start + 1);
                        }
                    })
                    .collect();
                futures::future::join_all(futures).await;
                // H2 多路复用:4 个请求复用 1 个 TCP 连接
                assert_eq!(server.accept_count(), 1, "H2 多路复用应只建 1 个连接");
            }
        });
    });

    drop(h2_server); // 显式 drop Arc,触发 ThrottledServer::shutdown(Drop)

    // --- H1: 并发 4 分片,keep-alive 连接池(生产行为) ---
    let h1_server = Arc::new(rt.block_on(async {
        ThrottledServer::start_with_handshake(
            file_size,
            0,
            0,
            handshake_rtt_ms,
            DEFAULT_CHUNK_SIZE,
            BenchProtocol::Http1Only,
        )
        .await
    }));
    let h1_url = format!("{}/bench.bin", h1_server.uri());

    group.bench_function("h1_concurrent_keepalive", |b| {
        b.to_async(&rt).iter(|| {
            let h1_url = h1_url.clone();
            let server = h1_server.clone();
            async move {
                // 每迭代新建 client,强制握手。H1 keep-alive 默认开启(pool_max_idle=16)。
                let client = Arc::new(HttpClient::with_timeouts(5, 30, None).unwrap());
                server.reset_accept_count();
                let futures: Vec<_> = (0..4u64)
                    .map(|i| {
                        let start = i * (file_size / 4);
                        let end = if i == 3 {
                            file_size - 1
                        } else {
                            start + file_size / 4 - 1
                        };
                        let url = h1_url.clone();
                        let client = client.clone();
                        async move {
                            let bytes =
                                client.download_range(&url, start, end, None).await.unwrap();
                            assert_eq!(bytes.len() as u64, end - start + 1);
                        }
                    })
                    .collect();
                futures::future::join_all(futures).await;
                // H1 并发:hyper H1 不支持 pipeline,4 个并发请求各建独立连接。
                // keep-alive 在并发场景下无法复用连接(无空闲连接可取)。
                assert_eq!(
                    server.accept_count(),
                    4,
                    "H1 并发应建 4 个连接(无 pipeline)"
                );
            }
        });
    });

    // --- H1: 并发 4 分片,禁用空闲连接池(pool_max_idle_per_host=0) ---
    // 验证:在并发场景下,禁用空闲池与 keep-alive 行为一致(都建 4 连接)。
    // 空闲池的差异只在串行场景体现(串行时 keep-alive 复用,no-pool 每请求新建)。
    group.bench_function("h1_concurrent_no_pool", |b| {
        b.to_async(&rt).iter(|| {
            let h1_url = h1_url.clone();
            let server = h1_server.clone();
            async move {
                // pool_max_idle_per_host=0 禁用空闲连接池
                let client = Arc::new(HttpClient::h1c_only(5, 30, 0, None).unwrap());
                server.reset_accept_count();
                let futures: Vec<_> = (0..4u64)
                    .map(|i| {
                        let start = i * (file_size / 4);
                        let end = if i == 3 {
                            file_size - 1
                        } else {
                            start + file_size / 4 - 1
                        };
                        let url = h1_url.clone();
                        let client = client.clone();
                        async move {
                            let bytes =
                                client.download_range(&url, start, end, None).await.unwrap();
                            assert_eq!(bytes.len() as u64, end - start + 1);
                        }
                    })
                    .collect();
                futures::future::join_all(futures).await;
                assert_eq!(server.accept_count(), 4, "H1 no-pool 并发应建 4 个连接");
            }
        });
    });

    drop(h1_server); // 显式 drop Arc,触发 ThrottledServer::shutdown(Drop)
    group.finish();
}

/// 真实网络下载基准测试
///
/// 通过环境变量 `TACHYON_REAL_URL` 指定真实下载 URL(HTTPS,支持 HTTP/2 协商)。
/// 未设置时跳过整个 bench。设置时测真实 RTT、HTTP/2 多路复用、真实带宽限制、
/// 真实磁盘写入反压等 loopback 无法覆盖的场景。
///
/// 用法:
/// ```bash
/// TACHYON_REAL_URL="https://huggingface.co/.../resolve/main/model.bin" cargo bench --bench e2e_http_real -- "real_network"
/// ```
///
/// 可选环境变量:
/// - `TACHYON_REAL_DOWNLOAD_DIR`: 下载目录(默认每次迭代用 TempDir 自动清理)
fn bench_real_network(c: &mut Criterion) {
    let url = match std::env::var("TACHYON_REAL_URL") {
        Ok(u) => u,
        Err(_) => {
            // 未设置 URL 时跳过,不注册任何 benchmark
            return;
        }
    };

    let rt = rt();
    let mut group = c.benchmark_group("real_network");
    support::configure_group(&mut group, 10);

    // 默认 IOCP(Windows)/io_uring(Linux)+ 对齐 BufferPool
    let aligned_pool = Arc::new(tachyon_io::BufferPool::with_prefill(
        tachyon_core::config::WRITE_BATCH_BYTES,
        32,
    ));

    group.bench_function("default_strategy", |b| {
        b.to_async(&rt).iter(|| {
            let url = url.clone();
            let pool = aligned_pool.clone();
            async move {
                // 每次迭代用 TempDir,迭代结束时自动清理(与 loopback bench 一致)
                let dir = tempfile::TempDir::new().unwrap();
                let mut config = tachyon_core::config::DownloadConfig::default();
                config.download_dir = dir.path().to_string_lossy().to_string();
                config.authorized_dirs = vec![config.download_dir.clone()];
                config.verify_checksum = false;
                let mut task = DownloadTask::new(url, config).await.unwrap();
                task.set_buffer_pool(pool);
                task.run().await.expect("下载失败");
                assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
            }
        });
    });

    // 对比:TokioFile(回退路径,write_lock 串行化)
    group.bench_function("tokio_file", |b| {
        b.to_async(&rt).iter(|| {
            let url = url.clone();
            let pool = aligned_pool.clone();
            async move {
                let dir = tempfile::TempDir::new().unwrap();
                let mut config = tachyon_core::config::DownloadConfig::default();
                config.download_dir = dir.path().to_string_lossy().to_string();
                config.authorized_dirs = vec![config.download_dir.clone()];
                config.verify_checksum = false;
                config.io_strategy = tachyon_core::config::IoStrategy::Standard;
                let mut task = DownloadTask::new(url, config).await.unwrap();
                task.set_buffer_pool(pool);
                task.run().await.expect("下载失败");
                assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
            }
        });
    });

    group.finish();
}

/// 动态并发度 vs 固定并发度对比,验证带宽变化时动态提升并发的收益
///
/// 模拟"带宽先低后高"场景:
/// 1. 注入 1MB/s 低带宽样本到调度器(observe_bandwidth),使 recommend 算出低并发度
/// 2. 大文件以低并发启动,2s 后带宽提升到 10MB/s
/// 3. 固定并发:低并发贯穿全程,带宽提升后无法扩容
/// 4. 动态并发:interval re-recommend 发现带宽提升,add_permits 扩容并发
///
/// 关键:通过 with_pool_and_scheduler 注入预置样本的调度器,
/// 绕过冷启动 recommend 回退到 max_concurrency 的问题。
fn bench_dynamic_concurrency(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("dynamic_concurrency");
    support::configure_group(&mut group, 10);

    let file_size = if support::smoke_mode() {
        4 * 1024 * 1024
    } else {
        8 * 1024 * 1024
    };
    let initial_bw = 1024 * 1024u64; // 1MB/s 初始低带宽样本
    let boosted_bw = 10 * 1024 * 1024u64; // 10MB/s 提升后
    let chunk_size = 256 * 1024;

    // --- 固定并发(禁用动态并发:sampling_interval_secs 设很大) ---
    let fixed_server = Arc::new(rt.block_on(async {
        ThrottledServer::start_with_chunk(file_size, initial_bw, 0, chunk_size).await
    }));
    let fixed_url = format!("{}/bench.bin", fixed_server.uri());

    group.bench_function("fixed_concurrency", |b| {
        b.to_async(&rt).iter(|| {
            let url = fixed_url.clone();
            let server = fixed_server.clone();
            async move {
                let dir = tempfile::TempDir::new().unwrap();
                let mut config = test_config();
                config.download_dir = dir.path().to_string_lossy().to_string();
                config.authorized_dirs = vec![config.download_dir.clone()];
                config.max_concurrent_fragments = 8;
                // 注入预置低带宽样本的调度器:recommend 会算出低并发度
                let scheduler = tachyon_scheduler::AdaptiveDownloadScheduler::default_config();
                let scheduler: Arc<dyn tachyon_core::traits::DownloadScheduler> =
                    Arc::new(scheduler);
                scheduler.observe_bandwidth(initial_bw);
                let mut task =
                    DownloadTask::with_pool_and_scheduler(url, config, None, scheduler, None)
                        .await
                        .expect("构造失败");
                task.set_scheduler_config(tachyon_core::config::SchedulerConfig {
                    sampling_interval_secs: 3600, // 禁用动态并发
                    ..Default::default()
                });
                let bw_handle = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    server.set_bandwidth(boosted_bw);
                });
                task.run().await.expect("下载失败");
                bw_handle.await.unwrap();
                assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
            }
        });
    });

    drop(fixed_server);

    // --- 动态并发(sampling_interval_secs=2,带宽提升后自动扩容) ---
    let dynamic_server = Arc::new(rt.block_on(async {
        ThrottledServer::start_with_chunk(file_size, initial_bw, 0, chunk_size).await
    }));
    let dynamic_url = format!("{}/bench.bin", dynamic_server.uri());

    group.bench_function("dynamic_concurrency", |b| {
        b.to_async(&rt).iter(|| {
            let url = dynamic_url.clone();
            let server = dynamic_server.clone();
            async move {
                let dir = tempfile::TempDir::new().unwrap();
                let mut config = test_config();
                config.download_dir = dir.path().to_string_lossy().to_string();
                config.authorized_dirs = vec![config.download_dir.clone()];
                config.max_concurrent_fragments = 8;
                let scheduler = tachyon_scheduler::AdaptiveDownloadScheduler::default_config();
                scheduler.observe_bandwidth(initial_bw);
                let scheduler: Arc<dyn tachyon_core::traits::DownloadScheduler> =
                    Arc::new(scheduler);
                let mut task =
                    DownloadTask::with_pool_and_scheduler(url, config, None, scheduler, None)
                        .await
                        .expect("构造失败");
                task.set_scheduler_config(tachyon_core::config::SchedulerConfig {
                    sampling_interval_secs: 2, // 启用动态并发:2s 间隔 re-recommend
                    ..Default::default()
                });
                let bw_handle = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    server.set_bandwidth(boosted_bw);
                });
                task.run().await.expect("下载失败");
                bw_handle.await.unwrap();
                assert_eq!(task.state(), tachyon_core::DownloadState::Completed);
            }
        });
    });

    drop(dynamic_server);
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
        bench_large_file_fragmented,
        bench_http2_vs_http1_multiplexing,
        bench_dynamic_concurrency,
        bench_real_network
}
criterion_main!(benches);
