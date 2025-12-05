# Reorg Manager

## Overview

The `ReorgManager` is a critical component responsible for detecting and handling Ethereum blockchain reorganizations (reorgs) to maintain cache consistency. When the Ethereum chain reorganizes - typically due to temporary forks near the chain tip - the reorg manager invalidates affected cached data to prevent serving stale or incorrect information.

The manager operates by:
- Tracking the current chain tip and recent block hashes
- Detecting when a block hash changes at a known block height (reorg signal)
- Finding the divergence point where chains split
- Invalidating all affected cached blocks
- Maintaining a configurable "safe depth" window for blocks that might reorganize

**File Location**: `crates/prism-core/src/cache/reorg_manager.rs`

## Configuration

### ReorgManagerConfig

Configuration structure for reorg detection and handling behavior.

**Definition**: Lines 11-16

```rust
pub struct ReorgManagerConfig {
    pub safety_depth: u64,
    pub max_reorg_depth: u64,
    pub reorg_detection_threshold: u64,
    pub coalesce_window_ms: u64,
}
```

#### Fields

- **`safety_depth: u64`**
  - Number of blocks from the chain tip considered "unsafe" and subject to reorganization
  - Default: `12` blocks (approximately 2.4 minutes on Ethereum mainnet)
  - Blocks within this depth from the tip should be treated with caution
  - The "safe head" is calculated as: `current_tip - safety_depth`

- **`max_reorg_depth: u64`**
  - Maximum depth to track block hashes in history
  - Default: `100` blocks
  - Determines how far back the manager can detect reorgs
  - Also controls automatic cleanup of old block hash history
  - Memory usage scales with this value

- **`reorg_detection_threshold: u64`**
  - Threshold for triggering reorg detection logic
  - Default: `2` blocks
  - Currently not actively used in the implementation

- **`coalesce_window_ms: u64`**
  - Time window for batching rapid reorgs (milliseconds)
  - Default: `100` ms
  - Multiple reorgs within this window are coalesced to prevent cache thrashing
  - Uses monotonic time (Instant) to be immune to system clock adjustments

#### Default Configuration

**Implementation**: Lines 38-46

```rust
impl Default for ReorgManagerConfig {
    fn default() -> Self {
        Self {
            safety_depth: 12,
            max_reorg_depth: 100,
            reorg_detection_threshold: 2,
            coalesce_window_ms: 100,
        }
    }
}
```

## Data Structures

### ReorgManager

The main reorg management structure using shared `ChainState` and lock-free patterns for high-performance concurrent access.

**Definition**: Lines 132-160

```rust
pub struct ReorgManager {
    config: ReorgManagerConfig,

    /// Shared chain state - single source of truth for tip/finalized/hash.
    /// ReorgManager is the **primary writer** to ChainState.
    chain_state: Arc<ChainState>,

    block_hash_history: Arc<DashMap<u64, [u8; 32]>>,
    cache_manager: Arc<CacheManager>,

    /// Coordination lock to prevent races between WebSocket updates and
    /// health check rollback detection.
    update_lock: Mutex<()>,

    /// Reorg coalescer for handling reorg storms
    coalescer: ReorgCoalescer,
}
```

#### Fields

- **`config: ReorgManagerConfig`**
  - Configuration controlling reorg detection behavior
  - Immutable after construction

- **`chain_state: Arc<ChainState>`**
  - Shared chain state for unified tip/finalized block tracking
  - **ReorgManager is the primary writer** - it updates tip, hash, and finalized block
  - Other components (`CacheManager`, `ScoringEngine`) read from the same instance
  - Uses lock-free SeqLock pattern for P99 < 50ns reads
  - See [ChainState documentation](../chain/chain_state.md) for ownership pattern details

- **`block_hash_history: Arc<DashMap<u64, [u8; 32]>>`**
  - Concurrent hash map tracking recent block hashes
  - Key: block number, Value: block hash
  - Uses DashMap for lock-free concurrent access
  - Automatically cleaned up when size exceeds `max_reorg_depth`
  - Critical for reorg detection and divergence point finding

