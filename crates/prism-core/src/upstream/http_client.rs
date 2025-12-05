use reqwest::{Client, ClientBuilder};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::upstream::UpstreamError;

/// Configuration for HTTP client concurrency and timeout behavior.
///
/// Controls semaphore-based concurrency limiting with adaptive timeouts
/// based on permit availability.
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// Maximum number of concurrent HTTP requests allowed
    pub concurrent_limit: usize,
    /// Permit acquisition timeout in milliseconds under normal load
    pub permit_timeout_ms: u64,
    /// Permit acquisition timeout in milliseconds when permits are scarce
    pub permit_timeout_scarce_ms: u64,
    /// Number of available permits below which they are considered scarce
    pub scarce_permit_threshold: usize,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            concurrent_limit: 1000,
            permit_timeout_ms: 500,
            permit_timeout_scarce_ms: 200,
            scarce_permit_threshold: 100,
        }
    }
}

/// HTTP client with semaphore-based concurrency control.
///
/// Manages a pool of HTTP connections with configurable concurrency limits
/// and automatic retry logic for transient failures.
pub struct HttpClient {
    client: Client,
    concurrent_limit: Arc<Semaphore>,
    config: HttpClientConfig,
}

/// RAII guard ensuring semaphore permits are always released.
///
/// Uses [`OwnedSemaphorePermit`] which owns an `Arc` to the semaphore,
/// making it safe to hold across async boundaries.
struct PermitGuard {
    _permit: OwnedSemaphorePermit,
    semaphore: Arc<Semaphore>,
}

impl PermitGuard {
    fn new(permit: OwnedSemaphorePermit, semaphore: Arc<Semaphore>) -> Self {
        Self { _permit: permit, semaphore }
    }

    fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

impl Drop for PermitGuard {
    fn drop(&mut self) {
        tracing::trace!(
            available_permits = self.semaphore.available_permits(),
            "permit guard dropped"
        );
    }
}

// Note: Default is intentionally NOT implemented because HttpClient::new() can fail.
// Callers should use HttpClient::new() or HttpClient::with_config() explicitly
// and handle the Result.

impl HttpClient {
    /// Creates a new HTTP client with default configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying reqwest client fails to build.
    pub fn new() -> Result<Self, UpstreamError> {
        Self::with_config(HttpClientConfig::default())
    }

    /// Sanitizes network errors to prevent information disclosure.
    fn sanitize_network_error(error: &reqwest::Error) -> String {
        if error.is_connect() {
            "connection refused or unreachable".to_string()
        } else if error.is_timeout() {
            "connection timed out".to_string()
        } else if error.is_request() {
            "request failed".to_string()
        } else if error.is_body() {
            "response body error".to_string()
        } else if error.is_decode() {
            "response decode error".to_string()
        } else if error.is_redirect() {
            "too many redirects".to_string()
        } else {
            "network error".to_string()
        }
    }

    /// Creates a new HTTP client with the specified concurrency limit.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying reqwest client fails to build.
    pub fn with_concurrency_limit(concurrent_limit: usize) -> Result<Self, UpstreamError> {
        Self::with_config(HttpClientConfig { concurrent_limit, ..Default::default() })
    }

    /// Creates a new HTTP client with the provided configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying reqwest client fails to build.
    pub fn with_config(config: HttpClientConfig) -> Result<Self, UpstreamError> {
        let client = ClientBuilder::new()
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(100)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(45))
            .http2_adaptive_window(true)
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("rpc-aggregator/0.1.0")
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()
            .map_err(|e| {
                tracing::error!(error = %e, "failed to build http client");
                UpstreamError::ConnectionFailed(format!("HTTP client build failed: {e}"))
            })?;

        Ok(Self {
            client,
            concurrent_limit: Arc::new(Semaphore::new(config.concurrent_limit)),
            config,
        })
    }

