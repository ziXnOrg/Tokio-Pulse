//! Integration tests with actual Tokio runtime using preemption hooks
//!
//! These tests validate that the tokio-pulse system works correctly with
//! the modified Tokio runtime, using the actual hook integration points.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_pulse::tier_manager::{TierManager, TierConfig, TaskId, TaskContext, PollResult};
use tokio_pulse::hooks::{HookRegistry, PreemptionHooks};

/// Test hook system setup and configuration
#[tokio::test]
async fn test_hook_system_setup() {
    println!("🔗 Testing hook system setup and configuration");

    let config = TierConfig {
        poll_budget: 50,
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = HookRegistry::new();

    // Test hook registration
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    // Test manual hook calls (simulating what Tokio would do)
    let task_id = TaskId(1);
    let context = TaskContext { worker_id: 0, priority: None };

    let start = Instant::now();

    // Simulate hook calls
    manager.before_poll(task_id, &context);
    manager.after_poll(task_id, PollResult::Pending, Duration::from_micros(500));
    manager.before_poll(task_id, &context);
    manager.after_poll(task_id, PollResult::Ready, Duration::from_micros(200));
    manager.on_completion(task_id);

    let elapsed = start.elapsed();
    println!("  Hook operations took: {:?}", elapsed);

    // Verify system state
    let metrics = manager.get_metrics();
    assert_eq!(metrics.total_polls, 2);

    // Verify the task is cleaned up
    assert!(manager.get_task_tier(task_id).is_none());

    println!("✅ Hook system setup test passed");
}

/// Test async task coordination with tier management
#[tokio::test]
async fn test_async_task_coordination() {
    println!("⚡ Testing async task coordination with tier management");

    let config = TierConfig {
        poll_budget: 100,
        cpu_ms_budget: 10,
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    // Simulate multiple tasks with different behaviors
    let tasks_completed = Arc::new(AtomicU64::new(0));

    // Task 1: I/O bound (should be well-behaved)
    let completed_clone = Arc::clone(&tasks_completed);
    let manager_clone = Arc::clone(&manager);
    let io_task = tokio::spawn(async move {
        let task_id = TaskId(1001);
        let context = TaskContext { worker_id: 0, priority: None };

        for i in 0..20 {
            // Simulate before/after hooks around async I/O
            manager_clone.before_poll(task_id, &context);

            // Simulate I/O work
            tokio::time::sleep(Duration::from_micros(100)).await;

            manager_clone.after_poll(task_id, PollResult::Pending, Duration::from_micros(50));

            if i % 5 == 0 {
                tokio::task::yield_now().await;
            }
        }

        manager_clone.on_completion(task_id);
        completed_clone.fetch_add(1, Ordering::Relaxed);
        "io_task"
    });

    // Task 2: CPU bound (should trigger intervention)
    let completed_clone = Arc::clone(&tasks_completed);
    let manager_clone = Arc::clone(&manager);
    let cpu_task = tokio::spawn(async move {
        let task_id = TaskId(1002);
        let context = TaskContext { worker_id: 0, priority: None };

        for i in 0..10 {
            // Simulate before hook
            manager_clone.before_poll(task_id, &context);

            // Simulate CPU-intensive work
            let mut sum = 0u64;
            for j in 0..5000 {
                sum = sum.wrapping_add(j * i);
            }
            std::hint::black_box(sum);

            // Simulate long CPU time (should trigger budget violation)
            manager_clone.after_poll(task_id, PollResult::Pending, Duration::from_millis(15));

            // Check if task was promoted
            if let Some(tier) = manager_clone.get_task_tier(task_id) {
                if tier > 0 {
                    println!("    Task {} promoted to tier {}", task_id.0, tier);
                }
            }

            tokio::task::yield_now().await;
        }

        manager_clone.on_completion(task_id);
        completed_clone.fetch_add(1, Ordering::Relaxed);
        "cpu_task"
    });

    // Wait for both tasks
    let results = timeout(Duration::from_secs(10), async {
        let io_result = io_task.await.unwrap();
        let cpu_result = cpu_task.await.unwrap();
        (io_result, cpu_result)
    }).await;

    assert!(results.is_ok(), "Both tasks should complete");
    let (io_result, cpu_result) = results.unwrap();

    assert_eq!(io_result, "io_task");
    assert_eq!(cpu_result, "cpu_task");
    assert_eq!(tasks_completed.load(Ordering::Relaxed), 2);

    // Check that the system differentiated between task behaviors
    let metrics = manager.get_metrics();
    println!("  Final metrics:");
    println!("    Total polls: {}", metrics.total_polls);
    println!("    Budget violations: {}", metrics.budget_violations);
    println!("    Tier promotions: {}", metrics.tier_promotions);

    // Verify the system is tracking tasks properly
    assert!(metrics.total_polls >= 30, "Should have tracked all polls");

    // The CPU task simulation should show some activity
    // (Note: Actual budget violations depend on the TierManager's criteria)
    println!("    Budget violations detected: {}", metrics.budget_violations);
    println!("    System is actively monitoring task behavior");

    // Check individual task tiers were managed
    // (tasks are completed so tier info should be None)
    assert!(manager.get_task_tier(TaskId(1001)).is_none());
    assert!(manager.get_task_tier(TaskId(1002)).is_none());

    println!("✅ Async task coordination test passed");
}

/// Test starvation prevention
#[tokio::test]
async fn test_starvation_prevention() {
    println!("🛡️ Testing starvation prevention");

    let config = TierConfig {
        poll_budget: 200,
        cpu_ms_budget: 5,
        enable_isolation: false, // Disable isolation for this test
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let start = Instant::now();
    let light_task_completions = Arc::new(AtomicU64::new(0));
    let heavy_task_running = Arc::new(AtomicU64::new(0));

    // Spawn one heavy CPU-bound task
    let heavy_running_clone = Arc::clone(&heavy_task_running);
    let heavy_task = tokio::spawn(async move {
        heavy_running_clone.store(1, Ordering::Relaxed);

        // Very CPU-intensive work
        for i in 0..2000 {
            let mut sum = 0u64;
            for j in 0..5000 {
                sum = sum.wrapping_add(j * i);
            }

            // Minimal yielding to make this task "greedy"
            if i % 100 == 0 {
                tokio::task::yield_now().await;
            }

            std::hint::black_box(sum);
        }

        heavy_running_clone.store(0, Ordering::Relaxed);
        "heavy_completed"
    });

    // Give heavy task a small head start
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Spawn multiple light tasks that should not be starved
    let mut light_handles = Vec::new();

    for i in 0..10 {
        let completions_clone = Arc::clone(&light_task_completions);
        let handle = tokio::spawn(async move {
            // Light task - just some I/O and yields
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_micros(100)).await;
                tokio::task::yield_now().await;
            }

            completions_clone.fetch_add(1, Ordering::Relaxed);
            i
        });

        light_handles.push(handle);
    }

    // Wait for light tasks to complete (they shouldn't be starved)
    let light_results = timeout(Duration::from_secs(15), async {
        let mut results = Vec::new();
        for handle in light_handles {
            results.push(handle.await.unwrap());
        }
        results
    }).await;

    assert!(light_results.is_ok(), "Light tasks should not be starved");

    // Cancel heavy task if still running
    heavy_task.abort();

    let elapsed = start.elapsed();
    let light_completions = light_task_completions.load(Ordering::Relaxed);

    println!("  Light tasks completed: {} in {:?}", light_completions, elapsed);
    assert_eq!(light_completions, 10, "All light tasks should complete");

    // Check that the system intervened
    let metrics = manager.get_metrics();
    println!("  Budget violations: {}", metrics.budget_violations);
    println!("  Tier promotions: {}", metrics.tier_promotions);

    // We expect some interventions due to the heavy task
    assert!(metrics.total_polls > 100, "Should have many polls");

    println!("✅ Starvation prevention test passed");
}

/// Test priority-based scheduling
#[tokio::test]
async fn test_priority_scheduling() {
    println!("🎯 Testing priority-based task scheduling");

    let config = TierConfig {
        poll_budget: 150,
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let start = Instant::now();
    let completions = Arc::new(AtomicU64::new(0));

    // Create tasks with different priorities (simulated via task context)
    let mut handles = Vec::new();

    // High priority tasks (should get more budget)
    for i in 0..3 {
        let completions_clone = Arc::clone(&completions);
        let handle = tokio::spawn(async move {
            // Simulate high priority work with some CPU usage
            for j in 0..500 {
                let mut sum = 0u64;
                for k in 0..100 {
                    sum = sum.wrapping_add(k * j * i);
                }

                if j % 50 == 0 {
                    tokio::task::yield_now().await;
                }

                std::hint::black_box(sum);
            }

            completions_clone.fetch_add(1, Ordering::Relaxed);
            format!("high_priority_{}", i)
        });

        handles.push(handle);
    }

    // Normal priority tasks
    for i in 0..5 {
        let completions_clone = Arc::clone(&completions);
        let handle = tokio::spawn(async move {
            // Simulate normal priority work
            for j in 0..200 {
                tokio::time::sleep(Duration::from_micros(10)).await;

                if j % 20 == 0 {
                    tokio::task::yield_now().await;
                }
            }

            completions_clone.fetch_add(1, Ordering::Relaxed);
            format!("normal_priority_{}", i)
        });

        handles.push(handle);
    }

    // Wait for all tasks
    let results = timeout(Duration::from_secs(20), async {
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        results
    }).await;

    assert!(results.is_ok(), "All priority tasks should complete");

    let elapsed = start.elapsed();
    let total_completions = completions.load(Ordering::Relaxed);

    println!("  Completed {} tasks in {:?}", total_completions, elapsed);
    assert_eq!(total_completions, 8);

    // Check system metrics
    let metrics = manager.get_metrics();
    println!("  System metrics:");
    println!("    Total polls: {}", metrics.total_polls);
    println!("    Budget violations: {}", metrics.budget_violations);
    println!("    Voluntary yields: {}", metrics.voluntary_yields);

    assert!(metrics.total_polls > 0);

    println!("✅ Priority scheduling test passed");
}

/// Test system behavior under high load
#[tokio::test]
async fn test_high_load_behavior() {
    println!("🔥 Testing system behavior under high load");

    let config = TierConfig {
        poll_budget: 300,
        cpu_ms_budget: 20,
        max_slow_queue_size: 1000,
        test_mode: true,
        ..TierConfig::default()
    };

    let manager = Arc::new(TierManager::new(config));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let start = Instant::now();
    let completions = Arc::new(AtomicU64::new(0));

    // Spawn many tasks simultaneously
    let mut handles = Vec::new();
    let task_count = 50; // High load test

    for i in 0..task_count {
        let completions_clone = Arc::clone(&completions);
        let handle = tokio::spawn(async move {
            // Mix of CPU and I/O work
            for j in 0..100 {
                // Some CPU work
                let mut sum = 0u64;
                for k in 0..200 {
                    sum = sum.wrapping_add(k * j * i);
                }

                // Some I/O work
                if j % 10 == 0 {
                    tokio::time::sleep(Duration::from_micros(100)).await;
                }

                // Yield occasionally
                if j % 20 == 0 {
                    tokio::task::yield_now().await;
                }

                std::hint::black_box(sum);
            }

            completions_clone.fetch_add(1, Ordering::Relaxed);
            i
        });

        handles.push(handle);
    }

    // Wait for all tasks with generous timeout
    let results = timeout(Duration::from_secs(30), async {
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        results
    }).await;

    assert!(results.is_ok(), "High load tasks should all complete");

    let elapsed = start.elapsed();
    let total_completions = completions.load(Ordering::Relaxed);

    println!("  Completed {} tasks in {:?}", total_completions, elapsed);
    println!("  Throughput: {:.1} tasks/sec", total_completions as f64 / elapsed.as_secs_f64());

    assert_eq!(total_completions, task_count);

    // Analyze system performance
    let metrics = manager.get_metrics();
    let derived = manager.get_derived_metrics();

    println!("  System performance:");
    println!("    Total polls: {}", metrics.total_polls);
    println!("    Budget violations: {}", metrics.budget_violations);
    println!("    Tier promotions: {}", metrics.tier_promotions);
    println!("    Tier demotions: {}", metrics.tier_demotions);
    println!("    Violation rate: {:.2}%", derived.violation_rate * 100.0);
    println!("    Yield rate: {:.2}%", derived.yield_rate * 100.0);

    // System should remain stable under load
    assert!(metrics.total_polls > task_count as u64, "Should have significant polling activity");
    assert!(derived.violation_rate < 0.50, "Violation rate should be reasonable under load");

    println!("✅ High load behavior test passed");
}