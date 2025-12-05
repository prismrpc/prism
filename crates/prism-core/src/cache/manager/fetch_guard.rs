//! `FetchGuard` RAII type for coordinating concurrent block fetches.
//!
//! This module contains the `FetchGuard` type which ensures proper cleanup of
//! fetch coordination state via the Drop trait. The guard is created by
//! `CacheManager::try_begin_fetch()` and `CacheManager::begin_fetch_with_timeout()`.
//!
//! # Critical Patterns
//!
//! ## Channel-Based Cleanup in Drop
//!
//! The Drop implementation sends cleanup requests through an unbounded channel
//! instead of spawning a task per drop. This provides:
//! - **Zero allocation in Drop**: `UnboundedSender::send()` doesn't allocate
//! - **Non-blocking**: Never waits for channel capacity
//! - **Batched processing**: Cleanup worker processes multiple requests efficiently
//!
//! ## Preservation During Refactoring
//!
//! The Drop implementation must ALWAYS use channel send, NEVER task spawning.
//! This is a critical performance invariant - see C-CONCUR-2 in the code review.

use std::sync::Arc;
use tokio::{
    sync::{mpsc, OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

/// Cleanup request sent from `FetchGuard::Drop` to the cleanup worker.
///
/// This message type enables allocation-free cleanup by sending block information
/// through a channel instead of spawning a task per drop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CleanupRequest {
    /// Block number to clean up from inflight tracking
    pub block: u64,
    /// Whether to also release the fetch lock (`blocks_being_fetched` entry)
    pub release_fetch_lock: bool,
}

/// Tracks an in-flight fetch operation with a semaphore to coordinate concurrent fetches
/// of the same block and a timestamp for detecting stale fetches.
#[derive(Clone)]
pub struct InflightFetch {
    pub semaphore: Arc<Semaphore>,
    pub started_at: Instant,
}

/// RAII guard for fetch operations that ensures cleanup on drop.
///
/// # Cleanup Architecture
///
/// - **Zero allocation in Drop**: Channel send is allocation-free
/// - **Batched processing**: Cleanup worker batches requests for efficiency
/// - **Deterministic shutdown**: Pending requests processed before exit
///
/// The constructor ensures the RAII pattern is complete - semaphore creation
/// and guard creation happen atomically, preventing resource leaks on panic.
///
/// # State Machine Diagram
///
/// ```text
/// ┌─────────────────────┐
/// │  Initial Request    │
/// └──────────┬──────────┘
///            │
///            v
/// ┌─────────────────────────────┐
/// │ Phase 1: Pre-Flight Check   │ ◄─── Prevents stale inflight entries
/// │ is_block_fully_cached()?    │
/// └──────────┬──────────────────┘
///            │
///         ┌──┴──┐
///         │ Yes │──► Return None (already cached)
///         └─────┘
///            │ No
///            v
/// ┌────────────────────────────────┐
/// │ Phase 2: Semaphore Setup       │ ◄─── Lock released immediately
/// │ Get or create inflight entry   │      to prevent deadlock
/// │ Clone semaphore & release lock │
/// └──────────┬─────────────────────┘
///            │
///            v
/// ┌────────────────────────────────┐
/// │ Phase 3: Permit Acquisition    │ ◄─── Try/wait for permit
/// │ try_acquire_owned() or timeout │
/// └──────────┬─────────────────────┘
///            │
///       ┌────┴────┐
///       │ Failed  │──► Return None (already in progress)
///       └─────────┘
///            │ Success
///            v
/// ┌────────────────────────────────┐
/// │ Phase 4: Post-Permit Check     │ ◄─── Double-check pattern:
/// │ is_block_fully_cached()?       │      another task may have
/// └──────────┬─────────────────────┘      completed while we waited
///            │
///         ┌──┴──┐
///         │ Yes │──► Clean up & return None
///         └─────┘
///            │ No
///            v
/// ┌────────────────────────────────┐
/// │ Phase 5: Lock Acquisition      │ ◄─── Mark block as being fetched
/// │ try_acquire_fetch_lock()       │
/// └──────────┬─────────────────────┘
///            │
///            v
/// ┌────────────────────────────────┐
/// │   Return FetchGuard (RAII)     │ ◄─── Drop trait ensures cleanup
/// └────────────────────────────────┘
/// ```
pub struct FetchGuard {
    /// Channel sender for cleanup requests (cheap clone - reference counted)
    cleanup_tx: mpsc::UnboundedSender<CleanupRequest>,
    /// Block number for this guard
    block: u64,
    /// Semaphore permit (dropped after cleanup request is sent)
    _permit: OwnedSemaphorePermit,
    /// Whether we hold the fetch lock (`blocks_being_fetched` entry)
    has_fetch_lock: bool,
}

impl FetchGuard {
    /// Creates a new `FetchGuard` from pre-validated components.
    ///
    /// This constructor is called by `CacheManager::try_begin_fetch()` and
    /// `CacheManager::begin_fetch_with_timeout()` after they've completed
    /// all validation steps (cache checks, semaphore acquisition, etc.).
    ///
    /// # Arguments
    /// * `cleanup_tx` - Channel for sending cleanup requests on drop
    /// * `block` - Block number being fetched
    /// * `permit` - Owned semaphore permit (ensures exclusive fetch access)
    /// * `has_fetch_lock` - Whether the fetch lock was acquired
    pub(crate) fn new(
        cleanup_tx: mpsc::UnboundedSender<CleanupRequest>,
        block: u64,
        permit: OwnedSemaphorePermit,
        has_fetch_lock: bool,
    ) -> Self {
        Self { cleanup_tx, block, _permit: permit, has_fetch_lock }
    }

    /// Returns the block number this guard is protecting.
    #[must_use]
    pub fn block(&self) -> u64 {
        self.block
    }
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        // C-CONCUR-2 FIX: Channel-based cleanup instead of task spawning
        //
        // Send cleanup request through channel to dedicated worker task.
        // This is:
        // - Allocation-free: UnboundedSender::send() doesn't allocate
        // - Non-blocking: Never waits for capacity
        // - Batched: Worker processes multiple requests efficiently
        //
        // If channel send fails (worker shutdown), cleanup still happens
        // via the stale inflight cleanup task or will be garbage collected.
        let _ = self
            .cleanup_tx
            .send(CleanupRequest { block: self.block, release_fetch_lock: self.has_fetch_lock });
        // Note: We ignore send errors - if channel is closed,
        // shutdown is in progress and cleanup will be handled
        // by the stale inflight cleanup task.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inflight_fetch_clone() {
        let inflight =
            InflightFetch { semaphore: Arc::new(Semaphore::new(1)), started_at: Instant::now() };

        let cloned = inflight.clone();
        // Verify Arc is cloned (same underlying semaphore)
        assert!(Arc::ptr_eq(&inflight.semaphore, &cloned.semaphore));
    }

    #[test]
    fn test_cleanup_request_copy() {
        let req = CleanupRequest { block: 100, release_fetch_lock: true };
        let copied = req;
        assert_eq!(copied.block, 100);
        assert!(copied.release_fetch_lock);
    }
}
