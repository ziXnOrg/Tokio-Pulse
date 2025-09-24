# Tokio-Pulse Preemption System

## Project Overview
A production-ready preemption system for Tokio's async runtime, solving task starvation issues where CPU-bound tasks monopolize worker threads. This project implements graduated intervention with <100ns overhead per poll operation.

## Tech Stack
- Language: Rust 1.75+ (MSRV)
- Runtime: Tokio 1.x (forked for minimal hooks)
- Concurrency: crossbeam, dashmap, atomic operations
- Testing: criterion, proptest, tokio-test
- Metrics: metrics-rs, tokio-metrics
- Tracing: tracing, console-subscriber

## Architecture
The system uses a hybrid multi-tier approach:
1. **Minimal Core Hooks**: ~50-100ns overhead in Tokio scheduler
2. **External TierManager**: Graduated intervention (Monitor→Warn→Yield→Isolate)
3. **Compile-time Macros**: #[preemption_budget] for instrumentation
4. **Cross-platform CPU timing**: Linux/Windows/macOS with fallback

## Project Structure
```
tokio-pulse/
├── tokio-pulse/             # Main library crate
│   ├── src/
│   │   ├── lib.rs           # Public API surface
│   │   ├── budget.rs        # TaskBudget (16-byte aligned)
│   │   ├── tier_manager.rs  # Multi-tier intervention
│   │   ├── timing/          # Cross-platform CPU time
│   │   └── metrics.rs       # Performance monitoring
│   ├── tests/               # Integration tests
│   └── benches/             # Criterion benchmarks
├── tokio-pulse-macros/      # Procedural macros
└── tokio/ (fork)            # Modified Tokio with hooks
```

## Key Files & Concepts
- `TaskBudget`: 16-byte cache-aligned struct with atomic fields (remaining, deadline_ns, tier)
- `TierManager`: DashMap-based task state tracking with SegQueue for slow tasks
- `PreemptionHooks`: Minimal interface added to Tokio (before_poll, after_poll, on_yield)
- CPU timing: Platform-specific implementations with <50ns overhead

## Commands
```bash
# Development
cargo build --all-features           # Build with all features
cargo test --all-features            # Run all tests
cargo bench                          # Run Criterion benchmarks
cargo clippy -- -D warnings          # Lint with pedantic settings
cargo doc --all-features --no-deps   # Generate documentation

# Testing
cargo test --doc                     # Test documentation examples
cargo tarpaulin                      # Generate coverage report
cargo test --features tokio_unstable # Test with unstable features

# Security & Quality
cargo audit                          # Check for vulnerabilities
cargo fmt -- --check                 # Check formatting
cargo geiger                         # Audit unsafe usage
```

## Code Style Guidelines
- **Memory Layout**: Align TaskBudget to 16 bytes for cache optimization
- **Atomics**: Use Relaxed ordering for budget ops, Acquire/Release for tier changes
- **Performance**: All operations must be <100ns; context switches ~1.5μs
- **Safety**: Forbid unsafe in public APIs; document all invariants
- **Testing**: Maintain >95% coverage; property tests for concurrency
- **Documentation**: Every public item needs examples that compile

## Development Methodology
- **Incremental Development**: Break every feature into smallest verifiable units
- **Research First**: Verify design decisions against research papers before coding
- **Documentation Driven**: Write docs/comments before implementation
- **Continuous Verification**: Run clippy, fmt, and tests after each small change
- **No Code Duplication**: Extract common patterns immediately
- **Benchmark Everything**: Measure performance impact of every change
- **Review Checkpoints**: Self-review code every 50-100 lines
- **Methodical Approach**: Plan thoroughly, implement carefully, verify constantly

## Performance Requirements
- Per-poll overhead: <100ns (ideally <50ns)
- Budget check: <20ns atomic operation
- CPU time measurement: <50ns per call
- Memory footprint: 16 bytes per task
- Zero overhead when disabled via null hooks

## Platform-Specific Notes
- **Linux**: clock_gettime(CLOCK_THREAD_CPUTIME_ID) for nanosecond CPU time
- **Windows**: QueryThreadCycleTime with frequency conversion
- **macOS**: thread_info with THREAD_BASIC_INFO
- **Fallback**: Instant::now() when precise timers unavailable

## Testing Strategy
1. **Unit Tests**: Co-located with modules for private access
2. **Integration Tests**: Multi-threaded scenarios in tests/
3. **Property Tests**: proptest for invariant verification
4. **Benchmarks**: Criterion with regression detection
5. **Cross-platform CI**: Linux/Windows/macOS on stable/beta/nightly

## Critical Design Decisions
- External library first, minimal Tokio changes
- Graduated intervention to avoid false positives
- Lock-free data structures for scalability
- Opt-in via feature flags for zero-cost abstraction
- Per-worker thread-local state to avoid contention

## Do NOT
- Do not use signals for preemption (unsafe in Rust)
- Do not modify task stacks or program counters
- Do not add >100ns overhead to well-behaved tasks
- Do not use global locks in hot paths
- Do not panic in production code (use Result)
- Do not commit directly to tokio fork main branch

## Common Workflows

### Adding a new tier intervention
1. Update `InterventionAction` enum in tier_manager.rs
2. Implement handler in `TierManager::apply_intervention()`
3. Add metrics counter for new intervention type
4. Write integration test for tier promotion
5. Update benchmarks to measure overhead

### Implementing platform support
1. Add new module in src/timing/
2. Use cfg attributes for conditional compilation
3. Implement `CpuTimer` trait
4. Add calibration for overhead compensation
5. Test on target platform CI

### Debugging slow tasks
1. Enable tracing feature
2. Use tokio-console to visualize task tiers
3. Check metrics for budget_exhaustion_count
4. Review slow_queue_size metrics
5. Examine tier promotion patterns

## References
- Issue: tokio-rs/tokio#6315 (Task starvation problem)
- BEAM: 4000 reductions before yield
- Go: SIGURG after 10ms, but unsafe for Rust
- Research: All PDFs in tokio-pulse/ directory

## Contact & Status
- Status: Phase I Implementation (Foundation & Core)
- Current Focus: TaskBudget and cross-platform timing
- Next: TierManager and Tokio fork hooks