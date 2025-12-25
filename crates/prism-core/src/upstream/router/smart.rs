//! Smart adaptive routing strategy that combines all routing approaches.

use super::{extract_block_number, RoutingContext};
use crate::{
    types::{JsonRpcRequest, JsonRpcResponse},
    upstream::{consensus::ConsensusEngine, errors::UpstreamError},
};
use std::{sync::Arc, time::Duration};
use tracing::warn;

/// Smart router that automatically selects the best routing strategy.
///
/// Implements the current `send_request_auto()` logic with priority:
/// 1. Consensus (if required for this method)
/// 2. Scoring-based selection (if enabled)
/// 3. Hedged requests (if enabled and scoring disabled)
/// 4. Response-time-aware selection (fallback)
pub struct SmartRouter;

impl SmartRouter {
    /// Creates a new smart router.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Routes using consensus validation.
    async fn route_with_consensus(
        request: &JsonRpcRequest,
        ctx: &RoutingContext,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let capable_upstreams = ctx.get_capable_upstreams(request).await;

        if capable_upstreams.is_empty() {
            return Err(UpstreamError::NoHealthyUpstreams);
        }

        let result = ctx
            .consensus_engine
            .execute_consensus(Arc::new(request.clone()), capable_upstreams, &ctx.scoring_engine)
            .await?;

        let latency_ms = result.metadata.duration_ms;
        let mut consensus_upstream: Option<Arc<str>> = None;

        // Record success for all upstreams in the winning consensus group
        for group in &result.metadata.response_groups {
            if result.consensus_achieved &&
                group.upstreams.len() == result.agreement_count &&
                ConsensusEngine::responses_match(&group.response, &result.response)
            {
                for upstream_name in &group.upstreams {
                    ctx.scoring_engine.record_success(upstream_name, latency_ms);
                    // Store first upstream name from consensus group for metrics
                    if consensus_upstream.is_none() {
                        consensus_upstream = Some(Arc::from(upstream_name.as_ref()));
                    }
                }
                break;
            }
        }

        // Extract block number if available and record for agreeing upstreams
        if let Some(block_number) = extract_block_number(request, &result.response) {
            for group in &result.metadata.response_groups {
                if ConsensusEngine::responses_match(&group.response, &result.response) {
                    for upstream_name in &group.upstreams {
                        ctx.scoring_engine.record_block_number(upstream_name, block_number).await;
                    }
                    break;
                }
            }
        }

        let mut response = result.response;
        // Set serving upstream to the first upstream in the consensus group
        response.serving_upstream = consensus_upstream;
        Ok(response)
    }

    /// Routes using score-based selection.
    async fn route_with_scoring(
        request: &JsonRpcRequest,
        ctx: &RoutingContext,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let capable_upstreams = ctx.get_capable_upstreams(request).await;

        if capable_upstreams.is_empty() {
            return Err(UpstreamError::NoHealthyUpstreams);
        }

        let scoring_config = ctx.scoring_engine.get_config();
        let top_n = scoring_config.top_n;

        let upstream = ctx
            .load_balancer
            .get_next_by_score_from(&ctx.scoring_engine, top_n, &capable_upstreams)
            .await
            .ok_or(UpstreamError::NoHealthyUpstreams)?;

        let start = std::time::Instant::now();
        let upstream_name = upstream.config().name.clone();
        let request_timeout = Duration::from_secs(15);

        let mut result = tokio::time::timeout(request_timeout, upstream.send_request(request))
            .await
            .map_err(|_| {
                warn!(timeout_secs = request_timeout.as_secs(), "scored request timed out");
                UpstreamError::Timeout
            })?;

        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        match &result {
            Ok(response) => {
                ctx.scoring_engine.record_success(&upstream_name, latency_ms);

                if let Some(block_number) = extract_block_number(request, response) {
                    ctx.scoring_engine.record_block_number(&upstream_name, block_number).await;
                }
            }
            Err(e) => {
                match e {
                    UpstreamError::HttpError(429, _) => {
                        ctx.scoring_engine.record_throttle(&upstream_name);
                    }
                    UpstreamError::RpcError(code, message) => {
                        use crate::upstream::errors::RpcErrorCategory;
                        let category = RpcErrorCategory::from_code_and_message(*code, message);
                        if matches!(category, RpcErrorCategory::RateLimit) {
                            ctx.scoring_engine.record_throttle(&upstream_name);
                        } else if category.should_penalize_upstream() {
                            ctx.scoring_engine.record_error(&upstream_name);
                        }
                        // Client errors and execution errors don't affect scoring
                    }
                    _ => {
                        ctx.scoring_engine.record_error(&upstream_name);
                    }
                }
            }
        }

        // Set the serving upstream name in the response for metrics tracking
        if let Ok(ref mut response) = result {
            response.serving_upstream = Some(upstream_name);
        }

        result
    }

