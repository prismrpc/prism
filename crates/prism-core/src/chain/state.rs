//! Unified chain state tracking.
//!
//! `ChainState` provides a single source of truth for the current chain tip,
//! finalized block, and head hash. All components (`CacheManager`, `ReorgManager`,
//! `ScoringEngine`) read from this shared state instead of maintaining independent
//! tracking.

use arc_swap::ArcSwap;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::RwLock;
use tracing::trace;

/// Combined tip state: block number and hash.
#[derive(Clone, Copy, Debug, Default)]
struct ChainTip {
    block_number: u64,
    block_hash: [u8; 32],
}

/// Shared chain state tracking the current tip, finalized block, and head hash.
///
/// # Thread Safety
///
/// All methods are thread-safe. The chain tip uses `ArcSwap` for atomic updates,
/// and the finalized block uses atomic operations with `Acquire`/`Release` ordering.
///
/// # Tip Update Method Selection
///
/// Choose the appropriate method based on your use case:
///
/// | Method | Use When | Behavior |
/// |--------|----------|----------|
/// | `update_tip()` | Normal block progression | Rejects if new block ≤ current |
/// | `force_update_tip()` | Rollback/reorg handling | Always updates, no rejection |
/// | `update_tip_simple()` | Only updating number | Same as `update_tip`, hash unchanged |
/// | `update_head_hash()` | Same-height reorg | Updates hash only, number unchanged |
///
/// **Typical usage:**
/// - `ReorgManager` uses `update_tip()` for forward progression
/// - `ReorgManager` uses `force_update_tip()` after detecting rollback
/// - WebSocket handler uses `update_head_hash()` for same-height reorgs
///
/// # Example
///
/// ```no_run
/// use prism_core::chain::ChainState;
/// use std::sync::Arc;
///
/// # async fn example() {
/// let chain_state = Arc::new(ChainState::new());
///
/// // Update the chain tip (async - requires coordination)
/// chain_state.update_tip(1000, [1u8; 32]).await;
///
/// // Read the current tip (lock-free, sync)
/// let tip = chain_state.current_tip();
/// assert_eq!(tip, 1000);
///
/// // Calculate safe head (lock-free, sync)
/// let safe_head = chain_state.safe_head(12);
/// assert_eq!(safe_head, 988);
/// # }
/// ```
#[derive(Clone)]
pub struct ChainState {
    /// Current chain tip: block number and hash protected by `ArcSwap`.
    tip: Arc<ArcSwap<ChainTip>>,

    /// Async `RwLock` for coordinating async updates.
    tip_write_lock: Arc<RwLock<()>>,

    /// Finalized block number (beyond which reorgs are impossible).
    finalized_block: Arc<AtomicU64>,

    /// Unix timestamp (seconds) of the last tip update.
    last_tip_update: Arc<AtomicU64>,
}

