# Tokio-Pulse Benchmark Architecture

## Design Document Version 1.0

### Executive Summary

This document defines the formal benchmark architecture for Tokio-Pulse, establishing categories, measurement methodologies, and validation criteria based on production system analysis.

## Architecture Overview

### Benchmark Hierarchy

```
tokio-pulse/benches/
├── tier0_critical/       # <100ns requirement
│   ├── hooks.rs          # Hook registry operations
│   ├── budget.rs         # TaskBudget atomic operations
│   └── tier_check.rs     # Tier state checks
├── tier1_fast/           # <500ns target
│   ├── tier_manager.rs   # TierManager operations
│   ├── metrics.rs        # Metrics collection
│   └── state.rs          # State transitions
├── tier2_concurrent/     # Contention scenarios
│   ├── contention.rs     # Progressive thread scaling
│   ├── fairness.rs       # Scheduling fairness
│   └── thundering.rs     # Thundering herd tests
├── tier3_system/         # System integration
│   ├── isolation.rs      # OS isolation overhead
│   ├── slow_queue.rs     # Slow queue operations
│   └── intervention.rs   # Tier interventions
└── regression/           # Baseline comparisons
    ├── baseline.rs       # Performance baselines
    └── memory.rs         # Memory usage tracking
```

## Benchmark Categories

### Tier 0: Critical Path (<100ns)

#### Requirements
- Maximum overhead: 100ns
- Target overhead: 50ns
- Coefficient of variation: <5%
- Minimum samples: 1000

#### Operations
```rust
// Hook Registry
- HookRegistry::before_poll()     // Target: <20ns
- HookRegistry::after_poll()      // Target: <20ns
- HookRegistry::has_hooks()       // Target: <10ns

// Task Budget
- TaskBudget::consume()           // Target: <15ns
- TaskBudget::is_exhausted()      // Target: <10ns
- TaskBudget::remaining()         // Target: <10ns

// Tier Checks
- TierManager::get_tier()         // Target: <20ns
- TierManager::should_yield()     // Target: <15ns
```

#### Measurement Strategy
```rust
// Batch operations for sub-50ns measurements
const BATCH_SIZE: usize = 100;

group.bench_function("hook_before_poll", |b| {
    let registry = HookRegistry::new();
    let task_id = TaskId(1);
    let context = TaskContext::default();

    b.iter(|| {
        for _ in 0..BATCH_SIZE {
            registry.before_poll(
                black_box(task_id),
                black_box(&context)
            );
        }
    });
});
// Divide result by BATCH_SIZE for per-operation time
```

### Tier 1: Fast Path (<500ns)

#### Requirements
- Maximum overhead: 500ns
- Target overhead: 200ns
- Coefficient of variation: <10%
- Minimum samples: 500

#### Operations
```rust
// TierManager
- TierManager::record_poll_time()    // Target: <100ns
- TierManager::update_tier()         // Target: <200ns
- TierManager::get_metrics()         // Target: <150ns

// Metrics Collection
- Metrics::increment_counter()       // Target: <50ns
- Metrics::record_histogram()        // Target: <100ns

// State Management
- StateManager::transition()         // Target: <200ns
- StateManager::check_cooldown()     // Target: <50ns
```

### Tier 2: Concurrent Operations

#### Requirements
- Progressive scaling: 1, 2, 4, 8, 16 threads
- Fairness measurement
- Contention characterization

#### Contention Scenarios
```rust
const THREAD_COUNTS: &[usize] = &[1, 2, 4, 8, 16];
const OPERATIONS_PER_THREAD: usize = 10_000;

for &num_threads in THREAD_COUNTS {
    group.bench_with_input(
        BenchmarkId::new("contention", num_threads),
        &num_threads,
        |b, &threads| {
            let shared = Arc::new(Structure::new());

            b.iter(|| {
                let barrier = Arc::new(Barrier::new(threads));

                crossbeam::scope(|s| {
                    for _ in 0..threads {
                        let local = Arc::clone(&shared);
                        let b = Arc::clone(&barrier);

                        s.spawn(move |_| {
                            b.wait(); // Synchronize start
                            for _ in 0..OPERATIONS_PER_THREAD {
                                local.operation();
                            }
                        });
                    }
                }).unwrap();
            });
        }
    );
}
```