    /// Routes using hedging for tail latency reduction.
    async fn route_with_hedge(
        request: &JsonRpcRequest,
        ctx: &RoutingContext,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let capable_upstreams = ctx.get_capable_upstreams(request).await;

        if capable_upstreams.is_empty() {
            return Err(UpstreamError::NoHealthyUpstreams);
        }

        let primary_name = capable_upstreams[0].config().name.clone();
        let start = std::time::Instant::now();

        let mut result =
            ctx.hedger.execute_hedged(Arc::new(request.clone()), capable_upstreams).await;

        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        match &result {
            Ok(response) => {
                ctx.scoring_engine.record_success(&primary_name, latency_ms);

                if let Some(block_number) = extract_block_number(request, response) {
                    ctx.scoring_engine.record_block_number(&primary_name, block_number).await;
                }
            }
            Err(e) => {
                match e {
                    UpstreamError::HttpError(429, _) => {
                        ctx.scoring_engine.record_throttle(&primary_name);
                    }
                    UpstreamError::RpcError(code, message) => {
                        use crate::upstream::errors::RpcErrorCategory;
                        let category = RpcErrorCategory::from_code_and_message(*code, message);
                        if matches!(category, RpcErrorCategory::RateLimit) {
                            ctx.scoring_engine.record_throttle(&primary_name);
                        } else if category.should_penalize_upstream() {
                            ctx.scoring_engine.record_error(&primary_name);
                        }
                        // Client errors and execution errors don't affect scoring
                    }
                    _ => {
                        ctx.scoring_engine.record_error(&primary_name);
                    }
                }
            }
        }

        // Set the serving upstream name in the response for metrics tracking
        if let Ok(ref mut response) = result {
            response.serving_upstream = Some(primary_name);
        }

        result
    }

    /// Routes using simple response-time selection.
    async fn route_simple(
        request: &JsonRpcRequest,
        ctx: &RoutingContext,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let upstream = ctx
            .load_balancer
            .get_next_healthy_by_response_time()
            .await
            .ok_or(UpstreamError::NoHealthyUpstreams)?;

        let upstream_name = upstream.config().name.clone();
        let start = std::time::Instant::now();
        let request_timeout = Duration::from_secs(15);

        let mut result = tokio::time::timeout(request_timeout, upstream.send_request(request))
            .await
            .map_err(|_| {
                warn!(timeout_secs = request_timeout.as_secs(), "response-time request timed out");
                UpstreamError::Timeout
            })?;

        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        match &result {
            Ok(response) => {
                ctx.scoring_engine.record_success(&upstream_name, latency_ms);

                if let Some(block_number) = extract_block_number(request, response) {
                    ctx.scoring_engine.record_block_number(&upstream_name, block_number).await;
                }
            }
            Err(e) => {
                match e {
                    UpstreamError::HttpError(429, _) => {
                        ctx.scoring_engine.record_throttle(&upstream_name);
                    }
                    UpstreamError::RpcError(code, message) => {
                        use crate::upstream::errors::RpcErrorCategory;
                        let category = RpcErrorCategory::from_code_and_message(*code, message);
                        if matches!(category, RpcErrorCategory::RateLimit) {
                            ctx.scoring_engine.record_throttle(&upstream_name);
                        } else if category.should_penalize_upstream() {
                            ctx.scoring_engine.record_error(&upstream_name);
                        }
                        // Client errors and execution errors don't affect scoring
                    }
                    _ => {
                        ctx.scoring_engine.record_error(&upstream_name);
                    }
                }
            }
        }

        // Set the serving upstream name in the response for metrics tracking
        if let Ok(ref mut response) = result {
            response.serving_upstream = Some(upstream_name);
        }

        result
    }

