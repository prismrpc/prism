//! Tests for fetch lock operations.

use super::*;

#[test]
fn test_try_acquire_fetch_lock() {
    let cache = create_test_cache_manager();

    // First acquire should succeed
    let acquired = cache.try_acquire_fetch_lock(100);
    assert!(acquired, "First lock acquisition should succeed");

    // Check if block is being fetched
    assert!(cache.is_block_being_fetched(100));

    // Second acquire should fail
    let acquired_again = cache.try_acquire_fetch_lock(100);
    assert!(!acquired_again, "Second lock acquisition should fail");
}

#[test]
fn test_release_fetch_lock() {
    let cache = create_test_cache_manager();

    // Acquire and release
    let _ = cache.try_acquire_fetch_lock(100);
    assert!(cache.is_block_being_fetched(100));

    let released = cache.release_fetch_lock(100);
    assert!(released, "Release should return true");
    assert!(!cache.is_block_being_fetched(100));
}

#[test]
fn test_release_nonexistent_fetch_lock() {
    let cache = create_test_cache_manager();

    // Release without acquire
    let released = cache.release_fetch_lock(999);
    assert!(!released, "Release of nonexistent lock should return false");
}

#[test]
fn test_is_block_being_fetched_not_found() {
    let cache = create_test_cache_manager();

    assert!(!cache.is_block_being_fetched(100));
    assert!(!cache.is_block_being_fetched(0));
    assert!(!cache.is_block_being_fetched(u64::MAX));
}

#[test]
fn test_multiple_fetch_locks() {
    let cache = create_test_cache_manager();

    // Acquire multiple locks for different blocks
    assert!(cache.try_acquire_fetch_lock(100));
    assert!(cache.try_acquire_fetch_lock(200));
    assert!(cache.try_acquire_fetch_lock(300));

    // All should be tracked
    assert!(cache.is_block_being_fetched(100));
    assert!(cache.is_block_being_fetched(200));
    assert!(cache.is_block_being_fetched(300));

    // Release in different order
    let _ = cache.release_fetch_lock(200);
    assert!(!cache.is_block_being_fetched(200));
    assert!(cache.is_block_being_fetched(100));
    assert!(cache.is_block_being_fetched(300));
}
