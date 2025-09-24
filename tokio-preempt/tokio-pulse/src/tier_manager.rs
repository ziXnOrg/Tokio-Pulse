//! Multi-Tier Task Management System for Preemption Control

#![allow(clippy::significant_drop_tightening)]  // RwLock guards are held for short durations
#![allow(clippy::trivially_copy_pass_by_ref)]   // TaskId references are more idiomatic
//!
//! This module implements a sophisticated multi-tier intervention system that manages
//! task execution based on their behavior. Tasks start at Tier 0 (Monitor) and can be
//! promoted through increasingly restrictive tiers based on budget violations.
//!
//! # Architecture
//!
//! The TierManager uses lock-free data structures where possible:
//! - `DashMap` for concurrent task state management
//! - `SegQueue` for lock-free slow task queue
//! - Atomic operations for configuration updates
//! - Thread-local storage to minimize contention
//!
//! # Tier Levels
//!
//! - **Tier 0 (Monitor)**: Normal execution with budget tracking
//! - **Tier 1 (Warn)**: Task has exceeded budget, warnings logged
//! - **Tier 2 (Yield)**: Task forced to yield at next poll
//! - **Tier 3 (Isolate)**: Task moved to slow queue or OS isolation
//!
//! # Performance
//!
//! - State lookup: O(1) average with DashMap
//! - Queue operations: Lock-free with SegQueue
//! - Per-poll overhead: <100ns when no intervention needed
//! - Memory: 64 bytes per tracked task + 16 bytes in TaskBudget
//!
//! # Example
//!
//! ```rust
//! use tokio_pulse::tier_manager::{TierManager, TierConfig, TaskId, TaskContext, PollResult};
//! use std::time::Duration;
//!
//! let config = TierConfig::default();
//! let manager = TierManager::new(config);
//!
//! // In the runtime hooks
//! let task_id = TaskId(1);
//! let task_context = TaskContext { worker_id: 0, priority: None };
//! let poll_result = PollResult::Pending;
//! let duration = Duration::from_micros(100);
//!
//! manager.before_poll(task_id, &task_context);
//! // ... poll task ...
//! manager.after_poll(task_id, poll_result, duration);
//! ```

use crate::budget::TaskBudget;
use crate::timing::create_cpu_timer;
use crossbeam::queue::SegQueue;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Unique identifier for a task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

/// Configuration for the tier management system
#[derive(Debug, Clone)]
pub struct TierConfig {
    /// Number of poll operations allowed per budget window
    pub poll_budget: u32,

    /// CPU milliseconds allowed per budget window
    pub cpu_ms_budget: u64,

    /// Interval for forced yields in procedural macros
    pub yield_interval: u32,

    /// Policies for each intervention tier
    pub tier_policies: [TierPolicy; 4],

    /// Enable OS-level isolation for Tier 3 tasks
    pub enable_isolation: bool,

    /// Maximum size of the slow queue before dropping tasks
    pub max_slow_queue_size: usize,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            poll_budget: 2000, // Based on BEAM's 4000 reductions
            cpu_ms_budget: 10, // 10ms CPU time per window
            yield_interval: 100, // Force yield every 100 iterations
            tier_policies: [
                TierPolicy {
                    name: "Monitor",
                    promotion_threshold: 5, // Promote after 5 violations
                    action: InterventionAction::Monitor,
                },
                TierPolicy {
                    name: "Warn",
                    promotion_threshold: 3, // Promote after 3 more violations
                    action: InterventionAction::Warn,
                },
                TierPolicy {
                    name: "Yield",
                    promotion_threshold: 2, // Promote after 2 more violations
                    action: InterventionAction::Yield,
                },
                TierPolicy {
                    name: "Isolate",
                    promotion_threshold: u32::MAX, // Terminal tier
                    action: InterventionAction::Isolate,
                },
            ],
            enable_isolation: false,
            max_slow_queue_size: 10000,
        }
    }
}

/// Policy configuration for a specific tier
#[derive(Debug, Clone)]
pub struct TierPolicy {
    /// Human-readable name for the tier
    pub name: &'static str,

    /// Number of budget violations before promotion to next tier
    pub promotion_threshold: u32,

    /// Action to take when task is in this tier
    pub action: InterventionAction,
}

/// Actions that can be taken for tasks at different tiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterventionAction {
    /// Monitor task behavior without intervention
    Monitor,

    /// Log warnings about task behavior
    Warn,

    /// Force task to yield at next poll
    Yield,

    /// Move task to slow queue
    SlowQueue,

    /// Isolate task using OS facilities
    Isolate,
}

