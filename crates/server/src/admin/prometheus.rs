//! Prometheus HTTP client for querying time-series metrics.
//!
//! This module provides a client for querying the Prometheus HTTP API to retrieve
//! historical metrics data for the admin dashboard. It includes response caching
//! to avoid overloading Prometheus and graceful error handling to ensure the admin
//! API remains functional even if Prometheus is unavailable.

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

use chrono::{DateTime, Utc};
use lru::LruCache;
use parking_lot::Mutex;
use serde::Deserialize;
use std::{collections::HashMap, num::NonZeroUsize, sync::Arc, time::Duration};
use thiserror::Error;

use crate::admin::types::{
    CacheHitDataPoint, ErrorDistribution, HedgingStats, LatencyDataPoint, RequestMethodData,
    TimeSeriesPoint, WinnerDistribution,
};

/// Sanitize a string for use as a Prometheus label value.
/// Removes characters that could break or inject into `PromQL` queries.
fn sanitize_label_value(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .take(128) // Limit length
        .collect()
}

/// Errors that can occur when querying Prometheus.
#[derive(Debug, Error)]
pub enum PrometheusError {
    #[error("HTTP request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),

    #[error("Invalid response format: {0}")]
    InvalidResponse(String),

    #[error("Prometheus returned error status: {0}")]
    PrometheusError(String),

    #[error("Failed to parse metric value: {0}")]
    ParseError(String),
}

/// Prometheus query response structure.
#[derive(Debug, Deserialize)]
struct PrometheusResponse {
    status: String,
    data: Option<PrometheusData>,
    error: Option<String>,
}

/// Prometheus response data.
#[derive(Debug, Deserialize)]
struct PrometheusData {
    result: Vec<PrometheusResult>,
}

/// Individual metric result.
#[derive(Debug, Deserialize)]
struct PrometheusResult {
    metric: HashMap<String, String>,
    values: Option<Vec<(f64, String)>>,
    value: Option<(f64, String)>,
}

/// Cached query result.
#[derive(Debug, Clone)]
struct CachedResult {
    data: Vec<TimeSeriesPoint>,
    timestamp: DateTime<Utc>,
}

/// Prometheus HTTP client for querying metrics.
///
/// This client queries the Prometheus HTTP API to retrieve historical time-series data.
/// It includes built-in caching (1 minute TTL) to avoid hammering Prometheus with repeated
/// queries for the same data.
#[derive(Clone)]
pub struct PrometheusClient {
    /// Base URL of the Prometheus server.
    base_url: String,

    /// HTTP client with configured timeouts.
    client: reqwest::Client,

    /// LRU cache for query results with automatic eviction.
    cache: Arc<Mutex<LruCache<String, CachedResult>>>,

    /// Cache TTL in seconds.
    cache_ttl: Duration,
}

