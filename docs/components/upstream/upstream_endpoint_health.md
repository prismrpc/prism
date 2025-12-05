# Upstream Endpoint and Health Management

## Overview

The upstream endpoint and health management system provides robust connectivity to Ethereum RPC providers with automatic health monitoring, circuit breaker protection, and WebSocket support for real-time chain updates.

**Key Components:**
- **`UpstreamEndpoint`** - Individual RPC provider endpoint with integrated health tracking
- **`HealthChecker`** - Periodic health monitoring service for all upstreams
- **Circuit Breaker** - Fail-fast protection to prevent cascading failures
- **WebSocket Handler** - Real-time block updates for cache invalidation

**Location:**
- `crates/prism-core/src/upstream/endpoint.rs`
- `crates/prism-core/src/upstream/health.rs`

## Endpoint Configuration

### UpstreamConfig

Defines the configuration for an RPC provider endpoint.

**Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `url` | `String` | HTTP/HTTPS URL for JSON-RPC requests |
| `ws_url` | `Option<String>` | WebSocket URL for subscriptions (optional) |
| `name` | `String` | Unique identifier for the upstream provider |
| `chain_id` | `u64` | Ethereum chain ID (e.g., 1 for mainnet, 11155111 for sepolia) |
| `weight` | `u32` | Load balancing weight (higher = more requests) |
| `timeout_seconds` | `u64` | Default timeout for requests in seconds |
| `supports_websocket` | `bool` | Whether this endpoint supports WebSocket subscriptions |
| `circuit_breaker_threshold` | `u32` | Number of failures before circuit breaker opens |
| `circuit_breaker_timeout_seconds` | `u64` | Time to wait before retrying after circuit opens |

**Default Values:**
```rust
UpstreamConfig {
    url: String::new(),
    ws_url: None,
    name: String::new(),
    chain_id: 0,
    weight: 1,
    timeout_seconds: 30,
    supports_websocket: false,
    circuit_breaker_threshold: 5,
    circuit_breaker_timeout_seconds: 60,
}
```

## UpstreamEndpoint

The main structure representing a single RPC provider endpoint with integrated health tracking.

### Structure

```rust
pub struct UpstreamEndpoint {
    config: UpstreamConfig,
    health: Arc<RwLock<UpstreamHealth>>,
    circuit_breaker: Arc<CircuitBreaker>,
    http_client: Arc<HttpClient>,
    websocket_handler: WebSocketHandler,
}
```

### Constructor

#### `new(config: UpstreamConfig, http_client: Arc<HttpClient>) -> Self`

Creates a new upstream endpoint instance.

**Parameters:**
- `config` - Endpoint configuration
- `http_client` - Shared HTTP client for making requests

**Returns:** Initialized `UpstreamEndpoint` with default health status and closed circuit breaker

**Location:** Line 27-42 in `endpoint.rs`

## Request Handling

### Single Request

#### `async fn send_request(&self, request: &JsonRpcRequest) -> Result<JsonRpcResponse, UpstreamError>`

Sends a single JSON-RPC request to the upstream provider with circuit breaker protection.

**Process:**
1. Checks circuit breaker state
2. Applies method-specific timeout
3. Serializes request to JSON
4. Sends HTTP POST request
5. Deserializes response
6. Updates health metrics and circuit breaker state

**Parameters:**
- `request` - JSON-RPC request to send

**Returns:**
- `Ok(JsonRpcResponse)` - Successful response
- `Err(UpstreamError)` - Request failed or circuit breaker open

**Circuit Breaker Behavior:**
- Returns `UpstreamError::CircuitBreakerOpen` immediately if circuit is open
- Records failure on error (HTTP error, invalid response, or RPC error)
- Records success on valid response

**Location:** Line 47-110 in `endpoint.rs`

### Batch Request

#### `async fn send_batch_request(&self, batch_data: &[u8], timeout: Duration) -> Result<Vec<u8>, UpstreamError>`

Sends a batch of JSON-RPC requests as raw bytes.

**Parameters:**
- `batch_data` - Raw JSON batch request bytes
- `timeout` - Custom timeout for the batch

**Returns:**
- `Ok(Vec<u8>)` - Raw response bytes
- `Err(UpstreamError)` - Request failed

**Use Case:** Efficiently processes multiple RPC calls in a single HTTP request

**Location:** Line 116-156 in `endpoint.rs`

## Method-Specific Timeouts

Different RPC methods have different timeout requirements based on expected execution time:

| Method Pattern | Timeout | Rationale |
|----------------|---------|-----------|
| `eth_blockNumber`, `eth_chainId`, `eth_gasPrice` | 5 seconds | Fast, lightweight queries |
| `eth_getBlockByNumber`, `eth_getBlockByHash`, `eth_getTransactionByHash`, `eth_getTransactionReceipt` | 10 seconds | Standard lookup operations |
| `eth_getLogs` | 30 seconds | Complex queries that may scan many blocks |
| All other methods | Config default | Uses `timeout_seconds` from config |

**Implementation:** Line 266-276 in `endpoint.rs`

## Health Status

### UpstreamHealth

Tracks the current health state of an upstream endpoint.

**Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `is_healthy` | `bool` | Overall health status (true = healthy) |
| `last_check` | `Instant` | Timestamp of the most recent health check |
| `error_count` | `u32` | Consecutive errors since last success |
| `response_time_ms` | `Option<u64>` | Most recent response time in milliseconds |

**Health State Transitions:**

```
Initial State: is_healthy=true, error_count=0

Success:
  error_count → 0
  is_healthy → true

1st Error:
  error_count → 1
  is_healthy → true (still healthy)

2nd Error:
  error_count → 2
  is_healthy → false (during health_check)

3rd Error:
  error_count → 3
  is_healthy → false (during update_health)
```

### Health Check Methods

#### `async fn is_healthy(&self) -> bool`

Comprehensive health check considering both health status and circuit breaker state.

**Returns:** `true` only if:
- `UpstreamHealth.is_healthy` is `true`
- Circuit breaker `can_execute()` is `true`

**Location:** Line 159-164 in `endpoint.rs`

#### `async fn get_health(&self) -> UpstreamHealth`

Returns a clone of the current health information.

**Returns:** Complete health snapshot with all metrics

**Location:** Line 167-169 in `endpoint.rs`

#### `async fn health_check(&self) -> bool`

Performs an active health check by sending an `eth_blockNumber` request.

**Process:**
1. Checks circuit breaker state
2. Sends `eth_blockNumber` RPC request
3. Updates health based on result

**Success Behavior:**
- Sets `is_healthy = true`
- Resets `error_count = 0`

**Failure Behavior:**
- Increments `error_count`
- Sets `is_healthy = false` if `error_count >= 2` OR circuit breaker is open
- Logs warning with error details

**Location:** Line 177-221 in `endpoint.rs`

#### `async fn update_health(&self, success: bool, response_time_ms: u64)`

Internal method to update health metrics after each request.

**Parameters:**
- `success` - Whether the request succeeded
- `response_time_ms` - Response time in milliseconds

**Behavior:**
- **On Success:** Resets error count, marks as healthy
- **On Failure:** Increments error count, marks unhealthy after 3 consecutive errors

**Location:** Line 278-298 in `endpoint.rs`

## Circuit Breaker Integration

The circuit breaker prevents cascading failures by temporarily stopping requests to failing upstreams.

### Circuit Breaker States

| State | Description | Behavior |
|-------|-------------|----------|
| `Closed` | Normal operation | All requests allowed |
| `Open` | Too many failures | All requests blocked |
| `HalfOpen` | Testing recovery | Limited requests allowed |

### Circuit Breaker Methods

#### `async fn get_circuit_breaker_state(&self) -> CircuitBreakerState`

Returns the current circuit breaker state.

**Location:** Line 251-253 in `endpoint.rs`

#### `async fn get_circuit_breaker_failure_count(&self) -> u32`

Returns the number of consecutive failures recorded.

**Location:** Line 256-258 in `endpoint.rs`

#### `fn circuit_breaker(&self) -> &Arc<CircuitBreaker>`

Returns a reference to the circuit breaker for direct access (testing/monitoring).

**Location:** Line 262-264 in `endpoint.rs`

## WebSocket Subscription

Endpoints can subscribe to real-time block updates via WebSocket for efficient cache invalidation.

### WebSocket Methods

#### `async fn subscribe_to_new_heads(&self, cache_manager: Arc<CacheManager>, reorg_manager: Arc<ReorgManager>) -> Result<(), UpstreamError>`

Establishes WebSocket connection and subscribes to `newHeads` events.

**Parameters:**
- `cache_manager` - Cache manager to invalidate on new blocks
- `reorg_manager` - Reorg manager to handle chain reorganizations

**Use Case:** Real-time cache invalidation when new blocks arrive

**Location:** Line 227-233 in `endpoint.rs`

#### `async fn should_attempt_websocket_subscription(&self) -> bool`

Checks if WebSocket subscription should be attempted based on failure history.

**Returns:** `false` if too many recent failures, `true` otherwise

**Location:** Line 236-238 in `endpoint.rs`

#### `async fn record_websocket_failure(&self)`

