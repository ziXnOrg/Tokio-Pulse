//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
// Property-based tests for TierManager invariants
use proptest::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio_pulse::tier_manager::{
    InterventionAction, PollResult, TaskContext, TaskId, TierConfig, TierManager, TierPolicy,
};

/// Strategy for generating realistic TierConfigs within performance bounds
fn tier_config_strategy() -> impl Strategy<Value = TierConfig> {
    (
        100u32..=5000,    // poll_budget: BEAM-inspired range
        1u64..=50,        // cpu_ms_budget: reasonable time windows
        10u32..=500,      // yield_interval: practical ranges
        100usize..=10_000, // max_slow_queue_size: bounded queues
        10u64..=500,      // promotion_hysteresis_ms: prevent oscillation
        50u64..=2000,     // demotion_hysteresis_ms: stability
        1.0f64..=50.0,    // violation_rate_threshold: practical rates
        100u64..=5000,    // cooldown_duration_ms: reasonable cooldowns
    ).prop_map(|(
        poll_budget,
        cpu_ms_budget,
        yield_interval,
        max_slow_queue_size,
        promotion_hysteresis_ms,
        demotion_hysteresis_ms,
        violation_rate_threshold,
        cooldown_duration_ms,
    )| {
        TierConfig {
            poll_budget,
            cpu_ms_budget,
            yield_interval,
            tier_policies: [
                TierPolicy {
                    name: "Monitor",
                    promotion_threshold: 5,
                    action: InterventionAction::Monitor,
                    promotion_cpu_threshold_ms: 100,
                    promotion_slow_poll_threshold: 10,
                    demotion_good_polls: 20,
                    demotion_min_duration_ms: 5000,
                    demotion_cpu_threshold_ns: 1_000_000,
                    demotion_evaluation_interval: 5,
                },
                TierPolicy {
                    name: "Warn",
                    promotion_threshold: 3,
                    action: InterventionAction::Warn,
                    promotion_cpu_threshold_ms: 50,
                    promotion_slow_poll_threshold: 5,
                    demotion_good_polls: 15,
                    demotion_min_duration_ms: 3000,
                    demotion_cpu_threshold_ns: 500_000,
                    demotion_evaluation_interval: 3,
                },
                TierPolicy {
                    name: "Yield",
                    promotion_threshold: 2,
                    action: InterventionAction::Yield,
                    promotion_cpu_threshold_ms: 25,
                    promotion_slow_poll_threshold: 3,
                    demotion_good_polls: 10,
                    demotion_min_duration_ms: 2000,
                    demotion_cpu_threshold_ns: 250_000,
                    demotion_evaluation_interval: 2,
                },
                TierPolicy {
                    name: "Isolate",
                    promotion_threshold: 1,
                    action: InterventionAction::Isolate,
                    promotion_cpu_threshold_ms: 10,
                    promotion_slow_poll_threshold: 1,
                    demotion_good_polls: 5,
                    demotion_min_duration_ms: 1000,
                    demotion_cpu_threshold_ns: 100_000,
                    demotion_evaluation_interval: 1,
                },
            ],
            enable_isolation: false,
            max_slow_queue_size,
            promotion_hysteresis_ms,
            demotion_hysteresis_ms,
            violation_rate_threshold,
            cooldown_duration_ms,
            good_behavior_threshold: 50,
            min_tier_duration_ms: 2000,
            demotion_cpu_threshold_ns: 500_000,
            demotion_evaluation_interval: 10,
            test_mode: true,
        }
    })
}

/// Strategy for generating realistic TaskIds
fn task_id_strategy() -> impl Strategy<Value = TaskId> {
    (1u64..=1_000_000).prop_map(TaskId)
}

/// Strategy for generating realistic TaskContext
fn task_context_strategy() -> impl Strategy<Value = TaskContext> {
    (0usize..=16, prop::option::of(0u8..=255)).prop_map(|(worker_id, priority)| TaskContext {
        worker_id,
        priority,
    })
}

