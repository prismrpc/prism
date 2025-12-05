//! Integration tests for cleanup operations.
//!
//! Unit tests for `process_cleanup_batch` are in `background.rs`.
//! These tests verify cleanup behavior through `CacheManager`.

use super::*;
use crate::cache::manager::background;

#[tokio::test]
async fn test_perform_cleanup_no_tip() {
    let cache = create_test_cache_manager();

    // Should not panic when tip is 0
    cache.perform_cleanup().await;
}

#[tokio::test]
async fn test_cleanup_worker_processes_requests() {
    let inflight: DashMap<u64, InflightFetch> = DashMap::new();
    let blocks_being_fetched: DashSet<u64> = DashSet::new();

    // Setup inflight entries
    inflight.insert(
        1000,
        InflightFetch { semaphore: Arc::new(Semaphore::new(1)), started_at: Instant::now() },
    );
    inflight.insert(
        1001,
        InflightFetch { semaphore: Arc::new(Semaphore::new(1)), started_at: Instant::now() },
    );
    blocks_being_fetched.insert(1000);
    blocks_being_fetched.insert(1001);

    assert_eq!(inflight.len(), 2);
    assert_eq!(blocks_being_fetched.len(), 2);

    // Create channel and send cleanup requests
    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    tx.send(CleanupRequest { block: 1000, release_fetch_lock: true }).unwrap();
    tx.send(CleanupRequest { block: 1001, release_fetch_lock: true }).unwrap();

    // Drop sender to close channel (simulates all FetchGuards dropped)
    drop(tx);

    // Run the cleanup worker (it will exit when channel closes)
    background::run_cleanup_worker(rx, inflight, blocks_being_fetched, shutdown_rx).await;

    drop(shutdown_tx);
}

#[tokio::test]
async fn test_cleanup_worker_shutdown() {
    let inflight: DashMap<u64, InflightFetch> = DashMap::new();
    let blocks_being_fetched: DashSet<u64> = DashSet::new();

    // Setup inflight entries
    for i in 0..5u64 {
        inflight.insert(
            2000 + i,
            InflightFetch { semaphore: Arc::new(Semaphore::new(1)), started_at: Instant::now() },
        );
        blocks_being_fetched.insert(2000 + i);
    }

    assert_eq!(inflight.len(), 5);

    // Create channel
    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    // Send cleanup requests
    for i in 0..5u64 {
        tx.send(CleanupRequest { block: 2000 + i, release_fetch_lock: true }).unwrap();
    }

    // Send shutdown signal immediately
    let _ = shutdown_tx.send(());

    // Keep sender alive during worker execution, drop after
    let _tx_handle = tx;

    // Run the cleanup worker (it will exit on shutdown after draining)
    background::run_cleanup_worker(rx, inflight, blocks_being_fetched, shutdown_rx).await;
}

#[tokio::test]
async fn test_update_tip_no_cleanup_when_disabled() {
    let config = CacheManagerConfig { cleanup_every_n_blocks: 0, ..Default::default() };
    let chain_state = Arc::new(ChainState::new());
    let _ = chain_state.update_tip_simple(100).await;
    let cache = Arc::new(CacheManager::new(&config, chain_state).unwrap());

    cache.check_and_trigger_cleanup(200);

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        cache.last_cleanup_block.load(std::sync::atomic::Ordering::Acquire),
        0,
        "Cleanup should not trigger when disabled"
    );
}

#[tokio::test]
async fn test_update_tip_triggers_cleanup() {
    let config = CacheManagerConfig { cleanup_every_n_blocks: 10, ..Default::default() };
    let chain_state = Arc::new(ChainState::new());
    let _ = chain_state.update_tip_simple(100).await;
    let cache = Arc::new(CacheManager::new(&config, chain_state).unwrap());

    // Update by more than threshold
    cache.check_and_trigger_cleanup(115);

    // Wait for spawned task
    tokio::time::sleep(Duration::from_millis(200)).await;

    let last_cleanup = cache.last_cleanup_block.load(std::sync::atomic::Ordering::Acquire);
    assert!(last_cleanup > 0, "Cleanup should have been triggered");
}

#[tokio::test]
async fn test_update_tip_no_regression() {
    let config = CacheManagerConfig { cleanup_every_n_blocks: 10, ..Default::default() };
    let chain_state = Arc::new(ChainState::new());
    let _ = chain_state.update_tip_simple(200).await;
    let cache = Arc::new(CacheManager::new(&config, chain_state).unwrap());

    // Try to update with lower block
    cache.check_and_trigger_cleanup(150);

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        cache.last_cleanup_block.load(std::sync::atomic::Ordering::Acquire),
        0,
        "Cleanup should not trigger for tip regression"
    );
}

#[tokio::test]
async fn test_perform_cleanup_with_finalized() {
    let config = CacheManagerConfig { retain_blocks: 100, ..Default::default() };
    let chain_state = Arc::new(ChainState::new());
    let _ = chain_state.update_tip_simple(1000).await;
    let _ = chain_state.update_finalized(900).await;

    let cache = Arc::new(CacheManager::new(&config, chain_state).unwrap());

    // Cache some blocks
    for num in 850..=1000 {
        cache.insert_header(create_test_header(num)).await;
    }

    cache.perform_cleanup().await;

    // Finalized blocks should be protected
    assert!(cache.get_header_by_number(900).is_some(), "Finalized block should be protected");
}

#[tokio::test]
async fn test_perform_cleanup_without_finalized() {
    let config = CacheManagerConfig { retain_blocks: 50, ..Default::default() };
    let chain_state = Arc::new(ChainState::new());
    let _ = chain_state.update_tip_simple(500).await;
    // Don't set finalized (stays at 0)

    let cache = Arc::new(CacheManager::new(&config, chain_state).unwrap());

    for num in 400..=500 {
        cache.insert_header(create_test_header(num)).await;
    }

    cache.perform_cleanup().await;

    // Recent blocks should be retained
    assert!(cache.get_header_by_number(500).is_some());
}

#[tokio::test]
async fn test_perform_cleanup_executes() {
    let cache = create_test_cache_manager();
    let _ = cache.chain_state.update_tip_simple(1000).await;

    // Should complete without panicking
    cache.perform_cleanup().await;
}

#[tokio::test]
async fn test_start_background_cleanup_disabled() {
    let config = CacheManagerConfig { enable_auto_cleanup: false, ..Default::default() };
    let chain_state = Arc::new(ChainState::new());
    let cache = Arc::new(CacheManager::new(&config, chain_state).unwrap());
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Should not panic when disabled
    cache.start_background_cleanup_tasks(shutdown_tx.subscribe());

    let _ = shutdown_tx.send(());
}
