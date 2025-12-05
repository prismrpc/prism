//! Builder pattern for initializing the Prism runtime with configurable components.

use crate::{
    cache::{reorg_manager::ReorgManager, CacheManager},
    chain::ChainState,
    config::AppConfig,
    metrics::MetricsCollector,
    proxy::ProxyEngine,
    upstream::health::HealthChecker,
};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::broadcast;
use tracing::{debug, info};

use super::{lifecycle::PrismRuntime, PrismComponents};

/// Errors that can occur during runtime initialization.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Metrics collector initialization failed
    #[error("Failed to initialize metrics collector: {0}")]
    MetricsInitialization(String),

    /// Configuration validation failed
    #[error("Configuration validation failed: {0}")]
    ConfigValidation(String),

    /// No upstreams configured
    #[error("No upstream providers configured")]
    NoUpstreams,

    /// Generic initialization error
    #[error("Runtime initialization failed: {0}")]
    Initialization(String),
}

/// Configuration options for the runtime builder.
#[derive(Clone)]
struct RuntimeOptions {
    enable_health_checker: bool,
    enable_websocket_subscriptions: bool,
    shutdown_channel_capacity: usize,
    enable_cache_cleanup: bool,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            enable_health_checker: false,
            enable_websocket_subscriptions: false,
            shutdown_channel_capacity: 16,
            enable_cache_cleanup: true,
        }
    }
}

/// Builder for constructing a `PrismRuntime` with configurable components.
///
/// # Examples
///
/// ```no_run
/// # use prism_core::{config::AppConfig, runtime::PrismRuntimeBuilder};
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config = AppConfig::load()?;
///
/// let runtime = PrismRuntimeBuilder::new()
///     .with_config(config)
///     .enable_health_checker()
///     .enable_websocket_subscriptions()
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct PrismRuntimeBuilder {
    config: Option<AppConfig>,
    options: RuntimeOptions,
}

impl PrismRuntimeBuilder {
    /// Creates a new runtime builder with default options.
    #[must_use]
    pub fn new() -> Self {
        Self { config: None, options: RuntimeOptions::default() }
    }

