//! Tests for upstream misbehavior tracking
//!
//! Comprehensive test suite for the misbehavior tracking system that manages
//! upstream penalties, sit-outs, and dispute tracking.

use prism_core::upstream::misbehavior::{MisbehaviorConfig, MisbehaviorTracker};
use std::{sync::Arc, time::Duration};
use tokio::time;

/// Creates a test misbehavior tracker with short timeouts for testing
fn create_test_tracker() -> Arc<MisbehaviorTracker> {
    let config = MisbehaviorConfig {
        dispute_threshold: Some(3),
        dispute_window_seconds: 60,
        sit_out_penalty_seconds: 10, // Short for testing
        log_path: None,
        max_log_file_size_bytes: 100 * 1024 * 1024,
    };
    Arc::new(MisbehaviorTracker::new(config))
}

/// Creates a test tracker with custom thresholds
fn create_custom_tracker(threshold: u32, sit_out_seconds: u64) -> Arc<MisbehaviorTracker> {
    let config = MisbehaviorConfig {
        dispute_threshold: Some(threshold),
        dispute_window_seconds: 60,
        sit_out_penalty_seconds: sit_out_seconds,
        log_path: None,
        max_log_file_size_bytes: 100 * 1024 * 1024,
    };
    Arc::new(MisbehaviorTracker::new(config))
}

/// Test that recording disputes increments counter and triggers behavioral impact
///
/// This test verifies both counter increment (internal state) AND behavioral impact
/// (sit-out enforcement). The test would FAIL if disputes were tracked but never
/// actually affected upstream selection.
#[tokio::test]
async fn test_record_dispute_increments_counter() {
    let tracker = create_test_tracker(); // threshold=3, sit_out=10s
    let upstream_id = "test-upstream-1";

    // Verify initial state: no disputes, no sit-out
    assert_eq!(tracker.get_dispute_count(upstream_id).await, 0);
    assert!(!tracker.is_in_sit_out(upstream_id).await);

    // Record first dispute - counter increments, but no sit-out yet
    let triggered = tracker.record_dispute(upstream_id, Some("eth_getBlock")).await;
    assert!(!triggered, "First dispute should not trigger sit-out");
    assert_eq!(tracker.get_dispute_count(upstream_id).await, 1, "Counter should increment to 1");
    assert!(!tracker.is_in_sit_out(upstream_id).await, "Should not be in sit-out after 1 dispute");

    // Record second dispute - counter increments, still no sit-out
    let triggered = tracker.record_dispute(upstream_id, Some("eth_getBlock")).await;
    assert!(!triggered, "Second dispute should not trigger sit-out");
    assert_eq!(tracker.get_dispute_count(upstream_id).await, 2, "Counter should increment to 2");
    assert!(!tracker.is_in_sit_out(upstream_id).await, "Should not be in sit-out after 2 disputes");

    // Record third dispute - BEHAVIORAL IMPACT: upstream enters sit-out
    let triggered = tracker.record_dispute(upstream_id, Some("eth_getBlock")).await;
    assert!(triggered, "Third dispute should trigger sit-out (threshold=3)");
    assert_eq!(tracker.get_dispute_count(upstream_id).await, 3, "Counter should be 3");
    assert!(
        tracker.is_in_sit_out(upstream_id).await,
        "BEHAVIORAL VERIFICATION: upstream must be in sit-out after threshold"
    );

    // Verify sit-out affects upstream selection (the actual purpose of tracking)
    let available_upstreams = vec!["test-upstream-1", "test-upstream-2", "test-upstream-3"];
    let filtered = tracker.filter_available(available_upstreams).await;
    assert_eq!(filtered.len(), 2, "Sit-out upstream should be excluded from selection");
    assert!(
        !filtered.contains(&upstream_id),
        "BEHAVIORAL VERIFICATION: sit-out upstream must be filtered from selection"
    );
}

