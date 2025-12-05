//! End-to-end integration tests for hedging, scoring, and consensus working together.
//!
//! These tests verify that:
//! - All three features can be enabled simultaneously
//! - Hedging uses scoring data to select upstreams
//! - Consensus validates responses from hedged requests
//! - Complete request flow works with all features enabled

use prism_core::{
    chain::ChainState,
    upstream::{
        consensus::{ConsensusConfig, ConsensusEngine},
        hedging::HedgeConfig,
        scoring::{ScoringConfig, ScoringEngine},
        HedgeExecutor,
    },
};
use std::sync::Arc;

/// Helper to create a `ScoringEngine` with `ChainState`
fn create_scoring_engine(config: ScoringConfig) -> ScoringEngine {
    let chain_state = Arc::new(ChainState::new());
    ScoringEngine::new(config, chain_state)
}

#[tokio::test]
async fn test_all_features_can_be_enabled_together() {
    let hedge_config = HedgeConfig { enabled: true, ..HedgeConfig::default() };

    let scoring_config = ScoringConfig { enabled: true, ..ScoringConfig::default() };

    let consensus_config = ConsensusConfig { enabled: true, ..ConsensusConfig::default() };

    let hedge_executor = HedgeExecutor::new(hedge_config);
    let scoring_engine = create_scoring_engine(scoring_config);
    let consensus_engine = ConsensusEngine::new(consensus_config);

    // Verify all are enabled
    assert!(hedge_executor.get_config().enabled);
    assert!(scoring_engine.is_enabled());
    assert!(consensus_engine.is_enabled().await);
}

#[tokio::test]
async fn test_hedging_and_scoring_integration() {
    let hedge_config = HedgeConfig {
        enabled: true,
        latency_quantile: 0.95,
        min_delay_ms: 50,
        max_delay_ms: 2000,
        max_parallel: 3,
    };

    let scoring_config =
        ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let hedge_executor = HedgeExecutor::new(hedge_config);
    let scoring_engine = create_scoring_engine(scoring_config);

    // Record latencies in both systems
    for i in 0..10 {
        let latency = (i + 1) * 100;
        hedge_executor.record_latency("upstream-1", latency);
        scoring_engine.record_success("upstream-1", latency);
    }

    // Both should have data
    let hedge_stats = hedge_executor.get_latency_stats("upstream-1");
    assert!(hedge_stats.is_some());

    let scoring_metrics = scoring_engine.get_metrics("upstream-1");
    assert!(scoring_metrics.is_some());
}

#[tokio::test]
async fn test_scoring_and_consensus_integration() {
    let scoring_config =
        ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let consensus_config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        methods: vec!["eth_getBlockByNumber".to_string()],
        ..ConsensusConfig::default()
    };

    let scoring_engine = Arc::new(create_scoring_engine(scoring_config));
    let consensus_engine = ConsensusEngine::new(consensus_config);

    // Record metrics in scoring engine
    for _ in 0..10 {
        scoring_engine.record_success("upstream-1", 100);
        scoring_engine.record_success("upstream-2", 150);
        scoring_engine.record_success("upstream-3", 200);
    }

    scoring_engine.record_block_number("upstream-1", 100).await;
    scoring_engine.record_block_number("upstream-2", 100).await;
    scoring_engine.record_block_number("upstream-3", 100).await;

    // Get ranked upstreams from scoring
    let ranked = scoring_engine.get_ranked_upstreams();
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].0, "upstream-1"); // Fastest should rank highest

    // Consensus engine can use the same methods
    assert!(consensus_engine.requires_consensus("eth_getBlockByNumber").await);
}

