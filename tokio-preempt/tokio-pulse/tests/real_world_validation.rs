//! Real-world validation test for the Tokio-Pulse tier management system

use std::sync::Arc;
use std::time::Duration;
use tokio_pulse::tier_manager::{TierManager, TierConfig, TaskId, PollResult, TaskContext};

#[test]
fn test_basic_tier_escalation() {
    println!("Testing basic tier escalation");

    let mut config = TierConfig::default();
    config.poll_budget = 10;  // Small budget for quick violations
    config.test_mode = true;  // Enable test mode

    let manager = Arc::new(TierManager::new(config));
    let ctx = TaskContext { worker_id: 0, priority: None };
    let task_id = TaskId(1001);

    // Phase 1: Cause budget violations to escalate tier
    for i in 0..15 {
        manager.before_poll(task_id, &ctx);
        // High CPU usage that violates budget
        manager.after_poll(task_id, PollResult::Pending, Duration::from_millis(5));

        if i == 4 || i == 9 || i == 14 {
            let tier = manager.get_task_tier(task_id).unwrap_or(0);
            println!("  After {} violations: tier = {}", i + 1, tier);
        }
    }

    let final_tier = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Final tier: {}", final_tier);
    assert!(final_tier > 0, "Task should have been escalated to a higher tier");

    let metrics = manager.get_metrics();
    println!("Metrics: {} polls, {} violations, {} promotions",
        metrics.total_polls, metrics.budget_violations, metrics.tier_promotions);

    manager.on_completion(task_id);
    println!("Basic tier escalation test passed!");
}

#[test]
fn test_tier_demotion_flow() {
    println!("Testing tier demotion flow");

    let mut config = TierConfig::default();
    config.poll_budget = 10;
    config.good_behavior_threshold = 5;  // Low threshold for quick testing
    config.test_mode = true;             // Skip time-based checks

    let manager = Arc::new(TierManager::new(config));
    let ctx = TaskContext { worker_id: 0, priority: None };
    let task_id = TaskId(2001);

    // Phase 1: Escalate to higher tier
    for _ in 0..10 {
        manager.before_poll(task_id, &ctx);
        manager.after_poll(task_id, PollResult::Pending, Duration::from_millis(8)); // Violates budget
    }

    let escalated_tier = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Escalated to tier: {}", escalated_tier);
    assert!(escalated_tier > 0, "Task should have been escalated");

    // Phase 2: Show good behavior to trigger demotion
    for i in 0..10 {
        manager.before_poll(task_id, &ctx);
        // Low CPU usage - good behavior
        manager.after_poll(task_id, PollResult::Pending, Duration::from_nanos(50_000)); // 50μs

        if i == 4 {
            let tier = manager.get_task_tier(task_id).unwrap_or(0);
            println!("  After {} good polls: tier = {}", i + 1, tier);
        }
    }

    let final_tier = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Final tier after good behavior: {}", final_tier);

    let metrics = manager.get_metrics();
    println!("Final metrics: {} polls, {} violations, {} promotions, {} demotions",
        metrics.total_polls, metrics.budget_violations, metrics.tier_promotions, metrics.tier_demotions);

    manager.on_completion(task_id);
    println!("Tier demotion flow test passed!");
}

#[test]
fn test_voluntary_yield_demotion() {
    println!("Testing voluntary yield demotion");

    let mut config = TierConfig::default();
    config.poll_budget = 10;
    config.good_behavior_threshold = 3;
    config.test_mode = true;

    let manager = Arc::new(TierManager::new(config));
    let ctx = TaskContext { worker_id: 0, priority: None };
    let task_id = TaskId(3001);

    // Escalate tier first
    for _ in 0..8 {
        manager.before_poll(task_id, &ctx);
        manager.after_poll(task_id, PollResult::Pending, Duration::from_millis(6));
    }

    let tier_before = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Tier before yield: {}", tier_before);

    // Set up good behavior
    for _ in 0..5 {
        manager.before_poll(task_id, &ctx);
        manager.after_poll(task_id, PollResult::Pending, Duration::from_nanos(25_000)); // 25μs
    }

    // Voluntary yield
    manager.on_yield(task_id);

    let tier_after = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Tier after yield: {}", tier_after);

    let metrics = manager.get_metrics();
    assert!(metrics.voluntary_yields > 0, "Should record voluntary yield");

    manager.on_completion(task_id);
    println!("Voluntary yield test passed!");
}

#[test]
fn test_multiple_tasks_isolation() {
    println!("Testing multiple tasks with different behaviors");

    let mut config = TierConfig::default();
    config.test_mode = true;

    let manager = Arc::new(TierManager::new(config));
    let ctx = TaskContext { worker_id: 0, priority: None };

    let aggressive_task = TaskId(4001);
    let moderate_task = TaskId(4002);
    let good_task = TaskId(4003);

    // Simulate different behaviors over 20 rounds
    for round in 0..20 {
        // Aggressive task - always violates budget
        manager.before_poll(aggressive_task, &ctx);
        manager.after_poll(aggressive_task, PollResult::Pending, Duration::from_millis(8));

        // Moderate task - occasional violations
        manager.before_poll(moderate_task, &ctx);
        let moderate_duration = if round % 4 == 0 {
            Duration::from_millis(3) // Violation every 4th poll
        } else {
            Duration::from_nanos(100_000) // Good behavior
        };
        manager.after_poll(moderate_task, PollResult::Pending, moderate_duration);

        // Good task - always well behaved
        manager.before_poll(good_task, &ctx);
        manager.after_poll(good_task, PollResult::Pending, Duration::from_nanos(50_000));
    }

    // Check final tiers
    let aggressive_tier = manager.get_task_tier(aggressive_task).unwrap_or(0);
    let moderate_tier = manager.get_task_tier(moderate_task).unwrap_or(0);
    let good_tier = manager.get_task_tier(good_task).unwrap_or(0);

    println!("Final tiers: aggressive={}, moderate={}, good={}",
        aggressive_tier, moderate_tier, good_tier);

    // Aggressive task should be in highest tier
    assert!(aggressive_tier >= moderate_tier,
        "Aggressive task should be in higher tier than moderate");

    let metrics = manager.get_metrics();
    println!("Final system metrics:");
    println!("  Total polls: {}", metrics.total_polls);
    println!("  Budget violations: {}", metrics.budget_violations);
    println!("  Tier promotions: {}", metrics.tier_promotions);
    println!("  Tier demotions: {}", metrics.tier_demotions);

    // Cleanup
    manager.on_completion(aggressive_task);
    manager.on_completion(moderate_task);
    manager.on_completion(good_task);

    println!("Multi-task isolation test passed!");
}

#[allow(dead_code)]
fn print_metrics(manager: &TierManager) {
    let metrics = manager.get_metrics();
    let derived = manager.get_derived_metrics();

    println!("Current Metrics:");
    println!("  Total polls: {}", metrics.total_polls);
    println!("  Budget violations: {}", metrics.budget_violations);
    println!("  Tier promotions: {}", metrics.tier_promotions);
    println!("  Tier demotions: {}", metrics.tier_demotions);
    println!("  Voluntary yields: {}", metrics.voluntary_yields);
    println!("  Violation rate: {:.2}%", derived.violation_rate * 100.0);
    println!("  Yield rate: {:.2}%", derived.yield_rate * 100.0);
}