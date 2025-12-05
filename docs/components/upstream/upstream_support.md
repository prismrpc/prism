# Upstream Support Components

## Overview

The upstream support modules provide resilient error handling and fault tolerance for RPC provider interactions. This documentation covers three critical components:

- **Circuit Breaker** (`circuit_breaker.rs`): Implements the circuit breaker pattern to prevent cascading failures
- **Error Types** (`errors.rs`): Comprehensive error taxonomy for upstream operations
- **Module Interface** (`mod.rs`): Public API exports for upstream functionality

### Purpose

The circuit breaker pattern protects the system from repeatedly attempting operations that are likely to fail, allowing failed services time to recover. Combined with detailed error types, this creates a robust foundation for managing multiple upstream RPC providers with different failure modes.

## Circuit Breaker

**File**: `crates/prism-core/src/upstream/circuit_breaker.rs`

### Circuit Breaker States

The circuit breaker operates in three distinct states with automatic transitions:

#### 1. Closed State (Normal Operation)
- **Behavior**: All requests are allowed to pass through
- **Failure Tracking**: Counts consecutive failures
- **Transition**: Moves to Open state when failure count reaches threshold
- **Location**: Lines 14-15, 60-62

In the Closed state, the circuit breaker operates transparently, allowing all requests while monitoring for failures. Successful requests reset the failure counter to zero.

#### 2. Open State (Failing Fast)
- **Behavior**: All requests are immediately rejected without attempting execution
- **Purpose**: Prevents overwhelming a failing service with additional requests
- **Recovery**: After timeout duration elapses, transitions to Half-Open state
- **Transition Logic**: Lines 37-52
- **Logging**: Warns when circuit opens (Line 83)

When a circuit opens, it records the timestamp of the last failure. Subsequent requests check if enough time has elapsed for recovery. This "fast fail" behavior prevents resource exhaustion and cascading failures.

#### 3. Half-Open State (Testing Recovery)
- **Behavior**: Allows limited requests to test if the service has recovered
- **Success**: Single successful request closes the circuit
- **Failure**: Any failure immediately reopens the circuit
- **Transition**: Lines 43-44, 64-68

The Half-Open state acts as a trial period. If the service has recovered, the first successful request will close the circuit and resume normal operation. If the service is still failing, the circuit reopens immediately.

### State Transition Diagram

```
        Threshold Reached
    Closed ──────────────────> Open
      ^                          │
      │                          │ Timeout Elapsed
      │                          ▼
      └────────────────────── HalfOpen
         Success in HalfOpen
```

### Circuit Breaker Configuration

The `CircuitBreaker` is configured with two key parameters:

```rust
pub fn new(threshold: u32, timeout_seconds: u64) -> Self
```
**Location**: Lines 22-30

#### Configuration Parameters

**Threshold** (`u32`)
- Number of consecutive failures before circuit opens
- Must be reached or exceeded to trigger state change
- Example: `threshold = 3` means circuit opens on 3rd failure
- Checked at: Line 80

**Timeout** (`Duration`)
- How long to wait before attempting recovery
- Converted from seconds to `Duration`
- After timeout, circuit transitions from Open to Half-Open
- Used at: Line 40

#### Configuration Recommendations

- **Low-latency services**: Lower threshold (3-5) with shorter timeout (30-60s)
- **High-latency services**: Higher threshold (10-15) with longer timeout (120-300s)
- **Critical services**: Lower threshold to fail fast
- **Best-effort services**: Higher threshold to tolerate transient issues

### Public API

#### Constructor

```rust
pub fn new(threshold: u32, timeout_seconds: u64) -> Self
```
**Location**: Lines 22-30

Creates a new circuit breaker instance with specified threshold and timeout.

**Parameters**:
- `threshold`: Number of failures before opening circuit
- `timeout_seconds`: Seconds to wait before testing recovery

**Returns**: Configured `CircuitBreaker` instance

**Initial State**: Closed with zero failure count

#### Request Permission Check

```rust
pub async fn can_execute(&self) -> bool
```
**Location**: Lines 33-54

