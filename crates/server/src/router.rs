use axum::{http::StatusCode, response::IntoResponse, Json};
use prism_core::{
    proxy::ProxyEngine,
    types::{CacheStatus, JsonRpcError, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION_COW},
};
use serde_json::Value;
use std::sync::Arc;
use tracing::info;

type BatchResponse = (StatusCode, [(&'static str, String); 1], Json<Value>);

/// Handles JSON-RPC requests (single or batched).
///
/// Per JSON-RPC 2.0 spec, a batch request is an array of request objects.
/// This handler detects arrays vs objects and processes accordingly.
///
/// # Panics
///
/// Panics if `JsonRpcResponse` serialization fails, which should never occur as the type
/// is guaranteed to be serializable.
pub async fn handle_rpc(
    axum::extract::State(proxy_engine): axum::extract::State<Arc<ProxyEngine>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    if payload.is_array() {
        handle_batch_request(proxy_engine, payload).await
    } else {
        handle_single_request(proxy_engine, payload).await
    }
}

async fn handle_single_request(proxy_engine: Arc<ProxyEngine>, payload: Value) -> BatchResponse {
    let request: JsonRpcRequest = match serde_json::from_value(payload) {
        Ok(req) => req,
        Err(e) => {
            let error_response = JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("Parse error: {e}"),
                    data: None,
                }),
                id: Arc::new(serde_json::Value::Null),
                cache_status: None,
                serving_upstream: None,
            };
            return (
                StatusCode::BAD_REQUEST,
                [("x-cache-status", "MISS".to_string())],
                Json(
                    serde_json::to_value(error_response)
                        .expect("JsonRpcResponse serialization cannot fail"),
                ),
            );
        }
    };

    match proxy_engine.process_request(request).await {
        Ok(response) => {
            let cache_status_header = response
                .cache_status
                .as_ref()
                .map_or_else(|| "MISS".to_string(), std::string::ToString::to_string);

            (
                StatusCode::OK,
                [("x-cache-status", cache_status_header)],
                Json(
                    serde_json::to_value(response)
                        .expect("JsonRpcResponse serialization cannot fail"),
                ),
            )
        }
        Err(e) => {
            let error_response = JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: None,
                error: Some(JsonRpcError {
                    code: match e {
                        prism_core::proxy::ProxyError::InvalidRequest(_) => -32600,
                        prism_core::proxy::ProxyError::MethodNotSupported(_) => -32601,
                        _ => -32603,
                    },
                    message: e.to_string(),
                    data: None,
                }),
                id: Arc::new(serde_json::Value::Null),
                cache_status: None,
                serving_upstream: None,
            };

            (
                StatusCode::BAD_REQUEST,
                [("x-cache-status", "MISS".to_string())],
                Json(
                    serde_json::to_value(error_response)
                        .expect("JsonRpcResponse serialization cannot fail"),
                ),
            )
        }
    }
}

/// Validates that the payload is an array and extracts it.
/// Returns an error response if validation fails.
fn validate_batch_payload(payload: Value) -> Result<Vec<Value>, BatchResponse> {
    if let Value::Array(arr) = payload {
        Ok(arr)
    } else {
        let error_response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: None,
            error: Some(JsonRpcError {
                code: -32600,
                message: "Invalid Request: expected array for batch".to_string(),
                data: None,
            }),
            id: Arc::new(serde_json::Value::Null),
            cache_status: None,
            serving_upstream: None,
        };
        Err((
            StatusCode::BAD_REQUEST,
            [("x-cache-status", "MISS".to_string())],
            Json(
                serde_json::to_value(error_response)
                    .expect("JsonRpcResponse serialization cannot fail"),
            ),
        ))
    }
}

/// Processes a single item in a batch request.
/// Returns a tuple of (`response_value`, `has_cache_hit`).
async fn process_batch_item(
    proxy_engine: Arc<ProxyEngine>,
    item: Value,
    item_id: Arc<Value>,
) -> (Value, bool) {
    if let Ok(request) = serde_json::from_value::<JsonRpcRequest>(item) {
        match proxy_engine.process_request(request).await {
            Ok(response) => {
                let has_cache_hit = response.cache_status.as_ref().is_some_and(|status| {
                    matches!(status, CacheStatus::Full | CacheStatus::Partial | CacheStatus::Empty)
                });

                let response_value = serde_json::to_value(response)
                    .expect("JsonRpcResponse serialization cannot fail");

                (response_value, has_cache_hit)
            }
            Err(e) => {
                let error_response = JsonRpcResponse {
                    jsonrpc: JSONRPC_VERSION_COW,
                    result: Some(serde_json::Value::Null),
                    error: Some(JsonRpcError { code: -32603, message: e.to_string(), data: None }),
                    id: item_id,
                    cache_status: None,
                    serving_upstream: None,
                };
                let response_value = serde_json::to_value(error_response)
                    .expect("JsonRpcResponse serialization cannot fail");

                (response_value, false)
            }
        }
    } else {
        let error_response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: Some(serde_json::Value::Null),
            error: Some(JsonRpcError {
                code: -32600,
                message: "Invalid request".to_string(),
                data: None,
            }),
            id: item_id,
            cache_status: None,
            serving_upstream: None,
        };
        let response_value = serde_json::to_value(error_response)
            .expect("JsonRpcResponse serialization cannot fail");

        (response_value, false)
    }
}

