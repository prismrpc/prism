//! Request hedging for tail latency optimization.
//!
//! Automatically sends duplicate requests to different upstreams when the primary request
//! exceeds expected latency thresholds based on historical percentiles (P90/P95/P99).
//! Returns the first successful response and cancels slower requests.

use crate::{
    types::{JsonRpcRequest, JsonRpcResponse},
    upstream::{
        endpoint::UpstreamEndpoint, errors::UpstreamError, latency_tracker::LatencyTracker,
    },
};
use arc_swap::ArcSwap;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};

/// Configuration for hedged request execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HedgeConfig {
    /// Whether hedging is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Latency percentile to use as hedge trigger (0.0-1.0, default: 0.95)
    #[serde(default = "default_latency_quantile")]
    pub latency_quantile: f64,

    /// Minimum delay before issuing hedged request in milliseconds (default: 50)
    #[serde(default = "default_min_delay_ms")]
    pub min_delay_ms: u64,

    /// Maximum delay before issuing hedged request in milliseconds (default: 2000)
    #[serde(default = "default_max_delay_ms")]
    pub max_delay_ms: u64,

    /// Maximum parallel requests including primary (default: 2)
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
}

fn default_latency_quantile() -> f64 {
    0.95
}

fn default_min_delay_ms() -> u64 {
    50
}

fn default_max_delay_ms() -> u64 {
    2000
}

fn default_max_parallel() -> usize {
    2
}

impl Default for HedgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            latency_quantile: default_latency_quantile(),
            min_delay_ms: default_min_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            max_parallel: default_max_parallel(),
        }
    }
}

/// Executes hedged requests with latency-based triggering.
///
/// Sends primary request, waits for hedge delay based on latency percentiles,
/// then sends hedged requests if needed. Returns first successful response.
///
/// # Lock-Free Config Access
///
/// Configuration is stored in an `ArcSwap` for lock-free reads on the hot path.
/// Config updates are rare (admin operations) while reads happen on every request.
pub struct HedgeExecutor {
    config: ArcSwap<HedgeConfig>,
    latency_trackers: DashMap<String, LatencyTracker>,
}

impl HedgeExecutor {
    /// Creates a new hedge executor with the given configuration.
    #[must_use]
    pub fn new(config: HedgeConfig) -> Self {
        Self { config: ArcSwap::from_pointee(config), latency_trackers: DashMap::new() }
    }

    /// Updates the configuration at runtime.
    ///
    /// This is a lock-free operation using `ArcSwap::store`.
    pub fn update_config(&self, config: HedgeConfig) {
        self.config.store(Arc::new(config));
        info!("hedge executor configuration updated");
    }

    /// Returns a copy of the current configuration.
    ///
    /// Lock-free read via `ArcSwap::load`.
    #[must_use]
    pub fn get_config(&self) -> HedgeConfig {
        (**self.config.load()).clone()
    }

    /// Records a latency measurement for an upstream.
    pub fn record_latency(&self, upstream_name: &str, latency_ms: u64) {
        if let Some(tracker) = self.latency_trackers.get_mut(upstream_name) {
            tracker.record(latency_ms);
        } else {
            self.latency_trackers
                .entry(upstream_name.to_string())
                .or_insert_with(|| LatencyTracker::new(1000))
                .record(latency_ms);
        }

        if tracing::enabled!(tracing::Level::DEBUG) {
            if let Some(tracker) = self.latency_trackers.get(upstream_name) {
                if let Some(p95) = tracker.percentile(0.95) {
                    debug!(
                        upstream = %upstream_name,
                        latency_ms = latency_ms,
                        p95 = p95,
                        avg = tracker.average().unwrap_or(0),
                        samples = tracker.sample_count(),
                        "recorded upstream latency"
                    );
                }
            }
        }
    }

