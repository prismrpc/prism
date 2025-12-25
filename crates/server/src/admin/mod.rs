//! Admin API module.
//!
//! Provides a separate HTTP server for monitoring and managing the Prism RPC server.
//! Runs on a different port from the main RPC server but shares the same process
//! and state (via `Arc` references).

#![allow(clippy::needless_for_each)]

pub mod audit;
pub mod handlers;
pub mod logging;
pub mod prometheus;
pub mod rate_limit;
pub mod types;

use std::{sync::Arc, time::Instant};

use axum::{
    http::HeaderMap,
    middleware as axum_middleware,
    routing::{get, post, put},
    Router,
};
use prism_core::{
    auth::repository::SqliteRepository, chain::ChainState, config::AppConfig,
    middleware::ApiKeyAuth, proxy::ProxyEngine,
};

use crate::{admin::logging::LogBuffer, middleware::AdminAuthState};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

/// Shared state for admin handlers.
///
/// Contains references to all shared components plus admin-specific data
/// like server start time and build information.
#[derive(Clone)]
pub struct AdminState {
    /// Reference to the proxy engine (contains cache, upstream, metrics managers).
    pub proxy_engine: Arc<ProxyEngine>,

    /// Reference to the application configuration.
    pub config: Arc<AppConfig>,

    /// Reference to the chain state.
    pub chain_state: Arc<ChainState>,

    /// Optional API key repository (only available if auth is enabled).
    pub api_key_repo: Option<Arc<SqliteRepository>>,

    /// Optional API key auth system (reused from RPC auth).
    pub api_key_auth: Option<Arc<ApiKeyAuth>>,

    /// Log buffer for querying recent logs.
    pub log_buffer: Arc<LogBuffer>,

    /// Optional Prometheus client for querying historical metrics.
    pub prometheus_client: Option<Arc<prometheus::PrometheusClient>>,

    /// Server start time for uptime calculation.
    pub start_time: Instant,

    /// Application version from `Cargo.toml`.
    pub version: &'static str,

    /// Git commit hash (if available at build time).
    pub git_commit: &'static str,

    /// Build date (if available at build time).
    pub build_date: &'static str,
}

impl AdminState {
    /// Creates a new admin state.
    #[must_use]
    pub fn new(
        proxy_engine: Arc<ProxyEngine>,
        config: Arc<AppConfig>,
        chain_state: Arc<ChainState>,
        api_key_repo: Option<Arc<SqliteRepository>>,
        api_key_auth: Option<Arc<ApiKeyAuth>>,
        log_buffer: Arc<LogBuffer>,
    ) -> Self {
        // Initialize Prometheus client if URL is configured
        let prometheus_client = config.admin.prometheus_url.as_ref().and_then(|url| {
            match prometheus::PrometheusClient::new(url) {
                Ok(client) => {
                    tracing::info!("initialized Prometheus client for {}", url);
                    Some(Arc::new(client))
                }
                Err(e) => {
                    tracing::warn!("failed to initialize Prometheus client: {}", e);
                    None
                }
            }
        });

        Self {
            proxy_engine,
            config,
            chain_state,
            api_key_repo,
            api_key_auth,
            log_buffer,
            prometheus_client,
            start_time: Instant::now(),
            version: env!("CARGO_PKG_VERSION"),
            git_commit: option_env!("GIT_COMMIT").unwrap_or("unknown"),
            build_date: option_env!("BUILD_DATE").unwrap_or("unknown"),
        }
    }

    /// Returns a human-readable uptime string.
    #[must_use]
    pub fn uptime_string(&self) -> String {
        let secs = self.start_time.elapsed().as_secs();
        let days = secs / 86400;
        let hours = (secs % 86400) / 3600;
        let mins = (secs % 3600) / 60;
        format!("{days}d {hours}h {mins}m")
    }
}

/// Extracts the correlation ID from request headers.
///
/// The correlation ID is set by the `correlation_id` middleware
/// and is available via the `X-Request-ID` header.
#[must_use]
pub fn get_correlation_id(headers: &HeaderMap) -> Option<String> {
    headers.get("x-request-id").and_then(|v| v.to_str().ok()).map(String::from)
}

