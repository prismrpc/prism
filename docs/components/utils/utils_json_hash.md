# JSON Hash Utilities

## Overview

The JSON hash module provides high-performance JSON hashing utilities optimized for consensus operations in the Prism RPC aggregator. It eliminates memory allocations through direct value traversal and thread-local buffer reuse, achieving 2-3x performance improvements over string-based serialization approaches.

Location: `crates/prism-core/src/utils/json_hash.rs`

## Purpose

The JSON hash utilities serve several key functions in the consensus pipeline:

1. **Eliminate allocations** - Direct value hashing avoids string serialization overhead
2. **Deterministic hashing** - Object keys are sorted for consistent hashes regardless of insertion order
3. **Type discrimination** - Prevents hash collisions between different JSON types (null vs false vs 0)
4. **Filtered hashing** - Exclude chain-specific fields (timestamps, hashes) from consensus comparisons
5. **IEEE 754 normalization** - Handles NaN/Infinity consistently across responses

## Thread-Local Buffer

### JSON_BUFFER

**Location**: Line 29

**Purpose**: Buffer for fallback JSON serialization when direct hashing isn't suitable

**Initial Capacity**: 2048 bytes (sufficient for most RPC responses without reallocation)

**Used By**: `hash_json_buffered()` - Serialization-based hashing

**Thread Safety**: Each thread gets its own independent buffer via `thread_local!` macro

## Core Functions

### hash_json_value

**Location**: Lines 55-124

**Purpose**: Hash a `serde_json::Value` directly without serialization

**Signature**:
```rust
pub fn hash_json_value(value: &Value, hasher: &mut impl Hasher)
```

**Type Discrimination**:
Each JSON type is prefixed with a discriminant byte to prevent collisions:
- Null: `0u8`
- Bool: `1u8` + bool value
- Number: `2u8` + number representation
- String: `3u8` + length + bytes
- Array: `4u8` + length + each element
- Object: `5u8` + length + sorted (key, value) pairs

**Key Sorting** (Lines 113-114):
Objects keys are collected into a `Vec<&String>` and sorted to ensure deterministic hashing:
```rust
let mut sorted_keys: Vec<&String> = obj.keys().collect();
sorted_keys.sort_unstable();
```

This ensures `{"a":1,"b":2}` and `{"b":2,"a":1}` produce identical hashes.

**Float Normalization** (Lines 75-93):
NaN and Infinity values are normalized to canonical representations before hashing to prevent consensus violations from different IEEE 754 bit patterns.

### hash_json_value_filtered

**Location**: Lines 146-230

**Purpose**: Hash a JSON value while excluding specified field paths

**Signature**:
```rust
pub fn hash_json_value_filtered(
    value: &Value,
    ignore_paths: &[String],
    hasher: &mut impl Hasher,
    current_path: &str,
)
```

**Path Patterns**:
- Simple paths: `"timestamp"`, `"blockHash"`
- Nested paths: `"result.timestamp"`, `"transactions.gasPrice"`
- Wildcard paths: `"transactions.*.gasPrice"` (matches all array elements)

**Use Case**: Exclude chain-specific fields from consensus comparisons (e.g., different timestamps across upstreams).

### hash_json_response

**Location**: Lines 232-250

**Purpose**: Hash a `JsonRpcResponse` for consensus comparison

**Signature**:
```rust
pub fn hash_json_response(response: &JsonRpcResponse) -> u64
```

**Fields Included**:
- `jsonrpc` version
- `result` (if present)
- `error` (if present)

**Fields Excluded**:
- `id` - Request correlation, not semantic content
- `cache_status` - Internal metadata

### hash_json_response_filtered

**Location**: Lines 252-275

**Purpose**: Hash a response while excluding specified paths from the result

**Signature**:
```rust
pub fn hash_json_response_filtered(response: &JsonRpcResponse, ignore_paths: &[String]) -> u64
```

### hash_json_buffered

**Location**: Lines 277-295

**Purpose**: Fallback hashing via JSON serialization (when direct hashing isn't suitable)

**Signature**:
```rust
pub fn hash_json_buffered(value: &Value) -> u64
```

**Performance**: Slower than direct hashing due to serialization overhead, but useful for debugging or when compact serialization is needed.

## Performance Characteristics

**Direct Hashing vs Serialization**:
- Direct hashing: ~2-3x faster than string-based approach
- Zero allocations for most operations
- No UTF-8 encoding overhead

**Benchmark Reference**: `benches/json_hash_benchmarks.rs`

## Integration Points

**Used By**:
- `upstream/consensus/quorum.rs` - Compare responses from multiple upstreams
- Consensus engine for response deduplication

**Exported By**:
- `utils/mod.rs`

## Testing

**Location**: Lines 328-end

Comprehensive tests covering:
- Determinism verification
- Key order independence
- Type discrimination (null vs false vs 0)
- Filtered hashing with various path patterns
- Float normalization edge cases

## See Also

- [Hex Buffer Utilities](./utils_hex_buffer.md) - Similar zero-allocation patterns
- [Consensus Architecture](../consensus/architecture.md) - How hashing integrates with consensus
