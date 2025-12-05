# Proxy Handlers Documentation

## Overview

The proxy handlers module (`crates/prism-core/src/proxy/handlers/`) implements specialized request handlers for different categories of Ethereum JSON-RPC methods. Each handler is responsible for:

1. Validating and parsing incoming requests
2. Checking the cache for existing data
3. Fetching from upstream when necessary
4. Caching responses for future requests
5. Recording metrics and cache statistics

Handlers are instantiated by the `ProxyEngine` (at `crates/prism-core/src/proxy/engine.rs:57-71`) which routes requests to the appropriate handler based on the RPC method name.

## Handler Architecture

### Common Patterns

All handlers follow a consistent architecture pattern:

**Structure** (`handlers/blocks.rs:16-20`, `handlers/logs.rs:23-27`, `handlers/transactions.rs:25-29`):
```rust
pub struct Handler {
    cache_manager: Arc<CacheManager>,
    upstream_manager: Arc<UpstreamManager>,
    metrics_collector: Arc<MetricsCollector>,
}
```

**Request Flow**:
1. Parse and validate request parameters
2. Check cache for existing data
3. On cache hit: Return cached data with `CacheStatus::Full`
4. On cache miss: Forward to upstream, cache response, return with `CacheStatus::Miss`
5. On partial cache hit (logs only): Fetch missing ranges, merge with cached data, return with `CacheStatus::Partial`

**Error Handling**:
All handlers return `Result<JsonRpcResponse, ProxyError>` where `ProxyError` can be:
- `InvalidRequest(String)` - Malformed parameters or invalid data
- `Upstream(Box<dyn Error>)` - Upstream RPC provider error
- Other variants defined in `crates/prism-core/src/proxy/errors.rs:8-50`

## Block Handlers

**File**: `crates/prism-core/src/proxy/handlers/blocks.rs`

### BlocksHandler::new
**Location**: `blocks.rs:23-31` (`BlocksHandler::new`)

Constructor that creates a new `BlocksHandler` instance.

**Parameters**:
- `cache_manager: Arc<CacheManager>` - Shared cache manager for storing/retrieving blocks
- `upstream_manager: Arc<UpstreamManager>` - Shared upstream manager for RPC requests
- `metrics_collector: Arc<MetricsCollector>` - Shared metrics collector for tracking performance

**Returns**: `Self`

---

### BlocksHandler::handle_block_by_number_request
**Location**: `blocks.rs:42-95` (`handle_block_by_number_request`)

Handles `eth_getBlockByNumber` JSON-RPC requests with intelligent caching for numeric block numbers.

**Parameters**:
- `request: JsonRpcRequest` - The JSON-RPC request containing method and parameters

**Request Format**:
```json
{
  "jsonrpc": "2.0",
  "method": "eth_getBlockByNumber",
  "params": ["0x1234", false],
  "id": 1
}
```

**Logic Flow**:
1. **Special Tag Handling** (`blocks.rs:48-61`): Bypasses cache for special block tags (`latest`, `earliest`, `pending`, `safe`, `finalized`) and forwards directly to upstream since these values change over time.

2. **Parameter Parsing** (`blocks.rs:63-68`): Parses hex-prefixed (`0x123`) or decimal block numbers. Returns `InvalidRequest` error for malformed numbers.

3. **Cache Lookup** (`blocks.rs:70-83`): Checks `cache_manager.get_block_by_number()` for cached block header and body. On hit, converts to JSON via `block_header_and_body_to_json()` and returns immediately.

4. **Upstream Fetch** (`blocks.rs:85-94`): On cache miss, forwards request to upstream and caches the response if successful.

**Caveats**:
- Special block tags are never cached to avoid returning stale data
- Requires both header and body to be cached for a cache hit
- Metrics are recorded for both hits (`blocks.rs:72`) and misses (`blocks.rs:86`)

**Returns**: `JsonRpcResponse` with cache status set appropriately

---

### BlocksHandler::handle_block_by_hash_request
**Location**: `blocks.rs:104-152` (`handle_block_by_hash_request`)

