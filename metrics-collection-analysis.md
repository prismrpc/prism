# Metrics Collection Analysis: Empty Results Root Cause

## Executive Summary

**Issue**: Admin API endpoints for upstream health history and request distribution are returning empty arrays.

**Root Cause**: **Data retrieval issue**, not a collection issue. Metrics ARE being collected and recorded via Prometheus, but:
1. **Health history tracking is not implemented** in the core upstream layer
2. **Request distribution by method** depends on Prometheus being available and properly scraped

**Impact**: Medium - Admin dashboard displays incomplete monitoring data, affecting operational visibility.

---

## Investigation Findings

### 1. Metrics Collection (WORKING ✅)

#### Evidence of Active Collection

**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/metrics/mod.rs`

The `MetricsCollector` is recording ALL necessary metrics:

```rust
// Health metrics (lines 369-374)
pub fn record_upstream_health(&self, upstream: &str, is_healthy: bool) {
    let upstream_cow = upstream_to_static(upstream);
    let health_value = if is_healthy { 1.0 } else { 0.0 };
    gauge!("rpc_upstream_health", "upstream" => upstream_cow).set(health_value);
}

// Request metrics by method (lines 281-320)
pub async fn record_request(
    &self,
    method: &str,
    upstream: &str,
    success: bool,
    latency_ms: u64,
) {
    let method_cow = method_to_static(method);
    let upstream_cow = upstream_to_static(upstream);

    counter!("rpc_requests_total", "method" => method_cow.clone(), "upstream" => upstream_cow.clone()).increment(1);
    histogram!("rpc_request_duration_seconds", "method" => method_cow.clone(), "upstream" => upstream_cow.clone()).record(latency_ms as f64 / 1000.0);

    if success {
        counter!("rpc_requests_success_total", "method" => method_cow, "upstream" => upstream_cow).increment(1);
    } else {
        counter!("rpc_requests_error_total", "method" => method_cow, "upstream" => upstream_cow).increment(1);
    }
    // ...
}

// Health check metrics (lines 518-540)
pub fn record_health_check(&self, upstream: &str, success: bool, response_time_ms: u64) {
    histogram!("rpc_health_check_duration_seconds", "upstream" => upstream.to_string())
        .record(response_time_ms as f64 / 1000.0);

    if success {
        counter!("rpc_health_check_success_total", "upstream" => upstream.to_string())
            .increment(1);
    } else {
        counter!("rpc_health_check_failure_total", "upstream" => upstream.to_string())
            .increment(1);
    }
}
```

#### Health Checks Executed Regularly

**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/health.rs`

The `HealthChecker` runs periodic health checks (lines 117-281):

```rust
async fn check_all_upstreams(
    upstream_manager: &UpstreamManager,
    metrics_collector: &MetricsCollector,
    // ...
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let upstreams = upstream_manager.get_all_upstreams();

    for upstream in upstreams.iter() {
        let upstream_name = Arc::clone(&upstream.config().name);
        let health = upstream.get_health().await;
        let response_time_ms = health.response_time_ms.unwrap_or(0);

        let (is_healthy, block_number) = upstream.health_check_with_block_number().await;

        if is_healthy {
            healthy_count += 1;
            metrics_collector.record_upstream_health(&upstream_name, true);
            metrics_collector.record_health_check(&upstream_name, true, response_time_ms);
            // ...
        } else {
            metrics_collector.record_upstream_health(&upstream_name, false);
            metrics_collector.record_health_check(&upstream_name, false, response_time_ms);
            // ...
        }
    }
}
```

**Conclusion**: Metrics ARE being recorded to Prometheus. Collection is working.

---

### 2. Health History Endpoint (NOT IMPLEMENTED ❌)

#### Current Implementation

**Location**: `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs:685-719`

```rust
pub async fn get_health_history(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(params): Query<HealthHistoryQuery>,
) -> Result<Json<Vec<HealthHistoryEntry>>, (StatusCode, String)> {
    let idx: usize = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();

    let _upstream = upstreams
        .get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    // Health history tracking is not currently implemented in UpstreamManager
    // This would require maintaining a buffer of recent health check results
    // For now, return empty data

    // TODO: Implement health history tracking in UpstreamManager
    // This could be done by:
    // 1. Adding a ring buffer to each upstream to store recent health checks
    // 2. Recording timestamp, success/failure, latency, and error message
    // 3. Exposing this via a method on the upstream

    let _limit = params.limit.unwrap_or(50);

    tracing::debug!(
        upstream_id = %id,
        limit = _limit,
        "health history endpoint called (tracking not yet implemented)"
    );

    Ok(Json(Vec::new()))  // ⚠️ ALWAYS RETURNS EMPTY
}
```

