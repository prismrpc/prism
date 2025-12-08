# Hot Window Cache Performance Analysis

## Executive Summary

**Finding**: The `hot_window` cache is being **actively populated and used** for recent blocks, but there are **performance monitoring gaps** and **unused telemetry**.

**Status**: ✅ **WORKING** but with optimization opportunities

---

## 1. How hot_window Tracks "Hot" Entries

### Architecture Overview

The `hot_window` is a **circular buffer** designed for O(1) access to the most recent blocks:

```rust
// Location: crates/prism-core/src/cache/block_cache.rs:70-76
struct HotWindow {
    size: usize,                                    // Default: 200 blocks
    start_block: u64,                               // First block in window
    start_index: usize,                             // Circular buffer start position
    headers: Vec<Option<Arc<BlockHeader>>>,         // Ring buffer for headers
    bodies: Vec<Option<Arc<BlockBody>>>,            // Ring buffer for bodies
}
```

### "Hot" Definition

Blocks are considered "hot" based on **recency**, not access frequency:
- **Window Size**: Last 200 blocks (configurable via `hot_window_size`)
- **Insertion Logic**: Blocks are added to hot_window if they fall within the current window range
- **Eviction Strategy**: Oldest blocks automatically evicted when window advances

### Data Flow

```
Block Insert
    ↓
is_in_hot_window(block_number) → Check if block is recent enough
    ↓
insert() → Add to circular buffer (lines 94-133)
    ↓
advance_window() → Evict old blocks when window moves (lines 144-176)
```

**Key Code Locations**:
- **Insertion**: `block_cache.rs:264-312` (`insert_header`, `insert_body`)
- **Lookup**: `block_cache.rs:410-427` (`get_header_by_number` - checks hot_window first)
- **Window Management**: `block_cache.rs:144-176` (`advance_window`)

---

## 2. Background Tasks and Updates

### Current Background Tasks

#### ✅ Active Tasks (Started in `CacheManager::start_all_background_tasks`):

1. **FetchGuard Cleanup Worker**
   - **Purpose**: Processes cleanup requests from dropped FetchGuard instances
   - **Frequency**: Event-driven (triggered on FetchGuard drop)
   - **Code**: `cache/manager/background.rs:51-102`

2. **Inflight Fetch Cleanup**
   - **Purpose**: Removes stale fetch entries (>2min old)
   - **Frequency**: Every 60 seconds
   - **Code**: `cache/manager/background.rs:131-184`

3. **Background Stats Updater** (Log Cache Only)
   - **Purpose**: Forces stats update if >10min since last update
   - **Frequency**: Every 60 seconds
   - **Code**: `cache/log_cache/invalidation.rs:196-218`
   - **⚠️ LIMITATION**: Only updates log cache stats, not block cache!

4. **Background Cleanup Task**
   - **Purpose**: Prunes old cache entries based on finality
   - **Frequency**: Every 300 seconds (5 minutes) by default
   - **Code**: `cache/manager/background.rs:193-222`

### ❌ Missing: Hot Window Stats Background Task

**Issue**: There is **NO dedicated background task** for updating hot_window statistics.

**Current Behavior**:
- Stats are recomputed **on-demand** when `get_stats()` is called
- Uses a **dirty flag** pattern to avoid unnecessary recomputation:

```rust
// block_cache.rs:569-578
pub async fn get_stats(&self) -> CacheStats {
    // Only recompute if dirty flag is set
    if self.stats_dirty.load(Ordering::Relaxed) &&
        self.stats_dirty.swap(false, Ordering::Relaxed)
    {
        self.compute_stats_internal().await;  // ← This is where hot_window_size is calculated
    }
    self.stats.read().await.clone()
}
```

**Stats Computation**:

```rust
// block_cache.rs:580-595
async fn compute_stats_internal(&self) {
    let header_cache_size = self.headers_by_hash.len();
    let body_cache_size = self.bodies_by_hash.len();

    // ✅ hot_window_size IS being calculated here
    let hot_window_size = {
        let hot_window = self.hot_window.read().await;
        hot_window.headers.iter().filter(|h| h.is_some()).count()  // ← Counts populated slots
    };

    let mut stats = self.stats.write().await;
    stats.hot_window_size = hot_window_size;  // ← UPDATED HERE
    stats.block_cache_hits = self.hits.load(Ordering::Relaxed);
    stats.block_cache_misses = self.misses.load(Ordering::Relaxed);
}
```

