//! Consensus engine implementation for quorum-based RPC queries.
//!
//! This module provides the `ConsensusEngine` which orchestrates consensus
//! queries across multiple upstreams. The core consensus algorithm is
//! implemented in the [`super::quorum`] module.

use super::{
    config::{ConsensusConfig, LowParticipantsBehavior},
    quorum,
    types::{ConsensusMetadata, ConsensusResult, ResponseGroup, ResponseType, SelectionMethod},
    validation,
};
use crate::{
    types::{JsonRpcRequest, JsonRpcResponse},
    upstream::{endpoint::UpstreamEndpoint, errors::UpstreamError, scoring::ScoringEngine},
};
use std::{sync::Arc, time::Instant};
use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, info, warn};

use crate::upstream::misbehavior::MisbehaviorTracker;

pub struct ConsensusEngine {
    config: Arc<RwLock<ConsensusConfig>>,
    misbehavior_tracker: Option<Arc<MisbehaviorTracker>>,
    /// Semaphore for limiting concurrent consensus operations (backpressure)
    query_semaphore: Arc<Semaphore>,
}

impl ConsensusEngine {
    /// Creates a new consensus engine with the given configuration.
    ///
    /// Initializes the misbehavior tracker if `dispute_threshold` is configured
    /// and creates a semaphore for backpressure control.
    #[must_use]
    pub fn new(config: ConsensusConfig) -> Self {
        let misbehavior_tracker = if config.misbehavior_enabled() {
            let tracker = Arc::new(MisbehaviorTracker::new(config.misbehavior_config()));
            info!("misbehavior tracking enabled for consensus engine");
            Some(tracker)
        } else {
            None
        };

        let query_semaphore = Arc::new(Semaphore::new(config.max_concurrent_queries));

        Self { config: Arc::new(RwLock::new(config)), misbehavior_tracker, query_semaphore }
    }

    /// Returns whether consensus is enabled.
    pub async fn is_enabled(&self) -> bool {
        self.config.read().await.enabled
    }

    /// Returns whether a method requires consensus.
    pub async fn requires_consensus(&self, method: &str) -> bool {
        let config = self.config.read().await;
        config.enabled && config.methods.iter().any(|m| m == method)
    }

    /// Updates the consensus configuration at runtime.
    ///
    /// Also updates the misbehavior tracker configuration if tracking is enabled.
    pub async fn update_config(&self, config: ConsensusConfig) {
        // Update misbehavior tracker config if it exists
        if let Some(tracker) = &self.misbehavior_tracker {
            tracker.update_config(config.misbehavior_config()).await;
        }

        *self.config.write().await = config;
        info!("consensus configuration updated");
    }

    /// Returns a copy of the current configuration.
    pub async fn get_config(&self) -> ConsensusConfig {
        self.config.read().await.clone()
    }

    /// Filters out upstreams currently in sit-out due to misbehavior.
    ///
    /// Returns only upstreams that are available for consensus queries.
    /// Optimized to avoid intermediate allocations by checking sit-out status inline.
    async fn filter_available_upstreams(
        &self,
        upstreams: Vec<Arc<UpstreamEndpoint>>,
    ) -> Vec<Arc<UpstreamEndpoint>> {
        if let Some(tracker) = &self.misbehavior_tracker {
            // Single-pass filter: check each upstream's sit-out status directly
            // Avoids 4 intermediate Vec/HashSet allocations from previous implementation
            let mut available = Vec::with_capacity(upstreams.len());
            for upstream in upstreams {
                if !tracker.is_in_sit_out(upstream.config().name.as_ref()).await {
                    available.push(upstream);
                }
            }
            available
        } else {
            // No misbehavior tracking - all upstreams available
            upstreams
        }
    }

