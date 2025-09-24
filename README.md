# Tokio-Pulse

High-performance preemption system for the Tokio asynchronous runtime.

## Overview

Tokio-Pulse provides preemptive task scheduling for the Tokio runtime, preventing CPU-bound tasks from monopolizing worker threads. The system implements graduated intervention with sub-100ns overhead per poll operation.

## System Requirements

### Minimum Requirements
- Rust 1.75.0 or later
- Tokio 1.40.0 or later
- C compiler (for build dependencies)

### Supported Platforms

| Platform | Architecture | Compiler | Notes |
|----------|-------------|----------|-------|
| Linux | x86_64, aarch64 | GCC 9+, Clang 10+ | Primary development platform |
| Windows | x86_64 | MSVC 2019+, MinGW | QueryThreadCycleTime API required |
| macOS | x86_64, aarch64 | Xcode 12+ | Higher timing overhead due to mach ports |

### Hardware Requirements
- 64-bit processor with atomic instruction support
- Minimum 16 bytes per tracked task
- Cache line size awareness (typically 64 bytes)

## Installation

### Using Cargo (Recommended)

Add to your `Cargo.toml`:

```toml
[dependencies]
tokio-pulse = "0.1"
```

### From Source

```bash
git clone https://github.com/ziXnOrg/Tokio-Pulse
cd Tokio-Pulse
cargo build --workspace --release
```

### Development Build

```bash
git clone --recursive https://github.com/ziXnOrg/Tokio-Pulse
cd Tokio-Pulse
./dev.sh build
```

## Build Configuration

### Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `metrics` | Yes | Enable metrics collection via metrics-rs |
| `tracing` | Yes | Enable detailed tracing via tracing-subscriber |
| `tokio_unstable` | No | Enable unstable Tokio features |
| `nightly` | No | Enable nightly Rust optimizations |

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `TOKIO_PULSE_BUDGET` | Default task budget | 2000 |
| `TOKIO_PULSE_CPU_THRESHOLD` | CPU time threshold (ns) | 1000000 |
| `TOKIO_PULSE_TIER_ESCALATION` | Enable tier escalation | true |

## API Reference

### Core Types

#### `TaskBudget`
16-byte cache-aligned structure managing task execution budget.

```rust
pub struct TaskBudget {
    deadline_ns: AtomicU64,  // CPU time deadline
    remaining: AtomicU32,     // Operations remaining
    tier: AtomicU8,          // Intervention tier (0-3)
    _padding: [u8; 3],       // Cache alignment
}
```

#### `TierManager`
Central coordinator for task preemption decisions.

```rust
pub struct TierManager {
    config: Arc<RwLock<TierConfig>>,
    task_states: DashMap<TaskId, Arc<TaskState>>,
    slow_queue: SegQueue<TaskId>,
    // ... metrics fields
}
```

#### `HookRegistry`
Runtime hook installation mechanism.

```rust
pub struct HookRegistry {
    hooks: AtomicPtr<HooksArc>,
}
```

### Key Functions

#### `TaskBudget::consume()`
Decrements budget and returns true when exhausted.
- Overhead: <20ns
- Memory ordering: Relaxed
- Thread-safe: Yes

#### `TierManager::before_poll()`
Pre-poll hook for task state initialization.
- Overhead: <50ns typical
- Allocations: None

#### `TierManager::after_poll()`
Post-poll hook for tier evaluation.
- Overhead: <50ns typical
- May trigger tier promotion

## Architecture

### Component Overview

