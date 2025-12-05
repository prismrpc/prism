//! Chain reorganization detection and cache invalidation.
//!
//! This module provides the **core reorg handling logic** for Prism's cache layer. It detects
//! when the upstream blockchain reorganizes (changes history), identifies affected blocks, and
//! invalidates stale cache entries to ensure clients always receive canonical chain data.
//!
//! # Problem Statement
//!
//! Blockchains can "reorganize" (reorg) when a competing fork becomes the canonical chain:
//!
//! ```text
//! Before Reorg (old chain):          After Reorg (new canonical chain):
//!
//!   990 ─── 991 ─── 992 ─── 993        990 ─── 991 ─── 992' ── 993' ── 994'
//!    │       │       │       │          │       │       │        │        │
//!    A       B       C       D          A       B       E        F        G
//!
//! Blocks 992-993 with data C-D        Blocks 992-993 now have data E-F
//! were the canonical chain            Old blocks C-D are INVALID
//! ```
//!
//! Without reorg detection, cached block 992 would return stale data `C` instead of canonical data
//! `E`. This module ensures the cache stays synchronized with the canonical chain.
//!
//! # Reorg Detection Algorithm
//!
//! The `ReorgManager` detects three types of chain updates:
//!
//! ## 1. Forward Progression (Normal Case)
//!
//! ```text
//! Current: block 1000, hash 0xAAA
//! Update:  block 1001, hash 0xBBB
//! Action:  Update tip, no invalidation
//! ```
//!
//! Fast path: just update chain tip atomically.
//!
//! ## 2. Same-Height Reorg
//!
//! ```text
//! Current: block 1000, hash 0xAAA
//! Update:  block 1000, hash 0xBBB  (different hash!)
//! Action:  Reorg detected, invalidate blocks [divergence_point..1000]
//! ```
//!
//! Compare block hashes at same height. Different hash = reorg.
//!
//! ## 3. Rollback (Health Check Detection)
//!
//! ```text
//! Current: block 1000, hash 0xAAA
//! Update:  block 990, hash 0xCCC
//! Action:  Chain rolled back, invalidate blocks [990..1000]
//! ```
//!
//! Tip went backwards. This happens when WebSocket missed reorg events and the
//! health checker detects the discrepancy.
//!
//! # Coalescing Strategy
//!
//! During "reorg storms" (5+ rapid reorgs in succession), naively invalidating cache on every
//! reorg causes **cache thrashing** - blocks are invalidated, refetched, invalidated again, etc.
//!
//! The `ReorgCoalescer` batches rapid updates within a **100ms window**:
//!
//! ```text
//! Time:    0ms    25ms   50ms   75ms   100ms  125ms
//! Reorgs:  R1     R2     R3     R4     R5     R6
//!          │      │      │      │      │      │
//!          └──────┴──────┴──────┘      │      │
//!                 COALESCE              PROCESS│
//!                 (deferred)            (R1-R4)│
//!                                              │
//!                                              └───> PROCESS R6
//! ```
//!
//! - **R1**: Process immediately (window expired)
//! - **R2-R4**: Defer (within 100ms of R1)
//! - **R5**: Process R2-R4 batch + R5 (window expired)
//! - **R6**: Process immediately (window expired)
//!
//! This prevents excessive cache invalidation while ensuring eventual consistency.
//!
//! # Finality Boundaries
//!
//! Not all blocks can be reorganized. The system respects **finality checkpoints**:
//!
//! ## Finality Levels
//!
//! 1. **Finalized Blocks**: Cryptographically committed by consensus (e.g., Ethereum's finality
//!    gadget). These blocks can **NEVER** be reorged. Divergence point will never go below
//!    finalized boundary.
//!
//! 2. **Safe Blocks**: Beyond `safety_depth` (default: 12 blocks from tip). Probabilistically safe
//!    from reorgs in normal operation. Only invalidated for deep reorgs.
//!
//! 3. **Unsafe Blocks**: Within `safety_depth` of tip. Subject to reorgs. Always invalidated during
//!    reorg detection.
//!
//! ```text
//! Block Height:   900         988          1000 (tip)
//!                 │           │            │
//!                 │           │            │
//! Finality:   FINALIZED     SAFE        UNSAFE
//!             (never        (rarely     (frequently
//!              reorgs)       reorgs)     reorgs)
//!                │           │            │
//!                └───────────┴────────────┘
//!                  Invalidation Range During Reorg
//!                  (starts at max(finalized+1, safe_head+1))
//! ```
//!
//! # Coordination with `ChainState`
//!
//! `ReorgManager` is the **primary writer** to `ChainState`, the shared source of truth for
//! chain tip/finalized/hash state. Other components (`CacheManager`, `ScoringEngine`) hold
//! `Arc<ChainState>` references and read from it.
//!
//! ## `ChainState` Updates
//!
//! - `update_tip(block, hash)`: Atomic update of tip + hash (forward progression)
//! - `force_update_tip(block, hash)`: Update even if block number decreased (rollback)
//! - `update_finalized(block)`: Monotonically increase finalized checkpoint
//! - `update_head_hash(hash)`: Update hash without changing block number
//!
//! ## Concurrency Control
//!
//! The `update_lock` mutex prevents races between:
//! - WebSocket block notifications (calling `update_tip`)
//! - Health check rollback detection (calling `update_tip_number_only`)
//!
//! This ensures tip updates, reorg detection, and cache invalidation happen atomically.
//!
//! # Cache Invalidation Flow
//!
//! When a reorg is detected, `ReorgManager` coordinates with `CacheManager`:
//!
//! ```text
//! ReorgManager::update_tip(1000, hash_B)
//!      │
//!      ├─> Current tip: 1000, hash_A
//!      ├─> Same height, different hash → REORG!
//!      │
//!      ├─> find_divergence_point()
//!      │    ├─> Check finalized boundary (e.g., 900)
//!      │    ├─> Check safe_head (tip - 12 = 988)
//!      │    └─> Return divergence = max(901, 989) = 989
//!      │
//!      ├─> Invalidate blocks [989..1000]
//!      │    └─> CacheManager::invalidate_block(block_num)
//!      │         ├─> Remove headers from DashMap
//!      │         ├─> Remove bodies from DashMap
//!      │         ├─> Invalidate log indices
//!      │         └─> Remove from Merkle tree cache
//!      │
//!      └─> Update ChainState to new tip
//! ```
//!
//! # Block Hash History
//!
//! `ReorgManager` maintains a `DashMap<u64, [u8; 32]>` of recent block hashes for validation:
//!
//! - **Size**: Limited to `max_reorg_depth` (default: 100 blocks)
//! - **Purpose**: Validate block hashes without querying upstream
//! - **Cleanup**: Old entries removed when history exceeds configured depth
//!
//! # Concurrency Safety
//!
//! The module is fully thread-safe:
//!
//! - `ChainState`: Atomic operations for tip/finalized/hash
//! - `DashMap`: Lock-free concurrent hash map for block history
//! - `update_lock`: Mutex for coordinating WebSocket vs health check updates
//! - `ReorgCoalescer`: Atomic CAS loop for claiming processing rights
//!
//! Multiple concurrent calls to `update_tip()` serialize via `update_lock`, preventing
//! race conditions where two reorgs could corrupt state.
//!
//! # Example: Reorg Handling
//!
//! ```rust,no_run
//! use prism_core::{
//!     cache::{
//!         reorg_manager::{ReorgManager, ReorgManagerConfig},
//!         CacheManager, CacheManagerConfig,
//!     },
//!     chain::ChainState,
//! };
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Initialize components
//! let chain_state = Arc::new(ChainState::new());
//! let cache_manager =
//!     Arc::new(CacheManager::new(&CacheManagerConfig::default(), chain_state.clone())?);
//!
//! let reorg_config = ReorgManagerConfig {
//!     safety_depth: 12,
//!     max_reorg_depth: 100,
//!     reorg_detection_threshold: 2,
//!     coalesce_window_ms: 100,
//! };
//!
//! let reorg_manager = ReorgManager::new(reorg_config, chain_state.clone(), cache_manager.clone());
//!
//! // Process block updates from WebSocket
//! reorg_manager.update_tip(1000, [0xAA; 32]).await;
//! reorg_manager.update_tip(1001, [0xBB; 32]).await;
//!
//! // Reorg detected: same height, different hash
//! reorg_manager.update_tip(1001, [0xCC; 32]).await;
//! // ^ This invalidates affected blocks and updates chain state
//!
//! // Update finalized checkpoint
//! reorg_manager.update_finalized_block(990).await;
//!
//! // Query reorg stats
//! let stats = reorg_manager.get_reorg_stats();
//! println!("Current tip: {}", stats.current_tip);
//! println!("Safe head: {}", stats.safe_head);
//! println!("Unsafe window: {} blocks", stats.unsafe_window_size);
//! # Ok(())
//! # }
//! ```
//!
//! # Configuration
//!
//! - `safety_depth`: Blocks beyond this depth from tip are considered safe (default: 12)
//! - `max_reorg_depth`: Maximum blocks to search backwards for divergence (default: 100)
//! - `reorg_detection_threshold`: Consecutive reorgs before alerting (currently unused)
//! - `coalesce_window_ms`: Time window for batching rapid reorgs (default: 100ms)
//!
//! # Metrics
//!
//! Use `get_reorg_stats()` to monitor reorg management:
//!
//! - `current_tip`: Latest block number
//! - `safe_head`: Tip minus `safety_depth`
//! - `unsafe_window_size`: Number of blocks in unsafe window
//! - `history_size`: Number of block hashes stored
//! - `max_reorg_depth`: Configured maximum reorg depth

