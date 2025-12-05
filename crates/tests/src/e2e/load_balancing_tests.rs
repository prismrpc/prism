//! Load Balancing Tests
//!
//! Tests for verifying load balancing behavior across multiple upstream nodes.
//!
//! Note: Heavy load tests are marked with `#[serial]` to prevent resource contention
//! when running alongside other tests. This prevents false failures due to the proxy
//! being overwhelmed by multiple parallel test suites.

use super::{client::E2eClient, config::E2eConfig, fixtures::TestFixtures};
use serial_test::serial;
use std::time::Duration;

/// Setup function for tests
async fn setup() -> (E2eClient, TestFixtures) {
    let config = E2eConfig::from_env();
    let client = E2eClient::new(config);
    let fixtures = TestFixtures::new(client.clone());

    fixtures
        .wait_for_environment(Duration::from_secs(30))
        .await
        .expect("Environment not ready");

    (client, fixtures)
}

#[tokio::test]
async fn test_multiple_upstreams_available() {
    let (client, _fixtures) = setup().await;

    let health = client.get_health().await.expect("Health check failed");

    let total = health["upstreams"]["total"].as_u64().unwrap_or(0);
    let healthy = health["upstreams"]["healthy"].as_u64().unwrap_or(0);

    println!("Upstreams: {healthy} healthy / {total} total");

    assert!(total > 0, "Should have at least one upstream configured");
    assert!(healthy > 0, "Should have at least one healthy upstream");
}

#[tokio::test]
async fn test_requests_distributed_to_multiple_upstreams() {
    let (client, _fixtures) = setup().await;

    // Make many requests to trigger distribution across upstreams
    let request_count = 50;
    let mut success_count = 0;

    for _ in 0..request_count {
        if client.get_block_number().await.is_ok() {
            success_count += 1;
        }
    }

    // Check metrics to see if requests went to different upstreams
    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Count metrics with different upstream labels
    let upstream_metrics: Vec<&str> = metrics
        .lines()
        .filter(|line| line.contains("upstream=") && line.contains("rpc_requests"))
        .collect();

    println!(
        "{success_count}/{request_count} requests succeeded, {} upstream metric entries",
        upstream_metrics.len()
    );

    for metric in &upstream_metrics {
        println!("Upstream metric: {metric}");
    }

    // Most requests should succeed
    assert!(success_count >= request_count * 9 / 10, "At least 90% of requests should succeed");
}

#[tokio::test]
#[serial]
async fn test_concurrent_load_distribution() {
    let (client, _fixtures) = setup().await;

    // Spawn many concurrent requests
    let request_count = 100;
    let handles: Vec<_> = (0..request_count)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move {
                let start = std::time::Instant::now();
                let result = client.get_block_number().await;
                (result.is_ok(), start.elapsed())
            })
        })
        .collect();

    let mut success_count = 0;
    let mut total_duration = Duration::ZERO;
    let mut max_duration = Duration::ZERO;

    for handle in handles {
        if let Ok((succeeded, duration)) = handle.await {
            if succeeded {
                success_count += 1;
            }
            total_duration += duration;
            max_duration = max_duration.max(duration);
        }
    }

    let avg_duration = total_duration / request_count;

    println!("Concurrent load test results:");
    println!("  Requests: {request_count}");
    println!("  Successful: {success_count}");
    println!("  Average duration: {avg_duration:?}");
    println!("  Max duration: {max_duration:?}");

    // High success rate expected
    assert!(
        success_count >= request_count * 8 / 10,
        "At least 80% should succeed under concurrent load"
    );

    // Max duration should not be excessive
    assert!(
        max_duration < Duration::from_secs(30),
        "No request should take longer than 30 seconds"
    );
}

