# ProxyEngine Component Documentation

## Overview

The `ProxyEngine` is the central request orchestration component in Prism's RPC aggregator architecture. It acts as the main entry point for processing JSON-RPC requests, coordinating between caching, upstream forwarding, and metrics collection. The engine implements intelligent routing logic to determine how each RPC method should be handled, whether through specialized handlers with caching support or direct upstream forwarding.

**Location:** `crates/prism-core/src/proxy/engine.rs`

**Key Responsibilities:**
- Request validation and method authorization
- Intelligent routing to specialized handlers
- Coordination between cache, upstream, and metrics systems
- Statistics aggregation and reporting
- Thread-safe concurrent request processing

## Architecture

### Component Structure

```
ProxyEngine
├── ctx: Arc<SharedContext>                    // Shared context with managers
│   ├── cache_manager: Arc<CacheManager>      // Shared cache access
│   ├── upstream_manager: Arc<UpstreamManager> // Upstream RPC endpoint management
│   └── metrics_collector: Arc<MetricsCollector> // Prometheus metrics
├── logs_handler: LogsHandler                  // eth_getLogs specialist
├── blocks_handler: BlocksHandler              // Block query specialist
└── transactions_handler: TransactionsHandler  // Transaction query specialist
```

The engine uses an Arc-based shared ownership model for thread safety and efficient cloning across async tasks. Each specialized handler receives Arc clones of the core managers during initialization, ensuring consistent access to caching, upstream forwarding, and metrics collection.

### Design Philosophy

The ProxyEngine follows a **handler delegation pattern** where:

1. **Method-specific handlers** implement optimized caching strategies for frequently-used methods
2. **Generic forwarding** handles all other allowed methods without caching
3. **Centralized validation** ensures all requests meet security and format requirements
4. **Shared context** through Arc enables efficient resource sharing without locks on read paths

This architecture allows the engine to scale horizontally (handle many concurrent requests) while maintaining vertical optimization (specialized handling for important methods).

## Public API

### Constructor

#### `ProxyEngine::new`

```rust
pub fn new(
    cache_manager: Arc<CacheManager>,
    upstream_manager: Arc<UpstreamManager>,
    metrics_collector: Arc<MetricsCollector>,
) -> Self
```

**Purpose:** Creates a new ProxyEngine instance with the provided managers.

**Parameters:**
- `cache_manager`: Shared reference to the cache manager for storing and retrieving cached data
- `upstream_manager`: Shared reference to the upstream manager for forwarding requests to RPC providers
- `metrics_collector`: Shared reference to the metrics collector for recording performance data

**Returns:** A fully initialized ProxyEngine with all handlers configured

**Implementation Details:**
- Initializes three specialized handlers (logs, blocks, transactions) by cloning the Arc references
- Each handler receives the same shared managers for consistent behavior
- No I/O operations performed during construction
- All handlers are ready to process requests immediately

**File Reference:** Lines 109-121 in `crates/prism-core/src/proxy/engine.rs` (`ProxyEngine::new`)

**Usage Example:**
```rust
use std::sync::Arc;
use prism_core::{
    cache::{CacheManager, CacheManagerConfig},
    metrics::MetricsCollector,
    upstream::manager::UpstreamManager,
    proxy::ProxyEngine,
};

let chain_state = Arc::new(ChainState::new());
let cache = Arc::new(
    CacheManager::new(&CacheManagerConfig::default(), chain_state.clone())?
);
let upstream = Arc::new(
    UpstreamManagerBuilder::new()
        .chain_state(chain_state.clone())
        .build()?
);
let metrics = Arc::new(MetricsCollector::new()?);

let engine = ProxyEngine::new(cache, upstream, metrics);
```

### Core Request Processing

#### `process_request`

```rust
pub async fn process_request(
    &self,
    request: JsonRpcRequest,
) -> Result<JsonRpcResponse, ProxyError>
```

**Purpose:** The main entry point for processing JSON-RPC requests with validation and routing.

**Parameters:**
- `request`: A JSON-RPC request structure containing method, params, and id

**Returns:**
- `Ok(JsonRpcResponse)`: Successful response with result and cache status
- `Err(ProxyError)`: Validation failure or processing error

