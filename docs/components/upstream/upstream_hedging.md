# Upstream Hedging

**File**: `crates/prism-core/src/upstream/hedging.rs`

**Status**:  **Integrated** - Hedging is fully integrated into all request handlers via `send_request_auto()`.

## Overview

The hedging module implements request hedging for improving tail latency in RPC requests. When a primary request takes longer than expected, hedged requests are automatically sent to different upstream endpoints. The first successful response is returned, dramatically improving P95 and P99 latencies.

This feature is inspired by eRPC's hedge failsafe mechanism and follows the principle of "sending duplicate requests to avoid waiting on slow responses."

### Integration Status

Hedging is automatically used by all handlers when enabled in configuration:

| Handler | Method | Status |
|---------|--------|--------|
| `ProxyEngine` | `forward_to_upstream()` |  Uses `send_request_auto()` |
| `LogsHandler` | `forward_to_upstream()` |  Uses `send_request_auto()` |
| `BlocksHandler` | `forward_to_upstream()` |  Uses `send_request_auto()` |
| `TransactionsHandler` | `forward_to_upstream()` |  Uses `send_request_auto()` |

**Key Method**: All handlers use `UpstreamManager::send_request_auto()` which delegates to the router (SmartRouter by default). The router automatically selects hedging when:
- Hedging is enabled (`hedging.enabled = true`)
- Scoring is disabled (hedging has lower priority than scoring)
- Method does not require consensus (consensus has highest priority)

To enable hedging, set `hedging.enabled = true` in your configuration. No code changes required - the router handles strategy selection automatically. See [Upstream Router documentation](./upstream_router.md) for routing priority details.

### Core Responsibilities

- Tracking latency percentiles (P50/P90/P95/P99) per upstream endpoint
- Calculating optimal hedge delays based on historical latency data
- Executing parallel hedged requests using `tokio::select!`
- Racing requests and returning the first successful response

## Architecture

### Component Structure

```
HedgeExecutor
├── config: ArcSwap<HedgeConfig>
│   └── Runtime-updatable hedge configuration (lock-free reads)
└── latency_trackers: DashMap<String, LatencyTracker>
    └── Per-upstream latency percentile tracking

LatencyTracker
├── samples: VecDeque<u64>
│   └── Circular buffer of latency samples
├── max_samples: usize (default: 1000)
├── sum: u64 (for average calculation)
└── count: usize
```

### Key Components

1. **HedgeConfig** (Lines 30-89)
   - Configuration for hedged request behavior
   - Controls when and how hedge requests are issued
   - Serde-serializable for TOML configuration

2. **LatencyTracker** (Lines 100-195)
   - Efficient sliding window percentile calculator
   - O(1) sample insertion, O(n log n) percentile calculation
   - Maintains last 1000 samples per upstream

3. **HedgeExecutor** (Lines 208-570+)
   - Main execution engine for hedged requests
   - Coordinates primary and hedge requests
   - Uses `tokio::select!` for racing

## Configuration

### HedgeConfig

**Lines 30-89** (`crates/prism-core/src/upstream/hedging.rs`)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Whether hedging is enabled |
| `latency_quantile` | `f64` | `0.95` | Percentile for hedge trigger (0.0-1.0) |
| `min_delay_ms` | `u64` | `50` | Minimum delay before hedging |
| `max_delay_ms` | `u64` | `2000` | Maximum delay before hedging |
| `max_parallel` | `usize` | `2` | Maximum parallel requests (including primary) |

**TOML Configuration:**
```toml
[hedging]
enabled = true
latency_quantile = 0.95
min_delay_ms = 50
max_delay_ms = 2000
max_parallel = 2
```

**Environment Variables:**
```bash
PRISM__HEDGING__ENABLED=true
PRISM__HEDGING__LATENCY_QUANTILE=0.99
PRISM__HEDGING__MIN_DELAY_MS=100
PRISM__HEDGING__MAX_DELAY_MS=1500
PRISM__HEDGING__MAX_PARALLEL=3
```

## Public API

### UpstreamManager Integration

#### `send_request_auto(&self, request: &JsonRpcRequest) -> Result<JsonRpcResponse, UpstreamError>`

**Location**: `crates/prism-core/src/upstream/manager.rs` (lines 216-234)

**Recommended Method**: This is the primary method for sending requests. It delegates to the router which automatically selects the optimal strategy based on configuration.

**Parameters:**
- `request`: The JSON-RPC request to send

**Returns:**
- `Ok(JsonRpcResponse)`: Successful response from an upstream
- `Err(UpstreamError)`: All upstreams failed or no healthy upstreams available

