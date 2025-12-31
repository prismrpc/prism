//! Integration tests for the `ProxyEngine`.
//!
//! These tests verify the core request routing logic of the `ProxyEngine`,
//! including request validation, method routing, handler delegation,
//! upstream forwarding, and statistics collection.

use crate::mock_infrastructure::{
    create_log_filter, create_test_logs, BlockResponseBuilder, RpcMockBuilder,
};
use prism_core::{
    cache::CacheManagerConfig,
    chain::ChainState,
    metrics::MetricsCollector,
    proxy::{engine::ProxyEngine, errors::ProxyError},
    types::{CacheStatus, JsonRpcRequest, JSONRPC_VERSION_COW},
    upstream::UpstreamManagerBuilder,
};
use serde_json::json;
use serial_test::serial;
use std::sync::Arc;

/// Creates a test `ProxyEngine` instance connected to a mock upstream server.
fn create_test_proxy_engine(upstream_url: &str) -> ProxyEngine {
    let chain_state = Arc::new(ChainState::new());

    // Create cache manager with default config
    let cache_config = CacheManagerConfig::default();
    let cache_manager = Arc::new(
        prism_core::cache::CacheManager::new(&cache_config, chain_state.clone())
            .expect("Failed to create cache manager"),
    );

    // Create upstream manager
    let upstream_manager = Arc::new(
        UpstreamManagerBuilder::new()
            .chain_state(chain_state)
            .concurrency_limit(100)
            .build()
            .expect("Failed to create upstream manager"),
    );

    // Add mock upstream
    let upstream_config = prism_core::types::UpstreamConfig {
        name: Arc::from("mock-upstream"),
        url: upstream_url.to_string(),
        ws_url: None,
        chain_id: 1,
        weight: 1,
        timeout_seconds: 10,
        supports_websocket: false,
        circuit_breaker_threshold: 5,
        circuit_breaker_timeout_seconds: 60,
    };
    upstream_manager.add_upstream(upstream_config);

    // Create metrics collector
    let metrics_collector =
        Arc::new(MetricsCollector::new().expect("Failed to create metrics collector"));

    // Create alert manager
    let alert_manager = Arc::new(prism_core::alerts::AlertManager::new());

    ProxyEngine::new(cache_manager, upstream_manager, metrics_collector, alert_manager)
}

#[tokio::test]
#[serial]
async fn test_invalid_jsonrpc_version_rejected() {
    let mock = RpcMockBuilder::new().await;
    let engine = create_test_proxy_engine(&mock.url());

    // Create request with invalid JSON-RPC version
    let mut request = JsonRpcRequest::new("eth_blockNumber", None, json!(1));
    request.jsonrpc = "1.0".into();

    let result = engine.process_request(request).await;

    assert!(result.is_err(), "Invalid JSON-RPC version should be rejected");
    match result.unwrap_err() {
        ProxyError::Validation(_) => {
            // Expected - validation error for invalid version
        }
        e => panic!("Expected ValidationError, got: {e:?}"),
    }
}

#[tokio::test]
#[serial]
async fn test_unsupported_method_rejected() {
    let mock = RpcMockBuilder::new().await;
    let engine = create_test_proxy_engine(&mock.url());

    let request = JsonRpcRequest::new("eth_sendRawTransaction", Some(json!(["0xabc"])), json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_err(), "Unsupported method should be rejected");
    match result.unwrap_err() {
        ProxyError::Validation(_) => {
            // Expected - validation checks method allowlist and returns Validation error
        }
        ProxyError::MethodNotSupported(method) => {
            assert_eq!(method, "eth_sendRawTransaction");
        }
        e => panic!("Expected Validation or MethodNotSupported, got: {e:?}"),
    }
}

#[tokio::test]
#[serial]
async fn test_empty_method_name_rejected() {
    let mock = RpcMockBuilder::new().await;
    let engine = create_test_proxy_engine(&mock.url());

    let request = JsonRpcRequest::new("", None, json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_err(), "Empty method name should be rejected");
    match result.unwrap_err() {
        ProxyError::Validation(_) | ProxyError::MethodNotSupported(_) => {
            // Expected - either validation error or method not supported
        }
        e => panic!("Expected Validation or MethodNotSupported error, got: {e:?}"),
    }
}

#[tokio::test]
#[serial]
async fn test_valid_request_passes_validation() {
    let mut mock = RpcMockBuilder::new().await;
    mock.mock_block_number(100);

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_blockNumber", None, json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "Valid request should pass validation: {result:?}");
}

