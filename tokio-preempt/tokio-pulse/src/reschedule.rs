//     ______   __  __     __         ______     ______
//    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
//    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
//     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
//      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
//
// Author: Colin MacRitchie / Ripple Group
//! Dedicated thread reschedule mechanism for slow tasks
//!
//! This module provides a dedicated thread pool for processing tasks that have
//! been moved to the slow queue, ensuring they don't interfere with the main
//! Tokio runtime while still getting processed fairly.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam::channel::{self, Receiver, Sender};

use crate::tier_manager::TaskId;

#[cfg(feature = "tracing")]
use tracing::{debug, error, info, warn};

/// Configuration for the dedicated thread scheduler
#[derive(Debug, Clone)]
pub struct ThreadSchedulerConfig {
    /// Minimum number of threads in the pool
    pub min_threads: usize,
    /// Maximum number of threads in the pool
    pub max_threads: usize,
    /// Duration to keep idle threads alive
    pub keep_alive_duration: Duration,
    /// Maximum tasks in the submission queue
    pub max_queue_size: usize,
    /// Thread name prefix
    pub thread_name_prefix: String,
}

impl Default for ThreadSchedulerConfig {
    fn default() -> Self {
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        Self {
            min_threads: 2,
            max_threads: (cpu_count / 2).max(2).min(8),
            keep_alive_duration: Duration::from_secs(60),
            max_queue_size: 1024,
            thread_name_prefix: "tokio-pulse-reschedule".to_string(),
        }
    }
}

/// Metrics for the thread scheduler
#[derive(Debug, Default)]
pub struct ThreadSchedulerMetrics {
    /// Total tasks submitted for rescheduling
    pub tasks_submitted: AtomicU64,
    /// Total tasks processed
    pub tasks_processed: AtomicU64,
    /// Tasks dropped due to queue overflow
    pub tasks_dropped: AtomicU64,
    /// Current number of active threads
    pub active_threads: AtomicUsize,
    /// Current queue depth
    pub queue_depth: AtomicUsize,
    /// Total thread creation events
    pub threads_created: AtomicU64,
    /// Total thread destruction events
    pub threads_destroyed: AtomicU64,
}

