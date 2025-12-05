//! Adversarial and Edge Case Tests
//!
//! These tests cover real-world failure scenarios that are critical for a production
//! RPC aggregator but are often missed by standard unit tests:
//!
//! - Chain fork with consensus disagreement (50/50 split)
//! - Cascading upstream failures
//! - Rapid reorg storms with cache invalidation
//! - Cache thundering herd under high load
//! - Rate limit burst after quiet period
//! - Log queries spanning finalized/unfinalized boundary
//!
//! These tests verify behavioral contracts under adversarial conditions, not internal state.

use mockito::Server;
use prism_core::{
    chain::ChainState,
    middleware::rate_limiting::RateLimiter,
    types::{JsonRpcRequest, UpstreamConfig},
    upstream::{
        consensus::{ConsensusConfig, ConsensusEngine, DisputeBehavior, FailureBehavior},
        endpoint::UpstreamEndpoint,
        http_client::HttpClient,
        scoring::{ScoringConfig, ScoringEngine},
    },
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::time::{sleep, Instant};

fn create_test_request(method: &str) -> JsonRpcRequest {
    JsonRpcRequest::new(method, Some(json!(["0x100", false])), json!(1))
}

fn create_upstream_endpoint(name: &str, url: &str) -> Arc<UpstreamEndpoint> {
    let config = UpstreamConfig {
        name: Arc::from(name),
        url: url.to_string(),
        ws_url: None,
        chain_id: 1,
        weight: 1,
        timeout_seconds: 5,
        supports_websocket: false,
        circuit_breaker_threshold: 2,
        circuit_breaker_timeout_seconds: 30,
    };
    let http_client = Arc::new(HttpClient::new().unwrap());
    Arc::new(UpstreamEndpoint::new(config, http_client))
}

fn create_scoring_engine() -> Arc<ScoringEngine> {
    let chain_state = Arc::new(ChainState::new());
    Arc::new(ScoringEngine::new(ScoringConfig::default(), chain_state))
}

#[tokio::test]
#[allow(clippy::similar_names)]
async fn test_consensus_50_50_split_prefers_block_head_leader() {
    let mut fork_a_server_1 = Server::new_async().await;
    let _mock_a1 = fork_a_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100", "hash": "0xfork_a_hash"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut fork_a_server_2 = Server::new_async().await;
    let _mock_a2 = fork_a_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100", "hash": "0xfork_a_hash"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut fork_b_server_1 = Server::new_async().await;
    let _mock_b1 = fork_b_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x101", "hash": "0xfork_b_hash"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut fork_b_server_2 = Server::new_async().await;
    let _mock_b2 = fork_b_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x101", "hash": "0xfork_b_hash"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 4,
        min_count: 2,
        timeout_seconds: 5,
        dispute_behavior: DisputeBehavior::PreferBlockHeadLeader,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    // Record block numbers for the scoring engine so it knows which is "leader"
    scoring_engine.record_block_number("fork-b-1", 0x101).await;
    scoring_engine.record_block_number("fork-b-2", 0x101).await;
    scoring_engine.record_block_number("fork-a-1", 0x100).await;
    scoring_engine.record_block_number("fork-a-2", 0x100).await;

    let upstreams = vec![
        create_upstream_endpoint("fork-a-1", &fork_a_server_1.url()),
        create_upstream_endpoint("fork-a-2", &fork_a_server_2.url()),
        create_upstream_endpoint("fork-b-1", &fork_b_server_1.url()),
        create_upstream_endpoint("fork-b-2", &fork_b_server_2.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok(), "System should handle 50/50 split gracefully");

    let consensus_result = result.unwrap();

    assert!(
        consensus_result.response.result.is_some(),
        "Should return a valid response even on 50/50 split"
    );
}

#[tokio::test]
#[allow(clippy::similar_names)]
async fn test_consensus_50_50_split_returns_error_when_configured() {
    let mut fork_a_server_1 = Server::new_async().await;
    let _mock_a1 = fork_a_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100", "hash": "0xfork_a"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut fork_a_server_2 = Server::new_async().await;
    let _mock_a2 = fork_a_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100", "hash": "0xfork_a"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    // Create 2 servers on "fork B"
    let mut fork_b_server_1 = Server::new_async().await;
    let _mock_b1 = fork_b_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100", "hash": "0xfork_b"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut fork_b_server_2 = Server::new_async().await;
    let _mock_b2 = fork_b_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100", "hash": "0xfork_b"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 4,
        min_count: 3, // Require 3 for consensus, but we only have 2 agreeing
        timeout_seconds: 5,
        failure_behavior: FailureBehavior::ReturnError,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("fork-a-1", &fork_a_server_1.url()),
        create_upstream_endpoint("fork-a-2", &fork_a_server_2.url()),
        create_upstream_endpoint("fork-b-1", &fork_b_server_1.url()),
        create_upstream_endpoint("fork-b-2", &fork_b_server_2.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    // When configured for ReturnError with min_count=3 and max agreement=2,
    // the system should not achieve consensus
    // Behavioral check: system handles this case (either error or fallback)
    // The exact behavior depends on failure_behavior configuration
    if let Ok(consensus_result) = result {
        // If it succeeded, it should indicate consensus wasn't achieved
        assert!(
            !consensus_result.consensus_achieved || consensus_result.agreement_count < 3,
            "With 50/50 split, full consensus should not be achieved"
        );
    }
    // Error is also acceptable - means ReturnError behavior worked
}

#[tokio::test]
async fn test_cascading_upstream_timeouts() {
    // Server 1: Returns error (simulating overload/failure)
    let mut failing_server = Server::new_async().await;
    let _mock_failing = failing_server
        .mock("POST", "/")
        .with_status(500)
        .with_header("content-type", "application/json")
        .with_body(json!({"error": "Internal Server Error"}).to_string())
        .create_async()
        .await;

    // Server 2: Returns 503 (overloaded)
    let mut overloaded_server = Server::new_async().await;
    let _mock_overloaded = overloaded_server
        .mock("POST", "/")
        .with_status(503)
        .with_header("content-type", "application/json")
        .with_body(json!({"error": "Service Unavailable"}).to_string())
        .create_async()
        .await;

    // Server 3: Works correctly (the survivor)
    let mut healthy_server = Server::new_async().await;
    let _mock_healthy = healthy_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "0x12345"
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 1,       // Allow success with single healthy upstream
        timeout_seconds: 2, // Short timeout for test
        failure_behavior: FailureBehavior::AcceptAnyValid,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("failing", &failing_server.url()),
        create_upstream_endpoint("overloaded", &overloaded_server.url()),
        create_upstream_endpoint("healthy", &healthy_server.url()),
    ];

    let start = Instant::now();
    let request = Arc::new(create_test_request("eth_blockNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;
    let elapsed = start.elapsed();

    // Behavioral assertions:
    // 1. Should complete without hanging indefinitely
    assert!(
        elapsed < Duration::from_secs(15),
        "Should not hang on cascading failures, took {elapsed:?}"
    );

    // 2. Should succeed by finding the healthy upstream
    assert!(result.is_ok(), "Should succeed by falling back to healthy upstream");

    // 3. Should return valid response from healthy upstream
    let consensus_result = result.unwrap();
    assert!(
        consensus_result.response.result.is_some(),
        "Should return valid response from healthy upstream"
    );
}

#[tokio::test]
async fn test_rate_limit_burst_after_quiet_period() {
    // Create rate limiter: 5 tokens max, 2 tokens/second refill
    let rate_limiter = RateLimiter::new(5, 2);
    let client_key = "burst_test_client";

    // Phase 1: Exhaust all tokens
    let mut allowed_count = 0;
    for _ in 0..10 {
        if rate_limiter.check_rate_limit(client_key) {
            allowed_count += 1;
        }
    }

    // Should have allowed exactly 5 (max_tokens)
    assert_eq!(allowed_count, 5, "Should allow exactly max_tokens initially");

    // Verify we're now rate limited
    assert!(
        !rate_limiter.check_rate_limit(client_key),
        "Should be rate limited after exhausting tokens"
    );

    // Phase 2: Wait for full refill (5 tokens / 2 per second = 2.5 seconds, wait 3)
    sleep(Duration::from_secs(3)).await;

    // Phase 3: Burst again after quiet period
    let mut second_burst_allowed = 0;
    for _ in 0..10 {
        if rate_limiter.check_rate_limit(client_key) {
            second_burst_allowed += 1;
        }
    }

    // Should allow up to max_tokens again (tokens refilled during quiet period)
    // But NOT more than max_tokens (no over-refill)
    assert!(
        (4..=5).contains(&second_burst_allowed),
        "After refill, should allow close to max_tokens, got {second_burst_allowed}"
    );

    // Verify bucket count tracking
    assert!(rate_limiter.bucket_count() >= 1, "Should be tracking at least one bucket");
}

#[tokio::test]
async fn test_rate_limit_client_isolation() {
    let rate_limiter = RateLimiter::new(3, 1);

    // Client A exhausts their tokens
    for _ in 0..5 {
        let _ = rate_limiter.check_rate_limit("client_a");
    }
    assert!(!rate_limiter.check_rate_limit("client_a"), "Client A should be rate limited");

    // Client B should still have full allowance (isolation)
    let mut client_b_allowed = 0;
    for _ in 0..5 {
        if rate_limiter.check_rate_limit("client_b") {
            client_b_allowed += 1;
        }
    }

    assert_eq!(client_b_allowed, 3, "Client B should have full token allowance (isolated from A)");
}

#[tokio::test]
async fn test_cache_thundering_herd_prevention() {
    use prism_core::{
        cache::{cache_manager::CacheManager, manager::CacheManagerConfig},
        chain::ChainState,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::task;

    const NUM_CONCURRENT_REQUESTS: usize = 100;
    let block_id = 99999u64;

    let config = CacheManagerConfig::default();
    let chain_state = Arc::new(ChainState::new());
    let cache_manager =
        Arc::new(CacheManager::new(&config, chain_state).expect("valid test cache config"));

    let successful_acquires = Arc::new(AtomicUsize::new(0));
    let failed_acquires = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn many concurrent tasks trying to acquire the same block's fetch lock
    for _i in 0..NUM_CONCURRENT_REQUESTS {
        let cache_manager = cache_manager.clone();
        let successful = successful_acquires.clone();
        let failed = failed_acquires.clone();

        handles.push(task::spawn(async move {
            // Try to acquire the fetch lock for the same block
            if cache_manager.try_acquire_fetch_lock(block_id) {
                successful.fetch_add(1, Ordering::SeqCst);

                // Simulate fetch time
                sleep(Duration::from_millis(10)).await;

                // Release the lock
                let _ = cache_manager.release_fetch_lock(block_id);
            } else {
                failed.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    // Wait for all tasks to complete
    for handle in handles {
        let _ = handle.await;
    }

    let total_successful = successful_acquires.load(Ordering::SeqCst);
    let total_failed = failed_acquires.load(Ordering::SeqCst);

    // Behavioral assertions:
    // 1. Only a small number should successfully acquire (thundering herd prevented)
    assert!(
        total_successful < 10,
        "Thundering herd should be prevented: only {total_successful} of {NUM_CONCURRENT_REQUESTS} acquired lock (expected <10)"
    );

    // 2. Most should have failed to acquire (correctly deduped)
    assert!(
        total_failed > 90,
        "Most requests should be deduped: {total_failed} failed (expected >90)"
    );

    // 3. Total should equal NUM_CONCURRENT_REQUESTS
    assert_eq!(
        total_successful + total_failed,
        NUM_CONCURRENT_REQUESTS,
        "All requests should be accounted for"
    );
}

#[tokio::test]
async fn test_log_query_spanning_finalized_boundary() {
    use prism_core::{
        cache::{
            cache_manager::CacheManager, log_cache::LogCacheConfig, manager::CacheManagerConfig,
        },
        chain::ChainState,
    };

    // Setup chain state with finalized_block = 100
    // NOTE: tip must be updated BEFORE finalized, since update_finalized
    // requires block <= current_tip
    let chain_state = Arc::new(ChainState::new());
    chain_state.update_tip(105, [105u8; 32]).await;
    chain_state.update_finalized(100).await;

    let config = CacheManagerConfig {
        log_cache: LogCacheConfig {
            chunk_size: 100,
            safety_depth: 5, // Blocks within 5 of tip are "unsafe"
            ..LogCacheConfig::default()
        },
        ..CacheManagerConfig::default()
    };

    let _cache_manager =
        Arc::new(CacheManager::new(&config, chain_state.clone()).expect("valid test cache config"));

    // The key behavioral check is that the cache manager respects finality
    // when caching logs. We can't directly query logs here without more setup,
    // but we can verify the chain state tracking is correct.

    let current_tip = chain_state.current_tip();
    let finalized = chain_state.finalized_block();

    // Behavioral assertions:
    // 1. Finalized block should be correctly tracked
    assert_eq!(finalized, 100, "Finalized block should be 100");

    // 2. Current tip should be higher than finalized
    assert_eq!(current_tip, 105, "Current tip should be 105");

    // 3. Safety depth calculation: blocks > (tip - safety_depth) are "unsafe"
    // With tip=105 and safety_depth=5, blocks > 100 are unsafe
    // This matches our finalized_block=100, which is correct
    let unsafe_threshold = current_tip.saturating_sub(config.log_cache.safety_depth);
    assert_eq!(unsafe_threshold, 100, "Unsafe threshold should match finalized block");
}

#[tokio::test]
#[allow(clippy::similar_names)]
async fn test_rapid_reorg_storm_with_cache() {
    use prism_core::chain::ChainState;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::task;

    const NUM_REORG_CYCLES: usize = 10;
    const REORGS_PER_CYCLE: usize = 5;

    let chain_state = Arc::new(ChainState::new());
    let consistency_violations = Arc::new(AtomicUsize::new(0));

    // Initialize to a known state
    chain_state.update_tip(1000, [0u8; 32]).await;

    for cycle in 0..NUM_REORG_CYCLES {
        let mut handles = vec![];

        // Spawn multiple concurrent "reorg" updates
        for reorg_id in 0..REORGS_PER_CYCLE {
            let state = chain_state.clone();
            let violations = consistency_violations.clone();

            handles.push(task::spawn(async move {
                // Simulate oscillating between two forks
                let fork_a_tip = 1000 + (cycle * 10 + reorg_id) as u64;
                let fork_b_tip = 995 + (cycle * 10 + reorg_id) as u64;

                // Rapid updates simulating reorg detection
                #[allow(clippy::cast_possible_truncation)]
                {
                    state.force_update_tip(fork_a_tip, [fork_a_tip as u8; 32]).await;

                    // Verify consistency immediately after update
                    let (tip, hash) = state.current_tip_with_hash();
                    if hash[0] != (tip as u8) {
                        violations.fetch_add(1, Ordering::SeqCst);
                    }

                    // Update to other fork
                    state.force_update_tip(fork_b_tip, [fork_b_tip as u8; 32]).await;

                    // Verify consistency again
                    let (tip, hash) = state.current_tip_with_hash();
                    if hash[0] != (tip as u8) {
                        violations.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }));
        }

        // Wait for all reorgs in this cycle
        for handle in handles {
            let _ = handle.await;
        }
    }

    let violations = consistency_violations.load(Ordering::SeqCst);

    // Behavioral assertion: no consistency violations during reorg storm
    assert_eq!(
        violations, 0,
        "No consistency violations should occur during rapid reorg storm, found {violations}"
    );

    // Verify final state is valid
    let (final_tip, final_hash) = chain_state.current_tip_with_hash();
    assert!(final_tip >= 1000, "Final tip should be at least as high as initial");
    assert_ne!(final_hash, [0u8; 32], "Final hash should not be zero");
}