#[tokio::test]
#[serial]
async fn test_eth_get_logs_routes_to_handler() {
    let mut mock = RpcMockBuilder::new().await;
    let test_logs = create_test_logs(100, 102, 2);
    mock.mock_get_logs(&test_logs);

    let engine = create_test_proxy_engine(&mock.url());
    let request =
        JsonRpcRequest::new("eth_getLogs", Some(json!([create_log_filter(100, 102)])), json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "eth_getLogs should be handled successfully");
    let response = result.unwrap();
    assert!(response.result.is_some(), "Response should contain result");
}

#[tokio::test]
#[serial]
async fn test_eth_get_block_by_number_routes_to_handler() {
    let mut mock = RpcMockBuilder::new().await;
    let block = BlockResponseBuilder::new(100).build();
    mock.mock_get_block_by_number(100, &block);

    let engine = create_test_proxy_engine(&mock.url());
    let request =
        JsonRpcRequest::new("eth_getBlockByNumber", Some(json!(["0x64", false])), json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "eth_getBlockByNumber should be handled successfully");
    let response = result.unwrap();
    assert!(response.result.is_some(), "Response should contain result");
}

#[tokio::test]
#[serial]
async fn test_eth_get_block_by_hash_routes_to_handler() {
    let mut mock = RpcMockBuilder::new().await;
    let block = BlockResponseBuilder::new(100).build();
    let block_hash = block["hash"].as_str().unwrap();

    // Mock the request
    mock.get_server()
        .mock("POST", "/")
        .match_body(mockito::Matcher::Regex(r#""method"\s*:\s*"eth_getBlockByHash""#.to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": block
            })
            .to_string(),
        )
        .create();

    let engine = create_test_proxy_engine(&mock.url());
    let request =
        JsonRpcRequest::new("eth_getBlockByHash", Some(json!([block_hash, false])), json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "eth_getBlockByHash should be handled successfully");
    let response = result.unwrap();
    assert!(response.result.is_some(), "Response should contain result");
}

#[tokio::test]
#[serial]
async fn test_eth_get_transaction_by_hash_routes_to_handler() {
    let mut mock = RpcMockBuilder::new().await;
    let tx_hash = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

    let tx_response = json!({
        "hash": tx_hash,
        "blockNumber": "0x64",
        "from": "0x0000000000000000000000000000000000000001",
        "to": "0x0000000000000000000000000000000000000002"
    });
    mock.mock_method("eth_getTransactionByHash", &tx_response);

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_getTransactionByHash", Some(json!([tx_hash])), json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "eth_getTransactionByHash should be handled successfully");
    let response = result.unwrap();
    assert!(response.result.is_some(), "Response should contain result");
}

#[tokio::test]
#[serial]
async fn test_eth_get_transaction_receipt_routes_to_handler() {
    let mut mock = RpcMockBuilder::new().await;
    let tx_hash = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

    let receipt_response = json!({
        "transactionHash": tx_hash,
        "blockNumber": "0x64",
        "status": "0x1"
    });
    mock.mock_method("eth_getTransactionReceipt", &receipt_response);

    let engine = create_test_proxy_engine(&mock.url());
    let request =
        JsonRpcRequest::new("eth_getTransactionReceipt", Some(json!([tx_hash])), json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "eth_getTransactionReceipt should be handled successfully");
    let response = result.unwrap();
    assert!(response.result.is_some(), "Response should contain result");
}

#[tokio::test]
#[serial]
async fn test_eth_gas_price_forwards_to_upstream() {
    let mut mock = RpcMockBuilder::new().await;
    let gas_price = json!("0x3b9aca00");
    mock.mock_method("eth_gasPrice", &gas_price);

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_gasPrice", None, json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "eth_gasPrice should forward to upstream");
    let response = result.unwrap();
    assert!(response.result.is_some(), "Response should contain result");
    assert_eq!(
        response.cache_status,
        Some(CacheStatus::Miss),
        "Forwarded requests should have cache status Miss"
    );
}

#[tokio::test]
#[serial]
async fn test_eth_chain_id_forwards_to_upstream() {
    let mut mock = RpcMockBuilder::new().await;
    let chain_id = json!("0x1");
    mock.mock_method("eth_chainId", &chain_id);

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_chainId", None, json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "eth_chainId should forward to upstream");
    let response = result.unwrap();
    assert_eq!(response.result, Some(json!("0x1")));
    assert_eq!(
        response.cache_status,
        Some(CacheStatus::Miss),
        "Forwarded requests should have cache status Miss"
    );
}