Handles `eth_getBlockByHash` JSON-RPC requests with caching based on block hash.

**Parameters**:
- `request: JsonRpcRequest` - The JSON-RPC request

**Request Format**:
```json
{
  "jsonrpc": "2.0",
  "method": "eth_getBlockByHash",
  "params": ["0xabc...def", false],
  "id": 1
}
```

**Logic Flow**:
1. **Hash Validation** (`blocks.rs:108-126`): Extracts block hash from parameters, decodes hex string (stripping `0x` prefix), and validates it's exactly 32 bytes. Returns `InvalidRequest` for invalid hashes.

2. **Cache Lookup** (`blocks.rs:127-140`): Uses `cache_manager.get_block_by_hash()` to check for cached block. Returns cached JSON on hit.

3. **Upstream Fetch** (`blocks.rs:142-151`): Forwards to upstream on miss and caches response.

**Returns**: `JsonRpcResponse` with block data or error

---

### BlocksHandler::cache_block_from_response (private)
**Location**: `blocks.rs:157-168` (`cache_block_from_response`)

Internal method that extracts block data from JSON response and stores it in cache.

**Parameters**:
- `block_json: &serde_json::Value` - JSON block data from upstream

**Logic**:
1. Converts JSON to `BlockHeader` using `json_block_to_block_header()`
2. Converts JSON to `BlockBody` using `json_block_to_block_body()`
3. Inserts both into cache via `cache_manager.insert_header()` and `insert_body()`
4. Logs warning if body conversion fails but continues with header caching

**Note**: This is called automatically after successful upstream fetches to populate cache.

---

### BlocksHandler::forward_to_upstream (private)
**Location**: `blocks.rs:171-183` (`forward_to_upstream`)

Internal helper that forwards requests to upstream and sets cache status.

**Parameters**:
- `request: &JsonRpcRequest` - Request to forward

**Returns**: `Result<JsonRpcResponse, ProxyError>` with `CacheStatus::Miss` set on success

## Log Handlers

**File**: `crates/prism-core/src/proxy/handlers/logs.rs`

The logs handler implements the most sophisticated caching logic in Prism, supporting partial cache hits and concurrent range fetching.

### LogsHandler::new
**Location**: `logs.rs:30-38` (`LogsHandler::new`)

Constructor for the logs handler.

**Parameters**: Same as other handlers (cache_manager, upstream_manager, metrics_collector)

**Returns**: `Self`

---

### LogsHandler::handle_advanced_logs_request
**Location**: `logs.rs:49-90` (`handle_advanced_logs_request`)

Main entry point for `eth_getLogs` requests with advanced partial-range cache support.

**Request Format**:
```json
{
  "jsonrpc": "2.0",
  "method": "eth_getLogs",
  "params": [{
    "fromBlock": "0x64",
    "toBlock": "0x6e",
    "address": "0x...",
    "topics": ["0x..."]
  }],
  "id": 1
}
```

**Parameters**:
- `request: JsonRpcRequest` - The eth_getLogs request

**Logic Flow**:
1. **Filter Parsing** (`logs.rs:53`): Converts JSON params to internal `LogFilter` structure
2. **Current Tip** (`logs.rs:54`): Gets current blockchain tip for reorg protection
3. **Cache Query** (`logs.rs:67-68`): Calls `cache_manager.get_logs_with_ids()` which returns:
   - `log_records_with_ids`: Vec of cached logs matching the filter
   - `missing_ranges`: Vec of block ranges not in cache

4. **Routing Based on Cache Status** (`logs.rs:70-89`):
   - `(has_logs=true, no_missing=true)` → Full cache hit
   - `(no_logs=true, no_missing=true)` → Empty cache hit (range cached but no logs found)
   - `(has_logs=true, has_missing=true)` → Partial cache hit
   - `(no_logs=true, has_missing=true)` → Complete cache miss

**Caveat**: This is the most complex handler due to range-based caching. See advanced-caching-system.md for full details on the partial-range fulfillment algorithm.

