//! E2E Test HTTP Client
//!
//! A specialized HTTP client for making JSON-RPC requests to the Prism proxy
//! and direct Geth nodes for testing purposes.

use super::config::E2eConfig;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use thiserror::Error;

/// Global shared HTTP client to prevent connection pool fragmentation
/// when running 85+ tests in parallel
static SHARED_HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Get or create the shared HTTP client with optimized pool settings
fn get_shared_http_client(timeout_seconds: u64) -> reqwest::Client {
    SHARED_HTTP_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(timeout_seconds))
                // Optimized pool settings for parallel test execution
                .pool_max_idle_per_host(200)
                .pool_idle_timeout(Duration::from_secs(60))
                .connect_timeout(Duration::from_secs(10))
                // Enable TCP keepalive for connection reuse
                .tcp_keepalive(Duration::from_secs(30))
                .build()
                .expect("Failed to create shared HTTP client")
        })
        .clone()
}

/// Errors that can occur during E2E testing
#[derive(Debug, Error)]
pub enum E2eError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("JSON serialization error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("JSON-RPC error: code={code}, message={message}")]
    RpcError { code: i32, message: String },

    #[error("Timeout waiting for condition: {0}")]
    Timeout(String),

    #[error("Assertion failed: {0}")]
    AssertionFailed(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Docker operation failed: {0}")]
    DockerError(String),
}

/// JSON-RPC request structure
#[derive(Debug, Clone, Serialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: serde_json::Value,
    pub id: u64,
}

impl RpcRequest {
    /// Create a new JSON-RPC request
    #[must_use]
    pub fn new(method: &str, params: serde_json::Value) -> Self {
        Self { jsonrpc: "2.0".to_string(), method: method.to_string(), params, id: 1 }
    }

    /// Create a request with a specific ID
    #[must_use]
    pub fn with_id(method: &str, params: serde_json::Value, id: u64) -> Self {
        Self { jsonrpc: "2.0".to_string(), method: method.to_string(), params, id }
    }
}

/// JSON-RPC response structure
#[derive(Debug, Clone, Deserialize)]
pub struct RpcResponse<T> {
    pub jsonrpc: String,
    pub result: Option<T>,
    pub error: Option<RpcErrorData>,
    pub id: serde_json::Value,
}

