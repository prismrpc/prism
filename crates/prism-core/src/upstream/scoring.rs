//! Multi-factor scoring system for upstream endpoint selection.
//!
//! Scores upstreams using latency percentiles, error rates, throttle rates, block lag,
//! and request load. Scores are calculated over a configurable time window and used by
//! the load balancer to select optimal upstreams.

use super::latency_tracker::LatencyTracker;
use crate::chain::ChainState;
use arc_swap::ArcSwap;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};
use tracing::debug;

/// Configuration for the scoring system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringConfig {
    /// Whether scoring is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Metric collection time window in seconds (default: 1800)
    #[serde(default = "default_window_seconds")]
    pub window_seconds: u64,

    /// Minimum samples required before scoring (default: 10)
    #[serde(default = "default_min_samples")]
    pub min_samples: usize,

    /// Factor weights
    #[serde(default)]
    pub weights: ScoringWeights,

    /// Maximum block lag before heavy penalty (default: 5)
    #[serde(default = "default_max_block_lag")]
    pub max_block_lag: u64,

    /// Number of top upstreams for weighted selection (default: 3)
    #[serde(default = "default_top_n")]
    pub top_n: usize,
}

fn default_window_seconds() -> u64 {
    1800
}
fn default_min_samples() -> usize {
    10
}
fn default_max_block_lag() -> u64 {
    5
}
fn default_top_n() -> usize {
    3
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            window_seconds: 1800,
            min_samples: 10,
            weights: ScoringWeights::default(),
            max_block_lag: 5,
            top_n: 3,
        }
    }
}

/// Weights for each scoring factor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringWeights {
    /// Latency factor weight (default: 8.0)
    #[serde(default = "default_latency_weight")]
    pub latency: f64,

    /// Error rate factor weight (default: 4.0)
    #[serde(default = "default_error_rate_weight")]
    pub error_rate: f64,

    /// Throttle rate factor weight (default: 3.0)
    #[serde(default = "default_throttle_rate_weight")]
    pub throttle_rate: f64,

    /// Block head lag factor weight (default: 2.0)
    #[serde(default = "default_block_head_lag_weight")]
    pub block_head_lag: f64,

    /// Total requests factor weight (default: 1.0)
    #[serde(default = "default_total_requests_weight")]
    pub total_requests: f64,
}

fn default_latency_weight() -> f64 {
    8.0
}
fn default_error_rate_weight() -> f64 {
    4.0
}
fn default_throttle_rate_weight() -> f64 {
    3.0
}
fn default_block_head_lag_weight() -> f64 {
    2.0
}
fn default_total_requests_weight() -> f64 {
    1.0
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            latency: 8.0,
            error_rate: 4.0,
            throttle_rate: 3.0,
            block_head_lag: 2.0,
            total_requests: 1.0,
        }
    }
}

/// Metrics collected for scoring an upstream endpoint.
///
/// # Lock-Free Design
///
/// All timestamp fields use atomic operations for lock-free access on the hot path.
/// An `epoch` field stores a reference `Instant`, and all timestamps are stored as
/// nanoseconds elapsed since this epoch in `AtomicU64` fields.
pub struct UpstreamMetrics {
    latency_tracker: LatencyTracker,
    total_requests: AtomicU64,
    error_count: AtomicU64,
    throttle_count: AtomicU64,
    success_count: AtomicU64,
    last_block_seen: AtomicU64,
    /// Nanoseconds since epoch when last block was seen (0 = never)
    last_block_time_nanos: AtomicU64,
    /// Nanoseconds since epoch when current window started
    window_start_nanos: AtomicU64,
    /// Reference point for nanosecond calculations
    epoch: Instant,
    window_duration_nanos: u64,
}

impl UpstreamMetrics {
    /// Creates a new metrics tracker with the specified window duration.
    #[must_use]
    pub fn new(window_seconds: u64) -> Self {
        let epoch = Instant::now();
        Self {
            latency_tracker: LatencyTracker::new(1000),
            total_requests: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            throttle_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            last_block_seen: AtomicU64::new(0),
            last_block_time_nanos: AtomicU64::new(0),
            window_start_nanos: AtomicU64::new(0),
            epoch,
            window_duration_nanos: window_seconds * 1_000_000_000,
        }
    }

