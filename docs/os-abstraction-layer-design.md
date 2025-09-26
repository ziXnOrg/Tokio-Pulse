/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */

# OS Abstraction Layer Design

## Executive Summary

This document specifies the OS abstraction layer for Tokio-Pulse, providing a unified interface for CPU isolation and resource control across Linux, Windows, and macOS. The design emphasizes zero-cost abstractions, graceful degradation, and platform-specific optimizations.

## Design Principles

1. **Zero-cost abstraction**: Platform-specific code compiled only for target OS
2. **Graceful degradation**: Fallback strategies when OS features unavailable
3. **Runtime detection**: Capability discovery at initialization
4. **Minimal overhead**: <100ns for capability checks
5. **Safety first**: No unsafe in public APIs

## Architecture Overview

### Layer Hierarchy

```
┌─────────────────────────────────────┐
│         Public API Layer            │
│    (Platform-agnostic interface)    │
├─────────────────────────────────────┤
│      Capability Detection Layer     │
│   (Runtime feature discovery)       │
├─────────────────────────────────────┤
│     Platform Abstraction Layer      │
│  (Trait-based OS abstractions)      │
├──────────┬──────────┬───────────────┤
│  Linux   │ Windows  │    macOS      │
│ Backend  │ Backend  │   Backend     │
└──────────┴──────────┴───────────────┘
```

## Core Traits

### CPU Isolation Trait

```rust
/// Platform-agnostic CPU isolation interface
pub trait CpuIsolation: Send + Sync {
    /// Apply CPU limit to current thread/process
    fn apply_cpu_limit(&self, percent: f32) -> Result<(), IsolationError>;

    /// Set thread/process priority
    fn set_priority(&self, level: PriorityLevel) -> Result<(), IsolationError>;

    /// Create isolated execution context
    fn create_isolation_context(&self) -> Result<IsolationContext, IsolationError>;

    /// Release isolation resources
    fn release_isolation(&self) -> Result<(), IsolationError>;

    /// Query current CPU usage
    fn get_cpu_usage(&self) -> Result<CpuUsage, IsolationError>;

    /// Platform-specific capability flags
    fn capabilities(&self) -> IsolationCapabilities;
}

/// Priority levels mapped to OS-specific values
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PriorityLevel {
    Realtime,     // Highest priority (requires privileges)
    High,         // Above normal priority
    Normal,       // Default priority
    Low,          // Below normal priority
    Idle,         // Lowest priority
}

/// CPU usage statistics
#[derive(Debug, Clone)]
pub struct CpuUsage {
    pub user_ns: u64,
    pub system_ns: u64,
    pub total_ns: u64,
    pub percent: f32,
}

/// Platform capabilities
#[derive(Debug, Clone)]
pub struct IsolationCapabilities {
    pub cpu_limiting: bool,
    pub priority_control: bool,
    pub numa_aware: bool,
    pub container_aware: bool,
    pub requires_privileges: bool,
}
```

### CPU Timing Trait

```rust
/// High-precision CPU time measurement
pub trait CpuTimer: Send + Sync {
    /// Get thread CPU time in nanoseconds
    fn thread_cpu_time_ns(&self) -> u64;

    /// Get process CPU time in nanoseconds
    fn process_cpu_time_ns(&self) -> u64;

    /// Measurement overhead in nanoseconds
    fn measurement_overhead_ns(&self) -> u64;

    /// Timer resolution in nanoseconds
    fn resolution_ns(&self) -> u64;
}
```

## Platform Implementations

### Linux Implementation

