/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */
/* Benchmarks for task budget operations */
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::sync::Arc;
use std::thread;
use tokio_pulse::budget::{DEFAULT_BUDGET, MAX_BUDGET, MIN_BUDGET, TaskBudget};

fn bench_budget_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_budget/creation");

    group.bench_function("new_default", |b| {
        b.iter(|| black_box(TaskBudget::new(DEFAULT_BUDGET)));
    });

    group.bench_function("new_min", |b| {
        b.iter(|| black_box(TaskBudget::new(MIN_BUDGET)));
    });

    group.bench_function("new_max", |b| {
        b.iter(|| black_box(TaskBudget::new(MAX_BUDGET)));
    });

    group.bench_function("new_arc", |b| {
        b.iter(|| black_box(Arc::new(TaskBudget::new(DEFAULT_BUDGET))));
    });

    group.finish();
}

fn bench_budget_operations(c: &mut Criterion) {
    let budget = TaskBudget::new(DEFAULT_BUDGET);

    let mut group = c.benchmark_group("task_budget/operations");

    group.bench_function("consume", |b| {
        budget.reset(DEFAULT_BUDGET);
        b.iter(|| black_box(budget.consume()));
    });

    group.bench_function("reset", |b| {
        b.iter(|| {
            budget.reset(black_box(DEFAULT_BUDGET));
        });
    });

    group.bench_function("remaining", |b| {
        b.iter(|| black_box(budget.remaining()));
    });

    group.bench_function("is_exhausted", |b| {
        b.iter(|| black_box(budget.is_exhausted()));
    });

    group.bench_function("tier", |b| {
        b.iter(|| black_box(budget.tier()));
    });

    group.bench_function("escalate_tier", |b| {
        b.iter(|| black_box(budget.escalate_tier()));
    });

    group.bench_function("reset_tier", |b| {
        b.iter(|| budget.reset_tier());
    });

    group.bench_function("set_deadline", |b| {
        b.iter(|| budget.set_deadline(black_box(100_000_000)));
    });

    group.bench_function("deadline", |b| {
        b.iter(|| black_box(budget.deadline()));
    });

    group.finish();
}

fn bench_budget_exhaustion(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_budget/exhaustion");

    group.bench_function("consume_until_exhausted", |b| {
        let budget = TaskBudget::new(100);
        b.iter(|| {
            budget.reset(100);
            while !budget.is_exhausted() {
                budget.consume();
            }
        });
    });

    group.bench_function("check_and_consume", |b| {
        let budget = TaskBudget::new(1000);
        b.iter(|| {
            budget.reset(1000);
            for _ in 0..100 {
                if !budget.is_exhausted() {
                    budget.consume();
                }
            }
        });
    });

    group.finish();
}

fn bench_budget_contention(c: &mut Criterion) {
    let budget = Arc::new(TaskBudget::new(MAX_BUDGET));

    let mut group = c.benchmark_group("task_budget/contention");

    for num_threads in [2, 4, 8].iter() {
        group.bench_with_input(
            BenchmarkId::new("concurrent_consume", num_threads),
            num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    budget.reset(MAX_BUDGET);
                    let handles: Vec<_> = (0..num_threads)
                        .map(|_| {
                            let budget = Arc::clone(&budget);
                            thread::spawn(move || {
                                for _ in 0..100 {
                                    budget.consume();
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

fn bench_budget_memory_layout(c: &mut Criterion) {
    use std::mem;

    c.bench_function("task_budget/memory/size", |b| {
        b.iter(|| black_box(mem::size_of::<TaskBudget>()));
    });

    c.bench_function("task_budget/memory/alignment", |b| {
        b.iter(|| black_box(mem::align_of::<TaskBudget>()));
    });

    /* Verify cache-line alignment */
    c.bench_function("task_budget/memory/cache_aligned", |b| {
        b.iter(|| {
            let budget = TaskBudget::new(DEFAULT_BUDGET);
            let addr = &budget as *const _ as usize;
            black_box(addr % 64 == 0) /* Check 64-byte alignment */
        });
    });
}

fn bench_budget_tier_escalation(c: &mut Criterion) {
    let budget = TaskBudget::new(DEFAULT_BUDGET);

    c.bench_function("task_budget/tier_escalation", |b| {
        b.iter(|| {
            budget.reset_tier();

            /* Escalate through all tiers */
            let tier1 = budget.escalate_tier();
            let tier2 = budget.escalate_tier();
            let tier3 = budget.escalate_tier();

            black_box((tier1, tier2, tier3));
        });
    });
}

fn bench_budget_typical_workload(c: &mut Criterion) {
    let budget = TaskBudget::new(DEFAULT_BUDGET);

    c.bench_function("task_budget/typical_workload", |b| {
        b.iter(|| {
            budget.reset(DEFAULT_BUDGET);

            /* Simulate typical poll pattern */
            for _ in 0..50 {
                if budget.is_exhausted() {
                    break;
                }
                budget.consume();
            }

            black_box(budget.remaining());
        });
    });
}

criterion_group!(
    benches,
    bench_budget_creation,
    bench_budget_operations,
    bench_budget_exhaustion,
    bench_budget_contention,
    bench_budget_memory_layout,
    bench_budget_tier_escalation,
    bench_budget_typical_workload
);

criterion_main!(benches);