**Expected Type** (from `/home/flo/workspace/personal/prism/crates/server/src/admin/types.rs:535-541`):

```rust
pub struct HealthHistoryEntry {
    pub timestamp: String,
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
```

#### Root Cause

**No in-memory history buffer exists in `UpstreamEndpoint`**.

The `UpstreamEndpoint` (`/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/endpoint.rs`) stores only:
- Current health status (`Arc<RwLock<UpstreamHealth>>`)
- Circuit breaker state
- Latest block number

There is **NO** ring buffer or deque storing historical health check results.

#### Why This Matters

While Prometheus DOES store `rpc_health_check_success_total` and `rpc_health_check_failure_total` counters, the admin API handler doesn't query Prometheus for this data. It expects in-memory storage that doesn't exist.

---

### 3. Request Distribution Endpoint (PARTIALLY IMPLEMENTED ⚠️)

#### Current Implementation

**Location**: `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs:743-789`

```rust
pub async fn get_request_distribution(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<Vec<RequestMethodData>>, (StatusCode, String)> {
    let idx: usize = id.parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))?;

    let upstreams = state.proxy_engine.get_upstream_manager().get_all_upstreams();
    let upstream = upstreams.get(idx)
        .ok_or((StatusCode::NOT_FOUND, "Upstream not found".to_string()))?;

    let upstream_name = upstream.config().name.to_string();

    // Try to query from Prometheus if available
    if let Some(prometheus_client) = &state.prometheus_client {
        let time_range = parse_time_range(&query.time_range);

        // Query per-upstream request distribution by method from Prometheus
        match prometheus_client
            .get_upstream_request_by_method(&upstream_name, time_range)
            .await
        {
            Ok(data) => return Ok(Json(data)),  // ✅ Returns data if Prometheus works
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
    Ok(Json(Vec::new()))  // ⚠️ Returns empty if no Prometheus
}
```

**Expected Type** (from `/home/flo/workspace/personal/prism/crates/server/src/admin/types.rs:289-296`):

```rust
pub struct RequestMethodData {
    pub method: String,
    pub count: u64,
    pub percentage: f64,
}
```

#### Root Cause

**Endpoint depends entirely on Prometheus availability**:

1. ✅ Metrics ARE recorded: `counter!("rpc_requests_total", "method" => ..., "upstream" => ...)`
2. ⚠️ But retrieval requires:
   - Prometheus client configured in `AdminState`
   - Prometheus scraping metrics from the application
   - Successful Prometheus query execution

If any of these fail, endpoint returns empty array.

#### Prometheus Client Setup

**Location**: `/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs:31-35`

```rust
pub struct AdminState {
    pub proxy_engine: Arc<ProxyEngine>,
    pub chain_state: Arc<ChainState>,
    pub start_time: std::time::Instant,
    pub prometheus_client: Option<Arc<prometheus::PrometheusClient>>,  // ⚠️ Optional
}
```

The Prometheus client is **optional**, so the endpoint gracefully degrades to empty results when unavailable.

---

## Data Flow Analysis

### Current State

```
┌─────────────────────────────────────────────────────────────────┐
│ REQUEST EXECUTION                                               │
│                                                                 │
│ ProxyEngine.execute_request()                                  │
│     ↓                                                           │
│ upstream.send_request()                                        │
│     ↓                                                           │
│ MetricsCollector.record_request(method, upstream, ...)        │
│     ↓                                                           │
│ counter!("rpc_requests_total",                                 │
│          "method" => method,                                   │
│          "upstream" => upstream)                               │
│     ↓                                                           │
│ [Prometheus Exporter] ← /metrics endpoint                      │
│     ↓                                                           │
│ [Prometheus Server] ← scrapes /metrics                         │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│ HEALTH CHECKS                                                   │
│                                                                 │
│ HealthChecker (background task, periodic)                      │
│     ↓                                                           │
│ upstream.health_check_with_block_number()                      │
│     ↓                                                           │
│ MetricsCollector.record_health_check(upstream, success, ...)  │
│     ↓                                                           │
│ counter!("rpc_health_check_success_total", "upstream" => ...)  │
│ gauge!("rpc_upstream_health", "upstream" => ...)               │
│     ↓                                                           │
│ [Prometheus Exporter] ← /metrics endpoint                      │
│     ↓                                                           │
│ [Prometheus Server] ← scrapes /metrics                         │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│ ADMIN API QUERIES                                               │
│                                                                 │
│ GET /admin/upstreams/:id/health-history                        │
│     ↓                                                           │
│ ❌ No in-memory storage → Returns []                            │
│                                                                 │
│ GET /admin/upstreams/:id/request-distribution                  │
│     ↓                                                           │
│ IF prometheus_client.is_some() {                               │
│     query_prometheus("rpc_requests_total{upstream=...}")       │
│     ↓                                                           │
│     ✅ Returns data if query succeeds                           │
│     ❌ Returns [] if query fails                                │
│ } ELSE {                                                        │
│     ❌ Returns []                                               │
│ }                                                               │
└─────────────────────────────────────────────────────────────────┘
```

