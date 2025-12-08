use std::sync::Arc;
use tracing::debug;

use crate::{
    cache::converter::block_header_and_body_to_json,
    proxy::engine::SharedContext,
    types::{CacheStatus, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION_COW},
    utils::BlockParameter,
};

use super::super::errors::ProxyError;

/// Handler for block-related RPC methods.
///
/// Provides caching and upstream forwarding for `eth_getBlockByNumber` and `eth_getBlockByHash`.
pub struct BlocksHandler {
    ctx: Arc<SharedContext>,
}

impl BlocksHandler {
    /// Creates a new `BlocksHandler` instance with shared context.
    #[must_use]
    pub fn new(ctx: Arc<SharedContext>) -> Self {
        Self { ctx }
    }

    /// Handles `eth_getBlockByNumber` requests with cache lookup.
    ///
    /// Special tags (latest, earliest, pending, safe, finalized) bypass cache and go directly to
    /// upstream. Numeric block numbers are checked against the cache first, then forwarded if
    /// not found.
    ///
    /// # Errors
    /// Returns [`ProxyError::InvalidRequest`] if parameters are malformed or missing.
    /// Returns [`ProxyError::Upstream`] if upstream request fails.
    pub async fn handle_block_by_number_request(
        &self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, ProxyError> {
        const SPECIAL_TAGS: &[&str] = &["latest", "earliest", "pending", "safe", "finalized"];

        let params = request.params.as_ref().ok_or_else(|| {
            ProxyError::InvalidRequest("Missing parameters for eth_getBlockByNumber".into())
        })?;

        let block_param = params
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProxyError::InvalidRequest("Invalid block parameter".into()))?;

        if SPECIAL_TAGS.contains(&block_param) {
            debug!(block_tag = block_param, "forwarding special block tag to upstream");
            return self.ctx.forward_to_upstream(&request).await;
        }

        let block_number = BlockParameter::parse_number(block_param)
            .ok_or_else(|| ProxyError::InvalidRequest("Invalid block number".into()))?;

        if let Some((header, body)) = self.ctx.cache_manager.get_block_by_number(block_number) {
            debug!(block_number = block_number, "block cache hit");
            self.ctx.metrics_collector.record_cache_hit("eth_getBlockByNumber");

            let block_json = block_header_and_body_to_json(&header, &body);

            return Ok(JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(block_json),
                error: None,
                id: request.id,
                cache_status: Some(CacheStatus::Full),
                serving_upstream: None,
            });
        }

        debug!(block_number = block_number, "block cache miss, fetching from upstream");
        self.ctx.metrics_collector.record_cache_miss("eth_getBlockByNumber");

        let response = self.ctx.forward_to_upstream(&request).await?;

        if let Some(result) = response.result.as_ref() {
            self.cache_block_from_response(result).await;
        }