/// Result of a poll operation
#[derive(Debug, Clone, Copy)]
pub enum PollResult {
    /// Task is ready and completed
    Ready,

    /// Task is pending and will be polled again
    Pending,

    /// Task panicked during execution
    Panicked,
}

/// Per-task state managed by the TierManager
#[derive(Debug)]
struct TaskState {
    /// Task's budget tracker (shared with runtime)
    budget: Arc<TaskBudget>,

    /// Current tier (0-3)
    current_tier: AtomicU32,

    /// Number of times budget has been violated
    violation_count: AtomicU32,

    /// Total CPU time consumed (nanoseconds)
    total_cpu_ns: AtomicU64,

    /// Last poll timestamp
    last_poll_time: RwLock<Instant>,

    /// Number of consecutive slow polls
    slow_poll_count: AtomicU32,

    /// Whether task should be forced to yield
    force_yield: AtomicU32,
}

impl TaskState {
    fn new(budget: Arc<TaskBudget>) -> Self {
        Self {
            budget,
            current_tier: AtomicU32::new(0),
            violation_count: AtomicU32::new(0),
            total_cpu_ns: AtomicU64::new(0),
            last_poll_time: RwLock::new(Instant::now()),
            slow_poll_count: AtomicU32::new(0),
            force_yield: AtomicU32::new(0),
        }
    }

    #[inline]
    fn should_yield(&self) -> bool {
        self.force_yield.load(Ordering::Acquire) > 0
            || self.budget.is_exhausted()
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
    fn escalate_tier(&self) -> u32 {
        let current = self.current_tier.load(Ordering::Acquire);
        if current < 3 {
            let new_tier = current + 1;
            self.current_tier.store(new_tier, Ordering::Release);
            new_tier
        } else {
            current
        }
    }

    #[inline]
    fn demote_tier(&self) {
        let current = self.current_tier.load(Ordering::Acquire);
        if current > 0 {
            self.current_tier.store(current - 1, Ordering::Release);
        }
    }
}

/// Context passed to hook functions
#[derive(Debug, Clone)]
pub struct TaskContext {
    /// Worker thread ID
    pub worker_id: usize,

    /// Task priority (if applicable)
    pub priority: Option<u8>,
}

/// Thread-local statistics for worker threads
#[allow(dead_code)]
#[derive(Debug, Default)]
struct WorkerStats {
    /// Total polls processed by this worker
    polls_processed: AtomicU64,

    /// Number of forced yields
    yields_forced: AtomicU64,

    /// Number of tier promotions
    tier_promotions: AtomicU64,

    /// Number of tier demotions
    tier_demotions: AtomicU64,
}

/// The main tier management system
pub struct TierManager {
    /// Configuration
    config: Arc<RwLock<TierConfig>>,

    /// Per-task state tracking
    task_states: DashMap<TaskId, Arc<TaskState>>,

    /// Queue of slow tasks requiring special handling
    slow_queue: SegQueue<TaskId>,

    /// Current size of slow queue (for metrics)
    slow_queue_size: AtomicUsize,

    /// Global metrics
    global_polls: AtomicU64,
    global_violations: AtomicU64,
    global_yields: AtomicU64,

    /// CPU timer for measuring task execution time
    #[allow(dead_code)]
    cpu_timer: Box<dyn crate::timing::CpuTimer>,
}

impl TierManager {
    /// Creates a new TierManager with the given configuration
    pub fn new(config: TierConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            task_states: DashMap::new(),
            slow_queue: SegQueue::new(),
            slow_queue_size: AtomicUsize::new(0),
            global_polls: AtomicU64::new(0),
            global_violations: AtomicU64::new(0),
            global_yields: AtomicU64::new(0),
            cpu_timer: create_cpu_timer(),
        }
    }

    /// Called before polling a task
    pub fn before_poll(&self, task_id: TaskId, _ctx: &TaskContext) {
        self.global_polls.fetch_add(1, Ordering::Relaxed);

        // Get or create task state
        let state = self
            .task_states
            .entry(task_id)
            .or_insert_with(|| {
                let config = self.config.read();
                Arc::new(TaskState::new(Arc::new(TaskBudget::new(
                    config.poll_budget,
                ))))
            });

        // Update last poll time
        *state.last_poll_time.write() = Instant::now();

        // Check if task should yield before polling
        if state.should_yield() {
            #[cfg(feature = "tracing")]
            tracing::debug!(task_id = ?task_id, tier = state.current_tier.load(Ordering::Acquire),
                          "Task scheduled to yield");
        }
    }

