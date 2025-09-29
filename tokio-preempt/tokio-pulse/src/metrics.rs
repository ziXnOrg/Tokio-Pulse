//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
//! Metrics collection and reporting for Tokio-Pulse preemption system
//!
//! This module provides comprehensive metrics for monitoring preemption behavior,
//! task performance, and system health.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[cfg(feature = "metrics")]
use metrics::{counter, histogram, gauge};

#[cfg(feature = "tracing")]
use tracing::{debug, info, warn};

/// Global metrics registry for preemption system
#[derive(Debug)]
pub struct PulseMetrics {
    /// Total number of task polls observed
    pub total_polls: AtomicU64,

    /// Number of budget violations detected
    pub budget_violations: AtomicU64,

    /// Number of voluntary yields performed
    pub voluntary_yields: AtomicU64,

    /// Number of tasks promoted to higher tiers
    pub tier_promotions: AtomicU64,

    /// Number of tasks demoted to lower tiers
    pub tier_demotions: AtomicU64,

    /// Number of tasks moved to slow queue
    pub slow_queue_enqueues: AtomicU64,

    /// Number of tasks processed from slow queue
    pub slow_queue_processed: AtomicU64,

    /// Total CPU time consumed (microseconds)
    pub total_cpu_time_us: AtomicU64,

    /// Number of intervention actions taken
    pub interventions_total: AtomicU64,

    /// Number of configuration updates
    pub config_updates: AtomicU64,

    /// System start time for uptime calculation
    start_time: Instant,
}

impl PulseMetrics {
    /// Create new metrics instance
    #[must_use]
    pub fn new() -> Self {
        Self {
            total_polls: AtomicU64::new(0),
            budget_violations: AtomicU64::new(0),
            voluntary_yields: AtomicU64::new(0),
            tier_promotions: AtomicU64::new(0),
            tier_demotions: AtomicU64::new(0),
            slow_queue_enqueues: AtomicU64::new(0),
            slow_queue_processed: AtomicU64::new(0),
            total_cpu_time_us: AtomicU64::new(0),
            interventions_total: AtomicU64::new(0),
            config_updates: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }

    /// Record a task poll operation
    pub fn record_poll(&self) {
        self.total_polls.fetch_add(1, Ordering::Relaxed);

        #[cfg(feature = "metrics")]
        counter!("pulse_polls_total").increment(1);
    }

    /// Record a budget violation
    pub fn record_budget_violation(&self) {
        self.budget_violations.fetch_add(1, Ordering::Relaxed);

        #[cfg(feature = "tracing")]
        warn!("Budget violation detected");

        #[cfg(feature = "metrics")]
        counter!("pulse_budget_violations_total").increment(1);
    }

    /// Record a voluntary yield
    pub fn record_voluntary_yield(&self) {
        self.voluntary_yields.fetch_add(1, Ordering::Relaxed);

        #[cfg(feature = "metrics")]
        counter!("pulse_voluntary_yields_total").increment(1);
    }

    /// Record tier promotion
    pub fn record_tier_promotion(&self, from_tier: u8, to_tier: u8) {
        self.tier_promotions.fetch_add(1, Ordering::Relaxed);

        #[cfg(feature = "tracing")]
        debug!(from_tier = from_tier, to_tier = to_tier, "Task tier promoted");

        #[cfg(feature = "metrics")]
        {
            counter!("pulse_tier_promotions_total").increment(1);
            counter!("pulse_tier_changes_total", "direction" => "promotion", "from_tier" => from_tier.to_string(), "to_tier" => to_tier.to_string()).increment(1);
        }
    }

    /// Record tier demotion
    pub fn record_tier_demotion(&self, from_tier: u8, to_tier: u8) {
        self.tier_demotions.fetch_add(1, Ordering::Relaxed);

        #[cfg(feature = "tracing")]
        debug!(from_tier = from_tier, to_tier = to_tier, "Task tier demoted");

        #[cfg(feature = "metrics")]
        {
            counter!("pulse_tier_demotions_total").increment(1);
            counter!("pulse_tier_changes_total", "direction" => "demotion", "from_tier" => from_tier.to_string(), "to_tier" => to_tier.to_string()).increment(1);
        }
    }

    /// Record slow queue enqueue
    pub fn record_slow_queue_enqueue(&self) {
        self.slow_queue_enqueues.fetch_add(1, Ordering::Relaxed);

        #[cfg(feature = "tracing")]
        info!("Task added to slow queue");

        #[cfg(feature = "metrics")]
        counter!("pulse_slow_queue_enqueues_total").increment(1);
    }

    /// Record slow queue processing
    pub fn record_slow_queue_processed(&self, count: usize) {
        self.slow_queue_processed.fetch_add(count as u64, Ordering::Relaxed);

        #[cfg(feature = "tracing")]
        debug!(count = count, "Tasks processed from slow queue");

        #[cfg(feature = "metrics")]
        counter!("pulse_slow_queue_processed_total").increment(count as u64);
    }

    /// Record CPU time usage
    pub fn record_cpu_time(&self, duration_us: u64) {
        self.total_cpu_time_us.fetch_add(duration_us, Ordering::Relaxed);

        #[cfg(feature = "metrics")]
        {
            histogram!("pulse_cpu_time_microseconds").record(duration_us as f64);
            counter!("pulse_total_cpu_time_microseconds").increment(duration_us);
        }
    }

    /// Record intervention action
    pub fn record_intervention(&self, action: &str) {
        self.interventions_total.fetch_add(1, Ordering::Relaxed);

        #[cfg(feature = "tracing")]
        warn!(action = action, "Intervention applied");

        #[cfg(feature = "metrics")]
        counter!("pulse_interventions_total", "action" => action.to_string()).increment(1);
    }

    /// Record configuration update
    pub fn record_config_update(&self) {
        self.config_updates.fetch_add(1, Ordering::Relaxed);

        #[cfg(feature = "metrics")]
        counter!("pulse_config_updates_total").increment(1);
    }

    /// Update current slow queue size gauge
    pub fn update_slow_queue_size(&self, size: usize) {
        #[cfg(feature = "metrics")]
        gauge!("pulse_slow_queue_size").set(size as f64);
    }

    /// Update current active tasks gauge
    pub fn update_active_tasks(&self, count: usize) {
        #[cfg(feature = "metrics")]
        gauge!("pulse_active_tasks").set(count as f64);
    }

    /// Get system uptime in seconds
    #[must_use]
    pub fn uptime_seconds(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Get snapshot of current metric values
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            total_polls: self.total_polls.load(Ordering::Relaxed),
            budget_violations: self.budget_violations.load(Ordering::Relaxed),
            voluntary_yields: self.voluntary_yields.load(Ordering::Relaxed),
            tier_promotions: self.tier_promotions.load(Ordering::Relaxed),
            tier_demotions: self.tier_demotions.load(Ordering::Relaxed),
            slow_queue_enqueues: self.slow_queue_enqueues.load(Ordering::Relaxed),
            slow_queue_processed: self.slow_queue_processed.load(Ordering::Relaxed),
            total_cpu_time_us: self.total_cpu_time_us.load(Ordering::Relaxed),
            interventions_total: self.interventions_total.load(Ordering::Relaxed),
            config_updates: self.config_updates.load(Ordering::Relaxed),
            uptime_seconds: self.uptime_seconds(),
        }
    }

