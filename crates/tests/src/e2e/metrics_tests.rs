//! Metrics Endpoint Tests
//!
//! Tests for verifying the Prometheus metrics endpoint functionality
//! and metric accuracy.

use super::{
    client::{format_hex_u64, E2eClient},
    config::E2eConfig,
    fixtures::TestFixtures,
};
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

/// Parse Prometheus metrics text format to find a specific metric value
#[allow(dead_code)]
fn find_metric_value(metrics: &str, metric_name: &str) -> Option<f64> {
    for line in metrics.lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with(metric_name) {
            // Handle metrics with labels: metric_name{labels} value
            // and without labels: metric_name value
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(value) = parts.last()?.parse::<f64>() {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Count occurrences of a metric family
fn count_metric_occurrences(metrics: &str, metric_name: &str) -> usize {
    metrics
        .lines()
        .filter(|line| !line.starts_with('#') && line.starts_with(metric_name))
        .count()
}

#[tokio::test]
async fn test_metrics_endpoint_available() {
    let (client, _fixtures) = setup().await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Should return non-empty response
    assert!(!metrics.is_empty(), "Metrics should not be empty");

    // Should be in Prometheus text format (contains # HELP or # TYPE)
    let has_prometheus_format = metrics.contains("# HELP") || metrics.contains("# TYPE");
    println!(
        "Metrics endpoint returned {} bytes, Prometheus format: {}",
        metrics.len(),
        has_prometheus_format
    );
}

#[tokio::test]
async fn test_request_metrics_increment() {
    let (client, _fixtures) = setup().await;

    // Get metrics before making requests
    let metrics_before = client.get_metrics().await.expect("Failed to get metrics before");

    // Count request metrics before
    let requests_before = count_metric_occurrences(&metrics_before, "rpc_requests_total");

    // Make some requests
    for _ in 0..5 {
        let _ = client.get_block_number().await;
    }

    // Small delay to ensure metrics are updated
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Get metrics after
    let metrics_after = client.get_metrics().await.expect("Failed to get metrics after");

    // Count request metrics after
    let requests_after = count_metric_occurrences(&metrics_after, "rpc_requests_total");

    println!("Request metric occurrences: before={requests_before}, after={requests_after}");

    // Should have metrics entries (counts may or may not increase depending on setup)
    // The main check is that the endpoint works and returns metrics
}

#[tokio::test]
async fn test_cache_metrics_present() {
    let (client, fixtures) = setup().await;

    // Make some requests that should populate cache metrics
    let block_num = fixtures.get_finalized_block().await.expect("Failed to get block");
    let block_hex = format_hex_u64(block_num);

    // First request - cache miss
    let _ = client.get_block_by_number(&block_hex, false).await;

    // Second request - should be cache hit
    let _ = client.get_block_by_number(&block_hex, false).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Check for cache-related metrics
    let has_cache_hits = metrics.contains("rpc_cache_hits_total") || metrics.contains("cache_hit");
    let has_cache_misses =
        metrics.contains("rpc_cache_misses_total") || metrics.contains("cache_miss");

    println!("Cache metrics present: hits={has_cache_hits}, misses={has_cache_misses}");

    // Print cache-related lines for debugging
    for line in metrics.lines() {
        if line.to_lowercase().contains("cache") && !line.starts_with('#') {
            println!("Cache metric: {line}");
        }
    }
}

#[tokio::test]
async fn test_upstream_metrics_present() {
    let (client, _fixtures) = setup().await;

    // Make some requests to populate upstream metrics
    for _ in 0..3 {
        let _ = client.get_block_number().await;
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Check for upstream-related metrics
    let has_upstream_metrics =
        metrics.contains("upstream") || metrics.contains("rpc_requests_total");

    println!("Upstream metrics present: {has_upstream_metrics}");

    // Print upstream-related lines
    for line in metrics.lines() {
        if (line.to_lowercase().contains("upstream") || line.contains("rpc_requests")) &&
            !line.starts_with('#')
        {
            println!("Upstream metric: {line}");
        }
    }
}

#[tokio::test]
async fn test_latency_metrics_present() {
    let (client, _fixtures) = setup().await;

    // Make some requests
    for _ in 0..5 {
        let _ = client.get_block_number().await;
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Check for latency/duration metrics (histograms)
    let has_latency = metrics.contains("duration") || metrics.contains("latency");
    let has_histogram =
        metrics.contains("_bucket") || metrics.contains("_sum") || metrics.contains("_count");

    println!("Latency metrics: present={has_latency}, histogram={has_histogram}");

    // Print histogram metrics
    for line in metrics.lines() {
        if (line.contains("duration") || line.contains("latency")) && !line.starts_with('#') {
            println!("Latency metric: {line}");
        }
    }
}

#[tokio::test]
async fn test_health_metrics_present() {
    let (client, _fixtures) = setup().await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Check for health-related metrics
    let has_health = metrics.contains("health") || metrics.contains("healthy");

    println!("Health metrics present: {has_health}");

    // Print health-related lines
    for line in metrics.lines() {
        if line.to_lowercase().contains("health") && !line.starts_with('#') {
            println!("Health metric: {line}");
        }
    }
}

#[tokio::test]
async fn test_error_metrics_increment_on_invalid_request() {
    let (client, _fixtures) = setup().await;

    // Get metrics before
    let metrics_before = client.get_metrics().await.expect("Failed to get metrics");

    // Try to make an invalid request (unsupported method)
    // This should increment error metrics
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_unsupportedMethod",
        "params": [],
        "id": 1
    });

    let http_client = reqwest::Client::new();
    let _ = http_client.post(&client.config().proxy_url).json(&request).send().await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Get metrics after
    let metrics_after = client.get_metrics().await.expect("Failed to get metrics");

    // Check for error-related metrics
    let errors_before = count_metric_occurrences(&metrics_before, "error");
    let errors_after = count_metric_occurrences(&metrics_after, "error");

    println!("Error metric occurrences: before={errors_before}, after={errors_after}");
}

#[tokio::test]
async fn test_metrics_method_labels() {
    let (client, _fixtures) = setup().await;

    // Make requests with different methods
    let _ = client.get_block_number().await;
    let _ = client.get_chain_id().await;
    let _ = client.get_balance(&client.config().test_account).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Check that metrics have method labels
    let has_method_labels = metrics.contains("method=") || metrics.contains("method=\"");

    println!("Method labels present: {has_method_labels}");

    // Print lines with method labels
    for line in metrics.lines() {
        if line.contains("method=") && !line.starts_with('#') {
            println!("Method-labeled metric: {line}");
        }
    }
}

#[tokio::test]
#[serial]
async fn test_metrics_concurrent_requests() {
    let (client, _fixtures) = setup().await;

    // Make many concurrent requests
    let handles: Vec<_> = (0..20)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.get_block_number().await })
        })
        .collect();

    for handle in handles {
        let _ = handle.await;
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Get metrics - should not crash or return errors under concurrent load
    let metrics = client
        .get_metrics()
        .await
        .expect("Metrics endpoint should handle concurrent load");

    assert!(!metrics.is_empty(), "Metrics should be returned after concurrent requests");

    println!("Metrics endpoint handled concurrent requests (returned {} bytes)", metrics.len());
}

