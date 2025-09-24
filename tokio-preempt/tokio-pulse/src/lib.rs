//! Preemption system for Tokio runtime.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![deny(unsafe_code)]

pub mod budget;
pub mod hooks;
pub mod tier_manager;
pub mod timing;

/* Public API exports */
pub use budget::TaskBudget;
pub use hooks::{HookRegistry, NullHooks, PreemptionHooks};
pub use tier_manager::{InterventionAction, PollResult, TaskContext, TaskId, TierConfig, TierManager, TierMetrics, TierPolicy};
pub use timing::{CpuTimer, TimingError, create_cpu_timer};
