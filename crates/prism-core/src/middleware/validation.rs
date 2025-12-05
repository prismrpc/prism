use crate::types::{is_method_allowed, JsonRpcRequest};

impl JsonRpcRequest {
    /// Validates a JSON-RPC request for correctness and security.
    ///
    /// Checks version, method name format, allowed methods, and method-specific
    /// parameter constraints like block ranges and topic counts.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] if the request fails validation checks:
    /// - [`ValidationError::InvalidVersion`] if not JSON-RPC 2.0
    /// - [`ValidationError::InvalidMethod`] if method contains invalid characters
    /// - [`ValidationError::MethodNotAllowed`] if method is not in allowlist
    /// - Method-specific validation errors for parameter constraints
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.jsonrpc != "2.0" {
            return Err(ValidationError::InvalidVersion(self.jsonrpc.to_string()));
        }

        if !self.method.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(ValidationError::InvalidMethod(self.method.clone()));
        }

        if !is_method_allowed(&self.method) {
            return Err(ValidationError::MethodNotAllowed(self.method.clone()));
        }

        if let Some(params) = &self.params {
            self.validate_params_for_method(params)?;
        }

        Ok(())
    }

    /// Validates parameters for specific JSON-RPC methods.
    ///
    /// Performs method-specific validation including block range limits
    /// for `eth_getLogs` and block parameter format for `eth_getBlockByNumber`.
    fn validate_params_for_method(
        &self,
        params: &serde_json::Value,
    ) -> Result<(), ValidationError> {
        match self.method.as_str() {
            "eth_getLogs" => {
                if let Some(filter) = params.as_array().and_then(|a| a.first()) {
                    Self::validate_log_filter(filter)?;
                }
            }
            "eth_getBlockByNumber" => {
                if let Some(array) = params.as_array() {
                    if let Some(block_param) = array.first() {
                        Self::validate_block_parameter(block_param)?;
                    }
                }
            }

            _ => {}
        }
        Ok(())
    }

    /// Validates `eth_getLogs` filter parameters.
    ///
    /// Enforces a maximum block range of 10,000 blocks and limits topics to 4 entries
    /// to prevent resource exhaustion attacks.
    fn validate_log_filter(filter: &serde_json::Value) -> Result<(), ValidationError> {
        if let Some(obj) = filter.as_object() {
            if let (Some(from), Some(to)) = (obj.get("fromBlock"), obj.get("toBlock")) {
                const MAX_BLOCK_RANGE: u64 = 10000;

                let from_block = Self::parse_block_number(from)?;
                let to_block = Self::parse_block_number(to)?;

                if to_block - from_block > MAX_BLOCK_RANGE {
                    return Err(ValidationError::BlockRangeTooLarge(to_block - from_block));
                }
            }

            if let Some(topics) = obj.get("topics") {
                if let Some(topics_array) = topics.as_array() {
                    if topics_array.len() > 4 {
                        return Err(ValidationError::TooManyTopics(topics_array.len()));
                    }
                }
            }
        }
        Ok(())
    }

    /// Validates block parameter format for methods like `eth_getBlockByNumber`.
    ///
    /// Accepts standard block tags (latest, earliest, pending, safe, finalized)
    /// or hex-encoded block numbers with 0x prefix. Hex values are limited to 66
    /// characters to prevent oversized inputs.
    fn validate_block_parameter(param: &serde_json::Value) -> Result<(), ValidationError> {
        if let Some(block_str) = param.as_str() {
            const VALID_TAGS: &[&str] = &["latest", "earliest", "pending", "safe", "finalized"];

            if !VALID_TAGS.contains(&block_str) && !block_str.starts_with("0x") {
                return Err(ValidationError::InvalidBlockParameter(block_str.to_string()));
            }

            if let Some(block_str) = block_str.strip_prefix("0x") {
                if block_str.len() > 66 {
                    return Err(ValidationError::InvalidBlockParameter(block_str.to_string()));
                }

                if !block_str.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(ValidationError::InvalidBlockParameter(block_str.to_string()));
                }
            }
        }
        Ok(())
    }

    /// Parses a block number from JSON-RPC parameter format.
    ///
    /// Supports hex strings (0x prefix), decimal strings, and block tags.
    /// Block tags "latest" and "pending" map to `u64::MAX`, "earliest" maps to 0.
    fn parse_block_number(value: &serde_json::Value) -> Result<u64, ValidationError> {
        if let Some(s) = value.as_str() {
            if s == "latest" || s == "pending" {
                return Ok(u64::MAX);
            }
            if s == "earliest" {
                return Ok(0);
            }
            if let Some(s) = s.strip_prefix("0x") {
                u64::from_str_radix(s, 16)
                    .map_err(|_| ValidationError::InvalidBlockParameter(s.to_string()))
            } else {
                s.parse().map_err(|_| ValidationError::InvalidBlockParameter(s.to_string()))
            }
        } else {
            Err(ValidationError::InvalidBlockParameter(format!("{value:?}")))
        }
    }
}

