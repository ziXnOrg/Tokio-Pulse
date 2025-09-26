/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */

# Windows Thread Scheduling and Job Objects Research

## Executive Summary

This document captures comprehensive research on Windows thread scheduling, job objects, and CPU resource control mechanisms for implementing OS-level task isolation in Tokio-Pulse.

## Windows Thread Scheduling Architecture

### Core Concepts

Windows scheduling operates at thread granularity, not process granularity. Processes provide resources and context, but threads are the schedulable entities.

#### Priority System
- **32 priority levels**: 0 (idle) to 31 (real-time)
- **Priority classes**: IDLE, BELOW_NORMAL, NORMAL, ABOVE_NORMAL, HIGH, REALTIME
- **Thread priorities**: Within each class: IDLE, LOWEST, BELOW_NORMAL, NORMAL, ABOVE_NORMAL, HIGHEST, TIME_CRITICAL
- **Dynamic priority boost**: Temporary increases for I/O completion, GUI events
- **Priority decay**: Gradual return to base priority after boost

#### Quantum and Time Slicing
- **Default quantum**: 2 clock intervals (20-30ms on client, 120ms on server)
- **Quantum units**: Variable based on system type and foreground/background
- **Processor affinity**: SetThreadAffinityMask for CPU pinning
- **Ideal processor**: Preferred CPU for thread execution

### Multi-Level Feedback Queue
```
Priority 31: Real-time critical
Priority 16-30: Real-time range
Priority 15: Time-critical dynamic
Priority 1-14: Dynamic range (most applications)
Priority 0: System idle thread
```

## Job Objects API

### Core Structure
```c
typedef struct _JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
    DWORD ControlFlags;
    union {
        DWORD CpuRate;      // For HARD_CAP or MIN_MAX_RATE
        DWORD Weight;       // For WEIGHT_BASED (1-9)
        struct {
            WORD MinRate;
            WORD MaxRate;
        } DUMMYSTRUCTNAME;
    } DUMMYUNIONNAME;
} JOBOBJECT_CPU_RATE_CONTROL_INFORMATION;
```

### Control Flags
```c
#define JOB_OBJECT_CPU_RATE_CONTROL_ENABLE        0x00000001
#define JOB_OBJECT_CPU_RATE_CONTROL_WEIGHT_BASED  0x00000002
#define JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP      0x00000004
#define JOB_OBJECT_CPU_RATE_CONTROL_NOTIFY        0x00000008
#define JOB_OBJECT_CPU_RATE_CONTROL_MIN_MAX_RATE  0x00000010
```

### Implementation Pattern
```c
// Create job object
HANDLE hJob = CreateJobObject(NULL, L"TokioPulse_Isolation");

// Configure CPU rate control
JOBOBJECT_CPU_RATE_CONTROL_INFORMATION cpuRateInfo = {0};
cpuRateInfo.ControlFlags = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE |
                           JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
cpuRateInfo.CpuRate = 5000;  // 50% of one CPU (value = percent * 100)

SetInformationJobObject(hJob,
                        JobObjectCpuRateControlInformation,
                        &cpuRateInfo,
                        sizeof(cpuRateInfo));

// Assign process to job
AssignProcessToJobObject(hJob, GetCurrentProcess());
```

### CPU Rate Control Mechanisms

#### 1. Hard Cap
- Absolute limit on CPU usage
- Specified as percentage * 100 (5000 = 50%)
- Enforced per scheduling interval
- Threads blocked when limit exceeded

#### 2. Weight-Based
- Relative CPU share (1-9 scale)
- 5 = default weight for normal workloads
- 1 = minimal share, 9 = maximum share
- Fair sharing among weighted jobs

#### 3. Min/Max Rate
- Guaranteed minimum CPU percentage
- Maximum ceiling for burst capacity
- Useful for QoS guarantees

### Hierarchical Job Control
- Nested job objects (Windows 8+)
- Child jobs inherit parent limits
- Rates calculated relative to parent allocation
- Enables fine-grained resource partitioning

## Performance Measurement APIs

### QueryThreadCycleTime
```c
BOOL QueryThreadCycleTime(
    HANDLE ThreadHandle,
    PULONG64 CycleTime
);
```

**Characteristics**:
- Returns CPU cycles consumed by thread
- Hardware counter based (RDTSC)
- Sub-microsecond measurement overhead
- Cannot reliably convert to wall time
- Best for relative performance metrics

### GetThreadTimes
```c
BOOL GetThreadTimes(
    HANDLE hThread,
    LPFILETIME lpCreationTime,
    LPFILETIME lpExitTime,
    LPFILETIME lpKernelTime,
    LPFILETIME lpUserTime
);
```

**Characteristics**:
- Returns time in 100ns units (FILETIME)
- Actual precision: ~15.625ms (scheduler quantum)
- Includes kernel and user mode separation
- Suitable for coarse-grained profiling

