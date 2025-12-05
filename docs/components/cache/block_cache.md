# Block Cache

## Overview

The Block Cache is a high-performance, three-tier caching system designed to optimize Ethereum block data retrieval in the Prism RPC aggregator. It provides fast O(1) access to recent blocks through a circular buffer hot window, backed by concurrent hash maps and LRU eviction for older blocks. The cache stores both block headers and bodies separately, allowing flexible querying by block number or hash while minimizing memory usage and maximizing cache hit rates.

**Key Features:**
- Three-tier architecture: Hot Window (circular buffer) → DashMap (concurrent) → LRU (eviction)
- O(1) lookups for blocks in the hot window
- Concurrent read/write access using `DashMap`
- Automatic LRU eviction for memory management
- Reorg-aware invalidation mechanisms
- Thread-safe operations with minimal lock contention
- Separate storage for headers and bodies with cross-referencing

**Location:** `crates/prism-core/src/cache/block_cache.rs`

## Configuration

### BlockCacheConfig

Configuration structure for tuning block cache behavior.

```rust
pub struct BlockCacheConfig {
    pub hot_window_size: usize,
    pub max_headers: usize,
    pub max_bodies: usize,
    pub safety_depth: u64,
}
```

**Fields:**

| Field | Type | Default | Purpose |
|-------|------|---------|---------|
| `hot_window_size` | `usize` | `200` | Number of recent blocks to keep in the circular buffer hot window for O(1) access |
| `max_headers` | `usize` | `10000` | Maximum number of block headers to cache before LRU eviction (must be non-zero) |
| `max_bodies` | `usize` | `10000` | Maximum number of block bodies to cache before LRU eviction (must be non-zero) |
| `safety_depth` | `u64` | `12` | Number of blocks from safe head to retain during pruning (prevents premature eviction) |

**Default Configuration:**
```rust
BlockCacheConfig::default() // 200 hot window, 10k headers, 10k bodies, 12 block safety depth
```

**Configuration Location:** Lines 8-21 in `crates/prism-core/src/cache/block_cache.rs`

## Data Structures

### BlockCache

Main cache structure managing block storage and retrieval.

```rust
pub struct BlockCache {
    _config: BlockCacheConfig,

    // Header storage
    headers_by_hash: DashMap<[u8; 32], Arc<BlockHeader>, RandomState>,
    headers_by_number: DashMap<u64, [u8; 32], RandomState>,
    header_lru: Arc<RwLock<LruCache<[u8; 32], Arc<BlockHeader>>>>,

    // Body storage
    bodies_by_hash: DashMap<[u8; 32], Arc<BlockBody>, RandomState>,
    body_lru: Arc<RwLock<LruCache<[u8; 32], Arc<BlockBody>>>>,

    // Hot window
    hot_window: Arc<RwLock<HotWindow>>,

    // Statistics
    stats: Arc<RwLock<CacheStats>>,
    stats_dirty: AtomicBool,
}
```

**Field Descriptions:**

- **`headers_by_hash`**: Primary concurrent hash map for header lookups by block hash (O(1) average)
- **`headers_by_number`**: Maps block numbers to their canonical hash for number-based lookups
- **`header_lru`**: LRU cache for header eviction tracking (requires write lock for updates)
- **`bodies_by_hash`**: Primary concurrent hash map for body lookups by block hash
- **`body_lru`**: LRU cache for body eviction tracking
- **`hot_window`**: Circular buffer for recent blocks with O(1) window operations
- **`stats`**: Cache statistics for monitoring and observability

**Structure Location:** Lines 24-37

### HotWindow

Circular buffer implementation for recent blocks with overflow protection.

```rust
struct HotWindow {
    size: usize,              // Fixed size of the circular buffer
    start_block: u64,         // Block number at start_index
    start_index: usize,       // Current start position in circular buffer (0 to size-1)
    headers: Vec<Option<BlockHeader>>,
    bodies: Vec<Option<BlockBody>>,
}
```

**Design Characteristics:**

