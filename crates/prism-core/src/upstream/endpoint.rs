use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{watch, RwLock};

use crate::{
    cache::{reorg_manager::ReorgManager, CacheManager},
    types::{JsonRpcRequest, JsonRpcResponse, UpstreamConfig, UpstreamHealth},
    upstream::{
        circuit_breaker::{CircuitBreaker, CircuitBreakerState},
        http_client::HttpClient,
        websocket::WebSocketHandler,
    },
};

use super::errors::UpstreamError;

/// Result of a health check with block number information.
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    pub is_healthy: bool,
    pub block_number: Option<u64>,
}

/// Entry in the health history log.
#[derive(Debug, Clone)]
pub struct HealthHistoryEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub healthy: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

const HEALTH_HISTORY_SIZE: usize = 100;

/// Time-to-live for cached health status in milliseconds.
/// Health checks that occur within this window will use the cached value.
const HEALTH_CACHE_TTL_MS: u64 = 100;

/// Represents an upstream RPC endpoint with health tracking, circuit breaker protection,
/// and WebSocket support for real-time chain updates.
///
/// This is the primary abstraction for communicating with external RPC providers.
/// It handles request routing, health monitoring, failure tracking, and automatic
/// recovery through circuit breaker patterns.
///
/// Implements singleflight pattern for health checks to prevent duplicate concurrent
/// requests to the same upstream.
pub struct UpstreamEndpoint {
    config: UpstreamConfig,
    health: Arc<RwLock<UpstreamHealth>>,
    circuit_breaker: Arc<CircuitBreaker>,
    http_client: Arc<HttpClient>,
    websocket_handler: WebSocketHandler,
    health_check_in_flight: AtomicBool,
    health_check_result: Arc<RwLock<watch::Sender<Option<HealthCheckResult>>>>,
    health_check_receiver: watch::Receiver<Option<HealthCheckResult>>,
    health_history: Arc<RwLock<VecDeque<HealthHistoryEntry>>>,
    /// Cached health status to avoid lock contention on every request
    cached_is_healthy: AtomicBool,
    /// Timestamp (ms since epoch) when cached health was last updated
    health_cache_time: AtomicU64,
}

impl UpstreamEndpoint {
    /// Creates a new upstream endpoint with the given configuration and HTTP client.
    ///
    /// Initializes the circuit breaker with thresholds from the config and sets up
    /// the WebSocket handler for chain tip subscriptions.
    #[must_use]
    pub fn new(config: UpstreamConfig, http_client: Arc<HttpClient>) -> Self {
        let circuit_breaker = Arc::new(CircuitBreaker::new(
            config.circuit_breaker_threshold,
            config.circuit_breaker_timeout_seconds,
        ));

        let websocket_handler = WebSocketHandler::new(config.clone(), http_client.clone());

        let (health_check_tx, health_check_rx) = watch::channel(None);

        Self {
            config,
            health: Arc::new(RwLock::new(UpstreamHealth::default())),
            circuit_breaker,
            http_client,
            websocket_handler,
            health_check_in_flight: AtomicBool::new(false),
            health_check_result: Arc::new(RwLock::new(health_check_tx)),
            health_check_receiver: health_check_rx,
            health_history: Arc::new(RwLock::new(VecDeque::with_capacity(HEALTH_HISTORY_SIZE))),
            cached_is_healthy: AtomicBool::new(false),
            health_cache_time: AtomicU64::new(0),
        }
    }

