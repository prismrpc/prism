use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use arc_swap::ArcSwap;
use futures::future::join_all;

use crate::{
    types::UpstreamConfig,
    upstream::{
        circuit_breaker::CircuitBreakerState, endpoint::UpstreamEndpoint, http_client::HttpClient,
    },
};

/// Configuration for load balancer timeouts and behavior.
///
/// Timeouts are configured to handle high concurrency scenarios (85+ parallel tests)
/// while maintaining responsiveness under normal load.
#[derive(Debug, Clone)]
pub struct LoadBalancerConfig {
    /// Standard timeout for acquiring read lock on upstreams list (milliseconds)
    pub lock_timeout_ms: u64,
    /// Reduced timeout used periodically to prevent resource leaks (milliseconds)
    pub aggressive_lock_timeout_ms: u64,
    /// Frequency at which to use aggressive timeout (every N requests)
    pub aggressive_timeout_interval: usize,
    /// Health check timeout under normal load conditions (milliseconds)
    pub health_check_timeout_ms: u64,
    /// Reduced health check timeout under high load (milliseconds)
    pub health_check_high_load_timeout_ms: u64,
    /// Timeout for fallback health checks when trying other upstreams (milliseconds)
    pub fallback_health_check_timeout_ms: u64,
    /// Request count threshold to switch to high-load behavior
    pub high_load_threshold: usize,
}

impl Default for LoadBalancerConfig {
    fn default() -> Self {
        Self {
            lock_timeout_ms: 500,
            aggressive_lock_timeout_ms: 200,
            aggressive_timeout_interval: 50,
            health_check_timeout_ms: 100,
            health_check_high_load_timeout_ms: 50,
            fallback_health_check_timeout_ms: 25,
            high_load_threshold: 500,
        }
    }
}

/// Round-robin load balancer for distributing requests across upstream endpoints.
///
/// Provides multiple selection strategies (round-robin, response-time-aware, weighted)
/// and handles health checking with configurable timeouts. Thread-safe for concurrent access.
///
/// Uses `ArcSwap` for lock-free reads of the upstream list. This eliminates `RwLock`
/// contention on the hot path where upstreams are read for every request. Writes
/// (adding/removing upstreams) are still atomic but happen via swap, not lock.
pub struct LoadBalancer {
    upstreams: ArcSwap<Vec<Arc<UpstreamEndpoint>>>,
    current_index: AtomicUsize,
    config: LoadBalancerConfig,
}

impl Default for LoadBalancer {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancer {
    /// Creates a new `LoadBalancer` with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(LoadBalancerConfig::default())
    }

    /// Creates a new `LoadBalancer` with custom configuration.
    ///
    /// # Arguments
    /// * `config` - Custom `LoadBalancerConfig` for timeout and behavior settings
    #[must_use]
    pub fn with_config(config: LoadBalancerConfig) -> Self {
        Self {
            upstreams: ArcSwap::from_pointee(Vec::new()),
            current_index: AtomicUsize::new(0),
            config,
        }
    }

    /// Adds a new upstream endpoint to the load balancer.
    ///
    /// Uses atomic read-copy-update to add without blocking readers.
    pub fn add_upstream(&self, config: UpstreamConfig, http_client: Arc<HttpClient>) {
        let upstream = Arc::new(UpstreamEndpoint::new(config, http_client));
        self.upstreams.rcu(|current| {
            let mut new_upstreams = (**current).clone();
            new_upstreams.push(upstream.clone());
            new_upstreams
        });
    }

    /// Returns the next healthy upstream using round-robin selection with timeouts.
    ///
    /// Uses lock-free reads via `ArcSwap` for the upstream list. Health checks still
    /// use timeouts based on load. Returns `None` if no healthy upstream is found.
    pub async fn get_next_healthy(&self) -> Option<Arc<UpstreamEndpoint>> {
        let current_index = self.current_index.load(Ordering::Relaxed);
        let upstreams = self.upstreams.load();

        if upstreams.is_empty() {
            return None;
        }

        let upstream_count = upstreams.len();
        let start_index = self.current_index.fetch_add(1, Ordering::Relaxed) % upstream_count;

        tracing::trace!(
            current_index = start_index,
            total_upstreams = upstream_count,
            "selecting upstream"
        );

        let health_timeout = if current_index > self.config.high_load_threshold {
            std::time::Duration::from_millis(self.config.health_check_high_load_timeout_ms)
        } else {
            std::time::Duration::from_millis(self.config.health_check_timeout_ms)
        };

        if let Ok(is_healthy) =
            tokio::time::timeout(health_timeout, upstreams[start_index].is_healthy()).await
        {
            if is_healthy {
                return Some(upstreams[start_index].clone());
            }
        }

        let fallback_timeout =
            std::time::Duration::from_millis(self.config.fallback_health_check_timeout_ms);
        for offset in 1..upstream_count {
            let index = (start_index + offset) % upstream_count;
            let upstream = &upstreams[index];

            if let Ok(is_healthy) =
                tokio::time::timeout(fallback_timeout, upstream.is_healthy()).await
            {
                if is_healthy {
                    return Some(upstream.clone());
                }
            }
        }

        None
    }

