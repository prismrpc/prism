//! Multi-tier caching system for Ethereum RPC responses.
//!
//! This module implements a sophisticated caching layer designed for high-performance
//! EVM RPC proxies with reorg-awareness and finality state tracking.
//!
//! # Architecture
//!
//! The cache system uses a three-tier strategy optimized for different access patterns:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                         CacheManager                                 │
//! │  (Orchestrates all caches, handles fetch deduplication, cleanup)    │
//! └─────────────────────────────────────────────────────────────────────┘
//!                │                  │                  │
//!        ┌───────▼───────┐  ┌───────▼───────┐  ┌───────▼───────┐
//!        │  BlockCache   │  │TransactionCache│  │   LogCache    │
//!        │               │  │               │  │               │
//!        │ • Hot window  │  │ • Hash→Record │  │ • Inverted    │
//!        │   (200 blocks)│  │   LRU cache   │  │   indexes     │
//!        │ • DashMap     │  │ • TTL-based   │  │ • Bitmap      │
//!        │   fallback    │  │   expiry      │  │   filtering   │
//!        │ • LRU (10k)   │  │               │  │ • Range-aware │
//!        └───────────────┘  └───────────────┘  └───────────────┘
//!                │                  │                  │
//!        ┌───────▼──────────────────▼──────────────────▼───────┐
//!        │                    ReorgManager                      │
//!        │  (Detects reorgs, invalidates affected cache entries)│
//!        │  • Hash history tracking                             │
//!        │  • Reorg coalescing (100ms window)                   │
//!        │  • Coordinates with ChainState for tip updates       │
//!        └──────────────────────────────────────────────────────┘
//! ```
//!
//! # Cache Types
//!
//! ## `BlockCache`
//! Stores block headers and bodies with a two-tier design:
//! - **Hot window**: Circular buffer for recent blocks (O(1) operations)
//! - **Cold storage**: `DashMap` + LRU for older blocks
//!
//! ## `TransactionCache`
//! Stores transaction records and receipts keyed by transaction hash.
//! Uses LRU eviction with configurable capacity.
//!
//! ## `LogCache`
//! Stores event logs with inverted indexes for efficient filtering:
//! - Block number → logs mapping
//! - Address → logs bitmap index
//! - Topic → logs bitmap index
//! - Supports partial range fulfillment for `eth_getLogs` queries
//!
//! # Reorg Handling
//!
//! The [`ReorgManager`] detects chain reorganizations and coordinates cache invalidation:
//!
//! 1. **Detection**: Compares block hashes from WebSocket `newHeads` or health checks
//! 2. **Coalescing**: Batches rapid reorgs within 100ms to prevent cache thrashing
//! 3. **Invalidation**: Clears all cache entries from divergence point to old tip
//! 4. **Coordination**: Uses shared [`ChainState`](crate::chain::ChainState) for atomic tip updates
//!
//! # Fetch Deduplication
//!
//! The [`CacheManager`] prevents thundering herd on cache misses:
//! - Semaphore-based coordination per block
//! - First requester fetches, others wait on semaphore
//! - RAII `FetchGuard` ensures cleanup on panic
//! - Stale fetch cleanup (120s threshold)
//!
//! # Validation
//!
//! The [`validation`] module provides defense-in-depth against cache poisoning:
//! - Block hash format validation
//! - Log range boundary enforcement
//! - Receipt transaction hash matching
//!
//! # Consistency Patterns
//!
//! The cache system follows standardized patterns for consistency across all cache types:
//!
//! ## Stats Tracking
//!
//! All caches use a **dirty flag pattern** for lazy stats computation:
//! - `stats_dirty: AtomicBool` marks when cache data has changed
//! - `get_stats()` checks the flag and recomputes only when dirty
//! - Lower overhead than counter-based approaches (single atomic read/write)
//!
//! Example (pseudocode):
//! ```rust,ignore
//! // On data change:
//! stats_dirty.store(true, Ordering::Relaxed);
//!
//! // On stats read:
//! if stats_dirty.swap(false, Ordering::Relaxed) {
//!     compute_stats();
//! }
//! ```
//!
//! ## Sync vs Async Getters
//!
//! **Sync getters** are preferred for in-memory cache lookups:
//! - Read from `DashMap` or other lock-free structures
//! - Return `Option<T>` for cache misses
//! - Examples: `get_header_by_number()`, `get_transaction()`, `get_log_records()`
//!
//! **Async getters** are used when:
//! - Performing I/O or network operations
//! - Aggregating from multiple async sources
//! - The operation inherently requires async (e.g., query processing)
//!
//! Examples:
//! - `get_logs()` - async because it performs async query work
//! - `get_stats()` - async because it aggregates from multiple caches
//!
//! ## Error Handling Conventions
//!
//! The cache system follows consistent error handling patterns:
//!
//! - **`Option<T>`**: Cache miss (expected, not an error)
//!   - `get_header_by_number()` → `Option<Arc<BlockHeader>>`
//!   - `get_transaction()` → `Option<Arc<TransactionRecord>>`
//!
//! - **`Result<T, E>`**: Initialization, configuration, or I/O errors
//!   - `CacheManager::new()` → `Result<Self, CacheManagerError>`
//!   - `BlockCache::new()` → `Result<Self, BlockCacheError>`
//!
//! - **`Result<Option<T>, E>`**: Operations that can fail AND return "not found"
//!   - Currently unused, but reserved for upstream fetch operations that may fail
//!
//! [`ReorgManager`]: reorg_manager::ReorgManager
//! [`CacheManager`]: cache_manager::CacheManager