```rust
#[cfg(target_os = "linux")]
pub struct LinuxIsolation {
    cgroup_version: CgroupVersion,
    cgroup_path: Option<PathBuf>,
    original_nice: i32,
    numa_nodes: Vec<NumaNode>,
}

#[cfg(target_os = "linux")]
impl LinuxIsolation {
    pub fn new() -> Result<Self, IsolationError> {
        let cgroup_version = detect_cgroup_version()?;
        let numa_nodes = detect_numa_topology()?;

        Ok(LinuxIsolation {
            cgroup_version,
            cgroup_path: None,
            original_nice: getpriority(PRIO_PROCESS, 0),
            numa_nodes,
        })
    }

    fn create_cgroup_v2(&self, cpu_percent: f32) -> Result<PathBuf, IsolationError> {
        let cgroup_name = format!("tokio-pulse-{}", std::process::id());
        let cgroup_path = Path::new("/sys/fs/cgroup").join(&cgroup_name);

        // Create cgroup directory
        std::fs::create_dir_all(&cgroup_path)?;

        // Set CPU max (percent * 100000 for 100ms period)
        let cpu_quota = (cpu_percent * 100_000.0) as u64;
        std::fs::write(
            cgroup_path.join("cpu.max"),
            format!("{} 100000", cpu_quota)
        )?;

        // Move current process to cgroup
        std::fs::write(
            cgroup_path.join("cgroup.procs"),
            format!("{}", std::process::id())
        )?;

        Ok(cgroup_path)
    }
}

#[cfg(target_os = "linux")]
impl CpuIsolation for LinuxIsolation {
    fn apply_cpu_limit(&self, percent: f32) -> Result<(), IsolationError> {
        match self.cgroup_version {
            CgroupVersion::V2 => {
                self.cgroup_path = Some(self.create_cgroup_v2(percent)?);
                Ok(())
            }
            CgroupVersion::V1 => {
                self.create_cgroup_v1(percent)
            }
            CgroupVersion::None => {
                // Fallback to nice values
                let nice_value = map_percent_to_nice(percent);
                setpriority(PRIO_PROCESS, 0, nice_value)?;
                Ok(())
            }
        }
    }

    fn set_priority(&self, level: PriorityLevel) -> Result<(), IsolationError> {
        let nice_value = match level {
            PriorityLevel::Realtime => -20,  // Requires CAP_SYS_NICE
            PriorityLevel::High => -10,
            PriorityLevel::Normal => 0,
            PriorityLevel::Low => 10,
            PriorityLevel::Idle => 19,
        };

        setpriority(PRIO_PROCESS, 0, nice_value)?;
        Ok(())
    }

    fn capabilities(&self) -> IsolationCapabilities {
        IsolationCapabilities {
            cpu_limiting: self.cgroup_version != CgroupVersion::None,
            priority_control: true,
            numa_aware: !self.numa_nodes.is_empty(),
            container_aware: detect_container_environment(),
            requires_privileges: false,  // cgroups v2 delegation
        }
    }
}

#[cfg(target_os = "linux")]
pub struct LinuxCpuTimer;

#[cfg(target_os = "linux")]
impl CpuTimer for LinuxCpuTimer {
    fn thread_cpu_time_ns(&self) -> u64 {
        let mut ts = timespec { tv_sec: 0, tv_nsec: 0 };
        unsafe {
            clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts);
        }
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    fn measurement_overhead_ns(&self) -> u64 {
        30  // ~30ns on modern Linux
    }

    fn resolution_ns(&self) -> u64 {
        1  // Nanosecond resolution
    }
}
```

### Windows Implementation

