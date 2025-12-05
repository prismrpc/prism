# Hex Buffer Utilities

## Overview

The hex buffer module provides high-performance hexadecimal encoding and decoding utilities optimized for Ethereum RPC operations. It eliminates memory allocations through thread-local buffer reuse, achieving 25-30% performance improvements over standard formatting approaches. The module is critical to Prism's performance, as nearly every RPC response involves converting between binary data and hexadecimal strings.

Location: `crates/prism-core/src/utils/hex_buffer.rs`

## Purpose

The hex buffer utilities serve several key functions in the caching and proxy pipeline:

1. **Eliminate allocations** - Thread-local buffers are reused across calls, avoiding repeated heap allocations
2. **Fast hex encoding** - Convert bytes to hex strings with "0x" prefix for JSON-RPC responses
3. **Fast hex decoding** - Parse hex strings from JSON-RPC requests into binary data
4. **Type-specific optimization** - Specialized functions for common Ethereum data types (hashes, addresses, u64 values)
5. **Direct serialization** - Write hex data directly to JSON output without intermediate string allocation

## Thread-Local Buffers

The module maintains four thread-local buffers, each optimized for different use cases:

### HEX_BUFFER

**Location**: Lines 5

**Purpose**: Primary buffer for general hex formatting operations

**Initial Capacity**: 66 bytes (enough for "0x" + 32-byte hash = 66 characters)

**Used By**:
- `format_hex()` - General byte slice formatting
- `format_hex_u64()` - Numeric value formatting
- `format_hash32()` - 32-byte hash formatting
- `format_address()` - 20-byte address formatting
- `with_hex_format()` - Callback-based formatting
- `with_hex_u64()` - Callback-based u64 formatting
- `with_hash32()` - Callback-based hash formatting
- `with_address()` - Callback-based address formatting

**Thread Safety**: Each thread gets its own independent buffer via `thread_local!` macro

### HEX_BUFFER_ALT

**Location**: Lines 8

**Purpose**: Secondary buffer for cases requiring two hex strings simultaneously

**Initial Capacity**: 66 bytes

**Used By**: `format_hex_alt()` - Alternative formatting to avoid buffer conflicts

**Use Case**: When you need two different hex strings in the same expression without the first buffer being overwritten

**Example Scenario**:
```rust
// Without ALT buffer, this would fail:
let combined = format!("{} and {}", format_hex(&data1), format_hex(&data2));
// Second call overwrites first buffer before format! reads it

// With ALT buffer, this works:
let combined = format!("{} and {}", format_hex(&data1), format_hex_alt(&data2));
```

### HEX_BUFFER_LARGE

**Location**: Lines 11

**Purpose**: Dedicated buffer for large hex data (transaction input, log data)

**Initial Capacity**: 1024 bytes (512 bytes of data in hex)

**Used By**: `format_hex_large()` - Large data formatting

**Auto-Resize**: Grows as needed if data exceeds 512 bytes (lines 280-284)

**Performance**: Pre-allocated capacity reduces reallocations for typical transaction input sizes

### PARSE_BUFFER

**Location**: Lines 20-23 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Buffer for parsing hex strings into binary data

**Initial Capacity**: 64 bytes

**Used By**:
- `parse_hex_u64()` - Parse to u64
- `parse_hex_u32()` - Parse to u32
- `parse_hex_bytes()` - Parse to byte vector
- `parse_hex_array()` - Parse to fixed-size array

**Strategy**: Stores hex string without "0x" prefix for parsing operations

## Callback-Based API

The callback-based functions provide the most efficient approach when you only need temporary access to the hex string. They execute your callback with a reference to the buffer, avoiding any string cloning.

### with_hex_format

```rust
pub fn with_hex_format<T, F>(bytes: &[u8], f: F) -> T
where
    F: FnOnce(&str) -> T
```

**Location**: Lines 38-53 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Format arbitrary byte slices as hex, execute callback with result

**Input**:
- `bytes` - Byte slice to format
- `f` - Callback function that receives hex string reference

**Output**: Returns whatever type `T` your callback returns

