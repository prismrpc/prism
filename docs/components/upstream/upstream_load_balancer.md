# Upstream Load Balancer

## Overview

The `LoadBalancer` manages and distributes RPC requests across multiple upstream Ethereum RPC endpoints with sophisticated selection strategies, health monitoring, and configurable timeout behavior. It serves as the core request routing component in Prism's upstream management system, providing:

- **Multiple Selection Strategies**: Round-robin, response-time-aware, and weighted selection algorithms
- **Health-Aware Routing**: Automatically skips unhealthy endpoints and respects circuit breaker states
- **High Concurrency Support**: Optimized for 85+ parallel requests with configurable lock timeouts
- **Dynamic Endpoint Management**: Add/remove upstreams at runtime without downtime
- **Statistics Tracking**: Monitors upstream health, response times, and circuit breaker status
- **Thread Safety**: All operations are concurrent-safe using atomics and `RwLock`

**Source**: `crates/prism-core/src/upstream/load_balancer.rs`

---

## Architecture

### LoadBalancer Structure

```rust
pub struct LoadBalancer {
    upstreams: ArcSwap<Vec<Arc<UpstreamEndpoint>>>,
    current_index: AtomicUsize,
    config: LoadBalancerConfig,
}
```

**Fields** (lines 60-64):

- **`upstreams`**: Lock-free list of upstream endpoints using `ArcSwap` for atomic read-copy-update operations
  - Eliminates `RwLock` contention on the hot path (every request reads upstreams)
  - Writes (add/remove) use atomic swap, not locks
  - Provides lock-free reads for high concurrency
- **`current_index`**: Lock-free atomic counter for round-robin selection (uses `Ordering::Relaxed`)
- **`config`**: Configuration controlling timeout behavior and health check strategies

### LoadBalancerConfig Structure

Configuration for load balancer timeouts and high-load behavior.

```rust
#[derive(Debug, Clone)]
pub struct LoadBalancerConfig {
    pub lock_timeout_ms: u64,
    pub aggressive_lock_timeout_ms: u64,
    pub aggressive_timeout_interval: usize,
    pub health_check_timeout_ms: u64,
    pub health_check_high_load_timeout_ms: u64,
    pub fallback_health_check_timeout_ms: u64,
    pub high_load_threshold: usize,
}
```

**Fields** (lines 16-32):

- **`lock_timeout_ms`**: Timeout for acquiring read lock on upstreams list
  - Default: 500ms (increased to handle high concurrency)

- **`aggressive_lock_timeout_ms`**: Shorter timeout used periodically to prevent resource leaks
  - Default: 200ms

- **`aggressive_timeout_interval`**: How often to use aggressive timeout (every N requests)
  - Default: 50 requests

- **`health_check_timeout_ms`**: Health check timeout during normal load
  - Default: 100ms

- **`health_check_high_load_timeout_ms`**: Health check timeout during high load
  - Default: 50ms

- **`fallback_health_check_timeout_ms`**: Timeout when checking fallback upstreams
  - Default: 25ms

- **`high_load_threshold`**: Request count threshold to consider "high load"
  - Default: 500 requests

**Default Configuration** (lines 34-49):
```rust
impl Default for LoadBalancerConfig {
    fn default() -> Self {
        Self {
            lock_timeout_ms: 500,
            aggressive_lock_timeout_ms: 200,
            aggressive_timeout_interval: 50,
            health_check_timeout_ms: 100,
            health_check_high_load_timeout_ms: 50,
            fallback_health_check_timeout_ms: 25,
            high_load_threshold: 500,
        }
    }
}
```

**Rationale** (lines 37-39):
- Lock timeouts are generous (500ms) since read locks are very fast (just clone the `Arc<Vec>`)
- This prevents spurious failures under high concurrency (85+ parallel tests)
- Health check timeouts remain fast since they're per-upstream operations

### LoadBalancerStats Structure

Statistics about load balancer state and upstream health.

```rust
#[derive(Debug, Clone)]
pub struct LoadBalancerStats {
    pub total_upstreams: usize,
    pub healthy_upstreams: usize,
    pub average_response_time_ms: u64,
}
```

**Fields** (lines 340-345):

- **`total_upstreams`**: Total number of configured upstream endpoints
- **`healthy_upstreams`**: Number of currently healthy endpoints (passing health checks and circuit breaker closed)
- **`average_response_time_ms`**: Average response time across all upstreams with recorded response times

---

## Load Balancing Strategies

The load balancer provides three different selection strategies, each optimized for specific use cases.

### 1. Round-Robin Selection (Default)

**Method**: `get_next_healthy() -> Option<Arc<UpstreamEndpoint>>`

**Location**: Lines 88-155

**Algorithm**:
1. Fetch current round-robin index atomically (incremented on each call)
2. Acquire read lock on upstreams list with configurable timeout
3. Calculate starting position: `current_index % upstream_count`
4. Check if upstream at starting position is healthy (with timeout)
5. If healthy, return immediately
6. If unhealthy, iterate through all other positions in round-robin order
7. Return first healthy upstream found, or `None` if all are unhealthy