use crate::{
    cache::{
        types::{FinalityStatus, ReorgInfo},
        CacheManager,
    },
    chain::ChainState,
};
use dashmap::DashMap;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Configuration for chain reorganization management.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReorgManagerConfig {
    /// Number of blocks to wait before considering a block safe from reorgs.
    pub safety_depth: u64,
    /// Maximum depth to search backwards when detecting reorg divergence points.
    pub max_reorg_depth: u64,
    /// Reorg coalescing window in milliseconds (default: 100ms).
    /// Multiple reorgs within this window are batched to reduce cache thrashing.
    #[serde(default = "default_coalesce_window_ms")]
    pub coalesce_window_ms: u64,
}

fn default_coalesce_window_ms() -> u64 {
    100
}

impl Default for ReorgManagerConfig {
    fn default() -> Self {
        Self { safety_depth: 12, max_reorg_depth: 100, coalesce_window_ms: 100 }
    }
}

/// Reorg coalescing to handle reorg storms.
///
/// Batches multiple reorgs within a short time window to prevent cache thrashing
/// during rapid reorg events (e.g., 5+ reorgs in quick succession).
///
/// Uses monotonic clock (`Instant`) instead of `SystemTime` to avoid issues
/// with system clock adjustments.
struct ReorgCoalescer {
    /// Pending reorg update waiting to be processed later (`block_number`, `block_hash`).
    /// Stores deferred reorg events for batch processing.
    pending_update: Mutex<Option<(u64, [u8; 32])>>,
    /// Minimum time between processing reorgs
    coalesce_window: Duration,
    /// Reference point for monotonic time calculations
    start_instant: Instant,
    /// Last time we processed a reorg (milliseconds since `start_instant`)
    last_processed_ms: AtomicU64,
}

impl ReorgCoalescer {
    /// Creates a new reorg coalescer with the specified window.
    pub fn new(coalesce_window_ms: u64) -> Self {
        Self {
            pending_update: Mutex::new(None),
            coalesce_window: Duration::from_millis(coalesce_window_ms),
            start_instant: Instant::now(),
            last_processed_ms: AtomicU64::new(0),
        }
    }

    /// Records a potential reorg. Returns true if we should process now.
    ///
    /// Returns `true` if enough time has elapsed since the last processing (process immediately).
    /// Returns `false` if within the coalesce window (deferred for later batch processing).
    ///
    /// Uses compare-exchange loop to prevent races where multiple concurrent calls both
    /// attempt to process. Only one caller successfully claims processing via atomic CAS.
    ///
    /// # Memory Ordering
    ///
    /// Uses compare-exchange loop to atomically check and update `last_processed_ms`:
    /// - `Acquire` on load: Synchronizes with previous `Release` store
    /// - `AcqRel` on success: Publishes timestamp and reads latest state
    /// - `Acquire` on failure: Ensures fresh read before retry
    ///
    /// The loop prevents TOCTOU race where two concurrent callers both see they're
    /// outside the window and both return true. Only one caller wins the CAS.
    ///
    /// Uses `compare_exchange_weak` for better performance on LL/SC architectures
    /// (ARM, RISC-V) - the loop naturally handles spurious failures.
    pub async fn record(&self, block_number: u64, block_hash: [u8; 32]) -> bool {
        // Use monotonic time: milliseconds since coalescer was created
        // This is immune to system clock changes (NTP, manual adjustments)
        let now_ms = u64::try_from(self.start_instant.elapsed().as_millis()).unwrap_or(u64::MAX);
        let window_ms = u64::try_from(self.coalesce_window.as_millis()).unwrap_or(u64::MAX);

        // Use compare-exchange loop to atomically check and update last_processed_ms.
        // This prevents TOCTOU race where two concurrent calls both see they're outside
        // the window and both return true.
        loop {
            let last_ms = self.last_processed_ms.load(Ordering::Acquire);

            // If within coalesce window, defer this update for batch processing later
            if now_ms.saturating_sub(last_ms) < window_ms {
                *self.pending_update.lock().await = Some((block_number, block_hash));
                return false; // Caller should NOT process now
            }

            // Outside window: try to atomically claim the right to process immediately
            if self
                .last_processed_ms
                .compare_exchange_weak(last_ms, now_ms, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                *self.pending_update.lock().await = None;
                return true;
            }
        }
    }

    /// Gets any coalesced reorg that should now be processed.
    pub async fn take_pending(&self) -> Option<(u64, [u8; 32])> {
        self.pending_update.lock().await.take()
    }
}

/// Manages chain reorganizations and cache invalidation.
///
/// Uses shared `ChainState` for unified chain tip tracking across all components.
/// Maintains a history of block hashes to detect when the chain reorganizes.
/// When a reorg is detected, invalidates affected blocks in the cache.
///
/// Thread-safe: uses shared `ChainState`, `DashMap` for block history, and
/// a coordination lock to serialize chain state updates from WebSocket and
/// health check paths.
pub struct ReorgManager {
    config: ReorgManagerConfig,