Determines whether a request should be allowed based on current circuit state.

**Returns**:
- `true`: Request is allowed (Closed or Half-Open state)
- `false`: Request is rejected (Open state, timeout not elapsed)

**Side Effects**: May transition Open → Half-Open if timeout has elapsed

**State Handling**:
- **Closed/Half-Open**: Always returns `true` (Lines 36)
- **Open**: Checks timeout and possibly transitions to Half-Open (Lines 37-52)

**Usage Pattern**:
```rust
if circuit_breaker.can_execute().await {
    // Attempt request
    match execute_request().await {
        Ok(result) => {
            circuit_breaker.on_success().await;
            Ok(result)
        }
        Err(e) => {
            circuit_breaker.on_failure().await;
            Err(e)
        }
    }
} else {
    // Fast fail - return CircuitBreakerOpen error
    Err(UpstreamError::CircuitBreakerOpen)
}
```

#### Success Callback

```rust
pub async fn on_success(&self)
```
**Location**: Lines 57-70

Records a successful request execution and updates circuit state.

**State Transitions**:
- **Closed**: Resets failure counter to zero
- **Half-Open**: Closes circuit and resets counter
- **Open**: Closes circuit and resets counter

**Critical Behavior**: A single success from Half-Open or Open state immediately closes the circuit, restoring normal operation.

#### Failure Callback

```rust
pub async fn on_failure(&self)
```
**Location**: Lines 73-85

Records a failed request execution and potentially opens the circuit.

**Actions**:
1. Increments failure counter (Line 75)
2. Records timestamp of failure (Line 78)
3. Opens circuit if threshold reached (Lines 80-84)

**Logging**: Warns when circuit opens with failure count (Line 83)

**Threshold Behavior**: Circuit opens when `failure_count >= threshold` (Line 80)

#### State Inspection

```rust
pub async fn get_state(&self) -> CircuitBreakerState
```
**Location**: Lines 88-90

Returns current circuit breaker state for monitoring and debugging.

**Returns**: Clone of current `CircuitBreakerState` enum

**Use Cases**:
- Health check endpoints
- Metrics collection
- Administrative dashboards
- Testing and debugging

```rust
pub async fn get_failure_count(&self) -> u32
```
**Location**: Lines 93-95

Returns current consecutive failure count.

**Returns**: Number of failures since last success or circuit open

**Use Cases**:
- Monitoring how close to threshold
- Metrics and alerting
- Debugging connection issues

### Thread Safety

The circuit breaker uses `Arc<RwLock<T>>` for all mutable state, making it safe to share across multiple async tasks:

- **failure_count**: `Arc<RwLock<u32>>` (Line 6)
- **last_failure_time**: `Arc<RwLock<Option<Instant>>>` (Line 7)
- **state**: `Arc<RwLock<CircuitBreakerState>>` (Line 10)

Multiple concurrent requests can safely interact with the same circuit breaker instance.

### Implementation Details

**Locking Strategy**: The implementation carefully manages read/write locks to avoid deadlocks. Note the explicit `drop(state)` at Line 41 before acquiring a write lock, preventing deadlock when transitioning from Open to Half-Open.

**Time Source**: Uses `std::time::Instant` for monotonic time measurements, immune to system clock adjustments (Line 40, 78).

**Async-First Design**: All methods are async to integrate seamlessly with tokio-based upstream operations.

## Error Types

**File**: `crates/prism-core/src/upstream/errors.rs`

The `UpstreamError` enum provides a comprehensive taxonomy of failures that can occur when communicating with RPC providers.

### Error Variants

#### Timeout
```rust
#[error("Request timeout")]
Timeout
```
**Location**: Lines 5-6

**When It Occurs**:
- Request exceeds configured timeout duration
- Network latency too high
- Upstream provider not responding

**Typical Causes**:
- Overloaded RPC provider
- Network congestion
- Provider rate limiting (silent drops)

**Handling**: Should trigger circuit breaker failure. Consider retry with exponential backoff.

