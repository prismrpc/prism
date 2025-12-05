//! Partial result handling for range-based operations.
//!
//! When fetching block ranges (e.g., for `eth_getLogs`), individual sub-ranges may fail
//! while others succeed. This module provides types to represent partial success,
//! allowing callers to:
//!
//! 1. Use successfully fetched data immediately
//! 2. Retry only the failed ranges
//! 3. Communicate partial success to clients for graceful degradation
//!
//! # Example
//!
//! ```rust,ignore
//! use prism_core::proxy::partial::{PartialResult, RangeFailure};
//!
//! // Fetch logs for range 100-200, suppose 150-160 failed
//! let result: PartialResult<Vec<Log>> = fetch_logs_with_partial(100, 200).await;
//!
//! if result.is_complete() {
//!     // Full success - all data available
//!     return Ok(result.data);
//! } else {
//!     // Partial success - log failures and return what we have
//!     for failure in &result.failed_ranges {
//!         warn!(from = failure.from_block, to = failure.to_block, "range fetch failed");
//!     }
//!     // Could retry failed ranges or return partial data with warning
//! }
//! ```

use super::errors::ProxyError;
use std::{fmt, time::Duration};

/// Configuration for retrying failed range fetches.
///
/// Controls exponential backoff with jitter to prevent thundering herd
/// when retrying failed ranges concurrently.
///
/// # Example
///
/// ```rust,ignore
/// use prism_core::proxy::partial::RetryConfig;
///
/// let config = RetryConfig::default()
///     .with_max_retries(3)
///     .with_base_delay_ms(100)
///     .with_max_delay_ms(2000);
/// ```
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts per range.
    ///
    /// After this many retries, the range is considered permanently failed.
    /// Default: 3
    pub max_retries: u32,

    /// Base delay in milliseconds for exponential backoff.
    ///
    /// Actual delay = `base_delay_ms * 2^attempt` (capped at `max_delay_ms`).
    /// Default: 100ms
    pub base_delay_ms: u64,

    /// Maximum delay in milliseconds between retries.
    ///
    /// Caps the exponential backoff to prevent excessively long waits.
    /// Default: 2000ms (2 seconds)
    pub max_delay_ms: u64,

    /// Jitter factor (0.0-1.0) added to delays to prevent thundering herd.
    ///
    /// A value of 0.25 means delays vary by ±12.5% of the calculated delay.
    /// Default: 0.25
    pub jitter_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self { max_retries: 3, base_delay_ms: 100, max_delay_ms: 2000, jitter_factor: 0.25 }
    }
}

impl RetryConfig {
    /// Creates a new retry configuration with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum number of retry attempts.
    #[must_use]
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Sets the base delay in milliseconds.
    #[must_use]
    pub fn with_base_delay_ms(mut self, base_delay_ms: u64) -> Self {
        self.base_delay_ms = base_delay_ms;
        self
    }

    /// Sets the maximum delay in milliseconds.
    #[must_use]
    pub fn with_max_delay_ms(mut self, max_delay_ms: u64) -> Self {
        self.max_delay_ms = max_delay_ms;
        self
    }

    /// Sets the jitter factor.
    #[must_use]
    pub fn with_jitter_factor(mut self, jitter_factor: f64) -> Self {
        self.jitter_factor = jitter_factor.clamp(0.0, 1.0);
        self
    }

    /// Calculates the delay for a given retry attempt.
    ///
    /// Uses exponential backoff with jitter: `base * 2^attempt + jitter`.
    ///
    /// # Arguments
    /// * `attempt` - The retry attempt number (0-indexed)
    ///
    /// # Returns
    /// The duration to wait before retrying.
    #[must_use]
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        use rand::Rng;

        // Exponential backoff: base * 2^attempt, capped at max
        let base_delay = self.base_delay_ms.saturating_mul(1u64 << attempt.min(10));
        let capped_delay = base_delay.min(self.max_delay_ms);

        // Add jitter: delay * (1.0 ± jitter_factor/2)
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let jitter_range = (capped_delay as f64 * self.jitter_factor) as u64;
        let jitter_offset = if jitter_range > 0 {
            rand::rng().random_range(0..jitter_range)
        } else {
            0
        };

        Duration::from_millis(capped_delay.saturating_sub(jitter_range / 2) + jitter_offset)
    }

    /// Returns true if another retry should be attempted.
    ///
    /// # Arguments
    /// * `current_attempt` - The current retry attempt number (0-indexed)
    #[must_use]
    pub fn should_retry(&self, current_attempt: u32) -> bool {
        current_attempt < self.max_retries
    }
}

