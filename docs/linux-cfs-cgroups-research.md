# Linux CFS and cgroups v2 Research

## Executive Summary

This document captures research findings on Linux CFS (Completely Fair Scheduler) and cgroups v2, essential for implementing OS-level task isolation in Tokio-Pulse.

## Linux CFS (Completely Fair Scheduler)

### Current Status (2024)
- **Important**: Linux 6.6+ replaced CFS with EEVDF scheduler
- CFS remains in kernels 2.6.23 through 6.5
- Most production systems still use CFS (RHEL 8/9, Ubuntu 22.04 LTS)

### Core Architecture

#### Virtual Runtime (vruntime)
```
Each task maintains vruntime (virtual runtime)
- Incremented when task runs
- Weighted by nice value
- Scheduler picks task with minimum vruntime
```

#### Red-Black Tree Structure
- Tasks sorted by vruntime in RB-tree
- O(log n) insertion/deletion
- Leftmost node = next task to run

#### Time Slice Calculation
```
time_slice = sched_period * task_weight / total_weight
Default sched_period = 6ms (for <=8 tasks)
```

### Scheduling Classes Hierarchy
1. **SCHED_FIFO/RR** - Real-time (highest priority)
2. **SCHED_NORMAL** - CFS managed (default)
3. **SCHED_BATCH** - CPU-intensive batch jobs
4. **SCHED_IDLE** - Lowest priority

### Key Parameters for Preemption

#### /proc/sys/kernel/sched_*
```bash
sched_min_granularity_ns    # Minimum preemption granularity (default: 3ms)
sched_wakeup_granularity_ns # Wake-up preemption granularity (default: 4ms)
sched_latency_ns            # Target preemption latency (default: 6ms)
sched_nr_latency            # Number of tasks for latency scaling (default: 8)
```

### CFS Group Scheduling
- **CONFIG_FAIR_GROUP_SCHED**: Enables task grouping
- CPU bandwidth control via cpu.cfs_quota_us/cpu.cfs_period_us
- Hierarchical scheduling with nested groups

## cgroups v2 Architecture

### Evolution from v1
- **Unified hierarchy**: Single mount point vs multiple in v1
- **Simplified interface**: Consistent file naming
- **Better resource delegation**: Improved container support
- **Thread-level control**: Since kernel 4.14

### CPU Controller (kernel 4.15+)

#### Interface Files
```
cpu.weight       # Proportional CPU distribution (1-10000, default 100)
cpu.weight.nice  # Nice value interface (-20 to 19)
cpu.max          # Hard bandwidth limit "max period"
cpu.stat         # Usage statistics
cpu.pressure     # PSI (Pressure Stall Information)
```

#### Bandwidth Control
```bash
# Limit to 50% CPU (50ms per 100ms period)
echo "50000 100000" > cpu.max

# Unlimited (default)
echo "max 100000" > cpu.max
```

### Implementation for Tokio-Pulse

#### Task Isolation Strategy

**Tier 3 (Isolate) Implementation**:
1. Create dedicated cgroup for misbehaving tasks
2. Apply cpu.max limits
3. Monitor via cpu.stat

**Directory Structure**:
```
/sys/fs/cgroup/
└── tokio-pulse/
    ├── normal/        # Tier 0-2 tasks
    └── isolated/      # Tier 3 tasks
        ├── task-123/  # Per-task isolation
        └── task-456/
```

#### System Calls Required

```c
// cgroup v2 operations
openat(AT_FDCWD, "/sys/fs/cgroup", O_RDONLY)
mkdirat(dirfd, "tokio-pulse/isolated/task-123", 0755)
openat(dirfd, "cgroup.procs", O_WRONLY)
write(fd, "12345\n", 6)  // Move PID to cgroup
```

### Permission Requirements

#### Privileged Operations
- Creating cgroups: Requires CAP_SYS_ADMIN or delegation
- Moving processes: Requires write access to cgroup.procs
- Setting limits: Requires write access to controller files

#### Unprivileged Delegation (kernel 4.18+)
```bash
# Delegate to user
echo "+cpu +memory" > /sys/fs/cgroup/user.slice/cgroup.subtree_control
chown -R user:user /sys/fs/cgroup/user.slice/user-1000.slice/
```

## Integration Points for Tokio-Pulse

### 1. Detection Phase
```rust
fn detect_cgroups_v2() -> bool {
    // Check if cgroup2 is mounted
    std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
}

fn check_cpu_controller() -> bool {
    // Verify CPU controller is available
    std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers")
        .map(|s| s.contains("cpu"))
        .unwrap_or(false)
}
```

### 2. Isolation Implementation
```rust
fn isolate_task(task_id: TaskId, cpu_quota_us: u64) -> Result<()> {
    let cgroup_path = format!("/sys/fs/cgroup/tokio-pulse/isolated/task-{}", task_id.0);

    // Create cgroup
    std::fs::create_dir_all(&cgroup_path)?;

    // Set CPU limit (50% = 50000/100000)
    std::fs::write(
        format!("{}/cpu.max", cgroup_path),
        format!("{} 100000", cpu_quota_us)
    )?;

    // Move current thread to cgroup
    std::fs::write(
        format!("{}/cgroup.procs", cgroup_path),
        format!("{}", std::process::id())
    )?;

    Ok(())
}
```