#### Connection Failed
```rust
#[error("Connection failed: {0}")]
ConnectionFailed(String)
```
**Location**: Lines 7-8

**When It Occurs**:
- Cannot establish TCP connection
- DNS resolution failure
- TLS handshake failure

**Message Contains**: Detailed error description from connection attempt

**Typical Causes**:
- Provider endpoint down
- Network connectivity issues
- Firewall blocking connection
- Invalid endpoint URL

**Handling**: Should trigger circuit breaker failure. Critical for health check failures.

#### HTTP Error
```rust
#[error("HTTP error: {0}")]
HttpError(u16, String)
```
**Location**: Lines 9-10

**When It Occurs**:
- Upstream returns non-200 HTTP status code

**Fields**:
- `u16`: HTTP status code (404, 500, 503, etc.)
- `String`: Response body or error message

**Common Status Codes**:
- `429`: Rate limiting
- `500`: Internal server error
- `502`: Bad gateway (proxy failure)
- `503`: Service unavailable
- `504`: Gateway timeout

**Handling**:
- 429: Implement backoff, don't count as circuit breaker failure
- 5xx: Count as failure, may indicate provider issues

#### RPC Error
```rust
#[error("RPC error: {0}")]
RpcError(i32, String)
```
**Location**: Lines 11-12

**When It Occurs**:
- JSON-RPC error response received
- Upstream processed request but returned error

**Fields**:
- `i32`: JSON-RPC error code
- `String`: Error message from provider

**Common Error Codes**:
- `-32700`: Parse error
- `-32600`: Invalid request
- `-32601`: Method not found
- `-32602`: Invalid params
- `-32603`: Internal error

**Handling**:
- Parse errors (-32700): May indicate serialization issues
- Invalid request (-32600, -32601, -32602): Client error, don't fail circuit
- Internal errors (-32603): Provider issue, count as failure

#### Network Error
```rust
#[error("Network error: {0}")]
Network(#[from] reqwest::Error)
```
**Location**: Lines 13-14

**When It Occurs**:
- Low-level network error from reqwest library
- Automatically converted from `reqwest::Error`

**Typical Causes**:
- Connection reset by peer
- Protocol errors
- Certificate validation failure
- Redirect issues

**Auto-Conversion**: The `#[from]` attribute enables automatic conversion:
```rust
// Automatically converts reqwest::Error to UpstreamError::Network
let result: Result<Response, UpstreamError> = http_client.get(url).await?;
```

**Handling**: Should trigger circuit breaker failure. Indicates network instability.

#### Invalid Response
```rust
#[error("Invalid response: {0}")]
InvalidResponse(String)
```
**Location**: Lines 15-16

**When It Occurs**:
- Response received but cannot be parsed
- JSON deserialization failure
- Unexpected response format
- Missing required fields

**Message Contains**: Description of what was invalid

**Typical Causes**:
- Provider returning HTML instead of JSON
- Malformed JSON-RPC response
- Schema mismatch between expected and actual response
- Provider API version mismatch

**Handling**: May indicate provider misconfiguration. Log for investigation, potentially count as failure.

#### No Healthy Upstreams
```rust
#[error("No healthy upstreams available")]
NoHealthyUpstreams
```
**Location**: Lines 17-18

**When It Occurs**:
- All configured upstream providers are unhealthy
- All circuit breakers are open
- Load balancer cannot find available endpoint

**Critical Error**: This represents total system failure for upstream communication

**Typical Causes**:
- Widespread provider outage
- Network partition
- All providers rate limiting
- Configuration error (no providers configured)

**Handling**:
- Return 503 Service Unavailable to clients
- Alert operations team
- Consider fallback strategies (cached responses, degraded mode)

#### Circuit Breaker Open
```rust
#[error("Circuit breaker is open")]
CircuitBreakerOpen
```
**Location**: Lines 19-20

**When It Occurs**:
- Request attempted while circuit breaker is in Open state
- Fast fail to protect failing service

**Not a Real Failure**: This error prevents attempting doomed requests