    /// Records disputes for disagreeing upstreams in the misbehavior tracker.
    ///
    /// Called after consensus is determined to track upstreams that provided
    /// incorrect responses.
    async fn record_disputes(&self, disagreeing_upstreams: &[Arc<str>], method: &str) {
        if let Some(tracker) = &self.misbehavior_tracker {
            for upstream_name in disagreeing_upstreams {
                let sit_out_triggered = tracker.record_dispute(upstream_name, Some(method)).await;
                if sit_out_triggered {
                    warn!(
                        upstream = %upstream_name,
                        method = %method,
                        "upstream entered sit-out due to excessive consensus disputes"
                    );
                }
            }
        }
    }

    /// Executes a consensus query across multiple upstreams.
    ///
    /// Acquires a permit from the backpressure semaphore before executing.
    /// This prevents overwhelming upstreams during traffic spikes.
    ///
    /// # Errors
    ///
    /// Returns `UpstreamError` variants:
    /// - `Timeout`: Backpressure semaphore acquisition timed out (high load)
    /// - `NoHealthyUpstreams`: All upstreams excluded (sitting out) or unhealthy
    /// - `ConsensusFailure`: Quorum not reached after querying upstreams
    /// - `RpcError`: Upstream returned JSON-RPC error (rare in consensus path)
    ///
    /// Error handling strategy:
    /// - `Timeout`: Caller should retry with backoff or return error to client
    /// - `NoHealthyUpstreams`: Fallback to non-consensus path if allowed
    /// - `ConsensusFailure`: Log dispute, consider returning most common response
    pub async fn execute_consensus(
        &self,
        request: Arc<JsonRpcRequest>,
        upstreams: Vec<Arc<UpstreamEndpoint>>,
        scoring_engine: &Arc<ScoringEngine>,
    ) -> Result<ConsensusResult, UpstreamError> {
        let start = Instant::now();
        // Clone config immediately to release lock before any async operations.
        // This prevents blocking config updates during long-running consensus queries
        // (which include network calls that can take seconds under timeout).
        // The per-query allocation (~200 bytes) is acceptable vs lock contention.
        let config = self.config.read().await.clone();

        // Acquire backpressure permit with timeout to prevent unbounded queue growth
        let _permit = tokio::time::timeout(
            std::time::Duration::from_secs(config.timeout_seconds),
            self.query_semaphore.acquire(),
        )
        .await
        .map_err(|_| {
            warn!(
                method = %request.method,
                available_permits = self.query_semaphore.available_permits(),
                "consensus query permit acquisition timeout - backpressure applied"
            );
            UpstreamError::Timeout
        })?
        .map_err(|_| {
            // Semaphore closed - should not happen in normal operation
            UpstreamError::ConsensusFailure("consensus engine shutting down".to_string())
        })?;

        // Filter out upstreams currently in sit-out due to misbehavior
        let available_upstreams = self.filter_available_upstreams(upstreams).await;

        if available_upstreams.is_empty() {
            return Err(UpstreamError::NoHealthyUpstreams);
        }

        if available_upstreams.len() < config.min_count {
            return self
                .handle_low_participants(
                    request,
                    &available_upstreams,
                    &config,
                    scoring_engine,
                    start,
                )
                .await;
        }

        let query_count = config.max_count.min(available_upstreams.len());
        let selected_upstreams = &available_upstreams[..query_count];

        debug!(
            method = %request.method,
            upstream_count = query_count,
            "executing consensus query"
        );

        let results =
            self.query_upstreams(Arc::clone(&request), selected_upstreams, &config).await?;
        let (successes, _failures) = Self::process_query_results(results, &config)?;
        let consensus_result = self
            .find_consensus_and_penalize(successes, scoring_engine, &config, &request.method)
            .await?;

        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(ConsensusResult {
            metadata: ConsensusMetadata { duration_ms, ..consensus_result.metadata },
            ..consensus_result
        })
    }

