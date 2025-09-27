# Production Benchmark Patterns Analysis

## Executive Summary

This document analyzes benchmark patterns from Tokio, Crossbeam, and Parking Lot to understand how production Rust projects measure nanosecond-level operations in concurrent systems.

## Tokio Benchmark Patterns

### 1. Runtime Configuration

#### Multi-threaded Runtime Setup
```rust
fn rt() -> Runtime {
    runtime::Builder::new_multi_thread()
        .worker_threads(NUM_WORKERS)
        .enable_all()
        .build()
        .unwrap()
}
```

#### Key Observations
- Fixed worker thread count (typically 4-6)
- Separate runtime per benchmark for isolation
- Uses `block_on()` for async operations in benchmarks

### 2. Contention Testing Patterns

#### Uncontended Scenario
```rust
b.iter(|| {
    let lock = Arc::new(RwLock::new(()));
    rt.block_on(async {
        let _guard = lock.read().await;
        black_box(_guard);
    })
});
```

#### Contended Scenario
```rust
let lock = Arc::new(RwLock::new(()));
let write_guard = rt.block_on(lock.write());

b.iter(|| {
    rt.block_on(async {
        tokio::join!(
            async { black_box(lock.read().await) },
            async { black_box(lock.read().await) }
        );
    })
});
```

### 3. Channel Benchmarking

#### MPSC Pattern
```rust
// Measure send operations
b.iter(|| {
    rt.block_on(async {
        let (tx, mut rx) = mpsc::channel::<i32>(1000);
        for i in 0..100 {
            tx.send(i).await.unwrap();
        }
        black_box(rx.recv().await);
    })
});
```

#### Contention Simulation
- Spawn multiple senders/receivers
- Use `tokio::spawn` for concurrent operations
- Measure throughput with fixed message counts

### 4. Scheduler Overhead Measurement

#### Spawn Patterns
```rust
// Local spawn (same worker)
b.iter(|| {
    rt.block_on(async {
        let h = tokio::spawn(async { 42 });
        black_box(h.await.unwrap());
    })
});

// Remote spawn (different worker)
b.iter(|| {
    let handle = rt.spawn(async { 42 });
    rt.block_on(handle);
});
```

#### Yield Testing
```rust
b.iter(|| {
    rt.block_on(async {
        for _ in 0..NUM_YIELD {
            tokio::task::yield_now().await;
        }
    })
});
```

## Crossbeam Benchmark Patterns

### 1. Lock-Free Channel Testing

#### SPSC (Single Producer Single Consumer)
```rust
b.iter(|| {
    let (s, r) = unbounded::<i32>();
    scope(|scope| {
        scope.spawn(|_| {
            for i in 0..TOTAL_STEPS {
                s.send(i).unwrap();
            }
        });
        scope.spawn(|_| {
            for _ in 0..TOTAL_STEPS {
                r.recv().unwrap();
            }
        });
    }).unwrap();
});
```

#### MPMC Scaling
```rust
let threads = num_cpus::get();
let steps_per_thread = TOTAL_STEPS / threads;

scope(|scope| {
    for _ in 0..threads {
        scope.spawn(|_| {
            for _ in 0..steps_per_thread {
                tx.send(1).unwrap();
            }
        });
    }
}).unwrap();
```

### 2. Epoch-Based Memory Reclamation

#### Defer Pattern
```rust
b.iter(|| {
    let guard = epoch::pin();
    for _ in 0..STEPS {
        let ptr = Owned::new(42).into_shared(&guard);
        guard.defer_destroy(ptr);
    }
});
```

#### Multi-threaded Pressure
```rust
b.iter(|| {
    scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|_| {
                let guard = epoch::pin();
                for _ in 0..STEPS {
                    guard.defer(|| {
                        // Lightweight operation
                        black_box(42);
                    });
                }
            });
        }
    }).unwrap();
});
```

### 3. Throughput Measurement

#### Constants
```rust
const TOTAL_STEPS: usize = 40_000;
const THREADS: usize = 8;
```

#### Scaling Pattern
- Calculate steps per thread dynamically
- Use CPU core count for thread scaling
- Fixed total work for consistent comparison

## Parking Lot Patterns

### 1. Fast Path Optimization

#### Uncontended Mutex
- Single atomic operation for lock/unlock
- Inline fast path for common case
- No heap allocation for uncontended case

### 2. Adaptive Spinning

#### Microcontention Handling
```rust
// Conceptual pattern (from documentation)
loop {
    if try_lock() { break; }

    spin_count += 1;
    if spin_count > SPIN_LIMIT {
        park_thread();
        break;
    }

    spin_wait();
}
```