    #[must_use]
    pub fn with_config(mut self, config: AppConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Enables health checking for upstream providers.
    ///
    /// Background task periodically checks upstream health using `eth_blockNumber`.
    #[must_use]
    pub fn enable_health_checker(mut self) -> Self {
        self.options.enable_health_checker = true;
        self
    }

    #[must_use]
    pub fn disable_health_checker(mut self) -> Self {
        self.options.enable_health_checker = false;
        self
    }

    /// Enables WebSocket subscriptions to upstream providers.
    ///
    /// Subscribes to `newHeads` for chain tip updates and cache invalidation on reorgs.
    #[must_use]
    pub fn enable_websocket_subscriptions(mut self) -> Self {
        self.options.enable_websocket_subscriptions = true;
        self
    }

    #[must_use]
    pub fn disable_websocket_subscriptions(mut self) -> Self {
        self.options.enable_websocket_subscriptions = false;
        self
    }

    /// Sets custom shutdown channel capacity (default: 16).
    #[must_use]
    pub fn with_shutdown_channel_capacity(mut self, capacity: usize) -> Self {
        self.options.shutdown_channel_capacity = capacity;
        self
    }

    #[must_use]
    pub fn disable_cache_cleanup(mut self) -> Self {
        self.options.enable_cache_cleanup = false;
        self
    }

    /// Builds the runtime, initializing all components and starting background tasks.
    ///
    /// # Errors
    ///
    /// Returns `RuntimeError` if configuration is missing/invalid, no upstreams are
    /// configured, or component initialization fails.
    pub fn build(self) -> Result<PrismRuntime, RuntimeError> {
        let config = self.config.ok_or_else(|| {
            RuntimeError::ConfigValidation("No configuration provided".to_string())
        })?;

        config.validate().map_err(RuntimeError::ConfigValidation)?;

        if config.upstreams.providers.is_empty() {
            return Err(RuntimeError::NoUpstreams);
        }

        info!(
            upstreams_count = config.upstreams.providers.len(),
            health_checker_enabled = self.options.enable_health_checker,
            websocket_enabled = self.options.enable_websocket_subscriptions,
            cache_cleanup_enabled = self.options.enable_cache_cleanup,
            "Initializing Prism runtime"
        );

        let (shutdown_tx, _) = broadcast::channel::<()>(self.options.shutdown_channel_capacity);

        // Create shared chain state FIRST - this is the single source of truth
        let chain_state = Arc::new(ChainState::new());
        debug!("Chain state initialized");

        let metrics_collector = Arc::new(
            MetricsCollector::new()
                .map_err(|e| RuntimeError::MetricsInitialization(e.to_string()))?,
        );
        debug!("Metrics collector initialized");

        let mut cache_config = config.cache.manager_config.clone();
        if !self.options.enable_cache_cleanup {
            cache_config.enable_auto_cleanup = false;
        }
        let cache_manager = Arc::new(
            CacheManager::new(&cache_config, chain_state.clone())
                .map_err(|e| RuntimeError::Initialization(format!("Cache manager: {e}")))?,
        );
        debug!("Cache manager initialized");
        cache_manager.start_all_background_tasks(&shutdown_tx);
        debug!(
            cleanup_enabled = cache_config.enable_auto_cleanup,
            "Cache background tasks started"
        );

        let reorg_manager = Arc::new(ReorgManager::new(
            config.cache.manager_config.reorg_manager.clone(),
            chain_state.clone(),
            cache_manager.clone(),
        ));
        debug!("Reorg manager initialized");

        let max_concurrent = config.server.max_concurrent_requests;
        let upstream_manager = Arc::new(
            crate::upstream::UpstreamManagerBuilder::new()
                .chain_state(chain_state.clone())
                .concurrency_limit(max_concurrent)
                .build()
                .map_err(|e| RuntimeError::Initialization(e.to_string()))?,
        );
        for upstream_config in config.to_legacy_upstreams() {
            upstream_manager.add_upstream(upstream_config);
        }
        info!(
            endpoints_count = config.upstreams.providers.len(),
            max_concurrent = max_concurrent,
            "Upstream manager initialized"
        );
        let health_checker = if self.options.enable_health_checker {
            let checker = Arc::new(HealthChecker::new(
                upstream_manager.clone(),
                metrics_collector.clone(),
                config.health_check_interval(),
            ));
            debug!("Health checker initialized");
            Some(checker)
        } else {
            debug!("Health checker disabled");
            None
        };
        let proxy_engine = Arc::new(ProxyEngine::new(
            cache_manager.clone(),
            upstream_manager.clone(),
            metrics_collector.clone(),
        ));
        debug!("Proxy engine initialized");
        let components = PrismComponents::new(
            metrics_collector,
            cache_manager.clone(),
            reorg_manager.clone(),
            upstream_manager.clone(),
            health_checker.clone(),
            proxy_engine,
        );
        let runtime = PrismRuntime::new(
            components,
            shutdown_tx,
            config,
            self.options.enable_health_checker,
            self.options.enable_websocket_subscriptions,
        );

        info!("Prism runtime initialization complete");

        Ok(runtime)
    }
}

impl Default for PrismRuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{UpstreamProvider, UpstreamsConfig};

    fn create_test_config() -> AppConfig {
        AppConfig {
            upstreams: UpstreamsConfig {
                providers: vec![UpstreamProvider {
                    name: "test".to_string(),
                    chain_id: 1,
                    https_url: "https://test.example.com".to_string(),
                    wss_url: None,
                    weight: 1,
                    timeout_seconds: 30,
                    circuit_breaker_threshold: 2,
                    circuit_breaker_timeout_seconds: 1,
                }],
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_builder_requires_config() {
        let result = PrismRuntimeBuilder::new().build();
        assert!(result.is_err());
        assert!(matches!(result, Err(RuntimeError::ConfigValidation(_))));
    }

    #[tokio::test]
    async fn test_builder_validates_config() {
        let mut config = AppConfig::default();
        config.upstreams.providers.clear(); // Invalid: no upstreams

        let result = PrismRuntimeBuilder::new().with_config(config).build();

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_builder_basic() {
        let config = create_test_config();

        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .build()
            .expect("Failed to build runtime");

        assert!(!runtime.components().has_health_checker());
    }

    #[tokio::test]
    async fn test_builder_with_health_checker() {
        let config = create_test_config();

        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .enable_health_checker()
            .build()
            .expect("Failed to build runtime");

        assert!(runtime.components().has_health_checker());

        // Cleanup
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn test_builder_chaining() {
        let config = create_test_config();

        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .enable_health_checker()
            .enable_websocket_subscriptions()
            .with_shutdown_channel_capacity(32)
            .disable_cache_cleanup()
            .build()
            .expect("Failed to build runtime");

        assert!(runtime.components().has_health_checker());

        // Cleanup
        runtime.shutdown().await;
    }
}