**Behavior:**
1. **Validation Phase**: Calls `request.validate()` to check JSON-RPC version, method format, and parameters
2. **Authorization Phase**: Verifies the method is in the allowed methods list using `is_method_allowed()`
3. **Routing Phase**: Delegates to `handle_request()` for method-specific processing

**Error Conditions:**
- `ProxyError::Validation`: Invalid JSON-RPC version, malformed method name, or invalid parameters
- `ProxyError::MethodNotSupported`: Method not in the allowed list
- Other errors propagated from downstream handlers

**Security Considerations:**
- All requests undergo validation before processing to prevent injection attacks
- Method allowlist prevents execution of dangerous or unsupported RPC calls
- Parameter validation specific to each method type (range checks, topic limits, etc.)

**File Reference:** Lines 93-104 in `crates/prism-core/src/proxy/engine.rs` (`ProxyEngine::process_request`)

**Usage Example:**
```rust
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(serde_json::json!(["0x1234", false])),
    id: serde_json::json!(1),
};

match engine.process_request(request).await {
    Ok(response) => {
        println!("Cache status: {:?}", response.cache_status);
        println!("Result: {:?}", response.result);
    }
    Err(ProxyError::MethodNotSupported(method)) => {
        eprintln!("Method {} is not supported", method);
    }
    Err(e) => eprintln!("Error: {}", e),
}
```

### Method Support

#### `is_method_supported`

```rust
pub fn is_method_supported(method: &str) -> bool
```

**Purpose:** Static method to check if a given RPC method is supported without creating an engine instance.

**Parameters:**
- `method`: Method name to check (e.g., "eth_getBlockByNumber")

**Returns:** `true` if the method is in the allowed list, `false` otherwise

**Allowed Methods List** (as of current implementation):
- `net_version` - Network version identification
- `eth_blockNumber` - Current block number
- `eth_chainId` - Chain ID
- `eth_gasPrice` - Current gas price
- `eth_getBalance` - Account balance lookup
- `eth_getBlockByHash` - Block retrieval by hash (cached)
- `eth_getBlockByNumber` - Block retrieval by number (cached)
- `eth_getLogs` - Event logs with advanced caching
- `eth_getTransactionByHash` - Transaction lookup (cached)
- `eth_getTransactionReceipt` - Transaction receipt (cached)

**Implementation Note:**
This method delegates to the centralized `is_method_allowed()` function in the types module, ensuring a single source of truth for method authorization.

**File Reference:** Lines 156-158 in `crates/prism-core/src/proxy/engine.rs` (`ProxyEngine::is_method_supported`)

**Usage Example:**
```rust
if ProxyEngine::is_method_supported("eth_getLogs") {
    // Safe to create a request
    let request = create_get_logs_request();
} else {
    // Method not supported, handle accordingly
    return Err("Unsupported method");
}
```

### Statistics and Monitoring

#### `get_cache_stats`

```rust
pub async fn get_cache_stats(&self) -> CacheStats
```

**Purpose:** Retrieves current cache statistics for monitoring and debugging.

**Returns:** `CacheStats` structure containing:
- `block_cache_entries`: Combined count of block headers and bodies
- `transaction_cache_entries`: Combined count of transactions and receipts
- `logs_cache_entries`: Count of log records in the log store

**Computation:**
- Queries the underlying `CacheManager` for comprehensive statistics
- Aggregates related cache types for simplified reporting
- Header + body = total block cache entries
- Transaction + receipt = total transaction cache entries

**Performance:** O(1) - reads atomic counters, no cache traversal required

**File Reference:** Lines 165-175 in `crates/prism-core/src/proxy/engine.rs` (`ProxyEngine::get_cache_stats`)

**Usage Example:**
```rust
let stats = engine.get_cache_stats().await;
println!("Blocks cached: {}", stats.block_cache_entries);
println!("Transactions cached: {}", stats.transaction_cache_entries);
println!("Logs cached: {}", stats.logs_cache_entries);
```

#### `get_upstream_stats`

```rust
pub async fn get_upstream_stats(&self) -> LoadBalancerStats
```

**Purpose:** Retrieves current load balancer and upstream health statistics.

**Returns:** `LoadBalancerStats` containing upstream health, response times, and request distribution

**Use Cases:**
- Monitoring upstream endpoint health
- Debugging connection issues
- Load balancing effectiveness analysis
- Capacity planning

