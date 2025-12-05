//! Data integrity validation for cached upstream responses.
//!
//! Protects against malicious or buggy upstreams by validating data before caching.

use crate::cache::{
    converter::{hex_to_array, hex_to_u64},
    types::LogFilter,
};
use serde_json::Value;
use tracing::{debug, warn};

#[must_use]
pub fn validate_block_response(block_json: &Value) -> bool {
    let Some(hash_str) = block_json.get("hash").and_then(|v| v.as_str()) else {
        warn!("block validation failed: missing hash field");
        return false;
    };

    if hex_to_array::<32>(hash_str).is_none() {
        warn!(hash = hash_str, "block validation failed: invalid hash format");
        return false;
    }

    // NOTE: Full block hash verification via RLP encoding + Keccak256 is not
    // currently implemented. This is acceptable because:
    // 1. We validate block hashes against ReorgManager's trusted history
    // 2. Upstream responses are assumed trustworthy (configured by operator)
    // 3. Full verification would add significant CPU overhead (RLP encoding)
    //
    // For defense-in-depth against malicious upstreams, consider implementing
    // full verification for high-security deployments.

    true
}

#[must_use]
pub fn validate_logs_response(logs_array: &[Value], filter: &LogFilter) -> bool {
    for (idx, log) in logs_array.iter().enumerate() {
        let Some(block_number) =
            log.get("blockNumber").and_then(|v| v.as_str()).and_then(hex_to_u64)
        else {
            warn!(log_index = idx, "log validation failed: missing or invalid blockNumber");
            return false;
        };

        if block_number < filter.from_block || block_number > filter.to_block {
            warn!(
                log_index = idx,
                block_number = block_number,
                from_block = filter.from_block,
                to_block = filter.to_block,
                "log validation failed: block number outside requested range"
            );
            return false;
        }

        if log.get("logIndex").is_none() {
            warn!(log_index = idx, "log validation failed: missing logIndex");
            return false;
        }

        if log.get("address").is_none() {
            warn!(log_index = idx, "log validation failed: missing address");
            return false;
        }
    }

    debug!(
        log_count = logs_array.len(),
        from_block = filter.from_block,
        to_block = filter.to_block,
        "logs validation passed"
    );
    true
}

#[must_use]
pub fn validate_receipt_response(receipt_json: &Value, expected_tx_hash: &[u8; 32]) -> bool {
    let Some(tx_hash_str) = receipt_json.get("transactionHash").and_then(|v| v.as_str()) else {
        warn!("receipt validation failed: missing transactionHash field");
        return false;
    };

    let Some(tx_hash) = hex_to_array::<32>(tx_hash_str) else {
        warn!(hash = tx_hash_str, "receipt validation failed: invalid hash format");
        return false;
    };

    if tx_hash != *expected_tx_hash {
        warn!(
            expected = ?expected_tx_hash,
            actual = ?tx_hash,
            "receipt validation failed: transaction hash mismatch"
        );
        return false;
    }

    if receipt_json
        .get("blockNumber")
        .and_then(|v| v.as_str())
        .and_then(hex_to_u64)
        .is_none()
    {
        warn!("receipt validation failed: missing or invalid blockNumber");
        return false;
    }

    debug!(tx_hash = ?expected_tx_hash, "receipt validation passed");
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_validate_block_response_valid() {
        let block = json!({
            "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
            "number": "0x1",
            "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000"
        });

        assert!(validate_block_response(&block));
    }

    #[test]
    fn test_validate_block_response_missing_hash() {
        let block = json!({
            "number": "0x1",
            "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000"
        });

        assert!(!validate_block_response(&block));
    }

    #[test]
    fn test_validate_block_response_invalid_hash() {
        let block = json!({
            "hash": "0xinvalid",
            "number": "0x1"
        });

        assert!(!validate_block_response(&block));
    }

    #[test]
    fn test_validate_logs_response_valid() {
        let filter = LogFilter {
            from_block: 100,
            to_block: 200,
            address: None,
            topics: [None, None, None, None],
            topics_anywhere: vec![],
        };

        let logs = vec![json!({
            "blockNumber": "0x64",  // 100
            "logIndex": "0x0",
            "address": "0x1234567890123456789012345678901234567890",
            "data": "0x",
            "topics": []
        })];

        assert!(validate_logs_response(&logs, &filter));
    }

    #[test]
    fn test_validate_logs_response_out_of_range() {
        let filter = LogFilter {
            from_block: 100,
            to_block: 200,
            address: None,
            topics: [None, None, None, None],
            topics_anywhere: vec![],
        };

        let logs = vec![json!({
            "blockNumber": "0x12c",  // 300 - outside range
            "logIndex": "0x0",
            "address": "0x1234567890123456789012345678901234567890",
            "data": "0x",
            "topics": []
        })];

        assert!(!validate_logs_response(&logs, &filter));
    }

    #[test]
    fn test_validate_logs_response_missing_block_number() {
        let filter = LogFilter {
            from_block: 100,
            to_block: 200,
            address: None,
            topics: [None, None, None, None],
            topics_anywhere: vec![],
        };

        let logs = vec![json!({
            "logIndex": "0x0",
            "address": "0x1234567890123456789012345678901234567890"
        })];

        assert!(!validate_logs_response(&logs, &filter));
    }

    #[test]
    fn test_validate_receipt_response_valid() {
        let expected_hash: [u8; 32] = [0x12; 32];
        let receipt = json!({
            "transactionHash": "0x1212121212121212121212121212121212121212121212121212121212121212",
            "blockNumber": "0x1",
            "status": "0x1"
        });

        assert!(validate_receipt_response(&receipt, &expected_hash));
    }

    #[test]
    fn test_validate_receipt_response_hash_mismatch() {
        let expected_hash: [u8; 32] = [0x12; 32];
        let receipt = json!({
            "transactionHash": "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "blockNumber": "0x1",
            "status": "0x1"
        });

        assert!(!validate_receipt_response(&receipt, &expected_hash));
    }

    #[test]
    fn test_validate_receipt_response_missing_hash() {
        let expected_hash: [u8; 32] = [0x12; 32];
        let receipt = json!({
            "blockNumber": "0x1",
            "status": "0x1"
        });

        assert!(!validate_receipt_response(&receipt, &expected_hash));
    }
}