/// Aggregates batch results and determines the cache status header.
fn aggregate_batch_results(results: Vec<(Value, bool)>) -> (Vec<Value>, String) {
    let mut responses = Vec::with_capacity(results.len());
    let mut has_cache_hit = false;

    for (response_value, cache_hit) in results {
        responses.push(response_value);
        has_cache_hit = has_cache_hit || cache_hit;
    }

    let cache_status_header = if has_cache_hit {
        "PARTIAL".to_string()
    } else {
        "MISS".to_string()
    };

    (responses, cache_status_header)
}

async fn handle_batch_request(proxy_engine: Arc<ProxyEngine>, payload: Value) -> BatchResponse {
    // Validate payload and extract array
    let items = match validate_batch_payload(payload) {
        Ok(arr) => arr,
        Err(error_response) => return error_response,
    };

    let start_time = std::time::Instant::now();
    let batch_size = items.len();
    info!("Received batched RPC request with {} items", batch_size);

    // Pre-allocate response vector with exact capacity to avoid reallocation
    let mut futures = Vec::with_capacity(batch_size);

    // Spawn concurrent tasks for all requests - no cloning needed as we own the items
    for item in items {
        let proxy_engine = Arc::clone(&proxy_engine);

        // Extract item ID early for error handling (before item is moved)
        let item_id = if let Value::Object(ref map) = item {
            Arc::new(map.get("id").cloned().unwrap_or(Value::Null))
        } else {
            Arc::new(Value::Null)
        };

        // Process each item concurrently
        let future = process_batch_item(proxy_engine, item, item_id);
        futures.push(future);
    }

    // Execute all requests concurrently and wait for all to complete
    // This preserves order as futures::join_all maintains input order
    let results = futures::future::join_all(futures).await;

    // Aggregate results with minimal allocations
    let (responses, cache_status_header) = aggregate_batch_results(results);

    #[allow(clippy::cast_possible_truncation)]
    let duration_ms = start_time.elapsed().as_millis() as u64;
    proxy_engine
        .get_metrics_collector()
        .record_batch_request(batch_size, duration_ms);

    (
        StatusCode::OK,
        [("x-cache-status", cache_status_header)],
        Json(serde_json::to_value(responses).expect("Vec<Value> serialization cannot fail")),
    )
}

#[allow(clippy::unused_async)]
pub async fn handle_metrics(
    axum::extract::State(proxy_engine): axum::extract::State<Arc<ProxyEngine>>,
) -> impl IntoResponse {
    let metrics_collector = proxy_engine.get_metrics_collector().clone();
    let prometheus_metrics = metrics_collector.get_prometheus_metrics();

    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        prometheus_metrics,
    )
}

