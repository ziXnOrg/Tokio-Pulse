#![forbid(unsafe_code)]

/*
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */
/* Task budget management for preemptive yielding */
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

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
    deadline_ns: AtomicU64, /* CPU time deadline (ns) */

    remaining: AtomicU32, /* Operations before yield */

    tier: AtomicU8, /* Intervention tier (0-3) */

    _padding: [u8; 3], /* Cache alignment */
}

/* Compile-time size verification */
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

    /// Consumes one unit of budget.
    ///
    /// Returns `true` if budget is exhausted.
    #[inline]
    pub fn consume(&self) -> bool {
        let previous = self.remaining.fetch_sub(1, Ordering::Relaxed);
        previous <= 1
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
}
