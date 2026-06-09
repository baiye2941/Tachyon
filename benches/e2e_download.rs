//! 端到端下载流程基准测试
//!
//! 测试核心下载路径的 CPU 性能：元数据探测、分片规划、状态机转换、
//! 快照序列化/反序列化、恢复加载等。所有测试使用内存或本地文件系统，
//! 不进行真实 HTTP 请求。

mod support;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use support::bench_config;
use std::path::PathBuf;
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
        id: id.to_string(),
        url: format!("https://example.com/{}.bin", id),
        save_path: format!("/tmp/{}.bin", id),
        file_name: format!("{}.bin", id),
        file_size: Some(1024 * 1024),
        downloaded,
        completed_fragments: (0..fragments / 2).collect(),
        total_fragments: fragments,
        fragment_size: 1024 * 1024 / fragments as u64,
        status,
        etag: Some("etag123".to_string()),
        last_modified: Some("2026-01-01T00:00:00Z".to_string()),
        content_length: Some(1024 * 1024),
        created_at: "2026-05-29T00:00:00Z".to_string(),
        updated_at: "2026-05-29T00:00:01Z".to_string(),
        fail_reason: None,
        retry_count: 0,
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
                b.iter(|| compute_fragment_size(*fs, *bw, 256 * 1024, 64 * 1024 * 1024, 16));
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
            record.start_download();
            record.complete_download(16, std::time::Duration::from_millis(50));
            record.verify_ok();
            record.write_done();
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

criterion_group! {
    name = benches;
    config = bench_config();
    targets =
        bench_snapshot_save_load,
        bench_snapshot_batch_save,
        bench_recover_pending,
        bench_fragment_size_computation,
        bench_fragment_lifecycle,
        bench_bandwidth_tracking_cycle
}
criterion_main!(benches);