pub mod block_cache;
pub mod cache_manager;
pub mod converter;
pub mod log_cache;
pub mod manager;
pub mod reorg_manager;
pub mod transaction_cache;
pub mod types;
pub mod validation;

use std::sync::Arc;

pub use cache_manager::CacheManager;
pub use log_cache::{LogCacheConfig, LogCacheConfigError, MAX_CHUNK_SIZE};
pub use manager::{CacheManagerConfig, CacheManagerError, FetchGuard, InflightFetch};

impl CacheManager {
    #[must_use]
    pub fn get_log_cache(&self) -> Arc<log_cache::LogCache> {
        self.log_cache.clone()
    }

    #[must_use]
    pub fn get_block_cache(&self) -> Arc<block_cache::BlockCache> {
        self.block_cache.clone()
    }

    #[must_use]
    pub fn get_transaction_cache(&self) -> Arc<transaction_cache::TransactionCache> {
        self.transaction_cache.clone()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod inline_tests {
    use super::*;
    use std::sync::Arc;

    /// Tests that `CacheManager` initializes with separate cache instances for each data type.
    ///
    /// This ensures that blocks, transactions, and logs each have their own independent cache
    /// with their own configurations and capacity limits.
    #[tokio::test]
    async fn test_cache_manager_creates_separate_cache_instances() {
        let chain_state = std::sync::Arc::new(crate::chain::ChainState::new());
        let manager = CacheManager::new(&CacheManagerConfig::default(), chain_state)
            .expect("valid test cache config");

        let block_cache = manager.get_block_cache();
        let logs_cache = manager.get_log_cache();
        let tx_cache = manager.get_transaction_cache();

        // Each cache type should be a distinct Arc instance
        assert_ne!(
            Arc::as_ptr(&block_cache).cast::<()>(),
            Arc::as_ptr(&logs_cache).cast::<()>(),
            "Block and log caches should be separate instances"
        );
        assert_ne!(
            Arc::as_ptr(&block_cache).cast::<()>(),
            Arc::as_ptr(&tx_cache).cast::<()>(),
            "Block and transaction caches should be separate instances"
        );
        assert_ne!(
            Arc::as_ptr(&logs_cache).cast::<()>(),
            Arc::as_ptr(&tx_cache).cast::<()>(),
            "Log and transaction caches should be separate instances"
        );
    }

    /// Tests that calling getters multiple times returns the same Arc instances.
    ///
    /// This verifies that `CacheManager` shares cached references rather than creating
    /// new instances on each call, which is important for consistent state.
    #[tokio::test]
    async fn test_cache_manager_returns_same_instances() {
        let chain_state = std::sync::Arc::new(crate::chain::ChainState::new());
        let manager = CacheManager::new(&CacheManagerConfig::default(), chain_state)
            .expect("valid test cache config");

        let block_cache_1 = manager.get_block_cache();
        let block_cache_2 = manager.get_block_cache();

        // Same getter call should return the same Arc
        assert_eq!(
            Arc::as_ptr(&block_cache_1),
            Arc::as_ptr(&block_cache_2),
            "Multiple calls should return the same block cache instance"
        );
    }
}
