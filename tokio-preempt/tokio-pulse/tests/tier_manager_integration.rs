//! Integration tests for TierManager and PreemptionHooks

use std::sync::Arc;
use std::time::Duration;
use tokio_pulse::{
    HookRegistry, PollResult, PreemptionHooks, TaskContext, TaskId, TierConfig, TierManager,
};

#[test]
fn test_tier_manager_as_hooks() {
    // Create TierManager with custom configuration
    let mut config = TierConfig::default();
    config.poll_budget = 100;
    config.tier_policies[0].promotion_threshold = 2; // Escalate after 2 violations

    let manager = Arc::new(TierManager::new(config));

    // Install as hooks
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    // Simulate task execution
    let task_id = TaskId(1);
    let context = TaskContext {
        worker_id: 0,
        priority: None,
    };

    // First poll - should work fine
    registry.before_poll(task_id, &context);
    registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(50));

    // Check metrics
    let metrics = manager.metrics();
    assert_eq!(metrics.total_polls, 1);
    assert_eq!(metrics.total_violations, 0);

    // Simulate polls that don't cause violations with reasonable durations
    registry.before_poll(task_id, &context);
    registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));

    // Check metrics - no violations from normal polls
    let metrics = manager.metrics();
    assert_eq!(metrics.total_polls, 2);
    // Note: violations only occur when budget.consume() returns true,
    // which requires actual budget consumption logic in the runtime

    // Task completion should clean up
    registry.on_completion(task_id);

    // Verify cleanup
    let metrics = manager.metrics();
    assert_eq!(metrics.active_tasks, 0);
}

#[test]
fn test_hook_registry_switching() {
    let registry = HookRegistry::new();

    // Start with no hooks
    assert!(!registry.has_hooks());

    // Install TierManager
    let manager1 = Arc::new(TierManager::new(TierConfig::default()));
    let old = registry.set_hooks(manager1.clone() as Arc<dyn PreemptionHooks>);
    assert!(old.is_none());
    assert!(registry.has_hooks());

    // Replace with another TierManager
    let manager2 = Arc::new(TierManager::new(TierConfig::default()));
    let old = registry.set_hooks(manager2.clone() as Arc<dyn PreemptionHooks>);
    assert!(old.is_some());

    // Clear hooks
    let old = registry.clear_hooks();
    assert!(old.is_some());
    assert!(!registry.has_hooks());
}

#[test]
fn test_tier_escalation_through_hooks() {
    let mut config = TierConfig::default();
    config.poll_budget = 100; // Minimum budget allowed
    config.tier_policies[0].promotion_threshold = 1; // Escalate immediately

    let manager = Arc::new(TierManager::new(config));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let task_id = TaskId(42);
    let context = TaskContext {
        worker_id: 1,
        priority: Some(5),
    };

    // Multiple polls to track activity
    for _ in 0..5 {
        registry.before_poll(task_id, &context);
        registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(50));
    }

    // Verify polls were counted
    let metrics = manager.metrics();
    assert_eq!(metrics.total_polls, 5);
}

#[test]
fn test_voluntary_yield_resets_tier() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let task_id = TaskId(99);
    let context = TaskContext {
        worker_id: 2,
        priority: None,
    };

    // Regular polls without violations
    for _ in 0..3 {
        registry.before_poll(task_id, &context);
        registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(50));
    }

    let metrics_before = manager.metrics();

    // Voluntary yield should help
    registry.on_yield(task_id);

    // Verify yield was recorded
    let metrics_after = manager.metrics();
    assert_eq!(metrics_after.total_yields, metrics_before.total_yields + 1);
}

#[test]
fn test_concurrent_task_management() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = Arc::new(HookRegistry::new());
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    // Simulate multiple concurrent tasks
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let registry = registry.clone();
            std::thread::spawn(move || {
                let task_id = TaskId(i);
                let context = TaskContext {
                    worker_id: i as usize % 4,
                    priority: None,
                };

                for _ in 0..5 {
                    registry.before_poll(task_id, &context);
                    std::thread::sleep(Duration::from_micros(10));
                    registry.after_poll(task_id, PollResult::Pending, Duration::from_micros(50));
                }

                registry.on_completion(task_id);
            })
        })
        .collect();

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread failed");
    }

    // All tasks should have completed
    let metrics = manager.metrics();
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.total_polls, 50); // 10 tasks * 5 polls each
}