---

## 3. Metrics and Counters

### ✅ Implemented Metrics

1. **Hot Window Size Gauge**
   ```rust
   // metrics/mod.rs:628
   gauge!("rpc_block_cache_hot_window_size").set(hot_window_size as f64);
   ```
   - **Updated**: When `record_cache_stats()` is called
   - **Source**: CacheStats from `CacheManager::get_stats()`

2. **Hot Window Advancement Counter** (DEFINED BUT UNUSED!)
   ```rust
   // metrics/mod.rs:667-670
   pub fn record_hot_window_advancement(&self, new_start_block: u64) {
       counter!("rpc_block_cache_hot_window_advancements_total").increment(1);
       gauge!("rpc_block_cache_hot_window_start").set(new_start_block as f64);
   }
   ```
   - **Status**: ❌ **NEVER CALLED**
   - **Issue**: Method exists but is not invoked anywhere in codebase

### ❌ Missing Metrics

The following useful metrics are NOT being collected:

- **Hot window hit rate** (% of lookups that hit hot_window vs DashMap)
- **Window advancement frequency** (how often the window slides forward)
- **Window utilization** (% of window slots populated)
- **Average block age in window**

---

## 4. Hot Window Calculation Requirements

### Lazy vs Eager Updates

**Current Strategy**: **Lazy with dirty flag optimization**

- ✅ **Efficient**: Only recomputes when stats are dirty
- ✅ **Accurate**: Stats reflect actual state when queried
- ❌ **Latency**: Admin API calls may block on stats computation

### When Stats Are Marked Dirty

Stats dirty flag is set on these operations:

```rust
// Operations that set stats_dirty = true:
self.stats_dirty.store(true, Ordering::Relaxed);
```

**Trigger Points** (from `block_cache.rs`):
1. Line 285: After `insert_header()`
2. Line 311: After `insert_body()`
3. Line 356: After `insert_headers_batch()`
4. Line 407: After `insert_bodies_batch()`
5. Line 518: After `invalidate_block()`
6. Line 546: After `clear_cache()`
7. Line 694: After pruning operations

### Stats Query Flow

```
Admin API: GET /admin/cache
    ↓
CacheManager::get_stats()  (cache_manager.rs:805)
    ↓
BlockCache::get_stats()  (block_cache.rs:569)
    ↓
Check dirty flag
    ↓ (if dirty)
compute_stats_internal()  (block_cache.rs:580)
    ↓
Acquire hot_window read lock
    ↓
Count populated slots: hot_window.headers.iter().filter(|h| h.is_some()).count()
    ↓
Update stats struct
    ↓
Return cached stats
```

**Performance Characteristics**:
- **Lock Acquisition**: RwLock read lock (uncontended in most cases)
- **Iteration Cost**: O(window_size) = O(200) operations
- **Filter Cost**: 200 `is_some()` checks (extremely cheap)
- **Expected Latency**: <1ms for 200-slot window

---

## 5. Is hot_window Actually Being Populated?

### ✅ Confirmation: YES, it is being populated

**Evidence**:

1. **Insertion Logic is Active**:
   ```rust
   // block_cache.rs:269-277
   pub async fn insert_header(&self, header: BlockHeader) {
       if self.is_in_hot_window(header_arc.number) {  // ✅ Check if recent enough
           let mut hot_window = self.hot_window.write().await;
           hot_window.insert(block_number, header_clone, empty_body);  // ✅ INSERT
       }
       // Also insert into DashMap for persistence
   }
   ```

2. **Lookup Checks Hot Window First**:
   ```rust
   // block_cache.rs:413-418
   pub fn get_header_by_number(&self, block_number: u64) -> Option<Arc<BlockHeader>> {
       if let Ok(hot_window) = self.hot_window.try_read() {
           if let Some(header) = hot_window.get_header(block_number) {
               return Some(header);  // ✅ Fast path hit
           }
       }
       // Fallback to DashMap
   }
   ```

