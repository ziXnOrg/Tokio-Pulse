//! Benchmarks for CPU timing implementations

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tokio_pulse::timing::create_cpu_timer;

fn bench_cpu_timer_single_measurement(c: &mut Criterion) {
    let timer = create_cpu_timer();
    let platform = timer.platform_name();

    c.bench_function(&format!("cpu_timer/{}/single_measurement", platform), |b| {
        b.iter(|| black_box(timer.thread_cpu_time_ns().unwrap()));
    });
}

fn bench_cpu_timer_overhead(c: &mut Criterion) {
    let timer = create_cpu_timer();
    let platform = timer.platform_name();

    // Benchmark the raw overhead of consecutive measurements
    c.bench_function(&format!("cpu_timer/{}/measurement_overhead", platform), |b| {
        b.iter(|| {
            let start = timer.thread_cpu_time_ns().unwrap();
            let end = timer.thread_cpu_time_ns().unwrap();
            black_box(end - start)
        });
    });
}

fn bench_cpu_timer_with_work(c: &mut Criterion) {
    let timer = create_cpu_timer();
    let platform = timer.platform_name();

    let mut group = c.benchmark_group("cpu_timer_with_work");

    for work_size in [100, 1000, 10000] {
        group.bench_with_input(BenchmarkId::new(platform, work_size), &work_size, |b, &size| {
            b.iter(|| {
                let start = timer.thread_cpu_time_ns().unwrap();

                // Do some work
                let mut sum = 0u64;
                for i in 0..size {
                    sum = sum.wrapping_add(i);
                }
                black_box(sum);

                let end = timer.thread_cpu_time_ns().unwrap();
                black_box(end - start)
            });
        });
    }
    group.finish();
}

fn bench_calibrated_overhead(c: &mut Criterion) {
    let timer = create_cpu_timer();
    let platform = timer.platform_name();
    let overhead = timer.calibrated_overhead_ns();

    // Report the calibrated overhead as a benchmark result
    c.bench_function(&format!("cpu_timer/{}/calibrated_overhead_ns", platform), |b| {
        b.iter(|| black_box(overhead));
    });
}

fn bench_multiple_timers(c: &mut Criterion) {
    let timers: Vec<_> = (0..4).map(|_| create_cpu_timer()).collect();

    c.bench_function("cpu_timer/multiple_timers/round_robin", |b| {
        let mut index = 0;
        b.iter(|| {
            let timer = &timers[index];
            index = (index + 1) % timers.len();
            black_box(timer.thread_cpu_time_ns().unwrap())
        });
    });
}

fn bench_comparison_with_instant(c: &mut Criterion) {
    use std::time::Instant;

    let cpu_timer = create_cpu_timer();

    let mut group = c.benchmark_group("timing_comparison");

    // Benchmark CPU timer
    group.bench_function("cpu_timer", |b| {
        b.iter(|| black_box(cpu_timer.thread_cpu_time_ns().unwrap()));
    });

    // Benchmark Instant::now()
    group.bench_function("instant_now", |b| {
        b.iter(|| black_box(Instant::now()));
    });

    // Benchmark Instant elapsed
    let start = Instant::now();
    group.bench_function("instant_elapsed", |b| {
        b.iter(|| black_box(start.elapsed().as_nanos()));
    });

    group.finish();
}

fn bench_high_frequency_access(c: &mut Criterion) {
    let timer = create_cpu_timer();

    c.bench_function("cpu_timer/high_frequency/1000_sequential", |b| {
        b.iter(|| {
            let mut last = 0u64;
            for _ in 0..1000 {
                last = timer.thread_cpu_time_ns().unwrap();
            }
            black_box(last)
        });
    });
}

// Combine all benchmark groups
criterion_group!(
    benches,
    bench_cpu_timer_single_measurement,
    bench_cpu_timer_overhead,
    bench_cpu_timer_with_work,
    bench_calibrated_overhead,
    bench_multiple_timers,
    bench_comparison_with_instant,
    bench_high_frequency_access,
);

criterion_main!(benches);
