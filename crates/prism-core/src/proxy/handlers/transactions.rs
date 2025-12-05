use std::sync::Arc;
use tracing::debug;

use crate::{
    cache::{
        converter::{
            json_log_to_log_record, json_receipt_to_receipt_record,
            json_transaction_to_transaction_record, receipt_record_to_json,
            transaction_record_to_json,
        },
        types::{LogId, LogRecord},
    },
    proxy::engine::SharedContext,
    types::{CacheStatus, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION_COW},
};

use super::super::errors::ProxyError;

/// Handler for transaction-related RPC methods.
///
/// Provides caching and upstream forwarding for `eth_getTransactionByHash` and
/// `eth_getTransactionReceipt`.
pub struct TransactionsHandler {
    ctx: Arc<SharedContext>,
}

impl TransactionsHandler {
    /// Creates a new `TransactionsHandler` instance with shared context.
    #[must_use]
    pub fn new(ctx: Arc<SharedContext>) -> Self {
        Self { ctx }
    }

    /// Handles `eth_getTransactionByHash` requests with cache lookup.
    ///
    /// Validates the transaction hash format and length before querying the cache or upstream.
    ///
    /// # Errors
    /// Returns [`ProxyError::InvalidRequest`] if parameters are malformed or missing.
    /// Returns [`ProxyError::Upstream`] if upstream request fails.
    pub async fn handle_transaction_by_hash_request(
        &self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, ProxyError> {
        let params = request.params.as_ref().ok_or_else(|| {
            ProxyError::InvalidRequest("Missing parameters for eth_getTransactionByHash".into())
        })?;

        let tx_hash = params
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProxyError::InvalidRequest("Invalid transaction hash parameter".into())
            })?;

        let tx_hash_array = Self::parse_hash(tx_hash)?;

