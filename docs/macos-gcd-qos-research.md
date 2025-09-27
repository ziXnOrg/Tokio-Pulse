/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */

# macOS GCD and QoS Classes Research

## Executive Summary

This document captures comprehensive research on macOS Grand Central Dispatch (GCD), Quality of Service (QoS) classes, and thread policy management for implementing OS-level task isolation in Tokio-Pulse.

## Grand Central Dispatch (GCD) Overview

### Architecture

GCD is Apple's low-level API for managing concurrent operations through dispatch queues and thread pools. It provides automatic thread management with QoS-based scheduling.

#### Queue Types
1. **Serial Queues**: Execute one task at a time in FIFO order
2. **Concurrent Queues**: Execute multiple tasks simultaneously
3. **Main Queue**: Serial queue for UI updates on main thread
4. **Global Queues**: System-provided concurrent queues per QoS class

### Quality of Service Classes

#### Priority Hierarchy (Highest to Lowest)

1. **User Interactive (QOS_CLASS_USER_INTERACTIVE)**
   - Latency: Instantaneous
   - Use: Main thread, UI updates, animations
   - Power: Maximum performance regardless of energy
   - Duration: Milliseconds

2. **User Initiated (QOS_CLASS_USER_INITIATED)**
   - Latency: Nearly instantaneous
   - Use: User-triggered actions requiring immediate results
   - Power: High performance, some energy awareness
   - Duration: Seconds or less

3. **Default (QOS_CLASS_DEFAULT)**
   - Latency: Not specified
   - Use: Developer-initiated work without explicit QoS
   - Power: Balanced performance/energy
   - Duration: Variable

4. **Utility (QOS_CLASS_UTILITY)**
   - Latency: Seconds to minutes
   - Use: Progress-indicated tasks (downloads, imports)
   - Power: Energy-efficient execution
   - Duration: Seconds to minutes

5. **Background (QOS_CLASS_BACKGROUND)**
   - Latency: Minutes to hours
   - Use: Maintenance, prefetching, backups
   - Power: Maximum energy efficiency
   - Duration: Minutes to hours
   - **Note**: Disk I/O throttled, timer coalescing applied

6. **Unspecified (QOS_CLASS_UNSPECIFIED)**
   - Legacy compatibility mode
   - Inherits QoS from context
   - Should be avoided in new code

### QoS Implementation Details

#### Dispatch Queue Creation with QoS
```c
// Create serial queue with specific QoS
dispatch_queue_attr_t attr = dispatch_queue_attr_make_with_qos_class(
    DISPATCH_QUEUE_SERIAL,
    QOS_CLASS_UTILITY,
    -1  // Relative priority within QoS class
);
dispatch_queue_t queue = dispatch_queue_create("com.tokiopulse.worker", attr);
```

#### Global Concurrent Queues
```c
// Get global queue for specific QoS
dispatch_queue_t queue = dispatch_get_global_queue(QOS_CLASS_UTILITY, 0);
```

#### pthread QoS Assignment
```c
// Set QoS for current pthread
pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0);

// Create pthread with QoS
pthread_attr_t attr;
pthread_attr_init(&attr);
pthread_attr_set_qos_class_np(&attr, QOS_CLASS_UTILITY, 0);
pthread_create(&thread, &attr, worker_function, NULL);
```

## Mach Thread Policies

### thread_policy_set API

The Mach kernel provides direct thread control through `thread_policy_set()`:

```c
kern_return_t thread_policy_set(
    thread_t thread,
    thread_policy_flavor_t flavor,
    thread_policy_t policy_info,
    mach_msg_type_number_t count
);
```

### Available Policies

#### 1. THREAD_AFFINITY_POLICY
```c
typedef struct thread_affinity_policy {
    integer_t affinity_tag;
} thread_affinity_policy_data_t;

// Example: Pin threads to same L2 cache
thread_affinity_policy_data_t policy;
policy.affinity_tag = 1;  // Non-zero tag groups threads
thread_policy_set(mach_thread_self(),
                 THREAD_AFFINITY_POLICY,
                 (thread_policy_t)&policy,
                 THREAD_AFFINITY_POLICY_COUNT);
```

**Important**: Not supported on Apple Silicon (M1/M2/M3)

#### 2. THREAD_PRECEDENCE_POLICY
```c
typedef struct thread_precedence_policy {
    integer_t importance;  // 0 (IDLE_PRI) to 63
} thread_precedence_policy_data_t;

// Example: Set thread to lowest priority
thread_precedence_policy_data_t policy;
policy.importance = 0;  // IDLE_PRI
thread_policy_set(mach_thread_self(),
                 THREAD_PRECEDENCE_POLICY,
                 (thread_policy_t)&policy,
                 THREAD_PRECEDENCE_POLICY_COUNT);
```

