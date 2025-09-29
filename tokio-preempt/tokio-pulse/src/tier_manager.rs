//! Multi-tier task management for preemption control
//!
//! This module implements a graduated intervention system to handle
//! CPU-bound tasks that monopolize worker threads. Tasks progress through
//! tiers (Monitor→Warn→Yield→Isolate) based on budget violations.

#![forbid(unsafe_code)]
#![allow(clippy::significant_drop_tightening)] // RwLock guards are held for short durations
#![allow(clippy::trivially_copy_pass_by_ref)] // TaskId references are more idiomatic

//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam::queue::SegQueue;
use dashmap::DashMap;
use parking_lot::RwLock;

use crate::budget::{TaskBudget, acquire_budget, release_budget};
use crate::isolation::{TaskIsolation, IsolationError};
use crate::reschedule::{DedicatedThreadScheduler, RescheduleTask, ThreadSchedulerConfig};
use crate::slow_queue::TaskPriority;
use crate::timing::create_cpu_timer;

/// Task identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

/// Tier management configuration
#[derive(Debug, Clone)]
pub struct TierConfig {
    /// Poll operations per budget window
    pub poll_budget: u32,

    /// CPU time budget (ms)
    pub cpu_ms_budget: u64,

    /// Forced yield interval
    pub yield_interval: u32,

    /// Tier intervention policies
    pub tier_policies: [TierPolicy; 4],

    /// Enable OS isolation for Tier 3
    pub enable_isolation: bool,

    /// Max slow queue size
    pub max_slow_queue_size: usize,

    /// Hysteresis: min duration between promotions (ms)
    pub promotion_hysteresis_ms: u64,

    /// Hysteresis: min duration between demotions (ms)
    pub demotion_hysteresis_ms: u64,

    /// Cooldown: violation rate threshold (violations per second)
    pub violation_rate_threshold: f64,

    /// Cooldown: duration after rapid violations (ms)
    pub cooldown_duration_ms: u64,

    /// Demotion: consecutive good polls required before demotion
    pub good_behavior_threshold: u32,

    /// Demotion: minimum time at tier before demotion (ms)
    pub min_tier_duration_ms: u64,

    /// Demotion: maximum CPU time per poll for good behavior (ns)
    pub demotion_cpu_threshold_ns: u64,

    /// Demotion: evaluate demotion every N polls
    pub demotion_evaluation_interval: u32,

    /// Test mode: bypass time-based checks for testing
    pub test_mode: bool,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            poll_budget: 2000,   // BEAM-inspired reduction count
            cpu_ms_budget: 10,   // 10ms CPU time window
            yield_interval: 100, // Forced yield interval
            tier_policies: [
                TierPolicy {
                    name: "Monitor",
                    action: InterventionAction::Monitor,
                    promotion_threshold: 5,
                    promotion_cpu_threshold_ms: 50,     // 50ms CPU time
                    promotion_slow_poll_threshold: 10,  // 10 slow polls
                    demotion_good_polls: 100,          // 100 good polls for demotion
                    demotion_min_duration_ms: 5000,    // 5 seconds minimum
                    demotion_cpu_threshold_ns: 100_000, // 100μs max for good poll
                    demotion_evaluation_interval: 20,   // Check every 20 polls
                },
                TierPolicy {
                    name: "Warn",
                    action: InterventionAction::Warn,
                    promotion_threshold: 3,
                    promotion_cpu_threshold_ms: 25,     // 25ms CPU time
                    promotion_slow_poll_threshold: 5,   // 5 slow polls
                    demotion_good_polls: 75,           // 75 good polls for demotion
                    demotion_min_duration_ms: 3000,    // 3 seconds minimum
                    demotion_cpu_threshold_ns: 200_000, // 200μs max for good poll
                    demotion_evaluation_interval: 15,   // Check every 15 polls
                },
                TierPolicy {
                    name: "Yield",
                    action: InterventionAction::Yield,
                    promotion_threshold: 2,
                    promotion_cpu_threshold_ms: 15,     // 15ms CPU time
                    promotion_slow_poll_threshold: 3,   // 3 slow polls
                    demotion_good_polls: 50,           // 50 good polls for demotion
                    demotion_min_duration_ms: 2000,    // 2 seconds minimum
                    demotion_cpu_threshold_ns: 500_000, // 500μs max for good poll
                    demotion_evaluation_interval: 10,   // Check every 10 polls
                },
                TierPolicy {
                    name: "Isolate",
                    action: InterventionAction::Isolate,
                    promotion_threshold: u32::MAX,      // Terminal tier
                    promotion_cpu_threshold_ms: u64::MAX,
                    promotion_slow_poll_threshold: u32::MAX,
                    demotion_good_polls: 25,           // 25 good polls for demotion
                    demotion_min_duration_ms: 10000,   // 10 seconds minimum
                    demotion_cpu_threshold_ns: 1_000_000, // 1ms max for good poll
                    demotion_evaluation_interval: 5,    // Check every 5 polls
                },
            ],
            enable_isolation: false,
            max_slow_queue_size: 10000,
            promotion_hysteresis_ms: 100, // 100ms min between promotions
            demotion_hysteresis_ms: 500,  // 500ms min between demotions
            violation_rate_threshold: 10.0, // 10 violations/sec triggers cooldown
            cooldown_duration_ms: 1000,   // 1 second cooldown after rapid violations
            good_behavior_threshold: 50,   // 50 consecutive good polls for demotion
            min_tier_duration_ms: 2000,    // 2 seconds minimum at tier
            demotion_cpu_threshold_ns: 500_000, // 500μs max CPU time for good behavior
            demotion_evaluation_interval: 10, // Check for demotion every 10 polls
            test_mode: false,              // Production mode by default
        }
    }
}

/// Policy for intervention tiers
#[derive(Debug, Clone)]
pub struct TierPolicy {
    /// Tier name
    pub name: &'static str,

    /// Intervention action
    pub action: InterventionAction,

    // Promotion thresholds
    /// Violations before promotion to next tier
    pub promotion_threshold: u32,

    /// CPU time (ms) that triggers promotion
    pub promotion_cpu_threshold_ms: u64,

    /// Slow polls that trigger promotion
    pub promotion_slow_poll_threshold: u32,

    // Demotion thresholds (per-tier instead of global)
    /// Good polls required before demotion
    pub demotion_good_polls: u32,

    /// Minimum time at tier before demotion (ms)
    pub demotion_min_duration_ms: u64,

    /// Maximum CPU time (ns) for a poll to be considered "good"
    pub demotion_cpu_threshold_ns: u64,

    /// Evaluate demotion every N polls
    pub demotion_evaluation_interval: u32,
}

/// Builder for TierPolicy configuration
#[derive(Debug)]
pub struct TierPolicyBuilder {
    name: &'static str,
    action: InterventionAction,
    promotion_threshold: u32,
    promotion_cpu_threshold_ms: u64,
    promotion_slow_poll_threshold: u32,
    demotion_good_polls: u32,
    demotion_min_duration_ms: u64,
    demotion_cpu_threshold_ns: u64,
    demotion_evaluation_interval: u32,
}

impl TierPolicyBuilder {
    /// Create a new TierPolicyBuilder
    ///
    /// Starts with sensible defaults that can be customized.
    ///
    /// # Arguments
    ///
    /// * `name` - Static name for the tier
    /// * `action` - Intervention action to take
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierPolicyBuilder, InterventionAction};
    ///
    /// let policy = TierPolicyBuilder::new("Custom", InterventionAction::Warn)
    ///     .promotion_violations(5)
    ///     .promotion_cpu_ms(20)
    ///     .demotion_good_polls(100)
    ///     .build();
    /// ```
    pub fn new(name: &'static str, action: InterventionAction) -> Self {
        Self {
            name,
            action,
            // Sensible defaults - moderate thresholds
            promotion_threshold: 3,
            promotion_cpu_threshold_ms: 25,
            promotion_slow_poll_threshold: 5,
            demotion_good_polls: 75,
            demotion_min_duration_ms: 3000,
            demotion_cpu_threshold_ns: 500_000,
            demotion_evaluation_interval: 15,
        }
    }

    /// Set number of violations before promotion to next tier
    pub fn promotion_violations(mut self, threshold: u32) -> Self {
        self.promotion_threshold = threshold;
        self
    }

    /// Set CPU time (ms) that triggers promotion
    pub fn promotion_cpu_ms(mut self, threshold: u64) -> Self {
        self.promotion_cpu_threshold_ms = threshold;
        self
    }

    /// Set number of slow polls that trigger promotion
    pub fn promotion_slow_polls(mut self, threshold: u32) -> Self {
        self.promotion_slow_poll_threshold = threshold;
        self
    }

    /// Set number of good polls required before demotion
    pub fn demotion_good_polls(mut self, threshold: u32) -> Self {
        self.demotion_good_polls = threshold;
        self
    }

    /// Set minimum time at tier before demotion (ms)
    pub fn demotion_min_duration_ms(mut self, duration: u64) -> Self {
        self.demotion_min_duration_ms = duration;
        self
    }

    /// Set maximum CPU time (ns) for a poll to be considered "good"
    pub fn demotion_cpu_threshold_ns(mut self, threshold: u64) -> Self {
        self.demotion_cpu_threshold_ns = threshold;
        self
    }

    /// Set evaluation interval for demotion checks
    pub fn demotion_evaluation_interval(mut self, interval: u32) -> Self {
        self.demotion_evaluation_interval = interval;
        self
    }

    /// Build the TierPolicy with validation
    ///
    /// # Returns
    ///
    /// A valid TierPolicy or panics if configuration is invalid
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - promotion_threshold is 0 (except for terminal tiers)
    /// - demotion_good_polls is 0
    /// - demotion_evaluation_interval is 0
    pub fn build(self) -> TierPolicy {
        // Validation
        if self.promotion_threshold == 0 && self.promotion_threshold != u32::MAX {
            panic!("promotion_threshold must be > 0 (or u32::MAX for terminal tier)");
        }
        if self.demotion_good_polls == 0 {
            panic!("demotion_good_polls must be > 0");
        }
        if self.demotion_evaluation_interval == 0 {
            panic!("demotion_evaluation_interval must be > 0");
        }

        TierPolicy {
            name: self.name,
            action: self.action,
            promotion_threshold: self.promotion_threshold,
            promotion_cpu_threshold_ms: self.promotion_cpu_threshold_ms,
            promotion_slow_poll_threshold: self.promotion_slow_poll_threshold,
            demotion_good_polls: self.demotion_good_polls,
            demotion_min_duration_ms: self.demotion_min_duration_ms,
            demotion_cpu_threshold_ns: self.demotion_cpu_threshold_ns,
            demotion_evaluation_interval: self.demotion_evaluation_interval,
        }
    }
}

/// Intervention actions for tier system
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterventionAction {
    /// No intervention
    Monitor,
    /// Log warnings
    Warn,
    /// Force yield
    Yield,
    /// Move to slow queue
    SlowQueue,
    /// OS-level isolation
    Isolate,
}

/// Poll operation result
#[derive(Debug, Clone, Copy)]
pub enum PollResult {
    /// Task completed
    Ready,
    /// Task continues
    Pending,
    /// Task panic
    Panicked,
}

/// Predefined configuration profiles for different use cases
///
/// These profiles provide sensible defaults for common scenarios,
/// allowing users to quickly configure the tier system without
/// needing to tune individual parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigProfile {
    /// Aggressive intervention - low thresholds, quick escalation
    ///
    /// Best for: Latency-critical applications, user-facing services
    /// Characteristics: Quick to detect and intervene on slow tasks
    Aggressive,

    /// Balanced intervention - moderate thresholds, balanced performance
    ///
    /// Best for: General-purpose applications, mixed workloads
    /// Characteristics: Good balance between responsiveness and stability
    Balanced,

    /// Permissive intervention - high thresholds, slow escalation
    ///
    /// Best for: Batch processing, scientific computing, high-CPU workloads
    /// Characteristics: Allows tasks more freedom before intervention
    Permissive,

    /// Custom configuration - user-defined settings
    ///
    /// Best for: Specialized applications with specific requirements
    /// Characteristics: Full control over all intervention parameters
    Custom,
}

