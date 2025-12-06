//! Admin API response types.
//!
//! These types match the TypeScript interfaces from the admin-api-specs.md document.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// System health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SystemHealth {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Upstream status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CircuitBreakerState {
    Closed,
    HalfOpen,
    Open,
}

/// Trend direction for KPI metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Trend {
    Up,
    Down,
    Stable,
}

/// System status response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub instance_name: String,
    pub health: SystemHealth,
    pub uptime: String,
    pub chain_tip: u64,
    pub chain_tip_age: String,
    pub finalized_block: u64,
    pub version: String,
}

/// System info response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub instance_name: String,
    pub version: String,
    pub build_date: String,
    pub git_commit: String,
    pub features: Vec<String>,
    pub capabilities: Capabilities,
    pub environment: String,
}

/// System capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub supports_web_socket: bool,
    pub supports_batch: bool,
    pub max_batch_size: u32,
}

/// Simple health check response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthCheck {
    pub status: String,
    pub timestamp: String,
}

/// Server settings response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    pub bind_address: String,
    pub port: u16,
    pub max_concurrent_requests: usize,
    pub request_timeout: u64,
    pub keep_alive_timeout: u64,
}

/// Request body for updating server settings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateServerSettingsRequest {
    /// Maximum concurrent requests (cannot be changed at runtime)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests: Option<usize>,
    /// Request timeout in milliseconds (cannot be changed at runtime)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout: Option<u64>,
}

/// Response for update operations that require restart.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettingsResponse {
    pub success: bool,
    pub message: String,
    pub requires_restart: bool,
}

/// Upstream information.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Upstream {
    pub id: String,
    pub name: String,
    pub url: String,
    pub status: UpstreamStatus,
    pub circuit_breaker_state: CircuitBreakerState,
    pub composite_score: f64,
    pub latency_p90: u64,
    pub error_rate: f64,
    pub throttle_rate: f64,
    pub block_lag: i64,
    pub requests_handled: u64,
    pub weight: u32,
    pub ws_connected: bool,
}

/// Upstream scoring factor breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamScoringFactor {
    pub factor: String,
    pub weight: f64,
    pub score: f64,
    pub value: String,
}

/// Circuit breaker metrics.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerMetrics {
    pub state: CircuitBreakerState,
    pub failure_threshold: u32,
    pub current_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_remaining: Option<u64>,
}

/// Upstream metrics response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamMetrics {
    pub upstream: Upstream,
    pub scoring_breakdown: Vec<UpstreamScoringFactor>,
    pub circuit_breaker: CircuitBreakerMetrics,
    pub request_distribution: Vec<UpstreamRequestDistribution>,
}

/// Request distribution by method.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamRequestDistribution {
    pub method: String,
    pub requests: u64,
    pub success: u64,
    pub failed: u64,
}

/// Block cache statistics.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlockCacheStats {
    pub hot_window_size: usize,
    pub lru_entries: usize,
    pub memory_usage: String,
    pub hit_rate: f64,
}

/// Log cache statistics.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogCacheStats {
    pub chunk_count: usize,
    pub indexed_blocks: usize,
    pub memory_usage: String,
    pub partial_fulfillment: f64,
}

/// Transaction cache statistics.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransactionCacheStats {
    pub entries: usize,
    pub receipt_entries: usize,
    pub memory_usage: String,
    pub hit_rate: f64,
}

/// Overall cache statistics.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub block_cache: BlockCacheStats,
    pub log_cache: LogCacheStats,
    pub transaction_cache: TransactionCacheStats,
}

/// Cache hit data point for time series.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CacheHitDataPoint {
    pub timestamp: String,
    pub hit: f64,
    pub partial: f64,
    pub miss: f64,
}

/// Cache hit rate by method.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CacheHitByMethod {
    pub method: String,
    pub hit_rate: f64,
}

/// Memory allocation breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAllocation {
    pub label: String,
    pub value: u64,
    pub color: String,
}

/// Cache settings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CacheSettingsResponse {
    pub retain_blocks: usize,
    pub block_cache_max_size: String,
    pub log_cache_max_size: String,
    pub transaction_cache_max_size: String,
    pub cleanup_interval: u64,
}