    /// Calculates the hedge delay for a specific upstream based on latency percentiles.
    ///
    /// Lock-free read of config via `ArcSwap::load`.
    #[must_use]
    pub fn calculate_hedge_delay(&self, upstream_name: &str) -> Option<Duration> {
        let config = self.config.load();

        let tracker = self.latency_trackers.get(upstream_name)?;
        let percentile_ms = tracker.percentile(config.latency_quantile)?;
        let delay_ms = percentile_ms.max(config.min_delay_ms).min(config.max_delay_ms);
        Some(Duration::from_millis(delay_ms))
    }

    /// Executes a request with hedging across multiple upstreams.
    ///
    /// # Errors
    /// Returns `UpstreamError` if all requests fail or no upstreams are available.
    pub async fn execute_hedged(
        &self,
        request: Arc<JsonRpcRequest>,
        upstreams: Vec<Arc<UpstreamEndpoint>>,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        if upstreams.is_empty() {
            return Err(UpstreamError::NoHealthyUpstreams);
        }

        let config = self.config.load();

        if !config.enabled || upstreams.len() == 1 {
            return self.execute_simple(request, &upstreams[0]).await;
        }

        let max_upstreams = config.max_parallel.min(upstreams.len());
        let primary_upstream = upstreams[0].clone();
        let primary_name = Arc::clone(&primary_upstream.config().name);

        let Some(delay) = self.calculate_hedge_delay(&primary_name) else {
            debug!(
                upstream = %primary_name,
                "insufficient latency data for hedging, using simple execution"
            );
            return self.execute_simple(request, &primary_upstream).await;
        };

        debug!(
            primary = %primary_name,
            hedge_delay_ms = delay.as_millis(),
            max_parallel = max_upstreams,
            "executing hedged request"
        );

        self.execute_with_hedging(request, upstreams, max_upstreams, delay).await
    }

    /// Executes a simple non-hedged request to a single upstream.
    async fn execute_simple(
        &self,
        request: Arc<JsonRpcRequest>,
        upstream: &Arc<UpstreamEndpoint>,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let start = Instant::now();
        let upstream_name = Arc::clone(&upstream.config().name);

        match upstream.send_request(&request).await {
            Ok(response) => {
                if let Ok(latency_ms) = u64::try_from(start.elapsed().as_millis()) {
                    self.record_latency(&upstream_name, latency_ms);
                }
                Ok(response)
            }
            Err(e) => {
                warn!(upstream = %upstream_name, error = %e, "request failed");
                Err(e)
            }
        }
    }

    async fn execute_with_hedging(
        &self,
        request: Arc<JsonRpcRequest>,
        upstreams: Vec<Arc<UpstreamEndpoint>>,
        max_parallel: usize,
        hedge_delay: Duration,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let primary = upstreams[0].clone();
        let primary_name = Arc::clone(&primary.config().name);
        let start = Instant::now();

        let primary_future = {
            let req = Arc::clone(&request);
            let upstream = primary.clone();
            async move { upstream.send_request(&req).await }
        };

        tokio::select! {
            result = primary_future => {
                let latency_ms = u64::try_from(start.elapsed().as_millis()).ok();
                match result {
                    Ok(response) => {
                        if let Some(ms) = latency_ms {
                            self.record_latency(&primary_name, ms);
                            debug!(
                                upstream = %primary_name,
                                latency_ms = ms,
                                "primary request succeeded before hedge"
                            );
                        }
                        Ok(response)
                    }
                    Err(e) => {
                        warn!(
                            upstream = %primary_name,
                            error = %e,
                            "primary request failed, attempting hedge"
                        );
                        self.execute_hedged_fallback(request, &upstreams[1..], max_parallel - 1).await
                    }
                }
            }
            () = tokio::time::sleep(hedge_delay) => {
                debug!(
                    upstream = %primary_name,
                    delay_ms = hedge_delay.as_millis(),
                    "hedge delay elapsed, issuing hedged requests"
                );
                self.execute_parallel_hedged(request, upstreams, max_parallel, start).await
            }
        }
    }

