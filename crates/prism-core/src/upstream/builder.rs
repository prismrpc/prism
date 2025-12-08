//! Builder pattern for constructing `UpstreamManager` with flexible configuration.

use super::{
    consensus::{ConsensusConfig, ConsensusEngine},
    dynamic_registry::DynamicUpstreamRegistry,
    hedging::{HedgeConfig, HedgeExecutor},
    http_client::HttpClient,
    load_balancer::LoadBalancer,
    manager::{UpstreamManager, UpstreamManagerConfig},
    router::{RoutingContext, SmartRouter},
    scoring::{ScoringConfig, ScoringEngine},
};
use crate::chain::ChainState;
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;
use tokio::sync::RwLock;

/// Errors that can occur during upstream manager construction.
#[derive(Debug, Error)]
pub enum BuilderError {
    /// HTTP client initialization failed
    #[error("Failed to initialize HTTP client: {0}")]
    HttpClientInit(String),

    /// `ChainState` is required but was not provided
    #[error("`ChainState` is required but was not provided")]
    MissingChainState,
}

/// Builder for constructing an `UpstreamManager`.
///
/// Provides a fluent API for configuring upstream management with
/// various routing strategies, concurrency limits, and features.
///
/// # Examples
///
/// ```no_run
/// # use prism_core::{upstream::UpstreamManagerBuilder, chain::ChainState};
/// # use std::sync::Arc;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let chain_state = Arc::new(ChainState::new());
///
/// let manager = UpstreamManagerBuilder::new()
///     .chain_state(chain_state)
///     .concurrency_limit(1000)
///     .enable_scoring()
///     .enable_hedging()
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct UpstreamManagerBuilder {
    chain_state: Option<Arc<ChainState>>,
    concurrency_limit: usize,
    config: UpstreamManagerConfig,
    scoring_config: ScoringConfig,
    hedge_config: HedgeConfig,
    consensus_config: ConsensusConfig,
    router: Option<Arc<SmartRouter>>,
    dynamic_storage_path: Option<PathBuf>,
}

impl UpstreamManagerBuilder {
    /// Creates a new builder with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            chain_state: None,
            concurrency_limit: 1000,
            config: UpstreamManagerConfig::default(),
            scoring_config: ScoringConfig::default(),
            hedge_config: HedgeConfig::default(),
            consensus_config: ConsensusConfig::default(),
            router: None,
            dynamic_storage_path: None,
        }
    }

    #[must_use]
    pub fn chain_state(mut self, chain_state: Arc<ChainState>) -> Self {
        self.chain_state = Some(chain_state);
        self
    }

    /// Sets HTTP client concurrency limit (default: 1000).
    #[must_use]
    pub fn concurrency_limit(mut self, limit: usize) -> Self {
        self.concurrency_limit = limit;
        self
    }

    #[must_use]
    pub fn config(mut self, config: UpstreamManagerConfig) -> Self {
        self.config = config;
        self
    }

    #[must_use]
    pub fn enable_scoring(mut self) -> Self {
        self.scoring_config.enabled = true;
        self
    }

    #[must_use]
    pub fn scoring_config(mut self, config: ScoringConfig) -> Self {
        self.scoring_config = config;
        self
    }

    #[must_use]
    pub fn enable_hedging(mut self) -> Self {
        self.hedge_config.enabled = true;
        self
    }

    #[must_use]
    pub fn hedge_config(mut self, config: HedgeConfig) -> Self {
        self.hedge_config = config;
        self
    }

    #[must_use]
    pub fn enable_consensus(mut self) -> Self {
        self.consensus_config.enabled = true;
        self
    }

    #[must_use]
    pub fn consensus_config(mut self, config: ConsensusConfig) -> Self {
        self.consensus_config = config;
        self
    }

    /// Sets custom `SmartRouter` instance (default: creates new `SmartRouter`).
    #[must_use]
    pub fn router(mut self, router: Arc<SmartRouter>) -> Self {
        self.router = Some(router);
        self
    }

    /// Sets the storage path for dynamic upstreams persistence.
    ///
    /// If set, dynamic upstreams will be persisted to this file and loaded on restart.
    #[must_use]
    pub fn dynamic_storage_path(mut self, path: PathBuf) -> Self {
        self.dynamic_storage_path = Some(path);
        self
    }

    /// Builds the `UpstreamManager`.
    ///
    /// # Errors
    ///
    /// Returns `BuilderError::MissingChainState` if chain state was not provided.
    /// Returns `BuilderError::HttpClientInit` if HTTP client initialization fails.
    pub fn build(self) -> Result<UpstreamManager, BuilderError> {
        let chain_state = self.chain_state.ok_or(BuilderError::MissingChainState)?;

        #[allow(clippy::expect_used)]
        let http_client = Arc::new(
            HttpClient::with_concurrency_limit(self.concurrency_limit)
                .map_err(|e| BuilderError::HttpClientInit(e.to_string()))?,
        );

        let load_balancer = Arc::new(LoadBalancer::new());
        let hedge_executor = Arc::new(HedgeExecutor::new(self.hedge_config));
        let scoring_engine = Arc::new(ScoringEngine::new(self.scoring_config, chain_state.clone()));
        let consensus_engine = Arc::new(ConsensusEngine::new(self.consensus_config));

        // Create dynamic upstream registry
        let dynamic_registry = Arc::new(DynamicUpstreamRegistry::new(self.dynamic_storage_path));

        // Create routing context
        let routing_context = Arc::new(RoutingContext::new(
            load_balancer.clone(),
            scoring_engine.clone(),
            consensus_engine.clone(),
            hedge_executor.clone(),
            chain_state,
        ));

        // Use provided router or default to SmartRouter
        let router = self.router.unwrap_or_else(|| Arc::new(SmartRouter::new()));

        Ok(UpstreamManager::new_with_router(
            load_balancer,
            Arc::new(RwLock::new(self.config)),
            http_client,
            hedge_executor,
            scoring_engine,
            consensus_engine,
            routing_context,
            router,
            dynamic_registry,
        ))
    }
}

impl Default for UpstreamManagerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_requires_chain_state() {
        let result = UpstreamManagerBuilder::new().build();
        assert!(result.is_err());
        assert!(matches!(result, Err(BuilderError::MissingChainState)));
    }

    #[test]
    fn test_builder_basic() {
        let chain_state = Arc::new(ChainState::new());
        let result = UpstreamManagerBuilder::new().chain_state(chain_state).build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_builder_with_features() {
        let chain_state = Arc::new(ChainState::new());
        let result = UpstreamManagerBuilder::new()
            .chain_state(chain_state)
            .enable_scoring()
            .enable_hedging()
            .enable_consensus()
            .concurrency_limit(500)
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_builder_with_custom_configs() {
        let chain_state = Arc::new(ChainState::new());

        let scoring_config = ScoringConfig { enabled: true, ..Default::default() };
        let hedge_config = HedgeConfig { enabled: true, ..Default::default() };

        let result = UpstreamManagerBuilder::new()
            .chain_state(chain_state)
            .scoring_config(scoring_config)
            .hedge_config(hedge_config)
            .build();

        assert!(result.is_ok());
    }
}
