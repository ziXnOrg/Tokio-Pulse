#![forbid(unsafe_code)]
#![allow(clippy::inline_always)] /* Performance-critical */

/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */

/* Runtime hooks for preemption control */

use crate::tier_manager::{PollResult, TaskContext, TaskId};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;

/// Preemption hook interface.
pub trait PreemptionHooks: Send + Sync {
    /// Called before polling a task.
    fn before_poll(&self, task_id: TaskId, context: &TaskContext);

    /// Called after polling a task.
    fn after_poll(&self, task_id: TaskId, result: PollResult, duration: Duration);

    /// Called when a task voluntarily yields.
    fn on_yield(&self, task_id: TaskId);

    /// Called when a task completes.
    fn on_completion(&self, task_id: TaskId);
}

/// Null implementation of `PreemptionHooks` for when preemption is disabled
#[derive(Debug, Default)]
pub struct NullHooks;

impl PreemptionHooks for NullHooks {
    #[inline(always)]
    fn before_poll(&self, _task_id: TaskId, _context: &TaskContext) {
        // No-op
    }

    #[inline(always)]
    fn after_poll(&self, _task_id: TaskId, _result: PollResult, _duration: Duration) {
        // No-op
    }

    #[inline(always)]
    fn on_yield(&self, _task_id: TaskId) {
        // No-op
    }

    #[inline(always)]
    fn on_completion(&self, _task_id: TaskId) {
        // No-op
    }
}

/* Hook registry using RwLock for safe concurrent access */
pub struct HookRegistry {
    hooks: Arc<RwLock<Option<Arc<dyn PreemptionHooks>>>>,
}

impl HookRegistry {
    /* Create new registry */
    #[must_use]
    pub fn new() -> Self {
        Self {
            hooks: Arc::new(RwLock::new(None)),
        }
    }

    /* Install hooks */
    pub fn set_hooks(&self, hooks: Arc<dyn PreemptionHooks>) -> Option<Arc<dyn PreemptionHooks>> {
        self.hooks.write().replace(hooks)
    }

    /* Remove hooks */
    pub fn clear_hooks(&self) -> Option<Arc<dyn PreemptionHooks>> {
        self.hooks.write().take()
    }

    /* Pre-poll hook */
    #[inline(always)]
    pub fn before_poll(&self, task_id: TaskId, context: &TaskContext) {
        if let Some(hooks) = self.hooks.read().as_ref() {
            hooks.before_poll(task_id, context);
        }
    }

    /* Post-poll hook */
    #[inline(always)]
    pub fn after_poll(&self, task_id: TaskId, result: PollResult, duration: Duration) {
        if let Some(hooks) = self.hooks.read().as_ref() {
            hooks.after_poll(task_id, result, duration);
        }
    }

    /* Yield hook */
    #[inline(always)]
    pub fn on_yield(&self, task_id: TaskId) {
        if let Some(hooks) = self.hooks.read().as_ref() {
            hooks.on_yield(task_id);
        }
    }

    /* Completion hook */
    #[inline(always)]
    pub fn on_completion(&self, task_id: TaskId) {
        if let Some(hooks) = self.hooks.read().as_ref() {
            hooks.on_completion(task_id);
        }
    }

    /* Check if hooks installed */
    #[inline]
    pub fn has_hooks(&self) -> bool {
        self.hooks.read().is_some()
    }
}

/* HookRegistry is Send + Sync via RwLock */

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Adapter to use `TierManager` as `PreemptionHooks`
impl PreemptionHooks for crate::tier_manager::TierManager {
    #[inline]
    fn before_poll(&self, task_id: TaskId, context: &TaskContext) {
        self.before_poll(task_id, context);
    }

    #[inline]
    fn after_poll(&self, task_id: TaskId, result: PollResult, duration: Duration) {
        self.after_poll(task_id, result, duration);
    }

    #[inline]
    fn on_yield(&self, task_id: TaskId) {
        self.on_yield(task_id);
    }

