//! Cross-platform CPU Time Measurement
//!
//! This module provides high-performance CPU time measurement across different platforms.
//! It abstracts platform-specific timing mechanisms behind a common trait interface.
//!
//! # Performance Requirements
//!
//! - Per-call overhead: <50ns on supported platforms
//! - Thread-local measurements only (no cross-thread timing)
//! - Monotonic guarantees where possible
//! - Automatic calibration to compensate for measurement overhead
//!
//! # Platform Support
//!
//! - **Linux**: Uses `clock_gettime(CLOCK_THREAD_CPUTIME_ID)` for nanosecond precision
//! - **Windows**: Uses `QueryThreadCycleTime` with frequency conversion
//! - **macOS**: Uses `thread_info` with `THREAD_BASIC_INFO`
//! - **Fallback**: Uses `std::time::Instant` (measures wall time, not CPU time)
//!
//! # Accuracy Notes
//!
//! CPU time measurement accuracy varies by platform:
//! - Linux: ~15ns overhead, nanosecond resolution
//! - Windows: ~30ns overhead, depends on CPU frequency
//! - macOS: ~40ns overhead, microsecond resolution
//! - Fallback: ~50-200ns overhead, measures wall time instead of CPU time
//!
//! # Example
//!
//! ```rust
//! use tokio_pulse::timing::create_cpu_timer;
//!
//! let timer = create_cpu_timer();
//! let start = timer.thread_cpu_time_ns().unwrap();
//!
//! // Do some CPU-intensive work
//! let mut sum = 0u64;
//! for i in 0..1000 {
//!     sum = sum.wrapping_add(i);
//! }
//!
//! let end = timer.thread_cpu_time_ns().unwrap();
//! let cpu_time = end - start;
//! println!("CPU time used: {} ns", cpu_time);
//! ```

use std::fmt;
use thiserror::Error;

mod fallback;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use fallback::FallbackTimer;
#[cfg(target_os = "linux")]
use linux::LinuxTimer;
#[cfg(target_os = "macos")]
use macos::MacOsTimer;
#[cfg(target_os = "windows")]
use windows::WindowsTimer;

/// Errors that can occur during CPU time measurement
#[derive(Debug, Error)]
pub enum TimingError {
    /// Platform is not supported for precise CPU timing
    #[error("Platform not supported: {0}")]
    PlatformNotSupported(String),

    /// System call failed
    #[error("System call failed: {0}")]
    SystemCallFailed(#[from] std::io::Error),

    /// Calibration failed
    #[error("Calibration failed: {0}")]
    CalibrationFailed(String),

    /// Overflow occurred during time calculation
    #[error("Time calculation overflow")]
    Overflow,
}

/// Trait for CPU time measurement implementations
///
/// All implementations must provide thread-local CPU time measurement
/// with minimal overhead. The trait is object-safe to allow dynamic dispatch.
pub trait CpuTimer: Send + Sync {
    /// Returns the current thread's CPU time in nanoseconds
    ///
    /// This measures the actual CPU time consumed by the current thread,
    /// not wall-clock time. The value is monotonic within a thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform call fails or is not supported.
    fn thread_cpu_time_ns(&self) -> Result<u64, TimingError>;

    /// Returns the calibrated overhead of measurement in nanoseconds
    ///
    /// This value represents the average time taken by the measurement
    /// itself and can be subtracted from measurements for higher accuracy.
    fn calibrated_overhead_ns(&self) -> u64;

    /// Returns the name of the platform implementation
    ///
    /// Useful for debugging and logging which timer is being used.
    fn platform_name(&self) -> &'static str;
}

/// Trait for timers that support calibration
pub trait Calibratable {
    /// Calibrate the timer by measuring its own overhead
    ///
    /// This should be called once during initialization to determine
    /// the measurement overhead that will be compensated for.
    ///
    /// # Errors
    ///
    /// Returns an error if calibration fails due to system call errors.
    fn calibrate(&mut self) -> Result<(), TimingError>;

