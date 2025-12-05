//! Consensus response types and metadata.

use crate::types::JsonRpcResponse;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Classification of response types for consensus logic.
///
/// This enables intelligent preference logic where `NonEmpty` responses (actual data)
/// can be preferred over `Empty` responses (null results) and `ConsensusError`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResponseType {
    /// Successful response with meaningful (non-null, non-empty) result data
    NonEmpty,
    /// Successful response with null or empty result (e.g., null for non-existent tx)
    Empty,
    /// JSON-RPC error from the upstream (e.g., "execution reverted", -32000 errors)
    ConsensusError,
    /// Transport/infrastructure failure (timeout, HTTP error, connection error)
    InfraError,
}

impl ResponseType {
    /// Determines the response type from a `JsonRpcResponse`.
    #[must_use]
    pub fn from_response(response: &JsonRpcResponse) -> Self {
        if response.error.is_some() {
            ResponseType::ConsensusError
        } else if let Some(ref result) = response.result {
            if Self::is_empty_result(result) {
                ResponseType::Empty
            } else {
                ResponseType::NonEmpty
            }
        } else {
            ResponseType::Empty
        }
    }

    /// Checks if a result value should be considered "empty".
    ///
    /// Empty values include:
    /// - `null`
    /// - Empty arrays `[]`
    /// - Empty objects `{}`
    /// - Empty strings or `"0x"` (common in Ethereum for "no data")
    #[must_use]
    fn is_empty_result(result: &serde_json::Value) -> bool {
        match result {
            serde_json::Value::Null => true,
            serde_json::Value::Array(arr) => arr.is_empty(),
            serde_json::Value::Object(obj) => obj.is_empty(),
            serde_json::Value::String(s) => s.is_empty() || s == "0x",
            _ => false,
        }
    }

    /// Returns the priority order for preference-based selection.
    /// Lower values are higher priority.
    #[must_use]
    pub fn priority_order(self) -> u8 {
        match self {
            ResponseType::NonEmpty => 0,
            ResponseType::Empty => 1,
            ResponseType::ConsensusError => 2,
            ResponseType::InfraError => 3,
        }
    }
}

/// Result of a consensus query.
#[derive(Debug, Clone)]
pub struct ConsensusResult {
    pub response: JsonRpcResponse,
    pub agreement_count: usize,
    pub total_queried: usize,
    pub consensus_achieved: bool,
    pub disagreeing_upstreams: Vec<Arc<str>>,
    pub metadata: ConsensusMetadata,
    /// The response type of the consensus winner.
    /// Allows callers to distinguish between:
    /// - `Empty`: Legitimate "not found" result (e.g., tx doesn't exist)
    /// - `ConsensusError`: JSON-RPC error from upstream
    /// - `NonEmpty`: Actual data returned
    pub response_type: ResponseType,
}

/// Consensus process metadata.
#[derive(Debug, Clone)]
pub struct ConsensusMetadata {
    pub selection_method: SelectionMethod,
    pub response_groups: Vec<ResponseGroup>,
    pub duration_ms: u64,
}

/// Response selection method.
#[derive(Debug, Clone, Copy)]
pub enum SelectionMethod {
    /// Consensus achieved through vote count meeting threshold
    Consensus,
    /// Selected block head leader (highest block number)
    BlockHeadLeader,
    /// Selected upstream with highest score
    HighestScore,
    /// Selected first valid response (fallback)
    FirstValid,
    /// Fallback error response
    FallbackError,
    /// Selected due to `prefer_non_empty` preference
    PreferNonEmpty,
}

/// Group of identical responses with type classification.
#[derive(Debug, Clone)]
pub struct ResponseGroup {
    /// Hash of the response for equality comparison.
    pub response_hash: u64,
    /// Names of upstreams that returned this response.
    pub upstreams: Vec<Arc<str>>,
    /// Number of upstreams returning this response.
    pub count: usize,
    /// The actual response.
    pub response: JsonRpcResponse,
    /// The classified type of this response (`NonEmpty`, `Empty`, `ConsensusError`, `InfraError`).
    pub response_type: ResponseType,
    /// Size of the response body in bytes (for `prefer_larger_responses` config).
    pub response_size: usize,
}
