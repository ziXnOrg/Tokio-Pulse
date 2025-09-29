//! Task budget management for preemptive yielding
//!
//! This module provides the core budget tracking mechanism that controls
//! when tasks should yield control back to the scheduler. The budget system
//! uses atomic operations for lock-free access and provides memory pooling
//! for efficient allocation.

#![forbid(unsafe_code)]

//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam::queue::SegQueue;

#[cfg(feature = "tracing")]
use tracing::debug;

/// Default budget allocation per task poll cycle.
pub const DEFAULT_BUDGET: u32 = 2000;

/// Minimum budget to prevent thrashing.
pub const MIN_BUDGET: u32 = 100;

/// Maximum budget to prevent starvation.
pub const MAX_BUDGET: u32 = 10000;

/// Task execution budget with cache-aligned layout.
#[repr(C, align(16))]
#[derive(Debug)]
pub struct TaskBudget {
    deadline_ns: AtomicU64, // CPU time deadline (ns)

    remaining: AtomicU32, // Operations before yield

    tier: AtomicU8, // Intervention tier (0-3)

    generation: AtomicU8, // Pool generation counter

    _padding: [u8; 2], // Cache alignment
}

// Compile-time size verification
const _: () = {
    assert!(std::mem::size_of::<TaskBudget>() == 16);
    assert!(std::mem::align_of::<TaskBudget>() == 16);
};

impl TaskBudget {
    /// Creates a new task budget with the specified initial allocation
    ///
    /// # Arguments
    ///
    /// * `initial_budget` - Number of operations allowed before yielding
    ///
    /// # Examples
    ///
    /// ```
    /// use tokio_pulse::budget::{TaskBudget, DEFAULT_BUDGET};
    ///
    /// let budget = TaskBudget::new(DEFAULT_BUDGET);
    /// assert_eq!(budget.remaining(), DEFAULT_BUDGET);
    /// assert_eq!(budget.tier(), 0);
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `initial_budget` is less than `MIN_BUDGET` or greater than
    /// `MAX_BUDGET`
    #[inline]
    #[must_use]
    pub fn new(initial_budget: u32) -> Self {
        assert!(
            (MIN_BUDGET..=MAX_BUDGET).contains(&initial_budget),
            "Budget must be between {MIN_BUDGET} and {MAX_BUDGET}, got \
             {initial_budget}"
        );

        Self {
            deadline_ns: AtomicU64::new(0),
            remaining: AtomicU32::new(initial_budget),
            tier: AtomicU8::new(0),
            generation: AtomicU8::new(0),
            _padding: [0; 2],
        }
    }

    /// Consumes one unit of budget.
    ///
    /// Returns `true` if budget is exhausted.
    #[inline]
    pub fn consume(&self) -> bool {
        let previous = self.remaining.fetch_sub(1, Ordering::Relaxed);
        let exhausted = previous <= 1;

        #[cfg(feature = "tracing")]
        if exhausted {
            debug!(remaining = previous.saturating_sub(1), "Task budget exhausted");
        }

        exhausted
    }

    /// Checks if budget is exhausted without consuming.
    #[inline]
    pub fn is_exhausted(&self) -> bool {
        self.remaining.load(Ordering::Relaxed) == 0
    }

    /// Resets budget for new poll cycle.
    #[inline]
    pub fn reset(&self, new_budget: u32) {
        self.remaining.store(new_budget, Ordering::Relaxed);

        #[cfg(feature = "tracing")]
        debug!(budget = new_budget, "Task budget reset");
    }

    /// Returns remaining budget.
    #[inline]
    pub fn remaining(&self) -> u32 {
        self.remaining.load(Ordering::Relaxed)
    }

    /// Returns current tier.
    #[inline]
    pub fn tier(&self) -> u8 {
        self.tier.load(Ordering::Acquire)
    }

    /// Escalates intervention tier.
    #[inline]
    pub fn escalate_tier(&self) -> u8 {
        let current = self.tier.load(Ordering::Acquire);
        if current < 3 {
            let new_tier = current + 1;
            self.tier.store(new_tier, Ordering::Release);
            new_tier
        } else {
            current
        }
    }

    /// Resets tier to monitor level.
    #[inline]
    pub fn reset_tier(&self) {
        self.tier.store(0, Ordering::Release);
    }

