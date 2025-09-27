/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */

# Kubernetes CPU Manager Implementation Research

## Executive Summary

This document captures comprehensive research on Kubernetes CPU Manager, Topology Manager, and CPU resource control mechanisms. Key findings reveal sophisticated NUMA-aware scheduling, CFS quota throttling issues, and lessons applicable to Tokio-Pulse's tier-based intervention system.

## CPU Manager Architecture

### Core Components

The Kubernetes CPU Manager is implemented in the kubelet and manages CPU resources at the node level through integration with Linux cgroups and cpusets.

#### Policy Types

1. **None Policy** (Default)
   - No CPU affinity constraints
   - CFS shares for all containers
   - Suitable for general workloads

2. **Static Policy**
   - Exclusive CPU assignment for Guaranteed QoS pods
   - Integer CPU requests get dedicated cores
   - CPUs removed from shared pool
   - Ideal for latency-sensitive workloads

### Guaranteed QoS Requirements

For exclusive CPU allocation, pods must meet strict criteria:

```yaml
apiVersion: v1
kind: Pod
spec:
  containers:
  - name: app
    resources:
      requests:
        memory: "1Gi"
        cpu: "2"          # Integer CPU count
      limits:
        memory: "1Gi"     # Must equal requests
        cpu: "2"          # Must equal requests
```

**Key Requirements**:
- `resources.requests.cpu` == `resources.limits.cpu`
- `resources.requests.memory` == `resources.limits.memory`
- CPU must be integer value ≥ 1

### CPU Pool Management

```
Total CPUs: 16
├── Reserved System CPUs: 2 (0-1)
├── Kube-Reserved: 2 (2-3)
└── Allocatable: 12 (4-15)
    ├── Exclusive CPUs: 8 (4-11) [Guaranteed pods]
    └── Shared Pool: 4 (12-15) [BestEffort/Burstable]
```

## Topology Manager

### NUMA-Aware Resource Allocation

The Topology Manager coordinates CPU, memory, and device allocation to optimize NUMA locality.

#### Topology Policies

1. **None**: No topology constraints
2. **BestEffort**: Prefer NUMA alignment, allow fallback
3. **Restricted**: Require preferred NUMA alignment
4. **SingleNumaNode**: Strict single NUMA node allocation

#### Hint Protocol

```go
// Simplified hint generation
type TopologyHint struct {
    NUMANodeAffinity bitmask
    Preferred        bool
}

// CPU Manager generates hints
func (cm *cpuManager) GetTopologyHints(pod *Pod) []TopologyHint {
    availableCPUs := cm.getAvailableCPUs()
    hints := []TopologyHint{}

    for _, numa := range cm.topology.NUMANodes {
        if numa.HasSufficientCPUs(pod.CPURequest) {
            hints = append(hints, TopologyHint{
                NUMANodeAffinity: numa.Mask,
                Preferred: true,
            })
        }
    }
    return hints
}
```

### Memory Manager Integration

Static Memory Manager policy for Guaranteed pods:
- Allocates memory from same NUMA node as CPUs
- Returns topology hints for memory placement
- Coordinates with Topology Manager for alignment

## CFS Quota and Throttling

### The Throttling Problem

CFS quota enforcement creates unexpected performance issues:

#### How CFS Quota Works
```
cpu.cfs_period_us = 100000 (100ms)
cpu.cfs_quota_us = 50000 (50ms for 0.5 CPU)

Reality:
- Container uses 50ms in first 15ms of period
- Throttled for remaining 85ms
- Node has idle CPU but container blocked
```

#### Performance Impact
- **Latency spikes**: 50ms → 500ms during throttling
- **Bursty workloads**: Severely impacted
- **Microservices**: Request cascading failures
- **Database queries**: Timeout chains

### Known Issues

1. **Kernel Bug**: CFS quota bug causes unnecessary throttling
2. **Multi-threaded apps**: All threads throttled together
3. **Idle CPU paradox**: Throttling despite available capacity
4. **Period misalignment**: 100ms periods don't match workload patterns

### Monitoring Throttling

Key metrics to track:
```prometheus
# Throttling rate
rate(container_cpu_cfs_throttled_periods_total[5m]) /
rate(container_cpu_cfs_periods_total[5m])

# Time spent throttled
rate(container_cpu_cfs_throttled_seconds_total[5m])

# Number of throttles
container_cpu_cfs_throttled_periods_total
```

## CPU Burst Feature

### Adaptive Quota Adjustment

Koordinator's CPU Burst dynamically adjusts CFS quota:

```go
func adjustCFSQuota(container *Container) {
    throttleRate := getThrottleRate(container)

    if throttleRate > threshold {
        // Temporarily increase quota
        newQuota := container.quota * burstFactor
        setCFSQuota(container, newQuota)

        // Schedule reset after burst period
        scheduleQuotaReset(container, burstDuration)
    }
}
```

### Burst Configuration
- **Burst ratio**: 1.5-3x normal quota
- **Burst duration**: 100-500ms
- **Cooldown period**: Prevent continuous bursting
- **Auto-detection**: Based on throttle metrics

## Reserved System Resources

### System Reserved
```bash
--system-reserved=cpu=2,memory=2Gi
--system-reserved-cgroup=/system.slice
```

### Kube Reserved
```bash
--kube-reserved=cpu=2,memory=2Gi
--kube-reserved-cgroup=/podruntime.slice
```

### Reserved System CPUs
```bash
--reserved-system-cpus=0-3
```