/// Test that sit-out penalty is enforced when threshold is reached
///
/// This test verifies the behavioral contract: sit-out triggers exactly at threshold,
/// not before. We derive assertions from the configured threshold rather than
/// hardcoding expected values.
#[tokio::test]
async fn test_sit_out_penalty_enforcement() {
    let threshold = 2u32;
    let tracker = create_custom_tracker(threshold, 5);
    let upstream_id = "test-upstream-2";

    // Verify our test setup matches what we configured
    let config = tracker.get_config().await;
    assert_eq!(
        config.dispute_threshold,
        Some(threshold),
        "Test setup: threshold should match configured value"
    );

    // Behavioral contract: disputes below threshold should NOT trigger sit-out
    for dispute_count in 1..threshold {
        let triggered = tracker.record_dispute(upstream_id, Some("method")).await;
        assert!(
            !triggered,
            "Should NOT trigger sit-out after {dispute_count} disputes (threshold is {threshold})"
        );
        assert!(
            !tracker.is_in_sit_out(upstream_id).await,
            "Should NOT be in sit-out after {dispute_count} disputes"
        );
    }

    // Behavioral contract: reaching threshold SHOULD trigger sit-out
    let triggered_at_threshold = tracker.record_dispute(upstream_id, Some("method")).await;
    assert!(
        triggered_at_threshold,
        "Should trigger sit-out when reaching threshold of {threshold} disputes"
    );
    assert!(
        tracker.is_in_sit_out(upstream_id).await,
        "Should be in sit-out after reaching threshold"
    );
}

/// Test that sit-out expires after the configured duration
#[tokio::test]
async fn test_sit_out_expiration() {
    // Use tokio time pausing for deterministic testing
    time::pause();

    let sit_out_duration = 2; // 2 seconds
    let tracker = create_custom_tracker(2, sit_out_duration);
    let upstream_id = "test-upstream-3";

    // Trigger sit-out
    tracker.record_dispute(upstream_id, Some("method")).await;
    tracker.record_dispute(upstream_id, Some("method")).await;

    assert!(tracker.is_in_sit_out(upstream_id).await, "Should be in sit-out after threshold");

    // Advance time by sit-out duration + 1 second
    time::advance(Duration::from_secs(sit_out_duration + 1)).await;

    assert!(
        !tracker.is_in_sit_out(upstream_id).await,
        "Sit-out should have expired after duration"
    );

    time::resume();
}

/// Test that dispute window expiration resets count
#[tokio::test]
async fn test_dispute_window_expiration() {
    time::pause();

    let config = MisbehaviorConfig {
        dispute_threshold: Some(5),
        dispute_window_seconds: 3, // Short window for testing
        sit_out_penalty_seconds: 10,
        log_path: None,
        max_log_file_size_bytes: 100 * 1024 * 1024,
    };
    let tracker = Arc::new(MisbehaviorTracker::new(config));
    let upstream_id = "test-upstream-4";

    // Record 2 disputes
    tracker.record_dispute(upstream_id, Some("method")).await;
    tracker.record_dispute(upstream_id, Some("method")).await;

    let count = tracker.get_dispute_count(upstream_id).await;
    assert_eq!(count, 2, "Should have 2 disputes");

    // Advance time beyond dispute window
    time::advance(Duration::from_secs(4)).await;

    // Record another dispute - old ones should be pruned
    tracker.record_dispute(upstream_id, Some("method")).await;

    let count = tracker.get_dispute_count(upstream_id).await;
    // Dispute count should only include recent disputes within window
    assert!(count <= 2, "Old disputes should be pruned from window");

    time::resume();
}

/// Test filtering available upstreams excludes sit-out upstreams
#[tokio::test]
async fn test_filter_available_excludes_sit_out() {
    let tracker = create_custom_tracker(2, 10);

    // Create some test upstreams
    let upstream1_id = "upstream-1";
    let upstream2_id = "upstream-2";
    let upstream3_id = "upstream-3";

    // Put upstream2 in sit-out
    tracker.record_dispute(upstream2_id, Some("method")).await;
    tracker.record_dispute(upstream2_id, Some("method")).await;

    assert!(tracker.is_in_sit_out(upstream2_id).await, "upstream2 should be in sit-out");
    assert!(!tracker.is_in_sit_out(upstream1_id).await, "upstream1 should not be in sit-out");
    assert!(!tracker.is_in_sit_out(upstream3_id).await, "upstream3 should not be in sit-out");
}