```
┌─────────────────────────────────────────────────┐
│                 Tokio Runtime                   │
│  ┌───────────────────────────────────────────┐  │
│  │          Worker Thread Pool               │  │
│  │  ┌─────────────┐  ┌─────────────┐       │  │
│  │  │   Worker 0  │  │   Worker 1  │  ...  │  │
│  │  └──────┬──────┘  └──────┬──────┘       │  │
│  └─────────┼─────────────────┼──────────────┘  │
│            │                 │                  │
│  ┌─────────▼─────────────────▼──────────────┐  │
│  │           Hook Registry                  │  │
│  │     (Atomic Pointer Swapping)           │  │
│  └─────────┬─────────────────┬──────────────┘  │
└────────────┼─────────────────┼──────────────────┘
             │                 │
    ┌────────▼────────┐ ┌──────▼──────┐
    │  Tier Manager   │ │ CPU Timer   │
    └─────────────────┘ └─────────────┘
```

### Memory Layout

Task state allocation per worker thread:

```
Worker Thread Local Storage (per thread):
├── TaskBudget (16 bytes, cache-aligned)
│   ├── deadline_ns: u64 (8 bytes)
│   ├── remaining: u32 (4 bytes)
│   ├── tier: u8 (1 byte)
│   └── padding: [u8; 3] (3 bytes)
└── WorkerStats (64 bytes, cache-aligned)
    ├── polls_processed: u64
    ├── total_cpu_ns: u64
    ├── violations: u64
    └── ... (additional metrics)
```

### Tier Progression

```
Tier 0: Monitor
  │ (violations > threshold)
  ▼
Tier 1: Warn
  │ (continued violations)
  ▼
Tier 2: Yield
  │ (persistent behavior)
  ▼
Tier 3: Isolate
```

## Performance Characteristics

### Microbenchmarks

| Operation | Linux (x86_64) | Windows (x86_64) | macOS (aarch64) |
|-----------|----------------|------------------|-----------------|
| Budget check | 15ns | 18ns | 16ns |
| CPU time query | 15ns | 30ns | 40ns |
| Tier promotion | 45ns | 48ns | 46ns |
| Hook installation | 25ns | 28ns | 26ns |
| Full poll overhead | 48ns | 65ns | 72ns |

### Memory Overhead

| Component | Per-Task | Per-Worker | Global |
|-----------|----------|------------|--------|
| TaskBudget | 16 bytes | - | - |
| TaskState | 64 bytes | - | - |
| WorkerStats | - | 64 bytes | - |
| TierManager | - | - | ~1KB + 80 bytes/task |

### Scalability

- Lock-free operations up to 256 concurrent workers
- O(1) budget checks
- O(1) tier promotion
- O(log n) slow task queue operations

## Configuration

### Basic Setup

```rust
use tokio_pulse::{TierManager, TierConfig, HookRegistry};
use std::sync::Arc;

fn main() {
    let config = TierConfig::default();
    let manager = Arc::new(TierManager::new(config));
    let registry = HookRegistry::new();

    registry.set_hooks(manager as Arc<dyn PreemptionHooks>);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .build()
        .unwrap();

    runtime.block_on(async {
        // Application code
    });
}
```

### Advanced Configuration

```rust
let mut config = TierConfig {
    poll_budget: 2000,
    cpu_threshold_ns: 1_000_000,  // 1ms
    tier_policies: [
        TierPolicy {
            promotion_threshold: 3,
            demotion_threshold: 10,
            action: InterventionAction::Monitor,
        },
        TierPolicy {
            promotion_threshold: 2,
            demotion_threshold: 5,
            action: InterventionAction::Warn,
        },
        TierPolicy {
            promotion_threshold: 1,
            demotion_threshold: 3,
            action: InterventionAction::Yield,
        },
        TierPolicy {
            promotion_threshold: 0,
            demotion_threshold: 0,
            action: InterventionAction::Isolate,
        },
    ],
};
```

### Runtime Integration

For existing Tokio applications:

```rust
// Before
let runtime = tokio::runtime::Runtime::new()?;

// After
let runtime = tokio_pulse::runtime::Builder::new_multi_thread()
    .preemption_enabled(true)
    .tier_config(TierConfig::default())
    .build()?;
```

## Performance Tuning

### Linux-Specific Optimizations

1. **CPU Affinity**: Pin workers to specific cores
   ```bash
   taskset -c 0-3 ./your-app
   ```