    /// Calculate derived metrics
    #[must_use]
    pub fn derived_metrics(&self) -> DerivedMetrics {
        let snapshot = self.snapshot();
        let uptime = snapshot.uptime_seconds.max(1.0); // Avoid division by zero

        DerivedMetrics {
            polls_per_second: snapshot.total_polls as f64 / uptime,
            violation_rate: if snapshot.total_polls > 0 {
                snapshot.budget_violations as f64 / snapshot.total_polls as f64
            } else {
                0.0
            },
            yield_rate: if snapshot.total_polls > 0 {
                snapshot.voluntary_yields as f64 / snapshot.total_polls as f64
            } else {
                0.0
            },
            avg_cpu_time_us: if snapshot.total_polls > 0 {
                snapshot.total_cpu_time_us as f64 / snapshot.total_polls as f64
            } else {
                0.0
            },
            promotion_rate: if snapshot.tier_promotions + snapshot.tier_demotions > 0 {
                snapshot.tier_promotions as f64 / (snapshot.tier_promotions + snapshot.tier_demotions) as f64
            } else {
                0.0
            },
        }
    }
}

/// Point-in-time snapshot of metrics
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSnapshot {
    /// Total number of task polls executed
    pub total_polls: u64,
    /// Total budget violations recorded
    pub budget_violations: u64,
    /// Total voluntary task yields
    pub voluntary_yields: u64,
    /// Total tier promotions (escalations)
    pub tier_promotions: u64,
    /// Total tier demotions (de-escalations)
    pub tier_demotions: u64,
    /// Total tasks enqueued to slow queue
    pub slow_queue_enqueues: u64,
    /// Total tasks processed from slow queue
    pub slow_queue_processed: u64,
    /// Total CPU time consumed (microseconds)
    pub total_cpu_time_us: u64,
    /// Total interventions applied across all tiers
    pub interventions_total: u64,
    /// Total configuration update events
    pub config_updates: u64,
    /// System uptime in seconds
    pub uptime_seconds: f64,
}

/// Derived metrics calculated from base counters
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedMetrics {
    /// Task polls executed per second
    pub polls_per_second: f64,
    /// Budget violation rate (violations/polls)
    pub violation_rate: f64,
    /// Voluntary yield rate (yields/polls)
    pub yield_rate: f64,
    /// Average CPU time per task (microseconds)
    pub avg_cpu_time_us: f64,
    /// Tier promotion rate (promotions/second)
    pub promotion_rate: f64,
}

/// Global metrics instance (optional, when metrics feature is enabled)
#[cfg(feature = "metrics")]
static GLOBAL_METRICS: std::sync::LazyLock<PulseMetrics> = std::sync::LazyLock::new(PulseMetrics::new);