- **Circular Buffer**: Uses modulo arithmetic for O(1) window advancement (lines 86-104)
- **Overflow Protection**: Saturating arithmetic prevents integer overflow in all operations
- **Sparse Storage**: Uses `Option` to handle gaps in block sequences
- **Fixed Capacity**: Pre-allocated vectors avoid reallocation overhead
- **Thread-Safe**: DashMaps provide lock-free concurrent access; LRU caches and hot window protected by `Arc<RwLock<>>`
- **Arc-Wrapped Values**: Headers and bodies are stored as `Arc<BlockHeader>` and `Arc<BlockBody>` for efficient cloning

**Key Algorithms:**

1. **Insert with Window Advancement** (lines 61-84):
   - Calculates offset from `start_block` using saturating subtraction
   - Advances window if offset exceeds size
   - Uses circular indexing: `(start_index + offset) % size`

2. **Window Advancement** (lines 86-104):
   - Clears only entries that will be overwritten (O(advance) operation)
   - Updates `start_index` with modulo for circular wrap-around
   - Updates `start_block` with saturating addition

3. **Range Checking** (lines 106-149):
   - Checks if block is before window: `block_number < start_block`
   - Checks if block is after window: `offset >= size`
   - Returns `None` for out-of-range blocks

**Structure Location:** Lines 40-150

### BlockHeader and BlockBody

Data types from `types.rs` representing Ethereum block data.

**BlockHeader** (lines 278-293 in types.rs):
```rust
pub struct BlockHeader {
    pub hash: [u8; 32],
    pub number: u64,
    pub parent_hash: [u8; 32],
    pub timestamp: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub miner: [u8; 20],

    // Arc-wrapped for cheap cloning
    pub extra_data: Arc<Vec<u8>>,
    pub logs_bloom: Arc<Vec<u8>>,

    pub transactions_root: [u8; 32],
    pub state_root: [u8; 32],
    pub receipts_root: [u8; 32],
}
```

**BlockBody** (lines 296-300 in types.rs):
```rust
pub struct BlockBody {
    pub hash: [u8; 32],
    pub transactions: Vec<[u8; 32]>,  // Transaction hashes only
}
```

**Design Notes:**
- `Arc<Vec<u8>>` used for large fields (`extra_data`, `logs_bloom`) to reduce clone overhead
- Bodies store only transaction hashes, not full transaction data
- Fixed-size arrays for cryptographic hashes enable efficient comparisons

## Public API

### Constructor

#### `new(config: &BlockCacheConfig) -> Self`

Creates a new block cache instance with specified configuration.

**Location:** Lines 152-174

**Panics:** If `max_headers` or `max_bodies` is zero (enforced by `NonZeroUsize`)

**Example:**
```rust
let config = BlockCacheConfig {
    hot_window_size: 200,
    max_headers: 10000,
    max_bodies: 10000,
    safety_depth: 12,
};
let cache = BlockCache::new(&config);
```

**Implementation Details:**
- Initializes all `DashMap` instances with default capacity
- Creates LRU caches with `NonZeroUsize` capacity from config
- Allocates hot window with `vec![None; size]` for sparse storage
- Initializes empty statistics

---

### Insertion Methods

#### `insert_header(&self, header: BlockHeader)` (async)

Inserts a block header into the cache with hot window awareness.

**Location:** Lines 176-196

**Behavior:**
1. Checks if block is in hot window range
2. If in hot window, acquires write lock and inserts header + empty body
3. Inserts into `headers_by_hash` concurrent map
4. Updates `headers_by_number` canonical mapping
5. Updates LRU cache for eviction tracking
6. Updates cache statistics

**Concurrency:** Write lock on hot window and LRU, lock-free for DashMaps

**Example:**
```rust
let header = BlockHeader {
    hash: [1u8; 32],
    number: 1000,
    // ... other fields
};
cache.insert_header(header).await;
```

#### `insert_body(&self, body: BlockBody)` (async)

Inserts a block body into the cache, updating hot window if header exists.

**Location:** Lines 198-218

**Behavior:**
1. Looks up corresponding header by hash (synchronous)
2. If header exists and in hot window, updates hot window entry
3. Inserts into `bodies_by_hash` concurrent map
4. Updates LRU cache for eviction tracking
5. Updates cache statistics

**Note:** Hot window update requires existing header to determine block number

