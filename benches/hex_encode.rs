mod support;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use support::bench_config;

fn bench_hex_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("hex_encode");

    for size in [16, 64, 256, 1024, 4096] {
        let data = vec![0xABu8; size];
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, d| {
            b.iter(|| tachyon_core::hex_encode(d));
        });
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = bench_config();
    targets = bench_hex_encode
}
criterion_main!(benches);
