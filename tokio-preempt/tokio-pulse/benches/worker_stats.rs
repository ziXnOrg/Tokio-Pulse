//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
// Benchmarks for worker statistics operations
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio_pulse::{
    HookRegistry, PollResult, PreemptionHooks, TaskContext, TaskId, TierConfig, TierManager,
};

fn create_test_manager() -> TierManager {
    TierManager::new(TierConfig::default())
}

fn bench_single_worker_metrics(c: &mut Criterion) {
    let manager = create_test_manager();

    let mut group = c.benchmark_group("worker_stats/single_worker");

    // Benchmark getting metrics for non-existent worker (should return defaults)
    group.bench_function("get_nonexistent_worker", |b| {
        b.iter(|| black_box(manager.get_worker_metrics(999)))
    });

    // Create some worker activity first
    let task_id = TaskId(1);
    let context = TaskContext { worker_id: 0, priority: None };
    manager.before_poll(task_id, &context);
    manager.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));

    // Benchmark getting metrics for existing worker
    group.bench_function("get_existing_worker", |b| {
        b.iter(|| black_box(manager.get_worker_metrics(0)))
    });

    // Benchmark reset operation
    group.bench_function("reset_worker_stats", |b| {
        b.iter(|| manager.reset_worker_stats(black_box(0)))
    });

    group.finish();
}

fn bench_worker_stats_updates(c: &mut Criterion) {
    let manager = create_test_manager();
    let registry = HookRegistry::new();
    registry.set_hooks(Arc::new(manager) as Arc<dyn PreemptionHooks>);

    let mut group = c.benchmark_group("worker_stats/updates");

    // Benchmark poll operations that update stats
    group.bench_function("poll_with_stats_update", |b| {
        let mut task_counter = 0u64;
        b.iter(|| {
            let task_id = TaskId(task_counter);
            let context = TaskContext { worker_id: 0, priority: None };

            registry.before_poll(task_id, &context);
            registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(50));

            task_counter += 1;
        })
    });

    // Benchmark yield operations
    group.bench_function("yield_with_stats_update", |b| {
        let mut task_counter = 1000u64;
        b.iter(|| {
            let task_id = TaskId(task_counter);
            registry.on_yield(task_id);
            task_counter += 1;
        })
    });

    // Benchmark slow poll tracking
    group.bench_function("slow_poll_tracking", |b| {
        let mut task_counter = 2000u64;
        b.iter(|| {
            let task_id = TaskId(task_counter);
            let context = TaskContext { worker_id: 0, priority: None };

            registry.before_poll(task_id, &context);
            // Use 2ms duration to trigger slow poll detection
            registry.after_poll(task_id, PollResult::Pending, Duration::from_millis(2));

            task_counter += 1;
        })
    });

    group.finish();
}

fn bench_multiple_workers(c: &mut Criterion) {
    let manager = Arc::new(create_test_manager());

    let mut group = c.benchmark_group("worker_stats/multiple_workers");

    // Create activity across multiple workers
    for worker_id in 0..10 {
        for i in 0..5 {
            let task_id = TaskId((worker_id * 100 + i) as u64);
            let context = TaskContext { worker_id, priority: None };
            manager.before_poll(task_id, &context);
            manager.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));
        }
    }

    // Benchmark getting all worker metrics
    group.bench_function("get_all_worker_metrics", |b| {
        b.iter(|| black_box(manager.get_all_worker_metrics()))
    });

    // Benchmark iterating through multiple workers
    group.bench_function("iterate_worker_metrics", |b| {
        b.iter(|| {
            for worker_id in 0..10 {
                black_box(manager.get_worker_metrics(worker_id));
            }
        })
    });

    group.finish();
}

