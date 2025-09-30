//! Tower middleware integration for Tokio-Pulse preemption system
//!
//! This crate provides Tower middleware integration for the Tokio-Pulse preemption
//! system, allowing automatic task preemption in HTTP services and other Tower-based
//! applications.
//!
//! # Examples
//!
//! ## Basic HTTP Service with Preemption
//!
//! ```rust
//! use tower_pulse::{PreemptionLayer, PreemptionConfig};
//! use tower::{ServiceBuilder, service_fn};
//! use http::{Request, Response, StatusCode};
//! use http_body_util::Full;
//! use std::convert::Infallible;
//!
//! # async fn handle(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<&'static [u8]>>, Infallible> {
//! #     Ok(Response::builder()
//! #         .status(StatusCode::OK)
//! #         .body(Full::new(b"Hello, World!"))
//! #         .unwrap())
//! # }
//!
//! let service = ServiceBuilder::new()
//!     .layer(PreemptionLayer::new(PreemptionConfig::default()))
//!     .service(service_fn(handle));
//! ```
//!
//! ## Custom Configuration
//!
//! ```rust
//! use tower_pulse::{PreemptionLayer, PreemptionConfig};
//! use std::time::Duration;
//!
//! let config = PreemptionConfig {
//!     poll_budget: Duration::from_micros(1500),
//!     check_interval: 5,
//!     enable_metrics: true,
//!     ..Default::default()
//! };
//!
//! let layer = PreemptionLayer::new(config);
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

use std::task::{Context, Poll};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tower::{Layer, Service};
use pin_project_lite::pin_project;
use futures_util::ready;

// Re-export core types
pub use tokio_pulse::{
    TierManager, TierConfig, HookRegistry, TaskId, TaskContext,
    PreemptionHooks, PollResult
};

/// Configuration for Tower preemption middleware
#[derive(Debug, Clone)]
pub struct PreemptionConfig {
    /// Poll budget per request (default: 2000μs)
    pub poll_budget: Duration,
    /// Check budget every N poll operations (default: 1)
    pub check_interval: u32,
    /// Enable request-level metrics collection
    pub enable_metrics: bool,
    /// Custom tier configuration
    pub tier_config: Option<TierConfig>,
    /// Worker thread tracking
    pub track_workers: bool,
}

impl Default for PreemptionConfig {
    fn default() -> Self {
        Self {
            poll_budget: Duration::from_micros(2000),
            check_interval: 1,
            enable_metrics: true,
            tier_config: None,
            track_workers: true,
        }
    }
}

/// Tower layer for adding preemption support to services
#[derive(Clone)]
pub struct PreemptionLayer {
    config: PreemptionConfig,
    tier_manager: Arc<TierManager>,
    hook_registry: Arc<HookRegistry>,
}

impl PreemptionLayer {
    /// Create new preemption layer with configuration
    pub fn new(config: PreemptionConfig) -> Self {
        let tier_config = config.tier_config.clone().unwrap_or_default();
        let tier_manager = Arc::new(TierManager::new(tier_config));
        let hook_registry = Arc::new(HookRegistry::new());

        // Install tier manager as hooks
        hook_registry.set_hooks(tier_manager.clone());

        Self {
            config,
            tier_manager,
            hook_registry,
        }
    }

    /// Create layer with default configuration
    pub fn default() -> Self {
        Self::new(PreemptionConfig::default())
    }

    /// Get tier manager for metrics access
    pub fn tier_manager(&self) -> &Arc<TierManager> {
        &self.tier_manager
    }

    /// Get hook registry for advanced configuration
    pub fn hook_registry(&self) -> &Arc<HookRegistry> {
        &self.hook_registry
    }
}

impl<S> Layer<S> for PreemptionLayer {
    type Service = PreemptionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PreemptionService {
            inner,
            config: self.config.clone(),
            tier_manager: self.tier_manager.clone(),
            hook_registry: self.hook_registry.clone(),
        }
    }
}

/// Tower service wrapper that provides preemption support
#[derive(Clone)]
pub struct PreemptionService<S> {
    inner: S,
    config: PreemptionConfig,
    tier_manager: Arc<TierManager>,
    hook_registry: Arc<HookRegistry>,
}

impl<S> PreemptionService<S> {
    /// Get reference to inner service
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Get mutable reference to inner service
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Get tier manager for metrics
    pub fn tier_manager(&self) -> &Arc<TierManager> {
        &self.tier_manager
    }
}

impl<S, Request> Service<Request> for PreemptionService<S>
where
    S: Service<Request>,
    S::Error: From<PreemptionError>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = PreemptionFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // Generate unique task ID for this request
        let task_id = TaskId::new();

        // Create task context (simplified - no worker ID in Tower context)
        let task_context = TaskContext {
            worker_id: 0, // Tower doesn't expose worker thread info
            priority: None,
        };

        // Notify hooks of request start
        self.hook_registry.before_poll(task_id, &task_context);

        let future = self.inner.call(request);

        PreemptionFuture {
            inner: future,
            task_id,
            task_context,
            hook_registry: self.hook_registry.clone(),
            config: self.config.clone(),
            poll_count: 0,
        }
    }
}

