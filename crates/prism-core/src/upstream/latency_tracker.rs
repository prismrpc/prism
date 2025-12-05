//! Latency percentile tracker using a sliding window.
//!
//! Maintains a fixed-size circular buffer of recent latency measurements
//! for computing percentiles without sorting on every request.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Default staleness threshold in seconds (5 minutes).
const DEFAULT_STALENESS_THRESHOLD_SECS: u64 = 300;

/// Latency percentile tracker using a sliding window.
///
/// Maintains a fixed-size circular buffer of recent latency measurements
/// for computing percentiles without sorting on every request.
///
/// # Interior Mutability Pattern
///
/// This type uses **interior mutability** via atomics to allow mutation through `&self`.
/// All public methods take `&self` (shared reference) but perform interior mutation
/// via `AtomicU64`, `AtomicUsize` fields.
///
/// ## Why This Violates Typical Rust Expectations
///
/// In typical Rust code, `&self` methods are read-only and `&mut self` is required for
/// mutation. This type breaks that convention by using atomics for lock-free concurrent
/// updates from multiple threads.
///
/// ## Safety Guarantees
///
/// Despite the interior mutability, this type is **completely safe**:
///
/// - All mutations use atomic operations with appropriate ordering (Relaxed for performance)
/// - No `unsafe` code is required
/// - Thread-safe by design - multiple threads can call `record()` concurrently
///
/// ## Usage Example
///
/// ```ignore
/// let tracker = Arc::new(LatencyTracker::new(1000));
///
/// // Multiple threads can record concurrently using shared reference
/// let t1 = tracker.clone();
/// tokio::spawn(async move {
///     t1.record(100);  // Takes &self but mutates atomically
/// });
///
/// let t2 = tracker.clone();
/// tokio::spawn(async move {
///     t2.record(150);  // Also takes &self, safe to run concurrently
/// });
/// ```
///
/// ## When To Use This Pattern
///
/// Interior mutability with atomics is appropriate when:
/// - High-frequency concurrent updates from multiple threads
/// - Lock-free performance is critical (hot path)
/// - Approximate values are acceptable (percentiles, averages)
///
/// Do NOT use this pattern when:
/// - Exact synchronization is required
/// - Complex invariants must be maintained across multiple fields
/// - Single-threaded access is sufficient (use `Cell` or `RefCell` instead)
///
/// # Staleness Detection
///
/// The tracker maintains a timestamp of the last recorded sample. If no samples
/// have been recorded for longer than the staleness threshold, the data is considered
/// stale and percentile calculations return `None`. This prevents hedging decisions
/// based on outdated latency data.
///
/// # Lock-Free Recording
///
/// Uses a lock-free atomic ring buffer for recording latency samples,
/// eliminating write lock contention on the hot path. Percentile calculations
/// still require collecting samples into a sorted vector.
pub struct LatencyTracker {
    /// Lock-free ring buffer using atomic operations
    samples: Box<[AtomicU64]>,
    max_samples: usize,
    /// Write index for circular buffer (wraps around at `max_samples`)
    write_index: AtomicUsize,
    /// Number of samples recorded (capped at `max_samples`)
    count: AtomicUsize,
    /// Timestamp of the last recorded sample (for staleness detection)
    /// Stored as Unix timestamp in milliseconds (0 = never updated)
    last_updated_ms: AtomicU64,
    /// Duration after which data is considered stale (default: 5 minutes)
    staleness_threshold_secs: u64,
}

impl LatencyTracker {
    /// Creates a new latency tracker with the specified window size.
    #[must_use]
    pub fn new(max_samples: usize) -> Self {
        // Create atomic array initialized to 0
        let samples = (0..max_samples).map(|_| AtomicU64::new(0)).collect::<Vec<_>>();
        Self {
            samples: samples.into_boxed_slice(),
            max_samples,
            write_index: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
            last_updated_ms: AtomicU64::new(0),
            staleness_threshold_secs: DEFAULT_STALENESS_THRESHOLD_SECS,
        }
    }

    /// Creates a new latency tracker with custom staleness threshold.
    #[must_use]
    pub fn with_staleness_threshold(max_samples: usize, staleness_threshold_secs: u64) -> Self {
        let samples = (0..max_samples).map(|_| AtomicU64::new(0)).collect::<Vec<_>>();
        Self {
            samples: samples.into_boxed_slice(),
            max_samples,
            write_index: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
            last_updated_ms: AtomicU64::new(0),
            staleness_threshold_secs,
        }
    }

    /// Checks if the latency data is stale (no updates for longer than threshold).
    #[must_use]
    pub fn is_stale(&self) -> bool {
        let last_ms = self.last_updated_ms.load(Ordering::Relaxed);
        if last_ms == 0 {
            return true; // Never updated
        }

        // Convert current time to milliseconds since epoch
        #[allow(clippy::cast_possible_truncation)]
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let elapsed_secs = (now_ms.saturating_sub(last_ms)) / 1000;
        elapsed_secs > self.staleness_threshold_secs
    }

