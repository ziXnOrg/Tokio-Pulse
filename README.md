# Tokio-Pulse

 preemption system for Tokio's async runtime developed to address task starvation issues where CPU-bound tasks monopolize worker threads. The library provides graduated intervention with measured overhead below 100 nanoseconds per poll operation.

## Overview

multi-tier intervention system that monitors task execution and applies escalating interventions to prevent monopolization of worker threads. The system operates through four tiers: Monitor, Warn, Yield, and Isolate, with configurable thresholds and policies.

## Supported Platforms

support for the following platforms with optimized CPU timing implementations:

- **Linux**: x86-64, utilizing clock_gettime(CLOCK_THREAD_CPUTIME_ID) for nanosecond precision CPU timing
- **Windows**: x86-64, utilizing QueryThreadCycleTime with frequency conversion
- **macOS**: x86-64 and ARM64, utilizing thread_info with THREAD_BASIC_INFO
- **Other platforms**: Fallback implementation using std::time::Instant

## System Requirements

- Rust 1.75 or later (MSRV)
- Tokio 1.41 or later
- Platform-specific dependencies automatically resolved via conditional compilation

## Installation

Tokio-Pulse is organized as a Cargo workspace. To build from source:

```bash
git clone https://github.com/ziXnOrg/Tokio-Pulse.git
cd Tokio-Pulse
cargo build --release
```

## Architecture

The system consists of several interconnected components:

- **TaskBudget**: 16-byte cache-aligned structures for atomic budget tracking
- **TierManager**: Multi-tier intervention system with configurable policies
- **HookRegistry**: Runtime instrumentation interface for task monitoring
- **CPU Timing**: Cross-platform high-precision CPU time measurement
- **Worker Statistics**: Per-worker thread performance metrics with lock-free updates

## Performance Characteristics

Measured performance on x86-64 systems:

- Task budget operations: 8-20 nanoseconds
- CPU time measurement: 25-75 nanoseconds (platform dependent)
- Tier evaluation: 40-80 nanoseconds
- Hook overhead per poll: 50-95 nanoseconds
- Memory footprint: 16 bytes per active task

## API Usage

### Basic Configuration

```rust
use tokio_pulse::{TierManager, TierConfig, HookRegistry};
use std::sync::Arc;

let mut config = TierConfig::default();
config.poll_budget = 1000; // Microseconds
config.tier_policies[0].promotion_threshold = 5;

let manager = Arc::new(TierManager::new(config));
let registry = HookRegistry::new();
registry.set_hooks(manager.clone());
```

### CPU Timing Example

```rust
use tokio_pulse::timing::create_cpu_timer;

let timer = create_cpu_timer();
println!("Timer: {}", timer.platform_name());
println!("Overhead: {} ns", timer.calibrated_overhead_ns());

let start = timer.thread_cpu_time_ns().unwrap();
// Perform CPU-intensive work
let elapsed = timer.thread_cpu_time_ns().unwrap() - start;
```

### Worker Statistics

```rust
let worker_metrics = manager.get_worker_metrics(0);
println!("Worker 0: {} polls, {} violations",
         worker_metrics.polls, worker_metrics.violations);

let all_metrics = manager.get_all_worker_metrics();
for (worker_id, metrics) in all_metrics {
    println!("Worker {}: {} tasks active", worker_id, metrics.tasks);
}
```

## Building

build process:

```bash
cargo build --release          # Optimized build
cargo test --all-features      # Complete test suite
cargo bench                    # Performance benchmarks
cargo doc --no-deps --open     # API documentation
```

## Benchmarking

benchmarks:

```bash
cargo bench --bench cpu_timing     # CPU timing implementations
cargo bench --bench budget         # TaskBudget operations
cargo bench --bench tier_manager   # TierManager performance
cargo bench --bench hooks          # Hook system overhead
cargo bench --bench worker_stats   # Worker statistics performance
```

## Testing

test categories:

- Unit tests: `cargo test --lib`
- Integration tests: `cargo test --test '*'`
- Property-based tests: `cargo test tier_manager_properties`
- Stress tests: `cargo test stress_tests`
- Documentation tests: `cargo test --doc`

## License

Licensed under the MIT License. See LICENSE file for details.

## Support

For technical issues and contributions, please use the GitHub issue tracker at https://github.com/ziXnOrg/Tokio-Pulse.
