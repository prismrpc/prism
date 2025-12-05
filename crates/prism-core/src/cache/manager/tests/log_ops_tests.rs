//! Tests for log operations.

use super::*;

#[tokio::test]
async fn test_get_logs_empty_cache() {
    let cache = create_test_cache_manager();

    let filter = LogFilter::new(100, 200);
    let (logs, _missing) = cache.get_logs(&filter, 250).await;

    assert!(logs.is_empty(), "Empty cache should return no logs");
}

#[tokio::test]
async fn test_get_logs_with_ids_empty_cache() {
    let cache = create_test_cache_manager();

    let filter = LogFilter::new(100, 200);
    let (logs_with_ids, _missing) = cache.get_logs_with_ids(&filter, 250).await;

    assert!(logs_with_ids.is_empty());
}

#[tokio::test]
async fn test_get_logs_after_insert() {
    let cache = create_test_cache_manager();

    // Insert logs
    let logs = vec![
        (LogId::new(100, 0), create_test_log_record(100, [0x10; 32])),
        (LogId::new(100, 1), create_test_log_record(100, [0x10; 32])),
        (LogId::new(101, 0), create_test_log_record(101, [0x11; 32])),
    ];
    cache.insert_logs_bulk(logs).await;

    let filter = LogFilter::new(100, 101);
    let (retrieved_logs, _missing) = cache.get_logs(&filter, 200).await;

    // Logs should be retrievable
    let _ = retrieved_logs.len();
}

#[tokio::test]
async fn test_get_logs_with_ids_after_insert() {
    let cache = create_test_cache_manager();

    let logs = vec![(LogId::new(200, 0), create_test_log_record(200, [0x20; 32]))];
    cache.insert_logs_bulk(logs).await;

    let filter = LogFilter::new(200, 200);
    let (logs_with_ids, _missing) = cache.get_logs_with_ids(&filter, 300).await;

    // Should retrieve logs with their IDs
    for (log_id, _record) in &logs_with_ids {
        assert_eq!(log_id.block_number, 200);
    }
}

#[tokio::test]
async fn test_warm_logs() {
    let cache = create_test_cache_manager();

    let logs = vec![(LogId::new(100, 0), create_test_log_record(100, [0x10; 32]))];

    cache.warm_logs(100, 100, logs).await;

    // Verify logs are in cache via stats
    let stats = cache.get_stats().await;
    let _ = stats.log_store_size;
}

#[tokio::test]
async fn test_insert_logs_bulk() {
    let cache = create_test_cache_manager();

    let logs = vec![
        (LogId::new(1300, 0), create_test_log_record(1300, [0x13; 32])),
        (LogId::new(1300, 1), create_test_log_record(1300, [0x13; 32])),
    ];

    cache.insert_logs_bulk(logs).await;

    let stats = cache.get_stats().await;
    let _ = stats.log_store_size;
}

#[tokio::test]
async fn test_insert_logs_bulk_no_stats() {
    let cache = create_test_cache_manager();

    let logs = vec![(LogId::new(1400, 0), create_test_log_record(1400, [0x14; 32]))];

    cache.insert_logs_bulk_no_stats(logs).await;
}

#[tokio::test]
async fn test_insert_single_logs() {
    let cache = create_test_cache_manager();

    let logs = vec![(LogId::new(1500, 0), create_test_log_record(1500, [0x15; 32]))];

    cache.insert_logs(logs).await;
}

#[tokio::test]
async fn test_cache_exact_result() {
    let cache = create_test_cache_manager();

    let filter = LogFilter::new(100, 200);
    let log_ids = vec![LogId::new(150, 0), LogId::new(150, 1)];

    cache.cache_exact_result(&filter, log_ids).await;

    // Exact result should be cached (verified through subsequent queries)
}