#### 3. THREAD_TIME_CONSTRAINT_POLICY
```c
typedef struct thread_time_constraint_policy {
    uint32_t period;       // Nominal periodicity (ns)
    uint32_t computation;  // Maximum computation time (ns)
    uint32_t constraint;   // Deadline from start of period (ns)
    boolean_t preemptible; // Can be preempted?
} thread_time_constraint_policy_data_t;

// Example: Real-time audio thread
thread_time_constraint_policy_data_t policy;
policy.period = 11025000;      // ~11ms (44.1kHz/512 samples)
policy.computation = 2000000;   // 2ms computation
policy.constraint = 11025000;   // Same as period
policy.preemptible = FALSE;
```

## CPU Usage Measurement

### thread_info() with THREAD_BASIC_INFO
```c
#include <mach/mach_init.h>
#include <mach/thread_act.h>
#include <mach/thread_info.h>

typedef struct {
    int64_t user_time_us;
    int64_t system_time_us;
} thread_cpu_time_t;

thread_cpu_time_t get_thread_cpu_time() {
    thread_basic_info_data_t info;
    mach_msg_type_number_t count = THREAD_BASIC_INFO_COUNT;

    kern_return_t kr = thread_info(
        mach_thread_self(),
        THREAD_BASIC_INFO,
        (thread_info_t)&info,
        &count
    );

    if (kr == KERN_SUCCESS) {
        int64_t user_us = info.user_time.seconds * 1000000 +
                         info.user_time.microseconds;
        int64_t system_us = info.system_time.seconds * 1000000 +
                           info.system_time.microseconds;
        return (thread_cpu_time_t){user_us, system_us};
    }

    return (thread_cpu_time_t){-1, -1};
}
```

### Alternative: clock_gettime (macOS 10.12+)
```c
#include <time.h>

uint64_t get_thread_cpu_ns() {
    struct timespec ts;
    if (clock_gettime(CLOCK_THREAD_CPUTIME_ID, &ts) == 0) {
        return ts.tv_sec * 1000000000ULL + ts.tv_nsec;
    }
    return 0;
}
```

## Priority Inversion Handling

### Automatic Resolution
GCD automatically resolves priority inversions in certain cases:

1. **Synchronous dispatch**: `dispatch_sync()` boosts target queue QoS
2. **Semaphore wait**: `dispatch_semaphore_wait()` propagates QoS
3. **Mutex lock**: `pthread_mutex_lock()` boosts lock holder
4. **Dispatch group**: `dispatch_group_wait()` elevates group operations

### Manual QoS Override
```c
// Override QoS for specific block
dispatch_block_t block = dispatch_block_create_with_qos_class(
    DISPATCH_BLOCK_ENFORCE_QOS_CLASS,
    QOS_CLASS_USER_INITIATED,
    0,
    ^{ /* work */ }
);
```

## Platform-Specific Limitations

### Apple Silicon (M1/M2/M3)
1. **No thread affinity**: `THREAD_AFFINITY_POLICY` returns `KERN_NOT_SUPPORTED`
2. **Asymmetric cores**: Performance vs Efficiency cores handled automatically
3. **QoS mapping**: System maps QoS to appropriate core type
4. **No direct core binding**: Cannot pin threads to specific cores

### Intel Macs
1. **Thread affinity works**: Can hint L2 cache sharing
2. **NUMA considerations**: Multi-socket Mac Pro systems
3. **Hyperthreading**: Logical vs physical core distinctions

### General Limitations
1. **No CPU percentage limits**: Unlike Windows job objects
2. **No hard CPU caps**: Only priority-based scheduling
3. **Advisory policies**: Kernel may ignore hints
4. **Entitlements required**: Some APIs need special app permissions

## Implementation Strategy for Tokio-Pulse

### Tier-Based QoS Mapping

```rust
#[cfg(target_os = "macos")]
pub struct MacOSQoS {
    original_qos: qos_class_t,
    thread_port: mach_port_t,
}

impl MacOSQoS {
    pub fn new() -> Self {
        unsafe {
            let thread_port = pthread_mach_thread_np(pthread_self());
            let original_qos = pthread_get_qos_class_np();

            MacOSQoS {
                original_qos,
                thread_port,
            }
        }
    }

    pub fn apply_tier(&self, tier: u8) {
        let qos_class = match tier {
            0 => QOS_CLASS_USER_INITIATED,  // Normal tasks
            1 => QOS_CLASS_DEFAULT,         // Slightly degraded
            2 => QOS_CLASS_UTILITY,         // Noticeable degradation
            3 => QOS_CLASS_BACKGROUND,      // Maximum throttling
            _ => QOS_CLASS_BACKGROUND,
        };

        unsafe {
            pthread_set_qos_class_self_np(qos_class, 0);
        }
    }

    pub fn set_precedence(&self, tier: u8) {
        // For finer control within QoS class
        let importance = match tier {
            0 => 63,  // Maximum
            1 => 31,  // Medium
            2 => 15,  // Low
            3 => 0,   // IDLE_PRI
            _ => 0,
        };

        unsafe {
            let mut policy = thread_precedence_policy_data_t {
                importance,
            };

            thread_policy_set(
                self.thread_port,
                THREAD_PRECEDENCE_POLICY,
                &mut policy as *mut _ as thread_policy_t,
                THREAD_PRECEDENCE_POLICY_COUNT,
            );
        }
    }
}
```

