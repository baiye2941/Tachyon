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

/// HTTP/2 多路复用 vs HTTP/1.1 对比,验证 H2 在高 RTT 下的多路复用收益
///
/// 4 个并发 Range 请求(分片下载),人工 RTT 50ms 放大连接建立成本。
/// H2: 4 个请求复用 1 个 TCP 连接(省 3 个连接握手 RTT)
/// H1: 4 个请求各建独立 TCP 连接(4 × 握手 RTT)
///
/// 明文 loopback 上 reqwest 默认走 H1。H2 子 bench 通过 `http2_prior_knowledge()`
/// 强制 h2c(H2 over cleartext,客户端直接发 H2 preface),server 用 `auto::Builder`
/// 检测 H2 preface 自动切换到 H2。这验证了产品激进 H2 参数(1MiB 流窗口 /
/// 16MiB 连接窗口 / 1MiB 帧)与 H2 server 的互操作性。
///
/// 不声称 loopback 上 H2 吞吐更快(帧开销在小数据量上可能抵消收益),
/// 只验证 H2 互操作性 + 高 RTT 下多路复用省连接握手 RTT 的收益。
fn bench_http2_vs_http1_multiplexing(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("http2_vs_http1_multiplexing");
    support::configure_group(&mut group, 10);

    // CI 模式用小文件(512KiB)避免超时;完整模式用 2MiB(>1MB 触发分片)
    let file_size = if support::smoke_mode() {
        512 * 1024 // CI: 512KiB
    } else {
        2 * 1024 * 1024 // 完整: 2MiB
    };
    // 人工 RTT 50ms 放大连接建立成本,使 H2 多路复用收益可见
    let rtt_ms = 50;

    // H2 子 bench:auto server(支持 H1/H2 自动协商),HttpClient 用 h2c_prior_knowledge
    // 强制 h2c。明文 loopback 上 reqwest 默认不发 H2 preface,需 prior_knowledge 强制。
    // 用 HttpClient::h2c_prior_knowledge 注入 h2c client(仍走 tachyon-protocol 层,
    // 不绕过),H2 参数与产品 build_client 完全一致。
    let mut h2_server = rt.block_on(async {
        ThrottledServer::start_with_protocol(
            file_size,
            0,
            rtt_ms,
            DEFAULT_CHUNK_SIZE,
            BenchProtocol::Auto,
        )
        .await
    });
    let h2_url = format!("{}/bench.bin", h2_server.uri());
    let h2_client = Arc::new(HttpClient::h2c_prior_knowledge(5, 30, None).unwrap());

    group.bench_function("h2_multiplexed", |b| {
        b.to_async(&rt).iter(|| {
            let h2_client = h2_client.clone();
            let h2_url = h2_url.clone();
            async move {
                // 4 个分片并发,模拟分片下载。H2 下复用单 TCP 连接(多路复用)。
                let futures: Vec<_> = (0..4u64)
                    .map(|i| {
                        let start = i * (file_size / 4);
                        let end = if i == 3 {
                            file_size - 1
                        } else {
                            start + file_size / 4 - 1
                        };
                        let url = h2_url.clone();
                        let client = h2_client.clone();
                        async move {
                            let bytes = client.download_range(&url, start, end).await.unwrap();
                            assert_eq!(bytes.len() as u64, end - start + 1);
                        }
                    })
                    .collect();
                futures::future::join_all(futures).await;
            }
        });
    });

    h2_server.shutdown();

    // H1 子 bench:Http1Only server,HttpClient 禁用 H2(with_timeouts)。
    // 4 个 Range 请求各建独立 TCP 连接(4 × 握手 RTT)。
    let mut h1_server = rt.block_on(async {
        ThrottledServer::start_with_protocol(
            file_size,
            0,
            rtt_ms,
            DEFAULT_CHUNK_SIZE,
            BenchProtocol::Http1Only,
        )
        .await
    });
    let h1_url = format!("{}/bench.bin", h1_server.uri());
    let h1_client = Arc::new(HttpClient::with_timeouts(5, 30, None).unwrap());

    group.bench_function("h1_multiple_connections", |b| {
        b.to_async(&rt).iter(|| {
            let h1_client = h1_client.clone();
            let h1_url = h1_url.clone();
            async move {
                // 4 个分片并发,H1 下各建独立连接(4 × 握手 RTT)
                let futures: Vec<_> = (0..4u64)
                    .map(|i| {
                        let start = i * (file_size / 4);
                        let end = if i == 3 {
                            file_size - 1
                        } else {
                            start + file_size / 4 - 1
                        };
                        let url = h1_url.clone();
                        let client = h1_client.clone();
                        async move {
                            let bytes = client.download_range(&url, start, end).await.unwrap();
                            assert_eq!(bytes.len() as u64, end - start + 1);
                        }
                    })
                    .collect();
                futures::future::join_all(futures).await;
            }
        });
    });

    h1_server.shutdown();
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
        bench_real_network
}
criterion_main!(benches);
