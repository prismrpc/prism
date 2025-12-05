//! Core component container for the Prism runtime.

use crate::{
    cache::{reorg_manager::ReorgManager, CacheManager},
    metrics::MetricsCollector,
    proxy::ProxyEngine,
    upstream::{health::HealthChecker, manager::UpstreamManager},
};
use std::sync::Arc;

/// Container for all initialized Prism core components.
///
/// All components are wrapped in `Arc` for efficient sharing across threads and tasks.
/// Components implement interior mutability where needed and are safe to clone and share.
#[derive(Clone)]
pub struct PrismComponents {
    metrics_collector: Arc<MetricsCollector>,
    cache_manager: Arc<CacheManager>,
    reorg_manager: Arc<ReorgManager>,
    upstream_manager: Arc<UpstreamManager>,
    health_checker: Option<Arc<HealthChecker>>,
    proxy_engine: Arc<ProxyEngine>,
}

impl PrismComponents {
    /// Creates a new components container.
    ///
    /// Called by `PrismRuntimeBuilder` during initialization.
    #[must_use]
    pub fn new(
        metrics_collector: Arc<MetricsCollector>,
        cache_manager: Arc<CacheManager>,
        reorg_manager: Arc<ReorgManager>,
        upstream_manager: Arc<UpstreamManager>,
        health_checker: Option<Arc<HealthChecker>>,
        proxy_engine: Arc<ProxyEngine>,
    ) -> Self {
        Self {
            metrics_collector,
            cache_manager,
            reorg_manager,
            upstream_manager,
            health_checker,
            proxy_engine,
        }
    }

    /// Returns a reference to the metrics collector.
    #[must_use]
    pub fn metrics_collector(&self) -> &Arc<MetricsCollector> {
        &self.metrics_collector
    }

    /// Returns a reference to the cache manager.
    #[must_use]
    pub fn cache_manager(&self) -> &Arc<CacheManager> {
        &self.cache_manager
    }

    /// Returns a reference to the reorg manager.
    #[must_use]
    pub fn reorg_manager(&self) -> &Arc<ReorgManager> {
        &self.reorg_manager
    }

    /// Returns a reference to the upstream manager.
    #[must_use]
    pub fn upstream_manager(&self) -> &Arc<UpstreamManager> {
        &self.upstream_manager
    }

    /// Returns a reference to the health checker, if enabled.
    ///
    /// Returns `None` if health checking was disabled during runtime initialization.
    #[must_use]
    pub fn health_checker(&self) -> Option<&Arc<HealthChecker>> {
        self.health_checker.as_ref()
    }

    /// Returns a reference to the proxy engine.
    #[must_use]
    pub fn proxy_engine(&self) -> &Arc<ProxyEngine> {
        &self.proxy_engine
    }

    /// Returns whether health checking is enabled.
    #[must_use]
    pub fn has_health_checker(&self) -> bool {
        self.health_checker.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cache::{reorg_manager::ReorgManagerConfig, CacheManagerConfig},
        config::AppConfig,
    };

    #[tokio::test]
    async fn test_components_creation() {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let metrics = Arc::new(MetricsCollector::new().unwrap());
        let cache = Arc::new(
            CacheManager::new(&CacheManagerConfig::default(), chain_state.clone())
                .expect("valid test cache config"),
        );
        let reorg = Arc::new(ReorgManager::new(
            ReorgManagerConfig::default(),
            chain_state.clone(),
            cache.clone(),
        ));
        let upstream = Arc::new(
            crate::upstream::UpstreamManagerBuilder::new()
                .chain_state(chain_state.clone())
                .build()
                .unwrap(),
        );
        let proxy = Arc::new(ProxyEngine::new(cache.clone(), upstream.clone(), metrics.clone()));

        let components = PrismComponents::new(metrics, cache, reorg, upstream, None, proxy);

        assert!(!components.has_health_checker());
        assert!(components.health_checker().is_none());
    }

    #[tokio::test]
    async fn test_components_with_health_checker() {
        let config = AppConfig::default();
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let metrics = Arc::new(MetricsCollector::new().unwrap());
        let cache = Arc::new(
            CacheManager::new(&CacheManagerConfig::default(), chain_state.clone())
                .expect("valid test cache config"),
        );
        let reorg = Arc::new(ReorgManager::new(
            ReorgManagerConfig::default(),
            chain_state.clone(),
            cache.clone(),
        ));
        let upstream = Arc::new(
            crate::upstream::UpstreamManagerBuilder::new()
                .chain_state(chain_state.clone())
                .build()
                .unwrap(),
        );
        let health = Arc::new(HealthChecker::new(
            upstream.clone(),
            metrics.clone(),
            config.health_check_interval(),
        ));
        let proxy = Arc::new(ProxyEngine::new(cache.clone(), upstream.clone(), metrics.clone()));

        let components = PrismComponents::new(metrics, cache, reorg, upstream, Some(health), proxy);

        assert!(components.has_health_checker());
        assert!(components.health_checker().is_some());
    }
}
