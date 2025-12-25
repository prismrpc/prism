//! # Metrics Architecture
//!
//! Dual-path metrics system optimized for high-throughput RPC processing.
//!
//! ## Hot Path (Lock-Free)
//!
//! Prometheus counters and histograms are recorded on every request:
//! - Counter increments are atomic (no locks)
//! - Histogram observations use lock-free algorithms
//! - String interning eliminates allocations for method/upstream names
//!
//! ## Aggregation Path
//!
//! Internal `RpcMetrics` provides richer statistics:
//! - Updated opportunistically via `try_write()` to avoid blocking
//! - Read via `/metrics` endpoint with read lock
//! - May be slightly stale but never blocks hot path
//!
//! ## Performance Characteristics
//!
//! | Operation | Latency | Notes |
//! |-----------|---------|-------|
//! | Prometheus record | ~10ns | Lock-free atomic ops |
//! | Internal metrics (hit) | ~50ns | Opportunistic write |
//! | Internal metrics (contention) | 0ns | Skipped, no block |
//!
//! ## String Interning
//!
//! Method names and upstream identifiers are interned to avoid per-request
//! allocations. This is an intentional bounded memory leak (~1KB total).

use crate::{
    auth::AuthError,
    middleware::validation::ValidationError,
    proxy::errors::ProxyError,
    types::RpcMetrics,
    upstream::{circuit_breaker::CircuitBreakerState, errors::UpstreamError},
};
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    sync::{Arc, OnceLock},
    time::Instant,
};
use tokio::sync::RwLock;

static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

// SECURITY NOTE: Upstream names are exposed in metrics labels.
// This is intentional for operational visibility and debugging.
// If running a public metrics endpoint, consider:
// 1. Restricting /metrics to internal network only
// 2. Using generic upstream names in configuration (e.g., "upstream-1")
// 3. Implementing metric label filtering in your metrics scraper

static UPSTREAM_NAME_POOL: OnceLock<dashmap::DashMap<String, &'static str>> = OnceLock::new();

#[inline]
fn upstream_to_static(upstream: &str) -> Cow<'static, str> {
    let pool = UPSTREAM_NAME_POOL.get_or_init(dashmap::DashMap::new);

    if let Some(interned) = pool.get(upstream) {
        return Cow::Borrowed(*interned);
    }

    let owned = upstream.to_string();
    let leaked: &'static str = Box::leak(owned.clone().into_boxed_str());
    pool.insert(owned, leaked);
    Cow::Borrowed(leaked)
}

#[inline]
fn method_to_static(method: &str) -> Cow<'static, str> {
    match method {
        "net_version" => Cow::Borrowed("net_version"),
        "eth_blockNumber" => Cow::Borrowed("eth_blockNumber"),
        "eth_chainId" => Cow::Borrowed("eth_chainId"),
        "eth_gasPrice" => Cow::Borrowed("eth_gasPrice"),
        "eth_getBalance" => Cow::Borrowed("eth_getBalance"),
        "eth_getBlockByHash" => Cow::Borrowed("eth_getBlockByHash"),
        "eth_getBlockByNumber" => Cow::Borrowed("eth_getBlockByNumber"),
        "eth_getLogs" => Cow::Borrowed("eth_getLogs"),
        "eth_getTransactionByHash" => Cow::Borrowed("eth_getTransactionByHash"),
        "eth_getTransactionReceipt" => Cow::Borrowed("eth_getTransactionReceipt"),
        "eth_getCode" => Cow::Borrowed("eth_getCode"),
        "eth_call" => Cow::Borrowed("eth_call"),
        "eth_estimateGas" => Cow::Borrowed("eth_estimateGas"),
        "eth_getTransactionCount" => Cow::Borrowed("eth_getTransactionCount"),
        _ => Cow::Owned(method.to_string()),
    }
}

pub trait MetricsState {
    fn as_metric_str(&self) -> &'static str;
    fn as_gauge_value(&self) -> f64;
}

impl MetricsState for CircuitBreakerState {
    fn as_metric_str(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }

    fn as_gauge_value(&self) -> f64 {
        match self {
            Self::Closed => 0.0,
            Self::Open => 1.0,
            Self::HalfOpen => 0.5,
        }
    }
}

impl MetricsState for UpstreamError {
    fn as_metric_str(&self) -> &'static str {
        use crate::upstream::errors::RpcErrorCategory;

        match self {
            Self::Timeout => "timeout",
            Self::ConnectionFailed(_) => "connection_failed",
            Self::HttpError(_, _) => "http_error",
            Self::RpcError(code, message) => {
                match RpcErrorCategory::from_code_and_message(*code, message) {
                    RpcErrorCategory::ClientError => "rpc_client_error",
                    RpcErrorCategory::ProviderError => "rpc_provider_error",
                    RpcErrorCategory::RateLimit => "rpc_rate_limit",
                    RpcErrorCategory::ParseError => "rpc_parse_error",
                    RpcErrorCategory::ExecutionError => "rpc_execution_error",
                }
            }
            Self::Network(_) => "network_error",
            Self::InvalidResponse(_) => "invalid_response",
            Self::NoHealthyUpstreams => "no_healthy_upstreams",
            Self::CircuitBreakerOpen => "circuit_breaker_open",
            Self::InvalidRequest(_) => "invalid_request",
            Self::ConcurrencyLimit(_) => "concurrency_limit",
            Self::ConsensusFailure(_) => "consensus_failure",
        }
    }

    fn as_gauge_value(&self) -> f64 {
        use crate::upstream::errors::RpcErrorCategory;

        match self {
            Self::Timeout | Self::Network(_) | Self::ConcurrencyLimit(_) => 0.5,
            Self::RpcError(code, message) => {
                match RpcErrorCategory::from_code_and_message(*code, message) {
                    RpcErrorCategory::ClientError |
                    RpcErrorCategory::ExecutionError |
                    RpcErrorCategory::RateLimit => 0.5,
                    RpcErrorCategory::ProviderError | RpcErrorCategory::ParseError => 1.0,
                }
            }
            Self::ConnectionFailed(_) |
            Self::HttpError(_, _) |
            Self::InvalidResponse(_) |
            Self::NoHealthyUpstreams |
            Self::CircuitBreakerOpen |
            Self::InvalidRequest(_) |
            Self::ConsensusFailure(_) => 1.0,
        }
    }
}

impl MetricsState for ValidationError {
    fn as_metric_str(&self) -> &'static str {
        match self {
            Self::InvalidVersion(_) => "invalid_version",
            Self::InvalidMethod(_) => "invalid_method",
            Self::MethodNotAllowed(_) => "method_not_allowed",
            Self::InvalidBlockParameter(_) => "invalid_block_parameter",
            Self::BlockRangeTooLarge(_) => "block_range_too_large",
            Self::TooManyTopics(_) => "too_many_topics",
            Self::InvalidBlockHash => "invalid_block_hash",
        }
    }

    fn as_gauge_value(&self) -> f64 {
        0.5
    }
}