    /// Returns nanoseconds elapsed since epoch for the current instant.
    ///
    /// The u128 to u64 truncation is intentional - nanoseconds will only overflow
    /// u64 after ~584 years of uptime, which is acceptable for metric windows.
    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    fn now_nanos(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    /// Records a successful request with its latency.
    ///
    /// Lock-free operation using atomic CAS for window reset.
    pub fn record_success(&self, latency_ms: u64) {
        self.maybe_reset_window();
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.success_count.fetch_add(1, Ordering::Relaxed);
        self.latency_tracker.record(latency_ms);
    }

    /// Records an error (non-throttle).
    ///
    /// Lock-free operation using atomic CAS for window reset.
    pub fn record_error(&self) {
        self.maybe_reset_window();
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a throttle response (HTTP 429 or rate limit error).
    ///
    /// Lock-free operation using atomic CAS for window reset.
    pub fn record_throttle(&self) {
        self.maybe_reset_window();
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.throttle_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Updates the last seen block number.
    ///
    /// Lock-free operation using atomic store.
    pub fn record_block_number(&self, block: u64) {
        let current = self.last_block_seen.load(Ordering::Relaxed);
        if block > current {
            self.last_block_seen.store(block, Ordering::Relaxed);
            self.last_block_time_nanos.store(self.now_nanos(), Ordering::Relaxed);
        }
    }

    /// Returns the error rate (errors / total requests).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn error_rate(&self) -> f64 {
        let total = self.total_requests.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        let errors = self.error_count.load(Ordering::Relaxed);
        errors as f64 / total as f64
    }

    /// Returns the throttle rate (throttles / total requests).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn throttle_rate(&self) -> f64 {
        let total = self.total_requests.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        let throttles = self.throttle_count.load(Ordering::Relaxed);
        throttles as f64 / total as f64
    }

    /// Returns the total number of requests in the current window.
    #[must_use]
    pub fn total_requests(&self) -> u64 {
        self.total_requests.load(Ordering::Relaxed)
    }

    /// Returns the last seen block number.
    #[must_use]
    pub fn last_block_seen(&self) -> u64 {
        self.last_block_seen.load(Ordering::Relaxed)
    }

    /// Returns latency percentiles (P50, P90, P95, P99).
    #[must_use]
    pub fn latency_percentiles(&self) -> Option<(u64, u64, u64, u64)> {
        let p50 = self.latency_tracker.percentile(0.50)?;
        let p90 = self.latency_tracker.percentile(0.90)?;
        let p95 = self.latency_tracker.percentile(0.95)?;
        let p99 = self.latency_tracker.percentile(0.99)?;
        Some((p50, p90, p95, p99))
    }

    /// Returns the average latency.
    #[must_use]
    pub fn average_latency(&self) -> Option<u64> {
        self.latency_tracker.average()
    }

    /// Returns the sample count for latency tracking.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.latency_tracker.sample_count()
    }

    /// Resets request counters when window duration expires.
    ///
    /// Uses atomic compare-and-swap (CAS) for lock-free window reset.
    /// If the window has expired and this thread wins the CAS race,
    /// it resets all counters. Other threads will see the updated window
    /// start and skip the reset.
    ///
    /// # Memory Ordering
    ///
    /// The reset uses a three-phase approach for proper synchronization:
    /// 1. CAS atomically claims the reset (prevents concurrent resets)
    /// 2. Counter stores use `Relaxed` ordering (fast, unordered)
    /// 3. Final `Release` store on `window_start_nanos` publishes all resets
    ///
    /// When another thread loads `window_start_nanos` with `Acquire` and sees
    /// the new value, all counter stores are guaranteed to be visible because
    /// they are sequenced-before the final `Release` store (step 3).
    ///
    /// Note: There is a brief window between step 1 and step 3 where readers
    /// might see the new window timestamp but old counter values. This is
    /// acceptable for metrics: the data self-corrects within nanoseconds and
    /// any brief inconsistency is within acceptable tolerances for scoring.
    fn maybe_reset_window(&self) {
        let now_nanos = self.now_nanos();
        let window_start = self.window_start_nanos.load(Ordering::Acquire);

        if now_nanos.saturating_sub(window_start) > self.window_duration_nanos {
            // Try to claim the reset with CAS.
            // Use Relaxed ordering since we'll publish with Release store below.
            if self
                .window_start_nanos
                .compare_exchange_weak(
                    window_start,
                    now_nanos,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                // Won the race - reset all counters with Relaxed ordering
                self.total_requests.store(0, Ordering::Relaxed);
                self.error_count.store(0, Ordering::Relaxed);
                self.throttle_count.store(0, Ordering::Relaxed);
                self.success_count.store(0, Ordering::Relaxed);

                // Publish the reset by storing window_start_nanos with Release.
                // This ensures all counter stores above are visible to any thread
                // that subsequently loads window_start_nanos with Acquire ordering.
                // The value is the same as the CAS wrote, but the Release ordering
                // is what provides the synchronization guarantee.
                self.window_start_nanos.store(now_nanos, Ordering::Release);
            }
            // If CAS failed, another thread already reset the window
        }
    }
}

/// Computed score for an upstream endpoint.
#[derive(Debug, Clone)]
pub struct UpstreamScore {
    pub upstream_name: Arc<str>,
    pub composite_score: f64,
    pub latency_factor: f64,
    pub error_rate_factor: f64,
    pub throttle_factor: f64,
    pub block_lag_factor: f64,
    pub load_factor: f64,
    pub latency_p90_ms: Option<u64>,
    pub latency_p99_ms: Option<u64>,
    pub error_rate: f64,
    pub throttle_rate: f64,
    pub block_head_lag: u64,
    pub total_requests: u64,
    pub last_updated: Instant,
}

/// Scoring engine that tracks metrics and calculates scores.
///
/// Uses shared `ChainState` for unified chain tip tracking. Metrics are stored
/// in a `DashMap` for lock-free concurrent access.
///
/// # Lock-Free Config Access
///
/// Configuration is stored in an `ArcSwap` for lock-free reads on the hot path.
/// Config updates are rare (admin operations) while reads happen on every request.
pub struct ScoringEngine {
    config: ArcSwap<ScoringConfig>,
    metrics: DashMap<String, Arc<UpstreamMetrics>>,
    /// Shared chain state. Secondary writer that updates tip number when upstreams
    /// report new blocks. See [`crate::chain`] for ownership pattern.
    chain_state: Arc<ChainState>,
}