/// JSON-RPC error data
#[derive(Debug, Clone, Deserialize)]
pub struct RpcErrorData {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

/// Response with timing information
#[derive(Debug, Clone)]
pub struct TimedResponse<T> {
    pub response: RpcResponse<T>,
    pub duration: Duration,
    pub cache_status: Option<String>,
}

/// E2E test client for making requests to Prism and Anvil nodes
#[derive(Clone)]
pub struct E2eClient {
    http_client: reqwest::Client,
    config: E2eConfig,
    request_counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl E2eClient {
    /// Create a new E2E test client
    ///
    /// Uses a shared HTTP client across all test instances to prevent
    /// connection pool fragmentation under parallel test execution.
    #[must_use]
    pub fn new(config: E2eConfig) -> Self {
        // Use shared HTTP client to prevent 85+ separate connection pools
        let http_client = get_shared_http_client(config.timeout_seconds);

        Self {
            http_client,
            config,
            request_counter: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    /// Create a client with default configuration
    #[must_use]
    pub fn default_client() -> Self {
        Self::new(E2eConfig::default())
    }

    /// Get the configuration
    #[must_use]
    pub fn config(&self) -> &E2eConfig {
        &self.config
    }

    /// Generate a unique request ID
    fn next_id(&self) -> u64 {
        self.request_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Send a JSON-RPC request to the proxy
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails, JSON parsing fails, or the RPC returns an error.
    pub async fn proxy_request<T: DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<TimedResponse<T>, E2eError> {
        self.request_to_url(&self.config.proxy_url, method, params).await
    }

    /// Send a JSON-RPC request directly to a Geth node
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails, JSON parsing fails, or the RPC returns an error.
    pub async fn direct_request<T: DeserializeOwned>(
        &self,
        url: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<TimedResponse<T>, E2eError> {
        self.request_to_url(url, method, params).await
    }

    /// Send a JSON-RPC request to a specific URL
    async fn request_to_url<T: DeserializeOwned>(
        &self,
        url: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<TimedResponse<T>, E2eError> {
        let request = RpcRequest::with_id(method, params, self.next_id());
        let start = Instant::now();

        let response = self.http_client.post(url).json(&request).send().await?;

        let cache_status = response
            .headers()
            .get("x-cache-status")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let body: RpcResponse<T> = response.json().await?;
        let duration = start.elapsed();

        if let Some(error) = &body.error {
            return Err(E2eError::RpcError { code: error.code, message: error.message.clone() });
        }

        Ok(TimedResponse { response: body, duration, cache_status })
    }

    /// Send a batch of JSON-RPC requests to the proxy
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or JSON parsing fails.
    pub async fn proxy_batch_request(
        &self,
        requests: Vec<RpcRequest>,
    ) -> Result<(Vec<RpcResponse<serde_json::Value>>, Duration, Option<String>), E2eError> {
        // Batch requests are now handled on the same endpoint as single requests
        // The router detects arrays and processes them as batches
        let url = self.config.proxy_url.clone();
        let start = Instant::now();

        let response = self.http_client.post(&url).json(&requests).send().await?;

        let cache_status = response
            .headers()
            .get("x-cache-status")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let body: Vec<RpcResponse<serde_json::Value>> = response.json().await?;
        let duration = start.elapsed();

        Ok((body, duration, cache_status))
    }

    /// Get the current block number from the proxy
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_block_number(&self) -> Result<u64, E2eError> {
        let response: TimedResponse<String> =
            self.proxy_request("eth_blockNumber", serde_json::json!([])).await?;

        let block_hex = response.response.result.ok_or_else(|| {
            E2eError::AssertionFailed("No result in eth_blockNumber response".to_string())
        })?;

        parse_hex_u64(&block_hex)
    }

    /// Get the chain ID from the proxy
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_chain_id(&self) -> Result<u64, E2eError> {
        let response: TimedResponse<String> =
            self.proxy_request("eth_chainId", serde_json::json!([])).await?;

        let chain_hex = response.response.result.ok_or_else(|| {
            E2eError::AssertionFailed("No result in eth_chainId response".to_string())
        })?;

        parse_hex_u64(&chain_hex)
    }

    /// Get balance for an address
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_balance(&self, address: &str) -> Result<(String, Duration), E2eError> {
        let response: TimedResponse<String> = self
            .proxy_request("eth_getBalance", serde_json::json!([address, "latest"]))
            .await?;

        let balance = response.response.result.ok_or_else(|| {
            E2eError::AssertionFailed("No result in eth_getBalance response".to_string())
        })?;

        Ok((balance, response.duration))
    }

    /// Get block by number
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_block_by_number(
        &self,
        block_number: &str,
        full_transactions: bool,
    ) -> Result<TimedResponse<serde_json::Value>, E2eError> {
        self.proxy_request(
            "eth_getBlockByNumber",
            serde_json::json!([block_number, full_transactions]),
        )
        .await
    }

    /// Get block by hash
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_block_by_hash(
        &self,
        block_hash: &str,
        full_transactions: bool,
    ) -> Result<TimedResponse<serde_json::Value>, E2eError> {
        self.proxy_request("eth_getBlockByHash", serde_json::json!([block_hash, full_transactions]))
            .await
    }

    /// Get transaction by hash
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_transaction_by_hash(
        &self,
        tx_hash: &str,
    ) -> Result<TimedResponse<serde_json::Value>, E2eError> {
        self.proxy_request("eth_getTransactionByHash", serde_json::json!([tx_hash]))
            .await
    }

    /// Get transaction receipt
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_transaction_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<TimedResponse<serde_json::Value>, E2eError> {
        self.proxy_request("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await
    }

    /// Get logs with a filter
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_logs(
        &self,
        filter: serde_json::Value,
    ) -> Result<TimedResponse<Vec<serde_json::Value>>, E2eError> {
        self.proxy_request("eth_getLogs", serde_json::json!([filter])).await
    }

    /// Get health status from the proxy
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn get_health(&self) -> Result<serde_json::Value, E2eError> {
        let url = format!("{}/health", self.config.proxy_url);
        let response = self.http_client.get(&url).send().await?;
        let body: serde_json::Value = response.json().await?;
        Ok(body)
    }

    /// Get metrics from the proxy
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn get_metrics(&self) -> Result<String, E2eError> {
        let url = format!("{}/metrics", self.config.proxy_url);
        let response = self.http_client.get(&url).send().await?;
        let body = response.text().await?;
        Ok(body)
    }

    /// Wait for a condition to be true with timeout
    ///
    /// # Errors
    ///
    /// Returns an error if the condition does not become true within the timeout period.
    pub async fn wait_for<F, Fut>(
        &self,
        condition_name: &str,
        timeout: Duration,
        mut check: F,
    ) -> Result<(), E2eError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if check().await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(E2eError::Timeout(condition_name.to_string()))
    }

    /// Check if the proxy is healthy
    pub async fn is_proxy_healthy(&self) -> bool {
        match self.get_health().await {
            Ok(health) => health.get("status").and_then(|s| s.as_str()) == Some("healthy"),
            Err(_) => false,
        }
    }

    /// Wait for the proxy to become healthy
    ///
    /// # Errors
    ///
    /// Returns an error if the proxy does not become healthy within the timeout period.
    pub async fn wait_for_proxy_healthy(&self, timeout: Duration) -> Result<(), E2eError> {
        self.wait_for("proxy healthy", timeout, || async { self.is_proxy_healthy().await })
            .await
    }

    /// Check if a Geth node is reachable
    pub async fn is_node_reachable(&self, url: &str) -> bool {
        self.direct_request::<String>(url, "eth_blockNumber", serde_json::json!([]))
            .await
            .is_ok()
    }

    /// Wait for a Geth node to become reachable
    ///
    /// # Errors
    ///
    /// Returns an error if the node does not become reachable within the timeout period.
    pub async fn wait_for_node(&self, url: &str, timeout: Duration) -> Result<(), E2eError> {
        let url = url.to_string();
        self.wait_for(&format!("geth node at {url}"), timeout, || {
            let url = url.clone();
            async move { self.is_node_reachable(&url).await }
        })
        .await
    }
}

/// Parse a hex string (with or without 0x prefix) to u64
///
/// # Errors
///
/// Returns an error if the hex string cannot be parsed as a valid u64.
pub fn parse_hex_u64(hex: &str) -> Result<u64, E2eError> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    u64::from_str_radix(hex, 16)
        .map_err(|_| E2eError::AssertionFailed(format!("Invalid hex number: {hex}")))
}

/// Retry an async operation with exponential backoff and jitter
///
/// This is useful for handling transient network failures in tests.
/// Uses jitter to prevent thundering herd problems when multiple tests
/// retry simultaneously.
///
/// # Errors
///
/// Returns an error if all retry attempts fail, wrapping the last error encountered.
pub async fn retry_with_backoff<T, E, F, Fut>(
    operation_name: &str,
    max_retries: usize,
    initial_delay_ms: u64,
    mut operation: F,
) -> Result<T, E2eError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    // Use more retries and longer delays for better resilience under load
    let effective_max_retries = max_retries.max(5);
    let effective_initial_delay = initial_delay_ms.max(500);

    let mut last_error = None;
    let mut delay = Duration::from_millis(effective_initial_delay);

    for attempt in 1..=effective_max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = Some(format!("{e}"));
                if attempt < effective_max_retries {
                    // Add jitter: random 0-50% extra delay to prevent thundering herd
                    let jitter_ms = u64::try_from(delay.as_millis())
                        .unwrap_or(0)
                        .checked_mul(rand_jitter())
                        .unwrap_or(0) /
                        100;
                    let delay_with_jitter = delay + Duration::from_millis(jitter_ms);

                    println!(
                        "  {operation_name} attempt {attempt}/{effective_max_retries} failed: {e}, retrying in {delay_with_jitter:?}"
                    );
                    tokio::time::sleep(delay_with_jitter).await;
                    // Exponential backoff with cap at 5 seconds
                    delay = delay.saturating_mul(2).min(Duration::from_secs(5));
                }
            }
        }
    }

    Err(E2eError::Timeout(format!(
        "{} failed after {} attempts. Last error: {}",
        operation_name,
        effective_max_retries,
        last_error.unwrap_or_else(|| "unknown".to_string())
    )))
}

/// Generate a random jitter percentage (0-50) using simple PRNG
/// Uses thread-local state to avoid synchronization overhead
fn rand_jitter() -> u64 {
    use std::cell::Cell;
    thread_local! {
        static SEED: Cell<u64> = const { Cell::new(0) };
    }
    SEED.with(|seed| {
        // Initialize seed from current time if not set
        let mut s = seed.get();
        if s == 0 {
            s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_nanos()).ok())
                .unwrap_or(12345);
        }
        // Simple LCG PRNG
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        seed.set(s);
        (s >> 33) % 51 // 0-50
    })
}

/// Format a u64 as a hex string with 0x prefix
#[must_use]
pub fn format_hex_u64(value: u64) -> String {
    format!("0x{value:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_u64() {
        assert_eq!(parse_hex_u64("0x10").unwrap(), 16);
        assert_eq!(parse_hex_u64("10").unwrap(), 16);
        assert_eq!(parse_hex_u64("0xff").unwrap(), 255);
        assert_eq!(parse_hex_u64("0x0").unwrap(), 0);
    }

    #[test]
    fn test_format_hex_u64() {
        assert_eq!(format_hex_u64(16), "0x10");
        assert_eq!(format_hex_u64(255), "0xff");
        assert_eq!(format_hex_u64(0), "0x0");
    }

    #[test]
    fn test_rpc_request_creation() {
        let req = RpcRequest::new("eth_blockNumber", serde_json::json!([]));
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.method, "eth_blockNumber");
        assert_eq!(req.id, 1);
    }
}
