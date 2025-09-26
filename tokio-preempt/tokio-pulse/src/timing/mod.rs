#![allow(unsafe_code)] /* OS-level timing APIs require unsafe */

/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */
/* Cross-platform CPU time measurement */
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

/* CPU timing errors */
#[derive(Debug, Error)]
pub enum TimingError {
    /* Platform not supported */
    #[error("Platform not supported: {0}")]
    PlatformNotSupported(String),

    /* System call failure */
    #[error("System call failed: {0}")]
    SystemCallFailed(#[from] std::io::Error),

    /* Calibration failure */
    #[error("Calibration failed: {0}")]
    CalibrationFailed(String),

    /* Time calculation overflow */
    #[error("Time calculation overflow")]
    Overflow,
}

/* CPU timer trait */
pub trait CpuTimer: Send + Sync {
    /* Get thread CPU time (ns) */
    fn thread_cpu_time_ns(&self) -> Result<u64, TimingError>;

    /* Measurement overhead (ns) */
    fn calibrated_overhead_ns(&self) -> u64;

    /* Platform name */
    fn platform_name(&self) -> &'static str;
}

/* Calibratable timer trait */
pub trait Calibratable {
    /* Calibrate measurement overhead */
    fn calibrate(&mut self) -> Result<(), TimingError>;

    /* Measure operation overhead */
    fn measure_overhead(&self) -> u64;
}

/* Create platform-specific CPU timer */
#[must_use]
pub fn create_cpu_timer() -> Box<dyn CpuTimer> {
    #[cfg(target_os = "linux")]
    {
        match LinuxTimer::new() {
            Ok(mut timer) => {
                let _ = timer.calibrate(); /* Ignore errors */
                Box::new(timer)
            },
            Err(_) => Box::new(FallbackTimer::new()),
        }
    }

    #[cfg(target_os = "windows")]
    {
        match WindowsTimer::new() {
            Ok(mut timer) => {
                let _ = timer.calibrate(); /* Ignore errors */
                Box::new(timer)
            },
            Err(_) => Box::new(FallbackTimer::new()),
        }
    }

    #[cfg(target_os = "macos")]
    {
        match MacOsTimer::new() {
            Ok(mut timer) => {
                let _ = timer.calibrate(); /* Ignore errors */
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

/* Timer implementation info */
#[derive(Debug, Clone)]
pub struct TimerInfo {
    pub platform: String,        /* Platform name */
    pub overhead_ns: u64,        /* Expected overhead (ns) */
    pub resolution_ns: u64,      /* Timer resolution (ns) */
    pub measures_cpu_time: bool, /* CPU vs wall time */
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

/* Calculate median of sorted values */
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

        /* Should return valid timer */
        assert!(!timer.platform_name().is_empty());

        /* Should get CPU time */
        let result = timer.thread_cpu_time_ns();
        assert!(result.is_ok(), "Failed to get CPU time: {:?}", result);
    }

    #[test]
    fn test_timer_monotonicity() {
        let timer = create_cpu_timer();

        let mut previous = timer.thread_cpu_time_ns().unwrap();

        /* Verify monotonicity */
        for _ in 0..100 {
            let mut sum = 0u64;
            for i in 0..1000 {
                sum = sum.wrapping_add(i);
            }

            let current = timer.thread_cpu_time_ns().unwrap();
            assert!(current >= previous, "Time went backwards: {} < {}", current, previous);
            previous = current;

            /* Prevent optimization */
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
