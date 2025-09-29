//! Comprehensive tests for worker-specific statistics tracking
//!
//! This test suite provides thorough coverage of the worker statistics
//! functionality including initialization, isolation, concurrency, overflow
//! handling, and API consistency.

use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio_pulse::{
    HookRegistry, PollResult, PreemptionHooks, TaskContext, TaskId, TierConfig, TierManager,
};

#[test]
fn test_worker_stats_initialization() {
    let manager = TierManager::new(TierConfig::default());

    // New manager should have no worker stats
    let all_stats = manager.get_all_worker_metrics();
    assert!(all_stats.is_empty(), "New manager should have no worker stats");

    // Getting stats for non-existent worker should return default values
    let nonexistent_worker = manager.get_worker_metrics(999);
    assert_eq!(nonexistent_worker.worker_id, 999);
    assert_eq!(nonexistent_worker.polls, 0);
    assert_eq!(nonexistent_worker.violations, 0);
    assert_eq!(nonexistent_worker.yields, 0);
    assert_eq!(nonexistent_worker.cpu_time_ns, 0);
    assert_eq!(nonexistent_worker.tasks, 0);
    assert_eq!(nonexistent_worker.slow_polls, 0);
}

#[test]
fn test_worker_isolation() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    // Create tasks for different workers
    let tasks_per_worker = 3;
    let num_workers = 5;

    for worker_id in 0..num_workers {
        for task_idx in 0..tasks_per_worker {
            let task_id = TaskId((worker_id * 100 + task_idx) as u64);
            let context = TaskContext { worker_id, priority: None };

            registry.before_poll(task_id, &context);
            registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));

            // Some tasks do voluntary yields
            if task_idx == 0 {
                registry.on_yield(task_id);
            }
        }
    }

    // Verify each worker has independent stats
    for worker_id in 0..num_workers {
        let worker_metrics = manager.get_worker_metrics(worker_id);
        assert_eq!(worker_metrics.worker_id, worker_id);
        assert_eq!(worker_metrics.polls, tasks_per_worker as u64);
        assert_eq!(worker_metrics.tasks, tasks_per_worker as u64);
        assert_eq!(worker_metrics.yields, 1); // One yield per worker
        assert!(worker_metrics.cpu_time_ns > 0);
    }

    // Verify total across all workers
    let all_stats = manager.get_all_worker_metrics();
    assert_eq!(all_stats.len(), num_workers);

    let total_polls: u64 = all_stats.values().map(|s| s.polls).sum();
    assert_eq!(total_polls, (num_workers * tasks_per_worker) as u64);
}

#[test]
fn test_concurrent_worker_updates() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = Arc::new(HookRegistry::new());
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let num_workers = 12;
    let operations_per_worker = 100;

    // Spawn concurrent worker threads
    let handles: Vec<_> = (0..num_workers)
        .map(|worker_id| {
            let registry = registry.clone();
            let manager = manager.clone();

            thread::spawn(move || {
                for i in 0..operations_per_worker {
                    let task_id = TaskId((worker_id * 1000 + i) as u64);
                    let context = TaskContext { worker_id, priority: None };

                    registry.before_poll(task_id, &context);
                    registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(50));

                    // Random yields
                    if i % 10 == 0 {
                        registry.on_yield(task_id);
                    }

                    // Verify stats are consistent during concurrent updates
                    let worker_stats = manager.get_worker_metrics(worker_id);
                    assert_eq!(worker_stats.worker_id, worker_id);
                    assert!(worker_stats.polls <= (i + 1) as u64);
                }
            })
        })
        .collect();

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("Worker thread should complete successfully");
    }

    // Verify final state
    for worker_id in 0..num_workers {
        let worker_metrics = manager.get_worker_metrics(worker_id);
        assert_eq!(worker_metrics.worker_id, worker_id);
        assert_eq!(worker_metrics.polls, operations_per_worker as u64);
        assert_eq!(worker_metrics.tasks, operations_per_worker as u64);
        assert!(worker_metrics.yields >= 10); // At least 10 yields per worker
    }
}

