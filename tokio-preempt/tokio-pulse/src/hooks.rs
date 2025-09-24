//! Runtime hooks for preemption control.

#![allow(unsafe_code)]
#![allow(clippy::inline_always)] /* Performance-critical */

use crate::tier_manager::{PollResult, TaskContext, TaskId};
use std::sync::atomic::{AtomicPtr, Ordering};
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

/// Type alias for Arc of `PreemptionHooks`
type HooksArc = Arc<dyn PreemptionHooks>;

/// Global hook registry for the runtime
///
/// This uses an atomic pointer to allow runtime installation and removal
/// of hooks without locks in the hot path.
pub struct HookRegistry {
    /// Pointer to the current hooks implementation (Arc<dyn PreemptionHooks>)
    hooks: AtomicPtr<HooksArc>,
}

impl HookRegistry {
    /// Creates a new registry with no hooks installed
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hooks: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    /// Installs a new set of hooks
    ///
    /// Returns the previously installed hooks, if any.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no tasks are currently being polled
    /// when hooks are changed, or that the hook implementation can
    /// handle concurrent calls during the transition.
    pub fn set_hooks(&self, hooks: Arc<dyn PreemptionHooks>) -> Option<Arc<dyn PreemptionHooks>> {
        let boxed = Box::new(hooks);
        let new_ptr = Box::into_raw(boxed);
        let old_ptr = self.hooks.swap(new_ptr, Ordering::AcqRel);

        if old_ptr.is_null() {
            None
        } else {
            // SAFETY: We know old_ptr came from Box::into_raw
            let boxed = unsafe { Box::from_raw(old_ptr) };
            Some(*boxed)
        }
    }

    /// Removes the current hooks
    ///
    /// Returns the previously installed hooks, if any.
    pub fn clear_hooks(&self) -> Option<Arc<dyn PreemptionHooks>> {
        let old_ptr = self.hooks.swap(std::ptr::null_mut(), Ordering::AcqRel);

        if old_ptr.is_null() {
            None
        } else {
            // SAFETY: We know old_ptr came from Box::into_raw
            let boxed = unsafe { Box::from_raw(old_ptr) };
            Some(*boxed)
        }
    }

    /// Calls the `before_poll` hook if hooks are installed
    #[inline(always)]
    pub fn before_poll(&self, task_id: TaskId, context: &TaskContext) {
        let hooks_ptr = self.hooks.load(Ordering::Acquire);
        if !hooks_ptr.is_null() {
            // SAFETY: We check for null and the pointer is valid while in use
            unsafe {
                let arc = &*hooks_ptr;
                arc.before_poll(task_id, context);
            }
        }
    }

    /// Calls the `after_poll` hook if hooks are installed
    #[inline(always)]
    pub fn after_poll(&self, task_id: TaskId, result: PollResult, duration: Duration) {
        let hooks_ptr = self.hooks.load(Ordering::Acquire);
        if !hooks_ptr.is_null() {
            // SAFETY: We check for null and the pointer is valid while in use
            unsafe {
                let arc = &*hooks_ptr;
                arc.after_poll(task_id, result, duration);
            }
        }
    }

    /// Calls the `on_yield` hook if hooks are installed
    #[inline(always)]
    pub fn on_yield(&self, task_id: TaskId) {
        let hooks_ptr = self.hooks.load(Ordering::Acquire);
        if !hooks_ptr.is_null() {
            // SAFETY: We check for null and the pointer is valid while in use
            unsafe {
                let arc = &*hooks_ptr;
                arc.on_yield(task_id);
            }
        }
    }

    /// Calls the `on_completion` hook if hooks are installed
    #[inline(always)]
    pub fn on_completion(&self, task_id: TaskId) {
        let hooks_ptr = self.hooks.load(Ordering::Acquire);
        if !hooks_ptr.is_null() {
            // SAFETY: We check for null and the pointer is valid while in use
            unsafe {
                let arc = &*hooks_ptr;
                arc.on_completion(task_id);
            }
        }
    }

    /// Checks if hooks are currently installed
    #[inline]
    pub fn has_hooks(&self) -> bool {
        !self.hooks.load(Ordering::Acquire).is_null()
    }
}

// SAFETY: HookRegistry can be safely shared between threads because:
// - AtomicPtr operations are thread-safe
// - The Arc<dyn PreemptionHooks> we store is Send + Sync
// - We only do atomic pointer swaps, no data races possible
unsafe impl Sync for HookRegistry {}
unsafe impl Send for HookRegistry {}

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
    use std::sync::atomic::AtomicU64;

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