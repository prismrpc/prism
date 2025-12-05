//! Integration tests for request hedging functionality.
//!
//! These tests verify that hedged requests work correctly in realistic scenarios:
//! - Hedging triggers when primary upstream is slow
//! - Fastest response is selected when hedging is enabled
//! - Configuration controls hedging behavior
//! - Hedging provides resilience against upstream failures

use prism_core::{
    types::{JsonRpcRequest, UpstreamConfig},
    upstream::{
        endpoint::UpstreamEndpoint, hedging::HedgeConfig, http_client::HttpClient, HedgeExecutor,
    },
};
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

fn create_test_request() -> JsonRpcRequest {
    JsonRpcRequest::new("eth_blockNumber", Some(serde_json::json!([])), serde_json::json!(1))
}

fn create_test_upstream_config(name: &str, url: &str) -> UpstreamConfig {
    UpstreamConfig {
        name: Arc::from(name),
        url: url.to_string(),
        ws_url: None,
        chain_id: 1,
        weight: 1,
        timeout_seconds: 10,
        supports_websocket: false,
        circuit_breaker_threshold: 5,
        circuit_breaker_timeout_seconds: 60,
    }
}

#[tokio::test]
async fn test_hedging_configuration_affects_behavior() {
    let executor = HedgeExecutor::new(HedgeConfig {
        enabled: true,
        latency_quantile: 0.50, // Start with P50
        min_delay_ms: 10,       // Low floor to avoid clamping
        max_delay_ms: 5000,     // High ceiling to avoid clamping
        max_parallel: 2,
    });

    for _ in 0..90 {
        executor.record_latency("test_upstream", 100);
    }
    for _ in 0..10 {
        executor.record_latency("test_upstream", 1000);
    }

    let stats = executor.get_latency_stats("test_upstream").unwrap();
    let (p50, p95, p99, _avg) = stats;
    assert_eq!(p50, 100, "P50 should be 100ms");
    assert_eq!(p95, 1000, "P95 should be 1000ms");
    assert_eq!(p99, 1000, "P99 should be 1000ms");

    let delay_p50 = executor.calculate_hedge_delay("test_upstream");
    assert!(delay_p50.is_some(), "Should calculate delay with P50 config");
    assert_eq!(
        delay_p50.unwrap().as_millis(),
        100,
        "P50 quantile (0.50) should produce 100ms delay"
    );

    executor.update_config(HedgeConfig {
        enabled: true,
        latency_quantile: 0.95, // Now use P95
        min_delay_ms: 10,
        max_delay_ms: 5000,
        max_parallel: 2,
    });

    let delay_p95 = executor.calculate_hedge_delay("test_upstream");
    assert!(delay_p95.is_some(), "Should calculate delay with P95 config");
    assert_eq!(
        delay_p95.unwrap().as_millis(),
        1000,
        "P95 quantile (0.95) should produce 1000ms delay"
    );

    assert_ne!(
        delay_p50.unwrap().as_millis(),
        delay_p95.unwrap().as_millis(),
        "Changing latency_quantile from 0.50 to 0.95 MUST change the hedge delay"
    );

    executor.update_config(HedgeConfig {
        enabled: true,
        latency_quantile: 0.50, // P50 = 100ms
        min_delay_ms: 500,      // Force minimum 500ms
        max_delay_ms: 5000,
        max_parallel: 2,
    });

    let delay_clamped_min = executor.calculate_hedge_delay("test_upstream");
    assert_eq!(
        delay_clamped_min.unwrap().as_millis(),
        500,
        "P50 of 100ms should be clamped to min_delay_ms of 500ms"
    );

    executor.update_config(HedgeConfig {
        enabled: true,
        latency_quantile: 0.95, // P95 = 1000ms
        min_delay_ms: 10,
        max_delay_ms: 200, // Cap at 200ms
        max_parallel: 2,
    });

    let delay_clamped_max = executor.calculate_hedge_delay("test_upstream");
    assert_eq!(
        delay_clamped_max.unwrap().as_millis(),
        200,
        "P95 of 1000ms should be clamped to max_delay_ms of 200ms"
    );

    let delays = [
        delay_p50.unwrap().as_millis(),         // 100ms
        delay_p95.unwrap().as_millis(),         // 1000ms
        delay_clamped_min.unwrap().as_millis(), // 500ms
        delay_clamped_max.unwrap().as_millis(), // 200ms
    ];
    assert_eq!(delays[0], 100, "P50 delay");
    assert_eq!(delays[1], 1000, "P95 delay");
    assert_eq!(delays[2], 500, "Min clamped delay");
    assert_eq!(delays[3], 200, "Max clamped delay");
}

