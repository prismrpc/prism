# Admin API Architecture Review: Upstream Health & Request Distribution

**Reviewer**: Claude (Architecture Review Agent)  
**Date**: 2025-12-06  
**Scope**: Admin API endpoints for upstream health history and request distribution  
**Codebase**: Prism RPC Proxy (Rust)

---

## Executive Summary

The admin API implementation shows **good separation of concerns** with a clear data flow path from upstream components → metrics collection → API handlers. However, there are **critical architectural gaps** where health history and per-upstream request distribution data **do not flow to the API endpoints**, resulting in empty responses even when the underlying systems are functioning correctly.

### Key Findings

✅ **Strengths**:
- Well-structured layered architecture with clear component boundaries
- Lock-free metrics collection on hot path (Prometheus counters)
- Dual-path metrics: Prometheus for time-series + in-memory for aggregates
- Proper separation between config-based and runtime upstreams

❌ **Critical Gaps**:
1. Health history not tracked (no ring buffer in UpstreamEndpoint)
2. Per-upstream request distribution requires Prometheus queries (not implemented)
3. No fallback to in-memory metrics when Prometheus unavailable
4. Missing connection between ScoringEngine metrics and API responses

---

## 1. Data Flow Analysis: Upstream Health

### Current Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Upstream Health Data Flow                     │
└─────────────────────────────────────────────────────────────────┘

[UpstreamEndpoint]
    │
    ├─► health: Arc<RwLock<UpstreamHealth>>
    │   ├─ is_healthy: bool
    │   ├─ response_time_ms: Option<u64>
    │   ├─ latency_p50/p95/p99_ms: Option<u64>
    │   ├─ latest_block: Option<u64>
    │   └─ finalized_block: Option<u64>
    │
    └─► circuit_breaker: Arc<CircuitBreaker>
        ├─ state: RwLock<CircuitBreakerState>
        ├─ failure_count: AtomicU32
        └─ last_failure_time: RwLock<Option<Instant>>

            ↓ (periodic health checks)

[HealthChecker] (background task)
    │
    └─► check_all_upstreams() every N seconds
        │
        ├─► upstream.health_check_with_block_number()
        │   └─► updates UpstreamHealth fields
        │
        ├─► metrics_collector.record_health_check()
        │   └─► Prometheus: rpc_health_check_duration_seconds
        │                     rpc_health_check_success_total
        │
        ├─► metrics_collector.record_upstream_health()
        │   └─► Prometheus: rpc_upstream_health (gauge)
        │
        └─► metrics_collector.record_circuit_breaker_state()
            └─► Prometheus: rpc_circuit_breaker_state
                            rpc_circuit_breaker_failure_count

            ↓ (NO STORAGE OF HISTORICAL EVENTS)

[Admin API GET /admin/upstreams/:id/health-history]
    │
    └─► Returns: Vec::new() ❌ ALWAYS EMPTY
```

### Gap Analysis

**Problem**: Health history endpoint returns empty data because no historical buffer exists.

**Root Cause**: `UpstreamEndpoint` only stores **current** health state in `UpstreamHealth`. There is no ring buffer or event log to track state transitions over time.

**Location**: `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs:718`

```rust
// TODO: Implement health history tracking in UpstreamManager
// This could be done by:
// 1. Adding a ring buffer to each upstream to store recent health checks
// 2. Recording timestamp, success/failure, latency, and error message
// 3. Exposing this via a method on the upstream

Ok(Json(Vec::new()))  // ❌ Always returns empty
```

### Recommended Architecture

```rust
// In UpstreamEndpoint:
struct HealthHistoryEntry {
    timestamp: Instant,
    is_healthy: bool,
    response_time_ms: Option<u64>,
    circuit_breaker_state: CircuitBreakerState,
    error_message: Option<String>,
}

struct UpstreamEndpoint {
    // ... existing fields ...
    health_history: Arc<RwLock<VecDeque<HealthHistoryEntry>>>,  // Ring buffer (capacity: 100)
}

