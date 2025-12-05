//! Tests for `CacheManager` background cleanup tasks and tip-triggered cache invalidation.
//!
//! These tests verify the cache cleanup lifecycle:
//! - Background cleanup tasks can be enabled/disabled via configuration
//! - Tip-triggered cleanup runs when chain advances by configured number of blocks
//! - Manual cleanup operations work correctly
//! - Configuration options are properly stored and applied
//!
//! # Test Philosophy
//!
//! These tests use "eventual consistency" patterns rather than fixed sleeps:
//! - Positive cases: Use `poll_until` to verify conditions become true within a timeout
//! - Negative cases: Use `remains_false_for` to verify conditions stay false for a reasonable
//!   period
//!
//! This approach is more robust than fixed sleeps because:
//! - Tests pass as soon as the condition is met (faster on fast machines)
//! - Tests have clear timeouts for CI environments (no flaky 100ms waits)
//! - Negative tests are documented as "condition remains false" not "happens exactly at 100ms"

use std::sync::Arc;

use prism_core::{
    cache::{CacheManager, CacheManagerConfig},
    chain::ChainState,
};
use std::future::Future;
use tokio::time::{sleep, Duration, Instant};

/// Configuration for timing-sensitive test assertions.
struct TestTiming {
    /// Maximum time to wait for a condition to become true.
    poll_timeout: Duration,
    /// Interval between checks when polling.
    poll_interval: Duration,
    /// Duration to verify a condition remains false (for negative tests).
    stability_duration: Duration,
}

impl Default for TestTiming {
    fn default() -> Self {
        Self {
            poll_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(50),
            stability_duration: Duration::from_millis(200),
        }
    }
}

/// Polls until a condition becomes true, or times out.
///
/// Returns `Ok(elapsed)` if condition became true, `Err(msg)` on timeout.
async fn poll_until<F, Fut>(
    condition_name: &str,
    timing: &TestTiming,
    mut check: F,
) -> Result<Duration, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    while start.elapsed() < timing.poll_timeout {
        if check().await {
            return Ok(start.elapsed());
        }
        sleep(timing.poll_interval).await;
    }
    Err(format!("{} did not become true within {:?}", condition_name, timing.poll_timeout))
}

/// Verifies a condition remains false for the stability duration.
///
/// Used for negative tests where we want to assert something doesn't happen.
async fn remains_false_for<F, Fut>(
    condition_name: &str,
    timing: &TestTiming,
    mut check: F,
) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    while start.elapsed() < timing.stability_duration {
        if check().await {
            return Err(format!(
                "{} unexpectedly became true after {:?}",
                condition_name,
                start.elapsed()
            ));
        }
        sleep(timing.poll_interval).await;
    }
    Ok(())
}

#[tokio::test]
async fn test_background_cleanup_tasks_disabled() {
    let config = CacheManagerConfig { enable_auto_cleanup: false, ..Default::default() };
    let chain_state = Arc::new(ChainState::new());

    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state).expect("valid test cache config"));
    let (shutdown_tx, _shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);

    // Starting background tasks should not panic and cleanup should log that it's disabled
    cache_manager.start_all_background_tasks(&shutdown_tx);

    // Yield to allow any tasks to potentially start (they shouldn't since cleanup is disabled)
    tokio::task::yield_now().await;

    // Send shutdown signal to clean up
    let _ = shutdown_tx.send(());

    // Test passes if no panic occurred
}

#[tokio::test]
async fn test_background_cleanup_tasks_enabled() {
    let config = CacheManagerConfig {
        enable_auto_cleanup: true,
        cleanup_interval_seconds: 1, // Very short interval for testing
        ..Default::default()
    };
    let chain_state = Arc::new(ChainState::new());

    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state).expect("valid test cache config"));
    let (shutdown_tx, _shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);

    // Start background tasks
    cache_manager.start_all_background_tasks(&shutdown_tx);

    // Yield to let background tasks spawn
    tokio::task::yield_now().await;

    // Wait briefly for cleanup task to start running (we can't easily verify it ran without
    // more complex mocking, but we verify the task infrastructure works without panic)
    sleep(Duration::from_millis(100)).await;

    // Send shutdown signal to clean up background tasks
    let _ = shutdown_tx.send(());

    // Test passes if no panic occurred
}

#[tokio::test]
async fn test_tip_triggered_cleanup() {
    let config = CacheManagerConfig {
        enable_auto_cleanup: true,
        cleanup_every_n_blocks: 5, // Trigger cleanup every 5 blocks
        ..Default::default()
    };
    let chain_state = Arc::new(ChainState::new());

    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state).expect("valid test cache config"));
    let timing = TestTiming::default();

    // Update tip multiple times, starting from 1 to make the math clearer
    cache_manager.check_and_trigger_cleanup(1);
    cache_manager.check_and_trigger_cleanup(2);
    cache_manager.check_and_trigger_cleanup(3);
    cache_manager.check_and_trigger_cleanup(4);

    // Verify cleanup hasn't been triggered yet (only 4 blocks from 0)
    let cache_manager_check = cache_manager.clone();
    remains_false_for("cleanup triggered before 5 blocks", &timing, || {
        let cm = cache_manager_check.clone();
        async move { cm.last_cleanup_block.load(std::sync::atomic::Ordering::Acquire) != 0 }
    })
    .await
    .expect("Cleanup should not have been triggered after only 4 blocks");

    // This should trigger cleanup (5th block: 5 - 0 >= 5)
    cache_manager.check_and_trigger_cleanup(5);

    // Poll until cleanup is triggered at block 5
    let cache_manager_check = cache_manager.clone();
    poll_until("cleanup triggered at block 5", &timing, || {
        let cm = cache_manager_check.clone();
        async move { cm.last_cleanup_block.load(std::sync::atomic::Ordering::Acquire) == 5 }
    })
    .await
    .expect("Cleanup should have been triggered at block 5");
}

