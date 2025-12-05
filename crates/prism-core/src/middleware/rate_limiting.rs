use dashmap::DashMap;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Rate limiter using token bucket algorithm.
///
/// **Security**: Limits maximum tracked clients to prevent OOM from spoofed IPs.
pub struct RateLimiter {
    buckets: Arc<DashMap<String, TokenBucket>>,
    max_tokens: u32,
    refill_rate: u32,
    cleanup_interval: Duration,
    bucket_ttl: Duration,
    max_buckets: usize,
}

#[derive(Debug, Clone)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    last_access: Instant,
}

impl RateLimiter {
    const DEFAULT_MAX_BUCKETS: usize = 100_000;

    #[must_use]
    pub fn new(max_tokens: u32, refill_rate: u32) -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            max_tokens,
            refill_rate,
            cleanup_interval: Duration::from_secs(300),
            bucket_ttl: Duration::from_secs(300),
            max_buckets: Self::DEFAULT_MAX_BUCKETS,
        }
    }

    #[must_use]
    pub fn with_max_buckets(max_tokens: u32, refill_rate: u32, max_buckets: usize) -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            max_tokens,
            refill_rate,
            cleanup_interval: Duration::from_secs(300),
            bucket_ttl: Duration::from_secs(300),
            max_buckets,
        }
    }

    pub fn start_cleanup_task(&self) {
        let cleanup_interval = self.cleanup_interval;
        let bucket_ttl = self.bucket_ttl;
        let buckets = self.buckets.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);

            loop {
                interval.tick().await;

                let now = Instant::now();
                buckets.retain(|_, bucket| now.duration_since(bucket.last_access) < bucket_ttl);
            }
        });
    }

    /// Checks rate limit for client. Rejects new clients at capacity to prevent OOM.
    #[must_use]
    pub fn check_rate_limit(&self, key: &str) -> bool {
        let now = Instant::now();

        if let Some(mut bucket) = self.buckets.get_mut(key) {
            return Self::process_existing_bucket(
                &mut bucket,
                now,
                self.max_tokens,
                self.refill_rate,
            );
        }

        if self.buckets.len() >= self.max_buckets {
            return false;
        }

        let mut bucket = self.buckets.entry(key.to_string()).or_insert_with(|| TokenBucket {
            tokens: f64::from(self.max_tokens),
            last_refill: now,
            last_access: now,
        });

        Self::process_existing_bucket(&mut bucket, now, self.max_tokens, self.refill_rate)
    }

    fn process_existing_bucket(
        bucket: &mut TokenBucket,
        now: Instant,
        max_tokens: u32,
        refill_rate: u32,
    ) -> bool {
        bucket.last_access = now;

        let elapsed = now.duration_since(bucket.last_refill);
        let tokens_to_add =
            (elapsed.as_secs_f64() * f64::from(refill_rate)).min(f64::from(max_tokens));

        if tokens_to_add > 0.0 {
            bucket.tokens = (bucket.tokens + tokens_to_add).min(f64::from(max_tokens));
            bucket.last_refill = now;
        }

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn cleanup_old_buckets(&self) -> usize {
        let now = Instant::now();
        let before_count = self.buckets.len();

        self.buckets
            .retain(|_, bucket| now.duration_since(bucket.last_access) < self.bucket_ttl);

        before_count - self.buckets.len()
    }

    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn get_bucket_info(&self, key: &str) -> Option<(f64, Instant)> {
        self.buckets.get(key).map(|bucket| (bucket.tokens, bucket.last_access))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_rate_limiter_basic() {
        let limiter = RateLimiter::new(2, 1);
        let client_key = "test_client";

        assert!(limiter.check_rate_limit(client_key));
        assert!(limiter.check_rate_limit(client_key));

        assert!(!limiter.check_rate_limit(client_key));
    }

    #[tokio::test]
    async fn test_rate_limiter_refill() {
        // Token bucket: 1 max token, refill rate of 2 tokens/second
        // After consuming 1 token, we need >= 0.5s for 1 token to refill
        // Use 750ms to add buffer for timing jitter in CI
        let limiter = RateLimiter::new(1, 2);
        let client_key = "test_client";

        assert!(limiter.check_rate_limit(client_key));
        assert!(!limiter.check_rate_limit(client_key));

        // 750ms sleep gives us 1.5 tokens at 2 tokens/sec refill rate
        sleep(Duration::from_millis(750)).await;

        assert!(limiter.check_rate_limit(client_key));
    }

    #[tokio::test]
    async fn test_rate_limiter_cleanup() {
        let limiter = RateLimiter::new(5, 1);

        let _ = limiter.check_rate_limit("client1");
        let _ = limiter.check_rate_limit("client2");
        let _ = limiter.check_rate_limit("client3");

        assert_eq!(limiter.bucket_count(), 3);

        let removed = limiter.cleanup_old_buckets();
        assert_eq!(removed, 0);
        assert_eq!(limiter.bucket_count(), 3);
    }

    #[tokio::test]
    async fn test_rate_limiter_concurrent_access() {
        let limiter = Arc::new(RateLimiter::new(10, 5));
        let client_key = "test_client";

        let mut handles = vec![];
        for _ in 0..5 {
            let limiter_clone = limiter.clone();
            let handle = tokio::spawn(async move {
                let mut successful = 0;
                for _ in 0..3 {
                    if limiter_clone.check_rate_limit(client_key) {
                        successful += 1;
                    }
                }
                successful
            });
            handles.push(handle);
        }

        let mut total_successful = 0;
        for handle in handles {
            total_successful += handle.await.unwrap();
        }

        assert!(total_successful <= 10);
    }

    #[tokio::test]
    async fn test_rate_limiter_multiple_clients() {
        let limiter = RateLimiter::new(2, 1);

        assert!(limiter.check_rate_limit("client1"));
        assert!(limiter.check_rate_limit("client2"));
        assert!(limiter.check_rate_limit("client1"));
        assert!(limiter.check_rate_limit("client2"));

        assert!(!limiter.check_rate_limit("client1"));
        assert!(!limiter.check_rate_limit("client2"));
    }

    #[tokio::test]
    async fn test_rate_limiter_zero_tokens() {
        let limiter = RateLimiter::new(0, 1);
        let client_key = "test_client";

        assert!(!limiter.check_rate_limit(client_key));
        assert!(!limiter.check_rate_limit(client_key));
    }

    #[tokio::test]
    async fn test_rate_limiter_high_refill_rate() {
        let limiter = RateLimiter::new(1, 1000);
        let client_key = "test_client";

        for _ in 0..10 {
            assert!(limiter.check_rate_limit(client_key));
            sleep(Duration::from_millis(1)).await;
        }
    }

    #[tokio::test]
    async fn test_rate_limiter_edge_cases() {
        let limiter = RateLimiter::new(1, 1);

        assert!(limiter.check_rate_limit(""));

        let long_key = "a".repeat(1000);
        assert!(limiter.check_rate_limit(&long_key));

        assert!(limiter.check_rate_limit("测试"));

        assert!(limiter.check_rate_limit("test_client"));
    }
}