impl MetricsState for AuthError {
    fn as_metric_str(&self) -> &'static str {
        match self {
            Self::InvalidApiKey => "invalid_api_key",
            Self::ExpiredApiKey => "expired_api_key",
            Self::InactiveApiKey => "inactive_api_key",
            Self::MethodNotAllowed(_) => "method_not_allowed",
            Self::ScopeNotAllowed(_, _) => "scope_not_allowed",
            Self::RateLimitExceeded => "rate_limit_exceeded",
            Self::QuotaExceeded => "quota_exceeded",
            Self::DatabaseError(_) => "database_error",
            Self::ConfigError(_) => "config_error",
            Self::KeyGenerationError(_) => "key_generation_error",
        }
    }

    fn as_gauge_value(&self) -> f64 {
        match self {
            Self::InvalidApiKey |
            Self::ExpiredApiKey |
            Self::InactiveApiKey |
            Self::MethodNotAllowed(_) |
            Self::ScopeNotAllowed(_, _) |
            Self::RateLimitExceeded |
            Self::QuotaExceeded => 0.5,
            Self::DatabaseError(_) | Self::ConfigError(_) | Self::KeyGenerationError(_) => 1.0,
        }
    }
}

impl MetricsState for ProxyError {
    fn as_metric_str(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::MethodNotSupported(_) => "method_not_supported",
            Self::RateLimited => "rate_limited",
            Self::Validation(_) => "validation_error",
            Self::Upstream(_) => "upstream_error",
            Self::Internal(_) => "internal_error",
        }
    }

    fn as_gauge_value(&self) -> f64 {
        match self {
            Self::InvalidRequest(_) |
            Self::MethodNotSupported(_) |
            Self::RateLimited |
            Self::Validation(_) => 0.5,
            Self::Upstream(_) | Self::Internal(_) => 1.0,
        }
    }
}

fn try_init_prometheus_recorder(
) -> Result<PrometheusHandle, metrics_exporter_prometheus::BuildError> {
    PrometheusBuilder::new().install_recorder()
}

#[allow(clippy::expect_used)]
fn init_prometheus_recorder() -> PrometheusHandle {
    PROMETHEUS_HANDLE
        .get_or_init(|| {
            match try_init_prometheus_recorder() {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Failed to install primary Prometheus recorder, attempting fallback"
                    );

                    let recorder = PrometheusBuilder::new().build_recorder();
                    tracing::warn!(
                        "Using fallback Prometheus recorder (install error: {e}) - metrics may not be globally visible"
                    );
                    recorder.handle()
                }
            }
        })
        .clone()
}

pub struct MetricsCollector {
    metrics: Arc<RwLock<RpcMetrics>>,
    prometheus_handle: PrometheusHandle,
}

impl MetricsCollector {
    /// # Errors
    ///
    /// Returns an error if the Prometheus recorder cannot be initialized.
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let prometheus_handle = init_prometheus_recorder();