impl ScoringEngine {
    /// Creates a new scoring engine with the given configuration.
    #[must_use]
    pub fn new(config: ScoringConfig, chain_state: Arc<ChainState>) -> Self {
        Self { config: ArcSwap::from_pointee(config), metrics: DashMap::new(), chain_state }
    }

    /// Updates the scoring configuration at runtime.
    ///
    /// This is a lock-free operation using `ArcSwap::store`.
    pub fn update_config(&self, config: ScoringConfig) {
        self.config.store(Arc::new(config));
    }

    /// Returns the current configuration.
    ///
    /// Lock-free read via `ArcSwap::load`.
    #[must_use]
    pub fn get_config(&self) -> ScoringConfig {
        (**self.config.load()).clone()
    }

    /// Returns whether scoring is enabled.
    ///
    /// Lock-free read via `ArcSwap::load`.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.load().enabled
    }

    /// Records a successful request for an upstream.
    ///
    /// Lock-free operation - no async overhead.
    pub fn record_success(&self, upstream_name: &str, latency_ms: u64) {
        let window_seconds = self.config.load().window_seconds;
        let metrics = self.get_or_create_metrics(upstream_name, window_seconds);
        metrics.record_success(latency_ms);
    }

    /// Records an error for an upstream.
    ///
    /// Lock-free operation - no async overhead.
    pub fn record_error(&self, upstream_name: &str) {
        let window_seconds = self.config.load().window_seconds;
        let metrics = self.get_or_create_metrics(upstream_name, window_seconds);
        metrics.record_error();
    }

    /// Records a throttle response for an upstream.
    ///
    /// Lock-free operation - no async overhead.
    pub fn record_throttle(&self, upstream_name: &str) {
        let window_seconds = self.config.load().window_seconds;
        let metrics = self.get_or_create_metrics(upstream_name, window_seconds);
        metrics.record_throttle();
    }

    fn get_or_create_metrics(
        &self,
        upstream_name: &str,
        window_seconds: u64,
    ) -> Arc<UpstreamMetrics> {
        if let Some(entry) = self.metrics.get(upstream_name) {
            return Arc::clone(entry.value());
        }

        Arc::clone(
            self.metrics
                .entry(upstream_name.to_string())
                .or_insert_with(|| Arc::new(UpstreamMetrics::new(window_seconds)))
                .value(),
        )
    }

    /// Records a block number seen from an upstream.
    ///
    /// The chain state update is async, but the metric recording is lock-free.
    pub async fn record_block_number(&self, upstream_name: &str, block: u64) {
        let _ = self.chain_state.update_tip_simple(block).await;

        let window_seconds = self.config.load().window_seconds;
        let metrics = self.get_or_create_metrics(upstream_name, window_seconds);
        metrics.record_block_number(block);
    }

    /// Returns the current chain tip from shared chain state.
    #[must_use]
    #[inline]
    pub fn chain_tip(&self) -> u64 {
        self.chain_state.current_tip()
    }

    /// Returns a reference to the shared chain state.
    #[must_use]
    #[inline]
    pub fn chain_state(&self) -> &Arc<ChainState> {
        &self.chain_state
    }

    /// Calculates the score for a single upstream.
    ///
    /// Lock-free read of config via `ArcSwap::load`.
    pub fn calculate_score(&self, upstream_name: &str) -> Option<UpstreamScore> {
        let metrics = self.metrics.get(upstream_name).map(|entry| Arc::clone(entry.value()))?;
        let config = self.get_config();

        if metrics.sample_count() < config.min_samples {
            debug!(upstream = %upstream_name, "insufficient samples for scoring");
            return None;
        }

        let chain_tip = self.chain_state.current_tip();
        let last_block = metrics.last_block_seen();
        let block_lag = chain_tip.saturating_sub(last_block);

        let (_p50, p90, _p95, p99) = metrics.latency_percentiles().unwrap_or((0, 0, 0, 0));
        let error_rate = metrics.error_rate();
        let throttle_rate = metrics.throttle_rate();
        let total_requests = metrics.total_requests();

        let latency_factor = Self::calculate_latency_factor(p90, &config);
        let error_rate_factor = Self::calculate_error_rate_factor(error_rate, &config);
        let throttle_factor = Self::calculate_throttle_factor(throttle_rate, &config);
        let block_lag_factor = Self::calculate_block_lag_factor(block_lag, &config);
        let load_factor = Self::calculate_load_factor(total_requests, &config);

        let composite = latency_factor.powf(config.weights.latency) *
            error_rate_factor.powf(config.weights.error_rate) *
            throttle_factor.powf(config.weights.throttle_rate) *
            block_lag_factor.powf(config.weights.block_head_lag) *
            load_factor.powf(config.weights.total_requests);

        let composite_score = composite * 100.0;

        Some(UpstreamScore {
            upstream_name: Arc::from(upstream_name),
            composite_score,
            latency_factor,
            error_rate_factor,
            throttle_factor,
            block_lag_factor,
            load_factor,
            latency_p90_ms: Some(p90),
            latency_p99_ms: Some(p99),
            error_rate,
            throttle_rate,
            block_head_lag: block_lag,
            total_requests,
            last_updated: Instant::now(),
        })
    }

    /// Calculates latency factor using P90 latency with logarithmic scaling.
    /// Returns 1.0 (best) for 0ms, decreasing logarithmically, clamped to minimum of 0.1.
    ///
    /// Algorithm: Uses log2 scaling over 14 bits (~16384ms range) for smooth degradation.
    fn calculate_latency_factor(p90_ms: u64, _config: &ScoringConfig) -> f64 {
        if p90_ms == 0 {
            return 1.0;
        }

        #[allow(clippy::cast_precision_loss)]
        let normalized = (p90_ms as f64).log2() / 14.0;
        (1.0 - normalized).clamp(0.1, 1.0)
    }

    /// Calculates error rate factor as linear penalty.
    /// Returns 1.0 (best) for 0% errors, 0.0 (worst) for 100% errors.
    fn calculate_error_rate_factor(error_rate: f64, _config: &ScoringConfig) -> f64 {
        (1.0 - error_rate).clamp(0.0, 1.0)
    }

    /// Calculates throttle factor with exponential decay penalty.
    /// Returns 1.0 (best) for 0% throttle, exponentially decaying for higher rates.
    ///
    /// Algorithm: e^(-3 * rate) provides sharper penalties than linear scaling.
    fn calculate_throttle_factor(throttle_rate: f64, _config: &ScoringConfig) -> f64 {
        (-3.0 * throttle_rate).exp().clamp(0.0, 1.0)
    }

    /// Calculates block lag factor with linear penalty up to `max_block_lag`.
    /// Returns 1.0 (best) for 0 lag, 0.0 (worst) for lag >= `max_block_lag`.
    fn calculate_block_lag_factor(block_lag: u64, config: &ScoringConfig) -> f64 {
        if block_lag == 0 {
            return 1.0;
        }

        #[allow(clippy::cast_precision_loss)]
        let penalty = (block_lag as f64 / config.max_block_lag as f64).min(1.0);
        (1.0 - penalty).clamp(0.0, 1.0)
    }

    /// Calculates load balancing factor (currently not implemented).
    /// Returns 1.0 (neutral) - reserved for future load-based scoring.
    fn calculate_load_factor(_total_requests: u64, _config: &ScoringConfig) -> f64 {
        1.0
    }

    /// Returns scores for all upstreams, sorted by composite score (best first).
    #[must_use]
    pub fn get_ranked_upstreams(&self) -> Vec<(String, f64)> {
        let upstream_names: Vec<String> =
            self.metrics.iter().map(|entry| entry.key().clone()).collect();

        let mut scores = Vec::new();
        for name in upstream_names {
            if let Some(score) = self.calculate_score(&name) {
                scores.push((name.clone(), score.composite_score));
            }
        }

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores
    }

    /// Returns the score for a specific upstream.
    #[must_use]
    pub fn get_score(&self, upstream_name: &str) -> Option<UpstreamScore> {
        self.calculate_score(upstream_name)
    }

    /// Returns metrics for a specific upstream.
    #[must_use]
    pub fn get_metrics(&self, upstream_name: &str) -> Option<(f64, f64, u64, u64, Option<u64>)> {
        let metrics = self.metrics.get(upstream_name).map(|entry| Arc::clone(entry.value()))?;

        Some((
            metrics.error_rate(),
            metrics.throttle_rate(),
            metrics.total_requests(),
            metrics.last_block_seen(),
            metrics.average_latency(),
        ))
    }

    /// Clears all metrics.
    pub fn clear(&self) {
        self.metrics.clear();
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn test_scoring_config_defaults() {
        let config = ScoringConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.window_seconds, 1800);
        assert_eq!(config.min_samples, 10);
        assert_eq!(config.weights.latency, 8.0);
        assert_eq!(config.weights.error_rate, 4.0);
        assert_eq!(config.weights.throttle_rate, 3.0);
        assert_eq!(config.weights.block_head_lag, 2.0);
        assert_eq!(config.weights.total_requests, 1.0);
    }

    #[test]
    fn test_scoring_weights_defaults() {
        let weights = ScoringWeights::default();
        assert_eq!(weights.latency, 8.0);
        assert_eq!(weights.error_rate, 4.0);
        assert_eq!(weights.throttle_rate, 3.0);
        assert_eq!(weights.block_head_lag, 2.0);
        assert_eq!(weights.total_requests, 1.0);
    }

    #[tokio::test]
    async fn test_upstream_metrics_basic() {
        let metrics = UpstreamMetrics::new(60);

        metrics.record_success(100);
        metrics.record_success(150);
        metrics.record_success(120);

        assert_eq!(metrics.total_requests(), 3);
        assert_eq!(metrics.error_rate(), 0.0);
        assert_eq!(metrics.throttle_rate(), 0.0);
    }

    #[tokio::test]
    async fn test_upstream_metrics_error_rate() {
        let metrics = UpstreamMetrics::new(60);

        metrics.record_success(100);
        metrics.record_success(100);
        metrics.record_success(100);
        metrics.record_error();

        assert_eq!(metrics.total_requests(), 4);
        assert!((metrics.error_rate() - 0.25).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_upstream_metrics_throttle_rate() {
        let metrics = UpstreamMetrics::new(60);

        metrics.record_success(100);
        metrics.record_success(100);
        metrics.record_success(100);
        metrics.record_success(100);
        metrics.record_throttle();

        assert_eq!(metrics.total_requests(), 5);
        assert!((metrics.throttle_rate() - 0.2).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_upstream_metrics_block_tracking() {
        let metrics = UpstreamMetrics::new(60);

        metrics.record_block_number(100);
        assert_eq!(metrics.last_block_seen(), 100);

        metrics.record_block_number(105);
        assert_eq!(metrics.last_block_seen(), 105);

        metrics.record_block_number(103);
        assert_eq!(metrics.last_block_seen(), 105);
    }

    #[test]
    fn test_scoring_engine_creation() {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(ScoringConfig::default(), chain_state);
        assert!(!engine.is_enabled());
        assert_eq!(engine.chain_tip(), 0);
    }

    #[tokio::test]
    async fn test_scoring_engine_record_success() {
        let config = ScoringConfig { min_samples: 1, ..ScoringConfig::default() };
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(config, chain_state);

        engine.record_success("upstream-1", 100);
        engine.record_success("upstream-1", 150);

        let metrics = engine.get_metrics("upstream-1");
        assert!(metrics.is_some());
        let (error_rate, throttle_rate, total, _block, _avg) = metrics.unwrap();
        assert_eq!(error_rate, 0.0);
        assert_eq!(throttle_rate, 0.0);
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn test_scoring_engine_chain_tip() {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(ScoringConfig::default(), chain_state);

        engine.record_block_number("upstream-1", 100).await;
        assert_eq!(engine.chain_tip(), 100);

        engine.record_block_number("upstream-2", 105).await;
        assert_eq!(engine.chain_tip(), 105);

        engine.record_block_number("upstream-3", 103).await;
        assert_eq!(engine.chain_tip(), 105);
    }

    #[tokio::test]
    async fn test_scoring_engine_calculate_score() {
        let config = ScoringConfig { min_samples: 5, enabled: true, ..ScoringConfig::default() };
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(config, chain_state);

        for _ in 0..10 {
            engine.record_success("upstream-1", 100);
        }
        engine.record_block_number("upstream-1", 100).await;

        let score = engine.calculate_score("upstream-1");
        assert!(score.is_some());
        let score = score.unwrap();
        assert!(score.composite_score > 0.0);
        assert!(score.composite_score <= 100.0);
        assert_eq!(score.error_rate, 0.0);
        assert_eq!(score.throttle_rate, 0.0);
    }

    #[tokio::test]
    async fn test_scoring_engine_insufficient_samples() {
        let config = ScoringConfig { min_samples: 10, ..ScoringConfig::default() };
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(config, chain_state);

        for _ in 0..5 {
            engine.record_success("upstream-1", 100);
        }

        let score = engine.calculate_score("upstream-1");
        assert!(score.is_none());
    }

    #[tokio::test]
    async fn test_scoring_engine_ranked_upstreams() {
        let config = ScoringConfig { min_samples: 5, enabled: true, ..ScoringConfig::default() };
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(config, chain_state);

        for _ in 0..10 {
            engine.record_success("upstream-1", 100);
        }
        engine.record_block_number("upstream-1", 100).await;

        for _ in 0..10 {
            engine.record_success("upstream-2", 500);
        }
        engine.record_block_number("upstream-2", 100).await;

        let ranked = engine.get_ranked_upstreams();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, "upstream-1");
        assert!(ranked[0].1 > ranked[1].1);
    }

    #[tokio::test]
    async fn test_scoring_engine_error_penalty() {
        let config = ScoringConfig { min_samples: 5, enabled: true, ..ScoringConfig::default() };
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(config, chain_state);

        for _ in 0..10 {
            engine.record_success("upstream-1", 100);
        }
        engine.record_block_number("upstream-1", 100).await;

        for _ in 0..8 {
            engine.record_success("upstream-2", 100);
        }
        for _ in 0..2 {
            engine.record_error("upstream-2");
        }
        engine.record_block_number("upstream-2", 100).await;

        let score1 = engine.calculate_score("upstream-1").unwrap();
        let score2 = engine.calculate_score("upstream-2").unwrap();

        assert!(score1.composite_score > score2.composite_score);
        assert_eq!(score1.error_rate, 0.0);
        assert!((score2.error_rate - 0.2).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_scoring_engine_block_lag_penalty() {
        let config = ScoringConfig {
            min_samples: 5,
            enabled: true,
            max_block_lag: 5,
            ..ScoringConfig::default()
        };
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(config, chain_state);

        engine.record_block_number("tip-setter", 110).await;

        for _ in 0..10 {
            engine.record_success("upstream-1", 100);
        }
        engine.record_block_number("upstream-1", 110).await;

        for _ in 0..10 {
            engine.record_success("upstream-2", 100);
        }
        engine.record_block_number("upstream-2", 107).await;

        let score1 = engine.calculate_score("upstream-1").unwrap();
        let score2 = engine.calculate_score("upstream-2").unwrap();

        assert!(score1.composite_score > score2.composite_score);
        assert_eq!(score1.block_head_lag, 0);
        assert_eq!(score2.block_head_lag, 3);
    }

    #[test]
    fn test_scoring_config_update() {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(ScoringConfig::default(), chain_state);
        assert!(!engine.is_enabled());

        let new_config = ScoringConfig { enabled: true, ..ScoringConfig::default() };
        engine.update_config(new_config);

        assert!(engine.is_enabled());
    }

    #[test]
    fn test_latency_factor_calculation() {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(ScoringConfig::default(), chain_state);
        let config = engine.get_config();

        let factor_zero = ScoringEngine::calculate_latency_factor(0, &config);
        assert!((factor_zero - 1.0).abs() < f64::EPSILON);

        let factor_100 = ScoringEngine::calculate_latency_factor(100, &config);
        assert!(factor_100 > 0.4 && factor_100 < 0.7);

        let factor_1000 = ScoringEngine::calculate_latency_factor(1000, &config);
        assert!(factor_1000 < factor_100);
        assert!(factor_1000 > 0.1);

        let factor_10000 = ScoringEngine::calculate_latency_factor(10000, &config);
        assert!(factor_10000 < factor_1000);
        assert!(factor_10000 >= 0.1);
    }

    #[test]
    #[allow(clippy::similar_names)]
    fn test_throttle_factor_calculation() {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(ScoringConfig::default(), chain_state);
        let config = engine.get_config();

        let factor_zero = ScoringEngine::calculate_throttle_factor(0.0, &config);
        assert!((factor_zero - 1.0).abs() < 0.001);

        let factor_five = ScoringEngine::calculate_throttle_factor(0.05, &config);
        assert!(factor_five < 1.0);
        assert!(factor_five > 0.8);

        let factor_twenty = ScoringEngine::calculate_throttle_factor(0.20, &config);
        assert!(factor_twenty < factor_five);
    }

    #[tokio::test]
    async fn test_sliding_window_reset() {
        // Use a very short window for faster test execution
        // Sleep 200ms extra to ensure we're past the window boundary
        // even under CI timing variability
        let window_seconds = 1;
        let metrics = UpstreamMetrics::new(window_seconds);

        metrics.record_success(100);
        metrics.record_success(150);
        assert_eq!(metrics.total_requests(), 2);

        // Sleep window + 200ms buffer to handle timing jitter
        tokio::time::sleep(tokio::time::Duration::from_millis(1200)).await;

        metrics.record_success(200);

        assert_eq!(metrics.total_requests(), 1);
        assert_eq!(metrics.error_rate(), 0.0);
    }

    #[tokio::test]
    async fn test_concurrent_scoring_access() {
        use std::sync::Arc;
        use tokio::task;

        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = Arc::new(ScoringEngine::new(
            ScoringConfig { min_samples: 5, enabled: true, ..ScoringConfig::default() },
            chain_state,
        ));

        let mut handles = vec![];

        for i in 0..10 {
            let engine_clone = engine.clone();
            let handle = task::spawn(async move {
                let upstream_name = format!("upstream-{}", i % 3);

                for _ in 0..10 {
                    engine_clone.record_success(&upstream_name, 100 + (i * 10));
                }

                engine_clone.record_block_number(&upstream_name, 1000 + i).await;

                engine_clone.calculate_score(&upstream_name)
            });
            handles.push(handle);
        }

        for handle in handles {
            let _ = handle.await;
        }

        let metrics = engine.get_metrics("upstream-0");
        assert!(metrics.is_some());

        let chain_tip = engine.chain_tip();
        assert!(chain_tip >= 1000);
    }

    #[tokio::test]
    async fn test_scoring_with_empty_window() {
        let config = ScoringConfig { min_samples: 10, enabled: true, ..ScoringConfig::default() };
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(config, chain_state);

        let score = engine.calculate_score("non-existent-upstream");
        assert!(score.is_none());

        for _ in 0..5 {
            engine.record_success("new-upstream", 100);
        }

        let score = engine.calculate_score("new-upstream");
        assert!(score.is_none());

        for _ in 0..5 {
            engine.record_success("new-upstream", 100);
        }

        let score = engine.calculate_score("new-upstream");
        assert!(score.is_some());
    }

    #[tokio::test]
    async fn test_throttle_vs_error_recording() {
        let metrics = UpstreamMetrics::new(60);

        metrics.record_success(100);
        metrics.record_success(100);
        metrics.record_error();
        metrics.record_throttle();
        metrics.record_throttle();

        assert_eq!(metrics.total_requests(), 5);

        let error_rate = metrics.error_rate();
        let throttle_rate = metrics.throttle_rate();

        assert!((error_rate - 0.2).abs() < 0.001);
        assert!((throttle_rate - 0.4).abs() < 0.001);

        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(ScoringConfig::default(), chain_state);

        engine.record_success("upstream-1", 100);
        engine.record_success("upstream-1", 100);
        engine.record_error("upstream-1");
        engine.record_throttle("upstream-1");

        let metrics_result = engine.get_metrics("upstream-1");
        assert!(metrics_result.is_some());

        let (err_rate, thr_rate, total, _block, _avg) = metrics_result.unwrap();
        assert_eq!(total, 4);
        assert!((err_rate - 0.25).abs() < 0.001);
        assert!((thr_rate - 0.25).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_latency_percentiles_accuracy() {
        let metrics = UpstreamMetrics::new(60);

        for i in 1..=100 {
            metrics.record_success(i);
        }

        let percentiles = metrics.latency_percentiles();
        assert!(percentiles.is_some());

        let (p50, p90, p95, p99) = percentiles.unwrap();

        assert!((45..=55).contains(&p50));
        assert!((85..=95).contains(&p90));
        assert!((90..=100).contains(&p95));
        assert!((95..=100).contains(&p99));
    }

    #[tokio::test]
    async fn test_window_reset_preserves_latency_samples() {
        let window_seconds = 1;
        let metrics = UpstreamMetrics::new(window_seconds);

        metrics.record_success(100);
        metrics.record_success(200);

        let avg_before = metrics.average_latency();
        assert!(avg_before.is_some());

        tokio::time::sleep(tokio::time::Duration::from_secs(window_seconds + 1)).await;

        metrics.record_success(300);

        assert_eq!(metrics.total_requests(), 1);

        let avg_after = metrics.average_latency();
        assert!(avg_after.is_some());
        assert!(avg_after.unwrap() > 100);
    }

    #[test]
    fn test_block_lag_factor_at_boundaries() {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(
            ScoringConfig { max_block_lag: 5, ..ScoringConfig::default() },
            chain_state,
        );
        let config = engine.get_config();

        let factor_zero = ScoringEngine::calculate_block_lag_factor(0, &config);
        assert!((factor_zero - 1.0).abs() < f64::EPSILON);

        let factor_max = ScoringEngine::calculate_block_lag_factor(5, &config);
        assert!((factor_max - 0.0).abs() < 0.001);

        let factor_half = ScoringEngine::calculate_block_lag_factor(2, &config);
        assert!((factor_half - 0.6).abs() < 0.1);

        let factor_beyond = ScoringEngine::calculate_block_lag_factor(10, &config);
        assert!((factor_beyond - 0.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_composite_score_calculation() {
        let config = ScoringConfig {
            min_samples: 5,
            enabled: true,
            max_block_lag: 5,
            ..ScoringConfig::default()
        };
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = ScoringEngine::new(config, chain_state);

        for _ in 0..10 {
            engine.record_success("good-upstream", 100);
        }
        engine.record_block_number("good-upstream", 1000).await;
        engine.record_block_number("chain-tip", 1000).await;

        for _ in 0..7 {
            engine.record_success("bad-upstream", 500);
        }
        for _ in 0..3 {
            engine.record_error("bad-upstream");
        }
        engine.record_block_number("bad-upstream", 995).await;

        let good_score = engine.calculate_score("good-upstream");
        let bad_score = engine.calculate_score("bad-upstream");

        assert!(good_score.is_some());
        assert!(bad_score.is_some());

        assert!(good_score.unwrap().composite_score > bad_score.unwrap().composite_score);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_no_deadlock_under_concurrent_load() {
        use std::sync::Arc;
        use tokio::{
            task,
            time::{timeout, Duration},
        };

        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = Arc::new(ScoringEngine::new(
            ScoringConfig { min_samples: 5, enabled: true, ..ScoringConfig::default() },
            chain_state,
        ));

        let mut handles = vec![];

        for i in 0..100 {
            let engine_clone = Arc::clone(&engine);
            let handle = task::spawn(async move {
                let upstream_name = format!("upstream-{}", i % 10);

                engine_clone.record_success(&upstream_name, 100 + i);
                engine_clone.record_error(&upstream_name);
                engine_clone.record_throttle(&upstream_name);
                engine_clone.record_block_number(&upstream_name, 1000 + i).await;

                let _ = engine_clone.calculate_score(&upstream_name);
                let _ = engine_clone.get_metrics(&upstream_name);
            });
            handles.push(handle);
        }

        let result = timeout(Duration::from_secs(10), async {
            for handle in handles {
                handle.await.expect("task should complete");
            }
        })
        .await;

        assert!(result.is_ok(), "Deadlock detected: operations did not complete within timeout");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_read_write_no_deadlock() {
        use std::sync::Arc;
        use tokio::{
            task,
            time::{timeout, Duration},
        };

        let chain_state = Arc::new(crate::chain::ChainState::new());
        let engine = Arc::new(ScoringEngine::new(
            ScoringConfig { min_samples: 1, enabled: true, ..ScoringConfig::default() },
            chain_state,
        ));

        for i in 0..5 {
            engine.record_success("test-upstream", 100 + i);
        }

        let mut handles = vec![];

        for _ in 0..20 {
            let engine_clone = Arc::clone(&engine);
            let handle = task::spawn(async move {
                for _ in 0..50 {
                    let _ = engine_clone.calculate_score("test-upstream");
                    let _ = engine_clone.get_metrics("test-upstream");
                    let _ = engine_clone.get_ranked_upstreams();
                }
            });
            handles.push(handle);
        }

        for i in 0..20 {
            let engine_clone = Arc::clone(&engine);
            let handle = task::spawn(async move {
                for _ in 0..50 {
                    engine_clone.record_success("test-upstream", 100 + i);
                    engine_clone.record_error("test-upstream");
                }
            });
            handles.push(handle);
        }

        let result = timeout(Duration::from_secs(10), async {
            for handle in handles {
                handle.await.expect("task should complete");
            }
        })
        .await;

        assert!(result.is_ok(), "Deadlock detected: concurrent read/write did not complete");
    }

    #[cfg(test)]
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn score_always_bounded(
                latency_p90 in 0u64..10000,
                error_rate in 0.0f64..1.0,
                throttle_rate in 0.0f64..1.0,
                block_lag in 0u64..1000,
                load in 0u64..100_000
            ) {
                let config = ScoringConfig::default();

                let latency_factor = ScoringEngine::calculate_latency_factor(latency_p90, &config);
                let error_factor = ScoringEngine::calculate_error_rate_factor(error_rate, &config);
                let throttle_factor = ScoringEngine::calculate_throttle_factor(throttle_rate, &config);
                let block_lag_factor = ScoringEngine::calculate_block_lag_factor(block_lag, &config);
                let load_factor = ScoringEngine::calculate_load_factor(load, &config);

                // Composite calculation from calculate_score method
                let composite = latency_factor.powf(config.weights.latency) *
                    error_factor.powf(config.weights.error_rate) *
                    throttle_factor.powf(config.weights.throttle_rate) *
                    block_lag_factor.powf(config.weights.block_head_lag) *
                    load_factor.powf(config.weights.total_requests);

                let score = composite * 100.0;

                prop_assert!((0.0..=100.0).contains(&score),
                    "Score {} out of bounds (latency={}, err={}, throttle={}, lag={}, load={})",
                    score, latency_p90, error_rate, throttle_rate, block_lag, load);
            }

            #[test]
            fn higher_error_rate_lower_score(
                latency_p90 in 0u64..1000,
                error_rate1 in 0.0f64..0.5,
                error_rate2 in 0.5f64..1.0,
                throttle_rate in 0.0f64..0.5,
                block_lag in 0u64..50,
                load in 0u64..10000
            ) {
                let config = ScoringConfig::default();

                // Calculate factors for both error rates
                let latency_factor = ScoringEngine::calculate_latency_factor(latency_p90, &config);
                let throttle_factor = ScoringEngine::calculate_throttle_factor(throttle_rate, &config);
                let block_lag_factor = ScoringEngine::calculate_block_lag_factor(block_lag, &config);
                let load_factor = ScoringEngine::calculate_load_factor(load, &config);

                let error_factor1 = ScoringEngine::calculate_error_rate_factor(error_rate1, &config);
                let error_factor2 = ScoringEngine::calculate_error_rate_factor(error_rate2, &config);

                let score1 = latency_factor.powf(config.weights.latency) *
                    error_factor1.powf(config.weights.error_rate) *
                    throttle_factor.powf(config.weights.throttle_rate) *
                    block_lag_factor.powf(config.weights.block_head_lag) *
                    load_factor.powf(config.weights.total_requests);

                let score2 = latency_factor.powf(config.weights.latency) *
                    error_factor2.powf(config.weights.error_rate) *
                    throttle_factor.powf(config.weights.throttle_rate) *
                    block_lag_factor.powf(config.weights.block_head_lag) *
                    load_factor.powf(config.weights.total_requests);

                prop_assert!(score1 >= score2,
                    "Lower error rate {} gave lower score {} vs higher error rate {} score {}",
                    error_rate1, score1 * 100.0, error_rate2, score2 * 100.0);
            }

            #[test]
            fn higher_latency_lower_score(
                latency1 in 0u64..500,
                latency2 in 500u64..5000,
                error_rate in 0.0f64..0.3,
                throttle_rate in 0.0f64..0.3,
                block_lag in 0u64..20,
                load in 0u64..5000
            ) {
                let config = ScoringConfig::default();

                let latency_factor1 = ScoringEngine::calculate_latency_factor(latency1, &config);
                let latency_factor2 = ScoringEngine::calculate_latency_factor(latency2, &config);

                let error_factor = ScoringEngine::calculate_error_rate_factor(error_rate, &config);
                let throttle_factor = ScoringEngine::calculate_throttle_factor(throttle_rate, &config);
                let block_lag_factor = ScoringEngine::calculate_block_lag_factor(block_lag, &config);
                let load_factor = ScoringEngine::calculate_load_factor(load, &config);

                let score1 = latency_factor1.powf(config.weights.latency) *
                    error_factor.powf(config.weights.error_rate) *
                    throttle_factor.powf(config.weights.throttle_rate) *
                    block_lag_factor.powf(config.weights.block_head_lag) *
                    load_factor.powf(config.weights.total_requests);

                let score2 = latency_factor2.powf(config.weights.latency) *
                    error_factor.powf(config.weights.error_rate) *
                    throttle_factor.powf(config.weights.throttle_rate) *
                    block_lag_factor.powf(config.weights.block_head_lag) *
                    load_factor.powf(config.weights.total_requests);

                prop_assert!(score1 >= score2,
                    "Lower latency {} gave lower score {} vs higher latency {} score {}",
                    latency1, score1 * 100.0, latency2, score2 * 100.0);
            }
        }
    }
}