**Health Check Timeouts** (lines 124-136):
- **Normal load** (`index <= high_load_threshold`): 100ms timeout
- **High load** (`index > high_load_threshold`): 50ms timeout
- **Fallback checks**: 25ms aggressive timeout

**Timeout Strategy** (lines 92-98):
- Every N requests (default: 50), uses aggressive lock timeout (200ms)
- Other requests use normal lock timeout (500ms)
- This prevents resource leaks while maintaining responsiveness

**Example**:
```rust
let balancer = LoadBalancer::new();

// Will use round-robin to select next healthy upstream
if let Some(upstream) = balancer.get_next_healthy().await {
    let response = upstream.send_request(&request).await?;
}
```

**Characteristics**:
- **Fair distribution**: Evenly distributes load across all healthy upstreams
- **Fast selection**: O(1) for best case (first upstream healthy), O(n) worst case
- **High throughput**: Optimized for 85+ concurrent requests
- **Automatic failover**: Skips unhealthy upstreams transparently

### 2. Response-Time-Aware Selection

**Method**: `get_next_healthy_by_response_time() -> Option<Arc<UpstreamEndpoint>>`

**Location**: Lines 159-214

**Algorithm**:
1. Acquire read lock on upstreams list with timeout (500ms)
2. Concurrently check health of ALL upstreams in parallel using `join_all()`
3. For each healthy upstream, retrieve its health metrics
4. Select upstream with lowest response time
5. If no upstream has response time recorded, return first healthy upstream

**Concurrent Health Checks** (lines 175-198):
- Uses `futures::future::join_all()` to check all upstreams in parallel
- Each health check has 100ms timeout
- Non-blocking design prevents slowdown from unhealthy upstreams

**Example**:
```rust
// Prefers upstream with fastest response time
if let Some(upstream) = balancer.get_next_healthy_by_response_time().await {
    let response = upstream.send_request(&request).await?;
}
```

**Characteristics**:
- **Performance-optimized**: Routes to fastest upstream
- **Concurrent checks**: O(1) time complexity due to parallelization
- **Adaptive**: Automatically adjusts to changing upstream performance
- **Graceful degradation**: Falls back to first healthy upstream if no metrics available

**When to Use**:
- Critical requests requiring lowest latency
- When upstream response times vary significantly
- Production environments with performance monitoring

### 3. Weighted Response-Time Selection

**Method**: `get_next_healthy_weighted() -> Option<Arc<UpstreamEndpoint>>`

**Location**: Lines 219-248

**Algorithm**:
1. Clone upstreams list and release lock (non-blocking approach)
2. Iterate through each upstream sequentially
3. For healthy upstreams, calculate score: `response_time / weight`
4. Select upstream with lowest score
5. If no response time available, use default: `1000ms / weight`

**Scoring Formula** (lines 234-238):
```rust
let score = if let Some(response_time) = health.response_time_ms {
    response_time as f64 / f64::from(config.weight)
} else {
    1000.0 / f64::from(config.weight)
};
```

**Example**:
```rust
// Upstream A: response_time=100ms, weight=2 → score = 50.0
// Upstream B: response_time=150ms, weight=1 → score = 150.0
// Upstream C: response_time=80ms, weight=1 → score = 80.0
// Selected: Upstream A (lowest score)

if let Some(upstream) = balancer.get_next_healthy_weighted().await {
    let response = upstream.send_request(&request).await?;
}
```

**Characteristics**:
- **Weight-aware**: Higher weight upstreams receive more traffic
- **Performance-balanced**: Combines both weight and response time
- **Sequential checks**: O(n) time complexity (no parallelization)
- **Flexible prioritization**: Allows manual traffic distribution via weights

**When to Use**:
- When some upstreams have higher capacity than others
- Gradual rollout of new upstreams (assign lower weights)
- Cost optimization (route more traffic to cheaper providers)

---

## Public API

### Constructor

#### `new() -> Self`

Creates a new load balancer with default configuration.

**Location**: Lines 67-69

```rust
let balancer = LoadBalancer::new();
```

**Initialization**:
- Empty upstreams list
- Round-robin index starts at 0
- Default config with 500ms lock timeout

#### `with_config(config: LoadBalancerConfig) -> Self`

Creates a new load balancer with custom configuration.

**Location**: Lines 72-78

```rust
let config = LoadBalancerConfig {
    lock_timeout_ms: 1000,
    health_check_timeout_ms: 200,
    high_load_threshold: 1000,
    ..Default::default()
};

let balancer = LoadBalancer::with_config(config);
```

**Use Cases**:
- High-concurrency environments needing longer timeouts
- Low-latency requirements needing aggressive timeouts
- Custom health check behavior

---

### Upstream Management

#### `add_upstream(&self, config: UpstreamConfig, http_client: Arc<HttpClient>)`

Adds a new upstream endpoint to the load balancer.

**Location**: Lines 81-85

```rust
let upstream_config = UpstreamConfig {
    url: "https://eth-mainnet.example.com".to_string(),
    name: "primary".to_string(),
    weight: 2,
    timeout_seconds: 30,
    supports_websocket: true,
    ws_url: Some("wss://eth-mainnet.example.com".to_string()),
    circuit_breaker_threshold: 5,
    circuit_breaker_timeout_seconds: 60,
    chain_id: 1,
};

balancer.add_upstream(upstream_config, http_client).await;
```