        Ok(Self { metrics: Arc::new(RwLock::new(RpcMetrics::new())), prometheus_handle })
    }

    /// Record an RPC request
    ///
    /// Hot path optimization: Uses only lock-free Prometheus counters/histograms.
    /// Internal metrics (`RpcMetrics`) are updated opportunistically with `try_write`
    /// to avoid blocking on contention.
    ///
    /// String interning eliminates 5-10 allocations per request.
    #[allow(clippy::unused_async)] // Async for API consistency with other record methods
    pub async fn record_request(
        &self,
        method: &str,
        upstream: &str,
        success: bool,
        latency_ms: u64,
    ) {
        // Hot path: lock-free Prometheus metrics (always recorded)
        // Use interned strings to eliminate allocations
        let method_cow = method_to_static(method);
        let upstream_cow = upstream_to_static(upstream);

        counter!("rpc_requests_total", "method" => method_cow.clone(), "upstream" => upstream_cow.clone()).increment(1);
        #[allow(clippy::cast_precision_loss)]
        histogram!("rpc_request_duration_seconds", "method" => method_cow.clone(), "upstream" => upstream_cow.clone()).record(latency_ms as f64 / 1000.0);

        if success {
            counter!("rpc_requests_success_total", "method" => method_cow, "upstream" => upstream_cow).increment(1);
        } else {
            counter!("rpc_requests_error_total", "method" => method_cow, "upstream" => upstream_cow).increment(1);
        }

        // Opportunistic internal metrics update (non-blocking)
        if let Ok(mut metrics) = self.metrics.try_write() {
            metrics.increment_requests(method);
            metrics.record_latency(method, upstream, latency_ms);
            if !success {
                metrics.increment_upstream_errors(upstream);
            }
        }
        // If lock contention: skip internal metrics, Prometheus metrics already recorded
    }

    /// Record a cache hit
    pub fn record_cache_hit(&self, method: &str) {
        // Use try_write for non-blocking access - cache hits should be FAST
        if let Ok(mut metrics) = self.metrics.try_write() {
            metrics.increment_cache_hits(method);
        }
        // If lock contention occurs, skip internal metrics update but still record to Prometheus

        let method_cow = method_to_static(method);
        counter!("rpc_cache_hits_total", "method" => method_cow).increment(1);
    }

    /// Record a cache miss
    pub fn record_cache_miss(&self, method: &str) {
        let method_cow = method_to_static(method);
        counter!("rpc_cache_misses_total", "method" => method_cow).increment(1);
    }

    /// Record upstream error
    ///
    /// Hot path optimization: Prometheus counter is lock-free (always recorded).
    /// Internal metrics updated opportunistically.
    /// String interning eliminates allocations.
    #[allow(clippy::unused_async)] // Async for API consistency with other record methods
    pub async fn record_upstream_error(&self, upstream: &str, error_type: &str) {
        // Hot path: lock-free Prometheus counter with interned strings
        let upstream_cow = upstream_to_static(upstream);
        counter!("rpc_upstream_errors_total", "upstream" => upstream_cow, "error_type" => error_type.to_string()).increment(1);

        // Opportunistic internal metrics update
        if let Ok(mut metrics) = self.metrics.try_write() {
            metrics.increment_upstream_errors(upstream);
        }
    }

    /// Record a JSON-RPC error with detailed category breakdown
    pub fn record_rpc_error(&self, upstream: &str, code: i32, category: &str) {
        let upstream_cow = upstream_to_static(upstream);
        counter!(
            "rpc_jsonrpc_errors_total",
            "upstream" => upstream_cow,
            "code" => code.to_string(),
            "category" => category.to_string()
        )
        .increment(1);
    }

    /// Record upstream health status
    pub fn record_upstream_health(&self, upstream: &str, is_healthy: bool) {
        let upstream_cow = upstream_to_static(upstream);
        let health_value = if is_healthy { 1.0 } else { 0.0 };
        gauge!("rpc_upstream_health", "upstream" => upstream_cow).set(health_value);
    }

    /// Record active connections
    pub fn record_active_connections(&self, count: u64) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_active_connections").set(count as f64);
    }

    /// Record cache statistics
    pub fn record_cache_stats(&self, cache_type: &str, entries: usize, hit_rate: f64) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_cache_entries", "cache_type" => cache_type.to_string()).set(entries as f64);
        gauge!("rpc_cache_hit_rate", "cache_type" => cache_type.to_string()).set(hit_rate);
    }

    /// Record rate limiting metrics
    pub fn record_rate_limit(&self, key: &str, allowed: bool, tokens_remaining: f64) {
        if allowed {
            counter!("rpc_rate_limit_allowed_total", "key" => key.to_string()).increment(1);
        } else {
            counter!("rpc_rate_limit_rejected_total", "key" => key.to_string()).increment(1);
        }
        gauge!("rpc_rate_limit_tokens_remaining", "key" => key.to_string()).set(tokens_remaining);
    }

    /// Record authentication metrics
    pub fn record_auth_attempt(&self, success: bool, key_id: Option<&str>) {
        let key = key_id.unwrap_or("unknown");
        if success {
            counter!("rpc_auth_success_total", "key_id" => key.to_string()).increment(1);
        } else {
            counter!("rpc_auth_failure_total", "key_id" => key.to_string()).increment(1);
        }
    }

    /// Record request validation metrics
    pub fn record_validation(&self, method: &str, success: bool, error_type: Option<&str>) {
        let method_cow = method_to_static(method);
        if success {
            counter!("rpc_validation_success_total", "method" => method_cow).increment(1);
        } else {
            let err_type = error_type.unwrap_or("unknown");
            counter!("rpc_validation_failure_total", "method" => method_cow, "error_type" => err_type.to_string()).increment(1);
        }
    }

    /// Record WebSocket connection metrics
    pub fn record_websocket_connection(&self, connected: bool) {
        if connected {
            counter!("rpc_websocket_connections_total").increment(1);
            gauge!("rpc_websocket_active_connections").increment(1.0);
        } else {
            counter!("rpc_websocket_disconnections_total").increment(1);
            gauge!("rpc_websocket_active_connections").decrement(1.0);
        }
    }

    /// Record WebSocket message metrics
    pub fn record_websocket_message(&self, message_type: &str, size_bytes: usize) {
        counter!("rpc_websocket_messages_total", "type" => message_type.to_string()).increment(1);
        #[allow(clippy::cast_precision_loss)]
        histogram!("rpc_websocket_message_size_bytes", "type" => message_type.to_string())
            .record(size_bytes as f64);
    }

    /// Record cache eviction metrics
    pub fn record_cache_eviction(&self, cache_type: &str, evicted_entries: usize) {
        #[allow(clippy::cast_precision_loss)]
        counter!("rpc_cache_evictions_total", "cache_type" => cache_type.to_string())
            .increment(evicted_entries as u64);
    }

    /// Record memory usage metrics
    pub fn record_memory_usage(&self, component: &str, bytes_used: u64) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_memory_usage_bytes", "component" => component.to_string())
            .set(bytes_used as f64);
    }

    /// Record upstream selection metrics
    pub fn record_upstream_selection(&self, upstream: &str, reason: &str) {
        let upstream_cow = upstream_to_static(upstream);
        counter!("rpc_upstream_selections_total", "upstream" => upstream_cow, "reason" => reason.to_string()).increment(1);
    }

    /// Record reorg detection metrics
    pub fn record_reorg(&self, depth: u64, block_number: u64) {
        counter!("rpc_reorgs_detected_total").increment(1);
        #[allow(clippy::cast_precision_loss)]
        histogram!("rpc_reorg_depth").record(depth as f64);
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_last_reorg_block").set(block_number as f64);
    }

    /// Record partial cache fulfillment metrics
    pub fn record_partial_cache_fulfillment(&self, method: &str, fulfilled_percentage: f64) {
        let method_cow = method_to_static(method);
        counter!("rpc_partial_cache_fulfillments_total", "method" => method_cow.clone())
            .increment(1);
        histogram!("rpc_partial_cache_fulfillment_percentage", "method" => method_cow)
            .record(fulfilled_percentage);
    }

    /// Record request queue metrics
    pub fn record_request_queue(&self, queue_name: &str, size: usize, wait_time_ms: u64) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_request_queue_size", "queue" => queue_name.to_string()).set(size as f64);
        #[allow(clippy::cast_precision_loss)]
        histogram!("rpc_request_queue_wait_time_seconds", "queue" => queue_name.to_string())
            .record(wait_time_ms as f64 / 1000.0);
    }

    // Circuit Breaker Metrics

    /// Record circuit breaker state transition
    pub fn record_circuit_breaker_state(
        &self,
        upstream: &str,
        state: CircuitBreakerState,
        failure_count: u32,
    ) {
        gauge!("rpc_circuit_breaker_state", "upstream" => upstream.to_string())
            .set(state.as_gauge_value());
        counter!(
            "rpc_circuit_breaker_transitions_total",
            "upstream" => upstream.to_string(),
            "to_state" => state.as_metric_str().to_string()
        )
        .increment(1);
        gauge!("rpc_circuit_breaker_failure_count", "upstream" => upstream.to_string())
            .set(f64::from(failure_count));
    }

    /// Record circuit breaker open duration when it closes
    pub fn record_circuit_breaker_open_duration(&self, upstream: &str, duration_seconds: f64) {
        histogram!(
            "rpc_circuit_breaker_open_duration_seconds",
            "upstream" => upstream.to_string()
        )
        .record(duration_seconds);
    }

    // Health Check Metrics

    /// Record health check response time
    pub fn record_health_check(&self, upstream: &str, success: bool, response_time_ms: u64) {
        #[allow(clippy::cast_precision_loss)]
        histogram!(
            "rpc_health_check_duration_seconds",
            "upstream" => upstream.to_string()
        )
        .record(response_time_ms as f64 / 1000.0);

        if success {
            counter!(
                "rpc_health_check_success_total",
                "upstream" => upstream.to_string()
            )
            .increment(1);
        } else {
            counter!(
                "rpc_health_check_failure_total",
                "upstream" => upstream.to_string()
            )
            .increment(1);
        }
    }

    /// Record number of healthy upstreams
    pub fn record_healthy_upstream_count(&self, count: usize) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_healthy_upstreams").set(count as f64);
    }

    /// Record upstream latency percentiles for hedging decisions
    pub fn record_upstream_latency_percentiles(
        &self,
        upstream: &str,
        p50: u64,
        p95: u64,
        p99: u64,
        avg: u64,
    ) {
        #[allow(clippy::cast_precision_loss)]
        {
            gauge!("rpc_upstream_latency_p50_ms", "upstream" => upstream.to_string())
                .set(p50 as f64);
            gauge!("rpc_upstream_latency_p95_ms", "upstream" => upstream.to_string())
                .set(p95 as f64);
            gauge!("rpc_upstream_latency_p99_ms", "upstream" => upstream.to_string())
                .set(p99 as f64);
            gauge!("rpc_upstream_latency_avg_ms", "upstream" => upstream.to_string())
                .set(avg as f64);
        }
    }

    // Scoring Metrics

    /// Record upstream scoring metrics for monitoring and debugging
    pub fn record_upstream_score(
        &self,
        upstream: &str,
        composite_score: f64,
        latency_factor: f64,
        error_rate_factor: f64,
        throttle_factor: f64,
        block_lag_factor: f64,
    ) {
        gauge!("rpc_upstream_composite_score", "upstream" => upstream.to_string())
            .set(composite_score);
        gauge!("rpc_upstream_latency_factor", "upstream" => upstream.to_string())
            .set(latency_factor);
        gauge!("rpc_upstream_error_rate_factor", "upstream" => upstream.to_string())
            .set(error_rate_factor);
        gauge!("rpc_upstream_throttle_factor", "upstream" => upstream.to_string())
            .set(throttle_factor);
        gauge!("rpc_upstream_block_lag_factor", "upstream" => upstream.to_string())
            .set(block_lag_factor);
    }

    /// Record upstream error rate for scoring
    pub fn record_upstream_error_rate(&self, upstream: &str, error_rate: f64) {
        gauge!("rpc_upstream_error_rate", "upstream" => upstream.to_string()).set(error_rate);
    }

    /// Record upstream throttle rate for scoring
    pub fn record_upstream_throttle_rate(&self, upstream: &str, throttle_rate: f64) {
        gauge!("rpc_upstream_throttle_rate", "upstream" => upstream.to_string()).set(throttle_rate);
    }

    /// Record upstream block head lag for scoring
    #[allow(clippy::cast_precision_loss)]
    pub fn record_upstream_block_lag(&self, upstream: &str, block_lag: u64) {
        gauge!("rpc_upstream_block_head_lag", "upstream" => upstream.to_string())
            .set(block_lag as f64);
    }

    /// Record the current chain tip (highest block seen)
    #[allow(clippy::cast_precision_loss)]
    pub fn record_chain_tip(&self, block_number: u64) {
        gauge!("rpc_chain_tip_block").set(block_number as f64);
    }

    // Cache Detailed Metrics

    /// Record block cache specific metrics
    pub fn record_block_cache_stats(
        &self,
        hot_window_size: usize,
        lru_entries: usize,
        total_bytes: u64,
    ) {
        #[allow(clippy::cast_precision_loss)]
        {
            gauge!("rpc_block_cache_hot_window_size").set(hot_window_size as f64);
            gauge!("rpc_block_cache_lru_entries").set(lru_entries as f64);
            gauge!("rpc_block_cache_bytes").set(total_bytes as f64);
        }
    }

    /// Record log cache specific metrics
    pub fn record_log_cache_stats(
        &self,
        chunks: usize,
        indexed_blocks: usize,
        bitmap_entries: usize,
        total_bytes: u64,
    ) {
        #[allow(clippy::cast_precision_loss)]
        {
            gauge!("rpc_log_cache_chunks").set(chunks as f64);
            gauge!("rpc_log_cache_indexed_blocks").set(indexed_blocks as f64);
            gauge!("rpc_log_cache_bitmap_entries").set(bitmap_entries as f64);
            gauge!("rpc_log_cache_bytes").set(total_bytes as f64);
        }
    }

    /// Record transaction cache specific metrics
    pub fn record_transaction_cache_stats(
        &self,
        transactions: usize,
        receipts: usize,
        total_bytes: u64,
    ) {
        #[allow(clippy::cast_precision_loss)]
        {
            gauge!("rpc_transaction_cache_entries").set(transactions as f64);
            gauge!("rpc_receipt_cache_entries").set(receipts as f64);
            gauge!("rpc_transaction_cache_bytes").set(total_bytes as f64);
        }
    }

    /// Record cache hot window advancement
    pub fn record_hot_window_advancement(&self, new_start_block: u64) {
        counter!("rpc_block_cache_hot_window_advancements_total").increment(1);
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_block_cache_hot_window_start").set(new_start_block as f64);
    }

    /// Record inflight fetch deduplication
    pub fn record_fetch_deduplication(&self, cache_type: &str, deduplicated: bool) {
        if deduplicated {
            counter!(
                "rpc_cache_fetch_deduplications_total",
                "cache_type" => cache_type.to_string()
            )
            .increment(1);
        }
    }

    /// Record current inflight fetch count
    pub fn record_inflight_fetches(&self, count: usize) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_cache_inflight_fetches").set(count as f64);
    }

    // Upstream Retry Metrics

    /// Record upstream retry attempt
    pub fn record_retry_attempt(&self, upstream: &str, attempt: u32, delay_ms: u64) {
        counter!(
            "rpc_upstream_retry_attempts_total",
            "upstream" => upstream.to_string()
        )
        .increment(1);
        #[allow(clippy::cast_precision_loss)]
        histogram!(
            "rpc_upstream_retry_delay_seconds",
            "upstream" => upstream.to_string()
        )
        .record(delay_ms as f64 / 1000.0);
        gauge!(
            "rpc_upstream_retry_attempt_number",
            "upstream" => upstream.to_string()
        )
        .set(f64::from(attempt));
    }

    /// Record when no healthy upstreams are available
    pub fn record_no_healthy_upstreams(&self) {
        counter!("rpc_no_healthy_upstreams_total").increment(1);
    }

    // Load Balancer Metrics

    /// Record load balancer selection
    pub fn record_load_balancer_selection(
        &self,
        upstream: &str,
        strategy: &str,
        healthy_count: usize,
    ) {
        counter!(
            "rpc_load_balancer_selections_total",
            "upstream" => upstream.to_string(),
            "strategy" => strategy.to_string()
        )
        .increment(1);
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_load_balancer_healthy_count").set(healthy_count as f64);
    }

    // Hedging Metrics

    /// Record a hedged request being issued
    pub fn record_hedged_request(&self, primary_upstream: &str, hedged_upstream: &str) {
        counter!(
            "rpc_hedged_requests_total",
            "primary" => primary_upstream.to_string(),
            "hedged" => hedged_upstream.to_string()
        )
        .increment(1);
    }

    /// Record which request won the hedge race
    pub fn record_hedge_winner(&self, upstream: &str, was_primary: bool) {
        let request_type = if was_primary { "primary" } else { "hedged" };
        counter!(
            "rpc_hedge_wins_total",
            "upstream" => upstream.to_string(),
            "type" => request_type.to_string()
        )
        .increment(1);
    }

    /// Record the hedge delay used
    pub fn record_hedge_delay(&self, upstream: &str, delay_ms: u64) {
        #[allow(clippy::cast_precision_loss)]
        histogram!("rpc_hedge_delay_ms", "upstream" => upstream.to_string())
            .record(delay_ms as f64);
    }

    /// Record hedging being skipped due to insufficient data
    pub fn record_hedge_skip(&self, reason: &str) {
        counter!("rpc_hedge_skipped_total", "reason" => reason.to_string()).increment(1);
    }

    // Auth Cache Metrics

    /// Record auth cache hit/miss
    pub fn record_auth_cache(&self, hit: bool) {
        if hit {
            counter!("rpc_auth_cache_hits_total").increment(1);
        } else {
            counter!("rpc_auth_cache_misses_total").increment(1);
        }
    }

    /// Record auth cache size
    pub fn record_auth_cache_size(&self, size: usize) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_auth_cache_entries").set(size as f64);
    }

    /// Record quota exceeded event
    pub fn record_quota_exceeded(&self, key_id: &str) {
        counter!("rpc_auth_quota_exceeded_total", "key_id" => key_id.to_string()).increment(1);
    }

    /// Record method permission denied
    pub fn record_method_permission_denied(&self, key_id: &str, method: &str) {
        counter!(
            "rpc_auth_method_denied_total",
            "key_id" => key_id.to_string(),
            "method" => method.to_string()
        )
        .increment(1);
    }

    // Rate Limiter Bucket Metrics

    /// Record rate limiter bucket count
    pub fn record_rate_limit_bucket_count(&self, count: usize) {
        #[allow(clippy::cast_precision_loss)]
        gauge!("rpc_rate_limit_bucket_count").set(count as f64);
    }

    /// Record rate limiter cleanup
    pub fn record_rate_limit_cleanup(&self, removed_buckets: usize) {
        #[allow(clippy::cast_precision_loss)]
        counter!("rpc_rate_limit_cleanup_removed_total").increment(removed_buckets as u64);
    }

    // Chain Tip Metrics

    // Note: record_chain_tip is defined in the Scoring Metrics section above

    /// Record chain tip update latency
    pub fn record_chain_tip_update_latency(&self, latency_ms: u64) {
        #[allow(clippy::cast_precision_loss)]
        histogram!("rpc_chain_tip_update_latency_seconds").record(latency_ms as f64 / 1000.0);
    }

    // Request Size Metrics

    /// Record request/response sizes
    pub fn record_request_size(&self, method: &str, request_bytes: usize, response_bytes: usize) {
        let method_cow = method_to_static(method);
        #[allow(clippy::cast_precision_loss)]
        {
            histogram!(
                "rpc_request_size_bytes",
                "method" => method_cow.clone()
            )
            .record(request_bytes as f64);
            histogram!(
                "rpc_response_size_bytes",
                "method" => method_cow
            )
            .record(response_bytes as f64);
        }
    }

    // Batch Request Metrics

    /// Record batch request metrics
    pub fn record_batch_request(&self, batch_size: usize, duration_ms: u64) {
        #[allow(clippy::cast_precision_loss)]
        {
            histogram!("rpc_batch_request_size").record(batch_size as f64);
            histogram!("rpc_batch_request_duration_seconds").record(duration_ms as f64 / 1000.0);
        }
        counter!("rpc_batch_requests_total").increment(1);
    }

    // Error Metrics (using MetricsState trait)

    /// Record an upstream error with detailed type breakdown
    pub fn record_upstream_error_typed(&self, upstream: &str, error: &UpstreamError) {
        counter!(
            "rpc_upstream_errors_by_type_total",
            "upstream" => upstream.to_string(),
            "error_type" => error.as_metric_str().to_string()
        )
        .increment(1);
        gauge!(
            "rpc_upstream_error_severity",
            "upstream" => upstream.to_string()
        )
        .set(error.as_gauge_value());
    }

    /// Record a validation error with type breakdown
    pub fn record_validation_error(&self, method: &str, error: &ValidationError) {
        let method_cow = method_to_static(method);
        counter!(
            "rpc_validation_errors_by_type_total",
            "method" => method_cow,
            "error_type" => error.as_metric_str()
        )
        .increment(1);
    }

    /// Record an authentication error with type breakdown
    pub fn record_auth_error(&self, error: &AuthError, key_id: Option<&str>) {
        let key = key_id.unwrap_or("unknown");
        counter!(
            "rpc_auth_errors_by_type_total",
            "key_id" => key.to_string(),
            "error_type" => error.as_metric_str().to_string()
        )
        .increment(1);
        // Track severity for alerting on system errors vs client errors
        if error.as_gauge_value() >= 1.0 {
            counter!("rpc_auth_system_errors_total").increment(1);
        }
    }

    /// Record a proxy error with type breakdown
    pub fn record_proxy_error(&self, method: &str, error: &ProxyError) {
        let method_cow = method_to_static(method);
        counter!(
            "rpc_proxy_errors_by_type_total",
            "method" => method_cow,
            "error_type" => error.as_metric_str()
        )
        .increment(1);
        // Track severity for alerting
        if error.as_gauge_value() >= 1.0 {
            counter!("rpc_proxy_critical_errors_total").increment(1);
        }
    }

    /// Get current metrics as a string (for Prometheus endpoint)
    #[must_use]
    pub fn get_prometheus_metrics(&self) -> String {
        self.prometheus_handle.render()
    }

    /// Get internal metrics
    pub async fn get_internal_metrics(&self) -> RpcMetrics {
        let guard = self.metrics.read().await;
        guard.clone()
    }

    /// Helper method to record a successful operation with timing
    pub async fn record_operation_success(
        &self,
        operation: &str,
        method: &str,
        upstream: &str,
        duration_ms: u64,
    ) {
        self.record_request(method, upstream, true, duration_ms).await;
        let method_cow = method_to_static(method);
        counter!("rpc_operations_success_total", "operation" => operation.to_string(), "method" => method_cow).increment(1);
    }

    /// Helper method to record a failed operation with timing and error details
    pub async fn record_operation_failure(
        &self,
        operation: &str,
        method: &str,
        upstream: &str,
        duration_ms: u64,
        error_type: &str,
    ) {
        self.record_request(method, upstream, false, duration_ms).await;
        let method_cow = method_to_static(method);
        counter!("rpc_operations_failure_total", "operation" => operation.to_string(), "method" => method_cow, "error_type" => error_type.to_string()).increment(1);
    }

    /// Reset all metrics
    pub async fn reset_metrics(&self) {
        let mut metrics = self.metrics.write().await;
        *metrics = RpcMetrics::new();
        tracing::info!("Metrics reset");
    }

    /// Get metrics summary
    #[allow(clippy::cast_precision_loss)]
    pub async fn get_metrics_summary(&self) -> MetricsSummary {
        let metrics = self.metrics.read().await;
        let total_requests: u64 = metrics.requests_total.values().sum();
        let total_cache_hits: u64 = metrics.cache_hits_total.values().sum();
        let total_upstream_errors: u64 = metrics.upstream_errors_total.values().sum();
        let cache_hit_rate = if total_requests > 0 {
            total_cache_hits as f64 / total_requests as f64
        } else {
            0.0
        };
        let avg_latency = if metrics.latency_histogram.is_empty() {
            0.0
        } else {
            let total_latency: u64 = metrics.latency_histogram.values().flatten().sum();
            let total_responses: usize =
                metrics.latency_histogram.values().map(std::collections::VecDeque::len).sum();
            if total_responses > 0 {
                total_latency as f64 / total_responses as f64
            } else {
                0.0
            }
        };
        MetricsSummary {
            total_requests,
            total_cache_hits,
            total_upstream_errors,
            cache_hit_rate,
            average_latency_ms: avg_latency,
            // Convert &'static str keys to String for serialization at the API boundary
            requests_by_method: metrics
                .requests_total
                .iter()
                .map(|(&k, &v)| (k.to_string(), v))
                .collect(),
            cache_hits_by_method: metrics
                .cache_hits_total
                .iter()
                .map(|(&k, &v)| (k.to_string(), v))
                .collect(),
            errors_by_upstream: metrics
                .upstream_errors_total
                .iter()
                .map(|(&k, &v)| (k.to_string(), v))
                .collect(),
            rate_limit_rejections: 0, // These would be tracked separately in production
            auth_failures: 0,
            validation_failures: 0,
            active_connections: 0,
            websocket_connections: 0,
            reorg_detections: 0,
        }
    }
}

