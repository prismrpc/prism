//! Upstream endpoint handlers.

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};

use crate::admin::{
    audit,
    prometheus::parse_time_range,
    types::{
        BulkHealthCheckResponse, CircuitBreakerMetrics, CircuitBreakerState, HealthCheckResult,
        HealthHistoryEntry, LatencyDataPoint, RequestMethodData, SuccessResponse, TimeRangeQuery,
        Upstream, UpstreamMetrics, UpstreamRequestDistribution, UpstreamResponse,
        UpstreamScoringFactor, UpstreamStatus,
    },
    AdminState,
};
use prism_core::upstream::{
    CreateUpstreamRequest as CoreCreateUpstreamRequest,
    UpdateUpstreamRequest as CoreUpdateUpstreamRequest,
};
use std::net::SocketAddr;

/// Default scoring weights used when scoring engine data is unavailable.
mod defaults {
    /// Default weight for latency factor in scoring.
    pub const LATENCY_WEIGHT: f64 = 8.0;
    /// Default weight for error rate factor in scoring.
    pub const ERROR_RATE_WEIGHT: f64 = 4.0;
    /// Default weight for block lag factor in scoring.
    pub const BLOCK_LAG_WEIGHT: f64 = 2.0;
}

/// Minimum allowed upstream weight.
const MIN_UPSTREAM_WEIGHT: u32 = 1;
/// Maximum allowed upstream weight.
const MAX_UPSTREAM_WEIGHT: u32 = 1000;
/// Maximum allowed upstream name length.
const MAX_UPSTREAM_NAME_LENGTH: usize = 128;

/// Validates an upstream URL for security (SSRF protection).
///
/// Only allows http and https schemes to prevent file://, ftp://, or other
/// potentially dangerous URL schemes.
fn validate_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL: {e}"))?;

    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => {
            Err(format!("Invalid URL scheme '{scheme}'. Only 'http' and 'https' are allowed."))
        }
    }
}

/// Validates an upstream name.
///
/// Names must be non-empty, within length limits, and contain only safe characters.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Upstream name cannot be empty".to_string());
    }

    if name.len() > MAX_UPSTREAM_NAME_LENGTH {
        return Err(format!(
            "Upstream name too long. Maximum length is {MAX_UPSTREAM_NAME_LENGTH} characters."
        ));
    }

    // Allow alphanumeric, dash, underscore, and dot
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(
            "Upstream name contains invalid characters. Only alphanumeric, dash, underscore, and dot are allowed.".to_string()
        );
    }

    Ok(())
}

/// Validates an upstream weight.
fn validate_weight(weight: u32) -> Result<(), String> {
    if weight < MIN_UPSTREAM_WEIGHT || weight > MAX_UPSTREAM_WEIGHT {
        return Err(format!(
            "Weight must be between {MIN_UPSTREAM_WEIGHT} and {MAX_UPSTREAM_WEIGHT}."
        ));
    }
    Ok(())
}

/// Masks sensitive parts of URLs (API keys, etc.).
fn mask_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        // Parse failed - return masked placeholder for safety
        return "[invalid-url]".to_string();
    };

    // Mask password if present
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some("***"));
    }

    // Mask sensitive query parameters
    let sensitive_keys = ["key", "apikey", "api_key", "token", "secret", "password", "auth"];
    let query_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| {
            let key_lower = key.to_lowercase();
            if sensitive_keys.iter().any(|s| key_lower.contains(s)) {
                (key.to_string(), "***".to_string())
            } else {
                (key.to_string(), value.to_string())
            }
        })
        .collect();

    if !query_pairs.is_empty() {
        let new_query: String = query_pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        parsed.set_query(Some(&new_query));
    }

    // Mask long path segments (likely API keys) - existing logic
    let path = parsed.path().to_string();
    let parts: Vec<&str> = path.split('/').collect();
    if let Some(last) = parts.last() {
        if last.len() > 20 {
            // Use safe slicing (already fixed by another agent)
            if last.len() >= 8 {
                let masked = format!("{}...{}", &last[..4], &last[last.len() - 4..]);
                let new_path = format!("{}/{masked}", parts[..parts.len() - 1].join("/"));
                parsed.set_path(&new_path);
            }
        }
    }

    parsed.to_string()
}

