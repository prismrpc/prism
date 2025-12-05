# Middleware Validation Component Documentation

## Overview

The Middleware Validation module provides comprehensive request validation and security controls for the Prism RPC aggregator. It operates at two levels: HTTP middleware for basic content-type validation, and JSON-RPC request validation for protocol compliance and security checks. The validation layer acts as the first line of defense against malformed requests, injection attacks, and resource exhaustion attacks.

**Location:** `crates/prism-core/src/middleware/validation.rs`

**Key Responsibilities:**
- HTTP header validation (Content-Type enforcement)
- JSON-RPC 2.0 protocol compliance verification
- Method name sanitization and allowlist enforcement
- Method-specific parameter validation
- Block range limits to prevent resource exhaustion
- Topic filter validation for log queries
- Block parameter format validation

## Architecture

### Validation Layers

```
HTTP Request
    ↓
┌────────────────────────────────┐
│ validate_request_middleware    │  ← HTTP Layer
├────────────────────────────────┤
│ 1. Content-Type check          │  ← Must be application/json
└────────┬───────────────────────┘
         ↓
┌────────────────────────────────┐
│ JsonRpcRequest::validate()     │  ← JSON-RPC Layer
├────────────────────────────────┤
│ 1. Version check (2.0)         │
│ 2. Method name sanitization    │
│ 3. Method allowlist check      │
│ 4. Method-specific validation  │
└────────┬───────────────────────┘
         ↓
    ProxyEngine
```

### Design Philosophy

The validation module follows a **defense-in-depth** strategy:

1. **Layer Separation**: HTTP concerns separated from JSON-RPC concerns
2. **Early Rejection**: Invalid requests rejected before reaching business logic
3. **Fail-Fast**: First validation error immediately returns, no further processing
4. **Clear Error Messages**: Each error type includes context for debugging
5. **Zero-Trust**: All input treated as potentially malicious until validated

This architecture prevents resource waste on invalid requests and provides clear error boundaries for debugging and monitoring.

## Public API

### HTTP Middleware

#### `validate_request_middleware`

```rust
pub async fn validate_request_middleware(
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode>
```

**Purpose:** Axum middleware that validates HTTP headers before request processing.

**Parameters:**
- `request`: Incoming HTTP request with body
- `next`: Next middleware or handler in the chain

**Returns:**
- `Ok(Response)`: Request passed validation, continues to next middleware
- `Err(StatusCode::UNSUPPORTED_MEDIA_TYPE)`: Content-Type validation failed

**Behavior:**
1. Extracts `Content-Type` header from request
2. Checks if header is present (required)
3. Validates header starts with `"application/json"`
4. Logs warning if invalid content type detected
5. Passes request to next middleware if valid

**Security Considerations:**
- Prevents non-JSON requests from reaching JSON parser
- Logs suspicious content types for security monitoring
- Requires explicit Content-Type header (no default assumption)

**File Reference:** Note - The HTTP middleware layer has been moved to the server. This module focuses on JSON-RPC validation in the `JsonRpcRequest::validate()` method.

**Usage Example:**
```rust
use axum::{Router, middleware};
use prism_core::middleware::validate_request_middleware;

let app = Router::new()
    .route("/", post(rpc_handler))
    .layer(middleware::from_fn(validate_request_middleware));
```

### JSON-RPC Validation

#### `JsonRpcRequest::validate`

```rust
impl JsonRpcRequest {
    pub fn validate(&self) -> Result<(), ValidationError>
}
```

**Purpose:** Validates JSON-RPC request structure and parameters according to protocol specification and security policies.

**Returns:**
- `Ok(())`: Request is valid and safe to process
- `Err(ValidationError)`: Specific validation failure with details

**Validation Steps:**
1. **Version Check**: Ensures `jsonrpc` field equals `"2.0"`
2. **Method Sanitization**: Verifies method name contains only alphanumeric characters and underscores
3. **Method Authorization**: Checks method is in the allowed methods list
4. **Parameter Validation**: Calls method-specific validation if parameters present

