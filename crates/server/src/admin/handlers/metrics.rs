//! Metrics endpoint handlers.

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]

use axum::{extract::State, response::IntoResponse, Json};

use crate::admin::{
    prometheus::parse_time_range,
    types::{
        ErrorDistribution, HedgingStats, KPIMetric, LatencyDataPoint, RequestMethodData,
        TimeRangeQuery, TimeSeriesPoint, Trend, WinnerDistribution,
    },
    AdminState,
};

/// GET /admin/metrics/kpis
///
/// Returns dashboard KPI metrics including total requests, cache hit rate, healthy upstreams,
/// average latency, cache entries, and error rate.
#[allow(clippy::too_many_lines)]
#[utoipa::path(
    get,
    path = "/admin/metrics/kpis",
    tag = "Metrics",
    responses(
        (status = 200, description = "Key performance indicators", body = Vec<KPIMetric>),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_kpis(State(state): State<AdminState>) -> impl IntoResponse {
    let upstream_stats = state.proxy_engine.get_upstream_stats().await;
    let cache_stats = state.proxy_engine.get_cache_stats().await;
    let metrics_collector = state.proxy_engine.get_metrics_collector();
    let metrics_summary = metrics_collector.get_metrics_summary().await;

    // Calculate cache hit rate as percentage
    let cache_hit_rate_pct = (metrics_summary.cache_hit_rate * 100.0).round();

    // Determine trend for cache hit rate
    let cache_trend = if cache_hit_rate_pct >= 80.0 {
        Trend::Up
    } else if cache_hit_rate_pct <= 20.0 {
        Trend::Down
    } else {
        Trend::Stable
    };

    // Determine trend for latency
    let latency_trend = if metrics_summary.average_latency_ms <= 100.0 {
        Trend::Up
    } else if metrics_summary.average_latency_ms >= 500.0 {
        Trend::Down
    } else {
        Trend::Stable
    };

    let kpis = vec![
        KPIMetric {
            id: "total-requests".to_string(),
            label: "Total Requests".to_string(),
            value: serde_json::json!(metrics_summary.total_requests),
            unit: None,
            change: None,
            trend: Trend::Stable,
            sparkline: None,
        },
        KPIMetric {
            id: "cache-hit-rate".to_string(),
            label: "Cache Hit Rate".to_string(),
            value: serde_json::json!(cache_hit_rate_pct),
            unit: Some("%".to_string()),
            change: None,
            trend: cache_trend,
            sparkline: None,
        },
        KPIMetric {
            id: "healthy-upstreams".to_string(),
            label: "Healthy Upstreams".to_string(),
            value: serde_json::json!(format!(
                "{}/{}",
                upstream_stats.healthy_upstreams, upstream_stats.total_upstreams
            )),
            unit: None,
            change: None,
            trend: if upstream_stats.healthy_upstreams == upstream_stats.total_upstreams {
                Trend::Up
            } else if upstream_stats.healthy_upstreams == 0 {
                Trend::Down
            } else {
                Trend::Stable
            },
            sparkline: None,
        },
        KPIMetric {
            id: "avg-latency".to_string(),
            label: "Avg Latency".to_string(),
            value: {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let latency = metrics_summary.average_latency_ms.round() as u64;
                serde_json::json!(latency)
            },
            unit: Some("ms".to_string()),
            change: None,
            trend: latency_trend,
            sparkline: None,
        },
        KPIMetric {
            id: "cache-entries".to_string(),
            label: "Cache Entries".to_string(),
            value: serde_json::json!(
                cache_stats.block_cache_entries +
                    cache_stats.transaction_cache_entries +
                    cache_stats.logs_cache_entries
            ),
            unit: None,
            change: None,
            trend: Trend::Stable,
            sparkline: None,
        },
        KPIMetric {
            id: "error-rate".to_string(),
            label: "Upstream Errors".to_string(),
            value: serde_json::json!(metrics_summary.total_upstream_errors),
            unit: None,
            change: None,
            trend: if metrics_summary.total_upstream_errors == 0 {
                Trend::Up
            } else if metrics_summary.total_upstream_errors > 100 {
                Trend::Down
            } else {
                Trend::Stable
            },
            sparkline: None,
        },
    ];

    Json(kpis)
}

/// GET /admin/metrics/latency
///
/// Returns latency percentiles (p50, p95, p99) over time from Prometheus.
#[utoipa::path(
    get,
    path = "/admin/metrics/latency",
    tag = "Metrics",
    params(
        ("timeRange" = Option<String>, Query, description = "Time range (e.g., '1h', '24h', '7d'). Default: 24h")
    ),
    responses(
        (status = 200, description = "Latency percentiles over time", body = Vec<LatencyDataPoint>),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_latency(
    State(state): State<AdminState>,
    axum::extract::Query(query): axum::extract::Query<TimeRangeQuery>,
) -> impl IntoResponse {
    // Query Prometheus for historical latency data if available
    if let Some(ref prom_client) = state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);
        match prom_client.get_latency_percentiles(time_range).await {
            Ok(data) if !data.is_empty() => return Json(data),
            Ok(_) => {
                tracing::debug!("no latency data from Prometheus");
            }
            Err(e) => {
                tracing::warn!("failed to query Prometheus for latency: {}", e);
            }
        }
    }

    // Fallback to in-memory metrics when Prometheus is unavailable
    let metrics_collector = state.proxy_engine.get_metrics_collector();
    let metrics_summary = metrics_collector.get_metrics_summary().await;

    // Return current average latency as a single data point
    // Note: In-memory metrics don't track percentiles, so we use average for all percentiles
    let data: Vec<LatencyDataPoint> = vec![LatencyDataPoint {
        timestamp: chrono::Utc::now().to_rfc3339(),
        p50: metrics_summary.average_latency_ms,
        p95: metrics_summary.average_latency_ms,
        p99: metrics_summary.average_latency_ms,
    }];

    Json(data)
}

