#![forbid(unsafe_code)]

//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
// Fallback timer using std::time::Instant (wall time, not CPU time)
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;                                                                     

use super::{Calibratable, CpuTimer, TimingError, median_of_sorted};

/// Fallback timer using `std::time::Instant`
///
/// WARNING: This measures wall time, not CPU time!
pub struct FallbackTimer {
    /// Start time of the timer
    start_time: Instant,
    /// Calibrated overhead in nanoseconds
    overhead_ns: AtomicU64,
    /// Whether we've logged a warning about using fallback
    warned: std::sync::Once,
}

impl FallbackTimer {
    /// Creates a new fallback timer
    pub fn new() -> Self {
        let timer = Self {
            start_time: Instant::now(),
            overhead_ns: AtomicU64::new(0),
            warned: std::sync::Once::new(),
        };

        // Calibrate on creation
        let mut calibratable = Self {
            start_time: timer.start_time,
            overhead_ns: AtomicU64::new(0),
            warned: std::sync::Once::new(),
        };
        let _ = calibratable.calibrate(); // Ignore errors
        timer
            .overhead_ns
            .store(calibratable.overhead_ns.load(Ordering::Relaxed), Ordering::Relaxed);

        timer
    }

    /// Logs a warning about using the fallback timer (once)
    fn warn_once(&self) {
        self.warned.call_once(|| {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Using fallback timer with Instant::now(). \
                This measures wall time, not CPU time, and may be inaccurate. \
                Consider using a platform with native CPU time support."
            );

            // Also log to stderr if tracing is not available
            #[cfg(not(feature = "tracing"))]
            eprintln!(
                "WARNING: Using fallback timer with Instant::now(). \
                This measures wall time, not CPU time, and may be inaccurate."
            );
        });
    }

    /// Gets raw time without overhead compensation
    #[inline]
    fn get_time_raw(&self) -> u64 {
        #[allow(clippy::cast_possible_truncation)]
        let nanos = self.start_time.elapsed().as_nanos() as u64;
        nanos
    }
}

impl CpuTimer for FallbackTimer {
    #[inline]
    fn thread_cpu_time_ns(&self) -> Result<u64, TimingError> {
        // Warn on first use
        self.warn_once();

        let raw_time = self.get_time_raw();
        let overhead = self.overhead_ns.load(Ordering::Relaxed);

        // Subtract calibrated overhead, but don't go negative
        Ok(raw_time.saturating_sub(overhead))
    }

    #[inline]
    fn calibrated_overhead_ns(&self) -> u64 {
        self.overhead_ns.load(Ordering::Relaxed)
    }

    fn platform_name(&self) -> &'static str {
        "Fallback (Instant::now - WARNING: measures wall time, not CPU time)"
    }
}

impl Calibratable for FallbackTimer {
    fn calibrate(&mut self) -> Result<(), TimingError> {
        const SAMPLES: usize = 1000;
        let mut overheads = Vec::with_capacity(SAMPLES);

        // Warm up
        for _ in 0..100 {
            let _ = self.get_time_raw();
        }

        // Measure overhead
        for _ in 0..SAMPLES {
            let start = self.get_time_raw();
            let end = self.get_time_raw();

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
            let start = self.get_time_raw();
            let end = self.get_time_raw();
            overheads.push(end.saturating_sub(start));
        }

        if overheads.is_empty() {
            return 0;
        }

        overheads.sort_unstable();
        median_of_sorted(&overheads)
    }
}

impl Default for FallbackTimer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_timer_creation() {
        let timer = FallbackTimer::new();

        // Should always succeed
        let result = timer.thread_cpu_time_ns();
        assert!(result.is_ok(), "Failed to get time: {:?}", result);
    }

    #[test]
    fn test_fallback_timer_measurement() {
        let timer = FallbackTimer::new();

        let start_time = timer.thread_cpu_time_ns().expect("Failed to get time");

        // Do some work (both CPU and sleeping)
        let mut sum = 0u64;
        for i in 0..100_000 {
            sum = sum.wrapping_add(i);
        }
        std::hint::black_box(sum);

        // Sleep to demonstrate wall time measurement
        std::thread::sleep(std::time::Duration::from_millis(1));

        let end_time = timer.thread_cpu_time_ns().expect("Failed to get time");

        // Time should have increased (includes sleep time)
        assert!(end_time > start_time, "Time did not increase: {} <= {}", end_time, start_time);

        // Should be at least 1ms due to sleep
        assert!(end_time - start_time >= 1_000_000, "Time difference too small: {} ns", end_time - start_time);
    }

    #[test]
    fn test_calibration() {
        let mut timer = FallbackTimer::new();

        // Should already be calibrated from new()
        let initial_overhead = timer.calibrated_overhead_ns();

        // Recalibrate
        timer.calibrate().expect("Calibration failed");

        // Overhead should be reasonable (< 5000ns for Instant)
        let overhead = timer.calibrated_overhead_ns();
        assert!(overhead < 5000, "Overhead too high: {} ns", overhead);

        // Should be similar to initial calibration
        let diff = (overhead as i64 - initial_overhead as i64).abs();
        assert!(diff < 1000, "Calibration inconsistent: {} vs {}", initial_overhead, overhead);
    }

    #[test]
    fn test_monotonicity() {
        let timer = FallbackTimer::new();

        let mut previous = timer.thread_cpu_time_ns().expect("Failed to get time");

        for _ in 0..1000 {
            let current = timer.thread_cpu_time_ns().expect("Failed to get time");
            assert!(current >= previous, "Time went backwards: {} < {}", current, previous);
            previous = current;
        }
    }

    #[test]
    fn test_platform_name() {
        let timer = FallbackTimer::new();
        assert!(timer.platform_name().contains("Fallback"));
        assert!(timer.platform_name().contains("WARNING"));
    }
}