    /// Sets CPU time deadline.
    #[inline]
    pub fn set_deadline(&self, deadline_ns: u64) {
        self.deadline_ns.store(deadline_ns, Ordering::Relaxed);
    }

    /// Returns CPU time deadline.
    #[inline]
    pub fn deadline(&self) -> u64 {
        self.deadline_ns.load(Ordering::Relaxed)
    }

    /// Resets all fields for pool reuse.
    #[inline]
    pub fn reset_all(&self, new_budget: u32) {
        self.remaining.store(new_budget, Ordering::Relaxed);
        self.deadline_ns.store(0, Ordering::Relaxed);
        self.tier.store(0, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Returns current generation.
    #[inline]
    pub fn generation(&self) -> u8 {
        self.generation.load(Ordering::Acquire)
    }
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self::new(DEFAULT_BUDGET)
    }
}

/// Pool configuration for TaskBudget instances
#[derive(Debug, Clone)]
pub struct BudgetPoolConfig {
    /// Initial pool size
    pub initial_size: usize,
    /// Maximum pool size
    pub max_size: usize,
    /// High watermark for pool shrinking
    pub high_watermark: usize,
    /// Low watermark for pool shrinking
    pub low_watermark: usize,
}

impl Default for BudgetPoolConfig {
    fn default() -> Self {
        Self {
            initial_size: 64,
            max_size: 256,
            high_watermark: 200,
            low_watermark: 32,
        }
    }
}

/// Statistics for budget pool operations
#[derive(Debug, Default)]
pub struct BudgetPoolStats {
    /// Total budgets acquired from pool
    pub pool_hits: AtomicU64,
    /// Total budgets allocated directly
    pub pool_misses: AtomicU64,
    /// Total budgets returned to pool
    pub pool_returns: AtomicU64,
    /// Total budgets discarded (pool full)
    pub pool_discards: AtomicU64,
    /// Current pool size
    pub current_size: AtomicU64,
}

impl BudgetPoolStats {
    /// Get snapshot of current statistics
    pub fn snapshot(&self) -> BudgetPoolStatsSnapshot {
        BudgetPoolStatsSnapshot {
            pool_hits: self.pool_hits.load(Ordering::Relaxed),
            pool_misses: self.pool_misses.load(Ordering::Relaxed),
            pool_returns: self.pool_returns.load(Ordering::Relaxed),
            pool_discards: self.pool_discards.load(Ordering::Relaxed),
            current_size: self.current_size.load(Ordering::Relaxed),
        }
    }

    /// Calculate pool hit rate (0.0 to 1.0)
    pub fn hit_rate(&self) -> f64 {
        let hits = self.pool_hits.load(Ordering::Relaxed);
        let misses = self.pool_misses.load(Ordering::Relaxed);
        let total = hits + misses;

        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }
}

/// Snapshot of budget pool statistics
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetPoolStatsSnapshot {
    /// Total budgets acquired from pool
    pub pool_hits: u64,
    /// Total budgets allocated directly
    pub pool_misses: u64,
    /// Total budgets returned to pool
    pub pool_returns: u64,
    /// Total budgets discarded (pool full)
    pub pool_discards: u64,
    /// Current pool size
    pub current_size: u64,
}

/// Thread-local pool for TaskBudget instances
pub struct BudgetPool {
    /// Lock-free queue of pooled budgets
    budgets: SegQueue<Arc<TaskBudget>>,
    /// Pool configuration
    config: BudgetPoolConfig,
    /// Pool statistics
    stats: BudgetPoolStats,
}

impl BudgetPool {
    /// Create new budget pool with configuration
    pub fn new(config: BudgetPoolConfig) -> Self {
        let pool = Self {
            budgets: SegQueue::new(),
            config,
            stats: BudgetPoolStats::default(),
        };

        pool.warm_up();
        pool
    }

    /// Pre-allocate pool entries
    fn warm_up(&self) {
        for _ in 0..self.config.initial_size {
            let budget = Arc::new(TaskBudget::new(DEFAULT_BUDGET));
            self.budgets.push(budget);
        }
        self.stats.current_size.store(self.config.initial_size as u64, Ordering::Relaxed);
    }

