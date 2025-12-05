# Upstream Manager

**File**: `crates/prism-core/src/upstream/manager.rs`

## Overview

The `UpstreamManager` is the primary interface for managing and routing RPC requests to multiple upstream Ethereum RPC providers. It orchestrates load balancing, health checking, retry logic, circuit breaker management, and concurrency control across a pool of upstream endpoints. The manager acts as a high-level abstraction that coordinates the LoadBalancer, UpstreamEndpoint instances, and HttpClient to provide reliable RPC request routing with automatic failover and retry mechanisms.

### Core Responsibilities

- Managing the lifecycle of upstream RPC endpoints (add/remove operations)
- Routing RPC requests to healthy upstream providers with automatic strategy selection
- Implementing hedged requests for improved tail latency (P95/P99)
- Providing response-time-aware upstream selection
- Implementing configurable retry logic with exponential backoff
- Coordinating circuit breaker states across all upstreams
- Managing shared HTTP client resources with concurrency limits
- Exposing health and statistics information for monitoring

## Architecture

### Component Structure

```
UpstreamManager
├── load_balancer: Arc<LoadBalancer>
│   └── Manages pool of UpstreamEndpoint instances
├── config: Arc<RwLock<UpstreamManagerConfig>>
│   └── Runtime-updatable configuration
├── http_client: Arc<HttpClient>
│   └── Shared HTTP client with connection pooling
└── hedge_executor: Arc<HedgeExecutor>
    └── Handles hedged request execution and latency tracking
```

### Key Components

1. **LoadBalancer** (`Arc<LoadBalancer>`)
   - Maintains the pool of upstream endpoints
   - Implements multiple selection strategies (round-robin, response-time-aware, weighted)
   - Provides health-aware endpoint selection
   - Location: Line 19 (manager.rs)

2. **Configuration** (`Arc<RwLock<UpstreamManagerConfig>>`)
   - Thread-safe, runtime-updatable configuration
   - Controls retry behavior and circuit breaker settings
   - Protected by RwLock for concurrent read access
   - Location: Line 20 (manager.rs)

3. **HTTP Client** (`Arc<HttpClient>`)
   - Shared across all upstream endpoints
   - Manages connection pooling and concurrency limits
   - Single instance prevents resource exhaustion
   - Location: Line 21 (manager.rs)

4. **Hedge Executor** (`Arc<HedgeExecutor>`)
   - Manages hedged request execution for improved tail latency
   - Tracks per-upstream latency statistics (P50, P95, P99)
   - Automatically issues parallel requests based on latency quantiles
   - Runtime-configurable hedging parameters
   - Location: Line 22 (manager.rs)

### Configuration Structure

The `UpstreamManagerConfig` struct (lines 29-38 in manager.rs) provides the following settings:

- **max_retries** (default: 1): Maximum number of retry attempts for failed requests
- **retry_delay_ms** (default: 1000): Delay between retry attempts in milliseconds
- **circuit_breaker_threshold** (default: 2): Number of failures before circuit breaker opens
- **circuit_breaker_timeout_seconds** (default: 60): Time before attempting to close circuit breaker

