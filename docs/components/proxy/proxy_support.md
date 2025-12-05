# Proxy Support Modules

This document describes the error handling, utility functions, and module exports for the proxy subsystem in Prism.

## Overview

The proxy support modules provide three critical pieces of infrastructure:

1. **Error Types** (`errors.rs`) - Structured error handling for all proxy operations
2. **Utility Functions** (`utils.rs`) - Helper functions for data conversion and formatting
3. **Module Exports** (`mod.rs`) - Public API surface and module organization

These modules work together to provide consistent error handling, data transformation, and a clean public interface for the proxy engine.

## File Locations

- `crates/prism-core/src/proxy/errors.rs`
- `crates/prism-core/src/proxy/utils.rs`
- `crates/prism-core/src/proxy/mod.rs`

---

## Error Types

### ProxyError Enum

**Location**: `errors.rs:8-50` (`ProxyError` enum)

The `ProxyError` enum represents all error conditions that can occur during proxy operations. It uses the `thiserror` crate for automatic `Error` trait implementation.

```rust
pub enum ProxyError {
    InvalidRequest(String),
    MethodNotSupported(String),
    RateLimited,
    Validation(ValidationError),
    Upstream(Box<dyn std::error::Error + Send + Sync>),
    Internal(String),
}
```

### Error Variants

#### InvalidRequest

**Purpose**: Indicates malformed or structurally invalid JSON-RPC requests.

**When it occurs**:
- Invalid JSON-RPC structure
- Missing required fields
- Malformed parameters

**Error message format**: `"Invalid request: {details}"`

**Example scenarios**:
- Non-array params when array expected
- Missing `jsonrpc` field
- Invalid parameter types

#### MethodNotSupported

**Purpose**: The requested RPC method is not supported by the proxy.

**When it occurs**:
- Request contains a method not in the `ALLOWED_METHODS` list
- After validation passes but method is not implemented

**Error message format**: `"Method not supported: {method_name}"`

**Example scenarios**:
- Client calls `eth_mining` (not in allowed list)
- Client calls custom or experimental methods

#### RateLimited

**Purpose**: The request has been rejected due to rate limiting.

**When it occurs**:
- IP-based rate limit exceeded
- Too many requests from a single source

**Error message format**: `"Rate limited"`

**Example scenarios**:
- More than allowed requests per second
- Burst limit exceeded

#### Validation

**Purpose**: Request validation failed due to parameter or structure issues.

**When it occurs**:
- After initial parsing but during validation
- Invalid block ranges, topics, or parameters

**Error message format**: `"Validation error: {validation_error}"`

**Contains**: A `ValidationError` enum with specific validation failure details:
- `InvalidVersion(String)` - Wrong JSON-RPC version (not "2.0")
- `InvalidMethod(String)` - Method name contains invalid characters
- `MethodNotAllowed(String)` - Method not in allowed list
- `InvalidBlockParameter(String)` - Invalid block number or tag
- `BlockRangeTooLarge(u64)` - Block range exceeds 10,000 blocks
- `TooManyTopics(usize)` - More than 4 topics specified
- `InvalidBlockHash` - Malformed block hash

**Example scenarios**:
- `eth_getLogs` with 11,000 block range
- Block parameter "invalid_block" instead of "latest" or "0x123"
- 5 topics in log filter (max is 4)

#### Upstream

**Purpose**: Error occurred while communicating with upstream RPC providers.

**When it occurs**:
- HTTP request to upstream fails
- Timeout while waiting for upstream response
- Upstream returns error response
- Network connectivity issues

**Error message format**: `"Upstream error: {underlying_error}"`

**Contains**: Boxed dynamic error (allows any error type with Send + Sync)

**Example scenarios**:
- Upstream server returns 500 error
- Connection timeout after 30 seconds
- Upstream JSON-RPC error response
- DNS resolution failure

#### Internal

**Purpose**: Internal proxy engine error that shouldn't normally occur.

**When it occurs**:
- Unexpected state inconsistencies
- Logic errors in the proxy engine
- Resource exhaustion