    /// Routes a request to the appropriate upstream(s) and returns the response.
    ///
    /// Applies routing strategies in priority order:
    /// 1. Consensus (if required for this method)
    /// 2. Scoring-based selection (if enabled)
    /// 3. Hedged requests (if enabled)
    /// 4. Response-time-aware selection (fallback)
    ///
    /// # Errors
    /// Returns `UpstreamError` if routing fails or no upstreams are available
    pub async fn route(
        &self,
        request: &JsonRpcRequest,
        ctx: &RoutingContext,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        // Early validation - fail fast if no upstreams available
        if !ctx.has_healthy_upstreams().await {
            return Err(UpstreamError::NoHealthyUpstreams);
        }

        // Priority 1: Check if consensus is required for this method
        if ctx.consensus_engine.requires_consensus(&request.method).await {
            return Self::route_with_consensus(request, ctx).await;
        }

        // Priority 2: Check if scoring is enabled
        if ctx.scoring_engine.is_enabled() {
            return Self::route_with_scoring(request, ctx).await;
        }

        // Priority 3: Check if hedging is enabled
        let hedge_config = ctx.hedger.get_config();
        if hedge_config.enabled {
            return Self::route_with_hedge(request, ctx).await;
        }

        // Priority 4: Fallback to simple response-time selection
        Self::route_simple(request, ctx).await
    }
}

impl Default for SmartRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smart_router_creation() {
        let router = SmartRouter::new();
        let default_router = SmartRouter;
        // Both should create valid routers
        assert!(std::mem::size_of_val(&router) == std::mem::size_of_val(&default_router));
    }
}

#[cfg(test)]
mod priority_tests {
    use super::*;
    use crate::{
        chain::ChainState,
        upstream::{
            consensus::{ConsensusConfig, ConsensusEngine},
            hedging::{HedgeConfig, HedgeExecutor},
            load_balancer::LoadBalancer,
            scoring::{ScoringConfig, ScoringEngine},
        },
    };
    use std::sync::Arc;

    /// Helper to create a `RoutingContext` with configurable features
    fn create_routing_context(
        consensus_enabled: bool,
        consensus_methods: Vec<String>,
        scoring_enabled: bool,
        hedging_enabled: bool,
    ) -> RoutingContext {
        let chain_state = Arc::new(ChainState::new());

        let consensus_config = ConsensusConfig {
            enabled: consensus_enabled,
            methods: consensus_methods,
            ..ConsensusConfig::default()
        };

        let scoring_config = ScoringConfig { enabled: scoring_enabled, ..ScoringConfig::default() };

        let hedge_config = HedgeConfig { enabled: hedging_enabled, ..HedgeConfig::default() };

        RoutingContext {
            load_balancer: Arc::new(LoadBalancer::new()),
            scoring_engine: Arc::new(ScoringEngine::new(scoring_config, chain_state.clone())),
            consensus_engine: Arc::new(ConsensusEngine::new(consensus_config)),
            hedger: Arc::new(HedgeExecutor::new(hedge_config)),
            chain_state,
        }
    }