    /// Shared chain state - single source of truth for tip/finalized/hash.
    ///
    /// `ReorgManager` is the **primary writer** to `ChainState`. It updates:
    /// - Chain tip (block number + hash) via `update_tip()` and `force_update_tip()`
    /// - Finalized block via `update_finalized()`
    /// - Head hash during reorgs via `update_head_hash()`
    ///
    /// Other components (`CacheManager`, `ScoringEngine`) hold the same `Arc<ChainState>`
    /// reference and read from it, but writes are coordinated through `ReorgManager`
    /// to ensure consistency during reorg handling.
    ///
    /// See [`crate::chain`] module docs for the ownership pattern explanation.
    chain_state: Arc<ChainState>,

    block_hash_history: Arc<DashMap<u64, [u8; 32]>>,

    cache_manager: Arc<CacheManager>,

    /// Coordination lock to prevent races between WebSocket updates and
    /// health check rollback detection. Ensures that tip updates, reorg
    /// detection, and cache invalidation happen atomically.
    update_lock: Mutex<()>,

    /// Reorg coalescer for handling reorg storms
    coalescer: ReorgCoalescer,
}

impl ReorgManager {
    #[must_use]
    pub fn new(
        config: ReorgManagerConfig,
        chain_state: Arc<ChainState>,
        cache_manager: Arc<CacheManager>,
    ) -> Self {
        let coalescer = ReorgCoalescer::new(config.coalesce_window_ms);
        Self {
            config,
            chain_state,
            block_hash_history: Arc::new(DashMap::new()),
            cache_manager,
            update_lock: Mutex::new(()),
            coalescer,
        }
    }

    /// Updates the current chain tip with a new block.
    ///
    /// Handles three cases:
    /// 1. **Newer block**: Updates tip atomically (fast path, no reorg check)
    /// 2. **Same height, different hash**: Detects reorg and invalidates cache
    /// 3. **Older block**: Just updates history (for late-arriving notifications)
    ///
    /// Automatically cleans up block history older than `max_reorg_depth`.
    ///
    /// # Reorg Coalescing
    ///
    /// During reorg storms (multiple rapid reorgs), uses the coalescer to batch
    /// updates within a time window (default: 100ms). This prevents cache thrashing
    /// and reduces lock contention during high reorg activity.
    ///
    /// # Concurrency
    ///
    /// Acquires the coordination lock to prevent races between WebSocket updates
    /// and health check rollback detection.
    ///
    /// # Async Behavior and Lock Holding
    ///
    /// This method holds `update_lock` for the entire operation, including during:
    /// - `ChainState::update_tip()` await (fast, lock-free Arc swap)
    /// - `handle_reorg()` await (may be slower during cache invalidation)
    /// - `handle_rollback()` await (may be slower during cache invalidation)
    ///
    /// The lock serializes concurrent updates from WebSocket and health check paths.
    /// During cache invalidation, other callers block on the mutex.
    pub async fn update_tip(&self, block_number: u64, block_hash: [u8; 32]) {
        // Acquire coordination lock to prevent races with health check updates
        let _guard = self.update_lock.lock().await;
        let current_tip = self.chain_state.current_tip();

        match block_number.cmp(&current_tip) {
            std::cmp::Ordering::Greater => {
                // Fast path: newer block, no reorg check needed
                self.chain_state.update_tip(block_number, block_hash).await;
                self.block_hash_history.insert(block_number, block_hash);
            }
            std::cmp::Ordering::Equal => {
                // Same height: check for reorg (different hash = reorg)
                let current_head_hash = self.chain_state.current_head_hash();

                if current_head_hash != block_hash && current_tip > 0 {
                    // Use coalescer to decide if we should process now
                    let should_process = self.coalescer.record(block_number, block_hash).await;

                    if should_process {
                        info!(
                            block = block_number,
                            old_hash = ?current_head_hash,
                            new_hash = ?block_hash,
                            "reorg detected at current tip - processing immediately"
                        );
                        self.handle_reorg(block_number, current_head_hash, block_hash).await;
                    } else {
                        info!(
                            block = block_number,
                            old_hash = ?current_head_hash,
                            new_hash = ?block_hash,
                            "reorg detected - coalescing with pending updates"
                        );
                    }
                } else {
                    // Process any pending coalesced reorgs
                    if let Some((pending_num, pending_hash)) = self.coalescer.take_pending().await {
                        let old_hash = self.chain_state.current_head_hash();
                        info!(
                            block = pending_num,
                            old_hash = ?old_hash,
                            new_hash = ?pending_hash,
                            "processing coalesced reorg"
                        );
                        self.handle_reorg(pending_num, old_hash, pending_hash).await;
                    }
                }

                // Always update hash for same-height blocks (handles reorg case)
                self.chain_state.update_head_hash(block_hash).await;
                self.block_hash_history.insert(block_number, block_hash);
            }
            std::cmp::Ordering::Less => {
                // Block number went backwards - this indicates a potential reorg
                // detected via health check when WebSocket missed the event
                let rollback_depth = current_tip - block_number;

                if rollback_depth > 0 {
                    warn!(
                        current_tip = current_tip,
                        new_block = block_number,
                        rollback_depth = rollback_depth,
                        "chain rollback detected, invalidating affected blocks"
                    );

                    // Invalidate all blocks from new tip to old tip
                    self.handle_rollback(block_number, current_tip).await;

                    // Force update chain state to the new (lower) tip - requires force since tip is
                    // going backwards
                    self.chain_state.force_update_tip(block_number, block_hash).await;
                }

                self.block_hash_history.insert(block_number, block_hash);
            }
        }

        // Cleanup old history entries
        self.cleanup_old_history(block_number);
    }

    /// Removes block history entries older than `max_reorg_depth`.
    fn cleanup_old_history(&self, current_block: u64) {
        let max_entries = usize::try_from(self.config.max_reorg_depth).unwrap_or(100);
        if self.block_hash_history.len() > max_entries {
            let cutoff = current_block.saturating_sub(self.config.max_reorg_depth);
            let to_remove: Vec<u64> = self
                .block_hash_history
                .iter()
                .filter(|entry| *entry.key() < cutoff)
                .map(|entry| *entry.key())
                .collect();

            for num in to_remove {
                self.block_hash_history.remove(&num);
            }
        }
    }

    /// Handles a chain rollback (tip went backwards) by invalidating affected blocks.
    ///
    /// Called when health checks detect the chain tip is lower than our stored tip,
    /// indicating a reorg happened that WebSocket missed.
    ///
    /// Invalidates ALL blocks from `new_tip` through `old_tip` because:
    /// - The new tip block itself has a DIFFERENT hash after the reorg
    /// - Any cached data for the new tip block is stale and must be refreshed
    /// - This ensures clients always get the canonical chain's data
    async fn handle_rollback(&self, new_tip: u64, old_tip: u64) {
        // Invalidate from new_tip through old_tip (inclusive)
        // The new tip block itself needs invalidation because its hash changed during reorg
        let invalidate_from = new_tip;

        if invalidate_from > old_tip {
            // Safety check: this condition should never occur in normal operation
            // since this function is only called when new_tip < old_tip (rollback detected)
            return;
        }

        info!(
            new_tip = new_tip,
            old_tip = old_tip,
            invalidate_from = invalidate_from,
            blocks_to_invalidate = old_tip.saturating_sub(invalidate_from) + 1,
            "handling chain rollback"
        );

        // INCLUSIVE: new_tip is invalidated because its hash changed during the reorg.
        // This is intentional - the block at new_tip now has a DIFFERENT hash than before.
        // Invalidate from new_tip through old_tip (inclusive)
        for block_num in invalidate_from..=old_tip {
            self.invalidate_block_internal(block_num).await;
            // Also remove from history since they're no longer valid
            self.block_hash_history.remove(&block_num);
        }

        info!(
            invalidated_from = invalidate_from,
            invalidated_to = old_tip,
            "rollback cache invalidation complete"
        );
    }

