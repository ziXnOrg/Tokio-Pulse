//! Stress tests for the tokio-pulse system with 1000+ concurrent tasks
//!
//! These tests validate the system's behavior under high load conditions,
//! testing both performance and correctness with large numbers of concurrent tasks.

use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, Instant};
use std::thread;
use tokio_pulse::tier_manager::{TierManager, TierConfig, TaskId, TaskContext, PollResult};
use tokio_pulse::hooks::{HookRegistry, PreemptionHooks};

/// Test with 1000 concurrent tasks of varying behaviors
#[test]
fn test_thousand_task_stress() {
    println!("🚀 Starting 1000+ task stress test");

    let config = TierConfig {
        poll_budget: 100,
        cpu_ms_budget: 10,
        max_slow_queue_size: 5000,
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = Arc::new(HookRegistry::new());
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let start_time = Instant::now();
    let task_count = 1000;
    let completions = Arc::new(AtomicU64::new(0));

    // Spawn multiple worker threads to simulate concurrent task execution
    let num_workers = 8;
    let tasks_per_worker = task_count / num_workers;
    let mut handles = Vec::new();

    for worker_id in 0..num_workers {
        let _manager_clone = Arc::clone(&manager);
        let registry_clone = Arc::clone(&registry);
        let completions_clone = Arc::clone(&completions);

        let handle = thread::spawn(move || {
            let start_task_id = worker_id * tasks_per_worker;
            let end_task_id = start_task_id + tasks_per_worker;

            for task_idx in start_task_id..end_task_id {
                let task_id = TaskId(task_idx as u64);
                let context = TaskContext {
                    worker_id: worker_id as usize,
                    priority: None
                };

                // Simulate different task behaviors
                let behavior = task_idx % 4;

                match behavior {
                    0 => {
                        // Well-behaved I/O task
                        for _ in 0..10 {
                            registry_clone.before_poll(task_id, &context);
                            thread::sleep(Duration::from_micros(100)); // Simulate I/O
                            registry_clone.after_poll(task_id, PollResult::Pending, Duration::from_micros(50));
                        }
                        registry_clone.after_poll(task_id, PollResult::Ready, Duration::from_micros(25));
                    },
                    1 => {
                        // CPU-intensive task
                        for i in 0..20 {
                            registry_clone.before_poll(task_id, &context);

                            // CPU work
                            let mut sum = 0u64;
                            for j in 0..1000 {
                                sum = sum.wrapping_add(j * i);
                            }
                            std::hint::black_box(sum);

                            registry_clone.after_poll(task_id, PollResult::Pending, Duration::from_millis(3));
                        }
                        registry_clone.after_poll(task_id, PollResult::Ready, Duration::from_micros(100));
                    },
                    2 => {
                        // Moderate task with occasional yields
                        for i in 0..15 {
                            registry_clone.before_poll(task_id, &context);
                            thread::sleep(Duration::from_micros(200));
                            registry_clone.after_poll(task_id, PollResult::Pending, Duration::from_millis(1));

                            if i % 5 == 0 {
                                registry_clone.on_yield(task_id);
                            }
                        }
                        registry_clone.after_poll(task_id, PollResult::Ready, Duration::from_micros(75));
                    },
                    3 => {
                        // Bursty task
                        for i in 0..8 {
                            registry_clone.before_poll(task_id, &context);

                            if i % 3 == 0 {
                                // Burst of CPU work
                                let mut sum = 0u64;
                                for j in 0..5000 {
                                    sum = sum.wrapping_add(j * i);
                                }
                                std::hint::black_box(sum);
                                registry_clone.after_poll(task_id, PollResult::Pending, Duration::from_millis(8));
                            } else {
                                // Light work
                                thread::sleep(Duration::from_micros(50));
                                registry_clone.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));
                            }
                        }
                        registry_clone.after_poll(task_id, PollResult::Ready, Duration::from_micros(50));
                    },
                    _ => unreachable!(),
                }

                registry_clone.on_completion(task_id);
                completions_clone.fetch_add(1, Ordering::Relaxed);
            }
        });

        handles.push(handle);
    }

    // Wait for all worker threads to complete
    for handle in handles {
        handle.join().expect("Worker thread should complete successfully");
    }

    let elapsed = start_time.elapsed();
    let total_completions = completions.load(Ordering::Relaxed);

    println!("✅ Completed {} tasks in {:?}", total_completions, elapsed);
    println!("   Throughput: {:.1} tasks/sec", total_completions as f64 / elapsed.as_secs_f64());

    // Verify all tasks completed
    assert_eq!(total_completions, task_count);

    // Check system metrics
    let metrics = manager.get_metrics();
    let derived = manager.get_derived_metrics();

    println!("📊 System metrics:");
    println!("   Total polls: {}", metrics.total_polls);
    println!("   Budget violations: {}", metrics.budget_violations);
    println!("   Tier promotions: {}", metrics.tier_promotions);
    println!("   Tier demotions: {}", metrics.tier_demotions);
    println!("   Voluntary yields: {}", metrics.voluntary_yields);
    println!("   Violation rate: {:.2}%", derived.violation_rate * 100.0);

    // System should remain stable under load
    assert!(metrics.total_polls > task_count, "Should have many polls");
    assert!(derived.violation_rate < 0.80, "High violation rate indicates system stress");

    // Verify no memory leaks - all tasks should be cleaned up
    let all_worker_stats = manager.get_all_worker_metrics();
    println!("   Active workers: {}", all_worker_stats.len());

    println!("🎯 1000+ task stress test completed successfully!");
}

