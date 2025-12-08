# Admin API - High Priority Issues Analysis

## Overview
This document captures the detailed analysis of high priority issues identified in the Prism Admin API during code review. These issues need to be addressed before the admin-api feature can be considered complete.

---

## 1. Code Quality Issues

### 1.1 Code Duplication (~180 lines duplicated 3x)

**Location**: `crates/server/src/admin/handlers/upstreams.rs`

The upstream metrics extraction logic is duplicated across three functions:
- `list_upstreams()` (lines 109-171) - extracts metrics at lines 131-147
- `get_upstream()` (lines 188-255) - same pattern at lines 216-232
- `get_upstream_metrics()` (lines 275-428) - same pattern at lines 304-320

**Duplicated Code Pattern:**
```rust
let (composite_score, error_rate, throttle_rate, requests_handled, latency_p90) =
    if let Some(score) = score {
        (score.composite_score, score.error_rate, score.throttle_rate,
         score.total_requests, score.latency_p90_ms.unwrap_or(0))
    } else if let Some((err_rate, thr_rate, total_req, _block, _avg)) = metrics {
        (0.0, err_rate, thr_rate, total_req, health.latency_p99_ms.unwrap_or(0))
    } else {
        (0.0, 0.0, 0.0, 0, health.latency_p99_ms.unwrap_or(0))
    };
```

**Impact:**
- Maintenance burden: Changes require updating 3 places
- Inconsistency risk: One function could get updated while others are forgotten
- Code bloat: ~60 lines of repeated code

**Recommended Fix:** Extract into a helper function:
```rust
fn extract_upstream_metrics(
    score: Option<&UpstreamScore>,
    metrics: Option<(f64, f64, u64, u64, u64)>,
    health: &HealthStatus
) -> UpstreamMetricsData
```

---

### 1.2 Inconsistent Error Types

The admin API uses three different error return patterns:

**Pattern 1: `Result<T, (StatusCode, String)>`**
- Location: `upstreams.rs`
- Example: `Err((StatusCode::BAD_REQUEST, "Invalid upstream ID".to_string()))`

**Pattern 2: `Result<T, StatusCode>`**
- Location: `alerts.rs`
- Example: `Err(StatusCode::NOT_FOUND)` - no message, just status

**Pattern 3: Custom Response Types**
- Location: `types.rs`
- Example: `SuccessResponse`, `UpdateSettingsResponse`

**Impact:**
- Inconsistent client experience
- No unified error schema
- Missing context in error responses

**Recommended Fix:** Create unified `AdminApiError` type:
```rust
pub struct AdminApiError {
    pub code: String,         // "UPSTREAM_NOT_FOUND"
    pub message: String,      // "Upstream with ID '5' not found"
    pub details: Option<Value>,
}
```

---

### 1.3 Magic Numbers

**Location 1**: `upstreams.rs` (lines 382-402) - Hardcoded scoring weights
```rust
weight: 8.0,  // Latency weight
weight: 4.0,  // Error rate weight
weight: 2.0,  // Block lag weight
```

**Location 2**: `prometheus.rs` (lines 120-125) - Cache settings
```rust
LruCache::new(NonZeroUsize::new(100))  // 100 entries
cache_ttl: Duration::from_secs(60)      // 60 second TTL
```

**Location 3**: `rate_limit.rs` (lines 131-134) - Cleanup interval
```rust
Duration::from_secs(300)  // 5 minutes hardcoded
```

**Impact:**
- No documentation of what values mean
- Hard to tune without code changes
- May not match actual configuration values

**Recommended Fix:** Define constants or pull from configuration.

---

### 1.4 Missing Validation

**Location**: `upstreams.rs` - `create_upstream()` (lines 839-881)

**Issues:**
- No weight validation (accepts 0 or extremely large values)
- No URL validation (SSRF risk - could be `file:///etc/passwd`)
- No name validation (empty strings, special characters accepted)

**Contrast with `update_weight()`** which DOES validate:
```rust
if request.weight == 0 || request.weight > 1000 {
    return Err((StatusCode::BAD_REQUEST, "Weight must be between 1 and 1000".to_string()));
}
```

**Recommended Fix:** Add validation middleware or helper:
```rust
fn validate_upstream_request(req: &CreateUpstreamRequest) -> Result<(), ValidationError>
```

---

## 2. Error Handling Issues (27 issues identified)

### 2.1 Prometheus Query Errors Silently Return Empty Data

**Locations:**
- `metrics.rs`: `get_latency()`, `get_request_volume()`, `get_request_methods()`, `get_error_distribution()`, `get_hedging_stats()`
- `prometheus.rs`: Multiple query methods

