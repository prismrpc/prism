//! Quorum-based consensus determination.
//!
//! This module contains the core consensus algorithm for determining agreement
//! among responses from multiple upstreams. All functions are stateless and
//! operate on response data.
//!
//! # Key Functions
//!
//! - [`find_consensus`]: Main consensus determination algorithm
//! - [`group_responses`]: Groups responses by hash value
//! - [`apply_preferences`]: Applies preference-based sorting to groups
//!
//! # Performance Characteristics
//!
//! These functions are on the hot path for every consensus query. Key optimizations:
//! - `group_responses`: O(n) hashing, O(1) `HashMap` lookups
//! - `apply_preferences`: In-place O(n log n) sort
//! - `estimate_json_size`: ~10x faster than serialization

use super::{
    config::{ConsensusConfig, DisputeBehavior},
    types::{ConsensusMetadata, ConsensusResult, ResponseGroup, ResponseType, SelectionMethod},
};
use crate::{types::JsonRpcResponse, upstream::errors::UpstreamError};
use std::{collections::HashMap, sync::Arc};

/// Finds consensus among successful responses.
///
/// Groups responses by hash, applies preference-based sorting, and determines
/// whether consensus is achieved based on configuration thresholds.
///
/// # Arguments
/// * `responses` - Pairs of upstream names and their responses
/// * `config` - Consensus configuration (thresholds, preferences, etc.)
/// * `method` - RPC method name for looking up per-method `ignore_fields`
///
/// # Returns
/// A `ConsensusResult` indicating whether consensus was achieved and the
/// winning response, or an error if consensus cannot be determined.
///
/// # Errors
/// Returns `UpstreamError::NoHealthyUpstreams` if responses is empty,
/// or `UpstreamError::ConsensusFailure` if consensus cannot be reached
/// and `dispute_behavior` is `ReturnError`.
pub fn find_consensus(
    responses: Vec<(Arc<str>, JsonRpcResponse)>,
    config: &ConsensusConfig,
    method: &str,
) -> Result<ConsensusResult, UpstreamError> {
    if responses.is_empty() {
        return Err(UpstreamError::NoHealthyUpstreams);
    }

    // Look up ignore_fields for this method
    let ignore_paths = config.ignore_fields.get(method).map(Vec::as_slice);
    let groups = group_responses(responses, ignore_paths);
    let mut sorted_groups = groups;

    // Apply preference-based sorting (NonEmpty preference, larger responses, then count)
    apply_preferences(&mut sorted_groups, config);

    let total_queried = sorted_groups.iter().map(|g| g.count).sum();

    // Extract values from best_group before moving sorted_groups
    // Use .first() to safely handle edge case where grouping returns empty
    let Some(best_group) = sorted_groups.first() else {
        return Err(UpstreamError::NoHealthyUpstreams);
    };
    let best_response = best_group.response.clone();
    let best_count = best_group.count;
    let best_response_type = best_group.response_type;

    // Determine if consensus is achieved based on config
    // With prefer_non_empty_always: NonEmpty wins regardless of count
    // Without: best group must meet min_count threshold
    let has_consensus = if config.prefer_non_empty && config.prefer_non_empty_always {
        // NonEmpty always wins if available (aggressive mode)
        best_response_type == ResponseType::NonEmpty || best_count >= config.min_count
    } else {
        // Standard mode: must meet threshold
        best_count >= config.min_count
    };

    if has_consensus {
        let disagreeing: Vec<Arc<str>> = sorted_groups
            .iter()
            .skip(1)
            .flat_map(|g| g.upstreams.iter().map(Arc::clone))
            .collect();

        // Determine selection method based on how consensus was achieved
        let selection_method = if config.prefer_non_empty &&
            best_response_type == ResponseType::NonEmpty &&
            best_count < config.min_count
        {
            SelectionMethod::PreferNonEmpty
        } else {
            SelectionMethod::Consensus
        };

        Ok(ConsensusResult {
            response: best_response,
            agreement_count: best_count,
            total_queried,
            consensus_achieved: true,
            disagreeing_upstreams: disagreeing,
            metadata: ConsensusMetadata {
                selection_method,
                response_groups: sorted_groups,
                duration_ms: 0,
            },
            response_type: best_response_type,
        })
    } else {
        match config.dispute_behavior {
            DisputeBehavior::ReturnError => Err(UpstreamError::ConsensusFailure(format!(
                "Consensus not reached: best group has {} responses, {} required",
                best_count, config.min_count
            ))),
            // All other behaviors return the best group as the result
            DisputeBehavior::AcceptAnyValid |
            DisputeBehavior::PreferBlockHeadLeader |
            DisputeBehavior::PreferHighestScore => {
                let disagreeing = sorted_groups
                    .iter()
                    .skip(1)
                    .flat_map(|g| g.upstreams.iter().map(Arc::clone))
                    .collect();

                Ok(ConsensusResult {
                    response: best_response,
                    agreement_count: best_count,
                    total_queried,
                    consensus_achieved: false,
                    disagreeing_upstreams: disagreeing,
                    metadata: ConsensusMetadata {
                        selection_method: SelectionMethod::FirstValid,
                        response_groups: sorted_groups,
                        duration_ms: 0,
                    },
                    response_type: best_response_type,
                })
            }
        }
    }
}