3. **Stats Show Populated Entries**:
   ```rust
   // block_cache.rs:586
   hot_window.headers.iter().filter(|h| h.is_some()).count()
   ```
   - This count is exposed via admin API at `/admin/cache`
   - Non-zero values indicate populated slots

4. **Test Coverage Confirms Functionality**:
   - Test: `test_block_cache_hot_window` (lines 960-991)
   - Test: `test_hot_window_basic_insert_and_get` (lines 756-766)
   - All tests pass, confirming insertion and retrieval work

### Window Population Strategy

**Blocks are added to hot_window when**:
- Block number falls within current window range
- Both `insert_header()` and `insert_body()` are called for that block

**Blocks are NOT added to hot_window when**:
- Block is older than `start_block` (already evicted)
- Block is beyond window size (triggers window advancement first)

### Verification Command

To verify hot_window is populated in production:

```bash
# Check admin API for hot_window_size
curl -H "Authorization: Bearer <admin-token>" \
  http://localhost:3000/admin/cache | jq '.block_cache.hot_window_size'

# Non-zero value = hot_window is populated
# Value approaching 200 = window is nearly full (healthy state)
```

---

## 6. Performance Bottlenecks and Optimization Opportunities

### Current Performance Profile

| Operation | Complexity | Lock Type | Contention Risk |
|-----------|-----------|-----------|----------------|
| `insert()` | O(1) amortized | Write lock | Low (inserts are sequential by block) |
| `get_header()` | O(1) | Try-read lock | Very low (lock-free fallback) |
| `advance_window()` | O(advance) or O(size) | Write lock (already held) | N/A |
| `compute_stats()` | O(200) | Read lock | Very low |

### Identified Issues

#### 1. ❌ **Unused Telemetry: Hot Window Advancement Tracking**

**Issue**: `record_hot_window_advancement()` is defined but never called.

**Impact**: Missing visibility into window advancement frequency.

**Fix Location**: `block_cache.rs:123` (in `HotWindow::insert`)

```rust
// BEFORE (current):
if offset >= self.size {
    let advance = offset.saturating_sub(self.size).saturating_add(1);
    self.advance_window(advance, Some(block_number));  // ← No metrics here
}

// AFTER (proposed):
if offset >= self.size {
    let advance = offset.saturating_sub(self.size).saturating_add(1);
    self.advance_window(advance, Some(block_number));

    // ✅ ADD METRICS CALL (need access to metrics instance)
    // metrics.record_hot_window_advancement(self.start_block);
}
```

**Challenge**: `HotWindow` is a private struct with no access to `MetricsRegistry`.

**Solution Options**:
- **A)** Pass metrics callback to `HotWindow::new()`
- **B)** Emit metrics from `BlockCache` layer (track dirty flag changes)
- **C)** Add optional metrics field to `HotWindow` struct

#### 2. ⚠️ **Potential Lock Contention on Stats Computation**

**Issue**: `compute_stats_internal()` acquires read lock on hot_window during admin API calls.

**Impact**:
- In high-frequency admin API polling scenarios, stats computation could block inserts
- RwLock read locks are cheap but not zero-cost

**Measurement Needed**:
- Profile admin API latency under load
- Monitor `hot_window` lock wait times

**Optimization** (if needed):
- Pre-compute stats in background task (eliminate on-demand computation)
- Use atomic counters for populated slot count (eliminate iteration)

#### 3. ⚠️ **No Hit Rate Differentiation**

**Issue**: Hit/miss counters don't distinguish between hot_window hits and DashMap hits.

**Impact**: Can't measure hot_window effectiveness (is the O(1) path actually being used?).

**Fix**: Add separate counters:
```rust
hot_window_hits: AtomicU64,      // Fast path (O(1) ring buffer)
dashmap_hits: AtomicU64,         // Slow path (O(1) hash lookup but with lock)
```

---

## 7. Recommendations

### High Priority (Performance Impact)

1. **Instrument Hot Window Advancements**
   - **Action**: Call `metrics.record_hot_window_advancement()` when window advances
   - **Benefit**: Visibility into window churn rate
   - **Effort**: Low (2-5 lines of code)