**Returns**: `JsonRpcResponse` with appropriate `CacheStatus`

---

### LogsHandler::parse_log_filter_from_request (private)
**Location**: `logs.rs:93-100` (`parse_log_filter_from_request`)

Validates and parses log filter parameters from request.

**Parameters**:
- `request: &JsonRpcRequest` - The request to parse

**Returns**: `Result<LogFilter, ProxyError>`

**Validation**: Returns `InvalidRequest` if params are missing or malformed

---

### LogsHandler::handle_full_cache_hit (private)
**Location**: `logs.rs:105-145` (`handle_full_cache_hit`)

Handles scenario where all requested logs are in cache.

**Parameters**:
- `log_records_with_ids: Vec<(LogId, LogRecord)>` - Cached logs
- `request: &JsonRpcRequest` - Original request
- `filter: &LogFilter` - Parsed filter

**Performance Optimization** (`logs.rs:121-136`):
- For collections > 1000 logs: Uses `tokio::task::spawn_blocking` with rayon's parallel iterator to avoid blocking async runtime
- For smaller collections: Uses regular iterator to stay async

**Returns**: `JsonRpcResponse` with `CacheStatus::Full` and converted JSON logs

---

### LogsHandler::handle_empty_cache_hit (private)
**Location**: `logs.rs:147-158` (`handle_empty_cache_hit`)

Returns empty array when the requested range is cached but contains no logs.

**Returns**: `JsonRpcResponse` with empty array and `CacheStatus::Empty`

**Note**: This is different from a cache miss - the range has been queried before and we know it's empty.

---

### LogsHandler::handle_partial_cache_hit (private)
**Location**: `logs.rs:163-219` (`handle_partial_cache_hit`)

Most complex handler scenario: some logs are cached, some ranges need fetching.

**Parameters**:
- `log_records_with_ids: Vec<(LogId, LogRecord)>` - Cached logs
- `missing_ranges: Vec<(u64, u64)>` - Block ranges not in cache
- `request: &JsonRpcRequest` - Original request
- `filter: &LogFilter` - Filter parameters

**Logic Flow**:
1. **Convert Cached Logs** (`logs.rs:188-190`): Transform cached records to JSON
2. **Fetch Missing Ranges** (`logs.rs:192-195`): Concurrently fetch missing ranges with max concurrency of 6
3. **Merge Results** (`logs.rs:197`): Combine cached and fetched logs
4. **Sort** (`logs.rs:199-201`): Sort merged results by block number and log index
5. **Detailed Timing** (`logs.rs:192-210`): Records fetch and sort times for performance monitoring

**Performance Metrics** (`logs.rs:203-210`): Debug logs include cached vs fetched counts, timing breakdown

**Returns**: `JsonRpcResponse` with `CacheStatus::Partial` and sorted logs

---

### LogsHandler::fetch_missing_ranges_concurrent (private)
**Location**: `logs.rs:224-315` (`fetch_missing_ranges_concurrent`)

Fetches logs from upstream for missing block ranges using bounded concurrency.

**Parameters**:
- `missing_ranges: Vec<(u64, u64)>` - Ranges to fetch
- `original_filter: &LogFilter` - Base filter to apply
- `request: &JsonRpcRequest` - Original request for ID
- `max_concurrency: usize` - Maximum concurrent requests (default: 6)

**Algorithm** (`logs.rs:237-289`):
1. Creates a stream of fetch tasks, one per range
2. Uses `buffer_unordered(max_concurrency)` to limit concurrent requests
3. Each task:
   - Creates a filter for its specific range (`logs.rs:238-244`)
   - Sends request to upstream (`logs.rs:255`)
   - Caches successful responses (`logs.rs:272`)
   - Records detailed timing metrics (`logs.rs:263-268`)

**Error Handling** (`logs.rs:291-306`): Continues fetching other ranges even if some fail, counts errors

**Performance**: This is critical for partial cache hits - parallel fetching significantly reduces latency vs sequential

**Returns**: `Result<Vec<serde_json::Value>, ProxyError>` - All fetched logs