/// Groups responses by hash value with type classification.
///
/// When `ignore_paths` is provided, those field paths are excluded from the hash
/// computation, allowing responses with minor differences (like timestamps) to
/// be grouped together.
///
/// # Performance
/// - O(n) where n is the number of responses
/// - Uses `HashMap` for O(1) group lookups
/// - Single pass over responses
#[must_use]
pub fn group_responses(
    responses: Vec<(Arc<str>, JsonRpcResponse)>,
    ignore_paths: Option<&[String]>,
) -> Vec<ResponseGroup> {
    let mut groups: HashMap<u64, ResponseGroup> = HashMap::with_capacity(responses.len());

    for (upstream_name, response) in responses {
        // Use filtered hashing when ignore_paths is configured
        let hash = match ignore_paths {
            Some(paths) if !paths.is_empty() => {
                crate::utils::json_hash::hash_json_response_filtered(&response, paths)
            }
            _ => hash_response(&response),
        };
        let response_type = ResponseType::from_response(&response);
        let response_size = calculate_response_size(&response);

        groups
            .entry(hash)
            .and_modify(|group| {
                group.upstreams.push(Arc::clone(&upstream_name));
                group.count += 1;
            })
            .or_insert_with(|| ResponseGroup {
                response_hash: hash,
                upstreams: vec![upstream_name],
                count: 1,
                response: response.clone(),
                response_type,
                response_size,
            });
    }

    groups.into_values().collect()
}

/// Applies preference-based sorting to response groups.
///
/// Sorting priority (when enabled):
/// 1. `prefer_non_empty`: Prioritize `NonEmpty` > `Empty` > `ConsensusError` > `InfraError`
/// 2. Within same type, sort by vote count (descending)
/// 3. `prefer_larger_responses`: Among same count, prefer larger response bodies
///
/// # Performance
/// - In-place sort: O(n log n)
/// - No allocations
pub fn apply_preferences(groups: &mut [ResponseGroup], config: &ConsensusConfig) {
    groups.sort_by(|a, b| {
        // Primary: If prefer_non_empty is enabled, sort by response type priority
        if config.prefer_non_empty {
            let type_cmp = a.response_type.priority_order().cmp(&b.response_type.priority_order());
            if type_cmp != std::cmp::Ordering::Equal {
                return type_cmp;
            }
        }

        // Secondary: Always sort by vote count (descending)
        let count_cmp = b.count.cmp(&a.count);
        if count_cmp != std::cmp::Ordering::Equal {
            return count_cmp;
        }

        // Tertiary: If prefer_larger_responses is enabled, sort by size (descending)
        if config.prefer_larger_responses {
            return b.response_size.cmp(&a.response_size);
        }

        std::cmp::Ordering::Equal
    });
}

/// Calculates the approximate size of a response in bytes.
///
/// Uses a length estimation algorithm instead of full JSON serialization
/// for better performance. The estimate is accurate enough for response
/// size comparison in consensus logic.
pub fn calculate_response_size(response: &JsonRpcResponse) -> usize {
    response.result.as_ref().map_or(0, estimate_json_size)
}