2. **Huge Pages**: Enable transparent huge pages
   ```bash
   echo always > /sys/kernel/mm/transparent_hugepage/enabled
   ```

3. **Scheduler Tuning**: Use SCHED_FIFO for workers
   ```rust
   use libc::{sched_param, sched_setscheduler, SCHED_FIFO};
   ```

### Windows-Specific Optimizations

1. **Processor Groups**: Awareness for >64 core systems
2. **Timer Resolution**: Set to 1ms for accurate timing
   ```rust
   winapi::um::timeapi::timeBeginPeriod(1);
   ```

### macOS-Specific Optimizations

1. **Quality of Service**: Set appropriate QoS class
2. **Dispatch Integration**: Consider libdispatch for I/O

## Troubleshooting

### Common Issues

| Problem | Cause | Solution |
|---------|-------|----------|
| High overhead | Debug build | Use `--release` |
| Tasks not yielding | Budget too high | Reduce `poll_budget` |
| Excessive yields | Budget too low | Increase `poll_budget` |
| Timer inaccuracy | Power saving | Disable CPU frequency scaling |

### Debugging

Enable detailed tracing:

```bash
RUST_LOG=tokio_pulse=trace cargo run
```

Use tokio-console for visualization:

```rust
#[cfg(feature = "console")]
console_subscriber::init();
```

## Benchmarking

### Running Benchmarks

```bash
# All benchmarks
cargo bench

# Specific benchmark
cargo bench --bench cpu_timing

# With profiling
cargo bench --bench tier_manager -- --profile-time=10
```

### Interpreting Results

Benchmark output format:
```
budget_check/consume    time:   [15.234 ns 15.456 ns 15.689 ns]
                        change: [-0.5234% +0.1234% +0.8765%] (p = 0.23)
```

Key metrics:
- Median time (middle value)
- 95% confidence interval
- Regression detection

## Development

### Project Structure

```
tokio-pulse/
├── tokio-preempt/
│   └── tokio-pulse/
│       ├── src/
│       │   ├── budget.rs         # Budget management
│       │   ├── tier_manager.rs   # Tier coordination
│       │   ├── hooks.rs          # Runtime integration
│       │   └── timing/           # Platform-specific timers
│       │       ├── mod.rs
│       │       ├── linux.rs
│       │       ├── windows.rs
│       │       └── macos.rs
│       ├── benches/
│       └── tests/
└── tokio-fork/                   # Modified runtime (submodule)
```

### Testing

```bash
# Unit tests
cargo test --lib

# Integration tests
cargo test --test '*'

# Doctest
cargo test --doc

# Property tests
cargo test --features proptest
```

### Code Quality

```bash
# Linting
cargo clippy --all-targets --all-features -- -D warnings

# Formatting
cargo fmt --all -- --check

# Security audit
cargo audit

# Dependency check
cargo outdated
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for:
- Code style guidelines
- Commit message format
- Pull request process
- Performance regression policy

## References

### Design Inspiration
- [Erlang Scheduler](https://www.erlang.org/doc/efficiency_guide/processes.html): Reduction-based preemption
- [Go Runtime](https://golang.org/src/runtime/proc.go): Signal-based preemption
- [Linux CFS](https://www.kernel.org/doc/html/latest/scheduler/sched-design-CFS.html): Fairness algorithms

### Related Issues
- [tokio#6315](https://github.com/tokio-rs/tokio/issues/6315): Task starvation
- [tokio#4730](https://github.com/tokio-rs/tokio/issues/4730): CPU-bound tasks
- [async-std#841](https://github.com/async-rs/async-std/issues/841): Preemption discussion

### Academic Papers
- "Efficient Scalable Thread-Safety-Violation Detection" (SOSP '19)
- "The Problem of Mutual Exclusion" (Dijkstra, 1965)
- "Time, Clocks, and the Ordering of Events" (Lamport, 1978)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.