// In health_check():
async fn health_check(&self) -> bool {
    let result = // ... existing check logic ...
    
    let entry = HealthHistoryEntry {
        timestamp: Instant::now(),
        is_healthy: result,
        response_time_ms: self.health.read().await.response_time_ms,
        circuit_breaker_state: self.circuit_breaker.get_state().await,
        error_message: None,
    };
    
    let mut history = self.health_history.write().await;
    if history.len() >= 100 {
        history.pop_front();
    }
    history.push_back(entry);
    
    result
}
```

**Impact**:
- Enables real-time health history visualization in admin UI
- Minimal memory overhead (~8KB per upstream for 100 entries)
- Lock-free reads with occasional write lock on health checks

---

## 2. Data Flow Analysis: Request Distribution

### Current Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                Request Distribution Data Flow                    │
└─────────────────────────────────────────────────────────────────┘

[ProxyEngine] processes request
    │
    ├─► upstream_manager.send_request_auto(request)
    │       │
    │       ├─► router.route() selects upstream
    │       │
    │       └─► upstream.send_request(request)
    │               │
    │               └─► http_client.send_request()
    │
    └─► metrics_collector.record_request(
            method,        // ✅ "eth_call", "eth_getBlockByNumber", etc.
            upstream,      // ✅ upstream name
            success,       // ✅ true/false
            latency_ms     // ✅ duration
        )
            │
            └─► Prometheus: rpc_requests_total{method, upstream}
                            rpc_request_duration_seconds{method, upstream}
                            rpc_requests_success_total{method, upstream}
                            rpc_requests_error_total{method, upstream}

            ↓ (Prometheus has the data!)

[Admin API GET /admin/upstreams/:id/request-distribution]
    │
    ├─► prometheus_client.get_upstream_request_by_method(upstream_name)
    │       │
    │       └─► ❌ NOT IMPLEMENTED - method does not exist
    │
    └─► Returns: Vec::new() or global data (not per-upstream)
```

### Gap Analysis

**Problem**: Per-upstream request distribution endpoint returns empty/global data.

**Root Cause**: The `PrometheusClient` does not have a method to query per-upstream request metrics grouped by method. The data exists in Prometheus (`rpc_requests_total{upstream="name",method="X"}`), but there's no query method.

**Location**: `/home/flo/workspace/personal/prism/crates/server/src/admin/prometheus.rs`

**Current Implementation** (missing):
```rust
// Method does not exist in PrometheusClient:
pub async fn get_upstream_request_by_method(
    &self,
    upstream: &str,
    time_range: Duration,
) -> Result<Vec<RequestMethodData>, PrometheusError>
```

**Workaround in Handler** (`upstreams.rs:773`):
```rust
match prometheus_client
    .get_upstream_request_by_method(&upstream_name, time_range)  // ❌ Method doesn't exist
    .await
{
    Ok(data) => return Ok(Json(data)),
    Err(e) => {
        tracing::warn!("failed to query request distribution from Prometheus");
        // Fall through to return empty data
    }
}

Ok(Json(Vec::new()))  // ❌ Returns empty
```

### Recommended Architecture

Add method to `PrometheusClient`:

```rust
/// Gets request distribution by method for a specific upstream.
pub async fn get_upstream_request_by_method(
    &self,
    upstream: &str,
    time_range: Duration,
) -> Result<Vec<RequestMethodData>, PrometheusError> {
    let end = Utc::now();
    let start = end - chrono::Duration::from_std(time_range).unwrap_or_default();
    
    // Query total requests by method for this upstream
    let query = format!(
        r#"sum by (method) (rate(rpc_requests_total{{upstream="{}"}}[5m]))"#,
        upstream
    );
    
    let points = self.query_range(&query, start, end, "5m").await?;
    
    // Also query success/failure breakdown
    let success_query = format!(
        r#"sum by (method) (rate(rpc_requests_success_total{{upstream="{}"}}[5m]))"#,
        upstream
    );
    let error_query = format!(
        r#"sum by (method) (rate(rpc_requests_error_total{{upstream="{}"}}[5m]))"#,
        upstream
    );
    
    // Parse results and combine into RequestMethodData
    // ...
}
```

**Alternative: In-Memory Fallback**

The `ScoringEngine` already tracks per-upstream request counts:

```rust
pub struct UpstreamMetrics {
    total_requests: AtomicU64,      // ✅ Available
    error_count: AtomicU64,         // ✅ Available
    success_count: AtomicU64,       // ✅ Available
    // ... but NO per-method breakdown
}
```

**Gap**: `ScoringEngine::UpstreamMetrics` does **not** track per-method request counts. It only has totals.

**Options**:
1. **Preferred**: Implement Prometheus query (data already exists)
2. **Fallback**: Add `DashMap<&'static str, AtomicU64>` to `UpstreamMetrics` for method counts (lock-free)

