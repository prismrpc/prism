# Rust Implementation Guide for Admin API Gaps

This document provides specific Rust implementation patterns for addressing the Admin API gaps in the Prism RPC Proxy project.

---

## 1. Prometheus Client Enhancements

**File**: `/home/flo/workspace/personal/prism/crates/server/src/admin/prometheus.rs`

### Missing Query Methods

The following methods need to be added to the `PrometheusClient` implementation:

#### 1.1 `get_upstream_latency`

```rust
/// Gets latency percentiles for a specific upstream over the specified time range.
///
/// # Arguments
///
/// * `upstream_id` - Upstream name to filter by
/// * `time_range` - Duration to look back
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn get_upstream_latency(
    &self,
    upstream_id: &str,
    time_range: Duration,
) -> Result<Vec<LatencyDataPoint>, PrometheusError> {
    // This method already exists as `get_upstream_latency_percentiles`
    // located at lines 356-424. It can be aliased or the name adjusted.
    self.get_upstream_latency_percentiles(upstream_id, time_range).await
}
```

**Note**: This functionality already exists as `get_upstream_latency_percentiles()` on line 356. You can either:
- Add an alias method with the new name
- Update documentation to reference the existing method
- Rename the existing method if desired

#### 1.2 `get_upstream_request_distribution`

```rust
/// Gets request distribution by method for a specific upstream over the specified time range.
///
/// # Arguments
///
/// * `upstream_id` - Upstream name to filter by
/// * `time_range` - Duration to look back
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn get_upstream_request_distribution(
    &self,
    upstream_id: &str,
    time_range: Duration,
) -> Result<Vec<RequestMethodData>, PrometheusError> {
    // This method already exists as `get_upstream_request_by_method`
    // located at lines 661-732
    self.get_upstream_request_by_method(upstream_id, time_range).await
}
```

**Note**: This functionality already exists as `get_upstream_request_by_method()` on line 661. Same options as above.

#### 1.3 `get_latency_distribution` (wrapper)

```rust
/// Gets latency percentiles over the specified time range.
///
/// # Arguments
///
/// * `time_range` - Duration to look back
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn get_latency_distribution(
    &self,
    time_range: Duration,
) -> Result<Vec<LatencyDataPoint>, PrometheusError> {
    // This already exists as `get_latency_percentiles()` on line 293
    self.get_latency_percentiles(time_range).await
}
```

#### 1.4 `get_request_volume` (wrapper)

```rust
/// Gets the request volume time series for the specified time range.
///
/// # Arguments
///
/// * `time_range` - Duration to look back
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn get_request_volume(
    &self,
    time_range: Duration,
) -> Result<Vec<TimeSeriesPoint>, PrometheusError> {
    // This already exists as `get_request_rate()` on line 269
    self.get_request_rate(time_range).await
}
```

#### 1.5 `get_request_methods` (wrapper)

```rust
/// Gets request distribution by RPC method with counts and percentages.
///
/// # Arguments
///
/// * `time_range` - Duration to look back
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn get_request_methods(
    &self,
    time_range: Duration,
) -> Result<Vec<RequestMethodData>, PrometheusError> {
    // This already exists as `get_request_by_method()` on line 583
    self.get_request_by_method(time_range).await
}
```

### Summary

**All required Prometheus query methods already exist!** They just have slightly different names:

| Required Name                        | Existing Method                      | Line |
|--------------------------------------|--------------------------------------|------|
| `get_upstream_latency`               | `get_upstream_latency_percentiles`   | 356  |
| `get_upstream_request_distribution`  | `get_upstream_request_by_method`     | 661  |
| `get_latency_distribution`           | `get_latency_percentiles`            | 293  |
| `get_request_volume`                 | `get_request_rate`                   | 269  |
| `get_request_methods`                | `get_request_by_method`              | 583  |
| `get_error_distribution`             | `get_error_distribution`             | 504  |
| `get_hedging_stats`                  | `get_hedging_stats`                  | 743  |