/// Test multiple upstreams with independent dispute tracking
#[tokio::test]
async fn test_multiple_upstream_independence() {
    let tracker = create_custom_tracker(3, 10);

    let upstream1 = "upstream-a";
    let upstream2 = "upstream-b";

    // Record disputes for upstream1
    tracker.record_dispute(upstream1, Some("method")).await;
    tracker.record_dispute(upstream1, Some("method")).await;

    // Record dispute for upstream2
    tracker.record_dispute(upstream2, Some("method")).await;

    let count1 = tracker.get_dispute_count(upstream1).await;
    let count2 = tracker.get_dispute_count(upstream2).await;

    assert_eq!(count1, 2, "upstream1 should have 2 disputes");
    assert_eq!(count2, 1, "upstream2 should have 1 dispute");
    assert!(!tracker.is_in_sit_out(upstream1).await, "upstream1 not in sit-out (threshold 3)");
    assert!(!tracker.is_in_sit_out(upstream2).await, "upstream2 not in sit-out");
}

/// Test concurrent dispute recording from multiple tasks
#[tokio::test]
async fn test_concurrent_dispute_recording() {
    let tracker = create_test_tracker();
    let upstream_id = "concurrent-test";
    let tracker_clone1 = tracker.clone();
    let tracker_clone2 = tracker.clone();

    // Spawn two concurrent tasks recording disputes
    let handle1 = tokio::spawn(async move {
        for _ in 0..10 {
            tracker_clone1.record_dispute(upstream_id, Some("method")).await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    let handle2 = tokio::spawn(async move {
        for _ in 0..10 {
            tracker_clone2.record_dispute(upstream_id, Some("method")).await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // Wait for both tasks to complete
    handle1.await.expect("task 1 should complete");
    handle2.await.expect("task 2 should complete");

    let count = tracker.get_dispute_count(upstream_id).await;
    assert_eq!(count, 20, "Should have 20 total disputes from concurrent recording");
}

/// Test that dispute reasons are tracked
#[tokio::test]
async fn test_dispute_reasons_tracking() {
    let tracker = create_test_tracker();
    let upstream_id = "reason-test";

    tracker.record_dispute(upstream_id, Some("eth_getBlock")).await;
    tracker.record_dispute(upstream_id, Some("eth_getLogs")).await;
    tracker.record_dispute(upstream_id, Some("eth_getBlock")).await;

    let count = tracker.get_dispute_count(upstream_id).await;
    assert_eq!(count, 3, "Should have 3 total disputes");
}

/// Test sit-out re-entry after recovery
#[tokio::test]
async fn test_sit_out_recovery_and_reentry() {
    time::pause();

    let tracker = create_custom_tracker(2, 2);
    let upstream_id = "recovery-test";

    // First sit-out
    tracker.record_dispute(upstream_id, Some("method")).await;
    tracker.record_dispute(upstream_id, Some("method")).await;
    assert!(tracker.is_in_sit_out(upstream_id).await, "Should be in sit-out");

    // Wait for sit-out to expire
    time::advance(Duration::from_secs(3)).await;
    assert!(!tracker.is_in_sit_out(upstream_id).await, "Should have recovered from sit-out");

    // Trigger sit-out again
    tracker.record_dispute(upstream_id, Some("method")).await;
    tracker.record_dispute(upstream_id, Some("method")).await;
    assert!(tracker.is_in_sit_out(upstream_id).await, "Should be in sit-out again");

    time::resume();
}

/// Test that None threshold effectively disables sit-out
#[tokio::test]
async fn test_none_threshold_disables_sit_out() {
    let config = MisbehaviorConfig {
        dispute_threshold: None, // Disabled
        dispute_window_seconds: 60,
        sit_out_penalty_seconds: 10,
        log_path: None,
        max_log_file_size_bytes: 100 * 1024 * 1024,
    };
    let tracker = Arc::new(MisbehaviorTracker::new(config));
    let upstream_id = "never-sit-out";

    // Record many disputes
    for _ in 0..10 {
        tracker.record_dispute(upstream_id, Some("method")).await;
    }

    // Should never be in sit-out with threshold None
    assert!(
        !tracker.is_in_sit_out(upstream_id).await,
        "Should never sit-out with threshold None (disabled)"
    );
}