        Ok(response)
    }

    /// Handles `eth_getBlockByHash` requests with cache lookup.
    ///
    /// Validates the hash format and length before querying the cache or upstream.
    ///
    /// # Errors
    /// Returns [`ProxyError::InvalidRequest`] if parameters are malformed or missing.
    /// Returns [`ProxyError::Upstream`] if upstream request fails.
    pub async fn handle_block_by_hash_request(
        &self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, ProxyError> {
        let params = request.params.as_ref().ok_or_else(|| {
            ProxyError::InvalidRequest("Missing parameters for eth_getBlockByHash".into())
        })?;

        let block_hash = params
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProxyError::InvalidRequest("Invalid block hash parameter".into()))?;

        let block_hash_bytes = hex::decode(&block_hash[2..])
            .map_err(|_| ProxyError::InvalidRequest("Invalid hex block hash".into()))?;

        if block_hash_bytes.len() != 32 {
            return Err(ProxyError::InvalidRequest("Invalid block hash length".into()));
        }

        let block_hash_array: [u8; 32] = <[u8; 32]>::try_from(block_hash_bytes)
            .map_err(|_| ProxyError::InvalidRequest("Invalid block hash length".into()))?;

        if let Some((header, body)) = self.ctx.cache_manager.get_block_by_hash(&block_hash_array) {
            debug!(block_hash = block_hash, "block cache hit");
            self.ctx.metrics_collector.record_cache_hit("eth_getBlockByHash");

            let block_json = block_header_and_body_to_json(&header, &body);

            return Ok(JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(block_json),
                error: None,
                id: request.id,
                cache_status: Some(CacheStatus::Full),
                serving_upstream: None,
            });
        }

        debug!(block_hash = block_hash, "block cache miss, fetching from upstream");
        self.ctx.metrics_collector.record_cache_miss("eth_getBlockByHash");

        let response = self.ctx.forward_to_upstream(&request).await?;

        if let Some(result) = response.result.as_ref() {
            self.cache_block_from_response(result).await;
        }

        Ok(response)
    }

    /// Caches block data from an upstream response with validation.
    ///
    /// Validates block data integrity before caching to prevent cache poisoning.
    /// Converts JSON block data to header and body records, validates integrity,
    /// then stores them in cache only if validation passes.
    ///
    /// If the block contains full transaction objects (fullTransactions=true),
    /// also extracts and caches individual transactions.
    async fn cache_block_from_response(&self, block_json: &serde_json::Value) {
        use crate::cache::{
            converter::{
                json_block_to_block_body, json_block_to_block_header,
                json_transaction_to_transaction_record,
            },
            validation::validate_block_response,
        };

        if !validate_block_response(block_json) {
            tracing::warn!("block validation failed, skipping cache");
            return;
        }

        if let Some(header) = json_block_to_block_header(block_json) {
            let block_number = header.number;

            if let Some(body) = json_block_to_block_body(block_json) {
                self.ctx.cache_manager.insert_header(header).await;
                self.ctx.cache_manager.insert_body(body).await;

                // Extract and cache individual transactions if present as objects
                if let Some(transactions) =
                    block_json.get("transactions").and_then(|v| v.as_array())
                {
                    // Check if transactions are full objects (not just hashes)
                    if let Some(first_tx) = transactions.first() {
                        if first_tx.is_object() {
                            // Full transaction objects - cache them individually
                            let mut cached_count = 0;
                            for tx_json in transactions {
                                if let Some(tx) = json_transaction_to_transaction_record(tx_json) {
                                    self.ctx
                                        .cache_manager
                                        .transaction_cache
                                        .insert_transaction(tx)
                                        .await;
                                    cached_count += 1;
                                }
                            }
                            if cached_count > 0 {
                                tracing::debug!(
                                    block_number = block_number,
                                    transaction_count = cached_count,
                                    "cached transactions from block"
                                );
                            }
                        }
                    }
                }
            } else {
                tracing::warn!("failed to convert json block to block body, skipping body cache");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Test parsing block parameter with hex format
    #[test]
    fn test_parse_block_parameter_hex() {
        // "0x123" → 291
        let hex_block = "0x123";
        let result = BlockParameter::parse_number(hex_block);
        assert_eq!(result, Some(0x123), "Should parse hex block number correctly");
    }

    #[test]
    fn test_parse_block_parameter_decimal() {
        // "123" → 123 (decimal parsing is supported)
        let decimal_block = "123";
        let result = BlockParameter::parse_number(decimal_block);
        assert_eq!(result, Some(123), "Should parse decimal block numbers");
    }

    #[test]
    fn test_parse_block_parameter_tags() {
        // Special tags like "latest", "earliest", "safe", "finalized", "pending"
        // These should NOT be parsed as numbers
        let tags = ["latest", "earliest", "safe", "finalized", "pending"];
        for tag in &tags {
            let result = BlockParameter::parse_number(tag);
            assert!(result.is_none(), "Tag '{tag}' should not be parsed as number");
        }
    }

    #[test]
    fn test_parse_block_parameter_zero() {
        let zero_block = "0x0";
        let result = BlockParameter::parse_number(zero_block);
        assert_eq!(result, Some(0), "Should parse 0x0 as block 0");
    }

    #[test]
    fn test_parse_block_parameter_large_number() {
        let large_block = "0xffffff"; // 16777215
        let result = BlockParameter::parse_number(large_block);
        assert_eq!(result, Some(0x00ff_ffff), "Should parse large hex numbers");
    }

    /// Test block hash validation
    #[test]
    fn test_block_hash_validation_length() {
        // Valid hash is 66 chars (0x + 64 hex chars)
        let valid_hash = "0x1234567890123456789012345678901234567890123456789012345678901234";
        assert_eq!(valid_hash.len(), 66, "Valid block hash should be 66 chars");

        // After stripping 0x, should be 64 chars (32 bytes)
        let hex_part = &valid_hash[2..];
        assert_eq!(hex_part.len(), 64, "Hex part should be 64 chars");
    }

    #[test]
    fn test_block_hash_hex_decode() {
        let valid_hash = "0x1234567890123456789012345678901234567890123456789012345678901234";
        let hex_part = &valid_hash[2..];
        let result = hex::decode(hex_part);
        assert!(result.is_ok(), "Valid hex should decode successfully");
        assert_eq!(result.unwrap().len(), 32, "Decoded hash should be 32 bytes");
    }

    #[test]
    fn test_block_hash_invalid_hex() {
        let invalid_hash = "0xGGGG567890123456789012345678901234567890123456789012345678901234";
        let hex_part = &invalid_hash[2..];
        let result = hex::decode(hex_part);
        assert!(result.is_err(), "Invalid hex characters should fail to decode");
    }

    #[test]
    fn test_block_hash_wrong_length() {
        let short_hash = "0x1234"; // Too short
        let hex_part = &short_hash[2..];
        let result = hex::decode(hex_part);
        assert!(result.is_ok(), "Decoding succeeds but length is wrong");
        assert_ne!(result.unwrap().len(), 32, "Length should not be 32 bytes");
    }

    /// Test special tag constants
    #[test]
    fn test_special_tags_constant() {
        const SPECIAL_TAGS: &[&str] = &["latest", "earliest", "pending", "safe", "finalized"];
        assert_eq!(SPECIAL_TAGS.len(), 5, "Should have 5 special tags");
        assert!(SPECIAL_TAGS.contains(&"latest"));
        assert!(SPECIAL_TAGS.contains(&"finalized"));
        assert!(!SPECIAL_TAGS.contains(&"invalid"));
    }

    /// Test block response assembly (conceptual test)
    #[test]
    fn test_block_response_structure() {
        // A valid block response should have these fields
        let block = json!({
            "number": "0x123",
            "hash": "0x1234567890123456789012345678901234567890123456789012345678901234",
            "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "0x5f5e100",
            "transactions": []
        });

        assert!(block.get("number").is_some());
        assert!(block.get("hash").is_some());
        assert!(block.get("parentHash").is_some());
    }

    /// Test response construction for block requests
    #[test]
    fn test_block_response_construction() {
        let response = JsonRpcResponse::success(
            json!({
                "number": "0x1",
                "hash": "0x1234567890123456789012345678901234567890123456789012345678901234"
            }),
            Arc::new(json!(1)),
        );

        assert_eq!(response.jsonrpc.as_ref(), "2.0");
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    /// Test parameter extraction
    #[test]
    fn test_extract_block_parameter_from_params() {
        let params = json!(["0x123", true]);
        let block_param = params.as_array().and_then(|arr| arr.first()).and_then(|v| v.as_str());
        assert_eq!(block_param, Some("0x123"));
    }

    #[test]
    fn test_extract_missing_parameter() {
        let params = json!([]);
        let block_param = params.as_array().and_then(|arr| arr.first()).and_then(|v| v.as_str());
        assert!(block_param.is_none(), "Should handle missing parameters");
    }
}
