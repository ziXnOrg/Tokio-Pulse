/* SAFETY: AtomicPtr requires unsafe for lock-free performance */
#![allow(unsafe_code)]
#![allow(clippy::inline_always)] /* Performance-critical */

/*
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
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};
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

/// Lock-free hook registry using `AtomicPtr` for minimal overhead
pub struct HookRegistry {
    hooks: AtomicPtr<Arc<dyn PreemptionHooks>>,
}

impl HookRegistry {
    /// Create new registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            hooks: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Install hooks, returning any previously set hooks
    pub fn set_hooks(&self, hooks: Arc<dyn PreemptionHooks>) -> Option<Arc<dyn PreemptionHooks>> {
        let new_ptr = Box::into_raw(Box::new(hooks));
        let old_ptr = self.hooks.swap(new_ptr, Ordering::AcqRel);

        if old_ptr.is_null() {
            None
        } else {
            /* SAFETY: We own the old pointer and convert it back to Arc */
            Some(unsafe { *Box::from_raw(old_ptr) })
        }
    }

    /// Remove hooks, returning the removed hooks if any
    pub fn clear_hooks(&self) -> Option<Arc<dyn PreemptionHooks>> {
        let old_ptr = self.hooks.swap(ptr::null_mut(), Ordering::AcqRel);

        if old_ptr.is_null() {
            None
        } else {
            /* SAFETY: We own the pointer and convert it back to Arc */
            Some(unsafe { *Box::from_raw(old_ptr) })
        }
    }

    /// Invoke `before_poll` hook if registered
    #[inline(always)]
    pub fn before_poll(&self, task_id: TaskId, context: &TaskContext) {
        let ptr = self.hooks.load(Ordering::Acquire);
        if !ptr.is_null() {
            /* SAFETY: Pointer is valid as long as registry exists */
            let hooks = unsafe { &*ptr };
            hooks.before_poll(task_id, context);
        }
    }

    /// Invoke `after_poll` hook if registered
    #[inline(always)]
    pub fn after_poll(&self, task_id: TaskId, result: PollResult, duration: Duration) {
        let ptr = self.hooks.load(Ordering::Acquire);
        if !ptr.is_null() {
            /* SAFETY: Pointer is valid as long as registry exists */
            let hooks = unsafe { &*ptr };
            hooks.after_poll(task_id, result, duration);
        }
    }

    /// Invoke `on_yield` hook if registered
    #[inline(always)]
    pub fn on_yield(&self, task_id: TaskId) {
        let ptr = self.hooks.load(Ordering::Acquire);
        if !ptr.is_null() {
            /* SAFETY: Pointer is valid as long as registry exists */
            let hooks = unsafe { &*ptr };
            hooks.on_yield(task_id);
        }
    }

    /// Invoke `on_completion` hook if registered
    #[inline(always)]
    pub fn on_completion(&self, task_id: TaskId) {
        let ptr = self.hooks.load(Ordering::Acquire);
        if !ptr.is_null() {
            /* SAFETY: Pointer is valid as long as registry exists */
            let hooks = unsafe { &*ptr };
            hooks.on_completion(task_id);
        }
    }

    /// Check if hooks are installed
    #[inline]
    pub fn has_hooks(&self) -> bool {
        !self.hooks.load(Ordering::Acquire).is_null()
    }
}

/* HookRegistry is Send + Sync via AtomicPtr */
/* SAFETY: Arc<dyn PreemptionHooks> is Send + Sync */
unsafe impl Send for HookRegistry {}
// SAFETY: HookRegistry only contains AtomicPtr<Arc<dyn PreemptionHooks>>, and Arc<dyn PreemptionHooks> is Sync
unsafe impl Sync for HookRegistry {}

impl Drop for HookRegistry {
    fn drop(&mut self) {
        /* Clean up any remaining hooks */
        let ptr = self.hooks.load(Ordering::Acquire);
        if !ptr.is_null() {
            /* SAFETY: We own the pointer and must free it */
            unsafe { drop(Box::from_raw(ptr)) }
        }
    }
}

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
    #[allow(clippy::struct_field_names)]
    struct CountingHooks {
        before_poll_calls: AtomicU64,
        after_poll_calls: AtomicU64,
        yield_calls: AtomicU64,
        completion_calls: AtomicU64,
    }

    impl CountingHooks {
        fn new() -> Self {
            Self {
                before_poll_calls: AtomicU64::new(0),
                after_poll_calls: AtomicU64::new(0),
                yield_calls: AtomicU64::new(0),
                completion_calls: AtomicU64::new(0),
            }
        }
    }

    impl PreemptionHooks for CountingHooks {
        fn before_poll(&self, _task_id: TaskId, _context: &TaskContext) {
            self.before_poll_calls.fetch_add(1, Ordering::Relaxed);
        }

        fn after_poll(&self, _task_id: TaskId, _result: PollResult, _duration: Duration) {
            self.after_poll_calls.fetch_add(1, Ordering::Relaxed);
        }

        fn on_yield(&self, _task_id: TaskId) {
            self.yield_calls.fetch_add(1, Ordering::Relaxed);
        }

        fn on_completion(&self, _task_id: TaskId) {
            self.completion_calls.fetch_add(1, Ordering::Relaxed);
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
        assert_eq!(hooks.before_poll_calls.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.after_poll_calls.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.yield_calls.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.completion_calls.load(Ordering::Relaxed), 1);
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
        assert_eq!(hooks1.before_poll_calls.load(Ordering::Relaxed), 0);
        assert_eq!(hooks2.before_poll_calls.load(Ordering::Relaxed), 1);
    }
}