---

### LogsHandler::extract_block_number (private)
**Location**: `logs.rs:318-323` (`extract_block_number`)

Helper to extract numeric block number from log JSON.

**Parameters**:
- `log: &serde_json::Value` - Log JSON object

**Returns**: `u64` block number (0 if parsing fails)

**Usage**: Used by sort algorithm to order logs

---

### LogsHandler::extract_log_index (private)
**Location**: `logs.rs:326-331` (`extract_log_index`)

Helper to extract log index from log JSON.

**Parameters**:
- `log: &serde_json::Value` - Log JSON object

**Returns**: `u32` log index (0 if parsing fails)

**Usage**: Used for secondary sort key (after block number)

---

### LogsHandler::sort_logs_fast (private)
**Location**: `logs.rs:336-353` (`sort_logs_fast`)

Optimized sorting for logs by pre-extracting numeric keys.

**Parameters**:
- `logs: &mut Vec<serde_json::Value>` - Logs to sort (modified in place)

**Algorithm**:
1. **Extract Keys** (`logs.rs:337-341`): Build parallel vec of (block_num, log_index, original_index)
2. **Sort Keys** (`logs.rs:343`): Sort by block number, then log index
3. **Reorder** (`logs.rs:345-352`): Reconstruct log vec in sorted order using indices

**Performance**: Faster than repeated JSON field access during comparison. Unstable sort is used since we don't need to preserve order of equal elements.

**Caveat**: Uses `std::mem::take` to avoid cloning large JSON values

---

### LogsHandler::handle_cache_miss (private)
**Location**: `logs.rs:356-382` (`handle_cache_miss`)

Handles complete cache miss - no ranges are cached.

**Parameters**:
- `request: &JsonRpcRequest` - Request to forward
- `filter: &LogFilter` - Filter for caching metadata

**Logic** (`logs.rs:361-381`):
1. Records cache miss metric
2. Forwards request to upstream
3. Caches returned logs via `cache_logs_from_response()`
4. Updates cache statistics if needed

**Returns**: `JsonRpcResponse` with `CacheStatus::Miss`

---

### LogsHandler::cache_logs_from_response (private)
**Location**: `logs.rs:387-414` (`cache_logs_from_response`)

Caches logs received from upstream with batch processing.

**Parameters**:
- `logs_array: &[serde_json::Value]` - Array of log JSON objects
- `filter: &LogFilter` - Filter used for the query

**Performance Optimization** (`logs.rs:388-405`):
- For arrays > 1000 logs: Uses `spawn_blocking` with rayon parallel processing
- For smaller arrays: Uses regular iterator

**Caching Steps**:
1. **Convert** (`logs.rs:388-405`): Transform JSON logs to internal `LogRecord` format
2. **Bulk Insert** (`logs.rs:408`): Insert all logs without updating stats
3. **Cache Filter Result** (`logs.rs:409`): Record which logs match this filter
4. **Conditional Stats Update** (`logs.rs:410-412`): Update statistics only if needed (rate-limited)

**Note**: Defers stats updates to avoid performance penalty on every cache operation

---

### LogsHandler::forward_to_upstream (private)
**Location**: `logs.rs:417-429` (`forward_to_upstream`)

Forwards request to upstream manager.

**Parameters**:
- `request: &JsonRpcRequest` - Request to forward

**Returns**: `Result<JsonRpcResponse, ProxyError>` with `CacheStatus::Miss`

## Transaction Handlers

**File**: `crates/prism-core/src/proxy/handlers/transactions.rs`

### TransactionsHandler::new
**Location**: `transactions.rs:32-40` (`TransactionsHandler::new`)

Constructor for transaction handler.

**Parameters**: Standard handler parameters

**Returns**: `Self`

---

### TransactionsHandler::handle_transaction_by_hash_request
**Location**: `transactions.rs:49-91` (`handle_transaction_by_hash_request`)

Handles `eth_getTransactionByHash` requests with transaction caching.