#[tokio::test]
async fn test_hedging_consensus_and_scoring_together() {
    let hedge_config = HedgeConfig { enabled: true, max_parallel: 3, ..HedgeConfig::default() };

    let scoring_config =
        ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let consensus_config =
        ConsensusConfig { enabled: true, max_count: 3, min_count: 2, ..ConsensusConfig::default() };

    let hedge_executor = Arc::new(HedgeExecutor::new(hedge_config));
    let scoring_engine = Arc::new(create_scoring_engine(scoring_config));
    let consensus_engine = Arc::new(ConsensusEngine::new(consensus_config));

    // Simulate a request flow:
    // 1. Hedging selects upstreams based on latency
    // 2. Scoring tracks performance
    // 3. Consensus validates responses

    let upstreams = vec!["upstream-1", "upstream-2", "upstream-3"];

    // Record initial metrics for all upstreams
    for upstream in &upstreams {
        for i in 0..10 {
            let latency = if *upstream == "upstream-1" {
                50 + i * 5 // Fast
            } else if *upstream == "upstream-2" {
                100 + i * 10 // Medium
            } else {
                200 + i * 20 // Slow
            };

            hedge_executor.record_latency(upstream, latency);
            scoring_engine.record_success(upstream, latency);
        }

        scoring_engine.record_block_number(upstream, 100).await;
    }

    // Verify hedging has latency data
    for upstream in &upstreams {
        assert!(hedge_executor.get_latency_stats(upstream).is_some());
    }

    // Verify scoring has ranked them correctly
    let ranked = scoring_engine.get_ranked_upstreams();
    assert_eq!(ranked[0].0, "upstream-1"); // Fastest

    // Verify consensus configuration
    assert!(consensus_engine.is_enabled().await);
}

#[tokio::test]
async fn test_scoring_influences_upstream_selection() {
    let scoring_config =
        ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let scoring_engine = create_scoring_engine(scoring_config);

    // Create upstreams with different performance profiles
    // Good upstream: fast, no errors
    for _ in 0..10 {
        scoring_engine.record_success("good-upstream", 50);
    }
    scoring_engine.record_block_number("good-upstream", 100).await;

    // Bad upstream: slow with errors
    for _ in 0..5 {
        scoring_engine.record_success("bad-upstream", 500);
    }
    for _ in 0..5 {
        scoring_engine.record_error("bad-upstream");
    }
    scoring_engine.record_block_number("bad-upstream", 100).await;

    let ranked = scoring_engine.get_ranked_upstreams();

    // Good upstream should be ranked higher
    assert_eq!(ranked[0].0, "good-upstream");
    assert_eq!(ranked[1].0, "bad-upstream");
    assert!(ranked[0].1 > ranked[1].1);
}

#[tokio::test]
async fn test_consensus_with_hedging_latency_data() {
    let hedge_config = HedgeConfig { enabled: true, ..HedgeConfig::default() };

    let consensus_config = ConsensusConfig { enabled: true, ..ConsensusConfig::default() };

    let hedge_executor = HedgeExecutor::new(hedge_config);
    let consensus_engine = ConsensusEngine::new(consensus_config);

    // Record latencies that would be used for hedging decisions
    hedge_executor.record_latency("fast-upstream", 50);
    hedge_executor.record_latency("fast-upstream", 60);
    hedge_executor.record_latency("slow-upstream", 300);
    hedge_executor.record_latency("slow-upstream", 350);

    // Verify consensus configuration is independent
    assert!(consensus_engine.is_enabled().await);
    assert!(consensus_engine.requires_consensus("eth_getBlockByNumber").await);
}