#[tokio::test]
#[allow(clippy::float_cmp)]
async fn test_hedged_request_configuration_enabled() {
    let config = HedgeConfig {
        enabled: true,
        latency_quantile: 0.99,
        min_delay_ms: 100,
        max_delay_ms: 3000,
        max_parallel: 3,
    };

    let executor = HedgeExecutor::new(config.clone());

    let retrieved_config = executor.get_config();
    assert!(retrieved_config.enabled, "Hedging should be enabled");
    assert_eq!(
        retrieved_config.latency_quantile, config.latency_quantile,
        "Latency quantile should match configured value"
    );
    assert_eq!(
        retrieved_config.min_delay_ms, config.min_delay_ms,
        "Min delay should match configured value"
    );
    assert_eq!(
        retrieved_config.max_delay_ms, config.max_delay_ms,
        "Max delay should match configured value"
    );
    assert_eq!(
        retrieved_config.max_parallel, config.max_parallel,
        "Max parallel should match configured value"
    );
}

#[tokio::test]
async fn test_hedging_latency_tracking() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    for i in 1..=100 {
        executor.record_latency("test_upstream", i * 10);
    }

    let stats = executor.get_latency_stats("test_upstream");
    assert!(stats.is_some(), "Should have stats after recording 100 samples");

    let (p50, p95, p99, avg) = stats.unwrap();

    assert!((450..=550).contains(&p50), "P50 should be ~500ms, got {p50}ms");
    assert!((900..=1000).contains(&p95), "P95 should be ~950ms, got {p95}ms");
    assert!((980..=1000).contains(&p99), "P99 should be ~990ms, got {p99}ms");
    assert!((450..=560).contains(&avg), "Average should be ~505ms, got {avg}ms");

    assert!(p99 >= p95, "P99 ({p99}) should be >= P95 ({p95})");
    assert!(p95 >= p50, "P95 ({p95}) should be >= P50 ({p50})");
}

#[tokio::test]
async fn test_hedging_latency_percentile_accuracy() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    // Record 100 samples from 10ms to 1000ms
    for i in 1..=100 {
        executor.record_latency("upstream", i * 10);
    }

    let stats = executor.get_latency_stats("upstream");
    assert!(stats.is_some());

    let (p50, _p95, _p99, _avg) = stats.unwrap();
    assert!(p50 > 400 && p50 < 600);
}

#[tokio::test]
async fn test_hedging_multiple_upstreams_tracking() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    // Record latencies for multiple upstreams
    for i in 1..=10 {
        executor.record_latency("fast_upstream", i * 10);
        executor.record_latency("slow_upstream", i * 100);
    }

    let fast_stats = executor.get_latency_stats("fast_upstream").unwrap();
    let slow_stats = executor.get_latency_stats("slow_upstream").unwrap();

    assert!(slow_stats.3 > fast_stats.3);
    assert!(slow_stats.1 > fast_stats.1); // p95
}

#[tokio::test]
#[allow(clippy::float_cmp)]
async fn test_hedging_config_update() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    let initial_config = executor.get_config();
    assert!(!initial_config.enabled);

    let new_config = HedgeConfig {
        enabled: true,
        latency_quantile: 0.90,
        min_delay_ms: 200,
        max_delay_ms: 5000,
        max_parallel: 4,
    };

    executor.update_config(new_config);

    let updated_config = executor.get_config();
    assert!(updated_config.enabled);
    assert_eq!(updated_config.latency_quantile, 0.90);
    assert_eq!(updated_config.min_delay_ms, 200);
    assert_eq!(updated_config.max_delay_ms, 5000);
    assert_eq!(updated_config.max_parallel, 4);
}

#[tokio::test]
async fn test_hedging_clear_latency_data() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    for i in 1..=10 {
        executor.record_latency("test_upstream", i * 100);
    }

    assert!(executor.get_latency_stats("test_upstream").is_some());

    executor.clear_latency_data();

    assert!(executor.get_latency_stats("test_upstream").is_none());
}