**Thread Safety**: Acquires write lock on upstreams list

**Hot-Reload**: Immediately available for request routing

#### `remove_upstream(&self, name: &str)`

Removes an upstream endpoint by name.

**Location**: Lines 271-274

```rust
// Remove unhealthy upstream from rotation
balancer.remove_upstream("failing-provider").await;
```

**Implementation**:
- Acquires write lock
- Uses `Vec::retain()` to filter out matching upstream
- Safe to call even if upstream doesn't exist

**Use Cases**:
- Dynamic upstream management
- Removing permanently failed upstreams
- Configuration changes without restart

---

### Endpoint Selection Methods

#### `get_next_healthy() -> Option<Arc<UpstreamEndpoint>>`

Gets the next healthy upstream using round-robin selection with timeout.

**Location**: Lines 88-155

**Returns**:
- `Some(upstream)` if a healthy upstream is found
- `None` if all upstreams are unhealthy or lock timeout occurs

**Lock Timeout Behavior** (lines 92-108):
```rust
// Uses aggressive timeout every 50 requests
let timeout_ms = if current_index % 50 == 0 {
    200  // aggressive_lock_timeout_ms
} else {
    500  // lock_timeout_ms
};
```

**Logging**:
- WARN: Lock timeout occurred (lines 106)
- TRACE: Selection details with index and count (lines 117-121)

**Example**:
```rust
if let Some(upstream) = balancer.get_next_healthy().await {
    match upstream.send_request(&request).await {
        Ok(response) => {
            // Process successful response
        }
        Err(e) => {
            // Handle error, upstream may become unhealthy
        }
    }
} else {
    // All upstreams unhealthy
    return Err("No healthy upstreams available");
}
```

#### `get_next_healthy_by_response_time() -> Option<Arc<UpstreamEndpoint>>`

Gets the next healthy upstream preferring lowest response time.

**Location**: Lines 159-214

**Returns**: `Some(upstream)` with lowest response time, or `None` if none healthy

**Concurrency** (lines 176-198):
- All upstreams checked in parallel
- Non-blocking health checks with 100ms timeout each
- Uses `futures::future::join_all()` for concurrent execution

**Logging**:
- WARN: Lock timeout during read lock acquisition (line 162)

**Example**:
```rust
// For latency-critical requests
if let Some(fastest_upstream) = balancer.get_next_healthy_by_response_time().await {
    let response = fastest_upstream.send_request(&request).await?;
}
```

#### `get_next_healthy_weighted() -> Option<Arc<UpstreamEndpoint>>`

Gets the next healthy upstream using weighted response time selection.

**Location**: Lines 219-248

**Returns**: `Some(upstream)` with best score (response_time / weight)

**Lock Strategy** (line 221):
- Clones upstreams list and releases lock immediately
- Allows long-running health checks without holding lock

**Example**:
```rust
// Balance between performance and capacity
if let Some(upstream) = balancer.get_next_healthy_weighted().await {
    let response = upstream.send_request(&request).await?;
}
```

---

### Upstream Queries

#### `get_all_upstreams() -> Vec<Arc<UpstreamEndpoint>>`

Gets all configured upstreams regardless of health status.

**Location**: Lines 251-253

```rust
let all_upstreams = balancer.get_all_upstreams().await;

for upstream in all_upstreams {
    println!("Upstream: {}", upstream.config().name);
    println!("  URL: {}", upstream.config().url);
    println!("  Weight: {}", upstream.config().weight);
}
```

**Returns**: Cloned vector of all upstreams

**Use Cases**:
- Admin dashboards
- Health monitoring
- Configuration inspection

#### `get_healthy_upstreams() -> Vec<Arc<UpstreamEndpoint>>`

Gets only currently healthy upstreams.

**Location**: Lines 256-268

```rust
let healthy = balancer.get_healthy_upstreams().await;
println!("Healthy upstreams: {}/{}", healthy.len(), total_count);

if healthy.is_empty() {
    // Alert: All upstreams down!
}
```

**Health Criteria** (line 262):
- `upstream.is_healthy().await` returns `true`
- Checks both health status AND circuit breaker state
- See `UpstreamEndpoint::is_healthy()` implementation

**Lock Strategy**:
- Clones upstreams list first (line 258)
- Releases lock before async health checks
- Prevents lock contention during slow health checks

---

### Statistics and Monitoring

#### `get_stats() -> LoadBalancerStats`

Gets comprehensive statistics about load balancer state.

**Location**: Lines 277-305

```rust
let stats = balancer.get_stats().await;

println!("Total upstreams: {}", stats.total_upstreams);
println!("Healthy upstreams: {}", stats.healthy_upstreams);
println!("Average response time: {}ms", stats.average_response_time_ms);

// Calculate health percentage
let health_pct = if stats.total_upstreams > 0 {
    (stats.healthy_upstreams * 100) / stats.total_upstreams
} else {
    0
};

if health_pct < 50 {
    // Alert: Less than 50% upstreams healthy
}
```