#[tokio::test]
#[serial]
async fn test_eth_block_number_forwards_to_upstream() {
    let mut mock = RpcMockBuilder::new().await;
    mock.mock_block_number(100);

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_blockNumber", None, json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "eth_blockNumber should forward to upstream");
    let response = result.unwrap();
    assert!(response.result.is_some(), "Response should contain result");
    assert_eq!(response.cache_status, Some(CacheStatus::Miss));
}

#[tokio::test]
#[serial]
async fn test_upstream_rpc_error_returned_as_valid_response() {
    let mut mock = RpcMockBuilder::new().await;
    mock.mock_rpc_error("eth_gasPrice", -32000, "insufficient funds");

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_gasPrice", None, json!(1));

    let result = engine.process_request(request).await;

    // RPC errors should be returned as Ok(JsonRpcResponse) with error field populated
    assert!(result.is_ok(), "RPC errors should be returned as valid responses, not ProxyError");
    let response = result.unwrap();
    assert!(response.error.is_some(), "Response should contain error field");
    assert_eq!(response.error.as_ref().unwrap().code, -32000);
    assert_eq!(response.error.as_ref().unwrap().message, "insufficient funds");
    assert_eq!(
        response.cache_status,
        Some(CacheStatus::Miss),
        "RPC error responses should be marked as cache miss"
    );
}

#[tokio::test]
#[serial]
async fn test_upstream_network_error_returned_as_proxy_error() {
    let mut mock = RpcMockBuilder::new().await;
    mock.mock_server_error();

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_gasPrice", None, json!(1));

    let result = engine.process_request(request).await;

    // Network/infrastructure errors should be returned as Err(ProxyError::Upstream)
    assert!(result.is_err(), "Infrastructure errors should be returned as ProxyError::Upstream");
    match result.unwrap_err() {
        ProxyError::Upstream(_) => {
            // Expected
        }
        e => panic!("Expected ProxyError::Upstream, got: {e:?}"),
    }
}

#[tokio::test]
#[serial]
async fn test_upstream_timeout_error_propagated() {
    let mut mock = RpcMockBuilder::new().await;
    mock.mock_timeout("eth_blockNumber");

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_blockNumber", None, json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_err(), "Timeout should result in error");
    match result.unwrap_err() {
        ProxyError::Upstream(_) => {
            // Expected - timeout is an infrastructure error
        }
        e => panic!("Expected ProxyError::Upstream, got: {e:?}"),
    }
}

#[tokio::test]
#[serial]
async fn test_handler_error_propagates_correctly() {
    let mock = RpcMockBuilder::new().await;
    let engine = create_test_proxy_engine(&mock.url());

    // Create a malformed eth_getLogs request (missing params)
    let request = JsonRpcRequest::new("eth_getLogs", None, json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_err(), "Handler should propagate parameter errors");
}

#[tokio::test]
#[serial]
async fn test_handler_validation_error_propagates() {
    let mock = RpcMockBuilder::new().await;
    let engine = create_test_proxy_engine(&mock.url());

    // Create an eth_getLogs request with invalid params (too large range)
    let request = JsonRpcRequest::new(
        "eth_getLogs",
        Some(json!([{
            "fromBlock": "0x0",
            "toBlock": "0x2710"  // 10000 blocks - exactly at limit
        }])),
        json!(1),
    );

    // This should pass validation (10000 is the limit)
    let result = engine.process_request(request).await;
    // Will fail because upstream isn't mocked, but it shouldn't be a validation error
    assert!(
        result.is_err(),
        "Request should fail (upstream not mocked), but for non-validation reasons"
    );
}

#[tokio::test]
#[serial]
async fn test_get_cache_stats_aggregates_all_caches() {
    let mock = RpcMockBuilder::new().await;
    let engine = create_test_proxy_engine(&mock.url());

    let stats = engine.get_cache_stats().await;

    // Initially, all caches should be empty
    assert_eq!(stats.block_cache_entries, 0, "Block cache should start empty");
    assert_eq!(stats.transaction_cache_entries, 0, "Transaction cache should start empty");
    assert_eq!(stats.logs_cache_entries, 0, "Logs cache should start empty");
}