**Request Format**:
```json
{
  "jsonrpc": "2.0",
  "method": "eth_getTransactionByHash",
  "params": ["0xabc...def"],
  "id": 1
}
```

**Parameters**:
- `request: JsonRpcRequest` - The transaction request

**Logic Flow**:
1. **Parameter Extraction** (`transactions.rs:53-62`): Extracts transaction hash from params array
2. **Hash Parsing** (`transactions.rs:64`): Validates and parses 32-byte hash using `parse_hash()`
3. **Cache Lookup** (`transactions.rs:66-79`): Checks transaction cache, converts to JSON on hit
4. **Upstream Fetch** (`transactions.rs:81-90`): Forwards to upstream on miss, caches response

**Returns**: `JsonRpcResponse` with transaction data or null if not found

---

### TransactionsHandler::handle_transaction_receipt_request
**Location**: `transactions.rs:101-143` (`handle_transaction_receipt_request`)

Handles `eth_getTransactionReceipt` requests with receipt and log caching.

**Request Format**:
```json
{
  "jsonrpc": "2.0",
  "method": "eth_getTransactionReceipt",
  "params": ["0xabc...def"],
  "id": 1
}
```

**Parameters**:
- `request: JsonRpcRequest` - The receipt request

**Logic Flow**:
1. **Hash Extraction and Validation** (`transactions.rs:105-116`): Same as transaction handler
2. **Cache Lookup** (`transactions.rs:118-131`): Calls `get_receipt_with_logs()` which returns both receipt and associated logs. Converts to JSON with logs embedded.
3. **Upstream Fetch** (`transactions.rs:133-142`): Forwards on miss, caches both receipt and logs

**Caveat**: Receipt cache requires both the receipt record AND its logs. Partial data is not cached.

**Returns**: `JsonRpcResponse` with receipt data including logs array

---

### TransactionsHandler::parse_hash (private)
**Location**: `transactions.rs:148-158` (`parse_hash`)

Shared hash validation used by both transaction handlers.

**Parameters**:
- `hash_str: &str` - Hex-encoded hash string (e.g., "0xabc...def")

**Validation**:
1. **Hex Decoding** (`transactions.rs:149-150`): Strips `0x` prefix and decodes hex
2. **Length Check** (`transactions.rs:152-154`): Ensures exactly 32 bytes
3. **Array Conversion** (`transactions.rs:156-157`): Converts to fixed-size array

**Returns**: `Result<[u8; 32], ProxyError>`

**Error Cases**: Invalid hex characters or incorrect length

---

### TransactionsHandler::cache_transaction_from_response (private)
**Location**: `transactions.rs:161-165` (`cache_transaction_from_response`)

Caches transaction data from upstream response.

**Parameters**:
- `tx_json: &serde_json::Value` - Transaction JSON from upstream

**Logic**:
1. Converts JSON to `TransactionRecord`
2. Inserts into transaction cache
3. Silently skips if conversion fails

---

### TransactionsHandler::cache_receipt_from_response (private)
**Location**: `transactions.rs:170-207` (`cache_receipt_from_response`)

Caches receipt and associated logs with detailed performance tracking.

**Parameters**:
- `receipt_json: &serde_json::Value` - Receipt JSON from upstream
- `_tx_hash: &[u8; 32]` - Transaction hash (currently unused)

**Logic Flow**:
1. **Extract Logs** (`transactions.rs:175`): Get logs array from receipt
2. **Convert Logs** (`transactions.rs:176-179`): Transform to internal format, timing the operation
3. **Bulk Insert Logs** (`transactions.rs:185`): Insert all logs without stats update
4. **Conditional Stats** (`transactions.rs:189-192`): Update statistics if threshold reached
5. **Cache Receipt** (`transactions.rs:204-205`): Convert and cache receipt record itself

**Performance Logging** (`transactions.rs:195-201`): Detailed timing breakdown for conversion, insertion, and stats

**Note**: Logs are cached first, then receipt, to ensure consistency

---

### TransactionsHandler::forward_to_upstream (private)
**Location**: `transactions.rs:210-223` (`forward_to_upstream`)