**Recommendation**: Add type aliases or documentation updates rather than duplicating code.

---

## 2. Metrics Handler - Fallback Pattern

**File**: `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/metrics.rs`

### Current Implementation Analysis

The metrics handlers already implement the fallback pattern correctly:

```rust
// Example from get_latency() handler (lines 158-202)
pub async fn get_latency(
    State(state): State<AdminState>,
    axum::extract::Query(query): axum::extract::Query<TimeRangeQuery>,
) -> impl IntoResponse {
    // Try Prometheus first
    if let Some(ref prom_client) = state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);
        match prom_client.get_latency_percentiles(time_range).await {
            Ok(data) if !data.is_empty() => {
                return Json(MetricsDataResponse::from_prometheus(data));
            }
            Ok(_) => {
                tracing::debug!("no latency data from Prometheus");
            }
            Err(e) => {
                tracing::warn!("failed to query Prometheus for latency: {}", e);
            }
        }
    }

    // Fallback to in-memory metrics
    let metrics_collector = state.proxy_engine.get_metrics_collector();
    let metrics_summary = metrics_collector.get_metrics_summary().await;

    // Build fallback response from in-memory data
    let data: Vec<LatencyDataPoint> = vec![LatencyDataPoint {
        timestamp: chrono::Utc::now().to_rfc3339(),
        p50: metrics_summary.average_latency_ms,
        p95: metrics_summary.average_latency_ms,
        p99: metrics_summary.average_latency_ms,
    }];

    let warning = if state.prometheus_client.is_some() {
        "Prometheus query failed, showing in-memory snapshot"
    } else {
        "Prometheus not configured, showing in-memory snapshot"
    };

    Json(MetricsDataResponse {
        data,
        source: DataSource::InMemory,
        warning: Some(warning.to_string()),
    })
}
```

### Fallback Pattern Template

For any new metrics endpoint, use this pattern:

```rust
pub async fn get_some_metric(
    State(state): State<AdminState>,
    axum::extract::Query(query): axum::extract::Query<TimeRangeQuery>,
) -> impl IntoResponse {
    // 1. Try Prometheus if configured
    if let Some(ref prom_client) = state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);
        match prom_client.get_some_metric(time_range).await {
            Ok(data) if !data.is_empty() => {
                return Json(MetricsDataResponse::from_prometheus(data));
            }
            Ok(_) => {
                tracing::debug!("no data from Prometheus for some_metric");
            }
            Err(e) => {
                tracing::warn!("failed to query Prometheus for some_metric: {}", e);
            }
        }
    }

    // 2. Fallback to in-memory MetricsCollector
    let metrics_collector = state.proxy_engine.get_metrics_collector();
    let metrics_summary = metrics_collector.get_metrics_summary().await;

    // 3. Transform MetricsSummary data into response format
    let data = transform_metrics_summary_to_response(&metrics_summary);

    // 4. Return with appropriate warning
    let warning = if state.prometheus_client.is_some() {
        "Prometheus query failed, showing in-memory data"
    } else {
        "Prometheus not configured, showing in-memory data"
    };

    Json(MetricsDataResponse {
        data,
        source: DataSource::InMemory,
        warning: Some(warning.to_string()),
    })
}
```

### Key Points

1. **Never panic** - All fallbacks are `Option<T>` or `Result<T, E>`
2. **Always log** - Use `tracing::debug!` for expected fallbacks, `tracing::warn!` for errors
3. **Data source transparency** - Always include `source` and `warning` fields in response
4. **Access pattern**: `state.prometheus_client` is `Option<PrometheusClient>`

---

## 3. Upstream Endpoint - Health History Ring Buffer

**File**: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/endpoint.rs`

### Current Implementation

The health history ring buffer is **already implemented**! See lines 30-40 and 502-540.

```rust
// Type definition (lines 31-37)
#[derive(Debug, Clone)]
pub struct HealthHistoryEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub healthy: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

const HEALTH_HISTORY_SIZE: usize = 100;