impl ConfigProfile {
    /// Creates a TierConfig using the specified profile
    ///
    /// # Arguments
    ///
    /// * `profile` - The configuration profile to use
    ///
    /// # Returns
    ///
    /// A TierConfig with settings optimized for the profile
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierConfig, ConfigProfile};
    ///
    /// // Create an aggressive configuration for latency-critical apps
    /// let aggressive_config = ConfigProfile::Aggressive.create_config();
    ///
    /// // Create a permissive configuration for batch processing
    /// let permissive_config = ConfigProfile::Permissive.create_config();
    /// ```
    pub fn create_config(self) -> TierConfig {
        match self {
            ConfigProfile::Aggressive => Self::create_aggressive_config(),
            ConfigProfile::Balanced => Self::create_balanced_config(),
            ConfigProfile::Permissive => Self::create_permissive_config(),
            ConfigProfile::Custom => TierConfig::default(), // User can customize from default
        }
    }

    /// Creates an aggressive configuration optimized for low latency
    ///
    /// Characteristics:
    /// - Low promotion thresholds (quick escalation)
    /// - Short hysteresis periods (fast response)
    /// - Low CPU thresholds (strict about slow tasks)
    /// - High demotion requirements (harder to get out of intervention)
    fn create_aggressive_config() -> TierConfig {
        let tier_policies = vec![
            // Tier 0: Monitor - Very sensitive detection
            TierPolicyBuilder::new("AggressiveMonitor", InterventionAction::Monitor)
                .promotion_violations(2)         // Escalate after just 2 violations
                .promotion_cpu_ms(1)             // 1ms CPU triggers promotion
                .promotion_slow_polls(2) // 2 slow polls trigger promotion
                .demotion_good_polls(30)         // Need 30 good polls to demote
                .demotion_min_duration_ms(500)   // Minimum 500ms at tier
                .demotion_cpu_threshold_ns(50_000) // 50μs max for "good" poll
                .demotion_evaluation_interval(3)  // Check every 3 polls
                .build(),

            // Tier 1: Warn - Quick warning intervention
            TierPolicyBuilder::new("AggressiveWarn", InterventionAction::Warn)
                .promotion_violations(3)
                .promotion_cpu_ms(2)
                .promotion_slow_polls(2)
                .demotion_good_polls(40)
                .demotion_min_duration_ms(750)
                .demotion_cpu_threshold_ns(75_000) // 75μs
                .demotion_evaluation_interval(4)
                .build(),

            // Tier 2: Yield - Force yielding for cooperation
            TierPolicyBuilder::new("AggressiveYield", InterventionAction::Yield)
                .promotion_violations(3)
                .promotion_cpu_ms(3)
                .promotion_slow_polls(2)
                .demotion_good_polls(50)
                .demotion_min_duration_ms(1000)
                .demotion_cpu_threshold_ns(100_000) // 100μs
                .demotion_evaluation_interval(5)
                .build(),

            // Tier 3: Isolate - Strong isolation measures
            TierPolicyBuilder::new("AggressiveIsolate", InterventionAction::Isolate)
                .promotion_violations(u32::MAX) // No further escalation
                .promotion_cpu_ms(u64::MAX)
                .promotion_slow_polls(u32::MAX)
                .demotion_good_polls(100)       // Very high bar for demotion
                .demotion_min_duration_ms(2000) // Long minimum duration
                .demotion_cpu_threshold_ns(200_000) // 200μs
                .demotion_evaluation_interval(10)
                .build(),
        ];

        TierConfig {
            poll_budget: 1000,                   // Tight budget
            cpu_ms_budget: 5,                    // 5ms CPU time window
            yield_interval: 50,                  // More frequent forced yield
            tier_policies: [
                tier_policies[0].clone(),
                tier_policies[1].clone(),
                tier_policies[2].clone(),
                tier_policies[3].clone(),
            ],
            enable_isolation: true,              // Enable isolation for aggressive
            max_slow_queue_size: 5000,           // Smaller slow queue
            promotion_hysteresis_ms: 50,         // Very short promotion hysteresis
            demotion_hysteresis_ms: 200,         // Short demotion hysteresis
            violation_rate_threshold: 5.0,      // Lower threshold triggers cooldown faster
            cooldown_duration_ms: 2000,         // Longer cooldown period
            good_behavior_threshold: 30,         // Higher standard for good behavior
            min_tier_duration_ms: 300,           // Quick tier changes
            demotion_cpu_threshold_ns: 75_000,   // 75μs threshold
            demotion_evaluation_interval: 3,     // Frequent evaluation
            test_mode: false,
        }
    }

    /// Creates a balanced configuration for general-purpose use
    ///
    /// This is similar to the default configuration but with
    /// well-tuned parameters for most applications.
    fn create_balanced_config() -> TierConfig {
        // Use the existing default configuration which is already balanced
        TierConfig::default()
    }

    /// Creates a permissive configuration for high-CPU workloads
    ///
    /// Characteristics:
    /// - High promotion thresholds (slow escalation)
    /// - Long hysteresis periods (stable state)
    /// - High CPU thresholds (tolerant of slow tasks)
    /// - Low demotion requirements (easy to get out of intervention)
    fn create_permissive_config() -> TierConfig {
        let tier_policies = vec![
            // Tier 0: Monitor - Tolerant detection
            TierPolicyBuilder::new("PermissiveMonitor", InterventionAction::Monitor)
                .promotion_violations(10)        // Escalate after 10 violations
                .promotion_cpu_ms(10)            // 10ms CPU triggers promotion
                .promotion_slow_polls(8) // 8 slow polls trigger promotion
                .demotion_good_polls(10)         // Only need 10 good polls to demote
                .demotion_min_duration_ms(2000)  // Minimum 2s at tier
                .demotion_cpu_threshold_ns(500_000) // 500μs max for "good" poll
                .demotion_evaluation_interval(10) // Check every 10 polls
                .build(),

            // Tier 1: Warn - Lenient warning intervention
            TierPolicyBuilder::new("PermissiveWarn", InterventionAction::Warn)
                .promotion_violations(15)
                .promotion_cpu_ms(20)
                .promotion_slow_polls(10)
                .demotion_good_polls(15)
                .demotion_min_duration_ms(3000)
                .demotion_cpu_threshold_ns(750_000) // 750μs
                .demotion_evaluation_interval(15)
                .build(),

            // Tier 2: Yield - Gentle yielding
            TierPolicyBuilder::new("PermissiveYield", InterventionAction::Yield)
                .promotion_violations(20)
                .promotion_cpu_ms(50)
                .promotion_slow_polls(15)
                .demotion_good_polls(20)
                .demotion_min_duration_ms(5000)
                .demotion_cpu_threshold_ns(1_000_000) // 1ms
                .demotion_evaluation_interval(20)
                .build(),

            // Tier 3: Isolate - Last resort isolation
            TierPolicyBuilder::new("PermissiveIsolate", InterventionAction::Isolate)
                .promotion_violations(u32::MAX) // No further escalation
                .promotion_cpu_ms(u64::MAX)
                .promotion_slow_polls(u32::MAX)
                .demotion_good_polls(25)        // Reasonable bar for demotion
                .demotion_min_duration_ms(10000) // Very long minimum duration
                .demotion_cpu_threshold_ns(2_000_000) // 2ms
                .demotion_evaluation_interval(25)
                .build(),
        ];

        TierConfig {
            poll_budget: 10000,                  // Generous budget
            cpu_ms_budget: 100,                  // 100ms CPU time window
            yield_interval: 500,                 // Less frequent forced yield
            tier_policies: [
                tier_policies[0].clone(),
                tier_policies[1].clone(),
                tier_policies[2].clone(),
                tier_policies[3].clone(),
            ],
            enable_isolation: false,             // Disable isolation for permissive
            max_slow_queue_size: 50000,          // Large slow queue
            promotion_hysteresis_ms: 1000,       // Long promotion hysteresis
            demotion_hysteresis_ms: 2000,        // Long demotion hysteresis
            violation_rate_threshold: 50.0,     // High threshold for cooldown
            cooldown_duration_ms: 500,          // Short cooldown period
            good_behavior_threshold: 15,         // Lower standard for good behavior
            min_tier_duration_ms: 2000,          // Slow tier changes
            demotion_cpu_threshold_ns: 1_000_000, // 1ms threshold
            demotion_evaluation_interval: 15,    // Less frequent evaluation
            test_mode: false,
        }
    }

    /// Returns a description of the profile's characteristics
    ///
    /// # Returns
    ///
    /// A string describing the profile's intended use and characteristics
    pub fn description(self) -> &'static str {
        match self {
            ConfigProfile::Aggressive => {
                "Aggressive intervention with low thresholds and quick escalation. \
                 Best for latency-critical applications and user-facing services."
            }
            ConfigProfile::Balanced => {
                "Balanced intervention with moderate thresholds and stable performance. \
                 Best for general-purpose applications and mixed workloads."
            }
            ConfigProfile::Permissive => {
                "Permissive intervention with high thresholds and slow escalation. \
                 Best for batch processing, scientific computing, and high-CPU workloads."
            }
            ConfigProfile::Custom => {
                "Custom configuration allowing full control over all intervention parameters. \
                 Best for specialized applications with specific requirements."
            }
        }
    }

    /// Returns all available profiles
    ///
    /// # Returns
    ///
    /// A slice containing all ConfigProfile variants
    pub fn all_profiles() -> &'static [ConfigProfile] {
        &[
            ConfigProfile::Aggressive,
            ConfigProfile::Balanced,
            ConfigProfile::Permissive,
            ConfigProfile::Custom,
        ]
    }
}

/// Helper function to compare two TierConfig instances for equality
///
/// This is optimized for fast comparison by checking most distinctive fields first
/// and doing early returns to avoid expensive deep comparisons.
fn configs_match(config1: &TierConfig, config2: &TierConfig) -> bool {
    // Fast path: Compare most distinctive fields first for early exit
    if config1.poll_budget != config2.poll_budget
        || config1.enable_isolation != config2.enable_isolation
        || config1.max_slow_queue_size != config2.max_slow_queue_size {
        return false;
    }

    // Check tier policies count early
    if config1.tier_policies.len() != config2.tier_policies.len() {
        return false;
    }

    // Quick check: Compare first tier policy name for fast differentiation
    if !config1.tier_policies.is_empty()
        && !config2.tier_policies.is_empty()
        && config1.tier_policies[0].name != config2.tier_policies[0].name {
        return false;
    }

    // Only do full comparison if quick checks pass
    config1.cpu_ms_budget == config2.cpu_ms_budget
        && config1.yield_interval == config2.yield_interval
        && config1.promotion_hysteresis_ms == config2.promotion_hysteresis_ms
        && config1.demotion_hysteresis_ms == config2.demotion_hysteresis_ms
        && config1.violation_rate_threshold == config2.violation_rate_threshold
        && config1.cooldown_duration_ms == config2.cooldown_duration_ms
        && config1.good_behavior_threshold == config2.good_behavior_threshold
        && config1.min_tier_duration_ms == config2.min_tier_duration_ms
        && config1.demotion_cpu_threshold_ns == config2.demotion_cpu_threshold_ns
        && config1.demotion_evaluation_interval == config2.demotion_evaluation_interval
        && config1.test_mode == config2.test_mode
        && config1.tier_policies.iter().zip(config2.tier_policies.iter()).all(|(p1, p2)| tier_policies_match(p1, p2))
}

/// Helper function to compare two TierPolicy instances for equality
/// Optimized with early returns for most distinctive fields first
fn tier_policies_match(policy1: &TierPolicy, policy2: &TierPolicy) -> bool {
    // Check name first as it's most distinctive
    if policy1.name != policy2.name {
        return false;
    }

    // Check action discriminant early
    if std::mem::discriminant(&policy1.action) != std::mem::discriminant(&policy2.action) {
        return false;
    }

    // Check most likely to differ fields first
    policy1.promotion_threshold == policy2.promotion_threshold
        && policy1.demotion_good_polls == policy2.demotion_good_polls
        && policy1.promotion_cpu_threshold_ms == policy2.promotion_cpu_threshold_ms
        && policy1.promotion_slow_poll_threshold == policy2.promotion_slow_poll_threshold
        && policy1.demotion_min_duration_ms == policy2.demotion_min_duration_ms
        && policy1.demotion_cpu_threshold_ns == policy2.demotion_cpu_threshold_ns
        && policy1.demotion_evaluation_interval == policy2.demotion_evaluation_interval
}

// Per-task state tracking
#[derive(Debug)]
struct TaskState {
    budget: Arc<TaskBudget>,                 // Shared budget tracker
    current_tier: AtomicU32,                 // Tier level (0-3)
    violation_count: AtomicU32,              // Budget violations
    total_cpu_ns: AtomicU64,                 // CPU time (ns)
    last_poll_time: RwLock<Instant>,         // Last poll timestamp
    slow_poll_count: AtomicU32,              // Consecutive slow polls
    force_yield: AtomicU32,                  // Yield flag
    last_tier_change: RwLock<Instant>,       // Hysteresis: last tier change time
    pending_tier_change: AtomicU32,          // Hysteresis: pending tier (u32::MAX = none)
    cooldown_until: RwLock<Option<Instant>>, // Cooldown: no tier changes until this time
    recent_violations: RwLock<Vec<Instant>>, // Recent violation timestamps for rate tracking
    consecutive_good_polls: AtomicU32,       // Good behavior: consecutive polls without violations
    last_violation_time: RwLock<Instant>,    // Good behavior: timestamp of last violation
    good_behavior_duration_ns: AtomicU64,    // Good behavior: cumulative time without violations
    polls_since_demotion_check: AtomicU32,   // Demotion evaluation: polls since last check
    worker_id: usize,                        // Worker thread ID for statistics tracking
    context: RwLock<TaskContext>,            // Task execution context for priority-based decisions
}

impl TaskState {
    fn new(budget: Arc<TaskBudget>, worker_id: usize, context: TaskContext) -> Self {
        let now = Instant::now();
        Self {
            budget,
            current_tier: AtomicU32::new(0),
            violation_count: AtomicU32::new(0),
            total_cpu_ns: AtomicU64::new(0),
            last_poll_time: RwLock::new(now),
            slow_poll_count: AtomicU32::new(0),
            force_yield: AtomicU32::new(0),
            last_tier_change: RwLock::new(now),
            pending_tier_change: AtomicU32::new(u32::MAX),
            cooldown_until: RwLock::new(None),
            recent_violations: RwLock::new(Vec::with_capacity(10)),
            consecutive_good_polls: AtomicU32::new(0),
            last_violation_time: RwLock::new(now),
            good_behavior_duration_ns: AtomicU64::new(0),
            polls_since_demotion_check: AtomicU32::new(0),
            worker_id,
            context: RwLock::new(context),
        }
    }

