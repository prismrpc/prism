//! Integration tests for multi-factor scoring system.
//!
//! These tests verify that upstream scoring works correctly:
//! - Scoring affects upstream selection decisions
//! - Slow upstreams are penalized appropriately
//! - Error rates impact scores correctly
//! - Scores recover when upstream performance improves
//! - Block lag penalties work as expected

use prism_core::{
    chain::ChainState,
    upstream::scoring::{ScoringConfig, ScoringEngine, ScoringWeights},
};
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;

/// Helper to create a `ScoringEngine` with `ChainState`
fn create_engine(config: ScoringConfig) -> ScoringEngine {
    let chain_state = Arc::new(ChainState::new());
    ScoringEngine::new(config, chain_state)
}

#[tokio::test]
#[allow(clippy::float_cmp)]
async fn test_scoring_basic_success_tracking() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Record successful requests
    for _ in 0..10 {
        engine.record_success("upstream-1", 100);
    }

    let metrics = engine.get_metrics("upstream-1");
    assert!(metrics.is_some());

    let (error_rate, throttle_rate, total_requests, _block, avg_latency) = metrics.unwrap();
    assert_eq!(error_rate, 0.0);
    assert_eq!(throttle_rate, 0.0);
    assert_eq!(total_requests, 10);
    assert_eq!(avg_latency, Some(100));
}

#[tokio::test]
async fn test_scoring_penalizes_slow_upstreams() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Record fast upstream
    for _ in 0..10 {
        engine.record_success("fast-upstream", 50);
    }

    // Record slow upstream
    for _ in 0..10 {
        engine.record_success("slow-upstream", 500);
    }

    // Both need block numbers to get scored
    engine.record_block_number("fast-upstream", 100).await;
    engine.record_block_number("slow-upstream", 100).await;

    let fast_score = engine.get_score("fast-upstream");
    let slow_score = engine.get_score("slow-upstream");

    assert!(fast_score.is_some());
    assert!(slow_score.is_some());

    // Fast upstream should have higher score
    assert!(
        fast_score.unwrap().composite_score > slow_score.unwrap().composite_score,
        "Fast upstream should score higher than slow upstream"
    );
}

#[tokio::test]
async fn test_scoring_penalizes_errors() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Upstream with no errors
    for _ in 0..10 {
        engine.record_success("reliable-upstream", 100);
    }
    engine.record_block_number("reliable-upstream", 100).await;

    // Upstream with 30% error rate
    for _ in 0..7 {
        engine.record_success("unreliable-upstream", 100);
    }
    for _ in 0..3 {
        engine.record_error("unreliable-upstream");
    }
    engine.record_block_number("unreliable-upstream", 100).await;

    let reliable_score = engine.get_score("reliable-upstream").unwrap();
    let unreliable_score = engine.get_score("unreliable-upstream").unwrap();

    // Reliable upstream should have higher score
    assert!(reliable_score.composite_score > unreliable_score.composite_score);
    assert!((reliable_score.error_rate - 0.0).abs() < f64::EPSILON);
    assert!((unreliable_score.error_rate - 0.3).abs() < 0.01);
}

#[tokio::test]
#[allow(clippy::float_cmp)]
async fn test_scoring_penalizes_throttling() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Upstream without throttling
    for _ in 0..10 {
        engine.record_success("normal-upstream", 100);
    }
    engine.record_block_number("normal-upstream", 100).await;

    // Upstream with 20% throttle rate
    for _ in 0..8 {
        engine.record_success("throttled-upstream", 100);
    }
    for _ in 0..2 {
        engine.record_throttle("throttled-upstream");
    }
    engine.record_block_number("throttled-upstream", 100).await;

    let normal_score = engine.get_score("normal-upstream").unwrap();
    let throttled_score = engine.get_score("throttled-upstream").unwrap();

    // Normal upstream should have higher score
    assert!(normal_score.composite_score > throttled_score.composite_score);
    assert_eq!(normal_score.throttle_rate, 0.0);
    assert!((throttled_score.throttle_rate - 0.2).abs() < 0.01);
}