    /// Measure the overhead of a single timing operation
    ///
    /// Returns the median overhead from multiple samples.
    fn measure_overhead(&self) -> u64;
}

/// Creates a CPU timer appropriate for the current platform
#[must_use]
///
/// This function automatically detects the platform and returns
/// the most accurate timer implementation available.
///
/// # Platform Detection
///
/// The function uses compile-time configuration to select the appropriate
/// implementation. If no platform-specific implementation is available,
/// it falls back to using `std::time::Instant`.
///
/// # Example
///
/// ```rust
/// use tokio_pulse::timing::create_cpu_timer;
///
/// let timer = create_cpu_timer();
/// println!("Using timer: {}", timer.platform_name());
/// ```
pub fn create_cpu_timer() -> Box<dyn CpuTimer> {
    #[cfg(target_os = "linux")]
    {
        match LinuxTimer::new() {
            Ok(mut timer) => {
                let _ = timer.calibrate(); // Ignore calibration errors
                Box::new(timer)
            },
            Err(_) => Box::new(FallbackTimer::new()),
        }
    }

    #[cfg(target_os = "windows")]
    {
        match WindowsTimer::new() {
            Ok(mut timer) => {
                let _ = timer.calibrate(); // Ignore calibration errors
                Box::new(timer)
            },
            Err(_) => Box::new(FallbackTimer::new()),
        }
    }

    #[cfg(target_os = "macos")]
    {
        match MacOsTimer::new() {
            Ok(mut timer) => {
                let _ = timer.calibrate(); // Ignore calibration errors
                Box::new(timer)
            },
            Err(_) => Box::new(FallbackTimer::new()),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Box::new(FallbackTimer::new())
    }
}

/// Information about a CPU timer implementation
#[derive(Debug, Clone)]
pub struct TimerInfo {
    /// Platform name
    pub platform: String,
    /// Expected overhead in nanoseconds
    pub overhead_ns: u64,
    /// Resolution in nanoseconds
    pub resolution_ns: u64,
    /// Whether this measures actual CPU time or wall time
    pub measures_cpu_time: bool,
}

impl fmt::Display for TimerInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Timer[{}: overhead={}ns, resolution={}ns, cpu_time={}]",
            self.platform, self.overhead_ns, self.resolution_ns, self.measures_cpu_time
        )
    }
}

/// Calculates the median of a sorted slice
fn median_of_sorted(values: &[u64]) -> u64 {
    let len = values.len();
    if len == 0 {
        return 0;
    }

    if len % 2 == 0 {
        (values[len / 2 - 1] + values[len / 2]) / 2
    } else {
        values[len / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_timer() {
        let timer = create_cpu_timer();

        // Should always return a valid timer
        assert!(!timer.platform_name().is_empty());

        // Should be able to get time
        let result = timer.thread_cpu_time_ns();
        assert!(result.is_ok(), "Failed to get CPU time: {:?}", result);
    }

    #[test]
    fn test_timer_monotonicity() {
        let timer = create_cpu_timer();

        let mut previous = timer.thread_cpu_time_ns().unwrap();

        // Do some work and verify time increases
        for _ in 0..100 {
            let mut sum = 0u64;
            for i in 0..1000 {
                sum = sum.wrapping_add(i);
            }

            let current = timer.thread_cpu_time_ns().unwrap();
            assert!(current >= previous, "Time went backwards: {} < {}", current, previous);
            previous = current;

            // Prevent optimization
            std::hint::black_box(sum);
        }
    }

    #[test]
    fn test_median_calculation() {
        assert_eq!(median_of_sorted(&[]), 0);
        assert_eq!(median_of_sorted(&[5]), 5);
        assert_eq!(median_of_sorted(&[1, 2]), 1); // (1+2)/2 = 1.5, rounds down
        assert_eq!(median_of_sorted(&[1, 2, 3]), 2);
        assert_eq!(median_of_sorted(&[1, 2, 3, 4]), 2); // (2+3)/2 = 2.5, rounds down
    }
}