/// Converts internal circuit breaker state to API type.
fn convert_cb_state(
    state: prism_core::upstream::circuit_breaker::CircuitBreakerState,
) -> CircuitBreakerState {
    match state {
        prism_core::upstream::circuit_breaker::CircuitBreakerState::Closed => {
            CircuitBreakerState::Closed
        }
        prism_core::upstream::circuit_breaker::CircuitBreakerState::Open => {
            CircuitBreakerState::Open
        }
        prism_core::upstream::circuit_breaker::CircuitBreakerState::HalfOpen => {
            CircuitBreakerState::HalfOpen
        }
    }
}

/// Extracted upstream metrics from scoring engine and health data.
struct ExtractedMetrics {
    composite_score: f64,
    error_rate: f64,
    throttle_rate: f64,
    requests_handled: u64,
    latency_p90: u64,
}

/// Extracts upstream metrics from scoring engine data and health status.
///
/// Prefers scoring engine data when available, falls back to raw metrics,
/// then to health data if neither is available.
fn extract_upstream_metrics(
    score: Option<&prism_core::upstream::scoring::UpstreamScore>,
    metrics: Option<(f64, f64, u64, u64, Option<u64>)>,
    health: &prism_core::types::UpstreamHealth,
) -> ExtractedMetrics {
    if let Some(score) = score {
        ExtractedMetrics {
            composite_score: score.composite_score,
            error_rate: score.error_rate,
            throttle_rate: score.throttle_rate,
            requests_handled: score.total_requests,
            latency_p90: score.latency_p90_ms.unwrap_or(0),
        }
    } else if let Some((err_rate, thr_rate, total_req, _block, _avg)) = metrics {
        ExtractedMetrics {
            composite_score: 0.0,
            error_rate: err_rate,
            throttle_rate: thr_rate,
            requests_handled: total_req,
            latency_p90: health.latency_p99_ms.unwrap_or(0),
        }
    } else {
        ExtractedMetrics {
            composite_score: 0.0,
            error_rate: 0.0,
            throttle_rate: 0.0,
            requests_handled: 0,
            latency_p90: health.latency_p99_ms.unwrap_or(0),
        }
    }
}

/// GET /admin/upstreams
///
/// Lists all configured upstreams with current metrics.
#[utoipa::path(
    get,
    path = "/admin/upstreams",
    tag = "Upstreams",
    responses(
        (status = 200, description = "List of upstreams", body = Vec<Upstream>)
    )
)]
pub async fn list_upstreams(State(state): State<AdminState>) -> impl IntoResponse {
    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();
    let scoring_engine = state.proxy_engine.get_upstream_manager().get_scoring_engine();

    let mut result: Vec<Upstream> = Vec::with_capacity(upstreams.len());

    for (idx, u) in upstreams.iter().enumerate() {
        let config = u.config();
        let health = u.get_health().await;
        let cb_state = u.get_circuit_breaker_state().await;
        let chain_tip = state.chain_state.current_tip();

        // Calculate block lag from latest_block
        #[allow(clippy::cast_possible_wrap)]
        let block_lag =
            health.latest_block.map_or(0, |latest| chain_tip.saturating_sub(latest) as i64);

        // Get scoring metrics if available
        let upstream_name = config.name.as_ref();
        let score = scoring_engine.get_score(upstream_name);
        let metrics_data = scoring_engine.get_metrics(upstream_name);

        let extracted_metrics = extract_upstream_metrics(score.as_ref(), metrics_data, &health);

        result.push(Upstream {
            id: idx.to_string(),
            name: config.name.to_string(),
            url: mask_url(&config.url),
            status: if health.is_healthy {
                UpstreamStatus::Healthy
            } else {
                UpstreamStatus::Unhealthy
            },
            circuit_breaker_state: convert_cb_state(cb_state),
            composite_score: extracted_metrics.composite_score,
            latency_p90: extracted_metrics.latency_p90,
            error_rate: extracted_metrics.error_rate,
            throttle_rate: extracted_metrics.throttle_rate,
            block_lag,
            requests_handled: extracted_metrics.requests_handled,
            weight: config.weight,
            ws_connected: config.supports_websocket && config.ws_url.is_some(),
        });
    }

    Json(result)
}