**Hex Format**: "0x" prefix + lowercase hex digits

**Use Case**: When you need to use hex string temporarily without owning it

**Example**:
```rust
// Parse as u64 without allocating string
let block_num = with_hex_format(&block_bytes, |hex| {
    u64::from_str_radix(&hex[2..], 16).unwrap()
});

// Compare against expected value
let is_match = with_hex_format(&hash, |hex| {
    hex == expected_hash
});
```

### with_hex_u64

```rust
pub fn with_hex_u64<T, F>(value: u64, f: F) -> T
where
    F: FnOnce(&str) -> T
```

**Location**: Lines 58-75 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Format u64 values as hex, optimized for block numbers and numeric values

**Input**:
- `value` - u64 to format
- `f` - Callback function

**Output**: Returns callback result

**Special Case**: Zero is formatted as "0x0" (line 67)

**Format**: Minimal hex representation without leading zeros (e.g., `0x3e8` for 1000, not `0x00000003e8`)

**Use Case**: Block numbers, timestamps, gas values, log indexes

**Example**:
```rust
// Include in JSON without allocating
with_hex_u64(block_number, |hex| {
    json_builder.add_field("blockNumber", hex)
});
```

### with_hash32

```rust
pub fn with_hash32<T, F>(hash: &[u8; 32], f: F) -> T
where
    F: FnOnce(&str) -> T
```

**Location**: Lines 80-102 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Format fixed-size 32-byte hashes (transaction hashes, block hashes)

**Input**:
- `hash` - 32-byte array reference
- `f` - Callback function

**Output**: Returns callback result

**Capacity Check**: Ensures buffer has 66-byte capacity before formatting (lines 88-92)

**Format**: Always produces exactly 66 characters ("0x" + 64 hex digits)

**Use Case**: Transaction hashes, block hashes, Merkle roots, state roots

**Performance**: Most frequently called function in Ethereum RPC operations

**Example**:
```rust
// Look up in cache without string allocation
let cached = with_hash32(&tx_hash, |hex| {
    cache.get(hex)
});
```

### with_address

```rust
pub fn with_address<T, F>(address: &[u8; 20], f: F) -> T
where
    F: FnOnce(&str) -> T
```

**Location**: Lines 107-128 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Format Ethereum addresses (20-byte values)

**Input**:
- `address` - 20-byte array reference
- `f` - Callback function

**Output**: Returns callback result

**Capacity Check**: Ensures 42-byte capacity (lines 115-118)

**Format**: Always produces exactly 42 characters ("0x" + 40 hex digits)

**Use Case**: Contract addresses, account addresses, sender/receiver fields

**Example**:
```rust
// Filter by address without allocation
let matches = with_address(&contract_address, |hex| {
    filter_address == hex
});
```

## String-Returning API

These functions clone the buffer content and return an owned String. Use when you need to store the hex string beyond the immediate scope.

### format_hex

```rust
pub fn format_hex(bytes: &[u8]) -> String
```

**Location**: Lines 172-184 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: General-purpose hex formatting for arbitrary byte slices

**Input**: Byte slice of any length

**Output**: String with "0x" prefix and lowercase hex encoding

**Buffer Reuse**: Uses HEX_BUFFER thread-local (line 173)

**Process**:
1. Clear buffer (line 175)
2. Add "0x" prefix (line 176)
3. Format each byte as two hex digits (lines 178-180)
4. Clone and return (line 182)

**Use Case**: When you need an owned String to pass around or store

**Performance**: One allocation (the final clone), but reuses buffer capacity

**Example**:
```rust
let hash_string = format_hex(&tx_hash);
cache.insert(hash_string, tx_data);
```

### format_hex_u64

```rust
pub fn format_hex_u64(value: u64) -> String
```

**Location**: Lines 193-207 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Format u64 as hex string, optimized for Ethereum numeric values

**Input**: u64 value (block number, timestamp, gas, etc.)

**Output**: Minimal hex representation with "0x" prefix

**Special Handling**:
- Zero → "0x0" (lines 198-199)
- Non-zero → "0x" + minimal hex (lines 200-202)

