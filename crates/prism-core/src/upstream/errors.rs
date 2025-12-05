use thiserror::Error;

/// Classification of JSON-RPC errors for intelligent handling.
///
/// Different error categories require different handling strategies:
/// - Client errors don't penalize upstreams
/// - Provider errors trigger circuit breakers and retries
/// - Rate limits trigger backoff without penalties
/// - Execution errors are forwarded to clients without upstream penalties
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RpcErrorCategory {
    /// Client errors: invalid request, method not found, invalid params.
    /// These are NOT the upstream's fault - don't penalize.
    ClientError,
    /// Provider/server errors: internal error, server error.
    /// These ARE the upstream's fault - should penalize.
    ProviderError,
    /// Rate limiting at JSON-RPC level (-32005).
    /// Transient, don't penalize, should retry on different upstream.
    RateLimit,
    /// Parse error from upstream - malformed response.
    ParseError,
    /// Execution errors (reverts, out of gas, etc.) - client's transaction issue.
    ExecutionError,
}

impl RpcErrorCategory {
    /// Classifies a JSON-RPC error code into a category.
    ///
    /// Standard JSON-RPC error codes:
    /// - -32700: Parse error
    /// - -32600: Invalid Request
    /// - -32601: Method not found
    /// - -32602: Invalid params
    /// - -32603: Internal error
    /// - -32000 to -32099: Server errors (varies by message content)
    /// - -32005: Limit exceeded (rate limiting)
    ///
    /// For the -32000 to -32099 range, we inspect the error message to distinguish
    /// between execution errors (client's fault) and provider errors (upstream's fault).
    #[must_use]
    pub fn from_code(code: i32) -> Self {
        match code {
            -32700 => Self::ParseError,
            -32602..=-32600 => Self::ClientError,
            -32603 => Self::ProviderError,
            -32005 => Self::RateLimit,
            // Server error range and unknown errors default to provider error
            _ => Self::ProviderError,
        }
    }

    /// Classifies a JSON-RPC error code and message into a category.
    ///
    /// This variant inspects the error message for execution-related errors in the
    /// -32000 range, providing more accurate classification.
    #[must_use]
    pub fn from_code_and_message(code: i32, message: &str) -> Self {
        match code {
            -32700 => Self::ParseError,
            -32602..=-32600 => Self::ClientError,
            -32603 => Self::ProviderError,
            -32005 => Self::RateLimit,
            // Server error range: check message for execution reverts
            -32099..=-32000 => {
                let message_lower = message.to_lowercase();
                if message_lower.contains("execution reverted") ||
                    message_lower.contains("out of gas") ||
                    message_lower.contains("revert") ||
                    message_lower.contains("insufficient funds") ||
                    message_lower.contains("nonce too low") ||
                    message_lower.contains("gas too low")
                {
                    Self::ExecutionError
                } else {
                    Self::ProviderError
                }
            }
            _ => Self::ProviderError, // Unknown errors treated as provider errors
        }
    }

    /// Returns `true` if this error category represents a transient error.
    ///
    /// Transient errors can be retried on a different upstream or after a delay.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::RateLimit | Self::ProviderError)
    }

    /// Returns `true` if this error should penalize the upstream's health score.
    ///
    /// Only provider errors and parse errors indicate upstream issues.
    #[must_use]
    pub fn should_penalize_upstream(&self) -> bool {
        matches!(self, Self::ProviderError | Self::ParseError)
    }

    /// Returns `true` if this error should trigger the circuit breaker.
    ///
    /// Only provider errors and parse errors indicate systemic upstream issues
    /// that warrant circuit breaker protection.
    #[must_use]
    pub fn should_trigger_circuit_breaker(&self) -> bool {
        matches!(self, Self::ProviderError | Self::ParseError)
    }

    /// Returns a static string representation for metrics labels.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClientError => "client_error",
            Self::ProviderError => "provider_error",
            Self::RateLimit => "rate_limit",
            Self::ParseError => "parse_error",
            Self::ExecutionError => "execution_error",
        }
    }
}

