//! 分片策略与带宽预测基准测试
//!
//! 测试分片大小计算、EWMA 带宽追踪、Holt-Winters 预测器的 CPU 性能,
//! 以及调度器优先级队列的吞吐量。

use amd_core::types::FragmentInfo;
use amd_engine::fragment::compute_fragment_size;
use amd_engine::fragment::{BandwidthTracker, FragmentRecord};
use amd_scheduler::predictor::HoltWintersPredictor;
use amd_scheduler::scheduler::{Priority, ScheduledTask, Scheduler};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::time::Duration;

// ---------- 辅助函数 ----------

/// 构造 FragmentInfo
fn make_fragment(index: u32, size: u64) -> FragmentInfo {
    FragmentInfo {
        index,
        start: index as u64 * size,
        end: (index as u64 + 1) * size - 1,
        size,
        downloaded: 0,
        hash: None,
    }
}

/// 构造 ScheduledTask
fn make_task(priority: Priority, size: u64, progress: f64) -> ScheduledTask {
    ScheduledTask {
        task_id: amd_core::TaskId::new_v4(),
        priority,
        file_size: size,
        progress,
    }
}

// ---------- 分片大小计算 ----------

/// 基准:compute_fragment_size 不同文件大小
fn bench_compute_fragment_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_fragment_size");
    // (文件大小, 带宽, 最小分片, 最大分片, 目标分片数)
    let cases: &[(&str, u64, u64, u64, u64, u32)] = &[
        (
            "10MB_1Mbps",
            10 * 1024 * 1024,
            1024 * 1024,
            256 * 1024,
            8 * 1024 * 1024,
            8,
        ),
        (
            "100MB_10Mbps",
            100 * 1024 * 1024,
            10 * 1024 * 1024,
            1024 * 1024,
            64 * 1024 * 1024,
            16,
        ),
        (
            "1GB_100Mbps",
            1024 * 1024 * 1024,
            100 * 1024 * 1024,
            1024 * 1024,
            64 * 1024 * 1024,
            32,
        ),
        (
            "10GB_1Gbps",
            10 * 1024 * 1024 * 1024,
            1024 * 1024 * 1024,
            4 * 1024 * 1024,
            128 * 1024 * 1024,
            64,
        ),
    ];

    for (name, file_size, bw, min_s, max_s, target) in cases.iter() {
        group.bench_with_input(
            BenchmarkId::new("fragment_size", name),
            &(file_size, bw, min_s, max_s, target),
            |b, &(fs, bw, min_s, max_s, target)| {
                b.iter(|| compute_fragment_size(*fs, *bw, *min_s, *max_s, *target));
            },
        );
    }
    group.finish();
}

// ---------- EWMA 带宽追踪 ----------

/// 基准:BandwidthTracker::record 单次记录
fn bench_bandwidth_tracker_record(c: &mut Criterion) {
    let mut group = c.benchmark_group("bandwidth_tracker");
    // 不同采样窗口大小
    for sample_count in [10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::new("record", sample_count),
            sample_count,
            |b, &count| {
                let mut tracker = BandwidthTracker::new(0.3);
                // 预填充历史数据
                for i in 0..count {
                    tracker.record(1_000_000 + (i as u64 * 1000));
                }
                b.iter(|| {
                    tracker.record(2_000_000);
                });
            },
        );
    }
    group.finish();
}

/// 基准:BandwidthTracker 连续 record + estimate 循环
fn bench_bandwidth_tracker_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("bandwidth_tracker_cycle");
    group.bench_function("record_estimate_100", |b| {
        let mut tracker = BandwidthTracker::new(0.3);
        b.iter(|| {
            for i in 0..100u64 {
                tracker.record(1_000_000 + i * 10_000);
                let _est = tracker.estimate();
            }
        });
    });
    group.finish();
}

// ---------- Holt-Winters 预测器 ----------

/// 基准:HoltWintersPredictor::observe 单次观测
fn bench_holt_winters_observe(c: &mut Criterion) {
    let mut group = c.benchmark_group("holt_winters");
    group.bench_function("observe", |b| {
        let mut pred = HoltWintersPredictor::default();
        // 预热
        for i in 0..50 {
            pred.observe(1_000_000.0 + i as f64 * 1000.0);
        }
        b.iter(|| {
            pred.observe(2_000_000.0);
        });
    });
    group.finish();
}

