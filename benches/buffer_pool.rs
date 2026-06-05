//! BufferPool 分配与回收基准测试
//!
//! 测试不同 buffer 大小和池容量下的分配/回收性能。

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tachyon_io::BufferPool;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn bench_buffer_alloc(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_alloc");
    for size in [1024, 4096, 16384, 65536].iter() {
        group.bench_with_input(BenchmarkId::new("prefill_pool", size), size, |b, &size| {
            let pool = BufferPool::with_prefill(size, 64);
            let rt = rt();
            b.iter(|| rt.block_on(pool.alloc()));
        });
    }
    group.finish();
}

fn bench_buffer_alloc_empty(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_alloc_empty");
    for size in [1024, 4096, 16384, 65536].iter() {
        group.bench_with_input(BenchmarkId::new("empty_pool", size), size, |b, &size| {
            let pool = BufferPool::new(size, 64);
            let rt = rt();
            b.iter(|| rt.block_on(pool.alloc()));
        });
    }
    group.finish();
}

fn bench_buffer_release(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_release");
    for size in [1024, 4096, 16384, 65536].iter() {
        group.bench_with_input(BenchmarkId::new("release", size), size, |b, &size| {
            let pool = BufferPool::new(size, 128);
            let rt = rt();
            b.iter(|| {
                let buf = rt.block_on(pool.alloc());
                pool.release(buf);
            });
        });
    }
    group.finish();
}

fn bench_buffer_alloc_release_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_cycle");
    for size in [4096, 16384, 65536].iter() {
        group.bench_with_input(BenchmarkId::new("cycle", size), size, |b, &size| {
            let pool = BufferPool::with_prefill(size, 64);
            let rt = rt();
            b.iter(|| {
                let buf = rt.block_on(pool.alloc());
                let _len = buf.capacity();
                pool.release(buf);
            });
        });
    }
    group.finish();
}

fn bench_buffer_pool_capacity(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool_capacity");
    for cap in [16, 64, 256, 1024].iter() {
        group.bench_with_input(BenchmarkId::new("capacity", cap), cap, |b, &cap| {
            let pool = BufferPool::with_prefill(4096, cap);
            let rt = rt();
            b.iter(|| {
                let buf = rt.block_on(pool.alloc());
                pool.release(buf);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_buffer_alloc,
    bench_buffer_alloc_empty,
    bench_buffer_release,
    bench_buffer_alloc_release_cycle,
    bench_buffer_pool_capacity,
);
criterion_main!(benches);