**Calculation Details** (lines 282-298):
- Iterates through all upstreams
- Counts healthy upstreams via `is_healthy()`
- Aggregates response times from health data
- Computes average response time (excludes upstreams without metrics)

**Performance**: O(n) where n = number of upstreams

**Lock Strategy**: Clones upstreams list before async operations

---

### Circuit Breaker Management

#### `get_circuit_breaker_status() -> Vec<(String, CircuitBreakerState, u32)>`

Gets circuit breaker status for all upstreams.

**Location**: Lines 308-320

```rust
let statuses = balancer.get_circuit_breaker_status().await;

for (name, state, failure_count) in statuses {
    println!("Upstream: {}", name);
    println!("  Circuit Breaker: {:?}", state);
    println!("  Failure Count: {}", failure_count);

    if state == CircuitBreakerState::Open {
        println!("  WARNING: Circuit breaker is OPEN!");
    }
}
```

**Returns**: Vector of tuples containing:
1. Upstream name
2. Circuit breaker state (Closed, Open, or HalfOpen)
3. Current failure count

**Circuit Breaker States**:
- **Closed**: Normal operation, requests allowed
- **Open**: Too many failures, requests blocked
- **HalfOpen**: Timeout expired, testing if upstream recovered

#### `reset_circuit_breaker(&self, upstream_name: &str) -> bool`

Resets circuit breaker for a specific upstream.

**Location**: Lines 323-336

```rust
// Manual recovery after fixing upstream issues
if balancer.reset_circuit_breaker("problematic-upstream").await {
    println!("Circuit breaker reset successfully");
} else {
    println!("Upstream not found");
}
```

**Returns**:
- `true` if upstream found and circuit breaker reset
- `false` if upstream not found

**Implementation** (lines 328):
- Calls `upstream.circuit_breaker().on_success().await`
- This resets failure count and closes circuit breaker

**Logging**:
- INFO: Circuit breaker reset successful (line 329)
- WARN: Upstream not found (line 334)

**Use Cases**:
- Manual intervention after fixing upstream issues
- Administrative reset tools
- Testing and debugging

---

## Health Integration

The load balancer integrates tightly with upstream health monitoring and circuit breaker patterns.

### Health Check Flow

```
┌─────────────────────┐
│  Request arrives    │
└──────────┬──────────┘
           │
           ▼
┌─────────────────────┐
│ get_next_healthy()  │  ← Uses current_index atomically
└──────────┬──────────┘
           │
           ▼
┌─────────────────────┐
│ Acquire read lock   │  ← Timeout: 500ms (or 200ms every 50 reqs)
│ on upstreams list   │
└──────────┬──────────┘
           │
           ▼
┌─────────────────────────────┐
│ Calculate start_index       │  ← start_index = current_index % upstream_count
│ = current_index % count     │
└──────────┬──────────────────┘
           │
           ▼
┌─────────────────────────────┐
│ Check health of upstream    │  ← upstream.is_healthy() with timeout
│ at start_index              │
└──────────┬──────────────────┘
           │
     ┌─────┴─────┐
     │           │
  Healthy?    Unhealthy
     │           │
     │           ▼
     │    ┌──────────────────┐
     │    │ Try next N-1     │  ← Round-robin through remaining
     │    │ upstreams        │     (with aggressive 25ms timeout)
     │    └──────┬───────────┘
     │           │
     │      ┌────┴────┐
     │      │         │
     │   Found?    None found
     │      │         │
     ▼      ▼         ▼
┌────────────┐   ┌────────┐
│ Return     │   │ Return │
│ upstream   │   │ None   │
└────────────┘   └────────┘
```

### UpstreamEndpoint Health Check

Each `UpstreamEndpoint` has its own health tracking:

**Location**: `crates/prism-core/src/upstream/endpoint.rs:159-164`

```rust
pub async fn is_healthy(&self) -> bool {
    let health = self.health.read().await;
    let circuit_breaker_allows = self.circuit_breaker.can_execute().await;

    health.is_healthy && circuit_breaker_allows
}
```

**Health Criteria**:
1. **Health Status**: `health.is_healthy` flag (set by health checker)
2. **Circuit Breaker**: Circuit breaker state must allow execution

**Health Updates** (endpoint.rs:278-298):
- Success: Resets error count, sets healthy
- Failure: Increments error count, sets unhealthy after 3 errors

### Circuit Breaker Integration

**Circuit Breaker States**:
- **Closed**: Normal operation
  - Failures increment counter
  - Opens after reaching threshold

- **Open**: Blocking requests
  - All requests immediately rejected
  - After timeout, transitions to HalfOpen

- **HalfOpen**: Testing recovery
  - Next request is test request
  - Success → Closed, Failure → Open

**Circuit Breaker Configuration** (UpstreamConfig):
- `circuit_breaker_threshold`: Failures before opening (default: 5)
- `circuit_breaker_timeout_seconds`: Time before HalfOpen (default: 60s)

**Example Flow**:
```
Upstream receives 5 consecutive failures
  ↓
Circuit breaker opens
  ↓
All requests blocked for 60 seconds
  ↓
After 60s, transitions to HalfOpen
  ↓
Next request is test request
  ↓
If successful → Circuit closes, upstream healthy
If fails → Circuit opens again for another 60s
```

