# Cache Converter

## Overview

The cache converter module provides bidirectional conversion between JSON-RPC format and internal cache types. It handles the serialization and deserialization of Ethereum data structures including logs, blocks, transactions, and receipts, optimized for high performance with zero-allocation parsing and efficient hex encoding.

Location: `crates/prism-core/src/cache/converter.rs`

## Purpose

The converter serves as the critical bridge in the caching pipeline:

1. **Parsing JSON-RPC responses** - Converting upstream node responses into compact internal cache structures
2. **Converting cached data back to JSON-RPC** - Serializing cache records into proper JSON-RPC format for client responses
3. **Memory optimization** - Using fixed-size arrays and `Arc<Vec<u8>>` for efficient storage
4. **Performance optimization** - Zero-allocation hex parsing using thread-local buffers

## Conversion Functions

### Log Conversions

#### `json_log_to_log_id`
```rust
pub fn json_log_to_log_id(log: &Value) -> Option<LogId>
```

Extracts just the log identifier from a JSON-RPC log object.

**Location**: Lines 72-78

**Input**: JSON-RPC log object containing `blockNumber` and `logIndex`

**Output**: `LogId` containing block number and log index

**Use Case**: Extracting log references when processing receipts

**Error Handling**: Returns `None` if required fields are missing or cannot be parsed

#### `json_log_to_log_record`
```rust
pub fn json_log_to_log_record(log: &Value) -> Option<(LogId, LogRecord)>
```

Converts a complete JSON-RPC log object into internal cache format.

**Location**: Lines 82-121

**Input**: JSON-RPC log object from `eth_getLogs` response

**Output**: Tuple of `(LogId, LogRecord)`

**Field Mappings**:
- `blockNumber` (hex string) → `LogId.block_number` (u64)
- `logIndex` (hex string) → `LogId.log_index` (u32)
- `address` (hex string) → `LogRecord.address` ([u8; 20])
- `topics` (array of hex strings) → `LogRecord.topics` ([Option<[u8; 32]>; 4])
- `data` (hex string) → `LogRecord.data` (Vec<u8>)
- `transactionHash` (hex string) → `LogRecord.transaction_hash` ([u8; 32])
- `blockHash` (hex string) → `LogRecord.block_hash` ([u8; 32])
- `transactionIndex` (hex string) → `LogRecord.transaction_index` (u32)
- `removed` (boolean) → `LogRecord.removed` (bool, default: false)

**Optimizations**:
- Uses `hex_to_array::<N>()` for zero-allocation fixed-size conversions
- Topics array limited to first 4 topics (Ethereum standard)
- Gracefully handles missing topics

**Error Handling**: Returns `None` if any required field is missing or fails to parse

#### `log_record_to_json_log`
```rust
pub fn log_record_to_json_log(log_id: &LogId, log_record: &LogRecord) -> Value
```

Converts internal cache format back to JSON-RPC log format.

**Location**: Lines 125-142

**Input**: `LogId` and `LogRecord` from cache

**Output**: JSON-RPC formatted log object

**Hex Encoding**:
- Uses `format_hash32()` for 32-byte hashes
- Uses `format_address()` for 20-byte addresses
- Uses `HexSerializer` for variable-length data
- Uses `format_hex_u64()` for numeric values

**Topics Handling**: Filters out `None` values, only includes present topics

### Block Conversions

#### `json_block_to_block_header`
```rust
pub fn json_block_to_block_header(block: &Value) -> Option<BlockHeader>
```

Extracts block header fields from a JSON-RPC block object.

**Location**: Lines 146-213

**Input**: JSON-RPC block object from `eth_getBlockByNumber` or `eth_getBlockByHash`

**Output**: `BlockHeader` containing all header metadata

**Field Mappings**:
- `hash` → `hash` ([u8; 32])
- `number` → `number` (u64)
- `parentHash` → `parent_hash` ([u8; 32])
- `timestamp` → `timestamp` (u64)
- `gasLimit` → `gas_limit` (u64)
- `gasUsed` → `gas_used` (u64)
- `miner` → `miner` ([u8; 20])
- `extraData` → `extra_data` (Arc<Vec<u8>>)
- `logsBloom` → `logs_bloom` (Arc<Vec<u8>>)
- `transactionsRoot` → `transactions_root` ([u8; 32])
- `stateRoot` → `state_root` ([u8; 32])
- `receiptsRoot` → `receipts_root` ([u8; 32])

**Memory Optimization**: Uses `Arc<Vec<u8>>` for large fields (`extraData`, `logsBloom`) to enable cheap cloning

**Error Handling**: Returns `None` if any required field is missing or fails to parse