/// Represents a partial success with some failed ranges.
///
/// This type enables graceful degradation when fetching block ranges:
/// - `data` contains all successfully retrieved items
/// - `failed_ranges` lists ranges that could not be fetched
/// - `is_complete` indicates whether all requested data was retrieved
///
/// # Type Parameter
///
/// `T` is typically `Vec<Log>` for log queries, but can be any collection type.
#[derive(Debug, Clone)]
pub struct PartialResult<T> {
    /// Successfully retrieved data.
    ///
    /// For log queries, this contains all logs from successfully fetched ranges.
    /// The data is unsorted - caller must sort if ordering is required.
    pub data: T,

    /// Ranges that failed to fetch.
    ///
    /// Each failure includes the block range, error details, and retry metadata.
    /// Empty if all ranges succeeded.
    pub failed_ranges: Vec<RangeFailure>,

    /// Whether the result is complete (no failures).
    ///
    /// Convenience field equivalent to `failed_ranges.is_empty()`.
    /// Useful for quick success checks without allocating.
    pub is_complete: bool,
}

impl<T> PartialResult<T> {
    /// Creates a new complete result with no failures.
    #[must_use]
    pub fn complete(data: T) -> Self {
        Self { data, failed_ranges: Vec::new(), is_complete: true }
    }

    /// Creates a new partial result with some failures.
    #[must_use]
    pub fn partial(data: T, failed_ranges: Vec<RangeFailure>) -> Self {
        let is_complete = failed_ranges.is_empty();
        Self { data, failed_ranges, is_complete }
    }

    /// Creates a completely failed result with no data.
    #[must_use]
    pub fn failed(failed_ranges: Vec<RangeFailure>) -> Self
    where
        T: Default,
    {
        Self { data: T::default(), failed_ranges, is_complete: false }
    }

    /// Returns true if all requested data was successfully retrieved.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.is_complete
    }

    /// Returns true if at least some data was successfully retrieved.
    #[must_use]
    pub fn has_data(&self) -> bool
    where
        T: HasLength,
    {
        self.data.len() > 0
    }

    /// Returns the number of failed ranges.
    #[must_use]
    pub fn failure_count(&self) -> usize {
        self.failed_ranges.len()
    }

    /// Returns the total number of failed blocks across all ranges.
    #[must_use]
    pub fn failed_block_count(&self) -> u64 {
        self.failed_ranges.iter().map(RangeFailure::block_count).sum()
    }

    /// Extracts failed ranges that can be retried.
    ///
    /// Returns ranges where the error is transient (not permanent).
    /// Permanent errors (e.g., invalid request) are excluded.
    #[must_use]
    pub fn retryable_ranges(&self) -> Vec<&RangeFailure> {
        self.failed_ranges.iter().filter(|r| r.is_retryable()).collect()
    }

    /// Converts the data using a mapping function.
    ///
    /// Preserves failure information while transforming the data type.
    pub fn map<U, F>(self, f: F) -> PartialResult<U>
    where
        F: FnOnce(T) -> U,
    {
        PartialResult {
            data: f(self.data),
            failed_ranges: self.failed_ranges,
            is_complete: self.is_complete,
        }
    }
}

impl<T: Default> Default for PartialResult<T> {
    fn default() -> Self {
        Self::complete(T::default())
    }
}

/// Information about a failed range fetch.
///
/// Captures the block range that failed, the error encountered, and metadata
/// useful for retry decisions and observability.
#[derive(Debug, Clone)]
pub struct RangeFailure {
    /// Start block of the failed range (inclusive).
    pub from_block: u64,

    /// End block of the failed range (inclusive).
    pub to_block: u64,

    /// The error that occurred.
    ///
    /// Wrapped in an Arc to allow cloning without cloning the full error.
    pub error: RangeError,

    /// Name of the upstream that failed (if known).
    ///
    /// `None` if the failure occurred before upstream selection.
    pub upstream: Option<String>,

    /// Number of retry attempts before this failure.
    ///
    /// 0 means this was the first attempt.
    pub retry_count: u32,
}

impl RangeFailure {
    /// Creates a new range failure.
    #[must_use]
    pub fn new(from_block: u64, to_block: u64, error: ProxyError) -> Self {
        Self {
            from_block,
            to_block,
            error: RangeError::from(error),
            upstream: None,
            retry_count: 0,
        }
    }

    /// Sets the upstream name for this failure.
    #[must_use]
    pub fn with_upstream(mut self, upstream: impl Into<String>) -> Self {
        self.upstream = Some(upstream.into());
        self
    }

    /// Sets the retry count for this failure.
    #[must_use]
    pub fn with_retry_count(mut self, count: u32) -> Self {
        self.retry_count = count;
        self
    }