**Use Case**: Block numbers, timestamps, nonces, gas values

**Example**:
```rust
let block_hex = format_hex_u64(block_number);
upstream.request("eth_getBlockByNumber", &[block_hex])
```

### format_hash32

```rust
pub fn format_hash32(hash: &[u8; 32]) -> String
```

**Location**: Lines 217-235 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Format 32-byte hashes with optimal performance

**Input**: 32-byte array reference

**Output**: 66-character hex string

**Capacity Management**: Pre-allocates exactly 66 bytes if needed (lines 222-226)

**Use Case**: Transaction hashes, block hashes, log topics

**Frequency**: Called thousands of times per second in production

**Example**:
```rust
let tx_hash_hex = format_hash32(&transaction.hash);
response.insert("transactionHash", tx_hash_hex);
```

### format_address

```rust
pub fn format_address(address: &[u8; 20]) -> String
```

**Location**: Lines 246-264 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Format Ethereum addresses efficiently

**Input**: 20-byte array reference

**Output**: 42-character hex string

**Capacity Management**: Pre-allocates 42 bytes if needed (lines 251-254)

**Use Case**: Contract addresses, account addresses, from/to fields

**Note**: Does not implement EIP-55 checksum encoding (all lowercase)

**Example**:
```rust
let contract_hex = format_address(&contract_address);
log_event.insert("address", contract_hex);
```

### format_hex_large

```rust
pub fn format_hex_large(bytes: &[u8]) -> String
```

**Location**: Lines 275-294 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Format large byte slices (transaction input, log data)

**Input**: Byte slice, typically hundreds or thousands of bytes

**Output**: Hex-encoded string

**Buffer**: Uses HEX_BUFFER_LARGE with 1024-byte initial capacity (line 276)

**Capacity Calculation**: Needs exactly `2 + bytes.len() * 2` characters (line 280)

**Auto-Resize**: Grows buffer if data exceeds capacity (lines 281-284)

**Use Case**: Transaction input data, log data fields, constructor arguments

**Performance**: Optimized for common transaction sizes (up to 512 bytes of data)

**Example**:
```rust
let input_data_hex = format_hex_large(&tx.input);
transaction_json.insert("input", input_data_hex);
```

### format_hex_alt

```rust
pub fn format_hex_alt(bytes: &[u8]) -> String
```

**Location**: Lines 307-319 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Alternative hex formatting using second buffer

**Input**: Byte slice

**Output**: Hex-encoded string

**Buffer**: Uses HEX_BUFFER_ALT instead of primary buffer (line 308)

**Use Case**: When you need two hex strings in the same expression

**Critical Scenario**: Prevents buffer overwrite race conditions

**Example**:
```rust
// Both format calls are in same expression
let json = json!({
    "from": format_address(&tx.from),
    "to": format_hex_alt(&tx.to)  // Use alt to avoid conflict
});
```

## Parsing Functions

These functions convert hex strings back to binary data, also using thread-local buffers to avoid allocations.

### parse_hex_u64

```rust
pub fn parse_hex_u64(hex: &str) -> Option<u64>
```

**Location**: Lines 364-374 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Parse hex string to u64 value

**Input**: Hex string with or without "0x" prefix

**Output**: `Some(u64)` on success, `None` on parse failure

**Process**:
1. Strip "0x" prefix if present (line 369)
2. Copy to PARSE_BUFFER (line 370)
3. Parse as radix-16 (line 372)

**Use Case**: Block numbers, timestamps, gas values from JSON-RPC

**Error Handling**: Returns `None` for invalid hex, overflow, empty string

**Example**:
```rust
let block_num = parse_hex_u64("0x3e8").unwrap(); // 1000
let timestamp = parse_hex_u64("64a2f1f0").unwrap(); // Without prefix
```

### parse_hex_u32

```rust
pub fn parse_hex_u32(hex: &str) -> Option<u32>
```

**Location**: Lines 381-391 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Parse hex string to u32 value

**Input**: Hex string with or without "0x" prefix

**Output**: `Some(u32)` on success, `None` on failure

**Use Case**: Log indexes, transaction indexes, smaller numeric values

