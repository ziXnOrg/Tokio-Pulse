/*
 *     ______   __  __     __         ______     ______
 *    /\  == \ /\ \/\ \   /\ \       /\  ___\   /\  ___\
 *    \ \  _-/ \ \ \_\ \  \ \ \____  \ \___  \  \ \  __\
 *     \ \_\    \ \_____\  \ \_____\  \/\_____\  \ \_____\
 *      \/_/     \/_____/   \/_____/   \/_____/   \/_____/
 *
 * Author: Colin MacRitchie / Ripple Group
 */
use crate::TaskId;
use crossbeam::queue::SegQueue;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Priority levels for slow queue processing
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskPriority {
    /// System tasks, never fully blocked
    Critical = 0,
    /// User-interactive tasks
    High = 1,
    /// Standard async tasks
    Normal = 2,
    /// Background work
    Low = 3,
}

impl TaskPriority {
    /// Boost priority by one level
    #[must_use]
    pub const fn boost(self) -> Self {
        match self {
            Self::Low => Self::Normal,
            Self::Normal => Self::High,
            Self::High | Self::Critical => self,
        }
    }
}

/// Task marked as slow and queued for controlled execution
#[derive(Debug, Clone)]
pub struct SlowTask {
    /// Task identifier
    pub task_id: TaskId,
    /// Time when task was enqueued
    pub enqueue_time: Instant,
    /// Current tier level
    pub tier: u8,
    /// Number of violations
    pub violations: u32,
    /// CPU time consumed in nanoseconds
    pub cpu_time_ns: u64,
    /// Number of polls executed
    pub poll_count: u64,
    /// Task priority
    pub priority: TaskPriority,
}

/// Error types for slow queue operations
#[derive(Debug, Clone)]
pub enum QueueError {
    /// Queue has reached maximum capacity
    QueueFull,
    /// Source has exceeded rate limit
    SourceThrottled,
    /// Task is in backoff period
    BackoffActive,
    /// Internal error
    Internal(String),
}

/// Metrics for slow queue monitoring
#[derive(Debug, Default)]
pub struct SlowQueueMetrics {
    /// Total tasks currently queued
    pub total_queued: AtomicUsize,
    /// Total tasks processed
    pub total_processed: AtomicU64,
    /// Tasks enqueued at critical priority
    pub critical_enqueued: AtomicU64,
    /// Tasks processed at critical priority
    pub critical_processed: AtomicU64,
    /// Number of starvation preventions
    pub starvation_prevented: AtomicU64,
    /// Number of priority promotions
    pub priority_promotions: AtomicU64,
    /// High water mark for queue depth
    pub queue_depth_high_water: AtomicUsize,
    /// Consecutive timeouts
    pub consecutive_timeouts: AtomicU32,
    /// Successful reintegrations
    pub reintegration_success: AtomicU64,
    /// Failed reintegrations
    pub reintegration_failure: AtomicU64,
}

impl SlowQueueMetrics {
    /// Record current queue depth
    pub fn record_queue_depth(&self, depth: usize) {
        self.total_queued.store(depth, Ordering::Relaxed);

        // Update high water mark
        let mut current = self.queue_depth_high_water.load(Ordering::Relaxed);
        while depth > current {
            match self.queue_depth_high_water.compare_exchange_weak(
                current,
                depth,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current = x,
            }
        }
    }
}

/// Configuration for slow queue behavior
#[derive(Debug, Clone)]
pub struct SlowQueueConfig {
    /// Maximum tasks in queue
    pub max_queue_size: usize,
    /// Batch size for processing
    pub default_batch_size: usize,
    /// Maximum batch size under load
    pub max_batch_size: usize,
    /// Processing interval in milliseconds
    pub process_interval_ms: u64,
    /// Starvation threshold in milliseconds
    pub starvation_threshold_ms: u64,
    /// Tier-specific execution budgets in milliseconds
    pub tier_budgets: [u64; 4],
    /// Number of good polls required for reintegration
    pub reintegration_good_polls: u32,
    /// Priority weights for fair scheduling
    pub priority_weights: [f32; 4],
    /// Maximum tasks per source
    pub max_tasks_per_source: usize,
}