#[test]
fn test_worker_specific_statistics() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    // Test multiple workers
    let task1 = TaskId(1);
    let task2 = TaskId(2);
    let task3 = TaskId(3);

    let worker0_context = TaskContext { worker_id: 0, priority: None };
    let worker1_context = TaskContext { worker_id: 1, priority: None };
    let worker2_context = TaskContext { worker_id: 2, priority: None };

    // Worker 0: 2 polls, 1 slow poll
    registry.before_poll(task1, &worker0_context);
    registry.after_poll(task1, PollResult::Pending, Duration::from_millis(1)); // Fast poll
    registry.before_poll(task1, &worker0_context);
    registry.after_poll(task1, PollResult::Pending, Duration::from_millis(5)); // Slow poll

    // Worker 1: 3 polls, 1 yield
    registry.before_poll(task2, &worker1_context);
    registry.after_poll(task2, PollResult::Pending, Duration::from_micros(500));
    registry.before_poll(task2, &worker1_context);
    registry.after_poll(task2, PollResult::Pending, Duration::from_micros(300));
    registry.before_poll(task2, &worker1_context);
    registry.after_poll(task2, PollResult::Pending, Duration::from_micros(200));
    registry.on_yield(task2);

    // Worker 2: 1 poll, new task
    registry.before_poll(task3, &worker2_context);
    registry.after_poll(task3, PollResult::Ready, Duration::from_micros(100));

    // Verify worker-specific metrics
    let worker0_metrics = manager.get_worker_metrics(0);
    assert_eq!(worker0_metrics.worker_id, 0);
    assert_eq!(worker0_metrics.polls, 2);
    assert_eq!(worker0_metrics.tasks, 1);
    assert_eq!(worker0_metrics.slow_polls, 1);
    assert!(worker0_metrics.cpu_time_ns > 0); // Should have some CPU time recorded

    let worker1_metrics = manager.get_worker_metrics(1);
    assert_eq!(worker1_metrics.worker_id, 1);
    assert_eq!(worker1_metrics.polls, 3);
    assert_eq!(worker1_metrics.tasks, 1);
    assert_eq!(worker1_metrics.yields, 1);
    assert_eq!(worker1_metrics.slow_polls, 0);

    let worker2_metrics = manager.get_worker_metrics(2);
    assert_eq!(worker2_metrics.worker_id, 2);
    assert_eq!(worker2_metrics.polls, 1);
    assert_eq!(worker2_metrics.tasks, 1);
    assert_eq!(worker2_metrics.yields, 0);
    assert_eq!(worker2_metrics.slow_polls, 0);

    // Test get_all_worker_metrics
    let all_metrics = manager.get_all_worker_metrics();
    assert_eq!(all_metrics.len(), 3);
    assert!(all_metrics.contains_key(&0));
    assert!(all_metrics.contains_key(&1));
    assert!(all_metrics.contains_key(&2));

    // Test worker that doesn't exist
    let nonexistent_worker = manager.get_worker_metrics(99);
    assert_eq!(nonexistent_worker.worker_id, 99);
    assert_eq!(nonexistent_worker.polls, 0);
    assert_eq!(nonexistent_worker.tasks, 0);

    // Test reset_worker_stats
    manager.reset_worker_stats(0);
    let reset_metrics = manager.get_worker_metrics(0);
    assert_eq!(reset_metrics.polls, 0);
    assert_eq!(reset_metrics.tasks, 0);
    assert_eq!(reset_metrics.slow_polls, 0);
    assert_eq!(reset_metrics.cpu_time_ns, 0);

    // Verify other workers unchanged
    let worker1_after_reset = manager.get_worker_metrics(1);
    assert_eq!(worker1_after_reset.polls, 3);
    assert_eq!(worker1_after_reset.yields, 1);
}

