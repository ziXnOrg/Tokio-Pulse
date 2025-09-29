//! Production-ready preemption system for Tokio async runtime
//!
//! This crate provides a multi-tier intervention system to prevent task starvation
//! in async applications by monitoring and controlling CPU-bound task execution.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
// Preemption system for Tokio runtime
/// Budget management for task preemption
pub mod budget;
/// Hook system for runtime instrumentation
pub mod hooks;
/// Task isolation mechanisms
pub mod isolation;
/// Metrics collection and reporting
pub mod metrics;
/// Queue management for slow tasks
pub mod slow_queue;
/// Dedicated thread reschedule mechanism
pub mod reschedule;
/// Multi-tier task management system
pub mod tier_manager;
/// Cross-platform CPU timing utilities
pub mod timing;

// Public API exports
pub use budget::TaskBudget;
pub use hooks::{HookRegistry, NullHooks, PreemptionHooks};
pub use isolation::{IsolationError, TaskIsolation};
pub use reschedule::{
    DedicatedThreadScheduler, RescheduleError, RescheduleTask, ThreadSchedulerConfig,
    ThreadSchedulerMetrics, ThreadSchedulerMetricsSnapshot,
};
pub use tier_manager::{
    ConfigProfile, InterventionAction, PollResult, TaskContext, TaskId, TierConfig, TierManager,
    TierMetrics, TierPolicy, TierPolicyBuilder,
};
pub use timing::{CpuTimer, TimingError, create_cpu_timer};
