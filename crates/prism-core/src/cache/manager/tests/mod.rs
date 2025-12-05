//! Tests for cache manager functionality.
//!
//! This module contains comprehensive unit and integration tests for `CacheManager`.
//! Tests are organized by functionality area for maintainability.

use crate::{
    cache::{
        cache_manager::CacheManager,
        manager::{CacheManagerConfig, CleanupRequest, InflightFetch},
        types::{BlockBody, BlockHeader, LogFilter, LogId, LogRecord},
    },
    chain::ChainState,
};
use dashmap::{DashMap, DashSet};
use serde_json::json;
use std::sync::Arc;
use tokio::{
    sync::{broadcast, mpsc, Semaphore},
    time::{Duration, Instant},
};

// ============================================================================
// Shared Test Helpers
// ============================================================================

/// Creates a test `CacheManager` with minimal configuration.
pub(crate) fn create_test_cache_manager() -> Arc<CacheManager> {
    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(ChainState::new());
    Arc::new(CacheManager::new(&config, chain_state).expect("valid test cache config"))
}

/// Creates a test `BlockHeader` with all required fields.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn create_test_header(block_num: u64) -> BlockHeader {
    BlockHeader {
        hash: [(block_num % 256) as u8; 32],
        number: block_num,
        parent_hash: [((block_num.saturating_sub(1)) % 256) as u8; 32],
        timestamp: block_num * 12,
        gas_limit: 30_000_000,
        gas_used: 15_000_000,
        miner: [0xAA; 20],
        extra_data: Arc::new(vec![]),
        logs_bloom: Arc::new(vec![0; 256]),
        transactions_root: [0; 32],
        state_root: [0; 32],
        receipts_root: [0; 32],
    }
}

/// Creates a test `BlockBody` with the given hash.
pub(crate) fn create_test_body(block_hash: [u8; 32]) -> BlockBody {
    BlockBody { hash: block_hash, transactions: vec![[1u8; 32], [2u8; 32]] }
}

/// Creates a test `LogRecord` for the given block.
pub(crate) fn create_test_log_record(_block_num: u64, block_hash: [u8; 32]) -> LogRecord {
    LogRecord {
        address: [0xCC; 20],
        topics: [Some([0xDD; 32]), None, None, None],
        data: vec![1, 2, 3],
        transaction_hash: [0xEE; 32],
        block_hash,
        transaction_index: 0,
        removed: false,
    }
}

// ============================================================================
// Test Submodules
// ============================================================================

mod block_ops_tests;
mod cleanup_tests;
mod config_tests;
mod fetch_guard_tests;
mod fetch_lock_tests;
mod log_ops_tests;
mod range_tests;
mod stats_tests;
mod transaction_tests;
mod validation_tests;
