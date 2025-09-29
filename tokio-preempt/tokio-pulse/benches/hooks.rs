//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
// Benchmarks for hook registry performance
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::sync::Arc;
use std::time::Duration;
use tokio_pulse::hooks::{HookRegistry, NullHooks, PreemptionHooks};
use tokio_pulse::tier_manager::{PollResult, TaskContext, TaskId};

// Mock hooks for benchmarking
struct NoOpHooks;

impl PreemptionHooks for NoOpHooks {
    fn before_poll(&self, _task_id: TaskId, _context: &TaskContext) {
        // Minimal work to measure overhead
        black_box(42);
    }

    fn after_poll(&self, _task_id: TaskId, _result: PollResult, _duration: Duration) {
        // Minimal work to measure overhead
        black_box(42);
    }

    fn on_yield(&self, _task_id: TaskId) {
        // Minimal work to measure overhead
        black_box(42);
    }

    fn on_completion(&self, _task_id: TaskId) {
        // Minimal work to measure overhead
        black_box(42);
    }
}

fn bench_hook_registry_no_hooks(c: &mut Criterion) {
    let registry = HookRegistry::new();
    let task_id = TaskId(1);
    let context = TaskContext {
        worker_id: 0,
        priority: None,
    };

    let mut group = c.benchmark_group("hook_registry/no_hooks");

    group.bench_function("before_poll", |b| {
        b.iter(|| {
            registry.before_poll(black_box(task_id), black_box(&context));
        });
    });

    group.bench_function("after_poll", |b| {
        b.iter(|| {
            registry.after_poll(
                black_box(task_id),
                black_box(PollResult::Ready),
                black_box(Duration::from_nanos(100)),
            );
        });
    });

    group.bench_function("on_yield", |b| {
        b.iter(|| {
            registry.on_yield(black_box(task_id));
        });
    });

    group.bench_function("on_completion", |b| {
        b.iter(|| {
            registry.on_completion(black_box(task_id));
        });
    });

    group.bench_function("has_hooks", |b| {
        b.iter(|| black_box(registry.has_hooks()));
    });

    group.finish();
}

fn bench_hook_registry_with_hooks(c: &mut Criterion) {
    let registry = HookRegistry::new();
    registry.set_hooks(Arc::new(NoOpHooks));

    let task_id = TaskId(1);
    let context = TaskContext {
        worker_id: 0,
        priority: None,
    };

    let mut group = c.benchmark_group("hook_registry/with_hooks");

    group.bench_function("before_poll", |b| {
        b.iter(|| {
            registry.before_poll(black_box(task_id), black_box(&context));
        });
    });

    group.bench_function("after_poll", |b| {
        b.iter(|| {
            registry.after_poll(
                black_box(task_id),
                black_box(PollResult::Ready),
                black_box(Duration::from_nanos(100)),
            );
        });
    });

    group.bench_function("on_yield", |b| {
        b.iter(|| {
            registry.on_yield(black_box(task_id));
        });
    });

    group.bench_function("on_completion", |b| {
        b.iter(|| {
            registry.on_completion(black_box(task_id));
        });
    });

    group.finish();
}

fn bench_hook_installation(c: &mut Criterion) {
    let mut group = c.benchmark_group("hook_registry/installation");

    group.bench_function("set_hooks", |b| {
        let registry = HookRegistry::new();
        b.iter(|| {
            let hooks = Arc::new(NoOpHooks);
            black_box(registry.set_hooks(hooks));
        });
    });

    group.bench_function("clear_hooks", |b| {
        let registry = HookRegistry::new();
        registry.set_hooks(Arc::new(NoOpHooks));
        b.iter(|| {
            black_box(registry.clear_hooks());
        });
    });

    group.finish();
}

fn bench_null_hooks(c: &mut Criterion) {
    let hooks = NullHooks;
    let task_id = TaskId(1);
    let context = TaskContext {
        worker_id: 0,
        priority: None,
    };

    let mut group = c.benchmark_group("null_hooks");

    group.bench_function("before_poll", |b| {
        b.iter(|| {
            hooks.before_poll(black_box(task_id), black_box(&context));
        });
    });

    group.bench_function("after_poll", |b| {
        b.iter(|| {
            hooks.after_poll(
                black_box(task_id),
                black_box(PollResult::Ready),
                black_box(Duration::from_nanos(100)),
            );
        });
    });

    group.finish();
}

fn bench_concurrent_access(c: &mut Criterion) {
    use std::sync::Barrier;
    use std::thread;

    let registry = Arc::new(HookRegistry::new());
    registry.set_hooks(Arc::new(NoOpHooks));

    let mut group = c.benchmark_group("hook_registry/concurrent");

    for num_threads in [2, 4, 8].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_threads),
            num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let barrier = Arc::new(Barrier::new(num_threads));
                    let handles: Vec<_> = (0..num_threads)
                        .map(|_| {
                            let registry = Arc::clone(&registry);
                            let barrier = Arc::clone(&barrier);
                            thread::spawn(move || {
                                barrier.wait();
                                let task_id = TaskId(1);
                                let context = TaskContext {
                                    worker_id: 0,
                                    priority: None,
                                };
                                for _ in 0..1000 {
                                    registry.before_poll(task_id, &context);
                                }
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

fn bench_critical_path(c: &mut Criterion) {
    let registry_no_hooks = HookRegistry::new();
    let registry_with_hooks = HookRegistry::new();
    registry_with_hooks.set_hooks(Arc::new(NoOpHooks));

    let task_id = TaskId(1);
    let context = TaskContext {
        worker_id: 0,
        priority: None,
    };

    let mut group = c.benchmark_group("critical_path");

    // Measure the complete poll cycle overhead
    group.bench_function("poll_cycle/no_hooks", |b| {
        b.iter(|| {
            registry_no_hooks.before_poll(black_box(task_id), black_box(&context));
            // Simulate poll work
            black_box(std::hint::black_box(42));
            registry_no_hooks.after_poll(
                black_box(task_id),
                black_box(PollResult::Ready),
                black_box(Duration::from_nanos(100)),
            );
        });
    });

    group.bench_function("poll_cycle/with_hooks", |b| {
        b.iter(|| {
            registry_with_hooks.before_poll(black_box(task_id), black_box(&context));
            // Simulate poll work
            black_box(std::hint::black_box(42));
            registry_with_hooks.after_poll(
                black_box(task_id),
                black_box(PollResult::Ready),
                black_box(Duration::from_nanos(100)),
            );
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_hook_registry_no_hooks,
    bench_hook_registry_with_hooks,
    bench_hook_installation,
    bench_null_hooks,
    bench_concurrent_access,
    bench_critical_path
);

criterion_main!(benches);