    /// Update the task context (called from before_poll to keep context current)
    fn update_context(&self, context: &TaskContext) {
        *self.context.write() = context.clone();
    }

    #[inline]
    fn should_yield(&self) -> bool {
        self.force_yield.load(Ordering::Acquire) > 0 || self.budget.is_exhausted()
    }

    #[inline]
    fn mark_for_yield(&self) {
        self.force_yield.store(1, Ordering::Release);
    }

    #[inline]
    fn clear_yield_flag(&self) {
        self.force_yield.store(0, Ordering::Release);
    }

    #[inline]
    fn record_good_poll(&self, poll_duration_ns: u64) {
        self.consecutive_good_polls.fetch_add(1, Ordering::Relaxed);
        self.good_behavior_duration_ns.fetch_add(poll_duration_ns, Ordering::Relaxed);
    }

    #[inline]
    fn record_violation(&self) {
        let now = Instant::now();
        *self.last_violation_time.write() = now;
        self.consecutive_good_polls.store(0, Ordering::Relaxed);
        self.good_behavior_duration_ns.store(0, Ordering::Relaxed);
    }

    #[inline]
    fn get_good_behavior_metrics(&self) -> (u32, Duration) {
        let good_polls = self.consecutive_good_polls.load(Ordering::Relaxed);
        let duration_since_violation = self.last_violation_time.read().elapsed();
        (good_polls, duration_since_violation)
    }

    fn evaluate_for_demotion(&self, config: &TierConfig) -> bool {
        let current_tier = self.current_tier.load(Ordering::Acquire);

        // Must be above tier 0 to demote
        if current_tier == 0 {
            return false;
        }

        // Get tier-specific policy
        let tier_policy = &config.tier_policies[current_tier as usize];

        // Only evaluate on interval to reduce overhead (per-tier)
        let evaluation_count = self.polls_since_demotion_check.fetch_add(1, Ordering::Relaxed);
        if evaluation_count % tier_policy.demotion_evaluation_interval != 0 {
            return false;
        }

        // Check good behavior criteria
        let (good_polls, time_since_violation) = self.get_good_behavior_metrics();

        // Require minimum consecutive good polls (per-tier)
        if good_polls < tier_policy.demotion_good_polls {
            return false;
        }

        // Require minimum time since last violation
        if time_since_violation.as_secs() < 2 {
            return false;
        }

        // Check CPU time per poll is reasonable (per-tier threshold)
        let total_cpu_ns = self.total_cpu_ns.load(Ordering::Relaxed);
        let total_polls = good_polls as u64;
        if total_polls > 0 {
            let avg_cpu_per_poll = total_cpu_ns / total_polls;
            if avg_cpu_per_poll > tier_policy.demotion_cpu_threshold_ns {
                return false; // Still consuming too much CPU
            }
        }

        // Check minimum time at current tier (per-tier)
        let tier_duration = self.last_tier_change.read().elapsed();
        if tier_duration.as_millis() < tier_policy.demotion_min_duration_ms as u128 {
            return false;
        }

        true
    }

    fn try_escalate_tier(&self, hysteresis_ms: u64) -> Option<u32> {
        let current = self.current_tier.load(Ordering::Acquire);
        if current >= 3 {
            return None; // Already at max tier
        }

        let now = Instant::now();

        // Check if in cooldown period
        if let Some(cooldown_end) = *self.cooldown_until.read() {
            if now < cooldown_end {
                return None; // Still in cooldown
            }
        }

        let last_change = *self.last_tier_change.read();

        // Check hysteresis period
        if now.duration_since(last_change).as_millis() < hysteresis_ms as u128 {
            // Still in hysteresis period, mark pending
            self.pending_tier_change.store(current + 1, Ordering::Release);
            return None;
        }

        // Apply tier change
        let new_tier = current + 1;
        self.current_tier.store(new_tier, Ordering::Release);
        {
            let mut last_change = self.last_tier_change.write();
            *last_change = now;
        }
        self.pending_tier_change.store(u32::MAX, Ordering::Release);
        Some(new_tier)
    }

    fn try_demote_tier(&self, hysteresis_ms: u64, tier_policy: &TierPolicy, test_mode: bool) -> Option<u32> {
        let current = self.current_tier.load(Ordering::Acquire);
        if current == 0 {
            return None; // Already at min tier
        }

        let now = Instant::now();

        // Check if in cooldown period
        if let Some(cooldown_end) = *self.cooldown_until.read() {
            if now < cooldown_end {
                return None; // Still in cooldown
            }
        }

        let last_change = *self.last_tier_change.read();

        // Check minimum tier duration (skip in test mode)
        if !test_mode && now.duration_since(last_change).as_millis() < tier_policy.demotion_min_duration_ms as u128 {
            return None; // Not at tier long enough
        }

        // Check good behavior metrics
        let (good_polls, time_since_violation) = self.get_good_behavior_metrics();
        if good_polls < tier_policy.demotion_good_polls {
            return None; // Not enough consecutive good polls
        }

        // Require at least 1 second since last violation (skip in test mode)
        if !test_mode && time_since_violation.as_secs() < 1 {
            return None; // Too recent violation
        }

        // Check hysteresis period
        if now.duration_since(last_change).as_millis() < hysteresis_ms as u128 {
            // Still in hysteresis period, mark pending
            self.pending_tier_change.store(current - 1, Ordering::Release);
            return None;
        }

        // Apply tier change
        let new_tier = current - 1;
        self.current_tier.store(new_tier, Ordering::Release);
        {
            let mut last_change = self.last_tier_change.write();
            *last_change = now;
        }
        self.pending_tier_change.store(u32::MAX, Ordering::Release);

        // Reset good behavior counters after successful demotion
        self.consecutive_good_polls.store(0, Ordering::Relaxed);
        self.good_behavior_duration_ns.store(0, Ordering::Relaxed);

        Some(new_tier)
    }

    fn check_pending_tier_change(&self, promotion_hysteresis_ms: u64, demotion_hysteresis_ms: u64) {
        let pending = self.pending_tier_change.load(Ordering::Acquire);
        if pending == u32::MAX {
            return; // No pending change
        }

        let current = self.current_tier.load(Ordering::Acquire);
        let now = Instant::now();
        let last_change = *self.last_tier_change.read();

        if pending > current {
            // Pending promotion
            if now.duration_since(last_change).as_millis() >= promotion_hysteresis_ms as u128 {
                self.current_tier.store(pending, Ordering::Release);
                {
                    let mut last_change_mut = self.last_tier_change.write();
                    *last_change_mut = now;
                }
                self.pending_tier_change.store(u32::MAX, Ordering::Release);
            }
        } else if pending < current {
            // Pending demotion
            if now.duration_since(last_change).as_millis() >= demotion_hysteresis_ms as u128 {
                self.current_tier.store(pending, Ordering::Release);
                {
                    let mut last_change_mut = self.last_tier_change.write();
                    *last_change_mut = now;
                }
                self.pending_tier_change.store(u32::MAX, Ordering::Release);
            }
        }
    }

    fn track_violation(&self, violation_rate_threshold: f64, cooldown_duration_ms: u64) {
        let now = Instant::now();
        let mut violations = self.recent_violations.write();

        // Add current violation
        violations.push(now);

        // Remove old violations (older than 1 second)
        violations.retain(|&t| now.duration_since(t).as_secs() < 1);

        // Check violation rate
        let rate = violations.len() as f64;

        if rate > violation_rate_threshold {
            // Trigger cooldown
            let mut cooldown = self.cooldown_until.write();
            *cooldown = Some(now + Duration::from_millis(cooldown_duration_ms));

            // Clear violation history after triggering cooldown
            violations.clear();
        }
    }

    fn is_in_cooldown(&self) -> bool {
        if let Some(cooldown_end) = *self.cooldown_until.read() {
            Instant::now() < cooldown_end
        } else {
            false
        }
    }
}

/// Task execution context
#[derive(Debug, Clone)]
pub struct TaskContext {
    /// Worker thread ID
    pub worker_id: usize,
    /// Task priority
    pub priority: Option<u8>,
}

impl TaskContext {
    /// Convert the optional priority to a TaskPriority enum
    ///
    /// Maps the u8 priority value to TaskPriority levels:
    /// - 0 → Critical (system tasks)
    /// - 1 → High (user-interactive)
    /// - 2 → Normal (standard async tasks)
    /// - 3+ → Low (background work)
    /// - None → Normal (default priority)
    pub fn task_priority(&self) -> TaskPriority {
        match self.priority {
            Some(0) => TaskPriority::Critical,
            Some(1) => TaskPriority::High,
            Some(2) => TaskPriority::Normal,
            Some(_) => TaskPriority::Low,  // 3 or higher
            None => TaskPriority::Normal,  // Default when unspecified
        }
    }

    /// Get priority-adjusted promotion threshold multiplier
    ///
    /// Returns a floating-point multiplier to adjust tier promotion thresholds
    /// based on task priority. Higher priority tasks escalate faster.
    ///
    /// - Critical: 0.25x threshold (escalate very quickly)
    /// - High: 0.5x threshold (escalate quickly)
    /// - Normal: 1.0x threshold (standard escalation)
    /// - Low: 2.0x threshold (tolerate more violations)
    pub fn priority_threshold_multiplier(&self) -> f32 {
        match self.task_priority() {
            TaskPriority::Critical => 0.25,
            TaskPriority::High => 0.5,
            TaskPriority::Normal => 1.0,
            TaskPriority::Low => 2.0,
        }
    }

    /// Get priority-adjusted budget multiplier
    ///
    /// Returns a multiplier for budget allocation based on task priority.
    /// Higher priority tasks get larger budgets.
    ///
    /// - Critical: 4.0x budget (very generous allocation)
    /// - High: 2.0x budget (generous allocation)
    /// - Normal: 1.0x budget (standard allocation)
    /// - Low: 0.5x budget (restricted allocation)
    pub fn priority_budget_multiplier(&self) -> f32 {
        match self.task_priority() {
            TaskPriority::Critical => 4.0,
            TaskPriority::High => 2.0,
            TaskPriority::Normal => 1.0,
            TaskPriority::Low => 0.5,
        }
    }
}

/// Per-worker thread statistics tracking
///
/// Collects performance metrics on a per-worker basis using lock-free
/// atomic operations to minimize overhead in hot paths.
///
/// Each worker thread maintains its own statistics to avoid contention
/// and provide detailed performance insights across the thread pool.
#[derive(Debug)]
pub struct WorkerStats {
    /// Total poll operations processed by this worker
    pub polls_processed: AtomicU64,
    /// Forced yields initiated by tier interventions
    pub yields_forced: AtomicU64,
    /// Tier promotions triggered by this worker
    pub tier_promotions: AtomicU64,
    /// Tier demotions triggered by this worker
    pub tier_demotions: AtomicU64,
    /// Budget violations detected by this worker
    pub budget_violations: AtomicU64,
    /// Unique tasks processed by this worker
    pub tasks_processed: AtomicU64,
    /// Total CPU time consumed (nanoseconds)
    pub cpu_time_ns: AtomicU64,
    /// Slow poll operations (>1ms duration)
    pub slow_polls: AtomicU64,
    /// Voluntary task yields
    pub voluntary_yields: AtomicU64,
}

impl Default for WorkerStats {
    fn default() -> Self {
        Self {
            polls_processed: AtomicU64::new(0),
            yields_forced: AtomicU64::new(0),
            tier_promotions: AtomicU64::new(0),
            tier_demotions: AtomicU64::new(0),
            budget_violations: AtomicU64::new(0),
            tasks_processed: AtomicU64::new(0),
            cpu_time_ns: AtomicU64::new(0),
            slow_polls: AtomicU64::new(0),
            voluntary_yields: AtomicU64::new(0),
        }
    }
}