// Storage in UpstreamEndpoint struct (line 63)
pub struct UpstreamEndpoint {
    // ... other fields ...
    health_history: Arc<RwLock<VecDeque<HealthHistoryEntry>>>,
    // ... other fields ...
}

// Recording method (lines 502-526)
async fn record_health_history(
    &self,
    healthy: bool,
    latency_ms: Option<u64>,
    error: Option<String>,
) {
    let mut history = self.health_history.write().await;

    // Add new entry
    history.push_back(HealthHistoryEntry {
        timestamp: chrono::Utc::now(),
        healthy,
        latency_ms,
        error,
    });

    // Evict oldest entry if buffer is full
    if history.len() > HEALTH_HISTORY_SIZE {
        history.pop_front();
    }
}

// Public getter (lines 528-540)
pub async fn get_health_history(&self, limit: usize) -> Vec<HealthHistoryEntry> {
    let history = self.health_history.read().await;

    // Return newest entries first
    history.iter().rev().take(limit).cloned().collect()
}
```

### Thread Safety Analysis

**Current Implementation**: ✅ **Correct and optimal**

- **Type**: `Arc<RwLock<VecDeque<HealthHistoryEntry>>>`
- **Why Arc?**: Allows sharing across multiple async tasks
- **Why RwLock?**: Allows multiple concurrent readers, single writer
- **Why not Mutex?**: `RwLock` allows parallel reads of history (common case)
- **Why VecDeque?**: Efficient ring buffer with O(1) push/pop on both ends

### Usage in Admin API

The health history is already exposed via the upstream handlers. To use it:

```rust
// In admin handlers
let endpoint: &UpstreamEndpoint = /* get from load balancer */;
let history = endpoint.get_health_history(50).await;

// Convert to admin API types
let api_history: Vec<crate::admin::types::HealthHistoryEntry> = history
    .into_iter()
    .map(|entry| crate::admin::types::HealthHistoryEntry {
        timestamp: entry.timestamp.to_rfc3339(),
        healthy: entry.healthy,
        latency_ms: entry.latency_ms,
        error: entry.error,
    })
    .collect();
```

### No Changes Needed

The implementation is already production-ready and follows Rust best practices:
- Lock-free reads via `Arc`
- Efficient write locking via `RwLock`
- Bounded memory via ring buffer with eviction
- Thread-safe sharing via `Arc`

---

## 4. Cache Handler - Hardcoded Values Fix

**File**: `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/cache.rs`

### Issue: Hardcoded Memory Estimation at Line 270-271

```rust
// Current implementation (lines 268-271) - HARDCODED VALUES
let log_max_bytes =
    config.log_cache.max_exact_results * 1024 +  // ❌ Hardcoded 1KB
    config.log_cache.max_bitmap_entries * 100;   // ❌ Hardcoded 100 bytes
```

### Solution: Read from CacheManagerConfig

The `CacheManagerConfig` struct is defined in `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/mod.rs`. We need to check if it has configurable size estimates.

#### Option 1: Add Constants to CacheManagerConfig

**In** `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/mod.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogCacheConfig {
    pub max_exact_results: usize,
    pub max_bitmap_entries: usize,

    // Add these fields for memory estimation
    #[serde(default = "default_exact_result_size_bytes")]
    pub exact_result_size_bytes: usize,

    #[serde(default = "default_bitmap_entry_size_bytes")]
    pub bitmap_entry_size_bytes: usize,
}

fn default_exact_result_size_bytes() -> usize {
    1024  // 1KB per exact result
}

fn default_bitmap_entry_size_bytes() -> usize {
    100  // 100 bytes per bitmap entry
}
```

**In cache handler** (`cache.rs` line 270):

```rust
let log_max_bytes =
    config.log_cache.max_exact_results * config.log_cache.exact_result_size_bytes +
    config.log_cache.max_bitmap_entries * config.log_cache.bitmap_entry_size_bytes;