    /// Sends a JSON-RPC request to this upstream endpoint.
    ///
    /// Checks the circuit breaker state before sending, applies method-specific timeouts,
    /// and updates health metrics based on the response. Failed requests trigger circuit
    /// breaker failure tracking.
    ///
    /// # Errors
    ///
    /// Returns `UpstreamError::CircuitBreakerOpen` if the circuit breaker is open.
    /// Returns `UpstreamError::InvalidRequest` if request serialization fails.
    /// Returns `UpstreamError::InvalidResponse` if response parsing fails.
    /// Returns `UpstreamError::RpcError` if the RPC response contains an error.
    pub async fn send_request(
        &self,
        request: &JsonRpcRequest,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let circuit_breaker_state = self.circuit_breaker.get_state().await;

        if tracing::enabled!(tracing::Level::DEBUG) {
            let failure_count = self.circuit_breaker.get_failure_count().await;
            tracing::debug!(
                upstream = %self.config.name,
                circuit_breaker_state = ?circuit_breaker_state,
                failures = failure_count,
                "sending request to upstream"
            );
        }

        if !self.circuit_breaker.can_execute().await {
            return Err(UpstreamError::CircuitBreakerOpen);
        }

        let start_time = std::time::Instant::now();
        let timeout = self.get_timeout_for_method(&request.method);

        let body = serde_json::to_vec(request).map_err(|e| {
            UpstreamError::InvalidRequest(format!("Failed to serialize request: {e}"))
        })?;

        let response_bytes = match self
            .http_client
            .send_request(&self.config.url, bytes::Bytes::from(body), timeout)
            .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                if matches!(&e, UpstreamError::HttpError(429, _)) {
                    return Err(e);
                }
                self.circuit_breaker.on_failure().await;
                return Err(e);
            }
        };

        let elapsed_ms = start_time.elapsed().as_millis();
        #[allow(clippy::cast_possible_truncation)]
        let response_time = elapsed_ms as u64;

        let json_response: JsonRpcResponse = match serde_json::from_slice(&response_bytes) {
            Ok(r) => r,
            Err(e) => {
                self.circuit_breaker.on_failure().await;
                return Err(UpstreamError::InvalidResponse(format!("Invalid JSON: {e}")));
            }
        };

        if let Some(error) = &json_response.error {
            let rpc_error = UpstreamError::RpcError(error.code, error.message.clone());

            if let Some(category) = rpc_error.rpc_category() {
                if category.should_trigger_circuit_breaker() {
                    self.circuit_breaker.on_failure().await;
                }
            }

            return Err(rpc_error);
        }

        self.update_health(true, response_time).await;
        self.circuit_breaker.on_success().await;
        Ok(json_response)
    }

    /// Sends a batch JSON-RPC request (raw bytes) to this upstream endpoint.
    ///
    /// Similar to `send_request` but handles pre-serialized batch data and returns
    /// raw response bytes without parsing. The caller is responsible for parsing the
    /// batch response array.
    ///
    /// # Errors
    ///
    /// Returns `UpstreamError::CircuitBreakerOpen` if the circuit breaker is open.
    /// Returns an error if the HTTP request fails.
    pub async fn send_batch_request(
        &self,
        batch_data: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, UpstreamError> {
        let circuit_breaker_state = self.circuit_breaker.get_state().await;

        if tracing::enabled!(tracing::Level::DEBUG) {
            let failure_count = self.circuit_breaker.get_failure_count().await;
            tracing::debug!(
                upstream = %self.config.name,
                circuit_breaker_state = ?circuit_breaker_state,
                failures = failure_count,
                "sending batch request to upstream"
            );
        }

        if !self.circuit_breaker.can_execute().await {
            return Err(UpstreamError::CircuitBreakerOpen);
        }

        let start_time = std::time::Instant::now();

        let response_bytes = match self
            .http_client
            .send_request(&self.config.url, bytes::Bytes::copy_from_slice(batch_data), timeout)
            .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                self.circuit_breaker.on_failure().await;
                return Err(e);
            }
        };

        let elapsed_ms = start_time.elapsed().as_millis();
        #[allow(clippy::cast_possible_truncation)]
        let response_time = elapsed_ms as u64;

        self.update_health(true, response_time).await;
        self.circuit_breaker.on_success().await;
        Ok(response_bytes.to_vec())
    }

    /// Checks if this upstream is currently healthy and available for requests.
    ///
    /// Uses a 100ms cache to reduce lock contention on high-throughput workloads.
    /// Returns `true` only if both the health status is good and the circuit breaker
    /// is closed (allowing requests through).
    pub async fn is_healthy(&self) -> bool {
        // Fast path: check cache first (lock-free)
        let cache_time = self.health_cache_time.load(Ordering::Relaxed);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        if now_ms.saturating_sub(cache_time) < HEALTH_CACHE_TTL_MS {
            return self.cached_is_healthy.load(Ordering::Relaxed);
        }

        // Slow path: acquire locks and update cache
        let health = self.health.read().await;
        let circuit_breaker_allows = self.circuit_breaker.can_execute().await;
        let is_healthy = health.is_healthy && circuit_breaker_allows;

        // Update cache (race is benign - worst case is redundant computation)
        self.cached_is_healthy.store(is_healthy, Ordering::Relaxed);
        self.health_cache_time.store(now_ms, Ordering::Relaxed);

        is_healthy
    }

    /// Returns the current health status including response time and error count.
    pub async fn get_health(&self) -> UpstreamHealth {
        self.health.read().await.clone()
    }

    /// Invalidates the health cache, forcing the next `is_healthy()` call to recompute.
    /// This is useful after external state changes (like circuit breaker state changes)
    /// that affect health but don't go through `update_health()`.
    pub fn invalidate_health_cache(&self) {
        self.health_cache_time.store(0, Ordering::Relaxed);
    }

    /// Returns a reference to the upstream configuration.
    #[must_use]
    pub fn config(&self) -> &UpstreamConfig {
        &self.config
    }

    /// Performs an active health check by sending an `eth_blockNumber` request.
    ///
    /// Updates the internal health status based on the result. Marks the upstream
    /// unhealthy after 2 consecutive failures or if the circuit breaker opens.
    pub async fn health_check(&self) -> bool {
        self.health_check_with_block_number().await.0
    }

    /// Performs an active health check and returns both the result and block number.
    ///
    /// Returns a tuple of (`is_healthy`, `Option<block_number>`). The block number is
    /// extracted from the `eth_blockNumber` response and can be used for scoring
    /// to track which upstreams are closest to the chain tip.
    ///
    /// Also queries the finalized block number if available and updates the health status.
    ///
    /// Uses singleflight pattern to deduplicate concurrent health checks - if a check is
    /// already in-flight, callers will wait for and share the result instead of making
    /// duplicate requests.
    pub async fn health_check_with_block_number(&self) -> (bool, Option<u64>) {
        if self
            .health_check_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            let mut receiver = self.health_check_receiver.clone();
            if receiver.changed().await.is_ok() {
                if let Some(result) = receiver.borrow().clone() {
                    tracing::trace!(
                        upstream = %self.config.name,
                        "singleflight: reusing in-flight health check result"
                    );
                    return (result.is_healthy, result.block_number);
                }
            }
        }

        let result = self.do_health_check().await;

        {
            let sender = self.health_check_result.read().await;
            let _ = sender
                .send(Some(HealthCheckResult { is_healthy: result.0, block_number: result.1 }));
        }

        self.health_check_in_flight.store(false, Ordering::SeqCst);

        result
    }

    /// Internal implementation of health check logic.
    ///
    /// This is the actual health check implementation, separated from the singleflight
    /// coordination in `health_check_with_block_number`.
    async fn do_health_check(&self) -> (bool, Option<u64>) {
        let start_time = std::time::Instant::now();

        if !self.circuit_breaker.can_execute().await {
            let mut health = self.health.write().await;
            health.is_healthy = false;

            // Record failure in history
            self.record_health_history(false, None, Some("Circuit breaker open".to_string()))
                .await;

            return (false, None);
        }

        let health_request =
            JsonRpcRequest::new("eth_blockNumber", None, serde_json::Value::Number(1.into()));

        match self.send_request(&health_request).await {
            Ok(response) => {
                #[allow(clippy::cast_possible_truncation)]
                let latency_ms = start_time.elapsed().as_millis() as u64;
                let latest_block_number = response.result.as_ref().and_then(|result| {
                    result.as_str().and_then(|hex_str| {
                        let hex_str = hex_str.trim_start_matches("0x");
                        u64::from_str_radix(hex_str, 16).ok()
                    })
                });

                let finalized_block_number = self.query_finalized_block().await;

                let mut health = self.health.write().await;
                health.is_healthy = true;
                health.error_count = 0;
                health.latest_block = latest_block_number;
                health.finalized_block = finalized_block_number;

                // Record success in history
                self.record_health_history(true, Some(latency_ms), None).await;

                (true, latest_block_number)
            }
            Err(e) => {
                #[allow(clippy::cast_possible_truncation)]
                let latency_ms = start_time.elapsed().as_millis() as u64;
                let cb_state = self.circuit_breaker.get_state().await;
                let mut health = self.health.write().await;
                health.error_count += 1;
                if health.error_count >= 2 || cb_state == CircuitBreakerState::Open {
                    health.is_healthy = false;
                }

                let error_msg = match &e {
                    UpstreamError::RpcError(code, message) => {
                        tracing::warn!(
                            upstream = %self.config.name,
                            code = code,
                            message = %message,
                            circuit_breaker_state = ?cb_state,
                            "health check failed for upstream"
                        );
                        format!("RPC error {code}: {message}")
                    }
                    _ => e.to_string(),
                };

                // Record failure in history
                self.record_health_history(false, Some(latency_ms), Some(error_msg)).await;

                (false, None)
            }
        }
    }

    /// Queries the finalized block number from the upstream.
    ///
    /// Returns `None` if the upstream doesn't support the "finalized" tag or if the query fails.
    /// This is a best-effort operation that won't affect upstream health status.
    async fn query_finalized_block(&self) -> Option<u64> {
        let finalized_request = JsonRpcRequest::new(
            "eth_getBlockByNumber",
            Some(serde_json::json!(["finalized", false])),
            serde_json::Value::Number(2.into()),
        );

        if let Ok(response) = self.send_request(&finalized_request).await {
            response.result.as_ref().and_then(|result| {
                result.get("number").and_then(|num| {
                    num.as_str().and_then(|hex_str| {
                        let hex_str = hex_str.trim_start_matches("0x");
                        u64::from_str_radix(hex_str, 16).ok()
                    })
                })
            })
        } else {
            tracing::debug!(
                upstream = %self.config.name,
                "upstream does not support 'finalized' block tag"
            );
            None
        }
    }

    /// Subscribes to `newHeads` via WebSocket for real-time block updates.
    ///
    /// The subscription enables automatic cache invalidation and reorganization
    /// detection by monitoring new blocks as they arrive.
    ///
    /// # Errors
    ///
    /// Returns an error if the WebSocket connection fails or the subscription request fails.
    pub async fn subscribe_to_new_heads(
        &self,
        cache_manager: Arc<CacheManager>,
        reorg_manager: Arc<ReorgManager>,
    ) -> Result<(), UpstreamError> {
        self.websocket_handler
            .subscribe_to_new_heads(cache_manager, reorg_manager)
            .await
    }

    /// Returns whether a WebSocket subscription should be attempted.
    ///
    /// Based on backoff timing after previous failures to avoid repeated
    /// connection attempts to unavailable WebSocket endpoints.
    pub async fn should_attempt_websocket_subscription(&self) -> bool {
        self.websocket_handler.should_attempt_websocket_subscription().await
    }

    /// Records a WebSocket subscription failure for backoff calculation.
    pub async fn record_websocket_failure(&self) {
        self.websocket_handler.record_websocket_failure().await;
    }

    /// Records a successful WebSocket subscription, resetting failure tracking.
    pub async fn record_websocket_success(&self) {
        self.websocket_handler.record_websocket_success().await;
    }

    /// Returns the current circuit breaker state.
    pub async fn get_circuit_breaker_state(&self) -> CircuitBreakerState {
        self.circuit_breaker.get_state().await
    }

    /// Returns the current circuit breaker failure count.
    pub async fn get_circuit_breaker_failure_count(&self) -> u32 {
        self.circuit_breaker.get_failure_count().await
    }

    /// Returns a reference to the circuit breaker for testing and monitoring.
    #[must_use]
    pub fn circuit_breaker(&self) -> &Arc<CircuitBreaker> {
        &self.circuit_breaker
    }

    /// Resets the circuit breaker to a closed state.
    ///
    /// Used by the admin API to manually recover an upstream that has been
    /// marked as failing. This forces the circuit breaker back to the closed
    /// state and resets the failure count.
    pub async fn reset_circuit_breaker(&self) {
        self.circuit_breaker.on_success().await;
    }

    /// Records a health check result in the history buffer.
    ///
    /// Maintains a ring buffer of the last `HEALTH_HISTORY_SIZE` health check results.
    /// Older entries are automatically evicted when the buffer is full.
    async fn record_health_history(
        &self,
        healthy: bool,
        latency_ms: Option<u64>,
        error: Option<String>,
    ) {
        let mut history = self.health_history.write().await;

        // Add new entry
        history.push_back(HealthHistoryEntry {
            timestamp: chrono::Utc::now(),
            healthy,
            latency_ms,
            error,
        });

        // Evict oldest entry if buffer is full
        if history.len() > HEALTH_HISTORY_SIZE {
            history.pop_front();
        }
    }

    /// Returns recent health check history entries.
    ///
    /// # Arguments
    /// * `limit` - Maximum number of entries to return (default: 50)
    ///
    /// # Returns
    /// A vector of health history entries, newest first
    pub async fn get_health_history(&self, limit: usize) -> Vec<HealthHistoryEntry> {
        let history = self.health_history.read().await;

        // Return newest entries first
        history.iter().rev().take(limit).cloned().collect()
    }

    /// Checks if this upstream can serve the requested block number.
    ///
    /// Compares the requested block against the upstream's latest known block number
    /// from health checks. Returns `false` if the upstream is behind the requested block
    /// or if block information is unavailable.
    ///
    /// # Arguments
    /// * `block_number` - The block number that needs to be served
    ///
    /// # Returns
    /// `true` if the upstream can serve this block, `false` otherwise
    pub async fn can_serve_block(&self, block_number: u64) -> bool {
        let health = self.health.read().await;

        // If we don't have latest block info, we can't determine availability
        let Some(latest_block) = health.latest_block else {
            return false;
        };

        // Upstream can serve the block if it's at or beyond the requested block
        block_number <= latest_block
    }

    /// Returns the timeout duration for a given RPC method.
    ///
    /// Fast methods like `eth_blockNumber` get 5s, standard queries get 10s,
    /// and `eth_getLogs` gets 30s due to potentially large result sets.
    fn get_timeout_for_method(&self, method: &str) -> Duration {
        match method {
            "eth_blockNumber" | "eth_chainId" | "eth_gasPrice" => Duration::from_secs(5),
            "eth_getBlockByNumber" |
            "eth_getBlockByHash" |
            "eth_getTransactionByHash" |
            "eth_getTransactionReceipt" => Duration::from_secs(10),
            "eth_getLogs" => Duration::from_secs(30),
            _ => Duration::from_secs(self.config.timeout_seconds),
        }
    }

    /// Updates the health status based on request success or failure.
    ///
    /// Successful requests reset the error count. After 3 consecutive failures,
    /// the upstream is marked unhealthy.
    async fn update_health(&self, success: bool, response_time_ms: u64) {
        let mut health = self.health.write().await;
        health.last_check = std::time::Instant::now();
        health.response_time_ms = Some(response_time_ms);

        if success {
            health.error_count = 0;
            health.is_healthy = true;
        } else {
            health.error_count += 1;

            if health.error_count >= 3 {
                health.is_healthy = false;
                tracing::warn!(
                    upstream = %self.config.name,
                    error_count = health.error_count,
                    "upstream is unhealthy after consecutive errors"
                );
            }
        }

        // Invalidate health cache so next is_healthy() call will recompute
        self.health_cache_time.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UpstreamConfig;

    #[test]
    fn test_upstream_endpoint_creation() {
        let config = UpstreamConfig {
            url: "https://example.com".to_string(),
            name: Arc::from("test"),
            weight: 1,
            timeout_seconds: 30,
            supports_websocket: false,
            ws_url: None,
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 1,
            chain_id: 0,
        };

        let endpoint = UpstreamEndpoint::new(config, Arc::new(HttpClient::new().unwrap()));
        assert_eq!(&*endpoint.config().name, "test");
    }

    #[tokio::test]
    async fn test_upstream_endpoint_with_circuit_breaker() {
        let config = UpstreamConfig {
            url: "http://localhost:8545".to_string(),
            name: Arc::from("test_upstream"),
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 1,
            ..UpstreamConfig::default()
        };

        let endpoint = UpstreamEndpoint::new(config, Arc::new(HttpClient::new().unwrap()));

        assert!(endpoint.is_healthy().await);
        assert_eq!(endpoint.get_circuit_breaker_state().await, CircuitBreakerState::Closed);

        endpoint.circuit_breaker().on_failure().await;
        endpoint.circuit_breaker().on_failure().await;
        endpoint.invalidate_health_cache(); // Cache invalidation needed after direct circuit breaker manipulation

        assert_eq!(endpoint.get_circuit_breaker_state().await, CircuitBreakerState::Open);
        assert!(!endpoint.is_healthy().await);

        endpoint.circuit_breaker().on_success().await;
        endpoint.invalidate_health_cache(); // Cache invalidation needed after direct circuit breaker manipulation
        assert_eq!(endpoint.get_circuit_breaker_state().await, CircuitBreakerState::Closed);
        assert!(endpoint.is_healthy().await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_integration() {
        let config = UpstreamConfig {
            url: "http://localhost:8545".to_string(),
            name: Arc::from("test_upstream"),
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 1,
            ..UpstreamConfig::default()
        };

        let endpoint = UpstreamEndpoint::new(config, Arc::new(HttpClient::new().unwrap()));

        assert!(endpoint.is_healthy().await);
        assert_eq!(endpoint.get_circuit_breaker_state().await, CircuitBreakerState::Closed);
        assert_eq!(endpoint.get_circuit_breaker_failure_count().await, 0);

        endpoint.circuit_breaker().on_failure().await;
        endpoint.invalidate_health_cache(); // Cache invalidation needed after direct circuit breaker manipulation
        assert_eq!(endpoint.get_circuit_breaker_failure_count().await, 1);
        assert!(endpoint.is_healthy().await);

        endpoint.circuit_breaker().on_failure().await;
        endpoint.invalidate_health_cache(); // Cache invalidation needed after direct circuit breaker manipulation
        assert_eq!(endpoint.get_circuit_breaker_failure_count().await, 2);
        assert_eq!(endpoint.get_circuit_breaker_state().await, CircuitBreakerState::Open);
        assert!(!endpoint.is_healthy().await);

        endpoint.circuit_breaker().on_success().await;
        endpoint.invalidate_health_cache(); // Cache invalidation needed after direct circuit breaker manipulation
        assert_eq!(endpoint.get_circuit_breaker_state().await, CircuitBreakerState::Closed);
        assert!(endpoint.is_healthy().await);
        assert_eq!(endpoint.get_circuit_breaker_failure_count().await, 0);
    }

    #[tokio::test]
    async fn test_circuit_breaker_with_http_failures() {
        let config = UpstreamConfig {
            url: "http://localhost:9999".to_string(),
            name: Arc::from("failing_upstream"),
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 1,
            ..UpstreamConfig::default()
        };

        let endpoint = UpstreamEndpoint::new(config, Arc::new(HttpClient::new().unwrap()));

        let request =
            JsonRpcRequest::new("eth_blockNumber", None, serde_json::Value::Number(1.into()));

        let result1 = endpoint.send_request(&request).await;
        assert!(result1.is_err());

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let result2 = endpoint.send_request(&request).await;
        assert!(result2.is_err());

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        assert_eq!(endpoint.get_circuit_breaker_state().await, CircuitBreakerState::Open);
        assert!(!endpoint.is_healthy().await);

        let result3 = endpoint.send_request(&request).await;
        assert!(matches!(result3, Err(UpstreamError::CircuitBreakerOpen)));
    }
}