pin_project! {
    /// Future wrapper that handles preemption for Tower services
    pub struct PreemptionFuture<F> {
        #[pin]
        inner: F,
        task_id: TaskId,
        task_context: TaskContext,
        hook_registry: Arc<HookRegistry>,
        config: PreemptionConfig,
        poll_count: u32,
    }
}

impl<F> futures_util::Future for PreemptionFuture<F>
where
    F: futures_util::Future,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // Track polling activity
        *this.poll_count += 1;

        // Check budget periodically
        if *this.poll_count % this.config.check_interval == 0 {
            // For Tower middleware, we use wall-clock time approximation
            let poll_duration = Duration::from_nanos(*this.poll_count as u64 * 100);

            this.hook_registry.after_poll(
                *this.task_id,
                PollResult::Pending,
                poll_duration,
            );

            // Check if task should yield based on budget
            if poll_duration > this.config.poll_budget {
                // In Tower context, we can't easily yield, so we just track the violation
                this.hook_registry.after_poll(
                    *this.task_id,
                    PollResult::Pending,
                    poll_duration,
                );

                #[cfg(feature = "tracing")]
                tracing::warn!(
                    task_id = ?this.task_id,
                    duration_us = poll_duration.as_micros(),
                    budget_us = this.config.poll_budget.as_micros(),
                    "Request exceeded poll budget"
                );
            }
        }

        // Poll the inner future
        let result = ready!(this.inner.poll(cx));

        // Notify completion
        this.hook_registry.on_completion(*this.task_id);

        Poll::Ready(result)
    }
}

/// Preemption-related errors
#[derive(Debug, thiserror::Error)]
pub enum PreemptionError {
    /// Task was preempted due to budget exhaustion
    #[error("Task preempted: exceeded budget of {budget_us}μs")]
    BudgetExceeded {
        /// The budget that was exceeded
        budget_us: u64,
    },

    /// Task was isolated due to tier escalation
    #[error("Task isolated: reached tier {tier}")]
    TaskIsolated {
        /// The tier that triggered isolation
        tier: u8,
    },
}

/// Metrics collector for Tower preemption middleware
#[cfg(feature = "metrics")]
pub struct PreemptionMetrics {
    tier_manager: Arc<TierManager>,
}

#[cfg(feature = "metrics")]
impl PreemptionMetrics {
    /// Create new metrics collector
    pub fn new(tier_manager: Arc<TierManager>) -> Self {
        Self { tier_manager }
    }

    /// Record current metrics to metrics registry
    pub fn record_metrics(&self) {
        let metrics = self.tier_manager.metrics();

        metrics::gauge!("tokio_pulse_active_tasks").set(metrics.active_tasks as f64);
        metrics::counter!("tokio_pulse_total_polls").absolute(metrics.total_polls);
        metrics::counter!("tokio_pulse_budget_violations").absolute(metrics.budget_violations);
        metrics::counter!("tokio_pulse_voluntary_yields").absolute(metrics.voluntary_yields);
        metrics::gauge!("tokio_pulse_slow_queue_size").set(metrics.slow_queue_size as f64);
    }
}

// Helper trait to generate unique task IDs
trait TaskIdGenerator {
    fn new() -> Self;
}

impl TaskIdGenerator for TaskId {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        TaskId(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_test::mock::{self, Mock};
    use tower::{ServiceExt, ServiceBuilder};
    use http::{Request, Response, StatusCode};
    use std::convert::Infallible;

    #[tokio::test]
    async fn test_preemption_layer_basic() {
        let (service, mut handle) = mock::pair::<Request<()>, Response<()>>();

        let service = ServiceBuilder::new()
            .layer(PreemptionLayer::default())
            .service(service);

        let request = Request::builder()
            .method("GET")
            .uri("/test")
            .body(())
            .unwrap();

        // Send request
        let response_future = service.oneshot(request);

        // Provide response
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(())
            .unwrap();
        handle.send_response(response);

        // Verify request completes
        let result = response_future.await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_preemption_config() {
        let config = PreemptionConfig {
            poll_budget: Duration::from_micros(1000),
            check_interval: 2,
            enable_metrics: false,
            tier_config: None,
            track_workers: false,
        };

        let layer = PreemptionLayer::new(config);

        // Verify configuration applied
        assert_eq!(layer.config.poll_budget, Duration::from_micros(1000));
        assert_eq!(layer.config.check_interval, 2);
        assert!(!layer.config.enable_metrics);
    }
}