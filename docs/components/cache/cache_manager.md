# Cache Manager

## Overview

The `CacheManager` is the orchestration layer that coordinates all subcaches (log cache, block cache, transaction cache) and manages inflight fetch deduplication to prevent duplicate upstream requests. It serves as the central interface for all caching operations in Prism, providing:

- **Unified Cache Interface**: Single entry point for all caching operations across logs, blocks, and transactions
- **Fetch Coordination**: Semaphore-based deduplication prevents duplicate upstream requests for the same data
- **Partial Cache Results**: Returns both cached data and missing ranges for efficient overlapping requests
- **Background Tasks**: Automatic cleanup, stats updates, and stale fetch detection
- **Tip Tracking**: Maintains current chain tip for cache invalidation and cleanup decisions
- **Thread Safety**: All operations are concurrent-safe using lock-free data structures where possible

**Source**: `crates/prism-core/src/cache/cache_manager.rs`

---

## Configuration

### CacheManagerConfig

Configuration structure for the cache manager and all its subcaches.

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheManagerConfig {
    pub log_cache: LogCacheConfig,
    pub block_cache: BlockCacheConfig,
    pub transaction_cache: TransactionCacheConfig,
    pub reorg_manager: ReorgManagerConfig,
    pub retain_blocks: u64,
    pub enable_auto_cleanup: bool,
    pub cleanup_interval_seconds: u64,
    pub cleanup_every_n_blocks: u64,
}
```

**Fields** (lines 35-46 in `cache_manager.rs`):

- **`log_cache`**: Configuration for log cache (chunk size, max exact results, bitmap limits)
  - Default: 1000 blocks/chunk, 10k exact results, 100k bitmap entries

- **`block_cache`**: Configuration for block cache (hot window, LRU sizes)
  - Default: 200 block hot window, 10k headers, 10k bodies

- **`transaction_cache`**: Configuration for transaction/receipt cache
  - Default: 50k transactions, 50k receipts

- **`reorg_manager`**: Configuration for reorganization detection
  - Default: 12 block safety depth

- **`retain_blocks`**: Number of blocks to retain in cache from current tip
  - Default: 1000 blocks

- **`enable_auto_cleanup`**: Enable automatic background cleanup tasks
  - Default: `true`

- **`cleanup_interval_seconds`**: Time-based cleanup interval
  - Default: 300 seconds (5 minutes)

- **`cleanup_every_n_blocks`**: Trigger cleanup every N blocks on tip updates (0 to disable)
  - Default: 100 blocks

**Default Configuration** (lines 48-61 in `cache_manager.rs` - `CacheManagerConfig::default()`):
```rust
impl Default for CacheManagerConfig {
    fn default() -> Self {
        Self {
            log_cache: LogCacheConfig::default(),
            block_cache: BlockCacheConfig::default(),
            transaction_cache: TransactionCacheConfig::default(),
            reorg_manager: ReorgManagerConfig::default(),
            retain_blocks: 1000,
            enable_auto_cleanup: true,
            cleanup_interval_seconds: 300,
            cleanup_every_n_blocks: 100,
        }
    }
}
```

---

## Data Structures

### CacheManager

Main cache manager struct that orchestrates all caching operations.

```rust
#[derive(Clone)]
pub struct CacheManager {
    pub log_cache: Arc<LogCache>,
    pub block_cache: Arc<BlockCache>,
    pub transaction_cache: Arc<TransactionCache>,

    blocks_being_fetched: DashSet<u64>,
    inflight: DashMap<u64, InflightFetch>,

    /// Shared chain state - single source of truth for tip/finalized/hash.
    /// This is a shared reference to the same ChainState instance used by
    /// ReorgManager, ScoringEngine, and other components.
    chain_state: Arc<ChainState>,

    pub config: CacheManagerConfig,
    pub last_cleanup_block: Arc<AtomicU64>,
    cleanup_in_progress: Arc<AtomicBool>,

    /// Channel for `FetchGuard` cleanup requests.
    cleanup_tx: mpsc::UnboundedSender<CleanupRequest>,

