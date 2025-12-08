# Cache Memory Estimation - Implementation Patch

This patch centralizes hardcoded memory estimation values into a reusable module.

## Problem

Currently, memory size estimates are hardcoded in multiple locations:
- `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/cache.rs` lines 86-89, 219-223, 263-271

## Solution

Create a centralized memory estimation module with clear constants and helper functions.

---

## Step 1: Create Memory Estimation Module

**File**: `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/memory.rs`

```rust
//! Memory usage estimation for cache components.
//!
//! This module provides constants and functions for estimating memory usage
//! of cached data structures. Estimates are rough approximations based on
//! average entry sizes observed in production.

/// Average size of a block header in bytes.
///
/// Includes: block number, hash, parent hash, timestamp, miner, difficulty, etc.
pub const BLOCK_HEADER_BYTES: usize = 500;

/// Average size of a block body in bytes.
///
/// Includes: transaction list, uncle list, receipts root, logs bloom, etc.
/// This is a rough average - actual size varies significantly by block.
pub const BLOCK_BODY_BYTES: usize = 2048;

/// Average size of a transaction entry in bytes.
///
/// Includes: from, to, value, gas, nonce, data, signature, etc.
pub const TRANSACTION_BYTES: usize = 300;

/// Average size of a transaction receipt entry in bytes.
///
/// Includes: status, gas used, logs, contract address, etc.
pub const RECEIPT_BYTES: usize = 200;

/// Average size of an exact log query result in bytes.
///
/// Includes: serialized log entries with topics, data, address, etc.
pub const LOG_EXACT_RESULT_BYTES: usize = 1024;

/// Average size of a log cache bitmap entry in bytes.
///
/// Includes: roaring bitmap data structure for block ranges.
pub const LOG_BITMAP_ENTRY_BYTES: usize = 100;

/// Estimates total memory usage for the block cache.
///
/// # Arguments
///
/// * `max_headers` - Maximum number of block headers to cache
/// * `max_bodies` - Maximum number of block bodies to cache
///
/// # Returns
///
/// Estimated memory usage in bytes
///
/// # Example
///
/// ```rust
/// use prism_core::cache::memory;
///
/// let memory_bytes = memory::estimate_block_cache_memory(1000, 500);
/// assert_eq!(memory_bytes, 1000 * 500 + 500 * 2048);
/// ```
#[must_use]
pub const fn estimate_block_cache_memory(max_headers: usize, max_bodies: usize) -> usize {
    max_headers * BLOCK_HEADER_BYTES + max_bodies * BLOCK_BODY_BYTES
}

/// Estimates total memory usage for the transaction cache.
///
/// # Arguments
///
/// * `max_transactions` - Maximum number of transactions to cache
/// * `max_receipts` - Maximum number of receipts to cache
///
/// # Returns
///
/// Estimated memory usage in bytes
///
/// # Example
///
/// ```rust
/// use prism_core::cache::memory;
///
/// let memory_bytes = memory::estimate_tx_cache_memory(5000, 5000);
/// assert_eq!(memory_bytes, 5000 * 300 + 5000 * 200);
/// ```
#[must_use]
pub const fn estimate_tx_cache_memory(max_transactions: usize, max_receipts: usize) -> usize {
    max_transactions * TRANSACTION_BYTES + max_receipts * RECEIPT_BYTES
}