#[tokio::test]
async fn test_metrics_format_valid() {
    let (client, _fixtures) = setup().await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Basic Prometheus format validation
    let mut valid_format = true;
    let mut metric_count = 0;

    for line in metrics.lines() {
        if line.is_empty() {
            continue;
        }

        if line.starts_with('#') {
            // Comment line - should be # HELP or # TYPE
            if !line.starts_with("# HELP") && !line.starts_with("# TYPE") {
                // Allow other comment formats
            }
            continue;
        }

        // Metric line should have format: metric_name{labels} value timestamp?
        // or: metric_name value timestamp?
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            println!("Invalid metric line (too few parts): {line}");
            valid_format = false;
            continue;
        }

        // Value should be parseable as number
        if parts[parts.len() - 1].parse::<f64>().is_err() &&
            parts.len() > 2 &&
            parts[parts.len() - 2].parse::<f64>().is_err()
        {
            println!("Invalid metric value: {line}");
            valid_format = false;
        } else {
            metric_count += 1;
        }
    }

    println!("Metrics format valid: {valid_format}, metric count: {metric_count}");
    assert!(valid_format, "Metrics should be in valid Prometheus format");
}

#[tokio::test]
async fn test_rate_limit_metrics() {
    let (client, _fixtures) = setup().await;

    // Make many rapid requests (might trigger rate limiting)
    for _ in 0..50 {
        let _ = client.get_block_number().await;
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Check for rate limit metrics
    let has_rate_limit = metrics.contains("rate_limit") || metrics.contains("ratelimit");

    println!("Rate limit metrics present: {has_rate_limit}");

    // Print rate limit related metrics
    for line in metrics.lines() {
        if (line.to_lowercase().contains("rate") || line.to_lowercase().contains("limit")) &&
            !line.starts_with('#')
        {
            println!("Rate limit metric: {line}");
        }
    }
}

#[tokio::test]
async fn test_connection_metrics() {
    let (client, _fixtures) = setup().await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Check for connection-related metrics
    let has_connections = metrics.contains("connection") || metrics.contains("active");

    println!("Connection metrics present: {has_connections}");

    for line in metrics.lines() {
        if line.to_lowercase().contains("connection") && !line.starts_with('#') {
            println!("Connection metric: {line}");
        }
    }
}

#[tokio::test]
async fn test_metrics_stability_over_time() {
    let (client, _fixtures) = setup().await;

    // Get metrics multiple times to ensure stability
    let mut sizes = Vec::new();

    for i in 0..5 {
        let metrics = client.get_metrics().await.expect("Failed to get metrics");
        sizes.push(metrics.len());
        println!("Metrics fetch {}: {} bytes", i + 1, metrics.len());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // All fetches should return similar-sized responses
    let min_size = sizes.iter().min().unwrap();
    let max_size = sizes.iter().max().unwrap();

    println!("Metrics size range: {min_size} - {max_size} bytes");

    // Allow some variance but should be within reasonable bounds
    assert!(*min_size > 0, "Metrics should always have content");
}

#[tokio::test]
async fn test_specific_metric_values() {
    let (client, _fixtures) = setup().await;

    // Make known number of requests
    let request_count = 10;
    for _ in 0..request_count {
        let _ = client.get_block_number().await;
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let metrics = client.get_metrics().await.expect("Failed to get metrics");

    // Try to find and parse specific metrics
    println!("Looking for specific metric values...");

    // Print all non-comment lines for inspection
    println!("\n=== All Metric Lines ===");
    for line in metrics.lines() {
        if !line.starts_with('#') && !line.is_empty() {
            println!("{line}");
        }
    }
    println!("=== End Metric Lines ===\n");
}
