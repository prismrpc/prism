# Prism Admin API Implementation Plan

## Overview

Implement a separate admin HTTP server on port 3031 (same process) for monitoring and managing the Prism RPC server, as specified in `admin-api-specs.md`.

## Architecture

```
┌──────────────────────────────────────────────────────┐
│              Prism RPC Server Process                │
│                                                      │
│  ┌────────────────────┐    ┌────────────────────┐   │
│  │  RPC HTTP Server   │    │  Admin HTTP Server │   │
│  │  Port: 3030        │    │  Port: 3031        │   │
│  │  POST /            │    │  GET /admin/*      │   │
│  │  GET /health       │    │  PUT /admin/*      │   │
│  │  GET /metrics      │    │  POST /admin/*     │   │
│  └─────────┬──────────┘    └─────────┬──────────┘   │
│            └────────────┬────────────┘              │
│                 Arc<ProxyEngine>                    │
│                 Arc<AppConfig>                      │
│                 Arc<ChainState>                     │
└──────────────────────────────────────────────────────┘
```

## Key Decisions

- **Scope**: Full spec implementation (all endpoints including write ops, alerts, API keys, logs, auth)
- **Admin Auth**: No authentication in Phase 1 (rely on localhost-only bind for security)
- **Time Series**: Query Prometheus HTTP API for historical metrics data

---

## Implementation Phases

### Phase 1: Foundation (Core Infrastructure)

#### 1.1 Add AdminConfig
**File:** `crates/prism-core/src/config/mod.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_admin_bind_address")]
    pub bind_address: String,  // default: "127.0.0.1"
    #[serde(default = "default_admin_port")]
    pub port: u16,  // default: 3031
    #[serde(default = "default_instance_name")]
    pub instance_name: String,
}
```

Add `admin: AdminConfig` field to `AppConfig` struct.

#### 1.2 Create Admin Module Structure
**New files:**
```
crates/server/src/
  admin/
    mod.rs           # Module root, AdminState, create_admin_router()
    types.rs         # Response types (SystemStatus, Upstream, CacheStats, etc.)
    handlers/
      mod.rs         # Handler module exports
      system.rs      # /admin/system/* handlers
      upstreams.rs   # /admin/upstreams/* handlers
      cache.rs       # /admin/cache/* handlers
      metrics.rs     # /admin/metrics/* handlers
```

#### 1.3 AdminState Struct
**File:** `crates/server/src/admin/mod.rs`

```rust
#[derive(Clone)]
pub struct AdminState {
    pub proxy_engine: Arc<ProxyEngine>,
    pub config: Arc<AppConfig>,
    pub chain_state: Arc<ChainState>,
    pub start_time: Instant,
}
```

#### 1.4 Dual Server Startup
**File:** `crates/server/src/main.rs`

Modify `main()` to:
1. Create `AdminState` from existing services
2. If `config.admin.enabled`:
   - Create admin router via `admin::create_admin_router()`
   - Bind separate `TcpListener` on admin port
   - Run both servers with `tokio::select!` for graceful shutdown

```rust
if config.admin.enabled {
    let admin_state = AdminState::new(...);
    let admin_app = admin::create_admin_router(admin_state);
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;

    tokio::select! {
        res = rpc_server.with_graceful_shutdown(shutdown_signal()) => { ... }
        res = admin_server.with_graceful_shutdown(shutdown_signal()) => { ... }
    }
}
```

---

### Phase 2: Core Read-Only Endpoints (MVP)

#### 2.1 System Endpoints
**File:** `crates/server/src/admin/handlers/system.rs`

| Endpoint | Handler | Data Source |
|----------|---------|-------------|
| `GET /admin/system/status` | `get_status()` | ProxyEngine::get_upstream_stats(), ChainState |
| `GET /admin/system/info` | `get_info()` | Build constants, AppConfig |
| `GET /admin/system/health` | `get_health()` | ProxyEngine::get_upstream_stats() |
| `GET /admin/system/settings` | `get_settings()` | AppConfig.server |

#### 2.2 Upstreams Endpoints (Read-Only)
**File:** `crates/server/src/admin/handlers/upstreams.rs`

