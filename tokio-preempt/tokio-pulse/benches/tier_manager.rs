/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */

/* Benchmarks for tier manager operations */

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio_pulse::tier_manager::{
    InterventionAction, PollResult, TaskContext, TaskId, TierConfig, TierManager, TierPolicy,
};

fn create_test_manager() -> TierManager {
    let config = TierConfig {
        poll_budget: 2000,
        cpu_ms_budget: 10,
        yield_interval: 100,
        tier_policies: [
            TierPolicy {
                name: "Monitor",
                promotion_threshold: 3,
                action: InterventionAction::Monitor,
            },
            TierPolicy {
                name: "Warn",
                promotion_threshold: 3,
                action: InterventionAction::Warn,
            },
            TierPolicy {
                name: "Yield",
                promotion_threshold: 3,
                action: InterventionAction::Yield,
            },
            TierPolicy {
                name: "Isolate",
                promotion_threshold: 3,
                action: InterventionAction::Isolate,
            },
        ],
        enable_isolation: false,
        max_slow_queue_size: 1000,
    };
    TierManager::new(config)
}

fn bench_tier_manager_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("tier_manager/creation");

    group.bench_function("new_with_config", |b| {
        let config = TierConfig {
            poll_budget: 2000,
            cpu_ms_budget: 10,
            yield_interval: 100,
            tier_policies: [
                TierPolicy {
                    name: "Monitor",
                    promotion_threshold: 3,
                    action: InterventionAction::Monitor,
                },
                TierPolicy {
                    name: "Warn",
                    promotion_threshold: 3,
                    action: InterventionAction::Warn,
                },
                TierPolicy {
                    name: "Yield",
                    promotion_threshold: 3,
                    action: InterventionAction::Yield,
                },
                TierPolicy {
                    name: "Isolate",
                    promotion_threshold: 3,
                    action: InterventionAction::Isolate,
                },
            ],
            enable_isolation: false,
            max_slow_queue_size: 1000,
        };
        b.iter(|| black_box(TierManager::new(config.clone())));
    });

    group.finish();
}

fn bench_tier_manager_hooks(c: &mut Criterion) {
    let manager = create_test_manager();
    let task_id = TaskId(1);
    let context = TaskContext {
        worker_id: 0,
        priority: None,
    };

    let mut group = c.benchmark_group("tier_manager/hooks");

    group.bench_function("before_poll", |b| {
        b.iter(|| {
            manager.before_poll(black_box(task_id), black_box(&context));
        });
    });

    group.bench_function("after_poll/ready", |b| {
        b.iter(|| {
            manager.after_poll(
                black_box(task_id),
                black_box(PollResult::Ready),
                black_box(Duration::from_nanos(100)),
            );
        });
    });

    group.bench_function("after_poll/pending", |b| {
        b.iter(|| {
            manager.after_poll(
                black_box(task_id),
                black_box(PollResult::Pending),
                black_box(Duration::from_nanos(100)),
            );
        });
    });

    group.bench_function("on_yield", |b| {
        b.iter(|| {
            manager.on_yield(black_box(task_id));
        });
    });

    group.bench_function("on_completion", |b| {
        b.iter(|| {
            manager.on_completion(black_box(task_id));
        });
    });

    group.finish();
}

fn bench_tier_manager_operations(c: &mut Criterion) {
    let manager = create_test_manager();

    let mut group = c.benchmark_group("tier_manager/operations");

    group.bench_function("update_config", |b| {
        let new_config = TierConfig {
            poll_budget: 3000,
            cpu_ms_budget: 15,
            yield_interval: 150,
            tier_policies: [
                TierPolicy {
                    name: "Monitor",
                    promotion_threshold: 3,
                    action: InterventionAction::Monitor,
                },
                TierPolicy {
                    name: "Warn",
                    promotion_threshold: 3,
                    action: InterventionAction::Warn,
                },
                TierPolicy {
                    name: "Yield",
                    promotion_threshold: 3,
                    action: InterventionAction::Yield,
                },
                TierPolicy {
                    name: "Isolate",
                    promotion_threshold: 3,
                    action: InterventionAction::Isolate,
                },
            ],
            enable_isolation: false,
            max_slow_queue_size: 2000,
        };
        b.iter(|| {
            manager.update_config(black_box(new_config.clone()));
        });
    });

    group.bench_function("metrics", |b| {
        b.iter(|| black_box(manager.metrics()));
    });

    group.finish();
}

