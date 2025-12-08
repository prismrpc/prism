use crate::{
    cache::types::{
        BlockBody, BlockHeader, LogFilter, LogId, LogRecord, ReceiptRecord, TransactionRecord,
    },
    utils::hex_buffer::{
        format_address, format_hash32, format_hex, format_hex_large, format_hex_u64, HexSerializer,
    },
};
use serde_json::Value;
use std::sync::Arc;

// --- Hex Utilities ---

/// Decodes hex string to bytes, stripping optional "0x" prefix.
/// Returns `None` for invalid hex or odd-length strings.
#[must_use]
pub fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    hex::decode(hex.strip_prefix("0x").unwrap_or(hex)).ok()
}

/// Decodes hex string to a fixed-size byte array (e.g., addresses, hashes).
/// Returns `None` if length doesn't match `N*2` or contains invalid hex.
#[must_use]
pub fn hex_to_array<const N: usize>(hex: &str) -> Option<[u8; N]> {
    let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
    if hex_str.len() != N * 2 {
        return None;
    }

    let mut array = [0u8; N];
    for (i, chunk) in hex_str.as_bytes().chunks(2).enumerate() {
        let high = hex_digit_to_u8(chunk[0])?;
        let low = hex_digit_to_u8(chunk[1])?;
        array[i] = (high << 4) | low;
    }

    Some(array)
}

