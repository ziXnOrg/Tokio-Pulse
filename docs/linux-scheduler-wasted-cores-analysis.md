# Linux Scheduler: A Decade of Wasted Cores - Analysis

## Paper Summary

**Title**: The Linux Scheduler: A Decade of Wasted Cores
**Authors**: Jean-Pierre Lozi, Baptiste Lepers, Justin Funston, Fabien Gaud, Vivien Quéma, Alexandra Fedorova
**Venue**: EuroSys 2016
**Repository**: https://github.com/jplozi/wastedcores

## Key Discovery

The fundamental scheduler invariant - **"ready threads must be scheduled on available cores"** - is frequently violated in Linux, with cores remaining idle for seconds while runnable threads wait in other runqueues.

## Performance Impact

### Measured Degradation
- **Scientific applications**: Many-fold performance degradation (synchronization-heavy workloads)
- **Kernel compilation**: 13% higher latency for kernel make
- **Database (TPC-H)**: 14-23% throughput decrease
- **Query performance**: Most affected query slowed by 23%

### Detection Challenges
- Symptoms last only ~100ms, invisible to htop/sar/perf
- No crashes or hangs, just silent performance loss
- Standard monitoring tools inadequate

## Four Core Scheduling Bugs

### 1. Load Balancing Group Imbalance Bug

**Symptom**: Cores remain idle while other cores are overloaded
**Cause**: Hierarchical load balancing groups can have skewed load calculations
**Impact**: Some cores never receive migrated tasks despite being idle

**Relevance to Tokio-Pulse**:
- Similar issue could occur with work-stealing executors
- Need to monitor per-worker thread utilization
- TierManager should detect persistent imbalances

### 2. Scheduling Group Construction Bug

**Symptom**: Asymmetric scheduling groups on NUMA systems
**Cause**: Incorrect topology detection leads to unbalanced groups
**Impact**: Some cores systematically underutilized

**Relevance to Tokio-Pulse**:
- NUMA awareness critical for large systems
- Worker thread placement matters
- Consider topology in tier decisions

### 3. Overload-on-Wakeup Bug

**Symptom**: Tasks keep waking on busy cores while others idle
**Cause**: Wake affinity heuristics too aggressive
**Impact**: Cores idle for extended periods (seconds)

**Relevance to Tokio-Pulse**:
- Most relevant to async runtime behavior
- Tasks may exhibit wake affinity patterns
- Need to detect and break pathological wake chains

### 4. Missing Scheduling Domain Bug

**Symptom**: Load balancing completely disabled between some cores
**Cause**: Topology changes not properly propagated
**Impact**: Permanent idle cores alongside overloaded ones

**Relevance to Tokio-Pulse**:
- Runtime topology changes (CPU hotplug)
- Container CPU set modifications
- Need dynamic reconfiguration support

## Detection Methodology

### Online Invariant Checking
```
For each scheduling tick:
1. Check if any core is idle
2. Check if any runqueue has waiting threads
3. If both true → invariant violated
4. Record duration and frequency
```

### Visualization Tools
- Scheduling activity timeline
- Per-core utilization heatmap
- Runqueue depth over time
- Migration event tracking

### Key Metrics
- **Idle time with pending work**: Should be <1ms
- **Load imbalance factor**: Max/min runqueue depth
- **Migration frequency**: Should balance without thrashing
- **Wake affinity ratio**: Local vs remote wakeups

## Lessons for Tokio-Pulse

### 1. Invariant Monitoring

**Core Invariant**: No worker thread should idle while tasks are pending

```rust
fn check_scheduling_invariant(&self) -> bool {
    let idle_workers = self.count_idle_workers();
    let pending_tasks = self.count_pending_tasks();

    if idle_workers > 0 && pending_tasks > 0 {
        self.record_invariant_violation();
        false
    } else {
        true
    }
}
```

### 2. Work-Stealing Pathologies

**Problem**: Work-stealing can create similar patterns to Linux bugs
- Tasks clustered on subset of workers
- Stealing disabled by heuristics
- Wake affinity creating hotspots

