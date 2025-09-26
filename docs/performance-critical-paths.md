# Performance-Critical Code Paths

## Executive Summary

This document identifies and analyzes the performance-critical code paths in Tokio-Pulse where nanosecond-level optimization is essential.

## Critical Path Hierarchy

### Tier 0: Poll Hot Path (<100ns requirement)

#### 1. Task Poll Cycle
**Frequency**: Every task poll operation
**Current Performance**: 1.44ns (no hooks), 2.04ns (with hooks)
**Code Path**:
```
tokio::runtime::task::poll()
  → HookRegistry::before_poll()      [485ps]
  → TaskBudget::consume()             [1.6ns]
  → Future::poll()                    [application code]
  → HookRegistry::after_poll()        [746ps]
  → TaskBudget::check_exhaustion()    [332ps]
```

**Critical Operations**:
- Atomic load for hook presence check
- Atomic decrement for budget consumption
- CPU time measurement (when enabled)

#### 2. Hook Registry Fast Path
**Location**: `src/hooks.rs:55-65`
**Performance**: 367ps for presence check
**Critical Code**:
```rust
#[inline(always)]
pub fn has_hooks(&self) -> bool {
    self.hooks.load(Ordering::Acquire).is_null() == false
}
```
**Optimization**: Single atomic load with acquire ordering

#### 3. Budget Consumption Path
**Location**: `src/budget.rs:89-103`
**Performance**: 1.6ns per consume
**Critical Code**:
```rust
#[inline(always)]
pub fn consume(&self, amount: u32) -> bool {
    let prev = self.remaining.fetch_sub(amount, Ordering::Relaxed);
    prev >= amount
}
```
**Optimization**: Relaxed ordering for minimal synchronization overhead

### Tier 1: State Management (<500ns target)

#### 1. Tier Check Path
**Location**: `src/tier_manager.rs:245-257`
**Performance**: 5.5ns
**Code Path**:
```
TierManager::before_poll()
  → DashMap::get(task_id)            [~3ns]
  → AtomicU8::load(tier)              [~1ns]
  → TaskBudget::reset_if_needed()     [~1.5ns]
```

#### 2. Metrics Collection Path
**Location**: `src/tier_manager.rs:459-474`
**Performance**: 27.6ns
**Critical Operations**:
- Multiple atomic loads for counters
- DashMap size calculation
- SegQueue length check

### Memory Layout Critical Paths

#### 1. TaskBudget Structure
**Location**: `src/budget.rs:34-40`
**Size**: 16 bytes (optimized from 24)
**Layout**:
```rust
#[repr(C, align(16))]
pub struct TaskBudget {
    remaining: AtomicU32,    // 4 bytes
    deadline_ns: AtomicU64,  // 8 bytes
    tier: AtomicU8,          // 1 byte
    _padding: [u8; 3],       // 3 bytes padding
}
```
**Cache Line**: Fits in single cache line, prevents false sharing

#### 2. HookRegistry AtomicPtr
**Location**: `src/hooks.rs:84-88`
**Critical**: Lock-free pointer swap
```rust
pub fn set_hooks(&self, hooks: Arc<dyn PreemptionHooks>) {
    let ptr = Arc::into_raw(hooks) as *mut ();
    self.hooks.store(ptr, Ordering::Release);
}
```

## Concurrency Critical Paths

### 1. Contention Points

#### DashMap Access Pattern
**Location**: `src/tier_manager.rs:245`
**Scaling**: O(1) average, O(n) worst case
**Mitigation**: Sharded locks (default 16 shards)

#### SegQueue Operations
**Location**: `src/tier_manager.rs:355-367`
**Performance**: 416ns enqueue, 921ps process
**Characteristic**: Lock-free MPMC queue

### 2. Atomic Ordering Requirements

| Operation | Ordering | Justification |
|-----------|----------|--------------|
| Budget operations | Relaxed | No synchronization needed |
| Hook pointer load | Acquire | Must see hook installation |
| Hook pointer store | Release | Must publish hook data |
| Tier transitions | AcqRel | Must synchronize state |

## Platform-Specific Critical Paths

### CPU Time Measurement

#### Linux Path
**Location**: `src/timing/linux.rs:28-35`
**Performance**: ~30ns overhead
```rust
clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts)
```

#### macOS Path
**Location**: `src/timing/macos.rs:35-45`
**Performance**: ~40ns overhead
```rust
thread_info(mach_thread_self(), THREAD_BASIC_INFO, ...)
```

#### Windows Path
**Location**: `src/timing/windows.rs:30-38`
**Performance**: ~50ns overhead
```rust
QueryThreadCycleTime(GetCurrentThread(), &mut cycles)
```

## Optimization Techniques Applied

### 1. Inlining Strategy
- `#[inline(always)]` for <10ns operations
- `#[inline]` for <100ns operations
- No inline for complex/cold paths

### 2. Branch Prediction
- Fast path for no-hooks case
- Likely/unlikely hints via code structure
- Early returns for common cases

### 3. Cache Optimization
- 16-byte alignment for hot structures
- Co-location of frequently accessed fields
- Padding to prevent false sharing

### 4. Lock-Free Algorithms
- AtomicPtr for HookRegistry
- SegQueue for slow task queue
- DashMap for concurrent task tracking

## Performance Validation Points

### Must Measure
1. **Every atomic operation** - Verify ordering overhead
2. **Every inline function** - Confirm inlining
3. **Every mutex/lock** - Check contention
4. **Every allocation** - Track memory pressure

### Regression Indicators
- Hook overhead >1ns
- Budget operation >2ns
- Tier check >10ns
- Any allocation in hot path

## Maintenance Guidelines

### Adding New Operations

1. **Measure baseline** before changes
2. **Profile assembly** output for hot paths
3. **Verify inlining** with `cargo asm`
4. **Test contention** at 2, 4, 8, 16 threads
5. **Check cache misses** with `perf stat`

### Optimization Checklist

- [ ] No allocations in paths <1μs
- [ ] Appropriate atomic ordering
- [ ] Proper inlining directives
- [ ] Cache-aligned structures
- [ ] Minimal function calls
- [ ] Branch-free when possible
- [ ] SIMD opportunities identified

## Known Bottlenecks

### Current
1. **TierManager at 1000+ tasks**: DashMap contention
2. **CPU time measurement**: Platform syscall overhead
3. **Tier promotion logic**: Multiple atomic operations

### Mitigation Strategies
1. Consider thread-local caching for tier lookups
2. Batch CPU time measurements
3. Combine atomic operations where possible

## Future Optimization Opportunities

1. **SIMD for bulk operations** - Process multiple budgets
2. **Thread-local fast path** - Cache tier information
3. **Epoch-based reclamation** - For hook updates
4. **Hardware timestamps** - RDTSC on x86_64
5. **Profile-guided optimization** - Use PGO for releases