#[tokio::test]
async fn test_scoring_block_lag_penalty() {
    let config = ScoringConfig {
        enabled: true,
        min_samples: 5,
        max_block_lag: 5,
        ..ScoringConfig::default()
    };

    let engine = create_engine(config);

    // Set chain tip by recording highest block
    engine.record_block_number("tip-setter", 110).await;

    // Upstream at chain tip
    for _ in 0..10 {
        engine.record_success("synced-upstream", 100);
    }
    engine.record_block_number("synced-upstream", 110).await;

    // Upstream 4 blocks behind
    for _ in 0..10 {
        engine.record_success("lagging-upstream", 100);
    }
    engine.record_block_number("lagging-upstream", 106).await;

    let synced_score = engine.get_score("synced-upstream").unwrap();
    let lagging_score = engine.get_score("lagging-upstream").unwrap();

    // Synced upstream should have higher score
    assert!(synced_score.composite_score > lagging_score.composite_score);
    assert_eq!(synced_score.block_head_lag, 0);
    assert_eq!(lagging_score.block_head_lag, 4);
}

#[tokio::test]
async fn test_scoring_recovery_after_improvement() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Initially poor performance
    for _ in 0..10 {
        engine.record_success("recovering-upstream", 500);
    }
    for _ in 0..5 {
        engine.record_error("recovering-upstream");
    }
    engine.record_block_number("recovering-upstream", 100).await;

    let initial_score = engine.get_score("recovering-upstream").unwrap();

    // Clear metrics to simulate new window
    engine.clear();

    // Improved performance
    for _ in 0..15 {
        engine.record_success("recovering-upstream", 100);
    }
    engine.record_block_number("recovering-upstream", 110).await;

    let improved_score = engine.get_score("recovering-upstream").unwrap();

    // Score should improve after better performance
    assert!(improved_score.composite_score > initial_score.composite_score);
}

#[tokio::test]
async fn test_scoring_ranked_upstreams() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Create three upstreams with different characteristics
    // Best: fast, no errors
    for _ in 0..10 {
        engine.record_success("best-upstream", 50);
    }
    engine.record_block_number("best-upstream", 100).await;

    // Medium: moderate speed, no errors
    for _ in 0..10 {
        engine.record_success("medium-upstream", 200);
    }
    engine.record_block_number("medium-upstream", 100).await;

    // Worst: slow with errors
    for _ in 0..6 {
        engine.record_success("worst-upstream", 500);
    }
    for _ in 0..4 {
        engine.record_error("worst-upstream");
    }
    engine.record_block_number("worst-upstream", 100).await;

    let ranked = engine.get_ranked_upstreams();

    assert_eq!(ranked.len(), 3);
    // Best should be ranked first
    assert_eq!(ranked[0].0, "best-upstream");
    // Worst should be ranked last
    assert_eq!(ranked[2].0, "worst-upstream");
    // Scores should be in descending order
    assert!(ranked[0].1 > ranked[1].1);
    assert!(ranked[1].1 > ranked[2].1);
}

#[tokio::test]
async fn test_scoring_chain_tip_tracking() {
    let engine = create_engine(ScoringConfig::default());

    assert_eq!(engine.chain_tip(), 0);

    engine.record_block_number("upstream-1", 100).await;
    assert_eq!(engine.chain_tip(), 100);

    engine.record_block_number("upstream-2", 105).await;
    assert_eq!(engine.chain_tip(), 105);

    // Lower block shouldn't change tip
    engine.record_block_number("upstream-3", 103).await;
    assert_eq!(engine.chain_tip(), 105);
}