**Error Conditions:**
- `ValidationError::InvalidVersion`: JSON-RPC version is not "2.0"
- `ValidationError::InvalidMethod`: Method name contains invalid characters
- `ValidationError::MethodNotAllowed`: Method not in allowed list
- Method-specific errors (see below)

**File Reference:** Lines 16-34 in `crates/prism-core/src/middleware/validation.rs`

**Usage Example:**
```rust
use prism_core::types::JsonRpcRequest;

let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(serde_json::json!(["latest", false])),
    id: serde_json::json!(1),
};

match request.validate() {
    Ok(()) => {
        // Safe to process
        process_request(request).await?;
    }
    Err(e) => {
        eprintln!("Validation failed: {}", e);
        return Err(ProxyError::Validation(e));
    }
}
```

### Method-Specific Validation

#### `validate_params_for_method` (private)

```rust
fn validate_params_for_method(
    &self,
    params: &serde_json::Value,
) -> Result<(), ValidationError>
```

**Purpose:** Internal method that routes to specialized validators based on method name.

**Supported Methods:**
- `eth_getLogs`: Validates log filter parameters (block range, topics)
- `eth_getBlockByNumber`: Validates block parameter format

**Other Methods:** No parameter validation (pass-through)

**File Reference:** Lines 40-61 in `crates/prism-core/src/middleware/validation.rs`

#### `validate_log_filter` (private)

```rust
fn validate_log_filter(filter: &serde_json::Value) -> Result<(), ValidationError>
```

**Purpose:** Validates `eth_getLogs` filter parameters to prevent resource exhaustion attacks.

**Validation Rules:**

1. **Block Range Limit**: Maximum 10,000 blocks between `fromBlock` and `toBlock`
   - Prevents DoS via expensive queries
   - Applies to both hex numbers and block tags
   - Tags like `"latest"` treated as `u64::MAX` for comparison

2. **Topics Array Limit**: Maximum 4 topics in topics array
   - Ethereum specification limit
   - Prevents excessive filter complexity

**Error Conditions:**
- `ValidationError::BlockRangeTooLarge(u64)`: Range exceeds MAX_BLOCK_RANGE (10,000 blocks)
- `ValidationError::TooManyTopics(usize)`: More than 4 topics specified

**Configuration:**
```rust
const MAX_BLOCK_RANGE: u64 = 10000;  // Line 70
```

**File Reference:** Lines 67-89 in `crates/prism-core/src/middleware/validation.rs`

**Valid Filter Example:**
```json
{
  "fromBlock": "0x1000",
  "toBlock": "0x2710",       // 10,000 blocks later (exactly at limit)
  "address": "0x...",
  "topics": [
    "0xddf252ad...",
    null,
    "0x000000..."
  ]
}
```

**Invalid Filter Example (range too large):**
```json
{
  "fromBlock": "0x64",       // Block 100
  "toBlock": "0x2775",       // Block 10,101 (exceeds 10,000 block limit)
  "topics": ["0x..."]
}
```

#### `validate_block_parameter` (private)

```rust
fn validate_block_parameter(param: &serde_json::Value) -> Result<(), ValidationError>
```

**Purpose:** Validates block number/tag parameters for `eth_getBlockByNumber` and similar methods.

**Validation Rules:**

