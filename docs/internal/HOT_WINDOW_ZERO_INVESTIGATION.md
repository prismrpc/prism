# Hot Window Cache Always Shows 0 - Investigation Report

## Issue Summary
The admin API endpoint `/admin/cache/stats` always returns `hot_window_size: 0` even when blocks are being cached. This investigation traces the data flow from cache implementation to admin API reporting to identify the root cause.

## Data Flow Analysis

### 1. Cache Implementation (`block_cache.rs`)

#### Hot Window Structure
The hot window is a circular buffer stored in `BlockCache`:
```rust
struct HotWindow {
    size: usize,
    start_block: u64,
    start_index: usize,
    headers: Vec<Option<Arc<BlockHeader>>>,
    bodies: Vec<Option<Arc<BlockBody>>>,
}
```
- Located at: `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/block_cache.rs:70-76`

#### Insertion Logic (Line 264-286)
```rust
pub async fn insert_header(&self, header: BlockHeader) {
    let header_arc = Arc::new(header);

    if self.is_in_hot_window(header_arc.number) {  // LINE 269
        // Insert into hot window
        let mut hot_window = self.hot_window.write().await;
        hot_window.insert(block_number, header_clone, empty_body);
    }

    // Always insert into DashMap
    self.headers_by_hash.insert(header_arc.hash, Arc::clone(&header_arc));
    self.headers_by_number.insert(header_arc.number, header_arc.hash);

    // Always insert into LRU
    let mut lru = self.header_lru.write().await;
    lru.put(header_arc.hash, header_arc);
}
```

#### Critical Method: `is_in_hot_window` (Line 483-485)
```rust
fn is_in_hot_window(&self, block_number: u64) -> bool {
    self.hot_window.try_read().is_ok_and(|hw| hw.contains_block(block_number))
}
```

This checks if a block **already exists** in the hot window using `contains_block`.

#### `contains_block` Method (Line 208-224)
```rust
fn contains_block(&self, block_number: u64) -> bool {
    if block_number < self.start_block {
        return false;
    }

    let offset = block_number.saturating_sub(self.start_block);
    let Some(offset) = usize::try_from(offset).ok() else {
        return false;
    };
    if offset >= self.size {
        return false;
    }

    let index = (self.start_index + offset) % self.size;
    self.headers[index].is_some()  // Returns true only if header exists at this position
}
```

### 2. Stats Collection (`block_cache.rs`)

#### `compute_stats_internal` (Line 580-595)
```rust
async fn compute_stats_internal(&self) {
    let header_cache_size = self.headers_by_hash.len();
    let body_cache_size = self.bodies_by_hash.len();

    let hot_window_size = {
        let hot_window = self.hot_window.read().await;
        hot_window.headers.iter().filter(|h| h.is_some()).count()  // LINE 586
    };

    let mut stats = self.stats.write().await;
    stats.header_cache_size = header_cache_size;
    stats.body_cache_size = body_cache_size;
    stats.hot_window_size = hot_window_size;  // LINE 592
    stats.block_cache_hits = self.hits.load(std::sync::atomic::Ordering::Relaxed);
    stats.block_cache_misses = self.misses.load(std::sync::atomic::Ordering::Relaxed);
}
```

This correctly counts non-None entries in the hot window.

### 3. Admin API Handler (`cache.rs`)

#### `get_stats` Handler (Line 86-142)
```rust
pub async fn get_stats(State(state): State<AdminState>) -> impl IntoResponse {
    let cache_stats_raw = state.proxy_engine.get_cache_manager().get_stats().await;

    // ...

    let cache_stats = CacheStats {
        block_cache: BlockCacheStats {
            hot_window_size: cache_stats_raw.hot_window_size,  // LINE 122
            lru_entries: cache_stats_raw.header_cache_size + cache_stats_raw.body_cache_size,
            memory_usage: format_bytes(block_memory),
            hit_rate: block_hit_rate,
        },
        // ...
    };

    Json(cache_stats)
}
```

The admin API correctly passes through the `hot_window_size` value from the cache stats.

## Root Cause Analysis

### The Catch-22 Problem

The issue is a **circular dependency** in the hot window insertion logic:

1. **`insert_header` checks `is_in_hot_window(block_number)`** (line 269)
2. **`is_in_hot_window` calls `hw.contains_block(block_number)`** (line 484)
3. **`contains_block` checks if `headers[index].is_some()`** (line 222)
4. **Headers are only inserted into hot window if `is_in_hot_window` returns true** (line 269-276)

**Result**: The hot window is never populated because:
- When a new block arrives, it's not yet in the hot window
- `contains_block` returns `false` because `headers[index]` is `None`
- `is_in_hot_window` returns `false`
- Block is NOT inserted into hot window
- Hot window remains empty forever

### Visual Flow Diagram

```
New Block Arrives (e.g., block 1000)
         |
         v
insert_header(block 1000)
         |
         v
is_in_hot_window(1000)?
         |
         v
contains_block(1000)?
         |
         v
Check: start_block <= 1000 < start_block + size?
         |
         +---> YES, calculate index
         |            |
         |            v
         |     headers[index].is_some()?
         |            |
         |            +---> NO (None) --> Return false
         |
         v
is_in_hot_window returns false
         |
         v
Skip hot window insertion
         |
         v
Insert only into DashMap and LRU
         |
         v
Hot window stays at 0 entries
```

## Evidence from Code

### Empty Hot Window Initialization
```rust
// From HotWindow::new (line 79-87)
fn new(size: usize) -> Self {
    Self {
        size,
        start_block: 0,
        start_index: 0,
        headers: vec![None; size],      // All None initially
        bodies: vec![None; size],       // All None initially
    }
}
```

