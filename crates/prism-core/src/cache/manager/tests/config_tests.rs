//! Integration tests for cache manager configuration.
//!
//! Unit tests for `CacheManagerConfig` and `CacheManagerError` are in `config.rs`.
//! These tests verify `CacheManager` creation with various configurations.

use super::*;

#[test]
fn test_cache_manager_new_default_config() {
    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(ChainState::new());

    let result = CacheManager::new(&config, chain_state);
    assert!(result.is_ok(), "Should create cache manager with default config");
}

#[tokio::test]
async fn test_cache_manager_new_with_chain_state() {
    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(ChainState::new());

    // Set some initial state using async methods
    let _ = chain_state.update_tip_simple(1000).await;
    let _ = chain_state.update_finalized(950).await;

    let cache = CacheManager::new(&config, chain_state).expect("valid config");

    assert_eq!(cache.get_current_tip(), 1000);
    assert_eq!(cache.get_finalized_block(), 950);
}