    /// Temporary storage for cleanup channel receiver (taken when background tasks start).
    cleanup_rx: Arc<std::sync::Mutex<Option<mpsc::UnboundedReceiver<CleanupRequest>>>>,
}
```

**Fields** (lines 44-84 in `cache_manager.rs` - `CacheManager` struct):

**Subcache References**:
- **`log_cache`**: Log cache with inverted indexes for `eth_getLogs` queries
- **`block_cache`**: Two-tier block cache with hot window for recent blocks
- **`transaction_cache`**: Dual-storage cache for transactions and receipts

**Inflight Tracking**:
- **`blocks_being_fetched`**: Lock-free `DashSet` tracking blocks currently being fetched
- **`inflight`**: `DashMap` of inflight fetch operations with semaphore coordination

**Chain State**:
- **`chain_state`**: Shared reference to `ChainState` for unified tip/finalized block tracking. This is the same instance used by `ReorgManager`, `ScoringEngine`, and other components, ensuring all see identical chain state via lock-free reads.

**Cleanup Coordination**:
- **`last_cleanup_block`**: Last block number when cleanup was performed
- **`cleanup_in_progress`**: Atomic flag preventing concurrent cleanup operations
- **`cleanup_tx`**: Channel sender for `FetchGuard` cleanup requests (used in Drop implementation)
- **`cleanup_rx`**: Channel receiver (taken when `start_all_background_tasks` is called)

### InflightFetch

Information about an inflight fetch operation using semaphore-based coordination.

```rust
#[derive(Clone)]
pub struct InflightFetch {
    pub semaphore: Arc<Semaphore>,
    pub started_at: Instant,
}
```

**Fields** (lines 25-30 in `cache_manager.rs` - `InflightFetch` struct):
- **`semaphore`**: Semaphore with permit count of 1 to coordinate exclusive access
- **`started_at`**: Timestamp when fetch started (for stale detection)

### FetchGuard

RAII guard that automatically releases fetch locks when dropped.

```rust
pub struct FetchGuard {
    cleanup_tx: mpsc::UnboundedSender<CleanupRequest>,
    block: u64,
    _permit: OwnedSemaphorePermit,
    has_fetch_lock: bool,
}
```

**Fields** (lines 114-123 in `crates/prism-core/src/cache/manager/fetch_guard.rs`):
- **`cleanup_tx`**: Channel sender for cleanup requests (cheap clone - reference counted)
- **`block`**: Block number being fetched
- **`_permit`**: Owned semaphore permit (dropped after cleanup request is sent)
- **`has_fetch_lock`**: Whether we hold the fetch lock (`blocks_being_fetched` entry)

**Drop Implementation** (lines 158-177 in `fetch_guard.rs`):
- Sends cleanup request through channel to dedicated worker task (allocation-free)
- Worker batches multiple cleanup requests for efficiency
- Non-blocking: never waits for channel capacity
- If channel send fails (worker shutdown), cleanup handled by stale inflight cleanup task

---

## Public API

### Constructor & Background Tasks

#### `new(config: &CacheManagerConfig, chain_state: Arc<ChainState>) -> Result<Self, CacheManagerError>`

Creates a new cache manager with the given configuration and shared chain state.

**Location**: Lines 115-134 in `cache_manager.rs` (`CacheManager::new()`)

**Parameters**:
- `config`: Configuration for the cache manager and all its subcaches
- `chain_state`: Shared chain state instance for unified tip/finality tracking across components

**Returns**: `Result<Self, CacheManagerError>` - The cache manager instance or an initialization error

**Errors**:
- `CacheManagerError::LogCache` - Log cache initialization failed (e.g., chunk_size exceeds maximum of 1024)
- `CacheManagerError::BlockCache` - Block cache initialization failed (e.g., zero capacities)
- `CacheManagerError::TransactionCache` - Transaction cache initialization failed

```rust
use std::sync::Arc;
use prism_core::cache::{CacheManager, CacheManagerConfig, CacheManagerError};
use prism_core::chain::ChainState;

// Create shared chain state
let chain_state = Arc::new(ChainState::new());

// Create cache manager with error handling
let cache_manager = Arc::new(
    CacheManager::new(&CacheManagerConfig::default(), chain_state.clone())
        .expect("Failed to initialize cache manager")
);
```

**Initialization**:
1. Creates subcaches with their respective configurations (may fail with validation errors)
2. Initializes empty fetch tracking structures
3. Stores reference to shared `chain_state` (used for tip/finality reads)
4. Initializes cleanup state

**Chain State Integration**:
The `chain_state` parameter is a **shared reference** to the same `ChainState` instance used by `ReorgManager`, `ScoringEngine`, and other components. This ensures all components see identical chain tip and finalized block values via lock-free atomic reads. See [ChainState documentation](../chain/chain_state.md) for details on the ownership pattern.

#### `start_all_background_tasks(&self, shutdown_tx: &broadcast::Sender<()>)`

Starts all background tasks with graceful shutdown support.

**Location**: Lines 102-119 in `cache_manager.rs` (`CacheManager::start_all_background_tasks()`)

```rust
cache_manager.start_all_background_tasks(&shutdown_tx);
```

**Background Tasks Started**:
1. **Inflight Cleanup** (lines 122-176 in `cache_manager.rs`): Cleans up stale fetch entries every 60 seconds
   - Removes fetches older than 2 minutes
   - Cleans orphaned fetch locks when count exceeds 1000

2. **Stats Updater**: Updates cache statistics periodically

3. **Background Cleanup** (lines 726-761 in `cache_manager.rs`): Runs cache cleanup at configured intervals
   - Time-based: every `cleanup_interval_seconds`
   - Block-based: triggered by `update_tip()` every `cleanup_every_n_blocks`

---

### Fetch Coordination (Internal APIs)

> **Note**: The following methods are internal (`pub(crate)`) and not part of the public API.
> They are exposed as `pub` only when the `benchmarks` feature is enabled.
> Prefer using `try_begin_fetch()` or `begin_fetch_with_timeout()` which provide RAII-based cleanup.

These methods implement semaphore-based fetch deduplication to prevent duplicate upstream requests.

#### `try_acquire_fetch_lock(&self, block_number: u64) -> bool` *(internal)*

Attempts to acquire a fetch lock for a block number using lock-free `DashSet`.

**Returns**: `true` if lock was acquired, `false` if someone else is already fetching

**Performance**: O(1) lock-free operation using `DashSet::insert()`

#### `release_fetch_lock(&self, block_number: u64) -> bool` *(internal)*

Releases a fetch lock when fetch operation completes or fails.

**Returns**: `true` if lock was held and released, `false` otherwise

#### `is_block_being_fetched(&self, block_number: u64) -> bool` *(internal)*

Checks if a block is currently being fetched.

#### `try_begin_fetch(&self, block: u64) -> Option<FetchGuard>`

Attempts to begin a fetch operation, returning a guard if successful.

**Location**: Lines 201-225 in `cache_manager.rs` (`try_begin_fetch()`)

```rust
if let Some(guard) = cache_manager.try_begin_fetch(block_number) {
    // We have exclusive access, fetch from upstream
    // ... fetch operation ...
    // Guard automatically releases on drop
}
```

**Logic**:
1. Gets or creates `InflightFetch` entry with semaphore
2. Tries to acquire semaphore permit (non-blocking)
3. Checks if block is already fully cached
4. Acquires fetch lock if permit acquired
5. Returns `FetchGuard` for RAII cleanup

**Returns**: `Some(FetchGuard)` if fetch should proceed, `None` if already cached or another fetch in progress

#### `begin_fetch_with_timeout(&self, block: u64, timeout: Duration) -> Option<FetchGuard>`

Begins a fetch operation with timeout to prevent indefinite hanging.

**Location**: Lines 227-270 in `cache_manager.rs` (`begin_fetch_with_timeout()`)

**CRITICAL - DashMap Deadlock Prevention** (lines 233-242 in `cache_manager.rs`):

```rust
let sem = {
    let inflight_fetch = self.inflight.entry(block).or_insert_with(|| InflightFetch {
        semaphore: Arc::new(Semaphore::new(1)),
        started_at: Instant::now(),
    });
    inflight_fetch.semaphore.clone()
}; // DashMap entry lock is released here - BEFORE awaiting
```

**Why This Matters**:
- Holding a `DashMap` entry reference across an `await` point causes **deadlocks**
- `DashMap` uses internal sharding with per-shard locks
- Blocking a shard lock prevents other tasks from accessing that shard
- The semaphore is extracted and the entry lock is **explicitly dropped** before awaiting

**Logic**:
1. Extracts semaphore from `DashMap` entry (drops entry lock immediately)
2. Awaits semaphore permit with timeout
3. Checks if block is already fully cached
4. Acquires fetch lock if permit acquired
5. Cleans up stale entry on timeout

**Returns**:
- `Some(FetchGuard)` if fetch should proceed
- `None` if timeout, already cached, or another fetch in progress

> **Note**: The `end_fetch()` and `end_fetch_minimal()` methods have been **removed**. Fetch cleanup is now handled automatically by the `FetchGuard` RAII pattern - when the guard is dropped, all cleanup is performed in its `Drop` implementation. This ensures panic-safe cleanup and eliminates the possibility of forgetting to call cleanup methods.

---

### Log Operations

All log operations delegate to `LogCache` with partial result support.

#### `get_logs(&self, filter: &LogFilter, current_tip: u64) -> (Vec<LogRecord>, Vec<(u64, u64)>)`

Gets logs using the advanced caching system with partial cache results.

**Location**: Lines 286-340 in `cache_manager.rs` (`get_logs()`)

```rust
let filter = LogFilter::new(from_block, to_block)
    .with_address(contract_address)
    .with_topic(0, event_signature);

