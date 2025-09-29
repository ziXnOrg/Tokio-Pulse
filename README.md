# Tokio-Pulse

Production-ready preemption system for Tokio's async runtime, solving task starvation issues where CPU-bound tasks monopolize worker threads.

## Overview

Tokio-Pulse implements graduated intervention with sub-100ns overhead per poll operation, providing:

- Multi-tier intervention system (Monitor → Warn → Yield → Isolate)
- Cross-platform CPU timing (Linux/Windows/macOS with fallback)
- Lock-free concurrent data structures for scalability
- Fair scheduling algorithms (Lottery Scheduling, Deficit Round Robin)
- Comprehensive metrics and observability

## Architecture

The system uses a hybrid approach with minimal core hooks in Tokio's scheduler and an external TierManager for graduated intervention. TaskBudget structures are 16-byte cache-aligned for optimal performance.

## Performance

- Per-poll overhead: <100ns (typically <50ns)
- Budget check: <20ns atomic operation
- CPU time measurement: <50ns per call
- Memory footprint: 16 bytes per task
- Zero overhead when disabled via null hooks

## Requirements

- Rust 1.75+ (MSRV)
- Tokio 1.x
- Platform support: Linux, Windows, macOS, with fallback for others

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
tokio-pulse = "0.1"
```

## Quick Start

```rust
use tokio_pulse::{TierManager, TierConfig, HookRegistry};
use std::sync::Arc;

// Create tier manager with default configuration
let manager = Arc::new(TierManager::new(TierConfig::default()));

// Install as preemption hooks
let registry = HookRegistry::new();
registry.set_hooks(manager.clone());

// Tasks are now automatically monitored for preemption
```

## Configuration

```rust
let mut config = TierConfig::default();
config.poll_budget = 1000;  // Microseconds per poll budget
config.tier_policies[0].promotion_threshold = 5;  // Violations before tier promotion

let manager = TierManager::new(config);
```

## Documentation

Run `cargo doc --open` to view the complete API documentation.

## Testing

```bash
cargo test --all-features     # Run all tests
cargo bench                   # Run performance benchmarks
cargo test --doc              # Test documentation examples
```

## Platform Support

- **Linux**: clock_gettime(CLOCK_THREAD_CPUTIME_ID) for nanosecond precision
- **Windows**: QueryThreadCycleTime with frequency conversion
- **macOS**: thread_info with THREAD_BASIC_INFO
- **Fallback**: Instant::now() when precise timers unavailable

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

Contributions are welcome. Please ensure all tests pass and follow the existing code style.