### What's Missing

1. **Health History**: No ring buffer in `UpstreamEndpoint` to store recent health checks
2. **Request Distribution**: Fallback mechanism when Prometheus is unavailable

---

## Recommended Solutions

### Option 1: Add In-Memory Storage (Quick Fix)

**For Health History**:

Add a ring buffer to `UpstreamEndpoint`:

```rust
// In crates/prism-core/src/upstream/endpoint.rs
use std::collections::VecDeque;

pub struct UpstreamEndpoint {
    // ... existing fields ...
    health_history: Arc<RwLock<VecDeque<HealthHistoryRecord>>>,
}

struct HealthHistoryRecord {
    timestamp: chrono::DateTime<chrono::Utc>,
    is_healthy: bool,
    latency_ms: Option<u64>,
    error: Option<String>,
}

impl UpstreamEndpoint {
    fn record_health_check_result(&self, result: HealthHistoryRecord) {
        let mut history = self.health_history.write().await;
        if history.len() >= 100 {  // Keep last 100 checks
            history.pop_front();
        }
        history.push_back(result);
    }

    pub fn get_health_history(&self, limit: usize) -> Vec<HealthHistoryRecord> {
        let history = self.health_history.read().await;
        history.iter().rev().take(limit).cloned().collect()
    }
}
```

**For Request Distribution**:

Add per-method tracking to `ScoringEngine`:

```rust
// In crates/prism-core/src/upstream/scoring.rs
pub struct UpstreamMetrics {
    // ... existing fields ...
    requests_by_method: DashMap<String, AtomicU64>,
}

impl UpstreamMetrics {
    pub fn record_method(&self, method: &str) {
        self.requests_by_method
            .entry(method.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_method_distribution(&self) -> Vec<(String, u64)> {
        self.requests_by_method
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().load(Ordering::Relaxed)))
            .collect()
    }
}
```

**Pros**:
- ✅ Works without Prometheus
- ✅ Low latency (in-memory)
- ✅ Simple implementation

**Cons**:
- ❌ Data lost on restart
- ❌ Limited history (last N entries)
- ❌ Higher memory usage

---

### Option 2: Query Prometheus (Production-Ready)

**Ensure Prometheus is properly configured**:

1. Verify Prometheus is scraping the `/metrics` endpoint
2. Add Prometheus client initialization in server startup
3. Implement query methods in `PrometheusClient`

**For Health History**:

```rust
// In crates/server/src/admin/prometheus.rs
impl PrometheusClient {
    pub async fn get_upstream_health_history(
        &self,
        upstream_name: &str,
        time_range: (DateTime<Utc>, DateTime<Utc>),
        limit: usize,
    ) -> Result<Vec<HealthHistoryEntry>, PrometheusError> {
        let query = format!(
            "rpc_health_check_success_total{{upstream=\"{}\"}}",
            upstream_name
        );

        let points = self.query_range(&query, time_range.0, time_range.1, "1m").await?;

        // Convert to HealthHistoryEntry format
        // ...
    }
}
```

**Update admin handler**:

```rust
pub async fn get_health_history(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(params): Query<HealthHistoryQuery>,
) -> Result<Json<Vec<HealthHistoryEntry>>, (StatusCode, String)> {
    // ... existing validation ...

    if let Some(prometheus_client) = &state.prometheus_client {
        let time_range = parse_time_range("1h");  // Last hour
        match prometheus_client
            .get_upstream_health_history(&upstream_name, time_range, params.limit.unwrap_or(50))
            .await
        {
            Ok(data) => return Ok(Json(data)),
            Err(e) => {
                tracing::warn!("Failed to query health history from Prometheus: {}", e);
            }
        }
    }

    Ok(Json(Vec::new()))
}
```

**Pros**:
- ✅ Persistent storage
- ✅ Full history retention
- ✅ No extra memory in application
- ✅ Consistent with existing architecture

**Cons**:
- ❌ Requires Prometheus setup
- ❌ Higher latency (network query)
- ❌ More complex implementation

---

### Option 3: Hybrid Approach (Best of Both Worlds)