Hedging is configured separately via `HedgeConfig` - see [Hedging Configuration and Management](#hedging-configuration-and-management) section.

## Public API

### Constructor Methods

**DEPRECATED**: The direct constructor methods (`new()`, `with_concurrency_limit()`, `with_config()`) are legacy APIs. **Use `UpstreamManagerBuilder` instead** for all new code.

#### `UpstreamManagerBuilder` Pattern (Recommended)

**Location**: `crates/prism-core/src/upstream/builder.rs`

The `UpstreamManagerBuilder` provides a fluent API for configuring the `UpstreamManager` with all required dependencies and optional features.

**Basic Usage**:
```rust
use prism_core::upstream::UpstreamManagerBuilder;
use prism_core::chain::ChainState;
use std::sync::Arc;

let chain_state = Arc::new(ChainState::new());

let upstream_manager = UpstreamManagerBuilder::new()
    .chain_state(chain_state)
    .concurrency_limit(1000)
    .build()
    .expect("Failed to initialize upstream manager");
```

**Builder Methods**:

- **`chain_state(Arc<ChainState>)`** - **REQUIRED** - Sets the shared chain state for finality tracking
- **`concurrency_limit(usize)`** - Optional - Sets HTTP client concurrency limit (default: 1000)
- **`config(UpstreamManagerConfig)`** - Optional - Sets retry and circuit breaker configuration
- **`scoring_config(ScoringConfig)`** - Optional - Configures scoring-based upstream selection
- **`enable_scoring()`** - Optional - Enables scoring with default configuration
- **`hedge_config(HedgeConfig)`** - Optional - Configures hedging for tail latency reduction
- **`enable_hedging()`** - Optional - Enables hedging with default configuration
- **`consensus_config(ConsensusConfig)`** - Optional - Configures consensus validation
- **`enable_consensus()`** - Optional - Enables consensus with default configuration
- **`router(Arc<dyn RequestRouter>)`** - Optional - Sets custom request router (default: `SmartRouter`)
- **`build() -> Result<UpstreamManager, BuilderError>`** - Constructs the `UpstreamManager`

**Builder Errors**:
- `BuilderError::MissingChainState` - `chain_state()` was not called (required parameter)
- `BuilderError::HttpClientInit(String)` - HTTP client initialization failed

**Advanced Configuration**:
```rust
use prism_core::upstream::{
    UpstreamManagerBuilder,
    UpstreamManagerConfig,
    ScoringConfig,
    HedgeConfig,
};

let chain_state = Arc::new(ChainState::new());

let upstream_manager = UpstreamManagerBuilder::new()
    .chain_state(chain_state)
    .concurrency_limit(5000)
    .config(UpstreamManagerConfig {
        max_retries: 3,
        retry_delay_ms: 500,
        circuit_breaker_threshold: 5,
        circuit_breaker_timeout_seconds: 120,
    })
    .enable_scoring()  // Enable response-time-aware selection
    .enable_hedging()  // Enable hedged requests for better P95/P99
    .build()?;
```

**See Also**:
- [Scoring Documentation](../scoring/README.md) - Response-time-aware upstream selection
- [Hedging Documentation](../hedging/README.md) - Tail latency reduction
- [Consensus Documentation](../consensus/README.md) - Multi-upstream validation
- [ChainState Documentation](../chain/chain_state.md) - Shared chain state pattern

---

#### Legacy Constructor Methods (Deprecated)

The following methods are maintained for backward compatibility but should not be used in new code:

##### `new() -> Self` ️ DEPRECATED

**Location**: Lines 56-58 (manager.rs)

Creates an `UpstreamManager` with default settings. **Use `UpstreamManagerBuilder` instead.**

##### `with_concurrency_limit(concurrent_limit: usize) -> Self` ️ DEPRECATED

**Location**: Lines 66-75 (manager.rs)

Creates an `UpstreamManager` with a concurrency limit. **Use `UpstreamManagerBuilder::new().concurrency_limit()` instead.**

##### `with_config(config: UpstreamManagerConfig) -> Self` ️ DEPRECATED

**Location**: Lines 83-89 (manager.rs)

Creates an `UpstreamManager` with custom configuration. **Use `UpstreamManagerBuilder::new().config()` instead.**

### Upstream Lifecycle Management

#### `add_upstream(&self, config: UpstreamConfig) -> Future<Output = ()>`

**Location**: Lines 95-98 (manager.rs)

Adds a new upstream RPC endpoint to the managed pool.

**Parameters**:
- `config`: Configuration for the upstream endpoint including URL, name, timeout, circuit breaker settings

**Returns**: Async operation that completes when upstream is added

**Usage**:
```rust
let config = UpstreamConfig {
    url: "https://eth-mainnet.alchemyapi.io/v2/KEY".to_string(),
    name: "alchemy".to_string(),
    weight: 1,
    timeout_seconds: 30,
    supports_websocket: true,
    ws_url: Some("wss://eth-mainnet.alchemyapi.io/v2/KEY".to_string()),
    circuit_breaker_threshold: 5,
    circuit_breaker_timeout_seconds: 60,
    chain_id: 1,
};
manager.add_upstream(config).await;
```

**Implementation Notes**:
- Logs upstream addition with name and URL
- Passes shared HttpClient instance to new endpoint
- Delegates to LoadBalancer for actual addition
- Operation is async and awaitable

---

#### `remove_upstream(&self, name: &str) -> Future<Output = ()>`

**Location**: Lines 104-107 (manager.rs)

Removes an upstream endpoint from the managed pool by name.

**Parameters**:
- `name`: The name identifier of the upstream to remove

**Returns**: Async operation that completes when upstream is removed

**Usage**:
```rust
manager.remove_upstream("alchemy").await;
```

**Implementation Notes**:
- Logs removal operation
- Delegates to LoadBalancer for actual removal
- Safe to call even if upstream doesn't exist
- Does not return success/failure status

### Request Routing

#### `send_request_auto(&self, request: &JsonRpcRequest) -> Result<JsonRpcResponse, UpstreamError>`

**Location**: Lines 216-234 (manager.rs)

**RECOMMENDED METHOD** - Automatically selects the optimal request strategy based on configuration.

This is the primary method for sending RPC requests. It delegates to the configured router (SmartRouter by default) which automatically selects the best routing strategy: consensus (for consensus-required methods), scoring-based selection, hedging, or simple response-time selection.

**Parameters**:
- `request`: JSON-RPC request to send

**Returns**:
- `Ok(JsonRpcResponse)`: Successful response from upstream
- `Err(UpstreamError)`: All attempts failed or no healthy upstreams available

**Usage**:
```rust
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_blockNumber".to_string(),
    params: None,
    id: serde_json::Value::Number(1.into()),
};

// Automatically uses router to select best strategy (consensus/scoring/hedging/simple)
match manager.send_request_auto(&request).await {
    Ok(response) => println!("Block: {:?}", response.result),
    Err(e) => eprintln!("Request failed: {}", e),
}
```

**Implementation Notes**:
- Delegates to `router.route()` which implements the routing strategy selection (line 220 in manager.rs)
- Router automatically selects: consensus (if method requires it), scoring (if enabled), hedging (if enabled and scoring disabled), or simple routing (fallback)
- If all upstreams are unhealthy and `unhealthy_behavior.enabled` is true, retries with exponential backoff (lines 222-230)
- Used by all handlers (`ProxyEngine`, `LogsHandler`, `BlocksHandler`, `TransactionsHandler`)
- See [Upstream Router documentation](./upstream_router.md) for detailed routing strategy information

---

**Note**: The `send_request_with_hedge()` and `send_request_with_response_time()` methods have been moved to the router implementation. Routing strategy selection is now handled by the `SmartRouter` which is configured via `UpstreamManagerBuilder`. See [Upstream Router documentation](./upstream_router.md) for details on routing strategies.

---

#### `send_request(&self, request: &JsonRpcRequest) -> Result<JsonRpcResponse, UpstreamError>`

**Location**: Lines 144-183 (manager.rs)

**Note**: This is the legacy round-robin request method. Consider using `send_request_auto()` instead for better performance with response-time-aware selection or hedging.

Sends a JSON-RPC request using round-robin load balancing with automatic retry logic.

**Parameters**:
- `request`: JSON-RPC request to send

**Returns**:
- `Ok(JsonRpcResponse)`: Successful response from upstream
- `Err(UpstreamError)`: All retry attempts failed or no healthy upstreams available

**Error Handling**:
- `UpstreamError::CircuitBreakerOpen`: Circuit breaker is open, stops retrying
- `UpstreamError::NoHealthyUpstreams`: No healthy upstreams available, stops retrying
- Other errors: Triggers retry with delay

**Usage**:
```rust
// Legacy method - consider using send_request_auto() instead
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_blockNumber".to_string(),
    params: None,
    id: serde_json::Value::Number(1.into()),
};

match manager.send_request(&request).await {
    Ok(response) => println!("Block: {:?}", response.result),
    Err(e) => eprintln!("Request failed: {}", e),
}
```

**Implementation Notes**:
- Uses simple round-robin load balancing via `try_send_request` (line 152 in manager.rs)
- Reads config once at start to get retry settings (line 148 in manager.rs)
- Attempts up to `max_retries + 1` times (line 151 in manager.rs)
- Short-circuits on circuit breaker or no healthy upstream errors (lines 162-168 in manager.rs)
- Applies `retry_delay_ms` between attempts (lines 173-176 in manager.rs)
- Logs successful retries (line 155 in manager.rs)
- Returns last error if all attempts fail (line 182 in manager.rs)

### Health and Statistics

#### `get_all_upstreams(&self) -> Vec<Arc<UpstreamEndpoint>>`

**Location**: Lines 184-186 (manager.rs)

Returns all configured upstream endpoints regardless of health status.

**Returns**: Vector of all upstream endpoints wrapped in Arc

**Usage**:
```rust
let all_upstreams = manager.get_all_upstreams().await;
for upstream in all_upstreams {
    println!("Upstream: {}", upstream.config().name);
}
```

**Implementation Notes**:
- Returns Arc-wrapped endpoints to avoid cloning heavy objects
- Includes both healthy and unhealthy upstreams
- Delegates to LoadBalancer

---

#### `get_healthy_upstreams(&self) -> Vec<Arc<UpstreamEndpoint>>`

**Location**: Lines 190-192 (manager.rs)

Returns only the currently healthy upstream endpoints.

**Returns**: Vector of healthy upstream endpoints

**Usage**:
```rust
let healthy = manager.get_healthy_upstreams().await;
println!("{} healthy upstreams available", healthy.len());
```

**Implementation Notes**:
- Filters by health status and circuit breaker state
- Useful for monitoring and health checks
- May perform multiple async health checks

---

#### `get_stats(&self) -> LoadBalancerStats`

**Location**: Lines 195-197 (manager.rs)

Retrieves aggregated statistics about the load balancer and upstreams.

**Returns**: `LoadBalancerStats` containing:
- `total_upstreams`: Total number of configured upstreams
- `healthy_upstreams`: Number of currently healthy upstreams
- `average_response_time_ms`: Average response time across all upstreams

**Usage**:
```rust
let stats = manager.get_stats().await;
println!("Stats: {}/{} healthy, avg {}ms",
    stats.healthy_upstreams,
    stats.total_upstreams,
    stats.average_response_time_ms
);
```

**Implementation Notes**:
- Delegates to LoadBalancer for calculation
- May be expensive if many upstreams configured
- Response time averaged across all upstreams with recorded times

---

#### `has_healthy_upstreams(&self) -> bool`

**Location**: Lines 215-217 (manager.rs)

Quick check if any healthy upstreams are available.

**Returns**: `true` if at least one healthy upstream exists, `false` otherwise

**Usage**:
```rust
if !manager.has_healthy_upstreams().await {
    eprintln!("Warning: No healthy upstreams!");
}
```

**Implementation Notes**:
- Efficient check using `is_empty()` on filtered list
- Useful for pre-flight validation
- Returns quickly on first healthy upstream found

---

#### `total_upstreams(&self) -> usize`

**Location**: Lines 220-222 (manager.rs)

Returns the total number of configured upstreams.

**Returns**: Count of all upstreams

**Usage**:
```rust
println!("Total upstreams: {}", manager.total_upstreams().await);
```

---

#### `healthy_upstreams(&self) -> usize`

**Location**: Lines 225-227 (manager.rs)

Returns the count of currently healthy upstreams.

**Returns**: Count of healthy upstreams

**Usage**:
```rust
let healthy_count = manager.healthy_upstreams().await;
let total_count = manager.total_upstreams().await;
println!("Health: {}/{}", healthy_count, total_count);
```

### Configuration Management

#### `update_config(&self, config: UpstreamManagerConfig) -> Future<Output = ()>`

**Location**: Lines 203-207 (manager.rs)

Updates the manager's configuration at runtime.

**Parameters**:
- `config`: New configuration to apply

**Returns**: Async operation that completes when config is updated

**Usage**:
```rust
let new_config = UpstreamManagerConfig {
    max_retries: 5,
    retry_delay_ms: 2000,
    circuit_breaker_threshold: 10,
    circuit_breaker_timeout_seconds: 120,
};
manager.update_config(new_config).await;
```

**Implementation Notes**:
- Acquires write lock on config (line 204 in manager.rs)
- Replaces entire config atomically
- Logs configuration update
- Affects all subsequent requests
- Does not affect in-flight requests

---

#### `get_config(&self) -> UpstreamManagerConfig`

**Location**: Lines 210-212 (manager.rs)

Retrieves the current configuration.

**Returns**: Clone of current configuration

**Usage**:
```rust
let config = manager.get_config().await;
println!("Max retries: {}", config.max_retries);
```

**Implementation Notes**:
- Acquires read lock on config
- Returns cloned config to avoid holding lock
- Safe for concurrent access

### Circuit Breaker Management

#### `get_circuit_breaker_status(&self) -> Vec<(String, CircuitBreakerState, u32)>`

**Location**: Lines 301-313 (manager.rs)

Retrieves circuit breaker status for all upstreams.

**Returns**: Vector of tuples containing:
- Upstream name (String)
- Circuit breaker state (CircuitBreakerState: Closed, Open, or HalfOpen)
- Current failure count (u32)

**Usage**:
```rust
let status = manager.get_circuit_breaker_status().await;
for (name, state, failures) in status {
    println!("{}: {:?} ({} failures)", name, state, failures);
}
```

**Implementation Notes**:
- Queries all upstreams individually
- Useful for monitoring and debugging
- States: Closed (normal), Open (failing), HalfOpen (testing recovery)

---

#### `reset_circuit_breaker(&self, upstream_name: &str) -> bool`

**Location**: Lines 324-337 (manager.rs)

Manually resets the circuit breaker for a specific upstream.

**Parameters**:
- `upstream_name`: Name of the upstream to reset

**Returns**: `true` if upstream was found and reset, `false` otherwise

**Usage**:
```rust
if manager.reset_circuit_breaker("alchemy").await {
    println!("Circuit breaker reset successfully");
} else {
    eprintln!("Upstream not found");
}
```

**Implementation Notes**:
- Searches for upstream by name (line 328 in manager.rs)
- Calls `on_success()` to reset circuit breaker (line 329 in manager.rs)
- Logs reset operation (line 330 in manager.rs)
- Returns false if upstream not found (lines 335-336 in manager.rs)
- Useful for manual recovery after fixing upstream issues

### Resource Access

#### `get_http_client(&self) -> Arc<HttpClient>`

**Location**: Lines 341-343 (manager.rs)

Returns a reference to the shared HTTP client instance.

**Returns**: Arc-wrapped HttpClient

**Usage**:
```rust
let http_client = manager.get_http_client();
// Use for custom requests or testing
```

**Implementation Notes**:
- Returns cloned Arc (cheap operation)
- Same client instance used by all upstreams
- Useful for direct HTTP operations or testing
- Client has configured concurrency limits

### Hedging Configuration and Management

The following methods control hedging behavior for request routing. For the primary request methods, see the [Request Routing](#request-routing) section above.

---

#### `update_hedge_config(&self, hedge_config: HedgeConfig)`

**Location**: Lines 407-414 (manager.rs)

Updates the hedging configuration at runtime.

**Parameters**:
- `hedge_config`: New hedging configuration to apply

**Usage**:
```rust
use prism_core::upstream::HedgeConfig;

let new_config = HedgeConfig {
    enabled: true,
    latency_quantile: 0.99,
    min_delay_ms: 100,
    max_delay_ms: 1500,
    max_parallel: 3,
};
manager.update_hedge_config(new_config).await;
```

**Implementation Notes**:
- Delegates to `HedgeExecutor.update_config()` (line 412 in manager.rs)
- Takes effect immediately for new requests
- Does not affect in-flight requests

---

#### `get_hedge_config(&self) -> HedgeConfig`

**Location**: Lines 416-419 (manager.rs)

Returns the current hedging configuration.

**Returns**: Current `HedgeConfig`

**Usage**:
```rust
let config = manager.get_hedge_config().await;
println!("Hedging enabled: {}, quantile: {}", config.enabled, config.latency_quantile);
```

---

#### `get_upstream_latency_stats(&self, upstream_name: &str) -> Option<(u64, u64, u64, u64)>`

**Location**: Lines 421-433 (manager.rs)

Returns latency statistics for a specific upstream endpoint.

**Parameters**:
- `upstream_name`: Name of the upstream to query

**Returns**:
- `Some((p50, p95, p99, avg))`: Tuple of latencies in milliseconds
- `None`: No latency data available for this upstream

**Usage**:
```rust
if let Some((p50, p95, p99, avg)) = manager.get_upstream_latency_stats("alchemy").await {
    println!("Latency - P50: {}ms, P95: {}ms, P99: {}ms, Avg: {}ms", p50, p95, p99, avg);
}
```

**Implementation Notes**:
- Queries the internal `LatencyTracker` for the upstream
- Returns `None` if upstream hasn't received enough requests
- Useful for monitoring and debugging hedging behavior

---

#### `get_hedge_executor(&self) -> Arc<HedgeExecutor>`

**Location**: Lines 435-439 (manager.rs)

Returns a reference to the hedge executor for advanced use cases.

**Returns**: Arc-wrapped `HedgeExecutor`

**Usage**:
```rust
let executor = manager.get_hedge_executor();
// Use for custom hedging scenarios or testing
```

**Implementation Notes**:
- Returns cloned Arc (cheap operation)
- Allows direct access to latency tracking and hedging logic
- Useful for testing or custom integration scenarios

## Request Flow

### Recommended Flow: `send_request_auto()`

1. **Request Initiation** (`send_request_auto` - line 216 in manager.rs)
   - User calls `send_request_auto` with JsonRpcRequest
   - Delegates to `router.route()` (line 220 in manager.rs)

2. **Router Strategy Selection** (handled by SmartRouter)
   - Router checks if method requires consensus → uses consensus routing
   - If not, checks if scoring enabled → uses scoring-based selection
   - If not, checks if hedging enabled → uses hedged requests
   - Otherwise → uses simple response-time selection
   - See [Upstream Router documentation](./upstream_router.md) for detailed routing logic

3. **Unhealthy Upstream Handling** (lines 222-230 in manager.rs)
   - If router returns `NoHealthyUpstreams` and `unhealthy_behavior.enabled` is true
   - Retries with exponential backoff and jitter
   - Prevents thundering herd on upstream recovery

4. **Router Execution**
   - Router executes the selected strategy (consensus/scoring/hedging/simple)
   - Returns response or error
   - See [Upstream Router documentation](./upstream_router.md) for detailed flow

### Router-Based Request Flow

All routing strategies are now implemented in the router (SmartRouter by default). The router handles:
- Consensus routing for consensus-required methods
- Scoring-based selection when scoring is enabled
- Hedged requests when hedging is enabled (and scoring disabled)
- Simple response-time selection as fallback

See [Upstream Router documentation](./upstream_router.md) for detailed flow diagrams and strategy descriptions.

### Legacy Round-Robin Flow

Used by `send_request()` - maintained for backward compatibility.

1. **Request Initiation** (`send_request` - line 144 in manager.rs)
   - Configuration is read once for retry settings (line 148 in manager.rs)
   - Iterates up to `max_retries + 1` times (line 151 in manager.rs)

2. **Upstream Selection** (`try_send_request` - lines 190-205 in manager.rs)
   - Acquires next healthy upstream via round-robin (lines 194-195 in manager.rs)
   - Returns `NoHealthyUpstreams` error if none available

3. **Request Execution** (line 199 in manager.rs)
   - Wraps request in 15-second timeout
   - Delegates to `UpstreamEndpoint.send_request`

4. **Retry Behavior** (lines 151-180 in manager.rs)
   - Short-circuits on circuit breaker or no healthy upstream errors (lines 162-168 in manager.rs)
   - Sleeps for `retry_delay_ms` before next attempt (lines 173-176 in manager.rs)
   - Returns last error if all retries exhausted (line 182 in manager.rs)

### Timeout Strategy

- **Lock Acquisition**: Configurable timeouts to prevent deadlocks (LoadBalancer)
- **Health Checks**: 100ms normal, 50ms high load, 25ms fallback (LoadBalancer)
- **Request Timeout**: 15 seconds hardcoded (lines 197, 271 in manager.rs)
- **Retry Delay**: Configurable via `retry_delay_ms` (default 1000ms)
- **Hedge Delay**: Calculated dynamically based on latency percentiles (P95/P99)

## Configuration

### UpstreamManagerConfig

Default configuration (lines 36-44 in manager.rs):

```rust
UpstreamManagerConfig {
    max_retries: 1,                          // 2 total attempts
    retry_delay_ms: 1000,                    // 1 second between retries
    circuit_breaker_threshold: 2,            // Open after 2 failures
    circuit_breaker_timeout_seconds: 60,     // Try recovery after 60 seconds
}
```

### Configuration Guidelines

**max_retries**:
- Default: 1 (total 2 attempts)
- Increase for flaky networks
- Each retry adds latency, balance reliability vs. response time

**retry_delay_ms**:
- Default: 1000ms
- Prevents thundering herd on upstream recovery
- Consider upstream rate limits when setting

**circuit_breaker_threshold**:
- Default: 2 failures
- Lower values (1-3) for fail-fast behavior
- Higher values (5-10) for tolerating transient failures

**circuit_breaker_timeout_seconds**:
- Default: 60 seconds
- Time before attempting recovery (half-open state)
- Match to expected upstream recovery time

### Concurrency Limit

Set via `with_concurrency_limit()`:
- Default: 1000 concurrent requests
- Prevents connection pool exhaustion
- Should match or exceed expected peak load
- Exceeding limit returns `UpstreamError::ConcurrencyLimit`

## Health Management

### Health Check Integration

The UpstreamManager doesn't directly perform health checks but coordinates with:

1. **HealthChecker** (`crates/prism-core/src/upstream/health.rs`)
   - Periodic health checks via `eth_blockNumber`
   - Updates UpstreamEndpoint health status
   - Integrates with MetricsCollector

2. **Circuit Breaker** (`crates/prism-core/src/upstream/circuit_breaker.rs`)
   - Tracks consecutive failures
   - Opens after threshold reached
   - Half-open state for recovery testing
   - Automatic closure on success

### Health States

Each UpstreamEndpoint can be in one of these states:

1. **Healthy + Closed**: Normal operation, accepting requests
2. **Healthy + Open**: Recently failed, circuit breaker preventing requests
3. **Healthy + HalfOpen**: Testing recovery, single request allowed
4. **Unhealthy + Closed**: Health check failed but circuit breaker not triggered
5. **Unhealthy + Open**: Both health check and circuit breaker indicating failure

### Failover Behavior

When an upstream fails:

1. **Immediate**: Circuit breaker increments failure count
2. **After Threshold**: Circuit breaker opens, upstream marked unhealthy
3. **Load Balancer**: Excludes unhealthy upstreams from selection
4. **Retry Logic**: Automatically tries next available healthy upstream
5. **Recovery**: After timeout, circuit breaker enters half-open state
6. **Validation**: Single successful request closes circuit breaker

## Thread Safety

### Concurrency Model

The UpstreamManager is designed for high-concurrency environments:

1. **Shared Ownership** (Arc)
   - LoadBalancer: `Arc<LoadBalancer>` - shared across all operations
   - HttpClient: `Arc<HttpClient>` - single connection pool for all upstreams
   - Config: `Arc<RwLock<UpstreamManagerConfig>>` - thread-safe updates

2. **Read-Write Locks**
   - Configuration: `RwLock` allows concurrent reads, exclusive writes
   - LoadBalancer maintains internal `RwLock<Vec<Arc<UpstreamEndpoint>>>`
   - Minimizes lock contention via read-heavy access pattern

3. **Atomic Operations**
   - LoadBalancer uses `AtomicUsize` for round-robin index
   - Lock-free counter increment for upstream selection

### Safe Concurrent Usage

```rust
// Safe to share across threads
let chain_state = Arc::new(ChainState::new());
let manager = Arc::new(
    UpstreamManagerBuilder::new()
        .chain_state(chain_state)
        .build()
        .expect("Failed to create upstream manager")
);

// Multiple concurrent requests
let handles: Vec<_> = (0..100)
    .map(|_| {
        let manager = manager.clone();
        tokio::spawn(async move {
            manager.send_request(&request).await
        })
    })
    .collect();

// All requests execute concurrently without blocking
```

### Lock Acquisition Strategy

The implementation uses timeout-protected lock acquisition:

1. **Read Locks**: 500ms timeout (default), prevents deadlocks under high load
2. **Aggressive Timeouts**: 200ms every 50th request to prevent resource leaks
3. **Write Locks**: Standard acquisition for config updates (infrequent)

### Contention Mitigation

- **Clone upstreams list**: LoadBalancer clones `Vec<Arc<UpstreamEndpoint>>` to release read lock quickly
- **Async health checks**: Released from lock before async operations
- **Arc wrapping**: Cheap cloning via reference counting
- **Minimal critical sections**: Lock held only during data structure access

## Error Handling

### Error Types

All errors are represented by the `UpstreamError` enum (`crates/prism-core/src/upstream/errors.rs`):

1. **Timeout**: Request exceeded timeout duration
2. **ConnectionFailed(String)**: Network connection failed
3. **HttpError(u16, String)**: HTTP status error with code and message
4. **RpcError(i32, String)**: JSON-RPC error with code and message
5. **Network(reqwest::Error)**: Underlying network error
6. **InvalidResponse(String)**: Response parsing failed
7. **NoHealthyUpstreams**: No healthy upstreams available
8. **CircuitBreakerOpen**: Circuit breaker preventing request
9. **InvalidRequest(String)**: Request validation failed
10. **ConcurrencyLimit(String)**: Concurrency limit exceeded

### Error Propagation

Errors flow through these layers:

1. **HttpClient** → Network/HTTP errors
2. **UpstreamEndpoint** → Adds circuit breaker and timeout logic
3. **LoadBalancer** → Handles upstream selection failures
4. **UpstreamManager** → Adds retry logic and error aggregation

### Retry Decision Logic

```rust
// From send_request (lines 105-111)
if matches!(e,
    UpstreamError::CircuitBreakerOpen |
    UpstreamError::NoHealthyUpstreams
) {
    last_error = Some(e);
    break;  // Don't retry these errors
}
```

**Non-retryable Errors**:
- `CircuitBreakerOpen`: System intentionally blocking requests
- `NoHealthyUpstreams`: No upstreams available to try

**Retryable Errors**:
- `Timeout`: Might succeed with different upstream
- `ConnectionFailed`: Transient network issues
- `HttpError`: Upstream might be temporarily down
- `RpcError`: Upstream might be overloaded
- `Network`: Network conditions might improve
- `InvalidResponse`: Might be specific to one upstream

### Error Context

Errors include contextual information:

```rust
// RPC error includes code and message
UpstreamError::RpcError(-32000, "execution reverted")

// HTTP error includes status code
UpstreamError::HttpError(503, "Service Unavailable")

// Invalid response includes parse error
UpstreamError::InvalidResponse("Failed to deserialize: ...")
```

### Best Practices

1. **Use `send_request_auto()` as default** for all request routing:
```rust
// RECOMMENDED: Adapts to configuration automatically
match manager.send_request_auto(&request).await {
    Ok(response) => handle_success(response),
    Err(UpstreamError::NoHealthyUpstreams) => alert_no_upstreams(),
    Err(UpstreamError::CircuitBreakerOpen) => log_circuit_breaker(),
    Err(UpstreamError::Timeout) => retry_with_backoff(),
    Err(e) => log_error(e),
}
```

2. **Enable hedging for latency-sensitive workloads** to improve P95/P99 response times

3. **Monitor error patterns** via circuit breaker status and latency statistics

4. **Configure retries** based on error frequency and upstream reliability

5. **Use explicit methods** only when you need to bypass automatic strategy selection:
   - `send_request()` - Legacy round-robin (avoid in new code, use `send_request_auto()` instead)

## Usage Examples

### Basic Setup

```rust
use prism_core::{
    upstream::{UpstreamManagerBuilder, UpstreamConfig},
    types::JsonRpcRequest,
    chain::ChainState,
};
use std::sync::Arc;

// Create shared chain state
let chain_state = Arc::new(ChainState::new());

// Create manager using builder pattern
let manager = Arc::new(
    UpstreamManagerBuilder::new()
        .chain_state(chain_state)
        .concurrency_limit(1000)
        .build()
        .expect("Failed to initialize upstream manager")
);

// Add upstreams
let alchemy_config = UpstreamConfig {
    url: "https://eth-mainnet.alchemyapi.io/v2/YOUR_KEY".to_string(),
    name: "alchemy".to_string(),
    weight: 1,
    timeout_seconds: 30,
    supports_websocket: true,
    ws_url: Some("wss://eth-mainnet.alchemyapi.io/v2/YOUR_KEY".to_string()),
    circuit_breaker_threshold: 5,
    circuit_breaker_timeout_seconds: 60,
    chain_id: 1,
};

manager.add_upstream(alchemy_config).await;

// Send request using recommended method
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_blockNumber".to_string(),
    params: None,
    id: serde_json::Value::Number(1.into()),
};

// Use send_request_auto() - automatically selects best strategy
match manager.send_request_auto(&request).await {
    Ok(response) => println!("Response: {:?}", response),
    Err(e) => eprintln!("Error: {}", e),
}
```

### Multiple Upstreams with Failover

```rust
// Add multiple providers
let configs = vec![
    UpstreamConfig {
        url: "https://mainnet.infura.io/v3/YOUR_KEY".to_string(),
        name: "infura".to_string(),
        weight: 2,  // Higher weight for preferred provider
        ..Default::default()
    },
    UpstreamConfig {
        url: "https://eth-mainnet.alchemyapi.io/v2/YOUR_KEY".to_string(),
        name: "alchemy".to_string(),
        weight: 1,
        ..Default::default()
    },
    UpstreamConfig {
        url: "https://cloudflare-eth.com".to_string(),
        name: "cloudflare".to_string(),
        weight: 1,
        ..Default::default()
    },
];

for config in configs {
    manager.add_upstream(config).await;
}

// Automatic failover on errors using recommended method
let result = manager.send_request_auto(&request).await;
// If one upstream fails, automatically tries next available
// Uses response-time-aware selection or hedging based on config
```

### Custom Retry Configuration

```rust
use prism_core::{
    upstream::{UpstreamManagerBuilder, UpstreamManagerConfig},
    chain::ChainState,
};
use std::sync::Arc;

let chain_state = Arc::new(ChainState::new());

// Create manager with custom retry settings using builder
let config = UpstreamManagerConfig {
    max_retries: 3,              // Total of 4 attempts
    retry_delay_ms: 500,         // 500ms between attempts
    circuit_breaker_threshold: 5,
    circuit_breaker_timeout_seconds: 120,
};

let manager = UpstreamManagerBuilder::new()
    .chain_state(chain_state)
    .config(config)
    .build()
    .expect("Failed to initialize upstream manager");
```

### Monitoring Health

```rust
// Check overall health
if !manager.has_healthy_upstreams().await {
    eprintln!("ALERT: No healthy upstreams!");
}

// Get detailed stats
let stats = manager.get_stats().await;
println!("Upstreams: {}/{} healthy",
    stats.healthy_upstreams,
    stats.total_upstreams
);
println!("Average response time: {}ms", stats.average_response_time_ms);

// Check circuit breaker status
let cb_status = manager.get_circuit_breaker_status().await;
for (name, state, failures) in cb_status {
    println!("{}: {:?} ({} failures)", name, state, failures);
}
```

### Using Hedging for Improved Latency

```rust
use prism_core::{
    upstream::{UpstreamManagerBuilder, HedgeConfig},
    chain::ChainState,
};
use std::sync::Arc;

let chain_state = Arc::new(ChainState::new());

// Configure hedging during manager creation
let hedge_config = HedgeConfig {
    enabled: true,
    latency_quantile: 0.95,  // Use P95 latency for hedge timing
    min_delay_ms: 50,
    max_delay_ms: 1000,
    max_parallel: 2,  // Send up to 2 parallel requests
};

let manager = UpstreamManagerBuilder::new()
    .chain_state(chain_state)
    .hedge_config(hedge_config)
    .build()
    .expect("Failed to initialize upstream manager");

// Or update configuration at runtime
manager.update_hedge_config(hedge_config).await;

// Now send_request_auto() will automatically use hedging
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(serde_json::json!(["latest", false])),
    id: serde_json::Value::Number(1.into()),
};

// Automatically uses router to select best strategy (consensus/scoring/hedging/simple)
let response = manager.send_request_auto(&request).await?;
```

### Runtime Configuration Updates

```rust
use prism_core::{
    upstream::{UpstreamManagerBuilder, UpstreamManagerConfig},
    chain::ChainState,
};
use std::sync::Arc;

let chain_state = Arc::new(ChainState::new());

// Start with conservative settings
let manager = Arc::new(
    UpstreamManagerBuilder::new()
        .chain_state(chain_state)
        .build()
        .expect("Failed to initialize upstream manager")
);

// ... application runs ...

// Upstreams become more reliable, reduce retries
let new_config = UpstreamManagerConfig {
    max_retries: 1,
    retry_delay_ms: 500,
    circuit_breaker_threshold: 10,  // More tolerant
    circuit_breaker_timeout_seconds: 30,
};

manager.update_config(new_config).await;
```

### Manual Circuit Breaker Reset

```rust
// After fixing an upstream issue manually
if manager.reset_circuit_breaker("alchemy").await {
    println!("Circuit breaker reset, upstream ready");
} else {
    eprintln!("Upstream 'alchemy' not found");
}
```

### High-Concurrency Setup

```rust
use prism_core::{
    upstream::UpstreamManagerBuilder,
    chain::ChainState,
};
use std::sync::Arc;

let chain_state = Arc::new(ChainState::new());

// Configure for high-concurrency workload
let manager = UpstreamManagerBuilder::new()
    .chain_state(chain_state)
    .concurrency_limit(5000)
    .build()
    .expect("Failed to initialize upstream manager");

// Add multiple upstreams for load distribution
// ...

// Spawn many concurrent requests
let handles: Vec<_> = (0..1000)
    .map(|i| {
        let manager = manager.clone();
        let request = create_request(i);

        tokio::spawn(async move {
            // Use send_request_auto() for optimal performance
            manager.send_request_auto(&request).await
        })
    })
    .collect();

// Wait for all requests
for handle in handles {
    let _ = handle.await;
}
```

### Integration with Health Checker

```rust
use prism_core::{
    upstream::{UpstreamManagerBuilder, health::HealthChecker},
    metrics::MetricsCollector,
    chain::ChainState,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

let chain_state = Arc::new(ChainState::new());
let manager = Arc::new(
    UpstreamManagerBuilder::new()
        .chain_state(chain_state)
        .build()
        .expect("Failed to initialize upstream manager")
);
let metrics = Arc::new(MetricsCollector::new().unwrap());

// Create health checker that runs every 30 seconds
let health_checker = HealthChecker::new(
    manager.clone(),
    metrics.clone(),
    Duration::from_secs(30),
);

// Start background health checking
let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
let health_handle = health_checker.start_with_shutdown(shutdown_rx);

// ... application runs ...

// Shutdown health checker
let _ = shutdown_tx.send(());
let _ = health_handle.await;
```

## Related Components

### LoadBalancer
**File**: `crates/prism-core/src/upstream/load_balancer.rs`

- Manages pool of UpstreamEndpoint instances
- Implements selection strategies (round-robin, response-time, weighted)
- Provides timeout-protected lock acquisition
- Maintains circuit breaker status for all upstreams

### UpstreamEndpoint
**File**: `crates/prism-core/src/upstream/endpoint.rs`

- Individual upstream RPC endpoint
- Handles request execution with circuit breaker protection
- Tracks health metrics and response times
- Manages WebSocket connections for chain tip subscriptions

### CircuitBreaker
**File**: `crates/prism-core/src/upstream/circuit_breaker.rs`

- Implements circuit breaker pattern
- States: Closed (normal), Open (failing), HalfOpen (testing)
- Configurable failure threshold and timeout
- Automatic recovery testing

### HealthChecker
**File**: `crates/prism-core/src/upstream/health.rs`

- Periodic health monitoring
- Integrates with MetricsCollector
- Executes `eth_blockNumber` health checks
- Supports graceful shutdown

### HttpClient
**File**: `crates/prism-core/src/upstream/http_client.rs`

- Shared HTTP client with connection pooling
- Configurable concurrency limits
- Timeout protection
- Efficient resource management

### HedgeExecutor
**File**: `crates/prism-core/src/upstream/hedging/executor.rs`

- Executes hedged requests for improved tail latency
- Tracks per-upstream latency statistics (P50, P95, P99)
- Calculates optimal hedge timing based on latency quantiles
- Manages parallel request execution with automatic cancellation
- Runtime-configurable hedging parameters
- See [Hedging documentation](../hedging/README.md) for detailed documentation

## Performance Considerations

### Concurrency Limits

- **Default**: 1000 concurrent requests
- **Tuning**: Match to expected peak load
- **Impact**: Exceeding limit returns immediate error without queuing

### Lock Contention

- **Read-heavy pattern**: Config and upstream list mostly read
- **Clone-on-read**: LoadBalancer clones list to minimize lock hold time
- **Timeout protection**: Prevents deadlocks under extreme load
- **Atomic counters**: Lock-free round-robin selection

### Memory Usage

- **Arc sharing**: Minimal memory overhead via reference counting
- **Connection pooling**: Single HttpClient prevents connection multiplication
- **Upstream count**: Each upstream adds ~1KB overhead
- **Request buffering**: No request queuing, bounded memory usage

### Latency Impact

- **Health check latency**: 100ms normal, 50ms high load
- **Lock acquisition**: 500ms max wait, usually <1ms
- **Retry delay**: Configurable, default 1000ms between attempts
- **Timeout overhead**: 15 seconds max per request attempt

### Throughput Optimization

1. **Increase concurrency limit** for higher throughput
2. **Add more upstreams** to distribute load
3. **Tune retry settings** to balance reliability vs. latency
4. **Use response-time selection** for latency-sensitive workloads
5. **Monitor circuit breaker** to identify problematic upstreams

## Testing

### Unit Tests

Located in manager.rs (lines 346-395):

1. **test_upstream_manager_creation** (lines 352-357)
   - Validates initial state (0 upstreams)
   - Tests default constructor

2. **test_upstream_manager_with_custom_config** (lines 360-374)
   - Tests creation with custom configuration
   - Validates configuration retrieval

3. **test_upstream_manager_config_update** (lines 377-390)
   - Tests runtime configuration updates
   - Validates config persistence

### Integration Testing

```rust
#[tokio::test]
async fn test_failover_behavior() {
    let chain_state = Arc::new(ChainState::new());
    let manager = UpstreamManagerBuilder::new()
        .chain_state(chain_state)
        .build()
        .expect("Failed to create upstream manager");

    // Add failing upstream
    let failing_config = UpstreamConfig {
        url: "http://localhost:9999".to_string(),
        name: "failing".to_string(),
        ..Default::default()
    };

    // Add working upstream
    let working_config = UpstreamConfig {
        url: "http://localhost:8545".to_string(),
        name: "working".to_string(),
        ..Default::default()
    };

    manager.add_upstream(failing_config).await;
    manager.add_upstream(working_config).await;

    // Should automatically failover to working upstream
    let request = JsonRpcRequest { /* ... */ };
    let result = manager.send_request_auto(&request).await;

    assert!(result.is_ok());
}
```

### Load Testing

For high-concurrency testing, see:
- `crates/tests/src/e2e/basic_rpc_tests.rs`
- Performance benchmarks in `crates/prism-core/benches/`

## Changelog

### Key Implementation Details

- **manager.rs Lines 15-19**: Core component structure with Arc-wrapped dependencies including `HedgeExecutor`
- **manager.rs Lines 25-34**: Configuration structure with sensible defaults
- **manager.rs Lines 56-89**: Three constructor variants for different use cases
- **manager.rs Lines 144-183**: Legacy round-robin request routing with retry logic
- **manager.rs Lines 190-205**: Internal request execution with timeout protection
- **manager.rs Lines 292-322**: Response-time-aware request routing (used internally)
- **manager.rs Lines 335-346**: **Primary API** - `send_request_auto()` with automatic strategy selection
- **manager.rs Lines 407-419**: Hedged request support with `HedgeExecutor` integration
- **manager.rs Lines 348-387**: Circuit breaker management methods

### API Evolution

**Current Recommended API** (as of latest version):
1. **`send_request_auto()`** (lines 216-234) - Primary method for all request routing
   - Delegates to router for automatic strategy selection
   - Router handles: consensus → scoring → hedging → simple (priority order)
   - Handles unhealthy upstream retry with exponential backoff
   - Used by all handlers in the codebase

2. **`send_request()`** (lines 144-183) - Legacy round-robin method
   - Maintained for backward compatibility
   - Consider migrating to `send_request_auto()` for better performance

### Recent Changes

- Refactored routing logic into router trait (`RequestRouter`)
- Introduced `SmartRouter` for automatic strategy selection
- `send_request_auto()` now delegates to router instead of direct hedging/response-time logic
- Routing strategies: consensus (priority 1), scoring (priority 2), hedging (priority 3), simple (priority 4)
- Added unhealthy upstream retry with exponential backoff
- Maintained backward compatibility with legacy `send_request()` method