/// GET /admin/metrics/request-volume
///
/// Returns request volume (requests per second) over time from Prometheus.
#[utoipa::path(
    get,
    path = "/admin/metrics/request-volume",
    tag = "Metrics",
    params(
        ("timeRange" = Option<String>, Query, description = "Time range (e.g., '1h', '24h', '7d'). Default: 24h")
    ),
    responses(
        (status = 200, description = "Request volume time series", body = Vec<TimeSeriesPoint>),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_request_volume(
    State(state): State<AdminState>,
    axum::extract::Query(query): axum::extract::Query<TimeRangeQuery>,
) -> impl IntoResponse {
    // Query Prometheus for historical request data if available
    if let Some(ref prom_client) = state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);
        match prom_client.get_request_rate(time_range).await {
            Ok(data) if !data.is_empty() => return Json(data),
            Ok(_) => {
                tracing::debug!("no request volume data from Prometheus");
            }
            Err(e) => {
                tracing::warn!("failed to query Prometheus for request volume: {}", e);
            }
        }
    }

    // Fallback to in-memory metrics when Prometheus is unavailable
    let metrics_collector = state.proxy_engine.get_metrics_collector();
    let metrics_summary = metrics_collector.get_metrics_summary().await;

    // Return total requests as a single data point
    // Note: Without historical data, we can't calculate rate, so we return the total count
    let data: Vec<TimeSeriesPoint> = vec![TimeSeriesPoint {
        timestamp: chrono::Utc::now().to_rfc3339(),
        value: metrics_summary.total_requests as f64,
    }];

    Json(data)
}

/// GET /admin/metrics/request-methods
///
/// Returns request distribution by RPC method with counts and percentages.
#[utoipa::path(
    get,
    path = "/admin/metrics/request-methods",
    tag = "Metrics",
    params(
        ("timeRange" = Option<String>, Query, description = "Time range (e.g., '1h', '24h', '7d'). Default: 24h")
    ),
    responses(
        (status = 200, description = "Request distribution by method", body = Vec<RequestMethodData>),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_request_methods(
    State(state): State<AdminState>,
    axum::extract::Query(query): axum::extract::Query<TimeRangeQuery>,
) -> impl IntoResponse {
    // Query Prometheus for per-method request counts if available
    if let Some(ref prom_client) = state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);
        match prom_client.get_request_by_method(time_range).await {
            Ok(data) if !data.is_empty() => return Json(data),
            Ok(_) => {
                tracing::debug!("no request method data from Prometheus");
            }
            Err(e) => {
                tracing::warn!("failed to query Prometheus for request methods: {}", e);
            }
        }
    }

    // Fallback to in-memory metrics when Prometheus is unavailable
    let metrics_collector = state.proxy_engine.get_metrics_collector();
    let metrics_summary = metrics_collector.get_metrics_summary().await;

    // Calculate total requests for percentage calculation
    let total_requests: u64 = metrics_summary.requests_by_method.values().sum();

    // Convert the requests_by_method HashMap to RequestMethodData with percentages
    let mut data: Vec<RequestMethodData> = metrics_summary
        .requests_by_method
        .iter()
        .map(|(method, count)| {
            let percentage = if total_requests > 0 {
                (*count as f64 / total_requests as f64) * 100.0
            } else {
                0.0
            };
            RequestMethodData { method: method.clone(), count: *count, percentage }
        })
        .collect();

    // Sort by count descending for better presentation
    data.sort_by(|a, b| b.count.cmp(&a.count));

    Json(data)
}