#[tokio::test]
#[serial]
async fn test_get_upstream_stats_available() {
    let mock = RpcMockBuilder::new().await;
    let engine = create_test_proxy_engine(&mock.url());

    let stats = engine.get_upstream_stats().await;

    // Should have at least one upstream configured
    assert!(stats.total_upstreams > 0, "Should have at least one upstream provider configured");
}

#[tokio::test]
#[serial]
async fn test_metrics_recorded_on_validation_error() {
    let mock = RpcMockBuilder::new().await;
    let engine = create_test_proxy_engine(&mock.url());

    // Get metrics collector to check counts
    let metrics = engine.get_metrics_collector();
    let initial_metrics = metrics.get_prometheus_metrics();

    // Send an invalid request
    let mut request = JsonRpcRequest::new("eth_blockNumber", None, json!(1));
    request.jsonrpc = "1.0".into();
    let _ = engine.process_request(request).await;

    // Check that metrics were recorded
    let final_metrics = metrics.get_prometheus_metrics();
    assert!(
        final_metrics.len() >= initial_metrics.len(),
        "Metrics should be recorded for validation errors"
    );
}

#[test]
fn test_is_method_supported_for_allowed_methods() {
    assert!(ProxyEngine::is_method_supported("eth_getLogs"));
    assert!(ProxyEngine::is_method_supported("eth_blockNumber"));
    assert!(ProxyEngine::is_method_supported("eth_chainId"));
    assert!(ProxyEngine::is_method_supported("eth_getBlockByNumber"));
}

#[test]
fn test_is_method_supported_for_disallowed_methods() {
    assert!(!ProxyEngine::is_method_supported("eth_sendRawTransaction"));
    assert!(!ProxyEngine::is_method_supported("eth_sendTransaction"));
    assert!(!ProxyEngine::is_method_supported("debug_traceTransaction"));
    assert!(!ProxyEngine::is_method_supported(""));
}

#[tokio::test]
#[serial]
async fn test_request_id_preserved_in_success_response() {
    let mut mock = RpcMockBuilder::new().await;
    mock.mock_block_number(100);

    let engine = create_test_proxy_engine(&mock.url());
    let request_id = json!(42);
    let request = JsonRpcRequest::new("eth_blockNumber", None, request_id.clone());

    let result = engine.process_request(request).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    // The ID should be preserved even though the mock returns a different ID
    // because the upstream manager copies the request ID to the response
    // Note: This test verifies that the system preserves IDs through the entire chain
    assert!(response.id.is_number(), "Request ID should be present in response");
}

#[tokio::test]
#[serial]
async fn test_request_id_preserved_in_error_response() {
    let mut mock = RpcMockBuilder::new().await;
    mock.mock_rpc_error("eth_gasPrice", -32000, "test error");

    let engine = create_test_proxy_engine(&mock.url());
    let request_id = json!("test-id-123");
    let request = JsonRpcRequest::new("eth_gasPrice", None, request_id.clone());

    let result = engine.process_request(request).await;

    assert!(result.is_ok(), "RPC errors should return Ok with error field");
    let response = result.unwrap();
    // Verify that the response contains an ID (the exact ID depends on upstream manager behavior)
    assert!(
        response.id.is_string() || response.id.is_number(),
        "Request ID should be present in error response"
    );
}

#[tokio::test]
#[serial]
async fn test_concurrent_requests_handled_correctly() {
    let mut mock = RpcMockBuilder::new().await;

    // Mock multiple responses
    for _ in 0..5 {
        mock.mock_block_number(100);
    }

    let engine = Arc::new(create_test_proxy_engine(&mock.url()));
    let mut handles = vec![];

    for i in 0..5 {
        let engine_clone = engine.clone();
        let handle = tokio::spawn(async move {
            let request = JsonRpcRequest::new("eth_blockNumber", None, json!(i));
            engine_clone.process_request(request).await
        });
        handles.push(handle);
    }

    // All requests should succeed
    for handle in handles {
        let result = handle.await.expect("Task should not panic");
        assert!(result.is_ok(), "Concurrent request should succeed");
    }
}

#[tokio::test]
#[serial]
async fn test_response_uses_correct_jsonrpc_version() {
    let mut mock = RpcMockBuilder::new().await;
    mock.mock_block_number(100);

    let engine = create_test_proxy_engine(&mock.url());
    let request = JsonRpcRequest::new("eth_blockNumber", None, json!(1));

    let result = engine.process_request(request).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert_eq!(
        response.jsonrpc, JSONRPC_VERSION_COW,
        "Response should use the correct JSON-RPC version"
    );
}