**Example:**
```rust
let body = BlockBody {
    hash: [1u8; 32],
    transactions: vec![[7u8; 32], [8u8; 32]],
};
cache.insert_body(body).await;
```

---

### Query Methods (All Synchronous)

All query methods are synchronous and use lock-free operations where possible.

#### `get_header_by_number(&self, block_number: u64) -> Option<BlockHeader>`

Retrieves a block header by block number.

**Location:** Lines 220-238

**Lookup Strategy:**
1. Try hot window first (requires read lock, returns early if found)
2. Lookup in `headers_by_number` to get canonical hash
3. Lookup in `headers_by_hash` using the hash
4. Return `None` if not found

**Complexity:** O(1) average case

**Example:**
```rust
if let Some(header) = cache.get_header_by_number(1000) {
    println!("Block 1000 hash: {:02x?}", header.hash);
}
```

#### `get_header_by_hash(&self, block_hash: &[u8; 32]) -> Option<BlockHeader>`

Retrieves a block header by block hash.

**Location:** Lines 240-248

**Lookup Strategy:**
1. Direct lookup in `headers_by_hash` concurrent map
2. Returns cloned header if found

**Complexity:** O(1) average case

**Example:**
```rust
let hash = [1u8; 32];
if let Some(header) = cache.get_header_by_hash(&hash) {
    println!("Block number: {}", header.number);
}
```

#### `get_body_by_hash(&self, block_hash: &[u8; 32]) -> Option<BlockBody>`

Retrieves a block body by block hash.

**Location:** Lines 250-268

**Lookup Strategy:**
1. Try hot window first (requires header lookup to get block number)
2. Lookup in `bodies_by_hash` concurrent map
3. Return cloned body if found

**Complexity:** O(1) average case

**Example:**
```rust
let hash = [1u8; 32];
if let Some(body) = cache.get_body_by_hash(&hash) {
    println!("Transaction count: {}", body.transactions.len());
}
```

#### `get_block_by_number(&self, block_number: u64) -> Option<(BlockHeader, BlockBody)>`

Retrieves both header and body by block number.

**Location:** Lines 270-276

**Lookup Strategy:**
1. Get header by number
2. Get body by header's hash
3. Return tuple if both found

**Complexity:** O(1) average case

**Example:**
```rust
if let Some((header, body)) = cache.get_block_by_number(1000) {
    println!("Block 1000 has {} transactions", body.transactions.len());
}
```

#### `get_block_by_hash(&self, block_hash: &[u8; 32]) -> Option<(BlockHeader, BlockBody)>`

Retrieves both header and body by block hash.

**Location:** Lines 278-284

**Lookup Strategy:**
1. Get header by hash
2. Get body by hash
3. Return tuple if both found

**Complexity:** O(1) average case

**Example:**
```rust
let hash = [1u8; 32];
if let Some((header, body)) = cache.get_block_by_hash(&hash) {
    println!("Found block {} with {} txs", header.number, body.transactions.len());
}
```

---

### Maintenance Methods

#### `invalidate_block(&self, block_number: u64)` (async)

Invalidates a block during chain reorganization.

**Location:** Lines 296-328

**Behavior:**
1. Removes from `headers_by_number` (returns old hash)
2. Removes header and body from hash maps using the hash
3. Removes from both LRU caches (requires write locks)
4. Clears hot window entry if present (requires write lock)
5. Updates cache statistics

**Use Case:** Called when a reorg is detected to remove invalidated blocks

**Example:**
```rust
// Reorg detected, invalidate blocks 1000-1005
for block_num in 1000..=1005 {
    cache.invalidate_block(block_num).await;
}
```

#### `clear_cache(&self)` (async)

Clears all cached data (used for authentication changes or full reset).

**Location:** Lines 330-360

**Behavior:**
1. Clears all DashMap instances
2. Clears both LRU caches (requires write locks)
3. Resets hot window to empty state
4. Resets `start_block` and `start_index` to 0
5. Updates statistics

**Example:**
```rust
// Configuration changed, clear all caches
cache.clear_cache().await;
```

#### `warm_recent(&self, from_block: u64, to_block: u64, blocks: Vec<(BlockHeader, BlockBody)>)` (async)

Pre-populates the cache with recent blocks.

**Location:** Lines 369-382