**Solution**: TierManager intervention
```rust
fn detect_work_imbalance(&self) -> Option<ImbalanceType> {
    let worker_loads = self.get_worker_loads();
    let cv = coefficient_of_variation(&worker_loads);

    if cv > 0.5 {  // Significant imbalance
        Some(ImbalanceType::LoadImbalance {
            overloaded: self.find_overloaded_workers(),
            underloaded: self.find_idle_workers(),
        })
    } else {
        None
    }
}
```

### 3. CPU-Bound Task Detection

**Pattern Recognition**: Similar to kernel make workload
- High CPU utilization
- Minimal voluntary yields
- Long poll durations

**Detection Strategy**:
```rust
fn classify_task_behavior(&self, task_id: TaskId) -> TaskType {
    let stats = self.get_task_stats(task_id);

    match (stats.avg_poll_time, stats.yield_rate) {
        (t, _) if t > 10_000_000 => TaskType::CpuBound,     // >10ms polls
        (t, r) if t > 1_000_000 && r < 0.1 => TaskType::Mixed,
        _ => TaskType::IoBoUnd,
    }
}
```

### 4. Intervention Triggers

Based on paper's findings, trigger intervention when:

1. **Idle Detection**: Worker idle >1ms with pending work
2. **Imbalance Detection**: Load CV >0.5 across workers
3. **Monopolization**: Single task >50% of worker time
4. **Starvation**: Task waiting >100ms in queue

### 5. Measurement Overhead

**Paper's Approach**: Negligible overhead through:
- Sampling (check every N polls)
- Per-CPU data structures
- Lock-free statistics collection
- Lazy aggregation

**Tokio-Pulse Implementation**:
```rust
// Sample-based monitoring
if self.poll_count.fetch_add(1, Ordering::Relaxed) % SAMPLE_RATE == 0 {
    self.check_invariants();
}
```

## Implementation Recommendations

### Phase 1: Detection
- [ ] Implement scheduling invariant checker
- [ ] Add per-worker utilization tracking
- [ ] Detect work-stealing failures
- [ ] Monitor task wait times

### Phase 2: Analysis
- [ ] Classify task behavior patterns
- [ ] Identify pathological wake chains
- [ ] Detect load imbalance patterns
- [ ] Track intervention effectiveness

### Phase 3: Intervention
- [ ] Force task migration for balance
- [ ] Break wake affinity chains
- [ ] Adjust work-stealing parameters
- [ ] Apply CPU isolation for monopolizers

### Phase 4: Validation
- [ ] Reproduce paper's workloads
- [ ] Measure intervention overhead
- [ ] Validate invariant maintenance
- [ ] Benchmark under contention

## Key Insights

1. **Silent Performance Loss**: Most scheduling bugs don't crash, they silently degrade performance

2. **Invariant-Based Detection**: Simple invariants catch complex bugs

3. **Measurement Granularity**: Millisecond-level monitoring essential

4. **Heuristic Failures**: Well-intentioned optimizations (wake affinity) can backfire

5. **Topology Awareness**: Physical layout affects logical scheduling

## Metrics to Track

```rust
pub struct SchedulerHealthMetrics {
    // Invariant violations
    pub idle_with_pending_work: Counter,
    pub violation_duration_ns: Histogram,

    // Load balance
    pub worker_load_cv: Gauge,
    pub migration_count: Counter,
    pub failed_steals: Counter,

    // Task behavior
    pub cpu_bound_tasks: Gauge,
    pub avg_poll_duration_ns: Histogram,
    pub task_wait_time_ns: Histogram,

    // Intervention
    pub tier_promotions: Counter,
    pub forced_yields: Counter,
    pub task_migrations: Counter,
}
```

## Conclusion

The "Wasted Cores" paper reveals that even mature schedulers violate basic invariants, causing severe performance degradation. For Tokio-Pulse:

1. **Monitor invariants continuously** - Idle workers + pending work = bug
2. **Detect imbalance patterns** - Work-stealing isn't perfect
3. **Intervene proactively** - Don't wait for user complaints
4. **Measure with precision** - Millisecond granularity minimum
5. **Learn from pathologies** - Wake affinity, NUMA effects matter

The paper's tools and methodology provide a blueprint for detecting and fixing scheduling inefficiencies in async runtimes.