    /// Returns the next healthy upstream using response-time-aware selection.
    ///
    /// Selects the healthy upstream with the lowest average response time by checking
    /// all upstreams concurrently. Falls back to any healthy upstream if response time
    /// data is unavailable.
    pub async fn get_next_healthy_by_response_time(&self) -> Option<Arc<UpstreamEndpoint>> {
        let upstreams = self.upstreams.load();

        if upstreams.is_empty() {
            return None;
        }

        let mut best_upstream: Option<Arc<UpstreamEndpoint>> = None;
        let mut best_response_time = u64::MAX;

        let health_timeout = std::time::Duration::from_millis(self.config.health_check_timeout_ms);

        let futures: Vec<_> = upstreams
            .iter()
            .map(|upstream| {
                let upstream = upstream.clone();
                async move {
                    let is_healthy = tokio::time::timeout(health_timeout, upstream.is_healthy())
                        .await
                        .unwrap_or(false);

                    if is_healthy {
                        let health =
                            match tokio::time::timeout(health_timeout, upstream.get_health()).await
                            {
                                Ok(health) => Some(health),
                                Err(_timeout) => {
                                    tracing::debug!(
                                        upstream = %upstream.config().name,
                                        timeout_ms = health_timeout.as_millis(),
                                        "health check timed out during response-time selection"
                                    );
                                    None
                                }
                            };

                        Some((upstream, health))
                    } else {
                        None
                    }
                }
            })
            .collect();

        let results = join_all(futures).await;

        for result in results {
            if let Some((upstream, Some(health))) = result {
                if let Some(response_time) = health.response_time_ms {
                    if response_time < best_response_time {
                        best_response_time = response_time;
                        best_upstream = Some(upstream);
                    }
                } else if best_upstream.is_none() {
                    best_upstream = Some(upstream);
                }
            }
        }

        best_upstream
    }

    /// Returns the next healthy upstream using weighted response-time selection.
    ///
    /// Calculates a score for each upstream as `response_time / weight` and selects
    /// the one with the lowest score. This allows favoring faster upstreams while
    /// still respecting configured weights. Uses a default response time of 1000ms
    /// for upstreams without response time data.
    #[allow(clippy::cast_precision_loss)]
    pub async fn get_next_healthy_weighted(&self) -> Option<Arc<UpstreamEndpoint>> {
        let upstreams = self.upstreams.load();
        if upstreams.is_empty() {
            return None;
        }

        let mut best_upstream: Option<Arc<UpstreamEndpoint>> = None;
        let mut best_score = f64::MAX;

        for upstream in upstreams.iter() {
            if upstream.is_healthy().await {
                let health = upstream.get_health().await;
                let config = upstream.config();

                let score = if let Some(response_time) = health.response_time_ms {
                    response_time as f64 / f64::from(config.weight)
                } else {
                    1000.0 / f64::from(config.weight)
                };

                if score < best_score {
                    best_score = score;
                    best_upstream = Some(upstream.clone());
                }
            }
        }

        best_upstream
    }

    /// Selects an upstream from scored candidates using weighted random selection.
    ///
    /// Returns the selected upstream, or the first candidate if selection fails.
    #[allow(clippy::cast_precision_loss)]
    fn select_by_weighted_score(
        &self,
        valid_candidates: &[(Arc<UpstreamEndpoint>, f64)],
        log_context: &str,
    ) -> Option<Arc<UpstreamEndpoint>> {
        if valid_candidates.is_empty() {
            return None;
        }

        if valid_candidates.len() == 1 {
            return Some(valid_candidates[0].0.clone());
        }

        let total_score: f64 = valid_candidates.iter().map(|(_, s)| s).sum();
        if total_score <= 0.0 {
            return Some(valid_candidates[0].0.clone());
        }

        // Deterministic pseudo-random selection for reproducibility in tests
        let rand_val: f64 = {
            let idx = self.current_index.fetch_add(1, Ordering::Relaxed);
            ((idx * 31337) % 1000) as f64 / 1000.0
        };

        let mut cumulative = 0.0;
        for (upstream, score) in valid_candidates {
            cumulative += score / total_score;
            if rand_val <= cumulative {
                tracing::trace!(
                    upstream = %upstream.config().name,
                    score = %score,
                    "selected upstream by score ({})", log_context
                );
                return Some(upstream.clone());
            }
        }

        Some(valid_candidates[0].0.clone())
    }