**Error message format**: `"Internal error: {details}"`

**Example scenarios**:
- Cache corruption
- Unexpected None value where Some expected
- Invalid internal state transitions

---

## Error Conversion to JSON-RPC

**Note**: Error conversion happens in the HTTP server layer (not in the proxy module itself).

`ProxyError` variants are automatically converted to JSON-RPC error codes when returning responses to clients.

### Error Code Mapping

| ProxyError Variant | JSON-RPC Code | Description |
|-------------------|---------------|-------------|
| `InvalidRequest` | -32600 | Invalid Request |
| `MethodNotSupported` | -32601 | Method not found |
| `RateLimited` | -32603 | Internal error |
| `Validation` | -32603 | Internal error |
| `Upstream` | -32603 | Internal error |
| `Internal` | -32603 | Internal error |

### JSON-RPC Error Response Structure

```json
{
  "jsonrpc": "2.0",
  "result": null,
  "error": {
    "code": -32600,
    "message": "Invalid request: missing params",
    "data": null
  },
  "id": null,
  "cache_status": null
}
```

### HTTP Status Codes

- All errors return HTTP `400 BAD_REQUEST`
- All errors set `x-cache-status: MISS` header

### Standard JSON-RPC Error Codes

For reference, the JSON-RPC 2.0 specification defines:

- `-32700`: Parse error
- `-32600`: Invalid Request
- `-32601`: Method not found
- `-32602`: Invalid params
- `-32603`: Internal error
- `-32000 to -32099`: Server error (reserved for implementation-defined errors)

---

## Utility Functions

### log_filter_to_json_params

**Location**: `utils.rs:26-74` (`log_filter_to_json_params`)

Converts a `LogFilter` struct back to JSON-RPC parameters format for forwarding requests to upstream providers.

**Signature**:
```rust
pub fn log_filter_to_json_params(filter: &LogFilter) -> serde_json::Value
```

**Parameters**:
- `filter: &LogFilter` - Reference to parsed log filter

**Returns**:
- `serde_json::Value` - JSON array containing filter object as first element

**Purpose**:
When cache partially fulfills an `eth_getLogs` request, the proxy needs to forward modified queries to upstream for missing data. This function reconstructs the JSON-RPC params from internal `LogFilter` representation.

**Implementation details**:

1. **Block range** (`utils.rs:29-34`):
   - Converts `from_block` and `to_block` to hex strings
   - Uses `format_hex_u64` for consistent formatting
   - Always includes both fields

2. **Address filtering** (`utils.rs:36-39`):
   - Only includes if `address` is `Some`
   - Uses `format_address` for 20-byte address formatting

3. **Topics array** (`utils.rs:41-63`):
   - Converts `Option<[u8; 32]>` to JSON strings or null
   - Uses `format_hash32` for 32-byte hash formatting
   - Removes trailing null entries for cleaner JSON
   - Only includes topics array if at least one non-null topic exists

4. **Topics anywhere** (`utils.rs:65-71`):
   - Custom extension for flexible topic matching
   - Includes all topics in `topics_anywhere` field
   - Used for advanced filtering scenarios

5. **Return format** (`utils.rs:73`):
   - Returns array with single object: `[{filter_object}]`
   - Matches JSON-RPC parameter format

**Example**:

```rust
let filter = LogFilter {
    from_block: 100,
    to_block: 200,
    address: Some([0x12; 20]),
    topics: [
        Some([0xab; 32]),
        None,
        Some([0xcd; 32]),
        None,
    ],
    topics_anywhere: vec![],
};

let params = log_filter_to_json_params(&filter);
// Returns:
// [{
//   "fromBlock": "0x64",
//   "toBlock": "0xc8",
//   "address": "0x1212121212121212121212121212121212121212",
//   "topics": [
//     "0xabababababababababababababababababababababababababababababababab",
//     null,
//     "0xcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"
//   ]
// }]
```

**Usage context**:
- Called by log cache handlers when partial cache hit occurs
- Used in `eth_getLogs` request processing
- Enables cache range splitting and upstream query generation

