//! Centralized block parameter parsing utilities.
//!
//! Provides consistent parsing for block numbers, block tags, and block references
//! across all RPC methods, eliminating duplicate hex parsing logic.

use thiserror::Error;

/// Error types for block parameter parsing
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ParseError {
    #[error("invalid hex string: {0}")]
    InvalidHex(String),
    #[error("invalid number: {0}")]
    InvalidNumber(String),
}

/// Block reference types supported by Ethereum JSON-RPC
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRef {
    /// Specific block number
    Number(u64),
    /// Block tag (latest, earliest, etc.)
    Tag(BlockTag),
}

/// Standard Ethereum block tags
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTag {
    /// The most recent block in the canonical chain
    Latest,
    /// The earliest/genesis block
    Earliest,
    /// A block in the pending state
    Pending,
    /// The most recent safe head block
    Safe,
    /// The most recent finalized block
    Finalized,
}

/// Implement `TryFrom` for idiomatic Rust conversion
impl TryFrom<&str> for BlockRef {
    type Error = ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        BlockParameter::parse(value)
    }
}

/// Centralized block parameter parsing
pub struct BlockParameter;

impl BlockParameter {
    /// Parse a block parameter from a string (JSON-RPC request parameter).
    ///
    /// Handles:
    /// - Hex strings with "0x" prefix (e.g., "0x123")
    /// - Hex strings without prefix (e.g., "123")
    /// - Decimal strings (e.g., "123")
    /// - Block tags (e.g., "latest", "earliest", "safe", "finalized", "pending")
    ///
    /// # Examples
    /// ```
    /// use prism_core::utils::block_param::{BlockParameter, BlockRef, BlockTag};
    ///
    /// assert_eq!(BlockParameter::parse("latest").unwrap(), BlockRef::Tag(BlockTag::Latest));
    /// assert_eq!(BlockParameter::parse("0x10").unwrap(), BlockRef::Number(16));
    /// assert_eq!(BlockParameter::parse("100").unwrap(), BlockRef::Number(100));
    /// ```
    ///
    /// # Errors
    /// Returns `ParseError` if the input is not a valid block parameter.
    pub fn parse(param: &str) -> Result<BlockRef, ParseError> {
        match param {
            "latest" | "pending" => Ok(BlockRef::Tag(BlockTag::Latest)),
            "earliest" => Ok(BlockRef::Tag(BlockTag::Earliest)),
            "safe" => Ok(BlockRef::Tag(BlockTag::Safe)),
            "finalized" => Ok(BlockRef::Tag(BlockTag::Finalized)),
            s => {
                // Try parsing as hex first (with or without 0x prefix)
                if let Some(hex_str) = s.strip_prefix("0x") {
                    u64::from_str_radix(hex_str, 16)
                        .map(BlockRef::Number)
                        .map_err(|_| ParseError::InvalidHex(s.to_string()))
                } else {
                    // Try parsing as decimal number
                    s.parse::<u64>()
                        .map(BlockRef::Number)
                        .map_err(|_| ParseError::InvalidNumber(s.to_string()))
                }
            }
        }
    }

    /// Parse a block parameter and return only the block number if it's a specific number.
    ///
    /// Block tags (latest, earliest, etc.) return `None` since they don't represent
    /// a specific block number.
    ///
    /// # Examples
    /// ```
    /// use prism_core::utils::block_param::BlockParameter;
    ///
    /// assert_eq!(BlockParameter::parse_number("0x10"), Some(16));
    /// assert_eq!(BlockParameter::parse_number("latest"), None);
    /// ```
    #[must_use]
    pub fn parse_number(param: &str) -> Option<u64> {
        match Self::parse(param) {
            Ok(BlockRef::Number(n)) => Some(n),
            _ => None,
        }
    }

    /// Extract block number from a JSON value (typically from a response).
    ///
    /// Used for parsing block numbers from JSON-RPC responses where the value
    /// is expected to be a hex-encoded string.
    ///
    /// # Examples
    /// ```
    /// use prism_core::utils::block_param::BlockParameter;
    /// use serde_json::json;
    ///
    /// let value = json!("0xff");
    /// assert_eq!(BlockParameter::from_json_value(&value), Some(255));
    /// ```
    #[must_use]
    pub fn from_json_value(value: &serde_json::Value) -> Option<u64> {
        value.as_str().and_then(Self::parse_hex)
    }