    /// Returns the next healthy upstream using multi-factor score-based selection.
    ///
    /// Uses the scoring engine to select from the top-N scored upstreams using
    /// weighted random selection. Falls back to response-time-based selection
    /// if scoring is disabled or insufficient data is available.
    #[allow(clippy::cast_precision_loss)]
    pub async fn get_next_by_score(
        &self,
        scoring_engine: &super::scoring::ScoringEngine,
        top_n: usize,
    ) -> Option<Arc<UpstreamEndpoint>> {
        if !scoring_engine.is_enabled() {
            return self.get_next_healthy_by_response_time().await;
        }

        let ranked = scoring_engine.get_ranked_upstreams();
        if ranked.is_empty() {
            tracing::debug!(
                "no scored upstreams available, falling back to response-time selection"
            );
            return self.get_next_healthy_by_response_time().await;
        }

        let candidates: Vec<_> = ranked.into_iter().take(top_n).collect();
        let upstreams = self.upstreams.load();
        let mut valid_candidates: Vec<(Arc<UpstreamEndpoint>, f64)> = Vec::new();

        for (name, score) in &candidates {
            if let Some(upstream) =
                upstreams.iter().find(|u| u.config().name.as_ref() == name.as_str())
            {
                if upstream.is_healthy().await {
                    valid_candidates.push((upstream.clone(), *score));
                }
            }
        }

        if valid_candidates.is_empty() {
            tracing::debug!("no valid scored candidates, falling back to response-time selection");
            return self.get_next_healthy_by_response_time().await;
        }

        self.select_by_weighted_score(&valid_candidates, "from all upstreams")
    }

    /// Returns the next upstream from a pre-filtered list using multi-factor score-based selection.
    ///
    /// Similar to `get_next_by_score` but operates on an already-filtered list of upstreams
    /// (e.g., upstreams filtered by block availability). Falls back to the first upstream
    /// in the provided list if scoring is disabled or insufficient data is available.
    #[allow(clippy::cast_precision_loss)]
    pub async fn get_next_by_score_from(
        &self,
        scoring_engine: &super::scoring::ScoringEngine,
        top_n: usize,
        upstreams: &[Arc<UpstreamEndpoint>],
    ) -> Option<Arc<UpstreamEndpoint>> {
        if upstreams.is_empty() {
            return None;
        }

        if !scoring_engine.is_enabled() {
            for upstream in upstreams {
                if upstream.is_healthy().await {
                    return Some(upstream.clone());
                }
            }
            return None;
        }

        let ranked = scoring_engine.get_ranked_upstreams();
        if ranked.is_empty() {
            tracing::debug!(
                "no scored upstreams available, falling back to first from provided list"
            );
            return upstreams.first().cloned();
        }

        let mut valid_candidates: Vec<(Arc<UpstreamEndpoint>, f64)> = Vec::with_capacity(top_n);

        for (name, score) in &ranked {
            if valid_candidates.len() >= top_n {
                break;
            }
            if let Some(upstream) =
                upstreams.iter().find(|u| u.config().name.as_ref() == name.as_str())
            {
                if upstream.is_healthy().await {
                    valid_candidates.push((upstream.clone(), *score));
                }
            }
        }

        if valid_candidates.is_empty() {
            tracing::debug!("no valid scored candidates in provided list, using first available");
            return upstreams.first().cloned();
        }

        self.select_by_weighted_score(&valid_candidates, "from filtered list")
    }

    /// Returns an upstream by name.
    pub fn get_upstream_by_name(&self, name: &str) -> Option<Arc<UpstreamEndpoint>> {
        let upstreams = self.upstreams.load();
        upstreams.iter().find(|u| u.config().name.as_ref() == name).cloned()
    }

    /// Returns all registered upstreams, both healthy and unhealthy.
    pub fn get_all_upstreams(&self) -> Arc<Vec<Arc<UpstreamEndpoint>>> {
        Arc::clone(&self.upstreams.load())
    }

    /// Returns only the currently healthy upstreams.
    pub async fn get_healthy_upstreams(&self) -> Vec<Arc<UpstreamEndpoint>> {
        let upstreams = self.upstreams.load();
        let mut healthy = Vec::new();

        for upstream in upstreams.iter() {
            if upstream.is_healthy().await {
                healthy.push(upstream.clone());
            }
        }

        healthy
    }