#[tokio::test]
async fn test_scoring_insufficient_samples() {
    let config = ScoringConfig { enabled: true, min_samples: 10, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Only 5 samples, but min is 10
    for _ in 0..5 {
        engine.record_success("new-upstream", 100);
    }
    engine.record_block_number("new-upstream", 100).await;

    let score = engine.get_score("new-upstream");
    assert!(score.is_none(), "Should not return score with insufficient samples");
}

#[tokio::test]
async fn test_scoring_config_update_affects_behavior() {
    // Behavioral test: Verify that changing min_samples config actually affects scoring decisions
    let initial_config =
        ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };
    let engine = create_engine(initial_config);

    // Record 10 samples for upstream
    for _ in 0..10 {
        engine.record_success("test-upstream", 100);
    }
    engine.record_block_number("test-upstream", 100).await;

    // With min_samples=5 and 10 samples recorded, should get a score
    let score_before = engine.get_score("test-upstream");
    assert!(score_before.is_some(), "Should have score with 10 samples when min_samples=5");

    // Update config to require 20 samples
    let new_config = ScoringConfig { enabled: true, min_samples: 20, ..ScoringConfig::default() };
    engine.update_config(new_config);

    // BEHAVIORAL CHECK: Same upstream with 10 samples should now return None
    // because min_samples threshold increased to 20
    let score_after = engine.get_score("test-upstream");
    assert!(
        score_after.is_none(),
        "Should NOT have score with 10 samples when min_samples=20 - config change must affect behavior"
    );

    // Add more samples to meet new threshold
    for _ in 0..15 {
        engine.record_success("test-upstream", 100);
    }

    // Now with 25 total samples, should get score again
    let score_final = engine.get_score("test-upstream");
    assert!(score_final.is_some(), "Should have score with 25 samples when min_samples=20");
}

#[tokio::test]
async fn test_scoring_weights_affect_behavior() {
    // Behavioral test: Verify that changing weights actually affects score calculations
    // Create two upstreams: one with errors, one with high latency
    let initial_config = ScoringConfig {
        enabled: true,
        min_samples: 5,
        weights: ScoringWeights {
            latency: 1.0,     // Low weight on latency
            error_rate: 10.0, // High weight on errors
            ..ScoringWeights::default()
        },
        ..ScoringConfig::default()
    };
    let engine = create_engine(initial_config);

    // Upstream with HIGH latency (500ms) but NO errors
    for _ in 0..10 {
        engine.record_success("slow-upstream", 500);
    }
    engine.record_block_number("slow-upstream", 100).await;

    // Upstream with LOW latency (50ms) but 30% errors
    for _ in 0..7 {
        engine.record_success("unreliable-upstream", 50);
    }
    for _ in 0..3 {
        engine.record_error("unreliable-upstream");
    }
    engine.record_block_number("unreliable-upstream", 100).await;

    // With high error_rate weight (10.0) and low latency weight (1.0),
    // the unreliable upstream should score LOWER despite better latency
    let slow_score = engine.get_score("slow-upstream").unwrap();
    let unreliable_score = engine.get_score("unreliable-upstream").unwrap();

    assert!(
        slow_score.composite_score > unreliable_score.composite_score,
        "With high error_rate weight, errors should dominate: slow={} vs unreliable={}",
        slow_score.composite_score,
        unreliable_score.composite_score
    );

    // BEHAVIORAL CHECK: Flip the weights - make latency dominate
    let flipped_config = ScoringConfig {
        enabled: true,
        min_samples: 5,
        weights: ScoringWeights {
            latency: 10.0,   // High weight on latency
            error_rate: 1.0, // Low weight on errors
            ..ScoringWeights::default()
        },
        ..ScoringConfig::default()
    };
    engine.update_config(flipped_config);

    // Recalculate scores with new weights
    let slow_score_new = engine.get_score("slow-upstream").unwrap();
    let unreliable_score_new = engine.get_score("unreliable-upstream").unwrap();

    // Now with high latency weight (10.0) and low error weight (1.0),
    // the low-latency upstream should score HIGHER despite having errors
    assert!(
        unreliable_score_new.composite_score > slow_score_new.composite_score,
        "With high latency weight, latency should dominate: unreliable={} vs slow={} - config change must affect behavior",
        unreliable_score_new.composite_score,
        slow_score_new.composite_score
    );
}