**Behavior:**
1. Iterates through provided blocks
2. Calls `insert_header` and `insert_body` for each block
3. Blocks automatically populate hot window if within range

**Use Case:** Warming cache on startup or after chain sync

**Example:**
```rust
let blocks = fetch_recent_blocks(990, 1000).await;
cache.warm_recent(990, 1000, blocks).await;
```

#### `prune_by_safe_head(&self, safe_head: u64, retain_blocks: u64)` (async)

Prunes old blocks based on safe head position.

**Location:** Lines 399-439

**Behavior:**
1. Calculates cutoff: `safe_head - retain_blocks`
2. Evicts LRU headers with `block_number < cutoff`
3. Evicts LRU bodies for blocks below cutoff
4. Removes orphaned bodies (no corresponding header)
5. Updates cache statistics

**Algorithm:** Iteratively pops from LRU, checks block number, removes if old

**Example:**
```rust
// Safe head at block 2000, retain last 1000 blocks
cache.prune_by_safe_head(2000, 1000).await;
// Blocks < 1000 will be evicted
```

---

### Utility Methods

#### `update_canonical_chain(&self, block_number: u64, block_hash: [u8; 32])`

Updates the canonical chain mapping for a block number.

**Location:** Lines 362-367

**Behavior:**
- Updates `headers_by_number` with new canonical hash
- Does not remove old header data (invalidation must be explicit)

**Use Case:** Called when canonical chain changes or new blocks arrive

**Example:**
```rust
cache.update_canonical_chain(1000, new_hash);
```

#### `is_in_hot_window(&self, block_number: u64) -> bool`

Checks if a block number is currently in the hot window.

**Location:** Lines 286-294

**Behavior:**
- Attempts non-blocking read lock with `try_read()`
- Returns `false` if lock unavailable
- Delegates to `HotWindow::contains_block`

**Example:**
```rust
if cache.is_in_hot_window(1000) {
    println!("Block 1000 is in hot window");
}
```

#### `get_stats(&self) -> CacheStats` (async)

Retrieves current cache statistics.

**Location:** Lines 384-387

**Returns:** `CacheStats` struct with current cache metrics

**Example:**
```rust
let stats = cache.get_stats().await;
println!("Headers cached: {}", stats.header_cache_size);
println!("Bodies cached: {}", stats.body_cache_size);
println!("Hot window size: {}", stats.hot_window_size);
```

---

## Key Algorithms

### Three-Tier Caching Strategy

The block cache uses a sophisticated three-tier architecture optimized for different access patterns:

**Tier 1: Hot Window (Circular Buffer)**
- **Purpose:** Ultra-fast access to most recent ~200 blocks
- **Implementation:** Fixed-size `Vec<Option<T>>` with circular indexing
- **Complexity:** O(1) for all operations
- **Location:** Lines 40-150
- **Characteristics:**
  - Pre-allocated memory (no reallocation)
  - Sparse storage with `Option` for gaps
  - Automatic advancement with saturation arithmetic
  - Read-heavy workload optimized

**Tier 2: Primary Cache (DashMap)**
- **Purpose:** Concurrent access to all cached blocks
- **Implementation:** Lock-free concurrent hash maps
- **Complexity:** O(1) average case lookups
- **Location:** Lines 27-28, 31-32
- **Characteristics:**
  - Sharded locking for high concurrency
  - No lock required for reads
  - Separate maps for headers and bodies
  - Cross-referenced via block hash

**Tier 3: LRU Eviction Layer**
- **Purpose:** Track access recency for cache eviction
- **Implementation:** `LruCache` with `RwLock` protection
- **Complexity:** O(1) for put/get, O(n) for iteration
- **Location:** Lines 29, 32
- **Characteristics:**
  - Enforces max capacity (`max_headers`, `max_bodies`)
  - Automatic eviction of least recently used
  - Synchronized with DashMap on eviction

**Query Flow:**
```
Query Request
     |
     v
[1] Try Hot Window (O(1))
     |
     | Miss
     v
[2] Try DashMap (O(1))
     |
     | Miss
     v
[3] Return None
```