    /// Returns healthy upstreams that can serve the requested block number.
    ///
    /// Filters out upstreams that are behind the requested block. This prevents
    /// routing requests to upstreams that would return "block not found" errors.
    pub async fn get_healthy_upstreams_for_block(
        &self,
        block_number: u64,
    ) -> Vec<Arc<UpstreamEndpoint>> {
        let upstreams = self.upstreams.load();

        let futures: Vec<_> = upstreams
            .iter()
            .map(|upstream| {
                let upstream = upstream.clone();
                async move {
                    if upstream.is_healthy().await && upstream.can_serve_block(block_number).await {
                        Some(upstream)
                    } else {
                        None
                    }
                }
            })
            .collect();

        let results = join_all(futures).await;
        let capable: Vec<_> = results.into_iter().flatten().collect();

        // Fall back to all healthy if none confirmed capable (block info may be unavailable)
        if capable.is_empty() {
            tracing::debug!(
                block = block_number,
                "no upstreams confirmed to serve block, falling back to all healthy"
            );

            let health_futures: Vec<_> = upstreams
                .iter()
                .map(|upstream| {
                    let upstream = upstream.clone();
                    async move {
                        if upstream.is_healthy().await {
                            Some(upstream)
                        } else {
                            None
                        }
                    }
                })
                .collect();

            return join_all(health_futures).await.into_iter().flatten().collect();
        }

        capable
    }

    /// Removes an upstream endpoint by name.
    ///
    /// Uses atomic read-copy-update to remove without blocking readers.
    pub fn remove_upstream(&self, name: &str) {
        self.upstreams.rcu(|current| {
            let new_upstreams: Vec<Arc<UpstreamEndpoint>> = current
                .iter()
                .filter(|upstream| upstream.config().name.as_ref() != name)
                .cloned()
                .collect();
            new_upstreams
        });
    }

    /// Updates an upstream's configuration atomically.
    ///
    /// Uses atomic read-copy-update to replace the upstream without blocking readers.
    /// The upstream is replaced in place, preserving its position in the list.
    /// Returns true if the upstream was found and updated, false otherwise.
    pub fn update_upstream(
        &self,
        name: &str,
        new_config: UpstreamConfig,
        http_client: Arc<HttpClient>,
    ) -> bool {
        // First check if the upstream exists
        let exists = {
            let upstreams = self.upstreams.load();
            upstreams.iter().any(|u| u.config().name.as_ref() == name)
        };

        if !exists {
            return false;
        }

        // Create the new upstream once, outside the rcu closure
        let new_upstream = Arc::new(UpstreamEndpoint::new(new_config, http_client));
        let name_owned = name.to_string();

        self.upstreams.rcu(move |current| {
            let mut new_upstreams = Vec::with_capacity(current.len());
            for upstream in current.iter() {
                if upstream.config().name.as_ref() == name_owned.as_str() {
                    // Replace with new upstream using updated config
                    new_upstreams.push(Arc::clone(&new_upstream));
                } else {
                    new_upstreams.push(upstream.clone());
                }
            }
            new_upstreams
        });
        true
    }

    /// Returns load balancer statistics including counts and average response times.
    pub async fn get_stats(&self) -> LoadBalancerStats {
        let upstreams = self.upstreams.load();
        let total = upstreams.len();
        let mut healthy = 0;
        let mut total_response_time = 0u64;
        let mut response_count = 0;

        for upstream in upstreams.iter() {
            if upstream.is_healthy().await {
                healthy += 1;
            }

            let health = upstream.get_health().await;
            if let Some(response_time) = health.response_time_ms {
                total_response_time += response_time;
                response_count += 1;
            }
        }

        let avg_response_time = if response_count > 0 {
            total_response_time / response_count
        } else {
            0
        };

        LoadBalancerStats {
            total_upstreams: total,
            healthy_upstreams: healthy,
            average_response_time_ms: avg_response_time,
        }
    }

    /// Returns the circuit breaker status for all upstreams.
    pub async fn get_circuit_breaker_status(&self) -> Vec<(Arc<str>, CircuitBreakerState, u32)> {
        let upstreams = self.get_all_upstreams();
        let mut status = Vec::new();

        for upstream in upstreams.iter() {
            let name = Arc::clone(&upstream.config().name);
            let state = upstream.get_circuit_breaker_state().await;
            let failure_count = upstream.get_circuit_breaker_failure_count().await;
            status.push((name, state, failure_count));
        }

        status
    }

    /// Resets the circuit breaker for a specific upstream.
    ///
    /// Forces the circuit breaker to closed state, clearing all failure counts.
    /// Returns `true` if the upstream was found and reset, `false` otherwise.
    pub async fn reset_circuit_breaker(&self, upstream_name: &str) -> bool {
        let upstreams = self.get_all_upstreams();

        for upstream in upstreams.iter() {
            if upstream.config().name.as_ref() == upstream_name {
                upstream.circuit_breaker().on_success().await;
                tracing::info!(name = %upstream_name, "circuit breaker reset");
                return true;
            }
        }

        tracing::warn!(name = %upstream_name, "upstream not found for circuit breaker reset");
        false
    }
}