**Same Pattern**: Identical logic to `parse_hex_u64` but returns u32

**Example**:
```rust
let log_index = parse_hex_u32("0x5").unwrap(); // 5
```

### parse_hex_bytes

```rust
pub fn parse_hex_bytes(hex: &str) -> Option<Vec<u8>>
```

**Location**: Lines 398-408 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Parse hex string to byte vector

**Input**: Hex string with or without "0x" prefix

**Output**: `Some(Vec<u8>)` on success, `None` on invalid hex

**Process**:
1. Strip "0x" prefix (line 403)
2. Copy to PARSE_BUFFER (line 404)
3. Use `hex::decode()` crate (line 406)

**Use Case**: Variable-length data like transaction input, log data

**Validation**: Returns `None` if hex string has odd length or invalid characters

**Example**:
```rust
let data = parse_hex_bytes("0xdeadbeef").unwrap();
assert_eq!(data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
```

### parse_hex_array

```rust
pub fn parse_hex_array<const N: usize>(hex: &str) -> Option<[u8; N]>
```

**Location**: Lines 416-442 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Parse hex string to fixed-size array (zero allocations)

**Input**: Hex string with or without "0x" prefix

**Output**: `Some([u8; N])` on success, `None` on wrong length or invalid hex

**Generic Parameter**: `N` specifies exact array size (20 for addresses, 32 for hashes)

**Length Validation**: Rejects strings that don't produce exactly N bytes (lines 423-425)

**Process**:
1. Strip "0x" prefix (line 421)
2. Check length is exactly `N * 2` characters (lines 423-425)
3. Copy to PARSE_BUFFER (line 427)
4. Parse two hex digits at a time into array (lines 430-438)

**Performance**: Fastest parsing method - no heap allocation, direct to stack array

**Use Case**: Addresses (20 bytes), hashes (32 bytes), topics (32 bytes)

**Example**:
```rust
let address: [u8; 20] = parse_hex_array("0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb").unwrap();
let tx_hash: [u8; 32] = parse_hex_array("0xabc...def").unwrap();
```

### hex_digit_to_u8 (Helper)

```rust
fn hex_digit_to_u8(c: u8) -> Option<u8>
```

**Location**: Lines 445-452 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Convert single hex character to numeric value

**Input**: ASCII byte representing hex digit

**Output**: `Some(0..=15)` for valid hex, `None` for invalid

**Pattern Matching**:
- `'0'..='9'` → 0..=9
- `'a'..='f'` → 10..=15
- `'A'..='F'` → 10..=15
- Anything else → `None`

**Use Case**: Internal helper for `parse_hex_array()`

## HexSerializer

Direct serialization to JSON output without intermediate string allocation.

### HexSerializer

```rust
pub struct HexSerializer<'a> {
    bytes: &'a [u8],
}
```

**Location**: Lines 134-158 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Serde serializer that writes hex directly to JSON output

**Lifetime**: Borrows byte slice, no copying required

**Constructor**:
```rust
pub fn new(bytes: &'a [u8]) -> Self
```

**Location**: Lines 140-143

**Use Case**: Embedding hex data in serde-serialized structs

**Performance Benefit**: Avoids creating intermediate String, writes directly to serializer output

**Implementation**: Lines 146-158

**Process**:
1. Allocate output string with exact capacity (line 151)
2. Write "0x" prefix (line 152)
3. Write hex digits directly (lines 153-155)
4. Serialize string (line 156)

**Example**:
```rust
#[derive(Serialize)]
struct LogResponse {
    #[serde(serialize_with = "serialize_data")]
    data: Vec<u8>,
}

fn serialize_data<S>(data: &Vec<u8>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    HexSerializer::new(data).serialize(s)
}
```

## Buffer Statistics

Functions for monitoring and testing buffer usage.

### get_hex_buffer_stats

```rust
pub fn get_hex_buffer_stats() -> (u64, u64)
```

**Location**: Lines 326-331 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Get current HEX_BUFFER capacity and length

**Output**: Tuple of `(capacity, length)`

**Use Case**: Verify buffer reuse, debug allocation patterns