#### Expected Scaling
| Threads | Target Latency | Maximum Latency |
|---------|----------------|-----------------|
| 1       | <10ns          | 20ns            |
| 2       | <50ns          | 100ns           |
| 4       | <100ns         | 200ns           |
| 8       | <200ns         | 400ns           |
| 16      | <400ns         | 800ns           |

### Tier 3: System Integration

#### Requirements
- OS-specific isolation testing
- Container environment validation
- Permission handling verification

#### Test Matrix
```rust
// OS Isolation Overhead
- Linux cgroups v2 setup         // Target: <5μs
- Windows job object creation    // Target: <10μs
- macOS thread policy set        // Target: <2μs

// Slow Queue Operations
- Enqueue task                   // Target: <500ns
- Process batch (10 tasks)       // Target: <5μs
- Queue rebalancing              // Target: <10μs

// Intervention Actions
- Monitor (no-op)                // Target: <10ns
- Warn (log + metrics)           // Target: <200ns
- Yield (force yield)            // Target: <1μs
- Isolate (OS call)             // Target: <10μs
```

## Measurement Methodology

### Statistical Requirements

#### Tier 0 (Critical Path)
```rust
criterion_group! {
    name = tier0_critical;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(3))
        .measurement_time(Duration::from_secs(10))
        .sample_size(1000)
        .significance_level(0.01)
        .noise_threshold(0.02);
    targets = bench_hooks, bench_budget, bench_tier_check
}
```

#### Tier 1 (Fast Path)
```rust
criterion_group! {
    name = tier1_fast;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(5))
        .sample_size(500)
        .significance_level(0.05)
        .noise_threshold(0.05);
    targets = bench_tier_manager, bench_metrics
}
```

### Black Box Patterns

#### Input Protection
```rust
// Prevent constant folding
let input = black_box(42);
let result = operation(input);
black_box(result); // Prevent dead code elimination
```

#### Structure Protection
```rust
// For complex structures
let config = black_box(TierConfig {
    poll_budget: black_box(2000),
    cpu_ms_budget: black_box(10),
    ..Default::default()
});
```

### Runtime Isolation

#### Per-Benchmark Runtime
```rust
fn create_runtime() -> Runtime {
    runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("bench-worker")
        .enable_all()
        .build()
        .unwrap()
}

b.iter_batched(
    || create_runtime(),
    |rt| {
        rt.block_on(async_operation())
    },
    BatchSize::SmallInput
);
```

## Validation Criteria

### Performance Regression Detection

#### Critical Path
- Flag if >5% regression from baseline
- Automatic CI failure
- Require explicit approval to merge

#### Fast Path
- Flag if >10% regression from baseline
- Warning in CI
- Recommend investigation

#### System Integration
- Flag if >20% regression from baseline
- Informational only
- Track trends over time

### Baseline Management

#### Baseline Files
```
baselines/
├── main.json          # Current main branch baseline
├── v0.1.0.json        # Release baselines
├── v0.2.0.json
└── nightly/           # Nightly regression tracking
    ├── 2024-01-01.json
    └── 2024-01-02.json
```

#### Baseline Comparison
```rust
// In CI pipeline
cargo bench -- --baseline main --save-baseline pr-$PR_NUMBER

// Check regression
if let Some(regression) = check_regression("main", "pr-$PR_NUMBER") {
    if regression.tier == 0 && regression.percent > 5.0 {
        panic!("Critical path regression: {:.1}%", regression.percent);
    }
}
```

## Platform-Specific Considerations

### Linux
```rust
#[cfg(target_os = "linux")]
fn configure_linux_performance() {
    // Set CPU governor to performance
    Command::new("sudo")
        .args(&["cpupower", "frequency-set", "--governor", "performance"])
        .status()
        .ok();

    // Disable CPU frequency scaling
    std::fs::write(
        "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
        "performance"
    ).ok();
}
```

### macOS
```rust
#[cfg(target_os = "macos")]
fn configure_macos_performance() {
    // Disable power nap
    Command::new("sudo")
        .args(&["pmset", "-a", "powernap", "0"])
        .status()
        .ok();
}
```