    /// Handles the case when there are insufficient upstreams for consensus.
    ///
    /// Applies the configured `LowParticipantsBehavior` strategy to determine
    /// whether to return an error, query only the block head leader, or proceed
    /// with the available upstreams.
    async fn handle_low_participants(
        &self,
        request: Arc<JsonRpcRequest>,
        upstreams: &[Arc<UpstreamEndpoint>],
        config: &ConsensusConfig,
        scoring_engine: &Arc<ScoringEngine>,
        start: Instant,
    ) -> Result<ConsensusResult, UpstreamError> {
        match config.low_participants_behavior {
            LowParticipantsBehavior::ReturnError => Err(UpstreamError::ConsensusFailure(format!(
                "Insufficient upstreams: {} available, {} required",
                upstreams.len(),
                config.min_count
            ))),
            LowParticipantsBehavior::OnlyBlockHeadLeader => {
                let leader = self.select_block_head_leader(upstreams, scoring_engine);
                let Some(upstream) = leader else {
                    return Err(UpstreamError::NoHealthyUpstreams);
                };

                let response = upstream.send_request(&request).await?;
                let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                let response_type = ResponseType::from_response(&response);

                Ok(ConsensusResult {
                    response: response.clone(),
                    agreement_count: 1,
                    total_queried: 1,
                    consensus_achieved: false,
                    disagreeing_upstreams: vec![],
                    metadata: ConsensusMetadata {
                        selection_method: SelectionMethod::BlockHeadLeader,
                        response_groups: vec![ResponseGroup {
                            response_hash: Self::hash_response(&response),
                            upstreams: vec![Arc::clone(&upstream.config().name)],
                            count: 1,
                            response_type,
                            response_size: Self::calculate_response_size(&response),
                            response,
                        }],
                        duration_ms,
                    },
                    response_type,
                })
            }
            LowParticipantsBehavior::AcceptAvailable => {
                // Continue with normal consensus logic using available upstreams
                let query_count = config.max_count.min(upstreams.len());
                let selected_upstreams = &upstreams[..query_count];
                let results =
                    self.query_upstreams(Arc::clone(&request), selected_upstreams, config).await?;
                let (successes, _failures) = Self::process_query_results(results, config)?;
                let consensus_result = self
                    .find_consensus_and_penalize(successes, scoring_engine, config, &request.method)
                    .await?;

                let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

                Ok(ConsensusResult {
                    metadata: ConsensusMetadata { duration_ms, ..consensus_result.metadata },
                    ..consensus_result
                })
            }
        }
    }

    /// Queries multiple upstreams in parallel with a timeout.
    ///
    /// Sends the request to each upstream and waits for all responses
    /// or until the configured timeout is reached.
    async fn query_upstreams(
        &self,
        request: Arc<JsonRpcRequest>,
        upstreams: &[Arc<UpstreamEndpoint>],
        config: &ConsensusConfig,
    ) -> Result<Vec<Option<(Arc<str>, Result<JsonRpcResponse, UpstreamError>)>>, UpstreamError>
    {
        let futures: Vec<_> = upstreams
            .iter()
            .map(|upstream| {
                let req = Arc::clone(&request);
                let up = upstream.clone();
                async move {
                    let name = Arc::clone(&up.config().name);
                    match up.send_request(&req).await {
                        Ok(resp) => Some((name, Ok(resp))),
                        Err(e) => Some((name, Err(e))),
                    }
                }
            })
            .collect();

        let timeout = std::time::Duration::from_secs(config.timeout_seconds);
        tokio::time::timeout(timeout, futures_util::future::join_all(futures))
            .await
            .map_err(|_| UpstreamError::Timeout)
    }

    /// Processes query results, separating successes from failures.
    ///
    /// Returns successful responses or an error based on the configured
    /// `FailureBehavior` if all queries failed.
    ///
    /// The return type `(Vec<(Arc<str>, JsonRpcResponse)>, Vec<Arc<str>>)` represents
    /// `(successes, failures)` where each success pairs an upstream name with its response.
    /// A type alias would obscure this semantic meaning at call sites where tuple
    /// destructuring makes the intent clear: `let (successes, failures) = ...`.
    #[allow(clippy::type_complexity)]
    fn process_query_results(
        results: Vec<Option<(Arc<str>, Result<JsonRpcResponse, UpstreamError>)>>,
        config: &ConsensusConfig,
    ) -> Result<(Vec<(Arc<str>, JsonRpcResponse)>, Vec<Arc<str>>), UpstreamError> {
        validation::process_query_results(results, config)
    }

