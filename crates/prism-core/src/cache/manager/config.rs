//! Configuration and error types for the cache manager.
//!
//! This module contains the configuration structs and error types used by
//! `CacheManager` and its sub-components.

use crate::cache::{
    block_cache::{BlockCacheConfig, BlockCacheError},
    log_cache::{LogCacheConfig, LogCacheConfigError},
    reorg_manager::ReorgManagerConfig,
    transaction_cache::{TransactionCacheConfig, TransactionCacheError},
};
use thiserror::Error;

/// Errors that occur during cache manager initialization.
///
/// Each variant wraps a specific cache component's initialization error,
/// providing context about which component failed to start.
#[derive(Debug, Error)]
pub enum CacheManagerError {
    /// Log cache failed to initialize, typically due to invalid chunk size
    #[error("Log cache initialization failed: {0}")]
    LogCache(#[from] LogCacheConfigError),

    /// Block cache failed to initialize, typically due to zero capacity or invalid config
    #[error("Block cache initialization failed: {0}")]
    BlockCache(#[from] BlockCacheError),

    /// Transaction cache failed to initialize, typically due to zero capacity
    #[error("Transaction cache initialization failed: {0}")]
    TransactionCache(#[from] TransactionCacheError),
}

/// Configuration for cache manager behavior and background cleanup.
///
/// Aggregates configuration for all cache components (logs, blocks, transactions)
/// and controls automatic cleanup scheduling. Background cleanup prunes old data
/// to prevent unbounded memory growth while preserving recent and finalized data.
///
/// # Cleanup Triggers
///
/// Cleanup can be triggered two ways:
/// - **Time-based**: Every `cleanup_interval_seconds` (default: 5 minutes)
/// - **Block-based**: Every `cleanup_every_n_blocks` (default: 100 blocks)
///
/// Set either to 0 to disable that trigger. Both are enabled by default for robust cleanup.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheManagerConfig {
    /// Configuration for log cache (chunk size, bitmap limits, safety depth)
    pub log_cache: LogCacheConfig,
    /// Configuration for block cache (hot window size, LRU capacities)
    pub block_cache: BlockCacheConfig,
    /// Configuration for transaction and receipt caches
    pub transaction_cache: TransactionCacheConfig,
    /// Configuration for reorg detection and handling
    pub reorg_manager: ReorgManagerConfig,
    /// Number of blocks to retain beyond finalized checkpoint (default: 1000)
    pub retain_blocks: u64,
    /// Enable automatic background cleanup tasks (default: true)
    pub enable_auto_cleanup: bool,
    /// Cleanup interval in seconds (default: 300 = 5 minutes, 0 to disable)
    pub cleanup_interval_seconds: u64,
    /// Trigger cleanup every N blocks on tip updates (default: 100, 0 to disable)
    pub cleanup_every_n_blocks: u64,
}

impl Default for CacheManagerConfig {
    fn default() -> Self {
        Self {
            log_cache: LogCacheConfig::default(),
            block_cache: BlockCacheConfig::default(),
            transaction_cache: TransactionCacheConfig::default(),
            reorg_manager: ReorgManagerConfig::default(),
            retain_blocks: 1000,
            enable_auto_cleanup: true,
            cleanup_interval_seconds: 300, // 5 minutes
            cleanup_every_n_blocks: 100,   // Every 100 blocks
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default_values() {
        let config = CacheManagerConfig::default();

        assert_eq!(config.retain_blocks, 1000);
        assert!(config.enable_auto_cleanup);
        assert_eq!(config.cleanup_interval_seconds, 300);
        assert_eq!(config.cleanup_every_n_blocks, 100);
    }

    #[test]
    fn test_config_custom_values() {
        let config = CacheManagerConfig {
            retain_blocks: 500,
            enable_auto_cleanup: false,
            cleanup_interval_seconds: 60,
            cleanup_every_n_blocks: 50,
            ..CacheManagerConfig::default()
        };

        assert_eq!(config.retain_blocks, 500);
        assert!(!config.enable_auto_cleanup);
        assert_eq!(config.cleanup_interval_seconds, 60);
        assert_eq!(config.cleanup_every_n_blocks, 50);
    }

    #[test]
    fn test_error_display() {
        use crate::cache::log_cache::LogCacheConfigError;

        let err: CacheManagerError =
            CacheManagerError::LogCache(LogCacheConfigError::ChunkSizeTooLarge {
                chunk_size: 9999,
                max: 1000,
            });
        let display = format!("{err}");
        assert!(display.contains("Log cache"));
    }
}