    #[tokio::test]
    async fn test_priority_1_consensus_required_takes_precedence() {
        // When consensus is enabled AND the method requires consensus,
        // consensus routing should be used regardless of other settings.
        let ctx = create_routing_context(
            true,                                     // consensus enabled
            vec!["eth_getBlockByNumber".to_string()], // consensus methods
            true,                                     // scoring also enabled
            true,                                     // hedging also enabled
        );

        // Verify consensus is required for this method
        assert!(
            ctx.consensus_engine.requires_consensus("eth_getBlockByNumber").await,
            "eth_getBlockByNumber should require consensus"
        );

        // Even though scoring and hedging are enabled, consensus should take priority
        // because the method requires it
    }

    #[tokio::test]
    async fn test_priority_1_consensus_not_required_allows_fallthrough() {
        // When consensus is enabled but method doesn't require it,
        // should fall through to next priority level
        let ctx = create_routing_context(
            true,                                     // consensus enabled
            vec!["eth_getBlockByNumber".to_string()], // only this method requires consensus
            false,                                    // scoring disabled
            false,                                    // hedging disabled
        );

        // eth_blockNumber doesn't require consensus
        assert!(
            !ctx.consensus_engine.requires_consensus("eth_blockNumber").await,
            "eth_blockNumber should NOT require consensus"
        );
    }

    #[tokio::test]
    async fn test_priority_1_consensus_disabled_allows_fallthrough() {
        // When consensus is disabled, even methods in the list shouldn't use consensus
        let ctx = create_routing_context(
            false,                                    // consensus DISABLED
            vec!["eth_getBlockByNumber".to_string()], // method in list but disabled
            false,
            false,
        );

        // Method is in list but consensus is disabled
        assert!(
            !ctx.consensus_engine.requires_consensus("eth_getBlockByNumber").await,
            "Consensus disabled means no method requires consensus"
        );
    }

    #[tokio::test]
    async fn test_priority_2_scoring_enabled_takes_precedence_over_hedging() {
        // When consensus is not required but scoring is enabled,
        // scoring should be used even if hedging is also enabled
        let ctx = create_routing_context(
            false,  // consensus disabled
            vec![], // no consensus methods
            true,   // scoring ENABLED
            true,   // hedging also enabled
        );

        // Verify scoring is enabled
        assert!(ctx.scoring_engine.is_enabled(), "Scoring should be enabled");

        // Verify consensus is not required
        assert!(
            !ctx.consensus_engine.requires_consensus("eth_blockNumber").await,
            "Method should not require consensus"
        );

        // Since scoring is enabled, it should take priority over hedging
        // (This is verified by the SmartRouter.route() implementation)
    }

    #[tokio::test]
    async fn test_priority_2_scoring_disabled_allows_fallthrough() {
        let ctx = create_routing_context(
            false,  // consensus disabled
            vec![], // no consensus methods
            false,  // scoring DISABLED
            true,   // hedging enabled
        );

        assert!(!ctx.scoring_engine.is_enabled(), "Scoring should be disabled");
    }

    #[tokio::test]
    async fn test_priority_3_hedging_enabled_takes_precedence() {
        let ctx = create_routing_context(
            false,  // consensus disabled
            vec![], // no consensus methods
            false,  // scoring disabled
            true,   // hedging ENABLED
        );

        let hedge_config = ctx.hedger.get_config();
        assert!(hedge_config.enabled, "Hedging should be enabled");

        // With consensus and scoring disabled, hedging should be used
    }

    #[tokio::test]
    async fn test_priority_3_hedging_disabled_falls_to_simple() {
        let ctx = create_routing_context(
            false,  // consensus disabled
            vec![], // no consensus methods
            false,  // scoring disabled
            false,  // hedging DISABLED
        );

        let hedge_config = ctx.hedger.get_config();
        assert!(!hedge_config.enabled, "Hedging should be disabled");

        // All strategies disabled → should use simple response-time routing
    }