**Caveats**:
- Assumes `LogFilter` is already validated
- Does not check for maximum block range
- Trailing nulls in topics are stripped for cleaner JSON
- The `topicsAnywhere` field is non-standard and may not be supported by all upstreams

---

## Module Exports

**Location**: `mod.rs:1-9` (module structure)

### Module Structure

```rust
pub mod engine;
pub mod errors;
pub mod handlers;
pub mod utils;
```

The proxy module is organized into four submodules:
- `engine` - Core `ProxyEngine` implementation
- `errors` - Error types and definitions
- `handlers` - RPC method-specific handlers
- `utils` - Utility functions

### Public Exports

```rust
pub use engine::{CacheStats, ProxyEngine};
pub use errors::ProxyError;
pub use utils::log_filter_to_json_params;
```

### What's Exported

#### ProxyEngine

The main proxy engine type that processes JSON-RPC requests.

**Location**: Re-exported from `engine` module

**Purpose**: Primary interface for request processing, cache coordination, and upstream management.

**Key methods**:
- `process_request(request: JsonRpcRequest) -> Result<JsonRpcResponse, ProxyError>`
- `is_method_supported(method: &str) -> bool`
- `get_cache_stats() -> CacheStats`
- `get_upstream_stats() -> UpstreamStats`

#### CacheStats

Statistics about cache performance and contents.

**Location**: Re-exported from `engine` module

**Purpose**: Provides insight into cache utilization and hit rates.

**Fields**:
- `block_cache_entries: usize`
- `transaction_cache_entries: usize`
- `logs_cache_entries: usize`

#### ProxyError

The error enum described in detail above.

**Location**: Re-exported from `errors` module

**Purpose**: Unified error type for all proxy operations.

#### log_filter_to_json_params

The utility function described in detail above.

**Location**: Re-exported from `utils` module

**Purpose**: Convert internal filter representation to JSON-RPC params.

### What's NOT Exported

The following remain private to the proxy module:
- Individual handler implementations
- Internal engine state structures
- Helper functions not needed by external consumers
- Test utilities and fixtures

---

## Usage Patterns

### Error Handling Pattern

The proxy uses `Result<JsonRpcResponse, ProxyError>` throughout:

```rust
// In handlers
pub async fn handle_request(request: JsonRpcRequest) -> Result<JsonRpcResponse, ProxyError> {
    // Validation
    request.validate()
        .map_err(ProxyError::Validation)?;

    // Check method support
    if !ProxyEngine::is_method_supported(&request.method) {
        return Err(ProxyError::MethodNotSupported(request.method.clone()));
    }

    // Call upstream
    let response = upstream_call(&request)
        .await
        .map_err(|e| ProxyError::Upstream(Box::new(e)))?;

    Ok(response)
}
```

### Error Conversion Pattern

Errors are converted to JSON-RPC responses in the router:

```rust
// In router/mod.rs
match proxy_engine.process_request(request).await {
    Ok(response) => {
        // Return success response
        (StatusCode::OK, Json(response))
    }
    Err(e) => {
        // Convert ProxyError to JSON-RPC error
        let error_response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code: match e {
                    ProxyError::InvalidRequest(_) => -32600,
                    ProxyError::MethodNotSupported(_) => -32601,
                    _ => -32603,
                },
                message: e.to_string(),
                data: None,
            }),
            id: serde_json::Value::Null,
            cache_status: None,
        };
        (StatusCode::BAD_REQUEST, Json(error_response))
    }
}
```

### Validation Error Wrapping

Validation errors are wrapped in `ProxyError::Validation`:

```rust
// Validation returns ValidationError
let validation_result = request.validate();

// Wrap in ProxyError for consistent handling
validation_result.map_err(ProxyError::Validation)?;
```

### Upstream Error Boxing

Upstream errors use dynamic boxing for flexibility:

```rust
// Any error type can be converted
upstream_manager
    .call_upstream(&request)
    .await
    .map_err(|e| ProxyError::Upstream(Box::new(e)))?;
```

### Utility Function Usage

When splitting cache ranges and generating upstream requests:

```rust
// After cache partial hit, need to fetch missing data
let missing_filter = LogFilter {
    from_block: cached_range.to + 1,
    to_block: original_filter.to_block,
    address: original_filter.address,
    topics: original_filter.topics.clone(),
    topics_anywhere: vec![],
};

// Convert back to JSON params for upstream
let upstream_params = log_filter_to_json_params(&missing_filter);

let upstream_request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getLogs".to_string(),
    params: Some(upstream_params),
    id: original_request.id.clone(),
};
```

---

## Integration with Other Components

### ProxyEngine

The main consumer of these support modules:
- Uses `ProxyError` for all error returns
- Calls `log_filter_to_json_params` when generating upstream queries
- Exports types and functions through `mod.rs`

### Router (HTTP Layer)

Converts proxy errors to HTTP responses:
- Maps `ProxyError` to JSON-RPC error codes
- Returns appropriate HTTP status codes
- Sets cache status headers

### Validation Middleware

Source of `ValidationError`:
- Validates requests before proxy processing
- Errors get wrapped in `ProxyError::Validation`
- Prevents invalid requests from reaching proxy engine

### Cache Handlers

Primary users of utility functions:
- Log cache uses `log_filter_to_json_params` for range splitting
- Handlers return `ProxyError` variants
- Coordinate with upstream through consistent error handling

---

## Testing

### Test Coverage

**Note**: Tests are located in the proxy module's test suite, not in the support files themselves.

The module includes comprehensive tests for:

1. **Request validation**
   - Valid requests pass validation
   - Invalid JSON-RPC versions are rejected

2. **Method support**
   - Supported methods return true
   - Unsupported methods return false
   - Includes recent additions like `net_version`

3. **Proxy engine integration**
   - Engine processes requests correctly
   - Handles errors appropriately

4. **Validation edge cases**
   - Valid requests with all field types
   - Invalid version strings
   - Unsupported methods
   - Log filters with valid ranges
   - Log filters exceeding 10,000 block range
   - Too many topics (>4)
   - Invalid block parameters
   - Valid block tags ("latest", "earliest", "pending", "safe", "finalized")

### Test Assertions

Tests verify:
- Error variant types match expected errors
- Error messages contain relevant information
- Valid inputs are accepted
- Edge cases are handled correctly

### Test Attributes

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests { ... }
```

Tests allow `unwrap` and `expect` for cleaner test code, though production code forbids these.

---

## Dependencies

### External Crates

- `thiserror` - Error derive macros for `ProxyError`
- `serde_json` - JSON value manipulation in utilities

### Internal Modules

- `crate::cache::types::LogFilter` - Filter type for logs
- `crate::utils::hex_buffer` - Hex formatting functions
- `crate::middleware::validation::ValidationError` - Validation errors
- `crate::types` - Core types like `JsonRpcRequest`, `JsonRpcResponse`

---

## Future Considerations

### Potential Enhancements

1. **Structured error data**
   - Add `data` field to `ProxyError` variants
   - Provide detailed context for debugging
   - Include request IDs for tracing

2. **More specific error codes**
   - Use -32000 to -32099 range for custom errors
   - Map each `ProxyError` variant to specific code
   - Improve client error handling

3. **Error metrics**
   - Track error rates by type
   - Monitor validation failure patterns
   - Alert on error rate spikes

4. **Additional utilities**
   - Block parameter conversion functions
   - Transaction parameter formatting
   - Request cloning optimizations

5. **Error recovery**
   - Automatic retry logic for certain errors
   - Graceful degradation patterns
   - Circuit breaker integration

---

## Summary

The proxy support modules provide essential infrastructure for error handling and data conversion:

- **Comprehensive error types** covering all failure modes
- **Standard JSON-RPC error conversion** for client compatibility
- **Utility functions** for cache-to-upstream data transformation
- **Clean module organization** with minimal public surface
- **Extensive test coverage** ensuring reliability

These modules enable the proxy engine to handle errors consistently, communicate effectively with clients using JSON-RPC standards, and efficiently coordinate between cache and upstream providers.