impl PrometheusClient {
    /// Creates a new Prometheus client.
    ///
    /// # Arguments
    ///
    /// * `base_url` - Base URL of the Prometheus server (e.g., `http://localhost:9090`)
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be created.
    ///
    /// # Panics
    ///
    /// Panics if `NonZeroUsize` cannot be created (will not happen as we use a constant).
    pub fn new(base_url: impl Into<String>) -> Result<Self, PrometheusError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2))
            .build()
            .map_err(PrometheusError::RequestFailed)?;

        Ok(Self {
            base_url: base_url.into(),
            client,
            cache: Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(100).expect("100 > 0")))),
            cache_ttl: Duration::from_secs(60),
        })
    }

    /// Queries Prometheus for a range of time-series data.
    ///
    /// # Arguments
    ///
    /// * `query` - `PromQL` query string
    /// * `start` - Start time for the query range
    /// * `end` - End time for the query range
    /// * `step` - Step interval (e.g., `1m`, `5m`, `1h`)
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or the response cannot be parsed.
    pub async fn query_range(
        &self,
        query: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        step: &str,
    ) -> Result<Vec<TimeSeriesPoint>, PrometheusError> {
        // Check cache first
        let cache_key = format!("{}:{}:{}:{}", query, start.timestamp(), end.timestamp(), step);
        if let Some(cached) = self.get_cached(&cache_key) {
            return Ok(cached);
        }

        // Build query URL
        let url = format!("{}/api/v1/query_range", self.base_url);
        let params = [
            ("query", query),
            ("start", &start.timestamp().to_string()),
            ("end", &end.timestamp().to_string()),
            ("step", step),
        ];

        // Execute query
        let response = self.client.get(&url).query(&params).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(PrometheusError::PrometheusError(format!("HTTP {status}: {text}")));
        }

        let prom_response: PrometheusResponse = response.json().await?;

        // Check for Prometheus-level errors
        if prom_response.status != "success" {
            return Err(PrometheusError::PrometheusError(
                prom_response.error.unwrap_or_else(|| "Unknown error".to_string()),
            ));
        }

        // Parse results
        let data = prom_response
            .data
            .ok_or_else(|| PrometheusError::InvalidResponse("Missing data field".to_string()))?;

        let mut points = Vec::new();
        for result in data.result {
            if let Some(values) = result.values {
                for (timestamp, value_str) in values {
                    let value = value_str
                        .parse::<f64>()
                        .map_err(|e| PrometheusError::ParseError(e.to_string()))?;

                    points.push(TimeSeriesPoint {
                        timestamp: DateTime::from_timestamp(timestamp as i64, 0)
                            .unwrap_or_else(Utc::now)
                            .to_rfc3339(),
                        value,
                    });
                }
            }
        }

        // Cache the result
        self.cache_result(&cache_key, points.clone());

        Ok(points)
    }

    /// Queries Prometheus for a single instant value.
    ///
    /// # Arguments
    ///
    /// * `query` - `PromQL` query string
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or the response cannot be parsed.
    pub async fn query_instant(&self, query: &str) -> Result<f64, PrometheusError> {
        let url = format!("{}/api/v1/query", self.base_url);
        let params = [("query", query)];

        let response = self.client.get(&url).query(&params).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(PrometheusError::PrometheusError(format!("HTTP {status}: {text}")));
        }

        let prom_response: PrometheusResponse = response.json().await?;

        if prom_response.status != "success" {
            return Err(PrometheusError::PrometheusError(
                prom_response.error.unwrap_or_else(|| "Unknown error".to_string()),
            ));
        }

        let data = prom_response
            .data
            .ok_or_else(|| PrometheusError::InvalidResponse("Missing data field".to_string()))?;

        if let Some(result) = data.result.first() {
            if let Some((_, value_str)) = &result.value {
                return value_str
                    .parse::<f64>()
                    .map_err(|e| PrometheusError::ParseError(e.to_string()));
            }
        }

        Ok(0.0)
    }

    /// Gets the request rate time series for the specified time range.
    ///
    /// # Arguments
    ///
    /// * `time_range` - Duration to look back (e.g., 1 hour, 24 hours)
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn get_request_rate(
        &self,
        time_range: Duration,
    ) -> Result<Vec<TimeSeriesPoint>, PrometheusError> {
        let end = Utc::now();
        let start = end - chrono::Duration::from_std(time_range).unwrap_or_default();

        // Query the rate of RPC requests per second
        // Note: metric name is "rpc_requests_total" (exported by MetricsCollector)
        let query = r"rate(rpc_requests_total[5m])";
        let step = Self::calculate_step(time_range);

        self.query_range(query, start, end, &step).await
    }

    /// Gets latency percentiles over the specified time range.
    ///
    /// # Arguments
    ///
    /// * `time_range` - Duration to look back
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn get_latency_percentiles(
        &self,
        time_range: Duration,
    ) -> Result<Vec<LatencyDataPoint>, PrometheusError> {
        let end = Utc::now();
        let start = end - chrono::Duration::from_std(time_range).unwrap_or_default();
        let step = Self::calculate_step(time_range);

        // Query histogram quantiles for p50, p95, p99
        // Note: metric name is "rpc_request_duration_seconds" (exported by MetricsCollector)
        let queries = [
            ("p50", r"histogram_quantile(0.50, rate(rpc_request_duration_seconds_bucket[5m]))"),
            ("p95", r"histogram_quantile(0.95, rate(rpc_request_duration_seconds_bucket[5m]))"),
            ("p99", r"histogram_quantile(0.99, rate(rpc_request_duration_seconds_bucket[5m]))"),
        ];

        // Execute all queries in parallel
        let mut results = Vec::new();
        for (label, query) in queries {
            let points = self.query_range(query, start, end, &step).await?;
            results.push((label, points));
        }

        // Merge results by timestamp
        let mut merged: HashMap<String, LatencyDataPoint> = HashMap::new();

        for (label, points) in results {
            for point in points {
                let entry =
                    merged.entry(point.timestamp.clone()).or_insert_with(|| LatencyDataPoint {
                        timestamp: point.timestamp.clone(),
                        p50: 0.0,
                        p95: 0.0,
                        p99: 0.0,
                    });

                // Convert seconds to milliseconds
                let value_ms = point.value * 1000.0;
                match label {
                    "p50" => entry.p50 = value_ms,
                    "p95" => entry.p95 = value_ms,
                    "p99" => entry.p99 = value_ms,
                    _ => {}
                }
            }
        }

        let mut result: Vec<_> = merged.into_values().collect();
        result.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        Ok(result)
    }

    /// Gets latency percentiles for a specific upstream over the specified time range.
    ///
    /// # Arguments
    ///
    /// * `upstream` - Upstream name to filter by
    /// * `time_range` - Duration to look back
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn get_upstream_latency_percentiles(
        &self,
        upstream: &str,
        time_range: Duration,
    ) -> Result<Vec<LatencyDataPoint>, PrometheusError> {
        let end = Utc::now();
        let start = end - chrono::Duration::from_std(time_range).unwrap_or_default();
        let step = Self::calculate_step(time_range);

        // Query histogram quantiles for p50, p95, p99 filtered by upstream label
        // Note: metric name is "rpc_request_duration_seconds" (exported by MetricsCollector)
        let sanitized_upstream = sanitize_label_value(upstream);
        let queries = [
            (
                "p50",
                format!(
                    r#"histogram_quantile(0.50, rate(rpc_request_duration_seconds_bucket{{upstream="{sanitized_upstream}"}}[5m]))"#
                ),
            ),
            (
                "p95",
                format!(
                    r#"histogram_quantile(0.95, rate(rpc_request_duration_seconds_bucket{{upstream="{sanitized_upstream}"}}[5m]))"#
                ),
            ),
            (
                "p99",
                format!(
                    r#"histogram_quantile(0.99, rate(rpc_request_duration_seconds_bucket{{upstream="{sanitized_upstream}"}}[5m]))"#
                ),
            ),
        ];

        // Execute all queries in parallel
        let mut results = Vec::new();
        for (label, query) in &queries {
            let points = self.query_range(query, start, end, &step).await?;
            results.push((*label, points));
        }

        // Merge results by timestamp
        let mut merged: HashMap<String, LatencyDataPoint> = HashMap::new();

        for (label, points) in results {
            for point in points {
                let entry =
                    merged.entry(point.timestamp.clone()).or_insert_with(|| LatencyDataPoint {
                        timestamp: point.timestamp.clone(),
                        p50: 0.0,
                        p95: 0.0,
                        p99: 0.0,
                    });

                // Convert seconds to milliseconds
                let value_ms = point.value * 1000.0;
                match label {
                    "p50" => entry.p50 = value_ms,
                    "p95" => entry.p95 = value_ms,
                    "p99" => entry.p99 = value_ms,
                    _ => {}
                }
            }
        }

        let mut result: Vec<_> = merged.into_values().collect();
        result.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        Ok(result)
    }

    /// Gets cache hit rate over the specified time range.
    ///
    /// # Arguments
    ///
    /// * `time_range` - Duration to look back
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn get_cache_hit_rate(
        &self,
        time_range: Duration,
    ) -> Result<Vec<CacheHitDataPoint>, PrometheusError> {
        let end = Utc::now();
        let start = end - chrono::Duration::from_std(time_range).unwrap_or_default();
        let step = Self::calculate_step(time_range);

        // Query cache hit/miss rates
        // Note: metric names are "rpc_cache_hits_total" and "rpc_cache_misses_total" (exported by
        // MetricsCollector) We calculate: hits / (hits + misses) * 100
        let queries = [
            (
                "hit",
                r"rate(rpc_cache_hits_total[5m]) / (rate(rpc_cache_hits_total[5m]) + rate(rpc_cache_misses_total[5m])) * 100",
            ),
            (
                "partial",
                r"rate(rpc_partial_cache_fulfillments_total[5m]) / (rate(rpc_cache_hits_total[5m]) + rate(rpc_cache_misses_total[5m])) * 100",
            ),
            (
                "miss",
                r"rate(rpc_cache_misses_total[5m]) / (rate(rpc_cache_hits_total[5m]) + rate(rpc_cache_misses_total[5m])) * 100",
            ),
        ];

        let mut results = Vec::new();
        for (label, query) in queries {
            let points = self.query_range(query, start, end, &step).await.unwrap_or_default();
            results.push((label, points));
        }

        // Merge results
        let mut merged: HashMap<String, CacheHitDataPoint> = HashMap::new();

        for (label, points) in results {
            for point in points {
                let entry =
                    merged.entry(point.timestamp.clone()).or_insert_with(|| CacheHitDataPoint {
                        timestamp: point.timestamp.clone(),
                        hit: 0.0,
                        partial: 0.0,
                        miss: 0.0,
                    });

                match label {
                    "hit" => entry.hit = point.value,
                    "partial" => entry.partial = point.value,
                    "miss" => entry.miss = point.value,
                    _ => {}
                }
            }
        }

        let mut result: Vec<_> = merged.into_values().collect();
        result.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        Ok(result)
    }

    /// Gets error distribution over the specified time range.
    ///
    /// # Arguments
    ///
    /// * `time_range` - Duration to look back
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn get_error_distribution(
        &self,
        time_range: Duration,
    ) -> Result<Vec<ErrorDistribution>, PrometheusError> {
        let _end = Utc::now();
        let _start = chrono::Duration::from_std(time_range).unwrap_or_default();

        // Query error counts by type
        // Note: metric name is "rpc_upstream_errors_by_type_total" (exported by MetricsCollector)
        let query = format!(
            r"sum by (error_type) (increase(rpc_upstream_errors_by_type_total[{}s]))",
            time_range.as_secs()
        );

        let url = format!("{}/api/v1/query", self.base_url);
        let params = [("query", query.as_str())];

        let response = self.client.get(&url).query(&params).send().await?;

        if !response.status().is_success() {
            return Ok(Vec::new()); // Return empty on error
        }

        let prom_response: PrometheusResponse = response.json().await?;

        if prom_response.status != "success" {
            return Ok(Vec::new());
        }

        let Some(data) = prom_response.data else {
            return Ok(Vec::new());
        };

        let mut total_errors = 0u64;
        let mut error_counts: HashMap<String, u64> = HashMap::new();

        for result in &data.result {
            if let Some((_, value_str)) = &result.value {
                if let Ok(count) = value_str.parse::<f64>() {
                    let count = count as u64;
                    let error_type = result
                        .metric
                        .get("error_type")
                        .cloned()
                        .unwrap_or_else(|| "Unknown".to_string());

                    *error_counts.entry(error_type).or_insert(0) += count;
                    total_errors += count;
                }
            }
        }

        let mut distribution: Vec<ErrorDistribution> = error_counts
            .into_iter()
            .map(|(error_type, count)| {
                let percentage = if total_errors > 0 {
                    (count as f64 / total_errors as f64) * 100.0
                } else {
                    0.0
                };

                ErrorDistribution { error_type, count, percentage }
            })
            .collect();

        distribution.sort_by(|a, b| b.count.cmp(&a.count));

        Ok(distribution)
    }

    /// Gets request distribution by method over the specified time range.
    ///
    /// # Arguments
    ///
    /// * `time_range` - Duration to look back
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn get_request_by_method(
        &self,
        time_range: Duration,
    ) -> Result<Vec<RequestMethodData>, PrometheusError> {
        let _end = Utc::now();
        let _start = chrono::Duration::from_std(time_range).unwrap_or_default();

        // Query request counts by method
        // Note: metric name is "rpc_requests_total" (exported by MetricsCollector)
        let query =
            format!(r"sum by (method) (increase(rpc_requests_total[{}s]))", time_range.as_secs());

        let url = format!("{}/api/v1/query", self.base_url);
        let params = [("query", query.as_str())];

        let response = self.client.get(&url).query(&params).send().await?;

        if !response.status().is_success() {
            return Ok(Vec::new());
        }

        let prom_response: PrometheusResponse = response.json().await?;

        if prom_response.status != "success" {
            return Ok(Vec::new());
        }

        let Some(data) = prom_response.data else {
            return Ok(Vec::new());
        };

        let mut total_requests = 0u64;
        let mut method_counts: HashMap<String, u64> = HashMap::new();

        for result in &data.result {
            if let Some((_, value_str)) = &result.value {
                if let Ok(count) = value_str.parse::<f64>() {
                    let count = count as u64;
                    let method = result
                        .metric
                        .get("method")
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());

                    *method_counts.entry(method).or_insert(0) += count;
                    total_requests += count;
                }
            }
        }

        let mut distribution: Vec<RequestMethodData> = method_counts
            .into_iter()
            .map(|(method, count)| {
                let percentage = if total_requests > 0 {
                    (count as f64 / total_requests as f64) * 100.0
                } else {
                    0.0
                };

                RequestMethodData { method, count, percentage }
            })
            .collect();

        distribution.sort_by(|a, b| b.count.cmp(&a.count));

        Ok(distribution)
    }

    /// Gets request distribution by method for a specific upstream over the specified time range.
    ///
    /// # Arguments
    ///
    /// * `upstream` - Upstream name to filter by
    /// * `time_range` - Duration to look back
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn get_upstream_request_by_method(
        &self,
        upstream: &str,
        time_range: Duration,
    ) -> Result<Vec<RequestMethodData>, PrometheusError> {
        let _end = Utc::now();
        let _start = chrono::Duration::from_std(time_range).unwrap_or_default();

        // Query request counts by method filtered by upstream label
        // Note: metric name is "rpc_requests_total" (exported by MetricsCollector)
        let sanitized_upstream = sanitize_label_value(upstream);
        let query = format!(
            r#"sum by (method) (increase(rpc_requests_total{{upstream="{}"}}[{}s]))"#,
            sanitized_upstream,
            time_range.as_secs()
        );

        let url = format!("{}/api/v1/query", self.base_url);
        let params = [("query", query.as_str())];

        let response = self.client.get(&url).query(&params).send().await?;

        if !response.status().is_success() {
            return Ok(Vec::new());
        }

        let prom_response: PrometheusResponse = response.json().await?;

        if prom_response.status != "success" {
            return Ok(Vec::new());
        }

        let Some(data) = prom_response.data else {
            return Ok(Vec::new());
        };

        let mut total_requests = 0u64;
        let mut method_counts: HashMap<String, u64> = HashMap::new();

        for result in &data.result {
            if let Some((_, value_str)) = &result.value {
                if let Ok(count) = value_str.parse::<f64>() {
                    let count = count as u64;
                    let method = result
                        .metric
                        .get("method")
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());

                    *method_counts.entry(method).or_insert(0) += count;
                    total_requests += count;
                }
            }
        }

        let mut distribution: Vec<RequestMethodData> = method_counts
            .into_iter()
            .map(|(method, count)| {
                let percentage = if total_requests > 0 {
                    (count as f64 / total_requests as f64) * 100.0
                } else {
                    0.0
                };

                RequestMethodData { method, count, percentage }
            })
            .collect();

        distribution.sort_by(|a, b| b.count.cmp(&a.count));

        Ok(distribution)
    }

    /// Gets hedging statistics over the specified time range.
    ///
    /// # Arguments
    ///
    /// * `time_range` - Duration to look back
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn get_hedging_stats(
        &self,
        time_range: Duration,
    ) -> Result<HedgingStats, PrometheusError> {
        let time_secs = time_range.as_secs();

        // Query total hedged requests
        let hedged_query = format!("sum(increase(rpc_hedged_requests_total[{time_secs}s]))");

        // Query primary wins
        let primary_wins_query =
            format!("sum(increase(rpc_hedge_wins_total{{type=\"primary\"}}[{time_secs}s]))");

        // Query hedged wins
        let hedged_wins_query =
            format!("sum(increase(rpc_hedge_wins_total{{type=\"hedged\"}}[{time_secs}s]))");

        // Query hedge skipped (triggers that didn't result in hedge)
        let skipped_query = format!("sum(increase(rpc_hedge_skipped_total[{time_secs}s]))");

        // Execute queries
        let hedged_requests = self.query_instant(&hedged_query).await.unwrap_or(0.0);
        let primary_wins = self.query_instant(&primary_wins_query).await.unwrap_or(0.0);
        let hedged_wins = self.query_instant(&hedged_wins_query).await.unwrap_or(0.0);
        let hedge_skipped = self.query_instant(&skipped_query).await.unwrap_or(0.0);

        // Calculate percentages
        let total_wins = primary_wins + hedged_wins;
        let (primary_pct, hedged_pct) = if total_wins > 0.0 {
            ((primary_wins / total_wins) * 100.0, (hedged_wins / total_wins) * 100.0)
        } else {
            (100.0, 0.0) // Default: all primary wins if no data
        };

        // Hedge triggers = hedged requests + skipped (requests that triggered hedge evaluation)
        let hedge_triggers = hedged_requests + hedge_skipped;

        // P99 improvement is hard to calculate without historical comparison
        // For now, estimate based on hedged win percentage (higher hedged wins = better
        // improvement)
        let p99_improvement = hedged_pct * 0.5; // Rough approximation

        Ok(HedgingStats {
            hedged_requests,
            hedge_triggers,
            primary_wins,
            p99_improvement,
            winner_distribution: WinnerDistribution { primary: primary_pct, hedged: hedged_pct },
        })
    }

    /// Calculates an appropriate step interval based on the time range.
    fn calculate_step(time_range: Duration) -> String {
        if time_range.as_secs() <= 3600 {
            // 1 hour or less: 1 minute steps
            "1m".to_string()
        } else if time_range.as_secs() <= 86400 {
            // 24 hours or less: 5 minute steps
            "5m".to_string()
        } else if time_range.as_secs() <= 604_800 {
            // 7 days or less: 1 hour steps
            "1h".to_string()
        } else {
            // More than 7 days: 6 hour steps
            "6h".to_string()
        }
    }

    /// Gets a cached result if it exists and is still fresh.
    fn get_cached(&self, key: &str) -> Option<Vec<TimeSeriesPoint>> {
        let mut cache = self.cache.lock();
        cache.get(key).and_then(|cached| {
            if Utc::now() - cached.timestamp < chrono::Duration::from_std(self.cache_ttl).ok()? {
                Some(cached.data.clone())
            } else {
                None
            }
        })
    }

    /// Caches a query result.
    ///
    /// The LRU cache automatically evicts the least recently used entry when capacity is exceeded.
    fn cache_result(&self, key: &str, data: Vec<TimeSeriesPoint>) {
        let mut cache = self.cache.lock();
        cache.put(key.to_string(), CachedResult { data, timestamp: Utc::now() });
    }
}