Records a WebSocket connection failure.

**Location:** Line 241-243 in `endpoint.rs`

#### `async fn record_websocket_success(&self)`

Records a successful WebSocket connection (resets failure counter).

**Location:** Line 246-248 in `endpoint.rs`

## HealthChecker Service

Periodic background service that monitors all upstream endpoints.

### Structure

```rust
pub struct HealthChecker {
    upstream_manager: Arc<UpstreamManager>,
    metrics_collector: Arc<MetricsCollector>,
    check_interval: Duration,
}
```

### Constructor

#### `fn new(upstream_manager: Arc<UpstreamManager>, metrics_collector: Arc<MetricsCollector>, check_interval: Duration) -> Self`

Creates a new health checker service.

**Parameters:**
- `upstream_manager` - Manager containing all upstream endpoints
- `metrics_collector` - Metrics collector for recording health status
- `check_interval` - How often to perform health checks (e.g., `Duration::from_secs(60)`)

**Location:** Line 17-23 in `health.rs`

### Starting the Service

#### `fn start_with_shutdown(&self, shutdown_rx: broadcast::Receiver<()>) -> tokio::task::JoinHandle<()>`

Starts the health checking loop in a background task.

**Parameters:**
- `shutdown_rx` - Broadcast receiver for graceful shutdown signal

**Returns:** Join handle for the background task

**Behavior:**
- Ticks at `check_interval` intervals
- Checks all upstreams on each tick
- Records metrics for each check
- Logs health status (info for pass, warning for fail)
- Gracefully shuts down on signal

**Location:** Line 26-51 in `health.rs`

### Health Check Process

#### `async fn check_all_upstreams(upstream_manager: &UpstreamManager, metrics_collector: &MetricsCollector) -> Result<(), Box<dyn std::error::Error + Send + Sync>>`

Internal method that performs health checks on all registered upstreams.

**Process for Each Upstream:**
1. Call `upstream.health_check()`
2. Record result in metrics collector
3. Log status with response time
4. **On Success:** Reset WebSocket failure tracker (enables reconnection after recovery)
5. **On Failure:** Log warning with details

**Special Behavior:**
When a health check passes, it calls `record_websocket_success()` to reset the WebSocket failure counter. This allows WebSocket connections to be re-attempted after container recovery or network issues.

**Location:** Line 54-91 in `health.rs`

### Manual Health Check

#### `async fn check_upstream(&self, name: &str) -> Option<bool>`

Forces an immediate health check on a specific upstream by name.

**Parameters:**
- `name` - The unique name of the upstream to check

**Returns:**
- `Some(true)` - Upstream found and healthy
- `Some(false)` - Upstream found but unhealthy
- `None` - Upstream not found

**Use Case:** CLI tools or administrative interfaces to manually verify upstream status

**Location:** Line 94-118 in `health.rs`

## Configuration Options

### Recommended Health Check Intervals

| Environment | Interval | Rationale |
|-------------|----------|-----------|
| Development | 10-30 seconds | Fast detection during testing |
| Production | 30-60 seconds | Balance between responsiveness and overhead |
| High-Load | 60-120 seconds | Reduce health check traffic |

### Circuit Breaker Tuning

**Conservative (Tolerant):**
```rust
circuit_breaker_threshold: 10,
circuit_breaker_timeout_seconds: 120,
```
- More tolerant of transient failures
- Longer recovery time
- Good for unstable networks

**Aggressive (Fast-Fail):**
```rust
circuit_breaker_threshold: 2,
circuit_breaker_timeout_seconds: 30,
```
- Quick detection of failures
- Fast recovery attempts
- Good for stable providers with fallbacks

**Balanced (Recommended):**
```rust
circuit_breaker_threshold: 5,
circuit_breaker_timeout_seconds: 60,
```
- Reasonable failure tolerance
- Moderate recovery time
- Good for most production scenarios

## Usage Examples

### Creating an Endpoint

```rust
use std::sync::Arc;
use prism_core::{
    types::UpstreamConfig,
    upstream::{endpoint::UpstreamEndpoint, http_client::HttpClient},
};

// Configure the endpoint
let config = UpstreamConfig {
    url: "https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY".to_string(),
    ws_url: Some("wss://eth-mainnet.g.alchemy.com/v2/YOUR_KEY".to_string()),
    name: "alchemy-mainnet".to_string(),
    chain_id: 1,
    weight: 100,
    timeout_seconds: 30,
    supports_websocket: true,
    circuit_breaker_threshold: 5,
    circuit_breaker_timeout_seconds: 60,
};

// Create HTTP client
let http_client = Arc::new(HttpClient::new().expect("Failed to create HTTP client"));

// Create endpoint
let endpoint = UpstreamEndpoint::new(config, http_client);
```