### Windows
```rust
#[cfg(target_os = "windows")]
fn configure_windows_performance() {
    // Set high performance power plan
    Command::new("powercfg")
        .args(&["/setactive", "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c"])
        .status()
        .ok();
}
```

## Memory Benchmarks

### Allocation Tracking
```rust
#[global_allocator]
static ALLOC: TrackingAllocator = TrackingAllocator;

group.bench_function("memory_allocation", |b| {
    b.iter_custom(|iters| {
        let before = ALLOC.allocated();

        for _ in 0..iters {
            operation();
        }

        let after = ALLOC.allocated();
        Duration::from_nanos((after - before) / iters)
    });
});
```

### Cache Miss Analysis
```rust
#[cfg(target_os = "linux")]
fn measure_cache_misses() {
    use perf_event::{Builder, Counter};

    let mut counter = Builder::new()
        .kind(perf_event::events::Hardware::CACHE_MISSES)
        .build()
        .unwrap();

    counter.enable().unwrap();
    operation();
    counter.disable().unwrap();

    println!("Cache misses: {}", counter.read().unwrap());
}
```

## Continuous Integration

### GitHub Actions Workflow
```yaml
benchmark:
  runs-on: ${{ matrix.os }}
  strategy:
    matrix:
      os: [ubuntu-latest, macos-latest, windows-latest]

  steps:
    - name: Configure Performance
      run: |
        ${{ matrix.os == 'ubuntu-latest' && 'sudo cpupower frequency-set --governor performance' || '' }}
        ${{ matrix.os == 'macos-latest' && 'sudo pmset -a powernap 0' || '' }}
        ${{ matrix.os == 'windows-latest' && 'powercfg /setactive 8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c' || '' }}

    - name: Run Benchmarks
      run: |
        cargo bench --all-features -- --save-baseline pr-${{ github.event.pull_request.number }}

    - name: Check Regression
      run: |
        python3 .github/scripts/check_regression.py \
          --baseline main \
          --current pr-${{ github.event.pull_request.number }} \
          --tier0-threshold 5 \
          --tier1-threshold 10
```

## Reporting

### Performance Dashboard
```
┌─────────────────────────────────────────┐
│        Tokio-Pulse Performance          │
├─────────────────────────────────────────┤
│ Critical Path (Tier 0)                  │
│ ├─ hook_before_poll:    18.2ns ✓       │
│ ├─ budget_consume:      12.5ns ✓       │
│ └─ tier_check:          15.8ns ✓       │
├─────────────────────────────────────────┤
│ Fast Path (Tier 1)                      │
│ ├─ tier_update:        156.3ns ✓       │
│ └─ metrics_record:      89.7ns ✓       │
├─────────────────────────────────────────┤
│ Contention Scaling                      │
│ ├─ 1 thread:            9.8ns ✓        │
│ ├─ 2 threads:          48.3ns ✓        │
│ ├─ 4 threads:          97.6ns ✓        │
│ └─ 8 threads:         195.2ns ✓        │
└─────────────────────────────────────────┘
```

## Maintenance

### Weekly Tasks
- Review nightly benchmark trends
- Update baselines after releases
- Investigate any consistent regressions

### Monthly Tasks
- Analyze long-term performance trends
- Update performance targets based on data
- Review and optimize slowest operations

### Quarterly Tasks
- Full benchmark suite audit
- Platform-specific optimization review
- Update measurement methodology based on learnings

## Success Metrics

### Phase 1 (Current)
- [ ] All Tier 0 operations <100ns
- [ ] All Tier 1 operations <500ns
- [ ] Contention scaling matches targets
- [ ] CI regression detection operational

### Phase 2 (Q2 2024)
- [ ] All Tier 0 operations <50ns
- [ ] Memory allocation tracking
- [ ] Cache miss analysis
- [ ] NUMA-aware benchmarks

### Phase 3 (Q3 2024)
- [ ] Hardware performance counter integration
- [ ] Automated performance optimization suggestions
- [ ] Cross-platform performance parity

## References

1. Criterion.rs Best Practices
2. Tokio Benchmark Suite Analysis
3. Crossbeam Performance Patterns
4. Parking Lot Fast Path Design