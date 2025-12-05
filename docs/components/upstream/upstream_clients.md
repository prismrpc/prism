# Upstream HTTP and WebSocket Clients

## Overview

The upstream client layer provides reliable, high-performance HTTP and WebSocket communication with Ethereum RPC providers. These clients handle connection pooling, concurrency limiting, retry logic, timeout management, and real-time chain tip subscriptions for cache invalidation.

**Key Components**:
- **HTTP Client**: Executes JSON-RPC requests with connection pooling, semaphore-based concurrency limits, and automatic retries
- **WebSocket Client**: Maintains persistent connections for `eth_subscribe` to `newHeads` events, enabling real-time cache updates
- **Error Handling**: Rich error types for timeout, network, RPC, and protocol failures
- **Failure Tracking**: Smart retry logic that prevents endless reconnection attempts

**Source Files**:
- HTTP: `crates/prism-core/src/upstream/http_client.rs`
- WebSocket: `crates/prism-core/src/upstream/websocket.rs`
- Errors: `crates/prism-core/src/upstream/errors.rs`

---

## HTTP Client

### Overview

The `HttpClient` wraps `reqwest::Client` with production-ready features for high-concurrency RPC proxy workloads:

- **Connection Pooling**: Persistent HTTP/2 connections with configurable idle timeouts
- **Concurrency Limiting**: Semaphore-based permits prevent overwhelming upstream providers
- **Automatic Retries**: Exponential backoff for server errors (500-599)
- **Adaptive Timeouts**: Shorter permit acquisition timeouts when permits are scarce
- **RAII Permit Guards**: Ensures semaphore permits are always released, even during panics

### Configuration

#### HttpClientConfig

```rust
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    pub concurrent_limit: usize,
    pub permit_timeout_ms: u64,
    pub permit_timeout_scarce_ms: u64,
    pub scarce_permit_threshold: usize,
}
```

**Fields** (lines 8-18):

- **`concurrent_limit`**: Maximum concurrent requests allowed (semaphore permits)
  - Default: 1000
  - Controls memory usage and upstream load

- **`permit_timeout_ms`**: Normal permit acquisition timeout in milliseconds
  - Default: 500ms (increased from 100ms to handle high concurrency)
  - How long to wait for a permit when permits are available

- **`permit_timeout_scarce_ms`**: Permit timeout when permits are scarce
  - Default: 200ms (increased from 50ms)
  - Shorter timeout prevents permit accumulation during high load

- **`scarce_permit_threshold`**: Available permits below this are considered "scarce"
  - Default: 100
  - When `available_permits() < 100`, use shorter timeout

**Default Configuration** (lines 20-30):
```rust
impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            concurrent_limit: 1000,
            permit_timeout_ms: 500,
            permit_timeout_scarce_ms: 200,
            scarce_permit_threshold: 100,
        }
    }
}
```

### Data Structures

#### HttpClient

Main HTTP client with connection pooling and concurrency control.

```rust
pub struct HttpClient {
    client: Client,
    concurrent_limit: Arc<Semaphore>,
    config: HttpClientConfig,
}
```

**Fields** (lines 32-36):

- **`client`**: Underlying `reqwest::Client` with connection pooling
  - HTTP/2 adaptive window enabled
  - TLS via rustls (no OpenSSL dependency)
  - TCP keepalive and nodelay for low latency

- **`concurrent_limit`**: Semaphore controlling concurrent request count
  - Shared via `Arc` for cloning
  - Prevents overwhelming upstream providers

- **`config`**: Configuration controlling timeouts and limits

#### PermitGuard

RAII guard ensuring semaphore permits are always released.

```rust
struct PermitGuard {
    _permit: OwnedSemaphorePermit,
    semaphore: Arc<Semaphore>,
}
```

**Fields** (lines 43-46):

- **`_permit`**: Owned semaphore permit (dropped automatically)
- **`semaphore`**: Reference to semaphore for debugging available permits

**Critical Design** (lines 38-66):
- Uses `OwnedSemaphorePermit` which owns an `Arc` to the semaphore
- Safe to hold across async boundaries without lifetime issues
- Automatically released on drop, even during panics or early returns
- Prevents resource leaks during load ramp-down

**Drop Implementation** (lines 59-66):
```rust
impl Drop for PermitGuard {
    fn drop(&mut self) {
        tracing::trace!(
            "PermitGuard dropped, available permits: {}",
            self.semaphore.available_permits()
        );
    }
}
```

### Public API

#### Constructors

##### `new() -> Result<Self, UpstreamError>`

Creates HTTP client with default configuration.

**Location**: Lines 77-84

```rust
let client = HttpClient::new()?;
```

**Errors**: Returns `UpstreamError::ConnectionFailed` if client build fails

##### `with_concurrency_limit(concurrent_limit: usize) -> Result<Self, UpstreamError>`

Creates HTTP client with custom concurrency limit, other settings default.

**Location**: Lines 86-92

```rust
let client = HttpClient::with_concurrency_limit(500)?;
```

##### `with_config(config: HttpClientConfig) -> Result<Self, UpstreamError>`

Creates HTTP client with full custom configuration.

**Location**: Lines 94-121

```rust
let config = HttpClientConfig {
    concurrent_limit: 2000,
    permit_timeout_ms: 1000,
    permit_timeout_scarce_ms: 300,
    scarce_permit_threshold: 200,
};
let client = HttpClient::with_config(config)?;
```

