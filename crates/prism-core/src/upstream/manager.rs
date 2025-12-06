use super::{
    circuit_breaker::CircuitBreakerState,
    consensus::{ConsensusConfig, ConsensusEngine},
    router::{RoutingContext, SmartRouter},
    runtime_registry::{RuntimeUpstreamConfig, RuntimeUpstreamRegistry, RuntimeUpstreamUpdate},
    scoring::{ScoringConfig, ScoringEngine, UpstreamScore},
    HedgeConfig, HedgeExecutor, LoadBalancer, UpstreamEndpoint, UpstreamError,
};
use crate::types::{JsonRpcRequest, JsonRpcResponse, UpstreamConfig};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Manages upstream RPC endpoints with load balancing, retries, and circuit breaking.
///
/// `UpstreamManager` coordinates multiple upstream endpoints through a `LoadBalancer`,
/// handling request routing via pluggable strategies, health monitoring, circuit breaker
/// management, hedged requests, multi-factor scoring for optimal upstream selection,
/// and consensus-based data validation. Thread-safe and designed for concurrent access.
///
/// # Architecture
///
/// The manager uses a `SmartRouter` for request routing with:
/// - `RoutingContext` providing shared state for routing decisions
/// - Adaptive routing based on consensus, scoring, hedging, and response-time strategies
///
/// # Construction
///
/// Use `UpstreamManagerBuilder` for flexible configuration:
///
/// ```no_run
/// # use prism_core::{upstream::UpstreamManagerBuilder, chain::ChainState};
/// # use std::sync::Arc;
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let chain_state = Arc::new(ChainState::new());
///
/// let manager = UpstreamManagerBuilder::new()
///     .chain_state(chain_state)
///     .concurrency_limit(1000)
///     .enable_scoring()
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct UpstreamManager {
    load_balancer: Arc<LoadBalancer>,
    config: Arc<RwLock<UpstreamManagerConfig>>,
    http_client: Arc<super::http_client::HttpClient>,
    hedge_executor: Arc<HedgeExecutor>,
    scoring_engine: Arc<ScoringEngine>,
    consensus_engine: Arc<ConsensusEngine>,
    routing_context: Arc<RoutingContext>,
    router: Arc<SmartRouter>,
    runtime_registry: Arc<RuntimeUpstreamRegistry>,
}

/// Configuration for the `UpstreamManager`.
#[derive(Debug, Clone)]
pub struct UpstreamManagerConfig {
    /// Maximum number of retry attempts for failed requests
    pub max_retries: u32,
    /// Delay in milliseconds between retry attempts
    pub retry_delay_ms: u64,
    /// Number of failures before opening circuit breaker
    pub circuit_breaker_threshold: u32,
    /// Seconds to wait before attempting to close an open circuit breaker
    pub circuit_breaker_timeout_seconds: u64,
    /// Configuration for graceful degradation when all upstreams are unhealthy
    pub unhealthy_behavior: UnhealthyBehaviorConfig,
}

/// Configuration for handling requests when all upstreams are unhealthy.
#[derive(Debug, Clone)]
pub struct UnhealthyBehaviorConfig {
    /// Whether to retry when all upstreams are unhealthy
    pub enabled: bool,
    /// Maximum number of retries when all upstreams are unhealthy
    pub max_retries: u32,
    /// Base delay in milliseconds between retries (uses exponential backoff with jitter)
    pub base_delay_ms: u64,
    /// Maximum delay in milliseconds between retries
    pub max_delay_ms: u64,
    /// Jitter factor (0.0-1.0) added to delays to prevent thundering herd
    pub jitter_factor: f64,
}

impl Default for UnhealthyBehaviorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_retries: 3,
            base_delay_ms: 100,
            max_delay_ms: 2000,
            jitter_factor: 0.25,
        }
    }
}

impl Default for UpstreamManagerConfig {
    fn default() -> Self {
        Self {
            max_retries: 1,
            retry_delay_ms: 1000,
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 60,
            unhealthy_behavior: UnhealthyBehaviorConfig::default(),
        }
    }
}