    /// Returns the number of blocks in this failed range.
    #[must_use]
    pub fn block_count(&self) -> u64 {
        self.to_block.saturating_sub(self.from_block) + 1
    }

    /// Returns true if this failure can be retried.
    ///
    /// Transient errors (timeouts, rate limits, temporary unavailability)
    /// are retryable. Permanent errors (invalid request, not found) are not.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.error.is_retryable()
    }
}

impl fmt::Display for RangeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blocks {}-{} failed: {}", self.from_block, self.to_block, self.error)?;
        if let Some(ref upstream) = self.upstream {
            write!(f, " (upstream: {upstream})")?;
        }
        if self.retry_count > 0 {
            write!(f, " (retries: {})", self.retry_count)?;
        }
        Ok(())
    }
}

/// Simplified error type for range failures.
///
/// Captures the essential error information without the full `ProxyError` overhead.
/// Designed for efficient storage and comparison.
#[derive(Debug, Clone)]
pub enum RangeError {
    /// Request timed out.
    Timeout,

    /// Rate limited by upstream (HTTP 429).
    RateLimited,

    /// Upstream returned an error response.
    UpstreamError {
        /// Error code from upstream (if available).
        code: Option<i32>,
        /// Error message from upstream.
        message: String,
    },

    /// Connection or network error.
    NetworkError(String),

    /// Internal error during processing.
    Internal(String),

    /// Request was invalid (not retryable).
    InvalidRequest(String),
}

impl RangeError {
    /// Returns true if this error is transient and can be retried.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            RangeError::Timeout | RangeError::RateLimited | RangeError::NetworkError(_) => true,
            RangeError::UpstreamError { code, .. } => {
                // Retry server errors (JSON-RPC reserved range -32000 to -32099)
                code.is_none_or(|c| (-32099..=-32000).contains(&c))
            }
            RangeError::Internal(_) | RangeError::InvalidRequest(_) => false,
        }
    }
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeError::Timeout => write!(f, "request timed out"),
            RangeError::RateLimited => write!(f, "rate limited"),
            RangeError::UpstreamError { code, message } => {
                if let Some(c) = code {
                    write!(f, "upstream error ({c}): {message}")
                } else {
                    write!(f, "upstream error: {message}")
                }
            }
            RangeError::NetworkError(msg) => write!(f, "network error: {msg}"),
            RangeError::Internal(msg) => write!(f, "internal error: {msg}"),
            RangeError::InvalidRequest(msg) => write!(f, "invalid request: {msg}"),
        }
    }
}

impl From<ProxyError> for RangeError {
    fn from(err: ProxyError) -> Self {
        use crate::upstream::errors::UpstreamError;

        match err {
            ProxyError::RateLimited => RangeError::RateLimited,
            ProxyError::InvalidRequest(msg) | ProxyError::MethodNotSupported(msg) => {
                RangeError::InvalidRequest(msg)
            }
            ProxyError::Validation(val_err) => RangeError::InvalidRequest(val_err.to_string()),
            ProxyError::Internal(msg) => RangeError::Internal(msg),
            ProxyError::Upstream(upstream_err) => match upstream_err {
                UpstreamError::Timeout => RangeError::Timeout,
                UpstreamError::ConnectionFailed(msg) | UpstreamError::ConcurrencyLimit(msg) => {
                    RangeError::NetworkError(msg)
                }
                UpstreamError::HttpError(code, msg) => {
                    RangeError::UpstreamError { code: Some(i32::from(code)), message: msg }
                }
                UpstreamError::RpcError(code, msg) => {
                    RangeError::UpstreamError { code: Some(code), message: msg }
                }
                UpstreamError::Network(req_err) => RangeError::NetworkError(req_err.to_string()),
                UpstreamError::InvalidResponse(msg) => {
                    RangeError::UpstreamError { code: None, message: msg }
                }
                UpstreamError::NoHealthyUpstreams => {
                    RangeError::NetworkError("no healthy upstreams available".to_string())
                }
                UpstreamError::CircuitBreakerOpen => {
                    RangeError::NetworkError("circuit breaker open".to_string())
                }
                UpstreamError::InvalidRequest(msg) => RangeError::InvalidRequest(msg),
                // Handle remaining variants as generic upstream errors
                _ => RangeError::UpstreamError { code: None, message: upstream_err.to_string() },
            },
        }
    }
}

/// Trait for types that have a length.
///
/// Used to check if partial results have any data.
pub trait HasLength {
    /// Returns the number of items.
    fn len(&self) -> usize;