- **`cache_manager: Arc<CacheManager>`**
  - Reference to the cache manager for invalidating affected blocks
  - Shared ownership via Arc allows async operations
  - Used to invalidate block, transaction, and log caches during reorgs

- **`update_lock: Mutex<()>`**
  - Coordination lock to serialize tip updates from WebSocket and health check paths
  - Ensures atomic handling of reorg detection and cache invalidation
  - Prevents races between concurrent update sources

- **`coalescer: ReorgCoalescer`**
  - Batches rapid reorgs within a time window (default: 100ms)
  - Prevents cache thrashing during "reorg storms" (5+ rapid reorgs)
  - Uses monotonic time (Instant) to be immune to system clock adjustments

### ReorgStats

Statistical information about reorg management state.

**Definition**: Lines 223-230

```rust
pub struct ReorgStats {
    pub current_tip: u64,
    pub safe_head: u64,
    pub unsafe_window_size: u64,
    pub history_size: usize,
    pub max_reorg_depth: u64,
}
```

#### Fields

- **`current_tip: u64`** - Current blockchain tip block number
- **`safe_head: u64`** - Block number of the safe head (`current_tip - safety_depth`)
- **`unsafe_window_size: u64`** - Size of the unsafe window (typically equals `safety_depth`)
- **`history_size: usize`** - Number of block hashes currently stored in history
- **`max_reorg_depth: u64`** - Maximum configured reorg tracking depth

## Public API

### Constructor

#### `new(config, chain_state, cache_manager) -> Self`

Creates a new reorg manager instance with shared chain state.

**Signature**: Lines 162-178

```rust
pub fn new(
    config: ReorgManagerConfig,
    chain_state: Arc<ChainState>,
    cache_manager: Arc<CacheManager>,
) -> Self
```

**Parameters**:
- `config: ReorgManagerConfig` - Configuration for reorg management
- `chain_state: Arc<ChainState>` - Shared chain state instance (same instance used by `CacheManager`, `ScoringEngine`, etc.)
- `cache_manager: Arc<CacheManager>` - Shared cache manager reference

**Returns**: New `ReorgManager` instance with initialized state

**Behavior**:
- Stores reference to shared `chain_state` (tip/hash reads delegated to it)
- Creates empty block hash history
- Stores Arc reference to cache manager
- Initializes coordination lock for update serialization
- Creates reorg coalescer with configured window

### Tip Management

#### `update_tip(&self, block_number, block_hash)`

Updates the chain tip and detects potential reorganizations.

**Signature**: Lines 48-99

```rust
pub async fn update_tip(&self, block_number: u64, block_hash: [u8; 32])
```

**Parameters**:
- `block_number: u64` - New block number
- `block_hash: [u8; 32]` - Block hash at this height

**Behavior**:

This is the most critical method in the reorg manager with two execution paths:

**Fast Path** (Lines 52-58):
- Used when `block_number > current_tip` (normal progression)
- Atomically updates tip without acquiring write lock
- Updates head hash with minimal lock time
- Inserts block hash into history
- Optimized for the common case of sequential block arrivals

**Slow Path** (Lines 60-83):
- Used when `block_number <= current_tip` (potential reorg)
- Acquires write lock to check for hash mismatch
- Detects reorg when hash differs at same block number
- Calls `handle_reorg()` to process the reorganization
- Releases lock before expensive reorg handling
- Reacquires lock after reorg to update state

**Automatic Cleanup** (Lines 85-98):
- Removes block hashes older than `max_reorg_depth`
- Runs outside of locks to minimize contention
- Collects stale entries and removes them in batch
- Prevents unbounded memory growth

#### `get_current_tip(&self) -> u64`

Returns the current blockchain tip block number.

**Signature**: Lines 174-177

```rust
pub fn get_current_tip(&self) -> u64
```

**Returns**: Current tip block number

**Thread Safety**: Lock-free synchronous operation using atomic load with Acquire ordering

#### `get_current_head_hash(&self) -> [u8; 32]`

Returns the block hash at the current tip.

**Signature**: Lines 179-182

```rust
pub async fn get_current_head_hash(&self) -> [u8; 32]
```

**Returns**: 32-byte block hash