    /// Handles a detected chain reorganization by invalidating affected blocks.
    async fn handle_reorg(&self, block_number: u64, old_hash: [u8; 32], new_hash: [u8; 32]) {
        info!(
            block = block_number,
            old_hash = ?old_hash,
            new_hash = ?new_hash,
            "handling reorg"
        );

        let divergence_block =
            self.calculate_invalidation_boundary(block_number, old_hash, new_hash);

        if divergence_block > 0 {
            info!(divergence_block = divergence_block, "calculated invalidation boundary");

            let mut reorg_info = ReorgInfo::new(block_number, block_number, divergence_block);

            for block_num in divergence_block..=block_number {
                self.invalidate_block_internal(block_num).await;
                reorg_info.old_blocks.push(block_num);
            }

            info!(
                invalidated_blocks = reorg_info.old_blocks.len(),
                from_block = divergence_block,
                to_block = block_number,
                "reorg completed"
            );
        } else {
            warn!("could not determine invalidation boundary");
        }
    }

    /// Calculates the invalidation boundary based on finality rules.
    ///
    /// **Note**: This does NOT search for the actual divergence point by comparing
    /// block hashes. Instead, it uses finality state to determine the conservative
    /// invalidation boundary:
    /// - **Finalized blocks**: Never invalidated (cryptographically committed)
    /// - **Safe blocks**: Only invalidated if reorg is deeper than `safety_depth`
    /// - **Unsafe blocks**: Always invalidated during any reorg
    ///
    /// Returns the first block number that should be invalidated.
    fn calculate_invalidation_boundary(
        &self,
        block_number: u64,
        _old_hash: [u8; 32],
        _new_hash: [u8; 32],
    ) -> u64 {
        let finalized_block = self.chain_state.finalized_block();
        let safe_head = self.chain_state.safe_head(self.config.safety_depth);
        let max_reorg_start = block_number.saturating_sub(self.config.max_reorg_depth);

        // Determine the lower bound for invalidation based on finality
        // Priority: finalized > safe_head > max_reorg_depth
        let lower_bound = if finalized_block > 0 {
            // Finalized blocks can NEVER be reorged - this is our absolute floor
            finalized_block + 1
        } else if safe_head > 0 {
            // Without finalized checkpoint, use safe_head as conservative bound
            safe_head + 1
        } else {
            // Fallback to max_reorg_depth limit
            max_reorg_start
        };

        // The divergence point is the maximum of our lower bound and max_reorg_start
        // This ensures we don't invalidate beyond max_reorg_depth even for deep reorgs
        let divergence = lower_bound.max(max_reorg_start);

        info!(
            block_number = block_number,
            finalized_block = finalized_block,
            safe_head = safe_head,
            invalidation_boundary = divergence,
            blocks_to_invalidate = block_number.saturating_sub(divergence) + 1,
            "calculated invalidation boundary using finality state"
        );

        divergence
    }

    async fn invalidate_block_internal(&self, block_number: u64) {
        self.cache_manager.invalidate_block(block_number).await;
    }

    /// Returns the safe head block number (tip minus `safety_depth`).
    ///
    /// Blocks at or below this height are considered safe from reorgs.
    /// Uses lock-free read - never blocks.
    #[inline]
    #[must_use]
    pub fn get_safe_head(&self) -> u64 {
        self.chain_state.safe_head(self.config.safety_depth)
    }

    /// Checks if a block is in the unsafe window (newer than safe head).
    /// Uses lock-free read - never blocks.
    #[inline]
    #[must_use]
    pub fn is_unsafe_block(&self, block_number: u64) -> bool {
        block_number > self.get_safe_head()
    }

    /// Returns the current chain tip block number.
    /// Uses lock-free read - never blocks.
    #[inline]
    #[must_use]
    pub fn get_current_tip(&self) -> u64 {
        self.chain_state.current_tip()
    }

    /// Returns the current head block hash.
    /// Uses lock-free read - never blocks.
    #[inline]
    #[must_use]
    pub fn get_current_head_hash(&self) -> [u8; 32] {
        self.chain_state.current_head_hash()
    }

    /// Validates a block hash against stored history.
    ///
    /// Checks the block hash against our stored history. For unsafe blocks,
    /// also checks against the block history if available.
    #[must_use]
    pub fn validate_block_hash(&self, block_number: u64, block_hash: [u8; 32]) -> bool {
        // First check block history (works for both safe and unsafe blocks)
        if let Some(stored_hash) = self.block_hash_history.get(&block_number) {
            return block_hash == *stored_hash;
        }

        // For current tip, check against chain state
        if block_number == self.get_current_tip() {
            let current_hash = self.get_current_head_hash();
            return block_hash == current_hash;
        }

        // No validation data available
        tracing::trace!(block = block_number, "no validation data available for block");
        false
    }
    /// Returns current reorg management statistics.
    #[must_use]
    pub fn get_reorg_stats(&self) -> ReorgStats {
        let tip = self.chain_state.current_tip();
        let safe_head = self.chain_state.safe_head(self.config.safety_depth);

        ReorgStats {
            current_tip: tip,
            safe_head,
            unsafe_window_size: tip.saturating_sub(safe_head),
            history_size: self.block_hash_history.len(),
            max_reorg_depth: self.config.max_reorg_depth,
        }
    }

    /// Updates the finalized block number.
    ///
    /// Called when upstreams report a new finalized block. Only updates if the new
    /// finalized block is higher than the current one (monotonic increase).
    pub async fn update_finalized_block(&self, finalized_block: u64) {
        if self.chain_state.update_finalized(finalized_block).await {
            info!(finalized_block = finalized_block, "updated finalized block");
        }
    }

    /// Returns the current finalized block number.
    #[must_use]
    pub fn get_finalized_block(&self) -> u64 {
        self.chain_state.finalized_block()
    }

    /// Updates only the chain tip block number without changing the hash.
    ///
    /// Used by health checker when the real hash is not available.
    /// This prevents storing placeholder [0u8; 32] hashes that would corrupt
    /// the block hash history.
    ///
    /// # Arguments
    ///
    /// * `block_number` - The new block number from the health check
    ///
    /// # Concurrency
    ///
    /// Acquires the coordination lock to prevent races between health check
    /// updates and WebSocket block notifications.
    pub async fn update_tip_number_only(&self, block_number: u64) {
        // Acquire coordination lock to prevent races with WebSocket updates
        let _guard = self.update_lock.lock().await;
        let current_tip = self.chain_state.current_tip();

        if block_number > current_tip {
            // Forward progression - just update the number
            let _ = self.chain_state.update_tip_simple(block_number).await;
        } else if block_number < current_tip {
            // Rollback - need to invalidate but preserve existing hash for new_tip if we have it
            let rollback_depth = current_tip - block_number;
            if rollback_depth > 0 {
                warn!(
                    current_tip = current_tip,
                    new_block = block_number,
                    rollback_depth = rollback_depth,
                    "chain rollback detected via health check (no hash available)"
                );
                self.handle_rollback(block_number, current_tip).await;
                // Force update tip number only, preserve whatever hash is in history
                self.chain_state.force_update_tip_simple(block_number).await;
            }
        }
        // Equal case: no action needed
    }