**File Reference:** Lines 181-183 in `crates/prism-core/src/proxy/engine.rs` (`ProxyEngine::get_upstream_stats`)

**Usage Example:**
```rust
let stats = engine.get_upstream_stats().await;
for endpoint in stats.endpoints {
    println!("{}: healthy={}, response_time={:?}ms",
        endpoint.name,
        endpoint.is_healthy,
        endpoint.avg_response_time_ms
    );
}
```

### Testing Utilities

#### `get_cache_manager`

```rust
pub fn get_cache_manager(&self) -> &Arc<CacheManager>
```

**Purpose:** Provides direct access to the cache manager for testing and advanced use cases.

**Returns:** Reference to the shared cache manager Arc

**Intended Use:**
- Integration testing where direct cache manipulation is needed
- Test fixtures that need to pre-populate cache
- Advanced debugging scenarios

**File Reference:** Lines 191-193 in `crates/prism-core/src/proxy/engine.rs` (`ProxyEngine::get_cache_manager`)

#### `get_metrics_collector`

```rust
pub fn get_metrics_collector(&self) -> &Arc<MetricsCollector>
```

**Purpose:** Provides direct access to the metrics collector for testing and metrics export.

**Returns:** Reference to the shared metrics collector Arc

**Intended Use:**
- Testing metrics recording behavior
- Prometheus exporter integration
- Custom metrics dashboards

**File Reference:** Lines 200-202 in `crates/prism-core/src/proxy/engine.rs` (`ProxyEngine::get_metrics_collector`)

## Request Flow

### High-Level Flow Diagram

```
Client Request
    ↓
┌─────────────────────────┐
│  process_request()      │  ← Entry point
├─────────────────────────┤
│ 1. Validate request     │  ← Check JSON-RPC format
│ 2. Check method allowed │  ← Security check
└─────────┬───────────────┘
          ↓
┌─────────────────────────┐
│  handle_request()       │  ← Internal routing
└─────────┬───────────────┘
          ↓
    ┌─────┴──────┐
    ↓            ↓
[Specialized] [Generic]
 Handlers     Forward
    ↓            ↓
  Cache      Upstream
  Check      Request
    ↓            ↓
  Result  ←  Result
```

### Detailed Request Processing

#### 1. Validation Phase

**File Reference:** Lines 97-100 in `crates/prism-core/src/proxy/engine.rs` (`process_request`)

```rust
request.validate().map_err(ProxyError::Validation)?;

if !is_method_allowed(&request.method) {
    return Err(ProxyError::MethodNotSupported(request.method));
}
```

**Validations Performed:**
- JSON-RPC version must be "2.0"
- Method name contains only alphanumeric characters and underscores
- Method is in the allowed methods list
- Method-specific parameter validation:
  - `eth_getLogs`: Block range ≤ 10,000 blocks, topics ≤ 4
  - `eth_getBlockByNumber`: Valid block parameter (hex or tag)
  - Other methods: Basic parameter presence checks

#### 2. Method Routing

**File Reference:** Lines 111-126 in `crates/prism-core/src/proxy/engine.rs` (`handle_request`)

The engine uses pattern matching to route requests to specialized handlers:

```rust
match request.method.as_str() {
    "eth_getLogs" =>
        logs_handler.handle_advanced_logs_request(request).await,
    "eth_getBlockByNumber" =>
        blocks_handler.handle_block_by_number_request(request).await,
    "eth_getBlockByHash" =>
        blocks_handler.handle_block_by_hash_request(request).await,
    "eth_getTransactionByHash" =>
        transactions_handler.handle_transaction_by_hash_request(request).await,
    "eth_getTransactionReceipt" =>
        transactions_handler.handle_transaction_receipt_request(request).await,
    _ =>
        forward_to_upstream(&request).await,
}
```

**Routing Strategy:**

**Specialized Handlers** (5 methods):
- `eth_getLogs`: Advanced partial-range caching with concurrent missing range fetch
- `eth_getBlockByNumber`: Block header/body caching with special tag handling
- `eth_getBlockByHash`: Block lookup by hash with header/body separation
- `eth_getTransactionByHash`: Transaction data caching
- `eth_getTransactionReceipt`: Receipt caching

**Generic Forwarding** (5 methods):
- `net_version`, `eth_blockNumber`, `eth_chainId`, `eth_gasPrice`, `eth_getBalance`
- No caching applied - always forwarded to upstream
- Response marked with `CacheStatus::Miss`