impl UpstreamManager {
    /// Creates a new `UpstreamManager` with router and all dependencies.
    ///
    /// This is the internal constructor used by `UpstreamManagerBuilder`.
    /// For public construction, use `UpstreamManagerBuilder` instead.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_with_router(
        load_balancer: Arc<LoadBalancer>,
        config: Arc<RwLock<UpstreamManagerConfig>>,
        http_client: Arc<super::http_client::HttpClient>,
        hedge_executor: Arc<HedgeExecutor>,
        scoring_engine: Arc<ScoringEngine>,
        consensus_engine: Arc<ConsensusEngine>,
        routing_context: Arc<RoutingContext>,
        router: Arc<SmartRouter>,
        runtime_registry: Arc<RuntimeUpstreamRegistry>,
    ) -> Self {
        Self {
            load_balancer,
            config,
            http_client,
            hedge_executor,
            scoring_engine,
            consensus_engine,
            routing_context,
            router,
            runtime_registry,
        }
    }

    /// Adds a new upstream endpoint to the load balancer.
    pub fn add_upstream(&self, config: UpstreamConfig) {
        info!(name = %config.name, url = %config.url, "adding upstream endpoint");
        self.load_balancer.add_upstream(config, self.http_client.clone());
    }

    /// Removes an upstream endpoint by name.
    ///
    /// This also cleans up metrics and latency tracking data.
    pub fn remove_upstream(&self, name: &str) {
        info!(name = %name, "removing upstream endpoint");
        self.load_balancer.remove_upstream(name);

        // Clean up tracking data
        self.scoring_engine.remove_metrics(name);
        self.hedge_executor.remove_latency_tracker(name);
    }

    /// Updates an upstream endpoint's configuration atomically.
    ///
    /// This preserves the upstream's position in the list and performs an atomic swap.
    /// Returns true if the upstream was found and updated, false otherwise.
    pub fn update_upstream(&self, name: &str, config: UpstreamConfig) -> bool {
        info!(name = %name, url = %config.url, weight = config.weight, "updating upstream endpoint");
        self.load_balancer.update_upstream(name, config, self.http_client.clone())
    }

    /// Returns all upstream endpoints, both healthy and unhealthy.
    ///
    /// # Ownership
    ///
    /// Returns `Arc<Vec<Arc<UpstreamEndpoint>>>` where:
    /// - Outer `Arc<Vec<...>>`: Snapshot of current upstream list (cheap to clone)
    /// - Inner `Arc<UpstreamEndpoint>`: Individual endpoints shared across components
    ///
    /// This structure enables lock-free reads via `ArcSwap`. The outer Arc allows
    /// atomic swaps of the entire list, while inner Arcs enable sharing endpoints.
    #[must_use]
    pub fn get_all_upstreams(&self) -> Arc<Vec<Arc<UpstreamEndpoint>>> {
        self.load_balancer.get_all_upstreams()
    }

    /// Returns only the healthy upstream endpoints.
    #[must_use]
    pub async fn get_healthy_upstreams(&self) -> Vec<Arc<UpstreamEndpoint>> {
        self.load_balancer.get_healthy_upstreams().await
    }

    /// Returns current load balancer statistics including upstream counts and response times.
    pub async fn get_stats(&self) -> super::LoadBalancerStats {
        self.load_balancer.get_stats().await
    }

    /// Updates the manager's configuration at runtime.
    pub async fn update_config(&self, config: UpstreamManagerConfig) {
        let mut current_config = self.config.write().await;
        *current_config = config;
        info!("upstream manager configuration updated");
    }

    /// Returns a copy of the current configuration.
    pub async fn get_config(&self) -> UpstreamManagerConfig {
        self.config.read().await.clone()
    }

    /// Checks whether any healthy upstreams are currently available.
    pub async fn has_healthy_upstreams(&self) -> bool {
        !self.load_balancer.get_healthy_upstreams().await.is_empty()
    }

    /// Returns the total number of registered upstreams.
    #[must_use]
    pub fn total_upstreams(&self) -> usize {
        self.load_balancer.get_all_upstreams().len()
    }

    /// Returns the number of currently healthy upstreams.
    pub async fn healthy_upstreams(&self) -> usize {
        self.load_balancer.get_healthy_upstreams().await.len()
    }

    /// Sends a request using the configured routing strategy.
    ///
    /// Delegates to the router implementation (consensus, scoring, hedging, or simple).
    /// When all upstreams are unhealthy and `unhealthy_behavior.enabled` is true,
    /// retries with exponential backoff and jitter to prevent thundering herd.
    ///
    /// # Errors
    ///
    /// Returns [`UpstreamError::NoHealthyUpstreams`] if no upstreams are available after retries.
    /// Returns [`UpstreamError::RpcError`] if the upstream returns a JSON-RPC error response.
    /// Returns [`UpstreamError::RequestFailed`] if network or HTTP errors occur.
    /// Returns [`UpstreamError::Timeout`] if the request exceeds the configured timeout.
    pub async fn send_request_auto(
        &self,
        request: &JsonRpcRequest,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        let result = self.router.route(request, &self.routing_context).await;

        // If we got NoHealthyUpstreams and retry is enabled, attempt graceful degradation
        if matches!(result, Err(UpstreamError::NoHealthyUpstreams)) {
            let config = self.config.read().await;
            let unhealthy_config = &config.unhealthy_behavior;

            if unhealthy_config.enabled {
                drop(config); // Release lock before async retry loop
                return self.retry_with_backoff(request).await;
            }
        }

        result
    }

    /// Retries a request with jittered exponential backoff when all upstreams are unhealthy.
    async fn retry_with_backoff(
        &self,
        request: &JsonRpcRequest,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        use rand::Rng;
        use std::time::Duration;

        let config = self.config.read().await;
        let unhealthy_config = config.unhealthy_behavior.clone();
        drop(config); // Release lock before retry loop

        let mut attempt = 0u32;

        loop {
            attempt += 1;

            let base_delay = unhealthy_config.base_delay_ms * (1u64 << attempt.min(10));
            let capped_delay = base_delay.min(unhealthy_config.max_delay_ms);

            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let jitter_range = (capped_delay as f64 * unhealthy_config.jitter_factor) as u64;
            let jitter_offset = if jitter_range > 0 {
                rand::rng().random_range(0..jitter_range)
            } else {
                0
            };
            let delay = Duration::from_millis(
                capped_delay.saturating_sub(jitter_range / 2) + jitter_offset,
            );

            tracing::debug!(
                attempt = attempt,
                max_retries = unhealthy_config.max_retries,
                delay_ms = delay.as_millis(),
                "waiting for healthy upstream (all unhealthy)"
            );

            tokio::time::sleep(delay).await;

            let result = self.router.route(request, &self.routing_context).await;

            match &result {
                Ok(_) => {
                    tracing::info!(
                        attempt = attempt,
                        "request succeeded after waiting for healthy upstream"
                    );
                    return result;
                }
                Err(UpstreamError::NoHealthyUpstreams) => {
                    if attempt >= unhealthy_config.max_retries {
                        tracing::warn!(
                            attempts = attempt,
                            "all upstreams remain unhealthy after max retries"
                        );
                        return result;
                    }
                }
                Err(_) => {
                    return result;
                }
            }
        }
    }

    /// Returns the circuit breaker status for all upstreams.
    pub async fn get_circuit_breaker_status(&self) -> Vec<(Arc<str>, CircuitBreakerState, u32)> {
        let upstreams = self.get_all_upstreams();
        let mut status = Vec::new();

        for upstream in upstreams.iter() {
            let name = upstream.config().name.clone();
            let state = upstream.get_circuit_breaker_state().await;
            let failure_count = upstream.get_circuit_breaker_failure_count().await;
            status.push((name, state, failure_count));
        }

        status
    }

    /// Resets the circuit breaker for a specific upstream.
    pub async fn reset_circuit_breaker(&self, upstream_name: &str) -> bool {
        let upstreams = self.get_all_upstreams();

        for upstream in upstreams.iter() {
            if upstream.config().name.as_ref() == upstream_name {
                upstream.circuit_breaker().on_success().await;
                info!(name = %upstream_name, "circuit breaker reset");
                return true;
            }
        }

        tracing::warn!(name = %upstream_name, "upstream not found for circuit breaker reset");
        false
    }

    /// Returns the shared `HttpClient` instance used by all upstreams.
    #[must_use]
    pub fn get_http_client(&self) -> Arc<super::http_client::HttpClient> {
        self.http_client.clone()
    }

    /// Updates the hedging configuration at runtime.
    pub fn update_hedge_config(&self, hedge_config: HedgeConfig) {
        self.hedge_executor.update_config(hedge_config);
        info!("hedging configuration updated");
    }

    /// Returns the current hedging configuration.
    #[must_use]
    pub fn get_hedge_config(&self) -> HedgeConfig {
        self.hedge_executor.get_config()
    }

    /// Returns latency statistics (P50, P95, P99, average) in milliseconds.
    #[must_use]
    pub fn get_upstream_latency_stats(&self, upstream_name: &str) -> Option<(u64, u64, u64, u64)> {
        self.hedge_executor.get_latency_stats(upstream_name)
    }

    /// Returns a reference to the hedge executor for advanced use cases.
    #[must_use]
    pub fn get_hedge_executor(&self) -> Arc<HedgeExecutor> {
        self.hedge_executor.clone()
    }

    #[must_use]
    pub fn get_scoring_engine(&self) -> Arc<ScoringEngine> {
        self.scoring_engine.clone()
    }

    /// Updates the scoring configuration at runtime.
    pub fn update_scoring_config(&self, scoring_config: ScoringConfig) {
        self.scoring_engine.update_config(scoring_config);
        info!("scoring configuration updated");
    }

    /// Returns the current scoring configuration.
    #[must_use]
    pub fn get_scoring_config(&self) -> ScoringConfig {
        self.scoring_engine.get_config()
    }

    /// Returns whether scoring-based selection is enabled.
    #[must_use]
    pub fn is_scoring_enabled(&self) -> bool {
        self.scoring_engine.is_enabled()
    }

    /// Returns the score for a specific upstream if sufficient data is available.
    #[must_use]
    pub fn get_upstream_score(&self, upstream_name: &str) -> Option<UpstreamScore> {
        self.scoring_engine.get_score(upstream_name)
    }

    /// Returns ranked list of all upstreams by composite score (best first).
    #[must_use]
    pub fn get_ranked_upstreams(&self) -> Vec<(String, f64)> {
        self.scoring_engine.get_ranked_upstreams()
    }

    /// Records a successful request for scoring purposes.
    ///
    /// Lock-free operation - no async overhead.
    pub fn record_scoring_success(&self, upstream_name: &str, latency_ms: u64) {
        self.scoring_engine.record_success(upstream_name, latency_ms);
    }

    /// Records an error for scoring purposes.
    ///
    /// Lock-free operation - no async overhead.
    pub fn record_scoring_error(&self, upstream_name: &str) {
        self.scoring_engine.record_error(upstream_name);
    }

    /// Records a throttle response for scoring purposes.
    ///
    /// Lock-free operation - no async overhead.
    pub fn record_scoring_throttle(&self, upstream_name: &str) {
        self.scoring_engine.record_throttle(upstream_name);
    }

    /// Records a block number from an upstream for lag tracking.
    pub async fn record_block_number(&self, upstream_name: &str, block_number: u64) {
        self.scoring_engine.record_block_number(upstream_name, block_number).await;
    }

    /// Records a finalized block number, updating shared `ChainState`.
    pub async fn record_finalized_block(&self, _upstream_name: &str, finalized_block: u64) {
        self.scoring_engine.chain_state().update_finalized(finalized_block).await;
    }

    #[must_use]
    pub fn get_chain_tip(&self) -> u64 {
        self.scoring_engine.chain_tip()
    }

    #[must_use]
    pub fn get_consensus_engine(&self) -> Arc<ConsensusEngine> {
        self.consensus_engine.clone()
    }

    /// Updates the consensus configuration at runtime.
    pub async fn update_consensus_config(&self, consensus_config: ConsensusConfig) {
        self.consensus_engine.update_config(consensus_config).await;
        info!("consensus configuration updated");
    }

    /// Returns the current consensus configuration.
    pub async fn get_consensus_config(&self) -> ConsensusConfig {
        self.consensus_engine.get_config().await
    }

    /// Returns whether consensus is enabled.
    pub async fn is_consensus_enabled(&self) -> bool {
        self.consensus_engine.is_enabled().await
    }

    /// Returns a reference to the runtime upstream registry.
    #[must_use]
    pub fn get_runtime_registry(&self) -> Arc<RuntimeUpstreamRegistry> {
        self.runtime_registry.clone()
    }

    /// Adds a runtime upstream and registers it in the runtime registry.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The upstream name conflicts with an existing upstream
    /// - Validation fails (invalid URL, weight out of range, etc.)
    /// - Registration in the runtime registry fails
    pub fn add_runtime_upstream(
        &self,
        request: CreateUpstreamRequest,
    ) -> Result<RuntimeUpstreamConfig, UpstreamError> {
        // Validate request
        validate_upstream_request(&request)?;

        // Generate ID
        let id = uuid::Uuid::new_v4().to_string();

        // Create runtime upstream config
        let now = chrono::Utc::now().to_rfc3339();
        let runtime_config = RuntimeUpstreamConfig {
            id: id.clone(),
            name: request.name.clone(),
            url: request.url.clone(),
            ws_url: request.ws_url.clone(),
            weight: request.weight,
            chain_id: request.chain_id,
            timeout_seconds: request.timeout_seconds,
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
        };

        // Register in runtime registry
        self.runtime_registry
            .add(&runtime_config)
            .map_err(UpstreamError::InvalidRequest)?;

        // Create UpstreamConfig for load balancer
        let upstream_config = UpstreamConfig {
            name: Arc::from(request.name.as_str()),
            url: request.url,
            ws_url: request.ws_url,
            weight: request.weight,
            chain_id: request.chain_id,
            timeout_seconds: request.timeout_seconds,
            supports_websocket: false,           // TODO: detect from ws_url
            circuit_breaker_threshold: 5,        // Default
            circuit_breaker_timeout_seconds: 60, // Default
        };

        // Add to load balancer
        self.add_upstream(upstream_config);

        info!(id = %id, name = %runtime_config.name, "added runtime upstream");
        Ok(runtime_config)
    }

    /// Updates a runtime upstream.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The upstream is a config-based upstream (cannot modify)
    /// - The upstream ID doesn't exist
    /// - Validation fails
    /// - Update fails
    pub fn update_runtime_upstream(
        &self,
        id: &str,
        updates: &UpdateUpstreamRequest,
    ) -> Result<RuntimeUpstreamConfig, UpstreamError> {
        // Get current config
        let current = self.runtime_registry.get(id).ok_or_else(|| {
            UpstreamError::InvalidRequest(format!("Runtime upstream '{id}' not found"))
        })?;

        // Validate updates
        validate_upstream_updates(updates)?;

        // Build update struct for registry
        let mut registry_update = RuntimeUpstreamUpdate::default();

        if let Some(ref name) = updates.name {
            registry_update.name = Some(name.clone());
        }
        if let Some(ref url) = updates.url {
            registry_update.url = Some(url.clone());
        }
        if let Some(weight) = updates.weight {
            registry_update.weight = Some(weight);
        }
        if let Some(enabled) = updates.enabled {
            registry_update.enabled = Some(enabled);
        }

        // Update in registry
        self.runtime_registry
            .update(id, registry_update)
            .map_err(UpstreamError::InvalidRequest)?;

        // Get updated config
        let updated_config = self.runtime_registry.get(id).ok_or_else(|| {
            UpstreamError::InvalidRequest("Failed to retrieve updated config".to_string())
        })?;

        // If name changed, we need to remove old and add new
        if updates.name.is_some() && updates.name.as_ref() != Some(&current.name) {
            self.remove_upstream(&current.name);

            let upstream_config = UpstreamConfig {
                name: Arc::from(updated_config.name.as_str()),
                url: updated_config.url.clone(),
                ws_url: updated_config.ws_url.clone(),
                weight: updated_config.weight,
                chain_id: updated_config.chain_id,
                timeout_seconds: updated_config.timeout_seconds,
                supports_websocket: updated_config.ws_url.is_some(),
                circuit_breaker_threshold: 5,
                circuit_breaker_timeout_seconds: 60,
            };

            self.add_upstream(upstream_config);
        } else {
            // Update existing upstream
            // For now, we recreate it
            self.remove_upstream(&updated_config.name);

            let upstream_config = UpstreamConfig {
                name: Arc::from(updated_config.name.as_str()),
                url: updated_config.url.clone(),
                ws_url: updated_config.ws_url.clone(),
                weight: updated_config.weight,
                chain_id: updated_config.chain_id,
                timeout_seconds: updated_config.timeout_seconds,
                supports_websocket: updated_config.ws_url.is_some(),
                circuit_breaker_threshold: 5,
                circuit_breaker_timeout_seconds: 60,
            };

            self.add_upstream(upstream_config);
        }

        info!(id = %id, name = %updated_config.name, "updated runtime upstream");
        Ok(updated_config)
    }

    /// Removes a runtime upstream.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The upstream is a config-based upstream (cannot remove)
    /// - The upstream ID doesn't exist
    pub fn remove_runtime_upstream(&self, id: &str) -> Result<(), UpstreamError> {
        // Get the config to check it exists and get the name
        let config = self.runtime_registry.get(id).ok_or_else(|| {
            UpstreamError::InvalidRequest(format!("Runtime upstream '{id}' not found"))
        })?;

        // Remove from registry
        self.runtime_registry.remove(id).map_err(UpstreamError::InvalidRequest)?;

        // Remove from load balancer
        self.remove_upstream(&config.name);

        info!(id = %id, name = %config.name, "removed runtime upstream");
        Ok(())
    }

    /// Gets a runtime upstream by ID.
    #[must_use]
    pub fn get_runtime_upstream(&self, id: &str) -> Option<RuntimeUpstreamConfig> {
        self.runtime_registry.get(id)
    }

    /// Lists all runtime upstreams.
    #[must_use]
    pub fn list_runtime_upstreams(&self) -> Vec<RuntimeUpstreamConfig> {
        self.runtime_registry.list_all()
    }
}

