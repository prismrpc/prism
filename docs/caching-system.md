# Advanced Caching System for RPC Aggregator

This document describes the comprehensive caching system implemented for the RPC aggregator, designed to serve DeFi heavy read traffic with very high hit rates and minimal CPU usage.

## Overview

The advanced caching system provides:

- **Deduplicated log storage** with inverted indexes using roaring bitmaps
- **Exact result caching** for overlapping queries
- **Hot window ring buffers** for recent blocks
- **Centralized reorg management** with automatic invalidation
- **Memory-efficient storage** optimized for 128GB nodes
- **High-performance bitmap operations** for log filtering

## Architecture

### Core Components

1. **CacheManager** - Central orchestrator
2. **LogCache** - Deduplicated log store with bitmap indexes
3. **BlockCache** - Headers and bodies with hot window
4. **TransactionCache** - Transactions and receipts
5. **ReorgManager** - Chain reorganization handling

### Data Flow

```
Client Request → CacheManager → Subcaches → Response
                     ↓
              ReorgManager (for chain updates)
```

## Log Cache Design

### Deduplicated Store

- **Key**: `LogId` (block_number + log_index)
- **Value**: `LogRecord` (address, topics, data, tx_hash, removed)
- Each log is stored once and shared by all queries

### Inverted Indexes

- **Address Index**: `[u8; 20] → RoaringBitmap<LogId>`
- **Topic Indexes**: `[u8; 32] → RoaringBitmap<LogId>` (4 slots)
- **Chunk Index**: `u64 → RoaringBitmap<LogId>` (per block chunk)

### Exact Result Cache

- Stores `Vec<LogId>` per filter per chunk per subrange
- Overlapping queries reuse the same IDs
- LRU eviction based on total IDs stored

### Query Flow

1. Normalize filter into canonical key
2. Check exact result cache
3. If miss, perform bitmap operations:
   - Start with chunk bitmaps for range
   - Intersect with address bitmap
   - Intersect with topic bitmaps
4. Cache exact result for future reuse

## Block Cache Design

### Hot Window Ring Buffer

- Pre-allocated fixed-size ring for recent blocks
- O(1) inserts without allocation
- Automatic window shifting for new blocks

### Storage Structure

- **Headers**: `[u8; 32] → BlockHeader` + `u64 → [u8; 32]`
- **Bodies**: `[u8; 32] → BlockBody`
- **LRU caches** for eviction

## Transaction Cache Design

### Storage Structure

- **Transactions**: `[u8; 32] → TransactionRecord`
- **Receipts**: `[u8; 32] → ReceiptRecord`
- **Block Mapping**: `u64 → Vec<[u8; 32]>` (for reorg handling)

### Log Resolution

- Receipts store `Vec<LogId>` references
- Logs resolved from log store at read time
- No duplication of log data

## Reorg Management

### Safety Depth

- **Safe Head**: `tip - safety_depth` (default: 12 blocks)
- **Unsafe Window**: Blocks newer than safe head
- **Final Data**: Blocks older than safe head

### Reorg Detection

1. Monitor chain tip updates
2. Detect hash mismatches at same block number
3. Find divergence point by walking backwards
4. Invalidate affected blocks across all caches

### Invalidation Process

1. Remove from log store and rebuild indexes
2. Clear block cache entries
3. Remove transaction/receipt records
4. Clear exact result cache entries

## Configuration

### LogCacheConfig

```rust
pub struct LogCacheConfig {
    pub chunk_size: u64,              // 1000 blocks per chunk
    pub max_exact_results: usize,     // 10000 entries
    pub max_bitmap_entries: usize,    // 100000 entries
    pub safety_depth: u64,            // 12 blocks
}
```

### BlockCacheConfig

```rust
pub struct BlockCacheConfig {
    pub hot_window_size: usize,       // 200 blocks
    pub max_headers: usize,           // 10000 entries
    pub max_bodies: usize,            // 10000 entries
    pub safety_depth: u64,            // 12 blocks
}
```

### TransactionCacheConfig

```rust
pub struct TransactionCacheConfig {
    pub max_transactions: usize,      // 50000 entries
    pub max_receipts: usize,          // 50000 entries
    pub safety_depth: u64,            // 12 blocks
}
```

## Usage Examples

### Basic Setup

```rust
use prism_core::cache::{CacheManager, CacheManagerConfig};
use prism_core::chain::ChainState;
use std::sync::Arc;

let config = CacheManagerConfig::default();
let chain_state = Arc::new(ChainState::new());
let cache_manager = Arc::new(
    CacheManager::new(&config, chain_state)
        .expect("Failed to initialize cache manager")
);
```

### Chain Tip Management