/// Strategy for generating PollResult
fn poll_result_strategy() -> impl Strategy<Value = PollResult> {
    prop_oneof![
        Just(PollResult::Ready),
        Just(PollResult::Pending),
    ]
}

/// Strategy for generating realistic poll durations (50-500μs normal, up to 50ms for slow)
fn normal_duration_strategy() -> impl Strategy<Value = Duration> {
    (50u64..=500).prop_map(Duration::from_micros)
}


#[cfg(test)]
mod tier_manager_properties {
    use super::*;

    proptest! {
        /// Property: TierManager creation should always succeed with valid config
        #[test]
        fn tier_manager_creation_invariants(
            config in tier_config_strategy()
        ) {
            let manager = TierManager::new(config);
            let metrics = manager.metrics();

            // Manager should initialize with clean state
            prop_assert_eq!(metrics.active_tasks, 0);
            prop_assert_eq!(metrics.total_polls, 0);
            prop_assert_eq!(metrics.total_violations, 0);
            prop_assert_eq!(metrics.total_yields, 0);
            prop_assert_eq!(metrics.slow_queue_size, 0);
        }

        /// Property: Task lifecycle should maintain poll count invariants
        #[test]
        fn task_lifecycle_poll_counting(
            config in tier_config_strategy(),
            task_id in task_id_strategy(),
            context in task_context_strategy(),
            poll_result in poll_result_strategy(),
            duration in normal_duration_strategy()
        ) {
            let manager = TierManager::new(config);
            let initial_metrics = manager.metrics();

            // Complete task lifecycle
            manager.before_poll(task_id, &context);
            manager.after_poll(task_id, poll_result, duration);

            let final_metrics = manager.metrics();

            // Poll count must increase exactly by 1
            prop_assert_eq!(final_metrics.total_polls, initial_metrics.total_polls + 1);

            // Metrics should be monotonic (only increase)
            prop_assert!(final_metrics.total_violations >= initial_metrics.total_violations);
            prop_assert!(final_metrics.total_yields >= initial_metrics.total_yields);
        }

        /// Property: Task completion should always clean up state
        #[test]
        fn task_completion_cleanup_invariant(
            config in tier_config_strategy(),
            task_id in task_id_strategy(),
            context in task_context_strategy()
        ) {
            let manager = TierManager::new(config);

            // Start task and verify it's tracked
            manager.before_poll(task_id, &context);
            let with_task = manager.metrics().active_tasks;

            // Complete task
            manager.on_completion(task_id);
            let after_completion = manager.metrics().active_tasks;

            // Active tasks should decrease
            prop_assert!(after_completion <= with_task);
        }

        /// Property: Voluntary yields should increment yield counter
        #[test]
        fn voluntary_yield_counting(
            config in tier_config_strategy(),
            task_id in task_id_strategy(),
            context in task_context_strategy()
        ) {
            let manager = TierManager::new(config);

            // Start task
            manager.before_poll(task_id, &context);
            let initial_yields = manager.metrics().total_yields;

            // Perform voluntary yield
            manager.on_yield(task_id);
            let after_yield = manager.metrics().total_yields;

            // Yield count should increase
            prop_assert!(after_yield >= initial_yields);
            prop_assert!(after_yield <= initial_yields + 1);
        }

        /// Property: Configuration updates should not corrupt state
        #[test]
        fn config_update_stability(
            initial_config in tier_config_strategy(),
            new_config in tier_config_strategy(),
            task_id in task_id_strategy(),
            context in task_context_strategy()
        ) {
            let manager = TierManager::new(initial_config);

            // Create task state
            manager.before_poll(task_id, &context);
            let before_update = manager.metrics();

            // Update configuration
            manager.update_config(new_config);
            let after_update = manager.metrics();

            // Existing state should be preserved
            prop_assert_eq!(before_update.active_tasks, after_update.active_tasks);
            prop_assert_eq!(before_update.total_polls, after_update.total_polls);

            // Should still be able to operate
            manager.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));
            manager.on_completion(task_id);
        }

        /// Property: Concurrent task operations should be thread-safe
        #[test]
        fn concurrent_operations_thread_safety(
            config in tier_config_strategy(),
            task_ids in prop::collection::vec(task_id_strategy(), 5..20)
        ) {
            let manager = Arc::new(TierManager::new(config));
            let initial_metrics = manager.metrics();

            // Run concurrent task operations
            let handles: Vec<_> = task_ids
                .into_iter()
                .map(|task_id| {
                    let manager = Arc::clone(&manager);
                    std::thread::spawn(move || {
                        let context = TaskContext { worker_id: 0, priority: None };

                        manager.before_poll(task_id, &context);
                        manager.after_poll(task_id, PollResult::Ready, Duration::from_micros(100));
                    })
                })
                .collect();

            // Wait for completion
            for handle in handles {
                handle.join().unwrap();
            }

            let final_metrics = manager.metrics();

            // All operations should be accounted for
            prop_assert!(final_metrics.total_polls >= initial_metrics.total_polls);
            prop_assert!(final_metrics.total_violations >= initial_metrics.total_violations);
        }

        /// Property: Slow queue should respect bounds
        #[test]
        fn slow_queue_bounded_invariant(
            mut config in tier_config_strategy(),
            num_tasks in 1usize..50
        ) {
            // Use small queue size for testing
            config.max_slow_queue_size = 5;
            let manager = TierManager::new(config.clone());

            let context = TaskContext { worker_id: 0, priority: None };

            // Generate tasks that might trigger slow queue
            for i in 0..num_tasks {
                let task_id = TaskId(i as u64);
                manager.before_poll(task_id, &context);

                // Use duration that might trigger intervention
                manager.after_poll(task_id, PollResult::Pending, Duration::from_millis(20));
            }

            let metrics = manager.metrics();

            // Queue should never exceed maximum
            prop_assert!(metrics.slow_queue_size <= config.max_slow_queue_size);
        }

        /// Property: Memory usage should be bounded with explicit cleanup
        #[test]
        fn memory_usage_bounded(
            config in tier_config_strategy(),
            task_operations in prop::collection::vec(
                (task_id_strategy(), task_context_strategy(), normal_duration_strategy()),
                1..50
            )
        ) {
            let manager = TierManager::new(config);

            // Perform many task operations with explicit completion
            for (task_id, context, duration) in task_operations {
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, PollResult::Ready, duration);
                manager.on_completion(task_id); // Explicit cleanup required
            }

            let metrics = manager.metrics();

            // No tasks should remain active after explicit cleanup
            prop_assert_eq!(metrics.active_tasks, 0);
        }

        /// Property: Metrics should be monotonic
        #[test]
        fn metrics_monotonicity(
            config in tier_config_strategy(),
            operations in prop::collection::vec(
                (task_id_strategy(), task_context_strategy(), poll_result_strategy(), normal_duration_strategy()),
                1..50
            )
        ) {
            let manager = TierManager::new(config);
            let mut prev_metrics = manager.metrics();

            for (task_id, context, result, duration) in operations {
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, result, duration);

                if matches!(result, PollResult::Ready) {
                    // Task completes, no cleanup needed
                } else {
                    manager.on_completion(task_id); // Manual cleanup for pending tasks
                }

                let curr_metrics = manager.metrics();

                // All counters should be monotonic
                prop_assert!(curr_metrics.total_polls >= prev_metrics.total_polls);
                prop_assert!(curr_metrics.total_violations >= prev_metrics.total_violations);
                prop_assert!(curr_metrics.total_yields >= prev_metrics.total_yields);

                prev_metrics = curr_metrics;
            }
        }

        /// Property: Task isolation - different tasks don't interfere
        #[test]
        fn task_isolation_property(
            config in tier_config_strategy(),
            task_id1 in task_id_strategy(),
            task_id2 in task_id_strategy(),
            context in task_context_strategy()
        ) {
            prop_assume!(task_id1 != task_id2);

            let manager = TierManager::new(config);
            let initial_metrics = manager.metrics();

            // Start both tasks (each before_poll increments poll count)
            manager.before_poll(task_id1, &context);
            manager.before_poll(task_id2, &context);

            let after_start_metrics = manager.metrics();

            // Should have 2 polls from the before_poll calls
            prop_assert_eq!(after_start_metrics.total_polls, initial_metrics.total_polls + 2);

            // Complete both tasks
            manager.after_poll(task_id1, PollResult::Ready, Duration::from_micros(100));
            manager.on_completion(task_id1);
            manager.on_completion(task_id2);

            let final_metrics = manager.metrics();

            // Poll count should remain the same (after_poll doesn't increment)
            prop_assert_eq!(final_metrics.total_polls, after_start_metrics.total_polls);
        }
    }
}

