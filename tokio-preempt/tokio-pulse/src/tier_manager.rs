#![forbid(unsafe_code)]
#![allow(clippy::significant_drop_tightening)] /* RwLock guards are held for short durations */
#![allow(clippy::trivially_copy_pass_by_ref)] /* TaskId references are more idiomatic */

/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */
/* Multi-tier task management for preemption control */
use crate::budget::TaskBudget;
use crate::timing::create_cpu_timer;
use crossbeam::queue::SegQueue;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            poll_budget: 2000,   /* BEAM-inspired reduction count */
            cpu_ms_budget: 10,   /* 10ms CPU time window */
            yield_interval: 100, /* Forced yield interval */
            tier_policies: [
                TierPolicy {
                    name: "Monitor",
                    promotion_threshold: 5, /* 5 violations to promote */
                    action: InterventionAction::Monitor,
                },
                TierPolicy {
                    name: "Warn",
                    promotion_threshold: 3, /* 3 violations to promote */
                    action: InterventionAction::Warn,
                },
                TierPolicy {
                    name: "Yield",
                    promotion_threshold: 2, /* 2 violations to promote */
                    action: InterventionAction::Yield,
                },
                TierPolicy {
                    name: "Isolate",
                    promotion_threshold: u32::MAX, /* Terminal tier */
                    action: InterventionAction::Isolate,
                },
            ],
            enable_isolation: false,
            max_slow_queue_size: 10000,
            promotion_hysteresis_ms: 100, /* 100ms min between promotions */
            demotion_hysteresis_ms: 500,  /* 500ms min between demotions */
            violation_rate_threshold: 10.0, /* 10 violations/sec triggers cooldown */
            cooldown_duration_ms: 1000,   /* 1 second cooldown after rapid violations */
        }
    }
}

/// Policy for intervention tiers
#[derive(Debug, Clone)]
pub struct TierPolicy {
    /// Tier name
    pub name: &'static str,

    /// Violations before promotion
    pub promotion_threshold: u32,

    /// Intervention action
    pub action: InterventionAction,
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

/* Per-task state tracking */
#[derive(Debug)]
struct TaskState {
    budget: Arc<TaskBudget>,                 /* Shared budget tracker */
    current_tier: AtomicU32,                 /* Tier level (0-3) */
    violation_count: AtomicU32,              /* Budget violations */
    total_cpu_ns: AtomicU64,                 /* CPU time (ns) */
    last_poll_time: RwLock<Instant>,         /* Last poll timestamp */
    slow_poll_count: AtomicU32,              /* Consecutive slow polls */
    force_yield: AtomicU32,                  /* Yield flag */
    last_tier_change: RwLock<Instant>,       /* Hysteresis: last tier change time */
    pending_tier_change: AtomicU32,          /* Hysteresis: pending tier (u32::MAX = none) */
    cooldown_until: RwLock<Option<Instant>>, /* Cooldown: no tier changes until this time */
    recent_violations: RwLock<Vec<Instant>>, /* Recent violation timestamps for rate tracking */
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
            last_tier_change: RwLock::new(Instant::now()),
            pending_tier_change: AtomicU32::new(u32::MAX),
            cooldown_until: RwLock::new(None),
            recent_violations: RwLock::new(Vec::with_capacity(10)),
        }
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

    fn try_escalate_tier(&self, hysteresis_ms: u64) -> Option<u32> {
        let current = self.current_tier.load(Ordering::Acquire);
        if current >= 3 {
            return None; /* Already at max tier */
        }

        let now = Instant::now();

        /* Check if in cooldown period */
        if let Some(cooldown_end) = *self.cooldown_until.read() {
            if now < cooldown_end {
                return None; /* Still in cooldown */
            }
        }

        let last_change = *self.last_tier_change.read();

        /* Check hysteresis period */
        if now.duration_since(last_change).as_millis() < hysteresis_ms as u128 {
            /* Still in hysteresis period, mark pending */
            self.pending_tier_change.store(current + 1, Ordering::Release);
            return None;
        }

        /* Apply tier change */
        let new_tier = current + 1;
        self.current_tier.store(new_tier, Ordering::Release);
        {
            let mut last_change = self.last_tier_change.write();
            *last_change = now;
        }
        self.pending_tier_change.store(u32::MAX, Ordering::Release);
        Some(new_tier)
    }

    fn try_demote_tier(&self, hysteresis_ms: u64) -> Option<u32> {
        let current = self.current_tier.load(Ordering::Acquire);
        if current == 0 {
            return None; /* Already at min tier */
        }

        let now = Instant::now();

        /* Check if in cooldown period */
        if let Some(cooldown_end) = *self.cooldown_until.read() {
            if now < cooldown_end {
                return None; /* Still in cooldown */
            }
        }

        let last_change = *self.last_tier_change.read();

        /* Check hysteresis period */
        if now.duration_since(last_change).as_millis() < hysteresis_ms as u128 {
            /* Still in hysteresis period, mark pending */
            self.pending_tier_change.store(current - 1, Ordering::Release);
            return None;
        }

        /* Apply tier change */
        let new_tier = current - 1;
        self.current_tier.store(new_tier, Ordering::Release);
        {
            let mut last_change = self.last_tier_change.write();
            *last_change = now;
        }
        self.pending_tier_change.store(u32::MAX, Ordering::Release);
        Some(new_tier)
    }