    /// Classifies the finality status of a block.
    ///
    /// Determines whether a block is finalized, safe, unsafe, or unknown based on:
    /// - Finalized checkpoint from upstreams
    /// - Safety depth configuration
    /// - Current chain tip
    ///
    /// # Arguments
    /// * `block_number` - The block number to classify
    ///
    /// # Returns
    /// The finality status of the block
    #[must_use]
    pub fn classify_block_finality(&self, block_number: u64) -> FinalityStatus {
        let finalized_block = self.chain_state.finalized_block();
        let current_tip = self.chain_state.current_tip();

        if current_tip == 0 {
            // No chain data available yet
            return FinalityStatus::Unknown;
        }

        // Check if block is finalized
        if finalized_block > 0 && block_number <= finalized_block {
            return FinalityStatus::Finalized;
        }

        // Check if block is in safe zone (beyond safety_depth)
        let safe_head = self.chain_state.safe_head(self.config.safety_depth);
        if block_number <= safe_head {
            return FinalityStatus::Safe;
        }

        // Check if block is in unsafe zone (within safety_depth of tip)
        if block_number <= current_tip {
            return FinalityStatus::Unsafe;
        }

        // Block is in the future - unknown
        FinalityStatus::Unknown
    }
}

/// Statistics snapshot for reorg management.
#[derive(Debug, Clone)]
pub struct ReorgStats {
    /// Current chain tip block number.
    pub current_tip: u64,
    /// Safe head block number (blocks below this are safe from reorgs).
    pub safe_head: u64,
    /// Number of blocks in the unsafe window.
    pub unsafe_window_size: u64,
    /// Number of blocks tracked in the history.
    pub history_size: usize,
    /// Maximum reorg depth configured.
    pub max_reorg_depth: u64,
}

#[cfg(test)]
#[allow(clippy::doc_markdown, clippy::uninlined_format_args)]
mod tests {
    use super::*;
    use crate::cache::{
        types::{BlockBody, BlockHeader},
        CacheManagerConfig,
    };

    // Test Helpers