impl Default for SlowQueueConfig {
    fn default() -> Self {
        Self {
            max_queue_size: 10_000,
            default_batch_size: 32,
            max_batch_size: 256,
            process_interval_ms: 10,
            starvation_threshold_ms: 1000,
            tier_budgets: [100, 50, 10, 1],
            reintegration_good_polls: 10,
            priority_weights: [4.0, 3.0, 2.0, 1.0],
            max_tasks_per_source: 100,
        }
    }
}

/// Main slow queue structure for managing misbehaving tasks
pub struct SlowQueue {
    /// Lock-free MPMC queue
    queue: Arc<SegQueue<SlowTask>>,
    /// Priority queues for fairness
    priority_queues: [Arc<SegQueue<SlowTask>>; 4],
    /// Metrics for monitoring
    metrics: Arc<SlowQueueMetrics>,
    /// Configuration
    config: SlowQueueConfig,
    /// Task source tracking for rate limiting
    source_counts: Arc<dashmap::DashMap<u64, usize>>,
    /// Backoff tracking for repeat offenders
    backoff_map: Arc<dashmap::DashMap<TaskId, Instant>>,
}

impl SlowQueue {
    /// Create new slow queue with configuration
    #[must_use]
    pub fn new(config: SlowQueueConfig) -> Self {
        Self {
            queue: Arc::new(SegQueue::new()),
            priority_queues: [
                Arc::new(SegQueue::new()),
                Arc::new(SegQueue::new()),
                Arc::new(SegQueue::new()),
                Arc::new(SegQueue::new()),
            ],
            metrics: Arc::new(SlowQueueMetrics::default()),
            config,
            source_counts: Arc::new(dashmap::DashMap::new()),
            backoff_map: Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Add task to slow queue with priority handling
    ///
    /// # Errors
    ///
    /// Returns `QueueError::Full` if the queue has reached capacity.
    /// Returns `QueueError::SourceThrottled` if per-source limits are exceeded.
    pub fn enqueue(&self, task: SlowTask) -> Result<(), QueueError> {
        // Check queue limits
        let current_size = self.metrics.total_queued.load(Ordering::Relaxed);
        if current_size >= self.config.max_queue_size {
            return Err(QueueError::QueueFull);
        }

        // Check backoff
        if let Some(backoff_until) = self.backoff_map.get(&task.task_id) {
            if Instant::now() < *backoff_until {
                return Err(QueueError::BackoffActive);
            }
            // Remove expired backoff
            drop(backoff_until);
            self.backoff_map.remove(&task.task_id);
        }

        // Check per-source rate limiting
        let source_id = self.identify_source(&task);
        let mut source_count = self.source_counts.entry(source_id).or_insert(0);
        if *source_count >= self.config.max_tasks_per_source {
            return Err(QueueError::SourceThrottled);
        }
        *source_count += 1;

        // Route to appropriate priority queue
        match task.priority {
            TaskPriority::Critical => {
                self.priority_queues[0].push(task);
                self.metrics.critical_enqueued.fetch_add(1, Ordering::Relaxed);
            },
            priority => {
                let idx = priority as usize;
                self.priority_queues[idx].push(task.clone());
                self.queue.push(task);
            },
        }

        // Update metrics
        self.metrics.total_queued.fetch_add(1, Ordering::Relaxed);
        self.metrics.record_queue_depth(current_size + 1);

        Ok(())
    }

    /// Process batch of slow tasks with fairness
    #[allow(clippy::excessive_nesting)]
    #[must_use]
    pub fn process_batch(&self, batch_size: usize) -> Vec<SlowTask> {
        let mut batch = Vec::with_capacity(batch_size);
        let mut priority_credits = [4, 3, 2, 1]; // Weight by priority

        for _ in 0..batch_size {
            // First check critical queue
            if let Some(task) = self.priority_queues[0].pop() {
                batch.push(task);
                self.metrics.critical_processed.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            // Process other priorities with credits
            let mut found = false;
            for (idx, credits) in priority_credits.iter_mut().enumerate().skip(1) {
                if *credits > 0 {
                    if let Some(task) = self.priority_queues[idx].pop() {
                        batch.push(task.clone());
                        *credits -= 1;
                        found = true;
                        self.decrement_source_count(&task);
                        break;
                    }
                }
            }

            // Fallback to general queue if no priority tasks
            if !found {
                if let Some(task) = self.queue.pop() {
                    batch.push(task.clone());
                    self.decrement_source_count(&task);
                } else {
                    break; // Queue empty
                }
            }
        }

        // Update metrics
        let batch_len = batch.len();
        self.metrics.total_processed.fetch_add(batch_len as u64, Ordering::Relaxed);
        self.metrics.total_queued.fetch_sub(batch_len, Ordering::Relaxed);

        batch
    }

    /// Get current queue size
    #[must_use]
    pub fn size(&self) -> usize {
        self.metrics.total_queued.load(Ordering::Relaxed)
    }

    /// Check for starving tasks and boost priority
    pub fn prevent_starvation(&self) {
        let now = Instant::now();
        let threshold = Duration::from_millis(self.config.starvation_threshold_ms);

        // Check each priority queue for old tasks
        for (_priority, pq) in self.priority_queues.iter().enumerate().skip(1) {
            // Note: SegQueue doesn't have peek, so we need to pop and re-push
            let mut temp_tasks = Vec::new();
            while let Some(task) = pq.pop() {
                if now.duration_since(task.enqueue_time) > threshold {
                    // Task is starving, boost it
                    let mut boosted_task = task.clone();
                    boosted_task.priority = boosted_task.priority.boost();

                    let new_idx = boosted_task.priority as usize;
                    self.priority_queues[new_idx].push(boosted_task);
                    self.metrics.starvation_prevented.fetch_add(1, Ordering::Relaxed);
                    self.metrics.priority_promotions.fetch_add(1, Ordering::Relaxed);
                } else {
                    temp_tasks.push(task);
                }
            }

            // Re-push non-boosted tasks
            for task in temp_tasks {
                pq.push(task);
            }
        }
    }

    /// Apply backoff to a task
    pub fn apply_backoff(&self, task_id: TaskId, duration: Duration) {
        let until = Instant::now() + duration;
        self.backoff_map.insert(task_id, until);
    }

    /// Clear all tasks from queue
    pub fn clear(&self) {
        while self.queue.pop().is_some() {}
        for pq in &self.priority_queues {
            while pq.pop().is_some() {}
        }
        self.source_counts.clear();
        self.backoff_map.clear();
        self.metrics.total_queued.store(0, Ordering::Relaxed);
    }

    /// Get metrics snapshot
    #[must_use]
    pub fn metrics(&self) -> SlowQueueMetricsSnapshot {
        SlowQueueMetricsSnapshot {
            total_queued: self.metrics.total_queued.load(Ordering::Relaxed),
            total_processed: self.metrics.total_processed.load(Ordering::Relaxed),
            critical_enqueued: self.metrics.critical_enqueued.load(Ordering::Relaxed),
            critical_processed: self.metrics.critical_processed.load(Ordering::Relaxed),
            starvation_prevented: self.metrics.starvation_prevented.load(Ordering::Relaxed),
            priority_promotions: self.metrics.priority_promotions.load(Ordering::Relaxed),
            queue_depth_high_water: self.metrics.queue_depth_high_water.load(Ordering::Relaxed),
            consecutive_timeouts: self.metrics.consecutive_timeouts.load(Ordering::Relaxed),
            reintegration_success: self.metrics.reintegration_success.load(Ordering::Relaxed),
            reintegration_failure: self.metrics.reintegration_failure.load(Ordering::Relaxed),
        }
    }

    /// Identify source of task for rate limiting
    fn identify_source(&self, task: &SlowTask) -> u64 {
        // Simple hash of task_id's upper bits as source identifier
        // In production, this could be more sophisticated (e.g., track spawning context)
        task.task_id.0 >> 32
    }

    /// Decrements the source count for a task, reducing nesting
    fn decrement_source_count(&self, task: &SlowTask) {
        let source_id = self.identify_source(task);
        if let Some(mut count) = self.source_counts.get_mut(&source_id) {
            *count = count.saturating_sub(1);
        }
    }

    /// Calculate dynamic batch size based on queue depth
    pub fn calculate_batch_size(&self) -> usize {
        let queue_size = self.size();
        let base_batch = self.config.default_batch_size;

        if queue_size > 1000 {
            // Scale up batch size under load
            (base_batch * 2).min(self.config.max_batch_size)
        } else if queue_size > 500 {
            ((base_batch * 3) / 2).min(self.config.max_batch_size)
        } else {
            base_batch
        }
    }
}

/// Snapshot of metrics for external consumption
#[derive(Debug, Clone)]
pub struct SlowQueueMetricsSnapshot {
    /// Total tasks currently queued
    pub total_queued: usize,
    /// Total tasks processed
    pub total_processed: u64,
    /// Tasks enqueued at critical priority
    pub critical_enqueued: u64,
    /// Tasks processed at critical priority
    pub critical_processed: u64,
    /// Number of starvation preventions
    pub starvation_prevented: u64,
    /// Number of priority promotions
    pub priority_promotions: u64,
    /// High water mark for queue depth
    pub queue_depth_high_water: usize,
    /// Consecutive timeouts
    pub consecutive_timeouts: u32,
    /// Successful reintegrations
    pub reintegration_success: u64,
    /// Failed reintegrations
    pub reintegration_failure: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enqueue_dequeue_basic() {
        let queue = SlowQueue::new(Default::default());

        let task = SlowTask {
            task_id: TaskId(1),
            enqueue_time: Instant::now(),
            tier: 2,
            violations: 3,
            cpu_time_ns: 1_000_000,
            poll_count: 100,
            priority: TaskPriority::Normal,
        };

        assert!(queue.enqueue(task).is_ok());
        assert_eq!(queue.size(), 1);

        let batch = queue.process_batch(1);
        assert_eq!(batch.len(), 1);
        assert_eq!(queue.size(), 0);
    }

    #[test]
    fn test_priority_ordering() {
        let queue = SlowQueue::new(Default::default());

        // Add tasks with different priorities
        for i in 0..4 {
            let task = SlowTask {
                task_id: TaskId(i),
                enqueue_time: Instant::now(),
                tier: 2,
                violations: 1,
                cpu_time_ns: 0,
                poll_count: 0,
                priority: match i {
                    0 => TaskPriority::Low,
                    1 => TaskPriority::Normal,
                    2 => TaskPriority::High,
                    3 => TaskPriority::Critical,
                    _ => TaskPriority::Normal,
                },
            };
            queue.enqueue(task).unwrap();
        }

        // Critical should be processed first
        let batch = queue.process_batch(1);
        assert_eq!(batch[0].priority, TaskPriority::Critical);
    }

    #[test]
    fn test_queue_full() {
        let mut config = SlowQueueConfig::default();
        config.max_queue_size = 2;
        let queue = SlowQueue::new(config);

        for i in 0..3 {
            let task = SlowTask {
                task_id: TaskId(i),
                enqueue_time: Instant::now(),
                tier: 2,
                violations: 1,
                cpu_time_ns: 0,
                poll_count: 0,
                priority: TaskPriority::Normal,
            };

            if i < 2 {
                assert!(queue.enqueue(task).is_ok());
            } else {
                assert!(matches!(queue.enqueue(task), Err(QueueError::QueueFull)));
            }
        }
    }

    #[test]
    fn test_source_throttling() {
        let mut config = SlowQueueConfig::default();
        config.max_tasks_per_source = 2;
        let queue = SlowQueue::new(config);

        // Tasks from same source (upper 32 bits of task_id)
        for i in 0..3 {
            let task = SlowTask {
                task_id: TaskId((1u64 << 32) | i),
                enqueue_time: Instant::now(),
                tier: 2,
                violations: 1,
                cpu_time_ns: 0,
                poll_count: 0,
                priority: TaskPriority::Normal,
            };

            if i < 2 {
                assert!(queue.enqueue(task).is_ok());
            } else {
                assert!(matches!(queue.enqueue(task), Err(QueueError::SourceThrottled)));
            }
        }
    }

    #[test]
    fn test_metrics_tracking() {
        let queue = SlowQueue::new(Default::default());

        let task = SlowTask {
            task_id: TaskId(1),
            enqueue_time: Instant::now(),
            tier: 2,
            violations: 1,
            cpu_time_ns: 0,
            poll_count: 0,
            priority: TaskPriority::Critical,
        };

        queue.enqueue(task).unwrap();
        let metrics = queue.metrics();
        assert_eq!(metrics.critical_enqueued, 1);

        let _ = queue.process_batch(1);
        let metrics = queue.metrics();
        assert_eq!(metrics.critical_processed, 1);
        assert_eq!(metrics.total_processed, 1);
    }
}