/// Statistics about load balancer performance and upstream health.
#[derive(Debug, Clone)]
pub struct LoadBalancerStats {
    /// Total number of registered upstreams
    pub total_upstreams: usize,
    /// Number of currently healthy upstreams
    pub healthy_upstreams: usize,
    /// Average response time across all upstreams with response time data (milliseconds)
    pub average_response_time_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UpstreamConfig;

    /// Helper to create a test `UpstreamConfig` with minimal settings.
    fn test_config(name: &str, url: &str) -> UpstreamConfig {
        UpstreamConfig {
            url: url.to_string(),
            name: Arc::from(name),
            weight: 100,
            timeout_seconds: 30,
            supports_websocket: false,
            ws_url: None,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_seconds: 60,
            chain_id: 1,
        }
    }

    /// Helper to create an `HttpClient` for testing.
    fn test_http_client() -> Arc<HttpClient> {
        Arc::new(HttpClient::new().unwrap())
    }

    #[test]
    fn test_load_balancer_config_default() {
        let config = LoadBalancerConfig::default();

        assert_eq!(config.lock_timeout_ms, 500);
        assert_eq!(config.aggressive_lock_timeout_ms, 200);
        assert_eq!(config.aggressive_timeout_interval, 50);
        assert_eq!(config.health_check_timeout_ms, 100);
        assert_eq!(config.health_check_high_load_timeout_ms, 50);
        assert_eq!(config.fallback_health_check_timeout_ms, 25);
        assert_eq!(config.high_load_threshold, 500);
    }

    #[test]
    fn test_load_balancer_config_custom() {
        let config = LoadBalancerConfig {
            lock_timeout_ms: 1000,
            aggressive_lock_timeout_ms: 500,
            aggressive_timeout_interval: 100,
            health_check_timeout_ms: 200,
            health_check_high_load_timeout_ms: 100,
            fallback_health_check_timeout_ms: 50,
            high_load_threshold: 1000,
        };

        assert_eq!(config.lock_timeout_ms, 1000);
        assert_eq!(config.high_load_threshold, 1000);
    }

    #[test]
    fn test_load_balancer_new() {
        let balancer = LoadBalancer::new();
        let upstreams = balancer.get_all_upstreams();
        assert!(upstreams.is_empty(), "New balancer should have no upstreams");
    }

    #[test]
    fn test_load_balancer_default() {
        let balancer = LoadBalancer::default();
        let upstreams = balancer.get_all_upstreams();
        assert!(upstreams.is_empty(), "Default balancer should have no upstreams");
    }

    #[test]
    fn test_load_balancer_with_custom_config() {
        let config =
            LoadBalancerConfig { health_check_timeout_ms: 500, ..LoadBalancerConfig::default() };

        let balancer = LoadBalancer::with_config(config);
        let upstreams = balancer.get_all_upstreams();
        assert!(upstreams.is_empty());
    }

    #[tokio::test]
    async fn test_add_single_upstream() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example.com"), test_http_client());