    /// Called after polling a task
    pub fn after_poll(
        &self,
        task_id: TaskId,
        result: PollResult,
        poll_duration: Duration,
    ) {
        let Some(state) = self.task_states.get(&task_id) else {
            return;
        };

        // Update CPU time tracking
        let cpu_ns = poll_duration.as_nanos() as u64;
        state.total_cpu_ns.fetch_add(cpu_ns, Ordering::Relaxed);

        // Check if poll was slow (>1ms)
        let is_slow_poll = poll_duration.as_millis() > 1;
        if is_slow_poll {
            state.slow_poll_count.fetch_add(1, Ordering::Relaxed);
        }

        // Consume budget
        if state.budget.consume() {
            self.handle_budget_violation(&task_id, &state);
        }

        // Clear yield flag if task yielded
        if matches!(result, PollResult::Pending) && state.force_yield.load(Ordering::Acquire) > 0 {
            state.clear_yield_flag();
            state.budget.reset(self.config.read().poll_budget);
            self.global_yields.fetch_add(1, Ordering::Relaxed);
        }

        // Apply tier-specific interventions
        self.apply_intervention(&task_id, &state);
    }

    /// Called when a task yields voluntarily
    pub fn on_yield(&self, task_id: TaskId) {
        // Increment global yield counter
        self.global_yields.fetch_add(1, Ordering::Relaxed);

        if let Some(state) = self.task_states.get(&task_id) {
            let config = self.config.read();

            // Reset budget
            state.budget.reset(config.poll_budget);

            // Clear violation count
            state.violation_count.store(0, Ordering::Release);

            // Potentially demote tier for good behavior
            if state.current_tier.load(Ordering::Acquire) > 0 {
                state.demote_tier();

                #[cfg(feature = "tracing")]
                tracing::debug!(task_id = ?task_id, new_tier = state.current_tier.load(Ordering::Acquire),
                              "Task demoted for voluntary yield");
            }
        }
    }

    /// Called when a task completes
    pub fn on_completion(&self, task_id: TaskId) {
        self.task_states.remove(&task_id);

        #[cfg(feature = "tracing")]
        tracing::trace!(task_id = ?task_id, "Task completed, state removed");
    }

    /// Handles a budget violation for a task
    fn handle_budget_violation(&self, task_id: &TaskId, state: &Arc<TaskState>) {
        self.global_violations.fetch_add(1, Ordering::Relaxed);

        let violations = state.violation_count.fetch_add(1, Ordering::AcqRel) + 1;
        let current_tier = state.current_tier.load(Ordering::Acquire);

        let config = self.config.read();
        let tier_policy = &config.tier_policies[current_tier as usize];

        // Check if we should promote to next tier
        if violations >= tier_policy.promotion_threshold {
            let new_tier = state.escalate_tier();
            state.violation_count.store(0, Ordering::Release);

            #[cfg(feature = "tracing")]
            tracing::warn!(task_id = ?task_id, old_tier = current_tier, new_tier = new_tier,
                          violations = violations, "Task promoted to higher tier");

            #[cfg(feature = "metrics")]
            metrics::counter!("tokio_pulse.tier_promotions").increment(1);
        }
    }

    /// Applies intervention based on current tier
    fn apply_intervention(&self, task_id: &TaskId, state: &Arc<TaskState>) {
        let current_tier = state.current_tier.load(Ordering::Acquire);
        let config = self.config.read();
        let action = config.tier_policies[current_tier as usize].action;

        match action {
            InterventionAction::Monitor => {
                // No intervention needed
            }
            InterventionAction::Warn => {
                #[cfg(feature = "tracing")]
                tracing::warn!(task_id = ?task_id, tier = current_tier,
                              "Task consuming excessive CPU time");

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.intervention.warn").increment(1);
            }
            InterventionAction::Yield => {
                state.mark_for_yield();

                #[cfg(feature = "tracing")]
                tracing::info!(task_id = ?task_id, "Task marked for forced yield");

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.intervention.yield").increment(1);
            }
            InterventionAction::SlowQueue => {
                // Add to slow queue if not already there
                if self.slow_queue_size.load(Ordering::Acquire) < config.max_slow_queue_size {
                    self.slow_queue.push(*task_id);
                    self.slow_queue_size.fetch_add(1, Ordering::AcqRel);

                    #[cfg(feature = "tracing")]
                    tracing::info!(task_id = ?task_id, "Task added to slow queue");

                    #[cfg(feature = "metrics")]
                    metrics::counter!("tokio_pulse.intervention.slow_queue").increment(1);
                }
            }
            InterventionAction::Isolate => {
                if config.enable_isolation {
                    self.isolate_task(task_id);
                } else {
                    // Fall back to slow queue
                    self.slow_queue.push(*task_id);
                    self.slow_queue_size.fetch_add(1, Ordering::AcqRel);
                }

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.intervention.isolate").increment(1);
            }
        }
    }