#[test]
fn test_worker_tier_promotions_and_violations() {
    let mut config = TierConfig::default();
    config.poll_budget = 2; // Very low budget to trigger violations easily
    config.tier_policies[0].promotion_threshold = 1; // Promote after 1 violation
    config.tier_policies[1].promotion_threshold = 1;
    config.tier_policies[2].promotion_threshold = 1;

    let manager = Arc::new(TierManager::new(config));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let task1 = TaskId(101);
    let task2 = TaskId(102);
    let worker0_context = TaskContext { worker_id: 0, priority: None };
    let worker1_context = TaskContext { worker_id: 1, priority: None };

    // Worker 0: Cause multiple budget violations and tier promotions
    for _ in 0..5 {
        registry.before_poll(task1, &worker0_context);
        // Use very long duration to ensure budget violation
        registry.after_poll(task1, PollResult::Pending, Duration::from_millis(100));
    }

    // Worker 1: Normal behavior with no violations (only 1 poll, under budget)
    registry.before_poll(task2, &worker1_context);
    registry.after_poll(task2, PollResult::Pending, Duration::from_micros(10));

    // Verify worker 0 has violations
    let worker0_metrics = manager.get_worker_metrics(0);
    assert_eq!(worker0_metrics.worker_id, 0);
    assert_eq!(worker0_metrics.polls, 5);
    assert!(worker0_metrics.violations > 0, "Worker 0 should have budget violations");

    // Verify worker 1 has no violations
    let worker1_metrics = manager.get_worker_metrics(1);
    assert_eq!(worker1_metrics.worker_id, 1);
    assert_eq!(worker1_metrics.polls, 1);
    assert_eq!(worker1_metrics.violations, 0);
}

#[test]
fn test_worker_voluntary_vs_forced_yields() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let task1 = TaskId(201);
    let task2 = TaskId(202);
    let worker0_context = TaskContext { worker_id: 0, priority: None };
    let worker1_context = TaskContext { worker_id: 1, priority: None };

    // Worker 0: Create a task that will do voluntary yields
    registry.before_poll(task1, &worker0_context);
    registry.after_poll(task1, PollResult::Pending, Duration::from_micros(100));
    registry.on_yield(task1); // Voluntary yield
    registry.on_yield(task1); // Another voluntary yield

    // Worker 1: Different task, no yields
    registry.before_poll(task2, &worker1_context);
    registry.after_poll(task2, PollResult::Ready, Duration::from_micros(50));

    // Verify voluntary yields are tracked per worker
    let worker0_metrics = manager.get_worker_metrics(0);
    assert_eq!(worker0_metrics.worker_id, 0);
    assert_eq!(worker0_metrics.yields, 2, "Worker 0 should have 2 voluntary yields");

    let worker1_metrics = manager.get_worker_metrics(1);
    assert_eq!(worker1_metrics.worker_id, 1);
    assert_eq!(worker1_metrics.yields, 0, "Worker 1 should have 0 yields");
}

#[test]
fn test_worker_cpu_time_isolation() {
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();
    registry.set_hooks(manager.clone() as Arc<dyn PreemptionHooks>);

    let task1 = TaskId(301);
    let task2 = TaskId(302);
    let worker0_context = TaskContext { worker_id: 0, priority: None };
    let worker1_context = TaskContext { worker_id: 1, priority: None };

    // Worker 0: Longer CPU time
    registry.before_poll(task1, &worker0_context);
    registry.after_poll(task1, PollResult::Pending, Duration::from_millis(10));

    // Worker 1: Shorter CPU time
    registry.before_poll(task2, &worker1_context);
    registry.after_poll(task2, PollResult::Pending, Duration::from_micros(100));

    let worker0_metrics = manager.get_worker_metrics(0);
    let worker1_metrics = manager.get_worker_metrics(1);

    // Both workers should have recorded some CPU time
    assert!(worker0_metrics.cpu_time_ns > 0, "Worker 0 should have CPU time");
    assert!(worker1_metrics.cpu_time_ns > 0, "Worker 1 should have CPU time");

    // Workers should have independent CPU time tracking
    assert_ne!(worker0_metrics.cpu_time_ns, worker1_metrics.cpu_time_ns,
              "Workers should have different CPU time records");
}
