# Prism Admin API Implementation Guide

This document provides a comprehensive guide for implementing the admin API in the Prism RPC server. It combines the architecture decisions with the complete API specification.

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Implementation Details](#implementation-details)
3. [Data Models](#data-models)
4. [API Endpoints](#api-endpoints)
5. [Prometheus Integration](#prometheus-integration)
6. [Issues & Recommendations](#issues--recommendations)

---

## Architecture Overview

### Decision: Separate Admin HTTP Server (Same Process)

Run admin endpoints on a separate HTTP listener/port in the same process as the RPC server. This isolates admin traffic from the RPC hot path.

### Architecture Diagram

```
┌─────────────────────────────────────────────────────────┐
│              Prism RPC Server Process                    │
│                                                          │
│  ┌────────────────────┐      ┌──────────────────────┐   │
│  │  RPC HTTP Server    │      │  Admin HTTP Server    │   │
│  │  Port: 3030        │      │  Port: 3031           │   │
│  │                     │      │                       │   │
│  │  - POST /          │      │  - GET /admin/*       │   │
│  │  - POST /batch     │      │  - PUT /admin/*       │   │
│  │  - GET /health     │      │  - POST /admin/*      │   │
│  │  - GET /metrics    │      │                       │   │
│  └──────────┬─────────┘      └──────────┬────────────┘   │
│             │                           │                 │
│             └───────────┬───────────────┘                 │
│                         │                                 │
│              ┌──────────▼──────────┐                      │
│              │  Shared State       │                      │
│              │  (Arc<ProxyEngine> │                      │
│              │   Arc<CacheManager>│                      │
│              │   etc.)            │                      │
│              └────────────────────┘                      │
└──────────────────────────────────────────────────────────┘
```

### Design Principles

1. **Zero overhead on RPC path**: Admin routes are on a separate listener; RPC requests never hit admin code.
2. **Shared state**: Both servers share the same `Arc<>`-wrapped components (ProxyEngine, CacheManager, etc.).
3. **Complete isolation**: Admin traffic cannot affect RPC performance (separate TCP connections, concurrency limits, etc.).
4. **Distributed-friendly**: Each Prism instance exposes its own admin port; the dashboard connects directly to each instance.

### Multi-Instance Architecture

**Important**: The Prism Console is designed to manage **multiple Prism server instances** simultaneously. A single console instance can connect to and monitor multiple Prism servers (e.g., `prod-us-east-1`, `prod-eu-west-1`, `prod-asia-1`), which is why the server selector exists in the UI.

**How It Works**:
- Each Prism instance runs its own admin server on a separate port (default: 3031)
- The dashboard maintains a list of Prism instance URLs (each with its admin port)
- The dashboard makes HTTP requests directly to each instance's admin port
- The dashboard aggregates responses from all instances
- No intermediate service needed - direct connections from dashboard to each instance

**Benefits**:
- Lower latency (no intermediate hops)
- Simpler deployment (no separate admin service)
- Standard HTTP (easier to debug, firewall, etc.)
- Works with distributed instances across regions

---

## Implementation Details

### Server Startup

1. Create two separate Axum `Router` instances: one for RPC, one for admin
2. Bind two separate `TcpListener`s on different ports
3. Run both servers concurrently using `tokio::select!`
4. Both servers share the same `Arc<>` state (ProxyEngine, CacheManager, etc.)

### Request Routing

- **RPC requests** → RPC listener (port 3030) → RPC router → RPC handlers
- **Admin requests** → Admin listener (port 3031) → Admin router → Admin handlers
- **No cross-contamination**: RPC router has no admin routes; admin router has no RPC routes

### State Access

- Both servers hold `Arc<ProxyEngine>`, `Arc<CacheManager>`, etc.
- Admin handlers read/write the same state as RPC handlers
- Thread-safe via `Arc<>` and internal synchronization

### Configuration

- `admin_port`: Port for admin server (default: 3031, or disabled if not set)
- `admin_enabled`: Enable/disable admin server (default: false)
- `admin_bind_address`: Bind address for admin server (default: "0.0.0.0", can restrict to localhost)

### Security Model

- **Separate authentication**: Admin routes use admin API keys (separate from RPC API keys)
- **Network isolation**: Admin port can be firewalled separately
- **Optional localhost-only**: Admin can bind to 127.0.0.1 for local-only access
- **Can be disabled**: Admin server can be completely disabled in production

### Performance Characteristics

- **RPC path overhead**: Unchanged (~100-200ns route matching, same as current)
- **Admin path overhead**: Only affects admin requests, not RPC
- **Memory**: Minimal (shared state via `Arc<>`, separate router instances are small)
- **Network**: Direct connection from dashboard to each instance (no intermediate hops)

### Benefits Over Alternatives

- **vs. Same-port path-based routing**: Zero overhead on RPC path (no route prefix checks)
- **vs. Separate admin service**: No extra network hop, lower latency, simpler deployment
- **vs. Unix sockets/gRPC**: Works with distributed instances, standard HTTP, easier to debug

### Implementation Notes

- Both servers use the same graceful shutdown signal
- Both servers can have independent concurrency limits
- Admin server can use different middleware stack (auth, rate limiting, etc.)
- Admin routes are conditionally compiled/registered based on config

---

## Data Models

### System/Server Models

#### PrismServer
```typescript
interface PrismServer {
  id: string;
  name: string;
  url: string;
  region?: string;
  status: 'healthy' | 'degraded' | 'unhealthy';
}
```

**Purpose**: Represents a Prism instance/server that can be managed by the console.

**Usage**: Server selector, multi-instance management.

---

#### SystemStatus
```typescript
interface SystemStatus {
  instanceName: string;      // e.g., "prod-us-east-1"
  health: SystemHealth;        // 'healthy' | 'degraded' | 'unhealthy'
  uptime: string;              // e.g., "14d 7h 32m"
  chainTip: number;            // Latest block number
  chainTipAge: string;         // e.g., "3s ago"
  finalizedBlock: number;      // Latest finalized block
  version: string;             // e.g., "v0.8.2"
}
```

**Purpose**: Current operational status of the Prism instance.

**Usage**: 
- TopBar status pills (health, uptime, chain tip, finalized block)
- SystemInfo component in Settings page
- Dashboard header

**Note**: Contains both operational state and static info. Consider splitting if needed.

---

#### ServerSettings
```typescript
interface ServerSettings {
  bindAddress: string;          // e.g., "0.0.0.0"
  port: number;                 // e.g., 8545
  maxConcurrentRequests: number;
  requestTimeout: number;       // milliseconds
  keepAliveTimeout: number;     // seconds
}
```

**Purpose**: Server configuration settings.

**Usage**: Settings page (read-only display, can be updated via PUT).

---

#### AuthSettings
```typescript
interface AuthSettings {
  enabled: boolean;
  databaseConnected: boolean;
  databaseUrl?: string;
}
```

**Purpose**: Authentication configuration.

**Usage**: Settings page authentication tab.

---

### Upstream Models

#### Upstream
```typescript
interface Upstream {
  id: string;
  name: string;                 // e.g., "alchemy-mainnet"
  url: string;                  // e.g., "https://eth-mainnet.g.alchemy.com/v2/***"
  status: UpstreamStatus;       // 'healthy' | 'degraded' | 'unhealthy'
  circuitBreakerState: CircuitBreakerState;  // 'closed' | 'half-open' | 'open'
  compositeScore: number;       // 0-100 scoring
  latencyP90: number;           // milliseconds
  errorRate: number;            // percentage
  throttleRate: number;         // percentage
  blockLag: number;             // blocks behind chain tip
  requestsHandled: number;      // count in time window
  weight: number;               // load balancing weight
  wsConnected: boolean;
}
```

**Purpose**: Represents an RPC endpoint upstream with current metrics.

**Usage**: 
- Dashboard UpstreamsTable
- Upstreams list page
- Upstream detail page
- Settings upstream view

**Prometheus**: Most metrics can be derived from Prometheus (see API endpoints section).

---

#### UpstreamScoringFactor
```typescript
interface UpstreamScoringFactor {
  factor: string;               // e.g., "Latency", "Error Rate"
  weight: number;               // e.g., 8.0
  score: number;                // 0-1 normalized
  value: string;                // e.g., "P90: 38ms"
}
```

**Purpose**: Breakdown of composite score calculation.

**Usage**: Upstream detail page scoring breakdown.

---

#### HealthHistoryEvent
```typescript
interface HealthHistoryEvent {
  timestamp: string;            // ISO 8601
  type: 'status' | 'circuit-breaker' | 'error';
  status?: 'healthy' | 'degraded' | 'unhealthy';
  circuitBreakerState?: 'closed' | 'half-open' | 'open';
  message: string;
}
```

**Purpose**: Historical health status changes and events.

**Usage**: Upstream detail page health history timeline.

---

#### CircuitBreakerMetrics
```typescript
interface CircuitBreakerMetrics {
  state: 'closed' | 'half-open' | 'open';
  failureThreshold: number;
  currentFailures: number;
  timeRemaining?: number;       // seconds until half-open attempt
}
```

**Purpose**: Detailed circuit breaker state and metrics.

**Usage**: Upstream detail page circuit breaker visualization.

---

#### UpstreamRequestDistribution
```typescript
interface UpstreamRequestDistribution {
  method: string;               // e.g., "eth_call"
  requests: number;
  success: number;
  failed: number;
}
```

**Purpose**: Request distribution by method for a specific upstream.

**Usage**: Upstream detail page request distribution chart.

---

### Cache Models

#### CacheStats
```typescript
interface CacheStats {
  blockCache: BlockCacheStats;
  logCache: LogCacheStats;
  transactionCache: TransactionCacheStats;
}
```

**Purpose**: Overall cache statistics.

**Usage**: Cache page overview.

---

#### BlockCacheStats
```typescript
interface BlockCacheStats {
  hotWindowSize: number;
  lruEntries: number;
  memoryUsage: string;         // e.g., "256 MB"
  hitRate: number;             // percentage
}
```

**Purpose**: Block cache specific statistics.

**Prometheus**: Hit rate and memory can come from Prometheus.

---

#### LogCacheStats
```typescript
interface LogCacheStats {
  chunkCount: number;
  indexedBlocks: number;
  memoryUsage: string;
  partialFulfillment: number;   // percentage
}
```

**Purpose**: Log cache specific statistics.

---

#### TransactionCacheStats
```typescript
interface TransactionCacheStats {
  entries: number;
  receiptEntries: number;
  memoryUsage: string;
  hitRate: number;             // percentage
}
```

**Purpose**: Transaction cache specific statistics.

---

#### CacheHitDataPoint
```typescript
interface CacheHitDataPoint {
  timestamp: string;            // ISO 8601
  hit: number;                  // percentage
  partial: number;              // percentage
  miss: number;                 // percentage
}
```

**Purpose**: Cache hit rate over time (time series).

**Usage**: Dashboard cache hit chart.

**Prometheus**: Can be derived from Prometheus metrics.

---

#### CacheHitByMethod
```typescript
interface CacheHitByMethod {
  method: string;               // e.g., "eth_getBlockByNumber"
  hitRate: number;             // percentage
}
```

**Purpose**: Cache hit rate broken down by RPC method.

**Usage**: Cache page chart.

**Prometheus**: Can be derived from Prometheus metrics.

---

#### CacheSettings
```typescript
interface CacheSettings {
  retainBlocks: number;
  blockCacheMaxSize: string;     // e.g., "512 MB"
  logCacheMaxSize: string;
  transactionCacheMaxSize: string;
  cleanupInterval: number;       // seconds
}
```

**Purpose**: Cache configuration settings.

**Usage**: Settings page cache tab.

**Note**: ⚠️ Memory sizes stored as strings. Consider using numeric bytes for calculations.

---

#### MemoryAllocation
```typescript
interface MemoryAllocation {
  label: string;                // e.g., "Block Cache"
  value: number;                // bytes
  color: string;                 // for UI
}
```

**Purpose**: Memory usage breakdown for visualization.

**Usage**: Cache page memory allocation chart.

---

### Metrics Models

#### KPIMetric
```typescript
interface KPIMetric {
  id: string;                   // e.g., "request-rate"
  label: string;                // e.g., "Request Rate"
  value: string | number;       // e.g., "12.4K" or 87.3
  unit?: string;                // e.g., "/s" or "%"
  change?: number;              // percentage change
  trend: Trend;                 // 'up' | 'down' | 'stable'
  sparkline?: number[];         // last 7 data points
}
```

**Purpose**: Key Performance Indicator for dashboard cards.

**Usage**: Dashboard KPI cards.

**Prometheus**: All values can be derived from Prometheus.

---

#### Trend
```typescript
type Trend = 'up' | 'down' | 'stable';
```

**Purpose**: Trend direction for KPIs.

---

#### TimeSeriesPoint
```typescript
interface TimeSeriesPoint {
  timestamp: string;            // ISO 8601
  value: number;
}
```

**Purpose**: Generic time series data point.

**Usage**: Request volume charts, various time series visualizations.

---

#### LatencyDataPoint
```typescript
interface LatencyDataPoint {
  timestamp: string;            // ISO 8601
  p50: number;                 // milliseconds
  p95: number;
  p99: number;
}
```

**Purpose**: Latency percentiles over time.

**Usage**: 
- Dashboard latency chart
- Upstream detail latency distribution chart

**Prometheus**: Can be derived from histogram metrics.

---

#### RequestMethodData
```typescript
interface RequestMethodData {
  method: string;               // e.g., "eth_call"
  count: number;
  percentage: number;
}
```

**Purpose**: Request distribution by method.

**Usage**: Dashboard request method chart.

**Prometheus**: Can be derived from Prometheus metrics.

---

#### ErrorDistribution
```typescript
interface ErrorDistribution {
  type: string;                 // e.g., "Timeout", "RPC Error"
  count: number;
  percentage: number;
}
```

**Purpose**: Error breakdown by type.

**Usage**: Metrics page error distribution chart.

**Prometheus**: Can be derived from Prometheus metrics.

---

#### HedgingStats
```typescript
interface HedgingStats {
  hedgedRequests: string;       // e.g., "8.4%"
  hedgeTriggers: string;       // e.g., "2.1%"
  primaryWins: string;          // e.g., "72%"
  p99Improvement: string;      // e.g., "-18ms"
  winnerDistribution: {
    primary: number;             // percentage
    hedged: number;
  };
}
```

**Purpose**: Request hedging performance metrics.

**Usage**: Metrics page hedging analytics.

**Prometheus**: Can be derived from Prometheus metrics.

**Note**: ⚠️ Values stored as strings. Consider using numeric values with formatting.

---

### Log Models

#### Log
```typescript
interface Log {
  id: string;
  timestamp: string;            // ISO 8601
  level: LogLevel;              // 'info' | 'warn' | 'error' | 'debug' | 'success'
  service: string;              // e.g., "api", "upstream", "cache"
  message: string;
  details?: string;
  requestId?: string;
  upstream?: string;
}
```

**Purpose**: Application log entry.

**Usage**: Logs page table.

**Note**: ⚠️ LogLevel includes 'success' which is non-standard. Consider if this is intentional.

---

#### LogLevel
```typescript
type LogLevel = 'info' | 'warn' | 'error' | 'debug' | 'success';
```

**Purpose**: Log severity level.

---

### Alert Models

#### Alert
```typescript
interface Alert {
  id: string;
  name: string;
  severity: AlertSeverity;      // 'critical' | 'warning' | 'info'
  status: AlertStatus;          // 'active' | 'resolved'
  rule: string;                 // e.g., "Error Rate > 5%"
  triggeredAt: string;          // ISO 8601
  lastTriggered: string;        // e.g., "2 min ago" or ISO 8601
  count: number;                 // number of times triggered
  description: string;
}
```

**Purpose**: Active or resolved alert instance.

**Usage**: Alerts page table.

**Note**: ⚠️ `rule` stored as string. Consider referencing `AlertRule.id` instead.

**Note**: ⚠️ `lastTriggered` format inconsistent (sometimes relative, sometimes ISO). Standardize.

---

#### AlertRule
```typescript
interface AlertRule {
  id: string;
  name: string;
  type: AlertRuleType;
  enabled: boolean;
  threshold: number;
  unit: string;                  // e.g., "%", "ms", "upstreams"
  condition: AlertCondition;    // 'greater-than' | 'less-than' | 'equals'
  channels: string[];           // e.g., ["email", "slack", "webhook", "pagerduty"]
  lastTriggered: string;        // e.g., "2 min ago" or "Never"
}
```

**Purpose**: Alert rule configuration.

**Usage**: Alerts page rules tab.

**Notification Channels**: `'email' | 'slack' | 'webhook' | 'pagerduty'`

---

#### AlertSeverity
```typescript
type AlertSeverity = 'critical' | 'warning' | 'info';
```

**Purpose**: Alert severity level.

---

#### AlertStatus
```typescript
type AlertStatus = 'active' | 'resolved';
```

**Purpose**: Alert resolution status.

---

#### AlertRuleType
```typescript
type AlertRuleType = 
  | 'error-rate' 
  | 'upstream-health' 
  | 'latency' 
  | 'cache-hit-rate' 
  | 'circuit-breaker' 
  | 'quota';
```

**Purpose**: Type of metric/condition the alert monitors.

---

#### AlertCondition
```typescript
type AlertCondition = 'greater-than' | 'less-than' | 'equals';
```

**Purpose**: Comparison operator for alert threshold.

---

### API Key Models

#### ApiKey
```typescript
interface ApiKey {
  id: string;
  name: string;                 // e.g., "Production Frontend"
  keyPrefix: string;           // e.g., "pk_live_a8f3..."
  status: ApiKeyStatus;         // 'active' | 'inactive' | 'expired'
  created: string;              // ISO 8601 date
  expires: string;              // ISO 8601 date
  dailyUsage: number;
  dailyLimit: number;
  rateTokens: number;           // current rate limit tokens
  rateLimit: number;            // requests per second
  lastUsed: string;             // e.g., "2 min ago" or ISO 8601
}
```

**Purpose**: API key with usage and limits.

**Usage**: API Keys page table.

**Note**: ⚠️ `keyPrefix` shows partial key. Full key should only be shown on creation.

**Note**: ⚠️ `lastUsed` format inconsistent. Standardize.

---

#### ApiKeyStatus
```typescript
type ApiKeyStatus = 'active' | 'inactive' | 'expired';
```

**Purpose**: API key status.

---

#### ApiKeyFormValues
```typescript
interface ApiKeyFormValues {
  name: string;
  status: ApiKeyStatus;
  rateLimit: number;
  dailyLimit: number;
  expires?: Date;
}
```

**Purpose**: Form values for creating/editing API keys.

**Usage**: API key create/edit dialog.

---

## API Endpoints

**Important**: All admin endpoints are prefixed with `/admin/` and run on the separate admin port (default: 3031).

### System/Server Endpoints

#### `GET /admin/system/status`
**Purpose**: Returns current operational status of the Prism instance.

**Response**: `SystemStatus`
```json
{
  "instanceName": "prod-us-east-1",
  "health": "healthy",
  "uptime": "14d 7h 32m",
  "chainTip": 19847632,
  "chainTipAge": "3s ago",
  "finalizedBlock": 19847600,
  "version": "v0.8.2"
}
```

**Usage**: 
- TopBar status pills (health, uptime, chain tip, finalized block)
- SystemInfo component in Settings page
- Dashboard header

**Implementation Notes**:
- `health`: Aggregate from upstreams/circuit breakers
- `uptime`: Process uptime (not from Prometheus)
- `chainTip`/`chainTipAge`/`finalizedBlock`: From chain state (not Prometheus)
- `version`: Build-time constant
- `instanceName`: Configuration/identifier

**Prometheus**: ❌ Not applicable (operational state, not metrics)

---

#### `GET /admin/system/info`
**Purpose**: Static system information (build info, capabilities, configuration metadata).

**Response**:
```json
{
  "instanceName": "prod-us-east-1",
  "version": "v0.8.2",
  "buildDate": "2024-01-15T10:00:00Z",
  "gitCommit": "abc123def456",
  "features": ["hedging", "cache", "auth"],
  "capabilities": {
    "supportsWebSocket": true,
    "supportsBatch": true,
    "maxBatchSize": 100
  },
  "environment": "production"
}
```

**Usage**: 
- SystemInfo component (if expanded)
- Feature detection
- Debugging

**Implementation Notes**:
- Static/build-time data
- Can overlap with `/admin/system/status` - consider merging if redundant

**Prometheus**: ❌ Not applicable

---

#### `GET /admin/system/health`
**Purpose**: Simple health check endpoint (for liveness/readiness probes).

**Response**:
```json
{
  "status": "healthy",
  "timestamp": "2024-01-15T10:23:45Z"
}
```

**Usage**: 
- Health checks (load balancers, monitoring)
- Quick status check

**Implementation Notes**:
- Lightweight, fast response
- Can be subset of `/admin/system/status`

**Prometheus**: ❌ Not applicable

---

#### `GET /admin/system/settings`
**Purpose**: Get current server configuration.

**Response**: `ServerSettings`
```json
{
  "bindAddress": "0.0.0.0",
  "port": 8545,
  "maxConcurrentRequests": 1000,
  "requestTimeout": 30000,
  "keepAliveTimeout": 5
}
```

**Usage**: Settings page (read-only display)

**Implementation Notes**: Read from configuration

**Prometheus**: ❌ Not applicable (configuration)

---

#### `PUT /admin/system/settings`
**Purpose**: Update server configuration.

**Request**: `ServerSettings` (partial updates supported)
```json
{
  "maxConcurrentRequests": 2000,
  "requestTimeout": 60000
}
```

**Response**: Updated `ServerSettings`

**Usage**: Settings page (save changes)

**Implementation Notes**: 
- Requires restart/reload for some settings
- Validate ranges/timeouts

**Prometheus**: ❌ Not applicable (configuration)

---

### Upstream Endpoints

#### `GET /admin/upstreams`
**Purpose**: List all configured upstreams with current metrics.

**Response**: `Upstream[]`
```json
[
  {
    "id": "1",
    "name": "alchemy-mainnet",
    "url": "https://eth-mainnet.g.alchemy.com/v2/***",
    "status": "healthy",
    "circuitBreakerState": "closed",
    "compositeScore": 92.4,
    "latencyP90": 38,
    "errorRate": 0.12,
    "throttleRate": 0,
    "blockLag": 0,
    "requestsHandled": 45230,
    "weight": 100,
    "wsConnected": true
  }
]
```

**Usage**: 
- Dashboard UpstreamsTable
- Upstreams list page
- Settings upstream view

**Prometheus**: ✅ Can derive:
- `latencyP90`: `histogram_quantile(0.90, upstream_request_duration_seconds_bucket{upstream="..."})`
- `errorRate`: `rate(upstream_requests_total{upstream="...",status="error"}[5m]) / rate(upstream_requests_total{upstream="..."}[5m])`
- `requestsHandled`: `rate(upstream_requests_total{upstream="..."}[5m])`
- `status`: Derived from error rate/latency thresholds
- `circuitBreakerState`: From circuit breaker metrics
- `blockLag`: From chain sync metrics
- `compositeScore`: Calculated from above

**Custom Logic**: Composite scoring, circuit breaker state transitions

---

#### `GET /admin/upstreams/:id`
**Purpose**: Get single upstream details.

**Response**: `Upstream` (same as above)

**Usage**: Upstream detail page header

**Prometheus**: ✅ Same as `/admin/upstreams`

---

#### `POST /admin/upstreams`
**Purpose**: Create a new upstream.

**Request**:
```json
{
  "name": "alchemy-mainnet",
  "url": "https://eth-mainnet.g.alchemy.com/v2/***",
  "weight": 100,
  "wsConnected": true
}
```

**Response**: Created `Upstream` (with generated `id` and initial metrics)

**Usage**: Upstreams page create dialog

**Validation**: 
- `name`: Must match regex `/^[a-zA-Z0-9_-]+$/`, 1-100 characters
- `url`: Valid URL (http/https/ws/wss), must have valid hostname
- `weight`: Integer between 1 and 1000
- `wsConnected`: Boolean

**Prometheus**: ❌ Not applicable (configuration)

---

#### `PUT /admin/upstreams/:id`
**Purpose**: Update an existing upstream.

**Request**: Partial update supported
```json
{
  "name": "alchemy-mainnet-updated",
  "weight": 150,
  "wsConnected": false
}
```

**Response**: Updated `Upstream`

**Usage**: Upstreams page edit dialog

**Validation**: Same as POST

**Prometheus**: ❌ Not applicable (configuration)

---

#### `DELETE /admin/upstreams/:id`
**Purpose**: Delete an upstream.

**Response**:
```json
{
  "success": true
}
```

**Usage**: Upstreams page delete confirmation dialog

**Prometheus**: ❌ Not applicable (configuration)

---

#### `GET /admin/upstreams/:id/metrics`
**Purpose**: Get aggregated metrics for an upstream.

**Response**:
```json
{
  "upstream": { /* Upstream object */ },
  "scoringBreakdown": [
    {
      "factor": "Latency",
      "weight": 8.0,
      "score": 0.85,
      "value": "P90: 38ms"
    }
  ],
  "circuitBreaker": {
    "state": "closed",
    "failureThreshold": 10,
    "currentFailures": 0,
    "timeRemaining": null
  },
  "requestDistribution": [
    {
      "method": "eth_call",
      "requests": 42500,
      "success": 41650,
      "failed": 850
    }
  ]
}
```

**Usage**: Upstream detail page

**Prometheus**: ✅ Can derive:
- Scoring factors from latency/error metrics
- Request distribution: `sum by (method) (rate(upstream_requests_total{upstream="...",method=~".+"}[5m]))`
- Circuit breaker failures: `upstream_circuit_breaker_failures{upstream="..."}`

**Custom Logic**: Scoring calculation, circuit breaker state machine

---

#### `GET /admin/upstreams/:id/latency-distribution?timeRange=24h`
**Purpose**: Get latency percentiles over time for an upstream.

**Query Parameters**:
- `timeRange`: `'1h' | '24h' | '7d' | '30d'` (default: `'24h'`)

**Response**: `LatencyDataPoint[]`
```json
[
  {
    "timestamp": "2024-01-15T10:00:00Z",
    "p50": 25,
    "p95": 60,
    "p99": 120
  }
]
```

**Usage**: Upstream detail latency chart

**Prometheus**: ✅ 
```promql
# P50
histogram_quantile(0.50, rate(upstream_request_duration_seconds_bucket{upstream="..."}[5m]))
# P95
histogram_quantile(0.95, rate(upstream_request_duration_seconds_bucket{upstream="..."}[5m]))
# P99
histogram_quantile(0.99, rate(upstream_request_duration_seconds_bucket{upstream="..."}[5m]))
```
Use `range_query` with appropriate step (e.g., 1m for 24h).

---

#### `GET /admin/upstreams/:id/health-history?limit=50`
**Purpose**: Get recent health status changes and events for an upstream.

**Query Parameters**:
- `limit`: `number` (default: `50`)

**Response**: `HealthHistoryEvent[]`
```json
[
  {
    "timestamp": "2024-01-15T10:20:00Z",
    "type": "status",
    "status": "healthy",
    "message": "Status changed to healthy"
  },
  {
    "timestamp": "2024-01-15T10:15:00Z",
    "type": "circuit-breaker",
    "circuitBreakerState": "half-open",
    "message": "Circuit breaker half-open"
  }
]
```

**Usage**: Upstream detail health history timeline

**Prometheus**: ❌ Not ideal (event log). Store in time-series DB or event log.

**Custom Logic**: Track state transitions and events

---

#### `GET /admin/upstreams/:id/request-distribution?timeRange=24h`
**Purpose**: Get request distribution by method for an upstream.

**Query Parameters**:
- `timeRange`: `'1h' | '24h' | '7d' | '30d'` (default: `'24h'`)

**Response**: `UpstreamRequestDistribution[]`
```json
[
  {
    "method": "eth_call",
    "requests": 42500,
    "success": 41650,
    "failed": 850
  }
]
```

**Usage**: Upstream detail request distribution chart

**Prometheus**: ✅
```promql
# Total requests by method
sum by (method) (rate(upstream_requests_total{upstream="...",method=~".+"}[5m]))
# Success/failed breakdown
sum by (method, status) (rate(upstream_requests_total{upstream="...",method=~".+"}[5m]))
```

---

#### `GET /admin/upstreams/:id/scoring-breakdown`
**Purpose**: Get composite score calculation breakdown.

**Response**: `UpstreamScoringFactor[]`
```json
[
  {
    "factor": "Latency",
    "weight": 8.0,
    "score": 0.85,
    "value": "P90: 38ms"
  },
  {
    "factor": "Error Rate",
    "weight": 4.0,
    "score": 0.98,
    "value": "0.12%"
  }
]
```

**Usage**: Upstream detail scoring breakdown

**Prometheus**: ✅ Derive from latency/error metrics, then compute scores

**Custom Logic**: Scoring algorithm

---

#### `POST /admin/upstreams/:id/health-check`
**Purpose**: Trigger immediate health check for an upstream.

**Response**:
```json
{
  "success": true,
  "latency": 35,
  "error": null
}
```

**Usage**: "Force Health Check" button

**Prometheus**: ❌ Not applicable (one-time action)

---

#### `POST /admin/upstreams/health-check`
**Purpose**: Trigger immediate health check for all upstreams.

**Response**:
```json
{
  "success": true,
  "checked": 5,
  "results": [
    {
      "upstreamId": "1",
      "success": true,
      "latency": 35,
      "error": null
    }
  ]
}
```

**Usage**: Upstreams page "Force Health Check" button

**Prometheus**: ❌ Not applicable (one-time action)

---

#### `PUT /admin/upstreams/:id/weight`
**Purpose**: Update load balancing weight for an upstream.

**Request**:
```json
{
  "weight": 90
}
```

**Response**: Updated `Upstream`

**Usage**: Upstream management (if implemented)

**Prometheus**: ❌ Not applicable (configuration)

---

#### `PUT /admin/upstreams/:id/circuit-breaker/reset`
**Purpose**: Manually reset circuit breaker to closed state.

**Response**:
```json
{
  "success": true,
  "newState": "closed"
}
```

**Usage**: Upstream detail page (if implemented)

**Prometheus**: ❌ Not applicable (action)

---

### Cache Endpoints

#### `GET /admin/cache/stats`
**Purpose**: Get current cache statistics.

**Response**: `CacheStats`
```json
{
  "blockCache": {
    "hotWindowSize": 128,
    "lruEntries": 10240,
    "memoryUsage": "256 MB",
    "hitRate": 94.2
  },
  "logCache": {
    "chunkCount": 4096,
    "indexedBlocks": 500000,
    "memoryUsage": "512 MB",
    "partialFulfillment": 74.8
  },
  "transactionCache": {
    "entries": 50000,
    "receiptEntries": 48500,
    "memoryUsage": "128 MB",
    "hitRate": 81.3
  }
}
```

**Usage**: Cache page overview cards

**Prometheus**: ✅ Can derive:
- `hitRate`: `rate(cache_hits_total{type="block"}[5m]) / rate(cache_requests_total{type="block"}[5m])`
- `memoryUsage`: `cache_memory_bytes{type="block"}`
- `entries`: `cache_entries{type="block"}`

**Custom Logic**: `hotWindowSize`, `indexedBlocks` (if not exposed)

---

#### `GET /admin/cache/hit-rate?timeRange=24h`
**Purpose**: Get cache hit rate over time.

**Query Parameters**:
- `timeRange`: `'1h' | '24h' | '7d' | '30d'` (default: `'24h'`)

**Response**: `CacheHitDataPoint[]`
```json
[
  {
    "timestamp": "2024-01-15T10:00:00Z",
    "hit": 87.3,
    "partial": 8.2,
    "miss": 4.5
  }
]
```

**Usage**: Dashboard cache hit chart

**Prometheus**: ✅
```promql
# Hit rate over time
rate(cache_hits_total[5m]) / rate(cache_requests_total[5m])
# Partial fulfillment
rate(cache_partial_hits_total[5m]) / rate(cache_requests_total[5m])
```

---

#### `GET /admin/cache/hit-by-method`
**Purpose**: Get cache hit rate by RPC method.

**Response**: `CacheHitByMethod[]`
```json
[
  {
    "method": "eth_getBlockByNumber",
    "hitRate": 96.2
  },
  {
    "method": "eth_call",
    "hitRate": 0
  }
]
```

**Usage**: Cache page chart

**Prometheus**: ✅
```promql
sum by (method) (rate(cache_hits_total{method=~".+"}[5m])) 
/ 
sum by (method) (rate(cache_requests_total{method=~".+"}[5m]))
```

---

#### `GET /admin/cache/memory-allocation`
**Purpose**: Get memory usage breakdown.

**Response**: `MemoryAllocation[]`
```json
[
  {
    "label": "Block Cache",
    "value": 268435456,
    "color": "bg-primary"
  },
  {
    "label": "Log Cache",
    "value": 536870912,
    "color": "bg-accent"
  }
]
```

**Usage**: Cache page memory allocation chart

**Prometheus**: ✅ `cache_memory_bytes{type="block|log|transaction"}`

---

#### `GET /admin/cache/settings`
**Purpose**: Get current cache configuration.

**Response**: `CacheSettings`
```json
{
  "retainBlocks": 128,
  "blockCacheMaxSize": "512 MB",
  "logCacheMaxSize": "1 GB",
  "transactionCacheMaxSize": "256 MB",
  "cleanupInterval": 60
}
```

**Usage**: Settings page

**Prometheus**: ❌ Not applicable (configuration)

---

#### `PUT /admin/cache/settings`
**Purpose**: Update cache configuration.

**Request**: `CacheSettings` (partial updates supported)
```json
{
  "retainBlocks": 256,
  "blockCacheMaxSize": "1 GB"
}
```

**Response**: Updated `CacheSettings`

**Usage**: Settings page

**Prometheus**: ❌ Not applicable (configuration)

---

### Metrics Endpoints

#### `GET /admin/metrics/kpis`
**Purpose**: Get dashboard KPI metrics.

**Response**: `KPIMetric[]`
```json
[
  {
    "id": "request-rate",
    "label": "Request Rate",
    "value": "12.4K",
    "unit": "/s",
    "change": 8.2,
    "trend": "up",
    "sparkline": [8200, 9100, 10400, 11200, 10800, 12100, 12400]
  },
  {
    "id": "cache-hit-rate",
    "label": "Cache Hit Rate",
    "value": 87.3,
    "unit": "%",
    "change": 2.1,
    "trend": "up",
    "sparkline": [82, 84, 83, 85, 86, 86, 87]
  }
]
```

**Usage**: Dashboard KPI cards

**Prometheus**: ✅
- Request rate: `rate(requests_total[5m])`
- Cache hit rate: `rate(cache_hits_total[5m]) / rate(cache_requests_total[5m])`
- Active connections: `active_connections`
- Error rate: `rate(requests_total{status="error"}[5m]) / rate(requests_total[5m])`
- P99 latency: `histogram_quantile(0.99, rate(request_duration_seconds_bucket[5m]))`

**Custom Logic**: Trend calculation, sparkline aggregation

---

#### `GET /admin/metrics/latency?timeRange=24h`
**Purpose**: Get overall latency percentiles over time.

**Query Parameters**:
- `timeRange`: `'1h' | '24h' | '7d' | '30d'` (default: `'24h'`)

**Response**: `LatencyDataPoint[]` (same as upstream latency)

**Usage**: Dashboard latency chart

**Prometheus**: ✅ Same as upstream latency, aggregated across all upstreams

---

#### `GET /admin/metrics/request-volume?timeRange=24h`
**Purpose**: Get total request volume over time.

**Query Parameters**:
- `timeRange`: `'1h' | '24h' | '7d' | '30d'` (default: `'24h'`)

**Response**: `TimeSeriesPoint[]`
```json
[
  {
    "timestamp": "2024-01-15T10:00:00Z",
    "value": 12400
  }
]
```

**Usage**: Metrics page request volume chart

**Prometheus**: ✅ `rate(requests_total[5m])` with range query

---

#### `GET /admin/metrics/request-methods?timeRange=24h`
**Purpose**: Get request distribution by method.

**Query Parameters**:
- `timeRange`: `'1h' | '24h' | '7d' | '30d'` (default: `'24h'`)

**Response**: `RequestMethodData[]`
```json
[
  {
    "method": "eth_call",
    "count": 42500,
    "percentage": 34.2
  }
]
```

**Usage**: Dashboard request method chart

**Prometheus**: ✅ `sum by (method) (rate(requests_total{method=~".+"}[5m]))`

---

#### `GET /admin/metrics/error-distribution?timeRange=24h`
**Purpose**: Get error breakdown by type.

**Query Parameters**:
- `timeRange`: `'1h' | '24h' | '7d' | '30d'` (default: `'24h'`)

**Response**: `ErrorDistribution[]`
```json
[
  {
    "type": "Timeout",
    "count": 124,
    "percentage": 42.3
  },
  {
    "type": "RPC Error",
    "count": 89,
    "percentage": 30.4
  }
]
```

**Usage**: Metrics page error distribution chart

**Prometheus**: ✅ `sum by (error_type) (rate(errors_total{error_type=~".+"}[5m]))`

---

#### `GET /admin/metrics/hedging-stats?timeRange=24h`
**Purpose**: Get request hedging performance metrics.

**Query Parameters**:
- `timeRange`: `'1h' | '24h' | '7d' | '30d'` (default: `'24h'`)

**Response**: `HedgingStats`
```json
{
  "hedgedRequests": "8.4%",
  "hedgeTriggers": "2.1%",
  "primaryWins": "72%",
  "p99Improvement": "-18ms",
  "winnerDistribution": {
    "primary": 72,
    "hedged": 28
  }
}
```

**Usage**: Metrics page hedging analytics

**Prometheus**: ✅
- Hedged requests: `rate(hedged_requests_total[5m]) / rate(requests_total[5m])`
- Primary wins: `rate(hedged_primary_wins_total[5m]) / rate(hedged_requests_total[5m])`
- P99 improvement: Compare hedged vs non-hedged latency

**Custom Logic**: Improvement calculation

---

### Logs Endpoints

#### `GET /admin/logs?level=all&service=all&search=&limit=100&offset=0`
**Purpose**: Query application logs.

**Query Parameters**:
- `level`: `'all' | 'info' | 'warn' | 'error' | 'debug' | 'success'` (default: `'all'`)
- `service`: `'all' | string` (default: `'all'`)
- `search`: `string` (text search, default: `''`)
- `limit`: `number` (default: `100`)
- `offset`: `number` (default: `0`)

**Response**:
```json
{
  "logs": [
    {
      "id": "1",
      "timestamp": "2024-01-15T10:23:45Z",
      "level": "info",
      "service": "api",
      "message": "Request processed successfully",
      "requestId": "req-abc123",
      "upstream": "alchemy-mainnet"
    }
  ],
  "total": 150,
  "limit": 100,
  "offset": 0
}
```

**Usage**: Logs page table

**Prometheus**: ❌ Not applicable (structured logs)

**Custom Logic**: Log aggregation/search (e.g., Loki, Elasticsearch)

---

#### `GET /admin/logs/export?level=all&service=all&search=&format=json`
**Purpose**: Export logs in various formats.

**Query Parameters**: Same as `/admin/logs` plus:
- `format`: `'json' | 'csv' | 'txt'` (default: `'json'`)

**Response**: File download

**Usage**: Logs page export button

**Prometheus**: ❌ Not applicable

---

#### `GET /admin/logs/services`
**Purpose**: Get list of available log services.

**Response**:
```json
{
  "services": ["api", "upstream", "cache", "circuit-breaker"]
}
```

**Usage**: Logs page filter dropdown

**Prometheus**: ❌ Not applicable

---

### Alerts Endpoints

#### `GET /admin/alerts?status=active`
**Purpose**: List active or resolved alerts.

**Query Parameters**:
- `status`: `'active' | 'resolved' | 'all'` (default: `'active'`)

**Response**: `Alert[]`
```json
[
  {
    "id": "1",
    "name": "High Error Rate",
    "severity": "critical",
    "status": "active",
    "rule": "Error Rate > 5%",
    "triggeredAt": "2024-01-15T10:23:45Z",
    "lastTriggered": "2 min ago",
    "count": 3,
    "description": "Error rate exceeded threshold of 5%"
  }
]
```

**Usage**: Alerts page table

**Prometheus**: ❌ Not applicable (alert state)

**Custom Logic**: Alert evaluation and state management

---

#### `GET /admin/alerts/:id`
**Purpose**: Get single alert details.

**Response**: `Alert`

**Usage**: Alert details (if implemented)

---

#### `POST /admin/alerts/:id/resolve`
**Purpose**: Mark alert as resolved.

**Response**: Updated `Alert`

**Usage**: Alerts page resolve button

---

#### `POST /admin/alerts/bulk-resolve`
**Purpose**: Mark multiple alerts as resolved.

**Request**:
```json
{
  "ids": ["1", "2", "3"]
}
```

**Response**:
```json
{
  "success": true,
  "resolved": 3
}
```

**Usage**: Alerts page bulk resolve action

---

#### `DELETE /admin/alerts/:id`
**Purpose**: Dismiss/delete alert.

**Response**:
```json
{
  "success": true
}
```

**Usage**: Alerts page dismiss button

---

#### `DELETE /admin/alerts/bulk-dismiss`
**Purpose**: Dismiss multiple alerts.

**Request**:
```json
{
  "ids": ["1", "2", "3"]
}
```

**Response**:
```json
{
  "success": true,
  "dismissed": 3
}
```

**Usage**: Alerts page bulk dismiss action

---

#### `GET /admin/alerts/rules`
**Purpose**: List all alert rules.

**Response**: `AlertRule[]`
```json
[
  {
    "id": "1",
    "name": "High Error Rate",
    "type": "error-rate",
    "enabled": true,
    "threshold": 5,
    "unit": "%",
    "condition": "greater-than",
    "channels": ["email", "slack"],
    "lastTriggered": "2 min ago"
  }
]
```

**Usage**: Alerts page rules tab

**Prometheus**: ❌ Not applicable (rule configuration)

---

#### `POST /admin/alerts/rules`
**Purpose**: Create new alert rule.

**Request**: `Omit<AlertRule, 'id' | 'lastTriggered'>`
```json
{
  "name": "High Latency",
  "type": "latency",
  "enabled": true,
  "threshold": 500,
  "unit": "ms",
  "condition": "greater-than",
  "channels": ["email", "slack", "webhook", "pagerduty"]
}
```

**Response**: Created `AlertRule` (with generated `id` and `lastTriggered: "Never"`)

**Usage**: Alerts page create rule dialog

**Validation**: 
- `name`: Non-empty string, 1-100 characters
- `type`: One of the AlertRuleType values
- `enabled`: Boolean
- `threshold`: Non-negative number
- `unit`: Non-empty string, 1-20 characters
- `condition`: One of: `'greater-than' | 'less-than' | 'equals'`
- `channels`: Array of strings, minimum 1, valid values: `'email' | 'slack' | 'webhook' | 'pagerduty'`

---

#### `PUT /admin/alerts/rules/:id`
**Purpose**: Update alert rule.

**Request**: `Partial<AlertRule>`
```json
{
  "name": "High Latency Updated",
  "threshold": 600,
  "enabled": false,
  "channels": ["email"]
}
```

**Response**: Updated `AlertRule`

**Usage**: Alert rule editing dialog

**Validation**: Same as POST (all fields optional, but must be valid if provided)

---

#### `DELETE /admin/alerts/rules/:id`
**Purpose**: Delete alert rule.

**Response**:
```json
{
  "success": true
}
```

**Usage**: Alerts page delete rule

---

#### `PUT /admin/alerts/rules/:id/toggle`
**Purpose**: Enable/disable alert rule.

**Request**:
```json
{
  "enabled": false
}
```

**Response**: Updated `AlertRule`

**Usage**: Alerts page toggle rule

---

### API Keys Endpoints

#### `GET /admin/apikeys?search=`
**Purpose**: List API keys.

**Query Parameters**:
- `search`: `string` (name search, default: `''`)

**Response**: `ApiKey[]`
```json
[
  {
    "id": "1",
    "name": "Production Frontend",
    "keyPrefix": "pk_live_a8f3...",
    "status": "active",
    "created": "2024-01-15",
    "expires": "2025-01-15",
    "dailyUsage": 45230,
    "dailyLimit": 100000,
    "rateTokens": 850,
    "rateLimit": 1000,
    "lastUsed": "2 min ago"
  }
]
```

**Usage**: API Keys page table

**Prometheus**: ❌ Not applicable (configuration/state)

**Custom Logic**: API key management (database)

---

#### `GET /admin/apikeys/:id`
**Purpose**: Get single API key details.

**Response**: `ApiKey`

**Usage**: API key details (if implemented)

---

#### `POST /admin/apikeys`
**Purpose**: Create new API key.

**Request**: `ApiKeyFormValues`
```json
{
  "name": "New API Key",
  "status": "active",
  "rateLimit": 1000,
  "dailyLimit": 100000,
  "expires": "2025-12-31"
}
```

**Response**: Created `ApiKey` (includes `keyPrefix`, full key only on creation)
```json
{
  "id": "6",
  "name": "New API Key",
  "keyPrefix": "pk_live_xyz789...",
  "key": "pk_live_xyz789abcdef123456",  // Only returned on creation
  "status": "active",
  "created": "2024-01-15",
  "expires": "2025-12-31",
  "dailyUsage": 0,
  "dailyLimit": 100000,
  "rateTokens": 1000,
  "rateLimit": 1000,
  "lastUsed": "Never"
}
```

**Usage**: API Keys page create dialog

---

#### `PUT /admin/apikeys/:id`
**Purpose**: Update API key (name, limits, expiration).

**Request**: `Partial<ApiKeyFormValues>`
```json
{
  "name": "Updated Name",
  "rateLimit": 2000,
  "dailyLimit": 200000
}
```

**Response**: Updated `ApiKey`

**Usage**: API Keys page edit dialog

---

#### `DELETE /admin/apikeys/:id`
**Purpose**: Revoke/delete API key.

**Response**:
```json
{
  "success": true
}
```

**Usage**: API Keys page revoke dialog

---

#### `POST /admin/apikeys/:id/revoke`
**Purpose**: Revoke API key (same as DELETE).

**Response**:
```json
{
  "success": true
}
```

**Usage**: API Keys page revoke

---

#### `GET /admin/apikeys/:id/usage?timeRange=24h`
**Purpose**: Get API key usage statistics.

**Query Parameters**:
- `timeRange`: `'24h' | '7d' | '30d' | '90d'` (default: `'7d'`)

**Response**:
```json
{
  "keyId": 1,
  "totalRequests": 45230,
  "requestsToday": 1200,
  "lastUsed": "2024-01-15T10:23:45Z",
  "usageByMethod": [
    {
      "method": "eth_blockNumber",
      "count": 25000,
      "percentage": 55.3
    },
    {
      "method": "eth_getLogs",
      "count": 15000,
      "percentage": 33.2
    },
    {
      "method": "eth_call",
      "count": 5230,
      "percentage": 11.5
    }
  ],
  "dailyUsage": [
    {
      "date": "2024-01-15",
      "requests": 1200
    },
    {
      "date": "2024-01-14",
      "requests": 44030
    }
  ]
}
```

**Usage**: API key usage display

**Implementation Status**: ✅ Implemented

**Notes**:
- Returns aggregated usage data from the `api_key_usage` table
- `totalRequests` is the sum of all requests within the time range
- `requestsToday` comes from the `daily_requests_used` field in `api_keys` table
- `usageByMethod` shows breakdown by RPC method with percentages
- `dailyUsage` shows daily request counts sorted by date descending

**Prometheus**: ✅ Can derive:
- Usage: `sum(rate(api_key_requests_total{api_key="..."}[5m]))`
- Rate tokens: Rate limiter state

**Custom Logic**: Rate limiter state, daily limits

---

### Authentication Endpoints

#### `POST /admin/auth/login`
**Purpose**: Authenticate user.

**Request**:
```json
{
  "username": "admin",
  "password": "password"
}
```

**Response**:
```json
{
  "token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
  "user": {
    "id": "1",
    "username": "admin"
  }
}
```

**Usage**: Login (if implemented)

---

#### `POST /admin/auth/logout`
**Purpose**: Logout user.

**Response**:
```json
{
  "success": true
}
```

**Usage**: Logout

---

#### `GET /admin/auth/me`
**Purpose**: Get current user information.

**Response**:
```json
{
  "id": "1",
  "username": "admin",
  "permissions": ["read", "write", "admin"]
}
```

**Usage**: User profile/authorization

---

#### `GET /admin/auth/settings`
**Purpose**: Get authentication configuration.

**Response**: `AuthSettings`
```json
{
  "enabled": true,
  "databaseConnected": true,
  "databaseUrl": "postgresql://localhost:5432/prism"
}
```

**Usage**: Settings page auth tab

---

#### `PUT /admin/auth/settings`
**Purpose**: Update authentication configuration.

**Request**: `AuthSettings`
```json
{
  "enabled": true,
  "databaseUrl": "postgresql://newhost:5432/prism"
}
```

**Response**: Updated `AuthSettings`

**Usage**: Settings page save

---

## Prometheus Integration

### Summary: Prometheus vs Custom Endpoints

#### ✅ Can Use Prometheus (Metrics/Time-Series)
- `/admin/metrics/*` - All metrics endpoints (KPIs, latency, request volume, methods, errors, hedging)
- `/admin/upstreams` - Metrics portion (latency, error rate, requests handled, etc.)
- `/admin/upstreams/:id/metrics` - Metrics portion
- `/admin/upstreams/:id/latency-distribution` - Latency percentiles
- `/admin/upstreams/:id/request-distribution` - Request distribution by method
- `/admin/cache/stats` - Hit rates, memory usage
- `/admin/cache/hit-rate` - Cache hit rate over time
- `/admin/cache/hit-by-method` - Cache hit rate by method
- `/admin/cache/memory-allocation` - Memory usage
- `/admin/apikeys/:id/usage` - Usage metrics

#### ❌ Custom Implementation Required (Configuration/State/Actions)
- `/admin/system/status` - Operational state
- `/admin/system/info` - Static info
- `/admin/system/health` - Health check
- `/admin/system/settings` - Configuration
- `/admin/upstreams` - Configuration portion (weight, URL, name)
- `/admin/upstreams/:id/health-check` - Action
- `/admin/upstreams/:id/weight` - Configuration
- `/admin/upstreams/:id/circuit-breaker/reset` - Action
- `/admin/upstreams/:id/health-history` - Event log
- `/admin/cache/settings` - Configuration
- `/admin/logs/*` - Structured logs
- `/admin/alerts/*` - Alert state/rules
- `/admin/apikeys/*` - API key management
- `/admin/auth/*` - Authentication

#### 🔀 Hybrid (Prometheus + Custom Logic)
- `/admin/upstreams/:id/scoring-breakdown` - Prometheus data + scoring algorithm
- `/admin/metrics/kpis` - Prometheus data + trend/sparkline calculation
- `/admin/metrics/hedging-stats` - Prometheus data + improvement calculation

### Prometheus Query Examples

#### Request Rate
```promql
rate(requests_total[5m])
```

#### Cache Hit Rate
```promql
rate(cache_hits_total[5m]) / rate(cache_requests_total[5m])
```

#### Upstream Latency P90
```promql
histogram_quantile(0.90, rate(upstream_request_duration_seconds_bucket{upstream="alchemy-mainnet"}[5m]))
```

#### Upstream Error Rate
```promql
rate(upstream_requests_total{upstream="alchemy-mainnet",status="error"}[5m]) 
/ 
rate(upstream_requests_total{upstream="alchemy-mainnet"}[5m])
```

#### Request Distribution by Method
```promql
sum by (method) (rate(requests_total{method=~".+"}[5m]))
```

#### Error Distribution by Type
```promql
sum by (error_type) (rate(errors_total{error_type=~".+"}[5m]))
```

#### Cache Memory Usage
```promql
cache_memory_bytes{type="block"}
```

#### API Key Usage
```promql
sum(rate(api_key_requests_total{api_key="pk_live_..."}[5m]))
```

---

## Issues & Recommendations

### Missing Data Models

1. ✅ **HealthHistoryEvent** - Added to specification
2. ✅ **CircuitBreakerMetrics** - Added to specification
3. ✅ **UpstreamRequestDistribution** - Added to specification
4. **PrismServer** - Not exported from `models/index.ts` (exists in `PrismServerContext.tsx`)

### Data Inconsistencies

1. **Upstream Latency**: Model has `latencyP90` but charts show P50/P95/P99. Clarify if P90 is separate metric or should be derived.
2. **Cache Memory**: `memoryUsage` stored as string (e.g., "256 MB"). Consider numeric bytes for calculations.
3. **Time Ranges**: No standard enum/type for time ranges (`'1h' | '24h' | '7d' | '30d'`).
4. **Alert `lastTriggered`**: Format inconsistent (sometimes relative "2 min ago", sometimes ISO). Standardize.
5. **HedgingStats**: Values stored as strings. Consider numeric values with formatting.
6. **SystemStatus.uptime**: String format. Consider ISO duration or numeric seconds.
7. **CacheSettings sizes**: Strings instead of numeric bytes.

### Missing API Capabilities

1. ✅ **Upstream Management**: Create/update/delete endpoints for upstreams - **Added**
2. ✅ **Upstream Configuration**: Weight updates endpoint - **Added**
3. ✅ **Alert Rule Creation**: Create/update/delete endpoints - **Added**
4. **API Key Full Key**: Only `keyPrefix` shown. No endpoint to retrieve full key (security by design?).
5. **Pagination**: Logs/metrics endpoints lack pagination parameters.
6. **Filtering**: Time range filtering not consistently defined.

### Questionable Design Decisions

1. **ApiKey.keyPrefix**: Partial key shown. Clarify if full key is ever exposed.
2. **Alert.rule**: Stored as string. Consider referencing `AlertRule.id`.
3. **LogLevel**: Includes 'success' which is non-standard.
4. **HedgingStats**: String values instead of numbers.

### Recommendations

1. ✅ **Add Missing Models**: Create `HealthHistoryEvent`, `CircuitBreakerMetrics`, `UpstreamRequestDistribution` types - **Added**
2. **Export PrismServer**: Add to `models/index.ts`.
3. **Standardize Time Formats**: Use ISO 8601 for timestamps, numeric for durations.
4. **Standardize Memory/Sizes**: Use numeric bytes with display formatting.
5. **Add Pagination**: Include `limit`, `offset`, `total` in list responses.
6. **Add Time Range Filtering**: Standardize `timeRange` enum.
7. **Clarify API Key Security**: Document when/if full keys are exposed.
8. **Add Real-time Endpoints**: WebSocket/SSE for live metrics/logs.
9. ✅ **Add Bulk Operations**: Bulk resolve/dismiss for alerts - **Added**

---

## Appendix: Time Range Standardization

### Recommended Time Range Enum
```typescript
type TimeRange = '1h' | '24h' | '7d' | '30d';
```

### Prometheus Range Query Steps
- `1h`: 1 minute step
- `24h`: 1 minute step
- `7d`: 5 minute step
- `30d`: 15 minute step

---

*Last Updated: 2024-01-15*
*Version: 2.0*