/// Estimates JSON value size without serialization.
///
/// This is ~10x faster than `serde_json::to_string().len()` for large objects
/// while providing accurate enough estimates for response comparison.
///
/// # Size Calculations
/// - Null: 4 bytes (`"null"`)
/// - Bool: 4-5 bytes (`"true"` or `"false"`)
/// - Number: variable (stringified length)
/// - String: length + 2 (quotes)
/// - Array: brackets + commas + element sizes
/// - Object: braces + commas + colons + keys + values
pub fn estimate_json_size(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4, // "null"
        serde_json::Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        } // "true" or "false"
        serde_json::Value::Number(n) => n.to_string().len(),
        serde_json::Value::String(s) => s.len() + 2, // quotes
        serde_json::Value::Array(arr) => {
            // brackets + commas + element sizes
            2 + arr.len().saturating_sub(1) + arr.iter().map(estimate_json_size).sum::<usize>()
        }
        serde_json::Value::Object(obj) => {
            // braces + commas + colons + key quotes + key lengths + value sizes
            2 + obj.len().saturating_sub(1) +
                obj.iter()
                    .map(|(k, v)| k.len() + 3 + estimate_json_size(v)) // key + quotes + colon
                    .sum::<usize>()
        }
    }
}

/// Computes a hash of a JSON-RPC response for comparison.
#[must_use]
pub fn hash_response(response: &JsonRpcResponse) -> u64 {
    crate::utils::json_hash::hash_json_response(response)
}

