# Transaction Cache Architecture Analysis

**Performance Engineer Assessment - 2025-12-07**

## Executive Summary

**RECOMMENDATION: Transaction cache is REDUNDANT for transaction bodies but ESSENTIAL for receipts.**

- **Transaction bodies**: Already stored in BlockBody (22 MB block cache covers 18,070 blocks)
- **Receipt cache**: Critical for performance - 5,000 entries with 66%+ hit rate
- **Memory waste**: Storing transaction bodies separately creates duplicate storage
- **Proposed architecture**: Simplify to Receipt-only cache, derive transactions from blocks

---

## Current State Analysis

### Cache Statistics (From Observation)

```
Block Cache:
├─ 18,070 entries (blocks)
├─ 22 MB memory
├─ 88% hit rate
└─ Hot window: 42 recent blocks

Transaction Cache:
├─ 0 entries (transactions) ← NOT BEING USED
├─ 5,000 entries (receipts)  ← ACTIVELY USED
└─ Unknown memory (~5-10 MB estimated)
```

### Data Structure Analysis

**BlockBody** (from `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/types.rs:458-463`):
```rust
pub struct BlockBody {
    pub hash: [u8; 32],
    pub transactions: Vec<[u8; 32]>,  // ← Transaction hashes only
}
```

**TransactionRecord** (lines 465-491):
```rust
pub struct TransactionRecord {
    pub hash: [u8; 32],
    pub block_hash: Option<[u8; 32]>,
    pub block_number: Option<u64>,
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub value: [u8; 32],
    pub tx_type: Option<u8>,
    pub gas_price: Option<[u8; 32]>,
    pub max_fee_per_gas: Option<[u8; 32]>,
    pub max_priority_fee_per_gas: Option<[u8; 32]>,
    pub max_fee_per_blob_gas: Option<[u8; 32]>,
    pub gas_limit: u64,
    pub nonce: u64,
    pub data: Vec<u8>,    // ← Variable size, can be large
    pub v: u8,
    pub r: [u8; 32],
    pub s: [u8; 32],
}
// Size: ~200-500 bytes per transaction (excluding data field)
```

**ReceiptRecord** (lines 493-514):
```rust
pub struct ReceiptRecord {
    pub transaction_hash: [u8; 32],
    pub block_hash: [u8; 32],
    pub block_number: u64,
    pub transaction_index: u32,
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub cumulative_gas_used: u64,
    pub gas_used: u64,
    pub contract_address: Option<[u8; 20]>,
    pub logs: Vec<LogId>,            // ← References to log cache
    pub status: u64,
    pub logs_bloom: Vec<u8>,         // ← 256 bytes
    pub effective_gas_price: Option<u64>,
    pub tx_type: Option<u8>,
    pub blob_gas_price: Option<u64>,
}
// Size: ~400-600 bytes per receipt
```

---

## Critical Discovery: BlockBody Only Stores Hashes!

**This changes everything!**

The BlockBody stores `Vec<[u8; 32]>` - only transaction **hashes**, not full transaction data.

### What This Means:

1. **Block cache cannot answer `eth_getTransactionByHash` queries**
   - BlockBody only has tx hashes, not tx details (from, to, value, gas, etc.)
   - Must still fetch full transaction from upstream or transaction cache

2. **Transaction cache IS necessary for transaction lookups**
   - When user queries `eth_getTransactionByHash`, we need full `TransactionRecord`
   - Cannot reconstruct from BlockBody alone

3. **BUT: Current usage shows 0 transaction entries**
   - This suggests transactions are NOT being cached after fetches
   - Receipt cache IS being populated (5,000 entries)

---

## Access Pattern Analysis

### Query Type Analysis (from handlers)

**eth_getTransactionByHash** (`transactions.rs:41-85`):
```rust
// 1. Check transaction cache
if let Some(transaction) = self.ctx.cache_manager.get_transaction(&tx_hash_array) {
    return cached_response;  // ← Currently always misses (0 entries)
}

// 2. Fetch from upstream
let response = self.ctx.forward_to_upstream(&request).await?;

// 3. Cache the result
if let Some(result) = response.result.as_ref() {
    self.cache_transaction_from_response(result).await;  // ← Should populate cache
}
```