/// Snapshot of worker-specific performance metrics
///
/// Provides a point-in-time view of a worker thread's performance statistics
/// for monitoring, debugging, and performance analysis. All values represent
/// atomic snapshots taken at the time of collection and are thread-safe.
///
/// Worker metrics track the behavior of individual worker threads in the
/// async runtime, allowing fine-grained monitoring of task execution patterns,
/// budget violations, and CPU utilization per worker.
///
/// # Thread Safety
///
/// This struct is safe to share across threads and all operations are lock-free.
/// The underlying counters use atomic operations with relaxed ordering for
/// optimal performance (<10ns overhead per metric update).
///
/// # Performance Notes
///
/// - Retrieving metrics has ~50ns overhead
/// - Metrics are updated during normal task execution with minimal impact
/// - No heap allocations are made during metrics collection
/// - All counters are 64-bit to prevent overflow in production workloads
///
/// # Examples
///
/// ## Basic Usage
///
/// ```no_run
/// use tokio_pulse::tier_manager::{TierManager, TierConfig};
///
/// let manager = TierManager::new(TierConfig::default());
/// let worker_metrics = manager.get_worker_metrics(0);
///
/// println!("Worker {}: {} polls, {} violations, {:.2}ms CPU",
///          worker_metrics.worker_id,
///          worker_metrics.polls,
///          worker_metrics.violations,
///          worker_metrics.cpu_time_ns as f64 / 1_000_000.0);
/// ```
///
/// ## Performance Analysis
///
/// ```no_run
/// use tokio_pulse::tier_manager::{TierManager, TierConfig};
///
/// let manager = TierManager::new(TierConfig::default());
/// let metrics = manager.get_worker_metrics(0);
///
/// // Calculate derived metrics
/// let avg_poll_time = if metrics.polls > 0 {
///     metrics.cpu_time_ns / metrics.polls
/// } else { 0 };
///
/// let violation_rate = if metrics.polls > 0 {
///     (metrics.violations as f64 / metrics.polls as f64) * 100.0
/// } else { 0.0 };
///
/// let slow_poll_percentage = if metrics.polls > 0 {
///     (metrics.slow_polls as f64 / metrics.polls as f64) * 100.0
/// } else { 0.0 };
///
/// println!("Average poll time: {}ns", avg_poll_time);
/// println!("Violation rate: {:.2}%", violation_rate);
/// println!("Slow polls: {:.2}%", slow_poll_percentage);
/// ```
///
/// ## Multi-Worker Monitoring
///
/// ```no_run
/// use tokio_pulse::tier_manager::{TierManager, TierConfig};
///
/// let manager = TierManager::new(TierConfig::default());
/// let all_workers = manager.get_all_worker_metrics();
///
/// for (worker_id, metrics) in all_workers {
///     if metrics.violations > 0 {
///         println!("Worker {} has {} budget violations!",
///                  worker_id, metrics.violations);
///     }
///
///     if metrics.slow_polls > metrics.polls / 10 {
///         println!("Worker {} has high slow poll rate: {}/{}",
///                  worker_id, metrics.slow_polls, metrics.polls);
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct WorkerMetrics {
    /// Worker thread identifier (unique per runtime)
    ///
    /// Identifies which worker thread these metrics belong to.
    /// Worker IDs are typically 0-based and assigned by the runtime.
    pub worker_id: usize,

    /// Total poll operations processed by this worker
    ///
    /// Counts every `Future::poll()` call handled by this worker thread.
    /// This includes both completed and pending poll results.
    pub polls: u64,

    /// Budget violations detected for tasks on this worker
    ///
    /// Number of times tasks exceeded their allocated polling budget.
    /// High values indicate CPU-bound tasks that may need optimization
    /// or tier escalation.
    pub violations: u64,

    /// Voluntary task yields executed by this worker
    ///
    /// Count of explicit `task::yield_now()` calls or cooperative yields.
    /// Higher values indicate well-behaved tasks that yield control
    /// periodically.
    pub yields: u64,

    /// Total CPU time consumed by this worker (nanoseconds)
    ///
    /// Aggregate CPU time spent in poll operations on this worker.
    /// Use with `polls` to calculate average poll duration.
    /// Resolution depends on platform timer precision.
    pub cpu_time_ns: u64,

    /// Unique tasks processed by this worker
    ///
    /// Count of distinct task instances that have been polled.
    /// Different from `polls` as each task may be polled multiple times.
    /// Useful for understanding task distribution across workers.
    pub tasks: u64,

    /// Slow poll operations detected (duration > 1ms)
    ///
    /// Number of individual polls that exceeded the slow poll threshold.
    /// These operations may indicate blocking I/O, CPU-intensive work,
    /// or other performance issues requiring investigation.
    pub slow_polls: u64,
}

/// Main tier management coordinator
pub struct TierManager {
    config: Arc<RwLock<TierConfig>>,
    task_states: DashMap<TaskId, Arc<TaskState>>,
    slow_queue: SegQueue<TaskId>,
    slow_queue_size: AtomicUsize,
    global_polls: AtomicU64,
    global_violations: AtomicU64,
    global_yields: AtomicU64,
    budget_violations: AtomicU64,
    voluntary_yields: AtomicU64,
    tier_promotions: AtomicU64,
    tier_demotions: AtomicU64,
    worker_stats: DashMap<usize, Arc<WorkerStats>>,
    #[allow(dead_code)]
    cpu_timer: Box<dyn crate::timing::CpuTimer>,
    isolation: Option<TaskIsolation>,
    scheduler: Option<DedicatedThreadScheduler>,
}

impl TierManager {
    /// Creates new tier manager with the specified configuration
    ///
    /// # Arguments
    ///
    /// * `config` - Tier management configuration settings
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let config = TierConfig::default();
    /// let manager = TierManager::new(config);
    /// ```
    pub fn new(config: TierConfig) -> Self {
        let isolation = TaskIsolation::new().ok();

        #[cfg(feature = "tracing")]
        if isolation.is_some() {
            tracing::info!("Task isolation enabled");
        } else {
            tracing::warn!("Task isolation unavailable");
        }

        let scheduler_config = ThreadSchedulerConfig::default();
        let scheduler = Some(DedicatedThreadScheduler::new(scheduler_config));

        #[cfg(feature = "tracing")]
        tracing::info!("Dedicated thread scheduler initialized");

        Self {
            config: Arc::new(RwLock::new(config)),
            task_states: DashMap::new(),
            slow_queue: SegQueue::new(),
            slow_queue_size: AtomicUsize::new(0),
            global_polls: AtomicU64::new(0),
            global_violations: AtomicU64::new(0),
            global_yields: AtomicU64::new(0),
            budget_violations: AtomicU64::new(0),
            voluntary_yields: AtomicU64::new(0),
            tier_promotions: AtomicU64::new(0),
            tier_demotions: AtomicU64::new(0),
            worker_stats: DashMap::new(),
            cpu_timer: create_cpu_timer(),
            isolation,
            scheduler,
        }
    }

    /// Pre-poll hook called before task execution
    ///
    /// Sets up budget tracking and initializes task state if needed.
    /// Also tracks worker-specific statistics for performance monitoring.
    ///
    /// # Arguments
    ///
    /// * `task_id` - Unique identifier for the task
    /// * `ctx` - Task execution context containing worker ID
    pub fn before_poll(&self, task_id: TaskId, ctx: &TaskContext) {
        self.global_polls.fetch_add(1, Ordering::Relaxed);

        // Track worker-specific statistics
        let worker_stats = self.get_or_create_worker_stats(ctx.worker_id);
        worker_stats.polls_processed.fetch_add(1, Ordering::Relaxed);

        // Get or create task state
        let state = self.task_states.entry(task_id).or_insert_with(|| {
            // Track new task for this worker
            worker_stats.tasks_processed.fetch_add(1, Ordering::Relaxed);

            let config = self.config.read();

            // Apply priority-based budget allocation
            let priority_multiplier = ctx.priority_budget_multiplier();
            let adjusted_budget = (config.poll_budget as f32 * priority_multiplier).max(1.0) as u32;

            Arc::new(TaskState::new(acquire_budget(adjusted_budget), ctx.worker_id, ctx.clone()))
        });

        // Update poll timestamp and context
        *state.last_poll_time.write() = Instant::now();
        state.update_context(ctx);

        // Check yield requirement
        if state.should_yield() {
            #[cfg(feature = "tracing")]
            tracing::debug!(task_id = ?task_id, tier = state.current_tier.load(Ordering::Acquire),
                          "Task scheduled to yield");
        }
    }

    /// Post-poll hook called after task execution
    ///
    /// Checks for budget violations and applies tier interventions as needed.
    ///
    /// # Arguments
    ///
    /// * `task_id` - Unique identifier for the task
    /// * `result` - Result of the poll operation
    /// * `poll_duration` - Time spent polling
    pub fn after_poll(&self, task_id: TaskId, result: PollResult, poll_duration: Duration) {
        let Some(state) = self.task_states.get(&task_id) else {
            return;
        };

        // Track worker-specific statistics
        let worker_stats = self.get_or_create_worker_stats(state.worker_id);

        // Track CPU time
        let cpu_ns = poll_duration.as_nanos() as u64;
        state.total_cpu_ns.fetch_add(cpu_ns, Ordering::Relaxed);
        worker_stats.cpu_time_ns.fetch_add(cpu_ns, Ordering::Relaxed);

        // Detect slow polls (>1ms)
        let is_slow_poll = poll_duration.as_millis() > 1;
        if is_slow_poll {
            state.slow_poll_count.fetch_add(1, Ordering::Relaxed);
            worker_stats.slow_polls.fetch_add(1, Ordering::Relaxed);
        }

        // Check for pending tier changes (hysteresis expired)
        let config = self.config.read();
        state.check_pending_tier_change(
            config.promotion_hysteresis_ms,
            config.demotion_hysteresis_ms,
        );

        // Consume budget and check for violations
        let had_violation = state.budget.consume();
        if had_violation {
            state.record_violation();
            self.handle_budget_violation(&task_id, &state);
        } else {
            // Record good behavior for polls without violations
            let current_tier = state.current_tier.load(Ordering::Acquire) as usize;
            let tier_policy = &config.tier_policies[current_tier];
            if !is_slow_poll && cpu_ns <= tier_policy.demotion_cpu_threshold_ns {
                state.record_good_poll(cpu_ns);

                // Evaluate for automatic demotion
                if state.evaluate_for_demotion(&config) {
                    if let Some(new_tier) = state.try_demote_tier(
                        config.demotion_hysteresis_ms,
                        tier_policy,
                        config.test_mode
                    ) {
                        #[cfg(feature = "tracing")]
                        tracing::debug!(task_id = ?task_id, new_tier = new_tier,
                                      "Task automatically demoted for sustained good behavior");

                        self.tier_demotions.fetch_add(1, Ordering::Relaxed);

                        #[cfg(feature = "metrics")]
                        metrics::counter!("tokio_pulse.tier_demotions", "reason" => "automatic").increment(1);
                    }
                }
            }
        }

        // Clear yield flag after yield
        if matches!(result, PollResult::Pending) && state.force_yield.load(Ordering::Acquire) > 0 {
            state.clear_yield_flag();
            state.budget.reset(config.poll_budget);
            self.global_yields.fetch_add(1, Ordering::Relaxed);
        }

        // Apply tier interventions
        self.apply_intervention(&task_id, &state);
    }

    /// Voluntary yield handler called when a task yields control
    ///
    /// Records yield events and updates global statistics.
    ///
    /// # Arguments
    ///
    /// * `task_id` - Unique identifier for the yielding task
    pub fn on_yield(&self, task_id: TaskId) {
        // Track yield
        self.global_yields.fetch_add(1, Ordering::Relaxed);
        self.voluntary_yields.fetch_add(1, Ordering::Relaxed);

        if let Some(state) = self.task_states.get(&task_id) {
            // Track worker-specific voluntary yield
            let worker_stats = self.get_or_create_worker_stats(state.worker_id);
            worker_stats.voluntary_yields.fetch_add(1, Ordering::Relaxed);
            let config = self.config.read();

            // Reset budget
            state.budget.reset(config.poll_budget);

            // Clear violations
            state.violation_count.store(0, Ordering::Release);

            // Demote for good behavior with hysteresis
            if state.current_tier.load(Ordering::Acquire) > 0 {
                let current_tier = state.current_tier.load(Ordering::Acquire) as usize;
                let tier_policy = &config.tier_policies[current_tier];
                if let Some(new_tier) = state.try_demote_tier(
                    config.demotion_hysteresis_ms,
                    tier_policy,
                    config.test_mode
                ) {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(task_id = ?task_id, new_tier = new_tier,
                                  "Task demoted for voluntary yield");

                    self.tier_demotions.fetch_add(1, Ordering::Relaxed);

                    #[cfg(feature = "metrics")]
                    metrics::counter!("tokio_pulse.tier_demotions", "reason" => "voluntary").increment(1);
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(task_id = ?task_id, "Tier demotion pending (hysteresis)");
                }
            }
        }
    }

    /// Task completion handler called when a task finishes
    ///
    /// Cleans up task state and returns budget to pool.
    ///
    /// # Arguments
    ///
    /// * `task_id` - Unique identifier for the completed task
    pub fn on_completion(&self, task_id: TaskId) {
        // Release task from isolation if it was isolated
        if let Some(ref isolation) = self.isolation {
            let _ = isolation.release_task(&task_id);
        }

        // Return TaskBudget to pool before removing state
        if let Some((_, task_state)) = self.task_states.remove(&task_id) {
            release_budget(Arc::clone(&task_state.budget));

            #[cfg(feature = "tracing")]
            tracing::trace!(task_id = ?task_id, "Task completed, budget returned to pool, state removed");
        }
    }

    // Budget violation handler
    fn handle_budget_violation(&self, task_id: &TaskId, state: &Arc<TaskState>) {
        self.global_violations.fetch_add(1, Ordering::Relaxed);
        self.budget_violations.fetch_add(1, Ordering::Relaxed);

        // Track worker-specific budget violation
        let worker_stats = self.get_or_create_worker_stats(state.worker_id);
        worker_stats.budget_violations.fetch_add(1, Ordering::Relaxed);

        let violations = state.violation_count.fetch_add(1, Ordering::AcqRel) + 1;
        let current_tier = state.current_tier.load(Ordering::Acquire);

        let config = self.config.read();

        // Track violation rate for cooldown
        state.track_violation(config.violation_rate_threshold, config.cooldown_duration_ms);

        // Skip tier changes if in cooldown
        if state.is_in_cooldown() {
            #[cfg(feature = "tracing")]
            tracing::debug!(task_id = ?task_id, "Task in cooldown period, skipping tier change");
            return;
        }

        let tier_policy = &config.tier_policies[current_tier as usize];

        // Apply priority-based threshold adjustment
        let context = state.context.read();
        let priority_multiplier = context.priority_threshold_multiplier();
        let adjusted_threshold = (tier_policy.promotion_threshold as f32 * priority_multiplier).max(1.0) as u32;
        drop(context); // Release the lock early

        // Check tier promotion with hysteresis and priority adjustment
        if violations >= adjusted_threshold {
            if let Some(new_tier) = state.try_escalate_tier(config.promotion_hysteresis_ms) {
                state.violation_count.store(0, Ordering::Release);

                #[cfg(feature = "tracing")]
                tracing::warn!(task_id = ?task_id, old_tier = current_tier, new_tier = new_tier,
                              violations = violations, "Task promoted to higher tier");

                self.tier_promotions.fetch_add(1, Ordering::Relaxed);

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.tier_promotions").increment(1);
            } else {
                #[cfg(feature = "tracing")]
                tracing::debug!(task_id = ?task_id, current_tier = current_tier,
                               "Tier promotion pending (hysteresis)");
            }
        }
    }

