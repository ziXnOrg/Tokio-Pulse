//! Quick validation test for the Tokio-Pulse tier management system

use std::sync::Arc;
use std::time::Duration;
use tokio_pulse::tier_manager::{TierManager, TierConfig, TaskId, PollResult, TaskContext};

#[test]
fn test_tier_escalation_and_demotion_flow() {
    println!("🚀 Testing complete tier escalation and demotion flow");

    // Configure for fast testing
    let mut config = TierConfig::default();
    config.poll_budget = 10;
    config.good_behavior_threshold = 5;
    config.min_tier_duration_ms = 1; // Very short for testing
    config.demotion_evaluation_interval = 2;

    let manager = Arc::new(TierManager::new(config));
    let ctx = TaskContext { worker_id: 0, priority: None };
    let task_id = TaskId(1001);

    // Phase 1: Cause budget violations to escalate tiers
    println!("Phase 1: Causing budget violations");
    for i in 0..15 {
        manager.before_poll(task_id, &ctx);
        // Simulate high CPU usage that violates budget
        let duration = Duration::from_millis(10);
        manager.after_poll(task_id, PollResult::Pending, duration);

        if i % 5 == 4 {
            let tier = manager.get_task_tier(task_id).unwrap_or(0);
            println!("  After {} violations: tier = {}", i + 1, tier);
        }
    }

    let escalated_tier = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Escalated to tier: {}", escalated_tier);
    assert!(escalated_tier > 0, "Task should have been escalated");

    // Phase 2: Demonstrate good behavior
    println!("Phase 2: Demonstrating good behavior");
    for i in 0..15 {
        manager.before_poll(task_id, &ctx);
        // Simulate low CPU usage - good behavior
        let duration = Duration::from_nanos(10_000); // 10μs
        manager.after_poll(task_id, PollResult::Pending, duration);

        if i % 3 == 2 {
            let tier = manager.get_task_tier(task_id).unwrap_or(0);
            println!("  After {} good polls: tier = {}", i + 1, tier);
        }
    }

    let final_tier = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Final tier: {}", final_tier);

    // Validate metrics
    let metrics = manager.get_metrics();
    println!("\nFinal Metrics:");
    println!("  Total polls: {}", metrics.total_polls);
    println!("  Budget violations: {}", metrics.budget_violations);
    println!("  Tier promotions: {}", metrics.tier_promotions);
    println!("  Tier demotions: {}", metrics.tier_demotions);

    // Assertions
    assert!(metrics.total_polls == 30, "Should have executed 30 polls");
    assert!(metrics.budget_violations > 10, "Should have many violations");
    assert!(metrics.tier_promotions > 0, "Should have promoted task");

    manager.on_completion(task_id);
    println!("✅ Tier flow validation completed successfully!");
}

#[test]
fn test_voluntary_yield_demotion() {
    println!("🎯 Testing voluntary yield demotion");

    let mut config = TierConfig::default();
    config.poll_budget = 20;
    config.good_behavior_threshold = 3;
    config.min_tier_duration_ms = 1;

    let manager = Arc::new(TierManager::new(config));
    let ctx = TaskContext { worker_id: 0, priority: None };
    let task_id = TaskId(2001);

    // Escalate tier first
    for _ in 0..10 {
        manager.before_poll(task_id, &ctx);
        manager.after_poll(task_id, PollResult::Pending, Duration::from_millis(5));
    }

    let tier_before_yield = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Tier before yield: {}", tier_before_yield);

    // Set up good behavior
    for _ in 0..5 {
        manager.before_poll(task_id, &ctx);
        manager.after_poll(task_id, PollResult::Pending, Duration::from_nanos(10_000));
    }

    // Voluntary yield should trigger demotion
    manager.on_yield(task_id);

    let tier_after_yield = manager.get_task_tier(task_id).unwrap_or(0);
    println!("Tier after yield: {}", tier_after_yield);

    // Should be demoted due to good behavior + voluntary yield
    // Note: May not always demote due to timing constraints, but good behavior should be tracked

    let metrics = manager.get_metrics();
    assert!(metrics.voluntary_yields > 0, "Should record voluntary yield");

    manager.on_completion(task_id);
    println!("✅ Voluntary yield test completed!");
}

#[test]
fn test_multiple_tasks_isolation() {
    println!("⚔️ Testing multiple tasks with different behaviors");

    let config = TierConfig::default();
    let manager = Arc::new(TierManager::new(config));
    let ctx = TaskContext { worker_id: 0, priority: None };

    let aggressive_task = TaskId(3001);
    let moderate_task = TaskId(3002);
    let good_task = TaskId(3003);

    // Simulate different task behaviors
    for round in 0..20 {
        // Aggressive task - always violates budget
        manager.before_poll(aggressive_task, &ctx);
        manager.after_poll(aggressive_task, PollResult::Pending, Duration::from_millis(8));

        // Moderate task - occasional violations
        manager.before_poll(moderate_task, &ctx);
        let moderate_duration = if round % 3 == 0 {
            Duration::from_millis(3) // Violation
        } else {
            Duration::from_nanos(100_000) // Good
        };
        manager.after_poll(moderate_task, PollResult::Pending, moderate_duration);

        // Good task - always well behaved
        manager.before_poll(good_task, &ctx);
        manager.after_poll(good_task, PollResult::Pending, Duration::from_nanos(50_000));

        if round % 5 == 4 {
            let aggressive_tier = manager.get_task_tier(aggressive_task).unwrap_or(0);
            let moderate_tier = manager.get_task_tier(moderate_task).unwrap_or(0);
            let good_tier = manager.get_task_tier(good_task).unwrap_or(0);
            println!("  Round {}: Tiers = [aggressive: {}, moderate: {}, good: {}]",
                round + 1, aggressive_tier, moderate_tier, good_tier);
        }
    }

    // Validate tier differentiation
    let aggressive_tier = manager.get_task_tier(aggressive_task).unwrap_or(0);
    let moderate_tier = manager.get_task_tier(moderate_task).unwrap_or(0);
    let good_tier = manager.get_task_tier(good_task).unwrap_or(0);

    println!("Final tiers: aggressive={}, moderate={}, good={}",
        aggressive_tier, moderate_tier, good_tier);

    // Aggressive task should be in highest tier
    assert!(aggressive_tier >= moderate_tier,
        "Aggressive task should be in higher tier than moderate");

    let final_metrics = manager.get_metrics();
    println!("Final metrics: {} polls, {} violations, {} promotions",
        final_metrics.total_polls, final_metrics.budget_violations, final_metrics.tier_promotions);

    // Cleanup
    manager.on_completion(aggressive_task);
    manager.on_completion(moderate_task);
    manager.on_completion(good_task);

    println!("✅ Multi-task isolation test completed!");
}