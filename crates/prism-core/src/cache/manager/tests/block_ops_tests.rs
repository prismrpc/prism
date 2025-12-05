//! Tests for block operations.

use super::*;

#[test]
fn test_is_block_fully_cached_false_for_missing() {
    let cache = create_test_cache_manager();

    assert!(!cache.is_block_fully_cached(100));
    assert!(!cache.is_block_fully_cached(0));
}

#[test]
fn test_get_block_by_number_not_found() {
    let cache = create_test_cache_manager();

    let result = cache.get_block_by_number(100);
    assert!(result.is_none());
}

#[test]
fn test_get_header_by_number_not_found() {
    let cache = create_test_cache_manager();

    let result = cache.get_header_by_number(100);
    assert!(result.is_none());
}

#[test]
fn test_get_block_by_hash_not_found() {
    let cache = create_test_cache_manager();

    let hash = [0u8; 32];
    let result = cache.get_block_by_hash(&hash);
    assert!(result.is_none());
}

#[test]
fn test_get_header_by_hash_not_found() {
    let cache = create_test_cache_manager();

    let hash = [0u8; 32];
    let result = cache.get_header_by_hash(&hash);
    assert!(result.is_none());
}

#[test]
fn test_get_body_by_hash_not_found() {
    let cache = create_test_cache_manager();

    let hash = [0u8; 32];
    let result = cache.get_body_by_hash(&hash);
    assert!(result.is_none());
}

#[tokio::test]
async fn test_warm_blocks() {
    let cache = create_test_cache_manager();

    let blocks = vec![
        (create_test_header(100), create_test_body([100; 32])),
        (create_test_header(101), create_test_body([101; 32])),
    ];

    cache.warm_blocks(100, 101, blocks).await;

    assert!(cache.get_header_by_number(100).is_some());
    assert!(cache.get_header_by_number(101).is_some());
}

#[tokio::test]
async fn test_insert_header_and_body() {
    let cache = create_test_cache_manager();

    let header = create_test_header(1600);
    let body = create_test_body([0x16; 32]);

    cache.insert_header(header).await;
    cache.insert_body(body).await;

    assert!(cache.get_header_by_number(1600).is_some());
}

#[tokio::test]
async fn test_invalidate_block_across_caches() {
    let cache = create_test_cache_manager();

    cache.insert_header(create_test_header(1200)).await;

    cache.invalidate_block(1200).await;

    assert!(cache.get_header_by_number(1200).is_none());
}

#[tokio::test]
async fn test_warm_recent_orchestration() {
    let cache = create_test_cache_manager();

    let blocks = vec![(create_test_header(300), create_test_body([44; 32]))];
    let transactions = vec![];
    let receipts = vec![];
    let logs = vec![(LogId::new(300, 0), create_test_log_record(300, [44; 32]))];

    cache.warm_recent(300, 300, blocks, transactions, receipts, logs).await;

    assert!(cache.get_header_by_number(300).is_some());
}
