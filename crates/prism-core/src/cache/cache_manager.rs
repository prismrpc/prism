use crate::{
    cache::{
        block_cache::BlockCache,
        log_cache::LogCache,
        manager::{
            CacheManagerConfig, CacheManagerError, CleanupRequest, FetchGuard, InflightFetch,
        },
        transaction_cache::TransactionCache,
        types::{
            BlockBody, BlockHeader, CacheStats, LogFilter, LogId, LogRecord, ReceiptRecord,
            TransactionRecord,
        },
    },
    chain::ChainState,
    types::BlockRange,
};
use dashmap::{DashMap, DashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::{
    sync::{broadcast, mpsc, Semaphore},
    time::{Duration, Instant},
};
use tracing::{debug, info, trace, warn};

/// Orchestrates log, block, and transaction caches with coordinated fetch deduplication,
/// background cleanup, and reorg handling.
///
/// Uses shared `ChainState` for unified chain tip tracking across all components.
///
/// # Cleanup Architecture
///
/// `FetchGuard` drops send cleanup requests through a channel to a dedicated worker task
/// instead of spawning a new task per drop. This provides:
/// - **Zero allocation in Drop**: Channel send is allocation-free
/// - **Batched processing**: Worker batches requests for efficiency
/// - **Deterministic shutdown**: Worker drains pending requests before exit
///
/// # Cloning
///
/// Implements `Clone` with cheap Arc reference counting. All clones share the same
/// underlying caches and state. Cloning is O(1) and safe for passing across async
/// task boundaries.
#[derive(Clone)]
pub struct CacheManager {
    pub log_cache: Arc<LogCache>,
    pub block_cache: Arc<BlockCache>,
    pub transaction_cache: Arc<TransactionCache>,

    pub(crate) blocks_being_fetched: DashSet<u64>,
    pub(crate) inflight: DashMap<u64, InflightFetch>,

    /// Shared chain state - single source of truth for tip/finalized/hash.
    ///
    /// This is a **shared reference** to the same `ChainState` instance used by
    /// `ReorgManager`, `ScoringEngine`, and other components. All components see
    /// identical chain tip/finalized values via lock-free reads.
    ///
    /// See [`crate::chain`] module docs for the ownership pattern explanation.
    pub(crate) chain_state: Arc<ChainState>,

    pub config: CacheManagerConfig,
    pub last_cleanup_block: Arc<AtomicU64>,
    cleanup_in_progress: Arc<AtomicBool>,

    /// Channel for `FetchGuard` cleanup requests.
    ///
    /// Using `UnboundedSender` because:
    /// - Drop cannot block waiting for channel capacity
    /// - Cleanup requests are small (16 bytes)
    /// - Worker drains fast enough to prevent unbounded growth
    ///
    /// The sender is cloned into each `FetchGuard` for use in `Drop`.
    cleanup_tx: mpsc::UnboundedSender<CleanupRequest>,

    /// Temporary storage for cleanup channel receiver.
    ///
    /// Taken (moved out) when `start_all_background_tasks` is called to start the
    /// cleanup worker. Using `std::sync::Mutex` (not tokio) because this is only
    /// accessed during initialization, not in hot paths.
    ///
    /// Wrapped in `Arc` to satisfy Clone derive (needed for `CacheManager` cloning).
    cleanup_rx: Arc<std::sync::Mutex<Option<mpsc::UnboundedReceiver<CleanupRequest>>>>,
}

impl CacheManager {
    /// Creates a new cache manager.
    ///
    /// # Errors
    ///
    /// Returns `CacheManagerError` if any cache component fails to initialize:
    /// - `LogCacheConfigError` if log cache `chunk_size` exceeds maximum (1024)
    /// - `BlockCacheError` if block cache configuration is invalid (e.g., zero capacities)
    /// - `TransactionCacheError` if transaction cache configuration is invalid
    pub fn new(
        config: &CacheManagerConfig,
        chain_state: Arc<ChainState>,
    ) -> Result<Self, CacheManagerError> {
        let log_cache = Arc::new(LogCache::new(&config.log_cache)?);
        let block_cache = Arc::new(BlockCache::new(&config.block_cache)?);
        let transaction_cache = Arc::new(TransactionCache::new(&config.transaction_cache)?);

        // Create cleanup channel for FetchGuard drops
        // The receiver is stored temporarily and consumed when start_all_background_tasks is called
        let (cleanup_tx, cleanup_rx) = mpsc::unbounded_channel();

        Ok(Self {
            log_cache,
            block_cache,
            transaction_cache,
            blocks_being_fetched: DashSet::new(),
            inflight: DashMap::new(),
            chain_state,
            config: config.clone(),
            last_cleanup_block: Arc::new(AtomicU64::new(0)),
            cleanup_in_progress: Arc::new(AtomicBool::new(false)),
            cleanup_tx,
            cleanup_rx: Arc::new(std::sync::Mutex::new(Some(cleanup_rx))),
        })
    }

    /// Starts all background maintenance tasks: inflight cleanup, stats updates, and cache pruning.
    /// All tasks shut down gracefully when the broadcast signal fires.
    pub fn start_all_background_tasks(self: &Arc<Self>, shutdown_tx: &broadcast::Sender<()>) {
        // Start FetchGuard cleanup worker (takes ownership of the receiver)
        self.start_fetch_guard_cleanup_worker(shutdown_tx.subscribe());

        // Start inflight cleanup
        self.start_inflight_cleanup(shutdown_tx.subscribe());

        // Start stats updater
        self.start_background_stats_updater(shutdown_tx.subscribe());

        // Start background cleanup
        self.start_background_cleanup_tasks(shutdown_tx.subscribe());
    }

    /// Starts the cleanup worker that processes `FetchGuard` drop requests.
    ///
    /// See [`crate::cache::manager::background::run_cleanup_worker`] for implementation details.
    #[allow(clippy::expect_used)] // Panics are intentional: mutex poisoning or double-start are programmer errors
    fn start_fetch_guard_cleanup_worker(&self, shutdown_rx: broadcast::Receiver<()>) {
        // Take the receiver (can only be taken once)
        let cleanup_rx = self
            .cleanup_rx
            .lock()
            .expect("cleanup_rx mutex poisoned")
            .take()
            .expect("cleanup worker already started or receiver not initialized");

        let inflight = self.inflight.clone();
        let blocks_being_fetched = self.blocks_being_fetched.clone();

        tokio::spawn(async move {
            crate::cache::manager::background::run_cleanup_worker(
                cleanup_rx,
                inflight,
                blocks_being_fetched,
                shutdown_rx,
            )
            .await;
        });

        debug!("FetchGuard cleanup worker started");
    }

    /// Periodically removes stale inflight fetch entries.
    ///
    /// See [`crate::cache::manager::background::run_inflight_cleanup`] for implementation details.
    fn start_inflight_cleanup(&self, shutdown_rx: broadcast::Receiver<()>) {
        let inflight = self.inflight.clone();
        let blocks_being_fetched = self.blocks_being_fetched.clone();

        tokio::spawn(async move {
            crate::cache::manager::background::run_inflight_cleanup(
                inflight,
                blocks_being_fetched,
                shutdown_rx,
            )
            .await;
        });
    }

    /// Attempts to acquire a fetch lock for a block number using lock-free `DashSet`.
    /// Returns true if the lock was acquired, false if already held by another task.
    ///
    /// # Note
    /// This is an internal API used for testing and benchmarking. Not intended for external use.
    #[doc(hidden)]
    #[must_use]
    pub fn try_acquire_fetch_lock(&self, block_number: u64) -> bool {
        self.blocks_being_fetched.insert(block_number)
    }

    /// Releases a fetch lock for a block number.
    ///
    /// # Note
    /// This is an internal API used for testing and benchmarking. Not intended for external use.
    #[doc(hidden)]
    #[must_use]
    pub fn release_fetch_lock(&self, block_number: u64) -> bool {
        self.blocks_being_fetched.remove(&block_number).is_some()
    }

    /// Checks if a block is currently being fetched.
    ///
    /// # Note
    /// This is an internal API used for testing and benchmarking. Not intended for external use.
    #[doc(hidden)]
    #[must_use]
    pub fn is_block_being_fetched(&self, block_number: u64) -> bool {
        self.blocks_being_fetched.contains(&block_number)
    }

    /// Attempts to acquire a fetch lock for the given block without waiting.
    ///
    /// # Returns
    ///
    /// Returns `None` in the following cases:
    /// 1. Block is already fully cached (pre-flight check at entry)
    /// 2. Semaphore permit unavailable (another task is actively fetching this block)
    /// 3. Fetch lock unavailable (`blocks_being_fetched` already contains this block)
    /// 4. Block became cached while waiting (post-acquire double-check after permit granted)
    ///
    /// # Panic Safety (CRITICAL)
    ///
    /// The RAII pattern ensures semaphore creation and guard construction happen
    /// atomically, preventing resource leaks on panic.
    ///
    /// # Deadlock Prevention (CRITICAL)
    ///
    /// The implementation follows strict lock ordering to prevent deadlocks:
    /// 1. `DashMap` shard lock is acquired to get/create the semaphore
    /// 2. Semaphore `Arc` is cloned (cheap operation)
    /// 3. `DashMap` shard lock is IMMEDIATELY released
    /// 4. Only THEN do we attempt semaphore operations (`try_acquire_owned`)
    ///
    /// **NEVER** hold a `DashMap` entry reference across await points or blocking operations.
    ///
    /// # Race Condition Prevention
    ///
    /// To prevent memory leaks from stale inflight entries, we check if the block is already
    /// cached BEFORE creating an inflight entry. This avoids the race where:
    /// 1. Thread A completes fetch and drops `FetchGuard` (removes inflight entry)
    /// 2. Thread B was waiting, now creates NEW inflight entry
    /// 3. Thread B sees block is cached, tries to cleanup, but Thread C created yet another entry
    ///
    /// The returned `FetchGuard` handles cleanup automatically via RAII (Drop trait).
    #[must_use]
    pub fn try_begin_fetch(self: &Arc<Self>, block: u64) -> Option<FetchGuard> {
        // Phase 1: Pre-flight check - prevent stale inflight entries
        if self.is_block_fully_cached(block) {
            return None;
        }

        // Phase 2: Get or create semaphore (DashMap lock released immediately after clone)
        let sem = self.get_or_create_semaphore(block);

        // Phase 3: Try to acquire permit (non-blocking)
        let permit = sem.clone().try_acquire_owned().ok()?;

        // Phase 4: Double-check after acquiring permit
        if self.is_block_fully_cached(block) {
            drop(permit);
            self.cleanup_fetch_state(block);
            return None;
        }

        // Phase 5: Acquire fetch lock and create guard
        let has_fetch_lock = self.try_acquire_fetch_lock(block);
        Some(FetchGuard::new(self.cleanup_tx.clone(), block, permit, has_fetch_lock))
    }

    /// Waits up to `timeout` to acquire a fetch lock for the given block.
    ///
    /// # Panic Safety (CRITICAL)
    ///
    /// The RAII pattern ensures semaphore creation and guard construction happen
    /// atomically, preventing resource leaks on panic.
    ///
    /// # Deadlock Prevention (CRITICAL)
    ///
    /// The implementation follows strict lock ordering to prevent deadlocks.
    /// **This is the most critical correctness invariant in the entire fetch coordination system.**
    ///
    /// ## Lock-Free Execution Path
    ///
    /// The implementation ensures the `DashMap` shard lock is NEVER held across await points:
    ///
    /// 1. **Lock acquisition**: `DashMap` shard lock acquired via `cache.inflight.entry(block)`
    /// 2. **Quick clone**: Semaphore `Arc` cloned (cheap pointer copy, no allocation)
    /// 3. **Lock release**: `DashMap` entry reference dropped, shard lock IMMEDIATELY released
    /// 4. **Async operation**: Only NOW do we `.await` on `semaphore.acquire_owned()`
    ///
    /// This ensures other tasks can access the same `DashMap` shard while we wait for the
    /// semaphore.
    ///
    /// ## Why This Matters
    ///
    /// `DashMap` uses sharding for concurrency - multiple tasks can access different shards
    /// concurrently, but tasks accessing the same shard are serialized by the shard lock.
    /// If we held the entry reference across the semaphore await:
    ///
    /// - **Deadlock scenario**: Task A holds shard lock, awaits semaphore held by Task B. Task B
    ///   tries to access same shard, blocks on shard lock held by Task A. DEADLOCK.
    ///
    /// - **Performance degradation**: Even without deadlock, holding the shard lock during await
    ///   would serialize ALL tasks trying to fetch blocks in the same shard, destroying the
    ///   concurrency benefits of the lock-free design.
    ///
    /// ## Implementation Guarantee
    ///
    /// The helper function `get_or_create_semaphore()` is marked `#[must_use]` and thoroughly
    /// documented to ensure this invariant is never violated during refactoring.
    ///
    /// **NEVER** hold a `DashMap` entry reference across await points. This will cause
    /// deadlocks or severe performance degradation.
    ///
    /// # Race Condition Prevention
    ///
    /// To prevent memory leaks from stale inflight entries, we check if the block is already
    /// cached BEFORE creating an inflight entry. See `try_begin_fetch` for details.
    ///
    /// The returned `FetchGuard` handles cleanup automatically via RAII (Drop trait).
    pub async fn begin_fetch_with_timeout(
        self: &Arc<Self>,
        block: u64,
        timeout: Duration,
    ) -> Option<FetchGuard> {
        // Phase 1: Pre-flight check
        if self.is_block_fully_cached(block) {
            return None;
        }

        // Phase 2: Get or create semaphore (DashMap lock released immediately)
        let sem = self.get_or_create_semaphore(block);

        // Phase 3: Try to acquire permit with timeout
        let permit = match tokio::time::timeout(timeout, sem.clone().acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                warn!(block = block, "failed to acquire semaphore permit");
                return None;
            }
            Err(_) => {
                warn!(block = block, "fetch lock acquisition timed out");
                self.cleanup_fetch_state(block);
                return None;
            }
        };

        // Phase 4: Double-check after acquiring permit
        if self.is_block_fully_cached(block) {
            drop(permit);
            self.cleanup_fetch_state(block);
            return None;
        }

        // Phase 5: Acquire fetch lock and create guard
        let has_fetch_lock = self.try_acquire_fetch_lock(block);
        Some(FetchGuard::new(self.cleanup_tx.clone(), block, permit, has_fetch_lock))
    }

    /// Creates inflight entry if it doesn't exist, clones semaphore,
    /// and immediately releases the `DashMap` lock to prevent deadlock.
    ///
    /// # Critical Invariant: `DashMap` Entry Lifetime
    ///
    /// This helper **MUST** ensure the `DashMap` entry reference is dropped
    /// before any await points or blocking operations. The entry lock is held
    /// only during the brief `.or_insert_with()` call and `.clone()` operation.
    ///
    /// **NEVER** hold the entry reference across await points. The return value
    /// is a cloned `Arc<Semaphore>` that can be safely held across awaits.
    ///
    /// ## Deadlock Example (What NOT to do)
    ///
    /// ```rust,ignore
    /// // WRONG: Holding DashMap entry across await causes deadlock
    /// let entry = self.inflight.entry(block);
    /// let sem = entry.or_insert_with(|| ...);
    /// sem.acquire().await;  // DEADLOCK: Other threads blocked on DashMap shard
    /// ```
    #[must_use]
    fn get_or_create_semaphore(&self, block: u64) -> Arc<Semaphore> {
        let inflight_fetch = self.inflight.entry(block).or_insert_with(|| InflightFetch {
            semaphore: Arc::new(Semaphore::new(1)),
            started_at: Instant::now(),
        });
        inflight_fetch.semaphore.clone()
        // DashMap entry lock released here
    }

    /// Cleanup helper: Remove inflight and fetch lock entries.
    ///
    /// Used internally during fetch coordination and exposed for testing.
    pub(crate) fn cleanup_fetch_state(&self, block: u64) {
        self.inflight.remove(&block);
        self.blocks_being_fetched.remove(&block);
    }

    /// Queries logs matching the filter, returning cached records and any missing block ranges.
    ///
    /// Uses the inverted index to efficiently find matching logs without full scans.
    /// Returns partial results if only some of the requested range is cached.
    ///
    /// # Arguments
    /// * `filter` - Filter criteria (address, topics, block range)
    /// * `current_tip` - Current chain tip for coverage calculation
    ///
    /// # Returns
    ///
    /// Returns `(matched_logs, missing_ranges)`:
    /// - `matched_logs`: `Vec<Arc<LogRecord>>` - Each log wrapped in Arc for cheap cloning
    ///   (reference counting, not deep copy). Callers can freely clone these.
    /// - `missing_ranges`: Block ranges needing upstream fetch (empty for full cache hit)
    #[allow(clippy::cast_precision_loss)]
    pub async fn get_logs(
        &self,
        filter: &LogFilter,
        current_tip: u64,
    ) -> (Vec<Arc<LogRecord>>, Vec<(u64, u64)>) {
        #[cfg(feature = "verbose-logging")]
        trace!(from_block = filter.from_block, to_block = filter.to_block, "querying logs");

        let (log_ids, missing_ranges) = self.log_cache.query_logs(filter, current_tip).await;

        if log_ids.is_empty() {
            #[cfg(feature = "verbose-logging")]
            debug!("log cache miss: no matching logs found");
            return (Vec::new(), missing_ranges);
        }

        let log_records = self.log_cache.get_log_records(&log_ids);

        #[cfg(feature = "verbose-logging")]
        debug!(
            logs_found = log_records.len(),
            missing_range_count = missing_ranges.len(),
            "log cache query complete"
        );

        if missing_ranges.is_empty() {
            info!(blocks = filter.to_block - filter.from_block + 1, "full cache hit for log query");
        } else {
            let total_blocks = filter.to_block - filter.from_block + 1;
            let missing_blocks: u64 = missing_ranges.iter().map(|(from, to)| to - from + 1).sum();
            let cached_blocks = total_blocks - missing_blocks;

            info!(
                total_blocks = total_blocks,
                cached_blocks = cached_blocks,
                missing_blocks = missing_blocks,
                cache_hit_pct =
                    ((u128::from(cached_blocks) * 1000) / u128::from(total_blocks)) as f64 / 10.0,
                logs_found = log_records.len(),
                ?missing_ranges,
                "partial cache hit for log query"
            );
        }

        (log_records, missing_ranges)
    }

    /// Returns cached log records with their IDs and any missing block ranges.
    #[allow(clippy::cast_precision_loss)]
    pub async fn get_logs_with_ids(
        &self,
        filter: &LogFilter,
        current_tip: u64,
    ) -> (Vec<(LogId, Arc<LogRecord>)>, Vec<(u64, u64)>) {
        trace!(
            from_block = filter.from_block,
            to_block = filter.to_block,
            "querying logs with IDs"
        );

        let (log_ids, missing_ranges) = self.log_cache.query_logs(filter, current_tip).await;

        if log_ids.is_empty() {
            debug!("log cache miss: no matching logs found");
            return (Vec::new(), missing_ranges);
        }

        let log_records_with_ids = self.log_cache.get_log_records_with_ids(&log_ids);

        debug!(
            logs_found = log_records_with_ids.len(),
            missing_range_count = missing_ranges.len(),
            "log cache query complete"
        );

        if missing_ranges.is_empty() {
            info!(blocks = filter.to_block - filter.from_block + 1, "full cache hit for log query");
        } else {
            let total_blocks = filter.to_block - filter.from_block + 1;
            let missing_blocks: u64 = missing_ranges.iter().map(|(from, to)| to - from + 1).sum();
            let cached_blocks = total_blocks - missing_blocks;

            info!(
                total_blocks = total_blocks,
                cached_blocks = cached_blocks,
                missing_blocks = missing_blocks,
                cache_hit_pct =
                    ((u128::from(cached_blocks) * 1000) / u128::from(total_blocks)) as f64 / 10.0,
                logs_found = log_records_with_ids.len(),
                ?missing_ranges,
                "partial cache hit for log query"
            );
        }
        (log_records_with_ids, missing_ranges)
    }

    // --- Log Operations ---

    /// Inserts a single log entry into the cache.
    ///
    /// Updates cache statistics after insertion.
    /// For bulk insertions, prefer [`insert_logs_bulk`](Self::insert_logs_bulk) for better
    /// performance.
    pub async fn insert_logs(&self, logs: Vec<(LogId, LogRecord)>) {
        trace!(count = logs.len(), "inserting logs");
        for (log_id, log_record) in logs {
            self.log_cache.insert_log(log_id, log_record).await;
        }
    }

    /// Inserts multiple log entries in a single batch operation.
    ///
    /// More efficient than multiple [`insert_logs`](Self::insert_logs) calls as it batches
    /// internal operations. Updates statistics once after all insertions.
    ///
    /// # Performance
    /// - Amortizes lock acquisition across all logs
    /// - Updates stats once at the end
    pub async fn insert_logs_bulk(&self, logs: Vec<(LogId, LogRecord)>) {
        trace!(count = logs.len(), "bulk inserting logs");
        self.log_cache.insert_logs_bulk(logs).await;
    }

    /// Inserts multiple log entries without updating statistics.
    ///
    /// Use this when inserting logs as part of a larger operation where
    /// statistics will be updated separately, such as during cache warming.
    ///
    /// # Warning
    /// Statistics will be stale until manually updated or another
    /// stats-updating method is called.
    pub async fn insert_logs_bulk_no_stats(&self, logs: Vec<(LogId, LogRecord)>) {
        trace!(count = logs.len(), "bulk inserting logs (skip stats)");
        self.log_cache.insert_logs_bulk_no_stats(logs).await;
    }

    /// Validates and inserts logs, filtering out any with invalid block hashes.
    ///
    /// Compares each log's `block_hash` against the block cache to ensure the log
    /// belongs to the canonical chain. Logs from orphaned/reorged blocks are
    /// filtered out to prevent caching stale data.
    ///
    /// This is the recommended method for caching logs from upstream responses,
    /// as it provides protection against caching data from reorged blocks.
    ///
    /// # Arguments
    /// * `logs` - Vector of (`LogId`, `LogRecord`) tuples to validate and insert
    ///
    /// # Returns
    /// Number of logs that were successfully validated and cached
    pub async fn insert_logs_validated(&self, logs: Vec<(LogId, LogRecord)>) -> usize {
        let (valid_logs, invalid_count) =
            crate::cache::manager::validation::validate_logs_against_blocks(self, logs);

        if invalid_count > 0 {
            info!(
                valid = valid_logs.len(),
                invalid = invalid_count,
                "filtered logs with mismatched block hashes"
            );
        }

        let valid_count = valid_logs.len();
        if !valid_logs.is_empty() {
            self.log_cache.insert_logs_bulk(valid_logs).await;
        }

        valid_count
    }

    /// Validates and inserts logs without updating stats (for bulk operations).
    ///
    /// Same as `insert_logs_validated` but skips stats update for performance
    /// during large bulk inserts.
    ///
    /// # Arguments
    /// * `logs` - Vector of (`LogId`, `LogRecord`) tuples to validate and insert
    ///
    /// # Returns
    /// Number of logs that were successfully validated and cached
    pub async fn insert_logs_validated_no_stats(&self, logs: Vec<(LogId, LogRecord)>) -> usize {
        let (valid_logs, invalid_count) =
            crate::cache::manager::validation::validate_logs_against_blocks(self, logs);

        if invalid_count > 0 {
            debug!(
                valid = valid_logs.len(),
                invalid = invalid_count,
                "filtered logs with mismatched block hashes (no stats)"
            );
        }

        let valid_count = valid_logs.len();
        if !valid_logs.is_empty() {
            self.log_cache.insert_logs_bulk_no_stats(valid_logs).await;
        }

        valid_count
    }

    pub async fn update_stats_on_demand(&self) {
        self.log_cache.update_stats_on_demand().await;
    }

    pub async fn should_update_stats(&self) -> bool {
        self.log_cache.should_update_stats().await
    }

    pub async fn force_update_stats(&self) {
        self.log_cache.force_update_stats().await;
    }

    /// Returns whether stats are dirty (need recomputation).
    ///
    /// # Deprecation Note
    /// This method wraps the deprecated `LogCache::get_changes_since_last_stats()`.
    /// Consider using `get_stats()` which automatically handles dirty flag checking.
    #[allow(deprecated)]
    #[must_use]
    pub fn get_changes_since_last_stats(&self) -> usize {
        self.log_cache.get_changes_since_last_stats()
    }

    pub fn start_background_stats_updater(&self, shutdown_rx: broadcast::Receiver<()>) {
        self.log_cache.clone().start_background_stats_updater(shutdown_rx);
    }

    pub async fn cache_exact_result(&self, filter: &LogFilter, log_ids: Vec<LogId>) {
        trace!(log_count = log_ids.len(), "caching exact filter result");
        self.log_cache.cache_exact_result(filter, log_ids).await;
    }

    pub async fn warm_logs(&self, from_block: u64, to_block: u64, logs: Vec<(LogId, LogRecord)>) {
        info!(from_block = from_block, to_block = to_block, "warming log cache");
        self.log_cache.warm_range(from_block, to_block, logs).await;
    }

    // --- Block Operations ---

    /// Retrieves a full block (header + body) by block number.
    ///
    /// Checks the hot window first for recent blocks, falling back to persistent storage.
    /// Returns `None` if either header or body is not cached.
    pub fn get_block_by_number(
        &self,
        block_number: u64,
    ) -> Option<(Arc<BlockHeader>, Arc<BlockBody>)> {
        trace!(block = block_number, "get block by number");
        self.block_cache.get_block_by_number(block_number)
    }

    /// Retrieves a full block (header + body) by block hash.
    ///
    /// Returns `None` if either header or body is not cached.
    pub fn get_block_by_hash(
        &self,
        block_hash: &[u8; 32],
    ) -> Option<(Arc<BlockHeader>, Arc<BlockBody>)> {
        trace!(hash = ?block_hash, "get block by hash");
        self.block_cache.get_block_by_hash(block_hash)
    }

    /// Retrieves a block header by number, checking hot window first for recent blocks.
    #[must_use]
    pub fn get_header_by_number(&self, block_number: u64) -> Option<Arc<BlockHeader>> {
        self.block_cache.get_header_by_number(block_number)
    }

    /// Retrieves a block header by hash.
    #[must_use]
    pub fn get_header_by_hash(&self, block_hash: &[u8; 32]) -> Option<Arc<BlockHeader>> {
        self.block_cache.get_header_by_hash(block_hash)
    }

    /// Retrieves a block body (transaction hashes) by block hash.
    #[must_use]
    pub fn get_body_by_hash(&self, block_hash: &[u8; 32]) -> Option<Arc<BlockBody>> {
        self.block_cache.get_body_by_hash(block_hash)
    }

    /// Inserts a block header into both hot window (if recent) and persistent storage.
    pub async fn insert_header(&self, header: BlockHeader) {
        self.block_cache.insert_header(header).await;
    }

    /// Inserts a block body, updating hot window if corresponding header exists.
    pub async fn insert_body(&self, body: BlockBody) {
        self.block_cache.insert_body(body).await;
    }

    pub async fn warm_blocks(
        &self,
        from_block: u64,
        to_block: u64,
        blocks: Vec<(BlockHeader, BlockBody)>,
    ) {
        info!(from_block = from_block, to_block = to_block, "warming block cache");
        self.block_cache.warm_recent(from_block, to_block, blocks).await;
    }

    // --- Transaction Operations ---

    /// Retrieves a cached transaction by hash.
    ///
    /// Returns the transaction data including block location, signature, and input data.
    pub fn get_transaction(&self, tx_hash: &[u8; 32]) -> Option<Arc<TransactionRecord>> {
        trace!(hash = ?tx_hash, "get transaction");
        self.transaction_cache.get_transaction(tx_hash)
    }

    /// Retrieves a cached receipt by transaction hash.
    ///
    /// The receipt contains log references (`LogId`s) but not full log data.
    /// Use `get_receipt_with_logs` to resolve log references.
    pub fn get_receipt(&self, tx_hash: &[u8; 32]) -> Option<Arc<ReceiptRecord>> {
        trace!(hash = ?tx_hash, "get receipt");
        self.transaction_cache.get_receipt(tx_hash)
    }

    /// Retrieves a receipt with all log data resolved from the log cache.
    ///
    /// Returns both the receipt and a vector of the actual log records.
    /// More convenient than manually resolving log references via `get_receipt`.
    pub fn get_receipt_with_logs(
        &self,
        tx_hash: &[u8; 32],
    ) -> Option<(Arc<ReceiptRecord>, Vec<Arc<LogRecord>>)> {
        trace!(hash = ?tx_hash, "get receipt with logs");
        self.transaction_cache.get_receipt_with_logs(tx_hash, &self.log_cache)
    }

    /// Inserts a receipt and updates the block-to-transaction index.
    pub async fn insert_receipt(&self, receipt: ReceiptRecord) {
        self.transaction_cache.insert_receipt(receipt).await;
    }

    /// Bulk inserts receipts without updating stats (for large batch operations).
    ///
    /// Use `update_stats_on_demand` after the bulk insert completes to refresh stats.
    pub async fn insert_receipts_bulk_no_stats(&self, receipts: Vec<ReceiptRecord>) {
        self.transaction_cache.insert_receipts_bulk_no_stats(receipts).await;
    }

    pub async fn warm_transactions(
        &self,
        from_block: u64,
        to_block: u64,
        transactions: Vec<TransactionRecord>,
        receipts: Vec<ReceiptRecord>,
    ) {
        info!(from_block = from_block, to_block = to_block, "warming transaction cache");
        self.transaction_cache
            .warm_range(from_block, to_block, transactions, receipts)
            .await;
    }

    // --- Cache Invalidation & Stats ---

    /// Removes a block and all associated data (logs, transactions, receipts) from all caches.
    ///
    /// Called during chain reorganizations to purge data from blocks that are no longer
    /// part of the canonical chain. Invalidates across all cache components atomically.
    pub async fn invalidate_block(&self, block_number: u64) {
        info!(block = block_number, "invalidating block across all caches");
        self.log_cache.invalidate_block(block_number);
        self.block_cache.invalidate_block(block_number).await;
        self.transaction_cache.invalidate_block(block_number).await;
    }

    /// Clears all cached data across logs, blocks, and transactions.
    ///
    /// Typically called when switching authentication contexts to prevent data leakage
    /// between API keys. Does not clear shared `ChainState` as it's used by other components.
    pub async fn clear_cache(&self) {
        info!("clearing all caches");
        self.log_cache.clear_cache().await;
        self.block_cache.clear_cache().await;
        self.transaction_cache.clear_cache().await;
        // Note: We don't clear chain_state here as it's shared with other components
    }

    /// Aggregates statistics from all cache components.
    ///
    /// Returns combined metrics including cache sizes, hit rates, and memory usage.
    /// Useful for monitoring and debugging cache performance.
    pub async fn get_stats(&self) -> CacheStats {
        let mut stats = CacheStats::default();

        let log_stats = self.log_cache.get_stats().await;
        let block_stats = self.block_cache.get_stats().await;
        let tx_stats = self.transaction_cache.get_stats().await;

        stats.log_store_size = log_stats.log_store_size;
        stats.header_cache_size = block_stats.header_cache_size;
        stats.body_cache_size = block_stats.body_cache_size;
        stats.transaction_cache_size = tx_stats.transaction_cache_size;
        stats.receipt_cache_size = tx_stats.receipt_cache_size;
        stats.exact_result_count = log_stats.exact_result_count;
        stats.total_log_ids_cached = log_stats.total_log_ids_cached;
        stats.bitmap_memory_usage = log_stats.bitmap_memory_usage;
        stats.hot_window_size = block_stats.hot_window_size;

        stats
    }

    // --- Cache Warming ---

    pub async fn warm_recent(
        &self,
        from_block: u64,
        to_block: u64,
        blocks: Vec<(BlockHeader, BlockBody)>,
        transactions: Vec<TransactionRecord>,
        receipts: Vec<ReceiptRecord>,
        logs: Vec<(LogId, LogRecord)>,
    ) {
        info!(from_block = from_block, to_block = to_block, "warming all caches");
        self.warm_blocks(from_block, to_block, blocks).await;
        self.warm_transactions(from_block, to_block, transactions, receipts).await;
        self.warm_logs(from_block, to_block, logs).await;
    }

    // --- Range Queries ---

    #[must_use]
    pub fn is_range_covered(&self, from_block: u64, to_block: u64) -> bool {
        // Use iterator `.all()` instead of manual loop for idiomatic Rust
        (from_block..=to_block).all(|block_num| self.get_header_by_number(block_num).is_some())
    }

    #[must_use]
    pub fn get_missing_ranges(&self, from_block: u64, to_block: u64) -> Vec<(u64, u64)> {
        let mut missing = Vec::new();
        let mut current = from_block;

        while current <= to_block {
            let mut missing_start = None;
            while current <= to_block {
                if self.get_header_by_number(current).is_none() {
                    if missing_start.is_none() {
                        missing_start = Some(current);
                    }
                } else if let Some(start) = missing_start {
                    missing.push((start, current - 1));
                    missing_start = None;
                }
                current += 1;
            }

            if let Some(start) = missing_start {
                missing.push((start, to_block));
            }
        }

        missing
    }

    // --- Tip Tracking ---

    /// Returns the current chain tip block number from shared chain state.
    #[must_use]
    #[inline]
    pub fn get_current_tip(&self) -> u64 {
        self.chain_state.current_tip()
    }

    /// Returns the finalized block number from shared chain state.
    #[must_use]
    #[inline]
    pub fn get_finalized_block(&self) -> u64 {
        self.chain_state.finalized_block()
    }

    /// Checks if cleanup should be triggered based on block progression.
    ///
    /// **WARNING**: This does NOT update the chain tip. It only checks if enough blocks
    /// have passed since the last cleanup and triggers cleanup if needed.
    ///
    /// Chain tip updates must go through `ReorgManager::update_tip()` which updates
    /// the shared `chain_state`. This function only reads from `chain_state`.
    pub fn check_and_trigger_cleanup(self: &Arc<Self>, block_number: u64) {
        let current = self.chain_state.current_tip();
        if block_number > current {
            // Trigger cleanup if needed, but don't update chain_state here
            // (ReorgManager is responsible for that)
            if self.config.cleanup_every_n_blocks > 0 {
                let last_cleanup = self.last_cleanup_block.load(Ordering::Acquire);
                if block_number.saturating_sub(last_cleanup) >= self.config.cleanup_every_n_blocks {
                    // Use atomic flag to prevent multiple concurrent cleanups
                    if !self.cleanup_in_progress.load(Ordering::Acquire) &&
                        self.cleanup_in_progress
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    {
                        // Intentionally spawn a new task for each cleanup trigger.
                        // Cleanup is infrequent (every N blocks, typically 100+) and the
                        // spawning overhead is negligible compared to the cleanup work itself.
                        // This approach is simpler than maintaining a dedicated cleanup worker
                        // with channel-based coordination.
                        let cache_manager = self.clone();
                        tokio::spawn(async move {
                            cache_manager.perform_cleanup().await;
                            cache_manager.last_cleanup_block.store(block_number, Ordering::Release);
                            cache_manager.cleanup_in_progress.store(false, Ordering::Release);
                        });
                    }
                }
            }
        }
    }

    // --- Utilities ---

    /// Extracts block range from `eth_getLogs` filter parameters.
    ///
    /// Parses `fromBlock` and `toBlock` fields from the JSON params object.
    /// If `toBlock` is not specified, uses `fromBlock` as the end of the range.
    #[must_use]
    pub fn get_block_range_from_logs(&self, params: &serde_json::Value) -> Option<BlockRange> {
        crate::cache::manager::utilities::get_block_range_from_logs(params)
    }

    #[must_use]
    pub fn is_block_fully_cached(&self, block_number: u64) -> bool {
        self.get_block_by_number(block_number).is_some()
    }

    // --- Background Cleanup ---

    /// Starts the periodic cache cleanup task.
    ///
    /// See [`crate::cache::manager::background::run_background_cleanup`] for implementation
    /// details.
    pub fn start_background_cleanup_tasks(self: &Arc<Self>, shutdown_rx: broadcast::Receiver<()>) {
        let cache_manager = self.clone();
        tokio::spawn(async move {
            crate::cache::manager::background::run_background_cleanup(cache_manager, shutdown_rx)
                .await;
        });
    }

    /// Performs finality-aware cache cleanup.
    ///
    /// Prunes old cache entries while respecting finality boundaries.
    pub async fn perform_cleanup(&self) {
        crate::cache::manager::background::perform_cleanup(self).await;
    }

    // --- Debug & Diagnostics ---

    /// Returns the current fetch state for diagnostics.
    ///
    /// # Returns
    /// Tuple of (`inflight_count`, `fetch_locks_count`)
    #[must_use]
    pub fn get_fetch_state(&self) -> (usize, usize) {
        crate::cache::manager::utilities::get_fetch_state(self)
    }

    /// Force clears all fetch coordination state.
    ///
    /// **Warning**: Only use this for recovery from stuck states.
    pub fn force_cleanup_fetches(&self) {
        crate::cache::manager::utilities::force_cleanup_fetches(self);
    }

    /// Processes any pending cleanup requests synchronously.
    ///
    /// This is primarily for testing - in production the cleanup worker
    /// processes requests asynchronously in the background.
    #[cfg(test)]
    pub fn flush_pending_cleanups(&self) {
        // Since we're in a sync context in tests, we'll process cleanup requests
        // by checking if the receiver is still available (not taken by worker)
        if let Ok(mut guard) = self.cleanup_rx.lock() {
            if let Some(rx) = guard.as_mut() {
                // Drain and process all pending requests
                while let Ok(req) = rx.try_recv() {
                    self.inflight.remove(&req.block);
                    if req.release_fetch_lock {
                        self.blocks_being_fetched.remove(&req.block);
                    }
                }
            }
        }
    }
}