#[tokio::test]
#[allow(clippy::float_cmp)]
async fn test_scoring_default_weights() {
    let weights = ScoringWeights::default();

    assert_eq!(weights.latency, 8.0);
    assert_eq!(weights.error_rate, 4.0);
    assert_eq!(weights.throttle_rate, 3.0);
    assert_eq!(weights.block_head_lag, 2.0);
    assert_eq!(weights.total_requests, 1.0);
}

#[tokio::test]
async fn test_scoring_multiple_upstreams_concurrently() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Record metrics for multiple upstreams concurrently
    let upstreams = vec!["upstream-1", "upstream-2", "upstream-3", "upstream-4"];

    for upstream in &upstreams {
        for i in 0..10 {
            engine.record_success(upstream, (i + 1) * 100);
        }
        engine.record_block_number(upstream, 100).await;
    }

    // All upstreams should have scores
    for upstream in &upstreams {
        let score = engine.get_score(upstream);
        assert!(score.is_some(), "Upstream {upstream} should have a score");
    }
}

#[tokio::test]
async fn test_scoring_metrics_retrieval() {
    let config = ScoringConfig { enabled: true, min_samples: 1, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Record mixed metrics
    engine.record_success("test-upstream", 100);
    engine.record_success("test-upstream", 150);
    engine.record_error("test-upstream");
    engine.record_throttle("test-upstream");
    engine.record_block_number("test-upstream", 50).await;

    let metrics = engine.get_metrics("test-upstream");
    assert!(metrics.is_some());

    let (error_rate, throttle_rate, total_requests, block, avg_latency) = metrics.unwrap();

    assert_eq!(total_requests, 4); // 2 success + 1 error + 1 throttle
    assert!((error_rate - 0.25).abs() < 0.01); // 1/4 = 25%
    assert!((throttle_rate - 0.25).abs() < 0.01); // 1/4 = 25%
    assert_eq!(block, 50);
    assert_eq!(avg_latency, Some(125)); // (100 + 150) / 2
}

#[tokio::test]
async fn test_scoring_clear_metrics() {
    let engine = create_engine(ScoringConfig::default());

    engine.record_success("upstream", 100);
    engine.record_block_number("upstream", 100).await;

    assert!(engine.get_metrics("upstream").is_some());

    engine.clear();

    // Metrics should be cleared
    assert!(engine.get_metrics("upstream").is_none());

    // Note: chain_tip is NOT cleared because ChainState is shared between components.
    // This is intentional - clearing metrics shouldn't reset the chain state.
    // The chain tip is managed by ReorgManager and should persist across engine clears.
    assert_eq!(engine.chain_tip(), 100);
}

#[tokio::test]
async fn test_scoring_extreme_error_rates() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // 100% error rate - but we need some successful requests for latency samples
    for _ in 0..5 {
        engine.record_success("failing-upstream", 100);
    }
    for _ in 0..10 {
        engine.record_error("failing-upstream");
    }
    engine.record_block_number("failing-upstream", 100).await;

    let score = engine.get_score("failing-upstream").unwrap();

    // High error rate
    assert!(score.error_rate > 0.5);
    // Score should be lower due to error rate
    assert!(score.composite_score < 100.0);
}