```rust
#[cfg(target_os = "windows")]
pub struct WindowsIsolation {
    job_handle: Option<HANDLE>,
    original_priority: i32,
    processor_count: u32,
}

#[cfg(target_os = "windows")]
impl WindowsIsolation {
    pub fn new() -> Result<Self, IsolationError> {
        let system_info = get_system_info();

        Ok(WindowsIsolation {
            job_handle: None,
            original_priority: GetThreadPriority(GetCurrentThread()),
            processor_count: system_info.dwNumberOfProcessors,
        })
    }

    fn create_job_with_cpu_limit(&mut self, percent: f32) -> Result<(), IsolationError> {
        unsafe {
            // Create job object
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return Err(IsolationError::JobCreationFailed);
            }

            // Configure CPU rate control
            let mut cpu_rate_info = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE |
                             JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                CpuRate: (percent * 100.0) as u32,  // Percent * 100
            };

            let result = SetInformationJobObject(
                job,
                JobObjectCpuRateControlInformation,
                &mut cpu_rate_info as *mut _ as *mut c_void,
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            );

            if result == 0 {
                CloseHandle(job);
                return Err(IsolationError::ConfigurationFailed);
            }

            // Assign current process to job
            if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
                CloseHandle(job);
                return Err(IsolationError::AssignmentFailed);
            }

            self.job_handle = Some(job);
            Ok(())
        }
    }
}

#[cfg(target_os = "windows")]
impl CpuIsolation for WindowsIsolation {
    fn apply_cpu_limit(&self, percent: f32) -> Result<(), IsolationError> {
        // Check Windows version (8.1+ required for CPU rate control)
        if !is_windows_8_1_or_greater() {
            // Fallback to thread priority
            return self.set_priority(map_percent_to_priority(percent));
        }

        self.create_job_with_cpu_limit(percent)
    }

    fn set_priority(&self, level: PriorityLevel) -> Result<(), IsolationError> {
        let priority = match level {
            PriorityLevel::Realtime => THREAD_PRIORITY_TIME_CRITICAL,
            PriorityLevel::High => THREAD_PRIORITY_HIGHEST,
            PriorityLevel::Normal => THREAD_PRIORITY_NORMAL,
            PriorityLevel::Low => THREAD_PRIORITY_BELOW_NORMAL,
            PriorityLevel::Idle => THREAD_PRIORITY_IDLE,
        };

        unsafe {
            SetThreadPriority(GetCurrentThread(), priority);
        }
        Ok(())
    }

    fn capabilities(&self) -> IsolationCapabilities {
        IsolationCapabilities {
            cpu_limiting: is_windows_8_1_or_greater(),
            priority_control: true,
            numa_aware: self.processor_count > 1,
            container_aware: detect_windows_container(),
            requires_privileges: false,  // Job objects work unprivileged
        }
    }
}

#[cfg(target_os = "windows")]
pub struct WindowsCpuTimer;

#[cfg(target_os = "windows")]
impl CpuTimer for WindowsCpuTimer {
    fn thread_cpu_time_ns(&self) -> u64 {
        let mut cycles: u64 = 0;
        unsafe {
            QueryThreadCycleTime(GetCurrentThread(), &mut cycles);
        }
        // Convert cycles to nanoseconds (approximate)
        cycles * 1_000_000_000 / get_cpu_frequency()
    }

    fn measurement_overhead_ns(&self) -> u64 {
        50  // ~50ns on Windows
    }

    fn resolution_ns(&self) -> u64 {
        100  // 100ns typical resolution
    }
}
```

### macOS Implementation

```rust
#[cfg(target_os = "macos")]
pub struct MacOSIsolation {
    thread_port: mach_port_t,
    original_qos: qos_class_t,
    original_priority: i32,
}

#[cfg(target_os = "macos")]
impl MacOSIsolation {
    pub fn new() -> Result<Self, IsolationError> {
        let thread_port = unsafe { pthread_mach_thread_np(pthread_self()) };
        let original_qos = unsafe { pthread_get_qos_class_np() };

        Ok(MacOSIsolation {
            thread_port,
            original_qos,
            original_priority: 0,
        })
    }

    fn set_qos_class(&self, qos: qos_class_t) -> Result<(), IsolationError> {
        unsafe {
            if pthread_set_qos_class_self_np(qos, 0) != 0 {
                return Err(IsolationError::QoSSetFailed);
            }
        }
        Ok(())
    }

    fn set_thread_policy(&self, importance: i32) -> Result<(), IsolationError> {
        unsafe {
            let mut policy = thread_precedence_policy_data_t {
                importance,
            };

            let result = thread_policy_set(
                self.thread_port,
                THREAD_PRECEDENCE_POLICY,
                &mut policy as *mut _ as thread_policy_t,
                THREAD_PRECEDENCE_POLICY_COUNT,
            );

            if result != KERN_SUCCESS {
                return Err(IsolationError::PolicySetFailed);
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl CpuIsolation for MacOSIsolation {
    fn apply_cpu_limit(&self, percent: f32) -> Result<(), IsolationError> {
        // macOS doesn't support hard CPU limits, use QoS classes
        let qos = match percent {
            p if p >= 0.75 => QOS_CLASS_USER_INITIATED,
            p if p >= 0.50 => QOS_CLASS_DEFAULT,
            p if p >= 0.25 => QOS_CLASS_UTILITY,
            _ => QOS_CLASS_BACKGROUND,
        };

        self.set_qos_class(qos)
    }

    fn set_priority(&self, level: PriorityLevel) -> Result<(), IsolationError> {
        let (qos, importance) = match level {
            PriorityLevel::Realtime => {
                // Real-time requires special entitlements
                return Err(IsolationError::InsufficientPrivileges);
            }
            PriorityLevel::High => (QOS_CLASS_USER_INITIATED, 63),
            PriorityLevel::Normal => (QOS_CLASS_DEFAULT, 31),
            PriorityLevel::Low => (QOS_CLASS_UTILITY, 15),
            PriorityLevel::Idle => (QOS_CLASS_BACKGROUND, 0),
        };

        self.set_qos_class(qos)?;
        self.set_thread_policy(importance)?;
        Ok(())
    }

    fn capabilities(&self) -> IsolationCapabilities {
        IsolationCapabilities {
            cpu_limiting: false,  // No hard limits on macOS
            priority_control: true,
            numa_aware: false,  // Not on Apple Silicon
            container_aware: false,
            requires_privileges: false,
        }
    }
}

#[cfg(target_os = "macos")]
pub struct MacOSCpuTimer;

#[cfg(target_os = "macos")]
impl CpuTimer for MacOSCpuTimer {
    fn thread_cpu_time_ns(&self) -> u64 {
        let mut info = thread_basic_info_data_t::default();
        let mut count = THREAD_BASIC_INFO_COUNT;

        unsafe {
            let kr = thread_info(
                mach_thread_self(),
                THREAD_BASIC_INFO,
                &mut info as *mut _ as thread_info_t,
                &mut count,
            );

            if kr == KERN_SUCCESS {
                let user_ns = info.user_time.seconds as u64 * 1_000_000_000 +
                             info.user_time.microseconds as u64 * 1_000;
                let system_ns = info.system_time.seconds as u64 * 1_000_000_000 +
                               info.system_time.microseconds as u64 * 1_000;
                user_ns + system_ns
            } else {
                0
            }
        }
    }

    fn measurement_overhead_ns(&self) -> u64 {
        40  // ~40ns on macOS
    }

    fn resolution_ns(&self) -> u64 {
        1000  // Microsecond resolution
    }
}
```

