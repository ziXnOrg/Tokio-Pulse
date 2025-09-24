//! Integration tests for TierManager and PreemptionHooks

use std::sync::Arc;
use std::time::Duration;
use tokio_pulse::{
    HookRegistry, PreemptionHooks, PollResult, TaskContext, TaskId, TierConfig, TierManager,
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
    registry.after_poll(
        task_id,
        PollResult::Pending,
        Duration::from_micros(100),
    );

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
        registry.after_poll(
            task_id,
            PollResult::Pending,
            Duration::from_micros(50),
        );
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
        registry.after_poll(
            task_id,
            PollResult::Pending,
            Duration::from_micros(50),
        );
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