/// Summary of collected metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSummary {
    pub total_requests: u64,
    pub total_cache_hits: u64,
    pub total_upstream_errors: u64,
    pub cache_hit_rate: f64,
    pub average_latency_ms: f64,
    pub requests_by_method: std::collections::HashMap<String, u64>,
    pub cache_hits_by_method: std::collections::HashMap<String, u64>,
    pub errors_by_upstream: std::collections::HashMap<String, u64>,
    pub rate_limit_rejections: u64,
    pub auth_failures: u64,
    pub validation_failures: u64,
    pub active_connections: u64,
    pub websocket_connections: u64,
    pub reorg_detections: u64,
}

/// Metrics middleware for tracking request timing
pub struct MetricsMiddleware {
    collector: Arc<MetricsCollector>,
}

impl MetricsMiddleware {
    #[must_use]
    pub fn new(collector: Arc<MetricsCollector>) -> Self {
        Self { collector }
    }

    /// Track a request with timing
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn track_request<F, T, E>(&self, method: &str, upstream: &str, f: F) -> Result<T, E>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        let start_time = Instant::now();
        let result = f.await;
        let latency_ms = u64::try_from(start_time.elapsed().as_millis()).unwrap_or(u64::MAX);
        let success = result.is_ok();
        self.collector.record_request(method, upstream, success, latency_ms).await;
        result
    }
}