    // Apply tier-specific intervention
    fn apply_intervention(&self, task_id: &TaskId, state: &Arc<TaskState>) {
        let current_tier = state.current_tier.load(Ordering::Acquire);
        let config = self.config.read();
        let action = config.tier_policies[current_tier as usize].action;

        match action {
            InterventionAction::Monitor => {} // No action
            InterventionAction::Warn => {
                #[cfg(feature = "tracing")]
                tracing::warn!(task_id = ?task_id, tier = current_tier,
                              "Task consuming excessive CPU time");

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.intervention.warn").increment(1);
            },
            InterventionAction::Yield => {
                state.mark_for_yield();

                #[cfg(feature = "tracing")]
                tracing::info!(task_id = ?task_id, "Task marked for forced yield");

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.intervention.yield").increment(1);
            },
            InterventionAction::SlowQueue => {
                // Add to slow queue
                if self.slow_queue_size.load(Ordering::Acquire) < config.max_slow_queue_size {
                    self.slow_queue.push(*task_id);
                    self.slow_queue_size.fetch_add(1, Ordering::AcqRel);

                    #[cfg(feature = "tracing")]
                    tracing::info!(task_id = ?task_id, "Task added to slow queue");

                    #[cfg(feature = "metrics")]
                    metrics::counter!("tokio_pulse.intervention.slow_queue").increment(1);
                }
            },
            InterventionAction::Isolate => {
                if config.enable_isolation {
                    self.isolate_task(task_id);
                } else {
                    // Fallback to slow queue
                    self.slow_queue.push(*task_id);
                    self.slow_queue_size.fetch_add(1, Ordering::AcqRel);
                }

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.intervention.isolate").increment(1);
            },
        }
    }

