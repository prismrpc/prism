//! Response validation and processing for consensus queries.
//!
//! This module contains helper functions for processing and validating
//! responses from upstream nodes during consensus operations.

use super::config::{ConsensusConfig, FailureBehavior};
use crate::{
    types::JsonRpcResponse,
    upstream::{endpoint::UpstreamEndpoint, errors::UpstreamError, scoring::ScoringEngine},
};
use std::sync::Arc;
use tracing::warn;

/// Processes query results, separating successes from failures.
///
/// Returns successful responses or an error based on the configured
/// `FailureBehavior` if all queries failed.
///
/// # Arguments
/// * `results` - Results from querying upstreams (Some = responded, None = timed out)
/// * `config` - Consensus configuration for failure handling
///
/// # Returns
/// `(successes, failures)` where each success pairs an upstream name with its response.
///
/// # Errors
/// Returns `UpstreamError::ConsensusFailure` if all queries failed and
/// `failure_behavior` is `ReturnError`, or `UpstreamError::NoHealthyUpstreams`
/// for other failure behaviors.
#[allow(clippy::type_complexity)]
pub fn process_query_results(
    results: Vec<Option<(Arc<str>, Result<JsonRpcResponse, UpstreamError>)>>,
    config: &ConsensusConfig,
) -> Result<(Vec<(Arc<str>, JsonRpcResponse)>, Vec<Arc<str>>), UpstreamError> {
    // Pre-allocate with expected capacity to avoid reallocations
    let expected_count = results.len();
    let mut successes: Vec<(Arc<str>, JsonRpcResponse)> = Vec::with_capacity(expected_count);
    let mut failures: Vec<Arc<str>> = Vec::with_capacity(expected_count);

    for result in results.into_iter().flatten() {
        match result {
            (name, Ok(response)) => {
                successes.push((name, response));
            }
            (name, Err(e)) => {
                warn!(upstream = %name, error = %e, "consensus query failed");
                failures.push(name);
            }
        }
    }

    if successes.is_empty() {
        match config.failure_behavior {
            FailureBehavior::ReturnError => {
                Err(UpstreamError::ConsensusFailure("All consensus queries failed".to_string()))
            }
            FailureBehavior::AcceptAnyValid | FailureBehavior::UseHighestScore => {
                Err(UpstreamError::NoHealthyUpstreams)
            }
        }
    } else {
        Ok((successes, failures))
    }
}

/// Selects the upstream with the highest block number.
///
/// This is used for the `OnlyBlockHeadLeader` low participants behavior
/// and `PreferBlockHeadLeader` dispute behavior.
///
/// # Arguments
/// * `upstreams` - Available upstreams to choose from
/// * `scoring_engine` - Scoring engine for reading block heights
///
/// # Returns
/// The upstream with the highest known block number, or the first upstream
/// if no block height information is available.
pub fn select_block_head_leader(
    upstreams: &[Arc<UpstreamEndpoint>],
    scoring_engine: &Arc<ScoringEngine>,
) -> Option<Arc<UpstreamEndpoint>> {
    let mut best_upstream: Option<Arc<UpstreamEndpoint>> = None;
    let mut highest_block = 0u64;

    for upstream in upstreams {
        let name = &upstream.config().name;
        if let Some((_error_rate, _throttle_rate, _total_req, block, _avg)) =
            scoring_engine.get_metrics(name)
        {
            if block > highest_block {
                highest_block = block;
                best_upstream = Some(upstream.clone());
            }
        }
    }

    best_upstream.or_else(|| upstreams.first().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JSONRPC_VERSION_COW;
    use serde_json::json;

    fn test_response(result: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW.clone(),
            result,
            error: None,
            id: Arc::new(json!(1)),
            cache_status: None,
        }
    }

    #[test]
    fn test_process_query_results_all_success() {
        let results = vec![
            Some((Arc::from("up1"), Ok(test_response(Some(json!({"block": 100})))))),
            Some((Arc::from("up2"), Ok(test_response(Some(json!({"block": 100})))))),
        ];

        let config = ConsensusConfig::default();
        let (successes, failures) = process_query_results(results, &config).unwrap();

        assert_eq!(successes.len(), 2);
        assert!(failures.is_empty());
    }

    #[test]
    fn test_process_query_results_mixed() {
        let results = vec![
            Some((Arc::from("up1"), Ok(test_response(Some(json!({"block": 100})))))),
            Some((Arc::from("up2"), Err(UpstreamError::Timeout))),
        ];

        let config = ConsensusConfig::default();
        let (successes, failures) = process_query_results(results, &config).unwrap();

        assert_eq!(successes.len(), 1);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].as_ref(), "up2");
    }

    #[test]
    fn test_process_query_results_all_failed_return_error() {
        let results = vec![
            Some((Arc::from("up1"), Err(UpstreamError::Timeout))),
            Some((Arc::from("up2"), Err(UpstreamError::Timeout))),
        ];

        let config = ConsensusConfig {
            failure_behavior: FailureBehavior::ReturnError,
            ..Default::default()
        };
        let result = process_query_results(results, &config);

        assert!(matches!(result, Err(UpstreamError::ConsensusFailure(_))));
    }

    #[test]
    fn test_process_query_results_all_failed_accept_any() {
        let results = vec![
            Some((Arc::from("up1"), Err(UpstreamError::Timeout))),
            Some((Arc::from("up2"), Err(UpstreamError::Timeout))),
        ];

        let config = ConsensusConfig {
            failure_behavior: FailureBehavior::AcceptAnyValid,
            ..Default::default()
        };
        let result = process_query_results(results, &config);

        assert!(matches!(result, Err(UpstreamError::NoHealthyUpstreams)));
    }

    #[test]
    fn test_process_query_results_with_none() {
        let results = vec![
            Some((Arc::from("up1"), Ok(test_response(Some(json!({"block": 100})))))),
            None, // Timed out response
        ];

        let config = ConsensusConfig::default();
        let (successes, failures) = process_query_results(results, &config).unwrap();

        // None entries are skipped (flatten)
        assert_eq!(successes.len(), 1);
        assert!(failures.is_empty());
    }
}