## Factory Pattern

### Platform Detection and Creation

```rust
/// Factory for creating platform-specific implementations
pub struct OsAbstractionFactory;

impl OsAbstractionFactory {
    /// Create appropriate isolation implementation for current platform
    pub fn create_isolation() -> Box<dyn CpuIsolation> {
        #[cfg(target_os = "linux")]
        {
            match LinuxIsolation::new() {
                Ok(isolation) => Box::new(isolation),
                Err(_) => Box::new(FallbackIsolation::new()),
            }
        }

        #[cfg(target_os = "windows")]
        {
            match WindowsIsolation::new() {
                Ok(isolation) => Box::new(isolation),
                Err(_) => Box::new(FallbackIsolation::new()),
            }
        }

        #[cfg(target_os = "macos")]
        {
            match MacOSIsolation::new() {
                Ok(isolation) => Box::new(isolation),
                Err(_) => Box::new(FallbackIsolation::new()),
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Box::new(FallbackIsolation::new())
        }
    }

    /// Create appropriate timer implementation for current platform
    pub fn create_timer() -> Box<dyn CpuTimer> {
        #[cfg(target_os = "linux")]
        {
            Box::new(LinuxCpuTimer)
        }

        #[cfg(target_os = "windows")]
        {
            Box::new(WindowsCpuTimer)
        }

        #[cfg(target_os = "macos")]
        {
            Box::new(MacOSCpuTimer)
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Box::new(FallbackCpuTimer)
        }
    }
}
```

## Fallback Implementation

```rust
/// Fallback implementation when OS features unavailable
pub struct FallbackIsolation {
    start_time: Instant,
}

impl FallbackIsolation {
    pub fn new() -> Self {
        FallbackIsolation {
            start_time: Instant::now(),
        }
    }
}

impl CpuIsolation for FallbackIsolation {
    fn apply_cpu_limit(&self, _percent: f32) -> Result<(), IsolationError> {
        // Cannot limit CPU without OS support
        Err(IsolationError::NotSupported)
    }

    fn set_priority(&self, _level: PriorityLevel) -> Result<(), IsolationError> {
        // Cannot set priority without OS support
        Err(IsolationError::NotSupported)
    }

    fn get_cpu_usage(&self) -> Result<CpuUsage, IsolationError> {
        // Approximate using wall clock time
        let elapsed = self.start_time.elapsed();
        Ok(CpuUsage {
            user_ns: 0,
            system_ns: 0,
            total_ns: elapsed.as_nanos() as u64,
            percent: 0.0,
        })
    }

    fn capabilities(&self) -> IsolationCapabilities {
        IsolationCapabilities {
            cpu_limiting: false,
            priority_control: false,
            numa_aware: false,
            container_aware: false,
            requires_privileges: false,
        }
    }
}

pub struct FallbackCpuTimer {
    start: Instant,
}

impl CpuTimer for FallbackCpuTimer {
    fn thread_cpu_time_ns(&self) -> u64 {
        // Use wall clock as approximation
        self.start.elapsed().as_nanos() as u64
    }

    fn measurement_overhead_ns(&self) -> u64 {
        100  // Estimate
    }

    fn resolution_ns(&self) -> u64 {
        1000  // Microsecond at best
    }
}
```