    /// Parse a hex string to u64 (with or without 0x prefix).
    ///
    /// This is a lower-level function for when you know the input is hex.
    /// For general block parameter parsing, use `parse()` instead.
    ///
    /// # Examples
    /// ```
    /// use prism_core::utils::block_param::BlockParameter;
    ///
    /// assert_eq!(BlockParameter::parse_hex("0xff"), Some(255));
    /// assert_eq!(BlockParameter::parse_hex("ff"), Some(255));
    /// assert_eq!(BlockParameter::parse_hex("invalid"), None);
    /// ```
    #[must_use]
    pub fn parse_hex(s: &str) -> Option<u64> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        u64::from_str_radix(s, 16).ok()
    }

    /// Parse a hex string to u32 (with or without 0x prefix).
    ///
    /// Used for parsing smaller numeric values like log indices.
    ///
    /// # Examples
    /// ```
    /// use prism_core::utils::block_param::BlockParameter;
    ///
    /// assert_eq!(BlockParameter::parse_hex_u32("0xff"), Some(255));
    /// assert_eq!(BlockParameter::parse_hex_u32("ff"), Some(255));
    /// ```
    #[must_use]
    pub fn parse_hex_u32(s: &str) -> Option<u32> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        u32::from_str_radix(s, 16).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_block_tags() {
        assert_eq!(BlockParameter::parse("latest").unwrap(), BlockRef::Tag(BlockTag::Latest));
        assert_eq!(BlockParameter::parse("earliest").unwrap(), BlockRef::Tag(BlockTag::Earliest));
        assert_eq!(BlockParameter::parse("pending").unwrap(), BlockRef::Tag(BlockTag::Latest));
        assert_eq!(BlockParameter::parse("safe").unwrap(), BlockRef::Tag(BlockTag::Safe));
        assert_eq!(BlockParameter::parse("finalized").unwrap(), BlockRef::Tag(BlockTag::Finalized));
    }

    #[test]
    fn test_parse_hex_numbers() {
        assert_eq!(BlockParameter::parse("0x0").unwrap(), BlockRef::Number(0));
        assert_eq!(BlockParameter::parse("0x10").unwrap(), BlockRef::Number(16));
        assert_eq!(BlockParameter::parse("0xff").unwrap(), BlockRef::Number(255));
        assert_eq!(BlockParameter::parse("0x3e8").unwrap(), BlockRef::Number(1000));
    }

    #[test]
    fn test_parse_decimal_numbers() {
        assert_eq!(BlockParameter::parse("0").unwrap(), BlockRef::Number(0));
        assert_eq!(BlockParameter::parse("100").unwrap(), BlockRef::Number(100));
        assert_eq!(BlockParameter::parse("1000").unwrap(), BlockRef::Number(1000));
    }

    #[test]
    fn test_parse_number() {
        assert_eq!(BlockParameter::parse_number("0x10"), Some(16));
        assert_eq!(BlockParameter::parse_number("100"), Some(100));
        assert_eq!(BlockParameter::parse_number("latest"), None);
        assert_eq!(BlockParameter::parse_number("finalized"), None);
    }

    #[test]
    fn test_parse_hex_standalone() {
        assert_eq!(BlockParameter::parse_hex("0xff"), Some(255));
        assert_eq!(BlockParameter::parse_hex("ff"), Some(255));
        assert_eq!(BlockParameter::parse_hex("0x0"), Some(0));
        assert_eq!(BlockParameter::parse_hex("invalid"), None);
    }

    #[test]
    fn test_parse_hex_u32() {
        assert_eq!(BlockParameter::parse_hex_u32("0xff"), Some(255));
        assert_eq!(BlockParameter::parse_hex_u32("ff"), Some(255));
        assert_eq!(BlockParameter::parse_hex_u32("0x0"), Some(0));
        assert_eq!(BlockParameter::parse_hex_u32("invalid"), None);
    }

    #[test]
    fn test_from_json_value() {
        use serde_json::json;

        assert_eq!(BlockParameter::from_json_value(&json!("0xff")), Some(255));
        assert_eq!(BlockParameter::from_json_value(&json!("0x3e8")), Some(1000));
        assert_eq!(BlockParameter::from_json_value(&json!("invalid")), None);
        assert_eq!(BlockParameter::from_json_value(&json!(123)), None); // Not a string
    }

    #[test]
    fn test_invalid_inputs() {
        assert!(BlockParameter::parse("invalid").is_err());
        assert!(BlockParameter::parse("0xinvalid").is_err());
        assert!(BlockParameter::parse("").is_err());
    }
}