/// Request to create a new runtime upstream.
#[derive(Debug, Clone)]
pub struct CreateUpstreamRequest {
    pub name: String,
    pub url: String,
    pub ws_url: Option<String>,
    pub weight: u32,
    pub chain_id: u64,
    pub timeout_seconds: u64,
}

/// Request to update a runtime upstream.
#[derive(Debug, Clone, Default)]
pub struct UpdateUpstreamRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub weight: Option<u32>,
    pub enabled: Option<bool>,
}

/// Validates a create upstream request.
fn validate_upstream_request(request: &CreateUpstreamRequest) -> Result<(), UpstreamError> {
    // Validate name
    if request.name.is_empty() || request.name.len() > 100 {
        return Err(UpstreamError::InvalidRequest("Name must be 1-100 characters".to_string()));
    }

    // Check name format (alphanumeric, dash, underscore)
    if !request.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err(UpstreamError::InvalidRequest(
            "Name must contain only alphanumeric characters, hyphens, and underscores".to_string(),
        ));
    }

    // Validate URL
    if url::Url::parse(&request.url).is_err() {
        return Err(UpstreamError::InvalidRequest("Invalid URL".to_string()));
    }

    // Validate weight
    if request.weight == 0 || request.weight > 1000 {
        return Err(UpstreamError::InvalidRequest("Weight must be between 1 and 1000".to_string()));
    }

    Ok(())
}