/// Checks if two responses match by comparing their hashes.
#[must_use]
pub fn responses_match(resp1: &JsonRpcResponse, resp2: &JsonRpcResponse) -> bool {
    hash_response(resp1) == hash_response(resp2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JSONRPC_VERSION_COW;
    use serde_json::json;

    /// Helper to create a test response for consensus tests
    fn test_response(result: Option<serde_json::Value>, id: serde_json::Value) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW.clone(),
            result,
            error: None,
            id: Arc::new(id),
            cache_status: None,
        }
    }

    #[test]
    fn test_group_responses_identical() {
        let responses = vec![
            (Arc::from("up1"), test_response(Some(json!({"block": 100})), json!(1))),
            (Arc::from("up2"), test_response(Some(json!({"block": 100})), json!(1))),
            (Arc::from("up3"), test_response(Some(json!({"block": 100})), json!(1))),
        ];

        let groups = group_responses(responses, None);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 3);
        assert_eq!(groups[0].upstreams.len(), 3);
    }

    #[test]
    fn test_group_responses_different() {
        let responses = vec![
            (Arc::from("up1"), test_response(Some(json!({"block": 100})), json!(1))),
            (Arc::from("up2"), test_response(Some(json!({"block": 101})), json!(1))),
            (Arc::from("up3"), test_response(Some(json!({"block": 102})), json!(1))),
        ];

        let groups = group_responses(responses, None);
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn test_group_responses_with_ignore_paths() {
        let responses = vec![
            (
                Arc::from("up1"),
                test_response(Some(json!({"block": 100, "timestamp": 1000})), json!(1)),
            ),
            (
                Arc::from("up2"),
                test_response(Some(json!({"block": 100, "timestamp": 2000})), json!(1)),
            ),
        ];

        // Paths are relative to the result (not prefixed with "result.")
        let ignore_paths = vec!["timestamp".to_string()];
        let groups = group_responses(responses, Some(&ignore_paths));

        // Should be grouped together since timestamp is ignored
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 2);
    }

    #[test]
    fn test_apply_preferences_prefer_non_empty() {
        let mut groups = vec![
            ResponseGroup {
                response_hash: 1,
                upstreams: vec![Arc::from("up1")],
                count: 2,
                response: test_response(None, json!(1)),
                response_type: ResponseType::Empty,
                response_size: 0,
            },
            ResponseGroup {
                response_hash: 2,
                upstreams: vec![Arc::from("up2")],
                count: 1,
                response: test_response(Some(json!({"data": "value"})), json!(1)),
                response_type: ResponseType::NonEmpty,
                response_size: 20,
            },
        ];

        let config = ConsensusConfig { prefer_non_empty: true, ..Default::default() };
        apply_preferences(&mut groups, &config);

        // NonEmpty should come first despite lower count
        assert_eq!(groups[0].response_type, ResponseType::NonEmpty);
    }

    #[test]
    fn test_apply_preferences_by_count() {
        let mut groups = vec![
            ResponseGroup {
                response_hash: 1,
                upstreams: vec![Arc::from("up1")],
                count: 1,
                response: test_response(Some(json!({"a": 1})), json!(1)),
                response_type: ResponseType::NonEmpty,
                response_size: 10,
            },
            ResponseGroup {
                response_hash: 2,
                upstreams: vec![Arc::from("up2"), Arc::from("up3")],
                count: 2,
                response: test_response(Some(json!({"b": 2})), json!(1)),
                response_type: ResponseType::NonEmpty,
                response_size: 10,
            },
        ];

        let config = ConsensusConfig::default();
        apply_preferences(&mut groups, &config);

        // Higher count should come first
        assert_eq!(groups[0].count, 2);
    }

    #[test]
    fn test_estimate_json_size_primitives() {
        assert_eq!(estimate_json_size(&json!(null)), 4);
        assert_eq!(estimate_json_size(&json!(true)), 4);
        assert_eq!(estimate_json_size(&json!(false)), 5);
        assert_eq!(estimate_json_size(&json!(123)), 3);
        assert_eq!(estimate_json_size(&json!("hello")), 7); // 5 + 2 quotes
    }

    #[test]
    fn test_estimate_json_size_complex() {
        let value = json!({"key": "value", "num": 42});
        let size = estimate_json_size(&value);
        // Should be close to serialized length
        let serialized = serde_json::to_string(&value).unwrap();
        #[allow(clippy::cast_possible_wrap)]
        let size_i64 = size as i64;
        #[allow(clippy::cast_possible_wrap)]
        let len_i64 = serialized.len() as i64;
        assert!(
            size_i64.saturating_sub(len_i64).abs() < 5,
            "estimate {} vs actual {}",
            size,
            serialized.len()
        );
    }

    #[test]
    fn test_hash_response_deterministic() {
        let resp1 = test_response(Some(json!({"data": 123})), json!(1));
        let resp2 = test_response(Some(json!({"data": 123})), json!(1));
        let resp3 = test_response(Some(json!({"data": 456})), json!(1));

        assert_eq!(hash_response(&resp1), hash_response(&resp2));
        assert_ne!(hash_response(&resp1), hash_response(&resp3));
    }

    #[test]
    fn test_responses_match() {
        let resp1 = test_response(Some(json!({"block": 100})), json!(1));
        let resp2 = test_response(Some(json!({"block": 100})), json!(1));
        let resp3 = test_response(Some(json!({"block": 101})), json!(1));

        assert!(responses_match(&resp1, &resp2));
        assert!(!responses_match(&resp1, &resp3));
    }

    #[test]
    fn test_find_consensus_achieved() {
        let responses = vec![
            (Arc::from("up1"), test_response(Some(json!({"block": 100})), json!(1))),
            (Arc::from("up2"), test_response(Some(json!({"block": 100})), json!(1))),
            (Arc::from("up3"), test_response(Some(json!({"block": 101})), json!(1))),
        ];

        let config = ConsensusConfig { min_count: 2, ..Default::default() };
        let result = find_consensus(responses, &config, "eth_getBlockByNumber").unwrap();

        assert!(result.consensus_achieved);
        assert_eq!(result.agreement_count, 2);
        assert_eq!(result.disagreeing_upstreams.len(), 1);
    }

    #[test]
    fn test_find_consensus_not_achieved() {
        let responses = vec![
            (Arc::from("up1"), test_response(Some(json!({"block": 100})), json!(1))),
            (Arc::from("up2"), test_response(Some(json!({"block": 101})), json!(1))),
            (Arc::from("up3"), test_response(Some(json!({"block": 102})), json!(1))),
        ];

        let config = ConsensusConfig {
            min_count: 2,
            dispute_behavior: DisputeBehavior::AcceptAnyValid,
            ..Default::default()
        };
        let result = find_consensus(responses, &config, "eth_getBlockByNumber").unwrap();

        assert!(!result.consensus_achieved);
        assert_eq!(result.agreement_count, 1);
    }

    #[test]
    fn test_find_consensus_empty() {
        let responses = vec![];
        let config = ConsensusConfig::default();
        let result = find_consensus(responses, &config, "test");

        assert!(result.is_err());
    }
}
