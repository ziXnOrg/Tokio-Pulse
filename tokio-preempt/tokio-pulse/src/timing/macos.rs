#![allow(unsafe_code)] /* Mach kernel APIs require unsafe */

/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */

/* macOS CPU time via thread_info */

use libc::{thread_basic_info, thread_info, thread_info_t};
use mach2::kern_return::KERN_SUCCESS;
use mach2::message::mach_msg_type_number_t;
use mach2::port::mach_port_t;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, Ordering};

// Thread flavor constants
const THREAD_BASIC_INFO: u32 = 3;
#[allow(clippy::cast_possible_truncation)]
const THREAD_BASIC_INFO_COUNT: mach_msg_type_number_t = (std::mem::size_of::<thread_basic_info>()
    / std::mem::size_of::<u32>())
    as mach_msg_type_number_t;

use super::{Calibratable, CpuTimer, TimingError, median_of_sorted};

/// macOS CPU timer using `thread_info`
#[derive(Debug)]
pub struct MacOsTimer {
    /// Calibrated overhead in nanoseconds
    overhead_ns: AtomicU64,
}

impl MacOsTimer {
    /// Creates a new macOS timer
    ///
    /// # Errors
    ///
    /// Returns an error if `thread_info` is not available or fails.
    pub fn new() -> Result<Self, TimingError> {
        let timer = Self {
            overhead_ns: AtomicU64::new(0),
        };

        // Verify that thread_info works
        timer.get_thread_cpu_time_raw()?;

        Ok(timer)
    }

    /// Gets raw CPU time without overhead compensation
    #[inline]
    #[allow(clippy::unused_self)]
    fn get_thread_cpu_time_raw(&self) -> Result<u64, TimingError> {
        // Initialize the info structure
        let mut info = MaybeUninit::<thread_basic_info>::uninit();
        let mut count = THREAD_BASIC_INFO_COUNT;

        // Get current thread handle
        // SAFETY: mach_thread_self returns a pseudo-handle that's always valid
        let thread: mach_port_t = unsafe { mach2::mach_init::mach_thread_self() };

        // Query thread information
        // SAFETY: thread_info is safe to call with valid parameters.
        // We pass a properly sized buffer and correct count.
        let kr = unsafe {
            thread_info(thread, THREAD_BASIC_INFO, info.as_mut_ptr() as thread_info_t, &mut count)
        };

        if kr != KERN_SUCCESS {
            return Err(TimingError::SystemCallFailed(std::io::Error::other(format!(
                "thread_info failed with kern_return: {kr}"
            ))));
        }

        // SAFETY: thread_info has initialized the structure
        let info = unsafe { info.assume_init() };

        // Convert user and system time to nanoseconds
        // Each time is in seconds and microseconds
        #[allow(clippy::cast_sign_loss)]
        let user_ns = (info.user_time.seconds as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add((info.user_time.microseconds as u64).saturating_mul(1_000));

        #[allow(clippy::cast_sign_loss)]
        let system_ns = (info.system_time.seconds as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add((info.system_time.microseconds as u64).saturating_mul(1_000));

        // Total CPU time is user + system
        Ok(user_ns.saturating_add(system_ns))
    }
}

impl CpuTimer for MacOsTimer {
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
        "macOS (thread_info)"
    }
}

impl Calibratable for MacOsTimer {
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
    fn test_macos_timer_creation() {
        let result = MacOsTimer::new();
        assert!(result.is_ok(), "Failed to create macOS timer: {:?}", result);
    }

    #[test]
    fn test_macos_timer_measurement() {
        let timer = MacOsTimer::new().expect("Failed to create timer");

        let time1 = timer.thread_cpu_time_ns().expect("Failed to get time");

        // Do some CPU work
        let mut sum = 0u64;
        for i in 0..100000 {
            sum = sum.wrapping_add(i);
        }
        std::hint::black_box(sum);

        let time2 = timer.thread_cpu_time_ns().expect("Failed to get time");

        // Time should have increased (note: microsecond resolution means
        // we might not see small changes)
        assert!(time2 >= time1, "Time went backwards: {} < {}", time2, time1);
    }

    #[test]
    fn test_calibration() {
        let mut timer = MacOsTimer::new().expect("Failed to create timer");

        // Initial overhead should be 0
        assert_eq!(timer.calibrated_overhead_ns(), 0);

        // Calibrate
        timer.calibrate().expect("Calibration failed");

        // Overhead should be reasonable (< 10000ns for macOS due to syscall overhead)
        // macOS thread_info syscall can be slow especially on CI systems
        let overhead = timer.calibrated_overhead_ns();
        assert!(overhead < 10000, "Overhead too high: {} ns", overhead);
    }

    #[test]
    fn test_monotonicity() {
        let timer = MacOsTimer::new().expect("Failed to create timer");

        let mut previous = timer.thread_cpu_time_ns().expect("Failed to get time");

        // Note: Due to microsecond resolution, we might see the same value multiple times
        for _ in 0..1000 {
            let current = timer.thread_cpu_time_ns().expect("Failed to get time");
            assert!(current >= previous, "Time went backwards: {} < {}", current, previous);
            previous = current;
        }
    }

    #[test]
    fn test_resolution() {
        let timer = MacOsTimer::new().expect("Failed to create timer");

        // Collect multiple samples
        let mut times = Vec::new();
        for _ in 0..100 {
            times.push(timer.thread_cpu_time_ns().expect("Failed to get time"));
        }

        // Find minimum non-zero difference (this is our resolution)
        let mut min_diff = u64::MAX;
        for i in 1..times.len() {
            let diff = times[i].saturating_sub(times[i - 1]);
            if diff > 0 && diff < min_diff {
                min_diff = diff;
            }
        }

        // Resolution should be around 1000ns (1 microsecond) for macOS
        if min_diff != u64::MAX {
            assert!(min_diff >= 1000, "Resolution better than expected: {} ns", min_diff);
        }
    }
}
