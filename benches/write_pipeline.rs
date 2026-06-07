//! WritePipeline 顺序写基准测试
//! 测量 TokioFile 和 WritePipeline 在不同写入大小下的顺序写吞吐量，用于对比。

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tachyon_io::{AsyncStorage, TokioFile, WritePipeline};
use tempfile::NamedTempFile;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// TokioFile 顺序写基准：从偏移 0 开始逐块写入指定大小的数据
fn bench_tokiofile_sequential_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("tokiofile_sequential_write");
    group.sample_size(20);

    for size in [256 * 1024, 1024 * 1024, 4 * 1024 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let rt = rt();
            b.iter(|| {
                rt.block_on(async {
                    let tmp = NamedTempFile::new().unwrap();
                    let file = TokioFile::open(tmp.path()).await.unwrap();
                    let data = Bytes::from(vec![0u8; size]);
                    file.write_at(0, data).await.unwrap();
                    file.close().await.unwrap();
                });
            });
        });
    }

    group.finish();
}

/// WritePipeline 顺序写基准：通过缓冲管道从偏移 0 开始逐块写入指定大小的数据
fn bench_pipeline_sequential_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline_sequential_write");
    group.sample_size(20);

    for size in [256 * 1024, 1024 * 1024, 4 * 1024 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let rt = rt();
            b.iter(|| {
                rt.block_on(async {
                    let tmp = NamedTempFile::new().unwrap();
                    let file = TokioFile::open(tmp.path()).await.unwrap();
                    let pipeline = WritePipeline::new(file, 64 * 1024, 4);
                    let data = vec![0u8; size];
                    pipeline.write(0, &data).await.unwrap();
                    pipeline.storage().close().await.unwrap();
                });
            });
        });
    }

    group.finish();
}

/// WritePipeline write_bytes 基准：通过 Bytes 零拷贝路径写入
/// 对比 pipeline_sequential_write 可量化 Bytes::clone() vs Bytes::copy_from_slice() 的差距
fn bench_pipeline_write_bytes(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline_write_bytes");
    group.sample_size(20);
    for size in [256 * 1024, 1024 * 1024, 4 * 1024 * 1024].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                rt().block_on(async {
                    let tmp = NamedTempFile::new().unwrap();
                    let file = TokioFile::open(tmp.path()).await.unwrap();
                    let pipeline = WritePipeline::new(file, 64 * 1024, 4);
                    let data = Bytes::from(vec![0u8; size]);
                    pipeline.write_bytes(0, &data).await.unwrap();
                    pipeline.storage().close().await.unwrap();
                });
            });
        });
    }
    group.finish();
}

/// 分片偏移写基准：模拟下载器分片写入模式——4 个 256KiB 分片写入不同偏移
fn bench_fragment_offset_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("fragment_offset_writes");
    group.sample_size(20);
    group.throughput(Throughput::Bytes(1024 * 1024)); // 4 × 256KiB = 1MiB

    let fragment_size = 256 * 1024usize;
    let total_size = 4 * fragment_size;
    let offsets: Vec<u64> = vec![0, 256 * 1024, 512 * 1024, 768 * 1024];

    // 逐片写入：4 次独立 write_at，每次 256KiB
    group.bench_function("4x256KiB_offsets", |b| {
        let rt = rt();
        b.iter(|| {
            rt.block_on(async {
                let tmp = NamedTempFile::new().unwrap();
                let file = TokioFile::open(tmp.path()).await.unwrap();
                file.allocate(total_size as u64).await.unwrap();
                for offset in &offsets {
                    let data = Bytes::from(vec![0u8; fragment_size]);
                    file.write_at(*offset, data).await.unwrap();
                }
                file.close().await.unwrap();
            });
        });
    });

    // 批量写入：1 次 write_at，1MiB 连续数据（模拟 downloader 256KiB 批量聚合后一次性刷盘）
    group.bench_function("batched_1MiB_sequential", |b| {
        let rt = rt();
        b.iter(|| {
            rt.block_on(async {
                let tmp = NamedTempFile::new().unwrap();
                let file = TokioFile::open(tmp.path()).await.unwrap();
                file.allocate(total_size as u64).await.unwrap();
                let data = Bytes::from(vec![0u8; total_size]);
                file.write_at(0, data).await.unwrap();
                file.close().await.unwrap();
            });
        });
    });

    // 无预分配写入：模拟无 allocate 的场景
    group.bench_function("4x256KiB_no_allocate", |b| {
        let rt = rt();
        b.iter(|| {
            rt.block_on(async {
                let tmp = NamedTempFile::new().unwrap();
                let file = TokioFile::open(tmp.path()).await.unwrap();
                for offset in &offsets {
                    let data = Bytes::from(vec![0u8; fragment_size]);
                    file.write_at(*offset, data).await.unwrap();
                }
                file.close().await.unwrap();
            });
        });
    });

    group.finish();
}

/// sync 频率基准：对比每次写后 sync vs 最后一次 sync 的性能差异
fn bench_sync_frequency(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync_frequency");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(1024 * 1024)); // 4 × 256KiB = 1MiB

    let fragment_size = 256 * 1024usize;
    let offsets: Vec<u64> = vec![0, 256 * 1024, 512 * 1024, 768 * 1024];

    // sync_per_write：每次写后都 sync
    group.bench_function("sync_per_write", |b| {
        let rt = rt();
        b.iter(|| {
            rt.block_on(async {
                let tmp = NamedTempFile::new().unwrap();
                let file = TokioFile::open(tmp.path()).await.unwrap();
                file.allocate(1024 * 1024).await.unwrap();
                for offset in &offsets {
                    let data = Bytes::from(vec![0u8; fragment_size]);
                    file.write_at(*offset, data).await.unwrap();
                    file.sync().await.unwrap();
                }
                file.close().await.unwrap();
            })
        })
    });

    // sync_once_at_end：写完所有数据后只 sync 一次
    group.bench_function("sync_once_at_end", |b| {
        let rt = rt();
        b.iter(|| {
            rt.block_on(async {
                let tmp = NamedTempFile::new().unwrap();
                let file = TokioFile::open(tmp.path()).await.unwrap();
                file.allocate(1024 * 1024).await.unwrap();
                for offset in &offsets {
                    let data = Bytes::from(vec![0u8; fragment_size]);
                    file.write_at(*offset, data).await.unwrap();
                }
                file.sync().await.unwrap();
                file.close().await.unwrap();
            })
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_tokiofile_sequential_write,
    bench_pipeline_sequential_write,
    bench_pipeline_write_bytes,
    bench_fragment_offset_writes,
    bench_sync_frequency
);
criterion_main!(benches);
