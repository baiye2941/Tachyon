use std::{env, time::Duration};

use criterion::{BenchmarkGroup, Criterion, PlottingBackend, measurement::WallTime};

const SMOKE_SAMPLE_SIZE: usize = 10;
const SMOKE_WARM_UP_MS: u64 = 100;
const SMOKE_MEASUREMENT_MS: u64 = 200;

pub fn smoke_mode() -> bool {
    matches!(
        env::var("TACHYON_BENCH_MODE").ok().as_deref(),
        Some("smoke") | Some("quick") | Some("ci")
    )
}

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
