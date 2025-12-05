//! Tests for cache stats operations.

use super::*;

#[tokio::test]
async fn test_get_stats_empty_cache() {
    let cache = create_test_cache_manager();

    let stats = cache.get_stats().await;

    assert_eq!(stats.log_store_size, 0);
    assert_eq!(stats.header_cache_size, 0);
    assert_eq!(stats.body_cache_size, 0);
    assert_eq!(stats.transaction_cache_size, 0);
    assert_eq!(stats.receipt_cache_size, 0);
}

#[test]
fn test_get_fetch_state_empty() {
    let cache = create_test_cache_manager();

    let (inflight, locks) = cache.get_fetch_state();
    assert_eq!(inflight, 0);
    assert_eq!(locks, 0);
}

#[test]
fn test_get_fetch_state_with_locks() {
    let cache = create_test_cache_manager();

    let _ = cache.try_acquire_fetch_lock(100);
    let _ = cache.try_acquire_fetch_lock(200);

    let (inflight, locks) = cache.get_fetch_state();
    // Note: inflight entries are created by try_begin_fetch, not try_acquire_fetch_lock
    assert_eq!(inflight, 0);
    assert_eq!(locks, 2);
}

#[test]
fn test_force_cleanup_fetches() {
    let cache = create_test_cache_manager();

    // Create some fetch state
    let _ = cache.try_acquire_fetch_lock(100);
    let _ = cache.try_acquire_fetch_lock(200);
    let _ = cache.try_acquire_fetch_lock(300);

    let (_, locks_before) = cache.get_fetch_state();
    assert_eq!(locks_before, 3);

    // Force cleanup
    cache.force_cleanup_fetches();

    let (inflight, locks) = cache.get_fetch_state();
    assert_eq!(inflight, 0);
    assert_eq!(locks, 0);
}

#[tokio::test]
async fn test_should_update_stats() {
    let cache = create_test_cache_manager();

    // Initial state should allow update
    let should_update = cache.should_update_stats().await;
    // Result depends on internal state, just verify it doesn't panic
    let _ = should_update;
}

#[tokio::test]
async fn test_force_update_stats() {
    let cache = create_test_cache_manager();

    // Should not panic
    cache.force_update_stats().await;
}

#[tokio::test]
async fn test_get_changes_since_last_stats() {
    let cache = create_test_cache_manager();

    let changes = cache.get_changes_since_last_stats();
    assert_eq!(changes, 0, "Empty cache should have 0 changes");
}

#[tokio::test]
async fn test_update_stats_on_demand() {
    let cache = create_test_cache_manager();
    cache.update_stats_on_demand().await;
}

#[tokio::test]
async fn test_clear_cache() {
    let cache = create_test_cache_manager();

    // Clear should not panic on empty cache
    cache.clear_cache().await;

    let stats = cache.get_stats().await;
    assert_eq!(stats.log_store_size, 0);
}

#[test]
fn test_get_current_tip_initial() {
    let cache = create_test_cache_manager();
    assert_eq!(cache.get_current_tip(), 0, "Initial tip should be 0");
}

#[test]
fn test_get_finalized_block_initial() {
    let cache = create_test_cache_manager();
    assert_eq!(cache.get_finalized_block(), 0, "Initial finalized should be 0");
}