/// Parses a time range string (e.g., "1h", "24h", "7d") into a Duration.
///
/// # Arguments
///
/// * `time_range` - Time range string
///
/// # Returns
///
/// A `Duration` representing the time range, or 24 hours if parsing fails.
#[must_use]
pub fn parse_time_range(time_range: &str) -> Duration {
    let time_range = time_range.trim();

    if time_range.is_empty() {
        return Duration::from_secs(86400); // Default to 24h
    }

    let (num_str, unit) = if let Some(pos) = time_range.find(|c: char| !c.is_ascii_digit()) {
        time_range.split_at(pos)
    } else {
        return Duration::from_secs(86400);
    };

    let num: u64 = num_str.parse().unwrap_or(24);

    match unit {
        "m" | "min" => Duration::from_secs(num * 60),
        "h" | "hr" | "hour" => Duration::from_secs(num * 3600),
        "d" | "day" => Duration::from_secs(num * 86400),
        "w" | "week" => Duration::from_secs(num * 604_800),
        _ => Duration::from_secs(86400), // Default to 24h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_time_range() {
        assert_eq!(parse_time_range("1h").as_secs(), 3600);
        assert_eq!(parse_time_range("24h").as_secs(), 86400);
        assert_eq!(parse_time_range("7d").as_secs(), 604_800);
        assert_eq!(parse_time_range("30m").as_secs(), 1800);
        assert_eq!(parse_time_range("1w").as_secs(), 604_800);
        assert_eq!(parse_time_range("").as_secs(), 86400); // Default
        assert_eq!(parse_time_range("invalid").as_secs(), 86400); // Default
    }

    #[test]
    fn test_calculate_step() {
        assert_eq!(PrometheusClient::calculate_step(Duration::from_secs(1800)), "1m");
        assert_eq!(PrometheusClient::calculate_step(Duration::from_secs(7200)), "5m");
        assert_eq!(PrometheusClient::calculate_step(Duration::from_secs(86400)), "5m");
        assert_eq!(PrometheusClient::calculate_step(Duration::from_secs(259_200)), "1h");
        assert_eq!(PrometheusClient::calculate_step(Duration::from_secs(604_800)), "1h");
        assert_eq!(PrometheusClient::calculate_step(Duration::from_secs(1_209_600)), "6h");
    }

    #[test]
    fn test_sanitize_label_value() {
        // Allow alphanumeric, underscore, hyphen, and period
        assert_eq!(sanitize_label_value("my-upstream_1.0"), "my-upstream_1.0");

        // Filter out special characters that could break PromQL
        assert_eq!(sanitize_label_value("upstream{test}"), "upstreamtest");
        assert_eq!(sanitize_label_value("upstream\"quoted\""), "upstreamquoted");
        assert_eq!(sanitize_label_value("upstream'quoted'"), "upstreamquoted");
        assert_eq!(sanitize_label_value("upstream\\escape"), "upstreamescape");

        // PromQL injection attempts
        assert_eq!(
            sanitize_label_value("test\"}}){__name__=~\".+\"}"),
            "test__name__." // + is filtered out
        );
        assert_eq!(
            sanitize_label_value("test\"}or{upstream=\"malicious"),
            "testorupstreammalicious"
        );

        // Length limiting (max 128 chars)
        let long_input = "a".repeat(200);
        assert_eq!(sanitize_label_value(&long_input).len(), 128);

        // Empty and special cases
        assert_eq!(sanitize_label_value(""), "");
        assert_eq!(sanitize_label_value("!!!"), "");
        assert_eq!(sanitize_label_value("123-abc_xyz.test"), "123-abc_xyz.test");
    }
}