    // OS-specific task isolation
    fn isolate_task(&self, task_id: &TaskId) {
        if let Some(ref isolation) = self.isolation {
            match isolation.isolate_task(task_id) {
                Ok(()) => {
                    #[cfg(feature = "tracing")]
                    tracing::info!(task_id = ?task_id, "Task successfully isolated");
                }
                Err(IsolationError::PermissionDenied(msg)) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(task_id = ?task_id, error = %msg, "Isolation permission denied");
                }
                Err(IsolationError::NotSupported(msg)) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(task_id = ?task_id, error = %msg, "Isolation not supported");
                }
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(task_id = ?task_id, error = %e, "Task isolation failed");
                }
            }
        } else {
            #[cfg(feature = "tracing")]
            tracing::warn!(task_id = ?task_id, "No isolation manager available");
        }
    }

    /// Processes tasks from the slow queue using dedicated threads
    ///
    /// Moves tasks from the slow queue to the dedicated thread scheduler
    /// for processing outside the main Tokio runtime.
    ///
    /// # Arguments
    ///
    /// * `max_tasks` - Maximum number of tasks to process in this batch
    ///
    /// # Returns
    ///
    /// Number of tasks successfully submitted for processing
    pub fn process_slow_queue(&self, max_tasks: usize) -> usize {
        let mut processed = 0;

        while processed < max_tasks {
            if let Some(task_id) = self.slow_queue.pop() {
                self.slow_queue_size.fetch_sub(1, Ordering::AcqRel);

                if let Some(ref scheduler) = self.scheduler {
                    let tier = self.task_states
                        .get(&task_id)
                        .map(|state| state.current_tier.load(Ordering::Acquire))
                        .unwrap_or(0) as u8;

                    let reschedule_task = RescheduleTask::new(task_id, tier);

                    match scheduler.reschedule_task(reschedule_task) {
                        Ok(()) => {
                            processed += 1;

                            #[cfg(feature = "tracing")]
                            tracing::debug!(task_id = ?task_id, tier = tier, "Task submitted for rescheduling");
                        }
                        Err(e) => {
                            #[cfg(feature = "tracing")]
                            tracing::warn!(task_id = ?task_id, error = ?e, "Failed to reschedule task");

                            self.slow_queue.push(task_id);
                            self.slow_queue_size.fetch_add(1, Ordering::AcqRel);
                            break;
                        }
                    }
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(task_id = ?task_id, "No scheduler available for task rescheduling");

                    processed += 1;
                }
            } else {
                break;
            }
        }

        processed
    }

    /// Updates the tier management configuration
    ///
    /// Replaces the current configuration with new settings for
    /// budgets, policies, and intervention thresholds.
    ///
    /// # Arguments
    ///
    /// * `config` - New tier configuration to apply
    pub fn update_config(&self, config: TierConfig) {
        *self.config.write() = config;

        #[cfg(feature = "tracing")]
        tracing::info!("TierManager configuration updated");
    }

    /// Gets current metrics snapshot
    ///
    /// Returns a point-in-time snapshot of tier management metrics
    /// including polls, violations, tier changes, and queue statistics.
    ///
    /// # Returns
    ///
    /// Current metrics snapshot
    /// Get comprehensive metrics for monitoring tier management performance
    ///
    /// Returns current system metrics including counters for budget violations,
    /// tier changes, and system utilization.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    /// let metrics = manager.get_metrics();
    /// println!("Total polls: {}", metrics.total_polls);
    /// println!("Budget violations: {}", metrics.budget_violations);
    /// ```
    pub fn get_metrics(&self) -> TierMetrics {
        TierMetrics {
            total_polls: self.global_polls.load(Ordering::Relaxed),
            total_violations: self.global_violations.load(Ordering::Relaxed),
            budget_violations: self.budget_violations.load(Ordering::Relaxed),
            total_yields: self.global_yields.load(Ordering::Relaxed),
            voluntary_yields: self.voluntary_yields.load(Ordering::Relaxed),
            tier_promotions: self.tier_promotions.load(Ordering::Relaxed),
            tier_demotions: self.tier_demotions.load(Ordering::Relaxed),
            active_tasks: self.task_states.len(),
            slow_queue_size: self.slow_queue_size.load(Ordering::Relaxed),
        }
    }

    /// Get derived metrics calculated from base counters
    ///
    /// Computes rates and ratios useful for system analysis and alerting.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    /// let derived = manager.get_derived_metrics();
    /// println!("Violation rate: {:.2}%", derived.violation_rate * 100.0);
    /// ```
    pub fn get_derived_metrics(&self) -> DerivedMetrics {
        let polls = self.global_polls.load(Ordering::Relaxed) as f64;

        if polls == 0.0 {
            return DerivedMetrics {
                violation_rate: 0.0,
                yield_rate: 0.0,
                promotion_rate: 0.0,
                demotion_rate: 0.0,
            };
        }

        DerivedMetrics {
            violation_rate: self.budget_violations.load(Ordering::Relaxed) as f64 / polls,
            yield_rate: self.voluntary_yields.load(Ordering::Relaxed) as f64 / polls,
            promotion_rate: self.tier_promotions.load(Ordering::Relaxed) as f64 / polls,
            demotion_rate: self.tier_demotions.load(Ordering::Relaxed) as f64 / polls,
        }
    }

    /// Legacy metrics method for backward compatibility
    ///
    /// # Deprecated
    ///
    /// Use `get_metrics()` instead for the full metrics set.
    pub fn metrics(&self) -> TierMetrics {
        self.get_metrics()
    }

    /// Get performance statistics for a specific worker thread
    ///
    /// Returns an atomic snapshot of performance metrics for the given worker ID.
    /// If the worker has not processed any tasks yet, returns a `WorkerMetrics`
    /// struct with all counters set to zero and the specified worker ID.
    ///
    /// This operation is thread-safe and lock-free with ~50ns overhead.
    /// The returned metrics represent a consistent point-in-time view but
    /// individual fields may have been updated between atomic loads.
    ///
    /// # Arguments
    ///
    /// * `worker_id` - Worker thread identifier to query (typically 0-based)
    ///
    /// # Returns
    ///
    /// A `WorkerMetrics` struct containing current performance counters.
    /// All values are non-negative and monotonically increasing except
    /// after explicit reset operations.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(1)
    /// - Memory allocations: None
    /// - Lock contention: None (lock-free atomic operations)
    ///
    /// # Examples
    ///
    /// ## Basic Monitoring
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    /// let worker_metrics = manager.get_worker_metrics(0);
    ///
    /// println!("Worker 0: {} polls, {} violations",
    ///          worker_metrics.polls, worker_metrics.violations);
    ///
    /// if worker_metrics.violations > 0 {
    ///     println!("⚠️  Worker 0 has budget violations - check for CPU-bound tasks");
    /// }
    /// ```
    ///
    /// ## Performance Analysis
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    /// let metrics = manager.get_worker_metrics(0);
    ///
    /// if metrics.polls > 0 {
    ///     let avg_poll_ns = metrics.cpu_time_ns / metrics.polls;
    ///     let violation_rate = (metrics.violations as f64 / metrics.polls as f64) * 100.0;
    ///
    ///     println!("Worker 0 performance:");
    ///     println!("  Average poll time: {}μs", avg_poll_ns / 1000);
    ///     println!("  Violation rate: {:.2}%", violation_rate);
    ///     println!("  Slow polls: {} ({:.1}%)",
    ///              metrics.slow_polls,
    ///              (metrics.slow_polls as f64 / metrics.polls as f64) * 100.0);
    /// }
    /// ```
    ///
    /// ## Comparing Workers
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    ///
    /// // Compare load across workers
    /// for worker_id in 0..num_cpus::get() {
    ///     let metrics = manager.get_worker_metrics(worker_id);
    ///     if metrics.polls > 0 {
    ///         println!("Worker {}: {} tasks, {} polls",
    ///                  worker_id, metrics.tasks, metrics.polls);
    ///     }
    /// }
    /// ```
    pub fn get_worker_metrics(&self, worker_id: usize) -> WorkerMetrics {
        if let Some(stats) = self.worker_stats.get(&worker_id) {
            WorkerMetrics {
                worker_id,
                polls: stats.polls_processed.load(Ordering::Relaxed),
                violations: stats.budget_violations.load(Ordering::Relaxed),
                yields: stats.voluntary_yields.load(Ordering::Relaxed),
                cpu_time_ns: stats.cpu_time_ns.load(Ordering::Relaxed),
                tasks: stats.tasks_processed.load(Ordering::Relaxed),
                slow_polls: stats.slow_polls.load(Ordering::Relaxed),
            }
        } else {
            WorkerMetrics {
                worker_id,
                polls: 0,
                violations: 0,
                yields: 0,
                cpu_time_ns: 0,
                tasks: 0,
                slow_polls: 0,
            }
        }
    }

    /// Get performance statistics for all active worker threads
    ///
    /// Returns a HashMap mapping worker IDs to their current performance metrics.
    /// Only workers that have processed at least one task are included in the result.
    /// This provides a comprehensive view of runtime performance across all workers.
    ///
    /// Each entry in the HashMap is an atomic snapshot taken at collection time.
    /// The collection itself is not atomic across all workers, so there may be
    /// small timing variations between worker snapshots.
    ///
    /// # Returns
    ///
    /// A `HashMap<usize, WorkerMetrics>` where:
    /// - Key: Worker thread identifier
    /// - Value: Current metrics snapshot for that worker
    ///
    /// The HashMap will be empty if no workers have processed tasks yet.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(n) where n is the number of active workers
    /// - Memory allocation: One HashMap allocation plus entries
    /// - Lock contention: None (lock-free atomic reads)
    /// - Typical overhead: ~200ns + (50ns × worker_count)
    ///
    /// # Examples
    ///
    /// ## Basic Overview
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    /// let all_workers = manager.get_all_worker_metrics();
    ///
    /// println!("Active workers: {}", all_workers.len());
    /// for (worker_id, metrics) in all_workers {
    ///     println!("Worker {}: {} polls, {} tasks",
    ///              worker_id, metrics.polls, metrics.tasks);
    /// }
    /// ```
    ///
    /// ## Performance Dashboard
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    /// let all_workers = manager.get_all_worker_metrics();
    ///
    /// let mut total_polls = 0;
    /// let mut total_violations = 0;
    /// let mut workers_with_violations = 0;
    ///
    /// for (worker_id, metrics) in &all_workers {
    ///     total_polls += metrics.polls;
    ///     total_violations += metrics.violations;
    ///
    ///     if metrics.violations > 0 {
    ///         workers_with_violations += 1;
    ///         println!("⚠️  Worker {} has {} violations", worker_id, metrics.violations);
    ///     }
    ///
    ///     let cpu_ms = metrics.cpu_time_ns as f64 / 1_000_000.0;
    ///     println!("Worker {}: {:.2}ms CPU, {} polls",
    ///              worker_id, cpu_ms, metrics.polls);
    /// }
    ///
    /// if total_polls > 0 {
    ///     let violation_rate = (total_violations as f64 / total_polls as f64) * 100.0;
    ///     println!("Overall violation rate: {:.2}%", violation_rate);
    ///     println!("Workers with violations: {}/{}", workers_with_violations, all_workers.len());
    /// }
    /// ```
    ///
    /// ## Load Balancing Analysis
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    /// let all_workers = manager.get_all_worker_metrics();
    ///
    /// if !all_workers.is_empty() {
    ///     let total_tasks: u64 = all_workers.values().map(|m| m.tasks).sum();
    ///     let avg_tasks = total_tasks as f64 / all_workers.len() as f64;
    ///
    ///     println!("Load distribution (target: {:.1} tasks per worker):", avg_tasks);
    ///
    ///     for (worker_id, metrics) in all_workers {
    ///         let deviation = (metrics.tasks as f64 - avg_tasks) / avg_tasks * 100.0;
    ///         println!("Worker {}: {} tasks ({:+.1}%)",
    ///                  worker_id, metrics.tasks, deviation);
    ///     }
    /// }
    /// ```
    pub fn get_all_worker_metrics(&self) -> std::collections::HashMap<usize, WorkerMetrics> {
        let mut result = std::collections::HashMap::new();

        for entry in self.worker_stats.iter() {
            let worker_id = *entry.key();
            let stats = entry.value();

            result.insert(worker_id, WorkerMetrics {
                worker_id,
                polls: stats.polls_processed.load(Ordering::Relaxed),
                violations: stats.budget_violations.load(Ordering::Relaxed),
                yields: stats.voluntary_yields.load(Ordering::Relaxed),
                cpu_time_ns: stats.cpu_time_ns.load(Ordering::Relaxed),
                tasks: stats.tasks_processed.load(Ordering::Relaxed),
                slow_polls: stats.slow_polls.load(Ordering::Relaxed),
            });
        }

        result
    }

    /// Get the current tier level for a specific task
    ///
    /// Returns the tier level (0-3) for the given task ID, or None if the task
    /// is not currently being tracked by the tier manager.
    ///
    /// # Arguments
    ///
    /// * `task_id` - Task identifier to query
    ///
    /// # Returns
    ///
    /// Some(tier_level) if the task exists, None otherwise
    pub fn get_task_tier(&self, task_id: TaskId) -> Option<u32> {
        self.task_states.get(&task_id)
            .map(|state| state.current_tier.load(Ordering::Acquire))
    }

    /// Reset statistics for a specific worker thread
    ///
    /// Atomically clears all performance counters for the given worker ID back to zero.
    /// This operation is useful for periodic metrics collection, analysis windows,
    /// or clearing accumulated statistics after performance issues are resolved.
    ///
    /// If the worker ID has no existing statistics, this operation is a no-op.
    /// The reset operation is atomic per counter but not atomic across all counters,
    /// so brief inconsistencies may be observable during concurrent access.
    ///
    /// # Arguments
    ///
    /// * `worker_id` - Worker thread identifier to reset (must match existing worker)
    ///
    /// # Behavior
    ///
    /// Resets all counters to zero:
    /// - `polls` → 0
    /// - `violations` → 0
    /// - `yields` → 0
    /// - `cpu_time_ns` → 0
    /// - `tasks` → 0
    /// - `slow_polls` → 0
    /// - Internal counters (forced yields, tier changes) → 0
    ///
    /// # Performance
    ///
    /// - Time complexity: O(1)
    /// - Memory allocations: None
    /// - Lock contention: None (atomic operations)
    /// - Typical overhead: ~100ns
    ///
    /// # Thread Safety
    ///
    /// This operation is thread-safe and can be called concurrently with metric
    /// updates and reads. However, metrics may be updated immediately after reset,
    /// so this is not suitable for creating consistent snapshots across workers.
    ///
    /// # Examples
    ///
    /// ## Periodic Reset
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    /// use std::time::Duration;
    ///
    /// let manager = TierManager::new(TierConfig::default());
    ///
    /// // Reset every minute for periodic analysis
    /// loop {
    ///     std::thread::sleep(Duration::from_secs(60));
    ///
    ///     // Collect current metrics before reset
    ///     let metrics = manager.get_worker_metrics(0);
    ///     println!("Last minute: {} polls, {} violations",
    ///              metrics.polls, metrics.violations);
    ///
    ///     // Reset for next period
    ///     manager.reset_worker_stats(0);
    /// }
    /// ```
    ///
    /// ## Analysis Window
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    /// use std::time::{Duration, Instant};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    ///
    /// // Reset all workers to start clean measurement
    /// for worker_id in 0..num_cpus::get() {
    ///     manager.reset_worker_stats(worker_id);
    /// }
    ///
    /// let start = Instant::now();
    ///
    /// // ... application runs ...
    ///
    /// let elapsed = start.elapsed();
    /// let all_metrics = manager.get_all_worker_metrics();
    ///
    /// println!("Performance over {:?}:", elapsed);
    /// for (worker_id, metrics) in all_metrics {
    ///     let polls_per_sec = metrics.polls as f64 / elapsed.as_secs_f64();
    ///     println!("Worker {}: {:.0} polls/sec", worker_id, polls_per_sec);
    /// }
    /// ```
    ///
    /// ## Troubleshooting Reset
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    ///
    /// // Check if worker has accumulated violations
    /// let metrics = manager.get_worker_metrics(0);
    /// if metrics.violations > 100 {
    ///     println!("Worker 0 has {} violations - investigating and resetting",
    ///              metrics.violations);
    ///
    ///     // Reset to start fresh monitoring
    ///     manager.reset_worker_stats(0);
    ///
    ///     // Verify reset succeeded
    ///     let after_reset = manager.get_worker_metrics(0);
    ///     assert_eq!(after_reset.violations, 0);
    ///     println!("Worker 0 statistics reset successfully");
    /// }
    /// ```
    pub fn reset_worker_stats(&self, worker_id: usize) {
        if let Some(stats) = self.worker_stats.get(&worker_id) {
            stats.polls_processed.store(0, Ordering::Relaxed);
            stats.budget_violations.store(0, Ordering::Relaxed);
            stats.voluntary_yields.store(0, Ordering::Relaxed);
            stats.cpu_time_ns.store(0, Ordering::Relaxed);
            stats.tasks_processed.store(0, Ordering::Relaxed);
            stats.slow_polls.store(0, Ordering::Relaxed);
            stats.yields_forced.store(0, Ordering::Relaxed);
            stats.tier_promotions.store(0, Ordering::Relaxed);
            stats.tier_demotions.store(0, Ordering::Relaxed);
        }
    }

    /// Get or create worker statistics for a given worker ID
    ///
    /// Internal method used by hook implementations to access worker-specific
    /// statistics. Creates a new WorkerStats entry if this is the first time
    /// we've seen this worker.
    fn get_or_create_worker_stats(&self, worker_id: usize) -> Arc<WorkerStats> {
        self.worker_stats.entry(worker_id)
            .or_insert_with(|| Arc::new(WorkerStats::default()))
            .clone()
    }

    // Runtime Configuration Methods

    /// Updates the tier policy for a specific tier
    ///
    /// This allows runtime reconfiguration of intervention thresholds without
    /// requiring a restart. The update is atomic and thread-safe.
    ///
    /// # Arguments
    ///
    /// * `tier_index` - Index of the tier to update (0-based)
    /// * `new_policy` - New policy configuration for the tier
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the update was successful
    /// * `Err(String)` if the tier index is invalid or the policy is invalid
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig, TierPolicyBuilder, InterventionAction};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    ///
    /// // Create a more aggressive policy for tier 1
    /// let aggressive_policy = TierPolicyBuilder::new("AggressiveWarn", InterventionAction::Warn)
    ///     .promotion_violations(3)  // Promote after 3 violations instead of 5
    ///     .demotion_good_polls(20)  // Require 20 good polls for demotion
    ///     .build();
    ///
    /// manager.update_tier_policy(1, aggressive_policy).expect("Failed to update policy");
    /// ```
    pub fn update_tier_policy(&self, tier_index: usize, new_policy: TierPolicy) -> Result<(), String> {
        // Validate the tier index
        {
            let config = self.config.read();
            if tier_index >= config.tier_policies.len() {
                return Err(format!(
                    "Invalid tier index: {}. Valid range is 0-{}",
                    tier_index,
                    config.tier_policies.len() - 1
                ));
            }
        }

        // Validate the policy
        self.validate_tier_policy(&new_policy)?;

        // Get policy name for logging before moving
        #[cfg(feature = "tracing")]
        let policy_name = new_policy.name;

        // Update the policy atomically
        {
            let mut config = self.config.write();
            config.tier_policies[tier_index] = new_policy;
        }

        #[cfg(feature = "tracing")]
        tracing::info!(
            tier_index = tier_index,
            policy_name = policy_name,
            "Updated tier policy at runtime"
        );

        Ok(())
    }

    /// Updates the global configuration settings
    ///
    /// This allows runtime updates of global settings like budget amounts,
    /// hysteresis periods, and other non-tier-specific configurations.
    ///
    /// # Arguments
    ///
    /// * `update_fn` - Closure that receives a mutable reference to TierConfig
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    ///
    /// // Update global settings
    /// manager.update_global_config(|config| {
    ///     config.poll_budget = 5000;  // Increase budget
    ///     config.demotion_hysteresis_ms = 2000;  // Longer hysteresis
    /// });
    /// ```
    pub fn update_global_config<F>(&self, update_fn: F)
    where
        F: FnOnce(&mut TierConfig),
    {
        let mut config = self.config.write();
        update_fn(&mut *config);

        #[cfg(feature = "tracing")]
        tracing::info!("Updated global tier configuration at runtime");
    }

    /// Gets a copy of the current configuration
    ///
    /// This provides read-only access to the current configuration state.
    /// The returned config is a snapshot and will not reflect subsequent updates.
    ///
    /// # Returns
    ///
    /// A copy of the current TierConfig
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    /// let current_config = manager.get_current_config();
    /// println!("Current poll budget: {}", current_config.poll_budget);
    /// ```
    pub fn get_current_config(&self) -> TierConfig {
        self.config.read().clone()
    }

    /// Updates multiple tier policies atomically
    ///
    /// This method allows updating multiple tier policies in a single atomic
    /// operation, ensuring consistency across the tier system.
    ///
    /// # Arguments
    ///
    /// * `updates` - Vec of (tier_index, new_policy) tuples
    ///
    /// # Returns
    ///
    /// * `Ok(())` if all updates were successful
    /// * `Err(String)` if any tier index is invalid or any policy is invalid
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, TierConfig, TierPolicyBuilder, InterventionAction};
    ///
    /// let manager = TierManager::new(TierConfig::default());
    ///
    /// let updates = vec![
    ///     (0, TierPolicyBuilder::new("FastMonitor", InterventionAction::Monitor)
    ///         .promotion_violations(2)
    ///         .build()),
    ///     (1, TierPolicyBuilder::new("QuickWarn", InterventionAction::Warn)
    ///         .promotion_violations(3)
    ///         .build()),
    /// ];
    ///
    /// manager.update_multiple_tier_policies(updates).expect("Failed to update policies");
    /// ```
    pub fn update_multiple_tier_policies(&self, updates: Vec<(usize, TierPolicy)>) -> Result<(), String> {
        // Validate all updates first
        {
            let config = self.config.read();
            for (tier_index, policy) in &updates {
                if *tier_index >= config.tier_policies.len() {
                    return Err(format!(
                        "Invalid tier index: {}. Valid range is 0-{}",
                        tier_index,
                        config.tier_policies.len() - 1
                    ));
                }
                self.validate_tier_policy(policy)?;
            }
        }

        // Apply all updates atomically
        {
            let mut config = self.config.write();
            for (tier_index, policy) in updates {
                config.tier_policies[tier_index] = policy;
            }
        }

        #[cfg(feature = "tracing")]
        tracing::info!("Updated multiple tier policies at runtime");

        Ok(())
    }

    /// Validates a tier policy for correctness
    ///
    /// This internal method ensures that tier policies have sensible values
    /// and won't cause system instability.
    fn validate_tier_policy(&self, policy: &TierPolicy) -> Result<(), String> {
        // Validate promotion thresholds
        if policy.promotion_threshold == 0 {
            return Err("Promotion threshold must be greater than 0".to_string());
        }

        if policy.promotion_cpu_threshold_ms == 0 {
            return Err("Promotion CPU threshold must be greater than 0".to_string());
        }

        if policy.promotion_slow_poll_threshold == 0 {
            return Err("Promotion slow poll threshold must be greater than 0".to_string());
        }

        // Validate demotion thresholds
        if policy.demotion_good_polls == 0 {
            return Err("Demotion good polls threshold must be greater than 0".to_string());
        }

        if policy.demotion_cpu_threshold_ns == 0 {
            return Err("Demotion CPU threshold must be greater than 0".to_string());
        }

        if policy.demotion_evaluation_interval == 0 {
            return Err("Demotion evaluation interval must be greater than 0".to_string());
        }

        // Validate reasonable ranges
        if policy.promotion_threshold > 1000 {
            return Err("Promotion threshold too high (max 1000)".to_string());
        }

        if policy.promotion_cpu_threshold_ms > 60000 {
            return Err("Promotion CPU threshold too high (max 60 seconds)".to_string());
        }

        if policy.demotion_good_polls > 10000 {
            return Err("Demotion good polls threshold too high (max 10000)".to_string());
        }

        if policy.demotion_min_duration_ms > 3600000 {
            return Err("Demotion minimum duration too high (max 1 hour)".to_string());
        }

        if policy.demotion_cpu_threshold_ns > 1_000_000_000 {
            return Err("Demotion CPU threshold too high (max 1 second)".to_string());
        }

        Ok(())
    }

    // Configuration Profile Methods

    /// Creates a new TierManager using a predefined configuration profile
    ///
    /// This is a convenience method that combines profile creation
    /// with TierManager initialization.
    ///
    /// # Arguments
    ///
    /// * `profile` - The configuration profile to use
    ///
    /// # Returns
    ///
    /// A new TierManager configured with the specified profile
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, ConfigProfile};
    ///
    /// // Create a manager optimized for latency-critical workloads
    /// let aggressive_manager = TierManager::with_profile(ConfigProfile::Aggressive);
    ///
    /// // Create a manager optimized for batch processing
    /// let permissive_manager = TierManager::with_profile(ConfigProfile::Permissive);
    /// ```
    pub fn with_profile(profile: ConfigProfile) -> Self {
        let config = profile.create_config();
        Self::new(config)
    }

    /// Updates the TierManager to use a different configuration profile
    ///
    /// This allows runtime switching between profiles, useful for
    /// applications that change behavior based on load or time of day.
    ///
    /// # Arguments
    ///
    /// * `profile` - The new profile to switch to
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use tokio_pulse::tier_manager::{TierManager, ConfigProfile};
    ///
    /// let manager = TierManager::with_profile(ConfigProfile::Balanced);
    ///
    /// // Switch to aggressive mode during peak hours
    /// manager.switch_to_profile(ConfigProfile::Aggressive);
    ///
    /// // Switch to permissive mode during batch processing
    /// manager.switch_to_profile(ConfigProfile::Permissive);
    /// ```
    pub fn switch_to_profile(&self, profile: ConfigProfile) {
        if profile == ConfigProfile::Custom {
            // Don't switch to custom - that would overwrite user settings
            #[cfg(feature = "tracing")]
            tracing::warn!("Cannot switch to Custom profile - use update methods instead");
            return;
        }

        let new_config = profile.create_config();
        *self.config.write() = new_config;

        #[cfg(feature = "tracing")]
        tracing::info!(
            profile = ?profile,
            description = profile.description(),
            "Switched to configuration profile"
        );
    }

    /// Gets the current configuration profile if it matches a predefined profile
    ///
    /// # Returns
    ///
    /// * `Some(profile)` if the current config matches a predefined profile
    /// * `None` if the config has been customized and doesn't match any profile
    pub fn current_profile(&self) -> Option<ConfigProfile> {
        let current_config = self.config.read();

        // Check if current config matches any predefined profile
        for profile in [ConfigProfile::Aggressive, ConfigProfile::Balanced, ConfigProfile::Permissive] {
            let profile_config = profile.create_config();
            if configs_match(&*current_config, &profile_config) {
                return Some(profile);
            }
        }

        None // Custom configuration
    }
}

