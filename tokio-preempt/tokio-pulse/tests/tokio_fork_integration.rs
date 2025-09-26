//! Integration test to verify tokio fork works with tokio-pulse

use std::sync::Arc;
use tokio_pulse::{HookRegistry, TierConfig, TierManager};

#[tokio::test]
async fn test_tokio_fork_basic_runtime() {
    // Verify we can create a basic tokio runtime
    let result = tokio::spawn(async { "Hello from forked Tokio!" }).await;

    assert_eq!(result.unwrap(), "Hello from forked Tokio!");
}

#[tokio::test]
async fn test_tokio_fork_with_preemption_setup() {
    // This test verifies our setup is ready for integration
    let manager = Arc::new(TierManager::new(TierConfig::default()));
    let registry = HookRegistry::new();

    // In the future, this will install hooks into the runtime
    // For now, just verify the components are ready
    assert!(!registry.has_hooks());

    // Simulate installing the manager
    registry.set_hooks(manager.clone() as Arc<dyn tokio_pulse::PreemptionHooks>);
    assert!(registry.has_hooks());

    // Run an async task
    let handle = tokio::spawn(async {
        let mut sum = 0u64;
        for i in 0..1000 {
            sum = sum.wrapping_add(i);
            if i % 100 == 0 {
                tokio::task::yield_now().await;
            }
        }
        sum
    });

    let result = handle.await.unwrap();
    assert_eq!(result, 499500);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_multi_threaded_runtime() {
    // Verify multi-threaded runtime works
    let handles: Vec<_> = (0..4)
        .map(|i| {
            tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                i * 2
            })
        })
        .collect();

    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }

    assert_eq!(results, vec![0, 2, 4, 6]);
}

#[test]
fn test_runtime_builder() {
    // Test that we can create a runtime with builder
    // This is where we'll add preemption hooks in the next phase
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("Failed to create runtime");

    runtime.block_on(async {
        let value = tokio::spawn(async { 42 }).await.unwrap();
        assert_eq!(value, 42);
    });
}