### Sending Requests

```rust
use prism_core::types::JsonRpcRequest;

// Create a request
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_blockNumber".to_string(),
    params: None,
    id: serde_json::Value::Number(1.into()),
};

// Send request
match endpoint.send_request(&request).await {
    Ok(response) => {
        println!("Block number: {:?}", response.result);
    }
    Err(e) => {
        eprintln!("Request failed: {}", e);
    }
}
```

### Checking Health

```rust
// Quick health check (combines health status + circuit breaker)
if endpoint.is_healthy().await {
    println!("Endpoint is healthy");
} else {
    println!("Endpoint is unhealthy");
}

// Detailed health information
let health = endpoint.get_health().await;
println!("Health Status:");
println!("  Healthy: {}", health.is_healthy);
println!("  Error Count: {}", health.error_count);
println!("  Response Time: {:?}ms", health.response_time_ms);

// Active health check (sends actual request)
if endpoint.health_check().await {
    println!("Active health check passed");
}
```

### Circuit Breaker Monitoring

```rust
use prism_core::upstream::circuit_breaker::CircuitBreakerState;

// Check circuit breaker state
let state = endpoint.get_circuit_breaker_state().await;
match state {
    CircuitBreakerState::Closed => println!("Circuit breaker: CLOSED (normal)"),
    CircuitBreakerState::Open => println!("Circuit breaker: OPEN (blocking requests)"),
    CircuitBreakerState::HalfOpen => println!("Circuit breaker: HALF-OPEN (testing)"),
}

// Check failure count
let failures = endpoint.get_circuit_breaker_failure_count().await;
println!("Consecutive failures: {}", failures);
```

### Setting Up Health Checker

```rust
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;
use prism_core::{
    metrics::MetricsCollector,
    upstream::{health::HealthChecker, manager::UpstreamManager},
};

// Create upstream manager and add endpoints
let chain_state = Arc::new(ChainState::new());
let upstream_manager = Arc::new(
    UpstreamManagerBuilder::new()
        .chain_state(chain_state)
        .build()
        .expect("Failed to create upstream manager")
);
upstream_manager.add_upstream(config1).await;
upstream_manager.add_upstream(config2).await;

// Create metrics collector
let metrics_collector = Arc::new(
    MetricsCollector::new().expect("Failed to create metrics collector")
);

// Create health checker with 60-second interval
let health_checker = HealthChecker::new(
    upstream_manager.clone(),
    metrics_collector,
    Duration::from_secs(60),
);

// Create shutdown channel
let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

// Start health checking in background
let health_check_handle = health_checker.start_with_shutdown(shutdown_rx);

// Later, to shutdown gracefully:
// let _ = shutdown_tx.send(());
// health_check_handle.await;
```

### Manual Health Check via CLI

```rust
// Force immediate health check on specific upstream
match health_checker.check_upstream("alchemy-mainnet").await {
    Some(true) => println!("Upstream is healthy"),
    Some(false) => println!("Upstream is unhealthy"),
    None => println!("Upstream not found"),
}
```

### WebSocket Subscription

```rust
use std::sync::Arc;
use prism_core::{
    cache::{CacheManager, CacheManagerConfig, reorg_manager::ReorgManager},
    chain::ChainState,
};
use std::sync::Arc;

// Create shared chain state
let chain_state = Arc::new(ChainState::new());

// Create cache manager and reorg manager
let cache_manager = Arc::new(
    CacheManager::new(&CacheManagerConfig::default(), chain_state.clone())
        .expect("Failed to initialize cache manager")
);
let reorg_manager = Arc::new(ReorgManager::new(
    ReorgManagerConfig::default(),
    chain_state.clone(),
    cache_manager.clone()
));

// Subscribe to new block headers
if endpoint.config().supports_websocket {
    if endpoint.should_attempt_websocket_subscription().await {
        match endpoint.subscribe_to_new_heads(
            cache_manager.clone(),
            reorg_manager.clone()
        ).await {
            Ok(_) => {
                println!("WebSocket subscription established");
                endpoint.record_websocket_success().await;
            }
            Err(e) => {
                eprintln!("WebSocket subscription failed: {}", e);
                endpoint.record_websocket_failure().await;
            }
        }
    } else {
        println!("Too many recent WebSocket failures, skipping attempt");
    }
}
```

## Important Caveats

### Circuit Breaker Behavior

1. **Immediate Blocking:** When the circuit breaker opens, requests are rejected immediately without attempting to contact the upstream. This returns `UpstreamError::CircuitBreakerOpen`.