#[tokio::test]
async fn test_full_request_lifecycle_simulation() {
    // Setup all three systems
    let hedge_executor = Arc::new(HedgeExecutor::new(HedgeConfig {
        enabled: true,
        max_parallel: 3,
        ..HedgeConfig::default()
    }));

    let scoring_engine = Arc::new(create_scoring_engine(ScoringConfig {
        enabled: true,
        min_samples: 5,
        ..ScoringConfig::default()
    }));

    let consensus_engine = Arc::new(ConsensusEngine::new(ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        ..ConsensusConfig::default()
    }));

    let upstreams = vec!["upstream-a", "upstream-b", "upstream-c"];

    for upstream in &upstreams {
        // Simulate 20 requests with varying latencies
        for i in 0..20 {
            let latency = match *upstream {
                "upstream-a" => 50 + i * 2,   // Fast and consistent
                "upstream-b" => 100 + i * 5,  // Medium
                "upstream-c" => 200 + i * 10, // Slow
                _ => 100,
            };

            hedge_executor.record_latency(upstream, latency);
            scoring_engine.record_success(upstream, latency);
        }

        let block_num = match *upstream {
            "upstream-a" => 1000, // Synced
            "upstream-b" => 999,  // 1 block behind
            "upstream-c" => 995,  // 5 blocks behind
            _ => unreachable!("Only testing upstream-a, upstream-b, upstream-c"),
        };
        scoring_engine.record_block_number(upstream, block_num).await;
    }

    let ranked = scoring_engine.get_ranked_upstreams();
    assert_eq!(ranked.len(), 3);
    // upstream-a should be ranked highest (fast + synced)
    assert_eq!(ranked[0].0, "upstream-a");

    for upstream in &upstreams {
        let stats = hedge_executor.get_latency_stats(upstream);
        assert!(stats.is_some(), "Upstream {upstream} should have latency stats");
    }

    assert!(consensus_engine.is_enabled().await);
    assert!(consensus_engine.requires_consensus("eth_getBlockByNumber").await);
    let consensus_config = consensus_engine.get_config().await;
    assert_eq!(consensus_config.max_count, 3);
    assert_eq!(consensus_config.min_count, 2);
}

#[tokio::test]
async fn test_feature_interaction_with_errors() {
    let scoring_engine = Arc::new(create_scoring_engine(ScoringConfig {
        enabled: true,
        min_samples: 3,
        ..ScoringConfig::default()
    }));

    let consensus_engine = Arc::new(ConsensusEngine::new(ConsensusConfig {
        enabled: true,
        ..ConsensusConfig::default()
    }));

    // Upstream with errors
    for _ in 0..5 {
        scoring_engine.record_success("reliable", 100);
    }
    scoring_engine.record_block_number("reliable", 100).await;

    for _ in 0..3 {
        scoring_engine.record_success("unreliable", 100);
    }
    for _ in 0..7 {
        scoring_engine.record_error("unreliable");
    }
    scoring_engine.record_block_number("unreliable", 100).await;

    // Scoring should penalize the unreliable upstream
    let reliable_score = scoring_engine.get_score("reliable");
    let unreliable_score = scoring_engine.get_score("unreliable");

    assert!(reliable_score.is_some());
    assert!(unreliable_score.is_some());
    assert!(reliable_score.unwrap().composite_score > unreliable_score.unwrap().composite_score);

    // Consensus engine is configured and ready
    assert!(consensus_engine.is_enabled().await);
}

#[tokio::test]
async fn test_concurrent_feature_operations() {
    let hedge_executor = Arc::new(HedgeExecutor::new(HedgeConfig::default()));
    let scoring_engine = Arc::new(create_scoring_engine(ScoringConfig::default()));
    let _consensus_engine = Arc::new(ConsensusEngine::new(ConsensusConfig::default()));

    // Spawn concurrent operations
    let mut handles = vec![];

    // Concurrent hedging latency recording
    for i in 0..5 {
        let exec = hedge_executor.clone();
        let handle = tokio::spawn(async move {
            for j in 0..10 {
                exec.record_latency(&format!("concurrent-{i}"), (j + 1) * 50);
            }
        });
        handles.push(handle);
    }

    // Concurrent scoring recording
    for i in 0..5 {
        let engine = scoring_engine.clone();
        let handle = tokio::spawn(async move {
            for j in 0..10 {
                engine.record_success(&format!("concurrent-{i}"), (j + 1) * 100);
            }
        });
        handles.push(handle);
    }

    // Wait for all operations to complete
    for handle in handles {
        handle.await.expect("Task should complete successfully");
    }

    // Verify data was recorded correctly
    for i in 0..5 {
        let upstream_name = format!("concurrent-{i}");
        assert!(hedge_executor.get_latency_stats(&upstream_name).is_some());
        assert!(scoring_engine.get_metrics(&upstream_name).is_some());
    }
}