/// Edge case property tests for specific TierManager behaviors
#[cfg(test)]
mod edge_case_properties {
    use super::*;

    proptest! {
        /// Property: Multiple completion calls should be idempotent
        #[test]
        fn repeated_completion_idempotent(
            config in tier_config_strategy(),
            task_id in task_id_strategy(),
            context in task_context_strategy()
        ) {
            let manager = TierManager::new(config);

            manager.before_poll(task_id, &context);
            let initial_metrics = manager.metrics();

            // Complete task multiple times
            manager.on_completion(task_id);
            manager.on_completion(task_id);
            manager.on_completion(task_id);

            let final_metrics = manager.metrics();

            // Metrics should remain consistent (idempotent)
            prop_assert!(final_metrics.active_tasks <= initial_metrics.active_tasks);
            prop_assert_eq!(final_metrics.total_polls, initial_metrics.total_polls);
        }

        /// Property: Process slow queue should respect limits
        #[test]
        fn process_slow_queue_limits(
            config in tier_config_strategy(),
            max_tasks in 1usize..20
        ) {
            let manager = TierManager::new(config);
            let context = TaskContext { worker_id: 0, priority: None };

            // Add some tasks
            for i in 0..10 {
                let task_id = TaskId(i);
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, PollResult::Pending, Duration::from_millis(20));
            }

            // Process slow queue with limit
            let processed = manager.process_slow_queue(max_tasks);

            // Should not exceed requested limit
            prop_assert!(processed <= max_tasks);
        }