    /// Returns true if empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> HasLength for Vec<T> {
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

/// Result of fetching a single range, preserving range info for failure tracking.
///
/// Used internally by concurrent range fetchers to preserve range context
/// when errors occur, enabling construction of `PartialResult`.
#[derive(Debug)]
pub struct RangeFetchResult<T> {
    /// Start block of the range (inclusive).
    pub from_block: u64,

    /// End block of the range (inclusive).
    pub to_block: u64,

    /// The result of fetching this range.
    pub result: Result<T, ProxyError>,

    /// Name of the upstream used (if known).
    pub upstream: Option<String>,
}

impl<T> RangeFetchResult<T> {
    /// Creates a successful result.
    #[must_use]
    pub fn success(from_block: u64, to_block: u64, data: T) -> Self {
        Self { from_block, to_block, result: Ok(data), upstream: None }
    }

    /// Creates a failed result.
    #[must_use]
    pub fn failure(from_block: u64, to_block: u64, error: ProxyError) -> Self {
        Self { from_block, to_block, result: Err(error), upstream: None }
    }

    /// Sets the upstream name.
    #[must_use]
    pub fn with_upstream(mut self, upstream: impl Into<String>) -> Self {
        self.upstream = Some(upstream.into());
        self
    }

    /// Returns true if this range was fetched successfully.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.result.is_ok()
    }