```

#### Option 2: Centralize Memory Estimation

Create a memory estimation module:

```rust
// In crates/prism-core/src/cache/memory.rs
pub mod memory {
    /// Memory size constants for cache entries
    pub const BLOCK_HEADER_BYTES: usize = 500;
    pub const BLOCK_BODY_BYTES: usize = 2048;
    pub const TRANSACTION_BYTES: usize = 300;
    pub const RECEIPT_BYTES: usize = 200;
    pub const LOG_EXACT_RESULT_BYTES: usize = 1024;
    pub const LOG_BITMAP_ENTRY_BYTES: usize = 100;

    /// Calculate estimated memory for log cache
    pub fn estimate_log_cache_memory(
        max_exact_results: usize,
        max_bitmap_entries: usize,
    ) -> usize {
        max_exact_results * LOG_EXACT_RESULT_BYTES +
        max_bitmap_entries * LOG_BITMAP_ENTRY_BYTES
    }

    /// Calculate estimated memory for block cache
    pub fn estimate_block_cache_memory(
        max_headers: usize,
        max_bodies: usize,
    ) -> usize {
        max_headers * BLOCK_HEADER_BYTES +
        max_bodies * BLOCK_BODY_BYTES
    }

    /// Calculate estimated memory for transaction cache
    pub fn estimate_tx_cache_memory(
        max_transactions: usize,
        max_receipts: usize,
    ) -> usize {
        max_transactions * TRANSACTION_BYTES +
        max_receipts * RECEIPT_BYTES
    }
}
```

**Then use in cache handler**:

```rust
use prism_core::cache::memory;

// In get_settings() handler
let log_max_bytes = memory::estimate_log_cache_memory(
    config.log_cache.max_exact_results,
    config.log_cache.max_bitmap_entries,
);

let block_max_bytes = memory::estimate_block_cache_memory(
    config.block_cache.max_headers,
    config.block_cache.max_bodies,
);

let tx_max_bytes = memory::estimate_tx_cache_memory(
    config.transaction_cache.max_transactions,
    config.transaction_cache.max_receipts,
);
```

### Recommended Approach

**Use Option 2** (Centralized Memory Estimation) because:

1. ✅ **DRY**: Eliminates duplicate magic numbers scattered across handlers
2. ✅ **Single source of truth**: All size estimates in one place
3. ✅ **Testable**: Can unit test memory calculations
4. ✅ **Consistency**: Same estimates used in `get_stats()` (line 86-89) and `get_settings()` (line 263-271)
5. ✅ **Documentation**: Clear constants with purpose

### Implementation Steps

1. Create `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/memory.rs`
2. Add `pub mod memory;` to `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/mod.rs`
3. Update `cache.rs` handlers to use `memory::estimate_*` functions
4. Remove hardcoded values at lines 86-89, 219-223, 263-271

---

## 5. Async/Concurrency Considerations

### AdminState Structure

The `AdminState` is designed for concurrent access:

```rust
#[derive(Clone)]
pub struct AdminState {
    pub proxy_engine: Arc<ProxyEngine>,
    pub config: Arc<AppConfig>,
    pub prometheus_client: Option<PrometheusClient>,
}
```

**Key observations**:
- ✅ `Clone` implementation allows sharing across handler tasks
- ✅ `Arc<ProxyEngine>` provides thread-safe shared ownership
- ✅ `Arc<AppConfig>` makes config immutable and shareable
- ✅ `PrometheusClient` is `Clone` (line 89 of prometheus.rs)

### PrometheusClient Thread Safety

```rust
#[derive(Clone)]
pub struct PrometheusClient {
    base_url: String,
    client: reqwest::Client,  // ✅ Already Clone + Send + Sync
    cache: Arc<Mutex<LruCache<String, CachedResult>>>,  // ✅ Thread-safe cache
    cache_ttl: Duration,
}
```

**Thread safety guarantees**:
- `reqwest::Client` is `Clone + Send + Sync` (designed for concurrent use)
- `Arc<Mutex<LruCache>>` provides synchronized cache access
- `parking_lot::Mutex` is used (faster than `std::sync::Mutex`)

### UpstreamEndpoint Concurrency

The `UpstreamEndpoint` uses multiple synchronization primitives:

```rust
pub struct UpstreamEndpoint {
    health: Arc<RwLock<UpstreamHealth>>,           // ✅ Read-optimized
    circuit_breaker: Arc<CircuitBreaker>,           // ✅ Shared state
    health_history: Arc<RwLock<VecDeque<...>>>,    // ✅ Read-optimized
    cached_is_healthy: AtomicBool,                  // ✅ Lock-free cache
    health_cache_time: AtomicU64,                   // ✅ Lock-free timestamp
    health_check_in_flight: AtomicBool,             // ✅ Singleflight coordination
}
```

**Concurrency patterns**:
1. **Lock-free health check**: Uses `AtomicBool` + `AtomicU64` for 100ms cache (lines 243-264)
2. **Singleflight pattern**: Prevents duplicate health checks (lines 304-333)
3. **RwLock for reads**: Allows multiple concurrent readers
4. **Arc for sharing**: Safe cross-thread sharing

### Best Practices Summary

1. **Always use Arc for shared state**: `Arc<T>` not `Rc<T>`
2. **Prefer RwLock for read-heavy workloads**: More readers can run concurrently
3. **Use Mutex for write-heavy workloads**: Simpler, less overhead
4. **Lock-free when possible**: `Atomic*` types for simple values
5. **Clone state, not locks**: Let Tokio runtime handle task distribution

---

## 6. Error Handling Patterns

### Standard Pattern in Admin API

All admin endpoints follow this error handling pattern:

```rust
// Pattern 1: Using Result with AdminApiErrorResponse
pub async fn some_handler(
    State(state): State<AdminState>,
) -> Result<Json<SomeResponse>, AdminApiErrorResponse> {
    let something = do_thing()
        .ok_or_else(|| AdminApiError::not_found(
            "THING_NOT_FOUND",
            "Thing not found"
        ))?;

    Ok(Json(SomeResponse { /* ... */ }))
}