fn bench_tier_promotion(c: &mut Criterion) {
    let manager = create_test_manager();
    let task_id = TaskId(1);
    let context = TaskContext {
        worker_id: 0,
        priority: None,
    };

    c.bench_function("tier_manager/promotion_cycle", |b| {
        b.iter(|| {
            /* Simulate task that triggers tier promotion */
            manager.before_poll(task_id, &context);

            /* Simulate slow poll - trigger warning tier */
            manager.after_poll(
                task_id,
                PollResult::Pending,
                Duration::from_millis(15),
            );

            /* Another slow poll - trigger yield tier */
            manager.before_poll(task_id, &context);
            manager.after_poll(
                task_id,
                PollResult::Pending,
                Duration::from_millis(60),
            );

            /* Clean up */
            manager.on_completion(task_id);
        });
    });
}

fn bench_concurrent_tasks(c: &mut Criterion) {
    let manager = Arc::new(create_test_manager());

    let mut group = c.benchmark_group("tier_manager/concurrent");

    for num_tasks in [10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_tasks),
            num_tasks,
            |b, &num_tasks| {
                b.iter(|| {
                    let handles: Vec<_> = (0..num_tasks)
                        .map(|i| {
                            let manager = Arc::clone(&manager);
                            let task_id = TaskId(i as u64);
                            let context = TaskContext {
                                worker_id: i % 4,
                                priority: None,
                            };

                            thread::spawn(move || {
                                manager.before_poll(task_id, &context);
                                manager.after_poll(
                                    task_id,
                                    PollResult::Ready,
                                    Duration::from_nanos(100),
                                );
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_memory_usage(c: &mut Criterion) {
    use std::mem;

    c.bench_function("tier_manager/memory/size", |b| {
        b.iter(|| black_box(mem::size_of::<TierManager>()));
    });

    /* Benchmark memory growth with many tasks */
    let mut group = c.benchmark_group("tier_manager/memory_growth");

    for num_tasks in [100, 1000, 10000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_tasks),
            num_tasks,
            |b, &num_tasks| {
                let manager = create_test_manager();
                b.iter(|| {
                    for i in 0..num_tasks {
                        let task_id = TaskId(i as u64);
                        let context = TaskContext {
                            worker_id: 0,
                            priority: None,
                        };
                        manager.before_poll(task_id, &context);
                        manager.after_poll(
                            task_id,
                            PollResult::Ready,
                            Duration::from_nanos(100),
                        );
                    }
                    black_box(manager.metrics().active_tasks);
                });
            },
        );
    }

    group.finish();
}

fn bench_slow_queue_operations(c: &mut Criterion) {
    let manager = create_test_manager();

    let mut group = c.benchmark_group("tier_manager/slow_queue");

    /* Benchmark adding tasks to slow queue */
    group.bench_function("enqueue", |b| {
        b.iter(|| {
            for i in 0..10 {
                let task_id = TaskId(i);
                let context = TaskContext {
                    worker_id: 0,
                    priority: None,
                };
                manager.before_poll(task_id, &context);
                /* Trigger slow task detection */
                manager.after_poll(
                    task_id,
                    PollResult::Pending,
                    Duration::from_millis(150),
                );
            }
        });
    });

    /* Benchmark processing slow queue */
    group.bench_function("process", |b| {
        /* Pre-populate slow queue */
        for i in 0..100 {
            let task_id = TaskId(i);
            let context = TaskContext {
                worker_id: 0,
                priority: None,
            };
            manager.before_poll(task_id, &context);
            manager.after_poll(
                task_id,
                PollResult::Pending,
                Duration::from_millis(150),
            );
        }

        b.iter(|| {
            black_box(manager.process_slow_queue(10));
        });
    });

    group.finish();
}

fn bench_worst_case_scenario(c: &mut Criterion) {
    let manager = Arc::new(create_test_manager());

    c.bench_function("tier_manager/worst_case/thundering_herd", |b| {
        b.iter(|| {
            /* Simulate thundering herd - many tasks becoming slow simultaneously */
            let handles: Vec<_> = (0..100)
                .map(|i| {
                    let manager = Arc::clone(&manager);
                    thread::spawn(move || {
                        let task_id = TaskId(i);
                        let context = TaskContext {
                            worker_id: (i as usize) % 8,
                            priority: None,
                        };

                        for _ in 0..10 {
                            manager.before_poll(task_id, &context);
                            manager.after_poll(
                                task_id,
                                PollResult::Pending,
                                Duration::from_millis(200), /* All tasks are slow */
                            );
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }

            black_box(manager.metrics());
        });
    });
}

criterion_group!(
    benches,
    bench_tier_manager_creation,
    bench_tier_manager_hooks,
    bench_tier_manager_operations,
    bench_tier_promotion,
    bench_concurrent_tasks,
    bench_memory_usage,
    bench_slow_queue_operations,
    bench_worst_case_scenario
);

criterion_main!(benches);