Used for:
- OS system daemons
- Interrupt handlers
- Kernel threads
- kubelet and container runtime

## Implementation Details

### CPUSet Controller

```go
// Simplified CPUSet management
type cpuSetManager struct {
    mu              sync.Mutex
    defaultCPUSet   cpuset.CPUSet
    assignedCPUs    map[string]cpuset.CPUSet
}

func (m *cpuSetManager) Allocate(podUID string, numCPUs int) error {
    m.mu.Lock()
    defer m.mu.Unlock()

    available := m.defaultCPUSet.Size()
    if available < numCPUs {
        return fmt.Errorf("insufficient CPUs")
    }

    // Select CPUs from same socket/NUMA node
    allocated := m.selectCPUs(numCPUs)
    m.assignedCPUs[podUID] = allocated
    m.defaultCPUSet = m.defaultCPUSet.Difference(allocated)

    // Update cgroup cpuset
    return m.updateCGroupCPUSet(podUID, allocated)
}
```

### CGroup Integration

```go
func updateCGroupCPUSet(podUID string, cpus cpuset.CPUSet) error {
    cgroupPath := fmt.Sprintf("/sys/fs/cgroup/cpuset/kubepods/%s", podUID)

    // Write CPU list
    cpuList := cpus.String() // e.g., "4-7,12-15"
    err := ioutil.WriteFile(
        filepath.Join(cgroupPath, "cpuset.cpus"),
        []byte(cpuList),
        0644,
    )

    // Set exclusive flag (if supported)
    ioutil.WriteFile(
        filepath.Join(cgroupPath, "cpuset.cpu_exclusive"),
        []byte("1"),
        0644,
    )

    return err
}
```

## Lessons for Tokio-Pulse

### 1. Avoid Hard Quotas
- CFS quotas cause throttling even with idle CPU
- Better to use shares/weights for soft limits
- Hard limits only for truly disruptive tasks

### 2. NUMA Awareness Critical
- Memory/CPU locality impacts performance
- Need topology detection in TierManager
- Align task placement with memory allocation

### 3. Burst Handling
```rust
impl TierManager {
    fn handle_burst(&mut self, task_id: TaskId) {
        let throttle_rate = self.get_throttle_rate(task_id);

        if throttle_rate > 0.1 {  // >10% throttling
            // Temporarily increase budget
            self.apply_burst_budget(task_id);

            // Schedule reset
            self.schedule_budget_reset(task_id, Duration::from_millis(200));
        }
    }
}
```

### 4. Monitoring Integration
```rust
pub struct ThrottleMetrics {
    pub periods_total: Counter,
    pub throttled_periods: Counter,
    pub throttled_time_ns: Histogram,
    pub burst_activations: Counter,
}

impl ThrottleMetrics {
    pub fn throttle_rate(&self) -> f64 {
        self.throttled_periods.value() / self.periods_total.value()
    }
}
```

### 5. Resource Reservation
```rust
pub struct SystemReservation {
    reserved_workers: Vec<WorkerId>,
    shared_pool: Vec<WorkerId>,
    exclusive_assignments: HashMap<TaskId, Vec<WorkerId>>,
}

impl SystemReservation {
    pub fn reserve_for_system(&mut self, count: usize) {
        // Reserve first N workers for system tasks
        self.reserved_workers = (0..count)
            .map(WorkerId::from)
            .collect();
    }
}
```

## Best Practices from Kubernetes

### 1. CPU Allocation Strategy
- **Requests without limits**: Prevent throttling
- **Integer CPU counts**: For predictable performance
- **NUMA alignment**: Co-locate CPU and memory

### 2. Monitoring and Alerting
- Track throttle rate continuously
- Alert on >5% throttling
- Monitor burst frequency
- Measure scheduling latency

### 3. Testing Considerations
- Test with various CPU counts
- Simulate NUMA systems
- Measure throttling under load
- Verify burst behavior

### 4. Documentation Requirements
- Clear QoS class requirements
- Throttling behavior explanation
- NUMA impact on performance
- Troubleshooting guide

## Performance Implications

### Throttling Cost
- **Context switch**: 1-3μs per throttle
- **Cache invalidation**: 10-100μs impact
- **Lock contention**: Cascading delays
- **Total impact**: 10-1000x latency increase

### NUMA Penalties
- **Remote memory access**: 2-3x slower
- **Cross-socket communication**: 100-200ns overhead
- **Cache coherency**: Additional latency
- **Bandwidth limitation**: Reduced throughput

### Mitigation Strategies
1. **Tier-based quotas**: Gradual degradation
2. **Burst allowance**: Handle spikes gracefully
3. **NUMA pinning**: Optimize memory locality
4. **Adaptive budgets**: Learn from patterns

## Conclusion

Kubernetes CPU Manager provides sophisticated resource management with key insights:

**Successes**:
1. NUMA-aware allocation via Topology Manager
2. Exclusive CPU assignment for guaranteed workloads
3. Hierarchical resource reservation
4. Integration with Linux kernel features

**Challenges**:
1. CFS quota throttling causes performance issues
2. Scheduler lacks topology awareness
3. Complex configuration requirements
4. Debugging throttling is difficult

**For Tokio-Pulse**:
1. Implement burst handling for Tier 1-2 tasks
2. Avoid hard CPU quotas; use priority/weight instead
3. Add NUMA awareness to TierManager
4. Monitor throttling metrics continuously
5. Provide clear documentation on tier behavior

The Kubernetes experience validates our tier-based approach while highlighting the importance of avoiding hard quotas and implementing burst tolerance.