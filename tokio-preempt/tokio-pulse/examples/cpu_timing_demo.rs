//! Demo of CPU timing functionality

use std::time::Duration;
use tokio_pulse::budget::TaskBudget;
use tokio_pulse::timing::create_cpu_timer;

fn main() {
    println!("CPU Timing Demo\n");

    // Create a CPU timer
    let timer = create_cpu_timer();
    println!("Using timer: {}", timer.platform_name());
    println!("Calibrated overhead: {} ns\n", timer.calibrated_overhead_ns());

    // Demonstrate CPU time vs wall time
    println!("CPU Time vs Wall Time:");
    let start_cpu = timer.thread_cpu_time_ns().unwrap();
    let start_wall = std::time::Instant::now();

    // Do some CPU work
    let mut sum = 0u64;
    for i in 0..10_000_000 {
        sum = sum.wrapping_add(i);
    }
    std::hint::black_box(sum);

    let cpu_after_work = timer.thread_cpu_time_ns().unwrap();
    let wall_after_work = start_wall.elapsed();

    // Sleep (no CPU usage)
    std::thread::sleep(Duration::from_millis(100));

    let cpu_after_sleep = timer.thread_cpu_time_ns().unwrap();
    let wall_after_sleep = start_wall.elapsed();

    println!("After CPU work:");
    println!("  CPU time:  {:>10} ns", cpu_after_work - start_cpu);
    println!("  Wall time: {:>10} ns", wall_after_work.as_nanos());

    println!("\nAfter 100ms sleep:");
    println!("  CPU time:  {:>10} ns (minimal increase)", cpu_after_sleep - start_cpu);
    println!("  Wall time: {:>10} ns (includes sleep)", wall_after_sleep.as_nanos());

    // Demonstrate integration with TaskBudget
    println!("\n\nIntegration with TaskBudget:");
    let budget = TaskBudget::new(1000);
    let timer = create_cpu_timer();

    println!("Initial budget: {}", budget.remaining());

    // Simulate task execution with timing
    for iteration in 1..=5 {
        let iter_start = timer.thread_cpu_time_ns().unwrap();

        // Simulate work
        for _ in 0..100 {
            if budget.consume() {
                println!("Iteration {}: Budget exhausted! Should yield.", iteration);
                budget.reset(1000);
                break;
            }

            // Do some work
            let mut work = 0u64;
            for i in 0..10000 {
                work = work.wrapping_add(i);
            }
            std::hint::black_box(work);
        }

        let iter_end = timer.thread_cpu_time_ns().unwrap();
        let cpu_used = iter_end - iter_start;

        println!(
            "Iteration {}: CPU time = {:.2} ms, Remaining budget = {}",
            iteration,
            cpu_used as f64 / 1_000_000.0,
            budget.remaining()
        );
    }

    println!("\nDemo complete!");
}