impl ThreadSchedulerMetrics {
    /// Get snapshot of current metrics
    pub fn snapshot(&self) -> ThreadSchedulerMetricsSnapshot {
        ThreadSchedulerMetricsSnapshot {
            tasks_submitted: self.tasks_submitted.load(Ordering::Relaxed),
            tasks_processed: self.tasks_processed.load(Ordering::Relaxed),
            tasks_dropped: self.tasks_dropped.load(Ordering::Relaxed),
            active_threads: self.active_threads.load(Ordering::Relaxed),
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            threads_created: self.threads_created.load(Ordering::Relaxed),
            threads_destroyed: self.threads_destroyed.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of thread scheduler metrics
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSchedulerMetricsSnapshot {
    /// Total tasks submitted for rescheduling
    pub tasks_submitted: u64,
    /// Total tasks processed by worker threads
    pub tasks_processed: u64,
    /// Total tasks dropped due to queue overflow
    pub tasks_dropped: u64,
    /// Current number of active worker threads
    pub active_threads: usize,
    /// Current depth of the task queue
    pub queue_depth: usize,
    /// Total worker threads created
    pub threads_created: u64,
    /// Total worker threads destroyed
    pub threads_destroyed: u64,
}

/// Task to be processed on dedicated thread
#[derive(Debug, Clone)]
pub struct RescheduleTask {
    /// Task identifier
    pub task_id: TaskId,
    /// Submission timestamp for latency tracking
    pub submitted_at: Instant,
    /// Current tier level
    pub tier: u8,
    /// Number of times this task has been rescheduled
    pub reschedule_count: u32,
}

impl RescheduleTask {
    /// Create new reschedule task
    pub fn new(task_id: TaskId, tier: u8) -> Self {
        Self {
            task_id,
            submitted_at: Instant::now(),
            tier,
            reschedule_count: 0,
        }
    }

    /// Get age of this task since submission
    pub fn age(&self) -> Duration {
        self.submitted_at.elapsed()
    }

    /// Increment reschedule count
    pub fn reschedule(&mut self) {
        self.reschedule_count += 1;
    }
}

/// Dedicated thread scheduler for slow tasks
pub struct DedicatedThreadScheduler {
    /// Configuration
    config: ThreadSchedulerConfig,
    /// Task submission channel sender
    task_sender: Sender<RescheduleTask>,
    /// Task submission channel receiver (for workers)
    task_receiver: Receiver<RescheduleTask>,
    /// Shutdown signal sender
    shutdown_sender: Sender<()>,
    /// Worker thread handles
    worker_handles: Vec<JoinHandle<()>>,
    /// Metrics collection
    metrics: Arc<ThreadSchedulerMetrics>,
    /// Indicates if shutdown has been initiated
    shutdown_initiated: Arc<AtomicU64>,
}

impl DedicatedThreadScheduler {
    /// Create new dedicated thread scheduler
    pub fn new(config: ThreadSchedulerConfig) -> Self {
        let (task_sender, task_receiver) = channel::bounded(config.max_queue_size);
        let (shutdown_sender, shutdown_receiver) = channel::bounded(1);
        let metrics = Arc::new(ThreadSchedulerMetrics::default());
        let shutdown_initiated = Arc::new(AtomicU64::new(0));

        #[cfg(feature = "tracing")]
        info!(
            min_threads = config.min_threads,
            max_threads = config.max_threads,
            "Initializing dedicated thread scheduler"
        );

        let mut scheduler = Self {
            config: config.clone(),
            task_sender,
            task_receiver: task_receiver.clone(),
            shutdown_sender,
            worker_handles: Vec::new(),
            metrics: metrics.clone(),
            shutdown_initiated: shutdown_initiated.clone(),
        };

        // Spawn minimum number of worker threads
        for i in 0..config.min_threads {
            let handle = scheduler.spawn_worker_thread(
                i,
                task_receiver.clone(),
                shutdown_receiver.clone(),
                metrics.clone(),
                shutdown_initiated.clone(),
            );
            scheduler.worker_handles.push(handle);
        }

        scheduler
    }

    /// Submit task for rescheduling on dedicated thread
    pub fn reschedule_task(&self, task: RescheduleTask) -> Result<(), RescheduleError> {
        // Update queue depth metric
        let queue_depth = self.task_receiver.len();
        self.metrics.queue_depth.store(queue_depth, Ordering::Relaxed);

        match self.task_sender.try_send(task.clone()) {
            Ok(()) => {
                self.metrics.tasks_submitted.fetch_add(1, Ordering::Relaxed);

                #[cfg(feature = "tracing")]
                debug!(
                    task_id = ?task.task_id,
                    tier = task.tier,
                    queue_depth = queue_depth,
                    "Task submitted for rescheduling"
                );

                Ok(())
            }
            Err(channel::TrySendError::Full(_)) => {
                self.metrics.tasks_dropped.fetch_add(1, Ordering::Relaxed);

                #[cfg(feature = "tracing")]
                warn!(
                    task_id = ?task.task_id,
                    queue_depth = queue_depth,
                    "Task dropped due to queue overflow"
                );

                Err(RescheduleError::QueueFull)
            }
            Err(channel::TrySendError::Disconnected(_)) => {
                #[cfg(feature = "tracing")]
                error!("Failed to submit task: scheduler shutting down");

                Err(RescheduleError::ShuttingDown)
            }
        }
    }

    /// Get current metrics snapshot
    pub fn metrics(&self) -> ThreadSchedulerMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Check if scheduler can accept more tasks
    pub fn can_accept_tasks(&self) -> bool {
        self.task_receiver.len() < self.config.max_queue_size * 90 / 100 // 90% threshold
    }

    /// Get current queue utilization (0.0 to 1.0)
    pub fn queue_utilization(&self) -> f64 {
        self.task_receiver.len() as f64 / self.config.max_queue_size as f64
    }

    /// Spawn a new worker thread
    fn spawn_worker_thread(
        &self,
        thread_id: usize,
        task_receiver: Receiver<RescheduleTask>,
        shutdown_receiver: Receiver<()>,
        metrics: Arc<ThreadSchedulerMetrics>,
        shutdown_initiated: Arc<AtomicU64>,
    ) -> JoinHandle<()> {
        let thread_name = format!("{}-{}", self.config.thread_name_prefix, thread_id);
        let keep_alive = self.config.keep_alive_duration;

        thread::Builder::new()
            .name(thread_name.clone())
            .spawn(move || {
                metrics.active_threads.fetch_add(1, Ordering::AcqRel);
                metrics.threads_created.fetch_add(1, Ordering::Relaxed);

                #[cfg(feature = "tracing")]
                debug!(thread_name = %thread_name, "Worker thread started");

                // Main worker loop
                loop {
                    // Check for shutdown signal first
                    if shutdown_initiated.load(Ordering::Acquire) != 0
                        || shutdown_receiver.try_recv().is_ok() {
                        break;
                    }

                    // Try to receive a task with timeout
                    match task_receiver.recv_timeout(keep_alive) {
                        Ok(task) => {
                            Self::process_task(task, &metrics);
                        }
                        Err(channel::RecvTimeoutError::Timeout) => {
                            // Keep-alive timeout exceeded, consider exiting
                            #[cfg(feature = "tracing")]
                            debug!(thread_name = %thread_name, "Worker thread idle timeout");

                            // For now, keep running (pool sizing could be dynamic in future)
                        }
                        Err(channel::RecvTimeoutError::Disconnected) => {
                            #[cfg(feature = "tracing")]
                            debug!(thread_name = %thread_name, "Task channel disconnected");
                            break;
                        }
                    }
                }

                metrics.active_threads.fetch_sub(1, Ordering::AcqRel);
                metrics.threads_destroyed.fetch_add(1, Ordering::Relaxed);

                #[cfg(feature = "tracing")]
                debug!(thread_name = %thread_name, "Worker thread exiting");
            })
            .expect("Failed to spawn worker thread")
    }

    /// Process a single task on the dedicated thread
    fn process_task(mut task: RescheduleTask, metrics: &Arc<ThreadSchedulerMetrics>) {
        let start_time = Instant::now();

        #[cfg(feature = "tracing")]
        debug!(
            task_id = ?task.task_id,
            tier = task.tier,
            reschedule_count = task.reschedule_count,
            age_ms = task.age().as_millis(),
            "Processing rescheduled task"
        );

        // Simulate task processing (in real implementation, this would
        // involve resuming the actual async task execution)

        // For slow tasks, apply deliberate delays based on tier
        let delay_ms = match task.tier {
            0..=1 => 1,   // Fast lane
            2 => 5,       // Medium delay
            3 => 10,      // High delay
            _ => 20,      // Maximum delay
        };

        thread::sleep(Duration::from_millis(delay_ms));

        task.reschedule();
        metrics.tasks_processed.fetch_add(1, Ordering::Relaxed);

        let processing_duration = start_time.elapsed();

        #[cfg(feature = "tracing")]
        debug!(
            task_id = ?task.task_id,
            processing_ms = processing_duration.as_millis(),
            "Task processing completed"
        );
    }

    /// Initiate graceful shutdown
    pub fn shutdown(&mut self) -> Result<(), RescheduleError> {
        #[cfg(feature = "tracing")]
        info!("Initiating dedicated thread scheduler shutdown");

        // Mark shutdown as initiated
        self.shutdown_initiated.store(1, Ordering::Release);

        // Send shutdown signal to all workers
        for _ in 0..self.worker_handles.len() {
            if let Err(e) = self.shutdown_sender.try_send(()) {
                #[cfg(feature = "tracing")]
                warn!("Failed to send shutdown signal: {:?}", e);
            }
        }

        // Wait for all worker threads to complete
        let mut handles = std::mem::take(&mut self.worker_handles);
        for handle in handles.drain(..) {
            if let Err(e) = handle.join() {
                #[cfg(feature = "tracing")]
                error!("Worker thread join failed: {:?}", e);
            }
        }

        #[cfg(feature = "tracing")]
        info!("Dedicated thread scheduler shutdown completed");

        Ok(())
    }
}

impl Drop for DedicatedThreadScheduler {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Errors that can occur during task rescheduling
#[derive(Debug, thiserror::Error)]
pub enum RescheduleError {
    /// Task queue is full
    #[error("Task queue is full")]
    QueueFull,

    /// Scheduler is shutting down
    #[error("Scheduler is shutting down")]
    ShuttingDown,

    /// Worker thread creation failed
    #[error("Failed to create worker thread: {0}")]
    ThreadCreationFailed(String),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_scheduler_creation() {
        let config = ThreadSchedulerConfig::default();
        let scheduler = DedicatedThreadScheduler::new(config);

        assert!(scheduler.can_accept_tasks());
        assert_eq!(scheduler.queue_utilization(), 0.0);
    }

    #[test]
    fn test_task_submission() {
        let config = ThreadSchedulerConfig::default();
        let scheduler = DedicatedThreadScheduler::new(config);

        let task = RescheduleTask::new(TaskId(12345), 2);
        let result = scheduler.reschedule_task(task);

        assert!(result.is_ok());

        let metrics = scheduler.metrics();
        assert_eq!(metrics.tasks_submitted, 1);
    }

    #[test]
    fn test_task_processing() {
        let mut config = ThreadSchedulerConfig::default();
        config.min_threads = 1;
        config.max_threads = 1;

        let scheduler = DedicatedThreadScheduler::new(config);

        // Submit multiple tasks
        for i in 0..5 {
            let task = RescheduleTask::new(TaskId(i), 1);
            let _ = scheduler.reschedule_task(task);
        }

        // Give time for processing
        thread::sleep(Duration::from_millis(100));

        let metrics = scheduler.metrics();
        assert_eq!(metrics.tasks_submitted, 5);
        assert!(metrics.tasks_processed > 0);
        assert_eq!(metrics.active_threads, 1);
    }

    #[test]
    fn test_queue_overflow() {
        let mut config = ThreadSchedulerConfig::default();
        config.max_queue_size = 2;
        config.min_threads = 0; // No processing threads

        let scheduler = DedicatedThreadScheduler::new(config);

        // Fill the queue
        for i in 0..3 {
            let task = RescheduleTask::new(TaskId(i), 1);
            let result = scheduler.reschedule_task(task);

            if i < 2 {
                assert!(result.is_ok());
            } else {
                assert!(matches!(result, Err(RescheduleError::QueueFull)));
            }
        }

        let metrics = scheduler.metrics();
        assert_eq!(metrics.tasks_submitted, 2);
        assert_eq!(metrics.tasks_dropped, 1);
    }

    #[test]
    fn test_reschedule_task_properties() {
        let mut task = RescheduleTask::new(TaskId(999), 3);

        assert_eq!(task.task_id, TaskId(999));
        assert_eq!(task.tier, 3);
        assert_eq!(task.reschedule_count, 0);

        task.reschedule();
        assert_eq!(task.reschedule_count, 1);

        assert!(task.age() >= Duration::ZERO);
    }

    #[test]
    fn test_metrics_snapshot() {
        let metrics = ThreadSchedulerMetrics::default();

        metrics.tasks_submitted.store(10, Ordering::Relaxed);
        metrics.tasks_processed.store(8, Ordering::Relaxed);
        metrics.active_threads.store(4, Ordering::Relaxed);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.tasks_submitted, 10);
        assert_eq!(snapshot.tasks_processed, 8);
        assert_eq!(snapshot.active_threads, 4);
    }
}