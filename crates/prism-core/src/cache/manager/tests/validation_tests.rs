//! Tests for log validation operations.

use super::*;
use crate::cache::types::{BlockHeader, LogId};
use std::sync::Arc;

/// Creates a full test `BlockHeader` with all required fields.
#[allow(dead_code)]
#[allow(clippy::cast_possible_truncation)]
fn create_full_test_header(block_num: u64) -> BlockHeader {
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

#[tokio::test]
async fn test_log_validation_finalized_fast_path() {
    let cache = create_test_cache_manager();
    let chain_state = cache.chain_state.clone();

    // Set finalized block at 1000
    let _ = chain_state.update_tip_simple(1100).await;
    let _ = chain_state.update_finalized(1000).await;

    // Create logs in FINALIZED range (should skip header validation)
    let logs = vec![
        (LogId::new(900, 0), create_test_log_record(900, [0x90; 32])),
        (LogId::new(1000, 0), create_test_log_record(1000, [0xA0; 32])),
    ];

    let valid_count = cache.insert_logs_validated(logs).await;
    assert_eq!(valid_count, 2, "Finalized logs should bypass header validation");
}

#[tokio::test]
async fn test_log_validation_non_finalized_missing_header() {
    let cache = create_test_cache_manager();
    let chain_state = cache.chain_state.clone();

    // Set finalized below log blocks
    let _ = chain_state.update_tip_simple(1100).await;
    let _ = chain_state.update_finalized(900).await;

    // Create logs for non-finalized blocks WITHOUT caching headers
    let logs = vec![(LogId::new(1050, 0), create_test_log_record(1050, [0x50; 32]))];

    let valid_count = cache.insert_logs_validated(logs).await;
    assert_eq!(valid_count, 0, "Non-finalized logs without headers should be rejected (TOCTOU)");
}

#[tokio::test]
async fn test_log_validation_block_hash_mismatch() {
    let cache = create_test_cache_manager();
    let chain_state = cache.chain_state.clone();

    let _ = chain_state.update_tip_simple(1100).await;
    let _ = chain_state.update_finalized(900).await;

    // Cache header with specific hash
    let correct_hash = [0x50; 32];
    cache
        .insert_header(BlockHeader {
            hash: correct_hash,
            number: 1050,
            parent_hash: [0x49; 32],
            timestamp: 12500,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [0xAA; 20],
            extra_data: Arc::new(vec![]),
            logs_bloom: Arc::new(vec![0; 256]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            receipts_root: [0; 32],
        })
        .await;

    // Create log with WRONG hash (simulating reorg)
    let logs = vec![(
        LogId::new(1050, 0),
        create_test_log_record(1050, [0xFF; 32]), // Wrong hash!
    )];

    let valid_count = cache.insert_logs_validated(logs).await;
    assert_eq!(valid_count, 0, "Logs with mismatched block hash should be rejected");
}

#[tokio::test]
async fn test_log_validation_block_hash_matches() {
    let cache = create_test_cache_manager();
    let chain_state = cache.chain_state.clone();

    let _ = chain_state.update_tip_simple(1100).await;
    let _ = chain_state.update_finalized(900).await;

    let block_hash = [0x50; 32];
    cache
        .insert_header(BlockHeader {
            hash: block_hash,
            number: 1050,
            parent_hash: [0x49; 32],
            timestamp: 12500,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [0xAA; 20],
            extra_data: Arc::new(vec![]),
            logs_bloom: Arc::new(vec![0; 256]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            receipts_root: [0; 32],
        })
        .await;

    // Create log with CORRECT hash
    let logs = vec![(LogId::new(1050, 0), create_test_log_record(1050, block_hash))];

    let valid_count = cache.insert_logs_validated(logs).await;
    assert_eq!(valid_count, 1, "Logs with matching block hash should be accepted");
}

#[tokio::test]
async fn test_log_validation_empty_input() {
    let cache = create_test_cache_manager();

    let valid_count = cache.insert_logs_validated(vec![]).await;
    assert_eq!(valid_count, 0, "Empty logs should return 0");
}

#[tokio::test]
async fn test_log_validation_no_stats_variant() {
    let cache = create_test_cache_manager();
    let chain_state = cache.chain_state.clone();

    let _ = chain_state.update_tip_simple(1100).await;
    let _ = chain_state.update_finalized(1000).await;

    let logs = vec![(LogId::new(900, 0), create_test_log_record(900, [0x90; 32]))];

    let valid_count = cache.insert_logs_validated_no_stats(logs).await;
    assert_eq!(valid_count, 1, "No-stats variant should work the same");
}

#[tokio::test]
async fn test_log_validation_mixed_valid_invalid() {
    let cache = create_test_cache_manager();
    let chain_state = cache.chain_state.clone();

    let _ = chain_state.update_tip_simple(1100).await;
    let _ = chain_state.update_finalized(1000).await;

    // Cache one header
    let valid_hash = [0x50; 32];
    cache
        .insert_header(BlockHeader {
            hash: valid_hash,
            number: 1050,
            parent_hash: [0x49; 32],
            timestamp: 12500,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [0xAA; 20],
            extra_data: Arc::new(vec![]),
            logs_bloom: Arc::new(vec![0; 256]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            receipts_root: [0; 32],
        })
        .await;

    let logs = vec![
        // Valid: finalized block
        (LogId::new(900, 0), create_test_log_record(900, [0x90; 32])),
        // Valid: correct hash
        (LogId::new(1050, 0), create_test_log_record(1050, valid_hash)),
        // Invalid: wrong hash
        (LogId::new(1050, 1), create_test_log_record(1050, [0xFF; 32])),
        // Invalid: no header cached
        (LogId::new(1060, 0), create_test_log_record(1060, [0x60; 32])),
    ];

    let valid_count = cache.insert_logs_validated(logs).await;
    assert_eq!(valid_count, 2, "Should accept 2 valid logs, reject 2 invalid");
}

#[tokio::test]
async fn test_log_validation_periodic_refresh() {
    let cache = create_test_cache_manager();
    let chain_state = cache.chain_state.clone();

    let _ = chain_state.update_tip_simple(2000).await;
    let _ = chain_state.update_finalized(1500).await;

    // Create more than 1000 logs to trigger periodic refresh
    let mut logs = Vec::with_capacity(1500);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    for i in 0..1500i32 {
        logs.push((
            LogId::new(1000 + (i / 10) as u64, (i % 10) as u32),
            create_test_log_record(1000 + (i / 10) as u64, [0x00; 32]),
        ));
    }

    // All finalized logs should pass (below 1500)
    let valid_count = cache.insert_logs_validated(logs).await;
    assert_eq!(valid_count, 1500, "Finalized logs should pass with periodic refresh");
}