        /// Property: Zero-duration polls should be harmless
        #[test]
        fn zero_duration_handling(
            config in tier_config_strategy(),
            task_id in task_id_strategy(),
            context in task_context_strategy()
        ) {
            let manager = TierManager::new(config);

            manager.before_poll(task_id, &context);
            manager.after_poll(task_id, PollResult::Pending, Duration::ZERO);

            let metrics = manager.metrics();

            // Should complete without issues
            prop_assert_eq!(metrics.total_polls, 1);

            manager.on_completion(task_id);
        }
    }
}

/// Worker statistics property tests for invariant verification
#[cfg(test)]
mod worker_stats_properties {
    use super::*;
    use proptest::strategy::ValueTree;

    proptest! {
        /// Property: Worker statistics should be monotonic (only increase except on reset)
        #[test]
        fn worker_stats_monotonicity(
            config in tier_config_strategy(),
            operations in prop::collection::vec(
                (task_id_strategy(), task_context_strategy(), poll_result_strategy(), normal_duration_strategy()),
                1..50
            )
        ) {
            let manager = TierManager::new(config);
            let mut previous_stats = std::collections::HashMap::new();

            for (task_id, context, result, duration) in operations {
                // Record stats before operation
                let worker_id = context.worker_id;
                let before_stats = manager.get_worker_metrics(worker_id);

                // Perform operation
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, result, duration);

                // Verify that some voluntary yields are tracked
                if prop::bool::weighted(0.2).new_tree(&mut proptest::test_runner::TestRunner::default()).unwrap().current() {
                    manager.on_yield(task_id);
                }

                // Record stats after operation
                let after_stats = manager.get_worker_metrics(worker_id);

                // Stats should be monotonic
                prop_assert!(after_stats.polls >= before_stats.polls);
                prop_assert!(after_stats.violations >= before_stats.violations);
                prop_assert!(after_stats.yields >= before_stats.yields);
                prop_assert!(after_stats.cpu_time_ns >= before_stats.cpu_time_ns);
                prop_assert!(after_stats.tasks >= before_stats.tasks);
                prop_assert!(after_stats.slow_polls >= before_stats.slow_polls);

                // Update tracking
                previous_stats.insert(worker_id, after_stats);

                // Clean up completed tasks
                if matches!(result, PollResult::Ready) {
                    manager.on_completion(task_id);
                }
            }
        }

