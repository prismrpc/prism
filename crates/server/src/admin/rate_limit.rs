//! Rate limiting middleware for admin API endpoints.
//!
//! Implements a token bucket algorithm to protect against `DoS` attacks through
//! expensive operations like Prometheus queries and bulk health checks.

#![allow(clippy::missing_errors_doc)]

use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use dashmap::DashMap;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

/// Token bucket rate limiter state.
///
/// Uses a token bucket algorithm for rate limiting based on IP address.
/// Each client IP gets its own bucket with a configurable number of tokens
/// that refill at a constant rate.
///
/// # Algorithm
///
/// - Each bucket starts with `max_tokens` tokens
/// - Tokens refill at `refill_rate` per second
/// - Each request consumes 1 token
/// - If tokens are less than 1.0, request is rejected with 429 Too Many Requests
///
/// # Example
///
/// ```rust,ignore
/// // Allow 100 requests burst, 10 requests/sec sustained
/// let limiter = RateLimiter::new(100, 10);
/// ```
pub struct RateLimiter {
    /// Map of IP address -> token bucket state
    buckets: DashMap<String, TokenBucket>,
    /// Maximum tokens per bucket (burst capacity)
    max_tokens: u32,
    /// Tokens refilled per second (sustained rate)
    refill_rate: u32,
}

/// Internal token bucket state for a single client.
struct TokenBucket {
    /// Current number of tokens (fractional for smooth refill)
    tokens: f64,
    /// Last time tokens were refilled
    last_refill: Instant,
}

impl RateLimiter {
    /// Creates a new rate limiter with the specified capacity and refill rate.
    ///
    /// # Arguments
    ///
    /// * `max_tokens` - Maximum tokens per bucket (burst capacity)
    /// * `refill_rate` - Tokens refilled per second (sustained rate)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // 100 requests burst, 10/sec sustained
    /// let limiter = RateLimiter::new(100, 10);
    /// ```
    #[must_use]
    pub fn new(max_tokens: u32, refill_rate: u32) -> Self {
        Self { buckets: DashMap::new(), max_tokens, refill_rate }
    }

    /// Checks if a request from the given key should be allowed.
    ///
    /// Automatically refills tokens based on elapsed time since last check.
    /// Consumes one token if available.
    ///
    /// # Arguments
    ///
    /// * `key` - Client identifier (typically IP address)
    ///
    /// # Returns
    ///
    /// `true` if the request is allowed, `false` if rate limited
    #[must_use]
    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();

        let mut bucket = self.buckets.entry(key.to_string()).or_insert_with(|| TokenBucket {
            tokens: f64::from(self.max_tokens),
            last_refill: now,
        });

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens =
            (bucket.tokens + elapsed * f64::from(self.refill_rate)).min(f64::from(self.max_tokens));
        bucket.last_refill = now;

        // Try to consume a token
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Removes idle entries from the rate limiter cache.
    ///
    /// Should be called periodically from a background task to prevent
    /// unbounded memory growth. Removes entries that haven't been accessed
    /// in 5 minutes.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Run cleanup every minute
    /// tokio::spawn(async move {
    ///     loop {
    ///         tokio::time::sleep(Duration::from_secs(60)).await;
    ///         limiter.cleanup();
    ///     }
    /// });
    /// ```
    pub fn cleanup(&self) {
        let cutoff = Instant::now()
            .checked_sub(Duration::from_secs(300))
            .unwrap_or_else(Instant::now); // 5 min idle
        self.buckets.retain(|_, bucket| bucket.last_refill > cutoff);
    }

    /// Returns the current number of tracked IP addresses.
    ///
    /// Useful for monitoring and metrics.
    #[must_use]
    pub fn tracked_ips_count(&self) -> usize {
        self.buckets.len()
    }
}