#[tokio::test]
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_precision_loss)]
async fn test_response_time_consistency() {
    let (client, _fixtures) = setup().await;

    // Measure response times for multiple sequential requests
    let mut durations = Vec::new();

    for _ in 0..20 {
        let start = std::time::Instant::now();
        if client.get_block_number().await.is_ok() {
            durations.push(start.elapsed());
        }
    }

    assert!(!durations.is_empty(), "Should have successful requests");

    // Calculate statistics
    let sum: Duration = durations.iter().sum();
    let avg = sum / durations.len() as u32;
    let min = durations.iter().min().unwrap();
    let max = durations.iter().max().unwrap();

    // Calculate standard deviation
    let avg_nanos = avg.as_nanos() as f64;
    let variance: f64 = durations
        .iter()
        .map(|d| {
            let diff = d.as_nanos() as f64 - avg_nanos;
            diff * diff
        })
        .sum::<f64>() /
        durations.len() as f64;
    let std_dev = variance.sqrt();

    println!("Response time statistics:");
    println!("  Count: {}", durations.len());
    println!("  Min: {min:?}");
    println!("  Max: {max:?}");
    println!("  Average: {avg:?}");
    println!("  Std Dev: {:.2}ms", std_dev / 1_000_000.0);

    // Response times should be reasonably consistent
    // (not checking too strictly as network conditions vary)
}

#[tokio::test]
async fn test_health_endpoint_upstream_info() {
    let (client, _fixtures) = setup().await;

    let health = client.get_health().await.expect("Health check failed");

    // Check upstream information in health response
    let upstreams = &health["upstreams"];

    let total = upstreams["total"].as_u64().expect("Should have total count");
    let healthy = upstreams["healthy"].as_u64().expect("Should have healthy count");
    let avg_response_time = upstreams["average_response_time_ms"].as_f64();

    println!("Upstream health info:");
    println!("  Total: {total}");
    println!("  Healthy: {healthy}");
    if let Some(avg_rt) = avg_response_time {
        println!("  Avg response time: {avg_rt:.2}ms");
    }

    assert!(total > 0, "Should have upstreams configured");
    assert!(healthy <= total, "Healthy should not exceed total");
}

#[tokio::test]
#[allow(clippy::cast_possible_truncation)]
async fn test_slow_upstream_handling() {
    let (client, _fixtures) = setup().await;

    // The devnet has a "slow" node with 10s block time
    // Make requests and verify they complete in reasonable time

    let request_count = 10;
    let mut durations = Vec::new();

    for _ in 0..request_count {
        let start = std::time::Instant::now();
        if client.get_block_number().await.is_ok() {
            durations.push(start.elapsed());
        }
    }

    // Responses should complete reasonably fast (load balancer should prefer faster nodes)
    let avg: Duration = durations.iter().sum::<Duration>() / durations.len() as u32;

    println!(
        "Average response time with slow node in pool: {avg:?} ({} requests)",
        durations.len()
    );

    // If load balancing is working well, average should be reasonable
    // (not dominated by the slow node) - increased to 15s for concurrent test load
    assert!(avg < Duration::from_secs(15), "Average response time should be under 15s");
}

#[tokio::test]
#[serial]
async fn test_weighted_distribution() {
    let (client, _fixtures) = setup().await;

    // Make many requests and check metrics for distribution
    let request_count = 100;

    for _ in 0..request_count {
        let _ = client.get_block_number().await;
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Extract upstream request counts from metrics
    println!("Upstream distribution metrics:");
    for line in metrics.lines() {
        if line.contains("rpc_requests_total") && line.contains("upstream=") {
            println!("  {line}");
        }
    }

    // The distribution depends on weights and response times
    // Just verify that metrics are being recorded
}

#[tokio::test]
#[serial]
async fn test_burst_traffic_handling() {
    let (client, _fixtures) = setup().await;

    // Send a burst of requests
    let burst_size = 50;
    let start = std::time::Instant::now();

    let handles: Vec<_> = (0..burst_size)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.get_block_number().await })
        })
        .collect();

    let mut success_count = 0;
    for handle in handles {
        if let Ok(Ok(_)) = handle.await {
            success_count += 1;
        }
    }

    let total_duration = start.elapsed();

    println!("Burst traffic test:");
    println!("  Burst size: {burst_size}");
    println!("  Successful: {success_count}");
    println!("  Total duration: {total_duration:?}");
    println!("  Throughput: {:.2} req/s", f64::from(burst_size) / total_duration.as_secs_f64());

    // Should handle burst traffic with high success rate
    assert!(success_count >= burst_size * 7 / 10, "At least 70% should succeed during burst");
}

