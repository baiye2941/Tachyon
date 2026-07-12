//! 端到端下载流程基准测试
//!
//! 测试核心下载路径的 CPU 性能：元数据探测、分片规划、状态机转换、
//! 快照序列化/反序列化、恢复加载等。所有测试使用内存或本地文件系统，
//! 不进行真实 HTTP 请求。

mod support;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::path::PathBuf;
use support::bench_config;
use tachyon_core::DownloadState;
use tachyon_engine::fragment::{BandwidthTracker, FragmentRecord, compute_fragment_size};
use tachyon_store::{KvStore, RecoveryManager, TaskSnapshot};
use tempfile::TempDir;

fn temp_dir() -> PathBuf {
    let dir = TempDir::new().unwrap();
    dir.keep()
}

fn make_snapshot(id: &str, status: DownloadState, downloaded: u64, fragments: u32) -> TaskSnapshot {
    TaskSnapshot {
        schema_version: tachyon_store::SNAPSHOT_SCHEMA_VERSION,
        id: id.to_string(),
        url: format!("https://example.com/{}.bin", id),
        save_path: format!("/tmp/{}.bin", id),
        file_name: format!("{}.bin", id),
        file_size: Some(1024 * 1024),
        downloaded,
        completed_fragments: (0..fragments / 2).collect(),
        partial_fragments: std::collections::HashMap::new(),
        total_fragments: fragments,
        fragment_size: 1024 * 1024 / fragments as u64,
        status,
        etag: Some("etag123".to_string()),
        last_modified: Some("2026-01-01T00:00:00Z".to_string()),
        content_length: Some(1024 * 1024),
        created_at: String::new(),
        updated_at: String::new(),
        fail_reason: None,
        retry_count: 0,
        tags: vec![],
        hf_meta: None,
        display_order: 0,
    }
}

fn make_fragment_record(index: u32, size: u64) -> FragmentRecord {
    let info = tachyon_core::types::FragmentInfo {
        index,
        start: index as u64 * size,
        end: (index as u64 + 1) * size - 1,
        size,
        downloaded: 0,
        hash: None,
    };
    FragmentRecord::new(info, 3)
}

fn bench_snapshot_save_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_save_load");
    for fragment_count in [1, 4, 16, 64].iter() {
        group.bench_with_input(
            BenchmarkId::new("save_load", fragment_count),
            fragment_count,
            |b, &fragments| {
                let dir = temp_dir();
                let kv = KvStore::open(&dir).unwrap();
                let manager = RecoveryManager::new(kv);
                let snapshot =
                    make_snapshot("bench-1", DownloadState::Downloading, 512 * 1024, fragments);

                b.iter(|| {
                    manager.save_task_snapshot(&snapshot).unwrap();
                    let _loaded = manager.load_task_snapshot("bench-1").unwrap();
                });
            },
        );
    }
    group.finish();
}