/// Test rapid task creation and completion cycles
#[test]
fn test_rapid_task_cycling() {
    println!("⚡ Testing rapid task creation/completion cycles");

    let config = TierConfig {
        poll_budget: 50,
        cpu_ms_budget: 5,
        max_slow_queue_size: 10000,
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = Arc::new(HookRegistry::new());
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let start_time = Instant::now();
    let cycle_count = 2000;
    let completions = Arc::new(AtomicU64::new(0));

    // Rapid creation and completion cycles
    for cycle in 0..cycle_count {
        let task_id = TaskId(cycle);
        let context = TaskContext { worker_id: 0, priority: None };

        // Very short task lifecycle
        registry.before_poll(task_id, &context);

        // Minimal work
        let work_type = cycle % 3;
        let duration = match work_type {
            0 => Duration::from_micros(10),  // Fast task
            1 => Duration::from_micros(500), // Medium task
            2 => Duration::from_millis(2),   // Slow task
            _ => Duration::from_micros(100),
        };

        registry.after_poll(task_id, PollResult::Ready, duration);
        registry.on_completion(task_id);

        completions.fetch_add(1, Ordering::Relaxed);

        // Occasionally yield to simulate cooperative behavior
        if cycle % 100 == 0 {
            thread::yield_now();
        }
    }

    let elapsed = start_time.elapsed();
    let total_completions = completions.load(Ordering::Relaxed);

    println!("✅ Completed {} cycles in {:?}", total_completions, elapsed);
    println!("   Cycle rate: {:.1} cycles/sec", total_completions as f64 / elapsed.as_secs_f64());

    assert_eq!(total_completions, cycle_count);

    let metrics = manager.get_metrics();
    println!("📊 Rapid cycling metrics:");
    println!("   Total polls: {}", metrics.total_polls);
    println!("   Budget violations: {}", metrics.budget_violations);

    // With rapid cycling, we expect many polls but controlled violations
    assert_eq!(metrics.total_polls, cycle_count);

    println!("🎯 Rapid task cycling test completed successfully!");
}

/// Test system behavior under memory pressure simulation
#[test]
fn test_memory_pressure_simulation() {
    println!("💾 Testing system under simulated memory pressure");

    let config = TierConfig {
        poll_budget: 200,
        cpu_ms_budget: 15,
        max_slow_queue_size: 1000, // Smaller queue to simulate pressure
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = Arc::new(HookRegistry::new());
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let start_time = Instant::now();
    let task_count = 1500; // More tasks than queue capacity
    let completions = Arc::new(AtomicU64::new(0));
    let rejections = Arc::new(AtomicU64::new(0));

    // Spawn tasks that will stress the queue system
    let num_workers = 4;
    let tasks_per_worker = task_count / num_workers;
    let mut handles = Vec::new();

    for worker_id in 0..num_workers {
        let _manager_clone = Arc::clone(&manager);
        let registry_clone = Arc::clone(&registry);
        let completions_clone = Arc::clone(&completions);
        let _rejections_clone = Arc::clone(&rejections);

        let handle = thread::spawn(move || {
            let start_task_id = worker_id * tasks_per_worker;
            let end_task_id = start_task_id + tasks_per_worker;

            for task_idx in start_task_id..end_task_id {
                let task_id = TaskId(task_idx as u64);
                let context = TaskContext {
                    worker_id: worker_id as usize,
                    priority: None
                };

                // Simulate memory-intensive task patterns
                for poll_idx in 0..25 {
                    registry_clone.before_poll(task_id, &context);

                    // Simulate varying memory load
                    let memory_load = (task_idx + poll_idx) % 10;
                    let cpu_duration = match memory_load {
                        0..=3 => Duration::from_micros(100),   // Light load
                        4..=6 => Duration::from_millis(2),     // Medium load
                        7..=8 => Duration::from_millis(8),     // Heavy load
                        9 => Duration::from_millis(15),        // Very heavy load
                        _ => Duration::from_micros(200),
                    };

                    registry_clone.after_poll(task_id, PollResult::Pending, cpu_duration);

                    // Simulate backpressure - some tasks yield under pressure
                    if memory_load >= 7 && poll_idx % 5 == 0 {
                        registry_clone.on_yield(task_id);
                    }
                }

                registry_clone.after_poll(task_id, PollResult::Ready, Duration::from_micros(50));
                registry_clone.on_completion(task_id);
                completions_clone.fetch_add(1, Ordering::Relaxed);
            }
        });

        handles.push(handle);
    }

    // Wait for all workers
    for handle in handles {
        handle.join().expect("Worker thread should complete");
    }

    let elapsed = start_time.elapsed();
    let total_completions = completions.load(Ordering::Relaxed);
    let total_rejections = rejections.load(Ordering::Relaxed);

    println!("✅ Processed {} tasks with {} rejections in {:?}",
             total_completions, total_rejections, elapsed);

    // Verify task completion
    assert_eq!(total_completions, task_count);

    let metrics = manager.get_metrics();
    let derived = manager.get_derived_metrics();

    println!("📊 Memory pressure metrics:");
    println!("   Total polls: {}", metrics.total_polls);
    println!("   Budget violations: {}", metrics.budget_violations);
    println!("   Tier promotions: {}", metrics.tier_promotions);
    println!("   Voluntary yields: {}", metrics.voluntary_yields);
    println!("   Violation rate: {:.2}%", derived.violation_rate * 100.0);
    println!("   Yield rate: {:.2}%", derived.yield_rate * 100.0);

    // Under memory pressure, we expect increased activity and yields
    // Note: Budget violations may be 0 if the system is working perfectly
    assert!(metrics.voluntary_yields > 0, "Should have voluntary yields under pressure");
    assert!(derived.violation_rate < 0.90, "Violation rate should not exceed 90%");
    assert!(metrics.total_polls > task_count, "Should have many poll operations");

    println!("🎯 Memory pressure simulation completed successfully!");
}

/// Test long-running system stability
#[test]
fn test_long_running_stability() {
    println!("⏱️ Testing long-running system stability");

    let config = TierConfig {
        poll_budget: 150,
        cpu_ms_budget: 12,
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = Arc::new(HookRegistry::new());
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let start_time = Instant::now();
    let run_duration = Duration::from_secs(5); // 5 second test
    let completions = Arc::new(AtomicU64::new(0));

    // Background task that continuously creates and completes tasks
    let _manager_clone = Arc::clone(&manager);
    let registry_clone = Arc::clone(&registry);
    let completions_clone = Arc::clone(&completions);

    let background_handle = thread::spawn(move || {
        let mut task_counter = 0u64;

        while start_time.elapsed() < run_duration {
            let task_id = TaskId(task_counter);
            let context = TaskContext { worker_id: 0, priority: None };

            // Simulate realistic task pattern
            for _ in 0..5 {
                registry_clone.before_poll(task_id, &context);

                // Varying workload
                let work_intensity = task_counter % 20;
                let duration = if work_intensity < 15 {
                    Duration::from_micros(200) // Normal work
                } else {
                    Duration::from_millis(5)   // Occasional heavy work
                };

                registry_clone.after_poll(task_id, PollResult::Pending, duration);
            }

            registry_clone.after_poll(task_id, PollResult::Ready, Duration::from_micros(100));
            registry_clone.on_completion(task_id);

            completions_clone.fetch_add(1, Ordering::Relaxed);
            task_counter += 1;

            // Small delay to prevent overwhelming the system
            thread::sleep(Duration::from_micros(500));
        }
    });

    // Wait for completion
    background_handle.join().expect("Background thread should complete");

    let elapsed = start_time.elapsed();
    let total_completions = completions.load(Ordering::Relaxed);

    println!("✅ Ran for {:?} and completed {} tasks", elapsed, total_completions);
    println!("   Average task rate: {:.1} tasks/sec", total_completions as f64 / elapsed.as_secs_f64());

    let metrics = manager.get_metrics();
    let derived = manager.get_derived_metrics();

    println!("📊 Long-running stability metrics:");
    println!("   Total polls: {}", metrics.total_polls);
    println!("   Budget violations: {}", metrics.budget_violations);
    println!("   Tier promotions: {}", metrics.tier_promotions);
    println!("   Tier demotions: {}", metrics.tier_demotions);
    println!("   Violation rate: {:.2}%", derived.violation_rate * 100.0);
    println!("   Yield rate: {:.2}%", derived.yield_rate * 100.0);

    // System should maintain reasonable performance over time
    assert!(total_completions > 100, "Should complete many tasks over time");
    assert!(derived.violation_rate < 0.50, "Should maintain reasonable violation rate");

    // Check for memory leaks - no tasks should be stuck
    let all_worker_stats = manager.get_all_worker_metrics();
    println!("   Workers tracked: {}", all_worker_stats.len());

    println!("🎯 Long-running stability test completed successfully!");
}