/// GET /admin/upstreams/:id
///
/// Gets a single upstream by ID.
#[utoipa::path(
    get,
    path = "/admin/upstreams/{id}",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID")
    ),
    responses(
        (status = 200, description = "Upstream details", body = Upstream),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn get_upstream(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Upstream>, (StatusCode, String)> {
    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();
    let scoring_engine = state.proxy_engine.get_upstream_manager().get_scoring_engine();

    let u = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    let config = u.config();
    let health = u.get_health().await;
    let cb_state = u.get_circuit_breaker_state().await;
    let chain_tip = state.chain_state.current_tip();

    #[allow(clippy::cast_possible_wrap)]
    let block_lag = health.latest_block.map_or(0, |latest| chain_tip.saturating_sub(latest) as i64);

    // Get scoring metrics if available
    let upstream_name = config.name.as_ref();
    let score = scoring_engine.get_score(upstream_name);
    let metrics_data = scoring_engine.get_metrics(upstream_name);

    let extracted_metrics = extract_upstream_metrics(score.as_ref(), metrics_data, &health);

    let upstream = Upstream {
        id: idx.to_string(),
        name: config.name.to_string(),
        url: mask_url(&config.url),
        status: if health.is_healthy {
            UpstreamStatus::Healthy
        } else {
            UpstreamStatus::Unhealthy
        },
        circuit_breaker_state: convert_cb_state(cb_state),
        composite_score: extracted_metrics.composite_score,
        latency_p90: extracted_metrics.latency_p90,
        error_rate: extracted_metrics.error_rate,
        throttle_rate: extracted_metrics.throttle_rate,
        block_lag,
        requests_handled: extracted_metrics.requests_handled,
        weight: config.weight,
        ws_connected: config.supports_websocket && config.ws_url.is_some(),
    };

    Ok(Json(upstream))
}

/// GET /admin/upstreams/:id/metrics
///
/// Gets detailed metrics for an upstream including scoring breakdown, circuit breaker state, and
/// request distribution.
#[allow(clippy::too_many_lines)]
#[utoipa::path(
    get,
    path = "/admin/upstreams/{id}/metrics",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID")
    ),
    responses(
        (status = 200, description = "Detailed upstream metrics", body = UpstreamMetrics),
        (status = 400, description = "Invalid upstream ID"),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn get_upstream_metrics(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<UpstreamMetrics>, (StatusCode, String)> {
    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();
    let scoring_engine = state.proxy_engine.get_upstream_manager().get_scoring_engine();

    let u = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    let config = u.config();
    let health = u.get_health().await;
    let cb_state = u.get_circuit_breaker_state().await;
    let cb_failures = u.get_circuit_breaker_failure_count().await;
    let chain_tip = state.chain_state.current_tip();

    #[allow(clippy::cast_possible_wrap)]
    let block_lag = health.latest_block.map_or(0, |latest| chain_tip.saturating_sub(latest) as i64);

    // Get scoring metrics if available
    let upstream_name = config.name.as_ref();
    let score = scoring_engine.get_score(upstream_name);
    let metrics_data = scoring_engine.get_metrics(upstream_name);

    let extracted_metrics = extract_upstream_metrics(score.as_ref(), metrics_data, &health);

    let upstream = Upstream {
        id: idx.to_string(),
        name: config.name.to_string(),
        url: mask_url(&config.url),
        status: if health.is_healthy {
            UpstreamStatus::Healthy
        } else {
            UpstreamStatus::Unhealthy
        },
        circuit_breaker_state: convert_cb_state(cb_state),
        composite_score: extracted_metrics.composite_score,
        latency_p90: extracted_metrics.latency_p90,
        error_rate: extracted_metrics.error_rate,
        throttle_rate: extracted_metrics.throttle_rate,
        block_lag,
        requests_handled: extracted_metrics.requests_handled,
        weight: config.weight,
        ws_connected: config.supports_websocket && config.ws_url.is_some(),
    };

    // Scoring breakdown from actual scoring engine
    let scoring_breakdown = if let Some(score) = score {
        let weights = scoring_engine.get_config().weights;
        vec![
            UpstreamScoringFactor {
                factor: "Latency".to_string(),
                weight: weights.latency,
                score: score.latency_factor,
                value: format!(
                    "P90: {}ms, P99: {}ms",
                    score.latency_p90_ms.unwrap_or(0),
                    score.latency_p99_ms.unwrap_or(0)
                ),
            },
            UpstreamScoringFactor {
                factor: "Error Rate".to_string(),
                weight: weights.error_rate,
                score: score.error_rate_factor,
                value: format!("{:.2}%", score.error_rate * 100.0),
            },
            UpstreamScoringFactor {
                factor: "Throttle Rate".to_string(),
                weight: weights.throttle_rate,
                score: score.throttle_factor,
                value: format!("{:.2}%", score.throttle_rate * 100.0),
            },
            UpstreamScoringFactor {
                factor: "Block Lag".to_string(),
                weight: weights.block_head_lag,
                score: score.block_lag_factor,
                value: format!("{} blocks behind", score.block_head_lag),
            },
            UpstreamScoringFactor {
                factor: "Load".to_string(),
                weight: weights.total_requests,
                score: score.load_factor,
                value: format!("{} requests", score.total_requests),
            },
        ]
    } else {
        // Fallback when no score is available
        vec![
            UpstreamScoringFactor {
                factor: "Latency".to_string(),
                weight: defaults::LATENCY_WEIGHT,
                score: 0.0,
                value: format!("P99: {}ms (insufficient data)", health.latency_p99_ms.unwrap_or(0)),
            },
            UpstreamScoringFactor {
                factor: "Error Rate".to_string(),
                weight: defaults::ERROR_RATE_WEIGHT,
                score: 0.0,
                value: format!("{:.2}% (insufficient data)", extracted_metrics.error_rate * 100.0),
            },
            UpstreamScoringFactor {
                factor: "Block Lag".to_string(),
                weight: defaults::BLOCK_LAG_WEIGHT,
                score: 0.0,
                value: format!("{block_lag} blocks (insufficient data)"),
            },
        ]
    };

    // Calculate time remaining until circuit breaker transitions from Open to HalfOpen
    let time_remaining = u.circuit_breaker().get_time_remaining().await.map(|d| d.as_secs());

    let circuit_breaker = CircuitBreakerMetrics {
        state: convert_cb_state(cb_state),
        failure_threshold: config.circuit_breaker_threshold,
        current_failures: cb_failures,
        time_remaining,
    };

    // Request distribution (placeholder - TODO: implement per-method tracking in future)
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let request_distribution = vec![UpstreamRequestDistribution {
        method: "all".to_string(),
        requests: extracted_metrics.requests_handled,
        success: extracted_metrics.requests_handled.saturating_sub(
            (extracted_metrics.requests_handled as f64 * extracted_metrics.error_rate) as u64,
        ),
        failed: (extracted_metrics.requests_handled as f64 * extracted_metrics.error_rate) as u64,
    }];

    let metrics =
        UpstreamMetrics { upstream, scoring_breakdown, circuit_breaker, request_distribution };

    Ok(Json(metrics))
}

/// POST /admin/upstreams/:id/health-check
///
/// Triggers an immediate health check for an upstream.
#[utoipa::path(
    post,
    path = "/admin/upstreams/{id}/health-check",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID")
    ),
    responses(
        (status = 200, description = "Health check result", body = HealthCheckResult),
        (status = 400, description = "Invalid upstream ID"),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn trigger_health_check(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<HealthCheckResult>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();

    let u = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    let upstream_name = u.config().name.to_string();

    // Perform health check
    let start = std::time::Instant::now();
    let is_healthy = u.health_check().await;
    #[allow(clippy::cast_possible_truncation)]
    let latency = start.elapsed().as_millis() as u64;

    tracing::info!(
        correlation_id = ?correlation_id,
        upstream = %upstream_name,
        upstream_id = %id,
        is_healthy = is_healthy,
        latency_ms = latency,
        "triggered health check"
    );

    let response = HealthCheckResult {
        upstream_id: id,
        success: is_healthy,
        latency: Some(latency),
        error: if is_healthy {
            None
        } else {
            Some("Health check failed".to_string())
        },
    };

    Ok(Json(response))
}

/// POST /admin/upstreams/health-check
///
/// Triggers health checks for all upstreams in parallel.
#[utoipa::path(
    post,
    path = "/admin/upstreams/health-check",
    tag = "Upstreams",
    responses(
        (status = 200, description = "Bulk health check results", body = BulkHealthCheckResponse),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn trigger_all_health_checks(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();

    tracing::info!(
        correlation_id = ?correlation_id,
        upstream_count = upstreams.len(),
        "triggered bulk health check for all upstreams"
    );

    // Run all health checks in parallel using futures::future::join_all
    let health_check_futures: Vec<_> = upstreams
        .iter()
        .enumerate()
        .map(|(idx, u)| {
            let upstream = u.clone();
            async move {
                let start = std::time::Instant::now();
                let is_healthy = upstream.health_check().await;
                #[allow(clippy::cast_possible_truncation)]
                let latency = start.elapsed().as_millis() as u64;

                HealthCheckResult {
                    upstream_id: idx.to_string(),
                    success: is_healthy,
                    latency: Some(latency),
                    error: if is_healthy {
                        None
                    } else {
                        Some("Health check failed".to_string())
                    },
                }
            }
        })
        .collect();

    let results = futures::future::join_all(health_check_futures).await;

    let response = BulkHealthCheckResponse { success: true, checked: results.len(), results };

    Json(response)
}

/// PUT /admin/upstreams/:id/circuit-breaker/reset
///
/// Resets the circuit breaker for an upstream, closing it and clearing failure count.
#[utoipa::path(
    put,
    path = "/admin/upstreams/{id}/circuit-breaker/reset",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID")
    ),
    responses(
        (status = 200, description = "Circuit breaker reset successfully", body = SuccessResponse),
        (status = 400, description = "Invalid upstream ID"),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn reset_circuit_breaker(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SuccessResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();

    let u = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    u.reset_circuit_breaker().await;

    let upstream_name = u.config().name.to_string();

    // Audit log the circuit breaker reset
    audit::log_update("upstream_circuit_breaker", &id, Some(addr), None);

    tracing::info!(
        correlation_id = ?correlation_id,
        upstream = %upstream_name,
        upstream_id = %id,
        "reset circuit breaker"
    );

    Ok(Json(SuccessResponse { success: true }))
}

/// PUT /admin/upstreams/:id/weight
///
/// Updates the load balancing weight for an upstream (dynamic weight adjustment).
#[utoipa::path(
    put,
    path = "/admin/upstreams/{id}/weight",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID")
    ),
    request_body = crate::admin::types::UpdateWeightRequest,
    responses(
        (status = 200, description = "Weight updated successfully", body = SuccessResponse),
        (status = 400, description = "Invalid upstream ID or weight"),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn update_weight(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    Json(request): Json<crate::admin::types::UpdateWeightRequest>,
) -> Result<Json<SuccessResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Validate weight
    validate_weight(request.weight).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();

    let u = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    // Get the current config and create a new one with updated weight
    let current_config = u.config();
    let upstream_name = current_config.name.to_string();
    let old_weight = current_config.weight;
    let new_config = prism_core::types::UpstreamConfig {
        weight: request.weight,
        url: current_config.url.clone(),
        ws_url: current_config.ws_url.clone(),
        name: current_config.name.clone(),
        chain_id: current_config.chain_id,
        timeout_seconds: current_config.timeout_seconds,
        supports_websocket: current_config.supports_websocket,
        circuit_breaker_threshold: current_config.circuit_breaker_threshold,
        circuit_breaker_timeout_seconds: current_config.circuit_breaker_timeout_seconds,
    };

    // Use atomic update to replace the upstream in place
    let updated = state
        .proxy_engine
        .get_upstream_manager()
        .update_upstream(&upstream_name, new_config);

    if !updated {
        return Err((StatusCode::NOT_FOUND, "Upstream not found".to_string()));
    }

    // Audit log with before/after weight
    let details = serde_json::json!({
        "weight": {
            "old": old_weight,
            "new": request.weight
        }
    });
    audit::log_update("upstream", &id, Some(addr), Some(details));

    tracing::info!(
        correlation_id = ?correlation_id,
        upstream = %upstream_name,
        new_weight = request.weight,
        "updated upstream weight"
    );

    Ok(Json(SuccessResponse { success: true }))
}

/// GET /admin/upstreams/:id/latency-distribution
///
/// Gets latency percentiles over time for a specific upstream.
#[utoipa::path(
    get,
    path = "/admin/upstreams/{id}/latency-distribution",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID"),
        ("timeRange" = Option<String>, Query, description = "Time range (e.g., '1h', '24h', '7d')")
    ),
    responses(
        (status = 200, description = "Latency distribution data", body = Vec<LatencyDataPoint>),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn get_latency_distribution(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<Vec<LatencyDataPoint>>, (StatusCode, String)> {
    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();

    let upstream = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    let upstream_name = upstream.config().name.to_string();

    // Try to query from Prometheus if available
    if let Some(prometheus_client) = &state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);

        tracing::debug!(
            upstream = %upstream_name,
            time_range = %query.time_range,
            "querying latency distribution for upstream from Prometheus"
        );

        // Query per-upstream latency percentiles from Prometheus
        match prometheus_client
            .get_upstream_latency_percentiles(&upstream_name, time_range)
            .await
        {
            Ok(data) => return Ok(Json(data)),
            Err(e) => {
                tracing::warn!(
                    upstream = %upstream_name,
                    error = %e,
                    "failed to query latency distribution from Prometheus"
                );
                // Fall through to return empty data
            }
        }
    }

    // No Prometheus client available or query failed
    Ok(Json(Vec::new()))
}

/// GET /admin/upstreams/:id/health-history
///
/// Gets recent health check results for an upstream.
#[utoipa::path(
    get,
    path = "/admin/upstreams/{id}/health-history",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID"),
        ("limit" = Option<usize>, Query, description = "Maximum number of entries (default: 50)")
    ),
    responses(
        (status = 200, description = "Health history entries", body = Vec<HealthHistoryEntry>),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn get_health_history(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(params): Query<HealthHistoryQuery>,
) -> Result<Json<Vec<HealthHistoryEntry>>, (StatusCode, String)> {
    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();

    let upstream = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    let limit = params.limit.unwrap_or(50);

    // Get health history from the upstream
    let history = upstream.get_health_history(limit).await;

    // Convert internal HealthHistoryEntry to API HealthHistoryEntry
    let entries: Vec<HealthHistoryEntry> = history
        .into_iter()
        .map(|entry| HealthHistoryEntry {
            timestamp: entry.timestamp.to_rfc3339(),
            healthy: entry.healthy,
            latency_ms: entry.latency_ms,
            error: entry.error,
        })
        .collect();

    tracing::debug!(
        upstream_id = %id,
        count = entries.len(),
        limit = limit,
        "returning health history"
    );

    Ok(Json(entries))
}

/// Query parameters for health history endpoint.
#[derive(Debug, serde::Deserialize)]
pub struct HealthHistoryQuery {
    pub limit: Option<usize>,
}

/// GET /admin/upstreams/:id/request-distribution
///
/// Gets request distribution by RPC method for a specific upstream.
#[utoipa::path(
    get,
    path = "/admin/upstreams/{id}/request-distribution",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID"),
        ("timeRange" = Option<String>, Query, description = "Time range (e.g., '1h', '24h', '7d')")
    ),
    responses(
        (status = 200, description = "Request distribution by method", body = Vec<RequestMethodData>),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn get_request_distribution(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<Vec<RequestMethodData>>, (StatusCode, String)> {
    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();

    let upstream = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    let upstream_name = upstream.config().name.to_string();

    // Try to query from Prometheus if available
    if let Some(prometheus_client) = &state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);

        tracing::debug!(
            upstream = %upstream_name,
            time_range = %query.time_range,
            "querying request distribution for upstream from Prometheus"
        );

        // Query per-upstream request distribution by method from Prometheus
        match prometheus_client
            .get_upstream_request_by_method(&upstream_name, time_range)
            .await
        {
            Ok(data) => return Ok(Json(data)),
            Err(e) => {
                tracing::warn!(
                    upstream = %upstream_name,
                    error = %e,
                    "failed to query request distribution from Prometheus"
                );
                // Fall through to return empty data
            }
        }
    }

    // No Prometheus client available or query failed
    Ok(Json(Vec::new()))
}

/// POST /admin/upstreams
///
/// Creates a new dynamic upstream.
#[utoipa::path(
    post,
    path = "/admin/upstreams",
    tag = "Upstreams",
    request_body = crate::admin::types::CreateUpstreamRequest,
    responses(
        (status = 200, description = "Upstream created", body = UpstreamResponse),
        (status = 400, description = "Invalid request"),
        (status = 409, description = "Upstream name conflicts")
    )
)]
pub async fn create_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(request): Json<crate::admin::types::CreateUpstreamRequest>,
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Validate input
    validate_name(&request.name).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    validate_url(&request.url).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if let Some(ref ws_url) = request.ws_url {
        validate_url(ws_url).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    }
    validate_weight(request.weight).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Convert to core request
    let core_request = CoreCreateUpstreamRequest {
        name: request.name.clone(),
        url: request.url,
        ws_url: request.ws_url,
        weight: request.weight,
        chain_id: request.chain_id,
        timeout_seconds: request.timeout_seconds,
    };

    // Create dynamic upstream
    let dynamic_config = state
        .proxy_engine
        .get_upstream_manager()
        .add_dynamic_upstream(core_request)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Convert to response
    let response = UpstreamResponse {
        id: dynamic_config.id.clone(),
        name: dynamic_config.name.clone(),
        url: mask_url(&dynamic_config.url),
        ws_url: dynamic_config.ws_url.as_ref().map(|u| mask_url(u)),
        weight: dynamic_config.weight,
        chain_id: dynamic_config.chain_id,
        timeout_seconds: dynamic_config.timeout_seconds,
        enabled: dynamic_config.enabled,
        created_at: dynamic_config.created_at,
        updated_at: dynamic_config.updated_at,
    };

    // Audit log the creation
    audit::log_create("upstream", &response.id, Some(addr));

    tracing::info!(
        correlation_id = ?correlation_id,
        id = %response.id,
        name = %response.name,
        "created dynamic upstream via admin API"
    );

    Ok(Json(response))
}