```rust
// Update chain tip
cache_manager.update_tip(1000, [1u8; 32]).await;

// Check if block is safe
let is_unsafe = cache_manager.is_unsafe_block(995).await;
```

### Log Queries

```rust
// Create filter
let filter = LogFilter::new(1000, 1010)
    .with_address(contract_address)
    .with_topic(0, transfer_topic);

// Query logs
let logs = cache_manager.get_logs(&filter, current_tip).await;
```

### Block Operations

```rust
// Insert block
cache_manager.insert_header(header).await;
cache_manager.insert_body(body).await;

// Retrieve block
let block = cache_manager.get_block_by_number(1000).await;
```

### Transaction Operations

```rust
// Insert transaction and receipt
cache_manager.insert_transaction(transaction).await;
cache_manager.insert_receipt(receipt).await;

// Get receipt with resolved logs
let (receipt, logs) = cache_manager.get_receipt_with_logs(&tx_hash).await;
```

### Cache Warming

```rust
// Warm all caches for a range
cache_manager.warm_recent(
    from_block, to_block,
    blocks, transactions, receipts, logs
).await;
```

## Performance Characteristics

### Memory Usage (128GB Node)

- **Log Store**: 30-60GB (depending on indexes)
- **Block Cache**: 15-30GB
- **Hot Window**: ~1GB
- **Bitmaps**: ~5-10GB
- **Exact Results**: ~1-5GB

### Performance Metrics

- **Log Query**: O(log n) with bitmaps
- **Block Retrieval**: O(1) from hot window
- **Reorg Invalidation**: O(affected_blocks)
- **Cache Hit Rate**: >95% for DeFi workloads

### Optimization Features

- **Lock-free reads** with immutable Arc segments
- **Column-like storage** for tight cache lines
- **Packed LogIds** for efficient bitmaps
- **LRU eviction** with size-based caps

## Monitoring and Observability

### Cache Statistics

```rust
let stats = cache_manager.get_stats().await;
println!("Log store size: {}", stats.log_store_size);
println!("Exact result count: {}", stats.exact_result_count);
println!("Bitmap memory usage: {} bytes", stats.bitmap_memory_usage);
```

### Reorg Statistics

```rust
let reorg_stats = cache_manager.get_reorg_stats().await;
println!("Current tip: {}", reorg_stats.current_tip);
println!("Safe head: {}", reorg_stats.safe_head);
println!("Unsafe window size: {}", reorg_stats.unsafe_window_size);
```

### Metrics to Track

- Cache hit/miss rates per API
- Memory usage per subcache
- Reorg frequency and depth
- Bitmap operation latency
- Exact result reuse rate

## Testing

### Unit Tests

Each component has comprehensive unit tests:

```bash
cargo test cache::log_cache
cargo test cache::block_cache
cargo test cache::transaction_cache
cargo test cache::reorg_manager
```

### Integration Tests

```bash
cargo test cache::advanced_cache_manager
```

### Performance Tests

```bash
cargo test --release cache::log_cache::tests::test_bitmap_performance
```

## Architecture Evolution

The current `CacheManager` implementation provides:

1. **Unified caching interface** across logs, blocks, and transactions
2. **Integrated reorg management** for chain tip updates via shared `ChainState`
3. **Finality-aware cleanup** with safe head tracking
4. **Configurable memory limits** based on node capacity

**Key Integration Points**:
- Shares `ChainState` with `ReorgManager` and `ScoringEngine` for unified chain tip tracking
- Uses atomic operations for lock-free tip/finality reads
- Coordinates with `ReorgManager` for automatic cache invalidation on chain reorganizations

## Future Enhancements

### Planned Features

- **Compression** for log data and bitmaps
- **Persistent storage** for cold data
- **Distributed caching** across multiple nodes
- **Advanced eviction policies** (LFU, adaptive)
- **Query optimization** for complex filters

### Performance Improvements

- **SIMD operations** for bitmap intersections
- **Memory-mapped storage** for large datasets
- **Async I/O** for disk operations
- **Batch processing** for bulk operations

## Troubleshooting

### Common Issues

1. **High Memory Usage**
   - Reduce chunk size
   - Lower max_exact_results
   - Enable compression

2. **Low Hit Rates**
   - Increase hot window size
   - Adjust safety depth
   - Check filter patterns

3. **Reorg Performance**
   - Optimize invalidation logic
   - Use batch operations
   - Monitor reorg frequency

### Debugging

Enable debug logging:

```rust
tracing_subscriber::fmt()
    .with_env_filter("prism_core::cache=debug")
    .init();
```

## Conclusion

The advanced caching system provides a robust, high-performance solution for DeFi RPC aggregation. It achieves high hit rates through intelligent data organization, efficient bitmap operations, and comprehensive reorg handling.

For questions or issues, please refer to the test suite and examples in the codebase.