**Handling**:
- Return 503 Service Unavailable
- Don't retry immediately
- Monitor circuit breaker state for recovery
- Consider routing to different provider

#### Invalid Request
```rust
#[error("Invalid request: {0}")]
InvalidRequest(String)
```
**Location**: Lines 21-22

**When It Occurs**:
- Request validation failed before sending to upstream
- Malformed JSON-RPC request
- Unsupported method
- Invalid parameters

**Message Contains**: Validation error details

**Client Error**: This is a 400-class error, not a provider failure

**Handling**:
- Return 400 Bad Request to client
- Don't count against circuit breaker
- Log for potential abuse detection

#### Concurrency Limit
```rust
#[error("Concurrency limit reached: {0}")]
ConcurrencyLimit(String)
```
**Location**: Lines 23-24

**When It Occurs**:
- Maximum concurrent requests to provider reached
- Semaphore or connection pool exhausted

**Message Contains**: Details about limit reached

**Protective Error**: Prevents overwhelming upstream or exceeding rate limits

**Handling**:
- Queue request for retry
- Return 429 Too Many Requests or 503 Service Unavailable
- Don't count against circuit breaker
- Consider scaling connection pool

### Error Derivation

Uses `thiserror::Error` for automatic implementations (Line 1, 3):
- `Display` trait using `#[error("...")]` messages
- `Error` trait with proper source chain
- `From` conversions via `#[from]` attribute

### Error Handling Patterns

#### Pattern 1: Circuit Breaker Integration
```rust
match upstream_call().await {
    Ok(response) => {
        circuit_breaker.on_success().await;
        Ok(response)
    }
    Err(e) => {
        match e {
            UpstreamError::CircuitBreakerOpen => {
                // Already rejected, don't call on_failure
                Err(e)
            }
            UpstreamError::InvalidRequest(_) => {
                // Client error, don't penalize circuit breaker
                Err(e)
            }
            _ => {
                // Real failure, record it
                circuit_breaker.on_failure().await;
                Err(e)
            }
        }
    }
}
```

#### Pattern 2: Fallback Chain
```rust
match primary_upstream().await {
    Ok(result) => Ok(result),
    Err(UpstreamError::Timeout | UpstreamError::CircuitBreakerOpen) => {
        // Try secondary upstream
        secondary_upstream().await
    }
    Err(e) => Err(e),
}
```

#### Pattern 3: Error Classification
```rust
fn is_retryable(error: &UpstreamError) -> bool {
    matches!(
        error,
        UpstreamError::Timeout
        | UpstreamError::Network(_)
        | UpstreamError::HttpError(502 | 503 | 504, _)
    )
}

fn is_client_error(error: &UpstreamError) -> bool {
    matches!(
        error,
        UpstreamError::InvalidRequest(_)
        | UpstreamError::RpcError(-32600..=-32602, _)
    )
}
```

### Error to HTTP Status Mapping

Recommended HTTP status codes for each error type:

| Error Variant | HTTP Status | Reason |
|---------------|-------------|--------|
| `Timeout` | 504 Gateway Timeout | Upstream didn't respond in time |
| `ConnectionFailed` | 502 Bad Gateway | Cannot reach upstream |
| `HttpError(4xx)` | Same 4xx | Pass through client errors |
| `HttpError(5xx)` | Same 5xx | Pass through server errors |
| `RpcError(-32600..-32602)` | 400 Bad Request | Client error in RPC |
| `RpcError(other)` | 500 Internal Server Error | Upstream processing error |
| `Network` | 502 Bad Gateway | Network-level failure |
| `InvalidResponse` | 502 Bad Gateway | Upstream returned garbage |
| `NoHealthyUpstreams` | 503 Service Unavailable | No providers available |
| `CircuitBreakerOpen` | 503 Service Unavailable | Provider temporarily unavailable |
| `InvalidRequest` | 400 Bad Request | Client sent invalid request |
| `ConcurrencyLimit` | 429 Too Many Requests | Rate limiting |

## Module Interface

