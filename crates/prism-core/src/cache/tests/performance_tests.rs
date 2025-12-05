//! Cache correctness tests for concurrent operations.
//!
//! These tests verify the correctness of cache operations under concurrent access.
//! For performance benchmarks, see `benches/cache_benchmarks.rs`.
//!
//! Run benchmarks with: `cargo bench -p prism-core --bench cache_benchmarks`

use crate::cache::{CacheManager, CacheManagerConfig};
use std::{sync::Arc, thread, time::Duration};

/// Test that fetch lock acquire/release semantics are correct.
///
/// Verifies:
/// - First acquisition succeeds
/// - Second acquisition of same block fails
/// - After release, acquisition succeeds again
#[tokio::test]
async fn test_cache_fetch_lock_correctness() {
    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(crate::chain::ChainState::new());
    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state).expect("valid test cache config"));

    let block_id = 12345u64;

    // First acquisition should succeed
    assert!(cache_manager.try_acquire_fetch_lock(block_id));

    // Second acquisition should fail
    assert!(!cache_manager.try_acquire_fetch_lock(block_id));

    // After release, acquisition should succeed again
    assert!(cache_manager.release_fetch_lock(block_id));
    assert!(cache_manager.try_acquire_fetch_lock(block_id));

    // Clean up
    assert!(cache_manager.release_fetch_lock(block_id));
}

/// Test concurrent fetch locks with unique block IDs.
///
/// Verifies that multiple threads can acquire locks for different blocks
/// without interference, and that lock state tracking is correct.
#[tokio::test]
async fn test_concurrent_fetch_locks() {
    const NUM_THREADS: usize = 8;
    const BLOCKS_PER_THREAD: usize = 100;

    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(crate::chain::ChainState::new());
    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state).expect("valid test cache config"));

    let mut handles = Vec::new();
    let success_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    for thread_id in 0..NUM_THREADS {
        let cache_manager = cache_manager.clone();
        let success_count = success_count.clone();

        let handle = thread::spawn(move || {
            let mut local_success = 0;

            for i in 0..BLOCKS_PER_THREAD {
                let block_id = (thread_id * BLOCKS_PER_THREAD + i) as u64;

                if cache_manager.try_acquire_fetch_lock(block_id) {
                    local_success += 1;

                    // Simulate some work
                    thread::sleep(Duration::from_micros(10));

                    // Verify the block is being tracked as fetched
                    assert!(cache_manager.is_block_being_fetched(block_id));

                    // Release the lock
                    assert!(cache_manager.release_fetch_lock(block_id));

                    // Verify it's no longer being fetched
                    assert!(!cache_manager.is_block_being_fetched(block_id));
                }
            }

            success_count.fetch_add(local_success, std::sync::atomic::Ordering::Relaxed);
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // All operations should have succeeded since we're using unique block IDs
    let total_success = success_count.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(total_success, NUM_THREADS * BLOCKS_PER_THREAD);
}

/// Test that fetch locks work correctly under high contention.
///
/// Multiple threads compete for locks on a small set of shared block IDs.
/// This tests the thread-safety of the locking mechanism.
#[tokio::test]
async fn test_high_contention_correctness() {
    const NUM_THREADS: usize = 8;
    const SHARED_BLOCKS: u64 = 10;
    const ATTEMPTS_PER_THREAD: usize = 100;

    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(crate::chain::ChainState::new());
    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state).expect("valid test cache config"));

    let total_acquisitions = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let mut handles = Vec::new();

    for thread_id in 0..NUM_THREADS {
        let cache_manager = cache_manager.clone();
        let total_acquisitions = total_acquisitions.clone();

        let handle = thread::spawn(move || {
            let mut local_acquisitions = 0;

            for i in 0..ATTEMPTS_PER_THREAD {
                let block_id = ((thread_id * ATTEMPTS_PER_THREAD + i) as u64) % SHARED_BLOCKS;

                if cache_manager.try_acquire_fetch_lock(block_id) {
                    local_acquisitions += 1;

                    // Hold the lock briefly
                    thread::sleep(Duration::from_micros(1));

                    // Release should always succeed after successful acquire
                    assert!(
                        cache_manager.release_fetch_lock(block_id),
                        "Release should succeed after acquire"
                    );
                }
            }

            total_acquisitions.fetch_add(local_acquisitions, std::sync::atomic::Ordering::Relaxed);
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Should have some successful acquisitions (exact count varies due to contention)
    let acquisitions = total_acquisitions.load(std::sync::atomic::Ordering::Relaxed);
    assert!(acquisitions > 0, "Should have at least some successful acquisitions");
}