    #[inline]
    fn on_completion(&self, task_id: TaskId) {
        self.on_completion(task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Test implementation that counts hook calls
    struct CountingHooks {
        before_poll_count: AtomicU64,
        after_poll_count: AtomicU64,
        on_yield_count: AtomicU64,
        on_completion_count: AtomicU64,
    }

    impl CountingHooks {
        fn new() -> Self {
            Self {
                before_poll_count: AtomicU64::new(0),
                after_poll_count: AtomicU64::new(0),
                on_yield_count: AtomicU64::new(0),
                on_completion_count: AtomicU64::new(0),
            }
        }
    }

    impl PreemptionHooks for CountingHooks {
        fn before_poll(&self, _task_id: TaskId, _context: &TaskContext) {
            self.before_poll_count.fetch_add(1, Ordering::Relaxed);
        }

        fn after_poll(&self, _task_id: TaskId, _result: PollResult, _duration: Duration) {
            self.after_poll_count.fetch_add(1, Ordering::Relaxed);
        }

        fn on_yield(&self, _task_id: TaskId) {
            self.on_yield_count.fetch_add(1, Ordering::Relaxed);
        }

        fn on_completion(&self, _task_id: TaskId) {
            self.on_completion_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_null_hooks() {
        let hooks = NullHooks;
        let task_id = TaskId(1);
        let context = TaskContext {
            worker_id: 0,
            priority: None,
        };

        // All methods should be no-ops
        hooks.before_poll(task_id, &context);
        hooks.after_poll(task_id, PollResult::Pending, Duration::from_secs(0));
        hooks.on_yield(task_id);
        hooks.on_completion(task_id);
    }

    #[test]
    fn test_hook_registry_no_hooks() {
        let registry = HookRegistry::new();
        assert!(!registry.has_hooks());

        let task_id = TaskId(1);
        let context = TaskContext {
            worker_id: 0,
            priority: None,
        };

        // Should not panic when no hooks installed
        registry.before_poll(task_id, &context);
        registry.after_poll(task_id, PollResult::Ready, Duration::from_secs(0));
        registry.on_yield(task_id);
        registry.on_completion(task_id);
    }

    #[test]
    fn test_hook_registry_with_hooks() {
        let registry = HookRegistry::new();
        let hooks = Arc::new(CountingHooks::new());
        let hooks_clone = Arc::clone(&hooks);

        // Install hooks
        let old = registry.set_hooks(hooks_clone);
        assert!(old.is_none());
        assert!(registry.has_hooks());

        let task_id = TaskId(1);
        let context = TaskContext {
            worker_id: 0,
            priority: None,
        };

        // Call hooks
        registry.before_poll(task_id, &context);
        registry.after_poll(task_id, PollResult::Pending, Duration::from_secs(0));
        registry.on_yield(task_id);
        registry.on_completion(task_id);

        // Verify counts
        assert_eq!(hooks.before_poll_count.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.after_poll_count.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.on_yield_count.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.on_completion_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_hook_registry_clear() {
        let registry = HookRegistry::new();
        let hooks = Arc::new(CountingHooks::new());

        // Install and then clear
        registry.set_hooks(hooks);
        assert!(registry.has_hooks());

        let old = registry.clear_hooks();
        assert!(old.is_some());
        assert!(!registry.has_hooks());

        // Clearing again should return None
        let old2 = registry.clear_hooks();
        assert!(old2.is_none());
    }

    #[test]
    fn test_hook_registry_replace() {
        let registry = HookRegistry::new();
        let hooks1 = Arc::new(CountingHooks::new());
        let hooks2 = Arc::new(CountingHooks::new());

        // Install first hooks
        let old = registry.set_hooks(hooks1.clone() as Arc<dyn PreemptionHooks>);
        assert!(old.is_none());

        // Replace with second hooks
        let old = registry.set_hooks(hooks2.clone() as Arc<dyn PreemptionHooks>);
        assert!(old.is_some());

        let task_id = TaskId(1);
        let context = TaskContext {
            worker_id: 0,
            priority: None,
        };

        // Call should go to hooks2
        registry.before_poll(task_id, &context);
        assert_eq!(hooks1.before_poll_count.load(Ordering::Relaxed), 0);
        assert_eq!(hooks2.before_poll_count.load(Ordering::Relaxed), 1);
    }
}