---

## 3. Scoring Engine Metrics Connection

### Current Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│              Scoring Engine → Admin API Data Flow                │
└─────────────────────────────────────────────────────────────────┘

[ScoringEngine]
    │
    ├─► metrics: DashMap<&'static str, Arc<UpstreamMetrics>>
    │   └─ Per-upstream metrics:
    │      ├─ latency_tracker: LatencyTracker (lock-free)
    │      ├─ total_requests: AtomicU64
    │      ├─ error_count: AtomicU64
    │      ├─ throttle_count: AtomicU64
    │      ├─ success_count: AtomicU64
    │      └─ last_block_seen: AtomicU64
    │
    └─► scores: DashMap<&'static str, Arc<UpstreamScore>>
        └─ Calculated scores:
           ├─ composite_score: f64
           ├─ latency_factor: f64 (from latency_p90_ms, latency_p99_ms)
           ├─ error_rate_factor: f64 (from error_count/total_requests)
           ├─ throttle_factor: f64 (from throttle_count/total_requests)
           ├─ block_lag_factor: f64 (from chain_tip - last_block_seen)
           └─ load_factor: f64 (from total_requests)

            ↓ (Called by admin handler)

[Admin API GET /admin/upstreams/:id/metrics]
    │
    ├─► scoring_engine.get_score(upstream_name)
    │   └─► Returns Option<UpstreamScore> ✅ WORKS
    │
    ├─► scoring_engine.get_metrics(upstream_name)
    │   └─► Returns Option<(error_rate, throttle_rate, total_req, block, avg)> ✅ WORKS
    │
    └─► Builds UpstreamMetrics response with:
        ├─ scoring_breakdown: Vec<UpstreamScoringFactor> ✅
        ├─ circuit_breaker: CircuitBreakerMetrics ✅
        └─ request_distribution: Vec<UpstreamRequestDistribution> ⚠️ PLACEHOLDER
```

### Analysis

**Strengths**:
- ✅ Scoring data properly flows from `ScoringEngine` to admin API
- ✅ Lock-free atomic metrics on hot path
- ✅ Composite score calculation works correctly
- ✅ Handler correctly checks for `Option<UpstreamScore>` and provides fallback

**Limitation**:
- ⚠️ Request distribution in `/admin/upstreams/:id/metrics` is a **placeholder**:

```rust
// Location: upstreams.rs:377
let request_distribution = vec![UpstreamRequestDistribution {
    method: "all".to_string(),           // ❌ Not broken down by method
    requests: requests_handled,
    success: requests_handled.saturating_sub((requests_handled as f64 * error_rate) as u64),
    failed: (requests_handled as f64 * error_rate) as u64,
}];
```

**Root Cause**: Same as section 2 - no per-method tracking in `ScoringEngine`.

---

## 4. Prometheus Integration Gaps

### Missing Query Methods

The `PrometheusClient` is missing several query methods that handlers expect:

| Missing Method | Expected By | Data Availability |
|----------------|-------------|-------------------|
| `get_upstream_latency_percentiles()` | `/admin/upstreams/:id/latency-distribution` | ✅ Available: `rpc_upstream_latency_p50_ms{upstream}` |
| `get_upstream_request_by_method()` | `/admin/upstreams/:id/request-distribution` | ✅ Available: `rpc_requests_total{upstream,method}` |
| `get_cache_hit_rate_by_method()` | `/admin/cache/hit-by-method` | ✅ Available: `rpc_cache_hits_total{method}` |

**Example**: Latency Distribution Handler (`upstreams.rs:649`)

```rust
match prometheus_client
    .get_upstream_latency_percentiles(&upstream_name, time_range)  // ❌ Method doesn't exist
    .await
{
    Ok(data) => return Ok(Json(data)),
    Err(e) => {
        tracing::warn!("failed to query latency distribution from Prometheus");
    }
}

Ok(Json(Vec::new()))  // ❌ Returns empty
```

**Issue**: Prometheus **has** the data (`rpc_upstream_latency_p50_ms`, `p95`, `p99` gauges), but there's no query method to fetch it.

### In-Memory Fallback Gap

When Prometheus is unavailable or returns empty, handlers should fall back to in-memory `MetricsSummary`, but they don't:

```rust
// Current implementation in metrics.rs handlers:
if let Some(prometheus_client) = &state.prometheus_client {
    match prometheus_client.get_request_methods(time_range).await {
        Ok(data) => return Ok(Json(data)),
        Err(e) => {
            tracing::warn!("Prometheus query failed: {}", e);
            // ❌ Falls through to return hardcoded/empty data
        }
    }
}

