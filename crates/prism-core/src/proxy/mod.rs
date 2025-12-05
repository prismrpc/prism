//! Proxy module for handling RPC request processing and routing.
//!
//! This module contains the core proxy engine that processes incoming JSON-RPC requests,
//! manages caching, handles upstream provider communication, and collects metrics.
//!
//! # Main Components
//!
//! - `ProxyEngine`: Main request processing engine with caching and upstream management
//! - `ProxyError`: Error types for request processing failures
//! - `PartialResult`: Partial success handling for range-based operations
//! - Request handlers for specific RPC methods (blocks, transactions, logs)
//! - Utility functions for converting between internal and RPC formats
//!
//! # Request Processing Flow
//!
//! ```text
//! Client Request
//!       │
//!       ▼
//! ┌─────────────┐
//! │  Validation │ ─── Invalid ──► Error Response
//! └──────┬──────┘
//!        │ Valid
//!        ▼
//! ┌─────────────────┐
//! │   ProxyEngine   │
//! │  process_request│
//! └────────┬────────┘
//!          │
//!    ┌─────┴─────┐
//!    │  Method?  │
//!    └─────┬─────┘
//!          │
//!    ┌─────┼─────────┬──────────────┬────────────┐
//!    ▼     ▼         ▼              ▼            ▼
//!  Logs  Blocks   Transactions   Forward     Other
//! Handler Handler   Handler     Upstream    Methods
//!    │     │         │              │            │
//!    ▼     ▼         ▼              ▼            ▼
//!  Cache  Cache    Cache         No Cache   Forward
//!  Check  Check    Check
//! ```
//!
//! # Handler Selection
//!
//! | Method | Handler | Caching Strategy |
//! |--------|---------|------------------|
//! | `eth_getLogs` | `LogsHandler` | Partial range fulfillment |
//! | `eth_getBlockByNumber` | `BlocksHandler` | Hot window + LRU |
//! | `eth_getBlockByHash` | `BlocksHandler` | Hash-keyed lookup |
//! | `eth_getTransactionByHash` | `TransactionsHandler` | LRU cache |
//! | `eth_getTransactionReceipt` | `TransactionsHandler` | LRU cache |
//! | Other allowed methods | Forward | No caching |
//!
//! # `SharedContext` Pattern
//!
//! Handlers receive `Arc<SharedContext>` containing shared references to reduce
//! Arc cloning overhead. Contains: `cache_manager`, `upstream_manager`, metrics.

pub mod engine;
pub mod errors;
pub mod handlers;
pub mod partial;
pub mod utils;

pub use engine::{CacheStats, ProxyEngine};
pub use errors::ProxyError;
pub use partial::{PartialResult, RangeError, RangeFailure, RetryConfig};
pub use utils::log_filter_to_json_params;

#[cfg(test)]
#[allow(clippy::single_match, clippy::match_same_arms, clippy::doc_markdown)]
mod tests {
    use super::*;
    use crate::{
        cache::{CacheManager, CacheManagerConfig},
        metrics::MetricsCollector,
        middleware::validation::ValidationError,
        types::JsonRpcRequest,
    };
    use std::sync::Arc;

    /// Helper to create a test proxy engine with no upstreams
    fn create_test_proxy() -> ProxyEngine {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let manager = Arc::new(
            crate::upstream::UpstreamManagerBuilder::new()
                .chain_state(chain_state.clone())
                .build()
                .unwrap(),
        );
        let metrics = Arc::new(MetricsCollector::new().unwrap());
        let cache = Arc::new(
            CacheManager::new(&CacheManagerConfig::default(), chain_state)
                .expect("valid test cache config"),
        );
        ProxyEngine::new(cache, manager, metrics)
    }