// Pattern 2: Using tuple error type
pub async fn other_handler(
    State(state): State<AdminState>,
) -> Result<Json<Response>, (StatusCode, String)> {
    let value = validate_input(&request.field)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(Response { /* ... */ }))
}
```

### AdminApiError Construction

The `AdminApiError` type provides builder methods:

```rust
// Pre-defined error constructors (from types.rs lines 675-736)
AdminApiError::upstream_not_found("upstream-1")
AdminApiError::invalid_upstream_id()
AdminApiError::validation_error("Invalid weight value")
AdminApiError::alert_not_found("alert-123")
AdminApiError::rule_not_found("rule-456")
AdminApiError::rule_conflict()

// Generic constructors
AdminApiError::bad_request("CODE", "message")
AdminApiError::not_found("CODE", "message")
AdminApiError::conflict("CODE", "message")
AdminApiError::internal("CODE", "message")

// With additional details
AdminApiError::bad_request("INVALID_INPUT", "Field validation failed")
    .with_details(serde_json::json!({
        "field": "weight",
        "reason": "must be > 0"
    }))
```

### Prometheus Query Error Handling

Never panic on Prometheus failures - always fall back gracefully:

```rust
// ✅ Good: Graceful fallback
match prom_client.get_some_metric(time_range).await {
    Ok(data) if !data.is_empty() => {
        return Json(MetricsDataResponse::from_prometheus(data));
    }
    Ok(_) => {
        tracing::debug!("no data from Prometheus");
    }
    Err(e) => {
        tracing::warn!("Prometheus query failed: {}", e);
    }
}
// Fall through to in-memory fallback