### Adaptive Timeout Strategy

The load balancer adapts its health check timeouts based on load:

**Normal Load** (index ≤ 500):
```rust
health_timeout = Duration::from_millis(100);  // 100ms
```

**High Load** (index > 500):
```rust
health_timeout = Duration::from_millis(50);   // 50ms (aggressive)
```

**Fallback Checks**:
```rust
fallback_timeout = Duration::from_millis(25); // 25ms (very aggressive)
```

**Rationale** (lines 124-128):
- Normal load: Allow time for accurate health checks
- High load: Prioritize responsiveness over accuracy
- Fallback: Very fast to minimize impact on request latency

---

## Thread Safety

The load balancer is fully thread-safe and optimized for high concurrency.

### Concurrency Primitives

**Atomic Operations**:
- `current_index: AtomicUsize` (line 55)
  - Uses `Ordering::Relaxed` for fetch_add (line 115)
  - No synchronization needed (just counter)
  - Lock-free, zero contention

**Read-Write Locks**:
- `upstreams: ArcSwap<Vec<...>>` (line 61)
  - Multiple concurrent readers allowed
  - Exclusive write access for add/remove
  - Timeout protection prevents indefinite blocking

**Arc-Based Sharing**:
- All upstreams wrapped in `Arc<UpstreamEndpoint>`
  - Cheap cloning (just increment ref count)
  - Safe to share across threads/tasks
  - Automatic cleanup when last reference dropped

### Lock Ordering

No explicit lock ordering required because:
1. Only one lock in `LoadBalancer` itself (upstreams RwLock)
2. Upstreams have their own internal locks (health, circuit breaker)
3. Lock acquisition is always same order: LoadBalancer → UpstreamEndpoint

### Clone Semantics

The `LoadBalancer` itself is NOT clonable, but commonly wrapped in `Arc`:

```rust
let balancer = Arc::new(LoadBalancer::new());

// Clone Arc, not LoadBalancer
let balancer_clone = balancer.clone(); // Cheap: just increment ref count

// Can share across threads
let handle = tokio::spawn(async move {
    balancer_clone.get_next_healthy().await
});
```

### Lock Timeout Strategy

**Why Timeouts?** (lines 37-39, 92-108)

Without timeouts, a stuck write operation (e.g., during `add_upstream()`) would cause all readers to block indefinitely. Timeouts ensure:

1. **Responsiveness**: Requests fail fast rather than hanging
2. **Resource leak prevention**: Prevents accumulation of blocked tasks
3. **Degraded operation**: Returns `None` instead of deadlocking

**Timeout Selection**:
- **500ms normal**: Read lock is very fast (just cloning Arc)
- **200ms aggressive**: Every 50th request uses shorter timeout
- **Prevents**: Slow accumulation of timeout-prone requests

**Failure Handling**:
```rust
let Ok(upstreams) = tokio::time::timeout(
    Duration::from_millis(timeout_ms),
    self.upstreams.read()
).await else {
    tracing::warn!("Timeout acquiring upstreams read lock after {}ms", timeout_ms);
    return None;  // Graceful degradation
};
```

### Lock-Free Hot Path

The most critical operation (round-robin index increment) is lock-free:

```rust
let start_index = self.current_index.fetch_add(1, Ordering::Relaxed) % upstream_count;
```

**Benefits**:
- No lock contention on hot path
- Scales linearly with CPU cores
- Sub-microsecond latency

---

## Usage Examples

### Example 1: Basic Setup with Multiple Upstreams

```rust
use prism_core::upstream::load_balancer::LoadBalancer;
use prism_core::types::UpstreamConfig;
use prism_core::upstream::http_client::HttpClient;
use std::sync::Arc;

// Create load balancer
let balancer = LoadBalancer::new();
let http_client = Arc::new(HttpClient::new()?);

// Add primary upstream
balancer.add_upstream(
    UpstreamConfig {
        url: "https://eth-mainnet.infura.io".to_string(),
        name: "infura".to_string(),
        weight: 2,
        timeout_seconds: 30,
        supports_websocket: true,
        ws_url: Some("wss://eth-mainnet.infura.io".to_string()),
        circuit_breaker_threshold: 5,
        circuit_breaker_timeout_seconds: 60,
        chain_id: 1,
    },
    http_client.clone()
).await;

// Add backup upstream
balancer.add_upstream(
    UpstreamConfig {
        url: "https://cloudflare-eth.com".to_string(),
        name: "cloudflare".to_string(),
        weight: 1,
        timeout_seconds: 30,
        supports_websocket: false,
        ws_url: None,
        circuit_breaker_threshold: 3,
        circuit_breaker_timeout_seconds: 30,
        chain_id: 1,
    },
    http_client.clone()
).await;

// Load balancer is now ready
println!("Configured upstreams: {}", balancer.get_all_upstreams().await.len());
```

**Source**: Pattern from lines 354-383 (tests)

### Example 2: Request Routing with Round-Robin

