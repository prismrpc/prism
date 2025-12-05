//! RPC Mock Builder for Ethereum JSON-RPC Testing
//!
//! Wraps mockito to provide Ethereum-specific response builders for common RPC methods.

use mockito::{Matcher, Mock, Server, ServerGuard};
use serde_json::{json, Value};

/// Builder for creating mock Ethereum RPC responses.
///
/// Uses mockito internally but provides Ethereum-specific helpers.
pub struct RpcMockBuilder {
    server: ServerGuard,
    mocks: Vec<Mock>,
}

impl RpcMockBuilder {
    /// Creates a new RPC mock builder with a fresh mockito server.
    pub async fn new() -> Self {
        Self { server: Server::new_async().await, mocks: Vec::new() }
    }

    /// Returns the URL of the mock server.
    #[must_use]
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Mocks an `eth_getBlockByNumber` request.
    pub fn mock_get_block_by_number(&mut self, block_number: u64, response: &Value) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .match_body(Matcher::Regex(format!(
                r#""method"\s*:\s*"eth_getBlockByNumber".*"params"\s*:\s*\["0x{block_number:x}""#
            )))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": response
                })
                .to_string(),
            )
            .create();

        self.mocks.push(mock);
        self
    }

    /// Mocks an `eth_getLogs` request.
    pub fn mock_get_logs(&mut self, logs: &[Value]) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .match_body(Matcher::Regex(r#""method"\s*:\s*"eth_getLogs""#.to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": logs
                })
                .to_string(),
            )
            .create();

        self.mocks.push(mock);
        self
    }

    /// Mocks an `eth_getLogs` request with specific from/to blocks.
    pub fn mock_get_logs_for_range(
        &mut self,
        from_block: u64,
        to_block: u64,
        logs: &[Value],
    ) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#""method"\s*:\s*"eth_getLogs""#.to_string()),
                Matcher::Regex(format!(r#""fromBlock"\s*:\s*"0x{from_block:x}""#)),
                Matcher::Regex(format!(r#""toBlock"\s*:\s*"0x{to_block:x}""#)),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": logs
                })
                .to_string(),
            )
            .create();

        self.mocks.push(mock);
        self
    }

    /// Mocks an `eth_blockNumber` request.
    pub fn mock_block_number(&mut self, block_number: u64) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .match_body(Matcher::Regex(r#""method"\s*:\s*"eth_blockNumber""#.to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": format!("0x{:x}", block_number)
                })
                .to_string(),
            )
            .create();

        self.mocks.push(mock);
        self
    }

    /// Mocks an `eth_getBlockReceipts` request.
    pub fn mock_get_block_receipts(&mut self, block_number: u64, receipts: &[Value]) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .match_body(Matcher::Regex(format!(
                r#""method"\s*:\s*"eth_getBlockReceipts".*"params"\s*:\s*\["0x{block_number:x}"\]"#
            )))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": receipts
                })
                .to_string(),
            )
            .create();

        self.mocks.push(mock);
        self
    }

    /// Mocks an RPC error response.
    pub fn mock_rpc_error(&mut self, method: &str, code: i32, message: &str) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .match_body(Matcher::Regex(format!(r#""method"\s*:\s*"{method}""#)))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {
                        "code": code,
                        "message": message
                    }
                })
                .to_string(),
            )
            .create();

        self.mocks.push(mock);
        self
    }

    /// Mocks a timeout by returning a gateway timeout error.
    ///
    /// Note: mockito doesn't support actual delays, so we simulate timeout
    /// with a 504 Gateway Timeout response.
    pub fn mock_timeout(&mut self, method: &str) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .match_body(Matcher::Regex(format!(r#""method"\s*:\s*"{method}""#)))
            .with_status(504)
            .with_body("Gateway Timeout")
            .create();

        self.mocks.push(mock);
        self
    }

    /// Mocks a server error (500).
    pub fn mock_server_error(&mut self) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .with_status(500)
            .with_body("Internal Server Error")
            .create();

        self.mocks.push(mock);
        self
    }

    /// Mocks a generic JSON-RPC method with custom response.
    pub fn mock_method(&mut self, method: &str, result: &Value) -> &mut Self {
        let mock = self
            .server
            .mock("POST", "/")
            .match_body(Matcher::Regex(format!(r#""method"\s*:\s*"{method}""#)))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": result
                })
                .to_string(),
            )
            .create();

        self.mocks.push(mock);
        self
    }

    /// Returns a reference to the underlying mockito server for advanced mocking.
    pub fn get_server(&mut self) -> &mut mockito::ServerGuard {
        &mut self.server
    }

    /// Verifies all mocks were called.
    #[must_use]
    pub fn verify_all_called(&self) -> bool {
        self.mocks.iter().all(mockito::Mock::matched)
    }

    /// Gets the number of times mocks were called.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.mocks.iter().filter(|m| m.matched()).count()
    }
}