/// PUT /admin/upstreams/:id
///
/// Updates an existing dynamic upstream.
#[utoipa::path(
    put,
    path = "/admin/upstreams/{id}",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID")
    ),
    request_body = crate::admin::types::UpdateUpstreamRequest,
    responses(
        (status = 200, description = "Upstream updated", body = UpstreamResponse),
        (status = 400, description = "Invalid request"),
        (status = 403, description = "Cannot modify config-based upstream"),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn update_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    Json(request): Json<crate::admin::types::UpdateUpstreamRequest>,
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Validate optional fields if provided
    if let Some(ref name) = request.name {
        validate_name(name).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    }
    if let Some(ref url) = request.url {
        validate_url(url).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    }
    if let Some(weight) = request.weight {
        validate_weight(weight).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    }

    // Check if this is a config-based upstream
    let manager = state.proxy_engine.get_upstream_manager();
    let registry = manager.get_dynamic_registry();

    // Verify it exists and is a dynamic upstream
    if registry.get(&id).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            "Dynamic upstream not found. Config-based upstreams cannot be modified via API."
                .to_string(),
        ));
    }

    // Convert to core request
    let core_request = CoreUpdateUpstreamRequest {
        name: request.name,
        url: request.url,
        weight: request.weight,
        enabled: request.enabled,
    };

    // Update dynamic upstream
    let dynamic_config = manager.update_dynamic_upstream(&id, &core_request).map_err(|e| {
        if e.to_string().contains("not found") {
            (StatusCode::NOT_FOUND, e.to_string())
        } else {
            (StatusCode::BAD_REQUEST, e.to_string())
        }
    })?;

    // Convert to response
    let response = UpstreamResponse {
        id: dynamic_config.id.clone(),
        name: dynamic_config.name.clone(),
        url: mask_url(&dynamic_config.url),
        ws_url: dynamic_config.ws_url.as_ref().map(|u| mask_url(u)),
        weight: dynamic_config.weight,
        chain_id: dynamic_config.chain_id,
        timeout_seconds: dynamic_config.timeout_seconds,
        enabled: dynamic_config.enabled,
        created_at: dynamic_config.created_at,
        updated_at: dynamic_config.updated_at,
    };

    // Audit log the update
    audit::log_update("upstream", &response.id, Some(addr), None);

    tracing::info!(
        correlation_id = ?correlation_id,
        id = %response.id,
        name = %response.name,
        "updated dynamic upstream via admin API"
    );

    Ok(Json(response))
}

