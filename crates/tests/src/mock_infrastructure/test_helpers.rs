//! Test Helper Functions and Utilities
//!
//! Common helpers for creating test data and fixtures.

use serde_json::{json, Value};

/// Creates a batch of test logs for a given block range.
#[must_use]
pub fn create_test_logs(from_block: u64, to_block: u64, logs_per_block: u64) -> Vec<Value> {
    let mut logs = Vec::new();
    for block in from_block..=to_block {
        for log_idx in 0..logs_per_block {
            logs.push(create_test_log(block, log_idx));
        }
    }
    logs
}

/// Creates a single test log.
#[must_use]
pub fn create_test_log(block_number: u64, log_index: u64) -> Value {
    json!({
        "address": "0x0000000000000000000000000000000000000001",
        "blockNumber": format!("0x{:x}", block_number),
        "blockHash": format!("0x{:064x}", block_number),
        "logIndex": format!("0x{:x}", log_index),
        "transactionHash": format!("0x{:064x}", block_number * 100 + log_index),
        "transactionIndex": "0x0",
        "topics": [format!("0x{:064x}", log_index)],
        "data": "0x",
        "removed": false
    })
}

/// Creates a test block response.
#[must_use]
pub fn create_test_block(block_number: u64, tx_count: usize) -> Value {
    let transactions: Vec<Value> = (0..tx_count)
        .map(|i| {
            json!({
                "hash": format!("0x{:064x}", block_number * 1000 + i as u64),
                "nonce": format!("0x{:x}", i),
                "blockHash": format!("0x{:064x}", block_number),
                "blockNumber": format!("0x{:x}", block_number),
                "transactionIndex": format!("0x{:x}", i),
                "from": "0x0000000000000000000000000000000000000001",
                "to": "0x0000000000000000000000000000000000000002",
                "value": "0x0",
                "gas": "0x5208",
                "gasPrice": "0x1",
                "input": "0x"
            })
        })
        .collect();

    json!({
        "number": format!("0x{:x}", block_number),
        "hash": format!("0x{:064x}", block_number),
        "parentHash": format!("0x{:064x}", block_number.saturating_sub(1)),
        "timestamp": format!("0x{:x}", 1_600_000_000 + block_number),
        "transactions": transactions,
        "gasLimit": "0x1c9c380",
        "gasUsed": "0x5208",
        "baseFeePerGas": "0x7",
        "difficulty": "0x0",
        "extraData": "0x",
        "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "miner": "0x0000000000000000000000000000000000000000",
        "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "nonce": "0x0000000000000000",
        "receiptsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
        "sha3Uncles": "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347",
        "stateRoot": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "transactionsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
    })
}

/// Creates a test receipt.
#[must_use]
pub fn create_test_receipt(block_number: u64, tx_index: u64, log_count: u64) -> Value {
    let logs: Vec<Value> = (0..log_count).map(|i| create_test_log(block_number, i)).collect();

    json!({
        "transactionHash": format!("0x{:064x}", block_number * 1000 + tx_index),
        "transactionIndex": format!("0x{:x}", tx_index),
        "blockHash": format!("0x{:064x}", block_number),
        "blockNumber": format!("0x{:x}", block_number),
        "from": "0x0000000000000000000000000000000000000001",
        "to": "0x0000000000000000000000000000000000000002",
        "cumulativeGasUsed": "0x5208",
        "gasUsed": "0x5208",
        "contractAddress": null,
        "logs": logs,
        "logsBloom": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "status": "0x1",
        "effectiveGasPrice": "0x1"
    })
}

/// Creates a JSON-RPC request.
#[must_use]
pub fn create_json_rpc_request(method: &str, params: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    })
}

/// Creates an `eth_getLogs` filter.
#[must_use]
pub fn create_log_filter(from_block: u64, to_block: u64) -> Value {
    json!({
        "fromBlock": format!("0x{:x}", from_block),
        "toBlock": format!("0x{:x}", to_block)
    })
}

/// Creates an `eth_getLogs` filter with an address.
#[must_use]
pub fn create_log_filter_with_address(from_block: u64, to_block: u64, address: &str) -> Value {
    json!({
        "fromBlock": format!("0x{:x}", from_block),
        "toBlock": format!("0x{:x}", to_block),
        "address": address
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_test_log() {
        let log = create_test_log(100, 5);
        assert_eq!(log["blockNumber"], "0x64");
        assert_eq!(log["logIndex"], "0x5");
    }

    #[test]
    fn test_create_test_logs() {
        let logs = create_test_logs(100, 102, 2);
        assert_eq!(logs.len(), 6); // 3 blocks * 2 logs each
    }

    #[test]
    fn test_create_test_block() {
        let block = create_test_block(100, 5);
        assert_eq!(block["number"], "0x64");
        assert_eq!(block["transactions"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn test_create_test_receipt() {
        let receipt = create_test_receipt(100, 0, 3);
        assert_eq!(receipt["blockNumber"], "0x64");
        assert_eq!(receipt["logs"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn test_create_log_filter() {
        let filter = create_log_filter(100, 200);
        assert_eq!(filter["fromBlock"], "0x64");
        assert_eq!(filter["toBlock"], "0xc8");
    }
}