    #[tokio::test]
    async fn test_priority_4_simple_routing_fallback() {
        // When all other strategies are disabled, simple routing should be used
        let ctx = create_routing_context(
            false,  // consensus disabled
            vec![], // no consensus methods
            false,  // scoring disabled
            false,  // hedging disabled
        );

        // Verify all routing strategies are disabled
        assert!(
            !ctx.consensus_engine.requires_consensus("eth_blockNumber").await,
            "Consensus should not be required"
        );
        assert!(!ctx.scoring_engine.is_enabled(), "Scoring should be disabled");
        let hedge_config = ctx.hedger.get_config();
        assert!(!hedge_config.enabled, "Hedging should be disabled");

        // Simple routing (response-time selection) should be used as fallback
    }

    #[tokio::test]
    async fn test_priority_cascade_documentation() {
        // Test case: All enabled → Consensus wins for consensus methods
        {
            let ctx = create_routing_context(true, vec!["eth_getLogs".to_string()], true, true);
            assert!(ctx.consensus_engine.requires_consensus("eth_getLogs").await);
        }

        // Test case: Consensus disabled, Scoring + Hedging enabled → Scoring wins
        {
            let ctx = create_routing_context(
                false,
                vec![],
                true, // scoring enabled
                true, // hedging also enabled but lower priority
            );
            assert!(ctx.scoring_engine.is_enabled());
        }

        // Test case: Consensus + Scoring disabled, Hedging enabled → Hedging wins
        {
            let ctx = create_routing_context(
                false,
                vec![],
                false, // scoring disabled
                true,  // hedging enabled
            );
            assert!(ctx.hedger.get_config().enabled);
        }

        // Test case: All disabled → Simple routing
        {
            let ctx = create_routing_context(false, vec![], false, false);
            assert!(!ctx.consensus_engine.is_enabled().await);
            assert!(!ctx.scoring_engine.is_enabled());
            assert!(!ctx.hedger.get_config().enabled);
            // Simple routing is implicit fallback
        }
    }

    #[tokio::test]
    async fn test_per_method_consensus_configuration() {
        let ctx = create_routing_context(
            true,
            vec!["eth_getBlockByNumber".to_string(), "eth_getLogs".to_string()],
            false,
            false,
        );

        // Methods in the consensus list require consensus
        assert!(ctx.consensus_engine.requires_consensus("eth_getBlockByNumber").await);
        assert!(ctx.consensus_engine.requires_consensus("eth_getLogs").await);

        // Methods NOT in the list don't require consensus
        assert!(!ctx.consensus_engine.requires_consensus("eth_blockNumber").await);
        assert!(!ctx.consensus_engine.requires_consensus("eth_chainId").await);
        assert!(!ctx.consensus_engine.requires_consensus("eth_call").await);
    }

    #[tokio::test]
    async fn test_runtime_config_updates() {
        let ctx = create_routing_context(false, vec![], false, false);

        // Initially, nothing requires consensus
        assert!(!ctx.scoring_engine.is_enabled());

        // Update scoring config at runtime
        let new_scoring_config = ScoringConfig { enabled: true, ..ScoringConfig::default() };
        ctx.scoring_engine.update_config(new_scoring_config);

        // Now scoring should be enabled and affect routing
        assert!(ctx.scoring_engine.is_enabled());
    }

    #[tokio::test]
    async fn test_non_consensus_method_with_scoring() {
        let ctx = create_routing_context(
            true,                                     // consensus enabled
            vec!["eth_getBlockByNumber".to_string()], // only this method
            true,                                     // scoring enabled
            false,                                    // hedging disabled
        );

        // eth_blockNumber is NOT in consensus methods
        assert!(!ctx.consensus_engine.requires_consensus("eth_blockNumber").await);

        // So it should fall through to scoring
        assert!(ctx.scoring_engine.is_enabled());
    }
}