| Endpoint | Handler | Data Source |
|----------|---------|-------------|
| `GET /admin/upstreams` | `list_upstreams()` | UpstreamManager::get_all_upstreams() |
| `GET /admin/upstreams/:id` | `get_upstream()` | UpstreamManager |
| `GET /admin/upstreams/:id/metrics` | `get_upstream_metrics()` | ScoringEngine, CircuitBreaker |

#### 2.3 Cache Endpoints
**File:** `crates/server/src/admin/handlers/cache.rs`

| Endpoint | Handler | Data Source |
|----------|---------|-------------|
| `GET /admin/cache/stats` | `get_stats()` | CacheManager::get_stats() |
| `GET /admin/cache/hit-rate` | `get_hit_rate()` | MetricsCollector |
| `GET /admin/cache/memory-allocation` | `get_memory_allocation()` | CacheManager |

#### 2.4 Metrics Endpoints
**File:** `crates/server/src/admin/handlers/metrics.rs`

| Endpoint | Handler | Data Source |
|----------|---------|-------------|
| `GET /admin/metrics/kpis` | `get_kpis()` | MetricsCollector::get_metrics_summary() |
| `GET /admin/metrics/latency` | `get_latency()` | Prometheus histograms |
| `GET /admin/metrics/request-volume` | `get_request_volume()` | Prometheus counters |

---

### Phase 3: Write Operations & Actions

#### 3.1 Upstream Actions
| Endpoint | Purpose |
|----------|---------|
| `POST /admin/upstreams/:id/health-check` | Trigger immediate health check |
| `PUT /admin/upstreams/:id/circuit-breaker/reset` | Reset circuit breaker |
| `PUT /admin/upstreams/:id/weight` | Update load balancing weight |

#### 3.2 Cache Actions
| Endpoint | Purpose |
|----------|---------|
| `POST /admin/cache/clear` | Clear specific cache types |
| `PUT /admin/cache/settings` | Update cache configuration |

---

### Phase 4: Alerts System

**New files:** `crates/server/src/admin/handlers/alerts.rs`

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/alerts` | List active/resolved alerts |
| `GET /admin/alerts/:id` | Get alert details |
| `POST /admin/alerts/:id/resolve` | Mark alert resolved |
| `POST /admin/alerts/bulk-resolve` | Bulk resolve alerts |
| `DELETE /admin/alerts/:id` | Dismiss alert |
| `GET /admin/alerts/rules` | List alert rules |
| `POST /admin/alerts/rules` | Create alert rule |
| `PUT /admin/alerts/rules/:id` | Update alert rule |
| `DELETE /admin/alerts/rules/:id` | Delete alert rule |
| `PUT /admin/alerts/rules/:id/toggle` | Enable/disable rule |

Requires: Alert evaluation background task, rule storage.

---

### Phase 5: API Keys Management

**New files:** `crates/server/src/admin/handlers/apikeys.rs`

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/apikeys` | List API keys |
| `GET /admin/apikeys/:id` | Get API key details |
| `POST /admin/apikeys` | Create new API key |
| `PUT /admin/apikeys/:id` | Update API key |
| `DELETE /admin/apikeys/:id` | Revoke API key |
| `POST /admin/apikeys/:id/revoke` | Revoke API key |
| `GET /admin/apikeys/:id/usage` | Get usage stats |

Leverages existing `crates/prism-core/src/auth/` module.

---

### Phase 6: Logs & Auth