// ❌ No fallback to MetricsSummary
Ok(Json(vec![
    RequestMethodData {
        method: "eth_call".to_string(),
        count: 0,  // ❌ Hardcoded zero
        percentage: 0.0,
    },
]))
```

**Should be**:

```rust
// Fallback to in-memory metrics
let metrics_summary = state
    .proxy_engine
    .get_metrics_collector()
    .get_metrics_summary()
    .await;

let total: u64 = metrics_summary.requests_by_method.values().sum();
let data: Vec<RequestMethodData> = metrics_summary
    .requests_by_method
    .iter()
    .map(|(method, count)| RequestMethodData {
        method: method.clone(),
        count: *count,
        percentage: (*count as f64 / total.max(1) as f64) * 100.0,
    })
    .collect();

Ok(Json(data))
```

---

## 5. Circuit Breaker Time Remaining

### Current Implementation

The circuit breaker stores failure state internally:

```rust
// In CircuitBreaker:
pub struct CircuitBreaker {
    state: RwLock<CircuitBreakerState>,
    failure_count: AtomicU32,
    threshold: u32,
    timeout: Duration,
    last_failure_time: RwLock<Option<Instant>>,  // ✅ Has timestamp
}
```

The admin API handler cannot access `last_failure_time`:

```rust
// upstreams.rs:367
let time_remaining = u.circuit_breaker().get_time_remaining().await.map(|d| d.as_secs());
                     // ✅ Method exists! But returns None when state != Open
```

**Issue**: The `get_time_remaining()` method exists but only returns a value when the circuit breaker is `Open`. When `HalfOpen`, it returns `None`.

### Recommended Fix

Modify `CircuitBreaker::get_time_remaining()` to calculate remaining time for both `Open` and `HalfOpen` states:

```rust
pub async fn get_time_remaining(&self) -> Option<Duration> {
    let state = self.state.read().await;
    let last_failure = self.last_failure_time.read().await;
    
    match (*state, *last_failure) {
        (CircuitBreakerState::Open, Some(last_failure)) => {
            let elapsed = last_failure.elapsed();
            if elapsed < self.timeout {
                Some(self.timeout - elapsed)
            } else {
                Some(Duration::ZERO)  // About to transition to HalfOpen
            }
        }
        (CircuitBreakerState::HalfOpen, Some(last_failure)) => {
            // Show time since entering HalfOpen (negative remaining)
            Some(Duration::ZERO)
        }
        _ => None,
    }
}
```

---

## 6. Architectural Recommendations

### High Priority (Data Loss Prevention)

1. **Implement Health History Tracking**
   - Add `health_history: Arc<RwLock<VecDeque<HealthHistoryEntry>>>` to `UpstreamEndpoint`
   - Update `health_check()` to record history entries
   - Expose via `get_health_history()` method
   - **Impact**: Enables health timeline visualization in admin UI
   - **Effort**: 2-4 hours

2. **Add Prometheus Query Methods**
   - Implement `get_upstream_latency_percentiles()`
   - Implement `get_upstream_request_by_method()`
   - Implement `get_cache_hit_rate_by_method()`
   - **Impact**: Fixes 3 empty endpoints
   - **Effort**: 4-6 hours

3. **Add In-Memory Fallbacks**
   - Update metrics handlers to use `MetricsSummary` when Prometheus unavailable
   - **Impact**: Admin API works without Prometheus
   - **Effort**: 2-3 hours

### Medium Priority (Enhanced Functionality)

4. **Track Per-Method Request Counts in ScoringEngine**
   - Add `method_counts: DashMap<&'static str, AtomicU64>` to `UpstreamMetrics`
   - Update `record_success(method, ...)` to track method
   - **Impact**: Request distribution works without Prometheus
   - **Effort**: 3-4 hours

5. **Expose Circuit Breaker Timing**
   - Improve `get_time_remaining()` to handle all states
   - **Impact**: Better UI feedback on recovery timing
   - **Effort**: 1 hour

### Low Priority (Nice to Have)

6. **Add Request ID Tracing**
   - Include request ID in health check entries for correlation
   - **Impact**: Better debugging
   - **Effort**: 2 hours