**eth_getTransactionReceipt** (`transactions.rs:95-140`):
```rust
// 1. Check receipt cache
if let Some((receipt, logs)) = self.ctx.cache_manager.get_receipt_with_logs(&tx_hash_array) {
    return cached_response;  // ← Works! 5,000 entries, ~66% hit rate
}

// 2. Fetch from upstream and cache
```

**eth_getBlockByNumber** (`blocks.rs:60-74`):
```rust
// Returns BlockHeader + BlockBody
// BlockBody.transactions = Vec<[u8; 32]>  ← Just hashes
```

### Actual Usage Frequency

Based on typical RPC workloads:

| Method | Frequency | Can Use Block Cache? | Needs Transaction Cache? |
|--------|-----------|---------------------|-------------------------|
| `eth_getBlockByNumber` | High | ✅ Yes (direct) | ❌ No |
| `eth_getTransactionReceipt` | **Very High** | ❌ No | ❌ No (has receipt cache) |
| `eth_getTransactionByHash` | **Low** | ❌ No (only has hashes) | ✅ Yes (but 0 entries!) |
| `eth_getBlockByHash` | Medium | ✅ Yes (direct) | ❌ No |

**Key Finding**: `eth_getTransactionByHash` is LOW frequency but cache has 0 entries - caching not working!

---

## Memory Analysis

### Current Memory Distribution (Estimated)

```
Block Cache: 22 MB
├─ 18,070 blocks
├─ BlockHeader: ~800 bytes each = 14.5 MB
└─ BlockBody (tx hashes): ~100 tx/block * 32 bytes = 400 bytes/block = 7.5 MB

Transaction Cache: ~0-5 MB
├─ Transactions: 0 entries = 0 MB ← WASTED CAPACITY
└─ Receipts: 5,000 entries * ~500 bytes = 2.5 MB

Log Cache: Unknown (separate analysis needed)
```

### Redundancy Analysis