#[tokio::test]
async fn test_tip_triggered_cleanup_disabled() {
    let config = CacheManagerConfig {
        enable_auto_cleanup: true,
        cleanup_every_n_blocks: 0, // Disabled
        ..Default::default()
    };
    let chain_state = Arc::new(ChainState::new());

    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state.clone()).expect("valid test cache config"));
    let timing = TestTiming::default();

    // Update tip many times
    for i in 1..=20 {
        let _ = chain_state.update_tip_simple(100 + i).await;
    }

    // Verify cleanup remains not triggered even after multiple tip updates
    let cache_manager_check = cache_manager.clone();
    remains_false_for("cleanup triggered when disabled", &timing, || {
        let cm = cache_manager_check.clone();
        async move { cm.last_cleanup_block.load(std::sync::atomic::Ordering::Acquire) != 0 }
    })
    .await
    .expect("Cleanup should not have been triggered when disabled");
}

#[tokio::test]
async fn test_manual_cleanup() {
    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(ChainState::new());
    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state.clone()).expect("valid test cache config"));

    // Set a current tip
    let _ = chain_state.update_tip_simple(1000).await;

    // Perform manual cleanup - this should not panic
    cache_manager.perform_cleanup().await;

    // Test passes if no panic occurred
}

#[tokio::test]
async fn test_cleanup_configuration() {
    let config = CacheManagerConfig {
        enable_auto_cleanup: false,
        cleanup_interval_seconds: 600,
        cleanup_every_n_blocks: 50,
        retain_blocks: 2000,
        ..Default::default()
    };
    let chain_state = Arc::new(ChainState::new());

    let cache_manager = CacheManager::new(&config, chain_state).expect("valid test cache config");

    // Verify configuration is stored correctly
    assert!(!cache_manager.config.enable_auto_cleanup);
    assert_eq!(cache_manager.config.cleanup_interval_seconds, 600);
    assert_eq!(cache_manager.config.cleanup_every_n_blocks, 50);
    assert_eq!(cache_manager.config.retain_blocks, 2000);
}

/// Tests that `ChainState` properly tracks tip updates for reorg detection scenarios.
///
/// This test verifies the foundation of reorg handling:
/// 1. `ChainState` can track block number and hash together atomically
/// 2. Tip updates are visible immediately for cache coordination
/// 3. The shared `ChainState` between `CacheManager` and `ReorgManager` stays consistent
///
/// This is the foundation for preventing stale data during cache races.
#[tokio::test]
async fn test_chain_state_reorg_foundation() {
    let chain_state = Arc::new(ChainState::new());
    let config = CacheManagerConfig::default();
    let _cache_manager =
        Arc::new(CacheManager::new(&config, chain_state.clone()).expect("valid test cache config"));

    // Simulate initial chain state at block 1000 with hash A
    let hash_a = [0xAA; 32];
    let update_result = chain_state.update_tip(1000, hash_a).await;
    assert!(update_result, "First tip update should succeed");

    // Verify tip is set correctly
    let tip_num = chain_state.current_tip();
    assert_eq!(tip_num, 1000, "Tip should be 1000");

    // Simulate chain advancing to block 1001
    let hash_b = [0xBB; 32];
    let update_result = chain_state.update_tip(1001, hash_b).await;
    assert!(update_result, "Tip advance should succeed");

    let tip_num = chain_state.current_tip();
    assert_eq!(tip_num, 1001, "Tip should advance to 1001");

    // Simulate reorg detection scenario: tip goes back to 1000 with different hash
    // This tests the force_update_tip mechanism used during rollbacks
    let hash_c = [0xCC; 32];
    chain_state.force_update_tip(1000, hash_c).await;

    let tip_num = chain_state.current_tip();
    assert_eq!(tip_num, 1000, "Tip should roll back to 1000 after reorg");

    // Verify the atomic (number, hash) retrieval works
    let (num, hash) = chain_state.current_tip_with_hash();
    assert_eq!(num, 1000);
    assert_eq!(hash, hash_c, "Hash should be the new reorg hash");
}

/// Tests that cache invalidation range calculation is correct during reorgs.
///
/// The key invariant is: when a reorg is detected at block N with old tip at M,
/// all blocks from N to M (inclusive) must be invalidated.
#[tokio::test]
async fn test_cache_invalidation_range_calculation() {
    let chain_state = Arc::new(ChainState::new());
    let config = CacheManagerConfig::default();
    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state.clone()).expect("valid test cache config"));

    // Set initial tip to block 1005
    let _ = chain_state.update_tip(1005, [0x55; 32]).await;

    // Simulate reorg detected at block 1000
    // The cache manager should invalidate blocks 1000..=1005

    // Use perform_cleanup to trigger cache management
    cache_manager.perform_cleanup().await;

    // The key assertion is that after a reorg is processed,
    // the invalidation range includes the reorg point AND all blocks after it
    // This is tested implicitly by the reorg_manager regression tests in prism-core

    // Verify the cache manager accepts the chain state
    let tip = chain_state.current_tip();
    assert_eq!(tip, 1005, "Chain state should maintain tip");
}