/// Returns the current unix timestamp in seconds.
fn current_unix_timestamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl ChainState {
    /// Creates a new `ChainState` with all values initialized to zero/empty.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tip: Arc::new(ArcSwap::from_pointee(ChainTip::default())),
            tip_write_lock: Arc::new(RwLock::new(())),
            finalized_block: Arc::new(AtomicU64::new(0)),
            last_tip_update: Arc::new(AtomicU64::new(current_unix_timestamp())),
        }
    }

    /// Returns the current chain tip block number.
    #[inline]
    #[must_use]
    pub fn current_tip(&self) -> u64 {
        self.tip.load().block_number
    }

    /// Returns the finalized block number.
    ///
    /// Blocks at or below this number are finalized and cannot be reorged.
    #[inline]
    #[must_use]
    pub fn finalized_block(&self) -> u64 {
        self.finalized_block.load(Ordering::Acquire)
    }

    /// Returns the number of seconds since the last tip update.
    ///
    /// Used by the admin API to show how fresh the chain state is.
    #[inline]
    #[must_use]
    pub fn tip_age_seconds(&self) -> u64 {
        let last_update = self.last_tip_update.load(Ordering::Acquire);
        current_unix_timestamp().saturating_sub(last_update)
    }

    /// Returns the current head block hash.
    #[inline]
    #[must_use]
    pub fn current_head_hash(&self) -> [u8; 32] {
        self.tip.load().block_hash
    }

    /// Returns both the current tip block number and hash atomically.
    #[inline]
    #[must_use]
    pub fn current_tip_with_hash(&self) -> (u64, [u8; 32]) {
        let tip = **self.tip.load();
        (tip.block_number, tip.block_hash)
    }

    /// Updates the chain tip with a new block number and hash.
    ///
    /// Only updates if the new block is newer than the current tip.
    ///
    /// # Returns
    ///
    /// `true` if the tip was updated, `false` if the new block is not newer
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use prism_core::chain::ChainState;
    /// # use std::sync::Arc;
    /// # async fn example() {
    /// let chain_state = Arc::new(ChainState::new());
    ///
    /// let updated = chain_state.update_tip(1000, [1u8; 32]).await;
    /// assert!(updated);
    ///
    /// // Older block - not updated
    /// let updated = chain_state.update_tip(999, [2u8; 32]).await;
    /// assert!(!updated);
    /// # }
    /// ```
    pub async fn update_tip(&self, block_number: u64, block_hash: [u8; 32]) -> bool {
        let _guard = self.tip_write_lock.write().await;

        let current = **self.tip.load();
        if block_number <= current.block_number {
            return false;
        }

        self.tip.store(Arc::new(ChainTip { block_number, block_hash }));
        self.last_tip_update.store(current_unix_timestamp(), Ordering::Release);
        trace!(block = block_number, "chain tip updated");
        true
    }

    /// Forces the chain tip to a specific block, even if lower than current.
    ///
    /// Used for rollback scenarios where the chain tip must go backwards.
    /// This is needed when reorgs are detected via health checks and the tip
    /// needs to be set to a lower block number.
    ///
    /// # Safety
    ///
    /// Only use this for verified rollback scenarios. Normal forward progression
    /// should use `update_tip()`.
    pub async fn force_update_tip(&self, block_number: u64, block_hash: [u8; 32]) {
        let _guard = self.tip_write_lock.write().await;
        self.tip.store(Arc::new(ChainTip { block_number, block_hash }));
        self.last_tip_update.store(current_unix_timestamp(), Ordering::Release);
        trace!(block = block_number, "chain tip force updated (rollback)");
    }

    /// Forces the tip to a specific block number without updating the hash.
    ///
    /// Used for rollback scenarios when the hash is not available.
    /// This preserves whatever hash was previously stored.
    pub async fn force_update_tip_simple(&self, block_number: u64) {
        let _guard = self.tip_write_lock.write().await;
        let current = **self.tip.load();
        self.tip
            .store(Arc::new(ChainTip { block_number, block_hash: current.block_hash }));
        self.last_tip_update.store(current_unix_timestamp(), Ordering::Release);
        trace!(block = block_number, "chain tip force updated (simple, no hash)");
    }

    /// Updates only the head hash at the same block height (for reorg handling).
    ///
    /// This is used when a reorg is detected at the current tip - the block number
    /// stays the same but the hash changes to the new canonical chain's block.
    pub async fn update_head_hash(&self, block_hash: [u8; 32]) {
        let _guard = self.tip_write_lock.write().await;
        let current = **self.tip.load();
        self.tip
            .store(Arc::new(ChainTip { block_number: current.block_number, block_hash }));
        trace!("head hash updated (reorg)");
    }

    /// Updates only the chain tip block number without updating the hash.
    ///
    /// This is useful when the hash is not available or not needed.
    /// Only updates if the new block is newer than the current tip.
    ///
    /// # Returns
    ///
    /// `true` if the tip was updated, `false` if the new block is not newer
    #[must_use]
    pub async fn update_tip_simple(&self, block_number: u64) -> bool {
        let _guard = self.tip_write_lock.write().await;
        let current = **self.tip.load();
        if block_number <= current.block_number {
            return false;
        }
        self.tip
            .store(Arc::new(ChainTip { block_number, block_hash: current.block_hash }));
        self.last_tip_update.store(current_unix_timestamp(), Ordering::Release);
        trace!(block = block_number, "chain tip updated (simple)");
        true
    }

    /// Updates the finalized block number.
    ///
    /// Only updates if the new finalized block is higher than the current one
    /// AND does not exceed the current chain tip (blockchain invariant).
    ///
    /// # Returns
    ///
    /// `true` if the finalized block was updated, `false` otherwise
    pub async fn update_finalized(&self, block: u64) -> bool {
        let _tip_guard = self.tip_write_lock.write().await;

        loop {
            let current_finalized = self.finalized_block.load(Ordering::Acquire);
            let current_tip = self.tip.load().block_number;

            if block <= current_finalized || block > current_tip {
                return false;
            }

            if self
                .finalized_block
                .compare_exchange_weak(
                    current_finalized,
                    block,
                    Ordering::Release,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                trace!(block = block, "finalized block updated");
                return true;
            }
        }
    }

    /// Returns the safe head block number (tip minus safety depth).
    ///
    /// # Arguments
    ///
    /// * `safety_depth` - Number of blocks to wait before considering a block safe
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use prism_core::chain::ChainState;
    /// # use std::sync::Arc;
    /// # async fn example() {
    /// let chain_state = Arc::new(ChainState::new());
    /// chain_state.update_tip(1000, [1u8; 32]).await;
    ///
    /// // With 12 block safety depth
    /// let safe_head = chain_state.safe_head(12);
    /// assert_eq!(safe_head, 988);
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn safe_head(&self, safety_depth: u64) -> u64 {
        self.current_tip().saturating_sub(safety_depth)
    }
}