**Insertion Flow:**
```
New Block
     |
     v
[1] Insert to DashMap
     |
     v
[2] Update LRU
     |
     v
[3] If in hot window range
     |
     v
[4] Insert to Hot Window
```

### Circular Buffer Design

The `HotWindow` circular buffer provides O(1) window operations without data copying.

**Core Algorithm (lines 86-104):**

```rust
fn advance_window(&mut self, advance: usize) {
    let advance = std::cmp::min(advance, self.size);  // Clamp to size

    // Clear only entries being overwritten - O(advance)
    for i in 0..advance {
        let clear_index = (self.start_index + i) % self.size;
        self.headers[clear_index] = None;
        self.bodies[clear_index] = None;
    }

    // Advance circular indices
    self.start_index = (self.start_index + advance) % self.size;
    self.start_block = self.start_block.saturating_add(advance as u64);
}
```

**Key Features:**
1. **No Data Movement:** Unlike traditional ring buffers, doesn't shift elements
2. **Selective Clearing:** Only clears slots to be overwritten
3. **Modulo Arithmetic:** Wraps indices without conditionals
4. **Saturation Protection:** Prevents overflow on `start_block` increment

**Index Calculation (lines 79-80, 117):**
```rust
let offset = block_number.saturating_sub(self.start_block);
let index = (self.start_index + offset) % self.size;
```

**Example Scenario:**
```
Initial State:
size = 10, start_block = 1000, start_index = 0
Buffer: [B1000, B1001, ..., B1009]

Insert Block 1015:
1. offset = 1015 - 1000 = 15 (exceeds size 10)
2. advance = 15 - 10 + 1 = 6
3. Clear indices 0-5
4. start_index = (0 + 6) % 10 = 6
5. start_block = 1000 + 6 = 1006
6. Insert B1015 at index (6 + 9) % 10 = 5

New State:
start_block = 1006, start_index = 6
Buffer: [None×6, B1006, B1007, B1008, B1009]
After insert: [B1015, None×5, B1006, B1007, B1008, B1009]
```

### Query Strategy

The query strategy optimizes for the common case (recent blocks) while falling back gracefully.

**get_header_by_number Strategy (lines 222-238):**

```rust
pub fn get_header_by_number(&self, block_number: u64) -> Option<BlockHeader> {
    // Fast path: Try hot window first
    {
        if let Ok(hot_window) = self.hot_window.try_read() {
            if let Some(header) = hot_window.get_header(block_number) {
                return Some(header.clone());  // Early return on hot hit
            }
        }
    }  // Drop lock immediately

    // Fallback path: DashMap lookup
    if let Some(hash) = self.headers_by_number.get(&block_number) {
        if let Some(header) = self.headers_by_hash.get(&*hash) {
            return Some(header.clone());
        }
    }

    None
}
```

**Optimizations:**
1. **Try-lock:** Uses `try_read()` to avoid blocking on hot window lock
2. **Early Return:** Returns immediately on hot window hit
3. **Lock Scope:** Drops hot window lock before DashMap access
4. **Lock-Free Fallback:** DashMap doesn't require locks for reads

**get_body_by_hash Strategy (lines 252-268):**

```rust
pub fn get_body_by_hash(&self, block_hash: &[u8; 32]) -> Option<BlockBody> {
    // Try hot window (requires header lookup for block number)
    {
        if let Ok(hot_window) = self.hot_window.try_read() {
            if let Some(header) = self.get_header_by_hash(block_hash) {
                if let Some(body) = hot_window.get_body(header.number) {
                    return Some(body.clone());
                }
            }
        }
    }

    // Fallback to DashMap
    if let Some(body) = self.bodies_by_hash.get(block_hash) {
        return Some(body.clone());
    }

    None
}
```

**Trade-off:** Hot window lookup by hash requires header lookup to get block number, but still faster than full cache miss.

---

## Performance Characteristics