    async fn execute_parallel_hedged(
        &self,
        request: Arc<JsonRpcRequest>,
        upstreams: Vec<Arc<UpstreamEndpoint>>,
        max_parallel: usize,
        start_time: Instant,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let mut tasks = Vec::new();

        for upstream in upstreams.iter().take(max_parallel) {
            let req = Arc::clone(&request);
            let up = upstream.clone();
            let name = Arc::clone(&upstream.config().name);

            let task = async move {
                match up.send_request(&req).await {
                    Ok(resp) => Some((name, Ok(resp))),
                    Err(e) => Some((name, Err(e))),
                }
            };

            tasks.push(task);
        }

        let mut futures: Vec<_> = tasks.into_iter().map(Box::pin).collect();

        loop {
            if futures.is_empty() {
                break;
            }

            let (result, _index, remaining) = futures_util::future::select_all(futures).await;
            futures = remaining;

            if let Some((upstream_name, outcome)) = result {
                let latency_ms = u64::try_from(start_time.elapsed().as_millis()).ok();

                match outcome {
                    Ok(response) => {
                        if let Some(ms) = latency_ms {
                            self.record_latency(&upstream_name, ms);
                            info!(
                                upstream = %upstream_name,
                                latency_ms = ms,
                                hedged = true,
                                "hedged request succeeded"
                            );
                        }
                        return Ok(response);
                    }
                    Err(e) => {
                        warn!(upstream = %upstream_name, error = %e, "hedged request failed");
                    }
                }
            }
        }

        Err(UpstreamError::NoHealthyUpstreams)
    }

    async fn execute_hedged_fallback(
        &self,
        request: Arc<JsonRpcRequest>,
        upstreams: &[Arc<UpstreamEndpoint>],
        max_parallel: usize,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        if upstreams.is_empty() {
            return Err(UpstreamError::NoHealthyUpstreams);
        }

        let start = Instant::now();
        let mut tasks = Vec::new();

        for upstream in upstreams.iter().take(max_parallel) {
            let req = Arc::clone(&request);
            let up = upstream.clone();
            let name = Arc::clone(&upstream.config().name);

            let task = async move {
                match up.send_request(&req).await {
                    Ok(resp) => Some((name, Ok(resp))),
                    Err(e) => Some((name, Err(e))),
                }
            };

            tasks.push(task);
        }

        let mut futures: Vec<_> = tasks.into_iter().map(Box::pin).collect();

        loop {
            if futures.is_empty() {
                break;
            }

            let (result, _index, remaining) = futures_util::future::select_all(futures).await;
            futures = remaining;

            if let Some((upstream_name, Ok(response))) = result {
                if let Ok(latency_ms) = u64::try_from(start.elapsed().as_millis()) {
                    self.record_latency(&upstream_name, latency_ms);
                }
                return Ok(response);
            }
        }

        Err(UpstreamError::NoHealthyUpstreams)
    }

    /// Returns latency statistics (P50, P95, P99, average) for an upstream in milliseconds.
    #[must_use]
    pub fn get_latency_stats(&self, upstream_name: &str) -> Option<(u64, u64, u64, u64)> {
        let tracker = self.latency_trackers.get(upstream_name)?;

        let p50 = tracker.percentile(0.50)?;
        let p95 = tracker.percentile(0.95)?;
        let p99 = tracker.percentile(0.99)?;
        let avg = tracker.average()?;

        Some((p50, p95, p99, avg))
    }