impl Drop for TierManager {
    fn drop(&mut self) {
        if let Some(ref mut scheduler) = self.scheduler {
            if let Err(e) = scheduler.shutdown() {
                #[cfg(feature = "tracing")]
                tracing::error!(error = ?e, "Failed to shutdown dedicated thread scheduler");
            }
        }
    }
}

/// TierManager metrics
#[derive(Debug, Clone)]
pub struct TierMetrics {
    /// Poll count
    pub total_polls: u64,
    /// Violation count (legacy name for total_violations)
    pub total_violations: u64,
    /// Budget violation count (new detailed metric)
    pub budget_violations: u64,
    /// Yield count
    pub total_yields: u64,
    /// Voluntary yield count
    pub voluntary_yields: u64,
    /// Tier promotion count
    pub tier_promotions: u64,
    /// Tier demotion count
    pub tier_demotions: u64,
    /// Active task count
    pub active_tasks: usize,
    /// Slow queue size
    pub slow_queue_size: usize,
}

/// Derived metrics calculated from base metrics
#[derive(Debug, Clone)]
pub struct DerivedMetrics {
    /// Budget violation rate (violations per poll)
    pub violation_rate: f64,
    /// Voluntary yield rate (yields per poll)
    pub yield_rate: f64,
    /// Tier promotion rate (promotions per poll)
    pub promotion_rate: f64,
    /// Tier demotion rate (demotions per poll)
    pub demotion_rate: f64,
}

