//! Request routing strategies for upstream management.
//!
//! This module provides the `SmartRouter` for intelligent request routing
//! with support for consensus, scoring, hedging, and response-time strategies.

mod smart;

pub use smart::SmartRouter;

use crate::{
    chain::ChainState,
    types::{JsonRpcRequest, JsonRpcResponse},
    upstream::{
        consensus::ConsensusEngine, endpoint::UpstreamEndpoint, hedging::HedgeExecutor,
        load_balancer::LoadBalancer, scoring::ScoringEngine,
    },
};
use std::sync::Arc;

/// Shared context required by request routers.
///
/// Contains all the dependencies needed for routing decisions,
/// avoiding the need to pass multiple parameters to each router.
///
/// # Context Pattern Design
///
/// This struct uses the **context pattern** to enable stateless router implementations:
///
/// - **Stateless routers**: The `SmartRouter` doesn't maintain its own state or dependencies. It
///   receives all necessary context through this struct at routing time.
///
/// - **Shared interface**: The `SmartRouter` uses the `route(request, ctx)` method, receiving all
///   context through this struct.
///
/// - **Flexible composition**: The router can select between different routing strategies
///   (consensus, scoring, hedging, simple) based on configuration.
///
/// # Field Guide
///
/// Each field represents a specific aspect of the routing decision:
///
/// - **`load_balancer`**: Maintains upstream health status and provides endpoint selection based on
///   availability, weights, and block availability filtering.
///
/// - **`scoring_engine`**: Tracks upstream performance metrics (latency, error rates) and
///   calculates quality scores for response-time-aware routing decisions.
///
/// - **`consensus_engine`**: Coordinates quorum-based validation across multiple upstreams to
///   detect discrepancies and ensure response correctness (used by `SmartRouter`).
///
/// - **`hedger`**: Executes hedged requests (parallel requests with staggered timing) to reduce
///   tail latency when enabled in configuration.
///
/// - **`chain_state`**: Provides lock-free access to current chain tip and finalized block for
///   block availability filtering and finality-aware routing decisions.
///
/// # Usage
///
/// The `SmartRouter` accesses these dependencies through the context to make routing decisions:
///
/// ```ignore
/// // SmartRouter.route() uses these fields:
/// let upstreams = ctx.get_capable_upstreams(request).await;  // load_balancer
/// let best = ctx.scoring_engine.select_best(&upstreams);     // scoring_engine
/// let response = best.send_request(request).await?;
/// ```
#[derive(Clone)]
pub struct RoutingContext {
    /// Load balancer for selecting upstreams
    pub load_balancer: Arc<LoadBalancer>,
    /// Scoring engine for upstream quality assessment
    pub scoring_engine: Arc<ScoringEngine>,
    /// Consensus engine for quorum-based validation
    pub consensus_engine: Arc<ConsensusEngine>,
    /// Hedging executor for tail latency reduction
    pub hedger: Arc<HedgeExecutor>,
    /// Shared chain state for finality tracking
    pub chain_state: Arc<ChainState>,
}

impl RoutingContext {
    /// Creates a new routing context.
    #[must_use]
    pub fn new(
        load_balancer: Arc<LoadBalancer>,
        scoring_engine: Arc<ScoringEngine>,
        consensus_engine: Arc<ConsensusEngine>,
        hedger: Arc<HedgeExecutor>,
        chain_state: Arc<ChainState>,
    ) -> Self {
        Self { load_balancer, scoring_engine, consensus_engine, hedger, chain_state }
    }

    /// Returns healthy upstreams filtered by block availability if applicable.
    ///
    /// If the request targets a specific block number, only returns upstreams
    /// that have synced to that block. During reorgs, block-specific filtering
    /// may temporarily return empty even when upstreams are healthy. This method
    /// falls back to all healthy upstreams in that case to prevent spurious errors.
    pub async fn get_capable_upstreams(
        &self,
        request: &JsonRpcRequest,
    ) -> Vec<Arc<UpstreamEndpoint>> {
        if let Some(block_number) = extract_requested_block(request) {
            let block_specific =
                self.load_balancer.get_healthy_upstreams_for_block(block_number).await;

            if block_specific.is_empty() {
                tracing::debug!(
                    block = block_number,
                    "block-specific filtering returned empty, falling back to all healthy upstreams"
                );
                self.load_balancer.get_healthy_upstreams().await
            } else {
                block_specific
            }
        } else {
            self.load_balancer.get_healthy_upstreams().await
        }
    }

    /// Checks if any healthy upstreams are available.
    pub async fn has_healthy_upstreams(&self) -> bool {
        !self.load_balancer.get_healthy_upstreams().await.is_empty()
    }
}

/// Extracts the requested block number from a JSON-RPC request.
///
/// Used for block availability pre-filtering to ensure requests are only
/// routed to upstreams that have synced to the required block.
///
/// Supports extracting block numbers from:
/// - `eth_getBlockByNumber`: first param is block number/tag
/// - `eth_getBalance`, `eth_getCode`: second param is block number/tag
/// - `eth_getLogs`: `toBlock` from filter object (uses the higher bound)
/// - `eth_getTransactionReceipt`, `eth_getTransactionByHash`: returns None (no block param)
#[must_use]
pub fn extract_requested_block(request: &JsonRpcRequest) -> Option<u64> {
    let params = request.params.as_ref()?;

    match request.method.as_str() {
        "eth_getBlockByNumber" => {
            let block_param = params.get(0).or_else(|| params.as_array()?.first())?;
            parse_block_param(block_param)
        }
        "eth_getBalance" | "eth_getCode" => {
            let block_param = params.get(1).or_else(|| params.as_array()?.get(1))?;
            parse_block_param(block_param)
        }
        "eth_getLogs" => {
            let filter = params.get(0).or_else(|| params.as_array()?.first())?;
            if let Some(to_block) = filter.get("toBlock") {
                parse_block_param(to_block)
            } else if let Some(from_block) = filter.get("fromBlock") {
                parse_block_param(from_block)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parses a block parameter value (hex string or tag) into a block number.
///
/// Returns None for special tags like "latest", "pending", "earliest" since
/// these don't require a specific block number from the upstream.
fn parse_block_param(value: &serde_json::Value) -> Option<u64> {
    use crate::utils::BlockParameter;

    let s = value.as_str()?;
    BlockParameter::parse_number(s)
}

/// Extracts block number from a JSON-RPC response if available.
///
/// Supports extracting block numbers from:
/// - `eth_blockNumber` responses
/// - `eth_getBlockByNumber` responses (block.number field)
/// - `eth_getBlockByHash` responses (block.number field)
#[must_use]
pub fn extract_block_number(request: &JsonRpcRequest, response: &JsonRpcResponse) -> Option<u64> {
    use crate::utils::BlockParameter;

    match request.method.as_str() {
        "eth_blockNumber" => response.result.as_ref().and_then(BlockParameter::from_json_value),
        "eth_getBlockByNumber" | "eth_getBlockByHash" => response
            .result
            .as_ref()
            .and_then(|result| result.get("number").and_then(BlockParameter::from_json_value)),
        _ => None,
    }
}
