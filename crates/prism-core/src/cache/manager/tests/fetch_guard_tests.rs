//! Integration tests for `FetchGuard` RAII functionality.
//!
//! Unit tests for `InflightFetch` and `CleanupRequest` are in `fetch_guard.rs`.
//! These tests verify `FetchGuard` behavior through `CacheManager` methods.

use super::*;

#[test]
fn test_try_begin_fetch_success() {
    let cache = create_test_cache_manager();

    // Block not in cache, should succeed
    let guard = cache.try_begin_fetch(100);
    assert!(guard.is_some(), "Should acquire fetch guard for uncached block");

    // Drop the guard and verify cleanup
    drop(guard);

    // Process pending cleanup requests (normally done by background worker)
    cache.flush_pending_cleanups();

    // State should be cleaned up
    let (inflight, _locks) = cache.get_fetch_state();
    assert_eq!(inflight, 0, "Inflight should be cleaned up after drop");
}

#[test]
fn test_try_begin_fetch_concurrent_returns_none() {
    let cache = create_test_cache_manager();

    // First fetch should succeed
    let guard1 = cache.try_begin_fetch(100);
    assert!(guard1.is_some());

    // Second fetch for same block should return None
    let guard2 = cache.try_begin_fetch(100);
    assert!(guard2.is_none(), "Concurrent fetch should return None");
}

#[tokio::test]
async fn test_begin_fetch_with_timeout() {
    let cache = create_test_cache_manager();

    // Should succeed for uncached block
    let guard = cache.begin_fetch_with_timeout(100, Duration::from_millis(100)).await;
    assert!(guard.is_some());
}

#[tokio::test]
async fn test_fetch_guard_timeout_returns_none() {
    let cache = create_test_cache_manager();

    // First guard holds the permit
    let _guard1 = cache.try_begin_fetch(700).expect("First guard should succeed");

    // Second guard with very short timeout should fail
    let guard2 = cache.begin_fetch_with_timeout(700, Duration::from_millis(10)).await;
    assert!(guard2.is_none(), "Should timeout waiting for permit");
}

#[tokio::test]
#[allow(clippy::cast_possible_truncation)]
async fn test_fetch_guard_already_cached_timeout_variant() {
    let cache = create_test_cache_manager();

    // Cache the block first
    cache.insert_header(create_test_header(800)).await;
    cache.insert_body(create_test_body([(800_u64 % 256) as u8; 32])).await;

    // Try to fetch already cached block
    let guard = cache.begin_fetch_with_timeout(800, Duration::from_millis(100)).await;
    assert!(guard.is_none(), "Should not fetch already cached block");

    // Verify no state leaked
    let (inflight, _) = cache.get_fetch_state();
    assert_eq!(inflight, 0, "No inflight state for cached block");
}

#[tokio::test]
async fn test_fetch_guard_cleanup_on_drop() {
    let cache = create_test_cache_manager();

    {
        let _guard = cache.try_begin_fetch(900).expect("Should acquire guard");
        assert!(cache.inflight.contains_key(&900));
        assert!(cache.is_block_being_fetched(900));
    } // guard dropped here

    // Process cleanup
    cache.flush_pending_cleanups();

    assert!(!cache.inflight.contains_key(&900), "Inflight should be cleaned");
    assert!(!cache.is_block_being_fetched(900), "Fetch lock should be released");
}

#[tokio::test]
#[allow(clippy::cast_possible_truncation)]
async fn test_fetch_guard_double_check_pattern() {
    let cache = create_test_cache_manager();

    // Start fetch
    let guard = cache.try_begin_fetch(950);
    assert!(guard.is_some());
    drop(guard);
    cache.flush_pending_cleanups();

    // Now cache the block
    cache.insert_header(create_test_header(950)).await;
    cache.insert_body(create_test_body([(950_u64 % 256) as u8; 32])).await;

    // Try to fetch again - should fail at double-check
    let guard2 = cache.try_begin_fetch(950);
    assert!(guard2.is_none(), "Double-check should prevent fetch of cached block");
}

#[tokio::test]
async fn test_fetch_guard_multiple_different_blocks() {
    let cache = create_test_cache_manager();

    let guard1 = cache.try_begin_fetch(1000);
    let guard2 = cache.try_begin_fetch(1001);
    let guard3 = cache.try_begin_fetch(1002);

    assert!(guard1.is_some());
    assert!(guard2.is_some());
    assert!(guard3.is_some());

    let (inflight, locks) = cache.get_fetch_state();
    assert_eq!(inflight, 3);
    assert_eq!(locks, 3);
}

#[tokio::test]
async fn test_cleanup_fetch_state_helper() {
    let cache = create_test_cache_manager();

    // Manually create state
    cache.inflight.insert(
        1100,
        InflightFetch { semaphore: Arc::new(Semaphore::new(1)), started_at: Instant::now() },
    );
    cache.blocks_being_fetched.insert(1100);

    cache.cleanup_fetch_state(1100);

    assert!(!cache.inflight.contains_key(&1100));
    assert!(!cache.blocks_being_fetched.contains(&1100));
}