/// Errors that can occur when interacting with upstream RPC providers.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum UpstreamError {
    /// Request exceeded the configured timeout duration.
    #[error("Request timeout")]
    Timeout,

    /// Failed to establish a connection to the upstream endpoint.
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// HTTP-level error occurred (non-2xx status code).
    ///
    /// First field is the HTTP status code, second is the error message.
    #[error("HTTP error: {0}")]
    HttpError(u16, String),

    /// JSON-RPC error returned by the upstream provider.
    ///
    /// First field is the RPC error code, second is the error message.
    #[error("RPC error: {0}")]
    RpcError(i32, String),

    /// Network-level error from the underlying HTTP client.
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    /// Response from upstream could not be parsed or was malformed.
    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    /// All configured upstream endpoints are unhealthy or unavailable.
    #[error("No healthy upstreams available")]
    NoHealthyUpstreams,

    /// Circuit breaker is open, blocking requests to protect the upstream.
    #[error("Circuit breaker is open")]
    CircuitBreakerOpen,

    /// Request validation failed before being sent to upstream.
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// Maximum concurrent requests limit has been reached.
    #[error("Concurrency limit reached: {0}")]
    ConcurrencyLimit(String),

    /// Consensus failure - upstreams could not agree on a response.
    #[error("Consensus failure: {0}")]
    ConsensusFailure(String),
}

impl UpstreamError {
    /// Returns the RPC error category if this is an RPC error.
    ///
    /// Used to determine how to handle JSON-RPC errors from upstreams.
    #[must_use]
    pub fn rpc_category(&self) -> Option<RpcErrorCategory> {
        match self {
            Self::RpcError(code, message) => {
                Some(RpcErrorCategory::from_code_and_message(*code, message))
            }
            _ => None,
        }
    }