/// Builder for constructing block responses.
pub struct BlockResponseBuilder {
    number: u64,
    hash: String,
    parent_hash: String,
    transactions: Vec<Value>,
    timestamp: u64,
}

impl BlockResponseBuilder {
    /// Creates a new block response builder.
    #[must_use]
    pub fn new(number: u64) -> Self {
        Self {
            number,
            hash: format!("0x{number:064x}"),
            parent_hash: format!("0x{:064x}", number.saturating_sub(1)),
            transactions: Vec::new(),
            timestamp: 1_600_000_000 + number,
        }
    }

    /// Sets a custom block hash.
    #[must_use]
    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.hash = hash.into();
        self
    }

    /// Sets a custom parent hash.
    #[must_use]
    pub fn with_parent_hash(mut self, hash: impl Into<String>) -> Self {
        self.parent_hash = hash.into();
        self
    }

    /// Adds transactions to the block.
    #[must_use]
    pub fn with_transactions(mut self, txs: Vec<Value>) -> Self {
        self.transactions = txs;
        self
    }

    /// Sets a custom timestamp.
    #[must_use]
    pub fn with_timestamp(mut self, timestamp: u64) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Builds the block response JSON.
    #[must_use]
    pub fn build(self) -> Value {
        json!({
            "number": format!("0x{:x}", self.number),
            "hash": self.hash,
            "parentHash": self.parent_hash,
            "timestamp": format!("0x{:x}", self.timestamp),
            "transactions": self.transactions,
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
}

/// Builder for constructing log responses.
pub struct LogResponseBuilder {
    address: String,
    block_number: u64,
    block_hash: String,
    log_index: u64,
    transaction_hash: String,
    transaction_index: u64,
    topics: Vec<String>,
    data: String,
}

impl LogResponseBuilder {
    /// Creates a new log response builder.
    #[must_use]
    pub fn new(block_number: u64, log_index: u64) -> Self {
        Self {
            address: "0x0000000000000000000000000000000000000001".to_string(),
            block_number,
            block_hash: format!("0x{block_number:064x}"),
            log_index,
            transaction_hash: format!("0x{:064x}", block_number * 100 + log_index),
            transaction_index: 0,
            topics: vec![format!("0x{:064x}", 0)],
            data: "0x".to_string(),
        }
    }

    /// Sets a custom contract address.
    #[must_use]
    pub fn with_address(mut self, address: impl Into<String>) -> Self {
        self.address = address.into();
        self
    }

    /// Sets topics for the log.
    #[must_use]
    pub fn with_topics(mut self, topics: Vec<String>) -> Self {
        self.topics = topics;
        self
    }

    /// Sets the log data.
    #[must_use]
    pub fn with_data(mut self, data: impl Into<String>) -> Self {
        self.data = data.into();
        self
    }

    /// Sets the transaction hash.
    #[must_use]
    pub fn with_transaction_hash(mut self, hash: impl Into<String>) -> Self {
        self.transaction_hash = hash.into();
        self
    }

    /// Builds the log response JSON.
    #[must_use]
    pub fn build(self) -> Value {
        json!({
            "address": self.address,
            "blockNumber": format!("0x{:x}", self.block_number),
            "blockHash": self.block_hash,
            "logIndex": format!("0x{:x}", self.log_index),
            "transactionHash": self.transaction_hash,
            "transactionIndex": format!("0x{:x}", self.transaction_index),
            "topics": self.topics,
            "data": self.data,
            "removed": false
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rpc_mock_builder_creation() {
        let mock = RpcMockBuilder::new().await;
        assert!(!mock.url().is_empty());
    }

    #[tokio::test]
    async fn test_block_response_builder() {
        let block = BlockResponseBuilder::new(100).build();
        assert_eq!(block["number"], "0x64");
        assert!(block["hash"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_log_response_builder() {
        let log = LogResponseBuilder::new(100, 5).build();
        assert_eq!(log["blockNumber"], "0x64");
        assert_eq!(log["logIndex"], "0x5");
    }

    #[test]
    fn test_block_response_with_custom_hash() {
        let block = BlockResponseBuilder::new(100).with_hash("0xabcdef").build();
        assert_eq!(block["hash"], "0xabcdef");
    }

    #[test]
    fn test_log_response_with_custom_address() {
        let log = LogResponseBuilder::new(100, 0)
            .with_address("0x1234567890abcdef1234567890abcdef12345678")
            .build();
        assert_eq!(log["address"], "0x1234567890abcdef1234567890abcdef12345678");
    }
}