```rust
use prism_core::types::JsonRpcRequest;

// Create JSON-RPC request
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_blockNumber".to_string(),
    params: None,
    id: serde_json::Value::Number(1.into()),
};

// Get next healthy upstream (round-robin)
if let Some(upstream) = balancer.get_next_healthy().await {
    println!("Selected upstream: {}", upstream.config().name);

    // Send request
    match upstream.send_request(&request).await {
        Ok(response) => {
            if let Some(result) = response.result {
                println!("Block number: {}", result);
            }
        }
        Err(e) => {
            eprintln!("Request failed: {:?}", e);
            // Upstream may be marked unhealthy automatically
        }
    }
} else {
    eprintln!("No healthy upstreams available!");
    // Implement fallback logic or return error
}
```

### Example 3: Performance-Optimized Selection

```rust
// For latency-critical requests, use response-time selection
async fn get_latest_block(balancer: &LoadBalancer) -> Result<u64> {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "eth_blockNumber".to_string(),
        params: None,
        id: serde_json::Value::Number(1.into()),
    };

    // Select fastest upstream
    let upstream = balancer
        .get_next_healthy_by_response_time()
        .await
        .ok_or("No healthy upstreams")?;

    let response = upstream.send_request(&request).await?;

    // Parse hex block number
    let block_hex = response.result
        .and_then(|v| v.as_str().map(String::from))
        .ok_or("Invalid response")?;

    let block_number = u64::from_str_radix(block_hex.trim_start_matches("0x"), 16)?;

    Ok(block_number)
}
```

**Source**: Pattern from lines 159-214

### Example 4: Weighted Load Balancing

```rust
// Configure upstreams with different weights
// Higher weight = more capacity/priority

// Add high-capacity upstream (weight=3)
balancer.add_upstream(
    UpstreamConfig {
        url: "https://premium-provider.com".to_string(),
        name: "premium".to_string(),
        weight: 3,  // High capacity
        // ... other config
    },
    http_client.clone()
).await;

// Add standard upstream (weight=1)
balancer.add_upstream(
    UpstreamConfig {
        url: "https://standard-provider.com".to_string(),
        name: "standard".to_string(),
        weight: 1,  // Standard capacity
        // ... other config
    },
    http_client.clone()
).await;

// Add new upstream being tested (weight=1, lower score due to higher response time expected)
balancer.add_upstream(
    UpstreamConfig {
        url: "https://new-provider.com".to_string(),
        name: "new-provider".to_string(),
        weight: 1,  // Testing phase
        // ... other config
    },
    http_client.clone()
).await;

// Use weighted selection
// Premium provider will be preferred if response times are similar
if let Some(upstream) = balancer.get_next_healthy_weighted().await {
    // Premium provider score: 100ms / 3 = 33.3
    // Standard provider score: 100ms / 1 = 100.0
    // New provider score: 200ms / 1 = 200.0
    // → Premium provider selected

    let response = upstream.send_request(&request).await?;
}
```

**Source**: Pattern from lines 219-248

### Example 5: Health Monitoring Dashboard

```rust
use prism_core::upstream::circuit_breaker::CircuitBreakerState;

async fn print_health_dashboard(balancer: &LoadBalancer) {
    // Get overall statistics
    let stats = balancer.get_stats().await;

    println!("=== Load Balancer Health Dashboard ===");
    println!("Total Upstreams: {}", stats.total_upstreams);
    println!("Healthy Upstreams: {}", stats.healthy_upstreams);
    println!("Average Response Time: {}ms", stats.average_response_time_ms);
    println!();

    // Calculate health percentage
    let health_pct = if stats.total_upstreams > 0 {
        (stats.healthy_upstreams * 100) / stats.total_upstreams
    } else {
        0
    };

    println!("Health Status: {}%", health_pct);
    if health_pct < 50 {
        println!("️  WARNING: Less than 50% upstreams healthy!");
    }
    println!();

    // Get circuit breaker status
    let cb_statuses = balancer.get_circuit_breaker_status().await;

    println!("=== Circuit Breaker Status ===");
    for (name, state, failure_count) in cb_statuses {
        let status_icon = match state {
            CircuitBreakerState::Closed => "",
            CircuitBreakerState::HalfOpen => "◐",
            CircuitBreakerState::Open => "✗",
        };

        println!("{} {} (state: {:?}, failures: {})",
            status_icon, name, state, failure_count);
    }
    println!();

    // Get detailed upstream info
    let all_upstreams = balancer.get_all_upstreams().await;

    println!("=== Upstream Details ===");
    for upstream in all_upstreams {
        let config = upstream.config();
        let health = upstream.get_health().await;
        let is_healthy = upstream.is_healthy().await;

        println!("Name: {}", config.name);
        println!("  URL: {}", config.url);
        println!("  Weight: {}", config.weight);
        println!("  Healthy: {}", is_healthy);

        if let Some(response_time) = health.response_time_ms {
            println!("  Response Time: {}ms", response_time);
        }

        println!("  Error Count: {}", health.error_count);
        println!();
    }
}
```

**Source**: Pattern from lines 277-320

### Example 6: Circuit Breaker Recovery