### Performance Comparison
| API | Precision | Overhead | Use Case |
|-----|-----------|----------|----------|
| QueryThreadCycleTime | <1μs | ~30-50ns | Fine-grained CPU profiling |
| GetThreadTimes | ~15ms | ~100ns | Coarse timing, billing |
| QueryPerformanceCounter | <1μs | ~800ns | Wall-clock intervals |

## Container and Virtualization Support

### Windows Containers
- **Process isolation**: Shared kernel, job object enforced
- **Hyper-V isolation**: Full VM isolation, overhead
- **Resource controls**: Memory, CPU, I/O via job objects

### WSL2 Considerations
- Linux scheduling inside VM
- Windows job objects don't apply to WSL2 processes
- Need VM-level resource controls

### Docker Desktop
- Uses Hyper-V or WSL2 backend
- Job objects apply to Docker daemon, not containers
- Container resource limits via runtime configuration

## Security and Privileges

### Required Privileges
```c
// No special privileges required for basic job object creation
HANDLE hJob = CreateJobObject(NULL, NULL);

// JOB_OBJECT_SET_ATTRIBUTES required for SetInformationJobObject
// Granted to process creator by default

// SE_INCREASE_BASE_PRIORITY_PRIVILEGE for real-time priorities
// SE_DEBUG_PRIVILEGE for cross-process operations
```

### Permission Denied Scenarios
1. **Restricted tokens**: AppContainer processes limited
2. **Integrity levels**: Low IL can't affect Medium IL
3. **Session isolation**: Cross-session restrictions
4. **UAC virtualization**: Redirected operations may fail

### Graceful Degradation
```rust
fn create_job_object() -> Result<Handle, Error> {
    match CreateJobObject(null_mut(), null()) {
        Ok(handle) => Ok(handle),
        Err(e) if e.code() == ERROR_ACCESS_DENIED => {
            // Fall back to thread priority manipulation
            warn!("Job objects unavailable, using thread priorities");
            Ok(Handle::ThreadPriority)
        }
        Err(e) => Err(e)
    }
}
```

## Chrome Site Isolation Insights

### Background Tab Throttling
- JavaScript timers limited to 1/minute in background
- CPU usage capped at 1% per background tab
- 5x reduction in CPU usage observed
- 1.25 hour battery life improvement

### Occlusion Tracking
- Windows-specific: Detects minimized/covered windows
- 25% faster startup, 7% faster page loads
- Automatic resource reduction for hidden tabs

### Implementation Lessons
1. **Gradual throttling**: 10-second grace period
2. **Smart exceptions**: Audio, WebRTC, WebSockets exempt
3. **User control**: Flags and policies for opt-out
4. **Measurement**: Extensive telemetry for impact

## Performance Characteristics

### Job Object Operations
| Operation | Typical Latency | Notes |
|-----------|-----------------|-------|
| CreateJobObject | 1-5μs | One-time cost |
| SetInformationJobObject | 500ns-2μs | Configuration overhead |
| AssignProcessToJobObject | 5-20μs | Process migration |
| QueryInformationJobObject | 200ns-1μs | Status check |

### Context Switch Costs
- **Windows 10/11**: 1-3μs typical
- **With job objects**: Additional 100-500ns
- **NUMA penalty**: 2-5x for cross-node

### Scalability Limits
- **Max job objects**: System memory limited
- **Practical limit**: 1000s of jobs without degradation
- **Nesting depth**: 10 levels recommended maximum

## Implementation Strategy for Tokio-Pulse

### Tier 3 (Isolate) Windows Implementation

```rust
#[cfg(windows)]
pub struct WindowsIsolation {
    job_handle: HANDLE,
    original_priority: i32,
}

impl WindowsIsolation {
    pub fn isolate_thread(cpu_percent: u32) -> Result<Self> {
        unsafe {
            // Create job for current process if needed
            let job = CreateJobObjectW(null(), null());

            // Configure CPU rate control
            let mut cpu_rate = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE |
                             JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                CpuRate: cpu_percent * 100,  // Convert to API format
                ..Default::default()
            };

            SetInformationJobObject(
                job,
                JobObjectCpuRateControlInformation,
                &mut cpu_rate as *mut _ as *mut c_void,
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            )?;

            // Assign current process
            AssignProcessToJobObject(job, GetCurrentProcess())?;

            Ok(WindowsIsolation {
                job_handle: job,
                original_priority: GetThreadPriority(GetCurrentThread()),
            })
        }
    }

    pub fn adjust_priority(&self, task_tier: u8) {
        unsafe {
            let priority = match task_tier {
                0 => THREAD_PRIORITY_NORMAL,
                1 => THREAD_PRIORITY_BELOW_NORMAL,
                2 => THREAD_PRIORITY_LOWEST,
                3 => THREAD_PRIORITY_IDLE,
                _ => THREAD_PRIORITY_IDLE,
            };
            SetThreadPriority(GetCurrentThread(), priority);
        }
    }
}
```

