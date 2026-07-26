use std::{env, time::Duration};

use criterion::{BenchmarkGroup, Criterion, PlottingBackend, measurement::WallTime};

pub mod bench_server;

const SMOKE_SAMPLE_SIZE: usize = 10;
// SMOKE 计时常量与 bench_config 为跨 bench 二进制共享的辅助项;部分 bench 入口
// (如 throughput_baseline 仅用 bench_server)未调用,在其编译单元构成 dead_code。
#[allow(dead_code)]
const SMOKE_WARM_UP_MS: u64 = 100;
#[allow(dead_code)]
const SMOKE_MEASUREMENT_MS: u64 = 200;

pub fn smoke_mode() -> bool {
    matches!(
        env::var("TACHYON_BENCH_MODE").ok().as_deref(),
        Some("smoke") | Some("quick") | Some("ci")
    )
}

#[allow(dead_code)]
pub fn bench_config() -> Criterion {
    let criterion = Criterion::default()
        .configure_from_args()
        .plotting_backend(PlottingBackend::Plotters);

    if smoke_mode() {
        criterion
            .sample_size(SMOKE_SAMPLE_SIZE)
            .warm_up_time(Duration::from_millis(SMOKE_WARM_UP_MS))
            .measurement_time(Duration::from_millis(SMOKE_MEASUREMENT_MS))
    } else {
        criterion
    }
}

#[allow(dead_code)]
pub fn configure_group(group: &mut BenchmarkGroup<'_, WallTime>, full_sample_size: usize) {
    if smoke_mode() {
        group.sample_size(SMOKE_SAMPLE_SIZE);
    } else {
        group.sample_size(full_sample_size);
    }
}
