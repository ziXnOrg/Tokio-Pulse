//! Integration tests for cross-platform CPU timing

use std::thread;
use std::time::Duration;
use tokio_pulse::timing::create_cpu_timer;

#[test]
fn test_timer_creation_and_basic_usage() {
    let timer = create_cpu_timer();

    // Should have a valid platform name
    let platform = timer.platform_name();
    assert!(!platform.is_empty());
    println!("Using timer: {}", platform);

    // Should be able to get CPU time
    let time1 = timer.thread_cpu_time_ns().expect("Failed to get initial CPU time");

    // Do some CPU-intensive work
    let mut sum = 0u64;
    for i in 0..1_000_000 {
        sum = sum.wrapping_add(i);
    }
    std::hint::black_box(sum);

    let time2 = timer.thread_cpu_time_ns().expect("Failed to get final CPU time");

    // Time should have increased
    assert!(time2 > time1, "CPU time did not increase");

    let elapsed = time2 - time1;
    println!("CPU time elapsed: {} ns ({:.3} ms)", elapsed, elapsed as f64 / 1_000_000.0);
}

#[test]
fn test_thread_isolation() {
    let timer = create_cpu_timer();

    // Get initial CPU time
    let initial = timer.thread_cpu_time_ns().expect("Failed to get initial time");

    // Spawn a CPU-intensive thread
    let handle = thread::spawn(|| {
        let mut sum = 0u64;
        for i in 0..10_000_000 {
            sum = sum.wrapping_add(i);
        }
        std::hint::black_box(sum);
    });

    // Sleep to ensure the other thread runs
    thread::sleep(Duration::from_millis(10));

    // Our thread's CPU time shouldn't increase much
    let after_sleep = timer.thread_cpu_time_ns().expect("Failed to get time after sleep");

    // Wait for the thread to complete
    handle.join().expect("Thread failed");

    let final_time = timer.thread_cpu_time_ns().expect("Failed to get final time");

    // CPU time during sleep should be minimal
    let sleep_cpu = after_sleep.saturating_sub(initial);
    let total_cpu = final_time.saturating_sub(initial);

    println!("CPU time during sleep: {} ns", sleep_cpu);
    println!("Total CPU time: {} ns", total_cpu);

    // During sleep, CPU time should be very low (< 1ms)
    // Note: Fallback timer will fail this test as it measures wall time
    if !timer.platform_name().contains("Fallback") {
        assert!(sleep_cpu < 1_000_000, "Too much CPU time during sleep: {} ns", sleep_cpu);
    }
}

#[test]
fn test_monotonicity_across_measurements() {
    let timer = create_cpu_timer();
    let mut previous = timer.thread_cpu_time_ns().expect("Failed to get initial time");

    for i in 0..1000 {
        // Do minimal work
        std::hint::black_box(i);

        let current = timer.thread_cpu_time_ns().expect("Failed to get time");

        assert!(
            current >= previous,
            "Time went backwards at iteration {}: {} < {}",
            i,
            current,
            previous
        );

        previous = current;
    }
}

#[test]
fn test_overhead_is_reasonable() {
    let timer = create_cpu_timer();
    let overhead = timer.calibrated_overhead_ns();

    println!("Calibrated overhead: {} ns", overhead);

    // Overhead should be reasonable based on platform
    let max_expected = if timer.platform_name().contains("Linux") {
        100 // Linux should be very fast
    } else if timer.platform_name().contains("Windows") {
        200 // Windows is slightly slower
    } else if timer.platform_name().contains("macOS") {
        5000 // macOS has much higher syscall overhead due to thread_info
    } else {
        10000 // Fallback can be quite slow
    };

    assert!(
        overhead <= max_expected,
        "Overhead {} ns exceeds maximum {} ns for {}",
        overhead,
        max_expected,
        timer.platform_name()
    );
}

#[test]
fn test_concurrent_timers() {
    // Create multiple timers
    let timers: Vec<_> = (0..4).map(|_| create_cpu_timer()).collect();

    // Each should work independently
    let handles: Vec<_> = timers
        .into_iter()
        .enumerate()
        .map(|(id, timer)| {
            thread::spawn(move || {
                let start = timer.thread_cpu_time_ns().expect("Failed to get start time");

                // Do some work
                let mut sum = 0u64;
                for i in 0..1_000_000 {
                    sum = sum.wrapping_add(i);
                }
                std::hint::black_box(sum);

                let end = timer.thread_cpu_time_ns().expect("Failed to get end time");

                let elapsed = end - start;
                println!("Thread {} CPU time: {} ns", id, elapsed);

                // Should have used some CPU time
                assert!(elapsed > 0, "Thread {} used no CPU time", id);

                elapsed
            })
        })
        .collect();

    // Wait for all threads
    let results: Vec<_> = handles.into_iter().map(|h| h.join().expect("Thread failed")).collect();

    // All threads should have reported similar CPU usage (within 100x)
    // Note: Thread scheduling can vary significantly on CI systems
    let min = *results.iter().min().unwrap();
    let max = *results.iter().max().unwrap();

    assert!(max < min * 100, "CPU time variance too high: min={}, max={}", min, max);
}

#[test]
fn test_high_frequency_measurements() {
    let timer = create_cpu_timer();
    let mut measurements = Vec::with_capacity(10000);

    let start = std::time::Instant::now();

    // Take many measurements rapidly
    for _ in 0..10000 {
        measurements.push(timer.thread_cpu_time_ns().expect("Failed to get time"));
    }

    let elapsed = start.elapsed();

    println!(
        "Took 10000 measurements in {:?} ({:.1} ns per measurement)",
        elapsed,
        elapsed.as_nanos() as f64 / 10000.0
    );

    // Average time per measurement should be reasonable for the platform
    let max_ns_per_measurement = if timer.platform_name().contains("Linux") {
        100
    } else if timer.platform_name().contains("Windows") {
        200
    } else if timer.platform_name().contains("macOS") {
        1000 // macOS thread_info is slower
    } else {
        2000 // Fallback
    };

    assert!(
        elapsed.as_nanos() / 10000 < max_ns_per_measurement,
        "Measurements too slow: {:?} for 10000 measurements ({}ns per measurement, max allowed: {}ns)",
        elapsed,
        elapsed.as_nanos() / 10000,
        max_ns_per_measurement
    );

    // Verify measurements are monotonic
    for i in 1..measurements.len() {
        assert!(
            measurements[i] >= measurements[i - 1],
            "Non-monotonic at index {}: {} < {}",
            i,
            measurements[i],
            measurements[i - 1]
        );
    }
}

#[cfg(feature = "tokio_unstable")]
#[tokio::test]
async fn test_async_context_usage() {
    let timer = create_cpu_timer();

    let initial = timer.thread_cpu_time_ns().expect("Failed to get initial time");

    // Do some async work
    let mut sum = 0u64;
    for i in 0..1_000_000 {
        sum = sum.wrapping_add(i);
        if i % 100_000 == 0 {
            tokio::task::yield_now().await;
        }
    }
    std::hint::black_box(sum);

    let final_time = timer.thread_cpu_time_ns().expect("Failed to get final time");

    let elapsed = final_time - initial;
    println!("CPU time in async context: {} ns", elapsed);

    // Should have used CPU time
    assert!(elapsed > 0, "No CPU time used in async context");
}