#### 3. Cache-Aware Processing (Specialized Handlers)

Each specialized handler implements a four-tier cache strategy:

**Full Cache Hit:**
- All requested data found in cache
- Immediate response with `CacheStatus::Full`
- No upstream communication required
- Fastest response path

**Partial Cache Hit** (eth_getLogs only):
- Some data cached, some missing
- Concurrent fetch of missing ranges from upstream
- Merge cached and fetched data
- Sort by block number and log index
- Response marked `CacheStatus::Partial`

**Empty Cache Hit** (eth_getLogs only):
- Block range previously queried but returned no logs
- Immediate empty array response
- No upstream communication
- Response marked `CacheStatus::Empty`

**Cache Miss:**
- No cached data available
- Forward to upstream
- Cache successful response for future requests
- Response marked `CacheStatus::Miss`

#### 4. Upstream Forwarding

**File Reference:** Lines 134-148 in `crates/prism-core/src/proxy/engine.rs` (`forward_to_upstream`)

```rust
async fn forward_to_upstream(
    &self,
    request: &JsonRpcRequest,
) -> Result<JsonRpcResponse, ProxyError>
```

**Process:**
1. Call `upstream_manager.send_request_auto(request)` which delegates to router
2. Router selects appropriate strategy (consensus/scoring/hedging/simple) and executes
3. Upstream manager handles:
   - Load balancing across healthy endpoints
   - Retry logic with exponential backoff (if unhealthy_behavior enabled)
   - Circuit breaker pattern for failing upstreams
   - Request timeout enforcement
4. Add `CacheStatus::Miss` to response
4. Return response or convert upstream error to `ProxyError::Upstream`

## Configuration

The ProxyEngine itself has no direct configuration - it inherits behavior from its dependencies:

### Cache Configuration (via CacheManager)

Controls caching behavior for specialized handlers:

```rust
CacheManagerConfig {
    header_cache_capacity: 10000,           // Block headers
    body_cache_capacity: 5000,              // Block bodies
    transaction_cache_capacity: 50000,      // Transactions
    receipt_cache_capacity: 50000,          // Receipts
    log_store_capacity: 100000,             // Log records
    reorg_protection_blocks: 64,            // Finality threshold
}
```

**Impact on ProxyEngine:**
- Higher capacities = more cache hits for specialized methods
- Reorg protection = blocks cached only when finalized
- Eviction policies affect cache hit rates

### Upstream Configuration (via UpstreamManager)

Controls upstream forwarding behavior:

```rust
UpstreamManagerConfig {
    max_retries: 1,                         // Retry attempts
    retry_delay_ms: 1000,                   // Delay between retries
    circuit_breaker_threshold: 2,           // Errors before opening circuit
    circuit_breaker_timeout_seconds: 60,    // Circuit reset timeout
}
```

**Impact on ProxyEngine:**
- Higher retries = better reliability but slower failure responses
- Circuit breaker prevents cascading failures to unhealthy upstreams

### Validation Configuration (Compile-Time)

Defined in validation middleware and types:

```rust
const MAX_BLOCK_RANGE: u64 = 10000;        // eth_getLogs range limit
const MAX_TOPICS: usize = 4;               // Topic filter limit
```

**Impact on ProxyEngine:**
- Requests exceeding limits rejected before processing
- Prevents DoS via expensive queries

## Dependencies

### Direct Dependencies

**Internal Crates:**
- `crate::cache::CacheManager` - Cache storage and retrieval
- `crate::metrics::MetricsCollector` - Prometheus metrics
- `crate::upstream::manager::UpstreamManager` - Upstream RPC forwarding
- `crate::types::{JsonRpcRequest, JsonRpcResponse, is_method_allowed}` - Type definitions
- `crate::proxy::errors::ProxyError` - Error types
- `crate::proxy::handlers::{LogsHandler, BlocksHandler, TransactionsHandler}` - Specialized processing

**External Crates:**
- `std::sync::Arc` - Thread-safe reference counting for shared ownership
- None directly imported (async runtime provided by handlers)

### Handler Dependencies

Each handler has additional dependencies:

**LogsHandler:**
- `futures_util::stream` - Concurrent range fetching
- `rayon::prelude` - Parallel log conversion for large responses
- `tokio::task::spawn_blocking` - CPU-intensive work offloading