**File**: `crates/prism-core/src/upstream/mod.rs`

### Module Structure

The upstream module is organized into focused submodules:

- **`circuit_breaker`**: Fault tolerance and failure handling (Line 1)
- **`endpoint`**: Upstream endpoint configuration (Line 2)
- **`errors`**: Error type definitions (Line 3)
- **`health`**: Health checking and monitoring (Line 4)
- **`http_client`**: HTTP client abstraction (Line 5)
- **`load_balancer`**: Request distribution strategies (Line 6)
- **`manager`**: Upstream lifecycle management (Line 7)
- **`websocket`**: WebSocket connection handling (Line 8)

### Public Exports

The module re-exports key types for convenient access:

#### From `circuit_breaker`
```rust
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerState};
```
**Location**: Line 10

**Exports**:
- `CircuitBreaker`: Main circuit breaker implementation
- `CircuitBreakerState`: State enum (Closed, Open, HalfOpen)

**Usage**:
```rust
use prism_core::upstream::{CircuitBreaker, CircuitBreakerState};

let breaker = CircuitBreaker::new(5, 60);
let state = breaker.get_state().await;
assert_eq!(state, CircuitBreakerState::Closed);
```

#### From `endpoint`
```rust
pub use endpoint::UpstreamEndpoint;
```
**Location**: Line 11

**Exports**:
- `UpstreamEndpoint`: Configuration for individual RPC provider

#### From `errors`
```rust
pub use errors::UpstreamError;
```
**Location**: Line 12

**Exports**:
- `UpstreamError`: Comprehensive error enum for upstream operations

**Usage**:
```rust
use prism_core::upstream::UpstreamError;

fn handle_upstream_error(err: UpstreamError) {
    match err {
        UpstreamError::CircuitBreakerOpen => {
            // Handle fast fail
        }
        UpstreamError::NoHealthyUpstreams => {
            // All providers down
        }
        _ => {
            // Other errors
        }
    }
}
```

#### From `http_client`
```rust
pub use http_client::{HttpClient, HttpClientConfig};
```
**Location**: Line 13

**Exports**:
- `HttpClient`: HTTP client wrapper for RPC calls
- `HttpClientConfig`: Configuration for HTTP client (timeouts, retries, etc.)

#### From `load_balancer`
```rust
pub use load_balancer::{LoadBalancer, LoadBalancerConfig, LoadBalancerStats};
```
**Location**: Line 14

**Exports**:
- `LoadBalancer`: Request distribution logic
- `LoadBalancerConfig`: Load balancing strategy configuration
- `LoadBalancerStats`: Statistics for load balancer performance

#### From `manager`
```rust
pub use manager::{UpstreamManager, UpstreamManagerConfig};
```
**Location**: Line 15

**Exports**:
- `UpstreamManager`: Central coordinator for all upstream operations
- `UpstreamManagerConfig`: Manager configuration

**Usage Pattern**:
```rust
use prism_core::upstream::{UpstreamManager, UpstreamManagerConfig};

let config = UpstreamManagerConfig::from_env();
let manager = UpstreamManager::new(config).await?;
let response = manager.execute_request(json_rpc_request).await?;
```

#### From `websocket`
```rust
pub use websocket::{WebSocketFailureTracker, WebSocketHandler};
```
**Location**: Line 16

**Exports**:
- `WebSocketFailureTracker`: Tracks WebSocket connection failures
- `WebSocketHandler`: Manages WebSocket connections to upstreams

### Import Patterns

**Full Path Import**:
```rust
use prism_core::upstream::errors::UpstreamError;
use prism_core::upstream::circuit_breaker::{CircuitBreaker, CircuitBreakerState};
```

**Grouped Import** (Recommended):
```rust
use prism_core::upstream::{
    CircuitBreaker,
    CircuitBreakerState,
    UpstreamError,
    UpstreamManager,
};
```

**Wildcard Import** (Use Sparingly):
```rust
use prism_core::upstream::*;
```

## Usage Examples

### Basic Circuit Breaker Usage

