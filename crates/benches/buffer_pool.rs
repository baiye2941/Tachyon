//! BufferPool 分配与回收基准测试
//!
//! 测试不同 buffer 大小和池容量下的分配/回收性能。

mod support;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use support::bench_config;
use tachyon_io::BufferPool;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn fill_pool(pool: &BufferPool, rt: &tokio::runtime::Runtime) {
    let mut buffers = Vec::with_capacity(pool.capacity());
    for _ in 0..pool.capacity() {
        buffers.push(rt.block_on(pool.alloc()));
    }
    for buf in buffers {
        pool.release(buf);
    }
}

fn bench_buffer_alloc(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_alloc");
    for size in [1024, 4096, 16384, 65536].iter() {
        group.bench_with_input(BenchmarkId::new("prefill_pool", size), size, |b, &size| {
            let rt = rt();
            b.iter_batched(
                || {
                    let pool = BufferPool::new(size, 64);
                    fill_pool(&pool, &rt);
                    pool
                },
                |pool| rt.block_on(pool.alloc()),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_buffer_alloc_empty(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_alloc_empty");
    for size in [1024, 4096, 16384, 65536].iter() {
        group.bench_with_input(BenchmarkId::new("empty_pool", size), size, |b, &size| {
            let rt = rt();
            b.iter_batched(
                || BufferPool::new(size, 64),
                |pool| rt.block_on(pool.alloc()),
                BatchSize::SmallInput,
            );
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
            let pool = BufferPool::new(size, 64);
            let rt = rt();
            fill_pool(&pool, &rt);
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
            let pool = BufferPool::new(4096, cap);
            let rt = rt();
            fill_pool(&pool, &rt);
            b.iter(|| {
                let buf = rt.block_on(pool.alloc());
                pool.release(buf);
            });
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = bench_config();
    targets =
        bench_buffer_alloc,
        bench_buffer_alloc_empty,
        bench_buffer_release,
        bench_buffer_alloc_release_cycle,
        bench_buffer_pool_capacity
}
criterion_main!(benches);