#[test]
fn test_worker_stats_overflow_handling() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    // Simulate very high poll counts to test overflow behavior
    let _task_id = TaskId(1);
    let context = TaskContext { worker_id: 0, priority: None };

    // Do a large number of operations to test atomic overflow behavior
    // Note: We can't actually overflow u64 in a reasonable test time,
    // but we can test large numbers
    let large_operations = 100_000;

    for i in 0..large_operations {
        let task_id = TaskId(i as u64);
        registry.before_poll(task_id, &context);
        registry.after_poll(task_id, PollResult::Ready, Duration::from_nanos(1));

        // Sample check to ensure stats remain consistent
        if i % 10_000 == 0 {
            let worker_metrics = manager.get_worker_metrics(0);
            assert_eq!(worker_metrics.worker_id, 0);
            assert!(worker_metrics.polls > 0);
            assert!(worker_metrics.tasks > 0);
        }
    }

    let final_metrics = manager.get_worker_metrics(0);
    assert_eq!(final_metrics.polls, large_operations as u64);
    assert_eq!(final_metrics.tasks, large_operations as u64);
}

#[test]
fn test_reset_individual_vs_all_workers() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let num_workers = 4;

    // Set up initial state for all workers
    for worker_id in 0..num_workers {
        for i in 0..5 {
            let task_id = TaskId((worker_id * 10 + i) as u64);
            let context = TaskContext { worker_id, priority: None };

            registry.before_poll(task_id, &context);
            registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));
            registry.on_yield(task_id);
        }
    }

    // Verify all workers have stats
    for worker_id in 0..num_workers {
        let worker_metrics = manager.get_worker_metrics(worker_id);
        assert!(worker_metrics.polls > 0);
        assert!(worker_metrics.yields > 0);
    }

    // Reset one specific worker
    manager.reset_worker_stats(1);

    // Verify worker 1 is reset but others are unchanged
    let worker_1_metrics = manager.get_worker_metrics(1);
    assert_eq!(worker_1_metrics.polls, 0);
    assert_eq!(worker_1_metrics.yields, 0);
    assert_eq!(worker_1_metrics.tasks, 0);
    assert_eq!(worker_1_metrics.cpu_time_ns, 0);

    // Verify other workers are unchanged
    for worker_id in [0, 2, 3] {
        let worker_metrics = manager.get_worker_metrics(worker_id);
        assert!(worker_metrics.polls > 0, "Worker {} should still have stats", worker_id);
        assert!(worker_metrics.yields > 0, "Worker {} should still have yields", worker_id);
    }

    // Test multiple resets are idempotent
    manager.reset_worker_stats(1);
    manager.reset_worker_stats(1);
    let worker_1_after_multiple_resets = manager.get_worker_metrics(1);
    assert_eq!(worker_1_after_multiple_resets.polls, 0);
    assert_eq!(worker_1_after_multiple_resets.yields, 0);
}

#[test]
fn test_worker_metrics_api_consistency() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let task_id = TaskId(42);
    let context = TaskContext { worker_id: 5, priority: None };

    // Perform various operations
    registry.before_poll(task_id, &context);
    registry.after_poll(task_id, PollResult::Pending, Duration::from_millis(2)); // Slow poll
    registry.on_yield(task_id);

    // Get stats via different APIs
    let single_worker_metrics = manager.get_worker_metrics(5);
    let all_worker_metrics = manager.get_all_worker_metrics();

    // Verify consistency between APIs
    assert!(all_worker_metrics.contains_key(&5));
    let worker_5_from_all = &all_worker_metrics[&5];

    assert_eq!(single_worker_metrics.worker_id, worker_5_from_all.worker_id);
    assert_eq!(single_worker_metrics.polls, worker_5_from_all.polls);
    assert_eq!(single_worker_metrics.violations, worker_5_from_all.violations);
    assert_eq!(single_worker_metrics.yields, worker_5_from_all.yields);
    assert_eq!(single_worker_metrics.cpu_time_ns, worker_5_from_all.cpu_time_ns);
    assert_eq!(single_worker_metrics.tasks, worker_5_from_all.tasks);
    assert_eq!(single_worker_metrics.slow_polls, worker_5_from_all.slow_polls);

    // Verify specific values
    assert_eq!(single_worker_metrics.worker_id, 5);
    assert_eq!(single_worker_metrics.polls, 1);
    assert_eq!(single_worker_metrics.tasks, 1);
    assert_eq!(single_worker_metrics.yields, 1);
    assert_eq!(single_worker_metrics.slow_polls, 1); // 2ms duration should be slow
    assert!(single_worker_metrics.cpu_time_ns > 0);
}