```rust
use tokio::time::{sleep, Duration};

async fn monitor_and_recover_circuit_breakers(balancer: Arc<LoadBalancer>) {
    loop {
        sleep(Duration::from_secs(30)).await;

        let statuses = balancer.get_circuit_breaker_status().await;

        for (name, state, failure_count) in statuses {
            if state == CircuitBreakerState::Open {
                tracing::warn!(
                    "Circuit breaker OPEN for upstream: {} (failures: {})",
                    name,
                    failure_count
                );

                // Optional: Manual reset after investigation
                // if admin_confirmed_recovery(&name).await {
                //     balancer.reset_circuit_breaker(&name).await;
                //     tracing::info!("Manually reset circuit breaker for {}", name);
                // }
            } else if state == CircuitBreakerState::HalfOpen {
                tracing::info!(
                    "Circuit breaker HALF-OPEN for upstream: {} (testing recovery)",
                    name
                );
            }
        }
    }
}
```

**Source**: Pattern from lines 308-336

### Example 7: Dynamic Upstream Management

```rust
async fn manage_upstreams_dynamically(balancer: Arc<LoadBalancer>) {
    // Add new upstream at runtime
    let new_upstream = UpstreamConfig {
        url: "https://new-provider.com".to_string(),
        name: "new-provider".to_string(),
        weight: 1,
        timeout_seconds: 30,
        supports_websocket: false,
        ws_url: None,
        circuit_breaker_threshold: 3,
        circuit_breaker_timeout_seconds: 60,
        chain_id: 1,
    };

    balancer.add_upstream(new_upstream, http_client.clone()).await;
    tracing::info!("Added new upstream: new-provider");

    // Monitor its health
    tokio::time::sleep(Duration::from_secs(60)).await;

    let stats = balancer.get_stats().await;
    if stats.healthy_upstreams < stats.total_upstreams {
        // One or more upstreams unhealthy
        let all = balancer.get_all_upstreams().await;

        for upstream in all {
            if !upstream.is_healthy().await {
                let name = upstream.config().name.clone();
                tracing::warn!("Unhealthy upstream detected: {}", name);

                // Optional: Remove after sustained failures
                // balancer.remove_upstream(&name).await;
                // tracing::info!("Removed unhealthy upstream: {}", name);
            }
        }
    }
}
```

**Source**: Pattern from lines 81-85, 271-274

### Example 8: Custom Configuration for High Concurrency

```rust
use prism_core::upstream::load_balancer::LoadBalancerConfig;

// Custom config for high-throughput environment (100+ concurrent requests)
let config = LoadBalancerConfig {
    lock_timeout_ms: 1000,              // Longer timeout for heavy load
    aggressive_lock_timeout_ms: 300,    // Still aggressive periodically
    aggressive_timeout_interval: 25,    // More frequent aggressive timeouts
    health_check_timeout_ms: 150,       // Slightly longer for accuracy
    health_check_high_load_timeout_ms: 75,  // Still fast under load
    fallback_health_check_timeout_ms: 50,   // Less aggressive fallback
    high_load_threshold: 1000,          // Higher threshold before aggressive mode
};

let balancer = LoadBalancer::with_config(config);

// Add upstreams
// ... (same as before)

// Now optimized for high-concurrency workload
```

**Source**: Pattern from lines 72-78, 16-49

---

## Performance Characteristics

### Time Complexity

**Endpoint Selection**:
- `get_next_healthy()`: O(1) best case (first upstream healthy), O(n) worst case
- `get_next_healthy_by_response_time()`: O(1) due to concurrent health checks
- `get_next_healthy_weighted()`: O(n) sequential health checks

**Upstream Management**:
- `add_upstream()`: O(1) amortized (Vec::push)
- `remove_upstream()`: O(n) (Vec::retain)
- `get_all_upstreams()`: O(n) clone
- `get_healthy_upstreams()`: O(n) with health checks

**Statistics**:
- `get_stats()`: O(n) with health checks per upstream
- `get_circuit_breaker_status()`: O(n) with async calls per upstream

### Space Complexity

**Per LoadBalancer**:
- Upstreams list: O(n) where n = number of upstreams
- Each upstream wrapped in Arc: ~8 bytes pointer overhead
- Config: ~56 bytes (7 fields)
- Current index: 8 bytes (AtomicUsize)
- Total: ~72 bytes + (n * 8 bytes for Arc pointers)

**Per UpstreamEndpoint**:
- See `crates/prism-core/src/upstream/endpoint.rs`
- Includes HttpClient, WebSocketHandler, CircuitBreaker, Health tracking

### Concurrency Performance

**Lock Contention**:
- Read lock: Supports multiple concurrent readers (no contention for reads)
- Write lock: Exclusive access (rare operations: add/remove)
- Atomic counter: Zero contention (lock-free)

**Scalability**:
- Tested with 85+ parallel requests (lines 37-39)
- Lock timeouts prevent deadlocks under extreme load
- Response-time selection scales with CPU cores (parallel health checks)

**Throughput Characteristics**:
- Round-robin: 100k+ requests/second (atomic operations only)
- Response-time: Limited by concurrent health check overhead
- Weighted: Similar to round-robin (sequential but fast health checks)

### Optimization Notes

**Hot Path Optimizations**:
- Atomic counter increment is lock-free (line 115)
- Read lock acquisition uses timeout to prevent hanging (lines 100-108)
- Health checks use aggressive timeouts under load (lines 124-152)