```rust
use prism_core::upstream::{CircuitBreaker, CircuitBreakerState, UpstreamError};

// Create circuit breaker: open after 3 failures, retry after 60 seconds
let circuit_breaker = CircuitBreaker::new(3, 60);

// Before each request, check if allowed
if !circuit_breaker.can_execute().await {
    return Err(UpstreamError::CircuitBreakerOpen);
}

// Attempt the request
match make_rpc_call().await {
    Ok(response) => {
        circuit_breaker.on_success().await;
        Ok(response)
    }
    Err(err) => {
        circuit_breaker.on_failure().await;
        Err(err)
    }
}
```

### Monitoring Circuit Breaker State

```rust
use prism_core::upstream::{CircuitBreaker, CircuitBreakerState};

async fn report_circuit_health(breaker: &CircuitBreaker) {
    let state = breaker.get_state().await;
    let failures = breaker.get_failure_count().await;

    match state {
        CircuitBreakerState::Closed => {
            println!("Circuit healthy - {} failures tracked", failures);
        }
        CircuitBreakerState::HalfOpen => {
            println!("Circuit testing recovery - {} failures before open", failures);
        }
        CircuitBreakerState::Open => {
            println!("Circuit OPEN - {} failures, waiting for timeout", failures);
        }
    }
}
```

### Advanced Error Handling

```rust
use prism_core::upstream::UpstreamError;

async fn handle_request_with_fallback(
    primary: &UpstreamEndpoint,
    fallback: &UpstreamEndpoint,
) -> Result<Response, UpstreamError> {
    match primary.execute().await {
        Ok(resp) => Ok(resp),
        Err(UpstreamError::Timeout | UpstreamError::CircuitBreakerOpen) => {
            tracing::warn!("Primary failed, trying fallback");
            fallback.execute().await
        }
        Err(UpstreamError::InvalidRequest(msg)) => {
            // Client error, don't fallback
            tracing::error!("Invalid request: {}", msg);
            Err(UpstreamError::InvalidRequest(msg))
        }
        Err(e) => {
            tracing::error!("Primary error: {:?}, trying fallback", e);
            fallback.execute().await
        }
    }
}
```

### Categorizing Errors for Metrics

```rust
use prism_core::upstream::UpstreamError;

fn categorize_error(err: &UpstreamError) -> &'static str {
    match err {
        UpstreamError::Timeout => "timeout",
        UpstreamError::ConnectionFailed(_) => "connection_failed",
        UpstreamError::HttpError(status, _) if *status >= 400 && *status < 500 => "client_error",
        UpstreamError::HttpError(status, _) if *status >= 500 => "server_error",
        UpstreamError::RpcError(code, _) if *code >= -32699 && *code <= -32600 => "rpc_client_error",
        UpstreamError::RpcError(_, _) => "rpc_server_error",
        UpstreamError::Network(_) => "network_error",
        UpstreamError::InvalidResponse(_) => "invalid_response",
        UpstreamError::NoHealthyUpstreams => "no_upstreams",
        UpstreamError::CircuitBreakerOpen => "circuit_open",
        UpstreamError::InvalidRequest(_) => "invalid_request",
        UpstreamError::ConcurrencyLimit(_) => "concurrency_limit",
    }
}

// Use in metrics
metrics::counter!(
    "upstream_errors_total",
    "error_type" => categorize_error(&error),
).increment(1);
```

## Performance Considerations

### Circuit Breaker Overhead

**Lock Contention**: The circuit breaker uses `RwLock` for state management. Under high concurrency:
- `can_execute()`: Acquires read lock (low contention)
- `on_success()/on_failure()`: Acquire write locks (potential contention)

**Mitigation**: Circuit breaker checks are fast (microseconds) and rarely block. The async nature prevents thread blocking.

### Memory Usage

Each `CircuitBreaker` instance consumes minimal memory:
- State: 1 byte (enum) + Arc overhead
- Failure count: 4 bytes (u32) + Arc overhead
- Last failure time: 16 bytes (Option<Instant>) + Arc overhead
- Configuration: 4 bytes (u32) + 8 bytes (Duration)

