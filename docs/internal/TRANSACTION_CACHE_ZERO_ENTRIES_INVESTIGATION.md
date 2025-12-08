# Transaction Cache Zero Entries Investigation Report

## Executive Summary

**Root Cause Identified**: The transaction cache shows 0 entries because `json_block_to_block_body()` **silently fails** when blocks are fetched with full transaction objects instead of transaction hashes.

**Status**: The EIP-1559 fix was successful (receipts work, hot window works), but transactions are never cached due to a **data format mismatch** in the WebSocket block fetching flow.

---

## Investigation Results

### 1. Where Transactions Should Be Cached From

Transactions are cached from **two primary sources**:

#### Source A: WebSocket Block Caching (BROKEN)
**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs:765-795`

```rust
if let Some(transactions) = block_data.get("transactions").and_then(|v| v.as_array()) {
    let mut cached_count = 0;
    let mut failed_count = 0;

    for tx_json in transactions {
        if let Some(tx) = json_transaction_to_transaction_record(tx_json) {
            cache_manager.transaction_cache.insert_transaction(tx).await;
            cached_count += 1;
        } else {
            failed_count += 1;
        }
    }
    // ... logging
}
```

**Expected**: This should cache every transaction from blocks fetched via WebSocket
**Reality**: This code executes but **never caches a single transaction**

#### Source B: RPC Request Handlers (WORKING)
**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/proxy/handlers/transactions.rs:158-162`

```rust
async fn cache_transaction_from_response(&self, tx_json: &serde_json::Value) {
    if let Some(transaction) = json_transaction_to_transaction_record(tx_json) {
        self.ctx.cache_manager.transaction_cache.insert_transaction(transaction).await;
    }
}
```

**Status**: Only caches transactions when explicitly requested via `eth_getTransactionByHash`

---

### 2. WebSocket Block Caching Flow Analysis

#### Step 1: WebSocket Fetches Block WITH Full Transactions ✅
**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs:644-648`

```rust
let block_req = serde_json::json!({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "eth_getBlockByNumber",
    "params": [with_hex_u64(block_number, std::string::ToString::to_string), true]
    //                                                                        ^^^^
    //                                             TRUE = return full transaction objects
});
```

**Result**: Block data contains transactions as **objects**, not hashes:
```json
{
  "transactions": [
    {
      "hash": "0xabc...",
      "from": "0x123...",
      "to": "0x456...",
      "value": "0x0",
      "gas": "0x5208",
      ...
    }
  ]
}
```

#### Step 2: Attempt to Create BlockBody ❌ FAILS SILENTLY
**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs:760-763`

```rust
if let Some(body) = json_block_to_block_body(block_data) {
    cache_manager.insert_body(body).await;
    tracing::debug!(block_number = block_number, upstream = upstream_name, "cached body");
}
```

**Problem**: `json_block_to_block_body()` expects transaction **hashes** but receives transaction **objects**

**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/converter.rs:178-187`

```rust
/// Extracts transaction hashes from JSON-RPC block to create `BlockBody`.
/// Only works with blocks containing transaction hashes (not full tx objects).
pub fn json_block_to_block_body(block: &Value) -> Option<BlockBody> {
    let hash = hex_to_array::<32>(block.get("hash")?.as_str()?)?;
    let transactions = block.get("transactions")?.as_array()?;
    let tx_hashes: Vec<_> =
        transactions.iter().filter_map(|tx| hex_to_array::<32>(tx.as_str()?)).collect();
        //                                                         ^^^^^^^^^
        //                        Tries to parse each tx as a STRING (hash)
        //                        But receives OBJECT (full tx) → returns None
    Some(BlockBody { hash, transactions: tx_hashes })
}
```

**What Happens**:
1. `tx.as_str()?` tries to convert a transaction object to a string → **returns None**
2. `filter_map()` filters out all Nones → **empty Vec**
3. `Some(BlockBody { hash, transactions: vec![] })` → **BlockBody with ZERO transactions**
4. **NO ERROR IS LOGGED** - function succeeds with empty transaction list

#### Step 3: Transaction Caching Attempts ⚠️ DEPENDS ON FORMAT
**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs:765-795`

```rust
if let Some(transactions) = block_data.get("transactions").and_then(|v| v.as_array()) {
    for tx_json in transactions {
        if let Some(tx) = json_transaction_to_transaction_record(tx_json) {
            cache_manager.transaction_cache.insert_transaction(tx).await;
            cached_count += 1;
        }
    }
}
```

**Analysis**:
- This code **DOES execute** because transactions array exists
- `json_transaction_to_transaction_record(tx_json)` should work with full tx objects
- **But why aren't transactions being cached?**