pub async fn handle_health(
    axum::extract::State(proxy_engine): axum::extract::State<Arc<ProxyEngine>>,
) -> impl IntoResponse {
    let upstream_stats = proxy_engine.get_upstream_stats().await;
    let cache_stats = proxy_engine.get_cache_stats().await;

    let health_status = serde_json::json!({
        "status": if upstream_stats.healthy_upstreams > 0 { "healthy" } else { "unhealthy" },
        "upstreams": {
            "total": upstream_stats.total_upstreams,
            "healthy": upstream_stats.healthy_upstreams,
            "average_response_time_ms": upstream_stats.average_response_time_ms
        },
        "cache": {
            "block_entries": cache_stats.block_cache_entries,
            "transaction_entries": cache_stats.transaction_cache_entries,
            "logs_entries": cache_stats.logs_cache_entries
        },
        "timestamp": chrono::Utc::now().to_rfc3339()
    });

    (
        if upstream_stats.healthy_upstreams > 0 {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        [("content-type", "application/json")],
        serde_json::to_string(&health_status).unwrap_or_default(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::State, http::StatusCode};
    use prism_core::{
        cache::CacheManager,
        chain::ChainState,
        metrics::MetricsCollector,
        proxy::ProxyEngine,
        types::{JsonRpcRequest, ALLOWED_METHODS},
    };
    use serde_json::Value;
    use std::sync::Arc;

    fn create_test_proxy_engine() -> Arc<ProxyEngine> {
        let cache_config = prism_core::cache::manager::CacheManagerConfig::default();
        let chain_state = Arc::new(ChainState::new());
        let cache_manager = Arc::new(
            CacheManager::new(&cache_config, chain_state.clone()).expect("valid test cache config"),
        );
        let upstream_manager = Arc::new(
            prism_core::upstream::UpstreamManagerBuilder::new()
                .chain_state(chain_state)
                .build()
                .expect("valid test upstream config"),
        );
        let metrics_collector = Arc::new(MetricsCollector::new().expect("valid test metrics"));
        let alert_manager = Arc::new(prism_core::alerts::AlertManager::new());

        Arc::new(ProxyEngine::new(
            cache_manager,
            upstream_manager,
            metrics_collector,
            alert_manager,
        ))
    }

    async fn body_to_bytes(body: Body) -> Vec<u8> {
        use axum::body::to_bytes;
        to_bytes(body, usize::MAX).await.unwrap().to_vec()
    }

    async fn body_to_json(body: Body) -> Value {
        let bytes = body_to_bytes(body).await;
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn test_json_rpc_parsing() {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getBlockByNumber",
            "params": ["0x123"],
            "id": 1
        });

        let request: Result<JsonRpcRequest, _> = serde_json::from_value(payload);
        assert!(request.is_ok());

        let request = request.unwrap();
        assert_eq!(request.method, "eth_getBlockByNumber");
        assert_eq!(request.jsonrpc, "2.0");
    }

    #[tokio::test]
    async fn test_handle_rpc_invalid_request() {
        let proxy_engine = create_test_proxy_engine();

        // Invalid jsonrpc version (not "2.0")
        let request = serde_json::json!({
            "jsonrpc": "",
            "method": "eth_blockNumber",
            "id": 1
        });

        let response = handle_rpc(State(proxy_engine), Json(request)).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);

        let cache_header =
            parts.headers.get("x-cache-status").and_then(|v| v.to_str().ok()).unwrap();
        assert_eq!(cache_header, "MISS");

        let body_json = body_to_json(body).await;
        assert!(body_json.get("error").is_some());

        let error_code = body_json["error"]["code"].as_i64().unwrap();
        assert_eq!(error_code, -32603);
    }

    #[tokio::test]
    async fn test_handle_rpc_method_not_supported() {
        let proxy_engine = create_test_proxy_engine();

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_unsupportedMethod",
            "id": 1
        });

        let response = handle_rpc(State(proxy_engine), Json(request)).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);

        let cache_header =
            parts.headers.get("x-cache-status").and_then(|v| v.to_str().ok()).unwrap();
        assert_eq!(cache_header, "MISS");

        let body_json = body_to_json(body).await;
        let error_code = body_json["error"]["code"].as_i64().unwrap();
        assert_eq!(error_code, -32603);
    }

    #[tokio::test]
    async fn test_handle_rpc_cache_status_headers() {
        let proxy_engine = create_test_proxy_engine();

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "id": 1
        });

        let response = handle_rpc(State(proxy_engine), Json(request)).await;
        let (parts, _) = response.into_response().into_parts();

        assert!(parts.headers.contains_key("x-cache-status"));
        let cache_header =
            parts.headers.get("x-cache-status").and_then(|v| v.to_str().ok()).unwrap();

        assert!(
            cache_header == "MISS" ||
                cache_header == "FULL" ||
                cache_header == "PARTIAL" ||
                cache_header == "EMPTY"
        );
    }

    #[tokio::test]
    async fn test_handle_metrics_returns_ok() {
        let proxy_engine = create_test_proxy_engine();

        let response = handle_metrics(State(proxy_engine)).await;
        let (parts, _) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::OK);

        let content_type = parts.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap();
        assert_eq!(content_type, "text/plain; version=0.0.4; charset=utf-8");
    }

    #[tokio::test]
    async fn test_handle_metrics_prometheus_format() {
        let proxy_engine = create_test_proxy_engine();

        let response = handle_metrics(State(proxy_engine)).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::OK);

        let body_bytes = body_to_bytes(body).await;
        let body_str = String::from_utf8(body_bytes).unwrap();

        let _ = body_str;
    }

    #[tokio::test]
    async fn test_handle_health_no_upstreams() {
        let proxy_engine = create_test_proxy_engine();

        let response = handle_health(State(proxy_engine)).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::SERVICE_UNAVAILABLE);

        let content_type = parts.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap();
        assert_eq!(content_type, "application/json");

        let body_bytes = body_to_bytes(body).await;
        let body_str = String::from_utf8(body_bytes).unwrap();
        let health_json: Value = serde_json::from_str(&body_str).unwrap();

        assert_eq!(health_json["status"], "unhealthy");
        assert_eq!(health_json["upstreams"]["healthy"], 0);
        assert!(health_json.get("timestamp").is_some());
    }

    #[tokio::test]
    async fn test_handle_health_response_structure() {
        let proxy_engine = create_test_proxy_engine();

        let response = handle_health(State(proxy_engine)).await;
        let (_, body) = response.into_response().into_parts();

        let health_json = body_to_json(body).await;

        assert!(health_json.get("status").is_some());
        assert!(health_json.get("upstreams").is_some());
        assert!(health_json.get("cache").is_some());
        assert!(health_json.get("timestamp").is_some());

        let upstreams = &health_json["upstreams"];
        assert!(upstreams.get("total").is_some());
        assert!(upstreams.get("healthy").is_some());
        assert!(upstreams.get("average_response_time_ms").is_some());

        let cache = &health_json["cache"];
        assert!(cache.get("block_entries").is_some());
        assert!(cache.get("transaction_entries").is_some());
        assert!(cache.get("logs_entries").is_some());
    }

    #[tokio::test]
    async fn test_handle_rpc_batched_multiple_requests() {
        let proxy_engine = create_test_proxy_engine();

        let batch = serde_json::json!([
            {
                "jsonrpc": "2.0",
                "method": "eth_blockNumber",
                "params": [],
                "id": 1
            },
            {
                "jsonrpc": "2.0",
                "method": "eth_chainId",
                "params": [],
                "id": 2
            }
        ]);

        let response = handle_rpc(State(proxy_engine), Json(batch)).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::OK);

        assert!(parts.headers.contains_key("x-cache-status"));

        let responses: Vec<Value> = body_to_json(body).await.as_array().unwrap().clone();
        assert_eq!(responses.len(), 2);
    }

    #[tokio::test]
    async fn test_handle_rpc_batched_preserves_request_ids() {
        let proxy_engine = create_test_proxy_engine();

        let batch = serde_json::json!([
            {
                "jsonrpc": "2.0",
                "method": "eth_blockNumber",
                "params": [],
                "id": 42
            },
            {
                "jsonrpc": "2.0",
                "method": "eth_chainId",
                "params": [],
                "id": "test-id"
            }
        ]);

        let response = handle_rpc(State(proxy_engine), Json(batch)).await;
        let (_, body) = response.into_response().into_parts();

        let responses: Vec<Value> = body_to_json(body).await.as_array().unwrap().clone();

        assert!(responses.iter().any(|r| r["id"] == 42 || r["id"] == "test-id"));
    }

    #[tokio::test]
    async fn test_handle_rpc_batched_invalid_request_in_batch() {
        let proxy_engine = create_test_proxy_engine();

        let batch = serde_json::json!([
            {
                "jsonrpc": "2.0",
                "method": "eth_blockNumber",
                "params": [],
                "id": 1
            },
            {
                "invalid": "request",
                "id": 2
            }
        ]);

        let response = handle_rpc(State(proxy_engine), Json(batch)).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::OK);

        let responses: Vec<Value> = body_to_json(body).await.as_array().unwrap().clone();
        assert_eq!(responses.len(), 2);

        let second_response = &responses[1];
        assert!(second_response.get("error").is_some());
        assert_eq!(second_response["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn test_handle_rpc_batched_cache_status_aggregation() {
        let proxy_engine = create_test_proxy_engine();

        let batch = serde_json::json!([{
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1
        }]);

        let response = handle_rpc(State(proxy_engine), Json(batch)).await;
        let (parts, _) = response.into_response().into_parts();

        let cache_header =
            parts.headers.get("x-cache-status").and_then(|v| v.to_str().ok()).unwrap();

        assert!(cache_header == "MISS" || cache_header == "PARTIAL");
    }

    #[tokio::test]
    async fn test_handle_rpc_batched_empty_batch() {
        let proxy_engine = create_test_proxy_engine();

        let batch = serde_json::json!([]);

        let response = handle_rpc(State(proxy_engine), Json(batch)).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::OK);

        let responses: Vec<Value> = body_to_json(body).await.as_array().unwrap().clone();
        assert_eq!(responses.len(), 0);
    }

    #[tokio::test]
    async fn test_error_response_json_structure() {
        let proxy_engine = create_test_proxy_engine();

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "invalid_method",
            "id": 123
        });

        let response = handle_rpc(State(proxy_engine), Json(request)).await;
        let (_, body) = response.into_response().into_parts();

        let body_json = body_to_json(body).await;

        assert_eq!(body_json["jsonrpc"], "2.0");
        assert!(body_json["result"].is_null());
        assert!(body_json["error"].is_object());
        assert_eq!(body_json["id"], Value::Null);

        let error = &body_json["error"];
        assert!(error.get("code").is_some());
        assert!(error.get("message").is_some());
    }

    #[tokio::test]
    async fn test_all_allowed_methods_are_valid() {
        for method in ALLOWED_METHODS {
            assert!(
                prism_core::types::is_method_allowed(method),
                "Method {method} should be allowed"
            );
        }
    }
}