fn bench_snapshot_batch_save(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_batch_save");
    for count in [10, 50, 100].iter() {
        group.bench_with_input(BenchmarkId::new("batch", count), count, |b, &count| {
            b.iter_batched(
                || {
                    let dir = temp_dir();
                    let kv = KvStore::open(&dir).unwrap();
                    let manager = RecoveryManager::new(kv);
                    (manager, dir)
                },
                |(manager, _dir)| {
                    for i in 0..count {
                        let snapshot =
                            make_snapshot(&format!("task-{}", i), DownloadState::Downloading, 0, 4);
                        manager.save_task_snapshot(&snapshot).unwrap();
                    }
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_recover_pending(c: &mut Criterion) {
    let mut group = c.benchmark_group("recover_pending");
    for count in [10, 50, 100].iter() {
        group.bench_with_input(BenchmarkId::new("recover", count), count, |b, &count| {
            b.iter_batched(
                || {
                    let dir = temp_dir();
                    let kv = KvStore::open(&dir).unwrap();
                    let manager = RecoveryManager::new(kv);
                    for i in 0..count {
                        let status = if i % 3 == 0 {
                            DownloadState::Downloading
                        } else if i % 3 == 1 {
                            DownloadState::Paused
                        } else {
                            DownloadState::Failed
                        };
                        let snapshot = make_snapshot(&format!("task-{}", i), status, i * 1024, 4);
                        manager.save_task_snapshot(&snapshot).unwrap();
                    }
                    (manager, dir)
                },
                |(manager, _dir)| {
                    let _pending = manager.recover_pending_snapshots().unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_fragment_size_computation(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_fragment_size");
    let cases: &[(&str, u64, u64)] = &[
        ("1MB", 1024 * 1024, 1_000_000),
        ("10MB", 10 * 1024 * 1024, 10_000_000),
        ("100MB", 100 * 1024 * 1024, 100_000_000),
        ("1GB", 1024 * 1024 * 1024, 1_000_000_000),
    ];

    for (name, file_size, bandwidth) in cases.iter() {
        group.bench_with_input(
            BenchmarkId::new("compute", name),
            &(file_size, bandwidth),
            |b, &(fs, bw)| {
                b.iter(|| {
                    compute_fragment_size(
                        *fs,
                        *bw,
                        256 * 1024,
                        64 * 1024 * 1024,
                        16,
                        100 * 1024 * 1024,
                        10 * 1024 * 1024,
                    )
                });
            },
        );
    }
    group.finish();
}

fn bench_fragment_lifecycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_fragment_lifecycle");
    group.bench_function("full_lifecycle", |b| {
        b.iter(|| {
            let mut record = make_fragment_record(0, 64 * 1024);
            let _ = record.start_download();
            let _ = record.complete_download(16, std::time::Duration::from_millis(50));
            let _ = record.verify_ok();
            let _ = record.write_done();
            assert!(record.is_done());
        });
    });
    group.finish();
}

fn bench_bandwidth_tracking_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_bandwidth");
    group.bench_function("record_estimate_1000", |b| {
        let mut tracker = BandwidthTracker::new(0.3);
        let mut sample = 1_000_000u64;
        b.iter(|| {
            for _ in 0..1000 {
                sample = sample * 95 / 100 + 2_000_000 * 5 / 100;
                tracker.record(sample);
                let _est = tracker.estimate();
            }
        });
    });
    group.finish();
}

/// 端到端下载执行路径基准:MockProtocol(分块流) -> probe -> execute_fragmented_download
///
/// 覆盖旧 bench 未触及的真实下载热路径:分片规划、并发下载、流式写入、
/// 状态机转换。MockProtocol 用 with_chunk_size 模拟 HTTP chunked transfer,
/// 使 download_range_stream 按 chunk 多次产出,覆盖引擎流读取循环的逐块刷写路径。
///
/// 数据规模:4 MiB / 4 分片 / 256 KiB chunk,兼顾可测性与运行速度。
fn bench_execute_download_path(c: &mut Criterion) {
    use bytes::Bytes;
    use std::sync::Arc;
    use tachyon_core::test_harness::harness::{test_config, test_metadata};
    use tachyon_core::traits::Protocol;
    use tachyon_engine::{DownloadTask, StorageKind};

    // 4 MiB 数据,4 个 1 MiB 分片,每分片按 256 KiB chunk 流式产出
    const FILE_SIZE: u64 = 4 * 1024 * 1024;
    const FRAGMENT_SIZE: u64 = 1024 * 1024;
    const CHUNK_SIZE: usize = 256 * 1024;

    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("e2e_execute_download");
    support::configure_group(&mut group, 10);

    group.bench_function("4MiB_4frag_chunked_mock", |b| {
        b.iter(|| {
            rt.block_on(async {
                // 构造 MockProtocol:每个分片区间填充随机数据,启用分块流模式
                let mut protocol = tachyon_core::test_harness::harness::MockProtocol::new(
                    test_metadata("bench.bin", FILE_SIZE),
                );
                let payload = Bytes::from(vec![0xA5u8; FRAGMENT_SIZE as usize]);
                for i in 0..4u64 {
                    let start = i * FRAGMENT_SIZE;
                    let end = start + FRAGMENT_SIZE - 1;
                    protocol = protocol.with_range_data(start, end, payload.clone());
                }
                protocol = protocol.with_chunk_size(CHUNK_SIZE);

                let protocol: Arc<dyn Protocol> = Arc::new(protocol);
                let mut task = DownloadTask::new_for_test(
                    "http://example.com/bench.bin".into(),
                    test_config(),
                    protocol,
                    StorageKind::memory(),
                );

                // probe 设置 metadata,plan 据此规划分片,execute 走并发分片下载
                task.probe().await.expect("probe 失败");
                task.plan().expect("plan 失败");
                task.execute().await.expect("execute 失败");
                assert_eq!(
                    task.state(),
                    tachyon_core::DownloadState::Completed,
                    "execute 后任务状态应为 Completed"
                );
            });
        });
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = bench_config();
    targets =
        bench_snapshot_save_load,
        bench_snapshot_batch_save,
        bench_recover_pending,
        bench_fragment_size_computation,
        bench_fragment_lifecycle,
        bench_bandwidth_tracking_cycle,
        bench_execute_download_path
}
criterion_main!(benches);