    /// Converts a failure into a `RangeFailure`.
    ///
    /// Returns `None` if this was a success.
    #[must_use]
    pub fn into_failure(self) -> Option<RangeFailure> {
        match self.result {
            Ok(_) => None,
            Err(e) => {
                let mut failure = RangeFailure::new(self.from_block, self.to_block, e);
                if let Some(upstream) = self.upstream {
                    failure = failure.with_upstream(upstream);
                }
                Some(failure)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::errors::UpstreamError;

    /// Helper to create a timeout error for testing.
    fn timeout_error() -> ProxyError {
        ProxyError::Upstream(UpstreamError::Timeout)
    }

    /// Helper to create a no healthy upstreams error for testing.
    fn no_upstreams_error() -> ProxyError {
        ProxyError::Upstream(UpstreamError::NoHealthyUpstreams)
    }

    #[test]
    fn test_partial_result_complete() {
        let result: PartialResult<Vec<u32>> = PartialResult::complete(vec![1, 2, 3]);

        assert!(result.is_complete());
        assert!(result.has_data());
        assert_eq!(result.failure_count(), 0);
        assert_eq!(result.failed_block_count(), 0);
    }

    #[test]
    fn test_partial_result_partial() {
        let failures = vec![RangeFailure::new(100, 110, timeout_error())];

        let result: PartialResult<Vec<u32>> = PartialResult::partial(vec![1, 2], failures);

        assert!(!result.is_complete());
        assert!(result.has_data());
        assert_eq!(result.failure_count(), 1);
        assert_eq!(result.failed_block_count(), 11);
    }

    #[test]
    fn test_partial_result_failed() {
        let failures = vec![
            RangeFailure::new(100, 110, timeout_error()),
            RangeFailure::new(200, 220, no_upstreams_error()),
        ];

        let result: PartialResult<Vec<u32>> = PartialResult::failed(failures);

        assert!(!result.is_complete());
        assert!(!result.has_data());
        assert_eq!(result.failure_count(), 2);
        assert_eq!(result.failed_block_count(), 32);
    }

    #[test]
    fn test_range_failure_display() {
        let failure = RangeFailure::new(100, 200, timeout_error())
            .with_upstream("alchemy")
            .with_retry_count(2);

        let display = failure.to_string();
        assert!(display.contains("100-200"));
        assert!(display.contains("alchemy"));
        assert!(display.contains("retries: 2"));
    }

    #[test]
    fn test_range_error_retryable() {
        assert!(RangeError::Timeout.is_retryable());
        assert!(RangeError::RateLimited.is_retryable());
        assert!(RangeError::NetworkError("test".into()).is_retryable());
        assert!(!RangeError::InvalidRequest("test".into()).is_retryable());
        assert!(!RangeError::Internal("test".into()).is_retryable());
    }

    #[test]
    fn test_retryable_ranges() {
        let failures = vec![
            RangeFailure::new(100, 110, timeout_error()),
            RangeFailure::new(200, 210, ProxyError::InvalidRequest("bad".into())),
            RangeFailure::new(300, 310, no_upstreams_error()),
        ];

        let result: PartialResult<Vec<u32>> = PartialResult::partial(vec![], failures);

        let retryable = result.retryable_ranges();
        assert_eq!(retryable.len(), 2);
        assert_eq!(retryable[0].from_block, 100);
        assert_eq!(retryable[1].from_block, 300);
    }

    #[test]
    fn test_partial_result_map() {
        let result: PartialResult<Vec<u32>> = PartialResult::complete(vec![1, 2, 3]);

        let mapped = result.map(|v| v.into_iter().map(|x| x * 2).collect::<Vec<_>>());

        assert!(mapped.is_complete());
        assert_eq!(mapped.data, vec![2, 4, 6]);
    }

    #[test]
    fn test_range_error_from_proxy_error() {
        // Test timeout conversion
        let timeout = RangeError::from(timeout_error());
        assert!(matches!(timeout, RangeError::Timeout));

        // Test rate limited conversion
        let rate_limited = RangeError::from(ProxyError::RateLimited);
        assert!(matches!(rate_limited, RangeError::RateLimited));

        // Test invalid request conversion
        let invalid = RangeError::from(ProxyError::InvalidRequest("test".into()));
        assert!(matches!(invalid, RangeError::InvalidRequest(_)));

        // Test RPC error conversion
        let rpc_err =
            RangeError::from(ProxyError::Upstream(UpstreamError::RpcError(-32000, "limit".into())));
        assert!(matches!(rpc_err, RangeError::UpstreamError { code: Some(-32000), .. }));
    }

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay_ms, 100);
        assert_eq!(config.max_delay_ms, 2000);
        assert!((config.jitter_factor - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn test_retry_config_builder() {
        let config = RetryConfig::new()
            .with_max_retries(5)
            .with_base_delay_ms(50)
            .with_max_delay_ms(5000)
            .with_jitter_factor(0.5);

        assert_eq!(config.max_retries, 5);
        assert_eq!(config.base_delay_ms, 50);
        assert_eq!(config.max_delay_ms, 5000);
        assert!((config.jitter_factor - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_retry_config_jitter_clamping() {
        // Jitter should be clamped to [0.0, 1.0]
        let config = RetryConfig::new().with_jitter_factor(2.0);
        assert!((config.jitter_factor - 1.0).abs() < f64::EPSILON);

        let config = RetryConfig::new().with_jitter_factor(-0.5);
        assert!(config.jitter_factor.abs() < f64::EPSILON);
    }

    #[test]
    fn test_retry_config_should_retry() {
        let config = RetryConfig::new().with_max_retries(3);

        assert!(config.should_retry(0));
        assert!(config.should_retry(1));
        assert!(config.should_retry(2));
        assert!(!config.should_retry(3));
        assert!(!config.should_retry(4));
    }

    #[test]
    fn test_retry_config_calculate_delay_exponential() {
        // Use zero jitter for predictable testing
        let config = RetryConfig::new()
            .with_base_delay_ms(100)
            .with_max_delay_ms(10000)
            .with_jitter_factor(0.0);

        // Exponential: 100 * 2^0 = 100
        assert_eq!(config.calculate_delay(0).as_millis(), 100);
        // Exponential: 100 * 2^1 = 200
        assert_eq!(config.calculate_delay(1).as_millis(), 200);
        // Exponential: 100 * 2^2 = 400
        assert_eq!(config.calculate_delay(2).as_millis(), 400);
        // Exponential: 100 * 2^3 = 800
        assert_eq!(config.calculate_delay(3).as_millis(), 800);
    }

    #[test]
    fn test_retry_config_calculate_delay_capped() {
        let config = RetryConfig::new()
            .with_base_delay_ms(100)
            .with_max_delay_ms(500)
            .with_jitter_factor(0.0);

        // Exponential: 100 * 2^0 = 100
        assert_eq!(config.calculate_delay(0).as_millis(), 100);
        // Exponential: 100 * 2^1 = 200
        assert_eq!(config.calculate_delay(1).as_millis(), 200);
        // Exponential: 100 * 2^2 = 400
        assert_eq!(config.calculate_delay(2).as_millis(), 400);
        // Exponential: 100 * 2^3 = 800, but capped at 500
        assert_eq!(config.calculate_delay(3).as_millis(), 500);
        // Still capped
        assert_eq!(config.calculate_delay(10).as_millis(), 500);
    }

    #[test]
    fn test_retry_config_calculate_delay_with_jitter() {
        let config = RetryConfig::new()
            .with_base_delay_ms(100)
            .with_max_delay_ms(10000)
            .with_jitter_factor(0.5);

        // With jitter, delay should be within expected range
        // Base delay = 100, jitter = 50%, so range is [75, 125]
        let delay = config.calculate_delay(0);
        assert!(delay.as_millis() >= 75);
        assert!(delay.as_millis() <= 125);
    }
}