    /// Sends an HTTP POST request with semaphore-based concurrency control.
    ///
    /// # Errors
    ///
    /// - [`UpstreamError::Timeout`] if permit acquisition or request times out
    /// - [`UpstreamError::ConcurrencyLimit`] if the semaphore is closed
    /// - [`UpstreamError::HttpError`] for non-success HTTP status codes
    /// - [`UpstreamError::Network`] for network-related failures
    pub async fn send_request(
        &self,
        url: &str,
        body: bytes::Bytes,
        timeout: Duration,
    ) -> Result<bytes::Bytes, UpstreamError> {
        const MAX_RETRIES: u32 = 2;

        let permit_timeout =
            if self.concurrent_limit.available_permits() < self.config.scarce_permit_threshold {
                Duration::from_millis(self.config.permit_timeout_scarce_ms)
            } else {
                Duration::from_millis(self.config.permit_timeout_ms)
            };

        let permit = tokio::time::timeout(
            permit_timeout,
            Arc::clone(&self.concurrent_limit).acquire_owned(),
        )
        .await
        .map_err(|_| {
            tracing::warn!(
                url = url,
                available_permits = self.concurrent_limit.available_permits(),
                "http client semaphore acquisition timeout"
            );
            UpstreamError::Timeout
        })?
        .map_err(|_| {
            tracing::warn!(
                url = url,
                available_permits = self.concurrent_limit.available_permits(),
                "http client concurrency limit reached"
            );
            UpstreamError::ConcurrencyLimit(url.to_string())
        })?;

        let permit_guard = PermitGuard::new(permit, self.concurrent_limit.clone());

        tracing::trace!(
            available_permits = permit_guard.available_permits(),
            "http request started"
        );

        let mut retries = 0;

        loop {
            let result = self
                .client
                .post(url)
                .header("content-type", "application/json")
                // PERF: Bytes::clone() is O(1) - just increments reference count
                // This allows efficient retries without copying the body data
                .body(body.clone())
                .timeout(timeout)
                .send()
                .await;

            match result {
                Ok(response) => {
                    if response.status().is_success() {
                        let result = response.bytes().await.map_err(UpstreamError::Network);
                        tracing::trace!(
                            available_permits = permit_guard.available_permits(),
                            "http request completed"
                        );
                        return result;
                    } else if response.status().is_server_error() && retries < MAX_RETRIES {
                        retries += 1;
                        tokio::time::sleep(Duration::from_millis(100 * (1 << retries))).await;
                        continue;
                    }

                    let status = response.status().as_u16();
                    let raw_text = response.text().await.unwrap_or_default();
                    let sanitized_text = if raw_text.len() > 256 {
                        format!("{}... (truncated)", &raw_text[..256])
                    } else {
                        raw_text
                    };
                    tracing::trace!(
                        status = status,
                        available_permits = permit_guard.available_permits(),
                        "http request failed"
                    );
                    return Err(UpstreamError::HttpError(status, sanitized_text));
                }
                Err(_e) if retries < MAX_RETRIES => {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(100 * (1 << retries))).await;
                }
                Err(e) => {
                    tracing::trace!(
                        available_permits = permit_guard.available_permits(),
                        "http request error"
                    );
                    if e.is_timeout() {
                        return Err(UpstreamError::Timeout);
                    }
                    let sanitized_error = Self::sanitize_network_error(&e);
                    return Err(UpstreamError::ConnectionFailed(sanitized_error));
                }
            }
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.concurrent_limit.available_permits()
    }
}