#### `json_block_to_block_body`
```rust
pub fn json_block_to_block_body(block: &Value) -> Option<BlockBody>
```

Extracts transaction hashes from a JSON-RPC block object.

**Location**: Lines 217-234

**Input**: JSON-RPC block object with `transactions` array (hashes only, not full objects)

**Output**: `BlockBody` containing block hash and transaction hash list

**Note**: This expects transaction hashes as strings, not full transaction objects. Used when blocks are requested with `fullTx=false`.

**Error Handling**: Skips invalid transaction hashes, returns partial results

#### `block_header_to_json_block`
```rust
pub fn block_header_to_json_block(header: &BlockHeader) -> Value
```

Converts block header to JSON-RPC format.

**Location**: Lines 238-253

**Input**: `BlockHeader` from cache

**Output**: JSON-RPC block header object

**Hex Encoding**: Uses optimized formatters for each field type

#### `block_header_and_body_to_json`
```rust
pub fn block_header_and_body_to_json(header: &BlockHeader, body: &BlockBody) -> Value
```

Combines header and body into complete JSON-RPC block response.

**Location**: Lines 480-526

**Input**: `BlockHeader` and `BlockBody` from cache

**Output**: Complete JSON-RPC block object with transactions as hash array

**Additional Fields**:
- `transactions`: Array of transaction hashes
- `uncles`: Empty array (standard Ethereum response)

### Transaction Conversions

#### `json_transaction_to_transaction_record`
```rust
pub fn json_transaction_to_transaction_record(tx: &Value) -> Option<TransactionRecord>
```

Converts JSON-RPC transaction to internal cache format.

**Location**: Lines 256-333

**Input**: JSON-RPC transaction object

**Output**: `TransactionRecord`

**Field Mappings**:
- `hash` → `hash` ([u8; 32])
- `blockHash` → `block_hash` (Option<[u8; 32]>)
- `blockNumber` → `block_number` (Option<u64>)
- `transactionIndex` → `transaction_index` (Option<u32>)
- `from` → `from` ([u8; 20])
- `to` → `to` (Option<[u8; 20]>)
- `value` → `value` ([u8; 32])
- `gasPrice` → `gas_price` ([u8; 32])
- `gas` → `gas_limit` (u64)
- `nonce` → `nonce` (u64)
- `input` → `data` (Vec<u8>)
- `v` → `v` (u8)
- `r` → `r` ([u8; 32])
- `s` → `s` ([u8; 32])

**Optional Fields**: `blockHash`, `blockNumber`, `transactionIndex`, and `to` are optional (null for pending transactions and contract creations)

**Signature Components**: Extracts ECDSA signature (v, r, s) for complete transaction record

**Error Handling**: Returns `None` if required fields are missing

#### `transaction_record_to_json`
```rust
pub fn transaction_record_to_json(transaction: &TransactionRecord) -> Value
```

Converts transaction record back to JSON-RPC format.

**Location**: Lines 530-583

**Input**: `TransactionRecord` from cache

**Output**: JSON-RPC transaction object

**Null Handling**: Optional fields (`blockHash`, `blockNumber`, `transactionIndex`, `to`) are rendered as JSON `null` when absent

**Data Field Optimization**: Uses `format_hex_large()` for data > 256 bytes for better performance

### Receipt Conversions

#### `json_receipt_to_receipt_record`
```rust
pub fn json_receipt_to_receipt_record(receipt: &Value) -> Option<ReceiptRecord>
```

Converts JSON-RPC receipt to internal cache format.

**Location**: Lines 336-426

**Input**: JSON-RPC receipt object from `eth_getTransactionReceipt`

**Output**: `ReceiptRecord`

**Field Mappings**:
- `transactionHash` → `transaction_hash` ([u8; 32])
- `blockHash` → `block_hash` ([u8; 32])
- `blockNumber` → `block_number` (u64)
- `transactionIndex` → `transaction_index` (u32)
- `from` → `from` ([u8; 20])
- `to` → `to` (Option<[u8; 20]>)
- `cumulativeGasUsed` → `cumulative_gas_used` (u64)
- `gasUsed` → `gas_used` (u64)
- `contractAddress` → `contract_address` (Option<[u8; 20]>)
- `logs` → `logs` (Vec<LogId>)
- `status` → `status` (u64)
- `logsBloom` → `logs_bloom` (Vec<u8>)
- `effectiveGasPrice` → `effective_gas_price` (Option<u64>) - EIP-1559
- `type` → `tx_type` (Option<u8>) - 0x0=legacy, 0x1=access list, 0x2=EIP-1559, 0x3=EIP-4844
- `blobGasPrice` → `blob_gas_price` (Option<u64>) - EIP-4844