    /// Records a new latency sample using lock-free atomic operations.
    pub fn record(&self, latency_ms: u64) {
        // Get current write index and increment atomically
        let index = self.write_index.fetch_add(1, Ordering::Relaxed) % self.max_samples;

        // Store the new sample
        let old_value = self.samples[index].swap(latency_ms, Ordering::Relaxed);

        // Increment count up to max_samples (only if this was an empty slot)
        if old_value == 0 {
            self.count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                    Some(c.saturating_add(1).min(self.max_samples))
                })
                .ok();
        }

        // Update timestamp (milliseconds since epoch)
        #[allow(clippy::cast_possible_truncation)]
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_updated_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Calculates the specified percentile from recorded samples.
    ///
    /// Returns `None` if no samples, quantile out of range (0.0-1.0), or data is stale.
    #[must_use]
    pub fn percentile(&self, quantile: f64) -> Option<u64> {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 || !(0.0..=1.0).contains(&quantile) {
            return None;
        }

        // Return None if data is stale to prevent hedging decisions on outdated data
        if self.is_stale() {
            return None;
        }

        let mut sorted: Vec<u64> = self
            .samples
            .iter()
            .take(count)
            .map(|atom| atom.load(Ordering::Relaxed))
            .filter(|&v| v > 0)
            .collect();

        if sorted.is_empty() {
            return None;
        }

        sorted.sort_unstable();

        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let index = ((sorted.len() as f64 - 1.0) * quantile) as usize;
        Some(sorted[index])
    }

    /// Returns the average latency across all samples.
    ///
    /// Calculates the average on-demand from the samples array.
    /// Returns `None` if no samples or data is stale.
    #[must_use]
    pub fn average(&self) -> Option<u64> {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 || self.is_stale() {
            return None;
        }

        // Calculate sum from samples on-demand
        let sum: u64 = self
            .samples
            .iter()
            .take(count)
            .map(|atom| atom.load(Ordering::Relaxed))
            .filter(|&v| v > 0)
            .sum();

        let valid_count = self
            .samples
            .iter()
            .take(count)
            .map(|atom| atom.load(Ordering::Relaxed))
            .filter(|&v| v > 0)
            .count();

        if valid_count == 0 {
            None
        } else {
            Some(sum / valid_count as u64)
        }
    }

    /// Returns the number of samples currently tracked.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    /// Returns the time since the last sample was recorded.
    #[must_use]
    pub fn time_since_last_update(&self) -> Option<std::time::Duration> {
        let last_ms = self.last_updated_ms.load(Ordering::Relaxed);
        if last_ms == 0 {
            return None;
        }

        #[allow(clippy::cast_possible_truncation)]
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Some(std::time::Duration::from_millis(now_ms.saturating_sub(last_ms)))
    }

    /// Clears all samples.
    pub fn clear(&self) {
        // Reset all atomic values
        for sample in &*self.samples {
            sample.store(0, Ordering::Relaxed);
        }
        self.write_index.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
        self.last_updated_ms.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_tracker_basic() {
        let tracker = LatencyTracker::new(100);

        // Record some samples
        tracker.record(100);
        tracker.record(200);
        tracker.record(300);

        assert_eq!(tracker.sample_count(), 3);
        assert_eq!(tracker.average(), Some(200));
    }

    #[test]
    fn test_latency_tracker_percentiles() {
        let tracker = LatencyTracker::new(100);

        // Record samples from 1-100ms
        for i in 1..=100 {
            tracker.record(i);
        }

        assert_eq!(tracker.sample_count(), 100);
        assert_eq!(tracker.percentile(0.50), Some(50)); // P50
        assert_eq!(tracker.percentile(0.95), Some(95)); // P95
        assert_eq!(tracker.percentile(0.99), Some(99)); // P99
    }

    #[test]
    fn test_latency_tracker_sliding_window() {
        let tracker = LatencyTracker::new(10);

        // Fill the window
        for i in 1..=10 {
            tracker.record(i);
        }

        assert_eq!(tracker.sample_count(), 10);

        // Add more samples (should evict old ones)
        tracker.record(100);
        tracker.record(200);

        assert_eq!(tracker.sample_count(), 10);
        assert!(tracker.average().unwrap() > 10); // Average should be higher
    }

    #[test]
    fn test_latency_tracker_clear() {
        let tracker = LatencyTracker::new(100);
        tracker.record(100);
        tracker.record(200);

        assert_eq!(tracker.sample_count(), 2);

        tracker.clear();

        assert_eq!(tracker.sample_count(), 0);
        assert_eq!(tracker.average(), None);
        assert_eq!(tracker.percentile(0.95), None);
    }

    #[test]
    fn test_latency_tracker_staleness() {
        // Create tracker with very short staleness threshold (0 seconds = always stale after
        // creation)
        let tracker = LatencyTracker::with_staleness_threshold(100, 0);

        // No samples yet - should be stale
        assert!(tracker.is_stale());
        assert_eq!(tracker.percentile(0.95), None);

        // Record a sample
        tracker.record(100);

        // Data is fresh right after recording (even with 0s threshold, we check >)
        assert!(!tracker.is_stale());
        assert_eq!(tracker.percentile(0.95), Some(100));

        // With longer threshold, data should remain fresh
        let tracker2 = LatencyTracker::with_staleness_threshold(100, 3600);
        tracker2.record(100);
        assert!(!tracker2.is_stale());
        assert_eq!(tracker2.percentile(0.95), Some(100));
    }

    #[test]
    fn test_latency_tracker_time_since_update() {
        let tracker = LatencyTracker::new(100);

        // No updates yet
        assert!(tracker.time_since_last_update().is_none());

        // After recording, should have an update time
        tracker.record(100);
        let elapsed = tracker.time_since_last_update();
        assert!(elapsed.is_some());
        assert!(elapsed.unwrap().as_millis() < 100); // Should be very recent
    }
}