/// Get global metrics instance
#[cfg(feature = "metrics")]
#[must_use]
pub fn global_metrics() -> &'static PulseMetrics {
    &GLOBAL_METRICS
}

/// Initialize metrics with custom registry (useful for testing)
#[cfg(feature = "metrics")]
pub fn init_metrics() {
    #[cfg(feature = "tracing")]
    info!("Initializing Tokio-Pulse metrics registry");

    // Register common metrics with default values
    counter!("pulse_polls_total").absolute(0);
    counter!("pulse_budget_violations_total").absolute(0);
    counter!("pulse_voluntary_yields_total").absolute(0);
    counter!("pulse_tier_promotions_total").absolute(0);
    counter!("pulse_tier_demotions_total").absolute(0);
    counter!("pulse_slow_queue_enqueues_total").absolute(0);
    counter!("pulse_slow_queue_processed_total").absolute(0);
    counter!("pulse_interventions_total").absolute(0);
    counter!("pulse_config_updates_total").absolute(0);
    counter!("pulse_total_cpu_time_microseconds").absolute(0);

    gauge!("pulse_slow_queue_size").set(0.0);
    gauge!("pulse_active_tasks").set(0.0);

    #[cfg(feature = "tracing")]
    info!("Tokio-Pulse metrics registry initialized successfully");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_creation() {
        let metrics = PulseMetrics::new();
        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.total_polls, 0);
        assert_eq!(snapshot.budget_violations, 0);
        assert!(snapshot.uptime_seconds >= 0.0);
    }

    #[test]
    fn test_poll_recording() {
        let metrics = PulseMetrics::new();

        metrics.record_poll();
        metrics.record_poll();

        assert_eq!(metrics.total_polls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_violation_recording() {
        let metrics = PulseMetrics::new();

        metrics.record_budget_violation();
        metrics.record_budget_violation();
        metrics.record_budget_violation();

        assert_eq!(metrics.budget_violations.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_tier_changes() {
        let metrics = PulseMetrics::new();

        metrics.record_tier_promotion(0, 1);
        metrics.record_tier_promotion(1, 2);
        metrics.record_tier_demotion(2, 1);

        assert_eq!(metrics.tier_promotions.load(Ordering::Relaxed), 2);
        assert_eq!(metrics.tier_demotions.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_derived_metrics() {
        let metrics = PulseMetrics::new();

        // Record some test data
        for _ in 0..100 {
            metrics.record_poll();
        }

        for _ in 0..10 {
            metrics.record_budget_violation();
        }

        for _ in 0..5 {
            metrics.record_voluntary_yield();
        }

        let derived = metrics.derived_metrics();

        assert_eq!(derived.violation_rate, 0.1); // 10/100
        assert_eq!(derived.yield_rate, 0.05); // 5/100
        assert!(derived.polls_per_second > 0.0);
    }

    #[test]
    fn test_cpu_time_recording() {
        let metrics = PulseMetrics::new();

        metrics.record_cpu_time(1000);
        metrics.record_cpu_time(2000);

        assert_eq!(metrics.total_cpu_time_us.load(Ordering::Relaxed), 3000);
    }

    #[test]
    fn test_slow_queue_metrics() {
        let metrics = PulseMetrics::new();

        metrics.record_slow_queue_enqueue();
        metrics.record_slow_queue_enqueue();
        metrics.record_slow_queue_processed(5);

        assert_eq!(metrics.slow_queue_enqueues.load(Ordering::Relaxed), 2);
        assert_eq!(metrics.slow_queue_processed.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn test_intervention_recording() {
        let metrics = PulseMetrics::new();

        metrics.record_intervention("yield");
        metrics.record_intervention("isolate");

        assert_eq!(metrics.interventions_total.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_metrics_snapshot() {
        let metrics = PulseMetrics::new();

        metrics.record_poll();
        metrics.record_budget_violation();

        let snapshot1 = metrics.snapshot();
        let snapshot2 = metrics.snapshot();

        assert_eq!(snapshot1.total_polls, snapshot2.total_polls);
        assert_eq!(snapshot1.budget_violations, snapshot2.budget_violations);
        assert_eq!(snapshot1.voluntary_yields, snapshot2.voluntary_yields);
        assert_eq!(snapshot1.tier_promotions, snapshot2.tier_promotions);
        assert_eq!(snapshot1.tier_demotions, snapshot2.tier_demotions);
        assert_eq!(snapshot1.slow_queue_enqueues, snapshot2.slow_queue_enqueues);
        assert_eq!(snapshot1.slow_queue_processed, snapshot2.slow_queue_processed);
        assert_eq!(snapshot1.total_cpu_time_us, snapshot2.total_cpu_time_us);
        assert_eq!(snapshot1.interventions_total, snapshot2.interventions_total);
        assert_eq!(snapshot1.config_updates, snapshot2.config_updates);

        assert_eq!(snapshot1.total_polls, 1);
        assert_eq!(snapshot1.budget_violations, 1);
        assert!(snapshot1.uptime_seconds >= 0.0);
        assert!(snapshot2.uptime_seconds >= snapshot1.uptime_seconds);
    }
}