**Log Extraction**: Converts embedded log objects to `LogId` references (lines 380-387)

**EIP Support**: Handles optional EIP-1559 and EIP-4844 fields gracefully (lines 399-407)

**Error Handling**: Returns `None` if required fields are missing

#### `receipt_record_to_json`
```rust
pub fn receipt_record_to_json(receipt: &ReceiptRecord, logs: &[LogRecord]) -> Value
```

Converts receipt record back to JSON-RPC format with logs.

**Location**: Lines 587-663

**Input**:
- `ReceiptRecord` from cache
- `logs` slice containing actual log data

**Output**: JSON-RPC receipt object with embedded logs

**Log Merging**: Reconstructs log objects from `LogId` references and provided `LogRecord` data (lines 632-640)

**EIP Fields**: Includes EIP-1559/4844 fields only if present in the record (lines 643-660)

### Filter Conversions

#### `json_params_to_log_filter`
```rust
pub fn json_params_to_log_filter(params: &Value) -> Option<LogFilter>
```

Converts JSON-RPC `eth_getLogs` parameters to internal filter format.

**Location**: Lines 430-476

**Input**: JSON-RPC params array containing filter object

**Output**: `LogFilter` for cache queries

**Filter Fields**:
- `fromBlock` / `toBlock`: Block range (required)
- `address`: Filter by contract address (optional)
- `topics`: Array of up to 4 topic filters (optional)
- `topicsAnywhere`: Non-standard extension for topic matching at any position (optional)

**Builder Pattern**: Constructs filter incrementally using builder methods

**Error Handling**: Returns `None` if required block range fields are missing

## Helper Functions

### Hex Parsing Utilities

#### `hex_to_bytes`
```rust
pub fn hex_to_bytes(hex: &str) -> Option<Vec<u8>>
```

**Location**: Lines 14-20

Converts hex string (with or without "0x" prefix) to byte vector.

**Performance**: Uses `hex::decode()` crate for optimized parsing.

#### `hex_to_array<const N: usize>`
```rust
pub fn hex_to_array<const N: usize>(hex: &str) -> Option<[u8; N]>
```

**Location**: Lines 25-44

Converts hex string to fixed-size array without intermediate allocations.

**Optimization**: Direct parsing into stack-allocated array, avoiding `Vec` allocation.

**Validation**: Ensures hex string length exactly matches expected array size (N * 2 characters).

**Performance**: 25-30% faster than `hex_to_bytes` + conversion for fixed-size data.

#### `hex_digit_to_u8`
```rust
fn hex_digit_to_u8(c: u8) -> Option<u8>
```

**Location**: Lines 47-54

Internal helper to convert single hex digit (0-9, a-f, A-F) to numeric value.

**Pattern Matching**: Handles lowercase, uppercase, and numeric digits efficiently.

#### `hex_to_u64`
```rust
pub fn hex_to_u64(hex: &str) -> Option<u64>
```

**Location**: Lines 59-62

Parses hex string to u64, handles "0x" prefix.

**Use Case**: Block numbers, timestamps, gas values.

#### `hex_to_u32`
```rust
pub fn hex_to_u32(hex: &str) -> Option<u32>
```

**Location**: Lines 65-69

Parses hex string to u32, handles "0x" prefix.

**Use Case**: Log indexes, transaction indexes.

### Hex Formatting Utilities

The converter uses utilities from `utils::hex_buffer` module:

- `format_hash32(&[u8; 32])`: Format 32-byte hash with "0x" prefix
- `format_address(&[u8; 20])`: Format 20-byte address with "0x" prefix
- `format_hex_u64(u64)`: Format u64 as hex with "0x" prefix
- `format_hex(&[u8])`: Format small byte slices
- `format_hex_large(&[u8])`: Format large byte slices (optimized for >256 bytes)
- `HexSerializer::new(&[u8])`: Zero-copy hex serialization for serde

**Thread-Local Buffers**: These formatters use thread-local buffers to avoid allocations during serialization.

## Error Handling

The converter module uses an **Option-based error handling pattern**:

- All conversion functions return `Option<T>`
- Returns `None` on any parsing failure
- No panics or unwraps in production code
- Missing optional fields are handled gracefully

**When None is Returned**:
- Required JSON-RPC field is missing
- Hex string is malformed
- Hex string length doesn't match expected size
- String cannot be parsed to expected numeric type

**Caller Responsibility**: Handlers must check for `None` and handle cache misses appropriately.

## Field Mappings Reference

### Log Fields