| Operation | Hot Window | DashMap | LRU | Overall |
|-----------|------------|---------|-----|---------|
| `get_header_by_number` | O(1) | O(1) | - | O(1) |
| `get_header_by_hash` | O(1)* | O(1) | - | O(1) |
| `get_body_by_hash` | O(1)* | O(1) | - | O(1) |
| `get_block_by_number` | O(1) | O(1) | - | O(1) |
| `get_block_by_hash` | O(1)* | O(1) | - | O(1) |
| `insert_header` | O(1) | O(1) | O(1) | O(1) |
| `insert_body` | O(1) | O(1) | O(1) | O(1) |
| `invalidate_block` | O(1) | O(1) | O(1) | O(1) |
| `clear_cache` | O(n) | O(n) | O(n) | O(n) |
| `warm_recent` | O(m) | O(m) | O(m) | O(m) |
| `prune_by_safe_head` | - | O(k) | O(k) | O(k) |

**Notes:**
- `*` Hot window hash lookup requires header lookup first (still O(1) but with higher constant)
- `n` = total cached items
- `m` = number of blocks to warm
- `k` = number of blocks to prune

**Memory Complexity:**
- Hot Window: O(hot_window_size) - fixed, pre-allocated
- Headers: O(max_headers) - bounded by LRU
- Bodies: O(max_bodies) - bounded by LRU
- Total: O(hot_window_size + max_headers + max_bodies)

**Concurrency Characteristics:**
- **Read Operations:** Mostly lock-free (DashMap), try-lock for hot window
- **Write Operations:** Write locks on hot window and LRU, lock-free for DashMap
- **Contention:** Low - hot window rarely locked, DashMap uses sharded locks
- **Scalability:** Excellent for read-heavy workloads (typical RPC pattern)

---

## Usage Examples

### Basic Caching Flow

```rust
use prism_core::cache::block_cache::{BlockCache, BlockCacheConfig};
use prism_core::cache::types::{BlockHeader, BlockBody};

// Initialize cache
let config = BlockCacheConfig::default();
let cache = BlockCache::new(&config);

// Create sample block data
let header = BlockHeader {
    hash: [1u8; 32],
    number: 1000,
    parent_hash: [0u8; 32],
    timestamp: 1234567890,
    gas_limit: 30_000_000,
    gas_used: 15_000_000,
    miner: [0u8; 20],
    extra_data: Arc::new(vec![]),
    logs_bloom: Arc::new(vec![0u8; 256]),
    transactions_root: [0u8; 32],
    state_root: [0u8; 32],
    receipts_root: [0u8; 32],
};

let body = BlockBody {
    hash: [1u8; 32],
    transactions: vec![[7u8; 32], [8u8; 32]],
};

// Insert into cache
cache.insert_header(header.clone()).await;
cache.insert_body(body.clone()).await;

// Query by number
if let Some((header, body)) = cache.get_block_by_number(1000) {
    println!("Found block 1000 with {} transactions", body.transactions.len());
}

// Query by hash
if let Some((header, body)) = cache.get_block_by_hash(&[1u8; 32]) {
    println!("Block number: {}", header.number);
}
```

### Handler Integration (from blocks.rs)

**Location:** `crates/prism-core/src/proxy/handlers/blocks.rs`

```rust
// Example: eth_getBlockByNumber handler (lines 32-85)
pub async fn handle_block_by_number_request(
    &self,
    request: JsonRpcRequest,
) -> Result<JsonRpcResponse, ProxyError> {
    // Parse block number from request
    let block_number = parse_block_number(&request)?;

    // Try cache first
    if let Some((header, body)) = self.cache_manager.get_block_by_number(block_number) {
        debug!("Block cache hit: block {}", block_number);
        self.metrics_collector.record_cache_hit("eth_getBlockByNumber").await;

        // Convert to JSON and return
        let block_json = block_header_and_body_to_json(&header, &body);
        return Ok(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(block_json),
            error: None,
            id: request.id,
            cache_status: Some(CacheStatus::Full),
        });
    }

    // Cache miss: fetch from upstream
    debug!("Block cache miss: fetching block {} from upstream", block_number);
    self.metrics_collector.record_cache_miss("eth_getBlockByNumber");

    let response = self.forward_to_upstream(&request).await?;

    // Cache the response for future requests
    if let Some(result) = response.result.as_ref() {
        self.cache_block_from_response(result).await;
    }

    Ok(response)
}

// Cache population helper (lines 140-152)
async fn cache_block_from_response(&self, block_json: &serde_json::Value) {
    use crate::cache::converter::{json_block_to_block_header, json_block_to_block_body};

    if let Some(header) = json_block_to_block_header(block_json) {
        if let Some(body) = json_block_to_block_body(block_json) {
            self.cache_manager.insert_header(header).await;
            self.cache_manager.insert_body(body).await;
        }
    }
}
```