    fn check_pending_tier_change(&self, promotion_hysteresis_ms: u64, demotion_hysteresis_ms: u64) {
        let pending = self.pending_tier_change.load(Ordering::Acquire);
        if pending == u32::MAX {
            return; /* No pending change */
        }

        let current = self.current_tier.load(Ordering::Acquire);
        let now = Instant::now();
        let last_change = *self.last_tier_change.read();

        if pending > current {
            /* Pending promotion */
            if now.duration_since(last_change).as_millis() >= promotion_hysteresis_ms as u128 {
                self.current_tier.store(pending, Ordering::Release);
                {
                    let mut last_change_mut = self.last_tier_change.write();
                    *last_change_mut = now;
                }
                self.pending_tier_change.store(u32::MAX, Ordering::Release);
            }
        } else if pending < current {
            /* Pending demotion */
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

        /* Add current violation */
        violations.push(now);

        /* Remove old violations (older than 1 second) */
        violations.retain(|&t| now.duration_since(t).as_secs() < 1);

        /* Check violation rate */
        let rate = violations.len() as f64;

        if rate > violation_rate_threshold {
            /* Trigger cooldown */
            let mut cooldown = self.cooldown_until.write();
            *cooldown = Some(now + Duration::from_millis(cooldown_duration_ms));

            /* Clear violation history after triggering cooldown */
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

/* Worker thread statistics */
#[allow(dead_code)]
#[derive(Debug, Default)]
struct WorkerStats {
    polls_processed: AtomicU64, /* Total polls */
    yields_forced: AtomicU64,   /* Forced yields */
    tier_promotions: AtomicU64, /* Tier promotions */
    tier_demotions: AtomicU64,  /* Tier demotions */
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
    #[allow(dead_code)]
    cpu_timer: Box<dyn crate::timing::CpuTimer>,
}

impl TierManager {
    /* Creates new tier manager */
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

    /* Pre-poll hook */
    pub fn before_poll(&self, task_id: TaskId, _ctx: &TaskContext) {
        self.global_polls.fetch_add(1, Ordering::Relaxed);

        /* Get or create task state */
        let state = self.task_states.entry(task_id).or_insert_with(|| {
            let config = self.config.read();
            Arc::new(TaskState::new(Arc::new(TaskBudget::new(config.poll_budget))))
        });

        /* Update poll timestamp */
        *state.last_poll_time.write() = Instant::now();

        /* Check yield requirement */
        if state.should_yield() {
            #[cfg(feature = "tracing")]
            tracing::debug!(task_id = ?task_id, tier = state.current_tier.load(Ordering::Acquire),
                          "Task scheduled to yield");
        }
    }

    /* Post-poll hook */
    pub fn after_poll(&self, task_id: TaskId, result: PollResult, poll_duration: Duration) {
        let Some(state) = self.task_states.get(&task_id) else {
            return;
        };

        /* Track CPU time */
        let cpu_ns = poll_duration.as_nanos() as u64;
        state.total_cpu_ns.fetch_add(cpu_ns, Ordering::Relaxed);

        /* Detect slow polls (>1ms) */
        let is_slow_poll = poll_duration.as_millis() > 1;
        if is_slow_poll {
            state.slow_poll_count.fetch_add(1, Ordering::Relaxed);
        }

        /* Check for pending tier changes (hysteresis expired) */
        let config = self.config.read();
        state.check_pending_tier_change(
            config.promotion_hysteresis_ms,
            config.demotion_hysteresis_ms,
        );

        /* Consume budget */
        if state.budget.consume() {
            self.handle_budget_violation(&task_id, &state);
        }

        /* Clear yield flag after yield */
        if matches!(result, PollResult::Pending) && state.force_yield.load(Ordering::Acquire) > 0 {
            state.clear_yield_flag();
            state.budget.reset(config.poll_budget);
            self.global_yields.fetch_add(1, Ordering::Relaxed);
        }

        /* Apply tier interventions */
        self.apply_intervention(&task_id, &state);
    }

    /* Voluntary yield handler */
    pub fn on_yield(&self, task_id: TaskId) {
        /* Track yield */
        self.global_yields.fetch_add(1, Ordering::Relaxed);

        if let Some(state) = self.task_states.get(&task_id) {
            let config = self.config.read();

            /* Reset budget */
            state.budget.reset(config.poll_budget);

            /* Clear violations */
            state.violation_count.store(0, Ordering::Release);

            /* Demote for good behavior with hysteresis */
            if state.current_tier.load(Ordering::Acquire) > 0 {
                if let Some(new_tier) = state.try_demote_tier(config.demotion_hysteresis_ms) {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(task_id = ?task_id, new_tier = new_tier,
                                  "Task demoted for voluntary yield");

                    #[cfg(feature = "metrics")]
                    metrics::counter!("tokio_pulse.tier_demotions").increment(1);
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(task_id = ?task_id, "Tier demotion pending (hysteresis)");
                }
            }
        }
    }

    /* Task completion handler */
    pub fn on_completion(&self, task_id: TaskId) {
        self.task_states.remove(&task_id);

        #[cfg(feature = "tracing")]
        tracing::trace!(task_id = ?task_id, "Task completed, state removed");
    }

    /* Budget violation handler */
    fn handle_budget_violation(&self, task_id: &TaskId, state: &Arc<TaskState>) {
        self.global_violations.fetch_add(1, Ordering::Relaxed);

        let violations = state.violation_count.fetch_add(1, Ordering::AcqRel) + 1;
        let current_tier = state.current_tier.load(Ordering::Acquire);

        let config = self.config.read();

        /* Track violation rate for cooldown */
        state.track_violation(config.violation_rate_threshold, config.cooldown_duration_ms);

        /* Skip tier changes if in cooldown */
        if state.is_in_cooldown() {
            #[cfg(feature = "tracing")]
            tracing::debug!(task_id = ?task_id, "Task in cooldown period, skipping tier change");
            return;
        }

        let tier_policy = &config.tier_policies[current_tier as usize];

        /* Check tier promotion with hysteresis */
        if violations >= tier_policy.promotion_threshold {
            if let Some(new_tier) = state.try_escalate_tier(config.promotion_hysteresis_ms) {
                state.violation_count.store(0, Ordering::Release);

                #[cfg(feature = "tracing")]
                tracing::warn!(task_id = ?task_id, old_tier = current_tier, new_tier = new_tier,
                              violations = violations, "Task promoted to higher tier");

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.tier_promotions").increment(1);
            } else {
                #[cfg(feature = "tracing")]
                tracing::debug!(task_id = ?task_id, current_tier = current_tier,
                               "Tier promotion pending (hysteresis)");
            }
        }
    }

    /* Apply tier-specific intervention */
    fn apply_intervention(&self, task_id: &TaskId, state: &Arc<TaskState>) {
        let current_tier = state.current_tier.load(Ordering::Acquire);
        let config = self.config.read();
        let action = config.tier_policies[current_tier as usize].action;

        match action {
            InterventionAction::Monitor => { /* No action */ },
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
                /* Add to slow queue */
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
                    /* Fallback to slow queue */
                    self.slow_queue.push(*task_id);
                    self.slow_queue_size.fetch_add(1, Ordering::AcqRel);
                }

                #[cfg(feature = "metrics")]
                metrics::counter!("tokio_pulse.intervention.isolate").increment(1);
            },
        }
    }

    /* OS-specific task isolation */
    #[allow(clippy::unused_self)] /* Future implementation */
    fn isolate_task(&self, task_id: &TaskId) {
        #[cfg(target_os = "linux")]
        {
            /* TODO: cgroup isolation */
            #[cfg(feature = "tracing")]
            tracing::error!(task_id = ?task_id, "Linux cgroup isolation not yet implemented");
        }

        #[cfg(target_os = "windows")]
        {
            /* TODO: job object isolation */
            #[cfg(feature = "tracing")]
            tracing::error!(task_id = ?task_id, "Windows job object isolation not yet implemented");
        }

        #[cfg(target_os = "macos")]
        {
            /* TODO: task_policy_set */
            #[cfg(feature = "tracing")]
            tracing::error!(task_id = ?task_id, "macOS task policy isolation not yet implemented");
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            #[cfg(feature = "tracing")]
            tracing::warn!(task_id = ?task_id, "OS isolation not available on this platform");
        }
    }

    /* Process slow queue tasks */
    pub fn process_slow_queue(&self, max_tasks: usize) -> usize {
        let mut processed = 0;

        while processed < max_tasks {
            if let Some(task_id) = self.slow_queue.pop() {
                self.slow_queue_size.fetch_sub(1, Ordering::AcqRel);
                processed += 1;

                /* TODO: reschedule on dedicated thread */
                #[cfg(feature = "tracing")]
                tracing::debug!(task_id = ?task_id, "Processing slow task");
            } else {
                break;
            }
        }

        processed
    }

    /* Update configuration */
    pub fn update_config(&self, config: TierConfig) {
        *self.config.write() = config;

        #[cfg(feature = "tracing")]
        tracing::info!("TierManager configuration updated");
    }

    /* Get metrics snapshot */
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

/// TierManager metrics
#[derive(Debug, Clone)]
pub struct TierMetrics {
    /// Poll count
    pub total_polls: u64,
    /// Violation count
    pub total_violations: u64,
    /// Yield count
    pub total_yields: u64,
    /// Active task count
    pub active_tasks: usize,
    /// Slow queue size
    pub slow_queue_size: usize,
}

/* Thread-local worker statistics */
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
        let budget = Arc::new(TaskBudget::new(100));
        let state = TaskState::new(budget);

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
        let budget = Arc::new(TaskBudget::new(100));
        let state = TaskState::new(budget);

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

        // Test demotion hysteresis
        let result = state.try_demote_tier(500);
        assert_eq!(result, None); // Should be pending due to hysteresis
        assert_eq!(state.pending_tier_change.load(Ordering::Acquire), 1);
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