/// Errors that occur during JSON-RPC request validation.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    /// JSON-RPC version is not "2.0".
    #[error("Invalid JSON-RPC version: {0}")]
    InvalidVersion(String),

    /// Method name contains invalid characters (only alphanumeric and underscore allowed).
    #[error("Invalid method name: {0}")]
    InvalidMethod(String),

    /// Requested method is not in the allowlist.
    #[error("Method not allowed: {0}")]
    MethodNotAllowed(String),

    /// Block parameter is not a valid block tag or hex number.
    #[error("Invalid block parameter: {0}")]
    InvalidBlockParameter(String),

    /// Block range exceeds the maximum allowed range of 10,000 blocks.
    #[error("Block range too large: {0} blocks")]
    BlockRangeTooLarge(u64),

    /// More than 4 topics specified in log filter (Ethereum limit is 4).
    #[error("Too many topics: {0}")]
    TooManyTopics(usize),

    /// Block hash format is invalid.
    #[error("Invalid block hash")]
    InvalidBlockHash,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JsonRpcRequest;
    use std::sync::Arc;

    /// Helper to create a test request
    fn test_req(method: &str, params: Option<serde_json::Value>) -> JsonRpcRequest {
        JsonRpcRequest::new(method, params, serde_json::json!(1))
    }

    /// Helper to create a test request with custom jsonrpc version
    fn test_req_with_version(
        jsonrpc: &str,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: std::borrow::Cow::Owned(jsonrpc.to_string()),
            method: method.to_string(),
            params,
            id: Arc::new(serde_json::json!(1)),
        }
    }

    #[tokio::test]
    async fn test_request_validation_valid() {
        let request = test_req("eth_getBlockByNumber", Some(serde_json::json!(["0x1234", false])));
        assert!(request.validate().is_ok());
    }

    #[tokio::test]
    async fn test_request_validation_invalid_version() {
        let request = test_req_with_version(
            "1.0",
            "eth_getBlockByNumber",
            Some(serde_json::json!(["0x1234", false])),
        );

        let result = request.validate();
        assert!(result.is_err());
        if let Err(ValidationError::InvalidVersion(version)) = result {
            assert_eq!(version, "1.0");
        } else {
            panic!("Expected InvalidVersion error");
        }
    }

    #[tokio::test]
    async fn test_request_validation_unsupported_method() {
        let request = test_req("eth_unsupported", Some(serde_json::json!(["0x1234", false])));

        let result = request.validate();
        assert!(result.is_err());
        if let Err(ValidationError::MethodNotAllowed(method)) = result {
            assert_eq!(method, "eth_unsupported");
        } else {
            panic!("Expected MethodNotAllowed error");
        }
    }

    #[tokio::test]
    async fn test_request_validation_logs_filter() {
        let request = test_req(
            "eth_getLogs",
            Some(serde_json::json!([{
                "fromBlock": "0x64",
                "toBlock": "0x6e",
                "topics": ["0x12345678"]
            }])),
        );
        assert!(request.validate().is_ok());
    }

    #[tokio::test]
    async fn test_request_validation_logs_filter_large_range() {
        // Block range: 0x64 (100) to 0x2775 (10101) = 10001 blocks (exceeds 10000 limit)
        let request = test_req(
            "eth_getLogs",
            Some(serde_json::json!([{
                "fromBlock": "0x64",
                "toBlock": "0x2775",
                "topics": ["0x12345678"]
            }])),
        );

        let result = request.validate();
        assert!(result.is_err());
        if let Err(ValidationError::BlockRangeTooLarge(range)) = result {
            assert_eq!(range, 10001, "Expected 10001 block range");
        } else {
            panic!("Expected BlockRangeTooLarge error, got: {result:?}");
        }
    }

    #[tokio::test]
    async fn test_request_validation_logs_filter_valid_range() {
        let request = test_req(
            "eth_getLogs",
            Some(serde_json::json!([{
                "fromBlock": "0x64",
                "toBlock": "0x2710",
                "topics": ["0x12345678"]
            }])),
        );

        let result = request.validate();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_request_validation_too_many_topics() {
        let request = test_req(
            "eth_getLogs",
            Some(serde_json::json!([{
                "fromBlock": "0x64",
                "toBlock": "0x6e",
                "topics": ["0x1", "0x2", "0x3", "0x4", "0x5"]
            }])),
        );

        let result = request.validate();
        assert!(result.is_err());
        if let Err(ValidationError::TooManyTopics(count)) = result {
            assert_eq!(count, 5);
        } else {
            panic!("Expected TooManyTopics error");
        }
    }

    #[tokio::test]
    async fn test_request_validation_invalid_block_parameter() {
        let request =
            test_req("eth_getBlockByNumber", Some(serde_json::json!(["invalid_block", false])));

        let result = request.validate();
        assert!(result.is_err());
        if let Err(ValidationError::InvalidBlockParameter(param)) = result {
            assert_eq!(param, "invalid_block");
        } else {
            panic!("Expected InvalidBlockParameter error");
        }
    }

    #[tokio::test]
    async fn test_request_validation_valid_block_tags() {
        let valid_tags = ["latest", "earliest", "pending", "safe", "finalized"];

        for tag in valid_tags {
            let request = test_req("eth_getBlockByNumber", Some(serde_json::json!([tag, false])));
            assert!(request.validate().is_ok(), "Tag {tag} should be valid");
        }
    }

    #[tokio::test]
    async fn test_request_validation_hex_block_numbers() {
        let valid_hexes = ["0x0", "0x1234", "0xabcdef", "0x1234567890abcdef"];

        for hex in valid_hexes {
            let request = test_req("eth_getBlockByNumber", Some(serde_json::json!([hex, false])));
            assert!(request.validate().is_ok(), "Hex {hex} should be valid");
        }
    }
}