let (cached_logs, missing_ranges) = cache_manager.get_logs(&filter, current_tip).await;

if !missing_ranges.is_empty() {
    // Fetch missing ranges from upstream
    for (from, to) in missing_ranges {
        let upstream_logs = fetch_from_upstream(from, to).await;
        // ... merge with cached_logs ...
    }
}
```

**Returns**:
- **Tuple**: `(Vec<LogRecord>, Vec<(u64, u64)>)`
  - First element: Logs found in cache
  - Second element: Missing block ranges that need upstream fetch

**Logging** (lines 313-323 in `cache_manager.rs`):
- Full cache coverage: Logs success message with block count
- Partial coverage: Detailed analysis showing cached vs missing percentages
- Verbose logging available with `verbose-logging` feature

#### `get_logs_with_ids(&self, filter: &LogFilter, current_tip: u64) -> (Vec<(LogId, LogRecord)>, Vec<(u64, u64)>)`

Gets logs with their IDs for exact result caching.

**Location**: Lines 324-372 in `cache_manager.rs` (`get_logs_with_ids()`)

**Similar to** `get_logs()` but returns `LogId` alongside each record for caching exact results.

#### `insert_logs_bulk(&self, logs: Vec<(LogId, LogRecord)>)`

Bulk inserts logs into the cache with stats updates.

**Location**: Lines 380-383 in `cache_manager.rs` (`insert_logs_bulk()`)

```rust
cache_manager.insert_logs_bulk(log_records).await;
```

**Performance**: Much more efficient than individual inserts

#### `insert_logs_bulk_no_stats(&self, logs: Vec<(LogId, LogRecord)>)`

Fastest bulk insert without updating stats (stats updated later in batch).

**Location**: Lines 385-388 in `cache_manager.rs` (`insert_logs_bulk_no_stats()`)

```rust
// High-performance path for WebSocket subscriptions
cache_manager.insert_logs_bulk_no_stats(log_records).await;
```

**Usage**: Preferred in hot paths like WebSocket handlers where stats can be updated separately

#### `cache_exact_result(&self, filter: &LogFilter, log_ids: Vec<LogId>)`

Caches exact result for a specific filter query.

**Location**: Lines 410-413 in `cache_manager.rs` (`cache_exact_result()`)

```rust
// After fetching complete result from upstream
cache_manager.cache_exact_result(&filter, log_ids).await;
```

**Purpose**: Enables instant cache hits for repeated identical queries

#### `warm_logs(&self, from_block: u64, to_block: u64, logs: Vec<(LogId, LogRecord)>)`

Warms the log cache with logs for a block range (used during startup).

**Location**: Lines 415-420 in `cache_manager.rs` (`warm_logs()`)

---

### Block Operations

All block operations delegate to `BlockCache` and are synchronous for performance.

#### `get_block_by_number(&self, block_number: u64) -> Option<(BlockHeader, BlockBody)>`

Gets a complete block by number (both header and body).

**Location**: Lines 422-425 in `cache_manager.rs` (`get_block_by_number()`)

```rust
if let Some((header, body)) = cache_manager.get_block_by_number(block_number) {
    // Block found in cache
    let block_hash = header.hash;
    let transactions = body.transactions;
}
```

**Returns**: `Some((BlockHeader, BlockBody))` if fully cached, `None` otherwise

**Performance**: Synchronous O(1) lookup in hot window or O(log n) in LRU

#### `get_block_by_hash(&self, block_hash: &[u8; 32]) -> Option<(BlockHeader, BlockBody)>`

Gets a complete block by hash.

**Location**: Lines 427-431 in `cache_manager.rs` (`get_block_by_hash()`)

#### `get_header_by_number(&self, block_number: u64) -> Option<BlockHeader>`

Gets only the block header by number.

**Location**: Lines 433-436 in `cache_manager.rs` (`get_header_by_number()`)

#### `get_header_by_hash(&self, block_hash: &[u8; 32]) -> Option<BlockHeader>`

Gets only the block header by hash.

**Location**: Lines 438-441 in `cache_manager.rs` (`get_header_by_hash()`)

#### `get_body_by_hash(&self, block_hash: &[u8; 32]) -> Option<BlockBody>`

Gets only the block body by hash.

**Location**: Lines 443-445 in `cache_manager.rs` (`get_body_by_hash()`)

#### `insert_header(&self, header: BlockHeader)`

Inserts a block header into the cache.

**Location**: Lines 447-449 in `cache_manager.rs` (`insert_header()`)

```rust
cache_manager.insert_header(header).await;
```

#### `insert_body(&self, body: BlockBody)`

Inserts a block body into the cache.

**Location**: Lines 451-453 in `cache_manager.rs` (`insert_body()`)

#### `warm_blocks(&self, from_block: u64, to_block: u64, blocks: Vec<(BlockHeader, BlockBody)>)`

Warms the block cache with a range of blocks.

**Location**: Lines 455-465 in `cache_manager.rs` (`warm_blocks()`)

---

### Transaction Operations

All transaction operations delegate to `TransactionCache`.

#### `get_transaction(&self, tx_hash: &[u8; 32]) -> Option<TransactionRecord>`

Gets a transaction by hash (synchronous).

**Location**: Lines 467-470 in `cache_manager.rs` (`get_transaction()`)

```rust
if let Some(tx) = cache_manager.get_transaction(&tx_hash) {
    let from = tx.from;
    let to = tx.to;
}
```

#### `get_receipt(&self, tx_hash: &[u8; 32]) -> Option<ReceiptRecord>`

Gets a transaction receipt by hash (synchronous).

**Location**: Lines 472-475 in `cache_manager.rs` (`get_receipt()`)

#### `get_receipt_with_logs(&self, tx_hash: &[u8; 32]) -> Option<(ReceiptRecord, Vec<LogRecord>)>`

Gets receipt with resolved logs from log cache.

**Location**: Lines 477-483 in `cache_manager.rs` (`get_receipt_with_logs()`)

```rust
if let Some((receipt, logs)) = cache_manager.get_receipt_with_logs(&tx_hash) {
    // Receipt with all logs resolved
    let status = receipt.status;
    let gas_used = receipt.gas_used;
}
```

**Note**: Resolves `LogId` references in receipt to full `LogRecord` entries

#### `insert_receipt(&self, receipt: ReceiptRecord)`

Inserts a single receipt into the cache.

**Location**: Lines 485-487 in `cache_manager.rs` (`insert_receipt()`)

#### `insert_receipts_bulk_no_stats(&self, receipts: Vec<ReceiptRecord>)`

Bulk inserts receipts without stats updates (fastest path).

**Location**: Lines 489-491 in `cache_manager.rs` (`insert_receipts_bulk_no_stats()`)

```rust
// High-performance bulk insert
cache_manager.insert_receipts_bulk_no_stats(receipts).await;
```

#### `warm_transactions(&self, from_block: u64, to_block: u64, transactions: Vec<TransactionRecord>, receipts: Vec<ReceiptRecord>)`

Warms transaction cache with a block range.

**Location**: Lines 493-504 in `cache_manager.rs` (`warm_transactions()`)

---

### Cache Management

#### `invalidate_block(&self, block_number: u64)`

Invalidates a block across all caches (for manual invalidation or reorgs).

**Location**: Lines 506-511 in `cache_manager.rs` (`invalidate_block()`)

```rust
// After detecting reorg
cache_manager.invalidate_block(divergence_block).await;
```

**Invalidation Scope**:
- Removes logs for the block
- Removes block header and body
- Removes all transactions from the block

#### `clear_cache(&self)`

Clears all caches completely (used for authentication changes).

**Location**: Lines 513-519 in `cache_manager.rs` (`clear_cache()`)

```rust
// After API key rotation
cache_manager.clear_cache().await;
```

**Cleanup**:
- Clears all log cache data
- Clears all block cache data
- Clears all transaction cache data
- Resets tip tracking to 0

#### `get_stats(&self) -> CacheStats`

Gets comprehensive cache statistics from all subcaches.

**Location**: Lines 521-541 in `cache_manager.rs` (`get_stats()`)

```rust
let stats = cache_manager.get_stats().await;
println!("Log store size: {}", stats.log_store_size);
println!("Header cache size: {}", stats.header_cache_size);
println!("Transaction cache size: {}", stats.transaction_cache_size);
```

**Statistics Gathered**:
- Log store size and exact result count
- Header and body cache sizes
- Transaction and receipt cache sizes
- Bitmap memory usage
- Hot window size

#### `perform_cleanup(&self)`

Performs cleanup operations on all caches based on current tip and retention policy.

**Location**: Lines 715-747 in `cache_manager.rs` (`perform_cleanup()`)

```rust
cache_manager.perform_cleanup().await;
```

**Cleanup Steps** (lines 716-745):
1. Gets current tip and calculates safe head (tip - 12 blocks)
2. Prunes old log cache chunks older than `retain_blocks`
3. Prunes block cache based on safe head
4. Prunes transaction cache to capacity limits

**Safety**: Uses safe finality depth of 12 blocks to avoid invalidating potentially reorganized data

---

### Tip Management

These methods delegate to the shared `ChainState` instance for unified chain tip tracking across all components.

#### `get_current_tip(&self) -> u64`

Gets the current chain tip by delegating to `chain_state.current_tip()`.

**Location**: Lines 828-832 in `cache_manager.rs` (`get_current_tip()`)

```rust
let current_tip = cache_manager.get_current_tip();
```

**Performance**: Lock-free SeqLock read (P99 < 50ns)

#### `update_tip(&self, block_number: u64)`

Updates the current tip and optionally triggers cleanup.

**Location**: Lines 606-630 in `cache_manager.rs` (`update_tip()`)

```rust
cache_manager.update_tip(new_block_number);
```

**Automatic Cleanup Triggers** (lines 610-628):
- Only updates if new block is higher than current tip
- If `cleanup_every_n_blocks` > 0, triggers cleanup every N blocks
- Uses atomic compare-exchange to prevent concurrent cleanup
- Spawns cleanup as background task
- Updates `last_cleanup_block` after completion

**Example**: With `cleanup_every_n_blocks = 100`, cleanup runs at blocks 100, 200, 300, etc.

> **Note**: For simple tip updates without cleanup triggers, use `chain_state.update_tip_simple()` directly on the shared `ChainState` instance. See [ChainState documentation](../chain/chain_state.md) for details.

---

### Utility Methods

#### `is_range_covered(&self, from_block: u64, to_block: u64) -> bool`

Checks if a block range is fully covered in the cache.

**Location**: Lines 561-569 in `cache_manager.rs` (`is_range_covered()`)

```rust
if cache_manager.is_range_covered(from_block, to_block) {
    // Entire range is cached, no upstream fetch needed
}
```

**Performance**: O(n) where n = number of blocks in range

#### `get_missing_ranges(&self, from_block: u64, to_block: u64) -> Vec<(u64, u64)>`

Gets missing block ranges within a requested range.

**Location**: Lines 571-598 in `cache_manager.rs` (`get_missing_ranges()`)

```rust
let missing = cache_manager.get_missing_ranges(1000, 2000);
// Returns: [(1050, 1099), (1200, 1299)] if those ranges are missing
```

**Returns**: Vector of `(start, end)` tuples representing missing ranges

**Algorithm**: Single-pass scan through range, coalescing consecutive missing blocks

#### `is_block_fully_cached(&self, block_number: u64) -> bool`

Checks if a block is fully cached (has both header and body).

**Location**: Lines 672-676 in `cache_manager.rs` (`is_block_fully_cached()`)

```rust
if cache_manager.is_block_fully_cached(block_number) {
    // Can skip fetch, block is complete in cache
}
```

**Usage**: Used by `try_begin_fetch()` to avoid unnecessary fetch operations

#### `get_block_range_from_logs(&self, params: &serde_json::Value) -> Option<BlockRange>`

Parses block range from `eth_getLogs` parameters.

**Location**: Lines 642-670 in `cache_manager.rs` (`get_block_range_from_logs()`)

```rust
if let Some(range) = cache_manager.get_block_range_from_logs(&params) {
    let from = range.from;
    let to = range.to;
}
```

**Parsing**: Handles both hex (`0x1234`) and decimal formats

---

## Key Algorithms

### Inflight Fetch Coordination

**Goal**: Prevent duplicate upstream requests for the same block

**Mechanism** (lines 193-270 in `cache_manager.rs`):
1. Each block has an `InflightFetch` entry with a semaphore (permit count = 1)
2. First requester acquires the semaphore permit via `try_acquire_owned()` or `acquire_owned()`
3. Subsequent requesters either:
   - Get `None` immediately (with `try_begin_fetch`)
   - Wait up to timeout duration (with `begin_fetch_with_timeout`)
4. When first requester completes, `FetchGuard` is dropped, releasing the permit
5. Next waiting requester (if any) can now proceed

**Benefits**:
- Eliminates duplicate upstream requests
- Reduces upstream rate limit pressure
- Improves cache hit rate by coordinating fills

**Stale Detection** (lines 122-176 in `cache_manager.rs`):
- Background task runs every 60 seconds
- Removes inflight fetches older than 2 minutes
- Cleans orphaned fetch locks when count exceeds 1000

### DashMap Deadlock Prevention

**Critical Pattern** (lines 233-242):

```rust
// CORRECT: Extract Arc before awaiting
let sem = {
    let entry = self.inflight.entry(block).or_insert_with(...);
    entry.semaphore.clone()
}; // Entry lock released here