## Integration with TierManager

```rust
impl TierManager {
    pub fn new(config: TierConfig) -> Self {
        let isolation = OsAbstractionFactory::create_isolation();
        let timer = OsAbstractionFactory::create_timer();

        let capabilities = isolation.capabilities();
        info!("OS isolation capabilities: {:?}", capabilities);

        // Adjust configuration based on capabilities
        let adjusted_config = if !capabilities.cpu_limiting {
            warn!("CPU limiting not available, using priority-only mode");
            config.with_priority_only_mode()
        } else {
            config
        };

        TierManager {
            isolation,
            timer,
            config: adjusted_config,
            // ... other fields
        }
    }

    pub fn apply_tier_isolation(&self, task_id: TaskId, tier: u8) -> Result<()> {
        let isolation_percent = match tier {
            0 => 100.0,  // No limit
            1 => 75.0,   // 75% CPU
            2 => 50.0,   // 50% CPU
            3 => 25.0,   // 25% CPU
            _ => 25.0,
        };

        self.isolation.apply_cpu_limit(isolation_percent)?;

        let priority = match tier {
            0 => PriorityLevel::Normal,
            1 => PriorityLevel::Low,
            2 => PriorityLevel::Low,
            3 => PriorityLevel::Idle,
            _ => PriorityLevel::Idle,
        };

        self.isolation.set_priority(priority)?;

        Ok(())
    }
}
```

## Testing Strategy

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_detection() {
        let isolation = OsAbstractionFactory::create_isolation();
        let caps = isolation.capabilities();

        // Verify platform-specific expectations
        #[cfg(target_os = "linux")]
        assert!(caps.priority_control);

        #[cfg(target_os = "windows")]
        assert!(caps.priority_control);

        #[cfg(target_os = "macos")]
        assert!(!caps.cpu_limiting);  // macOS doesn't support hard limits
    }

    #[test]
    fn test_graceful_degradation() {
        let fallback = FallbackIsolation::new();

        // Should return NotSupported
        assert!(matches!(
            fallback.apply_cpu_limit(50.0),
            Err(IsolationError::NotSupported)
        ));
    }

    #[test]
    fn test_timer_precision() {
        let timer = OsAbstractionFactory::create_timer();

        let start = timer.thread_cpu_time_ns();
        // Busy loop for measurable CPU time
        let mut sum = 0u64;
        for i in 0..1_000_000 {
            sum = sum.wrapping_add(i);
        }
        let end = timer.thread_cpu_time_ns();

        assert!(end > start);
        assert!(sum > 0);  // Prevent optimization
    }
}
```

## Performance Considerations

| Platform | CPU Limit | Priority | Timer | Overhead |
|----------|-----------|----------|-------|----------|
| Linux | cgroups v2 | nice | clock_gettime | ~30ns |
| Windows | Job Objects | Thread Priority | QueryThreadCycleTime | ~50ns |
| macOS | QoS Classes | Thread Policy | thread_info | ~40ns |
| Fallback | None | None | Instant::now | ~100ns |

## Conclusion

The OS abstraction layer provides:

1. **Unified interface**: Single API across all platforms
2. **Platform optimization**: Native features when available
3. **Graceful degradation**: Fallbacks for missing features
4. **Zero-cost abstractions**: Compile-time platform selection
5. **Runtime detection**: Capability discovery at initialization

This design ensures Tokio-Pulse can leverage platform-specific optimizations while maintaining portability and safety.