**Note**: Async because it requires acquiring read lock on `RwLock`

### Safety Methods

#### `get_safe_head(&self) -> u64`

Calculates the "safe head" - the block number below which reorgs are considered unlikely.

**Signature**: Lines 160-164

```rust
pub fn get_safe_head(&self) -> u64
```

**Returns**: Block number of safe head (`current_tip - safety_depth`)

**Behavior**:
- Uses saturating subtraction to prevent underflow
- Returns 0 if tip is less than safety depth
- Synchronous lock-free operation

**Usage**: Determine which blocks are safe to cache permanently vs. need revalidation

#### `is_unsafe_block(&self, block_number) -> bool`

Checks if a block is within the unsafe window and subject to potential reorganization.

**Signature**: Lines 166-171

```rust
pub fn is_unsafe_block(&self, block_number: u64) -> bool
```

**Parameters**:
- `block_number: u64` - Block number to check

**Returns**: `true` if block is in unsafe window, `false` if safe

**Behavior**: Returns `block_number > get_safe_head()`

#### `validate_block_hash(&self, block_number, block_hash) -> bool`

Validates that a block hash matches the known hash for that block number.

**Signature**: Lines 184-205

```rust
pub async fn validate_block_hash(&self, block_number: u64, block_hash: [u8; 32]) -> bool
```

**Parameters**:
- `block_number: u64` - Block number to validate
- `block_hash: [u8; 32]` - Hash to validate

**Returns**: `true` if hash is valid, `false` otherwise

**Behavior**:
- For unsafe blocks at tip: compares against current head hash
- For unsafe blocks not at tip: logs warning (cannot validate without additional context)
- For safe blocks: looks up in block hash history
- Returns `false` if block not found in history

### Statistics

#### `get_reorg_stats(&self) -> ReorgStats`

Returns statistical information about reorg management state.

**Signature**: Lines 207-219

```rust
pub fn get_reorg_stats(&self) -> ReorgStats
```

**Returns**: `ReorgStats` structure with current state information

**Usage**: Monitoring, metrics, debugging reorg detection behavior

## Key Algorithms

### Reorg Detection

**Implementation**: Lines 48-83 in `update_tip()`

The reorg detection algorithm works by comparing block hashes at the same block height:

1. **Normal Case**: When a new block arrives with `block_number > current_tip`
   - No reorg possible - this is normal chain progression
   - Fast path: update tip and hash atomically

2. **Potential Reorg**: When a block arrives at `block_number == current_tip`
   - Acquire write lock on `current_head_hash`
   - Compare new hash with stored hash
   - If hashes differ: **reorg detected**
   - Log detection event with old and new hashes
   - Trigger reorg handling

3. **Hash Comparison**: Uses direct array comparison
   - Block hashes are 32-byte arrays `[u8; 32]`
   - Equality check is fast and deterministic
   - No hash collisions possible (cryptographic hashes)

### Divergence Point Finding

**Implementation**: Lines 128-152 in `find_divergence_point()`

The divergence point algorithm searches backward through block hash history to find where chains split:

```rust
fn find_divergence_point(
    &self,
    block_number: u64,
    old_hash: [u8; 32],
    new_hash: [u8; 32],
) -> u64
```

**Algorithm**:

1. **Search Range**:
   - Start: `block_number - max_reorg_depth` (saturating subtraction)
   - End: `block_number` (the reorg point)
   - Search direction: backward (from newer to older blocks)

2. **Hash Matching**:
   - Iterate through stored block hashes in reverse
   - Skip the reorg block itself (line 139-141)
   - Look for block where stored hash matches old chain's hash
   - When found: divergence is at `block_num + 1`

3. **Fallback**:
   - If no match found, return `search_start`
   - This represents maximum possible divergence depth

**Complexity**: O(max_reorg_depth) worst case, but typically O(1-10) for shallow reorgs

### Cache Invalidation

**Implementation**: Lines 101-126 in `handle_reorg()`

When a reorg is detected, all affected blocks must be invalidated:

```rust
async fn handle_reorg(&self, block_number: u64, old_hash: [u8; 32], new_hash: [u8; 32])
```

**Algorithm**:

1. **Find Divergence**: Call `find_divergence_point()` to locate where chains split

2. **Invalidate Range**:
   - For each block from `divergence_block` to `block_number` (inclusive)
   - Call `invalidate_block_internal(block_num)`
   - Track invalidated blocks in `ReorgInfo` structure

3. **Cache Manager Delegation**:
   - `invalidate_block_internal()` calls `cache_manager.invalidate_block()`
   - Cache manager handles actual cache entry removal
   - Invalidates: block cache, transaction cache, log cache, receipt cache

4. **Logging**:
   - Info log when reorg starts
   - Info log when divergence found
   - Info log with invalidation summary (block count and range)
   - Warning if divergence point cannot be determined

### History Cleanup

**Implementation**: Lines 85-98 in `update_tip()`

Automatic memory management prevents unbounded history growth:

**Trigger**: When `block_hash_history.len() > max_reorg_depth`

**Algorithm**:
1. Collect all block numbers older than `current_block - max_reorg_depth`
2. Build vector of block numbers to remove
3. Iterate and remove each stale entry from DashMap
4. Executed outside of locks to minimize contention

**Performance**: O(n) where n = number of stale entries (typically small)

## Thread Safety

The `ReorgManager` is designed for high-concurrency access with minimal lock contention:

### Lock-Free Patterns

**AtomicU64 for Tip Tracking** (Lines 28, 50, 54, 162, 176, 209):
- Uses `Arc<AtomicU64>` for `current_tip`
- Lock-free reads with `Ordering::Acquire`
- Lock-free writes with `Ordering::Release`
- Provides sequentially consistent ordering
- Enables synchronous API without blocking

**DashMap for Block History** (Lines 31, 57, 82, 89-96, 138, 199):
- Concurrent hash map with internal sharding
- Lock-free reads and writes via internal synchronization
- No external locking required
- Supports high-throughput concurrent access
- Automatic cleanup doesn't block other operations

### RwLock Usage

**Current Head Hash** (Lines 29, 55, 61, 76, 181, 187):
- Uses `Arc<RwLock<[u8; 32]>>` for block hash
- Read lock: multiple concurrent readers
- Write lock: exclusive access during updates
- Held for minimal duration (released early on lines 58, 71)
- Reacquired after expensive operations (line 75)

### Lock Ordering

To prevent deadlocks, locks are acquired in consistent order:
1. Never hold multiple write locks simultaneously
2. Release locks before calling async functions (cache invalidation)
3. Reacquire locks only when necessary
4. DashMap operations are self-contained (no external lock dependencies)

### Async Safety

- All async methods (`update_tip`, `handle_reorg`, `get_current_head_hash`, `validate_block_hash`)
- Compatible with Tokio async runtime
- No blocking operations within lock critical sections
- Cache invalidation happens asynchronously without blocking tip updates

## Usage Examples

### Typical Usage with WebSocket Block Subscriptions

```rust
use prism_core::cache::{CacheManager, CacheManagerConfig};
use prism_core::cache::reorg_manager::{ReorgManager, ReorgManagerConfig};
use prism_core::chain::ChainState;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // Initialize shared chain state - single instance shared by all components
    let chain_state = Arc::new(ChainState::new());

    // Initialize cache manager with shared chain state
    let cache_config = CacheManagerConfig::default();
    let cache_manager = Arc::new(
        CacheManager::new(&cache_config, chain_state.clone())
            .expect("Failed to initialize cache manager")
    );

    // Initialize reorg manager with shared chain state
    // ReorgManager is the PRIMARY WRITER to chain_state
    let reorg_config = ReorgManagerConfig {
        safety_depth: 12,           // 12 blocks (~2.4 min on Ethereum)
        max_reorg_depth: 100,       // Track up to 100 blocks back
        reorg_detection_threshold: 2,
        coalesce_window_ms: 100,    // Batch reorgs within 100ms
    };
    let reorg_manager = Arc::new(ReorgManager::new(
        reorg_config,
        chain_state.clone(),  // Same chain_state instance
        cache_manager.clone()
    ));

    // In your WebSocket block subscription handler:
    // Assume we receive new block notifications via WebSocket
    let block_number = 18_000_000_u64;
    let block_hash = [0x12_u8; 32]; // Example block hash

    // Update tip - this will detect reorgs automatically
    reorg_manager.update_tip(block_number, block_hash).await;

    // Check if a block is safe to cache permanently
    let block_to_check = 17_999_980_u64;
    if reorg_manager.is_unsafe_block(block_to_check) {
        println!("Block {} is in unsafe window - may reorg", block_to_check);
    } else {
        println!("Block {} is safe - can cache permanently", block_to_check);
    }

    // Get current safe head
    let safe_head = reorg_manager.get_safe_head();
    println!("Safe head: {}", safe_head);

    // Get statistics for monitoring
    let stats = reorg_manager.get_reorg_stats();
    println!("Reorg Stats:");
    println!("  Current tip: {}", stats.current_tip);
    println!("  Safe head: {}", stats.safe_head);
    println!("  Unsafe window: {} blocks", stats.unsafe_window_size);
    println!("  History size: {} blocks", stats.history_size);
}
```