// Now safe to await
let permit = tokio::time::timeout(timeout, sem.acquire_owned()).await;
```

**Why This Is Required**:
- `DashMap` uses internal per-shard locks
- Holding an entry reference keeps the shard lock held
- If task is suspended at `.await` while holding shard lock, other tasks cannot access that shard
- Results in deadlock when multiple tasks need the same shard

**Incorrect Pattern** (would deadlock):
```rust
// WRONG: Holding entry reference across await
let entry = self.inflight.entry(block).or_insert_with(...);
let permit = entry.semaphore.acquire_owned().await; // DEADLOCK!
```

### Automatic Cleanup Triggers

**Two Mechanisms**:

1. **Time-Based** (lines 678-713 in `cache_manager.rs`):
   - Runs every `cleanup_interval_seconds` (default: 300s)
   - Started via `start_background_cleanup_tasks()`
   - Gracefully shuts down on shutdown signal

2. **Block-Based** (lines 610-628 in `cache_manager.rs`):
   - Triggered by `update_tip()` every `cleanup_every_n_blocks`
   - Uses atomic compare-exchange to prevent concurrent cleanup
   - Spawns cleanup as background task
   - Updates `last_cleanup_block` after completion

**Example Timeline** (with `cleanup_every_n_blocks = 100`):
```
Block 0:    No cleanup
Block 99:   No cleanup
Block 100:  Cleanup triggered (100 - 0 >= 100)
Block 150:  No cleanup
Block 200:  Cleanup triggered (200 - 100 >= 100)
```

### Partial Cache Results

**Traditional Approach** (all-or-nothing):
```
Query: blocks 1000-2000
Cache: Has 1000-1099, missing 1100-2000
Return: Cache miss → Fetch entire 1000-2000 from upstream
```

**Prism's Approach** (partial results):
```
Query: blocks 1000-2000
Cache: Has 1000-1099, missing 1100-2000
Return: (logs from 1000-1099, [(1100, 2000)])
Proxy: Merges cached logs + fetches only 1100-2000 from upstream
```

**Implementation** (lines 275-323 in `cache_manager.rs`):
1. `LogCache::query_logs()` returns `(log_ids, missing_ranges)`
2. Resolve `log_ids` to full `LogRecord` entries
3. Return both cached logs and missing ranges to caller
4. Proxy handler fetches only missing ranges and merges results

**Benefits**:
- Reduces upstream bandwidth by 50-90% for overlapping queries
- Faster response times using cached data
- Lower upstream rate limit consumption

---

## Background Tasks

### 1. Inflight Cleanup Task

**Purpose**: Prevent memory leaks from stale fetch entries

**Location**: Lines 122-176 in `cache_manager.rs` (inflight cleanup background task)

**Configuration**:
- Runs every 60 seconds
- Stale threshold: 2 minutes

**Cleanup Logic**:
1. Iterates through all inflight entries
2. Removes entries older than 2 minutes (with warning log)
3. If `blocks_being_fetched.len() > 1000`, cleans orphaned fetch locks
4. Orphaned lock = exists in `blocks_being_fetched` but not in `inflight`

**Shutdown**: Gracefully exits on shutdown signal

### 2. Stats Updater Task

**Purpose**: Periodically update cache statistics without blocking operations

**Started By**: `start_background_stats_updater()` (line 406 in `cache_manager.rs`)

**Delegates To**: `LogCache::start_background_stats_updater()`

### 3. Background Cleanup Task

**Purpose**: Periodic cache cleanup to maintain memory bounds

**Location**: Lines 678-713 in `cache_manager.rs` (background cleanup task)

**Configuration**:
- Runs every `cleanup_interval_seconds` (default: 300s)
- Disabled if `enable_auto_cleanup = false`

**Operations Performed** (lines 716-745 in `cache_manager.rs` `perform_cleanup()`):
1. Calculate safe head (current_tip - 12 blocks)
2. Prune log cache old chunks
3. Prune block cache by safe head
4. Prune transaction cache to capacity limits

**Error Handling**: Logs errors but continues running

**Shutdown**: Gracefully exits on shutdown signal

---

## Thread Safety

### Concurrent Access Patterns

**Lock-Free Operations**:
- `blocks_being_fetched` (`DashSet`): Lock-free insert/remove/contains
- `current_tip` (`AtomicU64`): Lock-free load/store/compare-exchange
- `last_cleanup_block` (`AtomicU64`): Lock-free load/store
- `cleanup_in_progress` (`AtomicBool`): Lock-free flag for cleanup coordination

**Concurrent-Safe Maps**:
- `inflight` (`DashMap`): Concurrent hashmap with per-shard locking
- All subcaches use `DashMap` for concurrent access

**Lock Ordering**:
- No explicit lock ordering required due to lock-free design
- `DashMap` entry references must be **dropped before awaiting** (see DashMap Deadlock Prevention)

**Clone Semantics**:
- `CacheManager` implements `Clone` (all fields are `Arc`-wrapped)
- Cloning is cheap (just incrementing ref counts)
- Safe to clone across threads/tasks

### Atomic Operations

**Memory Ordering**:
- `Ordering::Acquire` for reads: Ensures reads happen after previous writes
- `Ordering::Release` for writes: Ensures writes are visible to subsequent reads
- `Ordering::AcqRel` for compare-exchange: Both acquire and release semantics

**Example** (lines 607-609 in `cache_manager.rs`):
```rust
let current = self.current_tip.load(Ordering::Acquire);
if block_number > current {
    self.current_tip.store(block_number, Ordering::Release);
}
```

---

## Usage Examples

### Example 1: Basic Log Query with Partial Results

```rust
use prism_core::cache::{CacheManager, CacheManagerConfig};
use prism_core::cache::types::LogFilter;
use prism_core::chain::ChainState;
use std::sync::Arc;