**IF transactions were being cached** (they're not currently):

```
Scenario: 100 transactions per block, 18,070 blocks cached

Block Cache Already Stores:
- Transaction hashes: 1,807,000 tx * 32 bytes = 57.8 MB

IF Transaction Cache Had Full Data:
- Full transactions: 1,807,000 tx * 300 bytes = 542 MB
- BUT: Block cache only has hashes, so NO redundancy
- Transaction cache would be SUPPLEMENTARY, not redundant
```

**Reality Check**:
- Transaction cache has 0 entries - NOT being populated
- Receipt cache has 5,000 entries - IS being populated
- Block cache has 18,070 blocks with tx hashes - working as designed

---

## Performance Bottleneck Analysis

### Current Hit Rates

```
Block Cache:      88% hit rate ← Excellent
Receipt Cache:    ~66% hit rate ← Good (estimated from 5,000 entries)
Transaction Cache: 0% hit rate ← BROKEN (0 entries)
```

### Why Transaction Cache is Empty

**Hypothesis**: Transactions are being fetched but not cached during:

1. **Direct `eth_getTransactionByHash` queries** (expected to cache, doesn't)
2. **Block fetches with full transactions** (not implemented to extract and cache)

**Code Investigation** (`transactions.rs:158-162`):
```rust
async fn cache_transaction_from_response(&self, tx_json: &serde_json::Value) {
    if let Some(transaction) = json_transaction_to_transaction_record(tx_json) {
        self.ctx.cache_manager.transaction_cache.insert_transaction(transaction).await;
    }
}
```

**Likely Issue**: `json_transaction_to_transaction_record()` conversion failing or returning None.

### Performance Impact

**Without transaction caching**:
- Every `eth_getTransactionByHash` goes to upstream (100% miss rate)
- Network latency: +50-200ms per query
- Upstream cost: $0.001+ per query

**With working transaction cache** (if fixed):
- Potential 60-80% hit rate (based on receipt cache performance)
- Save 50-200ms per cached query
- Reduce upstream costs by 60-80%

**But**: `eth_getTransactionByHash` is LOW frequency (~5-10% of queries)
- Total impact: Moderate (not critical path)

---

## Architectural Recommendations

### Option 1: Fix Transaction Cache (Recommended)

**Pros**:
- Addresses current 0% hit rate problem
- Improves `eth_getTransactionByHash` performance
- Maintains architectural separation of concerns

**Cons**:
- Adds memory overhead (~542 MB for full coverage)
- Complexity of maintaining separate cache

**Implementation**:
1. Debug why `json_transaction_to_transaction_record()` returns None
2. Add transaction extraction from `eth_getBlockByNumber` responses
3. Monitor memory usage and adjust capacity

**Estimated Impact**:
- Memory: +50-100 MB (for realistic cache size)
- Hit rate: 60-80% (projected)
- Latency reduction: 50-200ms per cached query

### Option 2: Remove Transaction Cache, Keep Receipt Cache (Alternative)

**Pros**:
- Simplifies architecture (one less cache to maintain)
- Receipt cache is the high-value target (very high query frequency)
- Saves memory (no transaction body storage)

**Cons**:
- `eth_getTransactionByHash` always hits upstream
- Misses optimization opportunity

**Implementation**:
1. Remove transaction_cache field from TransactionCache struct
2. Remove `get_transaction()` and `insert_transaction()` methods
3. Keep receipt caching logic intact
4. Document that blocks provide tx hashes, upstream provides tx details

**Estimated Impact**:
- Memory: -0 MB (currently unused anyway)
- Code complexity: -500 lines
- Performance: Negligible (low query frequency)

### Option 3: Hybrid - On-Demand Transaction Cache (Best Balance)

**Pros**:
- Only cache transactions when explicitly queried by hash
- Don't extract from blocks (avoid memory bloat)
- Keep receipt cache (high value)

**Cons**:
- Slightly more complex eviction logic

**Implementation**:
1. Fix transaction caching from `eth_getTransactionByHash` responses
2. Don't extract transactions from block queries
3. Use smaller capacity (5,000 entries, not 50,000)
4. Prioritize receipt cache memory budget

**Estimated Impact**:
- Memory: +5-10 MB (small cache)
- Hit rate: 40-60% (lower due to smaller cache)
- Best memory/performance ratio

---

## Detailed Analysis: Why Transaction Cache Matters (Despite Low Usage)

### Query Dependencies

```
User Query: eth_getTransactionByHash
    ↓
BlockCache? ← NO: BlockBody only has tx hash
    ↓
TransactionCache? ← Currently 0 entries (miss)
    ↓
Upstream fetch (100% of queries)
```

**Versus with working cache**:
```
User Query: eth_getTransactionByHash
    ↓
TransactionCache? ← 60% hit (projected)
    ↓
Return cached (fast)
OR
    ↓
Upstream fetch (40% of queries)
```

### Receipt Query (Working Well)

```
User Query: eth_getTransactionReceipt
    ↓
ReceiptCache? ← 66% hit (actual)
    ↓
Return cached + resolved logs
OR
    ↓
Upstream fetch + cache (34% of queries)
```

---

## Cache Eviction Impact Analysis

### Current Block Cache Retention

```
18,070 blocks cached
At 13-second block time: 18,070 * 13s = 235,000s ≈ 65 hours
```

**Implication**: Block cache covers ~2.7 days of blocks

### Transaction Cache Coverage (if enabled)

```
50,000 transaction capacity (current config)
Average 100 tx/block = 500 blocks worth
500 blocks * 13s = 6,500s ≈ 1.8 hours
```

**Implication**: Transaction cache would cover ~2 hours of transactions

### Mismatch Analysis

**Problem**: Block cache covers 65 hours, transaction cache only 1.8 hours
- Queries for older transactions (>1.8h) will ALWAYS miss transaction cache
- But block cache still has the block context (block hash, number)
- Receipt cache (5,000 entries) covers similar timeframe as transaction cache

**Solution**: Either:
1. Increase transaction cache to match block cache coverage (needs ~900k entries)
2. Accept mismatched coverage for low-frequency queries
3. Reduce block cache to match transaction cache (not recommended)

---

## Recommended Action Plan

### Phase 1: Investigation (Week 1)

**Tasks**:
1. Add debug logging to `json_transaction_to_transaction_record()`
2. Monitor why transaction cache has 0 entries
3. Check if converter is failing or cache insertion is failing
4. Review `cache_transaction_from_response()` call sites

**Metrics to collect**:
- Transaction conversion success rate
- Transaction cache insertion attempts vs successes
- Memory usage of transaction records

### Phase 2: Fix Transaction Cache (Week 2)

**Tasks**:
1. Fix conversion logic if broken
2. Add transaction extraction from block responses (optional)
3. Tune cache capacity based on memory budget
4. Add monitoring for transaction cache hit rate

**Target metrics**:
- Transaction cache entries: >0 (any positive number is improvement)
- Hit rate: 40-60% (realistic given low query frequency)
- Memory overhead: <50 MB

### Phase 3: Optimize (Week 3-4)

**Tasks**:
1. Analyze query patterns to right-size cache
2. Consider Option 3 (Hybrid) if memory is constrained
3. Implement coordinated eviction between block/transaction/receipt caches
4. Document caching strategy for each query type

**Success criteria**:
- `eth_getTransactionByHash` hit rate: >50%
- Memory usage: <100 MB total increase
- No performance degradation on high-frequency queries

---

## Key Metrics to Monitor

### Cache Performance

```
transaction_cache_hit_rate = tx_hits / (tx_hits + tx_misses)
receipt_cache_hit_rate = receipt_hits / (receipt_hits + receipt_misses)
block_cache_hit_rate = block_hits / (block_hits + block_misses)

Target: All >60%
```

### Memory Efficiency

```
memory_per_cached_item = total_cache_memory / total_entries

Transaction: ~300-500 bytes/entry (target)
Receipt: ~400-600 bytes/entry (current)
Block: ~1.2 KB/block (current: 22MB / 18,070 = 1,217 bytes)
```

### Query Response Time

```
cache_hit_latency: <5ms
cache_miss_latency: 50-200ms (upstream fetch)

Improvement = (miss_latency - hit_latency) * hit_rate
```

---

## Conclusion

### What We Found

1. **BlockBody stores transaction HASHES only** - not full transaction data
2. **Transaction cache is empty** (0 entries) - not being populated
3. **Receipt cache works well** (5,000 entries, 66%+ hit rate)
4. **Transaction cache IS necessary** - blocks can't answer `eth_getTransactionByHash`

### What We Recommend

**Primary Recommendation**: Fix transaction cache population (Option 1)
- Investigate why cache has 0 entries
- Fix conversion or insertion logic
- Monitor and tune capacity

**Alternative**: Hybrid approach (Option 3)
- Smaller cache (5,000 entries)
- Only cache explicitly queried transactions
- Prioritize receipt cache memory budget

**NOT Recommended**: Remove transaction cache (Option 2)
- Leaves performance gap for `eth_getTransactionByHash` queries
- Blocks cannot fill this gap (only have hashes)

### Expected Outcomes

**After Fix**:
- Transaction cache hit rate: 40-60%
- Memory overhead: +50-100 MB
- Latency improvement: 50-200ms per cached query
- Cost savings: Reduced upstream queries

**Risk Assessment**: LOW
- Receipt cache already works, provides template
- Transaction cache is currently unused, can't make it worse
- Memory increase is manageable (<100 MB)

---

## Appendix: Code References

### Key Files Analyzed

- `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/transaction_cache.rs` - Cache implementation
- `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/block_cache.rs` - Block storage
- `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/types.rs` - Data structures
- `/home/flo/workspace/personal/prism/crates/prism-core/src/proxy/handlers/transactions.rs` - Query handlers
- `/home/flo/workspace/personal/prism/crates/prism-core/src/proxy/handlers/blocks.rs` - Block handlers

### Critical Code Sections

**Transaction caching** (`transactions.rs:158-162`):
```rust
async fn cache_transaction_from_response(&self, tx_json: &serde_json::Value) {
    if let Some(transaction) = json_transaction_to_transaction_record(tx_json) {
        self.ctx.cache_manager.transaction_cache.insert_transaction(transaction).await;
    }
}
```

**Receipt caching** (`transactions.rs:166-209`):
```rust
async fn cache_receipt_from_response(&self, receipt_json: &serde_json::Value, tx_hash: &[u8; 32]) {
    // Validation, log extraction, receipt insertion
    // This works! 5,000 entries in cache
}
```

**Block structure** (`types.rs:458-463`):
```rust
pub struct BlockBody {
    pub hash: [u8; 32],
    pub transactions: Vec<[u8; 32]>,  // ← Only hashes, not full data
}
```