/// `OpenAPI` documentation for the Prism Admin API.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Prism Admin API",
        version = "1.0.0",
        description = "Admin API for monitoring and managing the Prism RPC server"
    ),
    paths(
        // System endpoints
        handlers::system::get_status,
        handlers::system::get_info,
        handlers::system::get_health,
        handlers::system::get_settings,
        handlers::system::update_settings,
        // Upstream endpoints
        handlers::upstreams::list_upstreams,
        handlers::upstreams::get_upstream,
        handlers::upstreams::get_upstream_metrics,
        handlers::upstreams::trigger_health_check,
        handlers::upstreams::trigger_all_health_checks,
        handlers::upstreams::reset_circuit_breaker,
        handlers::upstreams::update_weight,
        handlers::upstreams::get_latency_distribution,
        handlers::upstreams::get_health_history,
        handlers::upstreams::get_request_distribution,
        handlers::upstreams::create_upstream,
        handlers::upstreams::update_upstream,
        handlers::upstreams::delete_upstream,
        // Metrics endpoints
        handlers::metrics::get_kpis,
        handlers::metrics::get_latency,
        handlers::metrics::get_request_volume,
        handlers::metrics::get_request_methods,
        handlers::metrics::get_error_distribution,
        handlers::metrics::get_hedging_stats,
        // Log endpoints
        handlers::logs::query_logs,
        handlers::logs::list_log_services,
        handlers::logs::export_logs,
        // Alert endpoints
        handlers::alerts::list_alerts,
        handlers::alerts::get_alert,
        handlers::alerts::acknowledge_alert,
        handlers::alerts::resolve_alert,
        handlers::alerts::dismiss_alert,
        handlers::alerts::bulk_resolve_alerts,
        handlers::alerts::bulk_dismiss_alerts,
        handlers::alerts::list_rules,
        handlers::alerts::create_rule,
        handlers::alerts::update_rule,
        handlers::alerts::delete_rule,
        handlers::alerts::toggle_rule,
        // API key endpoints
        handlers::apikeys::list_api_keys,
        handlers::apikeys::get_api_key,
        handlers::apikeys::create_api_key,
        handlers::apikeys::update_api_key,
        handlers::apikeys::delete_api_key,
        handlers::apikeys::revoke_api_key,
        handlers::apikeys::get_api_key_usage,
        // Config endpoints
        handlers::config::export_config,
        handlers::config::persist_config,
        // Cache endpoints
        handlers::cache::get_stats,
        handlers::cache::get_hit_rate,
        handlers::cache::get_hit_by_method,
        handlers::cache::get_memory_allocation,
        handlers::cache::get_settings,
        handlers::cache::clear_cache,
        handlers::cache::update_settings,
    ),
    components(schemas(
        types::SystemStatus,
        types::SystemInfo,
        types::SystemHealth,
        types::Capabilities,
        types::HealthCheck,
        types::ServerSettings,
        types::UpdateServerSettingsRequest,
        types::UpdateSettingsResponse,
        types::Upstream,
        types::UpstreamStatus,
        types::CircuitBreakerState,
        types::UpstreamMetrics,
        types::UpstreamScoringFactor,
        types::CircuitBreakerMetrics,
        types::UpstreamRequestDistribution,
        types::BulkHealthCheckResponse,
        types::HealthCheckResult,
        types::HealthHistoryEntry,
        types::SuccessResponse,
        types::CreateUpstreamRequest,
        types::UpdateUpstreamRequest,
        types::UpstreamResponse,
        types::CacheStats,
        types::BlockCacheStats,
        types::LogCacheStats,
        types::TransactionCacheStats,
        types::CacheHitDataPoint,
        types::CacheHitByMethod,
        types::MemoryAllocation,
        types::CacheSettingsResponse,
        types::KPIMetric,
        types::Trend,
        types::TimeSeriesPoint,
        types::TimeRangeQuery,
        types::LatencyDataPoint,
        types::RequestMethodData,
        types::ErrorDistribution,
        types::HedgingStats,
        types::WinnerDistribution,
        types::DataSource,
        types::MetricsDataResponse<Vec<types::LatencyDataPoint>>,
        types::MetricsDataResponse<Vec<types::TimeSeriesPoint>>,
        types::MetricsDataResponse<Vec<types::RequestMethodData>>,
        types::MetricsDataResponse<Vec<types::ErrorDistribution>>,
        types::MetricsDataResponse<types::HedgingStats>,
        types::LogEntry,
        types::LogQueryResponse,
        types::LogServicesResponse,
        types::ApiKeyResponse,
        types::ApiKeyCreatedResponse,
        types::CreateApiKeyRequest,
        types::UpdateApiKeyRequest,
        types::ApiKeyUsageResponse,
        types::MethodUsage,
        types::DailyUsage,
        types::TimeRangeQuery,
        types::UpdateWeightRequest,
        types::ClearCacheRequest,
        types::UpdateCacheSettingsRequest,
        types::AdminApiError,
        prism_core::alerts::Alert,
        prism_core::alerts::AlertRule,
        prism_core::alerts::AlertStatus,
        prism_core::alerts::AlertSeverity,
        prism_core::alerts::AlertCondition,
        crate::admin::handlers::alerts::ListAlertsQuery,
        crate::admin::handlers::alerts::CreateAlertRuleRequest,
        crate::admin::handlers::alerts::UpdateAlertRuleRequest,
        crate::admin::handlers::alerts::SuccessResponse,
        crate::admin::handlers::alerts::ToggleResponse,
        crate::admin::handlers::alerts::BulkAlertRequest,
        crate::admin::handlers::alerts::BulkAlertResponse,
        crate::admin::handlers::config::ConfigExportQuery,
        crate::admin::handlers::config::PersistConfigRequest,
        crate::admin::handlers::config::PersistConfigResponse,
    )),
    tags(
        (name = "System", description = "System status and configuration endpoints"),
        (name = "Config", description = "Configuration export and management"),
        (name = "Upstreams", description = "Upstream provider management"),
        (name = "Cache", description = "Cache statistics and management"),
        (name = "Metrics", description = "Performance metrics and analytics"),
        (name = "Alerts", description = "Alert management"),
        (name = "API Keys", description = "API key management"),
        (name = "Logs", description = "Log querying and export")
    )
)]
pub struct ApiDoc;