// ❌ Bad: Propagating error
let data = prom_client.get_some_metric(time_range).await?;  // Don't do this!
```

### Error Handling Checklist

- [ ] All Prometheus queries have fallback to in-memory metrics
- [ ] All database queries return proper error responses (not panics)
- [ ] Input validation returns 400 Bad Request with details
- [ ] Resource not found returns 404 Not Found
- [ ] Conflicts return 409 Conflict
- [ ] Internal errors return 500 Internal Server Error with generic message
- [ ] Never expose internal errors (stack traces, paths) to API consumers
- [ ] Always log full error context with `tracing::error!` or `tracing::warn!`

---

## 7. Type Definitions Needed

### None Required

All necessary types are already defined:

**In `/home/flo/workspace/personal/prism/crates/server/src/admin/types.rs`**:

- `TimeSeriesPoint` (line 271-277) - Generic time-series data
- `LatencyDataPoint` (line 279-287) - Latency percentiles over time
- `RequestMethodData` (line 289-296) - Request distribution by method
- `ErrorDistribution` (line 298-306) - Error distribution by type
- `HedgingStats` (line 308-317) - Hedging performance metrics
- `MetricsDataResponse<T>` (line 588-615) - Wrapper with data source indicator
- `DataSource` enum (line 575-585) - Prometheus, InMemory, or Fallback
- `HealthHistoryEntry` (line 532-542) - Health check history

### Adding New Types

If you need to add new response types, follow this pattern:

```rust
/// Description of what this represents.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]  // ✅ Use camelCase for JSON API
pub struct NewMetricData {
    /// Field documentation
    pub timestamp: String,
    pub some_value: f64,

    /// Optional field (omitted if None)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_field: Option<String>,
}
```

**Key attributes**:
- `#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]` - Standard derives
- `#[serde(rename_all = "camelCase")]` - JSON field naming convention
- `#[serde(skip_serializing_if = "Option::is_none")]` - Clean JSON output
- `/// Documentation` - Shows up in OpenAPI spec

---

## 8. Config Access Patterns

### Reading Cache Config

The cache configuration is accessible via `AdminState`:

```rust
pub async fn some_handler(State(state): State<AdminState>) -> impl IntoResponse {
    // Access the full app config
    let config: &AppConfig = &state.config;

    // Access cache-specific config
    let cache_config: &CacheConfig = &state.config.cache;
    let manager_config: &CacheManagerConfig = &state.config.cache.manager_config;

    // Access specific cache settings
    let retain_blocks = manager_config.retain_blocks;
    let cleanup_interval = manager_config.cleanup_interval_seconds;

    // Block cache settings
    let max_headers = manager_config.block_cache.max_headers;
    let max_bodies = manager_config.block_cache.max_bodies;

    // Log cache settings
    let max_exact_results = manager_config.log_cache.max_exact_results;
    let max_bitmap_entries = manager_config.log_cache.max_bitmap_entries;

    // Transaction cache settings
    let max_transactions = manager_config.transaction_cache.max_transactions;
    let max_receipts = manager_config.transaction_cache.max_receipts;

    Json(Response { /* ... */ })
}
```

### Config Immutability

⚠️ **Important**: The `AppConfig` is wrapped in `Arc`, making it **immutable at runtime**.

From `cache.rs` lines 359-380:

```rust
// Note: The current CacheManager config is stored in Arc<AppConfig> which is immutable.
// This implementation demonstrates the handler structure, but full runtime config updates
// would require making the config mutable or using atomic updates.
```

**To support runtime config updates**, you would need to:

1. **Wrap in RwLock**: Change `Arc<AppConfig>` to `Arc<RwLock<AppConfig>>`
2. **Add update methods**: Implement config update logic with validation
3. **Propagate changes**: Update cache managers when config changes

Example:

```rust
// In AdminState
pub struct AdminState {
    pub proxy_engine: Arc<ProxyEngine>,
    pub config: Arc<RwLock<AppConfig>>,  // ✅ Now mutable
    pub prometheus_client: Option<PrometheusClient>,
}

// In update handler
pub async fn update_cache_settings(
    State(state): State<AdminState>,
    Json(request): Json<UpdateCacheSettingsRequest>,
) -> Result<Json<SuccessResponse>, AdminApiErrorResponse> {
    let mut config = state.config.write().await;

    if let Some(retain_blocks) = request.retain_blocks {
        config.cache.manager_config.retain_blocks = retain_blocks;
    }

    // TODO: Notify cache manager of config change

    Ok(Json(SuccessResponse { success: true }))
}
```