        let upstreams = balancer.get_all_upstreams();
        assert_eq!(upstreams.len(), 1);
        assert_eq!(upstreams[0].config().name.as_ref(), "upstream1");
    }

    #[tokio::test]
    async fn test_add_multiple_upstreams() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example1.com"), test_http_client());
        balancer.add_upstream(test_config("upstream2", "https://example2.com"), test_http_client());
        balancer.add_upstream(test_config("upstream3", "https://example3.com"), test_http_client());

        let upstreams = balancer.get_all_upstreams();
        assert_eq!(upstreams.len(), 3);
    }

    #[tokio::test]
    async fn test_add_upstream_preserves_order() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("first", "https://first.com"), test_http_client());
        balancer.add_upstream(test_config("second", "https://second.com"), test_http_client());
        balancer.add_upstream(test_config("third", "https://third.com"), test_http_client());

        let upstreams = balancer.get_all_upstreams();
        assert_eq!(upstreams[0].config().name.as_ref(), "first");
        assert_eq!(upstreams[1].config().name.as_ref(), "second");
        assert_eq!(upstreams[2].config().name.as_ref(), "third");
    }

    #[tokio::test]
    async fn test_get_upstream_by_name_found() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("target", "https://target.com"), test_http_client());
        balancer.add_upstream(test_config("other", "https://other.com"), test_http_client());

        let found = balancer.get_upstream_by_name("target");
        assert!(found.is_some());
        assert_eq!(found.unwrap().config().name.as_ref(), "target");
    }

    #[tokio::test]
    async fn test_get_upstream_by_name_not_found() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example.com"), test_http_client());

        let found = balancer.get_upstream_by_name("nonexistent");
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_get_upstream_by_name_empty_balancer() {
        let balancer = LoadBalancer::new();

        let found = balancer.get_upstream_by_name("any");
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_remove_upstream_by_name() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("keep", "https://keep.com"), test_http_client());
        balancer.add_upstream(test_config("remove", "https://remove.com"), test_http_client());

        assert_eq!(balancer.get_all_upstreams().len(), 2);

        balancer.remove_upstream("remove");

        let upstreams = balancer.get_all_upstreams();
        assert_eq!(upstreams.len(), 1);
        assert_eq!(upstreams[0].config().name.as_ref(), "keep");
    }

    #[tokio::test]
    async fn test_remove_nonexistent_upstream() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example.com"), test_http_client());

        balancer.remove_upstream("nonexistent");

        assert_eq!(balancer.get_all_upstreams().len(), 1);
    }

    #[tokio::test]
    async fn test_remove_all_upstreams() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("first", "https://first.com"), test_http_client());
        balancer.add_upstream(test_config("second", "https://second.com"), test_http_client());

        balancer.remove_upstream("first");
        balancer.remove_upstream("second");

        assert!(balancer.get_all_upstreams().is_empty());
    }

    #[tokio::test]
    async fn test_get_next_healthy_empty_balancer() {
        let balancer = LoadBalancer::new();

        let result = balancer.get_next_healthy().await;
        assert!(result.is_none(), "Empty balancer should return None");
    }

    #[tokio::test]
    async fn test_get_next_healthy_all_healthy() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example1.com"), test_http_client());
        balancer.add_upstream(test_config("upstream2", "https://example2.com"), test_http_client());

        let result = balancer.get_next_healthy().await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_get_next_healthy_round_robin() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example1.com"), test_http_client());
        balancer.add_upstream(test_config("upstream2", "https://example2.com"), test_http_client());

        let first = balancer.get_next_healthy().await.expect("Should find upstream");
        let second = balancer.get_next_healthy().await.expect("Should find upstream");

        assert!(
            first.config().name.as_ref() == "upstream1" ||
                first.config().name.as_ref() == "upstream2"
        );
        assert!(
            second.config().name.as_ref() == "upstream1" ||
                second.config().name.as_ref() == "upstream2"
        );
    }

    #[tokio::test]
    async fn test_get_next_healthy_skips_unhealthy() {
        let balancer = LoadBalancer::new();

        let config_healthy = UpstreamConfig {
            circuit_breaker_threshold: 5,
            ..test_config("healthy", "https://healthy.com")
        };
        let config_failing = UpstreamConfig {
            circuit_breaker_threshold: 2,
            ..test_config("failing", "https://failing.com")
        };

        balancer.add_upstream(config_healthy, test_http_client());
        balancer.add_upstream(config_failing, test_http_client());

        let upstreams = balancer.get_all_upstreams();
        let failing = upstreams.iter().find(|u| u.config().name.as_ref() == "failing").unwrap();
        failing.circuit_breaker().on_failure().await;
        failing.circuit_breaker().on_failure().await;

        for _ in 0..5 {
            let result = balancer.get_next_healthy().await;
            assert!(result.is_some());
            assert_eq!(result.unwrap().config().name.as_ref(), "healthy");
        }
    }

    #[tokio::test]
    async fn test_get_healthy_upstreams_all_healthy() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example1.com"), test_http_client());
        balancer.add_upstream(test_config("upstream2", "https://example2.com"), test_http_client());

        let healthy = balancer.get_healthy_upstreams().await;
        assert_eq!(healthy.len(), 2);
    }

    #[tokio::test]
    async fn test_get_healthy_upstreams_some_unhealthy() {
        let balancer = LoadBalancer::new();

        let healthy_config = UpstreamConfig {
            circuit_breaker_threshold: 5,
            ..test_config("healthy", "https://healthy.com")
        };
        let failing_config = UpstreamConfig {
            circuit_breaker_threshold: 2,
            ..test_config("failing", "https://failing.com")
        };

        balancer.add_upstream(healthy_config, test_http_client());
        balancer.add_upstream(failing_config, test_http_client());

        let upstreams = balancer.get_all_upstreams();
        let failing = upstreams.iter().find(|u| u.config().name.as_ref() == "failing").unwrap();
        failing.circuit_breaker().on_failure().await;
        failing.circuit_breaker().on_failure().await;

        let healthy = balancer.get_healthy_upstreams().await;
        assert_eq!(healthy.len(), 1);
        assert_eq!(healthy[0].config().name.as_ref(), "healthy");
    }

    #[tokio::test]
    async fn test_get_healthy_upstreams_empty() {
        let balancer = LoadBalancer::new();

        let healthy = balancer.get_healthy_upstreams().await;
        assert!(healthy.is_empty());
    }

    #[tokio::test]
    async fn test_get_stats_empty() {
        let balancer = LoadBalancer::new();

        let stats = balancer.get_stats().await;

        assert_eq!(stats.total_upstreams, 0);
        assert_eq!(stats.healthy_upstreams, 0);
        assert_eq!(stats.average_response_time_ms, 0);
    }

    #[tokio::test]
    async fn test_get_stats_with_upstreams() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example1.com"), test_http_client());
        balancer.add_upstream(test_config("upstream2", "https://example2.com"), test_http_client());

        let stats = balancer.get_stats().await;

        assert_eq!(stats.total_upstreams, 2);
        assert_eq!(stats.healthy_upstreams, 2);
        assert_eq!(stats.average_response_time_ms, 0);
    }

    #[tokio::test]
    async fn test_load_balancer() {
        let balancer = LoadBalancer::new();

        let config1 = UpstreamConfig {
            url: "https://example1.com".to_string(),
            name: Arc::from("upstream1"),
            weight: 1,
            timeout_seconds: 30,
            supports_websocket: false,
            ws_url: None,
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 1,
            chain_id: 0,
        };

        let config2 = UpstreamConfig {
            url: "https://example2.com".to_string(),
            name: Arc::from("upstream2"),
            weight: 1,
            timeout_seconds: 30,
            supports_websocket: false,
            ws_url: None,
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 1,
            chain_id: 0,
        };

        balancer.add_upstream(config1, Arc::new(HttpClient::new().unwrap()));
        balancer.add_upstream(config2, Arc::new(HttpClient::new().unwrap()));

        let stats = balancer.get_stats().await;
        assert_eq!(stats.total_upstreams, 2);
    }

    #[tokio::test]
    async fn test_load_balancer_with_circuit_breaker() {
        let balancer = LoadBalancer::new();

        let healthy_config = UpstreamConfig {
            url: "http://localhost:8545".to_string(),
            name: Arc::from("healthy"),
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_seconds: 60,
            ..UpstreamConfig::default()
        };

        let failing_config = UpstreamConfig {
            url: "http://localhost:9999".to_string(),
            name: Arc::from("failing"),
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 60,
            ..UpstreamConfig::default()
        };

        balancer.add_upstream(healthy_config, Arc::new(HttpClient::new().unwrap()));
        balancer.add_upstream(failing_config, Arc::new(HttpClient::new().unwrap()));

        let healthy_upstreams = balancer.get_healthy_upstreams().await;
        assert_eq!(healthy_upstreams.len(), 2);

        let upstreams = balancer.get_all_upstreams();
        let failing_upstream =
            upstreams.iter().find(|u| u.config().name.as_ref() == "failing").unwrap();

        failing_upstream.circuit_breaker().on_failure().await;
        failing_upstream.circuit_breaker().on_failure().await;
        failing_upstream.invalidate_health_cache(); // Cache invalidation needed after direct circuit breaker manipulation

        let healthy_upstreams = balancer.get_healthy_upstreams().await;
        assert_eq!(healthy_upstreams.len(), 1);
        assert_eq!(healthy_upstreams[0].config().name.as_ref(), "healthy");
    }

    #[tokio::test]
    async fn test_get_circuit_breaker_status_all_closed() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example1.com"), test_http_client());
        balancer.add_upstream(test_config("upstream2", "https://example2.com"), test_http_client());

        let status = balancer.get_circuit_breaker_status().await;

        assert_eq!(status.len(), 2);
        for (_, state, failure_count) in &status {
            assert_eq!(*state, CircuitBreakerState::Closed);
            assert_eq!(*failure_count, 0);
        }
    }

    #[tokio::test]
    async fn test_get_circuit_breaker_status_one_open() {
        let balancer = LoadBalancer::new();

        let config = UpstreamConfig {
            circuit_breaker_threshold: 2,
            ..test_config("failing", "https://failing.com")
        };

        balancer.add_upstream(config, test_http_client());
        balancer.add_upstream(test_config("healthy", "https://healthy.com"), test_http_client());

        let upstreams = balancer.get_all_upstreams();
        let failing = upstreams.iter().find(|u| u.config().name.as_ref() == "failing").unwrap();
        failing.circuit_breaker().on_failure().await;
        failing.circuit_breaker().on_failure().await;

        let status = balancer.get_circuit_breaker_status().await;

        let failing_status = status.iter().find(|(name, _, _)| name.as_ref() == "failing").unwrap();
        assert_eq!(failing_status.1, CircuitBreakerState::Open);
        assert_eq!(failing_status.2, 2);

        let healthy_status = status.iter().find(|(name, _, _)| name.as_ref() == "healthy").unwrap();
        assert_eq!(healthy_status.1, CircuitBreakerState::Closed);
    }

    #[tokio::test]
    async fn test_reset_circuit_breaker_success() {
        let balancer = LoadBalancer::new();

        let config = UpstreamConfig {
            circuit_breaker_threshold: 2,
            ..test_config("upstream1", "https://example.com")
        };

        balancer.add_upstream(config, test_http_client());

        let upstreams = balancer.get_all_upstreams();
        upstreams[0].circuit_breaker().on_failure().await;
        upstreams[0].circuit_breaker().on_failure().await;

        let status = balancer.get_circuit_breaker_status().await;
        assert_eq!(status[0].1, CircuitBreakerState::Open);

        let result = balancer.reset_circuit_breaker("upstream1").await;
        assert!(result, "Reset should succeed");

        let status = balancer.get_circuit_breaker_status().await;
        assert_eq!(status[0].1, CircuitBreakerState::Closed);
    }

    #[tokio::test]
    async fn test_reset_circuit_breaker_not_found() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example.com"), test_http_client());

        let result = balancer.reset_circuit_breaker("nonexistent").await;
        assert!(!result, "Reset should fail for nonexistent upstream");
    }

    #[test]
    fn test_load_balancer_stats_struct() {
        let stats = LoadBalancerStats {
            total_upstreams: 5,
            healthy_upstreams: 3,
            average_response_time_ms: 150,
        };

        assert_eq!(stats.total_upstreams, 5);
        assert_eq!(stats.healthy_upstreams, 3);
        assert_eq!(stats.average_response_time_ms, 150);
    }

    #[test]
    fn test_load_balancer_stats_clone() {
        let stats = LoadBalancerStats {
            total_upstreams: 5,
            healthy_upstreams: 3,
            average_response_time_ms: 150,
        };

        let cloned = stats.clone();
        assert_eq!(cloned.total_upstreams, stats.total_upstreams);
        assert_eq!(cloned.healthy_upstreams, stats.healthy_upstreams);
    }

    #[tokio::test]
    async fn test_get_next_healthy_by_response_time_empty() {
        let balancer = LoadBalancer::new();

        let result = balancer.get_next_healthy_by_response_time().await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_next_healthy_by_response_time_with_upstreams() {
        let balancer = LoadBalancer::new();

        balancer.add_upstream(test_config("upstream1", "https://example1.com"), test_http_client());
        balancer.add_upstream(test_config("upstream2", "https://example2.com"), test_http_client());

        let result = balancer.get_next_healthy_by_response_time().await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_get_next_healthy_weighted_empty() {
        let balancer = LoadBalancer::new();

        let result = balancer.get_next_healthy_weighted().await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_next_healthy_weighted_with_different_weights() {
        let balancer = LoadBalancer::new();

        let config_high_weight =
            UpstreamConfig { weight: 200, ..test_config("high-weight", "https://high.com") };
        let config_low_weight =
            UpstreamConfig { weight: 50, ..test_config("low-weight", "https://low.com") };

        balancer.add_upstream(config_high_weight, test_http_client());
        balancer.add_upstream(config_low_weight, test_http_client());

        let result = balancer.get_next_healthy_weighted().await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_concurrent_add_remove() {
        let balancer = Arc::new(LoadBalancer::new());

        for i in 0..5 {
            balancer.add_upstream(
                test_config(&format!("upstream{i}"), &format!("https://example{i}.com")),
                test_http_client(),
            );
        }

        let balancer_add = Arc::clone(&balancer);
        let add_handle = tokio::spawn(async move {
            for i in 5..10 {
                balancer_add.add_upstream(
                    test_config(&format!("upstream{i}"), &format!("https://example{i}.com")),
                    test_http_client(),
                );
            }
        });

        let balancer_remove = Arc::clone(&balancer);
        let remove_handle = tokio::spawn(async move {
            for i in 0..3 {
                balancer_remove.remove_upstream(&format!("upstream{i}"));
            }
        });

        let _ = tokio::join!(add_handle, remove_handle);

        let upstreams = balancer.get_all_upstreams();
        assert!(!upstreams.is_empty(), "Should have some upstreams");
        assert!(upstreams.len() <= 10, "Should not exceed maximum added");
    }

    #[tokio::test]
    async fn test_concurrent_health_checks() {
        let balancer = Arc::new(LoadBalancer::new());

        for i in 0..5 {
            balancer.add_upstream(
                test_config(&format!("upstream{i}"), &format!("https://example{i}.com")),
                test_http_client(),
            );
        }

        let mut handles = vec![];
        for _ in 0..10 {
            let balancer_clone = Arc::clone(&balancer);
            handles.push(tokio::spawn(async move { balancer_clone.get_next_healthy().await }));
        }

        for handle in handles {
            let result = handle.await.expect("Task should complete");
            assert!(result.is_some());
        }
    }
}