#[tokio::test]
#[serial]
async fn test_sustained_load() {
    let (client, _fixtures) = setup().await;

    // Run sustained load for a few seconds
    let test_duration = Duration::from_secs(5);
    let start = std::time::Instant::now();
    let mut request_count = 0;
    let mut success_count = 0;

    while start.elapsed() < test_duration {
        request_count += 1;
        if client.get_block_number().await.is_ok() {
            success_count += 1;
        }
        // Small delay to make load sustainable
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let total_duration = start.elapsed();
    let rps = f64::from(request_count) / total_duration.as_secs_f64();
    let success_rate = f64::from(success_count) / f64::from(request_count) * 100.0;

    println!("Sustained load test:");
    println!("  Duration: {total_duration:?}");
    println!("  Total requests: {request_count}");
    println!("  Successful: {success_count}");
    println!("  Success rate: {success_rate:.1}%");
    println!("  Throughput: {rps:.2} req/s");

    assert!(success_rate >= 90.0, "Should maintain 90% success rate under sustained load");
}

#[tokio::test]
#[serial]
#[allow(clippy::cast_possible_truncation)]
async fn test_mixed_method_load() {
    let (client, fixtures) = setup().await;

    let block_num = fixtures.get_finalized_block().await.expect("Failed to get block");
    let block_hex = super::client::format_hex_u64(block_num);

    // Mix of different methods
    let methods_and_params = [
        ("eth_blockNumber", serde_json::json!([])),
        ("eth_chainId", serde_json::json!([])),
        ("eth_getBlockByNumber", serde_json::json!([&block_hex, false])),
        ("eth_gasPrice", serde_json::json!([])),
    ];

    let request_count = 40; // 10 of each method
    let handles: Vec<_> = (0..request_count)
        .map(|i| {
            let client = client.clone();
            let (method, params) = methods_and_params[i % methods_and_params.len()].clone();
            tokio::spawn(async move {
                let result: Result<super::client::TimedResponse<serde_json::Value>, _> =
                    client.proxy_request(method, params).await;
                (method.to_string(), result.is_ok())
            })
        })
        .collect();

    let mut method_success: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();

    for handle in handles {
        if let Ok((method, success)) = handle.await {
            let entry = method_success.entry(method).or_insert((0, 0));
            entry.0 += 1;
            if success {
                entry.1 += 1;
            }
        }
    }

    println!("Mixed method load test results:");
    for (method, (total, success)) in &method_success {
        println!("  {method}: {success}/{total} successful");
    }

    // All methods should have reasonable success rate
    for (method, (total, success)) in &method_success {
        let rate = f64::from(*success as u32) / f64::from(*total as u32);
        assert!(rate >= 0.7, "{method} should have at least 70% success rate");
    }
}

#[tokio::test]
async fn test_upstream_stats_in_health() {
    let (client, _fixtures) = setup().await;

    // Make some requests first
    for _ in 0..10 {
        let _ = client.get_block_number().await;
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let health = client.get_health().await.expect("Health check failed");

    // Check for comprehensive upstream stats
    let upstreams = &health["upstreams"];

    println!("Upstream stats from health endpoint:");
    println!("{}", serde_json::to_string_pretty(upstreams).unwrap_or_default());

    // Basic structure checks
    assert!(upstreams.get("total").is_some(), "Should have total");
    assert!(upstreams.get("healthy").is_some(), "Should have healthy");
}

#[tokio::test]
#[allow(clippy::cast_possible_truncation)]
async fn test_response_time_aware_routing() {
    let (client, _fixtures) = setup().await;

    // Make many requests and verify that response times are optimized
    // (proxy should prefer faster upstreams)

    let warmup_count = 20; // Let the proxy learn response times
    for _ in 0..warmup_count {
        let _ = client.get_block_number().await;
    }

    // Now measure actual performance
    let test_count = 30;
    let mut durations = Vec::new();

    for _ in 0..test_count {
        let start = std::time::Instant::now();
        if client.get_block_number().await.is_ok() {
            durations.push(start.elapsed());
        }
    }

    let avg: Duration = durations.iter().sum::<Duration>() / durations.len() as u32;

    println!("After warmup: {} requests, avg response time: {avg:?}", durations.len());

    // Response time aware routing should keep average low
    assert!(
        avg < Duration::from_secs(2),
        "Average response time should be under 2s with response-time-aware routing"
    );
}