**Behavior:**
1. Delegates to `router.route()` which implements routing strategy selection
2. Router checks: consensus (if method requires) → scoring (if enabled) → hedging (if enabled and scoring disabled) → simple (fallback)
3. If hedging is selected, router uses `HedgeExecutor::execute_hedged()` internally
4. If all upstreams are unhealthy and `unhealthy_behavior.enabled` is true, retries with exponential backoff

**Example:**
```rust
// Works with any configuration - router automatically adapts
let response = upstream_manager.send_request_auto(&request).await?;
```

**Note**: Hedging is now implemented in the router (`SmartRouter::route_with_hedge()`). The router automatically selects hedging when it's enabled and scoring is disabled. See [Upstream Router documentation](./upstream_router.md) for details.

---

### HedgeConfig

#### `default() -> Self`

**Location**: Lines 79-88

Creates a default configuration with hedging disabled.

**Default Values:**
- `enabled`: false
- `latency_quantile`: 0.95 (P95)
- `min_delay_ms`: 50ms
- `max_delay_ms`: 2000ms
- `max_parallel`: 2

---

### LatencyTracker

#### `new(max_samples: usize) -> Self`

**Location**: Lines 112-124

Creates a new latency tracker with the specified window size.

**Parameters:**
- `max_samples`: Maximum number of latency samples to retain (default: 1000)

**Returns:** New `LatencyTracker` instance

---

#### `record(&mut self, latency_ms: u64)`

**Location**: Lines 126-142

Records a new latency sample. If the window is full, removes the oldest sample first.

**Parameters:**
- `latency_ms`: Latency measurement in milliseconds

**Complexity:** O(1) amortized

---

#### `percentile(&self, quantile: f64) -> Option<u64>`

**Location**: Lines 144-171

Calculates the specified percentile from recorded samples.

**Parameters:**
- `quantile`: Percentile to calculate (0.0-1.0, e.g., 0.95 for P95)

**Returns:**
- `Some(latency_ms)`: Latency value at the specified percentile
- `None`: If no samples exist or quantile is out of range

**Complexity:** O(n log n) where n is the number of samples

---

#### `average(&self) -> Option<u64>`

**Location**: Lines 173-181

Returns the average latency across all samples.

**Returns:**
- `Some(avg_ms)`: Average latency in milliseconds
- `None`: If no samples exist

**Complexity:** O(1)

---

### HedgeExecutor

#### `new(config: HedgeConfig) -> Self`

**Location**: Lines 213-221

Creates a new hedge executor with the given configuration.

**Parameters:**
- `config`: Initial hedging configuration

**Returns:** New `HedgeExecutor` instance

---

#### `update_config(&self, config: HedgeConfig)`

**Location**: Lines 223-228

Updates the configuration at runtime without restarting the executor.

**Parameters:**
- `config`: New hedging configuration to apply

**Thread Safety:** Async write lock on configuration

---

#### `get_config(&self) -> HedgeConfig`

**Location**: Lines 230-233

Returns a copy of the current configuration.

**Returns:** Current `HedgeConfig`

**Thread Safety:** Async read lock on configuration

---

#### `record_latency(&self, upstream_name: &str, latency_ms: u64)`

**Location**: Lines 235-261

Records a latency measurement for an upstream endpoint. Used internally to track response times for hedging decisions.

**Parameters:**
- `upstream_name`: Name of the upstream endpoint
- `latency_ms`: Response latency in milliseconds

**Side Effects:**
- Creates new `LatencyTracker` if upstream hasn't been seen before
- Logs latency with P95 and average at DEBUG level

---

#### `execute_hedged(&self, request: &JsonRpcRequest, upstreams: Vec<Arc<UpstreamEndpoint>>) -> Result<JsonRpcResponse, UpstreamError>`

**Location**: Lines 301-344

Executes a request with hedging across multiple upstreams.

**Parameters:**
- `request`: The JSON-RPC request to send
- `upstreams`: List of upstream endpoints (first is primary)

**Returns:**
- `Ok(JsonRpcResponse)`: First successful response
- `Err(UpstreamError::NoHealthyUpstreams)`: No upstreams provided or all failed

**Behavior:**
1. If hedging disabled or only one upstream: uses simple execution
2. Calculates hedge delay from primary upstream's latency profile
3. Sends primary request immediately
4. If primary completes before hedge delay: returns response
5. If hedge delay expires: sends parallel requests to additional upstreams
6. Returns first successful response

---

## Internal Methods

### `calculate_hedge_delay(&self, upstream_name: &str) -> Option<Duration>`

**Location**: Lines 263-284