Combine both approaches:
1. Use in-memory ring buffer for recent data (last 50 entries)
2. Fall back to Prometheus for historical queries
3. Admin API tries in-memory first, then Prometheus

```rust
pub async fn get_health_history(...) -> Result<Json<Vec<HealthHistoryEntry>>, ...> {
    // Try in-memory first (fast, recent data)
    let in_memory = upstream.get_recent_health_history(params.limit.unwrap_or(50));
    if !in_memory.is_empty() {
        return Ok(Json(in_memory));
    }

    // Fall back to Prometheus (slower, full history)
    if let Some(prometheus_client) = &state.prometheus_client {
        match prometheus_client.get_upstream_health_history(...).await {
            Ok(data) => return Ok(Json(data)),
            Err(e) => tracing::warn!("Prometheus query failed: {}", e),
        }
    }

    Ok(Json(Vec::new()))
}
```

**Pros**:
- ✅ Fast for recent data
- ✅ Full history available
- ✅ Graceful degradation

**Cons**:
- ❌ Most complex implementation
- ❌ Requires both systems

---

## Performance Impact Analysis

### Current Metrics Recording (Hot Path)

**Latency**: ~10-50ns per metric recording (lock-free atomic operations)

From `/home/flo/workspace/personal/prism/crates/prism-core/src/metrics/mod.rs:19-25`:

```
| Operation | Latency | Notes |
|-----------|---------|-------|
| Prometheus record | ~10ns | Lock-free atomic ops |
| Internal metrics (hit) | ~50ns | Opportunistic write |
| Internal metrics (contention) | 0ns | Skipped, no block |
```

### Adding In-Memory Storage

**Ring Buffer Write**: ~100-200ns
- Lock acquisition (RwLock::write): ~50-100ns
- VecDeque push/pop: ~20-50ns
- Lock release: ~20-50ns

**Impact**: Negligible on request path. Health checks are background tasks.

### Prometheus Query

**Latency**: 10-100ms per query
- Network round-trip: 5-50ms
- Prometheus query execution: 5-50ms
- Response parsing: 1-5ms

**Impact**: Only affects admin API, not request path. Acceptable for dashboard.

---

## Verification Steps

### 1. Confirm Prometheus is Recording Metrics

```bash
# Check if metrics endpoint is working
curl http://localhost:9090/metrics | grep rpc_requests_total

# Should see output like:
# rpc_requests_total{method="eth_blockNumber",upstream="upstream1"} 1234
# rpc_requests_total{method="eth_getLogs",upstream="upstream1"} 567
```

### 2. Verify Prometheus Scraping

```bash
# Query Prometheus directly
curl 'http://localhost:9090/api/v1/query?query=rpc_requests_total'

# Should return JSON with metric values
```

### 3. Test Admin API

```bash
# Health history (currently returns empty)
curl http://localhost:8080/admin/upstreams/0/health-history

# Request distribution (returns empty if Prometheus not available)
curl http://localhost:8080/admin/upstreams/0/request-distribution?timeRange=1h
```

---

## Conclusion

### Root Cause Summary

| Endpoint | Issue Type | Root Cause |
|----------|-----------|------------|
| `/admin/upstreams/:id/health-history` | **NOT IMPLEMENTED** | No in-memory ring buffer in `UpstreamEndpoint`, Prometheus not queried |
| `/admin/upstreams/:id/request-distribution` | **DATA RETRIEVAL** | Depends on Prometheus availability, no in-memory fallback |

### Recommendation

**Implement Option 2 (Prometheus Queries)** for production:
1. Add Prometheus query methods for health history
2. Ensure Prometheus is properly configured and scraping
3. Update admin handlers to query Prometheus
4. Add proper error handling and logging

**Why**:
- Metrics are already being recorded
- Prometheus provides persistent, queryable storage
- Consistent with existing architecture (request distribution already uses this pattern)
- No additional memory overhead in application

### Estimated Effort

- Prometheus client methods: 2-3 hours
- Admin handler updates: 1-2 hours
- Testing and verification: 2-3 hours
- **Total**: 5-8 hours

---

## Files Modified (for Option 2 implementation)

1. `/home/flo/workspace/personal/prism/crates/server/src/admin/prometheus.rs`
   - Add `get_upstream_health_history()` method

2. `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs`
   - Update `get_health_history()` to query Prometheus
   - Verify `get_request_distribution()` Prometheus client availability

3. `/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs`
   - Ensure `prometheus_client` is initialized in `AdminState`

4. `/home/flo/workspace/personal/prism/config/development.toml`
   - Add Prometheus URL configuration if missing