/// KPI metric for dashboard.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KPIMetric {
    pub id: String,
    pub label: String,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<f64>,
    pub trend: Trend,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sparkline: Option<Vec<f64>>,
}

/// Generic time series data point.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeSeriesPoint {
    pub timestamp: String,
    pub value: f64,
}

/// Latency data point with percentiles.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LatencyDataPoint {
    pub timestamp: String,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

/// Request method distribution.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestMethodData {
    pub method: String,
    pub count: u64,
    pub percentage: f64,
}

/// Error distribution by type.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDistribution {
    #[serde(rename = "type")]
    pub error_type: String,
    pub count: u64,
    pub percentage: f64,
}

/// Hedging statistics.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HedgingStats {
    pub hedged_requests: f64,
    pub hedge_triggers: f64,
    pub primary_wins: f64,
    pub p99_improvement: f64,
    pub winner_distribution: WinnerDistribution,
}

/// Winner distribution for hedging.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WinnerDistribution {
    pub primary: f64,
    pub hedged: f64,
}

/// Query parameters for time range.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TimeRangeQuery {
    #[serde(rename = "timeRange", default = "default_time_range")]
    pub time_range: String,
}

fn default_time_range() -> String {
    "24h".to_string()
}

/// Query parameters for pagination.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PaginationQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize {
    100
}

/// Success response for actions.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SuccessResponse {
    pub success: bool,
}

/// Health check result for a single upstream.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckResult {
    pub upstream_id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Bulk health check response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkHealthCheckResponse {
    pub success: bool,
    pub checked: usize,
    pub results: Vec<HealthCheckResult>,
}

/// Log entry for system logging.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub target: String,
    pub message: String,
    pub fields: serde_json::Value,
}

/// Query parameters for log filtering.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogQueryParams {
    pub level: Option<String>,
    pub target: Option<String>,
    pub search: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(default = "default_log_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_log_limit() -> usize {
    100
}

/// Response for log queries.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogQueryResponse {
    pub entries: Vec<LogEntry>,
    pub total: usize,
    pub has_more: bool,
}

/// Response for log services/targets.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogServicesResponse {
    pub services: Vec<String>,
}

// --- Write Operation Request Types ---

/// Request to create a new upstream.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateUpstreamRequest {
    pub name: String,
    pub url: String,
    pub ws_url: Option<String>,
    pub weight: u32,
    pub chain_id: u64,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    30
}

/// Request to update an existing upstream.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUpstreamRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub weight: Option<u32>,
    pub enabled: Option<bool>,
}

/// Response for upstream creation/update.
#[derive(Debug, Clone, serde::Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamResponse {
    pub id: String,
    pub name: String,
    pub url: String,
    pub ws_url: Option<String>,
    pub weight: u32,
    pub chain_id: u64,
    pub timeout_seconds: u64,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to update upstream weight.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWeightRequest {
    pub weight: u32,
}

/// Request to clear specific cache types.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClearCacheRequest {
    pub cache_types: Vec<String>,
}

/// Request to update cache settings at runtime.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCacheSettingsRequest {
    pub retain_blocks: Option<u64>,
    pub cleanup_interval: Option<u64>,
}

// --- API Key Management Types ---

/// API key response (masks the full key).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyResponse {
    pub id: i64,
    pub name: String,
    pub key_prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked: bool,
    pub allowed_methods: Vec<String>,
}

/// API key response with full key (only shown on creation).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyCreatedResponse {
    pub id: i64,
    pub name: String,
    pub key: String,
    pub created_at: String,
}

/// Request to create a new API key.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub allowed_methods: Option<Vec<String>>,
}

/// Request to update an API key.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateApiKeyRequest {
    pub name: Option<String>,
    pub allowed_methods: Option<Vec<String>>,
}

/// Health history entry for upstream health checks.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthHistoryEntry {
    pub timestamp: String,
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// API key usage statistics response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyUsageResponse {
    pub key_id: i64,
    pub total_requests: u64,
    pub requests_today: u64,
    pub last_used: Option<String>,
    pub usage_by_method: Vec<MethodUsage>,
    pub daily_usage: Vec<DailyUsage>,
}

/// Usage statistics for a specific method.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MethodUsage {
    pub method: String,
    pub count: u64,
    pub percentage: f64,
}

/// Daily usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub date: String,
    pub requests: u64,
}