Calculates the optimal hedge delay for a specific upstream based on historical latency data.

**Parameters:**
- `upstream_name`: Name of the upstream to calculate delay for

**Returns:**
- `Some(Duration)`: Delay to wait before hedging
- `None`: Insufficient latency data available

**Algorithm:**
1. Get configured latency percentile (e.g., P95)
2. Clamp between `min_delay_ms` and `max_delay_ms`
3. Return as `Duration`

---

### `execute_simple(&self, request: &JsonRpcRequest, upstream: &Arc<UpstreamEndpoint>) -> Result<JsonRpcResponse, UpstreamError>`

**Location**: Lines 346-367

Executes a simple non-hedged request to a single upstream.

**Parameters:**
- `request`: The JSON-RPC request
- `upstream`: Single upstream to use

**Returns:** Response or error from the upstream

**Side Effects:** Records latency for successful responses

---

### `execute_with_hedging(&self, request: &JsonRpcRequest, upstreams: Vec<Arc<UpstreamEndpoint>>, max_parallel: usize, hedge_delay: Duration) -> Result<JsonRpcResponse, UpstreamError>`

**Location**: Lines 369-427

Core hedging logic using `tokio::select!` to race primary and hedged requests.

**Parameters:**
- `request`: The JSON-RPC request
- `upstreams`: All available upstreams
- `max_parallel`: Maximum number of parallel requests
- `hedge_delay`: Duration to wait before sending hedges

**Behavior:**
1. Starts primary request immediately
2. Uses `tokio::select!` to race:
   - Primary response completion
   - Hedge delay timer expiration
3. If primary wins: returns response
4. If timer expires: issues parallel hedge requests

---

### `execute_parallel_hedged(&self, request: &JsonRpcRequest, upstreams: Vec<Arc<UpstreamEndpoint>>, max_parallel: usize, start_time: Instant) -> Result<JsonRpcResponse, UpstreamError>`

**Location**: Lines 429-490

Executes multiple hedged requests in parallel using `futures_util::select_all`.

**Parameters:**
- `request`: The JSON-RPC request
- `upstreams`: All upstreams to try
- `max_parallel`: Maximum number of parallel requests
- `start_time`: When the overall request started (for latency tracking)

**Returns:** First successful response, or error if all fail

**Behavior:**
1. Creates futures for up to `max_parallel` upstreams
2. Uses `futures_util::select_all` to race all requests
3. Returns immediately on first success
4. If a request fails, continues waiting for others
5. Records latency for successful responses

---

## Usage Examples

### Basic Usage with UpstreamManager

```rust
use prism_core::upstream::{UpstreamManager, UpstreamManagerConfig, HedgeConfig};

// Create manager with hedging enabled
let hedge_config = HedgeConfig {
    enabled: true,
    latency_quantile: 0.95,
    min_delay_ms: 100,
    max_delay_ms: 1500,
    max_parallel: 3,
};

let manager = UpstreamManager::with_config_and_hedging(
    UpstreamManagerConfig::default(),
    hedge_config,
);

// Add upstreams
manager.add_upstream(config1).await;
manager.add_upstream(config2).await;
manager.add_upstream(config3).await;

// Send request - automatically uses hedging when enabled
let response = manager.send_request_auto(&request).await?;
```

### Runtime Configuration Update

```rust
// Update hedging configuration at runtime
let new_config = HedgeConfig {
    enabled: true,
    latency_quantile: 0.99,  // Switch to P99
    min_delay_ms: 50,
    max_delay_ms: 1000,
    max_parallel: 2,
};

manager.update_hedge_config(new_config).await;
```

### Checking Latency Statistics

```rust
// Get latency stats for an upstream
if let Some((p50, p95, p99, avg)) = manager.get_upstream_latency_stats("alchemy").await {
    println!("P50: {}ms, P95: {}ms, P99: {}ms, Avg: {}ms", p50, p95, p99, avg);
}
```

### Advanced: Direct Hedging Control

Hedging is automatically selected by the router when enabled. Use `send_request_auto()` for automatic strategy selection:

```rust
// Recommended: Automatic selection based on config
// Router will select hedging if enabled and scoring is disabled
let response = manager.send_request_auto(&request).await?;
```

**Note**: All handlers in Prism use `send_request_auto()` for consistency and automatic optimization. The router handles strategy selection (consensus → scoring → hedging → simple) based on configuration.

## Performance Characteristics

### Memory Usage

- **LatencyTracker**: ~8KB per upstream (1000 samples × 8 bytes)
- **HedgeExecutor**: Minimal overhead (Arc references only)

### Latency Impact