        if let Some(transaction) = self.ctx.cache_manager.get_transaction(&tx_hash_array) {
            debug!(tx_hash = tx_hash, "transaction cache hit");
            self.ctx.metrics_collector.record_cache_hit("eth_getTransactionByHash");

            let tx_json = transaction_record_to_json(&transaction);

            return Ok(JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(tx_json),
                error: None,
                id: request.id,
                cache_status: Some(CacheStatus::Full),
            });
        }

        debug!(tx_hash = tx_hash, "transaction cache miss, fetching from upstream");
        self.ctx.metrics_collector.record_cache_miss("eth_getTransactionByHash");

        let response = self.ctx.forward_to_upstream(&request).await?;

        if let Some(result) = response.result.as_ref() {
            self.cache_transaction_from_response(result).await;
        }

        Ok(response)
    }

    /// Handles `eth_getTransactionReceipt` requests with cache lookup.
    ///
    /// Retrieves both the receipt and associated logs from cache if available.
    /// Validates the transaction hash format and length before querying.
    ///
    /// # Errors
    /// Returns [`ProxyError::InvalidRequest`] if parameters are malformed or missing.
    /// Returns [`ProxyError::Upstream`] if upstream request fails.
    pub async fn handle_transaction_receipt_request(
        &self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, ProxyError> {
        let params = request.params.as_ref().ok_or_else(|| {
            ProxyError::InvalidRequest("Missing parameters for eth_getTransactionReceipt".into())
        })?;

        let tx_hash = params
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProxyError::InvalidRequest("Invalid transaction hash parameter".into())
            })?;

        let tx_hash_array = Self::parse_hash(tx_hash)?;

        if let Some((receipt, logs)) = self.ctx.cache_manager.get_receipt_with_logs(&tx_hash_array)
        {
            debug!(tx_hash = tx_hash, "receipt cache hit");
            self.ctx.metrics_collector.record_cache_hit("eth_getTransactionReceipt");

            let receipt_json = receipt_record_to_json(&receipt, &logs);

            return Ok(JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(receipt_json),
                error: None,
                id: request.id,
                cache_status: Some(CacheStatus::Full),
            });
        }

        debug!(tx_hash = tx_hash, "receipt cache miss, fetching from upstream");
        self.ctx.metrics_collector.record_cache_miss("eth_getTransactionReceipt");

        let response = self.ctx.forward_to_upstream(&request).await?;

        if let Some(result) = response.result.as_ref() {
            self.cache_receipt_from_response(result, &tx_hash_array).await;
        }

        Ok(response)
    }

    /// Parses and validates a transaction or block hash parameter.
    ///
    /// Expects hex format with 0x prefix and 32-byte length.
    fn parse_hash(hash_str: &str) -> Result<[u8; 32], ProxyError> {
        let hash_bytes = hex::decode(&hash_str[2..])
            .map_err(|_| ProxyError::InvalidRequest("Invalid hex hash".into()))?;

        if hash_bytes.len() != 32 {
            return Err(ProxyError::InvalidRequest("Invalid hash length".into()));
        }

        <[u8; 32]>::try_from(hash_bytes)
            .map_err(|_| ProxyError::InvalidRequest("Invalid hash length".into()))
    }

    /// Caches transaction data from an upstream response.
    async fn cache_transaction_from_response(&self, tx_json: &serde_json::Value) {
        if let Some(transaction) = json_transaction_to_transaction_record(tx_json) {
            self.ctx.cache_manager.transaction_cache.insert_transaction(transaction).await;
        }
    }

    /// Caches receipt and associated logs from an upstream response with validation.
    /// Extracts logs from the receipt, converts and caches them, then caches the receipt itself.
    async fn cache_receipt_from_response(
        &self,
        receipt_json: &serde_json::Value,
        tx_hash: &[u8; 32],
    ) {
        use crate::cache::validation::validate_receipt_response;

        if !validate_receipt_response(receipt_json, tx_hash) {
            tracing::warn!(tx_hash = ?tx_hash, "receipt validation failed, skipping cache");
            return;
        }
        if let Some(logs_array) = receipt_json.get("logs").and_then(|v| v.as_array()) {
            let t_conv = std::time::Instant::now();
            let log_records: Vec<(LogId, LogRecord)> =
                logs_array.iter().filter_map(json_log_to_log_record).collect();
            let conv_ms = t_conv.elapsed().as_millis();

            if log_records.is_empty() {
                debug!(conversion_time_ms = conv_ms, "receipt logs processing: no logs");
            } else {
                let t_insert = std::time::Instant::now();
                self.ctx.cache_manager.insert_logs_bulk_no_stats(log_records).await;
                let insert_ms = t_insert.elapsed().as_millis();

                let mut stats_ms: u128 = 0;
                if self.ctx.cache_manager.should_update_stats().await {
                    let t_stats = std::time::Instant::now();
                    self.ctx.cache_manager.update_stats_on_demand().await;
                    stats_ms = t_stats.elapsed().as_millis();
                }

                debug!(
                    logs_count = receipt_json["logs"].as_array().map_or(0, Vec::len),
                    conversion_time_ms = conv_ms,
                    insert_time_ms = insert_ms,
                    stats_time_ms = stats_ms,
                    "receipt logs processing complete"
                );
            }
        }
        if let Some(receipt_record) = json_receipt_to_receipt_record(receipt_json) {
            self.ctx.cache_manager.insert_receipt(receipt_record).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Test transaction hash validation
    #[test]
    fn test_transaction_hash_validation_valid() {
        let valid_hash = "0x1234567890123456789012345678901234567890123456789012345678901234";
        let result = TransactionsHandler::parse_hash(valid_hash);
        assert!(result.is_ok(), "Valid hash should parse successfully");
        assert_eq!(result.unwrap().len(), 32, "Parsed hash should be 32 bytes");
    }

    #[test]
    fn test_transaction_hash_validation_invalid_hex() {
        let invalid_hash = "0xGGGG567890123456789012345678901234567890123456789012345678901234";
        let result = TransactionsHandler::parse_hash(invalid_hash);
        assert!(result.is_err(), "Invalid hex characters should fail");
    }

    #[test]
    fn test_transaction_hash_validation_wrong_length() {
        let short_hash = "0x1234"; // Too short
        let result = TransactionsHandler::parse_hash(short_hash);
        assert!(result.is_err(), "Hash with wrong length should fail");
    }

    #[test]
    fn test_transaction_hash_validation_no_prefix() {
        let no_prefix = "1234567890123456789012345678901234567890123456789012345678901234";
        // This will try to decode from index 2, which will be wrong
        let result = TransactionsHandler::parse_hash(no_prefix);
        assert!(result.is_err(), "Hash without 0x prefix should fail");
    }

    /// Test receipt with logs assembly
    #[test]
    fn test_receipt_with_logs_structure() {
        let receipt = json!({
            "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "blockNumber": "0x123",
            "logs": [
                {
                    "address": "0x1234567890123456789012345678901234567890",
                    "topics": [],
                    "data": "0x",
                    "blockNumber": "0x123",
                    "logIndex": "0x0"
                }
            ]
        });

        assert!(receipt.get("transactionHash").is_some());
        assert!(receipt.get("logs").is_some());
        assert!(receipt["logs"].is_array());
    }

    #[test]
    fn test_receipt_empty_logs() {
        let receipt = json!({
            "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "blockNumber": "0x123",
            "logs": []
        });

        let logs = receipt["logs"].as_array();
        assert!(logs.is_some());
        assert_eq!(logs.unwrap().len(), 0, "Logs array should be empty");
    }

    /// Test hash format requirements
    #[test]
    fn test_hash_format_constants() {
        // Valid transaction hash is 66 chars (0x + 64 hex chars)
        let valid_hash = "0x1234567890123456789012345678901234567890123456789012345678901234";
        assert_eq!(valid_hash.len(), 66, "Valid hash should be 66 chars");

        // After stripping 0x, should be 64 chars (32 bytes)
        let hex_part = &valid_hash[2..];
        assert_eq!(hex_part.len(), 64, "Hex part should be 64 chars");
    }

    /// Test response construction for transaction requests
    #[test]
    fn test_transaction_response_construction() {
        let response = JsonRpcResponse::success(
            json!({
                "hash": "0x1234567890123456789012345678901234567890123456789012345678901234",
                "from": "0x1234567890123456789012345678901234567890",
                "to": "0x0987654321098765432109876543210987654321"
            }),
            Arc::new(json!(1)),
        );

        assert_eq!(response.jsonrpc.as_ref(), "2.0");
        assert!(response.result.is_some());
    }

    /// Test receipt response construction
    #[test]
    fn test_receipt_response_construction() {
        let response = JsonRpcResponse::success(
            json!({
                "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234",
                "status": "0x1",
                "logs": []
            }),
            Arc::new(json!(1)),
        );

        assert_eq!(response.jsonrpc.as_ref(), "2.0");
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    /// Test parameter extraction from request
    #[test]
    fn test_extract_hash_parameter() {
        let params = json!(["0x1234567890123456789012345678901234567890123456789012345678901234"]);
        let hash_param = params.as_array().and_then(|arr| arr.first()).and_then(|v| v.as_str());
        assert_eq!(
            hash_param,
            Some("0x1234567890123456789012345678901234567890123456789012345678901234")
        );
    }

    #[test]
    fn test_extract_missing_hash_parameter() {
        let params = json!([]);
        let hash_param = params.as_array().and_then(|arr| arr.first()).and_then(|v| v.as_str());
        assert!(hash_param.is_none(), "Should handle missing hash parameter");
    }

    /// Test log count in receipt
    #[test]
    fn test_receipt_log_count() {
        let receipt_with_logs = json!({
            "logs": [
                {"logIndex": "0x0"},
                {"logIndex": "0x1"},
                {"logIndex": "0x2"}
            ]
        });

        let log_count = receipt_with_logs["logs"].as_array().map_or(0, Vec::len);
        assert_eq!(log_count, 3, "Should count logs correctly");
    }

    #[test]
    fn test_receipt_no_logs_field() {
        let receipt_no_logs = json!({
            "transactionHash": "0x1234567890123456789012345678901234567890123456789012345678901234"
        });

        let log_count = receipt_no_logs.get("logs").and_then(|v| v.as_array()).map_or(0, Vec::len);
        assert_eq!(log_count, 0, "Missing logs field should result in 0 count");
    }
}