**BlocksHandler:**
- `tracing::debug` - Structured logging
- `hex::decode` - Block hash parsing

**TransactionsHandler:**
- Similar to BlocksHandler

## Thread Safety

### Concurrency Model

The ProxyEngine is fully thread-safe and designed for high-concurrency scenarios:

**Thread-Safe Components:**
- `Arc<CacheManager>`: Interior mutability via RwLock and DashMap
- `Arc<UpstreamManager>`: Interior mutability via RwLock for config, atomic operations for stats
- `Arc<MetricsCollector>`: Lock-free atomic counters and synchronized histograms

**Immutable Components:**
- Handlers (`LogsHandler`, `BlocksHandler`, `TransactionsHandler`): Contain only Arc clones, no mutable state

### Async Concurrency

All public methods are `async` and can safely run concurrently:

```rust
// Safe: Multiple concurrent requests
let (r1, r2, r3) = tokio::join!(
    engine.process_request(req1),
    engine.process_request(req2),
    engine.process_request(req3),
);
```

**Lock Contention:**
- Read-heavy workload benefits from RwLock (multiple concurrent readers)
- Write operations (cache inserts) use efficient concurrent data structures
- No global locks on the hot path for reads

**Async Runtime Compatibility:**
- All blocking operations (rayon parallel processing) offloaded to `spawn_blocking`
- HTTP requests use async HTTP client
- No blocking I/O on tokio runtime

### Memory Safety