/// Initialize default metrics
/// # Errors
///
/// Returns an error if the metrics collector cannot be initialized.
pub fn init_metrics() -> Result<Arc<MetricsCollector>, Box<dyn std::error::Error + Send + Sync>> {
    let collector = Arc::new(MetricsCollector::new()?);
    Ok(collector)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    // MetricsCollector Basic Tests

    #[tokio::test]
    async fn test_metrics_collector() {
        let collector = MetricsCollector::new().unwrap();
        collector.record_request("eth_getBlockByNumber", "infura", true, 100).await;
        collector.record_cache_hit("eth_getBlockByNumber");
        collector.record_upstream_error("infura", "timeout").await;
        let summary = collector.get_metrics_summary().await;
        assert_eq!(summary.total_requests, 1);
        assert_eq!(summary.total_cache_hits, 1);
        assert_eq!(summary.total_upstream_errors, 1);
    }

    #[test]
    fn test_metrics_summary() {
        let summary = MetricsSummary {
            total_requests: 100,
            total_cache_hits: 50,
            total_upstream_errors: 5,
            cache_hit_rate: 0.5,
            average_latency_ms: 150.0,
            requests_by_method: std::collections::HashMap::new(),
            cache_hits_by_method: std::collections::HashMap::new(),
            errors_by_upstream: std::collections::HashMap::new(),
            rate_limit_rejections: 10,
            auth_failures: 2,
            validation_failures: 3,
            active_connections: 25,
            websocket_connections: 5,
            reorg_detections: 1,
        };
        assert_eq!(summary.cache_hit_rate, 0.5);
        assert_eq!(summary.average_latency_ms, 150.0);
        assert_eq!(summary.rate_limit_rejections, 10);
        assert_eq!(summary.auth_failures, 2);
    }

    // method_to_static Tests

    #[test]
    fn test_method_to_static_common_methods() {
        // Common methods should return Borrowed (static strings)
        let methods = [
            "net_version",
            "eth_blockNumber",
            "eth_chainId",
            "eth_gasPrice",
            "eth_getBalance",
            "eth_getBlockByHash",
            "eth_getBlockByNumber",
            "eth_getLogs",
            "eth_getTransactionByHash",
            "eth_getTransactionReceipt",
            "eth_getCode",
            "eth_call",
            "eth_estimateGas",
            "eth_getTransactionCount",
        ];

        for method in &methods {
            let cow = method_to_static(method);
            assert!(
                matches!(cow, Cow::Borrowed(_)),
                "Method {method} should return Borrowed, got Owned"
            );
        }
    }

    #[test]
    fn test_method_to_static_unknown_method() {
        let cow = method_to_static("custom_unknown_method");
        assert!(matches!(cow, Cow::Owned(_)));
    }

    #[test]
    fn test_method_to_static_values() {
        assert_eq!(method_to_static("eth_blockNumber").as_ref(), "eth_blockNumber");
        assert_eq!(method_to_static("eth_getLogs").as_ref(), "eth_getLogs");
        assert_eq!(method_to_static("custom_method").as_ref(), "custom_method");
    }

    // upstream_to_static Tests

    #[test]
    fn test_upstream_to_static_interning() {
        let name1 = upstream_to_static("upstream-1");
        let name2 = upstream_to_static("upstream-1");

        // Both should be borrowed (interned)
        assert!(matches!(name1, Cow::Borrowed(_)));
        assert!(matches!(name2, Cow::Borrowed(_)));

        // Values should be equal
        assert_eq!(name1.as_ref(), "upstream-1");
        assert_eq!(name2.as_ref(), "upstream-1");
    }

    #[test]
    fn test_upstream_to_static_different_names() {
        let name1 = upstream_to_static("upstream-a");
        let name2 = upstream_to_static("upstream-b");

        assert_eq!(name1.as_ref(), "upstream-a");
        assert_eq!(name2.as_ref(), "upstream-b");
    }

    // MetricsState Trait Implementation Tests

    #[test]
    fn test_circuit_breaker_state_metrics() {
        assert_eq!(CircuitBreakerState::Closed.as_metric_str(), "closed");
        assert_eq!(CircuitBreakerState::Closed.as_gauge_value(), 0.0);

        assert_eq!(CircuitBreakerState::Open.as_metric_str(), "open");
        assert_eq!(CircuitBreakerState::Open.as_gauge_value(), 1.0);

        assert_eq!(CircuitBreakerState::HalfOpen.as_metric_str(), "half_open");
        assert_eq!(CircuitBreakerState::HalfOpen.as_gauge_value(), 0.5);
    }

    #[test]
    fn test_upstream_error_metrics() {
        // Test various error types
        assert_eq!(UpstreamError::Timeout.as_metric_str(), "timeout");
        assert_eq!(UpstreamError::Timeout.as_gauge_value(), 0.5);

        assert_eq!(
            UpstreamError::ConnectionFailed("test".to_string()).as_metric_str(),
            "connection_failed"
        );
        assert_eq!(UpstreamError::ConnectionFailed("test".to_string()).as_gauge_value(), 1.0);

        assert_eq!(UpstreamError::NoHealthyUpstreams.as_metric_str(), "no_healthy_upstreams");
        assert_eq!(UpstreamError::NoHealthyUpstreams.as_gauge_value(), 1.0);

        assert_eq!(UpstreamError::CircuitBreakerOpen.as_metric_str(), "circuit_breaker_open");
        assert_eq!(UpstreamError::CircuitBreakerOpen.as_gauge_value(), 1.0);
    }

    #[test]
    fn test_validation_error_metrics() {
        assert_eq!(
            ValidationError::InvalidMethod("test".to_string()).as_metric_str(),
            "invalid_method"
        );
        assert_eq!(ValidationError::InvalidMethod("test".to_string()).as_gauge_value(), 0.5);

        assert_eq!(
            ValidationError::BlockRangeTooLarge(10001).as_metric_str(),
            "block_range_too_large"
        );
        assert_eq!(ValidationError::BlockRangeTooLarge(10001).as_gauge_value(), 0.5);

        assert_eq!(ValidationError::InvalidBlockHash.as_metric_str(), "invalid_block_hash");
    }

    #[test]
    fn test_auth_error_metrics() {
        assert_eq!(AuthError::InvalidApiKey.as_metric_str(), "invalid_api_key");
        assert_eq!(AuthError::InvalidApiKey.as_gauge_value(), 0.5);

        assert_eq!(AuthError::ExpiredApiKey.as_metric_str(), "expired_api_key");
        assert_eq!(AuthError::RateLimitExceeded.as_metric_str(), "rate_limit_exceeded");
        assert_eq!(AuthError::QuotaExceeded.as_metric_str(), "quota_exceeded");

        // System errors should have 1.0 severity
        assert_eq!(AuthError::DatabaseError("test".to_string()).as_metric_str(), "database_error");
        assert_eq!(AuthError::DatabaseError("test".to_string()).as_gauge_value(), 1.0);
    }

    #[test]
    fn test_proxy_error_metrics() {
        assert_eq!(
            ProxyError::InvalidRequest("test".to_string()).as_metric_str(),
            "invalid_request"
        );
        assert_eq!(ProxyError::InvalidRequest("test".to_string()).as_gauge_value(), 0.5);

        assert_eq!(ProxyError::RateLimited.as_metric_str(), "rate_limited");
        assert_eq!(ProxyError::RateLimited.as_gauge_value(), 0.5);

        assert_eq!(ProxyError::Internal("test".to_string()).as_metric_str(), "internal_error");
        assert_eq!(ProxyError::Internal("test".to_string()).as_gauge_value(), 1.0);
    }

    // MetricsCollector Recording Tests

    #[tokio::test]
    async fn test_record_multiple_requests() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_request("eth_getLogs", "upstream-1", true, 100).await;
        collector.record_request("eth_getLogs", "upstream-1", true, 200).await;
        collector.record_request("eth_getLogs", "upstream-2", false, 300).await;

        let summary = collector.get_metrics_summary().await;
        assert_eq!(summary.total_requests, 3);
    }

    #[tokio::test]
    async fn test_record_cache_hit_miss() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_cache_hit("eth_getBlockByNumber");
        collector.record_cache_hit("eth_getBlockByNumber");
        collector.record_cache_miss("eth_getLogs");

        let summary = collector.get_metrics_summary().await;
        assert_eq!(summary.total_cache_hits, 2);
    }

    #[tokio::test]
    async fn test_record_upstream_error() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_upstream_error("upstream-1", "timeout").await;
        collector.record_upstream_error("upstream-1", "connection_failed").await;
        collector.record_upstream_error("upstream-2", "timeout").await;

        let summary = collector.get_metrics_summary().await;
        assert_eq!(summary.total_upstream_errors, 3);
    }

    #[tokio::test]
    async fn test_reset_metrics() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_request("eth_getLogs", "upstream-1", true, 100).await;
        collector.record_cache_hit("eth_getLogs");

        let summary_before = collector.get_metrics_summary().await;
        assert_eq!(summary_before.total_requests, 1);
        assert_eq!(summary_before.total_cache_hits, 1);

        collector.reset_metrics().await;

        let summary_after = collector.get_metrics_summary().await;
        assert_eq!(summary_after.total_requests, 0);
        assert_eq!(summary_after.total_cache_hits, 0);
    }

    #[tokio::test]
    async fn test_get_prometheus_metrics() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_request("eth_blockNumber", "test-upstream", true, 50).await;

        let metrics = collector.get_prometheus_metrics();

        // Should return some text (Prometheus format)
        assert!(!metrics.is_empty());
    }

    #[tokio::test]
    async fn test_get_internal_metrics() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_request("eth_getLogs", "upstream-1", true, 100).await;

        let internal = collector.get_internal_metrics().await;

        // Should have the recorded data
        assert!(internal.requests_total.contains_key("eth_getLogs"));
    }

    // Specialized Recording Tests

    #[test]
    fn test_record_upstream_health() {
        let collector = MetricsCollector::new().unwrap();

        // Just verify it doesn't panic
        collector.record_upstream_health("upstream-1", true);
        collector.record_upstream_health("upstream-1", false);
    }

    #[test]
    fn test_record_active_connections() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_active_connections(10);
        collector.record_active_connections(5);
    }

    #[test]
    fn test_record_cache_stats() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_cache_stats("block", 1000, 0.85);
        collector.record_cache_stats("log", 5000, 0.72);
    }

    #[test]
    fn test_record_rate_limit() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_rate_limit("127.0.0.1", true, 9.0);
        collector.record_rate_limit("127.0.0.1", false, 0.0);
    }

    #[test]
    fn test_record_auth_attempt() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_auth_attempt(true, Some("key-123"));
        collector.record_auth_attempt(false, None);
    }

    #[test]
    fn test_record_validation() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_validation("eth_getLogs", true, None);
        collector.record_validation("eth_getLogs", false, Some("block_range_too_large"));
    }

    #[test]
    fn test_record_websocket_connection() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_websocket_connection(true);
        collector.record_websocket_connection(false);
    }

    #[test]
    fn test_record_reorg() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_reorg(2, 12_345_678);
    }

    #[test]
    fn test_record_partial_cache_fulfillment() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_partial_cache_fulfillment("eth_getLogs", 0.75);
    }

    #[test]
    fn test_record_circuit_breaker_state() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_circuit_breaker_state("upstream-1", CircuitBreakerState::Closed, 0);
        collector.record_circuit_breaker_state("upstream-1", CircuitBreakerState::Open, 5);
        collector.record_circuit_breaker_state("upstream-1", CircuitBreakerState::HalfOpen, 3);
    }

    #[test]
    fn test_record_health_check() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_health_check("upstream-1", true, 50);
        collector.record_health_check("upstream-1", false, 5000);
    }

    #[test]
    fn test_record_upstream_score() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_upstream_score("upstream-1", 85.5, 0.9, 0.95, 1.0, 0.98);
    }

    #[test]
    fn test_record_block_cache_stats() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_block_cache_stats(100, 500, 1024 * 1024);
    }

    #[test]
    fn test_record_log_cache_stats() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_log_cache_stats(50, 10000, 500, 2 * 1024 * 1024);
    }

    #[test]
    fn test_record_transaction_cache_stats() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_transaction_cache_stats(1000, 500, 512 * 1024);
    }

    #[test]
    fn test_record_hedged_request() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_hedged_request("upstream-1", "upstream-2");
    }

    #[test]
    fn test_record_hedge_winner() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_hedge_winner("upstream-1", true);
        collector.record_hedge_winner("upstream-2", false);
    }

    #[test]
    fn test_record_auth_cache() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_auth_cache(true);
        collector.record_auth_cache(false);
    }

    #[test]
    fn test_record_batch_request() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_batch_request(5, 250);
    }

    // Typed Error Recording Tests

    #[test]
    fn test_record_upstream_error_typed() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_upstream_error_typed("upstream-1", &UpstreamError::Timeout);
        collector.record_upstream_error_typed(
            "upstream-1",
            &UpstreamError::ConnectionFailed("test".to_string()),
        );
    }

    #[test]
    fn test_record_validation_error() {
        let collector = MetricsCollector::new().unwrap();

        collector
            .record_validation_error("eth_getLogs", &ValidationError::BlockRangeTooLarge(10001));
    }

    #[test]
    fn test_record_auth_error() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_auth_error(&AuthError::InvalidApiKey, Some("key-123"));
        collector.record_auth_error(&AuthError::DatabaseError("db error".to_string()), None);
    }

    #[test]
    fn test_record_proxy_error() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_proxy_error("eth_getLogs", &ProxyError::RateLimited);
        collector
            .record_proxy_error("eth_call", &ProxyError::Internal("internal error".to_string()));
    }

    // MetricsMiddleware Tests

    #[tokio::test]
    async fn test_metrics_middleware_success() {
        let collector = Arc::new(MetricsCollector::new().unwrap());
        let middleware = MetricsMiddleware::new(collector.clone());

        let result: Result<String, String> = middleware
            .track_request("eth_blockNumber", "upstream-1", async { Ok("success".to_string()) })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");

        let summary = collector.get_metrics_summary().await;
        assert_eq!(summary.total_requests, 1);
    }

    #[tokio::test]
    async fn test_metrics_middleware_failure() {
        let collector = Arc::new(MetricsCollector::new().unwrap());
        let middleware = MetricsMiddleware::new(collector.clone());

        let result: Result<String, String> = middleware
            .track_request("eth_blockNumber", "upstream-1", async { Err("error".to_string()) })
            .await;

        assert!(result.is_err());

        let summary = collector.get_metrics_summary().await;
        assert_eq!(summary.total_requests, 1);
    }

    // init_metrics Tests

    #[test]
    fn test_init_metrics() {
        let result = init_metrics();
        assert!(result.is_ok());
    }

    // Concurrent Access Tests

    #[tokio::test]
    async fn test_concurrent_metric_recording() {
        let collector = Arc::new(MetricsCollector::new().unwrap());
        let mut handles = vec![];

        // Spawn multiple tasks recording metrics concurrently
        for i in 0..10 {
            let collector_clone = collector.clone();
            let upstream = format!("upstream-{}", i % 3);
            handles.push(tokio::spawn(async move {
                collector_clone.record_request("eth_blockNumber", &upstream, true, 100).await;
                collector_clone.record_cache_hit("eth_blockNumber");
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        let summary = collector.get_metrics_summary().await;
        assert_eq!(summary.total_requests, 10);
        assert_eq!(summary.total_cache_hits, 10);
    }

    // Average Latency Calculation Tests

    #[tokio::test]
    async fn test_average_latency_calculation() {
        let collector = MetricsCollector::new().unwrap();

        collector.record_request("eth_getLogs", "upstream-1", true, 100).await;
        collector.record_request("eth_getLogs", "upstream-1", true, 200).await;
        collector.record_request("eth_getLogs", "upstream-1", true, 300).await;

        let summary = collector.get_metrics_summary().await;
        // Average should be (100 + 200 + 300) / 3 = 200
        assert_eq!(summary.average_latency_ms, 200.0);
    }

    #[tokio::test]
    async fn test_cache_hit_rate_calculation() {
        let collector = MetricsCollector::new().unwrap();

        // Record some requests first
        collector.record_request("eth_getLogs", "upstream-1", true, 100).await;
        collector.record_request("eth_getLogs", "upstream-1", true, 100).await;

        // Then cache hits
        collector.record_cache_hit("eth_getLogs");

        let summary = collector.get_metrics_summary().await;
        // 1 cache hit / 2 requests = 0.5
        assert_eq!(summary.cache_hit_rate, 0.5);
    }
}