/// Estimates total memory usage for the log cache.
///
/// # Arguments
///
/// * `max_exact_results` - Maximum number of exact query results to cache
/// * `max_bitmap_entries` - Maximum number of bitmap entries to cache
///
/// # Returns
///
/// Estimated memory usage in bytes
///
/// # Example
///
/// ```rust
/// use prism_core::cache::memory;
///
/// let memory_bytes = memory::estimate_log_cache_memory(1000, 10000);
/// assert_eq!(memory_bytes, 1000 * 1024 + 10000 * 100);
/// ```
#[must_use]
pub const fn estimate_log_cache_memory(max_exact_results: usize, max_bitmap_entries: usize) -> usize {
    max_exact_results * LOG_EXACT_RESULT_BYTES + max_bitmap_entries * LOG_BITMAP_ENTRY_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_cache_estimation() {
        let memory = estimate_block_cache_memory(1000, 500);
        assert_eq!(memory, 1000 * 500 + 500 * 2048);
    }

    #[test]
    fn test_tx_cache_estimation() {
        let memory = estimate_tx_cache_memory(5000, 5000);
        assert_eq!(memory, 5000 * 300 + 5000 * 200);
    }

    #[test]
    fn test_log_cache_estimation() {
        let memory = estimate_log_cache_memory(1000, 10000);
        assert_eq!(memory, 1000 * 1024 + 10000 * 100);
    }

    #[test]
    fn test_zero_entries() {
        assert_eq!(estimate_block_cache_memory(0, 0), 0);
        assert_eq!(estimate_tx_cache_memory(0, 0), 0);
        assert_eq!(estimate_log_cache_memory(0, 0), 0);
    }
}
```

---

## Step 2: Update Cache Module

**File**: `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/mod.rs`

Add the new module to the cache module exports:

```rust
// Add this line near the top of the file with other module declarations
pub mod memory;

// ... rest of the file
```

---

## Step 3: Update Cache Handler

**File**: `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/cache.rs`

### Change 1: Add Import

Add to the imports section at the top of the file:

```rust
use prism_core::cache::memory;
```

### Change 2: Update `get_stats()` Handler (Lines 84-90)

**Before**:
```rust
// Estimate memory usage (rough approximation based on entry counts)
// Block header ~500 bytes, body ~2KB average, transaction ~300 bytes, receipt ~200 bytes
let block_memory =
    cache_stats_raw.header_cache_size * 500 + cache_stats_raw.body_cache_size * 2048;
let tx_memory =
    cache_stats_raw.transaction_cache_size * 300 + cache_stats_raw.receipt_cache_size * 200;
```

**After**:
```rust
// Estimate memory usage using centralized size constants
let block_memory = memory::estimate_block_cache_memory(
    cache_stats_raw.header_cache_size,
    cache_stats_raw.body_cache_size,
);
let tx_memory = memory::estimate_tx_cache_memory(
    cache_stats_raw.transaction_cache_size,
    cache_stats_raw.receipt_cache_size,
);
```

### Change 3: Update `get_memory_allocation()` Handler (Lines 215-223)

**Before**:
```rust
// Estimate memory usage (rough approximation based on entry counts)
// Block header ~500 bytes, body ~2KB average
let block_memory =
    (cache_stats_raw.header_cache_size * 500 + cache_stats_raw.body_cache_size * 2048) as u64;

// Transaction ~300 bytes, receipt ~200 bytes
let tx_memory = (cache_stats_raw.transaction_cache_size * 300 +
    cache_stats_raw.receipt_cache_size * 200) as u64;
```

**After**:
```rust
// Estimate memory usage using centralized size constants
let block_memory = memory::estimate_block_cache_memory(
    cache_stats_raw.header_cache_size,
    cache_stats_raw.body_cache_size,
) as u64;

let tx_memory = memory::estimate_tx_cache_memory(
    cache_stats_raw.transaction_cache_size,
    cache_stats_raw.receipt_cache_size,
) as u64;
```

### Change 4: Update `get_settings()` Handler (Lines 260-271)

**Before**:
```rust
// Calculate estimated max sizes from config entry counts
// Using same estimation factors as get_stats():
// Block header ~500 bytes, body ~2KB, transaction ~300 bytes, receipt ~200 bytes
let block_max_bytes =
    config.block_cache.max_headers * 500 + config.block_cache.max_bodies * 2048;
let tx_max_bytes = config.transaction_cache.max_transactions * 300 +
    config.transaction_cache.max_receipts * 200;

// Log cache: rough estimate based on bitmap entries and exact results
// Each exact result ~1KB, each bitmap entry ~100 bytes
let log_max_bytes =
    config.log_cache.max_exact_results * 1024 + config.log_cache.max_bitmap_entries * 100;