let chain_state = Arc::new(ChainState::new());
let cache_manager = Arc::new(
    CacheManager::new(&CacheManagerConfig::default(), chain_state)
        .expect("Failed to initialize cache manager")
);

// Query logs with filter
let filter = LogFilter::new(1000, 2000)
    .with_address(contract_address)
    .with_topic(0, event_signature);

let current_tip = 5000;
let (cached_logs, missing_ranges) = cache_manager.get_logs(&filter, current_tip).await;

// Process cached logs immediately
for log in &cached_logs {
    println!("Cached log: {:?}", log);
}

// Fetch missing ranges from upstream
if !missing_ranges.is_empty() {
    for (from_block, to_block) in missing_ranges {
        let upstream_logs = fetch_from_upstream(from_block, to_block).await;

        // Convert to cache format
        let log_records: Vec<(LogId, LogRecord)> = upstream_logs
            .into_iter()
            .map(|log| (LogId::new(log.block_number, log.log_index), log))
            .collect();

        // Insert into cache for future queries
        cache_manager.insert_logs_bulk_no_stats(log_records).await;
    }
}
```

**Source**: Pattern from `crates/prism-core/src/proxy/handlers/logs.rs:56`

### Example 2: Block Fetch with Deduplication

```rust
let block_number = 12345;

