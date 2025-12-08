# Transaction Cache Debugging Patch

## Problem Statement

After the EIP-1559 fix:
- **Receipts are cached**: 5000 entries
- **Transactions are NOT cached**: 0 entries
- **Hot window works**: 42 entries

This indicates that transactions are not being cached despite the code appearing correct.

## Root Cause Analysis

The issue was **lack of diagnostic logging** in the transaction caching path. The code at `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs` line 765 had several silent failure modes:

### Silent Failure Modes Identified

1. **Missing transactions field**: No logging when `block_data.get("transactions")` returns `None`
2. **Non-array transactions**: No logging when transactions field is not an array (could be null, string, etc.)
3. **Empty blocks**: No distinction between "no transactions to cache" vs "transactions failed to cache"
4. **Transaction hashes vs objects**: Critical bug where `fullTransactions=true` might not be working, but no detection
5. **Conversion failures**: No detailed logging showing WHICH transaction failed and WHY

## Code Changes

### File: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs`

#### Change 1: Enhanced Request Logging (Lines 651-656)

**Added**:
```rust
tracing::debug!(
    block_number = block_number,
    upstream = upstream_name,
    request = ?block_req,
    "sending eth_getBlockByNumber with fullTransactions=true"
);
```

**Purpose**: Verify that the request is being sent with the correct parameters.

#### Change 2: Response Structure Logging (Lines 673-708)

**Added**: Comprehensive logging to identify the structure of the transactions field:
- Detects if field is missing entirely
- Detects if field is null
- Detects if field is array of hashes (BUG!)
- Detects if field is array of objects (CORRECT)
- Detects if field is empty array (no transactions in block)
- Counts transactions for correlation with caching results

**Key Detection**:
```rust
if first.is_string() {
    format!("array_of_hashes (count: {})", arr.len())
} else if first.is_object() {
    format!("array_of_objects (count: {})", arr.len())
}
```

This will immediately show if `eth_getBlockByNumber` is returning transaction hashes instead of full objects.

#### Change 3: Transaction Caching Logic Enhancement (Lines 765-847)

**Before** (Lines 765-795):
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
// NO ELSE CLAUSE - silent failure!
```

**After** (Lines 765-847):
```rust
match block_data.get("transactions") {
    None => {
        tracing::warn!(
            block_number = block_number,
            upstream = upstream_name,
            "block data has no 'transactions' field"
        );
    }
    Some(tx_value) => {
        match tx_value.as_array() {
            None => {
                tracing::warn!(
                    block_number = block_number,
                    upstream = upstream_name,
                    tx_value_type = ?tx_value,
                    "transactions field is not an array (possibly null or transaction hashes)"
                );
            }
            Some(transactions) => {
                if transactions.is_empty() {
                    tracing::debug!(
                        block_number = block_number,
                        upstream = upstream_name,
                        "block has no transactions (empty block)"
                    );
                } else {
                    // Critical check: are we getting hashes or objects?
                    if let Some(first_tx) = transactions.first() {
                        if first_tx.is_string() {
                            tracing::error!(
                                block_number = block_number,
                                upstream = upstream_name,
                                transaction_count = transactions.len(),
                                first_tx = ?first_tx,
                                "BUG: eth_getBlockByNumber returned transaction HASHES instead of full objects! \
                                 This means fullTransactions=true parameter is not working correctly."
                            );
                            return;
                        }
                    }

                    let mut cached_count = 0;
                    let mut failed_count = 0;

                    for tx_json in transactions {
                        if let Some(tx) = json_transaction_to_transaction_record(tx_json) {
                            cache_manager.transaction_cache.insert_transaction(tx).await;
                            cached_count += 1;
                        } else {
                            failed_count += 1;
                            // NEW: Log the actual transaction that failed
                            tracing::debug!(
                                block_number = block_number,
                                upstream = upstream_name,
                                tx_json = ?tx_json,
                                "failed to convert transaction to record"
                            );
                        }
                    }

                    if failed_count > 0 {
                        tracing::warn!(
                            block_number = block_number,
                            upstream = upstream_name,
                            total_count = transactions.len(),
                            cached_count = cached_count,
                            failed_count = failed_count,
                            "some transactions failed to convert (check for EIP-1559+ support or missing fields)"
                        );
                    } else if cached_count > 0 {
                        tracing::debug!(
                            block_number = block_number,
                            upstream = upstream_name,
                            transaction_count = cached_count,
                            "cached transactions"
                        );
                    }
                }
            }
        }
    }
}
```

## Diagnostic Output

With these changes, you will now see one of the following:

### Scenario 1: Transaction Hashes Instead of Objects (BUG)
```
INFO received block response with transactions field transactions_type="array_of_hashes (count: 42)"
ERROR BUG: eth_getBlockByNumber returned transaction HASHES instead of full objects!
```

### Scenario 2: Empty Blocks
```
INFO received block response with transactions field transactions_type="empty_array"
DEBUG block has no transactions (empty block)
```

### Scenario 3: Missing Transactions Field
```
WARN block response has no transactions field at all
WARN block data has no 'transactions' field
```

### Scenario 4: Null Transactions
```
INFO received block response with transactions field transactions_type="null"
WARN transactions field is not an array (possibly null or transaction hashes)
```

### Scenario 5: Successful Caching
```
INFO received block response with transactions field transactions_type="array_of_objects (count: 42)"
DEBUG cached transactions transaction_count=42
```

### Scenario 6: Conversion Failures
```
INFO received block response with transactions field transactions_type="array_of_objects (count: 42)"
DEBUG failed to convert transaction to record tx_json={"hash":"0x...","from":"0x..."}
WARN some transactions failed to convert cached_count=38 failed_count=4
```

## Next Steps

1. **Run the server** with these changes
2. **Check the logs** for the diagnostic messages
3. **Identify which scenario** is occurring
4. **Apply the appropriate fix**:
   - If Scenario 1 (transaction hashes): Fix the request parameter or upstream configuration
   - If Scenario 2 (empty blocks): This is normal, no fix needed
   - If Scenario 3/4 (missing/null field): Investigate upstream response format
   - If Scenario 6 (conversion failures): Check which fields are missing in the transaction JSON

## Verification Commands

```bash
# Run server and watch for transaction caching logs
cargo run --bin prism-server 2>&1 | grep -E "(transactions_type|cached transactions|failed to convert)"

# Check cache stats
curl -s http://localhost:3030/admin/cache/stats | jq '.transaction_cache'

# Trigger a new block to see the logs
# (wait for new block via WebSocket or make a transaction)
```

## Expected Resolution

After running with this patch, the logs will reveal exactly why transactions aren't being cached:
- **Most likely**: Transaction hashes being returned instead of full objects (Scenario 1)
- **Less likely**: All blocks are empty (Scenario 2) - but this doesn't match the "5000 receipts" observation
- **Unlikely**: Missing transaction field entirely (Scenario 3/4)

Once the root cause is identified, a targeted fix can be applied to the upstream request or configuration.

## Files Modified

- `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/websocket.rs`
  - Lines 651-656: Added request logging
  - Lines 673-708: Added response structure detection
  - Lines 765-847: Enhanced transaction caching with comprehensive error detection

## Compilation Status

✅ Code compiles successfully with no warnings
✅ All existing tests pass
✅ No breaking changes to API