7. **Implement Health Check Deduplication Metrics**
   - Track how often singleflight pattern prevents duplicate checks
   - **Impact**: Operational visibility
   - **Effort**: 1 hour

---

## 7. Data Flow Summary

### What Works ✅

1. **Current Health State**: Flows from `UpstreamEndpoint` → `UpstreamHealth` → Admin API
2. **Scoring Metrics**: Flows from `ScoringEngine` → Admin API handlers
3. **Circuit Breaker State**: Flows from `CircuitBreaker` → Admin API
4. **Prometheus Metrics**: Recorded correctly via `MetricsCollector`
5. **In-Memory Aggregates**: `MetricsSummary` has current snapshot data

### What's Broken ❌

1. **Health History**: No storage mechanism → Always empty
2. **Per-Upstream Latency Distribution**: Prometheus query not implemented → Empty
3. **Per-Upstream Request Distribution**: Prometheus query not implemented → Empty/Placeholder
4. **Prometheus Fallback**: Handlers return hardcoded zeros instead of using `MetricsSummary`

### Architecture Quality

| Aspect | Rating | Notes |
|--------|--------|-------|
| Separation of Concerns | ⭐⭐⭐⭐⭐ | Excellent layering: core → metrics → admin API |
| Lock-Free Design | ⭐⭐⭐⭐⭐ | Atomics and ArcSwap on hot path |
| Error Handling | ⭐⭐⭐⭐ | Good graceful degradation, missing fallbacks |
| Observability | ⭐⭐⭐⭐ | Comprehensive Prometheus metrics |
| Data Completeness | ⭐⭐ | Multiple endpoints return empty data |
| Testability | ⭐⭐⭐⭐ | Well-structured with clear boundaries |

---

## 8. Implementation Roadmap

### Phase 1: Critical Fixes (1-2 weeks)
- [ ] Add health history ring buffer to `UpstreamEndpoint`
- [ ] Implement missing Prometheus query methods
- [ ] Add in-memory fallbacks to metrics handlers
- [ ] Test with Prometheus disabled

### Phase 2: Enhanced Metrics (1 week)
- [ ] Add per-method tracking to `ScoringEngine`
- [ ] Improve circuit breaker time remaining calculation
- [ ] Add request ID correlation to health history

### Phase 3: Validation (3-5 days)
- [ ] Integration tests for health history
- [ ] Load test with Prometheus unavailable
- [ ] Performance regression testing
- [ ] Documentation updates

---

## 9. Security Considerations

### Existing Good Practices ✅
- Upstream names exposed in metrics (documented in `metrics/mod.rs:51`)
- Admin API on separate port (3031) with separate auth
- Sensitive URLs masked in responses (`mask_url()` function)

### Recommendations
- Ensure health history doesn't leak sensitive error messages
- Consider TTL/expiry for health history entries to prevent unbounded growth
- Add rate limiting to health check trigger endpoints to prevent DoS

---

## 10. Files Requiring Changes

### Core Components
- `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/endpoint.rs`
  - Add health history tracking

### Admin Handlers
- `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs`
  - Implement health history endpoint
  - Fix request distribution endpoint

- `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/metrics.rs`
  - Add in-memory fallbacks

### Prometheus Client
- `/home/flo/workspace/personal/prism/crates/server/src/admin/prometheus.rs`
  - Add `get_upstream_latency_percentiles()`
  - Add `get_upstream_request_by_method()`
  - Add `get_cache_hit_rate_by_method()`

### Optional Enhancements
- `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/scoring.rs`
  - Add per-method request tracking

- `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/circuit_breaker.rs`
  - Improve `get_time_remaining()` logic

---

## Conclusion

The admin API architecture demonstrates **strong foundational design** with clear separation of concerns and excellent performance characteristics. However, **data flow gaps** prevent critical health history and per-upstream distribution metrics from reaching the API endpoints.

The primary issues are:
1. Missing intermediate storage (health history ring buffer)
2. Incomplete Prometheus query implementations
3. Lack of fallback mechanisms when Prometheus is unavailable

These are **architectural gaps** rather than fundamental design flaws. The data exists in the system; it simply isn't being captured or queried correctly. Implementing the recommended changes will restore full functionality while maintaining the system's existing performance and reliability characteristics.

**Estimated Total Effort**: 2-3 weeks for full implementation and testing.

**Risk Assessment**: Low - Changes are additive and don't modify hot path behavior.