1. **Valid Block Tags**: Must be one of the Ethereum-defined tags
   - `"latest"`: Most recent block
   - `"earliest"`: Genesis block (block 0)
   - `"pending"`: Pending block (miner's view)
   - `"safe"`: Safe head block (post-merge)
   - `"finalized"`: Finalized block (post-merge)

2. **Hex Block Numbers**: Must start with `"0x"` prefix
   - Maximum 66 characters (64 hex digits + "0x" prefix)
   - Only valid hexadecimal characters (0-9, a-f, A-F)
   - Examples: `"0x0"`, `"0x1234567"`, `"0xabcdef"`

**Error Conditions:**
- `ValidationError::InvalidBlockParameter`: Parameter is neither a valid tag nor valid hex number

**File Reference:** Lines 96-115 in `crates/prism-core/src/middleware/validation.rs`

**Valid Parameters:**
```rust
// Block tags
"latest"
"earliest"
"pending"
"safe"
"finalized"

// Hex numbers
"0x0"           // Genesis
"0x1"           // Block 1
"0x123abc"      // Block 1,194,684
"0x1234567890"  // Arbitrary block number
```

**Invalid Parameters:**
```rust
"123"           // Missing 0x prefix
"0xGGG"         // Invalid hex characters
"unknown"       // Not a valid tag
"0x" + ("f" * 67)  // Too long (>66 chars)
```

#### `parse_block_number` (private)

```rust
fn parse_block_number(value: &serde_json::Value) -> Result<u64, ValidationError>
```

**Purpose:** Converts block parameter (tag or hex) to numeric block number for range validation.

**Conversion Rules:**
- `"latest"` → `u64::MAX` (treated as current chain tip)
- `"pending"` → `u64::MAX` (treated as current chain tip)
- `"earliest"` → `0` (genesis block)
- `"safe"` → Must provide hex number (tag not converted)
- `"finalized"` → Must provide hex number (tag not converted)
- `"0x..."` → Parse as hexadecimal number
- Decimal strings → Parse as decimal number (legacy support)

**Error Conditions:**
- `ValidationError::InvalidBlockParameter`: Parse failure or invalid format

**File Reference:** Lines 121-138 in `crates/prism-core/src/middleware/validation.rs`

**Implementation Note:**
This method is used internally by `validate_log_filter` to compute block ranges. It treats dynamic tags like `"latest"` as `u64::MAX` to ensure range validation works correctly without knowing the current block height.

## Error Types

### `ValidationError` Enum

```rust
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("Invalid JSON-RPC version: {0}")]
    InvalidVersion(String),

    #[error("Invalid method name: {0}")]
    InvalidMethod(String),

    #[error("Method not allowed: {0}")]
    MethodNotAllowed(String),

    #[error("Invalid block parameter: {0}")]
    InvalidBlockParameter(String),

    #[error("Block range too large: {0} blocks")]
    BlockRangeTooLarge(u64),

    #[error("Too many topics: {0}")]
    TooManyTopics(usize),

    #[error("Invalid block hash")]
    InvalidBlockHash,
}
```

**File Reference:** Lines 142-171 in `crates/prism-core/src/middleware/validation.rs`

### Error Descriptions

#### `InvalidVersion(String)`
- **Trigger**: `jsonrpc` field is not exactly `"2.0"`
- **Context**: Contains the actual version string received
- **User Action**: Update client to use JSON-RPC 2.0 protocol
- **Example**: `InvalidVersion("1.0")` when client sends JSON-RPC 1.0 request

#### `InvalidMethod(String)`
- **Trigger**: Method name contains non-alphanumeric characters (except underscore)
- **Context**: Contains the invalid method name
- **Security**: Prevents injection attacks via method name field
- **Example**: `InvalidMethod("eth_getBlock; DROP TABLE")` blocks SQL injection attempt

#### `MethodNotAllowed(String)`
- **Trigger**: Method not in the `ALLOWED_METHODS` list
- **Context**: Contains the disallowed method name
- **Security**: Allowlist enforcement prevents unauthorized RPC access
- **Allowed Methods**: See Configuration section below
- **Example**: `MethodNotAllowed("eth_sendTransaction")` prevents write operations

#### `InvalidBlockParameter(String)`
- **Trigger**: Block parameter is neither a valid tag nor valid hex number
- **Context**: Contains the invalid parameter value
- **Common Causes**:
  - Missing `"0x"` prefix on hex numbers
  - Invalid characters in hex string
  - Misspelled block tags
  - String length exceeds 66 characters
- **Example**: `InvalidBlockParameter("invalid_block")`

#### `BlockRangeTooLarge(u64)`
- **Trigger**: `eth_getLogs` query spans more than 10,000 blocks
- **Context**: Contains the actual range size
- **Reason**: Resource exhaustion prevention
- **User Action**: Split query into multiple smaller ranges
- **Example**: `BlockRangeTooLarge(10001)` for a query from block 0 to 10,001

#### `TooManyTopics(usize)`
- **Trigger**: `eth_getLogs` filter contains more than 4 topics
- **Context**: Contains the actual number of topics
- **Reason**: Ethereum specification compliance
- **User Action**: Reduce topic filters to maximum of 4
- **Example**: `TooManyTopics(5)` when 5 topics specified

#### `InvalidBlockHash`
- **Trigger**: Block hash format validation fails (currently unused)
- **Future Use**: Reserved for `eth_getBlockByHash` parameter validation
- **Example**: `InvalidBlockHash` for malformed hash strings

## Request Validation Flow

### Complete Validation Pipeline

```
1. HTTP Middleware Layer
   ├─ Extract Content-Type header
   ├─ Validate header exists
   ├─ Validate starts with "application/json"
   └─ [FAIL] → 415 Unsupported Media Type
       [PASS] → Continue to JSON-RPC layer

2. JSON-RPC Version Validation
   ├─ Check jsonrpc == "2.0"
   └─ [FAIL] → InvalidVersion(version)
       [PASS] → Continue to method validation

3. Method Name Sanitization
   ├─ Check alphanumeric + underscore only
   └─ [FAIL] → InvalidMethod(method)
       [PASS] → Continue to authorization

4. Method Authorization
   ├─ Check method in ALLOWED_METHODS
   └─ [FAIL] → MethodNotAllowed(method)
       [PASS] → Continue to parameter validation

5. Method-Specific Parameter Validation
   ├─ eth_getLogs
   │  ├─ Validate block range ≤ 10,000
   │  ├─ Validate topics array ≤ 4
   │  └─ [FAIL] → BlockRangeTooLarge | TooManyTopics
   ├─ eth_getBlockByNumber
   │  ├─ Validate block parameter format
   │  └─ [FAIL] → InvalidBlockParameter
   └─ Other methods: No validation

6. Validation Complete
   └─ Request forwarded to ProxyEngine
```

### Validation Performance

**Hot Path Optimization:**
- Version check: Single string comparison (O(1))
- Method sanitization: Iterator over characters (O(n) where n = method name length)
- Authorization check: Array contains lookup (O(k) where k = allowed methods count ≈ 10)
- Parameter validation: Only for specific methods, skipped for most

**Typical Performance:**
- Cache hit on validation checks via CPU branch predictor
- No allocations for valid requests
- Early return on first validation failure
- Total validation time: < 1 microsecond for valid requests

**Memory Efficiency:**
- No cloning of request data during validation
- Borrows references to check values
- Error types contain only necessary context (String or usize)

## JSON-RPC Compliance

### JSON-RPC 2.0 Specification

The validation module enforces strict compliance with [JSON-RPC 2.0 Specification](https://www.jsonrpc.org/specification).

**Required Fields:**
- `jsonrpc`: Must be exactly `"2.0"` (string)
- `method`: String containing method name
- `id`: Identifier (number, string, or null)

**Optional Fields:**
- `params`: Structured parameter value (array or object)

### Specification Enforcement

#### Version Enforcement
```rust
if self.jsonrpc != "2.0" {
    return Err(ValidationError::InvalidVersion(self.jsonrpc.clone()));
}
```
**File Reference:** Lines 36-38

**Rationale:** Prevents protocol confusion attacks and ensures client compatibility.

#### Method Name Requirements

**JSON-RPC 2.0 Specification:**
> The method name should be a String containing the name of the method to be invoked.

**Prism Implementation:**
```rust
if !self.method.chars().all(|c| c.is_alphanumeric() || c == '_') {
    return Err(ValidationError::InvalidMethod(self.method.clone()));
}
```
**File Reference:** Lines 40-42

**Additional Restrictions:**
- Only alphanumeric characters and underscores allowed
- No special characters, spaces, or control characters
- Prevents injection attacks via method field

**Ethereum RPC Convention:**
All Ethereum RPC methods follow the pattern: `<namespace>_<methodName>`
- Examples: `eth_getBlockByNumber`, `net_version`, `eth_getLogs`
- All conform to alphanumeric + underscore rule

## Configuration

### Validation Limits

All validation limits are compile-time constants defined in the validation module:

#### Block Range Limit

```rust
const MAX_BLOCK_RANGE: u64 = 10000;
```
**File Reference:** Line 81 in `crates/prism-core/src/middleware/validation.rs`

**Purpose:** Maximum number of blocks allowed in `eth_getLogs` queries

**Rationale:**
- Prevents resource exhaustion attacks
- Limits response size to manageable levels
- Balances between usability and security
- Large RPC providers (Infura, Alchemy) use similar limits

**Performance Impact:**
- 10,000 blocks with high activity can return millions of logs
- Query time can exceed 30 seconds on some providers
- Response size can exceed 100MB for busy contracts

**Modification Considerations:**
If changing this value:
- Lower values: Better protection, reduced DoS risk, but less convenient for users
- Higher values: More convenient, but increased resource usage and DoS risk
- Recommended range: 5,000 - 50,000 blocks depending on infrastructure

#### Topics Array Limit

```rust
if topics_array.len() > 4 {
    return Err(ValidationError::TooManyTopics(topics_array.len()));
}
```
**File Reference:** Lines 93-95 in `crates/prism-core/src/middleware/validation.rs`

**Limit:** 4 topics maximum

**Purpose:** Enforce Ethereum log event specification

**Rationale:**
- Ethereum events support maximum 4 indexed topics
- First topic is event signature hash
- Remaining 3 topics are indexed event parameters
- Hard limit in Ethereum protocol, not configurable

**Example:**
```solidity
// ERC-20 Transfer event
event Transfer(
    address indexed from,    // Topic 1
    address indexed to,      // Topic 2
    uint256 value            // Not indexed (in data)
);

// Topics array:
// [0] = keccak256("Transfer(address,address,uint256)")
// [1] = from address
// [2] = to address
// [3] = (unused in this example)
```

#### Block Parameter Length Limit

```rust
if block_str.len() > 66 {
    return Err(ValidationError::InvalidBlockParameter(block_str.to_string()));
}
```
**File Reference:** Lines 111-113 in `crates/prism-core/src/middleware/validation.rs`

**Limit:** 66 characters maximum

**Purpose:** Maximum hex-encoded block number length

**Rationale:**
- Ethereum block numbers are `u256` (256-bit integers)
- Hex representation: 64 characters maximum
- Plus "0x" prefix = 66 characters total
- Current Ethereum block number is ~20M (requires ~7 hex digits)
- Limit allows for future growth while preventing abuse

#### Valid Block Tags

```rust
const VALID_TAGS: &[&str] = &["latest", "earliest", "pending", "safe", "finalized"];
```
**File Reference:** Line 104 in `crates/prism-core/src/middleware/validation.rs`

**Defined Tags:**
- `"latest"`: Most recent mined block
- `"earliest"`: Genesis block (block 0)
- `"pending"`: Pending block being mined (miner's view)
- `"safe"`: Safe head block (post-merge, ~25 epochs behind finalized)
- `"finalized"`: Finalized block (post-merge, ~2 epochs behind latest)

**Ethereum Specification:**
These tags are defined in the [Ethereum JSON-RPC API](https://ethereum.org/en/developers/docs/apis/json-rpc/#default-block) and the [EIP-1898](https://eips.ethereum.org/EIPS/eip-1898) specification.

### Allowed Methods

The allowed methods list is defined in the types module and referenced by validation:

```rust
pub const ALLOWED_METHODS: &[&str] = &[
    "net_version",
    "eth_blockNumber",
    "eth_chainId",
    "eth_gasPrice",
    "eth_getBalance",
    "eth_getBlockByHash",
    "eth_getBlockByNumber",
    "eth_getLogs",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
];
```
**File Reference:** Lines 5-16 in `crates/prism-core/src/types.rs`

**Method Categories:**

**Network Information (Read-Only):**
- `net_version`: Network ID (1 for mainnet, 5 for goerli, etc.)
- `eth_chainId`: Chain ID (1 for mainnet, matches EIP-155)

**Chain State (Read-Only):**
- `eth_blockNumber`: Current block height
- `eth_gasPrice`: Current gas price estimate

**Account Queries (Read-Only):**
- `eth_getBalance`: Get ETH balance for address

**Block Queries (Cacheable):**
- `eth_getBlockByNumber`: Get block by number or tag
- `eth_getBlockByHash`: Get block by hash

**Transaction Queries (Cacheable):**
- `eth_getTransactionByHash`: Get transaction details
- `eth_getTransactionReceipt`: Get transaction receipt

**Log Queries (Advanced Caching):**
- `eth_getLogs`: Get event logs with filter

**Notably Excluded (Security):**
- `eth_sendTransaction`: Write operation, not supported
- `eth_sendRawTransaction`: Write operation, not supported
- `eth_sign`: Signing operation, not supported
- `personal_*`: Account management, not supported
- `debug_*`: Debug namespace, not supported
- `admin_*`: Admin namespace, not supported

## Usage Examples

### Basic Request Validation

```rust
use prism_core::types::JsonRpcRequest;
use prism_core::middleware::ValidationError;

async fn handle_rpc_request(body: String) -> Result<(), Box<dyn std::error::Error>> {
    // Parse JSON-RPC request
    let request: JsonRpcRequest = serde_json::from_str(&body)?;

    // Validate request
    match request.validate() {
        Ok(()) => {
            println!("Request is valid: {}", request.method);
            // Process request...
        }
        Err(ValidationError::InvalidVersion(version)) => {
            eprintln!("Client using wrong JSON-RPC version: {}", version);
            return Err("Invalid JSON-RPC version".into());
        }
        Err(ValidationError::MethodNotAllowed(method)) => {
            eprintln!("Method not supported: {}", method);
            return Err("Method not allowed".into());
        }
        Err(e) => {
            eprintln!("Validation error: {}", e);
            return Err(e.into());
        }
    }

    Ok(())
}
```

### Axum Middleware Integration

```rust
use axum::{
    Router,
    routing::post,
    middleware,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use prism_core::{
    middleware::validate_request_middleware,
    types::{JsonRpcRequest, JsonRpcResponse},
    proxy::ProxyEngine,
};
use std::sync::Arc;

async fn rpc_handler(
    engine: Arc<ProxyEngine>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    // Validation happens in the request handler
    match request.validate() {
        Ok(()) => {
            // Process valid request
            match engine.process_request(request).await {
                Ok(response) => Json(response).into_response(),
                Err(e) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("{}", e))
                        .into_response()
                }
            }
        }
        Err(e) => {
            // Return validation error as 400 Bad Request
            (StatusCode::BAD_REQUEST, format!("Validation failed: {}", e))
                .into_response()
        }
    }
}

#[tokio::main]
async fn main() {
    let engine = Arc::new(/* ... initialize ProxyEngine ... */);

    let app = Router::new()
        .route("/", post(rpc_handler))
        .layer(middleware::from_fn(validate_request_middleware))
        .with_state(engine);

    // Run server...
}
```

### Error Handling by Type

```rust
use prism_core::middleware::ValidationError;
use axum::http::StatusCode;

fn validation_error_to_status_code(error: &ValidationError) -> StatusCode {
    match error {
        // Client errors (400 Bad Request)
        ValidationError::InvalidVersion(_) => StatusCode::BAD_REQUEST,
        ValidationError::InvalidMethod(_) => StatusCode::BAD_REQUEST,
        ValidationError::InvalidBlockParameter(_) => StatusCode::BAD_REQUEST,
        ValidationError::BlockRangeTooLarge(_) => StatusCode::BAD_REQUEST,
        ValidationError::TooManyTopics(_) => StatusCode::BAD_REQUEST,
        ValidationError::InvalidBlockHash => StatusCode::BAD_REQUEST,

        // Authorization errors (403 Forbidden)
        ValidationError::MethodNotAllowed(_) => StatusCode::FORBIDDEN,
    }
}

fn create_error_response(error: ValidationError) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: None,
        error: Some(JsonRpcError {
            code: -32600,  // Invalid Request
            message: error.to_string(),
            data: None,
        }),
        id: serde_json::json!(null),
        cache_status: None,
    }
}
```

### Validating eth_getLogs Requests

```rust
use prism_core::types::JsonRpcRequest;
use serde_json::json;

// Valid request: Small range
let valid_request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getLogs".to_string(),
    params: Some(json!([{
        "fromBlock": "0x1000000",
        "toBlock": "0x1001000",    // 4,096 blocks (well within limit)
        "address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "topics": [
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
        ]
    }])),
    id: json!(1),
};
assert!(valid_request.validate().is_ok());

// Invalid request: Range too large
let invalid_request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getLogs".to_string(),
    params: Some(json!([{
        "fromBlock": "0x0",        // Block 0
        "toBlock": "0x100000",     // Block 1,048,576 (exceeds 10,000 limit)
        "topics": ["0x..."]
    }])),
    id: json!(2),
};

match invalid_request.validate() {
    Err(ValidationError::BlockRangeTooLarge(range)) => {
        println!("Range {} exceeds limit of 10,000", range);
        // Split into multiple requests...
    }
    _ => unreachable!(),
}

// Invalid request: Too many topics
let too_many_topics = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getLogs".to_string(),
    params: Some(json!([{
        "fromBlock": "latest",
        "toBlock": "latest",
        "topics": ["0x1", "0x2", "0x3", "0x4", "0x5"]  // 5 topics > 4 limit
    }])),
    id: json!(3),
};

assert!(matches!(
    too_many_topics.validate(),
    Err(ValidationError::TooManyTopics(5))
));
```

### Validating Block Parameters

```rust
use prism_core::types::JsonRpcRequest;
use prism_core::middleware::ValidationError;
use serde_json::json;

// Valid: Block tag
let tag_request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(json!(["latest", false])),
    id: json!(1),
};
assert!(tag_request.validate().is_ok());

// Valid: Hex block number
let hex_request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(json!(["0x1234567", false])),
    id: json!(2),
};
assert!(hex_request.validate().is_ok());

// Invalid: Missing 0x prefix
let invalid_hex = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(json!(["1234567", false])),  // Missing 0x
    id: json!(3),
};
assert!(matches!(
    invalid_hex.validate(),
    Err(ValidationError::InvalidBlockParameter(_))
));

// Invalid: Unknown tag
let invalid_tag = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(json!(["newest", false])),  // Not a valid tag
    id: json!(4),
};
assert!(matches!(
    invalid_tag.validate(),
    Err(ValidationError::InvalidBlockParameter(_))
));
```

### Testing Validation Rules

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_version_validation() {
        let request = JsonRpcRequest {
            jsonrpc: "1.0".to_string(),
            method: "eth_blockNumber".to_string(),
            params: None,
            id: json!(1),
        };

        let result = request.validate();
        assert!(matches!(
            result,
            Err(ValidationError::InvalidVersion(v)) if v == "1.0"
        ));
    }

    #[tokio::test]
    async fn test_method_sanitization() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "eth_getBlock; DROP TABLE users;--".to_string(),
            params: None,
            id: json!(1),
        };

        let result = request.validate();
        assert!(matches!(
            result,
            Err(ValidationError::InvalidMethod(_))
        ));
    }

    #[tokio::test]
    async fn test_block_range_validation() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "eth_getLogs".to_string(),
            params: Some(json!([{
                "fromBlock": "0x64",      // 100
                "toBlock": "0x2775",      // 10,101 (exceeds limit)
            }])),
            id: json!(1),
        };

        let result = request.validate();
        assert!(matches!(
            result,
            Err(ValidationError::BlockRangeTooLarge(10001))
        ));
    }
}
```

## Performance Considerations

### Validation Overhead

**Hot Path Analysis:**
1. **Version Check**: 1 string comparison (~1 CPU cycle with branch prediction)
2. **Method Sanitization**: Character iteration (10-30 chars typical)
3. **Authorization Check**: Linear search through 10 methods (~10 comparisons)
4. **Parameter Validation**: Only for specific methods (eth_getLogs, eth_getBlockByNumber)

**Total Overhead:**
- Valid request: < 1 microsecond on modern CPU
- Invalid request: < 100 nanoseconds (early return)
- Negligible compared to network latency (1-100ms) and upstream processing (10-1000ms)

### Memory Efficiency

**Zero-Copy Validation:**
- All validation uses borrowed references (`&self`, `&serde_json::Value`)
- No cloning of request data during validation
- Error types contain only necessary context (String or numeric values)

**Allocation Pattern:**
```rust
// No allocations for valid requests
request.validate()?;  // Only borrows, no clones