**Reference Counting:**
- Arc prevents use-after-free
- Circular references avoided (handlers don't reference engine)
- Automatic cleanup when last Arc dropped

**Data Races:**
- All shared mutable state protected by locks or atomic operations
- No unsafe code in ProxyEngine or handlers
- Rust's type system prevents data races at compile time

## Error Handling

### Error Types

The engine uses the `ProxyError` enum for all error conditions:

```rust
pub enum ProxyError {
    InvalidRequest(String),        // Malformed request
    MethodNotSupported(String),    // Method not in allowed list
    RateLimited,                   // Rate limit exceeded
    Validation(ValidationError),   // Failed validation checks
    Upstream(Box<dyn Error>),      // Upstream RPC error
    Internal(String),              // Internal processing error
}
```

**File Reference:** `crates/prism-core/src/proxy/errors.rs`

### Error Propagation Strategy

**Early Return Pattern:**
- Validation errors return immediately, preventing wasted processing
- Method authorization check before routing
- Parameter validation before handler invocation

**Error Conversion:**
```rust
// Validation errors wrapped
request.validate().map_err(ProxyError::Validation)?;

// Upstream errors boxed
Err(e) => Err(ProxyError::Upstream(e.into()))
```

**Error Context:**
- Validation errors include the invalid value
- Method not supported errors include the method name
- Internal errors include descriptive messages

### Error Recovery

**No Automatic Recovery:**
- Engine does not retry on validation errors (client error)
- Upstream retry logic handled by UpstreamManager
- No fallback responses for invalid requests

**Caller Responsibility:**
- HTTP layer converts ProxyError to appropriate status codes
- Logging and monitoring at HTTP layer
- Client retry logic based on error type

### Error Handling Examples

```rust
match engine.process_request(request).await {
    Ok(response) => {
        // Success path
        if let Some(CacheStatus::Full) = response.cache_status {
            // Served from cache
        }
    }
    Err(ProxyError::Validation(ValidationError::BlockRangeTooLarge(range))) => {
        // Client error: request range too large
        eprintln!("Block range {} exceeds limit", range);
        // Return 400 Bad Request
    }
    Err(ProxyError::MethodNotSupported(method)) => {
        // Client error: unsupported method
        eprintln!("Method {} not supported", method);
        // Return 400 Bad Request
    }
    Err(ProxyError::Upstream(e)) => {
        // Server error: upstream failure
        eprintln!("Upstream error: {}", e);
        // Return 502 Bad Gateway
    }
    Err(ProxyError::Internal(msg)) => {
        // Server error: internal bug
        eprintln!("Internal error: {}", msg);
        // Return 500 Internal Server Error
    }
    Err(ProxyError::RateLimited) => {
        // Client error: too many requests
        eprintln!("Rate limited");
        // Return 429 Too Many Requests
    }
    Err(ProxyError::InvalidRequest(msg)) => {
        // Client error: malformed request
        eprintln!("Invalid request: {}", msg);
        // Return 400 Bad Request
    }
}
```

## Usage Examples

### Basic Setup

```rust
use std::sync::Arc;
use prism_core::{
    cache::{CacheManager, CacheManagerConfig},
    metrics::MetricsCollector,
    upstream::{manager::UpstreamManager, UpstreamConfig},
    proxy::ProxyEngine,
    types::JsonRpcRequest,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize components
    let chain_state = Arc::new(ChainState::new());
    let cache_config = CacheManagerConfig::default();
    let cache = Arc::new(
        CacheManager::new(&cache_config, chain_state.clone())?
    );

    let upstream = Arc::new(
        UpstreamManagerBuilder::new()
            .chain_state(chain_state.clone())
            .build()?
    );

    // Add upstream endpoints
    upstream.add_upstream(UpstreamConfig {
        url: "https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY".to_string(),
        name: "alchemy".to_string(),
        chain_id: 1,
        weight: 100,
        timeout_seconds: 30,
        ..Default::default()
    }).await;

    let metrics = Arc::new(MetricsCollector::new()?);

    // Create proxy engine
    let engine = ProxyEngine::new(cache, upstream, metrics);

    Ok(())
}
```

### Processing Requests

```rust
// Simple block query
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(serde_json::json!(["0x1234567", false])),
    id: serde_json::json!(1),
};

let response = engine.process_request(request).await?;
println!("Block: {:?}", response.result);
println!("Cache status: {:?}", response.cache_status);
```

### Advanced Logs Query with Caching

```rust
// eth_getLogs with filter
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getLogs".to_string(),
    params: Some(serde_json::json!([{
        "fromBlock": "0x1000000",
        "toBlock": "0x1000064",  // 100 blocks
        "address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "topics": [
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
        ]
    }])),
    id: serde_json::json!(42),
};

let response = engine.process_request(request).await?;

match response.cache_status {
    Some(CacheStatus::Full) => {
        println!("All logs served from cache");
    }
    Some(CacheStatus::Partial) => {
        println!("Some logs from cache, some fetched from upstream");
    }
    Some(CacheStatus::Miss) => {
        println!("All logs fetched from upstream (now cached)");
    }
    Some(CacheStatus::Empty) => {
        println!("No logs found (result cached as empty)");
    }
    None => {
        println!("Cache status not available");
    }
}
```

### Concurrent Request Processing

```rust
use tokio::task::JoinSet;

async fn process_batch(
    engine: &ProxyEngine,
    requests: Vec<JsonRpcRequest>
) -> Vec<Result<JsonRpcResponse, ProxyError>> {
    let mut set = JoinSet::new();

    for request in requests {
        let engine_clone = engine.clone(); // Arc clone is cheap
        set.spawn(async move {
            engine_clone.process_request(request).await
        });
    }

    let mut results = Vec::new();
    while let Some(result) = set.join_next().await {
        match result {
            Ok(response) => results.push(response),
            Err(e) => eprintln!("Task error: {}", e),
        }
    }

    results
}
```

### Health Monitoring

```rust
use std::time::Duration;

async fn monitor_health(engine: Arc<ProxyEngine>) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;

        // Cache statistics
        let cache_stats = engine.get_cache_stats().await;
        println!("Cache Stats:");
        println!("  Blocks: {}", cache_stats.block_cache_entries);
        println!("  Transactions: {}", cache_stats.transaction_cache_entries);
        println!("  Logs: {}", cache_stats.logs_cache_entries);

        // Upstream statistics
        let upstream_stats = engine.get_upstream_stats().await;
        println!("Upstream Stats:");
        println!("  Healthy endpoints: {}", upstream_stats.healthy_count);
        println!("  Total requests: {}", upstream_stats.total_requests);
    }
}

// Spawn monitoring task
tokio::spawn(monitor_health(engine.clone()));
```

### Error Handling in HTTP Handler

```rust
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

async fn rpc_handler(
    engine: Arc<ProxyEngine>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    match engine.process_request(request).await {
        Ok(response) => {
            Json(response).into_response()
        }
        Err(ProxyError::Validation(_)) |
        Err(ProxyError::MethodNotSupported(_)) |
        Err(ProxyError::InvalidRequest(_)) => {
            (StatusCode::BAD_REQUEST, "Invalid request").into_response()
        }
        Err(ProxyError::RateLimited) => {
            (StatusCode::TOO_MANY_REQUESTS, "Rate limited").into_response()
        }
        Err(ProxyError::Upstream(_)) => {
            (StatusCode::BAD_GATEWAY, "Upstream error").into_response()
        }
        Err(ProxyError::Internal(msg)) => {
            eprintln!("Internal error: {}", msg);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}
```

### Testing with Mock Data

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache_hit_flow() {
        let chain_state = Arc::new(ChainState::new());
        let cache = Arc::new(
            CacheManager::new(&CacheManagerConfig::default(), chain_state.clone())
                .expect("Failed to create cache manager")
        );
        let upstream = Arc::new(
            UpstreamManagerBuilder::new()
                .chain_state(chain_state)
                .build()
                .expect("Failed to create upstream manager")
        );
        let metrics = Arc::new(MetricsCollector::new().unwrap());

        let engine = ProxyEngine::new(cache.clone(), upstream, metrics);

        // Pre-populate cache via cache manager
        let cache_mgr = engine.get_cache_manager();
        // ... insert test data ...

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "eth_getBlockByNumber".to_string(),
            params: Some(serde_json::json!(["0x1234", false])),
            id: serde_json::json!(1),
        };

        let response = engine.process_request(request).await.unwrap();
        assert_eq!(response.cache_status, Some(CacheStatus::Full));
    }
}
```

## Performance Considerations

### Hot Path Optimization

**Request Validation:**
- Zero-allocation string checking for method names
- Early return on validation failure
- Method allowlist check uses compile-time array

**Cache Lookups:**
- O(1) hash table lookups in DashMap
- No serialization on cache hit
- Arc cloning is cheap (atomic increment)

**Response Construction:**
- Minimal allocations for cache hits
- Reuse of request.id (no clone for cache hits in some handlers)
- Cache status enum is Copy

### Memory Efficiency

**Shared Ownership:**
- Single CacheManager instance shared across all requests
- Single UpstreamManager instance for all upstreams
- Handlers contain only Arc references (24 bytes each)

**Cache Eviction:**
- LRU eviction in block/transaction caches prevents unbounded growth
- Log cache uses circular buffer for fixed memory footprint
- Automatic cleanup of stale entries

### Scalability Limits

**Vertical Scaling (Single Instance):**
- Limited by cache size configuration
- Concurrent request processing limited by tokio thread pool
- Metrics collection uses atomic operations (minimal contention)

**Horizontal Scaling (Multiple Instances):**
- Each instance maintains independent cache
- Upstream manager can share RPC provider endpoints
- No coordination between instances required

**Bottlenecks:**
- Upstream RPC provider rate limits
- Network latency for cache misses
- CPU for large log result sorting in LogsHandler

## Related Components

### Direct Dependencies
- **CacheManager** (`docs/components/cache_manager.md`) - Centralized cache coordination
- **LogsHandler** (`crates/prism-core/src/proxy/handlers/logs.rs`) - Advanced eth_getLogs caching
- **BlocksHandler** (`crates/prism-core/src/proxy/handlers/blocks.rs`) - Block caching logic
- **TransactionsHandler** (`crates/prism-core/src/proxy/handlers/transactions.rs`) - Transaction/receipt caching

### Indirect Dependencies
- **BlockCache** (`docs/components/block_cache.md`) - Block header/body storage
- **TransactionCache** (`docs/components/transaction_cache.md`) - Transaction/receipt storage
- **LogCache** (`docs/components/log_cache.md`) - Event log storage
- **ReorgManager** (`docs/components/reorg_manager.md`) - Chain reorganization handling

### Integration Points
- **HTTP Server** (`crates/server/src/main.rs`) - Wraps engine with HTTP layer
- **Metrics Exporter** - Prometheus /metrics endpoint
- **CLI Tools** (`crates/cli/`) - Engine management and testing

## Version History

**Current Implementation:** As of commit `ce591be` (2025-11-28)

**Recent Changes:**
- Added `net_version` to allowed methods
- Removed request deduplication feature (simplified architecture)
- Arc-based shared context pattern for handlers
- Metrics collection integration

**Future Enhancements:**
See `docs/future_features.md` for planned features.