| Scenario | Impact |
|----------|--------|
| Primary wins before hedge delay | Zero overhead |
| Hedge triggered, primary wins | One additional request (cancelled on completion) |
| Hedge triggered, hedge wins | Improved tail latency |
| All requests fail | No improvement |

### Throughput Impact

- **Worst case**: 2-3x upstream request rate (when hedging triggers frequently)
- **Typical case**: 10-30% increase in upstream requests
- **Best case**: No increase (primary always responds quickly)

## Metrics

The hedging system integrates with Prism's metrics via `MetricsCollector`:

| Metric | Type | Description |
|--------|------|-------------|
| `rpc_upstream_latency_p50_ms` | Gauge | P50 latency per upstream |
| `rpc_upstream_latency_p95_ms` | Gauge | P95 latency per upstream |
| `rpc_upstream_latency_p99_ms` | Gauge | P99 latency per upstream |
| `rpc_upstream_latency_avg_ms` | Gauge | Average latency per upstream |
| `rpc_hedged_requests_total` | Counter | Total hedge requests issued |
| `rpc_hedge_wins_total` | Counter | Requests won by hedge (not primary) |
| `rpc_hedge_delay_seconds` | Histogram | Distribution of hedge delays used |
| `rpc_hedge_skipped_total` | Counter | Hedges skipped (insufficient data) |

## Integration with Other Components

### UpstreamManager

The `HedgeExecutor` is integrated into the router system:

- **Router Integration**: `HedgeExecutor` is passed to `RoutingContext` and used by `SmartRouter`
- **Primary Method**: `send_request_auto()` (lines 216-234) - Delegates to router which selects hedging when appropriate
- **Router Method**: `SmartRouter::route_with_hedge()` - Implements hedged request execution
- **Config Update**: `update_hedge_config()` - Runtime configuration updates (via `HedgeExecutor`)
- **Priority**: Hedging is selected when enabled and scoring is disabled (priority 3 in router cascade)

### ProxyEngine

The `ProxyEngine` uses automatic strategy selection via `send_request_auto()`:

**Location**: `crates/prism-core/src/proxy/engine.rs` (lines 141-158)

```rust
// ProxyEngine automatically uses hedging when enabled via send_request_auto()
async fn forward_to_upstream(
    &self,
    request: &JsonRpcRequest,
) -> Result<JsonRpcResponse, ProxyError> {
    // Automatically use hedging if enabled, otherwise use response-time selection
    let response = self.upstream_manager.send_request_auto(request).await;
    // ... handle response
}
```

All handlers (ProxyEngine, BlocksHandler, TransactionsHandler, LogsHandler) use `send_request_auto()` which automatically delegates to hedging when enabled in configuration.

### AppConfig

Hedging configuration is part of `AppConfig` (config/mod.rs lines 215-217):

```rust
/// Request hedging configuration for improving tail latency.
#[serde(default)]
pub hedging: HedgeConfig,
```

## Testing

### Unit Tests

Located in `crates/prism-core/src/upstream/hedging.rs` (lines 575+):

1. `test_hedge_config_defaults` - Verifies default configuration values
2. `test_latency_tracker_basic` - Tests basic latency recording
3. `test_latency_tracker_sliding_window` - Tests circular buffer behavior
4. `test_latency_tracker_percentiles` - Tests percentile calculations
5. `test_latency_tracker_clear` - Tests clearing tracker data
6. `test_hedge_executor_creation` - Tests executor construction
7. `test_hedge_executor_config_update` - Tests runtime config updates
8. `test_hedge_executor_latency_recording` - Tests latency tracking integration
9. `test_hedge_executor_clear_data` - Tests clearing executor data

### Running Tests

```bash
# Run hedging tests only
cargo test --package prism-core --lib upstream::hedging

# Run all upstream tests
cargo test --package prism-core --lib upstream
```

## Troubleshooting

### Hedging Not Triggering

1. **Check if enabled**: Ensure `hedging.enabled = true` in config
2. **Check sample count**: Need ~100+ samples before hedging activates
3. **Check min_delay**: If `min_delay_ms` is too high, hedging may not trigger

### High Upstream Load

1. **Reduce max_parallel**: Lower from 3 to 2
2. **Increase min_delay**: Set `min_delay_ms` to 100-200ms
3. **Use P99 instead of P95**: Set `latency_quantile = 0.99`

### Inconsistent Latency Improvements

1. **Check upstream count**: Need 2+ healthy upstreams for hedging to work
2. **Review latency stats**: Use `get_upstream_latency_stats()` to check
3. **Enable debug logging**: Set `RUST_LOG=prism_core::upstream::hedging=debug`