**Total**: ~50-100 bytes per circuit breaker instance

### Error Allocation

`UpstreamError` variants containing `String` allocate heap memory:
- `ConnectionFailed(String)`
- `HttpError(u16, String)`
- `RpcError(i32, String)`
- `InvalidResponse(String)`
- `InvalidRequest(String)`
- `ConcurrencyLimit(String)`

**Recommendation**: Keep error messages concise to minimize allocations in hot paths.

## Testing Support

### Test Utilities

The circuit breaker includes comprehensive tests demonstrating key behaviors:

**Basic State Transitions** (Lines 104-120):
- Initial state is Closed
- Opens after threshold failures
- Success closes the circuit

**Half-Open Recovery** (Lines 123-137):
- Timeout triggers Half-Open state
- Success from Half-Open closes circuit

**Threshold Behavior** (Lines 140-152):
- Circuit stays closed below threshold
- Opens exactly at threshold

### Integration Testing

When testing components using circuit breakers:

```rust
#[tokio::test]
async fn test_circuit_breaker_integration() {
    // Use low threshold and timeout for tests
    let breaker = CircuitBreaker::new(2, 1);

    // Simulate failures
    breaker.on_failure().await;
    breaker.on_failure().await;
    assert_eq!(breaker.get_state().await, CircuitBreakerState::Open);

    // Wait for recovery window
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Should allow test request
    assert!(breaker.can_execute().await);
    assert_eq!(breaker.get_state().await, CircuitBreakerState::HalfOpen);
}
```

## Caveats and Gotchas

### Circuit Breaker Caveats

1. **No Automatic Reset in Closed State**: Failure count persists in Closed state until a success occurs. A service that occasionally fails but mostly succeeds will accumulate failures over time.

2. **Single Success Closes**: From Half-Open state, a single success immediately closes the circuit. If the service is still unstable, it may oscillate between states.

3. **No Rate Limiting**: The circuit breaker doesn't limit request rate in Half-Open state. A burst of requests could all attempt execution simultaneously.

4. **Shared State**: Multiple concurrent failures can race to increment the counter, but the atomic write lock ensures consistency.

### Error Handling Caveats

1. **Error Message Allocation**: String-containing errors allocate on every creation. Consider static messages for common cases.

2. **No Error Recovery Info**: Errors don't carry retry delay suggestions or recovery hints.

3. **Loss of Detail**: Converting from `reqwest::Error` to `Network` may lose some diagnostic information.

4. **No Error Codes**: HTTP and RPC errors include codes, but other variants don't have machine-readable identifiers.

## Integration with Other Components

### With `UpstreamManager`

The `UpstreamManager` coordinates circuit breakers across multiple endpoints, using them to track per-provider health.

### With `LoadBalancer`

The load balancer consults circuit breaker state when selecting endpoints, excluding those with open circuits.

### With `HealthCheck`

Health check failures trigger circuit breaker failures, allowing passive health detection based on request success/failure rates.

### With Metrics

Circuit breaker state changes and error occurrences should be exported as metrics for monitoring:
- `circuit_breaker_state{endpoint}`: Current state (0=Closed, 1=Open, 2=HalfOpen)
- `circuit_breaker_failures{endpoint}`: Current failure count
- `upstream_errors_total{error_type}`: Error counts by type

## Summary

The upstream support components provide production-ready fault tolerance:

1. **Circuit Breaker** protects against cascading failures with three-state pattern
2. **Error Types** enable precise error handling and classification
3. **Module Interface** exposes clean, focused public API

Key files and locations:
- Circuit breaker implementation: `crates/prism-core/src/upstream/circuit_breaker.rs` (Lines 1-154)
- Error definitions: `crates/prism-core/src/upstream/errors.rs` (Lines 1-26)
- Public API exports: `crates/prism-core/src/upstream/mod.rs` (Lines 1-17)

These components form the reliability foundation for the entire upstream provider interaction system.