### First Block Scenario
- Block 1000 arrives
- Hot window state: `start_block: 0`, all headers are `None`
- `contains_block(1000)`: offset = 1000, index = 1000 % size
- `headers[index]`: `None`
- `contains_block` returns `false`
- Block is not inserted into hot window

### The Logic Error

The method name `is_in_hot_window` suggests it should check if a block **should be** in the hot window (based on block range), but it actually checks if the block **currently exists** in the hot window (checking for `is_some()`).

## Correct Behavior Expected

The hot window should use a **range-based check** instead of an **existence check**:

```rust
// Current (incorrect)
fn is_in_hot_window(&self, block_number: u64) -> bool {
    self.hot_window.try_read().is_ok_and(|hw| hw.contains_block(block_number))
}

// Should be (correct)
fn is_in_hot_window(&self, block_number: u64) -> bool {
    self.hot_window.try_read().is_ok_and(|hw| {
        block_number >= hw.start_block &&
        block_number < hw.start_block + hw.size as u64
    })
}
```

Or create a separate method:
```rust
fn should_be_in_hot_window(&self, block_number: u64) -> bool {
    // Check if block number falls within the hot window range
    // regardless of whether it's currently stored there
}
```

## Impact

### Current State
- Hot window never gets populated
- All block lookups fall back to DashMap
- No O(1) hot path for recent blocks
- Performance degradation for recent block queries
- Admin API always shows `hot_window_size: 0`

### Expected State
- Recent blocks (within configured window size) should be in hot window
- Fast O(1) lookups for recent blocks
- Admin API should show non-zero hot_window_size reflecting actual recent blocks cached

## Verification

### Test Evidence
Looking at the test suite (lines 961-991), the test `test_block_cache_hot_window` inserts blocks but only checks that `get_header_by_number` works - it doesn't verify the hot window is actually used. The test passes because DashMap fallback works.

### Production Evidence
- Admin API consistently reports `hot_window_size: 0`
- Headers and bodies are being cached (non-zero counts in DashMap)
- No errors in logs
- System appears functional (due to DashMap fallback)

## Files Affected

1. **Core Cache Implementation**
   - `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/block_cache.rs`
     - Lines 269-276: `insert_header` logic
     - Lines 294-303: `insert_body` logic
     - Lines 483-485: `is_in_hot_window` method
     - Lines 208-224: `contains_block` method

2. **Stats Collection**
   - Same file, lines 580-595: `compute_stats_internal` (this is correct)

3. **Admin API Handler**
   - `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/cache.rs`
     - Lines 86-142: `get_stats` handler (this is correct)

4. **Type Definitions**
   - `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/types.rs`
     - Lines 508-536: `CacheStats` struct (this is correct)

## Recommended Fix

### Option 1: Range-Based Check (Recommended)
Replace `is_in_hot_window` to check block range instead of existence:

```rust
fn is_in_hot_window(&self, block_number: u64) -> bool {
    self.hot_window.try_read().is_ok_and(|hw| {
        block_number >= hw.start_block &&
        block_number < hw.start_block + (hw.size as u64)
    })
}
```

### Option 2: Track Latest Block
Maintain a `latest_block` field in HotWindow and use it to determine the range:

```rust
struct HotWindow {
    size: usize,
    start_block: u64,
    start_index: usize,
    latest_block: u64,  // Add this
    headers: Vec<Option<Arc<BlockHeader>>>,
    bodies: Vec<Option<Arc<BlockBody>>>,
}

fn is_in_hot_window(&self, block_number: u64) -> bool {
    self.hot_window.try_read().is_ok_and(|hw| {
        block_number > hw.latest_block.saturating_sub(hw.size as u64) &&
        block_number <= hw.latest_block
    })
}
```

### Option 3: Unconditional Insertion with Auto-Advance
Always try to insert into hot window and let `HotWindow::insert` handle the range logic (it already does this via `advance_window`):

```rust
pub async fn insert_header(&self, header: BlockHeader) {
    let header_arc = Arc::new(header);

    // Always insert into hot window (it will auto-advance if needed)
    let block_number = header_arc.number;
    let hash = header_arc.hash;
    let empty_body = Arc::new(BlockBody { hash, transactions: vec![] });
    let header_clone = Arc::clone(&header_arc);

    let mut hot_window = self.hot_window.write().await;
    hot_window.insert(block_number, header_clone, empty_body);

    // Also insert into persistent storage
    self.headers_by_hash.insert(header_arc.hash, Arc::clone(&header_arc));
    // ... rest unchanged
}
```

## Testing Recommendations

Add a test that specifically verifies hot window population:

```rust
#[tokio::test]
async fn test_hot_window_gets_populated() {
    let config = BlockCacheConfig { hot_window_size: 10, ..Default::default() };
    let cache = BlockCache::new(&config).expect("valid config");

    // Insert sequential blocks
    for i in 0..5 {
        cache.insert_header(create_test_header(1000 + i, i as u8)).await;
    }

    // Verify hot window has entries
    let stats = cache.get_stats().await;
    assert!(stats.hot_window_size > 0, "Hot window should have entries");
    assert_eq!(stats.hot_window_size, 5, "Should have all 5 blocks");
}
```

## Conclusion

The hot window cache is not being populated due to a logic error in the `is_in_hot_window` method. The method checks if a block **currently exists** in the hot window rather than if it **should be** in the hot window based on block number ranges. This creates a catch-22 where blocks are never inserted because they don't exist yet.

The fix is straightforward: change the check from existence-based to range-based. Option 3 (unconditional insertion) is the simplest and most robust solution since `HotWindow::insert` already handles all the edge cases internally.