    /// Acquire budget from pool or allocate new
    pub fn acquire(&self, initial_budget: u32) -> Arc<TaskBudget> {
        if let Some(budget) = self.budgets.pop() {
            budget.reset_all(initial_budget);
            self.stats.pool_hits.fetch_add(1, Ordering::Relaxed);
            self.stats.current_size.fetch_sub(1, Ordering::Relaxed);

            #[cfg(feature = "tracing")]
            debug!(budget = initial_budget, "Budget acquired from pool");

            budget
        } else {
            self.stats.pool_misses.fetch_add(1, Ordering::Relaxed);

            #[cfg(feature = "tracing")]
            debug!(budget = initial_budget, "Budget allocated (pool empty)");

            Arc::new(TaskBudget::new(initial_budget))
        }
    }

    /// Return budget to pool
    pub fn release(&self, budget: Arc<TaskBudget>) {
        let current_size = self.stats.current_size.load(Ordering::Relaxed) as usize;

        if current_size < self.config.max_size {
            self.budgets.push(budget);
            self.stats.pool_returns.fetch_add(1, Ordering::Relaxed);
            self.stats.current_size.fetch_add(1, Ordering::Relaxed);

            #[cfg(feature = "tracing")]
            debug!("Budget returned to pool");
        } else {
            self.stats.pool_discards.fetch_add(1, Ordering::Relaxed);

            #[cfg(feature = "tracing")]
            debug!("Budget discarded (pool full)");
        }
    }

    /// Get pool statistics
    pub fn stats(&self) -> BudgetPoolStatsSnapshot {
        self.stats.snapshot()
    }

    /// Get current pool size
    pub fn size(&self) -> usize {
        self.stats.current_size.load(Ordering::Relaxed) as usize
    }

    /// Check if pool needs shrinking under memory pressure
    pub fn should_shrink(&self, memory_pressure: f32) -> bool {
        let current_size = self.size();
        let threshold = if memory_pressure > 0.8 {
            self.config.low_watermark
        } else if memory_pressure > 0.6 {
            self.config.high_watermark / 2
        } else {
            self.config.high_watermark
        };

        current_size > threshold
    }