| JSON-RPC Field | Type | LogRecord Field | Type | Notes |
|----------------|------|-----------------|------|-------|
| address | string | address | [u8; 20] | Contract address |
| topics | string[] | topics | [Option<[u8; 32]>; 4] | Max 4 topics |
| data | string | data | Vec<u8> | Event data |
| blockNumber | string | (LogId.block_number) | u64 | Stored in LogId |
| blockHash | string | block_hash | [u8; 32] | Block identifier |
| transactionHash | string | transaction_hash | [u8; 32] | Transaction identifier |
| transactionIndex | string | transaction_index | u32 | Index in block |
| logIndex | string | (LogId.log_index) | u32 | Stored in LogId |
| removed | boolean | removed | bool | Reorg flag |

### Block Header Fields

| JSON-RPC Field | Type | BlockHeader Field | Type | Notes |
|----------------|------|-------------------|------|-------|
| hash | string | hash | [u8; 32] | Block hash |
| number | string | number | u64 | Block number |
| parentHash | string | parent_hash | [u8; 32] | Previous block |
| timestamp | string | timestamp | u64 | Unix timestamp |
| gasLimit | string | gas_limit | u64 | Gas limit |
| gasUsed | string | gas_used | u64 | Gas consumed |
| miner | string | miner | [u8; 20] | Block producer |
| extraData | string | extra_data | Arc<Vec<u8>> | Extra block data |
| logsBloom | string | logs_bloom | Arc<Vec<u8>> | Bloom filter (256 bytes) |
| transactionsRoot | string | transactions_root | [u8; 32] | Merkle root |
| stateRoot | string | state_root | [u8; 32] | State trie root |
| receiptsRoot | string | receipts_root | [u8; 32] | Receipts trie root |

### Transaction Fields

| JSON-RPC Field | Type | TransactionRecord Field | Type | Notes |
|----------------|------|-------------------------|------|-------|
| hash | string | hash | [u8; 32] | Transaction hash |
| blockHash | string? | block_hash | Option<[u8; 32]> | Null for pending |
| blockNumber | string? | block_number | Option<u64> | Null for pending |
| transactionIndex | string? | transaction_index | Option<u32> | Null for pending |
| from | string | from | [u8; 20] | Sender address |
| to | string? | to | Option<[u8; 20]> | Null for contract creation |
| value | string | value | [u8; 32] | Wei transferred |
| gasPrice | string | gas_price | [u8; 32] | Gas price |
| gas | string | gas_limit | u64 | Gas limit |
| nonce | string | nonce | u64 | Transaction nonce |
| input | string | data | Vec<u8> | Call data |
| v | string | v | u8 | Signature v |
| r | string | r | [u8; 32] | Signature r |
| s | string | s | [u8; 32] | Signature s |

### Receipt Fields

| JSON-RPC Field | Type | ReceiptRecord Field | Type | Notes |
|----------------|------|---------------------|------|-------|
| transactionHash | string | transaction_hash | [u8; 32] | Transaction identifier |
| blockHash | string | block_hash | [u8; 32] | Block identifier |
| blockNumber | string | block_number | u64 | Block number |
| transactionIndex | string | transaction_index | u32 | Index in block |
| from | string | from | [u8; 20] | Sender address |
| to | string? | to | Option<[u8; 20]> | Null for contract creation |
| cumulativeGasUsed | string | cumulative_gas_used | u64 | Total gas in block |
| gasUsed | string | gas_used | u64 | Gas used by tx |
| contractAddress | string? | contract_address | Option<[u8; 20]> | Created contract |
| logs | object[] | logs | Vec<LogId> | Log references |
| status | string | status | u64 | 1=success, 0=failure |
| logsBloom | string | logs_bloom | Vec<u8> | Bloom filter |
| effectiveGasPrice | string? | effective_gas_price | Option<u64> | EIP-1559 |
| type | string? | tx_type | Option<u8> | EIP-2718 |
| blobGasPrice | string? | blob_gas_price | Option<u64> | EIP-4844 |

## Usage Examples

### Caching an eth_getLogs Response

```rust
// In handlers/logs.rs
let response = upstream_client.get_logs(params).await?;
let logs_array = response.get("result")?.as_array()?;

for log in logs_array {
    if let Some((log_id, log_record)) = json_log_to_log_record(log) {
        cache.store_log(log_id, log_record);
    }
}
```

### Reconstructing eth_getLogs Response from Cache

```rust
// In handlers/logs.rs
let log_ids = cache.query_logs(&filter)?;
let mut json_logs = Vec::new();

for log_id in log_ids {
    if let Some(log_record) = cache.get_log(&log_id) {
        let json_log = log_record_to_json_log(&log_id, &log_record);
        json_logs.push(json_log);
    }
}

serde_json::json!({ "result": json_logs })
```