    /// Finds consensus among responses and applies penalties to disagreeing upstreams.
    ///
    /// Analyzes response groups to determine consensus, records errors in the scoring
    /// system, and tracks disputes in the misbehavior tracker (if enabled).
    ///
    /// # Penalty System (Intentional Multi-Layer Design)
    ///
    /// Disagreeing upstreams receive TWO complementary penalties:
    ///
    /// 1. **Scoring penalty** (`record_error`): Reduces the upstream's score, making it less likely
    ///    to be selected for future requests. This is a "soft" penalty that affects selection
    ///    probability but doesn't exclude the upstream.
    ///
    /// 2. **Misbehavior tracking** (`record_disputes`): Tracks dispute count toward a sit-out
    ///    threshold. After exceeding the threshold, the upstream is temporarily excluded from
    ///    consensus queries. This is a "hard" penalty for repeated offenses.
    ///
    /// This multi-layer approach provides defense-in-depth:
    /// - Minor/occasional disagreements: Reduced selection probability (scoring)
    /// - Repeated/persistent disagreements: Temporary exclusion (sit-out)
    ///
    /// The penalties are additive by design - scoring affects all requests while
    /// misbehavior tracking only triggers after a threshold is exceeded.
    async fn find_consensus_and_penalize(
        &self,
        successes: Vec<(Arc<str>, JsonRpcResponse)>,
        scoring_engine: &Arc<ScoringEngine>,
        config: &ConsensusConfig,
        method: &str,
    ) -> Result<ConsensusResult, UpstreamError> {
        let consensus_result = self.find_consensus(successes, method).await?;

        // Apply multi-layer penalties to disagreeing upstreams (see doc comment above)
        for upstream_name in &consensus_result.disagreeing_upstreams {
            // Layer 1: Soft penalty - reduce selection score
            scoring_engine.record_error(upstream_name);
            debug!(
                upstream = %upstream_name,
                penalty = config.disagreement_penalty,
                "penalizing disagreeing upstream (scoring)"
            );
        }

        // Layer 2: Hard penalty tracking - toward sit-out threshold
        if !consensus_result.disagreeing_upstreams.is_empty() {
            self.record_disputes(&consensus_result.disagreeing_upstreams, method).await;
        }

        Ok(consensus_result)
    }

    /// Finds consensus among successful responses.
    ///
    /// Delegates to [`quorum::find_consensus`] for the core algorithm.
    /// The `method` parameter is used to look up per-method `ignore_fields` configuration
    /// for filtering out chain-specific or timing-sensitive fields from response hashing.
    pub(crate) async fn find_consensus(
        &self,
        responses: Vec<(Arc<str>, JsonRpcResponse)>,
        method: &str,
    ) -> Result<ConsensusResult, UpstreamError> {
        let config = self.config.read().await.clone();
        quorum::find_consensus(responses, &config, method)
    }

    /// Calculates the approximate size of a response in bytes.
    ///
    /// Delegates to [`quorum::calculate_response_size`].
    fn calculate_response_size(response: &JsonRpcResponse) -> usize {
        quorum::calculate_response_size(response)
    }

    /// Computes a hash of a JSON-RPC response for comparison.
    pub(crate) fn hash_response(response: &JsonRpcResponse) -> u64 {
        quorum::hash_response(response)
    }

    /// Checks if two responses match.
    #[must_use]
    pub(crate) fn responses_match(resp1: &JsonRpcResponse, resp2: &JsonRpcResponse) -> bool {
        quorum::responses_match(resp1, resp2)
    }

    /// Selects the upstream with the highest block number.
    #[allow(clippy::unused_self)]
    pub(crate) fn select_block_head_leader(
        &self,
        upstreams: &[Arc<UpstreamEndpoint>],
        scoring_engine: &Arc<ScoringEngine>,
    ) -> Option<Arc<UpstreamEndpoint>> {
        validation::select_block_head_leader(upstreams, scoring_engine)
    }
}