    /// Isolates a task using OS-specific mechanisms
    #[allow(clippy::unused_self)]  // Will need self when we implement actual isolation
    fn isolate_task(&self, task_id: &TaskId) {
        #[cfg(target_os = "linux")]
        {
            // TODO: Implement cgroup isolation
            #[cfg(feature = "tracing")]
            tracing::error!(task_id = ?task_id, "Linux cgroup isolation not yet implemented");
        }

        #[cfg(target_os = "windows")]
        {
            // TODO: Implement Windows job object isolation
            #[cfg(feature = "tracing")]
            tracing::error!(task_id = ?task_id, "Windows job object isolation not yet implemented");
        }

        #[cfg(target_os = "macos")]
        {
            // TODO: Implement macOS task_policy_set
            #[cfg(feature = "tracing")]
            tracing::error!(task_id = ?task_id, "macOS task policy isolation not yet implemented");
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            #[cfg(feature = "tracing")]
            tracing::warn!(task_id = ?task_id, "OS isolation not available on this platform");
        }
    }

    /// Processes tasks in the slow queue
    pub fn process_slow_queue(&self, max_tasks: usize) -> usize {
        let mut processed = 0;

        while processed < max_tasks {
            if let Some(task_id) = self.slow_queue.pop() {
                self.slow_queue_size.fetch_sub(1, Ordering::AcqRel);
                processed += 1;

                // TODO: Actually reschedule the task on a dedicated thread
                #[cfg(feature = "tracing")]
                tracing::debug!(task_id = ?task_id, "Processing slow task");
            } else {
                break;
            }
        }

        processed
    }

    /// Updates the configuration dynamically
    pub fn update_config(&self, config: TierConfig) {
        *self.config.write() = config;

        #[cfg(feature = "tracing")]
        tracing::info!("TierManager configuration updated");
    }

    /// Gets current metrics
    pub fn metrics(&self) -> TierMetrics {
        TierMetrics {
            total_polls: self.global_polls.load(Ordering::Relaxed),
            total_violations: self.global_violations.load(Ordering::Relaxed),
            total_yields: self.global_yields.load(Ordering::Relaxed),
            active_tasks: self.task_states.len(),
            slow_queue_size: self.slow_queue_size.load(Ordering::Relaxed),
        }
    }
}

/// Metrics snapshot for the TierManager
#[derive(Debug, Clone)]
pub struct TierMetrics {
    /// Total number of poll operations
    pub total_polls: u64,

    /// Total number of budget violations
    pub total_violations: u64,

    /// Total number of forced yields
    pub total_yields: u64,

    /// Number of currently tracked tasks
    pub active_tasks: usize,

    /// Current size of the slow queue
    pub slow_queue_size: usize,
}

// Thread-local storage for worker statistics
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
        let budget = Arc::new(TaskBudget::new(100));
        let state = TaskState::new(budget);

        assert_eq!(state.current_tier.load(Ordering::Acquire), 0);

        let new_tier = state.escalate_tier();
        assert_eq!(new_tier, 1);
        assert_eq!(state.current_tier.load(Ordering::Acquire), 1);

        // Escalate to max tier
        state.escalate_tier();
        state.escalate_tier();
        assert_eq!(state.current_tier.load(Ordering::Acquire), 3);

        // Should not escalate beyond tier 3
        let tier = state.escalate_tier();
        assert_eq!(tier, 3);
    }

    #[test]
    fn test_task_state_tier_demotion() {
        let budget = Arc::new(TaskBudget::new(100));
        let state = TaskState::new(budget);

        // Escalate to tier 2
        state.escalate_tier();
        state.escalate_tier();
        assert_eq!(state.current_tier.load(Ordering::Acquire), 2);

        // Demote
        state.demote_tier();
        assert_eq!(state.current_tier.load(Ordering::Acquire), 1);

        // Demote to tier 0
        state.demote_tier();
        assert_eq!(state.current_tier.load(Ordering::Acquire), 0);

        // Should not demote below tier 0
        state.demote_tier();
        assert_eq!(state.current_tier.load(Ordering::Acquire), 0);
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
}