    /// Creates a test block header for a given block number.
    fn make_test_header(block_number: u64) -> BlockHeader {
        #[allow(clippy::cast_possible_truncation)]
        let hash_byte = block_number as u8;
        BlockHeader {
            hash: [hash_byte; 32],
            number: block_number,
            parent_hash: [hash_byte.wrapping_sub(1); 32],
            timestamp: 1_700_000_000 + block_number,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [0xAA; 20],
            extra_data: Arc::new(vec![]),
            logs_bloom: Arc::new(vec![0; 256]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            receipts_root: [0; 32],
        }
    }

    /// Creates a test block body for a given block hash.
    fn make_test_body(block_hash: [u8; 32]) -> BlockBody {
        BlockBody { hash: block_hash, transactions: vec![] }
    }

    /// Creates a ReorgManager with associated cache for testing.
    /// Uses `coalesce_window_ms: 0` to disable coalescing so updates are processed immediately.
    #[allow(clippy::unused_async)]
    async fn setup_reorg_manager() -> (ReorgManager, Arc<CacheManager>) {
        let config = ReorgManagerConfig {
            // Disable coalescing for tests - ensures each update_tip is processed immediately
            coalesce_window_ms: 0,
            ..ReorgManagerConfig::default()
        };
        let cache_config = CacheManagerConfig::default();
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let cache_manager = Arc::new(
            CacheManager::new(&cache_config, chain_state.clone()).expect("valid test cache config"),
        );
        let reorg_manager = ReorgManager::new(config, chain_state, cache_manager.clone());
        (reorg_manager, cache_manager)
    }

    // Observable Behavior Tests: Cache Invalidation
    // These tests verify that after a reorg, cached data is actually removed.

    /// After a reorg at block N, cached block headers at N and above should be invalidated.
    #[tokio::test]
    async fn test_reorg_invalidates_cached_block_headers() {
        let (reorg_manager, cache_manager) = setup_reorg_manager().await;

        // Populate cache with block headers 995-1000
        for block_num in 995..=1000_u64 {
            let header = make_test_header(block_num);
            cache_manager.insert_header(header).await;
        }

        // Verify blocks are cached
        assert!(
            cache_manager.get_header_by_number(998).is_some(),
            "Block 998 header should be cached before reorg"
        );
        assert!(
            cache_manager.get_header_by_number(1000).is_some(),
            "Block 1000 header should be cached before reorg"
        );

        // Set tip to 1000 and then trigger a reorg by receiving same height with different hash
        reorg_manager.update_tip(1000, [232u8; 32]).await;
        reorg_manager.update_tip(1000, [0xFF; 32]).await; // Different hash = reorg

        // After reorg, affected blocks should be invalidated from cache
        // The reorg handler invalidates from divergence point to current tip
        assert!(
            cache_manager.get_header_by_number(1000).is_none(),
            "Block 1000 should be invalidated after reorg"
        );
    }

    /// After a rollback from tip 1000 to 990, blocks 990-1000 should be invalidated.
    #[tokio::test]
    async fn test_rollback_invalidates_cached_blocks_in_range() {
        let (reorg_manager, cache_manager) = setup_reorg_manager().await;

        // Populate cache with block headers and bodies for blocks 985-1000
        for block_num in 985..=1000_u64 {
            let header = make_test_header(block_num);
            let body = make_test_body(header.hash);
            cache_manager.insert_header(header).await;
            cache_manager.insert_body(body).await;
        }

        // Set initial tip to 1000
        reorg_manager.update_tip(1000, [232u8; 32]).await;

        // Verify blocks in rollback range are cached
        assert!(cache_manager.get_header_by_number(990).is_some());
        assert!(cache_manager.get_header_by_number(995).is_some());
        assert!(cache_manager.get_header_by_number(1000).is_some());

        // Verify block outside rollback range is cached
        assert!(cache_manager.get_header_by_number(985).is_some());

        // Trigger rollback to block 990
        reorg_manager.update_tip(990, [0xBB; 32]).await;

        // After rollback: blocks 990-1000 should be invalidated
        assert!(
            cache_manager.get_header_by_number(991).is_none(),
            "Block 991 should be invalidated after rollback"
        );
        assert!(
            cache_manager.get_header_by_number(995).is_none(),
            "Block 995 should be invalidated after rollback"
        );
        assert!(
            cache_manager.get_header_by_number(1000).is_none(),
            "Block 1000 should be invalidated after rollback"
        );

        // Block 990 is also invalidated because its hash changed during the reorg
        assert!(
            cache_manager.get_header_by_number(990).is_none(),
            "Block 990 (new tip) should be invalidated as its hash changed"
        );

        // Block 985 should NOT be invalidated (outside rollback range)
        assert!(
            cache_manager.get_header_by_number(985).is_some(),
            "Block 985 should NOT be invalidated (before rollback range)"
        );
    }

    /// Blocks marked as finalized should never be invalidated during reorg.
    #[tokio::test]
    async fn test_finalized_blocks_not_invalidated_during_reorg() {
        let config = ReorgManagerConfig::default();
        let cache_config = CacheManagerConfig::default();
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let cache_manager = Arc::new(
            CacheManager::new(&cache_config, chain_state.clone()).expect("valid test cache config"),
        );
        let reorg_manager = ReorgManager::new(config, chain_state, cache_manager.clone());

        // Set tip to 1000
        reorg_manager.update_tip(1000, [232u8; 32]).await;

        // Mark block 950 as finalized
        reorg_manager.update_finalized_block(950).await;

        // Populate cache with blocks spanning finalized and non-finalized ranges
        for block_num in 940..=1000_u64 {
            let header = make_test_header(block_num);
            cache_manager.insert_header(header).await;
        }

        // Verify finalized block is cached
        assert!(cache_manager.get_header_by_number(945).is_some());

        // Trigger a reorg at block 1000
        reorg_manager.update_tip(1000, [0xFF; 32]).await;

        // Finalized blocks should still be in cache
        // (The reorg divergence point calculation respects finalized boundary)
        assert!(
            cache_manager.get_header_by_number(945).is_some(),
            "Finalized block 945 should NOT be invalidated"
        );
    }

    // Observable Behavior Tests: Tip Tracking

    /// Chain tip advances correctly when receiving new blocks.
    #[tokio::test]
    async fn test_tip_advances_with_new_blocks() {
        let (reorg_manager, _) = setup_reorg_manager().await;

        // Initial state: no tip
        assert_eq!(reorg_manager.get_current_tip(), 0);

        // Receive block 100
        reorg_manager.update_tip(100, [100u8; 32]).await;
        assert_eq!(reorg_manager.get_current_tip(), 100);

        // Receive block 101
        reorg_manager.update_tip(101, [101u8; 32]).await;
        assert_eq!(reorg_manager.get_current_tip(), 101);

        // Receive block 105 (skip some blocks - still advances)
        reorg_manager.update_tip(105, [105u8; 32]).await;
        assert_eq!(reorg_manager.get_current_tip(), 105);
    }

    /// Chain tip and hash update correctly during rollback.
    #[tokio::test]
    async fn test_rollback_updates_tip_and_hash() {
        let (reorg_manager, _) = setup_reorg_manager().await;

        // Set tip to 1000
        reorg_manager.update_tip(1000, [232u8; 32]).await;
        assert_eq!(reorg_manager.get_current_tip(), 1000);

        // Rollback to 990 with new hash
        let new_hash = [0xBB; 32];
        reorg_manager.update_tip(990, new_hash).await;

        // Tip should be 990
        assert_eq!(reorg_manager.get_current_tip(), 990);

        // Hash should be the new hash
        assert_eq!(reorg_manager.get_current_head_hash(), new_hash);
    }

    // Observable Behavior Tests: Finality Classification
    // These tests verify block classification from the user's perspective.

    /// Blocks are classified correctly based on their position relative to tip and finalized.
    #[tokio::test]
    async fn test_block_finality_classification() {
        let (reorg_manager, _) = setup_reorg_manager().await;

        // Before any tip is set, all blocks are Unknown
        assert_eq!(reorg_manager.classify_block_finality(100), FinalityStatus::Unknown);

        // Set tip to 1000
        reorg_manager.update_tip(1000, [232u8; 32]).await;

        // Future blocks are Unknown
        assert_eq!(
            reorg_manager.classify_block_finality(1001),
            FinalityStatus::Unknown,
            "Future blocks should be Unknown"
        );

        // Recent blocks (within default safety_depth=12) are Unsafe
        assert_eq!(
            reorg_manager.classify_block_finality(1000),
            FinalityStatus::Unsafe,
            "Current tip should be Unsafe"
        );
        assert_eq!(
            reorg_manager.classify_block_finality(995),
            FinalityStatus::Unsafe,
            "Block within safety_depth should be Unsafe"
        );

        // Older blocks are Safe
        assert_eq!(
            reorg_manager.classify_block_finality(900),
            FinalityStatus::Safe,
            "Old block should be Safe"
        );

        // Set finalized block
        reorg_manager.update_finalized_block(850).await;

        // Blocks at or before finalized are Finalized
        assert_eq!(
            reorg_manager.classify_block_finality(850),
            FinalityStatus::Finalized,
            "Block at finalized height should be Finalized"
        );
        assert_eq!(
            reorg_manager.classify_block_finality(800),
            FinalityStatus::Finalized,
            "Block before finalized should be Finalized"
        );
    }

    // Observable Behavior Tests: Finalized Block Management

    /// Finalized block only increases (monotonic).
    #[tokio::test]
    async fn test_finalized_block_only_increases() {
        let (reorg_manager, _) = setup_reorg_manager().await;

        // Set tip high enough
        reorg_manager.update_tip(500, [1u8; 32]).await;

        // Initial finalized is 0
        assert_eq!(reorg_manager.get_finalized_block(), 0);

        // Set to 100
        reorg_manager.update_finalized_block(100).await;
        assert_eq!(reorg_manager.get_finalized_block(), 100);

        // Try to decrease (should be ignored)
        reorg_manager.update_finalized_block(50).await;
        assert_eq!(reorg_manager.get_finalized_block(), 100, "Finalized should not decrease");

        // Increase to 200
        reorg_manager.update_finalized_block(200).await;
        assert_eq!(reorg_manager.get_finalized_block(), 200);
    }

    /// Finalized block cannot exceed current tip.
    #[tokio::test]
    async fn test_finalized_cannot_exceed_tip() {
        let (reorg_manager, _) = setup_reorg_manager().await;

        // Set tip to 100
        reorg_manager.update_tip(100, [1u8; 32]).await;

        // Try to set finalized above tip
        reorg_manager.update_finalized_block(200).await;

        // Should be rejected (finalized stays at 0)
        assert_eq!(reorg_manager.get_finalized_block(), 0, "Finalized should not exceed tip");

        // Set to valid value
        reorg_manager.update_finalized_block(50).await;
        assert_eq!(reorg_manager.get_finalized_block(), 50);
    }

    // Observable Behavior Tests: Hash Validation

    /// Block hashes can be validated against stored history.
    #[tokio::test]
    async fn test_block_hash_validation() {
        let (reorg_manager, _) = setup_reorg_manager().await;

        // Add blocks with known hashes
        reorg_manager.update_tip(100, [100u8; 32]).await;
        reorg_manager.update_tip(101, [101u8; 32]).await;
        reorg_manager.update_tip(102, [102u8; 32]).await;

        // Valid hashes should validate
        assert!(
            reorg_manager.validate_block_hash(100, [100u8; 32]),
            "Correct hash should validate"
        );
        assert!(
            reorg_manager.validate_block_hash(101, [101u8; 32]),
            "Correct hash should validate"
        );

        // Invalid hashes should not validate
        assert!(
            !reorg_manager.validate_block_hash(100, [0xFF; 32]),
            "Wrong hash should not validate"
        );

        // Unknown blocks should not validate
        assert!(
            !reorg_manager.validate_block_hash(999, [0xFF; 32]),
            "Unknown block should not validate"
        );
    }

    // Observable Behavior Tests: Health Check Tip-Only Updates

    /// update_tip_number_only advances tip without storing placeholder hashes.
    #[tokio::test]
    async fn test_tip_number_only_update_no_placeholder_hash() {
        let (reorg_manager, _) = setup_reorg_manager().await;

        // Set initial tip with real hash
        reorg_manager.update_tip(100, [0xAB; 32]).await;
        assert_eq!(reorg_manager.get_current_tip(), 100);
        assert_eq!(reorg_manager.get_current_head_hash(), [0xAB; 32]);

        // Use tip-number-only update (simulates health checker)
        reorg_manager.update_tip_number_only(101).await;

        // Tip should advance
        assert_eq!(reorg_manager.get_current_tip(), 101);

        // Hash should NOT be overwritten with placeholder
        // (It keeps whatever was there, which may be stale but not [0u8; 32])
        let hash = reorg_manager.get_current_head_hash();
        assert_ne!(hash, [0u8; 32], "Hash should not be overwritten with placeholder");
    }

    /// update_tip_number_only triggers rollback invalidation when tip goes backward.
    #[tokio::test]
    async fn test_tip_number_only_triggers_rollback() {
        let (reorg_manager, cache_manager) = setup_reorg_manager().await;

        // Populate cache and set tip to 1000
        for block_num in 990..=1000_u64 {
            let header = make_test_header(block_num);
            cache_manager.insert_header(header).await;
            #[allow(clippy::cast_possible_truncation)]
            reorg_manager.update_tip(block_num, [block_num as u8; 32]).await;
        }

        assert_eq!(reorg_manager.get_current_tip(), 1000);
        assert!(cache_manager.get_header_by_number(995).is_some());

        // Simulate health checker detecting rollback (tip-only, no hash)
        reorg_manager.update_tip_number_only(990).await;

        // Tip should be updated
        assert_eq!(reorg_manager.get_current_tip(), 990);

        // Blocks 991-1000 should be invalidated
        assert!(
            cache_manager.get_header_by_number(995).is_none(),
            "Block 995 should be invalidated after rollback"
        );
        assert!(
            cache_manager.get_header_by_number(1000).is_none(),
            "Block 1000 should be invalidated after rollback"
        );
    }

    // Edge Case Tests

    /// Single block rollback works correctly.
    #[tokio::test]
    async fn test_single_block_rollback() {
        let (reorg_manager, cache_manager) = setup_reorg_manager().await;

        // Set up tip at 100
        let header = make_test_header(100);
        cache_manager.insert_header(header).await;
        reorg_manager.update_tip(100, [100u8; 32]).await;

        // Rollback by 1 block
        reorg_manager.update_tip(99, [99u8; 32]).await;

        assert_eq!(reorg_manager.get_current_tip(), 99);
        assert!(
            cache_manager.get_header_by_number(100).is_none(),
            "Block 100 should be invalidated"
        );
    }

    /// Deep rollback respects max_reorg_depth configuration.
    #[tokio::test]
    async fn test_deep_rollback() {
        let config = ReorgManagerConfig { max_reorg_depth: 50, ..ReorgManagerConfig::default() };
        let cache_config = CacheManagerConfig::default();
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let cache_manager = Arc::new(
            CacheManager::new(&cache_config, chain_state.clone()).expect("valid test cache config"),
        );
        let reorg_manager = ReorgManager::new(config, chain_state, cache_manager.clone());

        // Set tip to 1000
        reorg_manager.update_tip(1000, [232u8; 32]).await;

        // Insert blocks from 900 to 1000
        for block_num in 900..=1000_u64 {
            let header = make_test_header(block_num);
            cache_manager.insert_header(header).await;
        }

        // Rollback to 950 (50 blocks)
        reorg_manager.update_tip(950, [0xBB; 32]).await;

        // Tip should be 950
        assert_eq!(reorg_manager.get_current_tip(), 950);

        // Blocks 951-1000 should be invalidated
        assert!(cache_manager.get_header_by_number(975).is_none());
        assert!(cache_manager.get_header_by_number(1000).is_none());

        // Block 949 should still be cached
        assert!(cache_manager.get_header_by_number(949).is_some());
    }

    /// Concurrent reorg coalescing - multiple rapid reorgs don't cause thrashing.
    #[tokio::test]
    async fn test_reorg_coalescing_batches_rapid_updates() {
        let config = ReorgManagerConfig {
            coalesce_window_ms: 50, // Short window for testing
            ..ReorgManagerConfig::default()
        };
        let cache_config = CacheManagerConfig::default();
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let cache_manager = Arc::new(
            CacheManager::new(&cache_config, chain_state.clone()).expect("valid test cache config"),
        );
        let reorg_manager = ReorgManager::new(config, chain_state, cache_manager);

        // Set initial tip
        reorg_manager.update_tip(1000, [1u8; 32]).await;

        // Fire rapid reorgs at same height (simulates reorg storm)
        for i in 2..=5_u8 {
            reorg_manager.update_tip(1000, [i; 32]).await;
        }

        // The final state should have the last hash
        let final_hash = reorg_manager.get_current_head_hash();
        assert_eq!(final_hash, [5u8; 32], "Should have latest hash after coalesced updates");
    }
    //
    // These tests verify that concurrent updates from WebSocket (update_tip) and
    // health check (update_tip_number_only) paths don't corrupt state or cause
    // race conditions. The ReorgManager uses an update_lock to coordinate.

    /// Property: Concurrent WebSocket and health check updates never corrupt chain state.
    ///
    /// Multiple concurrent calls to update_tip() and update_tip_number_only()
    /// should never leave the chain state in an inconsistent state where
    /// finalized > tip.
    #[tokio::test]
    async fn test_concurrent_websocket_and_healthcheck_preserve_invariants() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use tokio::task;

        const NUM_ITERATIONS: usize = 10;
        const NUM_WS_TASKS: usize = 8;
        const NUM_HC_TASKS: usize = 8;

        for iteration in 0..NUM_ITERATIONS {
            let (reorg_manager, cache_manager) = setup_reorg_manager().await;
            let reorg_manager = Arc::new(reorg_manager);

            // Pre-populate cache with blocks
            for block_num in 980..=1000 {
                let header = make_test_header(block_num);
                cache_manager.insert_header(header).await;
            }
            reorg_manager.update_tip(1000, [100u8; 32]).await;

            let invariant_violations = Arc::new(AtomicUsize::new(0));
            let mut handles = vec![];

            // Spawn WebSocket update tasks (advance tip)
            for i in 0..NUM_WS_TASKS {
                let rm = reorg_manager.clone();
                let violations = invariant_violations.clone();
                handles.push(task::spawn(async move {
                    // Vary timing to increase interleaving
                    if i % 3 == 0 {
                        tokio::task::yield_now().await;
                    }
                    let block = 1001 + (i as u64);
                    #[allow(clippy::cast_possible_truncation)]
                    rm.update_tip(block, [block as u8; 32]).await;

                    // Check invariant after update
                    let tip = rm.get_current_tip();
                    let finalized = rm.get_finalized_block();
                    if finalized > tip {
                        violations.fetch_add(1, AtomicOrdering::SeqCst);
                    }
                }));
            }

            // Spawn health check tasks (may detect rollback)
            for i in 0..NUM_HC_TASKS {
                let rm = reorg_manager.clone();
                let violations = invariant_violations.clone();
                handles.push(task::spawn(async move {
                    // Vary timing
                    if i % 2 == 0 {
                        tokio::task::yield_now().await;
                    }
                    // Alternate between forward and rollback scenarios
                    let block = if i % 2 == 0 { 1005 + (i as u64) } else { 995 };
                    rm.update_tip_number_only(block).await;

                    // Check invariant after update
                    let tip = rm.get_current_tip();
                    let finalized = rm.get_finalized_block();
                    if finalized > tip {
                        violations.fetch_add(1, AtomicOrdering::SeqCst);
                    }
                }));
            }

            for handle in handles {
                let _ = handle.await;
            }

            // Verify no invariant violations occurred
            let violations = invariant_violations.load(AtomicOrdering::SeqCst);
            assert_eq!(
                violations, 0,
                "Iteration {}: {} invariant violations (finalized > tip)",
                iteration, violations
            );

            // Final consistency check
            let tip = reorg_manager.get_current_tip();
            let finalized = reorg_manager.get_finalized_block();
            assert!(finalized <= tip, "Final state invalid: finalized {} > tip {}", finalized, tip);
        }
    }