**Pattern:**
```rust
match prom_client.get_latency_percentiles(time_range).await {
    Ok(data) if !data.is_empty() => return Json(data),
    Ok(_) => tracing::debug!("no latency data from Prometheus"),
    Err(e) => tracing::warn!("failed to query Prometheus: {}", e),
}
// Returns fallback data - client doesn't know Prometheus failed!
```

**Impact:**
- Silent data loss - client receives empty data without knowing why
- Debugging nightmare - can't distinguish "no data" from "query failed"
- Misleading dashboards

**Recommended Fix:** Return response with data source indicator:
```rust
pub struct MetricsResponse<T> {
    pub data: T,
    pub source: DataSource,  // "prometheus" | "in_memory" | "fallback"
    pub warning: Option<String>,
}
```

---

### 2.2 Missing Request Context in Logs

**Current State:**
```rust
tracing::info!(
    upstream = %upstream_name,
    new_weight = request.weight,
    "updated upstream weight"
);
```

**Missing Context:**
- Admin user identity
- Request ID / correlation ID
- Previous value (for auditing)
- Client IP address

---

### 2.3 No Audit Trail for Mutations

**All write operations** lack persistent audit logging:
- `create_upstream`, `update_upstream`, `delete_upstream`
- `update_weight`, `reset_circuit_breaker`
- Alert rule CRUD operations
- API key management

**Missing:**
- Persistent audit log (not just ephemeral tracing)
- Before/after comparison for updates
- Authentication context
- Structured audit format with timestamps

---

### 2.4 Generic Error Messages

**Example 1** - No message at all:
```rust
Err(StatusCode::NOT_FOUND)  // What wasn't found?
```

**Example 2** - String matching for error types:
```rust
if e.to_string().contains("not found") {  // Fragile!
    (StatusCode::NOT_FOUND, e.to_string())
}
```

**Impact:**
- Client can't programmatically handle specific errors
- Missing context about what failed
- Fragile error handling based on string matching

---

## 3. Documentation Gaps

### 3.1 No Getting Started Guide

**Missing:**
- Quick start for first-time setup
- Example curl commands for common operations
- Authentication setup instructions
- How to configure `admin_token`

### 3.2 No Configuration Reference

**Undocumented settings in `config/development.toml`:**
```toml
[admin]
port = 3031
admin_token = "your-secure-token-here"
prometheus_url = "http://localhost:9090"
rate_limit_max_tokens = 100
rate_limit_refill_rate = 10
```

**Unanswered questions:**
- What happens if `admin_token` is not set?
- Is `prometheus_url` required?
- What's a good value for rate limit settings?

### 3.3 Not Mentioned in Main README

Users don't know:
- That an admin API exists
- What capabilities it provides
- How to access it

### 3.4 No Authentication/Security Guide

**Missing documentation:**
- How admin authentication works
- Dev mode behavior (no token)
- Security recommendations for production
- Rate limiting behavior

---

## Summary Table

| Category | Issue | Priority | Effort | Files Affected |
|----------|-------|----------|--------|----------------|
| Code Quality | Code duplication | High | Low | upstreams.rs |
| Code Quality | Inconsistent error types | High | Medium | All handlers |
| Code Quality | Magic numbers | Medium | Low | Multiple |
| Code Quality | Missing validation | High | Low | upstreams.rs |
| Error Handling | Silent Prometheus failures | High | Medium | metrics.rs, prometheus.rs |
| Error Handling | Missing request context | Medium | Low | All handlers |
| Error Handling | No audit trail | High | Medium | All write handlers |
| Error Handling | Generic error messages | Medium | Medium | All handlers |
| Documentation | No getting started guide | High | Medium | New file |
| Documentation | No configuration reference | Medium | Medium | New file |
| Documentation | Not in README | High | Low | README.md |
| Documentation | No security guide | Medium | Medium | New file |

---

## Remediation Priority

**Phase 1 - Critical (Before Release):**
1. Add input validation to `create_upstream`/`update_upstream`
2. Fix inconsistent error types
3. Add admin API section to README

**Phase 2 - High Priority:**
4. Add Prometheus error indication to responses
5. Extract duplicated code
6. Add audit logging for mutations

**Phase 3 - Medium Priority:**
7. Replace magic numbers with constants/config
8. Add request context to logs
9. Create configuration reference
10. Write security guide

---

*Document created: Analysis session for admin-api feature completion*
*Related files: See file paths in each issue section*
