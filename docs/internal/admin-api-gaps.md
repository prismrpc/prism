# Admin API Implementation Gaps

## Summary of Issues Found

The admin API handlers have several incomplete implementations that cause empty/zero data to be returned even when Prometheus is configured and working.

---

## 1. CRITICAL: Prometheus Not Being Scraped

**Root Cause**: The Prometheus instance at `localhost:9090` is not scraping metrics from the Prism server.

**Evidence**: The `/metrics` endpoint on port 3030 shows real data:
```
rpc_cache_hits_total{method="eth_getBlockByNumber"} 468866
rpc_requests_total{method="eth_blockNumber"} ...
```

But Prometheus queries return empty because it's not scraping this endpoint.

**Fix Required**: Add Prometheus scrape config (external to this codebase):
```yaml
scrape_configs:
  - job_name: 'prism'
    static_configs:
      - targets: ['localhost:3030']  # or localhost:9091 depending on config
```

---

## 2. Missing In-Memory Fallbacks for Time Series Endpoints

When Prometheus is unavailable or returns empty, these endpoints return placeholder/empty data instead of querying in-memory metrics:

### `/admin/metrics/latency`
- **Current**: Returns single point with all zeros if Prometheus fails
- **Fix**: Could use `MetricsSummary.average_latency_ms` for at least current value

### `/admin/metrics/request-volume`
- **Current**: Returns single point with `value: 0.0`
- **Fix**: Could use `MetricsSummary.total_requests` for current snapshot

### `/admin/metrics/request-methods`
- **Current**: Returns hardcoded method list with `count: 0`
- **Fix**: Use `MetricsSummary.requests_by_method` HashMap

### `/admin/metrics/error-distribution`
- **Current**: Returns hardcoded error types with `count: 0`
- **Fix**: Use `MetricsSummary.errors_by_upstream` (though this is per-upstream, not per-type)

---

## 3. Per-Upstream Endpoints Return Empty Data

### `/admin/upstreams/:id/latency-distribution`
- **Status**: Returns `Vec::new()` always
- **Issue**: Per-upstream latency histograms not exposed to Prometheus
- **Fix Options**:
  1. Add per-upstream latency metrics (already recorded: `rpc_upstream_latency_p50_ms` etc.)
  2. Query using `rpc_request_duration_seconds{upstream="name"}`

### `/admin/upstreams/:id/health-history`
- **Status**: Returns `Vec::new()` always
- **Issue**: No health history buffer exists in `UpstreamManager`
- **Fix Required**: Add ring buffer to `Upstream` struct to store last N health checks

### `/admin/upstreams/:id/request-distribution`
- **Status**: Returns global data (not per-upstream) or empty
- **Issue**: Per-upstream+method breakdown needs filtering
- **Fix**: Query `rpc_requests_total{upstream="name"}` grouped by method

---

## 4. Handler-Specific TODOs

### `cache.rs:271` - Cache Settings
```rust
block_cache_max_size: "512 MB".to_string(), // TODO: Make configurable
log_cache_max_size: "1 GB".to_string(),
transaction_cache_max_size: "256 MB".to_string(),
```
**Fix**: These should come from `CacheManagerConfig`, not hardcoded

### `upstreams.rs:370` - Circuit Breaker Time Remaining
```rust
time_remaining: None, // TODO: Could calculate from last_failure_time if exposed
```
**Fix**: Expose `last_failure_time` from circuit breaker and calculate remaining cooldown

### `apikeys.rs:255` - API Key Updates
```rust
// TODO: Implement actual updates when repository supports it
```
**Fix**: Add `update_name()` and `update_methods()` to `ApiKeyRepository`

---

## 5. Priority Order for Fixes

### High Priority (Data is available but not surfaced)
1. **Per-upstream latency-distribution** - Data exists in Prometheus as `rpc_request_duration_seconds{upstream="..."}`
2. **Per-upstream request-distribution** - Data exists in Prometheus as `rpc_requests_total{upstream="..."}`
3. **In-memory fallbacks for metrics** - `MetricsSummary` has the data

### Medium Priority (Requires infrastructure work)
4. **Health history buffer** - Needs ring buffer added to upstream
5. **Cache size configuration** - Needs config fields added

### Low Priority (Nice to have)
6. **Circuit breaker time remaining** - UI convenience
7. **API key update methods** - Low usage feature

---

## 6. Quick Wins Available Now

### A. Fix per-upstream Prometheus queries in `prometheus.rs`

Add method to query per-upstream metrics:
```rust
pub async fn get_upstream_latency(&self, upstream: &str, time_range: Duration)
    -> Result<Vec<LatencyDataPoint>, PrometheusError>
{
    // histogram_quantile(0.50, rate(rpc_request_duration_seconds_bucket{upstream="name"}[5m]))
}
```

### B. Add in-memory fallback to metrics handlers

In `get_request_methods()`:
```rust
// Prometheus failed, use in-memory data
let metrics_summary = state.proxy_engine.get_metrics_collector().get_metrics_summary().await;
let total: u64 = metrics_summary.requests_by_method.values().sum();
let data: Vec<RequestMethodData> = metrics_summary.requests_by_method
    .iter()
    .map(|(method, count)| RequestMethodData {
        method: method.clone(),
        count: *count,
        percentage: (*count as f64 / total.max(1) as f64) * 100.0,
    })
    .collect();
```

---

## 7. Files That Need Changes

| File | Changes Needed |
|------|----------------|
| `handlers/metrics.rs` | Add in-memory fallbacks for all time series endpoints |
| `handlers/upstreams.rs` | Implement per-upstream Prometheus queries |
| `prometheus.rs` | Add `get_upstream_latency()`, `get_upstream_request_distribution()` |
| `handlers/cache.rs` | Source max sizes from config instead of hardcoding |
| `prism-core/upstream/endpoint.rs` | Add health history ring buffer |
| `handlers/apikeys.rs` | Implement update methods (requires repo changes) |