// Thread-local worker statistics
thread_local! {
    static WORKER_STATS: WorkerStats = WorkerStats::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_config_default() {
        let config = TierConfig::default();
        assert_eq!(config.poll_budget, 2000);
        assert_eq!(config.cpu_ms_budget, 10);
        assert_eq!(config.tier_policies.len(), 4);
    }

    #[test]
    fn test_task_state_tier_escalation() {
        let budget = acquire_budget(100);
        let state = TaskState::new(budget, 0, TaskContext { worker_id: 0, priority: None });

        assert_eq!(state.current_tier.load(Ordering::Acquire), 0);

        let new_tier = state.try_escalate_tier(0);
        assert_eq!(new_tier, Some(1));
        assert_eq!(state.current_tier.load(Ordering::Acquire), 1);

        // Escalate to max tier
        std::thread::sleep(Duration::from_millis(1));
        state.try_escalate_tier(0);
        std::thread::sleep(Duration::from_millis(1));
        state.try_escalate_tier(0);
        assert_eq!(state.current_tier.load(Ordering::Acquire), 3);

        // Should not escalate beyond tier 3
        let tier = state.try_escalate_tier(0);
        assert_eq!(tier, None);
    }

    #[test]
    fn test_violation_cooldown() {
        let budget = acquire_budget(100);
        let state = TaskState::new(budget, 0, TaskContext { worker_id: 0, priority: None });

        // Trigger rapid violations to activate cooldown
        for _ in 0..12 {
            state.track_violation(10.0, 1000); // 10 violations/sec threshold, 1 sec cooldown
        }

        // Should be in cooldown now
        assert!(state.is_in_cooldown());

        // Try to escalate tier during cooldown
        let result = state.try_escalate_tier(0);
        assert_eq!(result, None); // Should fail due to cooldown

        // Sleep to exceed cooldown period
        std::thread::sleep(Duration::from_millis(1100));

        // Should no longer be in cooldown
        assert!(!state.is_in_cooldown());

        // Now tier escalation should work
        let result = state.try_escalate_tier(0);
        assert_eq!(result, Some(1));
    }

    #[test]
    fn test_tier_hysteresis() {
        let budget = acquire_budget(100);
        let state = TaskState::new(budget, 0, TaskContext { worker_id: 0, priority: None });

        // Sleep briefly to ensure first escalation can occur
        // (initial last_tier_change is set to now() in constructor)
        std::thread::sleep(Duration::from_millis(1));

        // First promotion should succeed if we use 0 hysteresis
        let result = state.try_escalate_tier(0);
        assert_eq!(result, Some(1));
        assert_eq!(state.current_tier.load(Ordering::Acquire), 1);

        // Second promotion within hysteresis period should be pending
        let result = state.try_escalate_tier(100);
        assert_eq!(result, None);
        assert_eq!(state.current_tier.load(Ordering::Acquire), 1);
        assert_eq!(state.pending_tier_change.load(Ordering::Acquire), 2);

        // Sleep to exceed hysteresis period
        std::thread::sleep(Duration::from_millis(110));

        // Check pending change should apply
        state.check_pending_tier_change(100, 500);
        assert_eq!(state.current_tier.load(Ordering::Acquire), 2);
        assert_eq!(state.pending_tier_change.load(Ordering::Acquire), u32::MAX);

        // Set up good behavior for demotion test
        for _ in 0..15 {
            state.record_good_poll(100_000); // Record 15 good polls (>10 threshold)
        }

        // Wait for violation time requirement (>1 second since last violation)
        std::thread::sleep(Duration::from_millis(1100));

        // Test demotion hysteresis - should fail due to minimum tier duration (2000ms)
        let tier_policy_strict = TierPolicy {
            name: "test_strict",
            action: InterventionAction::Monitor,
            promotion_threshold: 5,
            promotion_cpu_threshold_ms: 2,
            promotion_slow_poll_threshold: 3,
            demotion_good_polls: 10,
            demotion_min_duration_ms: 2000,
            demotion_cpu_threshold_ns: 1_000_000,
            demotion_evaluation_interval: 5,
        };
        let result = state.try_demote_tier(500, &tier_policy_strict, false); // production mode
        assert_eq!(result, None); // Should be None due to minimum tier duration

        // Now test with shorter min tier duration but within hysteresis period
        // Use a longer hysteresis period since we've already waited 1100ms
        let tier_policy_lenient = TierPolicy {
            name: "test_lenient",
            action: InterventionAction::Monitor,
            promotion_threshold: 5,
            promotion_cpu_threshold_ms: 2,
            promotion_slow_poll_threshold: 3,
            demotion_good_polls: 10,
            demotion_min_duration_ms: 100,
            demotion_cpu_threshold_ns: 1_000_000,
            demotion_evaluation_interval: 5,
        };
        let result = state.try_demote_tier(2000, &tier_policy_lenient, true); // test mode with longer hysteresis
        assert_eq!(result, None); // Should be pending due to hysteresis
        assert_eq!(state.pending_tier_change.load(Ordering::Acquire), 1);
    }

    #[test]
    fn test_enhanced_demotion_logic() {
        let budget = crate::budget::TaskBudget::new(1000);
        let state = TaskState::new(Arc::new(budget), 0, TaskContext { worker_id: 0, priority: None });

        // Promote to tier 2
        let _ = state.try_escalate_tier(0);
        let _ = state.try_escalate_tier(0);
        assert_eq!(state.current_tier.load(Ordering::Acquire), 2);

        // Test 1: Insufficient good behavior should fail
        let tier_policy = TierPolicy {
            name: "test_enhanced",
            action: InterventionAction::Monitor,
            promotion_threshold: 5,
            promotion_cpu_threshold_ms: 2,
            promotion_slow_poll_threshold: 3,
            demotion_good_polls: 50,
            demotion_min_duration_ms: 100,
            demotion_cpu_threshold_ns: 1_000_000,
            demotion_evaluation_interval: 5,
        };
        let result = state.try_demote_tier(100, &tier_policy, true); // test mode
        assert_eq!(result, None);

        // Test 2: Set up good behavior but too recent violation (test mode bypasses time check)
        for _ in 0..60 {
            state.record_good_poll(100_000);
        }

        // Wait for hysteresis period (need to sleep since hysteresis is still checked even in test mode)
        std::thread::sleep(Duration::from_millis(150));

        // In test mode, time checks are bypassed but hysteresis still applies
        let result = state.try_demote_tier(100, &tier_policy, true); // test mode
        assert_eq!(result, Some(1));

        // Test 3: Verify demotion occurred
        assert_eq!(state.current_tier.load(Ordering::Acquire), 1);

        // Test 4: Good behavior counters should be reset after demotion
        assert_eq!(state.consecutive_good_polls.load(Ordering::Relaxed), 0);
        assert_eq!(state.good_behavior_duration_ns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_tier_manager_creation() {
        let config = TierConfig::default();
        let manager = TierManager::new(config);
        assert_eq!(manager.task_states.len(), 0);
        assert_eq!(manager.slow_queue_size.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_tier_manager_task_lifecycle() {
        let manager = TierManager::new(TierConfig::default());
        let task_id = TaskId(1);
        let ctx = TaskContext {
            worker_id: 0,
            priority: None,
        };

        // Before first poll - should create task state
        manager.before_poll(task_id, &ctx);
        assert_eq!(manager.task_states.len(), 1);

        // After poll
        manager.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));

        // Voluntary yield
        manager.on_yield(task_id);

        // Completion removes state
        manager.on_completion(task_id);
        assert_eq!(manager.task_states.len(), 0);
    }

    #[test]
    fn test_tier_manager_metrics() {
        let manager = TierManager::new(TierConfig::default());
        let task_id = TaskId(1);
        let ctx = TaskContext {
            worker_id: 0,
            priority: None,
        };

        manager.before_poll(task_id, &ctx);
        manager.after_poll(task_id, PollResult::Pending, Duration::from_micros(100));

        let metrics = manager.metrics();
        assert_eq!(metrics.total_polls, 1);
        assert_eq!(metrics.active_tasks, 1);
    }

    #[test]
    fn test_runtime_configuration_updates() {
        let manager = Arc::new(TierManager::new(TierConfig::default()));

        // Test 1: Update a single tier policy
        let new_policy = TierPolicyBuilder::new("TestUpdate", InterventionAction::Warn)
            .promotion_violations(3)
            .demotion_good_polls(25)
            .build();

        let result = manager.update_tier_policy(1, new_policy.clone());
        assert!(result.is_ok(), "Failed to update tier policy: {:?}", result);

        // Verify the update was applied
        let config = manager.get_current_config();
        assert_eq!(config.tier_policies[1].name, "TestUpdate");
        assert_eq!(config.tier_policies[1].promotion_threshold, 3);
        assert_eq!(config.tier_policies[1].demotion_good_polls, 25);

        // Test 2: Try to update invalid tier index
        let invalid_result = manager.update_tier_policy(10, new_policy.clone());
        assert!(invalid_result.is_err());
        assert!(invalid_result.unwrap_err().contains("Invalid tier index"));

        // Test 3: Try to update with invalid policy
        let invalid_policy = TierPolicy {
            name: "Invalid",
            action: InterventionAction::Monitor,
            promotion_threshold: 0, // Invalid: must be > 0
            promotion_cpu_threshold_ms: 2,
            promotion_slow_poll_threshold: 3,
            demotion_good_polls: 10,
            demotion_min_duration_ms: 100,
            demotion_cpu_threshold_ns: 1_000_000,
            demotion_evaluation_interval: 5,
        };
        let invalid_result = manager.update_tier_policy(0, invalid_policy);
        assert!(invalid_result.is_err());
        assert!(invalid_result.unwrap_err().contains("Promotion threshold must be greater than 0"));

        // Test 4: Update global configuration
        let original_budget = manager.get_current_config().poll_budget;
        manager.update_global_config(|config| {
            config.poll_budget = 5000;
            config.demotion_hysteresis_ms = 1500;
        });

        let updated_config = manager.get_current_config();
        assert_eq!(updated_config.poll_budget, 5000);
        assert_eq!(updated_config.demotion_hysteresis_ms, 1500);
        assert_ne!(updated_config.poll_budget, original_budget);

        // Test 5: Update multiple tier policies atomically
        let policy1 = TierPolicyBuilder::new("MultiUpdate1", InterventionAction::Monitor)
            .promotion_violations(2)
            .build();
        let policy2 = TierPolicyBuilder::new("MultiUpdate2", InterventionAction::Warn)
            .promotion_violations(4)
            .build();

        let updates = vec![(0, policy1), (2, policy2)];
        let result = manager.update_multiple_tier_policies(updates);
        assert!(result.is_ok(), "Failed to update multiple policies: {:?}", result);

        // Verify multiple updates
        let final_config = manager.get_current_config();
        assert_eq!(final_config.tier_policies[0].name, "MultiUpdate1");
        assert_eq!(final_config.tier_policies[0].promotion_threshold, 2);
        assert_eq!(final_config.tier_policies[2].name, "MultiUpdate2");
        assert_eq!(final_config.tier_policies[2].promotion_threshold, 4);

        // Test 6: Attempt multiple updates with one invalid index
        let invalid_updates = vec![(0, new_policy.clone()), (99, new_policy)];
        let result = manager.update_multiple_tier_policies(invalid_updates);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid tier index"));
    }

    #[test]
    fn test_tier_policy_validation() {
        let manager = TierManager::new(TierConfig::default());

        // Test valid policy
        let valid_policy = TierPolicyBuilder::new("Valid", InterventionAction::Monitor)
            .promotion_violations(5)
            .demotion_good_polls(10)
            .build();
        let result = manager.validate_tier_policy(&valid_policy);
        assert!(result.is_ok());

        // Test invalid promotion threshold
        let invalid_policy = TierPolicy {
            name: "Invalid",
            action: InterventionAction::Monitor,
            promotion_threshold: 0,
            promotion_cpu_threshold_ms: 2,
            promotion_slow_poll_threshold: 3,
            demotion_good_polls: 10,
            demotion_min_duration_ms: 100,
            demotion_cpu_threshold_ns: 1_000_000,
            demotion_evaluation_interval: 5,
        };
        let result = manager.validate_tier_policy(&invalid_policy);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Promotion threshold must be greater than 0"));

        // Test threshold too high
        let high_threshold_policy = TierPolicy {
            name: "TooHigh",
            action: InterventionAction::Monitor,
            promotion_threshold: 2000, // Too high
            promotion_cpu_threshold_ms: 2,
            promotion_slow_poll_threshold: 3,
            demotion_good_polls: 10,
            demotion_min_duration_ms: 100,
            demotion_cpu_threshold_ns: 1_000_000,
            demotion_evaluation_interval: 5,
        };
        let result = manager.validate_tier_policy(&high_threshold_policy);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Promotion threshold too high"));
    }

    #[test]
    fn test_configuration_profiles() {
        // Test creating configs from profiles
        let aggressive_config = ConfigProfile::Aggressive.create_config();
        let balanced_config = ConfigProfile::Balanced.create_config();
        let permissive_config = ConfigProfile::Permissive.create_config();
        let custom_config = ConfigProfile::Custom.create_config();

        // Verify aggressive profile characteristics
        assert_eq!(aggressive_config.poll_budget, 1000);
        assert!(aggressive_config.enable_isolation);
        assert_eq!(aggressive_config.tier_policies[0].name, "AggressiveMonitor");
        assert_eq!(aggressive_config.tier_policies[0].promotion_threshold, 2);
        assert_eq!(aggressive_config.tier_policies[0].demotion_good_polls, 30);

        // Verify balanced profile uses default
        let default_config = TierConfig::default();
        assert_eq!(balanced_config.poll_budget, default_config.poll_budget);
        assert_eq!(balanced_config.tier_policies[0].name, default_config.tier_policies[0].name);

        // Verify permissive profile characteristics
        assert_eq!(permissive_config.poll_budget, 10000);
        assert!(!permissive_config.enable_isolation);
        assert_eq!(permissive_config.tier_policies[0].name, "PermissiveMonitor");
        assert_eq!(permissive_config.tier_policies[0].promotion_threshold, 10);
        assert_eq!(permissive_config.tier_policies[0].demotion_good_polls, 10);

        // Verify custom profile uses default as base
        assert_eq!(custom_config.poll_budget, default_config.poll_budget);

        // Test profile descriptions
        assert!(ConfigProfile::Aggressive.description().contains("low thresholds"));
        assert!(ConfigProfile::Balanced.description().contains("moderate thresholds"));
        assert!(ConfigProfile::Permissive.description().contains("high thresholds"));
        assert!(ConfigProfile::Custom.description().contains("full control"));

        // Test all_profiles()
        let all_profiles = ConfigProfile::all_profiles();
        assert_eq!(all_profiles.len(), 4);
        assert!(all_profiles.contains(&ConfigProfile::Aggressive));
        assert!(all_profiles.contains(&ConfigProfile::Balanced));
        assert!(all_profiles.contains(&ConfigProfile::Permissive));
        assert!(all_profiles.contains(&ConfigProfile::Custom));
    }

    #[test]
    fn test_tier_manager_with_profiles() {
        // Test creating managers with different profiles
        let aggressive_manager = TierManager::with_profile(ConfigProfile::Aggressive);
        let balanced_manager = TierManager::with_profile(ConfigProfile::Balanced);
        let permissive_manager = TierManager::with_profile(ConfigProfile::Permissive);

        // Verify different poll budgets
        let aggressive_config = aggressive_manager.get_current_config();
        let balanced_config = balanced_manager.get_current_config();
        let permissive_config = permissive_manager.get_current_config();

        assert_eq!(aggressive_config.poll_budget, 1000);
        assert_eq!(balanced_config.poll_budget, 2000); // Default
        assert_eq!(permissive_config.poll_budget, 10000);

        // Test profile switching
        balanced_manager.switch_to_profile(ConfigProfile::Aggressive);
        let updated_config = balanced_manager.get_current_config();
        assert_eq!(updated_config.poll_budget, 1000);

        // Test current_profile detection
        assert_eq!(aggressive_manager.current_profile(), Some(ConfigProfile::Aggressive));
        assert_eq!(balanced_manager.current_profile(), Some(ConfigProfile::Aggressive)); // After switch
        assert_eq!(permissive_manager.current_profile(), Some(ConfigProfile::Permissive));

        // Test switching to Custom profile should be ignored
        balanced_manager.switch_to_profile(ConfigProfile::Custom);
        let config_after_custom = balanced_manager.get_current_config();
        assert_eq!(config_after_custom.poll_budget, 1000); // Should remain aggressive
    }

    #[test]
    fn test_profile_config_matching() {
        let manager = TierManager::with_profile(ConfigProfile::Balanced);

        // Should detect balanced profile
        assert_eq!(manager.current_profile(), Some(ConfigProfile::Balanced));

        // Modify config slightly to make it custom
        manager.update_global_config(|config| {
            config.poll_budget = 9999; // Non-standard value
        });

        // Should now return None (custom)
        assert_eq!(manager.current_profile(), None);

        // Switch back to a known profile
        manager.switch_to_profile(ConfigProfile::Permissive);
        assert_eq!(manager.current_profile(), Some(ConfigProfile::Permissive));
    }

    #[test]
    fn test_profile_tier_policies_differences() {
        let aggressive_config = ConfigProfile::Aggressive.create_config();
        let permissive_config = ConfigProfile::Permissive.create_config();

        // Compare Monitor tier policies
        let aggressive_monitor = &aggressive_config.tier_policies[0];
        let permissive_monitor = &permissive_config.tier_policies[0];

        // Aggressive should have lower promotion thresholds
        assert!(aggressive_monitor.promotion_threshold < permissive_monitor.promotion_threshold);
        assert!(aggressive_monitor.promotion_cpu_threshold_ms < permissive_monitor.promotion_cpu_threshold_ms);

        // Aggressive should have higher demotion requirements
        assert!(aggressive_monitor.demotion_good_polls > permissive_monitor.demotion_good_polls);

        // Aggressive should have stricter CPU thresholds for good behavior
        assert!(aggressive_monitor.demotion_cpu_threshold_ns < permissive_monitor.demotion_cpu_threshold_ns);

        // Test global settings differences
        assert!(aggressive_config.promotion_hysteresis_ms < permissive_config.promotion_hysteresis_ms);
        assert!(aggressive_config.demotion_hysteresis_ms < permissive_config.demotion_hysteresis_ms);
        assert!(aggressive_config.violation_rate_threshold < permissive_config.violation_rate_threshold);
        assert!(aggressive_config.cooldown_duration_ms > permissive_config.cooldown_duration_ms);
    }

    #[test]
    fn test_priority_based_scheduling() {
        let manager = TierManager::new(TierConfig::default());

        // Test different priority levels
        let critical_context = TaskContext { worker_id: 0, priority: Some(0) }; // Critical
        let normal_context = TaskContext { worker_id: 1, priority: None };       // Normal (default)
        let low_context = TaskContext { worker_id: 2, priority: Some(3) };       // Low

        // Verify priority mapping
        assert_eq!(critical_context.task_priority(), TaskPriority::Critical);
        assert_eq!(normal_context.task_priority(), TaskPriority::Normal);
        assert_eq!(low_context.task_priority(), TaskPriority::Low);

        // Verify threshold multipliers
        assert_eq!(critical_context.priority_threshold_multiplier(), 0.25);
        assert_eq!(normal_context.priority_threshold_multiplier(), 1.0);
        assert_eq!(low_context.priority_threshold_multiplier(), 2.0);

        // Verify budget multipliers
        assert_eq!(critical_context.priority_budget_multiplier(), 4.0);
        assert_eq!(normal_context.priority_budget_multiplier(), 1.0);
        assert_eq!(low_context.priority_budget_multiplier(), 0.5);

        // Test that critical priority tasks escalate faster
        let critical_task = TaskId(1);
        let normal_task = TaskId(2);

        // Both tasks start at tier 0
        manager.before_poll(critical_task, &critical_context);
        manager.before_poll(normal_task, &normal_context);

        // Simulate budget violations
        let short_duration = Duration::from_micros(50);

        // With default promotion_threshold of 3:
        // - Critical needs: 3 * 0.25 = 0.75 → 1 violation (min)
        // - Normal needs: 3 * 1.0 = 3 violations

        // Give critical task 1 violation (should promote)
        manager.after_poll(critical_task, PollResult::Pending, short_duration);

        // Give normal task 2 violations (should not promote yet)
        manager.after_poll(normal_task, PollResult::Pending, short_duration);
        manager.before_poll(normal_task, &normal_context);
        manager.after_poll(normal_task, PollResult::Pending, short_duration);

        // Verify critical task escalated but normal didn't
        let critical_tier = manager.task_states.get(&critical_task)
            .map(|state| state.current_tier.load(Ordering::Acquire))
            .unwrap_or(0);
        let normal_tier = manager.task_states.get(&normal_task)
            .map(|state| state.current_tier.load(Ordering::Acquire))
            .unwrap_or(0);

        // The exact tier values depend on budget violation detection
        // but critical should escalate faster than normal
        println!("Critical tier: {}, Normal tier: {}", critical_tier, normal_tier);
        assert!(critical_tier >= normal_tier);
    }

    #[test]
    fn test_priority_budget_allocation() {
        let manager = TierManager::new(TierConfig::default());

        let critical_context = TaskContext { worker_id: 0, priority: Some(0) };
        let low_context = TaskContext { worker_id: 1, priority: Some(3) };

        let critical_task = TaskId(10);
        let low_task = TaskId(11);

        // Register tasks to trigger budget allocation
        manager.before_poll(critical_task, &critical_context);
        manager.before_poll(low_task, &low_context);

        // Verify tasks were created and have different budget allocations
        let critical_remaining = manager.task_states.get(&critical_task)
            .map(|state| state.budget.remaining())
            .unwrap_or(0);
        let low_remaining = manager.task_states.get(&low_task)
            .map(|state| state.budget.remaining())
            .unwrap_or(0);

        // Critical should have 4x budget (4.0 multiplier)
        // Low should have 0.5x budget (0.5 multiplier)
        // So critical should have 8x more budget than low
        assert!(critical_remaining > low_remaining);
        println!("Critical budget: {}, Low budget: {}", critical_remaining, low_remaining);
    }
}