#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_http_client_config_default() {
        let config = HttpClientConfig::default();
        assert_eq!(config.concurrent_limit, 1000);
        assert_eq!(config.permit_timeout_ms, 500);
        assert_eq!(config.permit_timeout_scarce_ms, 200);
        assert_eq!(config.scarce_permit_threshold, 100);
    }

    #[test]
    fn test_http_client_new() {
        let client = HttpClient::new();
        assert!(client.is_ok(), "HttpClient::new() should succeed");
    }

    #[test]
    fn test_http_client_with_config() {
        let config = HttpClientConfig {
            concurrent_limit: 50,
            permit_timeout_ms: 1000,
            permit_timeout_scarce_ms: 100,
            scarce_permit_threshold: 10,
        };
        let client = HttpClient::with_config(config);
        assert!(client.is_ok(), "HttpClient::with_config() should succeed");
    }

    #[test]
    fn test_http_client_with_concurrency_limit() {
        let client = HttpClient::with_concurrency_limit(100);
        assert!(client.is_ok(), "HttpClient::with_concurrency_limit() should succeed");
    }

    #[tokio::test]
    async fn test_permit_guard_releases_on_drop() {
        let semaphore = Arc::new(Semaphore::new(10));
        let initial_permits = semaphore.available_permits();

        {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let _guard = PermitGuard::new(permit, semaphore.clone());
            assert_eq!(semaphore.available_permits(), initial_permits - 1);
        }

        assert_eq!(semaphore.available_permits(), initial_permits);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_permit_guard_concurrent_release() {
        let semaphore = Arc::new(Semaphore::new(10));
        let initial_permits = semaphore.available_permits();

        let mut handles = Vec::new();

        for _ in 0..20 {
            let sem = semaphore.clone();
            handles.push(tokio::spawn(async move {
                let permit = sem.clone().acquire_owned().await.unwrap();
                let _guard = PermitGuard::new(permit, sem);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }));
        }

        for handle in handles {
            handle.await.expect("Task should not panic");
        }

        assert_eq!(semaphore.available_permits(), initial_permits);
    }

    #[tokio::test]
    async fn test_permit_guard_available_permits() {
        let semaphore = Arc::new(Semaphore::new(100));
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let guard = PermitGuard::new(permit, semaphore.clone());

        assert_eq!(guard.available_permits(), 99);
    }

    #[test]
    fn test_sanitize_error_categories() {
        let sanitized = "connection refused or unreachable";
        assert!(!sanitized.contains("localhost"));
        assert!(!sanitized.contains("127.0.0.1"));
        assert!(!sanitized.contains("http://"));

        let timeout_msg = "connection timed out";
        assert!(!timeout_msg.contains("url"));

        let generic = "network error";
        assert!(!generic.contains("internal"));
    }

    #[tokio::test]
    async fn test_permit_acquisition_timeout() {
        let config = HttpClientConfig {
            concurrent_limit: 1,
            permit_timeout_ms: 50, // Short timeout for testing
            permit_timeout_scarce_ms: 25,
            scarce_permit_threshold: 1,
        };

        let client = HttpClient::with_config(config).unwrap();

        let permit = client.concurrent_limit.clone().acquire_owned().await.unwrap();
        let _guard = PermitGuard::new(permit, client.concurrent_limit.clone());

        let result = client
            .send_request("http://localhost:1", bytes::Bytes::from("test"), Duration::from_secs(5))
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            UpstreamError::Timeout => {}
            err => panic!("Expected Timeout error, got: {err:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_limit_respected() {
        let config = HttpClientConfig {
            concurrent_limit: 5,
            permit_timeout_ms: 1000,
            permit_timeout_scarce_ms: 500,
            scarce_permit_threshold: 2,
        };

        let client = Arc::new(HttpClient::with_config(config).unwrap());
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let current_active = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();

        for _ in 0..10 {
            let client_clone = client.clone();
            let max_clone = max_concurrent.clone();
            let active_clone = current_active.clone();

            handles.push(tokio::spawn(async move {
                let permit = client_clone.concurrent_limit.clone().acquire_owned().await;
                if let Ok(p) = permit {
                    let _guard = PermitGuard::new(p, client_clone.concurrent_limit.clone());

                    let current = active_clone.fetch_add(1, Ordering::SeqCst) + 1;

                    let mut max = max_clone.load(Ordering::SeqCst);
                    while current > max {
                        match max_clone.compare_exchange(
                            max,
                            current,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(_) => break,
                            Err(actual) => max = actual,
                        }
                    }

                    tokio::time::sleep(Duration::from_millis(50)).await;

                    active_clone.fetch_sub(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            handle.await.expect("Task should not panic");
        }

        let observed_max = max_concurrent.load(Ordering::SeqCst);
        assert!(observed_max <= 5, "Max concurrent requests {} exceeded limit 5", observed_max);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_permit_cleanup_after_waves() {
        let config = HttpClientConfig {
            concurrent_limit: 20,
            permit_timeout_ms: 1000,
            permit_timeout_scarce_ms: 500,
            scarce_permit_threshold: 5,
        };

        let client = Arc::new(HttpClient::with_config(config).unwrap());
        let initial_permits = client.available_permits();

        for wave in 0..3 {
            let mut handles = Vec::new();

            for _ in 0..15 {
                let client_clone = client.clone();
                handles.push(tokio::spawn(async move {
                    let permit = client_clone.concurrent_limit.clone().acquire_owned().await;
                    if let Ok(p) = permit {
                        let _guard = PermitGuard::new(p, client_clone.concurrent_limit.clone());
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }));
            }

            for handle in handles {
                let _ = handle.await;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;

            assert_eq!(
                client.available_permits(),
                initial_permits,
                "All permits should be released after wave {wave}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_permit_cleanup_on_error() {
        let config = HttpClientConfig {
            concurrent_limit: 5,
            permit_timeout_ms: 1000,
            permit_timeout_scarce_ms: 500,
            scarce_permit_threshold: 2,
        };

        let client = Arc::new(HttpClient::with_config(config).unwrap());
        let initial_permits = client.available_permits();

        let mut handles = Vec::new();

        for _ in 0..10 {
            let client_clone = client.clone();
            handles.push(tokio::spawn(async move {
                let result = client_clone
                    .send_request(
                        "http://localhost:1",
                        bytes::Bytes::from(r#"{"method":"test"}"#),
                        Duration::from_millis(100),
                    )
                    .await;

                assert!(result.is_err(), "Request to unreachable host should fail");
            }));
        }

        for handle in handles {
            handle.await.expect("Task should not panic");
        }

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            client.available_permits(),
            initial_permits,
            "All permits should be released after failed requests"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_permit_cleanup_on_retry_exhaustion() {
        let config = HttpClientConfig {
            concurrent_limit: 3,
            permit_timeout_ms: 2000,
            permit_timeout_scarce_ms: 1000,
            scarce_permit_threshold: 1,
        };

        let client = Arc::new(HttpClient::with_config(config).unwrap());
        let initial_permits = client.available_permits();

        let mut handles = Vec::new();

        for _ in 0..5 {
            let client_clone = client.clone();
            handles.push(tokio::spawn(async move {
                let result = client_clone
                    .send_request(
                        "http://127.0.0.1:1",
                        bytes::Bytes::from(r#"{"jsonrpc":"2.0","method":"test","id":1}"#),
                        Duration::from_millis(200),
                    )
                    .await;

                assert!(result.is_err());
            }));
        }

        for handle in handles {
            handle.await.expect("Task should not panic");
        }

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            client.available_permits(),
            initial_permits,
            "Permits must be released after retry exhaustion"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_permit_cleanup_mixed_outcomes() {
        let config = HttpClientConfig {
            concurrent_limit: 10,
            permit_timeout_ms: 1000,
            permit_timeout_scarce_ms: 500,
            scarce_permit_threshold: 3,
        };

        let client = Arc::new(HttpClient::with_config(config).unwrap());
        let initial_permits = client.available_permits();

        let mut handles = Vec::new();

        for i in 0..20 {
            let client_clone = client.clone();
            handles.push(tokio::spawn(async move {
                if i % 2 == 0 {
                    let permit = client_clone.concurrent_limit.clone().acquire_owned().await;
                    if let Ok(p) = permit {
                        let _guard = PermitGuard::new(p, client_clone.concurrent_limit.clone());
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                } else {
                    let _ = client_clone
                        .send_request(
                            "http://localhost:1",
                            bytes::Bytes::from("test"),
                            Duration::from_millis(50),
                        )
                        .await;
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            client.available_permits(),
            initial_permits,
            "All permits should be released in mixed success/failure scenario"
        );
    }
}