    /// Specific scenario: Health check detects rollback while WebSocket sends new blocks.
    ///
    /// Simulates: WebSocket sends blocks 1001-1005, while health check detects
    /// the chain rolled back to 990. The coordination lock should serialize these
    /// operations correctly.
    #[tokio::test]
    async fn test_healthcheck_rollback_during_websocket_block_storm() {
        let (reorg_manager, cache_manager) = setup_reorg_manager().await;
        let reorg_manager = Arc::new(reorg_manager);

        // Set up initial state at block 1000
        for block_num in 985..=1000 {
            let header = make_test_header(block_num);
            cache_manager.insert_header(header).await;
            #[allow(clippy::cast_possible_truncation)]
            reorg_manager.update_tip(block_num, [block_num as u8; 32]).await;
        }

        assert_eq!(reorg_manager.get_current_tip(), 1000);
        assert!(cache_manager.get_header_by_number(995).is_some());

        // Launch health check rollback after a small delay
        let healthcheck_task = {
            let rm = reorg_manager.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(2)).await;
                rm.update_tip_number_only(990).await;
            })
        };

        // Simultaneously, WebSocket sends blocks 1001-1010
        let mut ws_tasks = vec![];
        for block in 1001..=1010_u64 {
            let rm = reorg_manager.clone();
            ws_tasks.push(tokio::spawn(async move {
                #[allow(clippy::cast_possible_truncation)]
                rm.update_tip(block, [block as u8; 32]).await;
            }));
        }

        // Wait for all tasks
        healthcheck_task.await.unwrap();
        for task in ws_tasks {
            task.await.unwrap();
        }

        // After all updates, tip should be at least 990 (health check rollback target)
        // and at most 1010 (highest WebSocket block)
        let final_tip = reorg_manager.get_current_tip();
        assert!(
            (990..=1010).contains(&final_tip),
            "Final tip {} should be between 990 and 1010",
            final_tip
        );

        // The exact outcome depends on execution order, but state should be consistent
        let finalized = reorg_manager.get_finalized_block();
        assert!(finalized <= final_tip, "finalized {} > tip {}", finalized, final_tip);
    }

    /// Concurrent updates to the same block with different hashes (reorg detection).
    ///
    /// Multiple tasks sending blocks at the same height with different hashes
    /// should trigger proper reorg handling without corrupting state.
    #[tokio::test]
    async fn test_concurrent_same_height_different_hash_updates() {
        use tokio::task;

        const NUM_TASKS: usize = 10;

        let (reorg_manager, cache_manager) = setup_reorg_manager().await;
        let reorg_manager = Arc::new(reorg_manager);

        // Set initial tip
        reorg_manager.update_tip(1000, [0u8; 32]).await;

        // Populate cache
        for block_num in 995..=1000 {
            let header = make_test_header(block_num);
            cache_manager.insert_header(header).await;
        }

        // All tasks update to same block number with different hashes
        // (simulates receiving conflicting block notifications)
        let mut handles = vec![];
        for i in 0..NUM_TASKS {
            let rm = reorg_manager.clone();
            handles.push(task::spawn(async move {
                // Each task sends block 1000 with a unique hash
                #[allow(clippy::cast_possible_truncation)]
                rm.update_tip(1000, [i as u8; 32]).await;
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // State should be consistent - one of the hashes should "win"
        let final_tip = reorg_manager.get_current_tip();
        assert_eq!(final_tip, 1000, "Tip should still be 1000");

        // Hash should be one of the submitted hashes (0-9)
        let final_hash = reorg_manager.get_current_head_hash();
        let hash_value = usize::from(final_hash[0]);
        assert!(hash_value < NUM_TASKS, "Hash should be from one of the tasks, got {}", hash_value);
    }

    /// Stress test: Mixed operations under high concurrency.
    ///
    /// Combines forward updates, rollbacks, finalized updates, and hash changes
    /// all happening concurrently.
    #[tokio::test]
    async fn test_stress_mixed_concurrent_operations() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use tokio::task;

        const NUM_ITERATIONS: usize = 5;
        const OPS_PER_ITERATION: usize = 20;

        for iteration in 0..NUM_ITERATIONS {
            let (reorg_manager, _cache_manager) = setup_reorg_manager().await;
            let reorg_manager = Arc::new(reorg_manager);

            // Set initial state
            reorg_manager.update_tip(1000, [100u8; 32]).await;

            let errors = Arc::new(AtomicUsize::new(0));
            let mut handles = vec![];

            for i in 0..OPS_PER_ITERATION {
                let rm = reorg_manager.clone();
                let errs = errors.clone();

                let handle = task::spawn(async move {
                    match i % 4 {
                        0 => {
                            // Forward update via WebSocket
                            let block = 1001 + (i as u64);
                            #[allow(clippy::cast_possible_truncation)]
                            rm.update_tip(block, [block as u8; 32]).await;
                        }
                        1 => {
                            // Health check forward
                            let block = 1000 + (i as u64);
                            rm.update_tip_number_only(block).await;
                        }
                        2 => {
                            // Health check rollback
                            let block = 990 + ((i % 10) as u64);
                            rm.update_tip_number_only(block).await;
                        }
                        3 => {
                            // Finalized update (valid range)
                            let finalized = 900 + ((i % 50) as u64);
                            rm.update_finalized_block(finalized).await;
                        }
                        _ => unreachable!(),
                    }

                    // Verify invariants after each operation
                    let tip = rm.get_current_tip();
                    let finalized = rm.get_finalized_block();
                    if finalized > tip && tip > 0 {
                        errs.fetch_add(1, AtomicOrdering::SeqCst);
                    }
                });
                handles.push(handle);
            }

            for handle in handles {
                let _ = handle.await;
            }

            let error_count = errors.load(AtomicOrdering::SeqCst);
            assert_eq!(
                error_count, 0,
                "Iteration {}: {} invariant violations",
                iteration, error_count
            );
        }
    }
}