// Try to begin fetch with timeout
if let Some(guard) = cache_manager.begin_fetch_with_timeout(
    block_number,
    Duration::from_secs(5)
).await {
    // We have exclusive access to fetch this block

    // Check cache first (might have been filled while waiting)
    if let Some((header, body)) = cache_manager.get_block_by_number(block_number) {
        return Ok((header, body));
    }

    // Fetch from upstream
    let (header, body) = fetch_block_from_upstream(block_number).await?;

    // Insert into cache
    cache_manager.insert_header(header.clone()).await;
    cache_manager.insert_body(body.clone()).await;

    // Guard automatically releases on drop
    Ok((header, body))
} else {
    // Another task is fetching or timeout occurred
    // Wait a bit and check cache
    tokio::time::sleep(Duration::from_millis(100)).await;

    if let Some(block) = cache_manager.get_block_by_number(block_number) {
        Ok(block)
    } else {
        Err("Fetch timeout or failed")
    }
}
```

**Source**: Pattern from `crates/prism-core/src/proxy/handlers/blocks.rs:60,113`

### Example 3: WebSocket Block Caching with Tip Update

```rust
// WebSocket handler receives new block
async fn on_new_block(cache_manager: Arc<CacheManager>, block: Block) {
    let block_number = block.number;

    // Extract header and body
    let header = BlockHeader {
        hash: block.hash,
        number: block.number,
        parent_hash: block.parent_hash,
        // ... other fields
    };

    let body = BlockBody {
        hash: block.hash,
        transactions: block.transactions.iter().map(|tx| tx.hash).collect(),
    };

    // Insert block data
    cache_manager.insert_header(header).await;
    cache_manager.insert_body(body).await;

    // Extract and insert receipts (bulk, no stats)
    let receipts: Vec<ReceiptRecord> = block.receipts
        .into_iter()
        .map(|r| ReceiptRecord { /* ... */ })
        .collect();
    cache_manager.insert_receipts_bulk_no_stats(receipts).await;

    // Extract and insert logs (bulk, no stats)
    let log_records: Vec<(LogId, LogRecord)> = block.logs
        .into_iter()
        .map(|log| (LogId::new(block_number, log.log_index), log))
        .collect();
    cache_manager.insert_logs_bulk_no_stats(log_records).await;

    // Update tip (triggers cleanup if configured)
    cache_manager.update_tip(block_number);
}
```

**Source**: Pattern from `crates/prism-core/src/upstream/websocket.rs:716` and `crates/prism-core/src/proxy/handlers/transactions.rs:178`

### Example 4: Transaction Receipt Lookup

```rust
let tx_hash: [u8; 32] = /* transaction hash */;