**Memory Optimizations**:
- Arc-wrapping prevents cloning large structures
- Vec for upstreams (cache-friendly linear iteration)
- Upstreams cloned list released before async operations (lines 221, 258)

**Latency Optimizations**:
- Aggressive timeouts every N requests prevent leak accumulation
- Concurrent health checks (response-time selection)
- Fast-path for first upstream healthy (round-robin)

---

## UpstreamManager Integration

The `LoadBalancer` is typically used through the `UpstreamManager`, which provides higher-level request routing with automatic strategy selection.

### Using UpstreamManager with LoadBalancer

**Recommended Method**: `manager.send_request_auto()`

**Location**: `crates/prism-core/src/upstream/manager.rs`

The `send_request_auto()` method is the primary API for sending requests through the load balancer. It automatically chooses between hedging and response-time-aware strategies based on your configuration.

**Example**:
```rust
use prism_core::upstream::manager::UpstreamManager;
use prism_core::types::JsonRpcRequest;

// Create upstream manager (manages load balancer internally)
let manager = UpstreamManager::new(config)?;

// Create request
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_blockNumber".to_string(),
    params: None,
    id: serde_json::Value::Number(1.into()),
};

// Send request - automatically uses optimal strategy
match manager.send_request_auto(&request).await {
    Ok(response) => {
        println!("Response: {:?}", response);
    }
    Err(e) => {
        eprintln!("Request failed: {:?}", e);
    }
}
```

### Automatic Strategy Selection

The `send_request_auto()` method intelligently selects the routing strategy:

**With Hedging Enabled** (`hedging_enabled: true` in config):
- Sends requests to multiple upstreams simultaneously
- Returns first successful response
- Provides lowest latency at cost of more upstream requests
- Best for: Latency-critical applications, read-heavy workloads

**Without Hedging** (default):
- Uses response-time-aware selection via `LoadBalancer::get_next_healthy_by_response_time()`
- Routes to fastest upstream based on historical performance
- More efficient use of upstream capacity
- Best for: Most production scenarios, balanced performance

**Configuration Example**:
```rust
use prism_core::types::PrismConfig;

let config = PrismConfig {
    hedging_enabled: true,  // Enable hedging strategy
    // ... other config
};

let manager = UpstreamManager::new(config)?;

// Now send_request_auto() will use hedging
let response = manager.send_request_auto(&request).await?;
```

### Direct LoadBalancer Usage

For fine-grained control, you can access the load balancer directly:

```rust
// Access load balancer from manager
let balancer = manager.load_balancer();

// Choose specific strategy
let upstream = balancer.get_next_healthy_weighted().await
    .ok_or("No healthy upstreams")?;

// Send request to selected upstream
let response = upstream.send_request(&request).await?;
```

**When to Use Direct LoadBalancer**:
- Custom selection logic not covered by auto strategy
- A/B testing different selection strategies
- Advanced monitoring and metrics collection
- Special routing requirements (e.g., sticky sessions)

**When to Use UpstreamManager**:
- Standard production use (recommended)
- Automatic failover and retry logic
- Simplified API surface
- Hedging support

---

## Related Documentation

- **UpstreamEndpoint**: `crates/prism-core/src/upstream/endpoint.rs` - Individual upstream with health tracking
- **HealthChecker**: `crates/prism-core/src/upstream/health.rs` - Periodic health monitoring
- **CircuitBreaker**: `crates/prism-core/src/upstream/circuit_breaker.rs` - Failure protection pattern
- **UpstreamManager**: `crates/prism-core/src/upstream/manager.rs` - Higher-level upstream orchestration with `send_request_auto()`
- **HttpClient**: `crates/prism-core/src/upstream/http_client.rs` - HTTP request client

---

## Summary

The `LoadBalancer` is a production-ready, high-performance component that provides:

**Key Features**:
1. **Multiple selection strategies**: Round-robin, response-time, and weighted algorithms
2. **Health integration**: Automatic failover based on health checks and circuit breakers
3. **High concurrency**: Optimized for 85+ parallel requests with timeout protection
4. **Dynamic management**: Add/remove upstreams at runtime without downtime
5. **Comprehensive monitoring**: Statistics, circuit breaker status, and health tracking

**Performance Highlights**:
- Lock-free hot path (atomic counter for round-robin)
- Concurrent health checks (response-time selection)
- Configurable timeouts prevent resource leaks
- Scales linearly with CPU cores

**Thread Safety**:
- All operations are concurrent-safe
- Uses atomic operations and RwLock
- Timeout protection prevents deadlocks
- Arc-based sharing across threads/tasks

**When to Use Each Strategy**:
- **Round-Robin**: Default choice for fair load distribution
- **Response-Time**: Latency-critical requests requiring fastest upstream
- **Weighted**: Capacity-aware distribution or gradual rollouts

Key implementation details:
1. Lock timeout strategy balances responsiveness with concurrency (lines 92-108)
2. Adaptive health check timeouts optimize for load (lines 124-152)
3. Concurrent health checks enable O(1) response-time selection (lines 176-198)
4. Circuit breaker integration provides automatic failure protection
5. All statistics methods use lock-then-clone pattern to prevent contention