### Monitoring Implementation

```rust
pub struct WindowsCpuMonitor {
    thread_handle: HANDLE,
    last_cycles: u64,
    last_check: Instant,
}

impl WindowsCpuMonitor {
    pub fn measure_cpu_usage(&mut self) -> f64 {
        unsafe {
            let mut cycles: u64 = 0;
            QueryThreadCycleTime(self.thread_handle, &mut cycles);

            let elapsed_cycles = cycles - self.last_cycles;
            let elapsed_time = self.last_check.elapsed();

            self.last_cycles = cycles;
            self.last_check = Instant::now();

            // Approximate CPU usage (platform-specific calibration needed)
            let cycles_per_second = get_cpu_frequency();
            let cpu_percent = (elapsed_cycles as f64 / cycles_per_second) /
                            elapsed_time.as_secs_f64() * 100.0;

            cpu_percent
        }
    }
}
```

### Container Detection

```rust
fn detect_container_environment() -> ContainerType {
    // Check for Docker Desktop
    if Path::new(r"C:\ProgramData\Docker").exists() {
        return ContainerType::DockerDesktop;
    }

    // Check for Windows container
    if std::env::var("CONTAINER_SANDBOX_MOUNT_POINT").is_ok() {
        return ContainerType::WindowsContainer;
    }

    // Check for WSL
    if Path::new(r"/proc/sys/fs/binfmt_misc/WSLInterop").exists() {
        return ContainerType::WSL2;
    }

    ContainerType::None
}
```

## Platform-Specific Gotchas

### 1. Terminal Services/RDP
- Dynamic Fair Share Scheduling interferes with job objects
- CPU rate control may be disabled in RDP sessions
- Detection: GetSystemMetrics(SM_REMOTESESSION)

### 2. Antivirus Interference
- Some AV inject into processes, affecting job assignment
- May prevent job object creation or modification
- Mitigation: Retry with delays, fallback strategies

### 3. Power Management
- CPU frequency scaling affects cycle measurements
- Modern CPUs: Turbo boost invalidates cycle→time conversion
- Solution: Use relative measurements, not absolute time

### 4. NUMA Considerations
- Job objects don't respect NUMA boundaries by default
- Cross-node memory access adds latency
- Use SetThreadAffinityMask for NUMA-aware placement

### 5. Windows 11 Efficiency Cores
- Intel P-cores vs E-cores scheduling
- Job objects may not distinguish core types
- QoS classes interact with scheduler differently

## Production Best Practices

### 1. Defensive Initialization
```rust
pub fn init_isolation() -> IsolationStrategy {
    // Try job objects first
    if let Ok(_) = test_job_object_support() {
        return IsolationStrategy::JobObjects;
    }

    // Fall back to thread priorities
    if let Ok(_) = test_priority_support() {
        return IsolationStrategy::ThreadPriority;
    }

    // Last resort: cooperative yielding only
    warn!("No OS-level isolation available");
    IsolationStrategy::CooperativeOnly
}
```

### 2. Resource Cleanup
```rust
impl Drop for WindowsIsolation {
    fn drop(&mut self) {
        unsafe {
            // Job objects auto-cleanup when last handle closes
            if self.job_handle != INVALID_HANDLE_VALUE {
                CloseHandle(self.job_handle);
            }

            // Restore original thread priority
            SetThreadPriority(GetCurrentThread(), self.original_priority);
        }
    }
}
```

### 3. Monitoring and Diagnostics
```rust
pub struct IsolationMetrics {
    pub job_objects_created: AtomicU64,
    pub cpu_limit_enforcements: AtomicU64,
    pub fallback_activations: AtomicU64,
    pub permission_denied_errors: AtomicU64,
}
```

### 4. Testing Strategy
- Unit tests with admin/non-admin privileges
- Container environment tests
- RDP session compatibility
- Performance regression tests
- Stress tests with many job objects

## Conclusion

Windows provides robust CPU resource control through job objects with several advantages:

1. **Fine-grained control**: CPU caps, weights, and min/max rates
2. **Low overhead**: Sub-microsecond operations
3. **No special privileges**: Works in standard user context
4. **Production proven**: Used by Chrome, Windows containers

Key limitations:
1. **Windows 8.1+ required**: For CPU rate control
2. **Process-level granularity**: Can't isolate individual threads
3. **Container complications**: Different behavior in various environments
4. **Measurement precision**: 15ms granularity for time-based metrics

For Tokio-Pulse, job objects provide an excellent Tier 3 isolation mechanism with graceful fallback to thread priorities when unavailable.