#![allow(unsafe_code)] // clock_gettime requires unsafe

//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
// Linux CPU timing via clock_gettime
use libc::{CLOCK_THREAD_CPUTIME_ID, clock_gettime, timespec};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Calibratable, CpuTimer, TimingError, median_of_sorted};

// Linux timer implementation
#[derive(Debug)]
pub struct LinuxTimer {
    // Calibrated overhead (ns)
    overhead_ns: AtomicU64,
}

impl LinuxTimer {
    // Create Linux timer
    pub fn new() -> Result<Self, TimingError> {
        let timer = Self {
            overhead_ns: AtomicU64::new(0),
        };

        // Verify clock_gettime
        timer.get_thread_cpu_time_raw()?;

        Ok(timer)
    }

    // Raw CPU time without compensation
    #[inline]
    fn get_thread_cpu_time_raw(&self) -> Result<u64, TimingError> {
        let mut ts = timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };

        // SAFETY: Valid timespec and clock ID
        let ret = unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) };

        if ret == 0 {
            // Convert to nanoseconds
            let secs_ns = (ts.tv_sec as u64).saturating_mul(1_000_000_000);
            let total_ns = secs_ns.saturating_add(ts.tv_nsec as u64);
            Ok(total_ns)
        } else {
            Err(TimingError::SystemCallFailed(std::io::Error::last_os_error()))
        }
    }
}

impl CpuTimer for LinuxTimer {
    #[inline]
    fn thread_cpu_time_ns(&self) -> Result<u64, TimingError> {
        let raw_time = self.get_thread_cpu_time_raw()?;
        let overhead = self.overhead_ns.load(Ordering::Relaxed);

        // Subtract overhead
        Ok(raw_time.saturating_sub(overhead))
    }

    #[inline]
    fn calibrated_overhead_ns(&self) -> u64 {
        self.overhead_ns.load(Ordering::Relaxed)
    }

    fn platform_name(&self) -> &'static str {
        "Linux (clock_gettime)"
    }
}

impl Calibratable for LinuxTimer {
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
    fn test_linux_timer_creation() {
        let result = LinuxTimer::new();
        assert!(result.is_ok(), "Failed to create Linux timer: {:?}", result);
    }

    #[test]
    fn test_linux_timer_measurement() {
        let timer = LinuxTimer::new().expect("Failed to create timer");

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
        let mut timer = LinuxTimer::new().expect("Failed to create timer");

        // Initial overhead should be 0
        assert_eq!(timer.calibrated_overhead_ns(), 0);

        // Calibrate
        timer.calibrate().expect("Calibration failed");

        // Overhead should be reasonable (< 1000ns)
        let overhead = timer.calibrated_overhead_ns();
        assert!(overhead < 1000, "Overhead too high: {} ns", overhead);
    }

    #[test]
    fn test_monotonicity() {
        let timer = LinuxTimer::new().expect("Failed to create timer");

        let mut previous = timer.thread_cpu_time_ns().expect("Failed to get time");

        for _ in 0..1000 {
            let current = timer.thread_cpu_time_ns().expect("Failed to get time");
            assert!(current >= previous, "Time went backwards: {} < {}", current, previous);
            previous = current;
        }
    }
}
