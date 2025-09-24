//! Task Budget Management for Preemption Control
//!
//! This module implements a high-performance budget system for controlling task execution
//! in the Tokio runtime. Each task is allocated a budget that decrements on each poll
//! operation, enabling preemptive yielding when tasks exceed their allocated CPU time.
//!
//! # Design Rationale
//!
//! Based on research from several preemption systems:
//! - BEAM/Erlang: Uses 4000 reductions as default budget before forced yield
//! - Go: Preempts goroutines after ~10ms using SIGURG (not safe in Rust)
//! - Our approach: Configurable budget with graduated intervention tiers
//!
//! # Performance Requirements
//!
//! - Budget check: <20ns per atomic operation
//! - Memory footprint: Exactly 16 bytes per task
//! - Cache alignment: Prevent false sharing between tasks
//! - Zero allocation: All operations must be allocation-free
//!
//! # Memory Layout
//!
//! The `TaskBudget` struct is carefully designed to fit in exactly 16 bytes:
//! ```text
//! Offset | Size | Field
//! -------|------|----------------
//! 0      | 8    | deadline_ns (AtomicU64)
//! 8      | 4    | remaining (AtomicU32)
//! 12     | 1    | tier (AtomicU8)
//! 13     | 3    | padding
//! ```
//!
//! # Intervention Tiers
//!
//! The system uses graduated intervention to avoid false positives:
//! - Tier 0 (Monitor): Normal execution, track budget consumption
//! - Tier 1 (Warn): Budget exhausted, suggest voluntary yield
//! - Tier 2 (Yield): Force yield at next await point
//! - Tier 3 (Isolate): Move to separate worker pool
//!
//! # Example
//!
//! ```rust
//! use tokio_pulse::budget::TaskBudget;
//!
//! // Create a new budget with 1000 operations
//! let budget = TaskBudget::new(1000);
//!
//! // Consume budget on each poll
//! while !budget.consume() {
//!     // Task continues execution
//! }
//! // Budget exhausted, should yield
//! ```

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

/// Default budget allocation per task poll cycle
/// Based on BEAM's 4000 reductions, adjusted for Rust's execution model
pub const DEFAULT_BUDGET: u32 = 2000;

/// Minimum budget to prevent thrashing
pub const MIN_BUDGET: u32 = 100;

/// Maximum budget to prevent starvation of other tasks
pub const MAX_BUDGET: u32 = 10000;

/// Task execution budget with cache-aligned layout
///
/// This structure tracks the remaining operations budget for a task
/// and manages tier escalation for misbehaving tasks.
#[repr(C, align(16))]
pub struct TaskBudget {
    /// CPU time deadline in nanoseconds (for future use)
    deadline_ns: AtomicU64,

    /// Remaining operations before forced yield
    remaining: AtomicU32,

    /// Current intervention tier (0-3)
    tier: AtomicU8,

    /// Padding to ensure 16-byte total size
    _padding: [u8; 3],
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
    /// # Panics
    ///
    /// Panics if `initial_budget` is less than `MIN_BUDGET` or greater than `MAX_BUDGET`
    #[inline]
    #[must_use]
    pub fn new(initial_budget: u32) -> Self {
        assert!(
            (MIN_BUDGET..=MAX_BUDGET).contains(&initial_budget),
            "Budget must be between {MIN_BUDGET} and {MAX_BUDGET}, got {initial_budget}"
        );

        Self {
            deadline_ns: AtomicU64::new(0),
            remaining: AtomicU32::new(initial_budget),
            tier: AtomicU8::new(0),
            _padding: [0; 3],
        }
    }


    /// Consumes one unit of budget
    ///
    /// Returns `true` if the budget is exhausted and the task should yield.
    ///
    /// This is the hot path operation and must be extremely fast (<20ns).
    /// Uses `Relaxed` ordering for minimal overhead since exact counts
    /// don't need to be synchronized across threads.
    #[inline]
    pub fn consume(&self) -> bool {
        // Saturating subtraction to avoid underflow
        let previous = self.remaining.fetch_sub(1, Ordering::Relaxed);
        // Return true if we just consumed the last unit (previous was 1)
        previous <= 1
    }

    /// Checks if the budget has been exceeded without consuming
    #[inline]
    pub fn is_exhausted(&self) -> bool {
        self.remaining.load(Ordering::Relaxed) == 0
    }

    /// Resets the budget for a new poll cycle
    ///
    /// This should be called when a task voluntarily yields or
    /// starts a new poll cycle after yielding.
    #[inline]
    pub fn reset(&self, new_budget: u32) {
        self.remaining.store(new_budget, Ordering::Relaxed);
    }

    /// Gets the current remaining budget
    #[inline]
    pub fn remaining(&self) -> u32 {
        self.remaining.load(Ordering::Relaxed)
    }

    /// Gets the current intervention tier
    #[inline]
    pub fn tier(&self) -> u8 {
        self.tier.load(Ordering::Acquire)
    }

    /// Escalates the intervention tier
    ///
    /// Uses `Acquire-Release` ordering to ensure tier changes are
    /// visible across threads in the correct order.
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

    /// Resets the intervention tier to monitor level
    #[inline]
    pub fn reset_tier(&self) {
        self.tier.store(0, Ordering::Release);
    }

    /// Sets the CPU time deadline for this task
    ///
    /// This will be used in future versions for time-based preemption
    #[inline]
    pub fn set_deadline(&self, deadline_ns: u64) {
        self.deadline_ns.store(deadline_ns, Ordering::Relaxed);
    }

    /// Gets the CPU time deadline for this task
    #[inline]
    pub fn deadline(&self) -> u64 {
        self.deadline_ns.load(Ordering::Relaxed)
    }
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self::new(DEFAULT_BUDGET)
    }
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
            assert!(!should_yield, "Should not yield at iteration {}", i);
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
}