```

**After**:
```rust
// Calculate estimated max sizes from config using centralized size constants
let block_max_bytes = memory::estimate_block_cache_memory(
    config.block_cache.max_headers,
    config.block_cache.max_bodies,
);
let tx_max_bytes = memory::estimate_tx_cache_memory(
    config.transaction_cache.max_transactions,
    config.transaction_cache.max_receipts,
);
let log_max_bytes = memory::estimate_log_cache_memory(
    config.log_cache.max_exact_results,
    config.log_cache.max_bitmap_entries,
);
```

---

## Benefits of This Approach

1. **Single Source of Truth**: All memory size constants are defined in one place
2. **Consistency**: Same estimates used across all handlers
3. **Maintainability**: Easy to update if profiling shows different sizes
4. **Testability**: Memory estimation logic can be unit tested
5. **Documentation**: Constants are clearly documented with their purpose
6. **Type Safety**: `const fn` enables compile-time calculation where possible
7. **Zero Runtime Cost**: Constants are inlined by the compiler

---

## Testing the Changes

### Manual Testing

1. Start the server with admin API enabled:
   ```bash
   PRISM_ADMIN__ENABLED=true cargo run
   ```

2. Test the endpoints:
   ```bash
   # Cache stats
   curl http://localhost:3031/admin/cache/stats

   # Memory allocation
   curl http://localhost:3031/admin/cache/memory-allocation

   # Cache settings
   curl http://localhost:3031/admin/cache/settings
   ```

3. Verify the memory estimates are consistent across endpoints

### Unit Testing

Add to `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/memory.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consistency_with_hardcoded_values() {
        // Verify that our estimates match the old hardcoded values
        assert_eq!(BLOCK_HEADER_BYTES, 500);
        assert_eq!(BLOCK_BODY_BYTES, 2048);
        assert_eq!(TRANSACTION_BYTES, 300);
        assert_eq!(RECEIPT_BYTES, 200);
        assert_eq!(LOG_EXACT_RESULT_BYTES, 1024);
        assert_eq!(LOG_BITMAP_ENTRY_BYTES, 100);
    }

    #[test]
    fn test_realistic_scenarios() {
        // Typical configuration values
        let block_mem = estimate_block_cache_memory(10000, 5000);
        assert!(block_mem > 0);

        let tx_mem = estimate_tx_cache_memory(50000, 50000);
        assert!(tx_mem > 0);

        let log_mem = estimate_log_cache_memory(1000, 100000);
        assert!(log_mem > 0);
    }
}
```

---

## Migration Checklist

- [ ] Create `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/memory.rs`
- [ ] Add `pub mod memory;` to `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/mod.rs`
- [ ] Update `cache.rs` imports to include `use prism_core::cache::memory;`
- [ ] Update `get_stats()` handler (lines 84-90)
- [ ] Update `get_memory_allocation()` handler (lines 215-223)
- [ ] Update `get_settings()` handler (lines 260-271)
- [ ] Run `cargo test` to verify no regressions
- [ ] Run `cargo clippy` to verify no warnings
- [ ] Test endpoints manually
- [ ] Update documentation if needed

---

## Alternative: Configuration-Based Sizes

If you want to make the size estimates configurable at runtime, you could add them to `CacheManagerConfig`:

```rust
// In CacheManagerConfig
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheManagerConfig {
    // ... existing fields ...

    /// Memory estimation constants (optional, uses defaults if not set)
    #[serde(default)]
    pub memory_estimates: MemoryEstimateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEstimateConfig {
    #[serde(default = "default_block_header_bytes")]
    pub block_header_bytes: usize,

    #[serde(default = "default_block_body_bytes")]
    pub block_body_bytes: usize,

    #[serde(default = "default_transaction_bytes")]
    pub transaction_bytes: usize,

    #[serde(default = "default_receipt_bytes")]
    pub receipt_bytes: usize,

    #[serde(default = "default_log_exact_result_bytes")]
    pub log_exact_result_bytes: usize,

    #[serde(default = "default_log_bitmap_entry_bytes")]
    pub log_bitmap_entry_bytes: usize,
}

fn default_block_header_bytes() -> usize { 500 }
fn default_block_body_bytes() -> usize { 2048 }
fn default_transaction_bytes() -> usize { 300 }
fn default_receipt_bytes() -> usize { 200 }
fn default_log_exact_result_bytes() -> usize { 1024 }
fn default_log_bitmap_entry_bytes() -> usize { 100 }
```

However, **this is likely overkill** for this use case. The current approach with constants is simpler and sufficient.
