//! 哈希算法基准测试:blake3 vs sha256
//!
//! 对比不同数据大小下 blake3 和 sha256 的吞吐量,
//! 用于验证 blake3 在大数据量下的性能优势。

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
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

criterion_group!(
    benches,
    bench_blake3,
    bench_sha256,
    bench_blake3_direct,
    bench_sha256_direct,
    bench_verify,
);
criterion_main!(benches);