// Single allocation for error context
Err(ValidationError::InvalidMethod(method.clone()))  // Clone only on error path
```

### Security vs Performance Trade-offs

**Method Allowlist:**
- Current: Linear search through array (O(n) where n=10)
- Alternative: HashSet lookup (O(1) but requires static initialization)
- Decision: Array is faster for small lists (< 20 items) due to cache locality

**Block Range Validation:**
- Current: Parse and compare block numbers
- Alternative: Skip validation, handle DoS via timeout
- Decision: Early rejection prevents wasted upstream requests and resource consumption

**Parameter Parsing:**
- Current: Parse block numbers to u64 for validation
- Alternative: String comparison only
- Decision: Numeric validation prevents edge cases (overflow, underflow)

## Related Components

### Direct Dependencies
- **Types Module** (`crates/prism-core/src/types.rs`) - JsonRpcRequest, ALLOWED_METHODS
- **Axum** - HTTP middleware framework

### Integration Points
- **ProxyEngine** (`docs/components/proxy_engine.md`) - Receives validated requests
- **HTTP Server** (`crates/server/src/main.rs`) - Applies middleware stack
- **Rate Limiting** (`crates/prism-core/src/middleware/rate_limiting.rs`) - Applied before validation
- **Authentication** (`crates/prism-core/src/middleware/auth.rs`) - Applied before validation

### Middleware Stack Order

```
Client Request
    ↓