---

## 9. Testing Considerations

### Unit Testing Async Handlers

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::AdminState;

    #[tokio::test]
    async fn test_get_metrics_fallback() {
        let state = AdminState {
            proxy_engine: Arc::new(/* mock */),
            config: Arc::new(AppConfig::default()),
            prometheus_client: None,  // No Prometheus
        };

        let query = TimeRangeQuery { time_range: "1h".to_string() };
        let response = get_latency(State(state), axum::extract::Query(query)).await;

        // Assert fallback behavior
        assert_eq!(response.0.source, DataSource::InMemory);
        assert!(response.0.warning.is_some());
    }
}
```

### Integration Testing with Prometheus

```rust
#[tokio::test]
async fn test_prometheus_integration() {
    // Start mock Prometheus server
    let mock_server = MockServer::start().await;

    // Configure mock response
    Mock::given(method("GET"))
        .and(path("/api/v1/query_range"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": {
                "result": [/* ... */]
            }
        })))
        .mount(&mock_server)
        .await;

    let client = PrometheusClient::new(mock_server.uri()).unwrap();
    let result = client.get_latency_percentiles(Duration::from_secs(3600)).await;

    assert!(result.is_ok());
}
```

---

## 10. Quick Reference

### File Locations

| Component | File Path |
|-----------|-----------|
| Prometheus Client | `/home/flo/workspace/personal/prism/crates/server/src/admin/prometheus.rs` |
| Metrics Handlers | `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/metrics.rs` |
| Cache Handlers | `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/cache.rs` |
| Upstream Endpoint | `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/endpoint.rs` |
| Admin Types | `/home/flo/workspace/personal/prism/crates/server/src/admin/types.rs` |
| App Config | `/home/flo/workspace/personal/prism/crates/prism-core/src/config/mod.rs` |
| Cache Config | `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/mod.rs` |

### Key Traits and Types

```rust
// Error handling
use crate::admin::types::{AdminApiError, AdminApiErrorResponse};
use axum::http::StatusCode;

// Metrics data wrapper
use crate::admin::types::{MetricsDataResponse, DataSource};

// Time parsing
use crate::admin::prometheus::parse_time_range;
use std::time::Duration;

// Async runtime
use tokio::sync::{RwLock, Mutex};
use std::sync::Arc;
```

### Common Imports for New Handlers

```rust
use axum::{
    extract::{State, Query},
    response::IntoResponse,
    Json,
};
use std::sync::Arc;

use crate::admin::{
    AdminState,
    prometheus::parse_time_range,
    types::{
        MetricsDataResponse,
        DataSource,
        TimeRangeQuery,
        // ... other types
    },
};
```

---

## Conclusion

### Summary of Findings

1. ✅ **Prometheus queries**: All required methods already exist with slightly different names
2. ✅ **Fallback pattern**: Correctly implemented throughout metrics handlers
3. ✅ **Health history**: Already implemented with optimal thread-safe ring buffer
4. ❌ **Cache handler**: Has hardcoded values that should be centralized

### Immediate Action Items

1. **Add method aliases or documentation** for Prometheus client naming consistency
2. **Create centralized memory estimation module** (`cache/memory.rs`)
3. **Update cache handlers** to use centralized memory estimates
4. **Consider making config mutable** if runtime updates are required

### Best Practices Applied

- Zero-cost abstractions via `Arc` and atomic types
- Memory safety through ownership system
- Async/await for non-blocking I/O
- Comprehensive error handling with fallbacks
- Thread-safe concurrent access patterns
- Clear type definitions with serde integration

This implementation maintains Rust's safety guarantees while providing a robust, production-ready admin API.