/// Creates the admin API router with all endpoints.
#[allow(clippy::too_many_lines)]
pub fn create_admin_router(state: AdminState) -> Router {
    // Create admin auth state for the unified auth middleware
    let admin_auth_state = AdminAuthState::new(state.api_key_auth.clone());

    // Create rate limiter if configured
    let rate_limiter = state
        .config
        .admin
        .rate_limit_max_tokens
        .zip(state.config.admin.rate_limit_refill_rate)
        .map(|(max, rate)| Arc::new(rate_limit::RateLimiter::new(max, rate)));

    let router = Router::new()
        // System endpoints
        .route("/admin/system/status", get(handlers::system::get_status))
        .route("/admin/system/info", get(handlers::system::get_info))
        .route("/admin/system/health", get(handlers::system::get_health))
        .route(
            "/admin/system/settings",
            get(handlers::system::get_settings).put(handlers::system::update_settings),
        )
        // Upstream endpoints
        .route(
            "/admin/upstreams",
            get(handlers::upstreams::list_upstreams).post(handlers::upstreams::create_upstream),
        )
        .route(
            "/admin/upstreams/{id}",
            get(handlers::upstreams::get_upstream)
                .put(handlers::upstreams::update_upstream)
                .delete(handlers::upstreams::delete_upstream),
        )
        .route("/admin/upstreams/{id}/metrics", get(handlers::upstreams::get_upstream_metrics))
        // Upstream actions
        .route(
            "/admin/upstreams/{id}/health-check",
            post(handlers::upstreams::trigger_health_check),
        )
        .route(
            "/admin/upstreams/health-check",
            post(handlers::upstreams::trigger_all_health_checks),
        )
        .route(
            "/admin/upstreams/{id}/circuit-breaker/reset",
            put(handlers::upstreams::reset_circuit_breaker),
        )
        .route("/admin/upstreams/{id}/weight", put(handlers::upstreams::update_weight))
        .route(
            "/admin/upstreams/{id}/latency-distribution",
            get(handlers::upstreams::get_latency_distribution),
        )
        .route("/admin/upstreams/{id}/health-history", get(handlers::upstreams::get_health_history))
        .route(
            "/admin/upstreams/{id}/request-distribution",
            get(handlers::upstreams::get_request_distribution),
        )
        // Cache endpoints
        .route("/admin/cache/stats", get(handlers::cache::get_stats))
        .route("/admin/cache/hit-rate", get(handlers::cache::get_hit_rate))
        .route("/admin/cache/hit-by-method", get(handlers::cache::get_hit_by_method))
        .route("/admin/cache/memory-allocation", get(handlers::cache::get_memory_allocation))
        .route(
            "/admin/cache/settings",
            get(handlers::cache::get_settings).put(handlers::cache::update_settings),
        )
        .route("/admin/cache/clear", post(handlers::cache::clear_cache))
        // Metrics endpoints
        .route("/admin/metrics/kpis", get(handlers::metrics::get_kpis))
        .route("/admin/metrics/latency", get(handlers::metrics::get_latency))
        .route("/admin/metrics/request-volume", get(handlers::metrics::get_request_volume))
        .route("/admin/metrics/request-methods", get(handlers::metrics::get_request_methods))
        .route("/admin/metrics/error-distribution", get(handlers::metrics::get_error_distribution))
        .route("/admin/metrics/hedging-stats", get(handlers::metrics::get_hedging_stats))
        // Log endpoints
        .route("/admin/logs", get(handlers::logs::query_logs))
        .route("/admin/logs/services", get(handlers::logs::list_log_services))
        .route("/admin/logs/export", get(handlers::logs::export_logs))
        // Alert endpoints
        .route("/admin/alerts", get(handlers::alerts::list_alerts))
        .route(
            "/admin/alerts/{id}",
            get(handlers::alerts::get_alert).delete(handlers::alerts::dismiss_alert),
        )
        .route("/admin/alerts/{id}/acknowledge", post(handlers::alerts::acknowledge_alert))
        .route("/admin/alerts/{id}/resolve", post(handlers::alerts::resolve_alert))
        // Bulk alert endpoints
        .route("/admin/alerts/bulk-resolve", post(handlers::alerts::bulk_resolve_alerts))
        .route("/admin/alerts/bulk-dismiss", post(handlers::alerts::bulk_dismiss_alerts))
        // Config endpoints
        .route("/admin/config/export", get(handlers::config::export_config))
        .route("/admin/config/persist", post(handlers::config::persist_config))
        // Alert rule endpoints
        .route(
            "/admin/alerts/rules",
            get(handlers::alerts::list_rules).post(handlers::alerts::create_rule),
        )
        .route(
            "/admin/alerts/rules/{id}",
            put(handlers::alerts::update_rule).delete(handlers::alerts::delete_rule),
        )
        .route("/admin/alerts/rules/{id}/toggle", put(handlers::alerts::toggle_rule));

    // API key management endpoints (only if auth is enabled)
    let router = if state.api_key_repo.is_some() {
        router
            .route(
                "/admin/apikeys",
                get(handlers::apikeys::list_api_keys).post(handlers::apikeys::create_api_key),
            )
            .route(
                "/admin/apikeys/{id}",
                get(handlers::apikeys::get_api_key)
                    .put(handlers::apikeys::update_api_key)
                    .delete(handlers::apikeys::delete_api_key),
            )
            .route("/admin/apikeys/{id}/revoke", post(handlers::apikeys::revoke_api_key))
            .route("/admin/apikeys/{id}/usage", get(handlers::apikeys::get_api_key_usage))
    } else {
        router
    };

    // Apply middleware layers (rate limiting first, then authentication)
    let router = router.layer(axum_middleware::from_fn_with_state(
        admin_auth_state,
        crate::middleware::admin_auth_middleware,
    ));

    // Add rate limiting if configured (applied before auth middleware in execution order)
    let router = if let Some(limiter) = rate_limiter {
        router
            .layer(axum_middleware::from_fn_with_state(limiter, rate_limit::rate_limit_middleware))
    } else {
        router
    };

    router
        .merge(
            SwaggerUi::new("/admin/swagger-ui")
                .url("/admin/api-docs/openapi.json", ApiDoc::openapi()),
        )
        .with_state(state)
}