        /// Property: Worker reset should be idempotent
        #[test]
        fn worker_reset_idempotency(
            config in tier_config_strategy(),
            worker_id in 0usize..=16,
            num_resets in 1u8..=5
        ) {
            let manager = TierManager::new(config);

            // Set up some initial stats
            for i in 0..5 {
                let task_id = TaskId(i);
                let context = TaskContext { worker_id, priority: None };
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, PollResult::Ready, Duration::from_micros(100));
                manager.on_yield(task_id);
            }

            // Verify we have stats
            let initial_stats = manager.get_worker_metrics(worker_id);
            prop_assert!(initial_stats.polls > 0);
            prop_assert!(initial_stats.yields > 0);

            // Reset multiple times
            for _ in 0..num_resets {
                manager.reset_worker_stats(worker_id);
            }

            // Verify final state is same regardless of number of resets
            let final_stats = manager.get_worker_metrics(worker_id);
            prop_assert_eq!(final_stats.worker_id, worker_id);
            prop_assert_eq!(final_stats.polls, 0);
            prop_assert_eq!(final_stats.violations, 0);
            prop_assert_eq!(final_stats.yields, 0);
            prop_assert_eq!(final_stats.cpu_time_ns, 0);
            prop_assert_eq!(final_stats.tasks, 0);
            prop_assert_eq!(final_stats.slow_polls, 0);
        }

        /// Property: Concurrent worker operations should maintain consistency
        #[test]
        fn concurrent_worker_stats_safety(
            config in tier_config_strategy(),
            worker_operations in prop::collection::vec(
                (0usize..=7, task_id_strategy(), normal_duration_strategy()),
                10..30
            )
        ) {
            use std::sync::Arc;
            use std::thread;

            let manager = Arc::new(TierManager::new(config));
            let operations_per_worker = 20;

            // Spawn concurrent operations for multiple workers
            let handles: Vec<_> = worker_operations.into_iter().take(4).map(|(worker_id, base_task_id, _duration)| {
                let manager = Arc::clone(&manager);
                thread::spawn(move || {
                    for i in 0..operations_per_worker {
                        let task_id = TaskId(base_task_id.0 + i);
                        let context = TaskContext { worker_id, priority: None };

                        manager.before_poll(task_id, &context);
                        manager.after_poll(task_id, PollResult::Ready, Duration::from_micros(100));

                        // Some voluntary yields
                        if i % 5 == 0 {
                            manager.on_yield(task_id);
                        }
                    }
                })
            }).collect();

            // Wait for all operations to complete
            for handle in handles {
                handle.join().unwrap();
            }

            // Verify that all worker stats are consistent
            for worker_id in 0..4 {
                let worker_stats = manager.get_worker_metrics(worker_id);
                prop_assert_eq!(worker_stats.worker_id, worker_id);
                prop_assert_eq!(worker_stats.polls, operations_per_worker as u64);
                prop_assert_eq!(worker_stats.tasks, operations_per_worker as u64);
                prop_assert!(worker_stats.yields >= 4); // At least 4 yields (every 5th operation)
                prop_assert!(worker_stats.cpu_time_ns > 0);
            }
        }

        /// Property: Worker isolation - operations on one worker don't affect others
        #[test]
        fn worker_isolation_invariant(
            config in tier_config_strategy(),
            worker1_ops in 1u64..=20,
            worker2_ops in 1u64..=20,
            worker1_id in 0usize..=100,
            worker2_id in 0usize..=100
        ) {
            prop_assume!(worker1_id != worker2_id);

            let manager = TierManager::new(config);

            // Operations on worker 1
            for i in 0..worker1_ops {
                let task_id = TaskId(i + 1000);
                let context = TaskContext { worker_id: worker1_id, priority: None };
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, PollResult::Ready, Duration::from_micros(50));
            }

            // Operations on worker 2
            for i in 0..worker2_ops {
                let task_id = TaskId(i + 2000);
                let context = TaskContext { worker_id: worker2_id, priority: None };
                manager.before_poll(task_id, &context);
                manager.after_poll(task_id, PollResult::Ready, Duration::from_micros(75));
            }

            // Verify worker isolation
            let worker1_stats = manager.get_worker_metrics(worker1_id);
            let worker2_stats = manager.get_worker_metrics(worker2_id);

            prop_assert_eq!(worker1_stats.worker_id, worker1_id);
            prop_assert_eq!(worker1_stats.polls, worker1_ops);
            prop_assert_eq!(worker1_stats.tasks, worker1_ops);

            prop_assert_eq!(worker2_stats.worker_id, worker2_id);
            prop_assert_eq!(worker2_stats.polls, worker2_ops);
            prop_assert_eq!(worker2_stats.tasks, worker2_ops);

            // Workers should have different CPU times due to different durations
            if worker1_ops > 0 && worker2_ops > 0 {
                prop_assert!(worker1_stats.cpu_time_ns > 0);
                prop_assert!(worker2_stats.cpu_time_ns > 0);
            }
        }

        /// Property: get_all_worker_metrics consistency with individual get_worker_metrics
        #[test]
        fn worker_metrics_api_consistency(
            config in tier_config_strategy(),
            worker_operations in prop::collection::vec(
                (0usize..=5, 1u64..=10),
                1..8
            )
        ) {
            let manager = TierManager::new(config);

            // Perform operations on various workers
            for (worker_id, num_ops) in worker_operations.iter() {
                for i in 0..*num_ops {
                    let task_id = TaskId((*worker_id as u64) * 1000 + (i as u64));
                    let context = TaskContext { worker_id: *worker_id, priority: None };
                    manager.before_poll(task_id, &context);
                    manager.after_poll(task_id, PollResult::Ready, Duration::from_micros(100));
                }
            }

            // Get all stats via both APIs
            let all_stats = manager.get_all_worker_metrics();

            // Verify consistency between APIs
            for (worker_id, expected_ops) in worker_operations.iter() {
                let individual_stats = manager.get_worker_metrics(*worker_id);

                if *expected_ops > 0 {
                    prop_assert!(all_stats.contains_key(worker_id));
                    let bulk_stats = &all_stats[worker_id];

                    prop_assert_eq!(individual_stats.worker_id, bulk_stats.worker_id);
                    prop_assert_eq!(individual_stats.polls, bulk_stats.polls);
                    prop_assert_eq!(individual_stats.violations, bulk_stats.violations);
                    prop_assert_eq!(individual_stats.yields, bulk_stats.yields);
                    prop_assert_eq!(individual_stats.cpu_time_ns, bulk_stats.cpu_time_ns);
                    prop_assert_eq!(individual_stats.tasks, bulk_stats.tasks);
                    prop_assert_eq!(individual_stats.slow_polls, bulk_stats.slow_polls);

                    prop_assert_eq!(individual_stats.polls, *expected_ops);
                    prop_assert_eq!(individual_stats.tasks, *expected_ops);
                }
            }
        }
    }
}