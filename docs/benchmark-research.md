# Benchmark Research: Criterion Best Practices and Patterns

## Executive Summary

This document captures research findings on microbenchmarking best practices for Rust, specifically for measuring nanosecond-level operations in lock-free concurrent systems. The research focuses on Criterion.rs and patterns from production systems.

## Key Findings

### 1. Nanosecond-Level Measurement Requirements

#### Measurement Overhead
- Measuring duration itself takes ~10-50 nanoseconds
- For operations faster than 50ns, batch execution is recommended
- Criterion can accurately measure down to single instructions

#### Critical Thresholds
- Uncontended atomic operations: ~10 nanoseconds
- Contended atomic operations (2+ threads): ~50 nanoseconds
- Lock-free operations should target <100ns for our requirements

### 2. Criterion.rs Best Practices

#### Statistical Configuration
```rust
group.significance_level(0.1)  // Balance noise vs detection
     .sample_size(500)         // Larger = more accurate
     .warm_up_time(Duration::from_secs(1))
     .measurement_time(Duration::from_secs(5))
     .noise_threshold(0.01);    // Filter <1% changes
```

#### Sampling Modes
- **Auto** (default): Good for most benchmarks
- **Linear**: For predictable scaling
- **Flat**: For long-running benchmarks (>100ms)

### 3. Black Box Usage

#### Correct Pattern
```rust
b.iter(|| {
    // Input through black_box to prevent constant folding
    let input = black_box(42);

    // Actual operation
    let result = operation(input);

    // Output through black_box to prevent elimination
    black_box(result)
});
```

#### Common Mistakes
- Not black-boxing inputs (allows constant folding)
- Not black-boxing outputs (allows dead code elimination)
- Black-boxing inside hot loops (adds overhead)

### 4. Benchmarking Atomic Operations

#### Uncontended Measurement
```rust
b.iter(|| {
    let value = AtomicU64::new(0);
    for _ in 0..100 {
        value.fetch_add(1, Ordering::Relaxed);
    }
    black_box(value.load(Ordering::Relaxed))
});
```

#### Contended Measurement
```rust
let value = Arc::new(AtomicU64::new(0));
b.iter(|| {
    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let v = Arc::clone(&value);
            thread::spawn(move || {
                for _ in 0..ops_per_thread {
                    v.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
});
```

### 5. Lock-Free Specific Patterns

#### Memory Ordering Impact
- Relaxed: Baseline performance
- Acquire/Release: 5-10% overhead
- SeqCst: 10-30% overhead

#### Contention Scaling
- 1 thread: ~10ns per operation
- 2 threads: ~50ns per operation
- 4 threads: ~100-200ns per operation
- 8 threads: ~200-400ns per operation

### 6. Noise Reduction Techniques

#### System Configuration
```bash
# Disable CPU frequency scaling
sudo cpupower frequency-set --governor performance

# Pin benchmark to specific CPU
taskset -c 0 cargo bench

# Disable hyperthreading for core 0
echo 0 | sudo tee /sys/devices/system/cpu/cpu1/online
```

#### Process Priority
```rust
// Set real-time priority (requires privileges)
use libc::{sched_param, sched_setscheduler, SCHED_FIFO};

let param = sched_param { sched_priority: 1 };
unsafe {
    sched_setscheduler(0, SCHED_FIFO, &param);
}
```

### 7. Benchmark Categories for Tokio-Pulse

#### Critical Path (<100ns requirement)
- Hook registry operations
- Budget atomic operations
- Tier checks

#### Fast Path (<500ns target)
- TierManager lookups
- Metric updates
- State transitions

#### Slow Path (<5μs acceptable)
- OS isolation calls
- Slow queue operations
- Tier interventions

### 8. Statistical Significance Requirements

#### Minimum Standards
- Coefficient of variation <5% for critical path
- 95% confidence interval
- At least 100 samples for operations <1μs
- At least 1000 iterations per sample

#### Regression Detection
- Critical path: Flag >5% regression
- Fast path: Flag >10% regression
- Slow path: Flag >20% regression

## Implementation Recommendations

### 1. Benchmark Structure
```
benches/
├── critical_path.rs    # <100ns operations
├── concurrent.rs       # Contention scenarios
├── integration.rs      # Full system benchmarks
└── regression.rs       # Baseline comparisons
```

### 2. Measurement Strategy

#### For <50ns Operations
```rust
b.iter(|| {
    // Batch 100 operations
    for _ in 0..100 {
        operation();
    }
});
// Divide result by 100
```

#### For Atomic Operations
```rust
// Measure both uncontended and contended
group.bench_function("atomic/uncontended", |b| { ... });
group.bench_function("atomic/contended_2", |b| { ... });
group.bench_function("atomic/contended_4", |b| { ... });
group.bench_function("atomic/contended_8", |b| { ... });
```

### 3. Validation Checklist

- [ ] Compile with --release
- [ ] Use black_box on inputs and outputs
- [ ] Run for at least 5 seconds
- [ ] Collect at least 100 samples
- [ ] Verify coefficient of variation <5%
- [ ] Test on multiple platforms
- [ ] Compare against baseline
- [ ] Document expected ranges

## References

1. Criterion.rs Documentation: https://bheisler.github.io/criterion.rs/
2. Rust Performance Book: https://nnethercote.github.io/perf-book/
3. "Concurrency Cost Hierarchy": https://travisdowns.github.io/blog/2020/07/06/concurrency-costs.html
4. "Lock-freedom without garbage collection": https://aturon.github.io/blog/2015/08/27/epoch/
5. "Why my Rust benchmarks were wrong": https://gendignoux.com/blog/2022/01/31/rust-benchmarks.html

## Next Steps

1. Apply these patterns to fix compilation errors in existing benchmarks
2. Establish baseline measurements for all critical operations
3. Implement continuous regression detection in CI
4. Create benchmark dashboard for tracking trends