fn bench_concurrent_worker_access(c: &mut Criterion) {
    let manager = Arc::new(create_test_manager());

    let mut group = c.benchmark_group("worker_stats/concurrent_access");

    for num_threads in [2, 4, 8].iter() {
        group.bench_with_input(
            BenchmarkId::new("concurrent_stats_access", num_threads),
            num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|worker_id| {
                            let manager = Arc::clone(&manager);
                            thread::spawn(move || {
                                // Each thread does operations on its own worker
                                for i in 0..50 {
                                    let task_id = TaskId((worker_id * 1000 + i) as u64);
                                    let context = TaskContext {
                                        worker_id: worker_id as usize,
                                        priority: None
                                    };

                                    manager.before_poll(task_id, &context);
                                    manager.after_poll(task_id, PollResult::Pending, Duration::from_micros(10));

                                    // Occasionally check metrics
                                    if i % 10 == 0 {
                                        black_box(manager.get_worker_metrics(worker_id as usize));
                                    }
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                })
            },
        );
    }

    group.finish();
}

fn bench_worker_stats_memory_patterns(c: &mut Criterion) {
    let manager = Arc::new(create_test_manager());

    let mut group = c.benchmark_group("worker_stats/memory_patterns");

    // Benchmark creating stats for many workers
    group.bench_function("create_many_worker_stats", |b| {
        b.iter(|| {
            for worker_id in 0..100 {
                let task_id = TaskId(worker_id as u64);
                let context = TaskContext { worker_id, priority: None };
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, PollResult::Ready, Duration::from_micros(10));
            }

            // Get all stats to ensure they were created
            black_box(manager.get_all_worker_metrics().len());

            // Reset all workers to clean up
            for worker_id in 0..100 {
                manager.reset_worker_stats(worker_id);
            }
        })
    });

    // Benchmark worker stats under high frequency updates
    group.bench_function("high_frequency_updates", |b| {
        b.iter(|| {
            for i in 0..1000 {
                let task_id = TaskId(i);
                let context = TaskContext { worker_id: 0, priority: None };
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, PollResult::Ready, Duration::from_nanos(100));
            }

            // Sample stats check
            black_box(manager.get_worker_metrics(0));
        })
    });

    group.finish();
}

fn bench_worker_stats_edge_cases(c: &mut Criterion) {
    let manager = Arc::new(create_test_manager());

    c.bench_function("worker_stats/edge_cases/max_worker_id", |b| {
        let max_worker_id = usize::MAX;
        b.iter(|| {
            black_box(manager.get_worker_metrics(max_worker_id));
        })
    });

    c.bench_function("worker_stats/edge_cases/zero_duration_polls", |b| {
        let mut task_counter = 10000u64;
        b.iter(|| {
            let task_id = TaskId(task_counter);
            let context = TaskContext { worker_id: 0, priority: None };

            manager.before_poll(task_id, &context);
            manager.after_poll(task_id, PollResult::Ready, Duration::ZERO);

            task_counter += 1;
        })
    });

    c.bench_function("worker_stats/edge_cases/repeated_resets", |b| {
        b.iter(|| {
            for _ in 0..10 {
                manager.reset_worker_stats(black_box(0));
            }
        })
    });
}

fn bench_worker_stats_realistic_workload(c: &mut Criterion) {
    let manager = Arc::new(create_test_manager());
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    c.bench_function("worker_stats/realistic_workload", |b| {
        b.iter(|| {
            let mut task_counter = 0u64;

            // Simulate mixed workload across 4 workers
            for worker_id in 0..4 {
                for _ in 0..25 { // 25 tasks per worker = 100 total
                    let task_id = TaskId(task_counter);
                    let context = TaskContext { worker_id, priority: None };

                    registry.before_poll(task_id, &context);

                    // Mix of poll durations
                    let duration = match task_counter % 4 {
                        0 => Duration::from_micros(50),   // Fast
                        1 => Duration::from_micros(200),  // Medium
                        2 => Duration::from_millis(1),    // Slow
                        _ => Duration::from_micros(100),  // Normal
                    };

                    let result = if task_counter % 5 == 0 {
                        PollResult::Ready
                    } else {
                        PollResult::Pending
                    };

                    registry.after_poll(task_id, result, duration);

                    // Occasional voluntary yields
                    if task_counter % 10 == 0 {
                        registry.on_yield(task_id);
                    }

                    // Clean up completed tasks
                    if matches!(result, PollResult::Ready) {
                        registry.on_completion(task_id);
                    }

                    task_counter += 1;
                }

                // Check worker stats occasionally
                black_box(manager.get_worker_metrics(worker_id));
            }

            // Final check of all stats
            black_box(manager.get_all_worker_metrics());
        })
    });
}

criterion_group!(
    benches,
    bench_single_worker_metrics,
    bench_worker_stats_updates,
    bench_multiple_workers,
    bench_concurrent_worker_access,
    bench_worker_stats_memory_patterns,
    bench_worker_stats_edge_cases,
    bench_worker_stats_realistic_workload
);

criterion_main!(benches);