### Validating Block Hashes

```rust
// Validate a block hash before using cached data
let block_num = 17_999_990_u64;
let received_hash = [0xab_u8; 32];

if reorg_manager.validate_block_hash(block_num, received_hash).await {
    println!("Block hash is valid - safe to use cached data");
} else {
    println!("Block hash mismatch - possible reorg, fetch fresh data");
}
```

### Integration with Cache Queries

```rust
// Before serving cached block data:
let requested_block = 17_999_995_u64;

if reorg_manager.is_unsafe_block(requested_block) {
    // Block is in unsafe window - validate or fetch fresh
    let current_tip = reorg_manager.get_current_tip();
    if requested_block == current_tip {
        let hash = reorg_manager.get_current_head_hash().await;
        // Use hash to validate cached data
    }
} else {
    // Block is safe - serve from cache without validation
}
```

## Testing

The module includes comprehensive tests demonstrating key functionality:

### Basic Tip Updates
**Test**: `test_reorg_manager_basic` (Lines 238-250)
- Verifies basic tip tracking
- Confirms hash storage and retrieval

### Safe Head Calculation
**Test**: `test_reorg_manager_safe_head` (Lines 252-267)
- Tests `safety_depth` configuration
- Validates `is_unsafe_block()` logic
- Confirms safe/unsafe boundary calculations

### Reorg Detection
**Test**: `test_reorg_manager_reorg_detection` (Lines 269-283)
- Simulates a reorg scenario (same block, different hash)
- Verifies hash update after reorg detection
- Tests reorg handling workflow

## Related Components

- **CacheManager** (`crates/prism-core/src/cache/cache_manager.rs`) - Handles cache invalidation
- **ReorgInfo** (`crates/prism-core/src/cache/types.rs`, Lines 359-373) - Reorg event metadata
- **BlockCache** - Stores block data that may need invalidation
- **LogCache** - Stores log data affected by reorgs
- **TransactionCache** - Stores transaction data invalidated during reorgs

## Performance Characteristics

- **Tip Updates**: O(1) amortized (O(n) when cleanup triggers, n = stale blocks)
- **Safe Head Calculation**: O(1) - atomic load and subtraction
- **Unsafe Block Check**: O(1) - single comparison
- **Hash Validation**: O(1) - DashMap lookup
- **Reorg Detection**: O(1) - single hash comparison
- **Divergence Finding**: O(d) where d = divergence depth (typically < 10)
- **Cache Invalidation**: O(d) where d = blocks to invalidate
- **Memory Usage**: O(max_reorg_depth) - bounded by configuration

## Future Improvements

Potential enhancements for reorg management:

1. **Metrics Integration**: Export reorg events to Prometheus
2. **Reorg History**: Track historical reorg events for analysis
3. **Adaptive Safety Depth**: Adjust safety depth based on observed reorg frequency
4. **Chain-Specific Defaults**: Different configs for mainnet vs L2s
5. **Divergence Point Optimization**: Cache recent divergence points for faster lookups
6. **Finality Tracking**: Integrate with beacon chain finality for guaranteed safe blocks