/// 基准:HoltWintersPredictor::predict 预测
fn bench_holt_winters_predict(c: &mut Criterion) {
    let mut group = c.benchmark_group("holt_winters_predict");
    for steps in [1u64, 5, 10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("predict_steps", steps),
            steps,
            |b, &steps| {
                let mut pred = HoltWintersPredictor::default();
                for i in 0..100 {
                    pred.observe(1_000_000.0 + i as f64 * 5000.0);
                }
                b.iter(|| pred.predict(steps));
            },
        );
    }
    group.finish();
}

/// 基准:HoltWinters 预测器完整工作流(observe -> predict)
fn bench_holt_winters_workflow(c: &mut Criterion) {
    let mut group = c.benchmark_group("holt_winters_workflow");
    group.bench_function("observe_predict_cycle", |b| {
        let mut pred = HoltWintersPredictor::new(0.3, 0.1);
        let mut sample = 1_000_000.0f64;
        b.iter(|| {
            // 模拟带宽波动
            sample = sample * 0.95 + 2_000_000.0 * 0.05;
            pred.observe(sample);
            let _predicted = pred.predict(5);
        });
    });
    group.finish();
}

// ---------- 分片状态机 ----------

/// 基准:FragmentRecord 状态转换
fn bench_fragment_state_machine(c: &mut Criterion) {
    let mut group = c.benchmark_group("fragment_state_machine");
    group.bench_function("full_lifecycle", |b| {
        b.iter(|| {
            let info = make_fragment(0, 64 * 1024);
            let mut record = FragmentRecord::new(info, 3);
            record.start_download();
            record.complete_download(14, Duration::from_millis(50));
            record.verify_ok();
            record.write_done();
            assert!(record.is_done());
        });
    });
    group.finish();
}

/// 基准:FragmentRecord 退避计算
fn bench_fragment_backoff(c: &mut Criterion) {
    let mut group = c.benchmark_group("fragment_backoff");
    for retry in 0..6u32 {
        group.bench_with_input(
            BenchmarkId::new("backoff_retry", retry),
            &retry,
            |b, &retry| {
                let info = make_fragment(0, 1024);
                let mut record = FragmentRecord::new(info, 10);
                record.retry_count = retry;
                b.iter(|| record.backoff_duration());
            },
        );
    }
    group.finish();
}

// ---------- 调度器 ----------

/// 基准:Scheduler push/pop 操作
fn bench_scheduler_push_pop(c: &mut Criterion) {
    let mut group = c.benchmark_group("scheduler");
    // 不同队列深度
    for depth in [10, 100, 1000].iter() {
        group.bench_with_input(BenchmarkId::new("push_pop", depth), depth, |b, &depth| {
            let mut sched = Scheduler::new();
            // 预填充
            for _ in 0..depth {
                sched.push(make_task(Priority::Queue, 1024, 0.0));
            }
            b.iter(|| {
                sched.push(make_task(Priority::UserInitiated, 512, 0.5));
                let _task = sched.pop();
            });
        });
    }
    group.finish();
}

/// 基准:Scheduler 批量 push 后逐个 pop
fn bench_scheduler_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("scheduler_batch");
    for count in [64, 256, 1024].iter() {
        group.bench_with_input(BenchmarkId::new("batch_pop", count), count, |b, &count| {
            b.iter_batched(
                || {
                    // setup: 创建并填充调度器
                    let mut sched = Scheduler::new();
                    for i in 0..count {
                        let priority = match i % 3 {
                            0 => Priority::Prefetch,
                            1 => Priority::Queue,
                            _ => Priority::UserInitiated,
                        };
                        sched.push(make_task(
                            priority,
                            (i as u64 + 1) * 1024,
                            i as f64 / count as f64,
                        ));
                    }
                    sched
                },
                |mut sched| {
                    // 测量:逐个取出所有任务
                    while sched.pop().is_some() {}
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_compute_fragment_size,
    bench_bandwidth_tracker_record,
    bench_bandwidth_tracker_cycle,
    bench_holt_winters_observe,
    bench_holt_winters_predict,
    bench_holt_winters_workflow,
    bench_fragment_state_machine,
    bench_fragment_backoff,
    bench_scheduler_push_pop,
    bench_scheduler_batch,
);
criterion_main!(benches);