**Example**:
```rust
let (capacity, length) = get_hex_buffer_stats();
println!("Buffer has {} bytes capacity, {} bytes used", capacity, length);
```

### get_parse_buffer_stats

```rust
pub fn get_parse_buffer_stats() -> (u64, u64)
```

**Location**: Lines 338-343 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Get current PARSE_BUFFER capacity and length

**Output**: Tuple of `(capacity, length)`

**Use Case**: Verify parse buffer reuse

**Example**:
```rust
let (capacity, length) = get_parse_buffer_stats();
assert!(capacity >= 64, "Buffer should maintain initial capacity");
```

### reset_buffer_stats

```rust
pub fn reset_buffer_stats()
```

**Location**: Lines 348-357 (`crates/prism-core/src/utils/hex_buffer.rs`)

**Purpose**: Clear buffers for testing purposes

**Clears**: HEX_BUFFER and PARSE_BUFFER

**Use Case**: Test isolation, ensuring clean state between tests

**Example**:
```rust
#[test]
fn test_buffer_reuse() {
    reset_buffer_stats();

    format_hex(&[0xDE, 0xAD]);
    let (_, len) = get_hex_buffer_stats();
    assert_eq!(len, 6); // "0xdead" = 6 chars
}
```

## Performance Characteristics

### Allocation Elimination

**String-Returning Functions**:
- **Old approach**: `format!("0x{}", hex::encode(bytes))` - 2 allocations
- **New approach**: `format_hex(bytes)` - 1 allocation (reuses buffer)
- **Improvement**: ~25-30% faster (benchmarks moved to `benches/hex_benchmarks.rs`)

**Callback Functions**:
- **Zero allocations** when result doesn't need ownership
- **Optimal** for comparison, hashing, parsing operations

### Buffer Capacity Growth

**Smart Pre-allocation**:
- HEX_BUFFER starts at 66 bytes (most common size)
- HEX_BUFFER_LARGE starts at 1024 bytes (typical transaction size)
- Buffers grow but never shrink (amortized performance)

**Capacity Formula**:
- Hash (32 bytes) → 66 characters
- Address (20 bytes) → 42 characters
- N bytes → `2 + N * 2` characters

### Thread-Local Storage