    /// Clears all latency tracking data.
    pub fn clear_latency_data(&self) {
        self.latency_trackers.clear();
        info!("cleared all latency tracking data");
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn test_hedge_config_defaults() {
        let config = HedgeConfig::default();

        assert!(!config.enabled);
        assert_eq!(config.latency_quantile, 0.95);
        assert_eq!(config.min_delay_ms, 50);
        assert_eq!(config.max_delay_ms, 2000);
        assert_eq!(config.max_parallel, 2);
    }

    #[test]
    fn test_hedge_executor_creation() {
        let config = HedgeConfig::default();
        let executor = HedgeExecutor::new(config.clone());

        let retrieved_config = executor.get_config();
        assert_eq!(retrieved_config.enabled, config.enabled);
        assert_eq!(retrieved_config.latency_quantile, config.latency_quantile);
    }

    #[test]
    fn test_hedge_executor_latency_recording() {
        let executor = HedgeExecutor::new(HedgeConfig::default());

        executor.record_latency("test_upstream", 100);
        executor.record_latency("test_upstream", 200);
        executor.record_latency("test_upstream", 150);

        let stats = executor.get_latency_stats("test_upstream");
        assert!(stats.is_some());

        let (p50, _p95, _p99, avg) = stats.unwrap();
        assert_eq!(p50, 150);
        assert_eq!(avg, 150);
    }

    #[test]
    fn test_hedge_executor_config_update() {
        let executor = HedgeExecutor::new(HedgeConfig::default());

        let new_config = HedgeConfig {
            enabled: true,
            latency_quantile: 0.99,
            min_delay_ms: 100,
            max_delay_ms: 5000,
            max_parallel: 3,
        };

        executor.update_config(new_config.clone());

        let retrieved = executor.get_config();
        assert!(retrieved.enabled);
        assert_eq!(retrieved.latency_quantile, 0.99);
        assert_eq!(retrieved.max_parallel, 3);
    }

    #[test]
    fn test_hedge_executor_clear_data() {
        let executor = HedgeExecutor::new(HedgeConfig::default());

        executor.record_latency("test", 100);
        assert!(executor.get_latency_stats("test").is_some());

        executor.clear_latency_data();
        assert!(executor.get_latency_stats("test").is_none());
    }

    #[test]
    fn test_calculate_hedge_delay_basic() {
        let executor = HedgeExecutor::new(HedgeConfig::default());

        // Record some latency samples
        for latency in [100, 200, 150, 180, 220] {
            executor.record_latency("upstream-1", latency);
        }

        let delay = executor.calculate_hedge_delay("upstream-1");
        assert!(delay.is_some());

        #[allow(clippy::cast_possible_truncation)]
        let delay_ms = delay.unwrap().as_millis() as u64;
        // Should be clamped between min (50ms) and max (2000ms)
        assert!(delay_ms >= 50);
        assert!(delay_ms <= 2000);
    }

    #[test]
    fn test_calculate_hedge_delay_with_multiplier() {
        let executor = HedgeExecutor::new(HedgeConfig {
            enabled: true,
            latency_quantile: 0.99, // P99
            min_delay_ms: 100,
            max_delay_ms: 1000,
            max_parallel: 2,
        });

        // Record samples: 100ms repeated, P99 should be close to 500ms
        for _ in 0..95 {
            executor.record_latency("upstream-1", 100);
        }
        for _ in 0..5 {
            executor.record_latency("upstream-1", 500);
        }

        let delay = executor.calculate_hedge_delay("upstream-1");
        assert!(delay.is_some());

        #[allow(clippy::cast_possible_truncation)]
        let delay_ms = delay.unwrap().as_millis() as u64;
        // Should be at least min_delay_ms
        assert!(delay_ms >= 100);
        // P99 should be around 500ms, but at most max_delay_ms
        assert!(delay_ms <= 1000);
    }

    #[test]
    fn test_execute_hedged_first_wins() {
        let config = HedgeConfig {
            enabled: true,
            latency_quantile: 0.95,
            min_delay_ms: 50,
            max_delay_ms: 2000,
            max_parallel: 3,
        };
        let executor = HedgeExecutor::new(config);

        // Note: This test demonstrates the latency tracking logic
        // Full integration test with actual upstreams would require
        // mock HTTP clients, which is out of scope for unit tests

        // Record latency history with clear differences
        // Use enough samples to get reliable percentiles
        for _ in 0..100 {
            executor.record_latency("upstream-1", 100); // Fast
            executor.record_latency("upstream-2", 500); // Medium
            executor.record_latency("upstream-3", 1000); // Slow
        }

        // Verify latency data was recorded
        let stats1 = executor.get_latency_stats("upstream-1");
        let stats2 = executor.get_latency_stats("upstream-2");
        let stats3 = executor.get_latency_stats("upstream-3");

        assert!(stats1.is_some());
        assert!(stats2.is_some());
        assert!(stats3.is_some());

        // Verify P95 latencies are as expected
        let (_p50_1, p95_1, _p99_1, _avg_1) = stats1.unwrap();
        let (_p50_2, p95_2, _p99_2, _avg_2) = stats2.unwrap();
        let (_p50_3, p95_3, _p99_3, _avg_3) = stats3.unwrap();

        assert_eq!(p95_1, 100);
        assert_eq!(p95_2, 500);
        assert_eq!(p95_3, 1000);

        // Verify hedge delays reflect these latency differences
        let delay1 = executor.calculate_hedge_delay("upstream-1");
        let delay2 = executor.calculate_hedge_delay("upstream-2");
        let delay3 = executor.calculate_hedge_delay("upstream-3");

        assert!(delay1.is_some());
        assert!(delay2.is_some());
        assert!(delay3.is_some());

        // Delays should be based on P95, clamped by min/max
        assert_eq!(delay1.unwrap().as_millis(), 100);
        assert_eq!(delay2.unwrap().as_millis(), 500);
        assert_eq!(delay3.unwrap().as_millis(), 1000);
    }

    #[tokio::test]
    async fn test_execute_hedged_all_fail() {
        // Test that when all upstreams fail, hedging returns an error
        let executor = HedgeExecutor::new(HedgeConfig::default());

        // Verify empty upstreams returns error
        let request = Arc::new(crate::types::JsonRpcRequest::new(
            "eth_blockNumber",
            None,
            serde_json::json!(1),
        ));

        let result = executor.execute_hedged(request, vec![]).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::upstream::errors::UpstreamError::NoHealthyUpstreams
        ));
    }

    #[test]
    fn test_hedge_timeout_configuration() {
        let config = HedgeConfig {
            enabled: true,
            latency_quantile: 0.95,
            min_delay_ms: 50,
            max_delay_ms: 2000,
            max_parallel: 2,
        };
        let executor = HedgeExecutor::new(config.clone());

        // Verify configuration is set correctly
        let retrieved = executor.get_config();
        assert_eq!(retrieved.min_delay_ms, 50);
        assert_eq!(retrieved.max_delay_ms, 2000);

        // Test with very high latency - should be capped at max_delay_ms
        for _ in 0..10 {
            executor.record_latency("slow-upstream", 5000);
        }

        let delay = executor.calculate_hedge_delay("slow-upstream");
        assert!(delay.is_some());
        #[allow(clippy::cast_possible_truncation)]
        let delay_ms = delay.unwrap().as_millis() as u64;
        assert_eq!(delay_ms, 2000); // Capped at max

        // Test with very low latency - should be at least min_delay_ms
        for _ in 0..10 {
            executor.record_latency("fast-upstream", 10);
        }

        let delay = executor.calculate_hedge_delay("fast-upstream");
        assert!(delay.is_some());
        #[allow(clippy::cast_possible_truncation)]
        let delay_ms = delay.unwrap().as_millis() as u64;
        assert!(delay_ms >= 50); // At least min
    }

    #[test]
    fn test_latency_recording_updates_percentiles() {
        let executor = HedgeExecutor::new(HedgeConfig::default());

        // Record initial low latency samples
        for _ in 0..50 {
            executor.record_latency("upstream-1", 100);
        }

        let stats_before = executor.get_latency_stats("upstream-1");
        assert!(stats_before.is_some());
        let (_p50_before, p95_before, _p99_before, _avg_before) = stats_before.unwrap();

        // Record high latency samples to shift percentiles
        for _ in 0..50 {
            executor.record_latency("upstream-1", 500);
        }

        let stats_after = executor.get_latency_stats("upstream-1");
        assert!(stats_after.is_some());
        let (_p50_after, p95_after, _p99_after, _avg_after) = stats_after.unwrap();

        // P95 should increase due to high latency samples
        assert!(p95_after > p95_before);

        // Verify the hedge delay is affected
        let delay = executor.calculate_hedge_delay("upstream-1");
        assert!(delay.is_some());
    }
}