2. **Health Check Interaction:** Health checks respect the circuit breaker state. If the circuit is open, `health_check()` immediately marks the endpoint as unhealthy without sending a request.

3. **Recovery:** The circuit breaker automatically attempts recovery after `circuit_breaker_timeout_seconds`. The next request will test if the upstream has recovered (half-open state).

### Error Count Thresholds

The system uses **different thresholds** for different operations:

- **`health_check()`**: Marks unhealthy after **2 consecutive errors** (line 201-205)
- **`update_health()`**: Marks unhealthy after **3 consecutive errors** (line 289-296)

This dual-threshold approach provides tolerance for transient failures during normal operation while being stricter during dedicated health checks.

### WebSocket Failure Tracking

WebSocket subscription failures are tracked separately from HTTP request failures. The health checker resets the WebSocket failure counter on successful health checks (line 68), enabling reconnection after recovery without manual intervention.

### Thread Safety

All health state is protected by `Arc<RwLock<UpstreamHealth>>`, making it safe to access from multiple concurrent requests. The circuit breaker uses atomic operations for thread-safe state management.

### Response Time Overflow

Both `send_request()` and `send_batch_request()` handle response time overflow consistently:

```rust
let elapsed_ms = start_time.elapsed().as_millis();
let response_time = u64::try_from(elapsed_ms).unwrap_or(u64::MAX);
```

**Locations:**
- `send_request()`: Line 132 (`endpoint.rs`)
- `send_batch_request()`: Line 205 (`endpoint.rs`)

**Rationale:**
Using `u64::MAX` (instead of `0`) ensures failing/slow endpoints don't appear to have the best latency, which would incorrectly prioritize them in load balancing algorithms. This is an edge case for requests exceeding ~584 million years.

### Timeout Precedence

Method-specific timeouts override the configured `timeout_seconds` default. This ensures fast methods like `eth_blockNumber` don't wait unnecessarily long.

## Testing

The modules include comprehensive tests demonstrating key functionality:

### Endpoint Tests
- **`test_upstream_endpoint_creation`** (line 308-323): Basic endpoint initialization
- **`test_upstream_endpoint_with_circuit_breaker`** (line 326-349): Circuit breaker state transitions
- **`test_circuit_breaker_integration`** (line 352-380): Failure counting and recovery
- **`test_circuit_breaker_with_http_failures`** (line 383-417): Real HTTP failures triggering circuit breaker

### Health Checker Tests
- **`test_health_checker_creation`** (line 128-149): Health checker initialization and manual checks

## Integration with Other Components

### UpstreamManager
The `UpstreamManager` maintains a collection of `UpstreamEndpoint` instances and handles load balancing, routing, and endpoint selection based on health status.

**Request Routing:**
The manager uses `send_request_auto()` which automatically selects between hedging and response-time-aware strategies. Internally, it calls `UpstreamEndpoint.send_request()` on selected endpoints to forward requests to RPC providers.

**Health-Based Selection:**
The manager filters out unhealthy endpoints (using `UpstreamEndpoint.is_healthy()`) before selecting an upstream for request routing, ensuring requests are only sent to operational providers.

### MetricsCollector
Records health check results, response times, and circuit breaker state changes for Prometheus metrics export.

### CacheManager
Receives invalidation signals from WebSocket subscriptions when new blocks arrive, ensuring cache consistency.

### ReorgManager
Detects chain reorganizations from WebSocket block updates and coordinates cache cleanup.

## Performance Considerations

1. **Health Check Overhead:** Health checks send real `eth_blockNumber` requests. With many upstreams, frequent checks can generate significant traffic. Balance check interval with responsiveness needs.

2. **Lock Contention:** Health state uses `RwLock`, which allows multiple concurrent reads but exclusive writes. Under high load, health updates could theoretically cause brief contention, but impact is minimal since updates are infrequent.

3. **Circuit Breaker Fast Path:** When a circuit breaker is open, request rejection happens before any network I/O, making it extremely fast (<1μs).

4. **Memory Usage:** Each `UpstreamEndpoint` maintains minimal state (~200 bytes). The system scales efficiently to hundreds of upstream endpoints.

## Related Documentation

- **Circuit Breaker:** See `crates/prism-core/src/upstream/circuit_breaker.rs` for detailed circuit breaker implementation
- **Upstream Manager:** See `crates/prism-core/src/upstream/manager.rs` for load balancing and routing
- **HTTP Client:** See `crates/prism-core/src/upstream/http_client.rs` for connection pooling and request handling
- **WebSocket Handler:** See `crates/prism-core/src/upstream/websocket.rs` for subscription management