**Benefits**:
- No synchronization overhead (no locks, no atomics)
- Each thread has independent buffers
- Cache-friendly (buffers stay in thread's cache)

**Drawbacks**:
- Memory proportional to thread count
- Not suitable for sharing across threads

### Benchmark Results

Benchmarks have been moved to `benches/hex_benchmarks.rs`:

**Test Setup**: 100,000 iterations, 32-byte data
- Old method (`format!` + `hex::encode`): Baseline
- New method (`format_hex`): 25-30% faster
- Speedup ratio: >0.8x required (usually achieves 1.2-1.5x)

**Real-World Impact**:
- `eth_getLogs` with 1000 logs: Saves ~50ms in hex encoding
- `eth_getBlockByNumber` with 200 transactions: Saves ~15ms
- High throughput: Saves CPU cycles and reduces GC pressure

## Thread Safety

### Thread-Local Storage Model

**Mechanism**: `thread_local!` macro creates per-thread instances

**Safety Guarantees**:
- Each thread has completely independent buffers
- No data races possible
- No synchronization overhead

**RefCell Usage**:
- Interior mutability pattern
- Runtime borrow checking
- Panics if buffer is borrowed mutably twice (won't happen in normal use)

**Example**:
```rust
// Thread 1
let hex1 = format_hex(&data1); // Uses thread 1's HEX_BUFFER

// Thread 2 (simultaneously)
let hex2 = format_hex(&data2); // Uses thread 2's HEX_BUFFER

// No conflict - different buffers
```

### Borrow Checker Rules

**Critical Constraint**: Cannot use callback-based API recursively

**Would Panic**:
```rust
with_hex_format(&data1, |hex1| {
    // This panics - tries to borrow already-borrowed buffer
    with_hex_format(&data2, |hex2| {
        format!("{} {}", hex1, hex2)
    })
})
```

**Solution**: Use alternative buffer
```rust
with_hex_format(&data1, |hex1| {
    // Uses HEX_BUFFER_ALT - no conflict
    let hex2 = format_hex_alt(&data2);
    format!("{} {}", hex1, hex2)
})
```

## Usage Examples

### Basic Hex Formatting

```rust
use crate::utils::hex_buffer::*;

// Format various data types
let hash = [0xAB; 32];
let hash_hex = format_hash32(&hash);
// "0xabababab...abababab" (66 chars)

let address = [0x12; 20];
let addr_hex = format_address(&address);
// "0x1212121212...121212" (42 chars)

let block_num = 1000u64;
let block_hex = format_hex_u64(block_num);
// "0x3e8"

let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
let data_hex = format_hex(&data);
// "0xdeadbeef"
```

### Callback-Based Zero-Allocation

```rust
// Compare without allocating
let matches = with_hash32(&tx_hash, |hex| {
    hex == expected_hash_hex
});

// Use in format string without double allocation
let message = with_hex_u64(block_number, |hex| {
    format!("Processing block {}", hex)
});

// Look up in HashMap without allocating key
let cached = with_address(&contract_addr, |hex| {
    cache_map.get(hex).cloned()
});
```

### Parsing Hex Strings

```rust
// Parse block number
let block_num = parse_hex_u64("0x3e8").unwrap(); // 1000

// Parse address
let addr: [u8; 20] = parse_hex_array(
    "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb"
).unwrap();

// Parse transaction hash
let tx_hash: [u8; 32] = parse_hex_array(
    "0xabc123..."
).unwrap();

// Parse variable-length data
let input_data = parse_hex_bytes("0xdeadbeef").unwrap();
```

### Using HexSerializer in Structs

```rust
use serde::Serialize;
use crate::utils::hex_buffer::HexSerializer;

#[derive(Serialize)]
struct LogEvent {
    address: String,
    #[serde(serialize_with = "serialize_topics")]
    topics: Vec<[u8; 32]>,
    #[serde(serialize_with = "serialize_data")]
    data: Vec<u8>,
}

fn serialize_topics<S>(topics: &Vec<[u8; 32]>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(topics.len()))?;
    for topic in topics {
        seq.serialize_element(&HexSerializer::new(topic))?;
    }
    seq.end()
}

fn serialize_data<S>(data: &Vec<u8>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    HexSerializer::new(data).serialize(s)
}
```

### Multiple Hex Strings in One Expression

```rust
// Wrong - second call overwrites first buffer before format! reads it
let bad = format!("{} -> {}",
    format_hex(&from),
    format_hex(&to)  // Overwrites buffer!
);

// Correct - use alt buffer for second value
let good = format!("{} -> {}",
    format_hex(&from),
    format_hex_alt(&to)  // Uses separate buffer
);

// Also correct - callback API
let better = with_hex_format(&from, |from_hex| {
    let to_hex = format_hex(&to);  // Safe - callback finished
    format!("{} -> {}", from_hex, to_hex)
});
```

### Large Data Formatting

```rust
// Transaction input data (potentially KB in size)
let large_input = vec![0x42; 2048]; // 2KB of data

// Use large buffer for efficient formatting
let input_hex = format_hex_large(&large_input);

// Regular buffer would work but might reallocate
let input_hex_slow = format_hex(&large_input); // Less efficient
```

### Testing Buffer Reuse

```rust
#[test]
fn test_buffer_efficiency() {
    reset_buffer_stats();

    // First call
    let hex1 = format_hex(&[0xAB, 0xCD]);
    let (cap1, _) = get_hex_buffer_stats();

    // Second call - buffer should be reused
    let hex2 = format_hex(&[0x12, 0x34]);
    let (cap2, _) = get_hex_buffer_stats();

    // Capacity should not grow (buffer reused)
    assert_eq!(cap1, cap2);
}
```

## When to Use Which Function

### Callback vs String-Returning

**Use Callback API** (`with_*` functions) when:
- You only need the hex string temporarily
- You're comparing, hashing, or parsing the hex
- You want zero allocations
- Result doesn't outlive the function call

**Use String-Returning API** (`format_*` functions) when:
- You need to store the hex string
- You're inserting into a collection
- You're passing ownership to another function
- You need the string to outlive the current scope

### Specialized vs General Functions

**Use Specialized Functions**:
- `format_hash32()` for 32-byte hashes - faster, capacity pre-allocated
- `format_address()` for 20-byte addresses - faster, capacity pre-allocated
- `format_hex_u64()` for numeric values - minimal representation
- `format_hex_large()` for >256 bytes - optimized large buffer

**Use General Functions**:
- `format_hex()` for arbitrary small data
- `format_hex_alt()` when you need two strings simultaneously

### Parsing Functions Selection

**Use `parse_hex_array<N>()`** when:
- You know the exact size (addresses, hashes, topics)
- You want zero heap allocations
- Performance is critical

**Use `parse_hex_bytes()`** when:
- Size is variable (transaction input, log data)
- You need a Vec for further processing

**Use `parse_hex_u64()` / `parse_hex_u32()`** when:
- You're parsing numeric values
- Block numbers, timestamps, indexes

## Integration Points

### Used By

- `crates/prism-core/src/cache/converter.rs` (lines 125-142, 238-253, 480-663)
- `crates/prism-core/src/proxy/handlers/logs.rs` (throughout)
- `crates/prism-core/src/proxy/handlers/blocks.rs` (throughout)
- `crates/prism-core/src/proxy/handlers/transactions.rs` (throughout)
- `crates/prism-core/src/cache/types.rs` (field serialization)

### Exported By

`crates/prism-core/src/utils/mod.rs` (lines 3-5) exports:
- `format_address`
- `format_hash32`
- `format_hex`
- `format_hex_alt`
- `format_hex_large`
- `format_hex_u64`

### Dependencies

- `std::cell::RefCell` - Interior mutability for thread-local buffers
- `std::fmt::Write` - Formatting trait for buffer writing
- `hex` crate - Used in `parse_hex_bytes()` for decoding

## Testing

The module includes comprehensive tests at lines 454-475 in `crates/prism-core/src/utils/hex_buffer.rs`:

### test_hex_buffer_reuse

**Location**: Lines 461-470

**Purpose**: Verify buffer is reused across multiple calls

**Validates**:
- Buffer capacity is maintained
- Correct hex output
- Statistics tracking works

### Performance Benchmarks

**Note**: Performance benchmarks have been moved to `benches/hex_benchmarks.rs`

**Run With**:
```bash
cargo bench -p prism-core --bench hex_benchmarks
```

## Performance Optimization Summary

### Key Improvements

1. **Thread-Local Buffers**
   - Files: `crates/prism-core/src/utils/hex_buffer.rs` (lines 3-23)
   - Eliminates most string allocations
   - 25-30% faster than standard approach

2. **Specialized Functions**
   - `format_hash32()` (lines 217-235): Pre-allocated capacity for hashes
   - `format_address()` (lines 246-264): Pre-allocated capacity for addresses
   - `format_hex_u64()` (lines 193-207): Minimal representation for numbers

3. **Zero-Allocation Parsing**
   - `parse_hex_array()` (lines 416-442): Direct to stack array
   - `parse_hex_u64()` / `parse_hex_u32()` (lines 364-391): Thread-local buffer reuse

4. **Callback-Based API**
   - `with_*` functions (lines 38-128): Zero allocations for temporary use
   - Critical for hot paths (cache lookups, comparisons)

## Future Improvements

1. **SIMD Optimization** - Use SIMD instructions for parallel hex digit conversion on large data
2. **Const Generics** - More type safety for fixed-size arrays
3. **Error Types** - Return `Result` instead of `Option` for better error messages
4. **Benchmarking Suite** - Comprehensive criterion benchmarks for all functions
5. **EIP-55 Checksums** - Optional checksum encoding for addresses
6. **Buffer Pooling** - Experiment with buffer pools for better cache locality

## See Also

- [Cache Converter](../cache/converter.md) - Primary consumer of hex utilities
- [Advanced Caching System](../../caching-system.md) - Cache architecture