2. **Add Hot Window Hit Rate Metric**
   - **Action**: Split hit counters into `hot_window_hits` and `dashmap_hits`
   - **Benefit**: Measure effectiveness of O(1) fast path
   - **Effort**: Medium (requires refactoring `get_header_by_number`)

3. **Profile Admin API Stats Latency**
   - **Action**: Add timing metrics to `compute_stats_internal()`
   - **Benefit**: Identify if on-demand stats computation is a bottleneck
   - **Effort**: Low (add `metrics::histogram!("cache_stats_compute_duration_ms")`)

### Medium Priority (Observability)

4. **Add Background Stats Update Task for Block Cache**
   - **Action**: Mirror log cache pattern - periodic forced stats update
   - **Benefit**: Reduce admin API latency (pre-computed stats)
   - **Effort**: Medium (new background task)

5. **Expose Window Utilization Metric**
   - **Action**: Track `populated_slots / total_slots` ratio
   - **Benefit**: Detect under-utilization (window too large) or thrashing
   - **Effort**: Low (already calculated in `compute_stats_internal`)

### Low Priority (Code Quality)

6. **Document Hot Window Design**
   - **Action**: Add module-level docs explaining circular buffer strategy
   - **Benefit**: Easier onboarding for new developers
   - **Effort**: Low (documentation only)

7. **Add Benchmarks for Hot Window Operations**
   - **Action**: Criterion benchmarks for insert/lookup/advance operations
   - **Benefit**: Prevent performance regressions
   - **Effort**: Medium (requires benchmark harness setup)

---

## 8. Conclusion

### Summary of Findings

| Aspect | Status | Notes |
|--------|--------|-------|
| **Is hot_window populated?** | ✅ YES | Active insertion and lookup confirmed |
| **Background stats updates?** | ⚠️ PARTIAL | Log cache only, block cache is on-demand |
| **Metrics collection?** | ⚠️ PARTIAL | Size tracked, but advancement metric unused |
| **Performance issues?** | ✅ NONE DETECTED | O(1) operations, low lock contention |
| **Optimization opportunities?** | ⚠️ MODERATE | Telemetry gaps, no hit rate differentiation |

### Key Insights

1. **Hot window is working as designed**: Recent blocks are cached in O(1) circular buffer
2. **Stats are computed lazily**: Efficient dirty-flag pattern, no stale data
3. **Telemetry is incomplete**: Several metrics defined but not collected
4. **No background stats task for block cache**: Unlike log cache, stats only update on API calls

### Next Steps

**Immediate** (this sprint):
- Add hot window advancement metric instrumentation
- Profile admin API stats endpoint latency

**Short-term** (next sprint):
- Implement hit rate differentiation (hot_window vs DashMap)
- Add background stats updater for block cache

**Long-term** (backlog):
- Benchmark hot window operations under realistic load
- Consider pre-computed stats for high-frequency admin API usage

---

## Appendix: Code References

### Key Files

| File | Purpose | Lines of Interest |
|------|---------|------------------|
| `crates/prism-core/src/cache/block_cache.rs` | Hot window implementation | 60-224 (HotWindow), 569-595 (stats) |
| `crates/prism-core/src/cache/cache_manager.rs` | Stats aggregation | 805-841 |
| `crates/prism-core/src/cache/manager/background.rs` | Background tasks | 193-280 (cleanup task) |
| `crates/prism-core/src/metrics/mod.rs` | Metrics definitions | 667-670 (unused advancement metric) |
| `crates/server/src/admin/handlers/cache.rs` | Admin API stats endpoint | 100-142 (stats formatting) |

### Configuration

**Default hot_window_size**: 200 blocks

**Config locations**:
- `config/development.toml:88`
- `crates/prism-core/src/cache/block_cache.rs:31` (default value)

### Related Documentation

- **Architecture**: `docs/caching-system.md`
- **Block cache details**: `docs/components/cache/block_cache.md`
- **Cache manager**: `docs/components/cache/cache_manager.md`

---

**Report Generated**: 2025-12-07
**Analyzed Codebase**: prism (feat/admin-api branch)
**Analysis Focus**: Hot window cache population and performance characteristics