Rate Limiting       ← First: Prevent DoS
    ↓
Authentication      ← Second: Verify API key
    ↓
Validation          ← Third: Validate request format (THIS MODULE)
    ↓
ProxyEngine         ← Fourth: Process valid request
    ↓
Response
```

## Testing

The validation module includes comprehensive test coverage for all validation rules:

### Test Coverage

**File Reference:** Lines 173-356 in `crates/prism-core/src/middleware/validation.rs`

**Test Cases:**
1. `test_request_validation_valid` (Lines 180-189) - Valid request passes
2. `test_request_validation_invalid_version` (Lines 192-207) - Version validation
3. `test_request_validation_unsupported_method` (Lines 209-225) - Method authorization
4. `test_request_validation_logs_filter` (Lines 227-241) - Valid log filter
5. `test_request_validation_logs_filter_large_range` (Lines 243-264) - Block range limit
6. `test_request_validation_logs_filter_valid_range` (Lines 266-282) - Boundary testing
7. `test_request_validation_too_many_topics` (Lines 284-304) - Topic limit
8. `test_request_validation_invalid_block_parameter` (Lines 306-322) - Block parameter format
9. `test_request_validation_valid_block_tags` (Lines 324-338) - All valid tags
10. `test_request_validation_hex_block_numbers` (Lines 340-355) - Hex number formats

### Running Tests

```bash
# Run validation module tests only
cargo test --lib middleware::validation

# Run with output
cargo make test-core

# Run all middleware tests
cargo test --lib middleware
```

## Version History

**Current Implementation:** As of commit `ce591be` (2025-11-28)

**Recent Changes:**
- Added `net_version` to allowed methods
- Comprehensive validation for `eth_getLogs` and `eth_getBlockByNumber`
- Block range limit enforcement (10,000 blocks)
- Topic array limit enforcement (4 topics)
- HTTP Content-Type validation middleware

**Future Enhancements:**
- Configurable block range limits via environment variables
- Additional method-specific validations as needed
- Performance metrics for validation overhead
- Custom error codes for different validation failures
