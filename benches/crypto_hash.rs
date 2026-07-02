//! 哈希算法基准测试:blake3 vs sha256
//!
//! 对比不同数据大小下 blake3 和 sha256 的吞吐量,
//! 用于验证 blake3 在大数据量下的性能优势。

mod support;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use support::bench_config;
use tachyon_core::traits::Verifier;
use tachyon_crypto::cpu::CpuVerifier;

/// 测试数据大小(字节):1KB / 64KB / 1MB / 16MB
const DATA_SIZES: &[usize] = &[1024, 65536, 1048576, 16777216];

/// 生成指定大小的伪随机测试数据
///
/// 使用确定性种子确保每次生成相同数据,避免影响基准可重复性。
fn generate_test_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let mut state: u64 = 0xdead_beef_cafe_babe;
    for _ in 0..size {
        // 简单 xorshift 伪随机,足够生成不可压缩数据
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.push((state & 0xFF) as u8);
    }
    data
}

/// 基准:blake3 哈希吞吐量
fn bench_blake3(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake3");
    // 设置吞吐量统计(bytes)
    for &size in DATA_SIZES.iter() {
        group.throughput(criterion::Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("hash", size), &size, |b, &size| {
            let verifier = CpuVerifier::blake3();
            let data = generate_test_data(size);
            b.iter(|| verifier.compute_hash(&data));
        });
    }
    group.finish();
}

/// 基准:sha256 哈希吞吐量
fn bench_sha256(c: &mut Criterion) {
    let mut group = c.benchmark_group("sha256");
    for &size in DATA_SIZES.iter() {
        group.throughput(criterion::Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("hash", size), &size, |b, &size| {
            let verifier = CpuVerifier::sha256();
            let data = generate_test_data(size);
            b.iter(|| verifier.compute_hash(&data));
        });
    }
    group.finish();
}

/// 基准:blake3 直接 API(vs 通过 Verifier trait)
///
/// 测量 trait 调用本身的开销,与直接调用 blake3::hash 对比。
fn bench_blake3_direct(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake3_direct");
    for &size in DATA_SIZES.iter() {
        group.throughput(criterion::Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("direct_api", size), &size, |b, &size| {
            let data = generate_test_data(size);
            b.iter(|| {
                let hash = blake3::hash(&data);
                hash.to_hex().to_string()
            });
        });
    }
    group.finish();
}

/// 基准:sha256 直接 API(vs 通过 Verifier trait)
fn bench_sha256_direct(c: &mut Criterion) {
    let mut group = c.benchmark_group("sha256_direct");
    for &size in DATA_SIZES.iter() {
        group.throughput(criterion::Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("direct_api", size), &size, |b, &size| {
            let data = generate_test_data(size);
            b.iter(|| {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(&data);
                let result = hasher.finalize();
                // 使用与 CpuVerifier 相同的 hex 编码方式
                result
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            });
        });
    }
    group.finish();
}

/// 基准:verify 完整校验流程(计算哈希 + 比对)
///
/// 模拟真实校验场景:先算哈希,再校验数据完整性。
fn bench_verify(c: &mut Criterion) {
    let mut group = c.benchmark_group("verify");
    for &size in [1024, 65536, 1048576].iter() {
        group.throughput(criterion::Throughput::Bytes(size as u64));

        // blake3 verify
        group.bench_with_input(
            BenchmarkId::new("blake3_verify", size),
            &size,
            |b, &size| {
                let verifier = CpuVerifier::blake3();
                let data = generate_test_data(size);
                let hash = verifier.compute_hash(&data).unwrap();
                b.iter(|| verifier.verify(&data, &hash));
            },
        );

        // sha256 verify
        group.bench_with_input(
            BenchmarkId::new("sha256_verify", size),
            &size,
            |b, &size| {
                let verifier = CpuVerifier::sha256();
                let data = generate_test_data(size);
                let hash = verifier.compute_hash(&data).unwrap();
                b.iter(|| verifier.verify(&data, &hash));
            },
        );
    }
    group.finish();
}

/// 基准:blake3 整文件校验路径(compute_hash_from_path)
///
/// `compute_hash_from_path` 内部对 blake3 使用 `update_mmap_rayon`
/// (mmap 零拷贝读 + 多线程哈希)。本组对比"从文件路径计算"与"内存全量计算",
/// 验证大文件场景下 rayon 多线程 + mmap 的收益。
///
/// 注意:16MB 以下数据多已驻留 page cache,mmap 读接近零成本;rayon 线程
/// 切换开销在小数据上可能抵消并行收益。真正 10-20x 收益体现在 GB 级文件,
/// 此处以 1MB / 16MB 展示 API 正确性与中等规模趋势。
fn bench_blake3_from_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("blake3_from_path");
    // compute_hash_from_path 为 async,需在 tokio 运行时中执行。
    let rt = tokio::runtime::Runtime::new().unwrap();
    for &size in [1048576usize, 16777216].iter() {
        group.throughput(criterion::Throughput::Bytes(size as u64));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench_file.bin");
        let data = generate_test_data(size);
        std::fs::write(&path, &data).unwrap();

        let verifier = CpuVerifier::blake3();

        // 从文件路径(mmap + rayon 多线程)
        group.bench_with_input(BenchmarkId::new("mmap_rayon", size), &size, |b, &_size| {
            b.iter(|| {
                let h = rt
                    .block_on(verifier.compute_hash_from_path(&path, 65536))
                    .unwrap();
                std::hint::black_box(h);
            });
        });

        // 对照:内存全量(单线程 blake3::hash)
        group.bench_with_input(BenchmarkId::new("in_memory", size), &size, |b, &_size| {
            b.iter(|| {
                let h = verifier.compute_hash(&data).unwrap();
                std::hint::black_box(h);
            });
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = bench_config();
    targets =
        bench_blake3,
        bench_sha256,
        bench_blake3_direct,
        bench_sha256_direct,
        bench_verify,
        bench_blake3_from_path
}
criterion_main!(benches);