### WebSocket Chain Tip Updates (from websocket.rs)

**Location:** `crates/prism-core/src/upstream/websocket.rs` (lines 626-631)

```rust
// Update cache when new blocks arrive via WebSocket
if let Some(header) = parse_block_header(&new_block) {
    cache_manager.insert_header(header).await;
}

if let Some(body) = parse_block_body(&new_block) {
    cache_manager.insert_body(body).await;
}
```

### Cache Warming on Startup

```rust
async fn warm_block_cache(
    cache: &BlockCache,
    upstream: &UpstreamManager,
    current_block: u64,
) -> Result<(), Error> {
    let from_block = current_block.saturating_sub(150);
    let to_block = current_block;

    let mut blocks = Vec::new();
    for block_num in from_block..=to_block {
        if let Ok(block_data) = upstream.fetch_block(block_num).await {
            let header = parse_header(&block_data)?;
            let body = parse_body(&block_data)?;
            blocks.push((header, body));
        }
    }

    cache.warm_recent(from_block, to_block, blocks).await;
    Ok(())
}
```

### Monitoring Cache Performance

```rust
async fn monitor_cache_stats(cache: &BlockCache) {
    let stats = cache.get_stats().await;

    println!("Block Cache Statistics:");
    println!("  Headers cached: {}", stats.header_cache_size);
    println!("  Bodies cached: {}", stats.body_cache_size);
    println!("  Hot window utilization: {}/{}",
             stats.hot_window_size,
             200); // hot_window_size from config

    // Calculate cache efficiency
    let total_capacity = 10000; // max_headers
    let utilization = (stats.header_cache_size as f64 / total_capacity as f64) * 100.0;
    println!("  Cache utilization: {:.2}%", utilization);
}
```

---

## Reorg Handling

Chain reorganizations require careful cache invalidation to prevent serving stale data.

### Reorg Detection and Invalidation

When a reorg is detected by comparing the incoming block's parent hash with the cached block hash at the same height:

**Invalidation Strategy:**
1. Identify divergence point (last common ancestor)
2. Invalidate all blocks from divergence point to old chain tip
3. Update canonical chain mappings with new blocks
4. Preserve data below safety depth (typically 12 blocks from safe head)

**Example Reorg Handler:**

```rust
async fn handle_reorg(
    cache: &BlockCache,
    old_tip: u64,
    new_tip: u64,
    divergence_block: u64,
) {
    info!("Reorg detected: old_tip={}, new_tip={}, divergence={}",
          old_tip, new_tip, divergence_block);

    // Invalidate old chain blocks
    for block_num in (divergence_block + 1)..=old_tip {
        cache.invalidate_block(block_num).await;
    }

    // New blocks will be inserted as they arrive
    // via normal insertion flow
}
```

### Safety Depth

The `safety_depth` configuration parameter (default: 12) determines how many blocks below the safe head to retain during pruning:

**Purpose:**
- Prevents premature eviction of blocks that might be involved in reorgs
- Aligns with Ethereum's finality assumptions (~64 blocks for finality, 12 for safety)
- Balances memory usage with reorg resilience

**Pruning with Safety Depth (lines 399-439):**

```rust
pub async fn prune_by_safe_head(&self, safe_head: u64, retain_blocks: u64) {
    let cutoff = safe_head.saturating_sub(retain_blocks);

    // Example: safe_head = 2000, retain_blocks = 1000
    // cutoff = 1000
    // Only blocks < 1000 will be evicted

    // Evict headers below cutoff
    let mut lru = self.header_lru.write().await;
    while lru.len() > lru.cap().get() {
        if let Some((hash, header)) = lru.pop_lru() {
            if header.number < cutoff {
                self.headers_by_hash.remove(&hash);
                self.headers_by_number.remove(&header.number);
            } else {
                lru.put(hash, header);  // Put back, we've hit recent blocks
                break;
            }
        }
    }

    // Similar process for bodies...
}
```

