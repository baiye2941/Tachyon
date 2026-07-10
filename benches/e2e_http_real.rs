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
use tachyon_engine::DownloadTask;
use tachyon_protocol::HttpClient;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

/// 真实 HTTP loopback 下载(无节流),对比 MockProtocol 2ms 基线
///
/// 测 reqwest 连接建立 + HTTP 解析 + bytes_stream 分块的真实 CPU 开销。
/// server 在 bench 前启动一次,所有迭代复用(测下载性能,非 server 启停)。
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

/// 节流 HTTP 下载,验证带宽采样和分片规划
///
/// CI: 1MiB @ 10MB/s;完整: 4MiB @ 1MB/s。
/// server 在 bench 前启动,所有迭代复用(节流按连接生效,
/// 每次迭代发新 HTTP 请求,server 持续 accept 新连接)。
fn bench_http_range_throttled(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("http_range_throttled");
    support::configure_group(&mut group, 10);

    // CI 模式用高带宽(10MB/s)避免超时;完整模式用 1MB/s 测带宽采样
    let (file_size, bytes_per_sec) = if support::smoke_mode() {
        (1024 * 1024, 10 * 1024 * 1024) // CI: 1MiB @ 10MB/s ≈ 0.1s
    } else {
        (4 * 1024 * 1024, 1024 * 1024) // 完整: 4MiB @ 1MB/s ≈ 4s
    };

    let mut server =
        rt.block_on(async { ThrottledServer::start(file_size, bytes_per_sec, 0).await });
    let url = format!("{}/bench.bin", server.uri());
    let client = HttpClient::with_timeouts(5, 30, None).unwrap();

    group.bench_function("throttled_download", |b| {
        b.to_async(&rt).iter(|| async {
            let start = Instant::now();
            // 整文件下载(无 Range,走 200 路径)
            let bytes = client.download_full(&url).await.unwrap();
            let elapsed = start.elapsed();
            assert_eq!(bytes.len() as u64, file_size);
            // 节流验证:实际耗时应 >= 理论下限(file_size / bytes_per_sec)。
            // chunk+sleep 节流是"带宽上限"语义:服务端每秒最多发 bytes_per_sec 字节,
            // 因此客户端耗时不会低于理论值(带宽是硬约束)。实际耗时会略高
            // (sleep 唤醒抖动 + HTTP/TCP 开销),但不应低于理论下限。
            // 只验证下界,不设上界断言(bench 环境波动大,上界易误报)。
            let theoretical_min = Duration::from_secs_f64(file_size as f64 / bytes_per_sec as f64);
            assert!(
                elapsed >= theoretical_min,
                "节流下载耗时 {elapsed:?} 应 >= 理论下限 {theoretical_min:?}"
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

/// 多源聚合下载,验证 MirrorProtocol 快源多干
///
/// 3 个 ThrottledServer(快/中/慢,不同 RTT + 带宽),DownloadTask::with_mirrors
/// 下载同一文件。快源应承担更多分片(quality 高 -> 选源公式 score 小 -> 多被选)。
fn bench_mirror_aggregation(c: &mut Criterion) {
    let rt = rt();
    let mut group = c.benchmark_group("mirror_aggregation");
    support::configure_group(&mut group, 10);

    // CI 模式缩小文件避免超时
    let file_size = if support::smoke_mode() {
        512 * 1024 // 512KiB
    } else {
        4 * 1024 * 1024 // 4MiB
    };

    // 3 源:快(5ms RTT, 高带宽) / 中(50ms, 中带宽) / 慢(200ms, 低带宽)
    let bw = if support::smoke_mode() {
        50 * 1024 * 1024 // CI: 50MB/s(避免超时)
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

    // with_mirrors 内部自建 storage(从 config.download_dir 落盘),
    // 用 TempDir 确保迭代结束后清理文件,不在系统 temp 留垃圾。
    // 所有迭代复用同一 TempDir(文件覆盖写入),bench 结束后 TempDir drop 清理。
    let dir = tempfile::TempDir::new().unwrap();

    group.bench_function("3sources_mixed", |b| {
        b.to_async(&rt).iter(|| async {
            let mut config = test_config();
            config.max_concurrent_fragments = 8;
            config.download_dir = dir.path().to_string_lossy().to_string();
            config.authorized_dirs = vec![config.download_dir.clone()];

            let task =
                DownloadTask::with_mirrors(primary.clone(), mirror_urls.clone(), config, None)
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

criterion_group! {
    name = benches;
    config = support::bench_config();
    targets =
        bench_http_range_real_loopback,
        bench_http_range_throttled,
        bench_http_rtt_effect,
        bench_mirror_aggregation,
        bench_disk_io_backends
}
criterion_main!(benches);