#[test]
fn test_high_frequency_updates() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let start_time = std::time::Instant::now();
    let operations = 50_000; // Reduced from 100K for reasonable test time
    let worker_id = 0;

    // High-frequency operations
    for i in 0..operations {
        let task_id = TaskId(i as u64);
        let context = TaskContext { worker_id, priority: None };

        registry.before_poll(task_id, &context);
        registry.after_poll(task_id, PollResult::Ready, Duration::from_nanos(100));
    }

    let elapsed = start_time.elapsed();
    let final_metrics = manager.get_worker_metrics(worker_id);

    // Verify all operations were tracked
    assert_eq!(final_metrics.polls, operations as u64);
    assert_eq!(final_metrics.tasks, operations as u64);

    // Performance check - should be able to handle high frequency updates
    // Allow generous time for CI environments
    assert!(elapsed < Duration::from_secs(10),
           "High frequency updates took too long: {:?}", elapsed);

    println!("Processed {} operations in {:?} ({:.2} ops/sec)",
             operations, elapsed, operations as f64 / elapsed.as_secs_f64());
}

#[test]
fn test_worker_stats_memory_usage() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let num_workers = 100;
    let tasks_per_worker = 10;

    // Create stats for many workers
    for worker_id in 0..num_workers {
        for task_idx in 0..tasks_per_worker {
            let task_id = TaskId((worker_id * 1000 + task_idx) as u64);
            let context = TaskContext { worker_id, priority: None };

            registry.before_poll(task_id, &context);
            registry.after_poll(task_id, PollResult::Ready, Duration::from_micros(10));
            registry.on_completion(task_id); // Clean up tasks
        }
    }

    // Verify all worker stats exist
    let all_stats = manager.get_all_worker_metrics();
    assert_eq!(all_stats.len(), num_workers);

    // Check that each worker has expected stats
    for worker_id in 0..num_workers {
        let worker_metrics = manager.get_worker_metrics(worker_id);
        assert_eq!(worker_metrics.worker_id, worker_id);
        assert_eq!(worker_metrics.polls, tasks_per_worker as u64);
        assert_eq!(worker_metrics.tasks, tasks_per_worker as u64);
    }

    // Test resetting doesn't cause memory issues
    for worker_id in 0..num_workers {
        manager.reset_worker_stats(worker_id);
    }

    // Stats should still exist but be zeroed
    let all_stats_after_reset = manager.get_all_worker_metrics();
    assert_eq!(all_stats_after_reset.len(), num_workers);

    for worker_id in 0..num_workers {
        let worker_metrics = manager.get_worker_metrics(worker_id);
        assert_eq!(worker_metrics.polls, 0);
        assert_eq!(worker_metrics.tasks, 0);
        assert_eq!(worker_metrics.yields, 0);
        assert_eq!(worker_metrics.cpu_time_ns, 0);
    }
}

#[test]
fn test_worker_stats_edge_cases() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    // Test with maximum worker ID
    let max_worker_id = usize::MAX;
    let task_id = TaskId(1);
    let context = TaskContext { worker_id: max_worker_id, priority: Some(255) };

    registry.before_poll(task_id, &context);
    registry.after_poll(task_id, PollResult::Ready, Duration::from_micros(1));

    let max_worker_metrics = manager.get_worker_metrics(max_worker_id);
    assert_eq!(max_worker_metrics.worker_id, max_worker_id);
    assert_eq!(max_worker_metrics.polls, 1);
    assert_eq!(max_worker_metrics.tasks, 1);

    // Test rapid task creation and completion
    for i in 1000..2000 {
        let task_id = TaskId(i);
        let context = TaskContext { worker_id: 0, priority: None };

        registry.before_poll(task_id, &context);
        registry.after_poll(task_id, PollResult::Ready, Duration::from_nanos(1));
        registry.on_completion(task_id);
    }

    let worker_0_metrics = manager.get_worker_metrics(0);
    assert_eq!(worker_0_metrics.polls, 1000);
    assert_eq!(worker_0_metrics.tasks, 1000);

    // Test zero-duration polls
    let task_id = TaskId(9999);
    let context = TaskContext { worker_id: 1, priority: None };

    registry.before_poll(task_id, &context);
    registry.after_poll(task_id, PollResult::Pending, Duration::ZERO);

    let worker_1_metrics = manager.get_worker_metrics(1);
    assert_eq!(worker_1_metrics.polls, 1);
    assert_eq!(worker_1_metrics.cpu_time_ns, 0); // Zero duration should record 0 CPU time
}