/// Validates an update upstream request.
fn validate_upstream_updates(updates: &UpdateUpstreamRequest) -> Result<(), UpstreamError> {
    // Validate name if present
    if let Some(ref name) = updates.name {
        if name.is_empty() || name.len() > 100 {
            return Err(UpstreamError::InvalidRequest("Name must be 1-100 characters".to_string()));
        }

        if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            return Err(UpstreamError::InvalidRequest(
                "Name must contain only alphanumeric characters, hyphens, and underscores"
                    .to_string(),
            ));
        }
    }

    // Validate URL if present
    if let Some(ref url) = updates.url {
        if url::Url::parse(url).is_err() {
            return Err(UpstreamError::InvalidRequest("Invalid URL".to_string()));
        }
    }

    // Validate weight if present
    if let Some(weight) = updates.weight {
        if weight == 0 || weight > 1000 {
            return Err(UpstreamError::InvalidRequest(
                "Weight must be between 1 and 1000".to_string(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{chain::ChainState, upstream::UpstreamManagerBuilder};

    #[tokio::test]
    async fn test_upstream_manager_creation() {
        let chain_state = Arc::new(ChainState::new());
        let manager = UpstreamManagerBuilder::new().chain_state(chain_state).build().unwrap();
        assert_eq!(manager.total_upstreams(), 0);
        assert_eq!(manager.healthy_upstreams().await, 0);
    }

    #[tokio::test]
    async fn test_upstream_manager_with_custom_config() {
        let config = UpstreamManagerConfig {
            max_retries: 3,
            retry_delay_ms: 500,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_seconds: 120,
            unhealthy_behavior: UnhealthyBehaviorConfig::default(),
        };

        let chain_state = Arc::new(ChainState::new());
        let manager = UpstreamManagerBuilder::new()
            .chain_state(chain_state)
            .config(config.clone())
            .build()
            .unwrap();

        let retrieved_config = manager.get_config().await;

        assert_eq!(retrieved_config.max_retries, 3);
        assert_eq!(retrieved_config.retry_delay_ms, 500);
    }

    #[tokio::test]
    async fn test_upstream_manager_config_update() {
        let chain_state = Arc::new(ChainState::new());
        let manager = UpstreamManagerBuilder::new().chain_state(chain_state).build().unwrap();

        let initial_config = manager.get_config().await;
        assert_eq!(initial_config.max_retries, 1);

        let new_config = UpstreamManagerConfig { max_retries: 5, ..initial_config };

        manager.update_config(new_config).await;

        let updated_config = manager.get_config().await;
        assert_eq!(updated_config.max_retries, 5);
    }
}