/// GET /admin/metrics/error-distribution
///
/// Returns error distribution by type (timeout, RPC error, connection error, etc.) with counts and
/// percentages.
#[utoipa::path(
    get,
    path = "/admin/metrics/error-distribution",
    tag = "Metrics",
    params(
        ("timeRange" = Option<String>, Query, description = "Time range (e.g., '1h', '24h', '7d'). Default: 24h")
    ),
    responses(
        (status = 200, description = "Error distribution by type", body = Vec<ErrorDistribution>),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_error_distribution(
    State(state): State<AdminState>,
    axum::extract::Query(query): axum::extract::Query<TimeRangeQuery>,
) -> impl IntoResponse {
    // Query Prometheus for error breakdown if available
    if let Some(ref prom_client) = state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);
        match prom_client.get_error_distribution(time_range).await {
            Ok(data) if !data.is_empty() => return Json(data),
            Ok(_) => {
                tracing::debug!("no error distribution data from Prometheus");
            }
            Err(e) => {
                tracing::warn!("failed to query Prometheus for error distribution: {}", e);
            }
        }
    }

    // Fallback to in-memory metrics when Prometheus is unavailable
    let metrics_collector = state.proxy_engine.get_metrics_collector();
    let metrics_summary = metrics_collector.get_metrics_summary().await;

    // Calculate total errors for percentage calculation
    let total_errors: u64 = metrics_summary.errors_by_upstream.values().sum();

    // Convert the errors_by_upstream HashMap to ErrorDistribution with percentages
    // Note: The in-memory metrics track errors by upstream, not by error type
    // We use upstream name as the error type for now
    let mut data: Vec<ErrorDistribution> = metrics_summary
        .errors_by_upstream
        .iter()
        .map(|(upstream, count)| {
            let percentage = if total_errors > 0 {
                (*count as f64 / total_errors as f64) * 100.0
            } else {
                0.0
            };
            ErrorDistribution { error_type: upstream.clone(), count: *count, percentage }
        })
        .collect();

    // Sort by count descending for better presentation
    data.sort_by(|a, b| b.count.cmp(&a.count));

    Json(data)
}

/// GET /admin/metrics/hedging-stats
///
/// Returns request hedging performance metrics including hedge triggers, winner distribution, and
/// P99 improvement.
#[utoipa::path(
    get,
    path = "/admin/metrics/hedging-stats",
    tag = "Metrics",
    params(
        ("timeRange" = Option<String>, Query, description = "Time range (e.g., '1h', '24h', '7d'). Default: 24h")
    ),
    responses(
        (status = 200, description = "Hedging performance statistics", body = HedgingStats),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_hedging_stats(
    State(state): State<AdminState>,
    axum::extract::Query(query): axum::extract::Query<TimeRangeQuery>,
) -> impl IntoResponse {
    // Query Prometheus for hedging metrics if available
    if let Some(ref prom_client) = state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);
        match prom_client.get_hedging_stats(time_range).await {
            Ok(hedging_stats) => return Json(hedging_stats),
            Err(e) => {
                tracing::warn!("failed to query Prometheus for hedging stats: {}", e);
            }
        }
    }

    // Return default stats if Prometheus unavailable or query failed
    let hedging_stats = HedgingStats {
        hedged_requests: 0.0,
        hedge_triggers: 0.0,
        primary_wins: 0.0,
        p99_improvement: 0.0,
        winner_distribution: WinnerDistribution { primary: 100.0, hedged: 0.0 },
    };

    Json(hedging_stats)
}