**Client Configuration** (lines 99-114):
- **Pool idle timeout**: 30 seconds (reduced from 90s for faster cleanup)
- **Max idle per host**: 100 connections (increased from 50 for better load handling)
- **Connect timeout**: 5 seconds
- **Overall request timeout**: 45 seconds
- **HTTP/2 adaptive window**: Enabled for better flow control
- **TLS**: Rustls (memory-safe, no OpenSSL)
- **Redirect policy**: None (don't follow redirects)
- **User agent**: `rpc-aggregator/0.1.0`
- **TCP keepalive**: 30 seconds
- **TCP nodelay**: Enabled for low latency

#### Request Execution

##### `send_request(&self, url: &str, body: bytes::Bytes, timeout: Duration) -> Result<bytes::Bytes, UpstreamError>`

Sends POST request to upstream RPC endpoint with retries and concurrency limiting.

**Location**: Lines 123-228

```rust
let request_body = serde_json::to_vec(&json_rpc_request)?;
let response_bytes = http_client.send_request(
    "https://eth-mainnet.example.com",
    bytes::Bytes::from(request_body),
    Duration::from_secs(30),
).await?;
```

**Parameters**:
- **`url`**: Upstream RPC endpoint URL
- **`body`**: JSON-RPC request body as bytes (pre-serialized)
- **`timeout`**: Per-request timeout (not including permit acquisition)

**Returns**: Response body as bytes on success

**Errors**:
- `UpstreamError::Timeout`: Permit acquisition or request timeout
- `UpstreamError::ConcurrencyLimit`: Too many concurrent requests
- `UpstreamError::HttpError(status, text)`: Non-success HTTP status
- `UpstreamError::Network`: Network or protocol error

**Retry Logic** (lines 133, 175-227):
- **Max retries**: 2
- **Retry conditions**: Server errors (5xx) or network errors
- **Backoff**: Exponential `100ms * 2^retry` (100ms, 200ms)
- **Fast failure**: Client errors (4xx) fail immediately without retry

**Concurrency Control Algorithm** (lines 135-168):

1. **Adaptive Timeout Selection** (lines 136-141):
   ```rust
   let permit_timeout = if self.concurrent_limit.available_permits() < self.config.scarce_permit_threshold {
       Duration::from_millis(self.config.permit_timeout_scarce_ms)  // 200ms when scarce
   } else {
       Duration::from_millis(self.config.permit_timeout_ms)  // 500ms normally
   };
   ```
   - When `available_permits < 100`: Use shorter 200ms timeout
   - Otherwise: Use normal 500ms timeout
   - Prevents permit queue buildup during high load

2. **Permit Acquisition** (lines 144-165):
   ```rust
   let permit = tokio::time::timeout(
       permit_timeout,
       Arc::clone(&self.concurrent_limit).acquire_owned(),
   ).await??;
   ```
   - Clones `Arc<Semaphore>` to get `OwnedSemaphorePermit`
   - Timeout wrapper prevents indefinite waiting
   - Returns `UpstreamError::Timeout` on timeout
   - Returns `UpstreamError::ConcurrencyLimit` if semaphore closed

3. **RAII Guard Wrapping** (lines 167-168):
   ```rust
   let permit_guard = PermitGuard::new(permit, self.concurrent_limit.clone());
   ```
   - Wraps permit in RAII guard for automatic cleanup
   - Guard holds permit until function returns (success or error)
   - Prevents resource leaks during early returns or panics

**Request Execution** (lines 177-210):

```rust
loop {
    let result = self.client
        .post(url)
        .header("content-type", "application/json")
        .body(body.clone())
        .timeout(timeout)
        .send()
        .await;

    match result {
        Ok(response) if response.status().is_success() => {
            return response.bytes().await.map_err(UpstreamError::Network);
        }
        Ok(response) if response.status().is_server_error() && retries < MAX_RETRIES => {
            retries += 1;
            tokio::time::sleep(Duration::from_millis(100 * (1 << retries))).await;
            continue;  // Retry
        }
        Ok(response) => {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(UpstreamError::HttpError(status, text));
        }
        Err(_e) if retries < MAX_RETRIES => {
            retries += 1;
            tokio::time::sleep(Duration::from_millis(100 * (1 << retries))).await;
            continue;  // Retry
        }
        Err(e) if e.is_timeout() => {
            return Err(UpstreamError::Timeout);
        }
        Err(e) => {
            return Err(UpstreamError::Network(e));
        }
    }
}
```

**Key Behaviors**:
- Body is cloned for retries (cheap for `bytes::Bytes` due to refcounting)
- Timeout is per-attempt (each retry gets fresh timeout)
- Successful responses return immediately
- Server errors (5xx) trigger retry with exponential backoff
- Network errors trigger retry up to max attempts
- Client errors (4xx) fail immediately without retry

---

## WebSocket Client

### Overview

The `WebSocketHandler` manages persistent WebSocket connections to upstream RPC providers for real-time `eth_subscribe` notifications. Key features:

- **Real-time Block Subscriptions**: Subscribes to `newHeads` events for immediate cache updates
- **Automatic Block Caching**: Fetches full block data (header, body, transactions, receipts, logs) on new blocks
- **Failure Tracking**: Smart retry logic prevents endless reconnection attempts to broken upstreams
- **Chain Tip Updates**: Updates cache manager and reorg manager with latest block information
- **Graceful Error Handling**: Detailed error messages for common WebSocket connection issues

### Data Structures

#### WebSocketFailureTracker

Tracks WebSocket connection failures to avoid endless retries.

```rust
#[derive(Debug)]
pub struct WebSocketFailureTracker {
    consecutive_failures: u32,
    last_failure_time: Instant,
    max_consecutive_failures: u32,
    failure_reset_duration: Duration,
    permanently_failed: bool,
    last_reset_time: Instant,
}
```

**Fields** (lines 26-34):

- **`consecutive_failures`**: Count of consecutive connection failures
- **`last_failure_time`**: When the last failure occurred
- **`max_consecutive_failures`**: Threshold to stop retrying (default: 3)
- **`failure_reset_duration`**: Time before resetting failure count (default: 300s / 5 minutes)
- **`permanently_failed`**: Flag indicating upstream should be abandoned
- **`last_reset_time`**: When failure count was last reset

**Default Configuration** (lines 36-46):
```rust
impl Default for WebSocketFailureTracker {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_time: Instant::now(),
            max_consecutive_failures: 3,
            failure_reset_duration: Duration::from_secs(300),
            permanently_failed: false,
            last_reset_time: Instant::now(),
        }
    }
}
```

**Failure Tracking Logic** (lines 49-106):

- **`record_failure()`** (lines 51-60): Increments failure count, marks permanently failed after 6 consecutive failures (2x threshold)

- **`record_success()`** (lines 63-66): Resets failure count and permanent failure flag

- **`should_stop_retrying()`** (lines 69-82): Returns `true` if marked permanently failed OR if max failures reached within reset duration

- **`reset_if_expired()`** (lines 85-93): Resets failure count and permanent flag if reset duration has elapsed

- **`get_failure_count()`** (lines 96-99): Returns current consecutive failure count

- **`is_permanently_failed()`** (lines 102-105): Returns permanent failure status

#### WebSocketHandler

Manages WebSocket subscriptions for a single upstream.

```rust
pub struct WebSocketHandler {
    config: UpstreamConfig,
    http_client: Arc<HttpClient>,
    failure_tracker: Arc<RwLock<WebSocketFailureTracker>>,
}
```

**Fields** (lines 109-113):

- **`config`**: Upstream configuration (URL, WebSocket URL, name)
- **`http_client`**: HTTP client for fetching full block data after notifications
- **`failure_tracker`**: Shared failure tracker for retry coordination

### Public API

#### Constructor

##### `new(config: UpstreamConfig, http_client: Arc<HttpClient>) -> Self`

Creates a new WebSocket handler for an upstream.

**Location**: Lines 116-123

```rust
let ws_handler = WebSocketHandler::new(
    upstream_config.clone(),
    http_client.clone(),
);
```

#### Subscription Management

##### `subscribe_to_new_heads(&self, cache_manager: Arc<CacheManager>, reorg_manager: Arc<ReorgManager>) -> Result<(), UpstreamError>`

Subscribes to `newHeads` events and processes incoming blocks.

**Location**: Lines 125-142

```rust
let result = ws_handler.subscribe_to_new_heads(
    cache_manager.clone(),
    reorg_manager.clone(),
).await;
```

**Process Flow**:
1. Validates WebSocket URL configuration (lines 134)
2. Connects to WebSocket endpoint (lines 135)
3. Splits stream into write/read halves (lines 136)
4. Sends subscription message (lines 138)
5. Handles incoming messages in loop (lines 139)

**Errors**:
- `UpstreamError::InvalidResponse`: No WebSocket URL configured or invalid format
- `UpstreamError::ConnectionFailed`: WebSocket connection failed
- Network or protocol errors from WebSocket library

**Long-Running**: This function blocks processing messages until connection closes or errors

##### `should_attempt_websocket_subscription(&self) -> bool`

Checks if WebSocket subscription should be attempted based on failure history.

**Location**: Lines 144-149

```rust
if ws_handler.should_attempt_websocket_subscription().await {
    // Attempt subscription
    let result = ws_handler.subscribe_to_new_heads(...).await;
    // ...
}
```

**Logic**:
1. Acquires write lock on failure tracker
2. Resets failure count if reset duration has elapsed
3. Returns `false` if should stop retrying, `true` otherwise

**Usage**: Called before each subscription attempt to prevent endless retries

##### `record_websocket_failure(&self)`

Records a WebSocket subscription failure, incrementing failure count.

**Location**: Lines 151-176

```rust
if let Err(e) = ws_handler.subscribe_to_new_heads(...).await {
    ws_handler.record_websocket_failure().await;
}
```

**Logging** (lines 157-175):
- Warns if permanently failed immediately after reset
- Warns when max consecutive failures reached with retry cooldown duration
- Debug logs for failures below threshold

##### `record_websocket_success(&self)`

Records successful WebSocket subscription, resetting failure count.

**Location**: Lines 178-186

```rust
// After successful subscription and initial message processing
ws_handler.record_websocket_success().await;
```

#### Internal Methods

##### `validate_and_get_ws_url(&self) -> Result<&str, UpstreamError>`

Validates WebSocket URL configuration.

**Location**: Lines 188-207

**Validation Checks**:
1. WebSocket URL is configured (not `None`)
2. URL is not empty or whitespace
3. URL starts with `ws://` or `wss://`

**Errors**:
- `UpstreamError::InvalidResponse("No WebSocket URL configured")`
- `UpstreamError::InvalidResponse("WebSocket URL is empty")`
- `UpstreamError::InvalidResponse("Invalid WebSocket URL format: ...")`

##### `connect_websocket(&self, ws_url: &str) -> Result<WebSocketStream, UpstreamError>`

Connects to WebSocket endpoint with detailed error messages.

**Location**: Lines 209-262

```rust
let ws_stream = self.connect_websocket("wss://eth-mainnet.example.com").await?;
```

**Error Handling** (lines 232-260):

Converts common connection errors to descriptive messages:

- **200 OK error** (lines 240-244): Server doesn't support WebSocket protocol
  ```
  "Server returned 200 OK but does not support WebSocket protocol for upstream {name}"
  ```

- **405 Method Not Allowed** (lines 245-249): WebSocket not allowed on this endpoint
  ```
  "WebSocket method not allowed for upstream {name} (405 Method Not Allowed)"
  ```

- **403 Forbidden** (lines 250-254): Authentication or authorization failure
  ```
  "WebSocket access forbidden for upstream {name} (403 Forbidden)"
  ```

- **Other errors** (lines 255-259): Generic connection failure
  ```
  "WebSocket connection failed: {e}"
  ```

**Success Logging** (lines 225-230): Logs successful connection with HTTP status code

##### `send_subscription_message(&self, write: &mut SplitSink) -> Result<(), UpstreamError>`

Sends `eth_subscribe` message for `newHeads` events.

**Location**: Lines 264-296

**Subscription Message** (lines 276-281):
```json
{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "eth_subscribe",
    "params": ["newHeads"]
}
```

**Errors**: `UpstreamError::ConnectionFailed("WebSocket send error: ...")` if send fails

##### `handle_websocket_messages(&self, read: &mut SplitStream, cache_manager: Arc<CacheManager>, reorg_manager: Arc<ReorgManager>) -> Result<(), UpstreamError>`

Main message processing loop for WebSocket messages.

**Location**: Lines 298-329

**Message Handling**:
- **Text messages** (lines 313-314): Process JSON notifications
- **Close messages** (lines 316-319): Log warning and break loop
- **Error messages** (lines 320-323): Log error and break loop
- **Other messages** (ping, pong, binary): Ignored (line 324)

**Loop Behavior**: Continues until connection closes or errors

##### `process_text_message(&self, text: &str, cache_manager: &Arc<CacheManager>, reorg_manager: &Arc<ReorgManager>)`

Processes a single text message from WebSocket.

**Location**: Lines 331-351

**Processing Steps**:
1. Parse JSON from text (lines 340-341)
2. Handle subscription confirmation if present (lines 343-345)
3. Handle new heads notification (line 347)
4. Log warning if JSON parsing fails (line 349)

##### `handle_subscription_confirmation(&self, json: &serde_json::Value) -> bool`

Handles subscription confirmation message.

**Location**: Lines 353-366

**Confirmation Format**:
```json
{
    "jsonrpc": "2.0",
    "id": 1,
    "result": "0x123abc..."  // Subscription ID
}
```

**Returns**: `true` if message was a confirmation (has string result), `false` otherwise

##### `handle_new_heads_notification(&self, json: &serde_json::Value, cache_manager: &Arc<CacheManager>, reorg_manager: &Arc<ReorgManager>)`

Handles `newHeads` subscription notifications.

**Location**: Lines 368-386

**Notification Format**:
```json
{
    "jsonrpc": "2.0",
    "method": "eth_subscription",
    "params": {
        "subscription": "0x123abc...",
        "result": {
            "number": "0x1234",
            "hash": "0xabcd...",
            "parentHash": "0x...",
            // ... other block fields
        }
    }
}
```

**Processing**: Extracts `params.result` and delegates to `process_block_data()`

##### `process_block_data(&self, result: &serde_json::Value, cache_manager: &Arc<CacheManager>, reorg_manager: &Arc<ReorgManager>)`

Processes block data from new heads notification.

**Location**: Lines 388-462

**Extraction** (lines 395-408):
1. Extracts block number and hash from JSON
2. Parses hex strings to native types (`u64`, `[u8; 32]`)
3. Logs extraction details

**Chain Tip Update** (lines 409-422):
Spawns background task to update chain tip if block is newer than current tip:
```rust
tokio::spawn(async move {
    Self::update_chain_tip_if_newer(
        block_number,
        block_hash,
        &cache_manager_clone,
        &reorg_manager_clone,
        &upstream_name,
    ).await;
});
```

**Full Block Fetch** (lines 424-455):
Spawns background task to fetch and cache full block data:
1. Acquires fetch lock with 30-second timeout (lines 431-434)
2. Skips if block already cached or being fetched (lines 449-453)
3. Fetches block, transactions, receipts, and logs (lines 440-447)

##### `update_chain_tip_if_newer(block_number: u64, block_hash: [u8; 32], cache_manager: &Arc<CacheManager>, reorg_manager: &Arc<ReorgManager>, upstream_name: &str)`

Updates chain tip if new block is higher than current tip.

**Location**: Lines 464-491

**Logic** (lines 472-490):
```rust
let current_tip = cache_manager.get_current_tip();
if block_number > current_tip {
    cache_manager.update_tip(block_number);
    reorg_manager.update_tip(block_number, block_hash).await;
    // Log info
} else {
    // Log debug (ignoring older block)
}
```

**Updates**:
- **Cache Manager**: Updates current tip for cleanup decisions
- **Reorg Manager**: Updates tip for reorganization detection

##### `fetch_and_cache_full_block(...)`

Spawns background task to fetch full block data from upstream.

**Location**: Lines 493-532

**Fetch Guard Usage** (lines 508):
- Holds `FetchGuard` to prevent duplicate fetches
- Guard is moved into spawned task and held until completion
- Automatically released on drop

**Fetch Process** (lines 515-530):
1. Calls `do_fetch_and_cache_block()` to fetch data
2. Logs warning on error
3. Does not propagate error (best-effort caching)

##### `do_fetch_and_cache_block(...) -> Result<(), String>`

Fetches and caches full block data including transactions, receipts, and logs.

**Location**: Lines 534-576

**Fetch Steps**:

1. **Fetch Block** (lines 541-561):
   ```rust
   let block_req = serde_json::json!({
       "jsonrpc": "2.0",
       "id": 1,
       "method": "eth_getBlockByNumber",
       "params": [with_hex_u64(block_number, ...), true]  // true = include full tx objects
   });
   let block_response = Self::send_json_request(...).await?;
   ```

2. **Cache Block Data** (line 563):
   - Caches header, body, and transactions

3. **Fetch and Cache Receipts** (lines 565-572):
   - Fetches receipts using `eth_getBlockReceipts`
   - Extracts logs from receipts
   - Caches receipts and logs

##### `send_json_request(http_client: &Arc<HttpClient>, url: &str, req: serde_json::Value, timeout: Duration, upstream_name: &str, what: &str) -> Option<serde_json::Value>`

Sends JSON-RPC request via HTTP client.

**Location**: Lines 578-617

**Process**:
1. Serializes JSON request (lines 586-592)
2. Sends via HTTP client (lines 594-601)
3. Parses JSON response (lines 603-609)
4. Checks for RPC error field (lines 611-615)

**Error Handling**: Returns `None` on any error, logs warnings/errors

**Parameters**:
- **`what`**: Description for logging ("block", "receipts", etc.)

##### `cache_block_data(block_number: u64, upstream_name: &str, cache_manager: &Arc<CacheManager>, block_data: &serde_json::Value)`

Caches block header, body, and transactions.

**Location**: Lines 619-648

**Caching Steps** (lines 625-646):

1. **Cache Header** (lines 625-628):
   ```rust
   if let Some(header) = json_block_to_block_header(block_data) {
       cache_manager.insert_header(header).await;
   }
   ```

2. **Cache Body** (lines 630-633):
   ```rust
   if let Some(body) = json_block_to_block_body(block_data) {
       cache_manager.insert_body(body).await;
   }
   ```

3. **Cache Transactions** (lines 635-646):
   ```rust
   if let Some(transactions) = block_data.get("transactions").and_then(|v| v.as_array()) {
       for tx_json in transactions {
           if let Some(tx) = json_transaction_to_transaction_record(tx_json) {
               cache_manager.transaction_cache.insert_transaction(tx).await;
           }
       }
   }
   ```

##### `fetch_and_cache_receipts_for_block(http_client: &Arc<HttpClient>, url: &str, block_number: u64, upstream_name: &str, cache_manager: &Arc<CacheManager>)`

Fetches and caches receipts and logs for a block.

**Location**: Lines 650-764

**Fetch Request** (lines 657-672):
```rust
let receipts_req = serde_json::json!({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "eth_getBlockReceipts",
    "params": [with_hex_u64(block_number, ...)]
});
```

**Processing Receipts** (lines 677-703):

1. **Extract Logs and Receipts** (lines 679-695):
   ```rust
   let mut log_records = Vec::new();
   let mut receipts_vec = Vec::new();

   for receipt_json in receipts_array {
       // Extract logs from each receipt
       if let Some(logs) = receipt_json.get("logs").and_then(|v| v.as_array()) {
           for log in logs {
               if let Some((log_id, log_record)) = json_log_to_log_record(log) {
                   log_records.push((log_id, log_record));
               }
           }
       }

       // Convert receipt
       if let Some(receipt) = json_receipt_to_receipt_record(receipt_json) {
           receipts_vec.push(receipt);
       }
   }
   ```

2. **Parallel Caching** (lines 711-762):
   ```rust
   let logs_fut = async {
       cache_manager.insert_logs_bulk_no_stats(log_records).await;
       cache_manager.update_stats_on_demand().await;
   };

   let receipts_fut = async {
       cache_manager.insert_receipts_bulk_no_stats(receipts_vec).await;
       cache_manager.transaction_cache.update_stats_on_demand().await;
   };

   tokio::join!(logs_fut, receipts_fut);
   ```

**Performance Optimization**:
- Uses `*_no_stats()` methods for faster insertion
- Updates stats separately (on-demand)
- Parallel insertion of logs and receipts via `tokio::join!`
- Logs detailed timing information for conversion and insertion

---

## Error Handling

### UpstreamError

Comprehensive error type for all upstream communication failures.

**Source**: `crates/prism-core/src/upstream/errors.rs`

```rust
#[derive(Error, Debug)]
pub enum UpstreamError {
    #[error("Request timeout")]
    Timeout,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("HTTP error: {0}")]
    HttpError(u16, String),

    #[error("RPC error: {0}")]
    RpcError(i32, String),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("No healthy upstreams available")]
    NoHealthyUpstreams,

    #[error("Circuit breaker is open")]
    CircuitBreakerOpen,

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Concurrency limit reached: {0}")]
    ConcurrencyLimit(String),
}
```

**Error Types** (lines 4-25):

1. **`Timeout`**: Request or permit acquisition timeout
   - HTTP: Permit timeout or request timeout
   - WebSocket: N/A (no timeout on subscription)

2. **`ConnectionFailed(String)`**: Connection establishment failed
   - HTTP: Client build failed
   - WebSocket: Connection handshake failed (with detailed context)

3. **`HttpError(u16, String)`**: Non-success HTTP status code
   - Fields: `(status_code, response_body)`
   - Example: `HttpError(429, "Rate limit exceeded")`

4. **`RpcError(i32, String)`**: JSON-RPC error response
   - Fields: `(error_code, error_message)`
   - Example: `RpcError(-32000, "insufficient funds")`

5. **`Network(reqwest::Error)`**: Network-level error
   - Automatically converted from `reqwest::Error` via `#[from]`
   - DNS, TCP, TLS errors

6. **`InvalidResponse(String)`**: Response parsing or validation failed
   - WebSocket: Invalid URL, empty URL, wrong protocol
   - HTTP: Malformed JSON response

7. **`NoHealthyUpstreams`**: All upstreams are unhealthy
   - Used by `UpstreamManager` when no providers available

8. **`CircuitBreakerOpen`**: Circuit breaker preventing requests
   - Future feature for upstream failure protection

9. **`InvalidRequest(String)`**: Request validation failed
   - Malformed JSON-RPC request

10. **`ConcurrencyLimit(String)`**: Concurrency limit reached
    - HTTP: Semaphore exhausted
    - Field: URL being requested

### Error Context

**HTTP Client Error Contexts**:
```rust
// Timeout acquiring permit
UpstreamError::Timeout
// Logs: "HTTP client semaphore acquisition timeout for URL: {url} (available: {permits})"

// Concurrency limit reached
UpstreamError::ConcurrencyLimit(url)
// Logs: "HTTP client concurrency limit reached for URL: {url} (available: {permits})"

// HTTP status error
UpstreamError::HttpError(500, "Internal Server Error")

// Network error (timeout)
UpstreamError::Timeout  // from reqwest::Error::is_timeout()

// Network error (other)
UpstreamError::Network(e)
```

**WebSocket Error Contexts**:
```rust
// No WebSocket URL configured
UpstreamError::InvalidResponse("No WebSocket URL configured")

// Invalid WebSocket URL
UpstreamError::InvalidResponse("Invalid WebSocket URL format: http://...")

// Connection failed with 200 OK (not WebSocket)
UpstreamError::ConnectionFailed(
    "Server returned 200 OK but does not support WebSocket protocol for upstream {name}"
)

// Connection failed with 405
UpstreamError::ConnectionFailed(
    "WebSocket method not allowed for upstream {name} (405 Method Not Allowed)"
)

// Connection failed with 403
UpstreamError::ConnectionFailed(
    "WebSocket access forbidden for upstream {name} (403 Forbidden)"
)

// Generic connection failure
UpstreamError::ConnectionFailed("WebSocket connection failed: {e}")

// Send error
UpstreamError::ConnectionFailed("WebSocket send error: {e}")
```

---

## Usage Examples

### Example 1: Basic HTTP Request

```rust
use prism_core::upstream::http_client::{HttpClient, HttpClientConfig};
use std::time::Duration;

// Create HTTP client
let client = HttpClient::new()?;

// Prepare JSON-RPC request
let request = serde_json::json!({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "eth_blockNumber",
    "params": []
});

let request_bytes = serde_json::to_vec(&request)?;

// Send request
let response_bytes = client.send_request(
    "https://eth-mainnet.example.com",
    bytes::Bytes::from(request_bytes),
    Duration::from_secs(10),
).await?;

// Parse response
let response: serde_json::Value = serde_json::from_slice(&response_bytes)?;
println!("Latest block: {:?}", response["result"]);
```

**Source**: Pattern from `crates/prism-core/src/upstream/http_client.rs:127`

### Example 2: HTTP Client with Custom Configuration

```rust
use prism_core::upstream::http_client::{HttpClient, HttpClientConfig};

// Configure for high-concurrency workload
let config = HttpClientConfig {
    concurrent_limit: 2000,           // Allow 2000 concurrent requests
    permit_timeout_ms: 1000,          // 1 second normal timeout
    permit_timeout_scarce_ms: 500,    // 500ms when scarce
    scarce_permit_threshold: 200,     // <200 permits = scarce
};

let client = HttpClient::with_config(config)?;

// Now client can handle 2000 concurrent requests
```

**Source**: Configuration from `crates/prism-core/src/upstream/http_client.rs:98`

### Example 3: WebSocket Subscription

```rust
use prism_core::upstream::websocket::WebSocketHandler;
use prism_core::cache::CacheManager;
use prism_core::cache::reorg_manager::ReorgManager;
use std::sync::Arc;

// Create handler
let ws_handler = WebSocketHandler::new(
    upstream_config.clone(),
    http_client.clone(),
);

// Check if we should attempt subscription (respects failure tracking)
if ws_handler.should_attempt_websocket_subscription().await {
    // Attempt subscription
    match ws_handler.subscribe_to_new_heads(
        cache_manager.clone(),
        reorg_manager.clone(),
    ).await {
        Ok(()) => {
            // Subscription succeeded and processed messages
            ws_handler.record_websocket_success().await;
        }
        Err(e) => {
            // Subscription failed
            tracing::error!("WebSocket subscription failed: {}", e);
            ws_handler.record_websocket_failure().await;
        }
    }
} else {
    tracing::debug!("Skipping WebSocket subscription due to previous failures");
}
```

**Source**: Pattern from `crates/prism-core/src/upstream/websocket.rs:129,145,152,179`

### Example 4: Handling WebSocket Messages in Real-Time

The WebSocket handler automatically processes `newHeads` notifications:

```rust
// When a new block arrives via WebSocket:
// 1. Block data is parsed from JSON notification
// 2. Chain tip is updated if block is newer
// 3. Full block data is fetched in background
// 4. All data is cached automatically

// Example notification:
{
    "jsonrpc": "2.0",
    "method": "eth_subscription",
    "params": {
        "subscription": "0x123abc...",
        "result": {
            "number": "0xf4240",        // Block 1,000,000
            "hash": "0xabcd...",
            "parentHash": "0x1234...",
            "timestamp": "0x5f5e100",
            // ... other fields
        }
    }
}

// This triggers:
// - cache_manager.update_tip(1000000)
// - reorg_manager.update_tip(1000000, block_hash)
// - Background fetch of full block via eth_getBlockByNumber
// - Background fetch of receipts via eth_getBlockReceipts
// - Caching of header, body, transactions, receipts, logs
```

**Source**: Processing from `crates/prism-core/src/upstream/websocket.rs:388-576`

### Example 5: Error Handling Patterns

```rust
use prism_core::upstream::UpstreamError;

async fn fetch_with_retry(client: &HttpClient, url: &str, body: bytes::Bytes) -> Result<bytes::Bytes, String> {
    match client.send_request(url, body.clone(), Duration::from_secs(30)).await {
        Ok(response) => Ok(response),

        Err(UpstreamError::Timeout) => {
            // Timeout - maybe try a different upstream
            Err("Request timed out".to_string())
        }

        Err(UpstreamError::ConcurrencyLimit(url)) => {
            // Too many concurrent requests - wait and retry
            tokio::time::sleep(Duration::from_millis(100)).await;
            client.send_request(&url, body, Duration::from_secs(30))
                .await
                .map_err(|e| e.to_string())
        }

        Err(UpstreamError::HttpError(status, msg)) if status >= 500 => {
            // Server error - automatic retry already attempted, try different upstream
            Err(format!("Server error {}: {}", status, msg))
        }

        Err(UpstreamError::HttpError(status, msg)) => {
            // Client error - don't retry
            Err(format!("Client error {}: {}", status, msg))
        }

        Err(UpstreamError::Network(e)) => {
            // Network error - automatic retry already attempted
            Err(format!("Network error: {}", e))
        }

        Err(e) => {
            // Other errors
            Err(e.to_string())
        }
    }
}
```

**Source**: Error handling patterns from `crates/prism-core/src/upstream/http_client.rs:187-227`

### Example 6: Spawning WebSocket Subscription in Background

```rust
use tokio::task;

// Spawn WebSocket subscription as background task
let ws_handler_clone = ws_handler.clone();
let cache_manager_clone = cache_manager.clone();
let reorg_manager_clone = reorg_manager.clone();

let ws_task = task::spawn(async move {
    loop {
        // Check if we should attempt subscription
        if !ws_handler_clone.should_attempt_websocket_subscription().await {
            // Too many failures, wait before retrying
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        // Attempt subscription
        match ws_handler_clone.subscribe_to_new_heads(
            cache_manager_clone.clone(),
            reorg_manager_clone.clone(),
        ).await {
            Ok(()) => {
                // Connection closed normally
                ws_handler_clone.record_websocket_success().await;
                tracing::info!("WebSocket connection closed, reconnecting...");
            }
            Err(e) => {
                // Connection failed
                tracing::error!("WebSocket error: {}, will retry", e);
                ws_handler_clone.record_websocket_failure().await;
            }
        }

        // Wait before reconnecting
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
});

// Later: cancel the task
ws_task.abort();
```

**Source**: Background task pattern for WebSocket reconnection

---

## Performance Characteristics

### HTTP Client

**Connection Pooling**:
- **Idle timeout**: 30 seconds (aggressive cleanup)
- **Max idle per host**: 100 connections
- **Protocol**: HTTP/2 with adaptive window
- **Reuse**: Connections are reused across requests to same host

**Concurrency**:
- **Semaphore overhead**: O(1) atomic operations
- **Permit acquisition**: Non-blocking (`try_acquire_owned`) or timed (`acquire_owned` with timeout)
- **Adaptive timeouts**: Reduces wait time when permits are scarce
- **Default limit**: 1000 concurrent requests

**Retries**:
- **Max retries**: 2 (total 3 attempts)
- **Backoff**: Exponential 100ms, 200ms
- **Retry conditions**: 5xx errors or network errors
- **No retry**: 4xx errors (client errors)

**Memory**:
- **Per-request overhead**: ~56 bytes (PermitGuard + semaphore)
- **Connection pool**: Managed by reqwest (minimal overhead)
- **Body cloning**: Cheap for `bytes::Bytes` (refcounted)

### WebSocket Client

**Connection Lifecycle**:
- **Connection**: One-time handshake
- **Subscription**: One message sent
- **Message processing**: Event-driven loop
- **Reconnection**: Manual with exponential backoff via failure tracker

**Message Processing**:
- **Parsing**: Single-pass JSON parsing per message
- **Block extraction**: O(1) field lookups
- **Spawning tasks**: O(1) per block (2 tasks: tip update + full fetch)

**Background Fetching**:
- **Block fetch**: Single HTTP request via `eth_getBlockByNumber`
- **Receipts fetch**: Single HTTP request via `eth_getBlockReceipts`
- **Parallel caching**: Logs and receipts cached concurrently via `tokio::join!`

**Failure Tracking**:
- **State size**: ~48 bytes per upstream (WebSocketFailureTracker)
- **Lock contention**: Minimal (only on subscription attempts and failures)
- **Reset check**: O(1) time comparison

**Scalability**:
- **One connection per upstream**: Minimal network overhead
- **Event-driven**: No polling, uses WebSocket events
- **Background tasks**: Non-blocking cache updates

---

## Configuration Recommendations

### HTTP Client Tuning

**Low-Latency Workload** (high QPS, small responses):
```rust
HttpClientConfig {
    concurrent_limit: 2000,           // High concurrency
    permit_timeout_ms: 300,           // Fast timeout
    permit_timeout_scarce_ms: 100,    // Very fast when scarce
    scarce_permit_threshold: 200,
}
```

**High-Throughput Workload** (large responses, batch queries):
```rust
HttpClientConfig {
    concurrent_limit: 500,            // Lower concurrency
    permit_timeout_ms: 2000,          // Longer timeout
    permit_timeout_scarce_ms: 1000,   // Still reasonable when scarce
    scarce_permit_threshold: 50,
}
```

**Conservative** (rate-limited upstream):
```rust
HttpClientConfig {
    concurrent_limit: 100,            // Very conservative
    permit_timeout_ms: 5000,          // Long timeout
    permit_timeout_scarce_ms: 2000,
    scarce_permit_threshold: 20,
}
```

### WebSocket Failure Tracking Tuning

**Aggressive Retry** (fast recovery):
```rust
WebSocketFailureTracker {
    max_consecutive_failures: 5,            // More attempts before giving up
    failure_reset_duration: Duration::from_secs(60),  // Short cooldown
    ..Default::default()
}
```

**Conservative** (avoid hammering broken upstreams):
```rust
WebSocketFailureTracker {
    max_consecutive_failures: 2,            // Give up quickly
    failure_reset_duration: Duration::from_secs(600),  // Long cooldown (10 min)
    ..Default::default()
}
```

---

## Thread Safety

### HTTP Client

**Cloning**:
- `HttpClient` is `Clone` (cheap, just `Arc` increments)
- Safe to clone across threads and tasks
- All fields are `Arc`-wrapped or `Clone`

**Concurrent Access**:
- **Semaphore**: Thread-safe, atomic operations
- **reqwest::Client**: Thread-safe connection pooling
- **No locks**: Lock-free design

**Memory Ordering**:
- Semaphore uses internal atomic operations
- Permit guards ensure proper cleanup even during panics

### WebSocket Client

**Cloning**:
- `WebSocketHandler` fields are `Clone`-able
- `failure_tracker` is `Arc<RwLock<...>>` for shared mutation

**Concurrent Access**:
- **Failure Tracker**: Protected by `RwLock`
  - Multiple readers can check status concurrently
  - Single writer for recording failures/successes
- **WebSocket Stream**: Not thread-safe (single-threaded message processing)

**Task Spawning**:
- Background tasks spawned for tip updates and block fetching
- Each task operates independently
- No shared mutable state between tasks (only through `Arc<CacheManager>`)

---

## Related Documentation

- **Upstream Manager**: `crates/prism-core/src/upstream/manager.rs` - Orchestrates multiple upstreams
- **Upstream Endpoint**: `crates/prism-core/src/upstream/endpoint.rs` - Individual upstream management
- **Health Checking**: `crates/prism-core/src/upstream/health.rs` - Health monitoring
- **Cache Manager**: `docs/components/cache_manager.md` - Cache coordination
- **Reorg Manager**: `docs/components/reorg_manager.md` - Reorganization detection

---

## Summary

The upstream client layer provides robust, high-performance communication with Ethereum RPC providers:

**HTTP Client**:
- **Connection pooling** with HTTP/2 and persistent connections
- **Semaphore-based concurrency limiting** prevents overwhelming upstreams
- **Automatic retries** with exponential backoff for transient failures
- **Adaptive timeouts** reduce wait time during high load
- **RAII permit guards** ensure resources are always released

**WebSocket Client**:
- **Real-time block subscriptions** for immediate cache updates
- **Automatic full block fetching** caches all related data
- **Smart failure tracking** prevents endless reconnection attempts
- **Background task spawning** keeps message processing non-blocking
- **Detailed error messages** aid debugging connection issues

**Key Takeaways**:
1. HTTP client handles up to 1000 concurrent requests with automatic retries (up to 2 retries per request)
2. WebSocket handler automatically fetches and caches full block data on new heads notifications
3. Failure tracking prevents endless retries with 3-failure threshold and 5-minute cooldown
4. Permit guards ensure semaphore permits are always released, even during panics
5. All operations are thread-safe and can be called from multiple tasks concurrently
6. Background tasks spawn for non-blocking cache updates (2 tasks per block: tip update + full fetch)

**Performance Notes**:
- HTTP: ~56 bytes overhead per request, O(1) semaphore operations
- WebSocket: Event-driven (no polling), parallel caching of logs and receipts
- Connection pooling: 100 idle connections per host, 30-second idle timeout
- Retry backoff: 100ms, 200ms (exponential)