### 3. Eventual Fairness

#### Fair Unlock Pattern
- Track lock hold time
- Use fair unlock after 0.5ms average hold
- Or when held >1ms continuously

## Key Patterns for Tokio-Pulse

### 1. Benchmark Organization

```
benches/
├── critical_path/
│   ├── hooks.rs         # <100ns operations
│   ├── budget.rs        # Atomic operations
│   └── tier_check.rs    # Fast tier lookups
├── concurrent/
│   ├── contention.rs    # Multi-thread scenarios
│   ├── work_stealing.rs # Task migration
│   └── fairness.rs      # Scheduling fairness
└── integration/
    ├── full_system.rs   # End-to-end tests
    └── regression.rs    # Baseline comparison
```

### 2. Measurement Strategy

#### For <50ns Operations
```rust
group.bench_function("atomic_op", |b| {
    let value = AtomicU64::new(0);
    b.iter(|| {
        // Batch operations to amortize measurement overhead
        for _ in 0..100 {
            value.fetch_add(1, Ordering::Relaxed);
        }
    });
});
```

#### For Concurrent Operations
```rust
group.bench_function("concurrent_access", |b| {
    let shared = Arc::new(Structure::new());
    b.iter(|| {
        scope(|s| {
            for _ in 0..NUM_THREADS {
                let local = Arc::clone(&shared);
                s.spawn(move |_| {
                    for _ in 0..OPS_PER_THREAD {
                        local.operation();
                    }
                });
            }
        }).unwrap();
    });
});
```

### 3. Contention Scenarios

#### Progressive Contention
```rust
for threads in [1, 2, 4, 8, 16] {
    group.bench_with_input(
        BenchmarkId::from_parameter(threads),
        &threads,
        |b, &num_threads| {
            // Benchmark with specific thread count
        }
    );
}
```

### 4. Statistical Validation

#### Setup from Production Projects
```rust
group
    .warm_up_time(Duration::from_secs(1))
    .measurement_time(Duration::from_secs(5))
    .sample_size(100)
    .significance_level(0.05);
```

## Specific Techniques

### 1. Preventing False Sharing

```rust
#[repr(align(64))]  // Cache line alignment
struct PaddedAtomic {
    value: AtomicU64,
    _pad: [u8; 56],  // Padding to 64 bytes
}
```

### 2. Black Box Usage

```rust
b.iter(|| {
    let input = black_box(42);      // Prevent constant folding
    let result = operation(input);
    black_box(result);               // Prevent dead code elimination
});
```

### 3. Runtime Isolation

```rust
// Create fresh runtime per benchmark
b.iter_batched(
    || Runtime::new().unwrap(),     // Setup
    |rt| {
        rt.block_on(async_operation())
    },
    BatchSize::SmallInput
);
```

### 4. Thundering Herd Testing

```rust
let barrier = Arc::new(Barrier::new(num_threads));

scope(|s| {
    for _ in 0..num_threads {
        let b = Arc::clone(&barrier);
        s.spawn(move |_| {
            b.wait();  // Synchronize start
            // Concurrent operation
        });
    }
}).unwrap();
```

## Performance Targets from Analysis

### Based on Production Systems

| Operation Type | Target | Source |
|---------------|--------|--------|
| Uncontended atomic | <10ns | Crossbeam benchmarks |
| Uncontended mutex | <20ns | Parking lot fast path |
| 2-thread contention | <50ns | Crossbeam measurements |
| 8-thread contention | <200ns | Tokio benchmarks |
| Channel send/recv | <100ns | MPSC benchmarks |
| Task spawn | <500ns | Runtime benchmarks |
| Context switch | <1.5μs | Yield benchmarks |

## Recommendations for Tokio-Pulse

### 1. Adopt Tokio's Runtime Patterns
- Use fixed worker threads for consistency
- Separate runtime per benchmark
- Block on async operations

### 2. Follow Crossbeam's Scaling
- Test with 1, 2, 4, 8, 16 threads
- Use TOTAL_STEPS constant
- Scale work per thread

### 3. Implement Parking Lot's Techniques
- Fast path for uncontended case
- Adaptive spinning for microcontention
- Eventual fairness testing

### 4. Statistical Rigor
- Minimum 5 second measurement time
- 100+ samples for <1μs operations
- Progressive contention testing
- Baseline regression comparison

## Next Steps

1. Apply these patterns to fix compilation errors
2. Create benchmark categories matching production systems
3. Establish baseline using these measurement techniques
4. Implement continuous regression detection