    /// Shrink pool under memory pressure
    pub fn shrink(&self, target_size: usize) -> usize {
        let mut shrunk = 0;
        while self.size() > target_size && self.budgets.pop().is_some() {
            shrunk += 1;
            self.stats.current_size.fetch_sub(1, Ordering::Relaxed);
        }

        #[cfg(feature = "tracing")]
        if shrunk > 0 {
            debug!(shrunk = shrunk, target = target_size, "Pool shrunk due to memory pressure");
        }

        shrunk
    }
}

impl Default for BudgetPool {
    fn default() -> Self {
        Self::new(BudgetPoolConfig::default())
    }
}

/// Global budget pool for cross-thread access
static GLOBAL_BUDGET_POOL: std::sync::LazyLock<BudgetPool> =
    std::sync::LazyLock::new(|| BudgetPool::new(BudgetPoolConfig::default()));

thread_local! {
    static THREAD_LOCAL_POOL: BudgetPool = BudgetPool::new(BudgetPoolConfig::default());
}

/// Acquire budget from thread-local pool with global fallback
///
/// This function provides the primary interface for obtaining TaskBudget
/// instances from the efficient thread-local pool system.
///
/// # Arguments
///
/// * `initial_budget` - Number of operations allowed before yielding
///
/// # Examples
///
/// ```no_run
/// use tokio_pulse::budget::{acquire_budget, release_budget, DEFAULT_BUDGET};
///
/// let budget = acquire_budget(DEFAULT_BUDGET);
/// assert_eq!(budget.remaining(), DEFAULT_BUDGET);
///
/// // Use the budget for task processing
/// budget.consume();
///
/// // Return to pool when done
/// release_budget(budget);
/// ```
pub fn acquire_budget(initial_budget: u32) -> Arc<TaskBudget> {
    THREAD_LOCAL_POOL.with(|pool| pool.acquire(initial_budget))
}

/// Release budget to thread-local pool
///
/// Returns a TaskBudget instance to the thread-local pool for reuse.
/// If the pool is full, the budget is discarded to prevent unbounded
/// memory growth.
///
/// # Arguments
///
/// * `budget` - TaskBudget instance to return to pool
///
/// # Examples
///
/// ```no_run
/// use tokio_pulse::budget::{acquire_budget, release_budget, DEFAULT_BUDGET};
///
/// let budget = acquire_budget(DEFAULT_BUDGET);
/// // ... use budget ...
/// release_budget(budget);
/// ```
pub fn release_budget(budget: Arc<TaskBudget>) {
    THREAD_LOCAL_POOL.with(|pool| pool.release(budget));
}

/// Get global pool statistics
pub fn global_pool_stats() -> BudgetPoolStatsSnapshot {
    GLOBAL_BUDGET_POOL.stats()
}

/// Get thread-local pool statistics
pub fn thread_local_pool_stats() -> Option<BudgetPoolStatsSnapshot> {
    THREAD_LOCAL_POOL.try_with(|pool| pool.stats()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_size_and_alignment() {
        assert_eq!(std::mem::size_of::<TaskBudget>(), 16);
        assert_eq!(std::mem::align_of::<TaskBudget>(), 16);
    }

    #[test]
    fn test_budget_consumption() {
        let budget = TaskBudget::new(MIN_BUDGET);

        // Consume most of the budget (all but last)
        for i in 0..MIN_BUDGET - 1 {
            let should_yield = budget.consume();
            assert!(!should_yield, "Should not yield at iteration {i}");
        }

        // Last consumption should indicate exhaustion
        assert!(budget.consume(), "Should yield when budget exhausted");
        assert!(budget.is_exhausted());
    }

    #[test]
    fn test_budget_reset() {
        let budget = TaskBudget::new(100);

        // Exhaust budget
        for _ in 0..100 {
            budget.consume();
        }
        assert!(budget.is_exhausted());

        // Reset budget
        budget.reset(200);
        assert_eq!(budget.remaining(), 200);
        assert!(!budget.is_exhausted());
    }

    #[test]
    fn test_tier_escalation() {
        let budget = TaskBudget::new(1000);

        assert_eq!(budget.tier(), 0);

        // Escalate through tiers
        assert_eq!(budget.escalate_tier(), 1);
        assert_eq!(budget.tier(), 1);

        assert_eq!(budget.escalate_tier(), 2);
        assert_eq!(budget.tier(), 2);

        assert_eq!(budget.escalate_tier(), 3);
        assert_eq!(budget.tier(), 3);

        // Should not escalate beyond tier 3
        assert_eq!(budget.escalate_tier(), 3);
        assert_eq!(budget.tier(), 3);

        // Reset tier
        budget.reset_tier();
        assert_eq!(budget.tier(), 0);
    }

    #[test]
    #[should_panic(expected = "Budget must be between")]
    fn test_invalid_budget_too_low() {
        let _ = TaskBudget::new(50); // Below MIN_BUDGET
    }

    #[test]
    #[should_panic(expected = "Budget must be between")]
    fn test_invalid_budget_too_high() {
        let _ = TaskBudget::new(20000); // Above MAX_BUDGET
    }

    #[test]
    fn test_budget_pool_basic_operations() {
        let pool = BudgetPool::default();

        // Pool should start with initial size
        assert_eq!(pool.size(), 64); // Default initial_size

        // Acquire a budget
        let budget = pool.acquire(1000);
        assert_eq!(budget.remaining(), 1000);
        assert_eq!(pool.size(), 63); // Should decrease

        // Return the budget
        pool.release(budget);
        assert_eq!(pool.size(), 64); // Should increase back
    }

    #[test]
    fn test_budget_pool_warm_up() {
        let config = BudgetPoolConfig {
            initial_size: 10,
            max_size: 20,
            high_watermark: 15,
            low_watermark: 5,
        };
        let pool = BudgetPool::new(config);

        // Should start with initial size
        assert_eq!(pool.size(), 10);

        // Stats should reflect initial state
        let stats = pool.stats();
        assert_eq!(stats.current_size, 10);
        assert_eq!(stats.pool_hits, 0);
        assert_eq!(stats.pool_misses, 0);
    }

    #[test]
    fn test_budget_pool_acquire_miss() {
        let config = BudgetPoolConfig {
            initial_size: 0, // Start empty
            max_size: 10,
            high_watermark: 8,
            low_watermark: 2,
        };
        let pool = BudgetPool::new(config);

        // Should start empty
        assert_eq!(pool.size(), 0);

        // Acquire should create new budget (miss)
        let budget = pool.acquire(500);
        assert_eq!(budget.remaining(), 500);

        let stats = pool.stats();
        assert_eq!(stats.pool_misses, 1);
        assert_eq!(stats.pool_hits, 0);
    }

    #[test]
    fn test_budget_pool_overflow() {
        let config = BudgetPoolConfig {
            initial_size: 2,
            max_size: 2, // Small max size
            high_watermark: 2,
            low_watermark: 1,
        };
        let pool = BudgetPool::new(config);

        // Acquire all budgets
        let budget1 = pool.acquire(100);
        let budget2 = pool.acquire(200);
        assert_eq!(pool.size(), 0);

        // Return both - should fill the pool to max capacity
        pool.release(budget1);
        assert_eq!(pool.size(), 1);
        pool.release(budget2);
        assert_eq!(pool.size(), 2);

        // Create another budget and try to return when pool is full - should be discarded
        let budget3 = pool.acquire(300); // This removes one from pool
        assert_eq!(pool.size(), 1);

        let budget4 = pool.acquire(400); // This removes another from pool
        assert_eq!(pool.size(), 0);

        // Return first budget
        pool.release(budget3);
        assert_eq!(pool.size(), 1);

        // Return second budget
        pool.release(budget4);
        assert_eq!(pool.size(), 2);

        // Now pool is full. Create new budget and try to return - should be discarded
        let extra_budget = Arc::new(TaskBudget::new(500));
        pool.release(extra_budget); // Try to return when pool is full - should be discarded

        let stats = pool.stats();
        assert!(stats.pool_discards > 0);
    }

    #[test]
    fn test_budget_pool_reset_all() {
        let config = BudgetPoolConfig {
            initial_size: 1, // Only one budget to ensure we get the same one back
            max_size: 10,
            high_watermark: 8,
            low_watermark: 2,
        };
        let pool = BudgetPool::new(config);

        let budget = pool.acquire(1000);

        // Modify budget state
        budget.consume();
        budget.escalate_tier();
        budget.set_deadline(12345);

        // Values should be modified
        assert_eq!(budget.remaining(), 999);
        assert_eq!(budget.tier(), 1);
        assert_eq!(budget.deadline(), 12345);
        let old_generation = budget.generation();

        // Return and re-acquire - should get the same budget back reset
        pool.release(Arc::clone(&budget));
        let new_budget = pool.acquire(2000);

        // Should be reset (budget was reset during acquire)
        assert_eq!(new_budget.remaining(), 2000);
        assert_eq!(new_budget.tier(), 0);
        assert_eq!(new_budget.deadline(), 0);
        // Generation should be incremented due to reset_all call during acquire
        assert_eq!(new_budget.generation(), old_generation + 1);
    }

    #[test]
    fn test_budget_pool_memory_pressure() {
        let pool = BudgetPool::default();

        // Normal conditions - should not shrink
        assert!(!pool.should_shrink(0.3));

        // Medium pressure - should check high watermark
        assert!(!pool.should_shrink(0.7)); // Pool size 64 < high_watermark 200

        // High pressure - should check low watermark
        assert!(pool.should_shrink(0.9)); // Pool size 64 > low_watermark 32
    }

    #[test]
    fn test_budget_pool_shrink() {
        let pool = BudgetPool::default();
        let initial_size = pool.size();

        // Shrink to half size
        let target = initial_size / 2;
        let shrunk = pool.shrink(target);

        assert_eq!(shrunk, initial_size - target);
        assert_eq!(pool.size(), target);
    }

    #[test]
    fn test_global_pool_functions() {
        // Test global helper functions
        let budget1 = acquire_budget(1500);
        assert_eq!(budget1.remaining(), 1500);

        let budget2 = acquire_budget(2500);
        assert_eq!(budget2.remaining(), 2500);

        // Release budgets
        release_budget(budget1);
        release_budget(budget2);

        // Should be able to get stats
        let _stats = global_pool_stats();
        let _local_stats = thread_local_pool_stats();
    }

    #[test]
    fn test_budget_pool_stats() {
        let pool = BudgetPool::default();

        // Acquire some budgets
        let budget1 = pool.acquire(100);
        let budget2 = pool.acquire(200);

        let stats = pool.stats();
        assert_eq!(stats.pool_hits, 2);
        assert_eq!(stats.pool_misses, 0);
        assert_eq!(stats.pool_returns, 0);

        // Return budgets
        pool.release(budget1);
        pool.release(budget2);

        let stats = pool.stats();
        assert_eq!(stats.pool_returns, 2);
        assert_eq!(stats.pool_discards, 0);
    }
}