### 3. Monitoring
```rust
fn get_task_cpu_usage(task_id: TaskId) -> Result<CpuStats> {
    let stat_path = format!("/sys/fs/cgroup/tokio-pulse/isolated/task-{}/cpu.stat", task_id.0);
    let content = std::fs::read_to_string(stat_path)?;

    // Parse: usage_usec, user_usec, system_usec
    parse_cpu_stat(&content)
}
```

## Container Compatibility

### Docker/Podman
- Nested cgroups work if `--privileged` or proper capabilities
- Need to detect container environment
- May conflict with container's resource limits

### Kubernetes
- Pod security policies may block cgroup access
- CPU manager static policy may interfere
- Consider using pod QoS classes instead

### Detection
```rust
fn in_container() -> bool {
    // Check for /.dockerenv or /run/.containerenv
    std::path::Path::new("/.dockerenv").exists() ||
    std::path::Path::new("/run/.containerenv").exists() ||
    // Check cgroup path includes docker/containerd
    std::fs::read_to_string("/proc/self/cgroup")
        .map(|s| s.contains("docker") || s.contains("containerd"))
        .unwrap_or(false)
}
```

## Performance Considerations

### Overhead Analysis
- cgroup operations: ~10-100μs (filesystem operations)
- Process migration: ~50-500μs (depends on memory)
- Stat reading: ~1-10μs (cached in kernel)

### Optimization Strategies
1. **Batch operations**: Move multiple tasks together
2. **Lazy creation**: Create cgroups on first isolation
3. **Caching**: Cache cgroup file descriptors
4. **Async operations**: Use io_uring for cgroup operations

## Security Considerations

### Threats
1. **Privilege escalation**: Improper cgroup permissions
2. **DoS via cgroup creation**: Unbounded cgroup creation
3. **Information leakage**: cpu.stat exposes usage patterns

### Mitigations
1. **Capability dropping**: Drop CAP_SYS_ADMIN after setup
2. **Resource limits**: Limit number of isolated tasks
3. **Sanitization**: Validate all paths and values
4. **Monitoring**: Track cgroup operations in audit log

## Platform-Specific Notes

### RHEL/CentOS 8+
- cgroups v2 enabled by default
- systemd integration via slice units
- SELinux contexts must be configured

### Ubuntu 20.04+
- Hybrid mode by default (v1 and v2)
- Pure v2 via `systemd.unified_cgroup_hierarchy=1`
- AppArmor profiles may need adjustment

### Container Runtimes
- Docker 20.10+: Full cgroups v2 support
- Podman: Native cgroups v2
- containerd 1.4+: cgroups v2 support

## Implementation Roadmap

### Phase 1: Detection & Compatibility
- [ ] Detect cgroups v2 availability
- [ ] Check CPU controller presence
- [ ] Verify permissions
- [ ] Container environment detection

### Phase 2: Basic Isolation
- [ ] Create tokio-pulse cgroup hierarchy
- [ ] Implement task migration
- [ ] Set CPU bandwidth limits
- [ ] Basic monitoring via cpu.stat

### Phase 3: Advanced Features
- [ ] Memory isolation (memory controller)
- [ ] I/O isolation (io controller)
- [ ] PSI (Pressure Stall Information) monitoring
- [ ] Dynamic limit adjustment

### Phase 4: Production Hardening
- [ ] Permission dropping
- [ ] Resource limit enforcement
- [ ] Audit logging
- [ ] Performance optimization

## References

1. **Linux Kernel Documentation**
   - https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html
   - https://docs.kernel.org/scheduler/sched-design-CFS.html

2. **Red Hat Documentation**
   - RHEL 8 cgroups v2 guide
   - systemd resource control

3. **Research Papers**
   - "Towards achieving fairness in the Linux scheduler" (ACM SIGOPS 2008)
   - "The Linux Scheduler: a Decade of Wasted Cores" (EuroSys 2016)

4. **Source Code**
   - kernel/sched/fair.c (CFS implementation)
   - kernel/cgroup/cgroup.c (cgroups v2 core)
   - kernel/cgroup/cpuset.c (CPU controller)

## Conclusion

cgroups v2 provides robust mechanisms for CPU isolation suitable for Tokio-Pulse's Tier 3 intervention. Key considerations:

1. **Compatibility**: Must handle v1, v2, and container environments
2. **Permissions**: Graceful degradation when unprivileged
3. **Performance**: Sub-millisecond overhead acceptable for Tier 3
4. **Security**: Careful validation of all operations

Next step: Study "The Linux Scheduler: a Decade of Wasted Cores" paper for deeper insights into scheduling pathologies.