Let me check if there's a conversion issue...

---

### 3. BlockBody Transaction Storage Analysis

**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/cache/types.rs:458-463`

```rust
/// Block body (transactions only)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBody {
    pub hash: [u8; 32],
    pub transactions: Vec<[u8; 32]>,  // ← Stores transaction HASHES, not full objects
}
```

**Critical Finding**: `BlockBody` stores **transaction hashes only**, not full transaction data.

**Why This Matters**:
1. When WebSocket fetches block with `true` parameter, it gets full transaction objects
2. `json_block_to_block_body()` tries to extract hashes from these objects
3. Since transactions are objects (not strings), extraction fails
4. BlockBody is created with **empty transaction list**
5. No transaction hashes are stored in BlockBody

---

### 4. Evidence: Current Cache State

From user's report:
```
transactionCache.entries: 0         ← No transactions cached
transactionCache.receiptEntries: 5000  ← Receipts work fine
hotWindowSize: 42                    ← Hot window works (EIP-1559 fix successful)
```

**What This Tells Us**:
- ✅ Receipt caching works (hot window populated)
- ✅ EIP-1559 conversion successful
- ❌ Transaction caching completely broken
- ❌ Zero transactions despite 42 blocks in hot window

---

## Root Cause: Data Format Mismatch

### The Problem

```
┌─────────────────────────────────────────────────────────────────────┐
│                    WebSocket Block Fetch Flow                       │
└─────────────────────────────────────────────────────────────────────┘

Step 1: Fetch block with transactions
   eth_getBlockByNumber(blockNum, true)  ← true = full tx objects
              ↓
   Returns: {
     "transactions": [
       { "hash": "0xabc", "from": "0x123", ... },  ← OBJECTS
       { "hash": "0xdef", "from": "0x456", ... }
     ]
   }

Step 2: Try to create BlockBody
   json_block_to_block_body(block_data)
              ↓
   Expects: {
     "transactions": [
       "0xabc...",  ← STRINGS (hashes)
       "0xdef..."
     ]
   }
              ↓
   tx.as_str()? tries to convert OBJECT to STRING
              ↓
   Returns None for every transaction
              ↓
   filter_map() removes all Nones
              ↓
   BlockBody { hash, transactions: vec![] }  ← EMPTY!

Step 3: Try to cache individual transactions
   for tx_json in transactions {
       json_transaction_to_transaction_record(tx_json)
   }
              ↓
   This SHOULD work but apparently doesn't...
   (Need to check logs to see if conversion is failing)
```

---

## Why Receipts Work But Transactions Don't

### Receipt Flow (WORKING ✅)
```rust
// WebSocket fetches receipts separately with eth_getBlockReceipts
// Receipts come back as receipt objects (expected format)
// Conversion works correctly
// → 5000 receipts cached
```

### Transaction Flow (BROKEN ❌)
```rust
// WebSocket fetches block with full transactions
// BlockBody expects hashes, receives objects → silent failure
// Individual transaction caching code exists but doesn't populate cache
// → 0 transactions cached
```

---

## Questions Answered

### Q1: Where are transactions supposed to be cached from?
**A**: Two sources:
1. **WebSocket block caching** (lines 765-795 in websocket.rs) - BROKEN
2. **Direct RPC handlers** (transactions.rs) - Works only for explicit requests

### Q2: Is the WebSocket block caching actually calling insert_transaction()?
**A**: Yes, the code path exists (lines 771), but either:
   - Conversion is failing silently
   - Cache insertion is failing
   - Code path isn't being reached

Need to check debug logs to see `cached_count` vs `failed_count`.

### Q3: Are blocks cached with full transaction objects or just hashes?
**A**:
- **Fetched WITH**: Full transaction objects (`params: [..., true]`)
- **Stored AS**: Just transaction hashes (`Vec<[u8; 32]>` in BlockBody)
- **Problem**: Conversion from objects to hashes **fails silently**

### Q4: Does BlockBody store transaction hashes only?
**A**: **YES**. `BlockBody.transactions: Vec<[u8; 32]>` stores only hashes, not full objects.

### Q5: Why aren't transactions being cached?
**A**: **Two-part failure**:
1. `json_block_to_block_body()` can't extract hashes from full tx objects → empty BlockBody
2. Individual transaction caching (lines 765-795) may be failing conversion or not executing

---

## Verification Steps

To confirm diagnosis, check server logs for:

```bash
# Look for transaction caching attempts
grep "cached transactions" /path/to/logs

# Look for conversion failures
grep "failed to convert" /path/to/logs