Standard upstream forwarding with cache status.

**Parameters**:
- `request: &JsonRpcRequest` - Request to forward

**Returns**: `Result<JsonRpcResponse, ProxyError>` with `CacheStatus::Miss`

## Cache Integration

### Cache Lookup Patterns

**Direct Lookups**:
- `cache_manager.get_block_by_number(u64)` → `Option<(BlockHeader, BlockBody)>`
- `cache_manager.get_block_by_hash(&[u8; 32])` → `Option<(BlockHeader, BlockBody)>`
- `cache_manager.get_transaction(&[u8; 32])` → `Option<TransactionRecord>`
- `cache_manager.get_receipt_with_logs(&[u8; 32])` → `Option<(ReceiptRecord, Vec<LogRecord>)>`

**Range-Based Lookup** (logs only):
- `cache_manager.get_logs_with_ids(&LogFilter, current_tip)` → `(Vec<(LogId, LogRecord)>, Vec<(u64, u64)>)`

### Cache Insertion Patterns

**Individual Inserts**:
- `cache_manager.insert_header(BlockHeader)` - Blocks handler (`blocks.rs:162`)
- `cache_manager.insert_body(BlockBody)` - Blocks handler (`blocks.rs:163`)
- `cache_manager.transaction_cache.insert_transaction(TransactionRecord)` - Transactions handler (`transactions.rs:163`)
- `cache_manager.insert_receipt(ReceiptRecord)` - Transactions handler (`transactions.rs:205`)

**Bulk Operations** (logs only):
- `cache_manager.insert_logs_bulk_no_stats(Vec<(LogId, LogRecord)>)` - Deferred stats update for performance (`logs.rs:408`, `transactions.rs:185`)
- `cache_manager.cache_exact_result(&LogFilter, Vec<LogId>)` - Cache filter-to-logs mapping (`logs.rs:409`)

### Stats Management

**On-Demand Updates**:
```rust
if cache_manager.should_update_stats().await {
    cache_manager.update_stats_on_demand().await;
}
```

This pattern (seen in `logs.rs:410-412` and `transactions.rs:189-192`) rate-limits statistics updates to avoid performance degradation during bulk operations.

## Error Handling

### Error Types

**From `ProxyError` enum** (`crates/prism-core/src/proxy/errors.rs:8-50`):

1. **InvalidRequest(String)** - Used when:
   - Missing parameters (`blocks.rs:48-50`, `logs.rs:94-96`, `transactions.rs:53-57`)
   - Invalid parameter format (`blocks.rs:52-56`, `transactions.rs:59-62`)
   - Invalid block number (`blocks.rs:63-68`)
   - Invalid hash format (`blocks.rs:117-118`, `transactions.rs:149-150`)
   - Invalid hash length (`blocks.rs:120-122`, `transactions.rs:152-154`)

2. **Upstream(Box<dyn Error>)** - Used when:
   - Upstream RPC request fails (`blocks.rs:181`, `logs.rs:427`, `transactions.rs:220`)
   - Network errors, timeouts, invalid responses from providers

3. **Validation(ValidationError)** - Used when:
   - Request validation fails in engine (`engine.rs:97`)
   - Invalid JSON-RPC version, unsupported methods, etc.

### Error Propagation

All handlers use the `?` operator to propagate errors up to the `ProxyEngine`, which converts them to appropriate JSON-RPC error responses.

**Example error flow**:
1. Handler returns `Err(ProxyError::InvalidRequest(...))`
2. Engine catches error in `process_request()`
3. Converts to `JsonRpcResponse` with error field populated
4. Returns to HTTP server layer for response serialization

## RPC Method Mapping

**Defined in** `crates/prism-core/src/proxy/engine.rs:111-126`

