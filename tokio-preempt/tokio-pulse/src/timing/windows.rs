#![allow(unsafe_code)] /* Windows APIs require unsafe */

/*
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */
/* Windows CPU time via QueryThreadCycleTime */
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::System::{
    Performance::QueryPerformanceFrequency,
    Threading::{GetCurrentThread, QueryThreadCycleTime},
};

use super::{Calibratable, CpuTimer, TimingError, median_of_sorted};

/// Windows CPU timer using `QueryThreadCycleTime`
#[derive(Debug)]
pub struct WindowsTimer {
    /// CPU frequency for cycle-to-time conversion
    frequency: u64,
    /// Calibrated overhead in nanoseconds
    overhead_ns: AtomicU64,
}

impl WindowsTimer {
    /// Creates a new Windows timer
    ///
    /// # Errors
    ///
    /// Returns an error if QueryPerformanceFrequency or QueryThreadCycleTime fails.
    pub fn new() -> Result<Self, TimingError> {
        // Get the performance frequency for time conversion
        let frequency = Self::get_performance_frequency()?;

        let timer = Self {
            frequency,
            overhead_ns: AtomicU64::new(0),
        };

        // Verify that QueryThreadCycleTime works
        timer.get_thread_cycles()?;

        Ok(timer)
    }

    /// Gets the system's performance frequency
    fn get_performance_frequency() -> Result<u64, TimingError> {
        let mut frequency = 0i64;

        // SAFETY: QueryPerformanceFrequency is safe to call with a valid pointer
        let ret = unsafe { QueryPerformanceFrequency(&mut frequency) };

        if ret != 0 {
            Ok(frequency as u64)
        } else {
            Err(TimingError::SystemCallFailed(std::io::Error::last_os_error()))
        }
    }

    /// Gets the current thread's cycle count
    #[inline]
    fn get_thread_cycles(&self) -> Result<u64, TimingError> {
        let mut cycles = 0u64;

        // SAFETY: GetCurrentThread returns a pseudo-handle that doesn't need to be closed.
        // QueryThreadCycleTime is safe to call with a valid thread handle and pointer.
        let ret = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };

        if ret != 0 {
            Ok(cycles)
        } else {
            Err(TimingError::SystemCallFailed(std::io::Error::last_os_error()))
        }
    }

    /// Converts CPU cycles to nanoseconds
    #[inline]
    fn cycles_to_nanoseconds(&self, cycles: u64) -> u64 {
        // Convert cycles to nanoseconds using the frequency
        // cycles * 1_000_000_000 / frequency
        // Use saturating operations to prevent overflow
        cycles.saturating_mul(1_000_000_000).saturating_div(self.frequency)
    }

    /// Gets raw CPU time without overhead compensation
    #[inline]
    fn get_thread_cpu_time_raw(&self) -> Result<u64, TimingError> {
        let cycles = self.get_thread_cycles()?;
        Ok(self.cycles_to_nanoseconds(cycles))
    }
}

impl CpuTimer for WindowsTimer {
    #[inline]
    fn thread_cpu_time_ns(&self) -> Result<u64, TimingError> {
        let raw_time = self.get_thread_cpu_time_raw()?;
        let overhead = self.overhead_ns.load(Ordering::Relaxed);

        // Subtract calibrated overhead, but don't go negative
        Ok(raw_time.saturating_sub(overhead))
    }

    #[inline]
    fn calibrated_overhead_ns(&self) -> u64 {
        self.overhead_ns.load(Ordering::Relaxed)
    }

    fn platform_name(&self) -> &'static str {
        "Windows (QueryThreadCycleTime)"
    }
}

impl Calibratable for WindowsTimer {
    fn calibrate(&mut self) -> Result<(), TimingError> {
        const SAMPLES: usize = 1000;
        let mut overheads = Vec::with_capacity(SAMPLES);

        // Warm up
        for _ in 0..100 {
            let _ = self.get_thread_cpu_time_raw();
        }

        // Measure overhead
        for _ in 0..SAMPLES {
            let start = self.get_thread_cpu_time_raw()?;
            let end = self.get_thread_cpu_time_raw()?;

            // The difference between consecutive calls is our overhead
            let overhead = end.saturating_sub(start);
            overheads.push(overhead);
        }

        // Sort and take median to reduce noise
        overheads.sort_unstable();
        let median_overhead = median_of_sorted(&overheads);

        // Store the calibrated overhead
        self.overhead_ns.store(median_overhead, Ordering::Relaxed);

        Ok(())
    }

    fn measure_overhead(&self) -> u64 {
        const SAMPLES: usize = 100;
        let mut overheads = Vec::with_capacity(SAMPLES);

        for _ in 0..SAMPLES {
            if let (Ok(start), Ok(end)) =
                (self.get_thread_cpu_time_raw(), self.get_thread_cpu_time_raw())
            {
                overheads.push(end.saturating_sub(start));
            }
        }

        if overheads.is_empty() {
            return 0;
        }

        overheads.sort_unstable();
        median_of_sorted(&overheads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_windows_timer_creation() {
        let result = WindowsTimer::new();
        assert!(result.is_ok(), "Failed to create Windows timer: {:?}", result);
    }

    #[test]
    fn test_windows_timer_measurement() {
        let timer = WindowsTimer::new().expect("Failed to create timer");

        let time1 = timer.thread_cpu_time_ns().expect("Failed to get time");

        // Do some CPU work
        let mut sum = 0u64;
        for i in 0..100_000 {
            sum = sum.wrapping_add(i);
        }
        std::hint::black_box(sum);

        let time2 = timer.thread_cpu_time_ns().expect("Failed to get time");

        // Time should have increased
        assert!(time2 > time1, "Time did not increase: {} <= {}", time2, time1);
    }

    #[test]
    fn test_calibration() {
        let mut timer = WindowsTimer::new().expect("Failed to create timer");

        // Initial overhead should be 0
        assert_eq!(timer.calibrated_overhead_ns(), 0);

        // Calibrate
        timer.calibrate().expect("Calibration failed");

        // Overhead should be reasonable (< 1000ns)
        let overhead = timer.calibrated_overhead_ns();
        assert!(overhead < 1000, "Overhead too high: {} ns", overhead);
    }

    #[test]
    fn test_frequency() {
        let frequency = WindowsTimer::get_performance_frequency().expect("Failed to get frequency");

        // Frequency should be reasonable (typically in MHz to GHz range)
        assert!(frequency > 1_000_000, "Frequency too low: {}", frequency);
        assert!(frequency < 10_000_000_000, "Frequency too high: {}", frequency);
    }

    #[test]
    fn test_monotonicity() {
        let timer = WindowsTimer::new().expect("Failed to create timer");

        let mut previous = timer.thread_cpu_time_ns().expect("Failed to get time");

        for _ in 0..1000 {
            let current = timer.thread_cpu_time_ns().expect("Failed to get time");
            assert!(current >= previous, "Time went backwards: {} < {}", current, previous);
            previous = current;
        }
    }
}