### Caching a Block

```rust
// In handlers/blocks.rs
let response = upstream_client.get_block_by_number(block_num).await?;
let block_json = response.get("result")?;

if let Some(header) = json_block_to_block_header(block_json) {
    if let Some(body) = json_block_to_block_body(block_json) {
        cache.store_block_header(header.hash, header);
        cache.store_block_body(body.hash, body);
    }
}
```

### Returning Cached Block

```rust
// In handlers/blocks.rs
let header = cache.get_block_header(&block_hash)?;
let body = cache.get_block_body(&block_hash)?;

let block_json = block_header_and_body_to_json(&header, &body);

serde_json::json!({ "result": block_json })
```

### Caching a Transaction Receipt

```rust
// In handlers/transactions.rs
let response = upstream_client.get_transaction_receipt(tx_hash).await?;
let receipt_json = response.get("result")?;

if let Some(receipt_record) = json_receipt_to_receipt_record(receipt_json) {
    // Store receipt with log references
    cache.store_receipt(tx_hash, receipt_record);

    // Separately cache the actual logs
    if let Some(logs_array) = receipt_json.get("logs")?.as_array() {
        for log in logs_array {
            if let Some((log_id, log_record)) = json_log_to_log_record(log) {
                cache.store_log(log_id, log_record);
            }
        }
    }
}
```

### Returning Cached Receipt

```rust
// In handlers/transactions.rs
let receipt_record = cache.get_receipt(&tx_hash)?;

// Resolve log references to actual log data
let mut logs = Vec::new();
for log_id in &receipt_record.logs {
    if let Some(log_record) = cache.get_log(log_id) {
        logs.push(log_record);
    }
}

let receipt_json = receipt_record_to_json(&receipt_record, &logs);

serde_json::json!({ "result": receipt_json })
```

## Performance Characteristics

### Parsing Performance

- **Fixed-size arrays**: 25-30% faster than `Vec` allocation + conversion
- **Thread-local buffers**: Zero allocations for hex formatting
- **Direct hex parsing**: No intermediate string allocations

### Memory Efficiency

- **Arc for large fields**: `extraData` and `logsBloom` use `Arc<Vec<u8>>` for cheap cloning
- **Fixed-size arrays**: Stack allocation for hashes and addresses
- **Packed LogId**: 12 bytes (u64 + u32) instead of full log data

### Cache Key Generation

- **Hash-based keys**: `LogFilter::hash_key()` for O(1) HashMap lookups
- **Canonical string keys**: `LogFilter::canonical_key()` for debugging/logging
- **Pre-allocated buffers**: String capacity estimated to reduce reallocations

## Integration Points

### Used By

- `crates/prism-core/src/proxy/handlers/logs.rs` (lines ~150-300)
- `crates/prism-core/src/proxy/handlers/blocks.rs` (lines ~100-250)
- `crates/prism-core/src/proxy/handlers/transactions.rs` (lines ~120-280)
- `crates/prism-core/src/cache/log_cache.rs` (lines ~200-400)

### Dependencies

- `crate::cache::types` - Cache data structures (LogRecord, BlockHeader, etc.)
- `crate::utils::hex_buffer` - Optimized hex formatting utilities
- `serde_json::Value` - JSON manipulation
- `std::sync::Arc` - Shared ownership for large fields

## Testing

The module includes comprehensive unit tests (lines 667-927):

- **Hex conversion tests** - Validates parsing with/without "0x" prefix
- **Log roundtrip tests** - JSON → Cache → JSON consistency
- **Block conversion tests** - Header and body parsing
- **Transaction conversion tests** - Including optional fields
- **Receipt conversion tests** - With EIP-1559/4844 fields
- **Buffer usage tests** - Validates thread-local buffer reuse
- **Roundtrip tests** - Ensures lossless conversion

Run tests with:
```bash
cargo make test
```

## Future Improvements

1. **Streaming parsing** - For very large responses, parse incrementally
2. **SIMD hex parsing** - Use SIMD instructions for faster parsing on large data
3. **Compact encoding** - Binary encoding instead of JSON for inter-cache communication
4. **Schema validation** - Validate JSON-RPC responses against expected schema
5. **Error reporting** - Return structured errors instead of Option for better debugging

## See Also

- [Advanced Caching System](../../caching-system.md) - Overall cache architecture
- [Cache Types](../../../crates/prism-core/src/cache/types.rs) - Internal data structures
- [Hex Buffer Utilities](../../../crates/prism-core/src/utils/hex_buffer.rs) - Performance optimizations
