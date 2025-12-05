//! Tests for WebSocket subscription functionality
//!
//! Tests the WebSocket failure tracking and connection management logic.
//! Network-dependent tests are mocked or focus on unit-testable components.

use std::time::Duration;
use tokio::time;

/// Test WebSocket failure tracker records failures correctly
#[tokio::test]
async fn test_failure_tracker_records_failures() {
    use prism_core::upstream::websocket::WebSocketFailureTracker;

    let mut tracker = WebSocketFailureTracker::default();

    assert_eq!(tracker.get_failure_count(), 0, "Should start with 0 failures");
    assert!(!tracker.should_stop_retrying(), "Should not stop retrying initially");

    // Record first failure
    tracker.record_failure();
    assert_eq!(tracker.get_failure_count(), 1, "Should have 1 failure");
    assert!(!tracker.should_stop_retrying(), "Should not stop after 1 failure");

    // Record second failure
    tracker.record_failure();
    assert_eq!(tracker.get_failure_count(), 2, "Should have 2 failures");

    // Record third failure
    tracker.record_failure();
    assert_eq!(tracker.get_failure_count(), 3, "Should have 3 failures");
    assert!(tracker.should_stop_retrying(), "Should stop retrying after hitting threshold (3)");
}

/// Test WebSocket failure tracker resets on success
#[tokio::test]
async fn test_failure_tracker_resets_on_success() {
    use prism_core::upstream::websocket::WebSocketFailureTracker;

    let mut tracker = WebSocketFailureTracker::default();

    // Record failures
    tracker.record_failure();
    tracker.record_failure();
    assert_eq!(tracker.get_failure_count(), 2, "Should have 2 failures");

    // Record success
    tracker.record_success();
    assert_eq!(tracker.get_failure_count(), 0, "Failures should reset to 0 after success");
    assert!(
        !tracker.should_stop_retrying(),
        "Should not stop retrying after successful connection"
    );
}

/// Test WebSocket failure tracker permanent failure detection
#[tokio::test]
async fn test_failure_tracker_permanent_failure() {
    use prism_core::upstream::websocket::WebSocketFailureTracker;

    let mut tracker = WebSocketFailureTracker::default();

    // Record enough failures to trigger permanent failure (2 * max_consecutive_failures)
    // Default max_consecutive_failures is 3, so 6 failures triggers permanent
    for _ in 0..6 {
        tracker.record_failure();
    }

    assert_eq!(tracker.get_failure_count(), 6, "Should have 6 failures");
    assert!(tracker.is_permanently_failed(), "Should be permanently failed");
    assert!(tracker.should_stop_retrying(), "Should stop retrying when permanently failed");

    // Success should clear the permanent failure flag
    tracker.record_success();
    assert!(!tracker.is_permanently_failed(), "Success should clear permanent failure flag");
    assert_eq!(tracker.get_failure_count(), 0, "Success should reset count");
}

/// Test WebSocket failure tracker reset after cooldown period
#[tokio::test]
async fn test_failure_tracker_reset_after_cooldown() {
    use prism_core::upstream::websocket::WebSocketFailureTracker;

    time::pause();

    let mut tracker = WebSocketFailureTracker::default();

    // Record enough failures to stop retrying
    tracker.record_failure();
    tracker.record_failure();
    tracker.record_failure();

    assert!(tracker.should_stop_retrying(), "Should stop retrying initially");

    // Advance time beyond failure_reset_duration (300 seconds default)
    time::advance(Duration::from_secs(301)).await;

    // Should allow retrying again after cooldown
    assert!(!tracker.should_stop_retrying(), "Should allow retrying after cooldown period");

    time::resume();
}

/// Test WebSocket failure tracker `reset_if_expired` clears old failures
#[tokio::test]
async fn test_failure_tracker_reset_if_expired() {
    use prism_core::upstream::websocket::WebSocketFailureTracker;

    time::pause();

    let mut tracker = WebSocketFailureTracker::default();

    // Record failures to hit threshold
    for _ in 0..3 {
        tracker.record_failure();
    }
    assert_eq!(tracker.get_failure_count(), 3, "Should have 3 failures");

    // Advance time beyond reset duration
    time::advance(Duration::from_secs(301)).await;

    // Call reset_if_expired
    tracker.reset_if_expired();

    assert_eq!(tracker.get_failure_count(), 0, "Failures should be reset after expiration");
    assert!(!tracker.is_permanently_failed(), "Permanent failure should be cleared");

    time::resume();
}

/// Test WebSocket failure tracking with rapid failures
#[tokio::test]
async fn test_failure_tracker_rapid_failures() {
    use prism_core::upstream::websocket::WebSocketFailureTracker;

    let mut tracker = WebSocketFailureTracker::default();

    // Simulate rapid connection failures
    for i in 1..=10 {
        tracker.record_failure();
        if i >= 3 {
            assert!(tracker.should_stop_retrying(), "Should stop retrying after threshold");
        }
    }

    assert_eq!(tracker.get_failure_count(), 10, "Should track all failures");
}

/// Test WebSocket failure tracker with alternating success/failure
#[tokio::test]
async fn test_failure_tracker_alternating_states() {
    use prism_core::upstream::websocket::WebSocketFailureTracker;

    let mut tracker = WebSocketFailureTracker::default();

    // Alternate between failure and success
    tracker.record_failure();
    assert_eq!(tracker.get_failure_count(), 1);

    tracker.record_success();
    assert_eq!(tracker.get_failure_count(), 0);

    tracker.record_failure();
    tracker.record_failure();
    assert_eq!(tracker.get_failure_count(), 2);

    tracker.record_success();
    assert_eq!(tracker.get_failure_count(), 0, "Success should always reset count");
}

/// Test WebSocket failure tracker edge case: zero threshold
#[tokio::test]
async fn test_failure_tracker_edge_cases() {
    use prism_core::upstream::websocket::WebSocketFailureTracker;

    let mut tracker = WebSocketFailureTracker::default();

    // Test initial state
    assert!(!tracker.should_stop_retrying(), "Should not stop initially");
    assert!(!tracker.is_permanently_failed(), "Should not be permanently failed initially");

    // Test single failure
    tracker.record_failure();
    assert!(!tracker.should_stop_retrying(), "Should not stop after single failure");
}
