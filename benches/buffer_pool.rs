//! BufferPool 分配与回收基准测试
//!
//! 测试不同 buffer 大小和池容量下的分配/回收性能。
//!
//! Runtime 使用多线程模式以反映真实生产场景:
//! - 真实 worker 跨多核并发 alloc/release,信号量与无锁队列在多核下才暴露竞争
//! - new_current_thread 单线程会掩盖原子竞争与 cache line 抢占成本

mod support;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::sync::Arc;
use support::bench_config;
use tachyon_io::BufferPool;

/// 多线程 runtime,worker 数固定为物理核数(回落 4),稳定基准对比
fn rt() -> tokio::runtime::Runtime {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
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

/// 并发 alloc/release 基准:测量多 task 跨核竞争同一池的吞吐
///
/// 模拟生产场景:N 个 worker 各自在 tokio::spawn 中重复 alloc/release。
/// 池容量 = 并发数(无阻塞场景),测纯路径开销;
/// 池容量 = 并发数/2(有反压场景),测信号量在饥饿时的唤醒成本。
fn bench_buffer_alloc_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_alloc_concurrent");
    // 单次迭代每个 worker 完成 16 次 alloc/release
    const PER_WORKER_OPS: usize = 16;

    // 无反压:capacity == concurrency,所有 alloc 即取即得
    for concurrency in [4usize, 16, 64] {
        group.bench_with_input(
            BenchmarkId::new("no_backpressure", concurrency),
            &concurrency,
            |b, &n| {
                let rt = rt();
                let pool = Arc::new(BufferPool::new(4096, n));
                rt.block_on(pool.prewarm());
                b.iter(|| {
                    rt.block_on(async {
                        let mut handles = Vec::with_capacity(n);
                        for _ in 0..n {
                            let pool = pool.clone();
                            handles.push(tokio::spawn(async move {
                                for _ in 0..PER_WORKER_OPS {
                                    let buf = pool.alloc().await;
                                    pool.release(buf);
                                }
                            }));
                        }
                        for h in handles {
                            let _ = h.await;
                        }
                    });
                });
            },
        );
    }

    // 有反压:capacity = concurrency/2,半数 worker 须等待信号量
    for concurrency in [4usize, 16, 64] {
        group.bench_with_input(
            BenchmarkId::new("with_backpressure", concurrency),
            &concurrency,
            |b, &n| {
                let rt = rt();
                let cap = (n / 2).max(1);
                let pool = Arc::new(BufferPool::new(4096, cap));
                rt.block_on(pool.prewarm());
                b.iter(|| {
                    rt.block_on(async {
                        let mut handles = Vec::with_capacity(n);
                        for _ in 0..n {
                            let pool = pool.clone();
                            handles.push(tokio::spawn(async move {
                                for _ in 0..PER_WORKER_OPS {
                                    let buf = pool.alloc().await;
                                    pool.release(buf);
                                }
                            }));
                        }
                        for h in handles {
                            let _ = h.await;
                        }
                    });
                });
            },
        );
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
        bench_buffer_pool_capacity,
        bench_buffer_alloc_concurrent
}
criterion_main!(benches);