/// DELETE /admin/upstreams/:id
///
/// Deletes a dynamic upstream.
#[utoipa::path(
    delete,
    path = "/admin/upstreams/{id}",
    tag = "Upstreams",
    params(
        ("id" = String, Path, description = "Upstream ID")
    ),
    responses(
        (status = 200, description = "Upstream deleted", body = SuccessResponse),
        (status = 403, description = "Cannot delete config-based upstream"),
        (status = 404, description = "Upstream not found")
    )
)]
pub async fn delete_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<Json<SuccessResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    // Check if this is a config-based upstream
    let manager = state.proxy_engine.get_upstream_manager();
    let registry = manager.get_dynamic_registry();

    // Verify it exists and is a dynamic upstream
    if registry.get(&id).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            "Dynamic upstream not found. Config-based upstreams cannot be deleted via API."
                .to_string(),
        ));
    }

    // Remove dynamic upstream
    manager.remove_dynamic_upstream(&id).map_err(|e| {
        if e.to_string().contains("not found") {
            (StatusCode::NOT_FOUND, e.to_string())
        } else {
            (StatusCode::FORBIDDEN, e.to_string())
        }
    })?;

    // Audit log the deletion
    audit::log_delete("upstream", &id, Some(addr));

    tracing::info!(
        correlation_id = ?correlation_id,
        id = %id,
        "deleted dynamic upstream via admin API"
    );

    Ok(Json(SuccessResponse { success: true }))
}