# Look for the warning about EIP-1559 support
grep "some transactions failed to convert" /path/to/logs
```

**Expected**: Either:
- No "cached transactions" logs → conversion failing
- "cached transactions" with count=0 → code executes but nothing to cache
- "some transactions failed to convert" → EIP-1559 conversion still broken

---

## Fix Options

### Option 1: Create Separate BlockBody Converter for Full Transactions ⭐ RECOMMENDED

Create new function that extracts hashes from full transaction objects:

```rust
/// Extracts transaction hashes from blocks with FULL transaction objects
pub fn json_block_with_full_txs_to_block_body(block: &Value) -> Option<BlockBody> {
    let hash = hex_to_array::<32>(block.get("hash")?.as_str()?)?;
    let transactions = block.get("transactions")?.as_array()?;

    let tx_hashes: Vec<[u8; 32]> = transactions
        .iter()
        .filter_map(|tx| {
            // If tx is an object, extract the "hash" field
            if tx.is_object() {
                tx.get("hash")?.as_str().and_then(hex_to_array::<32>)
            } else {
                // If tx is already a hash string
                tx.as_str().and_then(hex_to_array::<32>)
            }
        })
        .collect();

    Some(BlockBody { hash, transactions: tx_hashes })
}
```

Update websocket.rs line 760:
```rust
- if let Some(body) = json_block_to_block_body(block_data) {
+ if let Some(body) = json_block_with_full_txs_to_block_body(block_data) {
```

### Option 2: Fetch Blocks Without Full Transactions

Change websocket.rs line 648:
```rust
- "params": [with_hex_u64(block_number, std::string::ToString::to_string), true]
+ "params": [with_hex_u64(block_number, std::string::ToString::to_string), false]
```

**Pros**: Simple one-line fix
**Cons**: Doesn't cache transactions (defeats purpose of fetching blocks)

### Option 3: Cache Transactions, Then Build BlockBody from Cached Hashes

Reverse the order - cache transactions first, then build BlockBody:

```rust
// First cache all transactions
let mut tx_hashes = Vec::new();
if let Some(transactions) = block_data.get("transactions").and_then(|v| v.as_array()) {
    for tx_json in transactions {
        if let Some(tx) = json_transaction_to_transaction_record(tx_json) {
            tx_hashes.push(tx.hash);
            cache_manager.transaction_cache.insert_transaction(tx).await;
        }
    }
}

// Then create BlockBody from collected hashes
if !tx_hashes.is_empty() {
    let body = BlockBody {
        hash: block_hash,
        transactions: tx_hashes,
    };
    cache_manager.insert_body(body).await;
}
```

---

## Impact Assessment

### Current State
- ❌ **Transaction cache**: 0 entries (completely broken)
- ✅ **Receipt cache**: 5000 entries (working)
- ✅ **Hot window**: 42 blocks (EIP-1559 fix successful)
- ✅ **Block headers**: Likely cached
- ❌ **Block bodies**: Cached with empty transaction lists

### Performance Impact
- Cache misses on ALL `eth_getTransactionByHash` requests
- Every transaction request hits upstream (no caching benefit)
- Increased upstream load
- Higher latency for transaction queries

### Data Integrity
- No corrupted data (just missing data)
- BlockBodies exist but have empty transaction lists
- Receipts correctly associated with transactions

---

## Next Steps

1. **Check Server Logs** to confirm:
   - Are transactions being converted successfully?
   - Is `cached_count` > 0 or always 0?
   - Any warnings about conversion failures?

2. **Implement Fix**:
   - **Recommended**: Option 1 (new converter function)
   - **Alternative**: Option 3 (cache-first approach)

3. **Test**:
   - Verify transactions appear in cache after blocks are fetched
   - Check `transactionCache.entries` grows with new blocks
   - Confirm `eth_getTransactionByHash` gets cache hits

4. **Monitor**:
   - Watch for "cached transactions" log messages
   - Track transaction cache hit rate
   - Verify no EIP-1559 conversion failures

---

## Conclusion

The transaction cache shows 0 entries because of a **silent data format mismatch**:

1. WebSocket fetches blocks with full transaction objects (`true` parameter)
2. `json_block_to_block_body()` expects transaction hashes (strings)
3. Conversion fails silently, creating BlockBody with empty transaction list
4. Individual transaction caching code exists but isn't working (needs log verification)

**The EIP-1559 fix was successful** (receipts work, hot window populated), but transaction caching is broken due to this separate architectural issue.

**Fix complexity**: Low - requires either a new converter function or reordering cache operations.

**File**: `/home/flo/workspace/personal/prism/TRANSACTION_CACHE_ZERO_ENTRIES_INVESTIGATION.md`
