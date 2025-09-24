//! Tokio-Pulse: Production-Ready Preemption System for Tokio
//!
//! This library provides a high-performance preemption system for the Tokio async runtime,
//! solving task starvation issues where CPU-bound tasks monopolize worker threads.
//!
//! # Features
//!
//! - **Task Budget Management**: Configurable operation budgets with <20ns overhead
//! - **Cross-Platform CPU Timing**: Accurate CPU time measurement across Linux/Windows/macOS
//! - **Graduated Intervention**: Multi-tier system from monitoring to isolation
//! - **Zero-Cost Abstraction**: No overhead when disabled via feature flags
//!
//! # Performance Guarantees
//!
//! - Per-poll overhead: <100ns (typically <50ns)
//! - Budget operations: <20ns atomic operations
//! - Memory footprint: 16 bytes per task
//! - Cache-aligned structures to prevent false sharing
//!
//! # Example
//!
//! ```rust
//! use tokio_pulse::budget::TaskBudget;
//! use std::task::Poll;
//!
//! // Create a budget for a task
//! let budget = TaskBudget::new(2000);
//!
//! // In your task's poll implementation
//! if budget.consume() {
//!     // Budget exhausted, yield control
//!     # let _example: Poll<()> =
//!     Poll::Pending
//!     # ;
//! }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![deny(unsafe_code)]

pub mod budget;
pub mod hooks;
pub mod tier_manager;
pub mod timing;

// Re-export commonly used types
pub use budget::TaskBudget;
pub use hooks::{HookRegistry, NullHooks, PreemptionHooks};
pub use tier_manager::{InterventionAction, PollResult, TaskContext, TaskId, TierConfig, TierManager, TierMetrics, TierPolicy};
pub use timing::{CpuTimer, TimingError, create_cpu_timer};
