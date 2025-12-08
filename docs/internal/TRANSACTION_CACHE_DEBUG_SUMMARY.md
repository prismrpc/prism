# Transaction Cache Debug Summary

## Quick Diagnosis Guide

### Problem
- Receipts cached: 5000 entries ✅
- Transactions cached: 0 entries ❌
- Hot window: 42 entries ✅

### Where to Look

#### File: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs`

**Line 648**: Request is made with `fullTransactions=true`
```rust
"params": [with_hex_u64(block_number, std::string::ToString::to_string), true]
```

**Lines 673-708**: NEW - Response structure detection
- Will log "array_of_hashes" if BUG detected
- Will log "array_of_objects" if working correctly
- Will log "empty_array" if no transactions
- Will log "null" if transactions field is null

**Lines 796-809**: NEW - Hash vs Object detection
```rust
if first_tx.is_string() {
    tracing::error!(
        "BUG: eth_getBlockByNumber returned transaction HASHES instead of full objects!"
    );
    return; // Abort caching
}
```

**Lines 811-824**: NEW - Individual transaction conversion logging
```rust
if let Some(tx) = json_transaction_to_transaction_record(tx_json) {
    cache_manager.transaction_cache.insert_transaction(tx).await;
    cached_count += 1;
} else {
    failed_count += 1;
    tracing::debug!(tx_json = ?tx_json, "failed to convert transaction to record");
}
```

## Log Patterns to Search For

### Critical Error (Likely Root Cause)
```bash
grep "BUG: eth_getBlockByNumber returned transaction HASHES" logs
```

If this appears, the problem is that the `fullTransactions=true` parameter is being ignored.

### Transaction Type Detection
```bash
grep "transactions_type" logs | tail -10
```

Should show:
- `transactions_type="array_of_objects (count: N)"` ✅ GOOD
- `transactions_type="array_of_hashes (count: N)"` ❌ BUG!

### Successful Caching
```bash
grep "cached transactions" logs | tail -10
```

Should show:
```
DEBUG cached transactions transaction_count=42 block_number=12345
```

### Conversion Failures
```bash
grep "failed to convert transaction" logs | tail -10
```

Will show the actual transaction JSON that failed to convert.

## Test Command

```bash
# Run server with debug logging
RUST_LOG=debug cargo run --bin prism-server 2>&1 | tee debug.log

# In another terminal, check cache stats
curl -s http://localhost:3030/admin/cache/stats | jq '{
  receipts: .receipt_cache.entries,
  transactions: .transaction_cache.entries,
  hot_window: .hot_window_size
}'

# Wait for a few blocks, then search logs
grep "transactions_type" debug.log | tail -5
```

## Expected Findings

### Most Likely: Transaction Hashes Bug
```
INFO received block response transactions_type="array_of_hashes (count: 42)"
ERROR BUG: eth_getBlockByNumber returned transaction HASHES instead of full objects!
```

**Fix**: Check upstream provider configuration or implementation of `eth_getBlockByNumber`.

### Less Likely: Empty Blocks
```
INFO received block response transactions_type="empty_array"
DEBUG block has no transactions (empty block)
```

**Fix**: This is normal for some blocks. But if ALL blocks are empty, check upstream data.

### Unlikely: Missing Field
```
WARN block response has no transactions field at all
```

**Fix**: Upstream is returning malformed responses.

## Verification After Fix

1. Restart server with debug logging
2. Wait for new block via WebSocket
3. Check logs for "cached transactions" message
4. Verify cache stats:
   ```bash
   curl -s http://localhost:3030/admin/cache/stats | jq .transaction_cache.entries
   ```
5. Should show non-zero value if fix worked

## Files Changed

Single file modified:
- `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs`

Three sections enhanced:
1. **Request logging** (lines 651-656)
2. **Response structure detection** (lines 673-708)
3. **Transaction caching with detailed errors** (lines 765-847)

## Code Quality

✅ Compiles without errors
✅ No clippy warnings
✅ Preserves existing functionality
✅ Adds comprehensive diagnostics
✅ Zero-cost when transactions cache correctly
✅ Detailed error reporting when they don't