    /// Helper to create a request with custom jsonrpc version
    fn test_req_with_version(
        jsonrpc: &str,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: std::borrow::Cow::Owned(jsonrpc.to_string()),
            method: method.to_string(),
            params,
            id: Arc::new(serde_json::json!(1)),
        }
    }

    #[tokio::test]
    async fn test_method_support() {
        assert!(ProxyEngine::is_method_supported("eth_getBlockByNumber"));
        assert!(ProxyEngine::is_method_supported("eth_getLogs"));
        assert!(ProxyEngine::is_method_supported("eth_getTransactionByHash"));
        assert!(ProxyEngine::is_method_supported("net_version"));

        assert!(!ProxyEngine::is_method_supported("eth_unsupported"));
        assert!(!ProxyEngine::is_method_supported("debug_traceTransaction"));
    }

    /// Tests all allowed methods for completeness
    #[tokio::test]
    async fn test_all_allowed_methods() {
        let allowed_methods = [
            "net_version",
            "eth_blockNumber",
            "eth_chainId",
            "eth_gasPrice",
            "eth_getBalance",
            "eth_getBlockByHash",
            "eth_getBlockByNumber",
            "eth_getLogs",
            "eth_getTransactionByHash",
            "eth_getTransactionReceipt",
            "eth_getCode",
        ];

        for method in allowed_methods {
            assert!(
                ProxyEngine::is_method_supported(method),
                "Method {method} should be supported"
            );
        }
    }

    /// Tests that case sensitivity is enforced for method names
    #[tokio::test]
    async fn test_method_support_case_sensitivity() {
        // Method names are case-sensitive per JSON-RPC spec
        assert!(!ProxyEngine::is_method_supported("ETH_GETBLOCKBYNUMBER"));
        assert!(!ProxyEngine::is_method_supported("Eth_GetBlockByNumber"));
        assert!(!ProxyEngine::is_method_supported("eth_getblockbynumber"));
    }

    #[tokio::test]
    async fn test_proxy_engine_no_upstreams() {
        let proxy = create_test_proxy();

        let request = JsonRpcRequest::new(
            "eth_getBlockByNumber",
            Some(serde_json::json!(["0x1234", false])),
            serde_json::json!(1),
        );

        let result = proxy.process_request(request).await;
        assert!(result.is_err(), "Request should fail without upstreams");
    }

    /// Tests that the `ProxyEngine` can be constructed with all required dependencies.
    #[tokio::test]
    async fn test_proxy_engine_initialization() {
        let _proxy = create_test_proxy();
    }

    /// Tests that SharedContext is properly initialized and shared
    #[tokio::test]
    async fn test_shared_context_initialization() {
        let proxy = create_test_proxy();

        // Verify we can access cache manager through the proxy
        let _cache_manager = proxy.get_cache_manager();
        let _metrics_collector = proxy.get_metrics_collector();
    }

    #[tokio::test]
    async fn test_process_request_invalid_version() {
        let proxy = create_test_proxy();
        let request = test_req_with_version(
            "1.0",
            "eth_getBlockByNumber",
            Some(serde_json::json!(["0x1234", false])),
        );

        let result = proxy.process_request(request).await;
        assert!(result.is_err(), "Request with invalid version should fail");

        match result.unwrap_err() {
            ProxyError::Validation(ValidationError::InvalidVersion(version)) => {
                assert_eq!(version, "1.0");
            }
            err => panic!("Expected InvalidVersion error, got: {err:?}"),
        }
    }

    /// Tests that validation errors are properly returned for invalid method names
    #[tokio::test]
    async fn test_process_request_invalid_method_chars() {
        let proxy = create_test_proxy();

        // Method names should only contain alphanumeric and underscore
        let invalid_methods = [
            "eth.getBlock", // dot
            "eth-getBlock", // hyphen
            "eth/getBlock", // slash
            "eth getBlock", // space
            "eth@getBlock", // at symbol
            "eth:getBlock", // colon
        ];

        for method in invalid_methods {
            let request = JsonRpcRequest::new(
                method,
                Some(serde_json::json!(["0x1234", false])),
                serde_json::json!(1),
            );

            let result = proxy.process_request(request).await;
            assert!(result.is_err(), "Method '{method}' should fail validation");

            match result.unwrap_err() {
                ProxyError::Validation(ValidationError::InvalidMethod(m)) => {
                    assert_eq!(m, method);
                }
                err => panic!("Expected InvalidMethod error for '{method}', got: {err:?}"),
            }
        }
    }

    /// Tests that MethodNotSupported is returned for unsupported methods
    #[tokio::test]
    async fn test_process_request_method_not_supported() {
        let proxy = create_test_proxy();

        // Valid method name format but not in allowed list
        // Note: validation checks is_method_allowed first, so we need a method
        // that passes the character check but isn't allowed
        let request = JsonRpcRequest::new(
            "debug_traceTransaction", // Valid format, not allowed
            Some(serde_json::json!(["0x1234"])),
            serde_json::json!(1),
        );

        let result = proxy.process_request(request).await;
        assert!(result.is_err(), "Unsupported method should fail");

        // Note: Validation happens before method support check, so this will
        // actually fail with MethodNotAllowed from validation
        match result.unwrap_err() {
            ProxyError::Validation(ValidationError::MethodNotAllowed(method)) => {
                assert_eq!(method, "debug_traceTransaction");
            }
            err => panic!("Expected MethodNotAllowed error, got: {err:?}"),
        }
    }

    /// Tests that block range validation is enforced for eth_getLogs
    #[tokio::test]
    async fn test_process_request_block_range_too_large() {
        let proxy = create_test_proxy();

        // Block range exceeds 10000 limit: 0x64 (100) to 0x2775 (10101) = 10001 blocks
        let request = JsonRpcRequest::new(
            "eth_getLogs",
            Some(serde_json::json!([{
                "fromBlock": "0x64",
                "toBlock": "0x2775"
            }])),
            serde_json::json!(1),
        );

        let result = proxy.process_request(request).await;
        assert!(result.is_err(), "Request with too large block range should fail");

        match result.unwrap_err() {
            ProxyError::Validation(ValidationError::BlockRangeTooLarge(range)) => {
                assert_eq!(range, 10001);
            }
            err => panic!("Expected BlockRangeTooLarge error, got: {err:?}"),
        }
    }

    /// Tests that topic count validation is enforced for eth_getLogs
    #[tokio::test]
    async fn test_process_request_too_many_topics() {
        let proxy = create_test_proxy();

        // More than 4 topics not allowed
        let request = JsonRpcRequest::new(
            "eth_getLogs",
            Some(serde_json::json!([{
                "fromBlock": "0x64",
                "toBlock": "0x6e",
                "topics": ["0x1", "0x2", "0x3", "0x4", "0x5"]
            }])),
            serde_json::json!(1),
        );

        let result = proxy.process_request(request).await;
        assert!(result.is_err(), "Request with too many topics should fail");

        match result.unwrap_err() {
            ProxyError::Validation(ValidationError::TooManyTopics(count)) => {
                assert_eq!(count, 5);
            }
            err => panic!("Expected TooManyTopics error, got: {err:?}"),
        }
    }

    /// Tests that invalid block parameters are rejected
    #[tokio::test]
    async fn test_process_request_invalid_block_parameter() {
        let proxy = create_test_proxy();

        let request = JsonRpcRequest::new(
            "eth_getBlockByNumber",
            Some(serde_json::json!(["invalid_block", false])),
            serde_json::json!(1),
        );

        let result = proxy.process_request(request).await;
        assert!(result.is_err(), "Request with invalid block parameter should fail");

        match result.unwrap_err() {
            ProxyError::Validation(ValidationError::InvalidBlockParameter(param)) => {
                assert_eq!(param, "invalid_block");
            }
            err => panic!("Expected InvalidBlockParameter error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_process_request_valid_passes_validation() {
        let proxy = create_test_proxy();

        let request = JsonRpcRequest::new(
            "eth_getBlockByNumber",
            Some(serde_json::json!(["latest", false])),
            serde_json::json!(1),
        );

        // Should pass validation but fail at upstream (no upstreams configured)
        let result = proxy.process_request(request).await;

        // Expect upstream error, not validation error
        match result {
            Err(ProxyError::Upstream(_)) => {} // Expected - no upstreams
            Err(ProxyError::Validation(_)) => panic!("Valid request should not fail validation"),
            Ok(_) => panic!("Request should fail without upstreams"),
            Err(err) => panic!("Unexpected error type: {err:?}"),
        }
    }

    /// Tests that valid block tags are accepted
    #[tokio::test]
    async fn test_process_request_valid_block_tags() {
        let proxy = create_test_proxy();
        let valid_tags = ["latest", "earliest", "pending", "safe", "finalized"];

        for tag in valid_tags {
            let request = JsonRpcRequest::new(
                "eth_getBlockByNumber",
                Some(serde_json::json!([tag, false])),
                serde_json::json!(1),
            );

            let result = proxy.process_request(request).await;

            // Should fail at upstream level, not validation
            match result {
                Err(ProxyError::Validation(_)) => {
                    panic!("Block tag '{tag}' should pass validation")
                }
                _ => {} // Expected to fail for other reasons (no upstream)
            }
        }
    }

    /// Tests that valid hex block numbers are accepted
    #[tokio::test]
    async fn test_process_request_valid_hex_block_numbers() {
        let proxy = create_test_proxy();
        let valid_hexes = ["0x0", "0x1234", "0xabcdef", "0x1234567890abcdef"];

        for hex in valid_hexes {
            let request = JsonRpcRequest::new(
                "eth_getBlockByNumber",
                Some(serde_json::json!([hex, false])),
                serde_json::json!(1),
            );

            let result = proxy.process_request(request).await;

            // Should fail at upstream level, not validation
            match result {
                Err(ProxyError::Validation(_)) => {
                    panic!("Hex block number '{hex}' should pass validation")
                }
                _ => {} // Expected to fail for other reasons (no upstream)
            }
        }
    }

    #[tokio::test]
    async fn test_get_cache_stats() {
        let proxy = create_test_proxy();

        let stats = proxy.get_cache_stats().await;

        // Fresh proxy should have empty caches
        assert_eq!(stats.block_cache_entries, 0);
        assert_eq!(stats.transaction_cache_entries, 0);
        assert_eq!(stats.logs_cache_entries, 0);
    }

    /// Tests that upstream stats can be retrieved
    #[tokio::test]
    async fn test_get_upstream_stats() {
        let proxy = create_test_proxy();

        let stats = proxy.get_upstream_stats().await;

        // Fresh proxy should have no upstreams
        assert_eq!(stats.total_upstreams, 0);
        assert_eq!(stats.healthy_upstreams, 0);
    }

    #[tokio::test]
    async fn test_request_id_preserved_in_validation_error() {
        let proxy = create_test_proxy();

        // Create request with specific ID
        let request_id = serde_json::json!({"custom": "id", "number": 42});
        let request = JsonRpcRequest::new(
            "debug_unsupported_method",
            Some(serde_json::json!([])),
            request_id.clone(),
        );

        // Store the original ID for comparison
        let original_id = Arc::clone(&request.id);

        let result = proxy.process_request(request).await;
        assert!(result.is_err());

        // The ID should still be accessible (for error response construction)
        // This verifies Arc::clone pattern is used correctly
        assert_eq!(*original_id, request_id);
    }

    /// Tests various request ID types are handled
    #[tokio::test]
    async fn test_request_id_types() {
        let proxy = create_test_proxy();

        let id_types = [
            serde_json::json!(1),
            serde_json::json!("string_id"),
            serde_json::json!(null),
            serde_json::json!(12_345_678_901_234_567_890_u64),
        ];

        for id in id_types {
            let request = JsonRpcRequest::new("eth_blockNumber", None, id.clone());

            let result = proxy.process_request(request).await;
            // Request should pass validation (fail at upstream)
            if let Err(ProxyError::Validation(_)) = result {
                panic!("Request with ID {id:?} should pass validation");
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_request_processing() {
        let proxy = Arc::new(create_test_proxy());

        let mut handles = Vec::new();

        // Spawn 20 concurrent requests
        for i in 0..20 {
            let proxy_clone = Arc::clone(&proxy);
            let handle = tokio::spawn(async move {
                let request = JsonRpcRequest::new("eth_blockNumber", None, serde_json::json!(i));
                proxy_clone.process_request(request).await
            });
            handles.push(handle);
        }

        // All requests should complete (fail at upstream, but not panic)
        for handle in handles {
            let result = handle.await.expect("Task should not panic");
            // Should fail at upstream level (no upstreams), not crash
            match result {
                Err(ProxyError::Upstream(_)) => {} // Expected
                Err(ProxyError::Validation(_)) => panic!("Should not fail validation"),
                Ok(_) => {} // Unexpected but not a problem
                Err(err) => panic!("Unexpected error type: {err:?}"),
            }
        }
    }

    /// Tests that mixed valid/invalid requests are handled concurrently
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_mixed_requests() {
        let proxy = Arc::new(create_test_proxy());

        let mut handles = Vec::new();

        for i in 0..10 {
            let proxy_clone = Arc::clone(&proxy);
            let handle = tokio::spawn(async move {
                let request = if i % 2 == 0 {
                    // Valid request
                    JsonRpcRequest::new("eth_blockNumber", None, serde_json::json!(i))
                } else {
                    // Invalid request (bad version)
                    JsonRpcRequest {
                        jsonrpc: std::borrow::Cow::Owned("1.0".to_string()),
                        method: "eth_blockNumber".to_string(),
                        params: None,
                        id: Arc::new(serde_json::json!(i)),
                    }
                };
                (i, proxy_clone.process_request(request).await)
            });
            handles.push(handle);
        }

        for handle in handles {
            let (i, result) = handle.await.expect("Task should not panic");
            if i % 2 == 0 {
                // Valid requests should fail at upstream
                match result {
                    Err(ProxyError::Upstream(_)) => {}
                    Err(ProxyError::Validation(_)) => {
                        panic!("Valid request {i} should not fail validation")
                    }
                    Ok(_) => {}
                    Err(err) => panic!("Unexpected error for request {i}: {err:?}"),
                }
            } else {
                // Invalid requests should fail validation
                match result {
                    Err(ProxyError::Validation(ValidationError::InvalidVersion(_))) => {}
                    Err(err) => {
                        panic!("Invalid request {i} should fail with InvalidVersion, got: {err:?}")
                    }
                    Ok(_) => panic!("Invalid request {i} should not succeed"),
                }
            }
        }
    }

    #[tokio::test]
    async fn test_empty_method_name() {
        let proxy = create_test_proxy();

        let request = JsonRpcRequest::new("", None, serde_json::json!(1));

        let result = proxy.process_request(request).await;
        assert!(result.is_err(), "Empty method name should fail");
    }

    /// Tests request with no params for methods that support it
    #[tokio::test]
    async fn test_request_without_params() {
        let proxy = create_test_proxy();

        // eth_blockNumber doesn't require params
        let request = JsonRpcRequest::new("eth_blockNumber", None, serde_json::json!(1));

        let result = proxy.process_request(request).await;
        // Should fail at upstream level, not validation
        match result {
            Err(ProxyError::Validation(_)) => {
                panic!("eth_blockNumber without params should pass validation")
            }
            _ => {} // Expected to fail for other reasons
        }
    }

    /// Tests valid block range at boundary (exactly 10000 blocks)
    #[tokio::test]
    async fn test_block_range_at_boundary() {
        let proxy = create_test_proxy();

        // Exactly 10000 blocks: 0x0 to 0x2710 (10000 in decimal) = 10000 block range
        let request = JsonRpcRequest::new(
            "eth_getLogs",
            Some(serde_json::json!([{
                "fromBlock": "0x0",
                "toBlock": "0x2710"
            }])),
            serde_json::json!(1),
        );

        let result = proxy.process_request(request).await;
        // Should fail at upstream level, not validation (10000 is within limit)
        match result {
            Err(ProxyError::Validation(ValidationError::BlockRangeTooLarge(_))) => {
                panic!("Block range of 10000 should be valid")
            }
            _ => {} // Expected to fail for other reasons
        }
    }

    /// Tests exactly 4 topics (maximum allowed)
    #[tokio::test]
    async fn test_topics_at_boundary() {
        let proxy = create_test_proxy();

        // Exactly 4 topics (maximum allowed)
        let request = JsonRpcRequest::new(
            "eth_getLogs",
            Some(serde_json::json!([{
                "fromBlock": "0x64",
                "toBlock": "0x6e",
                "topics": ["0x1", "0x2", "0x3", "0x4"]
            }])),
            serde_json::json!(1),
        );

        let result = proxy.process_request(request).await;
        // Should fail at upstream level, not validation (4 topics is valid)
        if let Err(ProxyError::Validation(ValidationError::TooManyTopics(_))) = result {
            panic!("4 topics should be valid");
        }
    }
}