// Try to get receipt with resolved logs
if let Some((receipt, logs)) = cache_manager.get_receipt_with_logs(&tx_hash) {
    // Receipt found in cache with logs resolved
    println!("Status: {}", receipt.status);
    println!("Gas used: {}", receipt.gas_used);
    println!("Logs: {}", logs.len());

    for log in logs {
        println!("  Log address: {:02x?}", log.address);
        println!("  Topics: {:?}", log.topics);
    }
} else {
    // Not in cache, fetch from upstream
    let receipt = fetch_receipt_from_upstream(&tx_hash).await?;

    // Insert into cache
    cache_manager.insert_receipt(receipt).await;
}
```

### Example 5: Cache Initialization with Background Tasks

```rust
use tokio::sync::broadcast;
use prism_core::chain::ChainState;
use std::sync::Arc;

// Create shared chain state
let chain_state = Arc::new(ChainState::new());

// Create cache manager with custom configuration
let cache_manager = Arc::new(
    CacheManager::new(
        &CacheManagerConfig {
            retain_blocks: 5000,
            enable_auto_cleanup: true,
            cleanup_interval_seconds: 300,
            cleanup_every_n_blocks: 100,
            ..Default::default()
        },
        chain_state.clone(),
    )
    .expect("Failed to initialize cache manager")
);

// Create shutdown channel
let (shutdown_tx, _) = broadcast::channel(1);