### Canonical Chain Updates

When the canonical chain changes (new block or reorg), update the number→hash mapping:

```rust
// Example: New block arrives
async fn on_new_block(cache: &BlockCache, header: BlockHeader) {
    // Update canonical mapping
    cache.update_canonical_chain(header.number, header.hash);

    // Insert header and body
    cache.insert_header(header).await;
    // ... insert body
}

// Example: Reorg changes canonical chain
async fn on_reorg(cache: &BlockCache, new_blocks: Vec<BlockHeader>) {
    for header in new_blocks {
        // This replaces the old canonical hash for this number
        cache.update_canonical_chain(header.number, header.hash);
        cache.insert_header(header).await;
    }
}
```

**Important:** `update_canonical_chain` only updates the mapping, it does NOT remove old header data. Explicit invalidation is required for cleanup.

---

## Testing

The block cache includes comprehensive test coverage:

**Test Location:** Lines 442-551 in `crates/prism-core/src/cache/block_cache.rs`

### Basic Operations Test (lines 447-483)

Tests insertion and retrieval by both number and hash:
```rust
#[tokio::test]
async fn test_block_cache_basic_operations() {
    let cache = BlockCache::new(&BlockCacheConfig::default());

    // Insert header and body
    cache.insert_header(header).await;
    cache.insert_body(body).await;

    // Verify retrieval by number
    let (header, body) = cache.get_block_by_number(1000).unwrap();
    assert_eq!(header.number, 1000);
    assert_eq!(body.transactions.len(), 2);

    // Verify retrieval by hash
    let (header, body) = cache.get_block_by_hash(&[1u8; 32]).unwrap();
    assert_eq!(header.number, 1000);
}
```

### Hot Window Test (lines 485-516)

Tests circular buffer behavior with multiple inserts:
```rust
#[tokio::test]
async fn test_block_cache_hot_window() {
    let config = BlockCacheConfig {
        hot_window_size: 10,
        ..Default::default()
    };
    let cache = BlockCache::new(&config);

    // Insert 5 blocks
    for i in 0..5 {
        cache.insert_header(create_header(1000 + i)).await;
        cache.insert_body(create_body(1000 + i)).await;
    }

    // Verify all blocks retrievable
    for i in 0..5 {
        assert!(cache.get_header_by_number(1000 + i).is_some());
    }
}
```

### Reorg Invalidation Test (lines 518-550)

Tests invalidation during reorg scenarios:
```rust
#[tokio::test]
async fn test_block_cache_reorg_invalidation() {
    let cache = BlockCache::new(&BlockCacheConfig::default());

    // Insert and verify block
    cache.insert_header(header).await;
    cache.insert_body(body).await;
    assert!(cache.get_block_by_number(1000).is_some());

    // Invalidate block
    cache.invalidate_block(1000).await;

    // Verify removal
    assert!(cache.get_block_by_number(1000).is_none());
}
```

### Additional Test Coverage

For integration tests, see:
- `crates/tests/src/overlapping_cache_tests.rs` (lines 10-80)
- `crates/tests/src/e2e/cache_tests.rs` (lines 38-490)

---

## Summary

The Block Cache is a production-ready, high-performance caching system that:

- **Provides O(1) access** to recent blocks through a circular buffer hot window
- **Scales efficiently** with concurrent DashMap storage and sharded locking
- **Manages memory** automatically with LRU eviction and configurable limits
- **Handles reorgs** gracefully with invalidation and safety depth mechanisms
- **Integrates seamlessly** with RPC handlers and WebSocket chain updates
- **Offers observability** through comprehensive statistics and metrics

**Performance Highlights:**
- Hot window: O(1) operations with zero data copying
- Lock-free reads for most operations (DashMap)
- Try-lock pattern minimizes contention
- Automatic eviction prevents unbounded growth

**Key Files:**
- Implementation: `crates/prism-core/src/cache/block_cache.rs`
- Types: `crates/prism-core/src/cache/types.rs` (lines 278-300)
- Handler Usage: `crates/prism-core/src/proxy/handlers/blocks.rs` (lines 60-85, 113-138)
- WebSocket Integration: `crates/prism-core/src/upstream/websocket.rs` (lines 626-631)