#[tokio::test]
async fn test_scoring_window_behavior() {
    // Test the behavioral contract: error rates reset when window expires.
    // Note: Latency tracking uses a circular buffer that does NOT reset with the window,
    // so we test error rate behavior which DOES reset.
    //
    // error_rate_factor = 1.0 - error_rate (so 1.0 is best, lower is worse)
    let window_seconds = 1u64;
    let config = ScoringConfig {
        enabled: true,
        min_samples: 3, // Lower min_samples for faster test
        window_seconds,
        ..ScoringConfig::default()
    };

    let engine = create_engine(config);

    // Record metrics with high error rate (will produce worse score)
    for _ in 0..5 {
        engine.record_success("windowed-upstream", 100);
    }
    for _ in 0..5 {
        engine.record_error("windowed-upstream");
    }
    engine.record_block_number("windowed-upstream", 100).await;
    engine.record_block_number("tip-setter", 100).await;

    let initial_score = engine.get_score("windowed-upstream");
    assert!(initial_score.is_some(), "Should have score after min_samples");
    let initial = initial_score.unwrap();

    // Initial score should reflect 50% error rate
    // error_rate_factor = 1.0 - 0.5 = 0.5 (lower values indicate more errors)
    assert!(
        initial.error_rate_factor < 0.6,
        "With 50% errors, error_rate_factor should be around 0.5: {:?}",
        initial.error_rate_factor
    );

    // Wait for window to expire (with margin)
    sleep(Duration::from_secs(window_seconds + 1)).await;

    // Record new metrics with NO errors
    for _ in 0..5 {
        engine.record_success("windowed-upstream", 100);
    }
    engine.record_block_number("windowed-upstream", 100).await;

    let new_score = engine.get_score("windowed-upstream");
    assert!(new_score.is_some(), "Should still have score after window reset");
    let updated = new_score.unwrap();

    // Behavioral assertion: after window reset, error counts should be cleared.
    // The new score should have 0% error rate since we only recorded successes.
    // error_rate_factor = 1.0 - 0.0 = 1.0 (best possible)
    // Higher error_rate_factor is BETTER (fewer errors)
    assert!(
        updated.error_rate_factor > initial.error_rate_factor,
        "After window reset with no errors, error_rate_factor should improve (higher is better): {:?} > {:?}",
        updated.error_rate_factor,
        initial.error_rate_factor
    );
}

#[tokio::test]
#[allow(clippy::float_cmp)]
async fn test_scoring_score_components() {
    let config = ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Record perfect upstream
    for _ in 0..10 {
        engine.record_success("perfect-upstream", 50);
    }
    engine.record_block_number("perfect-upstream", 100).await;
    engine.record_block_number("tip-setter", 100).await;

    let score = engine.get_score("perfect-upstream").unwrap();

    // All factors should be high for a perfect upstream
    assert!(score.latency_factor > 0.3); // Reasonable latency factor
    assert_eq!(score.error_rate_factor, 1.0); // No errors
    assert_eq!(score.throttle_factor, 1.0); // No throttling
    assert_eq!(score.block_lag_factor, 1.0); // No lag
    assert!(score.composite_score > 0.0); // Positive score
}

#[tokio::test]
#[allow(clippy::float_cmp)]
async fn test_scoring_massive_block_lag() {
    let config = ScoringConfig {
        enabled: true,
        min_samples: 5,
        max_block_lag: 5,
        ..ScoringConfig::default()
    };

    let engine = create_engine(config);

    // Set chain tip
    engine.record_block_number("tip", 1000).await;

    // Upstream massively behind (50 blocks)
    for _ in 0..10 {
        engine.record_success("lagging", 100);
    }
    engine.record_block_number("lagging", 950).await;

    let score = engine.get_score("lagging").unwrap();

    // Block lag factor should be at minimum (0.0) due to exceeding max_block_lag
    assert_eq!(score.block_lag_factor, 0.0);
    assert_eq!(score.block_head_lag, 50);
}

#[tokio::test]
async fn test_scoring_zero_latency_edge_case() {
    let config = ScoringConfig { enabled: true, min_samples: 1, ..ScoringConfig::default() };

    let engine = create_engine(config);

    // Record zero latency (edge case)
    engine.record_success("instant-upstream", 0);
    engine.record_block_number("instant-upstream", 100).await;

    let score = engine.get_score("instant-upstream");
    // Should handle gracefully, not panic
    assert!(score.is_none() || score.unwrap().composite_score > 0.0);
}
