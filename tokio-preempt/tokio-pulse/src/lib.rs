//! Production-ready preemption system for Tokio async runtime
//!
//! This crate provides a multi-tier intervention system to prevent task starvation
//! in async applications by monitoring and controlling CPU-bound task execution.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
// Note: unsafe code is forbidden in all modules except timing modules where OS APIs require it

/**
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */

/* Preemption system for Tokio runtime */

pub mod budget;
pub mod hooks;
pub mod slow_queue;
pub mod tier_manager;
pub mod timing;

/* Public API exports */
pub use budget::TaskBudget;
pub use hooks::{HookRegistry, NullHooks, PreemptionHooks};
pub use tier_manager::{InterventionAction, PollResult, TaskContext, TaskId, TierConfig, TierManager, TierMetrics, TierPolicy};
pub use timing::{CpuTimer, TimingError, create_cpu_timer};