**Logs endpoints:** `crates/server/src/admin/handlers/logs.rs`

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/logs` | Query logs with filters |
| `GET /admin/logs/export` | Export logs (JSON/CSV) |
| `GET /admin/logs/services` | List log services |

**Auth endpoints:** `crates/server/src/admin/handlers/auth.rs`

| Endpoint | Purpose |
|----------|---------|
| `POST /admin/auth/login` | Authenticate user |
| `POST /admin/auth/logout` | Logout user |
| `GET /admin/auth/me` | Get current user |
| `GET /admin/auth/settings` | Get auth config |
| `PUT /admin/auth/settings` | Update auth config |

---

## Critical Files to Modify

| File | Changes |
|------|---------|
| `crates/prism-core/src/config/mod.rs` | Add `AdminConfig` struct and field |
| `crates/server/src/main.rs` | Dual server startup with `tokio::select!` |
| `crates/server/src/lib.rs` | Export `admin` module |
| `crates/prism-core/src/proxy/engine.rs` | Add `get_upstream_manager()` accessor |
| `crates/prism-core/src/upstream/manager.rs` | Expose methods for upstream details |

## New Files to Create

| File | Purpose |
|------|---------|
| `crates/server/src/admin/mod.rs` | Module root, AdminState, router builder |
| `crates/server/src/admin/types.rs` | Response type definitions |
| `crates/server/src/admin/prometheus.rs` | Prometheus HTTP API client for time series |
| `crates/server/src/admin/handlers/mod.rs` | Handler module exports |
| `crates/server/src/admin/handlers/system.rs` | System endpoint handlers |
| `crates/server/src/admin/handlers/upstreams.rs` | Upstream endpoint handlers |
| `crates/server/src/admin/handlers/cache.rs` | Cache endpoint handlers |
| `crates/server/src/admin/handlers/metrics.rs` | Metrics endpoint handlers |
| `crates/server/src/admin/handlers/alerts.rs` | Alert endpoint handlers |
| `crates/server/src/admin/handlers/apikeys.rs` | API key endpoint handlers |
| `crates/server/src/admin/handlers/logs.rs` | Log endpoint handlers |
| `crates/server/src/admin/handlers/auth.rs` | Auth endpoint handlers |
| `crates/prism-core/src/alerts/mod.rs` | Alert rule storage & evaluation |
| `crates/prism-core/src/alerts/types.rs` | Alert types (Rule, Alert, etc.) |
| `crates/prism-core/src/alerts/evaluator.rs` | Background alert evaluation task |

## Response Types (admin/types.rs)

Key types to implement (matching TypeScript interfaces from specs):

```rust
// Enums
pub enum SystemHealth { Healthy, Degraded, Unhealthy }
pub enum UpstreamStatus { Healthy, Degraded, Unhealthy }
pub enum CircuitBreakerState { Closed, HalfOpen, Open }
pub enum Trend { Up, Down, Stable }

// Core response types
pub struct SystemStatus { instance_name, health, uptime, chain_tip, ... }
pub struct SystemInfo { version, build_date, git_commit, features, ... }
pub struct Upstream { id, name, url, status, circuit_breaker_state, ... }
pub struct CacheStats { block_cache, log_cache, transaction_cache }
pub struct KPIMetric { id, label, value, unit, change, trend, sparkline }
pub struct TimeSeriesPoint { timestamp, value }
pub struct LatencyDataPoint { timestamp, p50, p95, p99 }
```

## Testing Strategy

1. **Unit Tests**: Test each handler with mock state
2. **Integration Tests**: Spin up both servers, verify admin endpoints
3. **E2E Tests**: Test admin API with real upstream connections

## Configuration Example

```toml
[admin]
enabled = true
bind_address = "127.0.0.1"  # localhost-only by default
port = 3031
instance_name = "prod-us-east-1"
```

## Implementation Order

**Phase 1 - Foundation:**
1. AdminConfig in config/mod.rs
2. AdminState and admin module structure
3. Dual server startup in main.rs
4. Prometheus HTTP client for time series queries

**Phase 2 - Core Read Endpoints:**
5. System endpoints (status, info, health, settings)
6. Upstreams read endpoints (list, get, metrics, latency-distribution, health-history)
7. Cache endpoints (stats, hit-rate, hit-by-method, memory-allocation, settings)
8. Metrics endpoints (kpis, latency, request-volume, request-methods, error-distribution, hedging-stats)

**Phase 3 - Write Operations:**
9. Upstream CRUD (create, update, delete)
10. Upstream actions (health-check, weight update, circuit-breaker reset)
11. Cache actions (clear, settings update)
12. System settings update

**Phase 4 - Alerts System:**
13. Alert types and storage in prism-core
14. Alert evaluation background task
15. Alert CRUD endpoints
16. Alert rule CRUD endpoints

**Phase 5 - API Keys:**
17. API key CRUD endpoints
18. API key usage tracking endpoints

**Phase 6 - Logs & Auth:**
19. Log query and export endpoints
20. Auth endpoints (login, logout, me, settings)