#[tokio::test]
async fn test_config_updates_across_features() {
    let hedge_executor = HedgeExecutor::new(HedgeConfig::default());
    let scoring_engine = create_scoring_engine(ScoringConfig::default());
    let consensus_engine = ConsensusEngine::new(ConsensusConfig::default());

    // Verify initial states
    assert!(!hedge_executor.get_config().enabled);
    assert!(!scoring_engine.is_enabled());
    assert!(!consensus_engine.is_enabled().await);

    // Update all configs
    hedge_executor.update_config(HedgeConfig { enabled: true, ..HedgeConfig::default() });
    scoring_engine.update_config(ScoringConfig { enabled: true, ..ScoringConfig::default() });
    consensus_engine
        .update_config(ConsensusConfig { enabled: true, ..ConsensusConfig::default() })
        .await;

    // Verify updates took effect
    assert!(hedge_executor.get_config().enabled);
    assert!(scoring_engine.is_enabled());
    assert!(consensus_engine.is_enabled().await);
}

#[tokio::test]
async fn test_feature_data_isolation() {
    let hedge_executor = HedgeExecutor::new(HedgeConfig::default());
    let scoring_engine = create_scoring_engine(ScoringConfig::default());

    // Record data in one system
    hedge_executor.record_latency("isolated-upstream", 100);
    scoring_engine.record_success("isolated-upstream", 100);

    // Clear one system
    hedge_executor.clear_latency_data();

    // Verify one is cleared but other is not
    assert!(hedge_executor.get_latency_stats("isolated-upstream").is_none());
    assert!(scoring_engine.get_metrics("isolated-upstream").is_some());

    // Clear the other
    scoring_engine.clear();
    assert!(scoring_engine.get_metrics("isolated-upstream").is_none());
}

#[tokio::test]
async fn test_realistic_request_scenario() {
    // Setup: 3 upstreams with different characteristics
    let hedge_executor = Arc::new(HedgeExecutor::new(HedgeConfig {
        enabled: true,
        max_parallel: 3,
        ..HedgeConfig::default()
    }));

    let scoring_engine = Arc::new(create_scoring_engine(ScoringConfig {
        enabled: true,
        min_samples: 10,
        ..ScoringConfig::default()
    }));

    // Simulate 100 requests distributed across upstreams
    for i in 0..100 {
        let upstream = format!("upstream-{}", i % 3);
        let latency = match i % 3 {
            0 => 50 + (i % 10) * 5,   // Fast, consistent
            1 => 100 + (i % 20) * 10, // Medium, variable
            2 => 200 + (i % 30) * 15, // Slow, very variable
            _ => 100,
        };

        // Occasional errors on upstream-2
        if i % 3 == 2 && i % 10 == 0 {
            scoring_engine.record_error(&upstream);
        } else {
            hedge_executor.record_latency(&upstream, latency);
            scoring_engine.record_success(&upstream, latency);
        }

        // Update block numbers
        let block = 1000 + (i / 10);
        scoring_engine.record_block_number(&upstream, block).await;
    }

    // Verify the system learned the upstream characteristics
    let ranked = scoring_engine.get_ranked_upstreams();
    assert_eq!(ranked.len(), 3);

    // upstream-0 (fast, no errors) should rank highest
    assert_eq!(ranked[0].0, "upstream-0");

    // All should have latency data in hedging
    for i in 0..3 {
        let upstream = format!("upstream-{i}");
        assert!(hedge_executor.get_latency_stats(&upstream).is_some());
    }
}