fn hex_digit_to_u8(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Parses hex or decimal string to u64. Handles "0x" prefix.
/// For unprefixed strings, tries decimal first then hex fallback.
#[must_use]
pub fn hex_to_u64(s: &str) -> Option<u64> {
    if let Some(hex_str) = s.strip_prefix("0x") {
        u64::from_str_radix(hex_str, 16).ok()
    } else {
        s.parse::<u64>().ok().or_else(|| u64::from_str_radix(s, 16).ok())
    }
}

/// Parses hex or decimal string to u32. Handles "0x" prefix.
/// For unprefixed strings, tries decimal first then hex fallback.
#[must_use]
pub fn hex_to_u32(s: &str) -> Option<u32> {
    if let Some(hex_str) = s.strip_prefix("0x") {
        u32::from_str_radix(hex_str, 16).ok()
    } else {
        s.parse::<u32>().ok().or_else(|| u32::from_str_radix(s, 16).ok())
    }
}

/// Parses variable-length hex string to a 32-byte big-endian array (uint256).
/// Handles values like "0x0", "0xb59b9f7800", or full 32-byte hashes.
/// The result is left-padded with zeros (big-endian format).
#[must_use]
pub fn hex_to_u256(hex: &str) -> Option<[u8; 32]> {
    let hex_str = hex.strip_prefix("0x").unwrap_or(hex);

    // Handle empty or too long values
    if hex_str.is_empty() || hex_str.len() > 64 {
        return None;
    }

    // Decode hex bytes
    // Pad odd-length hex strings with leading zero
    let padded_hex = if hex_str.len() % 2 == 1 {
        format!("0{hex_str}")
    } else {
        hex_str.to_string()
    };

    let bytes = hex::decode(&padded_hex).ok()?;

    // Left-pad to 32 bytes (big-endian)
    let mut result = [0u8; 32];
    let start = 32 - bytes.len();
    result[start..].copy_from_slice(&bytes);

    Some(result)
}

// --- Log Conversions ---

/// Extracts log identifier from JSON-RPC log entry.
/// Returns `None` if required fields are missing or malformed.
#[must_use]
pub fn json_log_to_log_id(log: &Value) -> Option<LogId> {
    let block_number = hex_to_u64(log.get("blockNumber")?.as_str()?)?;
    let log_index = hex_to_u32(log.get("logIndex")?.as_str()?)?;
    Some(LogId::new(block_number, log_index))
}

/// Converts complete JSON-RPC log entry to `(LogId, LogRecord)`.
/// Topics are limited to 4 entries per Ethereum spec. Returns `None` if malformed.
#[must_use]
pub fn json_log_to_log_record(log: &Value) -> Option<(LogId, LogRecord)> {
    let block_number = hex_to_u64(log.get("blockNumber")?.as_str()?)?;
    let log_index = hex_to_u32(log.get("logIndex")?.as_str()?)?;
    let log_id = LogId::new(block_number, log_index);

    let address = hex_to_array::<20>(log.get("address")?.as_str()?)?;

    let topics = log.get("topics")?.as_array()?;
    let mut topic_array = [None; 4];
    for (i, topic) in topics.iter().take(4).enumerate() {
        if let Some(topic_str) = topic.as_str() {
            topic_array[i] = hex_to_array::<32>(topic_str);
        }
    }

    let data = hex_to_bytes(log.get("data")?.as_str()?)?;
    let transaction_hash = hex_to_array::<32>(log.get("transactionHash")?.as_str()?)?;
    let block_hash = hex_to_array::<32>(log.get("blockHash")?.as_str()?)?;
    let transaction_index = hex_to_u32(log.get("transactionIndex")?.as_str()?)?;
    let removed = log.get("removed").and_then(Value::as_bool).unwrap_or(false);

    Some((
        log_id,
        LogRecord::new(
            address,
            topic_array,
            data,
            transaction_hash,
            block_hash,
            transaction_index,
            removed,
        ),
    ))
}

/// Converts cached `LogRecord` back to JSON-RPC format.
/// All values are hex-encoded with "0x" prefix per Ethereum spec.
#[must_use]
pub fn log_record_to_json_log(log_id: &LogId, log_record: &LogRecord) -> Value {
    let mut topics = Vec::with_capacity(4);
    for topic in log_record.topics.iter().flatten() {
        topics.push(Value::String(format_hash32(topic)));
    }

    serde_json::json!({
        "address": format_address(&log_record.address),
        "topics": topics,
        "data": HexSerializer::new(&log_record.data),
        "blockNumber": format_hex_u64(log_id.block_number),
        "blockHash": format_hash32(&log_record.block_hash),
        "transactionHash": format_hash32(&log_record.transaction_hash),
        "transactionIndex": format_hex_u64(u64::from(log_record.transaction_index)),
        "logIndex": format_hex_u64(u64::from(log_id.log_index)),
        "removed": log_record.removed,
    })
}

// --- Block Conversions ---

/// Converts JSON-RPC block to internal `BlockHeader` format.
/// Large fields (`extra_data`, `logs_bloom`) are Arc-wrapped for efficient cloning.
/// Returns `None` if required fields are missing or malformed.
#[must_use]
pub fn json_block_to_block_header(block: &Value) -> Option<BlockHeader> {
    let hash = hex_to_array::<32>(block.get("hash")?.as_str()?)?;
    let number = hex_to_u64(block.get("number")?.as_str()?)?;
    let parent_hash = hex_to_array::<32>(block.get("parentHash")?.as_str()?)?;
    let timestamp = hex_to_u64(block.get("timestamp")?.as_str()?)?;
    let gas_limit = hex_to_u64(block.get("gasLimit")?.as_str()?)?;
    let gas_used = hex_to_u64(block.get("gasUsed")?.as_str()?)?;
    let miner = hex_to_array::<20>(block.get("miner")?.as_str()?)?;
    let extra_data = hex_to_bytes(block.get("extraData")?.as_str()?)?;
    let logs_bloom = hex_to_bytes(block.get("logsBloom")?.as_str()?)?;
    let transactions_root = hex_to_array::<32>(block.get("transactionsRoot")?.as_str()?)?;
    let state_root = hex_to_array::<32>(block.get("stateRoot")?.as_str()?)?;
    let receipts_root = hex_to_array::<32>(block.get("receiptsRoot")?.as_str()?)?;

    Some(BlockHeader {
        hash,
        number,
        parent_hash,
        timestamp,
        gas_limit,
        gas_used,
        miner,
        extra_data: Arc::new(extra_data),
        logs_bloom: Arc::new(logs_bloom),
        transactions_root,
        state_root,
        receipts_root,
    })
}

/// Extracts transaction hashes from JSON-RPC block to create `BlockBody`.
/// Handles both formats:
/// - Transaction hashes as strings (when `fullTransactions=false`)
/// - Full transaction objects (when `fullTransactions=true`) - extracts hash from each object
#[must_use]
pub fn json_block_to_block_body(block: &Value) -> Option<BlockBody> {
    let hash = hex_to_array::<32>(block.get("hash")?.as_str()?)?;
    let transactions = block.get("transactions")?.as_array()?;
    let tx_hashes: Vec<_> = transactions
        .iter()
        .filter_map(|tx| {
            // Handle both formats: hash string or full transaction object
            if let Some(hash_str) = tx.as_str() {
                // Transaction is a string (hash only) - fullTransactions=false
                hex_to_array::<32>(hash_str)
            } else if let Some(tx_obj) = tx.as_object() {
                // Transaction is an object (full tx) - fullTransactions=true
                tx_obj
                    .get("hash")
                    .and_then(|h| h.as_str())
                    .and_then(hex_to_array::<32>)
            } else {
                None
            }
        })
        .collect();
    Some(BlockBody { hash, transactions: tx_hashes })
}

/// Converts cached block header back to JSON-RPC format.
///
/// Only includes header fields, not transactions. Use `block_header_and_body_to_json`
/// for a complete block response.
#[must_use]
pub fn block_header_to_json_block(header: &BlockHeader) -> Value {
    serde_json::json!({
        "hash": format_hash32(&header.hash),
        "number": format_hex_u64(header.number),
        "parentHash": format_hash32(&header.parent_hash),
        "timestamp": format_hex_u64(header.timestamp),
        "gasLimit": format_hex_u64(header.gas_limit),
        "gasUsed": format_hex_u64(header.gas_used),
        "miner": format_address(&header.miner),
        "extraData": HexSerializer::new(&header.extra_data),
        "logsBloom": HexSerializer::new(&header.logs_bloom),
        "transactionsRoot": format_hash32(&header.transactions_root),
        "stateRoot": format_hash32(&header.state_root),
        "receiptsRoot": format_hash32(&header.receipts_root),
    })
}

/// Combines header and body into a complete JSON-RPC block response.
///
/// Includes all header fields, transaction hashes, and an empty uncles array
/// per Ethereum JSON-RPC specification.
#[must_use]
pub fn block_header_and_body_to_json(header: &BlockHeader, body: &BlockBody) -> Value {
    let mut block = serde_json::Map::new();
    block.insert("hash".to_string(), serde_json::Value::String(format_hash32(&header.hash)));
    block.insert("number".to_string(), serde_json::Value::String(format_hex_u64(header.number)));
    block.insert(
        "parentHash".to_string(),
        serde_json::Value::String(format_hash32(&header.parent_hash)),
    );
    block.insert(
        "timestamp".to_string(),
        serde_json::Value::String(format_hex_u64(header.timestamp)),
    );
    block.insert(
        "gasLimit".to_string(),
        serde_json::Value::String(format_hex_u64(header.gas_limit)),
    );
    block.insert("gasUsed".to_string(), serde_json::Value::String(format_hex_u64(header.gas_used)));
    block.insert("miner".to_string(), serde_json::Value::String(format_address(&header.miner)));
    block
        .insert("extraData".to_string(), serde_json::Value::String(format_hex(&header.extra_data)));
    block.insert(
        "logsBloom".to_string(),
        serde_json::Value::String(format_hex_large(&header.logs_bloom)),
    );
    block.insert(
        "transactionsRoot".to_string(),
        serde_json::Value::String(format_hash32(&header.transactions_root)),
    );
    block.insert(
        "stateRoot".to_string(),
        serde_json::Value::String(format_hash32(&header.state_root)),
    );
    block.insert(
        "receiptsRoot".to_string(),
        serde_json::Value::String(format_hash32(&header.receipts_root)),
    );

    let tx_hashes: Vec<Value> = body
        .transactions
        .iter()
        .map(|tx_hash| Value::String(format_hash32(tx_hash)))
        .collect();
    block.insert("transactions".to_string(), Value::Array(tx_hashes));

    block.insert("uncles".to_string(), Value::Array(vec![]));

    serde_json::Value::Object(block)
}

// --- Transaction Conversions ---

/// Converts JSON-RPC transaction to internal format.
/// Handles optional fields (null for pending transactions).
/// Supports all transaction types:
/// - Legacy (type 0x0): uses `gasPrice`
/// - EIP-2930 (type 0x1): uses `gasPrice` + `accessList`
/// - EIP-1559 (type 0x2): uses `maxFeePerGas` + `maxPriorityFeePerGas` (no `gasPrice`)
/// - EIP-4844 (type 0x3): uses `maxFeePerGas` + `maxPriorityFeePerGas` + `maxFeePerBlobGas`
pub fn json_transaction_to_transaction_record(tx: &Value) -> Option<TransactionRecord> {
    // Helper macro to log missing/invalid fields
    macro_rules! require_field {
        ($field:expr, $parser:expr) => {
            match $parser {
                Some(v) => v,
                None => {
                    tracing::warn!(
                        field = $field,
                        tx_hash = ?tx.get("hash").and_then(|v| v.as_str()),
                        raw_value = ?tx.get($field),
                        "transaction conversion failed: missing or invalid field"
                    );
                    return None;
                }
            }
        };
    }

    let hash = require_field!("hash", tx.get("hash").and_then(|v| v.as_str()).and_then(hex_to_array::<32>));
    let block_hash = tx.get("blockHash").and_then(|v| v.as_str()).and_then(hex_to_array::<32>);
    let block_number = tx.get("blockNumber").and_then(|v| v.as_str()).and_then(hex_to_u64);
    let transaction_index =
        tx.get("transactionIndex").and_then(|v| v.as_str()).and_then(hex_to_u32);
    let from = require_field!("from", tx.get("from").and_then(|v| v.as_str()).and_then(hex_to_array::<20>));
    let to = tx.get("to").and_then(|v| v.as_str()).and_then(hex_to_array::<20>);
    // Value is variable-length hex (e.g., "0x0", "0xb59b9f7800"), needs padding to 32 bytes
    let value = require_field!("value", tx.get("value").and_then(|v| v.as_str()).and_then(hex_to_u256));

    // EIP-2718 tx types (0-255): 0=Legacy, 1=EIP-2930, 2=EIP-1559, 3=EIP-4844
    #[allow(clippy::cast_possible_truncation)]
    let tx_type = tx.get("type").and_then(|v| v.as_str()).and_then(hex_to_u64).and_then(|v| {
        if v <= 255 {
            Some(v as u8)
        } else {
            tracing::warn!(tx_type = v, "Invalid transaction type exceeds u8 range");
            None
        }
    });

    // Parse gas pricing fields - all are variable-length hex values
    let gas_price = tx.get("gasPrice").and_then(|v| v.as_str()).and_then(hex_to_u256);
    let max_fee_per_gas =
        tx.get("maxFeePerGas").and_then(|v| v.as_str()).and_then(hex_to_u256);
    let max_priority_fee_per_gas = tx
        .get("maxPriorityFeePerGas")
        .and_then(|v| v.as_str())
        .and_then(hex_to_u256);
    let max_fee_per_blob_gas =
        tx.get("maxFeePerBlobGas").and_then(|v| v.as_str()).and_then(hex_to_u256);

    let gas_limit = require_field!("gas", tx.get("gas").and_then(|v| v.as_str()).and_then(hex_to_u64));
    let nonce = require_field!("nonce", tx.get("nonce").and_then(|v| v.as_str()).and_then(hex_to_u64));
    let data = require_field!("input", tx.get("input").and_then(|v| v.as_str()).and_then(hex_to_bytes));

    // v is u64: can be large for EIP-155 legacy transactions (chainId * 2 + 35/36)
    let v = require_field!("v", tx.get("v").and_then(|v| v.as_str()).and_then(hex_to_u64));

    // r and s are ECDSA signature components - can be <32 bytes if they have leading zeros
    let r = require_field!("r", tx.get("r").and_then(|v| v.as_str()).and_then(hex_to_u256));
    let s = require_field!("s", tx.get("s").and_then(|v| v.as_str()).and_then(hex_to_u256));

    Some(TransactionRecord {
        hash,
        block_hash,
        block_number,
        transaction_index,
        from,
        to,
        value,
        tx_type,
        gas_price,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        max_fee_per_blob_gas,
        gas_limit,
        nonce,
        data,
        v,
        r,
        s,
    })
}

/// Converts cached transaction back to JSON-RPC format.
///
/// Uses optimized hex formatting (`format_hex_large`) for large input data (>256 bytes).
/// Properly handles all transaction types by serializing the appropriate gas pricing fields.
#[must_use]
pub fn transaction_record_to_json(transaction: &TransactionRecord) -> Value {
    let mut tx = serde_json::Map::new();

    tx.insert("hash".to_string(), Value::String(format_hash32(&transaction.hash)));

    if let Some(block_hash) = transaction.block_hash {
        tx.insert("blockHash".to_string(), Value::String(format_hash32(&block_hash)));
    } else {
        tx.insert("blockHash".to_string(), Value::Null);
    }

    if let Some(block_number) = transaction.block_number {
        tx.insert("blockNumber".to_string(), Value::String(format_hex_u64(block_number)));
    } else {
        tx.insert("blockNumber".to_string(), Value::Null);
    }

    if let Some(tx_index) = transaction.transaction_index {
        tx.insert(
            "transactionIndex".to_string(),
            Value::String(format_hex_u64(u64::from(tx_index))),
        );
    } else {
        tx.insert("transactionIndex".to_string(), Value::Null);
    }

    tx.insert("from".to_string(), Value::String(format_address(&transaction.from)));

    if let Some(to_address) = transaction.to {
        tx.insert("to".to_string(), Value::String(format_address(&to_address)));
    } else {
        tx.insert("to".to_string(), Value::Null);
    }

    tx.insert("value".to_string(), Value::String(format_hash32(&transaction.value)));

    // Include transaction type if present
    if let Some(tx_type) = transaction.tx_type {
        tx.insert("type".to_string(), Value::String(format_hex_u64(u64::from(tx_type))));
    }

    // Serialize gas pricing fields based on what's available
    if let Some(gas_price) = transaction.gas_price {
        tx.insert("gasPrice".to_string(), Value::String(format_hash32(&gas_price)));
    }
    if let Some(max_fee_per_gas) = transaction.max_fee_per_gas {
        tx.insert("maxFeePerGas".to_string(), Value::String(format_hash32(&max_fee_per_gas)));
    }
    if let Some(max_priority_fee_per_gas) = transaction.max_priority_fee_per_gas {
        tx.insert(
            "maxPriorityFeePerGas".to_string(),
            Value::String(format_hash32(&max_priority_fee_per_gas)),
        );
    }
    if let Some(max_fee_per_blob_gas) = transaction.max_fee_per_blob_gas {
        tx.insert(
            "maxFeePerBlobGas".to_string(),
            Value::String(format_hash32(&max_fee_per_blob_gas)),
        );
    }

    tx.insert("gas".to_string(), Value::String(format_hex_u64(transaction.gas_limit)));
    tx.insert("nonce".to_string(), Value::String(format_hex_u64(transaction.nonce)));

    tx.insert(
        "input".to_string(),
        Value::String(if transaction.data.len() > 256 {
            format_hex_large(&transaction.data)
        } else {
            format_hex(&transaction.data)
        }),
    );

    tx.insert("v".to_string(), Value::String(format_hex_u64(transaction.v)));
    tx.insert("r".to_string(), Value::String(format_hash32(&transaction.r)));
    tx.insert("s".to_string(), Value::String(format_hash32(&transaction.s)));

    Value::Object(tx)
}

// --- Receipt Conversions ---

/// Converts JSON-RPC receipt to internal format.
/// Stores log ID references (not full log data) since logs are cached separately.
pub fn json_receipt_to_receipt_record(receipt: &Value) -> Option<ReceiptRecord> {
    let transaction_hash = hex_to_array::<32>(receipt.get("transactionHash")?.as_str()?)?;
    let block_hash = hex_to_array::<32>(receipt.get("blockHash")?.as_str()?)?;
    let block_number = hex_to_u64(receipt.get("blockNumber")?.as_str()?)?;
    let transaction_index = hex_to_u32(receipt.get("transactionIndex")?.as_str()?)?;
    let from = hex_to_array::<20>(receipt.get("from")?.as_str()?)?;
    let to = receipt.get("to").and_then(|v| v.as_str()).and_then(hex_to_array::<20>);
    let cumulative_gas_used = hex_to_u64(receipt.get("cumulativeGasUsed")?.as_str()?)?;
    let gas_used = hex_to_u64(receipt.get("gasUsed")?.as_str()?)?;
    let contract_address = receipt
        .get("contractAddress")
        .and_then(|v| v.as_str())
        .and_then(hex_to_array::<20>);

    let log_ids: Vec<_> =
        receipt.get("logs")?.as_array()?.iter().filter_map(json_log_to_log_id).collect();

    let status = hex_to_u64(receipt.get("status")?.as_str()?)?;
    let logs_bloom = hex_to_bytes(receipt.get("logsBloom")?.as_str()?)?;

    // Optional EIP-1559/4844 fields
    let effective_gas_price =
        receipt.get("effectiveGasPrice").and_then(|v| v.as_str()).and_then(hex_to_u64);
    let blob_gas_price = receipt.get("blobGasPrice").and_then(|v| v.as_str()).and_then(hex_to_u64);

    // EIP-2718 tx types (0-255): 0=Legacy, 1=EIP-2930, 2=EIP-1559, 3=EIP-4844
    #[allow(clippy::cast_possible_truncation)]
    let tx_type = receipt.get("type").and_then(|v| v.as_str()).and_then(hex_to_u64).and_then(|v| {
        if v <= 255 {
            Some(v as u8)
        } else {
            tracing::warn!(tx_type = v, "Invalid transaction type exceeds u8 range");
            None
        }
    });

    Some(ReceiptRecord {
        transaction_hash,
        block_hash,
        block_number,
        transaction_index,
        from,
        to,
        cumulative_gas_used,
        gas_used,
        contract_address,
        logs: log_ids,
        status,
        logs_bloom,
        effective_gas_price,
        tx_type,
        blob_gas_price,
    })
}

/// Converts cached receipt back to JSON-RPC format.
///
/// Requires a separate `logs` parameter because `ReceiptRecord` only stores `LogId`
/// references. Caller must resolve log IDs to actual log data via `get_receipt_with_logs`.
#[must_use]
pub fn receipt_record_to_json(receipt: &ReceiptRecord, logs: &[Arc<LogRecord>]) -> Value {
    let mut receipt_json = serde_json::Map::new();
    receipt_json.insert(
        "transactionHash".to_string(),
        serde_json::Value::String(format_hash32(&receipt.transaction_hash)),
    );
    receipt_json.insert(
        "blockHash".to_string(),
        serde_json::Value::String(format_hash32(&receipt.block_hash)),
    );
    receipt_json.insert(
        "blockNumber".to_string(),
        serde_json::Value::String(format_hex_u64(receipt.block_number)),
    );
    receipt_json.insert(
        "transactionIndex".to_string(),
        serde_json::Value::String(format_hex_u64(u64::from(receipt.transaction_index))),
    );
    receipt_json
        .insert("from".to_string(), serde_json::Value::String(format_address(&receipt.from)));
    receipt_json.insert(
        "to".to_string(),
        receipt.to.map_or(serde_json::Value::Null, |addr| {
            serde_json::Value::String(format_address(&addr))
        }),
    );
    receipt_json.insert(
        "cumulativeGasUsed".to_string(),
        serde_json::Value::String(format_hex_u64(receipt.cumulative_gas_used)),
    );
    receipt_json
        .insert("gasUsed".to_string(), serde_json::Value::String(format_hex_u64(receipt.gas_used)));
    receipt_json.insert(
        "contractAddress".to_string(),
        receipt.contract_address.map_or(serde_json::Value::Null, |addr| {
            serde_json::Value::String(format_address(&addr))
        }),
    );
    receipt_json
        .insert("status".to_string(), serde_json::Value::String(format_hex_u64(receipt.status)));
    receipt_json.insert(
        "logsBloom".to_string(),
        serde_json::Value::String(format_hex_large(&receipt.logs_bloom)),
    );

    // Use the actual LogId from receipt.logs (contains correct log indices)
    // rather than enumerate index which would be incorrect for receipts
    // with non-zero starting log indices in a block
    let json_logs: Vec<Value> = receipt
        .logs
        .iter()
        .zip(logs.iter())
        .map(|(log_id, log_record)| log_record_to_json_log(log_id, log_record))
        .collect();
    receipt_json.insert("logs".to_string(), serde_json::Value::Array(json_logs));

    if let Some(effective_gas_price) = receipt.effective_gas_price {
        receipt_json.insert(
            "effectiveGasPrice".to_string(),
            serde_json::Value::String(format_hex_u64(effective_gas_price)),
        );
    }
    if let Some(tx_type) = receipt.tx_type {
        receipt_json.insert(
            "type".to_string(),
            serde_json::Value::String(format_hex_u64(u64::from(tx_type))),
        );
    }
    if let Some(blob_gas_price) = receipt.blob_gas_price {
        receipt_json.insert(
            "blobGasPrice".to_string(),
            serde_json::Value::String(format_hex_u64(blob_gas_price)),
        );
    }

    serde_json::Value::Object(receipt_json)
}

// --- Filter Conversions ---

/// Parses `eth_getLogs` filter parameters from JSON-RPC.
/// Supports address filtering, positional topic filters, and a custom topicsAnywhere
/// extension for matching topics in any position.
#[must_use]
pub fn json_params_to_log_filter(params: &Value) -> Option<LogFilter> {
    let filter = params.as_array()?.first()?;

    let from_block = hex_to_u64(filter.get("fromBlock")?.as_str()?)?;
    let to_block = hex_to_u64(filter.get("toBlock")?.as_str()?)?;

    let mut log_filter = LogFilter::new(from_block, to_block);

    if let Some(address) = filter.get("address").and_then(|v| v.as_str()) {
        if let Some(addr_bytes) = hex_to_array::<20>(address) {
            log_filter = log_filter.with_address(addr_bytes);
        }
    }

    if let Some(topics) = filter.get("topics").and_then(|v| v.as_array()) {
        for (i, topic) in topics.iter().take(4).enumerate() {
            if let Some(topic_str) = topic.as_str() {
                if let Some(topic_bytes) = hex_to_array::<32>(topic_str) {
                    log_filter = log_filter.with_topic(i, topic_bytes);
                }
            }
        }
    }

    if let Some(topics_anywhere) = filter.get("topicsAnywhere").and_then(|v| v.as_array()) {
        for topic in topics_anywhere {
            if let Some(topic_str) = topic.as_str() {
                if let Some(topic_bytes) = hex_to_array::<32>(topic_str) {
                    log_filter = log_filter.with_topic_anywhere(topic_bytes);
                }
            }
        }
    }

    Some(log_filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::hex_buffer::{get_hex_buffer_stats, reset_buffer_stats};

    #[test]
    fn test_hex_conversion() {
        assert_eq!(hex_to_bytes("0x123456").expect("hex_to_bytes failed"), vec![0x12, 0x34, 0x56]);
        assert_eq!(hex_to_bytes("123456").expect("hex_to_bytes failed"), vec![0x12, 0x34, 0x56]);
        assert_eq!(hex_to_array::<3>("0x123456").expect("hex_to_array failed"), [0x12, 0x34, 0x56]);
    }

    #[test]
    fn test_hex_to_u256() {
        // Zero value
        let zero = hex_to_u256("0x0").unwrap();
        assert_eq!(zero, [0u8; 32]);

        // Small value (5 bytes) - should be left-padded
        let small = hex_to_u256("0xb59b9f7800").unwrap();
        let mut expected = [0u8; 32];
        expected[27..32].copy_from_slice(&[0xb5, 0x9b, 0x9f, 0x78, 0x00]);
        assert_eq!(small, expected);

        // Odd-length hex (should pad with leading zero)
        let odd = hex_to_u256("0x123").unwrap();
        let mut expected_odd = [0u8; 32];
        expected_odd[30..32].copy_from_slice(&[0x01, 0x23]);
        assert_eq!(odd, expected_odd);

        // Full 32-byte value
        let full = hex_to_u256("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap();
        let mut expected_full = [0u8; 32];
        expected_full[31] = 1;
        assert_eq!(full, expected_full);

        // Too long should fail
        assert!(hex_to_u256("0x00000000000000000000000000000000000000000000000000000000000000001").is_none());

        // Empty should fail
        assert!(hex_to_u256("0x").is_none());
    }

    #[test]
    fn test_hex_to_u256_leading_zeros() {
        // Test that values with leading zeros stripped are correctly padded
        // These are real examples from geth where leading zero bytes are stripped

        // r value with 63 hex chars (missing one leading zero nibble)
        // "0xd75..." should become "0x0d75..." when padded
        let r_stripped = "0xd7536a2cecb99c6b785769e345b6be4e112a336209e15acd8ff4643d7521cd4";
        assert_eq!(r_stripped.len(), 65); // 0x + 63 chars
        let r_padded = hex_to_u256(r_stripped).unwrap();
        // First byte should be 0x0d (the 'd' with leading zero)
        assert_eq!(r_padded[0], 0x0d, "First byte should have leading zero restored");
        assert_eq!(r_padded[1], 0x75, "Second byte should be 0x75");

        // s value with 62 hex chars (missing two leading zero nibbles = one full byte)
        let s_very_stripped = "0xeefaab9fb3d5330cddd143653ed4a2cce73c4367d7f9ed879f9682e579b2e8";
        assert_eq!(s_very_stripped.len(), 64); // 0x + 62 chars
        let s_padded = hex_to_u256(s_very_stripped).unwrap();
        // First byte should be 0x00 (full byte of leading zeros)
        assert_eq!(s_padded[0], 0x00, "First byte should be zero");
        assert_eq!(s_padded[1], 0xee, "Second byte should be 0xee");

        // Minimal value: "0x1" -> should be all zeros except last byte
        let minimal = hex_to_u256("0x1").unwrap();
        for i in 0..31 {
            assert_eq!(minimal[i], 0x00, "Byte {i} should be zero for minimal value");
        }
        assert_eq!(minimal[31], 0x01, "Last byte should be 0x01");

        // Verify numeric equivalence: padded and unpadded represent same value
        let full_r = "0x0d7536a2cecb99c6b785769e345b6be4e112a336209e15acd8ff4643d7521cd4";
        let full_r_parsed = hex_to_u256(full_r).unwrap();
        assert_eq!(r_padded, full_r_parsed, "Stripped and full r should be equal");
    }

    #[test]
    fn test_transaction_roundtrip_legacy() {
        // Legacy transaction with compact hex values (as returned by geth)
        let original_json = serde_json::json!({
            "hash": "0xe36d216b68600a17795301c19c499641f61ac0c088121bb4c7d5b9ff2ed76021",
            "blockHash": "0xabababababababababababababababababababababababababababababababab",
            "blockNumber": "0x1234",
            "transactionIndex": "0x5",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0xb59b9f7800",  // Compact: 5 bytes, not 32
            "gas": "0x5208",
            "gasPrice": "0x3b9aca00",  // Compact gas price
            "nonce": "0x42",
            "input": "0x",
            "type": "0x0",
            "v": "0x1c",  // Legacy v value (28)
            "r": "0xd7536a2cecb99c6b785769e345b6be4e112a336209e15acd8ff4643d7521cd4",  // 63 hex chars!
            "s": "0x1d2e640e84f58097604a89a3def961d10e6da51e1acda42a8ebb104512ca13a"   // 63 hex chars!
        });

        // Parse to TransactionRecord
        let record = json_transaction_to_transaction_record(&original_json)
            .expect("Failed to parse transaction");

        // Convert back to JSON
        let result_json = transaction_record_to_json(&record);

        // Verify critical fields match (values should be equivalent, format may differ)
        assert_eq!(
            result_json.get("hash").unwrap().as_str().unwrap().to_lowercase(),
            original_json.get("hash").unwrap().as_str().unwrap().to_lowercase()
        );
        assert_eq!(
            result_json.get("from").unwrap().as_str().unwrap().to_lowercase(),
            original_json.get("from").unwrap().as_str().unwrap().to_lowercase()
        );
        assert_eq!(
            result_json.get("to").unwrap().as_str().unwrap().to_lowercase(),
            original_json.get("to").unwrap().as_str().unwrap().to_lowercase()
        );

        // Value: original is compact, result may be padded - verify numeric equivalence
        let orig_value = hex_to_u256(original_json.get("value").unwrap().as_str().unwrap()).unwrap();
        let result_value = hex_to_u256(result_json.get("value").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_value, result_value, "value mismatch");

        // Gas price
        let orig_gas_price = hex_to_u256(original_json.get("gasPrice").unwrap().as_str().unwrap()).unwrap();
        let result_gas_price = hex_to_u256(result_json.get("gasPrice").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_gas_price, result_gas_price, "gasPrice mismatch");

        // r and s (signature components with stripped leading zeros)
        let orig_r = hex_to_u256(original_json.get("r").unwrap().as_str().unwrap()).unwrap();
        let result_r = hex_to_u256(result_json.get("r").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_r, result_r, "r signature mismatch");

        let orig_s = hex_to_u256(original_json.get("s").unwrap().as_str().unwrap()).unwrap();
        let result_s = hex_to_u256(result_json.get("s").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_s, result_s, "s signature mismatch");

        // v value
        let orig_v = hex_to_u64(original_json.get("v").unwrap().as_str().unwrap()).unwrap();
        let result_v = hex_to_u64(result_json.get("v").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_v, result_v, "v mismatch");
    }

    #[test]
    fn test_transaction_roundtrip_eip1559() {
        // EIP-1559 transaction (type 2) with compact hex values
        let original_json = serde_json::json!({
            "hash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "blockHash": "0xabababababababababababababababababababababababababababababababab",
            "blockNumber": "0xabc",
            "transactionIndex": "0x0",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0",  // Zero value transfer
            "gas": "0x5208",
            "maxFeePerGas": "0x59682f00",
            "maxPriorityFeePerGas": "0x3b9aca00",
            "nonce": "0x1",
            "input": "0x1234",
            "type": "0x2",
            "v": "0x1",  // EIP-1559: just parity (0 or 1)
            "r": "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "s": "0x1"  // Very small s value (1 byte after stripping)
        });

        let record = json_transaction_to_transaction_record(&original_json)
            .expect("Failed to parse EIP-1559 transaction");

        let result_json = transaction_record_to_json(&record);

        // Verify type
        assert_eq!(
            hex_to_u64(result_json.get("type").unwrap().as_str().unwrap()).unwrap(),
            2,
            "type should be 2 for EIP-1559"
        );

        // Verify EIP-1559 gas fields
        let orig_max_fee = hex_to_u256(original_json.get("maxFeePerGas").unwrap().as_str().unwrap()).unwrap();
        let result_max_fee = hex_to_u256(result_json.get("maxFeePerGas").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_max_fee, result_max_fee, "maxFeePerGas mismatch");

        let orig_priority = hex_to_u256(original_json.get("maxPriorityFeePerGas").unwrap().as_str().unwrap()).unwrap();
        let result_priority = hex_to_u256(result_json.get("maxPriorityFeePerGas").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_priority, result_priority, "maxPriorityFeePerGas mismatch");

        // Verify s with minimal value
        let orig_s = hex_to_u256(original_json.get("s").unwrap().as_str().unwrap()).unwrap();
        let result_s = hex_to_u256(result_json.get("s").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_s, result_s, "s signature mismatch for minimal value");

        // EIP-1559 should NOT have gasPrice in output
        assert!(result_json.get("gasPrice").is_none(), "EIP-1559 should not have gasPrice");
    }

    #[test]
    fn test_transaction_roundtrip_eip155_high_chainid() {
        // EIP-155 legacy transaction on a chain with high chainId (like devnet 1337)
        // v = chainId * 2 + 35 = 1337 * 2 + 35 = 2709 = 0xa95
        let original_json = serde_json::json!({
            "hash": "0x5555555555555555555555555555555555555555555555555555555555555555",
            "blockHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockNumber": "0x100",
            "transactionIndex": "0x0",
            "from": "0x1111111111111111111111111111111111111111",
            "to": "0x2222222222222222222222222222222222222222",
            "value": "0xde0b6b3a7640000",  // 1 ETH in wei
            "gas": "0x5208",
            "gasPrice": "0x4a817c800",
            "nonce": "0x0",
            "input": "0x",
            "type": "0x0",
            "v": "0xa95",  // 2709 decimal - doesn't fit in u8!
            "r": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "s": "0xabcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        });

        let record = json_transaction_to_transaction_record(&original_json)
            .expect("Failed to parse high chainId transaction");

        // Verify v is stored correctly as u64
        assert_eq!(record.v, 2709, "v should be 2709 for chainId 1337");

        let result_json = transaction_record_to_json(&record);

        let result_v = hex_to_u64(result_json.get("v").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(result_v, 2709, "v roundtrip failed for high chainId");
    }

    #[test]
    fn test_transaction_roundtrip_contract_creation() {
        // Contract creation transaction (to is null)
        let original_json = serde_json::json!({
            "hash": "0x9999999999999999999999999999999999999999999999999999999999999999",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x50",
            "transactionIndex": "0x3",
            "from": "0x3333333333333333333333333333333333333333",
            "to": null,  // Contract creation!
            "value": "0x0",
            "gas": "0x100000",
            "gasPrice": "0x3b9aca00",
            "nonce": "0x5",
            "input": "0x608060405234801561001057600080fd5b50",  // Contract bytecode
            "type": "0x0",
            "v": "0x1b",
            "r": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "s": "0x1234567890123456789012345678901234567890123456789012345678901234"
        });

        let record = json_transaction_to_transaction_record(&original_json)
            .expect("Failed to parse contract creation transaction");

        assert!(record.to.is_none(), "to should be None for contract creation");

        let result_json = transaction_record_to_json(&record);

        assert!(
            result_json.get("to").unwrap().is_null(),
            "to should be null in JSON for contract creation"
        );

        // Verify input data preserved
        let orig_input = hex_to_bytes(original_json.get("input").unwrap().as_str().unwrap()).unwrap();
        let result_input = hex_to_bytes(result_json.get("input").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(orig_input, result_input, "input bytecode mismatch");
    }

    #[test]
    fn test_log_conversion() {
        let json_log = serde_json::json!({
            "blockNumber": "0x3e8",
            "logIndex": "0x0",
            "address": "0x1234567890123456789012345678901234567890",
            "topics": [
                "0x1234567890123456789012345678901234567890123456789012345678901234"
            ],
            "data": "0x123456",
            "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "blockHash": "0xabababababababababababababababababababababababababababababababab",
            "transactionIndex": "0x5",
            "removed": false
        });

        let (log_id, log_record) = json_log_to_log_record(&json_log).unwrap();
        assert_eq!(log_id.block_number, 1000);
        assert_eq!(log_id.log_index, 0);
        assert_eq!(
            log_record.address,
            [
                0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x78,
                0x90, 0x12, 0x34, 0x56, 0x78, 0x90
            ]
        );
        assert_eq!(log_record.block_hash, [0xab; 32]);
        assert_eq!(log_record.transaction_index, 5);
    }

    #[test]
    fn test_hex_buffer_usage_in_formatting() {
        reset_buffer_stats();

        let (initial_capacity, _) = get_hex_buffer_stats();

        let test_hash = [0xAB; 32];
        let formatted1 = format_hash32(&test_hash);
        let formatted2 = format_hash32(&test_hash);

        assert_eq!(
            formatted1,
            "0xabababababababababababababababababababababababababababababababab"
        );
        assert_eq!(
            formatted2,
            "0xabababababababababababababababababababababababababababababababab"
        );

        let (final_capacity, _) = get_hex_buffer_stats();
        assert_eq!(initial_capacity, final_capacity, "Buffer capacity should be reused");
    }

    #[test]
    fn test_hex_buffer_usage_in_log_conversion() {
        reset_buffer_stats();
        let (initial_capacity, _) = get_hex_buffer_stats();

        let json_log = serde_json::json!({
            "blockNumber": "0x3e8",
            "logIndex": "0x0",
            "address": "0x1234567890123456789012345678901234567890",
            "topics": [
                "0x1234567890123456789012345678901234567890123456789012345678901234"
            ],
            "data": "0x123456",
            "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "blockHash": "0xabababababababababababababababababababababababababababababababab",
            "transactionIndex": "0x5",
            "removed": false
        });

        let (log_id, log_record) = json_log_to_log_record(&json_log).unwrap();
        let formatted_log = log_record_to_json_log(&log_id, &log_record);

        assert_eq!(formatted_log.get("blockNumber").unwrap().as_str().unwrap(), "0x3e8");
        assert_eq!(
            formatted_log.get("address").unwrap().as_str().unwrap(),
            "0x1234567890123456789012345678901234567890"
        );
        assert_eq!(
            formatted_log.get("transactionHash").unwrap().as_str().unwrap(),
            "0x1234567890123456789012345678901234567890123456789012345678901234"
        );

        let (final_capacity, _) = get_hex_buffer_stats();
        assert_eq!(initial_capacity, final_capacity, "Buffer should be reused");
    }

    #[test]
    fn test_hex_buffer_usage_in_block_conversion() {
        reset_buffer_stats();

        let (initial_capacity, _) = get_hex_buffer_stats();

        let header = BlockHeader {
            hash: [0xAA; 32],
            number: 12345,
            parent_hash: [0xBB; 32],
            timestamp: 1_640_995_200,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [0xCC; 20],
            extra_data: Arc::new(vec![0xDD; 32]),
            logs_bloom: Arc::new(vec![0xEE; 256]),
            transactions_root: [0xFF; 32],
            state_root: [0x11; 32],
            receipts_root: [0x22; 32],
        };

        let formatted = block_header_to_json_block(&header);

        let number = formatted.get("number").unwrap().as_str().unwrap();
        let hash = formatted.get("hash").unwrap().as_str().unwrap();
        let miner = formatted.get("miner").unwrap().as_str().unwrap();

        assert_eq!(number, "0x3039");
        assert_eq!(hash, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(miner, "0xcccccccccccccccccccccccccccccccccccccccc");

        let (final_capacity, _) = get_hex_buffer_stats();
        assert_eq!(initial_capacity, final_capacity, "Buffer should be reused");
    }

    #[test]
    fn test_hex_buffer_usage_in_transaction_conversion() {
        let transaction = TransactionRecord {
            hash: [0xAA; 32],
            block_hash: Some([0xBB; 32]),
            block_number: Some(12345),
            transaction_index: Some(5),
            from: [0xCC; 20],
            to: Some([0xDD; 20]),
            value: [0xEE; 32],
            tx_type: Some(0),
            gas_price: Some([0xFF; 32]),
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            max_fee_per_blob_gas: None,
            gas_limit: 21000,
            nonce: 42,
            data: vec![0x11; 100],
            v: 27,
            r: [0x22; 32],
            s: [0x33; 32],
        };

        let formatted = transaction_record_to_json(&transaction);

        let block_number = formatted.get("blockNumber").unwrap().as_str().unwrap();
        let gas = formatted.get("gas").unwrap().as_str().unwrap();
        let input = formatted.get("input").unwrap().as_str().unwrap();

        assert_eq!(block_number, "0x3039");
        assert_eq!(gas, "0x5208");
        assert!(input.starts_with("0x"));
    }

    #[test]
    fn test_hex_buffer_usage_in_receipt_conversion() {
        let receipt = ReceiptRecord {
            transaction_hash: [0xAA; 32],
            block_hash: [0xBB; 32],
            block_number: 12345,
            transaction_index: 5,
            from: [0xCC; 20],
            to: Some([0xDD; 20]),
            cumulative_gas_used: 1_500_000,
            gas_used: 21_000,
            contract_address: Some([0xEE; 20]),
            logs: vec![],
            status: 1,
            logs_bloom: vec![0xFF; 256],
            effective_gas_price: Some(1_000_000_000),
            tx_type: Some(2),
            blob_gas_price: None,
        };

        let logs = vec![];

        let formatted = receipt_record_to_json(&receipt, &logs);

        let block_number = formatted.get("blockNumber").unwrap().as_str().unwrap();
        let gas_used = formatted.get("gasUsed").unwrap().as_str().unwrap();
        let status = formatted.get("status").unwrap().as_str().unwrap();

        assert_eq!(block_number, "0x3039");
        assert_eq!(gas_used, "0x5208");
        assert_eq!(status, "0x1");
    }

    #[test]
    fn test_hex_buffer_parsing_functions() {
        assert_eq!(hex_to_u64("0x3e8").unwrap(), 1000);
        assert_eq!(hex_to_u64("3e8").unwrap(), 1000);
        assert_eq!(hex_to_u32("0x0").unwrap(), 0);
        assert_eq!(hex_to_u32("ff").unwrap(), 255);

        let address = hex_to_array::<20>("0x1234567890123456789012345678901234567890").unwrap();
        assert_eq!(address[0], 0x12);
        assert_eq!(address[19], 0x90);

        let bytes = hex_to_bytes("0x123456").unwrap();
        assert_eq!(bytes, vec![0x12, 0x34, 0x56]);
    }

    #[test]
    fn test_hex_buffer_roundtrip() {
        let original_hash = [0xAB; 32];
        let formatted = format_hash32(&original_hash);
        let parsed = hex_to_array::<32>(&formatted).unwrap();
        assert_eq!(original_hash, parsed);

        let original_address = [0xCD; 20];
        let formatted = format_address(&original_address);
        let parsed = hex_to_array::<20>(&formatted).unwrap();
        assert_eq!(original_address, parsed);

        let original_number = 12345u64;
        let formatted = format_hex_u64(original_number);
        let parsed = hex_to_u64(&formatted).unwrap();
        assert_eq!(original_number, parsed);
    }

    #[test]
    fn test_hex_to_array_wrong_length() {
        // Too short (line 31-32 early return)
        assert!(hex_to_array::<32>("0x1234").is_none());
        assert!(hex_to_array::<20>("0x12").is_none());

        // Too long
        assert!(hex_to_array::<3>("0x12345678").is_none());
    }

    #[test]
    fn test_hex_to_array_invalid_chars() {
        // Invalid hex characters (line 54: _ => None)
        assert!(hex_to_array::<3>("0xGGGGGG").is_none());
        assert!(hex_to_array::<3>("0x12ZZZZ").is_none());
        assert!(hex_to_array::<3>("0x!@#$%^").is_none());

        // Space characters
        assert!(hex_to_array::<3>("0x12 456").is_none());
    }

    #[test]
    fn test_hex_to_array_uppercase() {
        // Test uppercase hex digits (line 53: 'A'..='F')
        let result = hex_to_array::<3>("0xABCDEF");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), [0xAB, 0xCD, 0xEF]);

        // Mixed case
        let result = hex_to_array::<3>("0xAbCdEf");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), [0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn test_hex_digit_to_u8_all_cases() {
        // Test all valid hex digits through hex_to_array
        // "0123456789abcdefABCDEF0000" = 26 hex chars = 13 bytes
        let all_hex = "0x0123456789abcdefABCDEF0000";
        let result = hex_to_array::<13>(all_hex);
        assert!(result.is_some());
        let bytes = result.unwrap();
        assert_eq!(bytes[0], 0x01);
        assert_eq!(bytes[1], 0x23);
        assert_eq!(bytes[2], 0x45);
        assert_eq!(bytes[3], 0x67);
        assert_eq!(bytes[4], 0x89);
        assert_eq!(bytes[5], 0xAB);
        assert_eq!(bytes[6], 0xCD);
        assert_eq!(bytes[7], 0xEF);
        assert_eq!(bytes[8], 0xAB); // uppercase
        assert_eq!(bytes[9], 0xCD);
        assert_eq!(bytes[10], 0xEF);
        assert_eq!(bytes[11], 0x00);
        assert_eq!(bytes[12], 0x00);
    }

    #[test]
    fn test_json_log_to_log_id_valid() {
        let log = serde_json::json!({
            "blockNumber": "0x3e8",
            "logIndex": "0x5"
        });
        let log_id = json_log_to_log_id(&log);
        assert!(log_id.is_some());
        let log_id = log_id.unwrap();
        assert_eq!(log_id.block_number, 1000);
        assert_eq!(log_id.log_index, 5);
    }

    #[test]
    fn test_json_log_to_log_id_missing_block_number() {
        let log = serde_json::json!({
            "logIndex": "0x5"
        });
        assert!(json_log_to_log_id(&log).is_none());
    }

    #[test]
    fn test_json_log_to_log_id_missing_log_index() {
        let log = serde_json::json!({
            "blockNumber": "0x3e8"
        });
        assert!(json_log_to_log_id(&log).is_none());
    }

    #[test]
    fn test_json_log_to_log_id_invalid_block_number() {
        let log = serde_json::json!({
            "blockNumber": "invalid",
            "logIndex": "0x5"
        });
        assert!(json_log_to_log_id(&log).is_none());
    }

    #[test]
    fn test_json_log_to_log_id_invalid_log_index() {
        let log = serde_json::json!({
            "blockNumber": "0x3e8",
            "logIndex": "invalid"
        });
        assert!(json_log_to_log_id(&log).is_none());
    }

    #[test]
    fn test_json_log_to_log_id_null_fields() {
        let log = serde_json::json!({
            "blockNumber": null,
            "logIndex": "0x5"
        });
        assert!(json_log_to_log_id(&log).is_none());
    }

    #[test]
    fn test_json_block_to_block_body_with_tx_hashes() {
        let block = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "transactions": [
                "0x1111111111111111111111111111111111111111111111111111111111111111",
                "0x2222222222222222222222222222222222222222222222222222222222222222"
            ]
        });
        let body = json_block_to_block_body(&block);
        assert!(body.is_some());
        let body = body.unwrap();
        assert_eq!(body.hash, [0xAA; 32]);
        assert_eq!(body.transactions.len(), 2);
        assert_eq!(body.transactions[0], [0x11; 32]);
        assert_eq!(body.transactions[1], [0x22; 32]);
    }

    #[test]
    fn test_json_block_to_block_body_empty_transactions() {
        let block = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "transactions": []
        });
        let body = json_block_to_block_body(&block);
        assert!(body.is_some());
        assert_eq!(body.unwrap().transactions.len(), 0);
    }

    #[test]
    fn test_json_block_to_block_body_missing_hash() {
        let block = serde_json::json!({
            "transactions": ["0x1111111111111111111111111111111111111111111111111111111111111111"]
        });
        assert!(json_block_to_block_body(&block).is_none());
    }

    #[test]
    fn test_json_block_to_block_body_missing_transactions() {
        let block = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });
        assert!(json_block_to_block_body(&block).is_none());
    }

    #[test]
    fn test_json_block_to_block_body_invalid_tx_hash() {
        // Invalid tx hash should be skipped (lines 250-254)
        let block = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "transactions": [
                "0x1111111111111111111111111111111111111111111111111111111111111111",
                "invalid_hash",
                "0x2222222222222222222222222222222222222222222222222222222222222222"
            ]
        });
        let body = json_block_to_block_body(&block);
        assert!(body.is_some());
        // Invalid tx hash is skipped
        assert_eq!(body.unwrap().transactions.len(), 2);
    }

    #[test]
    fn test_json_block_to_block_body_with_full_tx_objects() {
        // When transactions are full objects (not hashes), extract hash from object
        let block = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "transactions": [
                {"hash": "0x1111111111111111111111111111111111111111111111111111111111111111", "from": "0x1234"},
                "0x2222222222222222222222222222222222222222222222222222222222222222"
            ]
        });
        let body = json_block_to_block_body(&block);
        assert!(body.is_some());
        // Both transaction hashes are extracted (from object and string)
        assert_eq!(body.unwrap().transactions.len(), 2);
    }

    #[test]
    fn test_block_header_and_body_to_json_complete() {
        let header = BlockHeader {
            hash: [0xAA; 32],
            number: 12345,
            parent_hash: [0xBB; 32],
            timestamp: 1_640_995_200,
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            miner: [0xCC; 20],
            extra_data: Arc::new(vec![0xDD; 32]),
            logs_bloom: Arc::new(vec![0xEE; 256]),
            transactions_root: [0xFF; 32],
            state_root: [0x11; 32],
            receipts_root: [0x22; 32],
        };

        let body = BlockBody { hash: [0xAA; 32], transactions: vec![[0x33; 32], [0x44; 32]] };

        let json = block_header_and_body_to_json(&header, &body);

        // Verify all header fields
        assert_eq!(
            json.get("hash").unwrap().as_str().unwrap(),
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(json.get("number").unwrap().as_str().unwrap(), "0x3039");
        assert_eq!(
            json.get("parentHash").unwrap().as_str().unwrap(),
            "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert_eq!(json.get("timestamp").unwrap().as_str().unwrap(), "0x61cf9980");
        assert_eq!(json.get("gasLimit").unwrap().as_str().unwrap(), "0x1c9c380");
        assert_eq!(json.get("gasUsed").unwrap().as_str().unwrap(), "0xe4e1c0");
        assert_eq!(
            json.get("miner").unwrap().as_str().unwrap(),
            "0xcccccccccccccccccccccccccccccccccccccccc"
        );

        // Verify transactions
        let txs = json.get("transactions").unwrap().as_array().unwrap();
        assert_eq!(txs.len(), 2);
        assert_eq!(
            txs[0].as_str().unwrap(),
            "0x3333333333333333333333333333333333333333333333333333333333333333"
        );

        // Verify uncles array (always empty)
        let uncles = json.get("uncles").unwrap().as_array().unwrap();
        assert!(uncles.is_empty());
    }

    #[test]
    fn test_block_header_and_body_to_json_empty_transactions() {
        let header = BlockHeader {
            hash: [0xAA; 32],
            number: 1,
            parent_hash: [0xBB; 32],
            timestamp: 100,
            gas_limit: 1000,
            gas_used: 500,
            miner: [0xCC; 20],
            extra_data: Arc::new(vec![]),
            logs_bloom: Arc::new(vec![0; 256]),
            transactions_root: [0xFF; 32],
            state_root: [0x11; 32],
            receipts_root: [0x22; 32],
        };

        let body = BlockBody { hash: [0xAA; 32], transactions: vec![] };

        let json = block_header_and_body_to_json(&header, &body);
        let txs = json.get("transactions").unwrap().as_array().unwrap();
        assert!(txs.is_empty());
    }

    #[test]
    fn test_json_transaction_to_transaction_record_confirmed() {
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x5",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0000000000000000000000000000000000000000000000000de0b6b3a7640000",
            "gasPrice": "0x0000000000000000000000000000000000000000000000000000000077359400",
            "gas": "0x5208",
            "nonce": "0x2a",
            "input": "0x",
            "v": "0x1b",
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222"
        });

        let record = json_transaction_to_transaction_record(&tx);
        assert!(record.is_some());
        let record = record.unwrap();

        assert_eq!(record.hash, [0xAA; 32]);
        assert_eq!(record.block_hash, Some([0xBB; 32]));
        assert_eq!(record.block_number, Some(1000));
        assert_eq!(record.transaction_index, Some(5));
        assert_eq!(record.gas_limit, 21000);
        assert_eq!(record.nonce, 42);
        assert_eq!(record.v, 27);
        assert!(record.to.is_some());
    }

    #[test]
    fn test_json_transaction_to_transaction_record_pending() {
        // Pending transactions have null blockHash, blockNumber, transactionIndex
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": null,
            "blockNumber": null,
            "transactionIndex": null,
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0000000000000000000000000000000000000000000000000de0b6b3a7640000",
            "gasPrice": "0x0000000000000000000000000000000000000000000000000000000077359400",
            "gas": "0x5208",
            "nonce": "0x0",
            "input": "0x",
            "v": "0x1c",
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222"
        });

        let record = json_transaction_to_transaction_record(&tx);
        assert!(record.is_some());
        let record = record.unwrap();

        assert!(record.block_hash.is_none());
        assert!(record.block_number.is_none());
        assert!(record.transaction_index.is_none());
    }

    #[test]
    fn test_json_transaction_to_transaction_record_contract_creation() {
        // Contract creation has null "to" field
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x0",
            "from": "0x1234567890123456789012345678901234567890",
            "to": null,
            "value": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "gasPrice": "0x0000000000000000000000000000000000000000000000000000000077359400",
            "gas": "0x7a120",
            "nonce": "0x0",
            "input": "0x608060405234801561001057600080fd5b50",
            "v": "0x1b",
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222"
        });

        let record = json_transaction_to_transaction_record(&tx);
        assert!(record.is_some());
        let record = record.unwrap();

        assert!(record.to.is_none());
        assert!(!record.data.is_empty()); // Contract bytecode
    }

    #[test]
    fn test_json_transaction_to_transaction_record_decimal_fallback() {
        // Test decimal parsing fallback (lines 343, 350, 369, 376, 386)
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "1000",  // decimal format
            "transactionIndex": "5",  // decimal format
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0000000000000000000000000000000000000000000000000de0b6b3a7640000",
            "gasPrice": "0x0000000000000000000000000000000000000000000000000000000077359400",
            "gas": "21000",  // decimal format
            "nonce": "42",  // decimal format
            "input": "0x",
            "v": "27",  // decimal format
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222"
        });

        let record = json_transaction_to_transaction_record(&tx);
        assert!(record.is_some());
        let record = record.unwrap();

        assert_eq!(record.block_number, Some(1000));
        assert_eq!(record.transaction_index, Some(5));
        assert_eq!(record.gas_limit, 21000);
        assert_eq!(record.nonce, 42);
        assert_eq!(record.v, 27);
    }

    #[test]
    fn test_json_transaction_to_transaction_record_missing_hash() {
        let tx = serde_json::json!({
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0",
            "gasPrice": "0x0",
            "gas": "0x5208",
            "nonce": "0x0",
            "input": "0x",
            "v": "0x1b",
            "r": "0x0",
            "s": "0x0"
        });
        assert!(json_transaction_to_transaction_record(&tx).is_none());
    }

    #[test]
    fn test_json_transaction_to_transaction_record_missing_from() {
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0",
            "gasPrice": "0x0",
            "gas": "0x5208",
            "nonce": "0x0",
            "input": "0x",
            "v": "0x1b",
            "r": "0x0",
            "s": "0x0"
        });
        assert!(json_transaction_to_transaction_record(&tx).is_none());
    }

    #[test]
    fn test_json_transaction_to_transaction_record_eip1559_with_max_fee() {
        // EIP-1559 transaction has maxFeePerGas and maxPriorityFeePerGas, no gasPrice
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x5",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0000000000000000000000000000000000000000000000000de0b6b3a7640000",
            "maxFeePerGas": "0x0000000000000000000000000000000000000000000000000000000077359400",
            "maxPriorityFeePerGas": "0x0000000000000000000000000000000000000000000000000000000059682f00",
            "gas": "0x5208",
            "nonce": "0x2a",
            "input": "0x",
            "v": "0x1",
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222",
            "type": "0x2"
        });

        let record = json_transaction_to_transaction_record(&tx);
        assert!(record.is_some());
        let record = record.unwrap();

        assert_eq!(record.hash, [0xAA; 32]);
        assert_eq!(record.block_number, Some(1000));
        assert_eq!(record.tx_type, Some(2));
        // EIP-1559 transaction should not have gasPrice
        assert!(record.gas_price.is_none());
        // Should have maxFeePerGas
        assert!(record.max_fee_per_gas.is_some());
        assert_eq!(record.max_fee_per_gas.unwrap()[31], 0x00);
        assert_eq!(record.max_fee_per_gas.unwrap()[30], 0x94);
        assert_eq!(record.gas_limit, 21000);
    }

    #[test]
    fn test_json_transaction_to_transaction_record_eip4844_blob_tx() {
        // EIP-4844 blob transaction with maxFeePerBlobGas
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x100",
            "transactionIndex": "0x0",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "maxFeePerGas": "0x0000000000000000000000000000000000000000000000000000000077359400",
            "maxPriorityFeePerGas": "0x0000000000000000000000000000000000000000000000000000000059682f00",
            "maxFeePerBlobGas": "0x0000000000000000000000000000000000000000000000000000000000000001",
            "gas": "0x5208",
            "nonce": "0x0",
            "input": "0x",
            "v": "0x1",
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222",
            "type": "0x3"
        });

        let record = json_transaction_to_transaction_record(&tx);
        assert!(record.is_some());
        let record = record.unwrap();

        assert_eq!(record.hash, [0xAA; 32]);
        assert_eq!(record.tx_type, Some(3));
        // EIP-4844 transaction should not have gasPrice
        assert!(record.gas_price.is_none());
        // Should have maxFeePerGas
        assert!(record.max_fee_per_gas.is_some());
        assert_eq!(record.max_fee_per_gas.unwrap()[31], 0x00);
        assert_eq!(record.max_fee_per_gas.unwrap()[30], 0x94);
        // Should have maxFeePerBlobGas
        assert!(record.max_fee_per_blob_gas.is_some());
        assert_eq!(record.max_fee_per_blob_gas.unwrap()[31], 0x01);
    }

    #[test]
    fn test_json_transaction_to_transaction_record_legacy_prefers_gas_price() {
        // Legacy transaction with both gasPrice and maxFeePerGas (should prefer gasPrice)
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x5",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0000000000000000000000000000000000000000000000000de0b6b3a7640000",
            "gasPrice": "0x0000000000000000000000000000000000000000000000000000000012345678",
            "maxFeePerGas": "0x0000000000000000000000000000000000000000000000000000000077359400",
            "gas": "0x5208",
            "nonce": "0x2a",
            "input": "0x",
            "v": "0x1b",
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222"
        });

        let record = json_transaction_to_transaction_record(&tx);
        assert!(record.is_some());
        let record = record.unwrap();

        // Should have gasPrice
        assert!(record.gas_price.is_some());
        let gas_price = record.gas_price.unwrap();
        assert_eq!(gas_price[31], 0x78);
        assert_eq!(gas_price[30], 0x56);
        assert_eq!(gas_price[29], 0x34);
        assert_eq!(gas_price[28], 0x12);
        // Should also have maxFeePerGas (both can be present)
        assert!(record.max_fee_per_gas.is_some());
    }

    #[test]
    fn test_json_transaction_to_transaction_record_missing_both_gas_prices() {
        // Transaction missing both gasPrice and maxFeePerGas should now succeed (both are optional)
        let tx = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x5",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0000000000000000000000000000000000000000000000000de0b6b3a7640000",
            "gas": "0x5208",
            "nonce": "0x2a",
            "input": "0x",
            "v": "0x1b",
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222"
        });

        let record = json_transaction_to_transaction_record(&tx);
        assert!(record.is_some());
        let record = record.unwrap();
        // Both should be None
        assert!(record.gas_price.is_none());
        assert!(record.max_fee_per_gas.is_none());
    }

    #[test]
    fn test_transaction_record_to_json_null_fields() {
        // Test null field serialization
        let transaction = TransactionRecord {
            hash: [0xAA; 32],
            block_hash: None,
            block_number: None,
            transaction_index: None,
            from: [0xCC; 20],
            to: None,
            value: [0; 32],
            tx_type: Some(0),
            gas_price: Some([0; 32]),
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            max_fee_per_blob_gas: None,
            gas_limit: 21000,
            nonce: 0,
            data: vec![],
            v: 27,
            r: [0; 32],
            s: [0; 32],
        };

        let json = transaction_record_to_json(&transaction);

        assert!(json.get("blockHash").unwrap().is_null());
        assert!(json.get("blockNumber").unwrap().is_null());
        assert!(json.get("transactionIndex").unwrap().is_null());
        assert!(json.get("to").unwrap().is_null());
    }

    #[test]
    fn test_transaction_record_to_json_large_input_data() {
        // Test format_hex_large path for data > 256 bytes
        let transaction = TransactionRecord {
            hash: [0xAA; 32],
            block_hash: Some([0xBB; 32]),
            block_number: Some(12345),
            transaction_index: Some(0),
            from: [0xCC; 20],
            to: Some([0xDD; 20]),
            value: [0; 32],
            tx_type: Some(0),
            gas_price: Some([0; 32]),
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            max_fee_per_blob_gas: None,
            gas_limit: 500_000,
            nonce: 0,
            data: vec![0xAB; 500], // > 256 bytes triggers format_hex_large
            v: 27,
            r: [0; 32],
            s: [0; 32],
        };

        let json = transaction_record_to_json(&transaction);
        let input = json.get("input").unwrap().as_str().unwrap();
        assert!(input.starts_with("0x"));
        assert_eq!(input.len(), 2 + 500 * 2); // "0x" + 500 bytes * 2 hex chars
    }

    #[test]
    fn test_json_transaction_eip1559_roundtrip() {
        // Test full roundtrip for EIP-1559 transaction
        let original_json = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x5",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "value": "0x0000000000000000000000000000000000000000000000000de0b6b3a7640000",
            "maxFeePerGas": "0x0000000000000000000000000000000000000000000000000000000077359400",
            "maxPriorityFeePerGas": "0x0000000000000000000000000000000000000000000000000000000059682f00",
            "gas": "0x5208",
            "nonce": "0x2a",
            "input": "0x",
            "v": "0x1b",
            "r": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "s": "0x2222222222222222222222222222222222222222222222222222222222222222",
            "type": "0x2"
        });

        // Parse to record
        let record = json_transaction_to_transaction_record(&original_json).unwrap();

        // Verify fields are parsed correctly
        assert_eq!(record.tx_type, Some(2));
        assert!(record.gas_price.is_none());
        assert!(record.max_fee_per_gas.is_some());
        assert!(record.max_priority_fee_per_gas.is_some());
        assert!(record.max_fee_per_blob_gas.is_none());

        // Convert back to JSON
        let json = transaction_record_to_json(&record);

        // Verify critical fields are present
        assert_eq!(json.get("type").unwrap().as_str().unwrap(), "0x2");
        assert!(json.get("maxFeePerGas").is_some());
        assert!(json.get("maxPriorityFeePerGas").is_some());
        assert!(json.get("gasPrice").is_none());
    }

    #[test]
    fn test_transaction_record_to_json_small_input_data() {
        // Test format_hex path for data <= 256 bytes
        let transaction = TransactionRecord {
            hash: [0xAA; 32],
            block_hash: Some([0xBB; 32]),
            block_number: Some(12345),
            transaction_index: Some(0),
            from: [0xCC; 20],
            to: Some([0xDD; 20]),
            value: [0; 32],
            tx_type: Some(0),
            gas_price: Some([0; 32]),
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            max_fee_per_blob_gas: None,
            gas_limit: 100_000,
            nonce: 0,
            data: vec![0xAB; 100], // <= 256 bytes uses format_hex
            v: 27,
            r: [0; 32],
            s: [0; 32],
        };

        let json = transaction_record_to_json(&transaction);
        let input = json.get("input").unwrap().as_str().unwrap();
        assert!(input.starts_with("0x"));
        assert_eq!(input.len(), 2 + 100 * 2);
    }

    #[test]
    fn test_json_receipt_to_receipt_record_complete() {
        let receipt = serde_json::json!({
            "transactionHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x5",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "cumulativeGasUsed": "0x16e360",
            "gasUsed": "0x5208",
            "contractAddress": null,
            "logs": [
                {
                    "blockNumber": "0x3e8",
                    "logIndex": "0x0"
                },
                {
                    "blockNumber": "0x3e8",
                    "logIndex": "0x1"
                }
            ],
            "status": "0x1",
            "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "effectiveGasPrice": "0x3b9aca00",
            "type": "0x2"
        });

        let record = json_receipt_to_receipt_record(&receipt);
        assert!(record.is_some());
        let record = record.unwrap();

        assert_eq!(record.transaction_hash, [0xAA; 32]);
        assert_eq!(record.block_hash, [0xBB; 32]);
        assert_eq!(record.block_number, 1000);
        assert_eq!(record.transaction_index, 5);
        assert_eq!(record.cumulative_gas_used, 1_500_000);
        assert_eq!(record.gas_used, 21_000);
        assert!(record.contract_address.is_none());
        assert_eq!(record.logs.len(), 2);
        assert_eq!(record.status, 1);
        assert_eq!(record.effective_gas_price, Some(1_000_000_000));
        assert_eq!(record.tx_type, Some(2));
        assert!(record.blob_gas_price.is_none());
        assert!(record.to.is_some());
    }

    #[test]
    fn test_json_receipt_to_receipt_record_contract_creation() {
        let receipt = serde_json::json!({
            "transactionHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x0",
            "from": "0x1234567890123456789012345678901234567890",
            "to": null,
            "cumulativeGasUsed": "0x7a120",
            "gasUsed": "0x7a120",
            "contractAddress": "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "logs": [],
            "status": "0x1",
            "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        });

        let record = json_receipt_to_receipt_record(&receipt);
        assert!(record.is_some());
        let record = record.unwrap();

        assert!(record.to.is_none());
        assert!(record.contract_address.is_some());
        assert_eq!(record.contract_address.unwrap(), [0xEE; 20]);
    }

    #[test]
    fn test_json_receipt_to_receipt_record_eip4844() {
        // Test EIP-4844 blob transaction with blob_gas_price (line 558)
        let receipt = serde_json::json!({
            "transactionHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x0",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "cumulativeGasUsed": "0x5208",
            "gasUsed": "0x5208",
            "contractAddress": null,
            "logs": [],
            "status": "0x1",
            "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "effectiveGasPrice": "0x3b9aca00",
            "type": "0x3",
            "blobGasPrice": "0x1"
        });

        let record = json_receipt_to_receipt_record(&receipt);
        assert!(record.is_some());
        let record = record.unwrap();

        assert_eq!(record.tx_type, Some(3)); // EIP-4844 type
        assert_eq!(record.blob_gas_price, Some(1));
    }

    #[test]
    fn test_json_receipt_to_receipt_record_decimal_fallback() {
        // Test decimal parsing fallback paths (lines 489, 496, 509, 516, 535)
        let receipt = serde_json::json!({
            "transactionHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "1000",  // decimal
            "transactionIndex": "5",  // decimal
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "cumulativeGasUsed": "1500000",  // decimal
            "gasUsed": "21000",  // decimal
            "contractAddress": null,
            "logs": [],
            "status": "1",  // decimal
            "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        });

        let record = json_receipt_to_receipt_record(&receipt);
        assert!(record.is_some());
        let record = record.unwrap();

        assert_eq!(record.block_number, 1000);
        assert_eq!(record.transaction_index, 5);
        assert_eq!(record.cumulative_gas_used, 1_500_000);
        assert_eq!(record.gas_used, 21_000);
        assert_eq!(record.status, 1);
    }

    #[test]
    fn test_json_receipt_to_receipt_record_invalid_tx_type() {
        // Test invalid tx_type > 255 (lines 549-555)
        let receipt = serde_json::json!({
            "transactionHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x0",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "cumulativeGasUsed": "0x5208",
            "gasUsed": "0x5208",
            "contractAddress": null,
            "logs": [],
            "status": "0x1",
            "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "type": "0x100"  // 256 - exceeds u8 range
        });

        let record = json_receipt_to_receipt_record(&receipt);
        assert!(record.is_some());
        let record = record.unwrap();

        // Invalid type should result in None
        assert!(record.tx_type.is_none());
    }

    #[test]
    fn test_json_receipt_to_receipt_record_missing_transaction_hash() {
        let receipt = serde_json::json!({
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x0",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "cumulativeGasUsed": "0x5208",
            "gasUsed": "0x5208",
            "logs": [],
            "status": "0x1",
            "logsBloom": "0x00"
        });
        assert!(json_receipt_to_receipt_record(&receipt).is_none());
    }

    #[test]
    fn test_json_receipt_to_receipt_record_with_logs() {
        // Test log parsing within receipt (lines 525-529)
        let receipt = serde_json::json!({
            "transactionHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "blockHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "blockNumber": "0x3e8",
            "transactionIndex": "0x0",
            "from": "0x1234567890123456789012345678901234567890",
            "to": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "cumulativeGasUsed": "0x5208",
            "gasUsed": "0x5208",
            "contractAddress": null,
            "logs": [
                {
                    "blockNumber": "0x3e8",
                    "logIndex": "0x0"
                },
                {
                    "blockNumber": "0x3e8",
                    "logIndex": "0x1"
                },
                {
                    "blockNumber": "invalid",  // Invalid log should be skipped
                    "logIndex": "0x2"
                }
            ],
            "status": "0x1",
            "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        });

        let record = json_receipt_to_receipt_record(&receipt);
        assert!(record.is_some());
        let record = record.unwrap();

        // Invalid log is skipped
        assert_eq!(record.logs.len(), 2);
        assert_eq!(record.logs[0].block_number, 1000);
        assert_eq!(record.logs[0].log_index, 0);
        assert_eq!(record.logs[1].log_index, 1);
    }

    #[test]
    fn test_receipt_record_to_json_with_blob_gas_price() {
        let receipt = ReceiptRecord {
            transaction_hash: [0xAA; 32],
            block_hash: [0xBB; 32],
            block_number: 12345,
            transaction_index: 0,
            from: [0xCC; 20],
            to: Some([0xDD; 20]),
            cumulative_gas_used: 21_000,
            gas_used: 21_000,
            contract_address: None,
            logs: vec![],
            status: 1,
            logs_bloom: vec![0; 256],
            effective_gas_price: Some(1_000_000_000),
            tx_type: Some(3), // EIP-4844
            blob_gas_price: Some(1_000_000),
        };

        let json = receipt_record_to_json(&receipt, &[]);

        assert_eq!(json.get("blobGasPrice").unwrap().as_str().unwrap(), "0xf4240");
        assert_eq!(json.get("type").unwrap().as_str().unwrap(), "0x3");
    }

    #[test]
    fn test_receipt_record_to_json_without_optional_fields() {
        let receipt = ReceiptRecord {
            transaction_hash: [0xAA; 32],
            block_hash: [0xBB; 32],
            block_number: 12345,
            transaction_index: 0,
            from: [0xCC; 20],
            to: None,
            cumulative_gas_used: 21_000,
            gas_used: 21_000,
            contract_address: Some([0xEE; 20]),
            logs: vec![],
            status: 1,
            logs_bloom: vec![0; 256],
            effective_gas_price: None,
            tx_type: None,
            blob_gas_price: None,
        };

        let json = receipt_record_to_json(&receipt, &[]);

        assert!(json.get("blobGasPrice").is_none());
        assert!(json.get("type").is_none());
        assert!(json.get("effectiveGasPrice").is_none());
        assert!(json.get("to").unwrap().is_null());
        assert!(json.get("contractAddress").unwrap().is_string());
    }

    #[test]
    fn test_receipt_record_to_json_with_logs() {
        let log_id = LogId::new(12345, 0);
        let log_record = Arc::new(LogRecord::new(
            [0x11; 20],
            [Some([0x22; 32]), None, None, None],
            vec![0x33],
            [0x44; 32],
            [0x55; 32],
            0,
            false,
        ));

        let receipt = ReceiptRecord {
            transaction_hash: [0xAA; 32],
            block_hash: [0xBB; 32],
            block_number: 12345,
            transaction_index: 0,
            from: [0xCC; 20],
            to: Some([0xDD; 20]),
            cumulative_gas_used: 21_000,
            gas_used: 21_000,
            contract_address: None,
            logs: vec![log_id],
            status: 1,
            logs_bloom: vec![0; 256],
            effective_gas_price: None,
            tx_type: None,
            blob_gas_price: None,
        };

        let json = receipt_record_to_json(&receipt, &[log_record]);

        let logs = json.get("logs").unwrap().as_array().unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].get("address").unwrap().as_str().unwrap(),
            "0x1111111111111111111111111111111111111111"
        );
    }

    #[test]
    fn test_json_params_to_log_filter_basic() {
        let params = serde_json::json!([{
            "fromBlock": "0x1",
            "toBlock": "0x100"
        }]);

        let filter = json_params_to_log_filter(&params);
        assert!(filter.is_some());
        let filter = filter.unwrap();

        assert_eq!(filter.from_block, 1);
        assert_eq!(filter.to_block, 256);
        assert!(filter.address.is_none());
        assert!(filter.topics.iter().all(Option::is_none));
    }

    #[test]
    fn test_json_params_to_log_filter_with_address() {
        let params = serde_json::json!([{
            "fromBlock": "0x1",
            "toBlock": "0x100",
            "address": "0x1234567890123456789012345678901234567890"
        }]);

        let filter = json_params_to_log_filter(&params);
        assert!(filter.is_some());
        let filter = filter.unwrap();

        assert!(filter.address.is_some());
    }

    #[test]
    fn test_json_params_to_log_filter_with_topics() {
        let params = serde_json::json!([{
            "fromBlock": "0x1",
            "toBlock": "0x100",
            "topics": [
                "0x1111111111111111111111111111111111111111111111111111111111111111",
                "0x2222222222222222222222222222222222222222222222222222222222222222",
                null,
                "0x4444444444444444444444444444444444444444444444444444444444444444"
            ]
        }]);

        let filter = json_params_to_log_filter(&params);
        assert!(filter.is_some());
        let filter = filter.unwrap();

        // Topics at positions 0, 1, 3 (null at position 2 is skipped)
        assert!(filter.topics[0].is_some());
        assert!(filter.topics[1].is_some());
        assert!(filter.topics[2].is_none());
        assert!(filter.topics[3].is_some());
    }

    #[test]
    fn test_json_params_to_log_filter_with_topics_anywhere() {
        let params = serde_json::json!([{
            "fromBlock": "0x1",
            "toBlock": "0x100",
            "topicsAnywhere": [
                "0x1111111111111111111111111111111111111111111111111111111111111111",
                "0x2222222222222222222222222222222222222222222222222222222222222222"
            ]
        }]);

        let filter = json_params_to_log_filter(&params);
        assert!(filter.is_some());
        let filter = filter.unwrap();

        assert_eq!(filter.topics_anywhere.len(), 2);
    }

    #[test]
    fn test_json_params_to_log_filter_decimal_fallback() {
        // Test decimal parsing fallback (lines 675, 682)
        let params = serde_json::json!([{
            "fromBlock": "100",
            "toBlock": "200"
        }]);

        let filter = json_params_to_log_filter(&params);
        assert!(filter.is_some());
        let filter = filter.unwrap();

        assert_eq!(filter.from_block, 100);
        assert_eq!(filter.to_block, 200);
    }

    #[test]
    fn test_json_params_to_log_filter_empty_params() {
        let params = serde_json::json!([]);
        assert!(json_params_to_log_filter(&params).is_none());
    }

    #[test]
    fn test_json_params_to_log_filter_not_array() {
        let params = serde_json::json!({
            "fromBlock": "0x1",
            "toBlock": "0x100"
        });
        assert!(json_params_to_log_filter(&params).is_none());
    }

    #[test]
    fn test_json_params_to_log_filter_missing_from_block() {
        let params = serde_json::json!([{
            "toBlock": "0x100"
        }]);
        assert!(json_params_to_log_filter(&params).is_none());
    }

    #[test]
    fn test_json_params_to_log_filter_missing_to_block() {
        let params = serde_json::json!([{
            "fromBlock": "0x1"
        }]);
        assert!(json_params_to_log_filter(&params).is_none());
    }

    #[test]
    fn test_json_params_to_log_filter_invalid_address() {
        // Invalid address should be ignored (lines 688-690)
        let params = serde_json::json!([{
            "fromBlock": "0x1",
            "toBlock": "0x100",
            "address": "invalid"
        }]);

        let filter = json_params_to_log_filter(&params);
        assert!(filter.is_some());
        // Invalid address is skipped
        assert!(filter.unwrap().address.is_none());
    }

    #[test]
    fn test_json_params_to_log_filter_invalid_topic() {
        // Invalid topics should be ignored (lines 696-698)
        let params = serde_json::json!([{
            "fromBlock": "0x1",
            "toBlock": "0x100",
            "topics": ["invalid_topic", null]
        }]);

        let filter = json_params_to_log_filter(&params);
        assert!(filter.is_some());
        // Invalid topic results in None for that position
        let filter = filter.unwrap();
        assert!(filter.topics[0].is_none()); // Invalid topic
        assert!(filter.topics[1].is_none()); // null
    }

    #[test]
    fn test_json_params_to_log_filter_invalid_topics_anywhere() {
        // Invalid topicsAnywhere should be ignored (lines 706-708)
        let params = serde_json::json!([{
            "fromBlock": "0x1",
            "toBlock": "0x100",
            "topicsAnywhere": ["invalid", 123]
        }]);

        let filter = json_params_to_log_filter(&params);
        assert!(filter.is_some());
        // Invalid topics are skipped
        assert!(filter.unwrap().topics_anywhere.is_empty());
    }

    #[test]
    fn test_json_block_to_block_header_decimal_fallback() {
        let block = serde_json::json!({
            "hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "number": "12345",  // decimal
            "parentHash": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "timestamp": "1640995200",  // decimal
            "gasLimit": "30000000",  // decimal
            "gasUsed": "15000000",  // decimal
            "miner": "0xcccccccccccccccccccccccccccccccccccccccc",
            "extraData": "0x1234",
            "logsBloom": "0x00",
            "transactionsRoot": "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "stateRoot": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "receiptsRoot": "0x2222222222222222222222222222222222222222222222222222222222222222"
        });

        let header = json_block_to_block_header(&block);
        assert!(header.is_some());
        let header = header.unwrap();

        assert_eq!(header.number, 12345);
        assert_eq!(header.timestamp, 1_640_995_200);
        assert_eq!(header.gas_limit, 30_000_000);
        assert_eq!(header.gas_used, 15_000_000);
    }

    #[test]
    fn test_json_log_to_log_record_invalid_topic() {
        let log = serde_json::json!({
            "blockNumber": "0x3e8",
            "logIndex": "0x0",
            "address": "0x1234567890123456789012345678901234567890",
            "topics": [
                "0x1234567890123456789012345678901234567890123456789012345678901234",
                "invalid_topic",  // Invalid topic should result in None for this slot
                "0x3333333333333333333333333333333333333333333333333333333333333333"
            ],
            "data": "0x",
            "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "blockHash": "0xabababababababababababababababababababababababababababababababab",
            "transactionIndex": "0x0",
            "removed": false
        });

        let result = json_log_to_log_record(&log);
        assert!(result.is_some());
        let (_, record) = result.unwrap();

        // Topic 0 and 2 are valid, topic 1 is invalid (None)
        assert!(record.topics[0].is_some());
        assert!(record.topics[1].is_none()); // Invalid topic becomes None
        assert!(record.topics[2].is_some());
    }

    #[test]
    fn test_json_log_to_log_record_removed_true() {
        let log = serde_json::json!({
            "blockNumber": "0x3e8",
            "logIndex": "0x0",
            "address": "0x1234567890123456789012345678901234567890",
            "topics": [],
            "data": "0x",
            "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "blockHash": "0xabababababababababababababababababababababababababababababababab",
            "transactionIndex": "0x0",
            "removed": true
        });

        let result = json_log_to_log_record(&log);
        assert!(result.is_some());
        let (_, record) = result.unwrap();
        assert!(record.removed);
    }

    #[test]
    fn test_json_log_to_log_record_missing_removed() {
        // removed field is optional and defaults to false (line 127)
        let log = serde_json::json!({
            "blockNumber": "0x3e8",
            "logIndex": "0x0",
            "address": "0x1234567890123456789012345678901234567890",
            "topics": [],
            "data": "0x",
            "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "blockHash": "0xabababababababababababababababababababababababababababababababab",
            "transactionIndex": "0x0"
        });

        let result = json_log_to_log_record(&log);
        assert!(result.is_some());
        let (_, record) = result.unwrap();
        assert!(!record.removed); // Defaults to false
    }
}