| Ethereum RPC Method | Handler | Function | Cache Type |
|---------------------|---------|----------|------------|
| `eth_getLogs` | LogsHandler | `handle_advanced_logs_request` | Range-based with partial fulfillment |
| `eth_getBlockByNumber` | BlocksHandler | `handle_block_by_number_request` | Block number index (except special tags) |
| `eth_getBlockByHash` | BlocksHandler | `handle_block_by_hash_request` | Block hash index |
| `eth_getTransactionByHash` | TransactionsHandler | `handle_transaction_by_hash_request` | Transaction hash index |
| `eth_getTransactionReceipt` | TransactionsHandler | `handle_transaction_receipt_request` | Receipt hash index + embedded logs |
| `eth_blockNumber` | ProxyEngine | `forward_to_upstream` | No caching (always fresh) |
| `eth_chainId` | ProxyEngine | `forward_to_upstream` | No caching (static value) |
| `eth_gasPrice` | ProxyEngine | `forward_to_upstream` | No caching (volatile) |
| `eth_getBalance` | ProxyEngine | `forward_to_upstream` | No caching (state query) |
| `net_version` | ProxyEngine | `forward_to_upstream` | No caching (static value) |

**Allowed Methods List**: Defined in `crates/prism-core/src/types.rs:5-16`

### Method Selection Criteria

**Cached Methods**:
- Historical data (blocks, transactions, receipts, logs)
- Immutable once finalized
- Frequently requested
- Expensive to compute

**Pass-Through Methods**:
- Real-time data (latest block, gas price, balances)
- Static configuration (chain ID, network version)
- State queries that vary by block/account

## Performance Considerations

### Concurrency Controls

**Log Range Fetching** (`logs.rs:237`, `logs.rs:294`):
- Max concurrency: 6 simultaneous requests
- Prevents overwhelming upstream providers
- Uses `buffer_unordered()` for efficient task management

### Async Runtime Compatibility

**Large Collection Processing** (`logs.rs:121-136`, `logs.rs:388-405`):
- Collections > 1000 items: Uses `tokio::task::spawn_blocking` to avoid blocking async runtime
- Smaller collections: Direct async processing
- Prevents rayon's thread pool from starving tokio tasks

### Memory Efficiency

**Log Sorting** (`logs.rs:336-353`):
- Pre-extracts numeric keys to avoid repeated JSON parsing
- Uses `std::mem::take` instead of cloning large JSON values
- Unstable sort for better performance

**Zero-Copy Patterns**:
- References (`&JsonRpcRequest`) instead of clones where possible
- Arc sharing for cache/upstream/metrics managers
- Deferred stats updates to batch operations

### Metrics Collection

**Cache Hit/Miss Recording**:
- `metrics_collector.record_cache_hit(method)` - Async, non-blocking
- `metrics_collector.record_cache_miss(method)` - Synchronous, fast path
- Method name passed as string for per-method tracking

**Example locations**:
- Block handler: `blocks.rs:72`, `blocks.rs:86`, `blocks.rs:129`, `blocks.rs:143`
- Log handler: `logs.rs:119`, `logs.rs:184`, `logs.rs:363`
- Transaction handler: `transactions.rs:68`, `transactions.rs:82`, `transactions.rs:120`, `transactions.rs:134`

## Related Documentation

- **Advanced Caching System**: `docs/advanced-caching-system.md` - Detailed explanation of log cache range fulfillment
- **Cache Manager**: `docs/components/cache_manager.md` - Cache manager API and architecture
- **Converter Module**: `docs/components/converter.md` - JSON ↔ internal type conversions
- **Log Cache**: `docs/components/log_cache.md` - Log cache internals
- **Block Cache**: `docs/components/block_cache.md` - Block cache implementation
- **Transaction Cache**: `docs/components/transaction_cache.md` - Transaction/receipt cache

## Summary

The proxy handlers module provides specialized, high-performance request handling for Ethereum RPC methods with intelligent caching:

- **BlocksHandler**: Simple key-value caching by block number/hash
- **LogsHandler**: Advanced range-based caching with partial fulfillment and concurrent fetching
- **TransactionsHandler**: Hash-based caching for transactions and receipts

All handlers share common patterns for error handling, metrics collection, and cache integration while implementing method-specific optimizations for their use cases.