### CPU Monitoring

```rust
pub struct MacOSCpuMonitor {
    thread_port: mach_port_t,
    last_user_time: u64,
    last_system_time: u64,
    last_check: Instant,
}

impl MacOSCpuMonitor {
    pub fn measure_cpu_percent(&mut self) -> f64 {
        unsafe {
            let mut info = thread_basic_info_data_t::default();
            let mut count = THREAD_BASIC_INFO_COUNT;

            let kr = thread_info(
                self.thread_port,
                THREAD_BASIC_INFO,
                &mut info as *mut _ as thread_info_t,
                &mut count,
            );

            if kr != KERN_SUCCESS {
                return 0.0;
            }

            let user_us = info.user_time.seconds * 1_000_000 +
                         info.user_time.microseconds;
            let system_us = info.system_time.seconds * 1_000_000 +
                           info.system_time.microseconds;

            let cpu_delta = (user_us - self.last_user_time) +
                           (system_us - self.last_system_time);
            let time_delta = self.last_check.elapsed().as_micros();

            self.last_user_time = user_us;
            self.last_system_time = system_us;
            self.last_check = Instant::now();

            (cpu_delta as f64 / time_delta as f64) * 100.0
        }
    }
}
```

### Dispatch Queue Integration

```rust
pub struct DispatchQueueIsolation {
    queues: [dispatch_queue_t; 4],
}

impl DispatchQueueIsolation {
    pub fn new() -> Self {
        let queues = [
            create_queue_with_qos(QOS_CLASS_USER_INITIATED),
            create_queue_with_qos(QOS_CLASS_DEFAULT),
            create_queue_with_qos(QOS_CLASS_UTILITY),
            create_queue_with_qos(QOS_CLASS_BACKGROUND),
        ];

        DispatchQueueIsolation { queues }
    }

    pub fn dispatch_to_tier(&self, tier: u8, work: Box<dyn FnOnce()>) {
        let queue = self.queues[tier.min(3) as usize];

        unsafe {
            dispatch_async_f(
                queue,
                Box::into_raw(work) as *mut c_void,
                execute_work,
            );
        }
    }
}

extern "C" fn execute_work(context: *mut c_void) {
    unsafe {
        let work = Box::from_raw(context as *mut Box<dyn FnOnce()>);
        work();
    }
}
```

## Best Practices

### 1. QoS Selection
- Start with Default, adjust based on measurement
- Avoid User Interactive unless truly UI-critical
- Use Background for all maintenance tasks
- Consider energy impact on battery-powered devices

### 2. Priority Inversions
- Design to minimize cross-QoS dependencies
- Use async patterns to avoid blocking
- Let GCD handle automatic boosting
- Monitor for unexpected priority inheritance

### 3. Performance Monitoring
- Sample CPU usage at regular intervals
- Compare against wall clock time for efficiency
- Track QoS changes and their impact
- Use Instruments for detailed profiling

### 4. Compatibility
- Check for Apple Silicon at runtime
- Provide fallbacks for unsupported APIs
- Test on both Intel and ARM Macs
- Consider Rosetta 2 translation effects

## Testing Recommendations

### Unit Tests
```rust
#[test]
#[cfg(target_os = "macos")]
fn test_qos_assignment() {
    let original_qos = get_current_qos();
    set_qos(QOS_CLASS_BACKGROUND);
    assert_eq!(get_current_qos(), QOS_CLASS_BACKGROUND);
    set_qos(original_qos);
}
```

### Integration Tests
1. Verify QoS changes affect scheduling
2. Measure actual CPU throttling
3. Test priority inversion scenarios
4. Validate energy efficiency improvements

### Performance Tests
1. Benchmark overhead of QoS changes (~50-200ns)
2. Measure thread_info() call cost (~500ns-1μs)
3. Compare dispatch queue vs direct pthread
4. Profile under different system loads

## Conclusion

macOS provides sophisticated thread scheduling through GCD and QoS classes, offering:

**Advantages**:
1. Automatic energy management
2. System-wide priority coordination
3. Built-in priority inversion handling
4. Fine-grained control through Mach APIs

**Limitations**:
1. No hard CPU percentage caps
2. Apple Silicon thread affinity unsupported
3. Advisory rather than mandatory policies
4. Limited documentation for low-level APIs

For Tokio-Pulse, QoS classes provide excellent Tier 0-3 differentiation with automatic system integration, while Mach thread policies offer additional fine-tuning for specialized scenarios.