impl Default for ChainState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::doc_markdown, clippy::uninlined_format_args)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_chain_state_new() {
        let state = ChainState::new();
        assert_eq!(state.current_tip(), 0);
        assert_eq!(state.finalized_block(), 0);
        assert_eq!(state.current_head_hash(), [0u8; 32]);
    }

    #[tokio::test]
    async fn test_update_tip() {
        let state = ChainState::new();

        let updated = state.update_tip(100, [1u8; 32]).await;
        assert!(updated);
        assert_eq!(state.current_tip(), 100);
        assert_eq!(state.current_head_hash(), [1u8; 32]);

        // Update to newer block
        let updated = state.update_tip(200, [2u8; 32]).await;
        assert!(updated);
        assert_eq!(state.current_tip(), 200);
        assert_eq!(state.current_head_hash(), [2u8; 32]);

        // Try to update with older block - should not update
        let updated = state.update_tip(150, [3u8; 32]).await;
        assert!(!updated);
        assert_eq!(state.current_tip(), 200);
        assert_eq!(state.current_head_hash(), [2u8; 32]);
    }

    #[tokio::test]
    async fn test_update_tip_simple() {
        let state = ChainState::new();

        let updated = state.update_tip_simple(100).await;
        assert!(updated);
        assert_eq!(state.current_tip(), 100);

        // Older block should not update
        let updated = state.update_tip_simple(50).await;
        assert!(!updated);
        assert_eq!(state.current_tip(), 100);

        // Newer block should update
        let updated = state.update_tip_simple(200).await;
        assert!(updated);
        assert_eq!(state.current_tip(), 200);
    }

    #[tokio::test]
    async fn test_update_finalized() {
        let state = ChainState::new();

        // Set tip first (finalized can't exceed tip)
        let _ = state.update_tip_simple(150).await;

        let updated = state.update_finalized(50).await;
        assert!(updated);
        assert_eq!(state.finalized_block(), 50);

        // Older block should not update
        let updated = state.update_finalized(30).await;
        assert!(!updated);
        assert_eq!(state.finalized_block(), 50);

        // Newer block should update
        let updated = state.update_finalized(100).await;
        assert!(updated);
        assert_eq!(state.finalized_block(), 100);

        // Block exceeding tip should not update
        let updated = state.update_finalized(200).await;
        assert!(!updated);
        assert_eq!(state.finalized_block(), 100);
    }

    #[tokio::test]
    async fn test_safe_head() {
        let state = ChainState::new();

        state.update_tip(1000, [1u8; 32]).await;

        assert_eq!(state.safe_head(12), 988);
        assert_eq!(state.safe_head(100), 900);
        assert_eq!(state.safe_head(0), 1000);

        // Test underflow protection
        assert_eq!(state.safe_head(2000), 0);
    }

    #[tokio::test]
    async fn test_clone_is_cheap() {
        let state = Arc::new(ChainState::new());
        state.update_tip(100, [1u8; 32]).await;

        // Cloning Arc is cheap
        let cloned = state.clone();

        // Both see the same state
        assert_eq!(state.current_tip(), 100);
        assert_eq!(cloned.current_tip(), 100);

        cloned.update_tip(200, [2u8; 32]).await;
        assert_eq!(state.current_tip(), 200);
        assert_eq!(state.current_head_hash(), [2u8; 32]);
    }

    #[tokio::test]
    async fn test_concurrent_access() {
        use tokio::task;

        let state = Arc::new(ChainState::new());
        let mut handles = vec![];

        // Spawn multiple tasks updating the tip
        for i in 0..10_u64 {
            let state_clone = state.clone();
            let handle = task::spawn(async move {
                #[allow(clippy::cast_possible_truncation)]
                state_clone.update_tip(i * 100, [i as u8; 32]).await;
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // Final tip should be one of the values (900 is highest)
        let final_tip = state.current_tip();
        assert!(final_tip <= 900);
    }

    #[tokio::test]
    async fn test_current_tip_with_hash_atomicity() {
        let state = ChainState::new();

        state.update_tip(100, [1u8; 32]).await;

        // Get both values atomically
        let (tip, hash) = state.current_tip_with_hash();
        assert_eq!(tip, 100);
        assert_eq!(hash, [1u8; 32]);

        // Update and verify atomicity
        state.update_tip(200, [2u8; 32]).await;
        let (tip, hash) = state.current_tip_with_hash();
        assert_eq!(tip, 200);
        assert_eq!(hash, [2u8; 32]);
    }

    // Property-Based Concurrency Tests
    // These tests verify invariants that must hold under concurrent access.

    /// Property: Concurrent updates never produce inconsistent (tip, hash) pairs.
    ///
    /// When reading `current_tip_with_hash()`, the returned (tip, hash) must correspond
    /// to a single atomic update - we should never observe a tip from one update paired
    /// with a hash from a different update.
    ///
    /// This test spawns concurrent updaters with deterministic (block, hash) pairs
    /// where hash = [block as u8; 32], then verifies consistency holds.
    #[tokio::test]
    #[allow(clippy::cast_possible_truncation)]
    async fn test_property_concurrent_updates_preserve_consistency() {
        use std::sync::atomic::AtomicUsize;
        use tokio::task;

        const NUM_ITERATIONS: usize = 20;
        const NUM_TASKS: u64 = 50;
        const NUM_READERS: usize = 10;

        for iteration in 0..NUM_ITERATIONS {
            let state = Arc::new(ChainState::new());
            let completed = Arc::new(AtomicUsize::new(0));
            let consistency_violations = Arc::new(AtomicUsize::new(0));

            // Spawn writer tasks
            let mut handles = vec![];
            for i in 1..=NUM_TASKS {
                let state_clone = state.clone();
                let completed_clone = completed.clone();
                handles.push(task::spawn(async move {
                    // Vary timing to increase interleaving
                    if i % 7 == 0 {
                        tokio::task::yield_now().await;
                    }
                    #[allow(clippy::cast_possible_truncation)]
                    let hash = [i as u8; 32];
                    state_clone.update_tip(i, hash).await;
                    completed_clone.fetch_add(1, Ordering::SeqCst);
                }));
            }

            // Spawn reader tasks that check consistency during updates
            for _ in 0..NUM_READERS {
                let state_clone = state.clone();
                let violations = consistency_violations.clone();
                handles.push(task::spawn(async move {
                    for _ in 0..10 {
                        let (tip, hash) = state_clone.current_tip_with_hash();
                        // Property: hash must correspond to tip
                        // (each writer uses hash = [block as u8; 32])
                        if tip > 0 {
                            #[allow(clippy::cast_possible_truncation)]
                            let expected_hash = [tip as u8; 32];
                            if hash != expected_hash {
                                violations.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        tokio::task::yield_now().await;
                    }
                }));
            }

            for handle in handles {
                let _ = handle.await;
            }

            // Verify property: no consistency violations occurred
            let violations = consistency_violations.load(Ordering::SeqCst);
            assert_eq!(
                violations, 0,
                "Property violation in iteration {}: {} inconsistent (tip, hash) reads",
                iteration, violations
            );

            // Verify all writers completed
            assert_eq!(completed.load(Ordering::SeqCst), NUM_TASKS as usize);

            // Final consistency check
            let (final_tip, final_hash) = state.current_tip_with_hash();
            #[allow(clippy::cast_possible_truncation)]
            let expected_final_hash = [final_tip as u8; 32];
            assert_eq!(
                final_hash, expected_final_hash,
                "Final state inconsistent: tip={}, hash={:?}",
                final_tip, final_hash
            );
        }
    }

    /// Property: Finalized block is always <= current tip.
    ///
    /// This invariant must hold regardless of the order of update_tip and
    /// update_finalized calls.
    #[tokio::test]
    async fn test_property_finalized_never_exceeds_tip() {
        let state = ChainState::new();

        // Property: finalized can be set when <= tip
        state.update_tip(100, [1u8; 32]).await;
        assert!(state.update_finalized(50).await, "finalized=50 should succeed when tip=100");
        assert!(state.update_finalized(100).await, "finalized=tip should succeed");

        // Property: finalized cannot exceed tip
        assert!(!state.update_finalized(101).await, "finalized>tip should fail");
        assert!(!state.update_finalized(200).await, "finalized>tip should fail");

        // Verify invariant holds
        let finalized = state.finalized_block();
        let tip = state.current_tip();
        assert!(finalized <= tip, "Invariant violated: finalized={} > tip={}", finalized, tip);
    }

    /// Property: Finalized can only increase, never decrease.
    #[tokio::test]
    async fn test_property_finalized_monotonically_increases() {
        let state = ChainState::new();

        state.update_tip(1000, [1u8; 32]).await;

        // Finalized increases
        assert!(state.update_finalized(100).await);
        assert_eq!(state.finalized_block(), 100);

        assert!(state.update_finalized(200).await);
        assert_eq!(state.finalized_block(), 200);

        // Cannot decrease (returns false, value unchanged)
        assert!(!state.update_finalized(150).await);
        assert_eq!(state.finalized_block(), 200, "Finalized should not decrease");

        // Cannot set to same value (no update needed)
        let _ = state.update_finalized(200).await;
        assert_eq!(state.finalized_block(), 200);
    }

    /// Property: force_update_tip allows rollbacks while maintaining consistency.
    ///
    /// Unlike update_tip (which only advances), force_update_tip can set any value.
    /// The key property is that tip and hash remain consistent after the call.
    #[tokio::test]
    async fn test_property_force_update_maintains_consistency() {
        let state = ChainState::new();

        // Set initial state
        state.update_tip(1000, [0xAA; 32]).await;
        let (tip, hash) = state.current_tip_with_hash();
        assert_eq!(tip, 1000);
        assert_eq!(hash, [0xAA; 32]);

        // Force rollback - should maintain consistency
        state.force_update_tip(500, [0xBB; 32]).await;
        let (tip, hash) = state.current_tip_with_hash();
        assert_eq!(tip, 500);
        assert_eq!(hash, [0xBB; 32]);

        // Force advance beyond original - should maintain consistency
        state.force_update_tip(2000, [0xCC; 32]).await;
        let (tip, hash) = state.current_tip_with_hash();
        assert_eq!(tip, 2000);
        assert_eq!(hash, [0xCC; 32]);
    }

    /// Property: Concurrent force_update_tip calls maintain consistency.
    ///
    /// Similar to update_tip but tests the forced variant which allows rollbacks.
    #[tokio::test]
    async fn test_property_concurrent_force_updates_preserve_consistency() {
        use tokio::task;

        const NUM_ITERATIONS: usize = 10;
        const NUM_TASKS: u64 = 30;

        for iteration in 0..NUM_ITERATIONS {
            let state = Arc::new(ChainState::new());

            // Spawn tasks that force various block numbers (including rollbacks)
            let mut handles = vec![];
            for i in 1..=NUM_TASKS {
                let state_clone = state.clone();
                handles.push(task::spawn(async move {
                    // Alternate between different block ranges to test rollbacks
                    let block = if i % 2 == 0 { i } else { NUM_TASKS - i + 1 };
                    #[allow(clippy::cast_possible_truncation)]
                    let hash = [block as u8; 32];
                    state_clone.force_update_tip(block, hash).await;
                }));
            }

            for handle in handles {
                let _ = handle.await;
            }

            // Property: Final state must be consistent
            let (final_tip, final_hash) = state.current_tip_with_hash();
            #[allow(clippy::cast_possible_truncation)]
            let expected_hash = [final_tip as u8; 32];
            assert_eq!(
                final_hash, expected_hash,
                "Iteration {}: inconsistent state - tip={}, hash={:?}",
                iteration, final_tip, final_hash
            );
        }
    }
}