/// Axum middleware for rate limiting admin API requests.
///
/// Uses the client's IP address (from `ConnectInfo`) as the rate limit key.
/// Returns 429 Too Many Requests if the client exceeds their rate limit.
///
/// # Security
///
/// - Rate limits are per-IP address
/// - Uses token bucket for smooth rate limiting
/// - Burst capacity allows legitimate spikes
/// - Sustained rate prevents abuse
///
/// # Example
///
/// ```rust,ignore
/// use axum::{Router, middleware};
/// use std::sync::Arc;
///
/// let limiter = Arc::new(RateLimiter::new(100, 10));
/// let router = Router::new()
///     .route("/admin/status", get(handler))
///     .layer(middleware::from_fn_with_state(limiter, rate_limit_middleware));
/// ```
pub async fn rate_limit_middleware(
    State(limiter): State<Arc<RateLimiter>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let key = addr.ip().to_string();

    if limiter.check(&key) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::TOO_MANY_REQUESTS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_allows_within_burst() {
        let limiter = RateLimiter::new(5, 1);

        // Should allow 5 requests (burst capacity)
        for _ in 0..5 {
            assert!(limiter.check("192.168.1.1"));
        }

        // 6th request should be rejected
        assert!(!limiter.check("192.168.1.1"));
    }

    #[test]
    fn test_rate_limiter_refills_over_time() {
        let limiter = RateLimiter::new(2, 10); // 2 tokens, 10/sec refill

        // Consume all tokens
        assert!(limiter.check("192.168.1.1"));
        assert!(limiter.check("192.168.1.1"));
        assert!(!limiter.check("192.168.1.1"));

        // Wait 100ms (should refill 1 token: 10 tokens/sec * 0.1s = 1 token)
        std::thread::sleep(Duration::from_millis(100));

        // Should have ~1 token now
        assert!(limiter.check("192.168.1.1"));
        assert!(!limiter.check("192.168.1.1"));
    }

    #[test]
    fn test_rate_limiter_isolates_clients() {
        let limiter = RateLimiter::new(1, 1);

        // First client uses their token
        assert!(limiter.check("192.168.1.1"));
        assert!(!limiter.check("192.168.1.1"));

        // Second client should have their own token
        assert!(limiter.check("192.168.1.2"));
        assert!(!limiter.check("192.168.1.2"));
    }

    #[test]
    fn test_rate_limiter_cleanup() {
        let limiter = RateLimiter::new(10, 5);

        // Add some entries
        let _ = limiter.check("192.168.1.1");
        let _ = limiter.check("192.168.1.2");
        let _ = limiter.check("192.168.1.3");

        assert_eq!(limiter.tracked_ips_count(), 3);

        // Cleanup won't remove recent entries (they're within 5 min)
        limiter.cleanup();
        assert_eq!(limiter.tracked_ips_count(), 3);
    }

    #[test]
    fn test_rate_limiter_max_tokens_cap() {
        let limiter = RateLimiter::new(5, 100); // Very high refill rate

        // Consume some tokens
        assert!(limiter.check("192.168.1.1"));
        assert!(limiter.check("192.168.1.1"));

        // Wait for refill
        std::thread::sleep(Duration::from_millis(100));

        // Should be capped at max_tokens (5), not exceed it
        for i in 0..5 {
            assert!(limiter.check("192.168.1.1"), "Request {} should succeed", i + 1);
        }

        // 6th should fail (only 5 max tokens)
        assert!(!limiter.check("192.168.1.1"));
    }

    #[test]
    fn test_concurrent_requests_from_multiple_ips() {
        let limiter = RateLimiter::new(5, 1);

        // Simulate requests from 10 different IPs
        for i in 1..=10 {
            let ip = format!("192.168.1.{i}");
            assert!(limiter.check(&ip), "IP {ip} should be allowed");
        }

        // All 10 IPs should be tracked
        assert_eq!(limiter.tracked_ips_count(), 10);
    }

    #[test]
    fn test_rate_limit_per_ip_strict_isolation() {
        let limiter = RateLimiter::new(2, 1);

        // Exhaust IP1's tokens
        assert!(limiter.check("192.168.1.1"));
        assert!(limiter.check("192.168.1.1"));
        assert!(!limiter.check("192.168.1.1"), "IP1 should be rate limited");

        // IP2 should still have full tokens
        assert!(limiter.check("192.168.1.2"));
        assert!(limiter.check("192.168.1.2"));
        assert!(!limiter.check("192.168.1.2"), "IP2 should be rate limited");

        // IP3 should also have full tokens
        assert!(limiter.check("192.168.1.3"));
    }
}
