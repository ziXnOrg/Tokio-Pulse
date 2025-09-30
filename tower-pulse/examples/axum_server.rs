//! Example Axum web server with Tokio-Pulse preemption middleware
//!
//! This example demonstrates how to integrate Tokio-Pulse preemption into an Axum
//! web server to prevent request handler monopolization.
//!
//! Run with: cargo run --example axum_server --features tracing

use axum::{
    extract::Query,
    http::StatusCode,
    response::{Html, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};
use tower_pulse::{PreemptionLayer, PreemptionConfig};
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
struct WorkParams {
    iterations: Option<u64>,
    delay_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct WorkResult {
    iterations: u64,
    sum: u64,
    duration_ms: u64,
    preempted: bool,
}

#[derive(Debug, Serialize)]
struct ServerStatus {
    active_tasks: u64,
    total_polls: u64,
    total_violations: u64,
    slow_queue_size: u64,
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // Configure preemption middleware
    let preemption_config = PreemptionConfig {
        poll_budget: Duration::from_micros(1500), // 1.5ms budget per request
        check_interval: 10, // Check every 10 polls
        enable_metrics: true,
        tier_config: None,
        track_workers: true,
    };

    let preemption_layer = PreemptionLayer::new(preemption_config);

    // Build application router
    let app = Router::new()
        .route("/", get(home))
        .route("/work", get(cpu_work))
        .route("/slow", get(slow_work))
        .route("/status", get(server_status))
        .route("/metrics", get(metrics))
        .layer(preemption_layer);

    info!("Starting server on http://localhost:3000");
    info!("Try these endpoints:");
    info!("  GET  /                    - Home page");
    info!("  GET  /work?iterations=N   - CPU-intensive work");
    info!("  GET  /slow?delay_ms=N     - I/O simulation");
    info!("  GET  /status              - Preemption status");
    info!("  GET  /metrics             - Detailed metrics");

    // Start server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .expect("Failed to bind to port 3000");

    axum::serve(listener, app)
        .await
        .expect("Server failed");
}

async fn home() -> Html<&'static str> {
    Html(r#"
<!DOCTYPE html>
<html>
<head>
    <title>Tokio-Pulse Demo Server</title>
    <style>
        body { font-family: Arial, sans-serif; margin: 40px; }
        .endpoint { margin: 20px 0; padding: 10px; background: #f5f5f5; }
        code { background: #eee; padding: 2px 4px; }
    </style>
</head>
<body>
    <h1>Tokio-Pulse Demo Server</h1>
    <p>This server demonstrates preemption middleware preventing task starvation.</p>

    <div class="endpoint">
        <h3>CPU Work</h3>
        <p>Simulate CPU-intensive work: <code>GET /work?iterations=1000000</code></p>
        <p>Try large values to trigger preemption warnings.</p>
    </div>

    <div class="endpoint">
        <h3>Slow I/O</h3>
        <p>Simulate slow I/O: <code>GET /slow?delay_ms=100</code></p>
        <p>This won't trigger preemption as it's truly async.</p>
    </div>

    <div class="endpoint">
        <h3>Status</h3>
        <p>View preemption status: <code>GET /status</code></p>
        <p>See active tasks and violation counts.</p>
    </div>

    <div class="endpoint">
        <h3>Metrics</h3>
        <p>Detailed metrics: <code>GET /metrics</code></p>
        <p>Complete performance and preemption data.</p>
    </div>
</body>
</html>
    "#)
}

async fn cpu_work(Query(params): Query<WorkParams>) -> Result<Json<WorkResult>, StatusCode> {
    let iterations = params.iterations.unwrap_or(100_000);
    let start = std::time::Instant::now();

    info!("Starting CPU work with {} iterations", iterations);

    // Simulate CPU-intensive work that might need preemption
    let mut sum = 0u64;
    for i in 0..iterations {
        sum = sum.wrapping_add(i);

        // Occasionally yield to allow preemption checks
        if i % 10_000 == 0 {
            tokio::task::yield_now().await;
        }
    }

    let duration = start.elapsed();
    let duration_ms = duration.as_millis() as u64;

    // Consider request preempted if it took longer than expected
    let preempted = duration_ms > (iterations / 10_000).max(10);

    if preempted {
        warn!(
            "CPU work completed with possible preemption: {}ms for {} iterations",
            duration_ms, iterations
        );
    } else {
        info!(
            "CPU work completed normally: {}ms for {} iterations",
            duration_ms, iterations
        );
    }

    Ok(Json(WorkResult {
        iterations,
        sum,
        duration_ms,
        preempted,
    }))
}

async fn slow_work(Query(params): Query<WorkParams>) -> Result<Json<WorkResult>, StatusCode> {
    let delay_ms = params.delay_ms.unwrap_or(100);
    let start = std::time::Instant::now();

    info!("Starting slow I/O work with {}ms delay", delay_ms);

    // Simulate slow I/O (this is truly async and won't trigger preemption)
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;

    let duration = start.elapsed();
    let duration_ms = duration.as_millis() as u64;

    info!("I/O work completed: {}ms", duration_ms);

    Ok(Json(WorkResult {
        iterations: 0,
        sum: 0,
        duration_ms,
        preempted: false,
    }))
}

async fn server_status() -> Json<ServerStatus> {
    // In a real implementation, we'd access the TierManager from the middleware
    // For this example, we return mock data
    Json(ServerStatus {
        active_tasks: 0,
        total_polls: 0,
        total_violations: 0,
        slow_queue_size: 0,
    })
}

async fn metrics() -> Json<HashMap<String, serde_json::Value>> {
    use serde_json::json;

    // In a real implementation, we'd collect actual metrics
    let mut metrics = HashMap::new();

    metrics.insert("tokio_pulse_active_tasks".to_string(), json!(0));
    metrics.insert("tokio_pulse_total_polls".to_string(), json!(0));
    metrics.insert("tokio_pulse_total_violations".to_string(), json!(0));
    metrics.insert("tokio_pulse_total_yields".to_string(), json!(0));
    metrics.insert("tokio_pulse_slow_queue_size".to_string(), json!(0));

    Json(metrics)
}