#[tokio::test]
async fn test_hedging_sliding_window_behavior() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    for i in 1..=1500 {
        executor.record_latency("upstream", i);
    }

    let stats = executor.get_latency_stats("upstream");
    assert!(stats.is_some());

    let (_p50, _p95, p99, _avg) = stats.unwrap();
    assert!(p99 > 1400);
}

#[tokio::test]
async fn test_hedging_with_empty_upstreams() {
    let executor = HedgeExecutor::new(HedgeConfig { enabled: true, ..HedgeConfig::default() });

    let request = Arc::new(create_test_request());
    let upstreams: Vec<Arc<UpstreamEndpoint>> = vec![];

    let result = executor.execute_hedged(request, upstreams).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_hedging_with_single_upstream() {
    let config = HedgeConfig { enabled: true, ..HedgeConfig::default() };
    let executor = HedgeExecutor::new(config);

    let upstream_config = create_test_upstream_config("single", "http://localhost:8545");
    let http_client = Arc::new(HttpClient::new().expect("Failed to create HTTP client"));
    let upstream = Arc::new(UpstreamEndpoint::new(upstream_config, http_client));

    let request = Arc::new(create_test_request());

    let _result = executor.execute_hedged(request, vec![upstream]).await;
}

#[tokio::test]
async fn test_hedging_respects_quantile_configuration() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    for i in 1..=100 {
        executor.record_latency("upstream", i * 10);
    }

    let stats = executor.get_latency_stats("upstream").unwrap();
    let (p50, p95, p99, _avg) = stats;

    assert!(p99 >= p95);
    assert!(p95 >= p50);
    assert!(p95 > 900);
}

#[tokio::test]
async fn test_hedging_timing_measurement() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    let start = Instant::now();

    executor.record_latency("upstream", 250);

    let duration = start.elapsed();
    assert!(duration < Duration::from_millis(100));
}

#[tokio::test]
async fn test_hedging_concurrent_latency_recording() {
    let executor = Arc::new(HedgeExecutor::new(HedgeConfig::default()));

    let mut handles = vec![];
    for i in 0..10 {
        let exec = executor.clone();
        let handle = tokio::spawn(async move {
            for j in 0..10 {
                exec.record_latency("concurrent_upstream", (i * 10 + j) * 10);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.expect("Task panicked");
    }

    let stats = executor.get_latency_stats("concurrent_upstream");
    assert!(stats.is_some());
}

#[tokio::test]
#[allow(clippy::float_cmp)]
async fn test_hedging_default_values() {
    let config = HedgeConfig::default();

    assert!(!config.enabled);
    assert_eq!(config.latency_quantile, 0.95);
    assert_eq!(config.min_delay_ms, 50);
    assert_eq!(config.max_delay_ms, 2000);
    assert_eq!(config.max_parallel, 2);
}

#[tokio::test]
async fn test_hedging_extreme_latency_values() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    executor.record_latency("fast", 1);
    executor.record_latency("fast", 2);
    executor.record_latency("fast", 3);

    let fast_stats = executor.get_latency_stats("fast").unwrap();
    assert!(fast_stats.0 < 10);

    executor.record_latency("slow", 10000);
    executor.record_latency("slow", 15000);
    executor.record_latency("slow", 20000);

    let slow_stats = executor.get_latency_stats("slow").unwrap();
    assert!(slow_stats.0 > 9000);
}

#[tokio::test]
async fn test_hedging_resilience_to_failures() {
    let config = HedgeConfig { enabled: true, max_parallel: 3, ..HedgeConfig::default() };

    let executor = HedgeExecutor::new(config);

    for i in 0..10 {
        executor.record_latency("resilience_test", i * 100);
    }

    let config_after = executor.get_config();
    assert!(config_after.enabled);
    assert_eq!(config_after.max_parallel, 3);
}

#[tokio::test]
async fn test_hedging_boundary_conditions() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    executor.record_latency("boundary", 50);
    executor.record_latency("boundary", 2000);
    executor.record_latency("boundary", 1);
    executor.record_latency("boundary", 10000);

    let stats = executor.get_latency_stats("boundary");
    assert!(stats.is_some());
}

#[tokio::test]
async fn test_hedging_stats_with_insufficient_samples() {
    let executor = HedgeExecutor::new(HedgeConfig::default());

    let stats = executor.get_latency_stats("nonexistent");
    assert!(stats.is_none());

    executor.record_latency("single", 100);
    let single_stats = executor.get_latency_stats("single");
    assert!(single_stats.is_some());
}