// Start all background tasks
cache_manager.start_all_background_tasks(&shutdown_tx);

// Warm cache with recent blocks on startup
let recent_blocks = fetch_recent_blocks(latest_block - 1000, latest_block).await?;
cache_manager.warm_recent(
    latest_block - 1000,
    latest_block,
    recent_blocks.blocks,
    recent_blocks.transactions,
    recent_blocks.receipts,
    recent_blocks.logs,
).await;

// Set initial tip
cache_manager.update_tip(latest_block);

// Later: graceful shutdown
shutdown_tx.send(()).ok();
```

### Example 6: Manual Cache Cleanup

```rust
// Periodic manual cleanup
loop {
    tokio::time::sleep(Duration::from_secs(600)).await; // Every 10 minutes

    // Get stats before cleanup
    let stats_before = cache_manager.get_stats().await;
    println!("Before cleanup:");
    println!("  Log store: {}", stats_before.log_store_size);
    println!("  Headers: {}", stats_before.header_cache_size);
    println!("  Bodies: {}", stats_before.body_cache_size);

    // Perform cleanup
    cache_manager.perform_cleanup().await;

    // Get stats after cleanup
    let stats_after = cache_manager.get_stats().await;
    println!("After cleanup:");
    println!("  Log store: {}", stats_after.log_store_size);
    println!("  Headers: {}", stats_after.header_cache_size);
    println!("  Bodies: {}", stats_after.body_cache_size);
}
```

---

## Performance Characteristics

### Time Complexity

**Fetch Coordination**:
- `try_acquire_fetch_lock()`: O(1) - lock-free `DashSet` insert
- `try_begin_fetch()`: O(1) - semaphore try_acquire
- `begin_fetch_with_timeout()`: O(1) amortized + timeout wait

**Block Operations**:
- `get_block_by_number()`: O(1) in hot window, O(log n) in LRU
- `get_block_by_hash()`: O(1) `DashMap` lookup
- `insert_header/body()`: O(1) amortized (may trigger LRU eviction)

**Log Operations**:
- `get_logs()`: O(bitmap intersection size) - depends on filter selectivity
- `insert_logs_bulk()`: O(n) where n = number of logs

**Transaction Operations**:
- `get_transaction/receipt()`: O(1) `DashMap` lookup
- `insert_receipt()`: O(1) amortized (may trigger LRU eviction)

### Memory Usage

**Per-Block Overhead**:
- `InflightFetch`: ~56 bytes (Arc<Semaphore> + Instant)
- `blocks_being_fetched`: ~8 bytes per entry (u64 in DashSet)

**Cache Capacities** (with defaults):
- Log store: Unbounded (controlled by chunk pruning)
- Block headers: 10,000 entries (configurable)
- Block bodies: 10,000 entries (configurable)
- Transactions: 50,000 entries (configurable)
- Receipts: 50,000 entries (configurable)
- Exact results: 10,000 filters (configurable)

**Memory Estimation** (rough):
- Header: ~500 bytes each → 10k headers ≈ 5 MB
- Body: ~100 bytes + tx hashes → 10k bodies ≈ 5-10 MB
- Transaction: ~200 bytes each → 50k tx ≈ 10 MB
- Receipt: ~150 bytes each → 50k receipts ≈ 7.5 MB
- **Total**: ~30-40 MB for block/tx caches + log cache bitmaps

### Optimization Notes

**Hot Paths**:
- Use `*_no_stats()` variants in WebSocket handlers to defer stats updates
- Batch inserts with `insert_logs_bulk()` instead of individual inserts
- Check cache synchronously before initiating upstream fetch

**Cleanup Optimization**:
- Atomic flag prevents concurrent cleanup spawns
- Background task defers cleanup to avoid blocking request path
- LRU eviction happens automatically on insert

**Lock Contention Avoidance**:
- Lock-free `DashSet` for fetch tracking
- `DashMap` provides per-shard locking (better than single `RwLock`)
- Minimal work in `FetchGuard::drop()` to avoid Drop contention

---

## Related Documentation

- **LogCache**: `crates/prism-core/src/cache/log_cache.rs` - Inverted index log caching
- **BlockCache**: `crates/prism-core/src/cache/block_cache.rs` - Two-tier block caching
- **TransactionCache**: `crates/prism-core/src/cache/transaction_cache.rs` - Dual-storage tx/receipt cache
- **Cache Types**: `crates/prism-core/src/cache/types.rs` - Shared cache data structures
- **Proxy Handlers**: `crates/prism-core/src/proxy/handlers/` - Cache usage in RPC handlers

---

## Summary

The `CacheManager` provides a high-performance, thread-safe caching layer for Ethereum RPC data with:

- **Partial Cache Results**: Returns both cached data and missing ranges for efficient overlapping queries
- **Fetch Deduplication**: Semaphore-based coordination prevents duplicate upstream requests
- **Background Tasks**: Automatic cleanup, stats updates, and stale fetch detection
- **Lock-Free Design**: Atomic operations and `DashSet`/`DashMap` for high concurrency
- **Flexible Configuration**: Tunable cache sizes, cleanup intervals, and retention policies
- **Production-Ready**: Handles edge cases like deadlocks, stale fetches, and concurrent cleanup

Key takeaways:
1. Always drop `DashMap` entry references before awaiting (line 233-242)
2. Use `*_no_stats()` variants in hot paths for better performance
3. Partial cache results reduce upstream bandwidth by 50-90%
4. Background tasks handle cleanup automatically with graceful shutdown
5. All operations are thread-safe and can be called from multiple tasks concurrently