    /// Returns `true` if this error is transient and the request should be retried.
    ///
    /// Transient errors include:
    /// - Timeouts (network congestion, slow upstream)
    /// - Network errors (temporary connectivity issues)
    /// - HTTP 5xx server errors (upstream issues)
    /// - HTTP 429 rate limiting (should back off and retry)
    /// - RPC rate limit errors (-32005)
    /// - RPC provider errors (may work on different upstream)
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            // Transient network/connection errors and circuit breaker (will recover)
            Self::Timeout |
            Self::Network(_) |
            Self::ConnectionFailed(_) |
            Self::CircuitBreakerOpen => true,
            Self::HttpError(status, _) => {
                // 5xx server errors or 429 rate limit are transient
                (500..=599).contains(status) || *status == 429
            }
            // Check RPC error category for transient errors
            Self::RpcError(_, _) => self.rpc_category().is_some_and(|cat| cat.is_transient()),
            // All others are not transient
            _ => false,
        }
    }

    /// Returns `true` if this error is permanent and retrying won't help.
    ///
    /// Permanent errors include:
    /// - Invalid requests (bad input from client)
    /// - Invalid responses (parsing errors, likely protocol issues)
    /// - HTTP 4xx client errors (except 429 rate limit)
    /// - RPC client errors (invalid params, method not found, etc.)
    /// - RPC execution errors (transaction reverts, out of gas, etc.)
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::InvalidRequest(_) | Self::InvalidResponse(_) => true,
            Self::HttpError(status, _) => {
                // 4xx client errors are permanent (except 429 which is transient)
                (400..=499).contains(status) && *status != 429
            }
            // Check RPC error category - client and execution errors are permanent
            Self::RpcError(_, _) => self.rpc_category().is_some_and(|cat| !cat.is_transient()),
            // Concurrency limit and consensus failures could be retried
            // NoHealthyUpstreams should trigger fallback, not retry
            _ => false,
        }
    }

    /// Returns `true` if this error should penalize the upstream's health score.
    ///
    /// Errors that indicate upstream issues (not client issues) should penalize:
    /// - Timeouts (upstream is slow or unresponsive)
    /// - Network/connection errors (upstream unreachable)
    /// - HTTP 5xx server errors (upstream has issues)
    /// - Invalid responses (upstream returning malformed data)
    /// - RPC provider errors and parse errors (upstream issues)
    ///
    /// Errors that should NOT penalize:
    /// - Client errors (bad request from client)
    /// - Execution errors (transaction issues from client)
    /// - Rate limits (expected behavior, not an error)
    #[must_use]
    pub fn should_penalize_upstream(&self) -> bool {
        match self {
            Self::Timeout |
            Self::Network(_) |
            Self::ConnectionFailed(_) |
            Self::InvalidResponse(_) => true,
            Self::HttpError(status, _) => {
                // Only 5xx server errors should penalize
                (500..=599).contains(status)
            }
            // Check RPC error category
            Self::RpcError(_, _) => {
                self.rpc_category().is_some_and(|cat| cat.should_penalize_upstream())
            }
            // Client errors, etc. are not the upstream's fault
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rpc_error_category_from_code() {
        // Parse error
        assert_eq!(RpcErrorCategory::from_code(-32700), RpcErrorCategory::ParseError);

        // Client errors
        assert_eq!(RpcErrorCategory::from_code(-32600), RpcErrorCategory::ClientError);
        assert_eq!(RpcErrorCategory::from_code(-32601), RpcErrorCategory::ClientError);
        assert_eq!(RpcErrorCategory::from_code(-32602), RpcErrorCategory::ClientError);

        // Provider errors
        assert_eq!(RpcErrorCategory::from_code(-32603), RpcErrorCategory::ProviderError);

        // Rate limit
        assert_eq!(RpcErrorCategory::from_code(-32005), RpcErrorCategory::RateLimit);

        // Server error range defaults to provider error
        assert_eq!(RpcErrorCategory::from_code(-32000), RpcErrorCategory::ProviderError);
        assert_eq!(RpcErrorCategory::from_code(-32050), RpcErrorCategory::ProviderError);
        assert_eq!(RpcErrorCategory::from_code(-32099), RpcErrorCategory::ProviderError);

        // Unknown codes default to provider error
        assert_eq!(RpcErrorCategory::from_code(-1), RpcErrorCategory::ProviderError);
        assert_eq!(RpcErrorCategory::from_code(100), RpcErrorCategory::ProviderError);
    }

    #[test]
    fn test_rpc_error_category_from_code_and_message_execution_errors() {
        // Execution reverted errors
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32000, "execution reverted"),
            RpcErrorCategory::ExecutionError
        );
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32000, "Execution Reverted: some reason"),
            RpcErrorCategory::ExecutionError
        );

        // Out of gas errors
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32000, "out of gas"),
            RpcErrorCategory::ExecutionError
        );
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32000, "Out of Gas"),
            RpcErrorCategory::ExecutionError
        );

        // Revert errors
        assert_eq!(
            RpcErrorCategory::from_code_and_message(
                -32000,
                "revert: SafeMath: subtraction overflow"
            ),
            RpcErrorCategory::ExecutionError
        );

        // Insufficient funds
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32000, "insufficient funds for transfer"),
            RpcErrorCategory::ExecutionError
        );

        // Nonce errors
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32000, "nonce too low"),
            RpcErrorCategory::ExecutionError
        );

        // Gas too low
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32000, "gas too low"),
            RpcErrorCategory::ExecutionError
        );
    }

    #[test]
    fn test_rpc_error_category_from_code_and_message_provider_errors() {
        // Non-execution errors in -32000 range should be provider errors
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32000, "server error"),
            RpcErrorCategory::ProviderError
        );
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32001, "internal server error"),
            RpcErrorCategory::ProviderError
        );

        // Standard provider errors
        assert_eq!(
            RpcErrorCategory::from_code_and_message(-32603, "Internal error"),
            RpcErrorCategory::ProviderError
        );
    }

    #[test]
    fn test_rpc_error_category_is_transient() {
        // Transient categories
        assert!(RpcErrorCategory::RateLimit.is_transient());
        assert!(RpcErrorCategory::ProviderError.is_transient());

        // Non-transient categories
        assert!(!RpcErrorCategory::ClientError.is_transient());
        assert!(!RpcErrorCategory::ParseError.is_transient());
        assert!(!RpcErrorCategory::ExecutionError.is_transient());
    }

    #[test]
    fn test_rpc_error_category_should_penalize_upstream() {
        // Should penalize
        assert!(RpcErrorCategory::ProviderError.should_penalize_upstream());
        assert!(RpcErrorCategory::ParseError.should_penalize_upstream());

        // Should not penalize
        assert!(!RpcErrorCategory::ClientError.should_penalize_upstream());
        assert!(!RpcErrorCategory::RateLimit.should_penalize_upstream());
        assert!(!RpcErrorCategory::ExecutionError.should_penalize_upstream());
    }

    #[test]
    fn test_rpc_error_category_should_trigger_circuit_breaker() {
        // Should trigger
        assert!(RpcErrorCategory::ProviderError.should_trigger_circuit_breaker());
        assert!(RpcErrorCategory::ParseError.should_trigger_circuit_breaker());

        // Should not trigger
        assert!(!RpcErrorCategory::ClientError.should_trigger_circuit_breaker());
        assert!(!RpcErrorCategory::RateLimit.should_trigger_circuit_breaker());
        assert!(!RpcErrorCategory::ExecutionError.should_trigger_circuit_breaker());
    }

    #[test]
    fn test_rpc_error_category_as_str() {
        assert_eq!(RpcErrorCategory::ClientError.as_str(), "client_error");
        assert_eq!(RpcErrorCategory::ProviderError.as_str(), "provider_error");
        assert_eq!(RpcErrorCategory::RateLimit.as_str(), "rate_limit");
        assert_eq!(RpcErrorCategory::ParseError.as_str(), "parse_error");
        assert_eq!(RpcErrorCategory::ExecutionError.as_str(), "execution_error");
    }

    #[test]
    fn test_upstream_error_rpc_category() {
        // Client error
        let err = UpstreamError::RpcError(-32600, "Invalid Request".to_string());
        assert_eq!(err.rpc_category(), Some(RpcErrorCategory::ClientError));

        // Provider error
        let err = UpstreamError::RpcError(-32603, "Internal error".to_string());
        assert_eq!(err.rpc_category(), Some(RpcErrorCategory::ProviderError));

        // Rate limit
        let err = UpstreamError::RpcError(-32005, "Limit exceeded".to_string());
        assert_eq!(err.rpc_category(), Some(RpcErrorCategory::RateLimit));

        // Execution error
        let err = UpstreamError::RpcError(-32000, "execution reverted".to_string());
        assert_eq!(err.rpc_category(), Some(RpcErrorCategory::ExecutionError));

        // Non-RPC error
        assert_eq!(UpstreamError::Timeout.rpc_category(), None);
    }

    #[test]
    fn test_transient_errors() {
        // Transient errors
        assert!(UpstreamError::Timeout.is_transient());
        assert!(UpstreamError::ConnectionFailed("test".into()).is_transient());
        assert!(UpstreamError::HttpError(500, "Internal Server Error".into()).is_transient());
        assert!(UpstreamError::HttpError(502, "Bad Gateway".into()).is_transient());
        assert!(UpstreamError::HttpError(503, "Service Unavailable".into()).is_transient());
        assert!(UpstreamError::HttpError(429, "Too Many Requests".into()).is_transient());
        assert!(UpstreamError::CircuitBreakerOpen.is_transient());

        // Non-transient errors
        assert!(!UpstreamError::InvalidRequest("bad".into()).is_transient());
        assert!(!UpstreamError::InvalidResponse("bad".into()).is_transient());
        assert!(!UpstreamError::HttpError(400, "Bad Request".into()).is_transient());
        assert!(!UpstreamError::HttpError(404, "Not Found".into()).is_transient());
        assert!(!UpstreamError::RpcError(-32600, "Invalid Request".into()).is_transient());

        // RPC errors - transient
        assert!(UpstreamError::RpcError(-32005, "Limit exceeded".into()).is_transient());
        assert!(UpstreamError::RpcError(-32603, "Internal error".into()).is_transient());

        // RPC errors - non-transient
        assert!(!UpstreamError::RpcError(-32600, "Invalid Request".into()).is_transient());
        assert!(!UpstreamError::RpcError(-32000, "execution reverted".into()).is_transient());
    }

    #[test]
    fn test_permanent_errors() {
        // Permanent errors
        assert!(UpstreamError::InvalidRequest("bad".into()).is_permanent());
        assert!(UpstreamError::InvalidResponse("bad".into()).is_permanent());
        assert!(UpstreamError::HttpError(400, "Bad Request".into()).is_permanent());
        assert!(UpstreamError::HttpError(401, "Unauthorized".into()).is_permanent());
        assert!(UpstreamError::HttpError(403, "Forbidden".into()).is_permanent());
        assert!(UpstreamError::HttpError(404, "Not Found".into()).is_permanent());
        assert!(UpstreamError::RpcError(-32600, "Invalid Request".into()).is_permanent());

        // Non-permanent errors
        assert!(!UpstreamError::Timeout.is_permanent());
        assert!(!UpstreamError::HttpError(500, "Internal Server Error".into()).is_permanent());
        assert!(!UpstreamError::HttpError(429, "Too Many Requests".into()).is_permanent());
        assert!(!UpstreamError::CircuitBreakerOpen.is_permanent());

        // RPC errors - permanent
        assert!(UpstreamError::RpcError(-32600, "Invalid Request".into()).is_permanent());
        assert!(UpstreamError::RpcError(-32601, "Method not found".into()).is_permanent());
        assert!(UpstreamError::RpcError(-32000, "execution reverted".into()).is_permanent());

        // RPC errors - non-permanent (transient)
        assert!(!UpstreamError::RpcError(-32005, "Limit exceeded".into()).is_permanent());
        assert!(!UpstreamError::RpcError(-32603, "Internal error".into()).is_permanent());
    }

    #[test]
    fn test_penalize_upstream() {
        // Should penalize
        assert!(UpstreamError::Timeout.should_penalize_upstream());
        assert!(UpstreamError::ConnectionFailed("test".into()).should_penalize_upstream());
        assert!(UpstreamError::InvalidResponse("bad".into()).should_penalize_upstream());
        assert!(UpstreamError::HttpError(500, "Internal Server Error".into())
            .should_penalize_upstream());
        assert!(UpstreamError::HttpError(502, "Bad Gateway".into()).should_penalize_upstream());

        // Should not penalize (client errors)
        assert!(!UpstreamError::InvalidRequest("bad".into()).should_penalize_upstream());
        assert!(!UpstreamError::HttpError(400, "Bad Request".into()).should_penalize_upstream());
        assert!(
            !UpstreamError::HttpError(429, "Too Many Requests".into()).should_penalize_upstream()
        );
        assert!(
            !UpstreamError::RpcError(-32600, "Invalid Request".into()).should_penalize_upstream()
        );

        // RPC errors - should penalize (provider errors)
        assert!(UpstreamError::RpcError(-32603, "Internal error".into()).should_penalize_upstream());
        assert!(UpstreamError::RpcError(-32001, "Server error".into()).should_penalize_upstream());

        // RPC errors - should not penalize (client/execution/rate limit)
        assert!(
            !UpstreamError::RpcError(-32600, "Invalid Request".into()).should_penalize_upstream()
        );
        assert!(
            !UpstreamError::RpcError(-32005, "Limit exceeded".into()).should_penalize_upstream()
        );
        assert!(!UpstreamError::RpcError(-32000, "execution reverted".into())
            .should_penalize_upstream());
    }
}
