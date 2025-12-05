//! Runtime lifecycle management including background tasks and graceful shutdown.

use crate::{
    cache::{reorg_manager::ReorgManager, CacheManager},
    config::AppConfig,
    upstream::manager::UpstreamManager,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::{sync::broadcast, task::JoinHandle};
use tracing::{debug, error, info, warn};

use super::{builder::PrismRuntimeBuilder, PrismComponents};

/// Main runtime container managing component lifecycles and background tasks.
///
/// Owns all initialized components and manages their background tasks, providing
/// graceful shutdown coordination via a broadcast channel. When `shutdown()` is called,
/// all background tasks are signaled and awaited for completion.
pub struct PrismRuntime {
    components: PrismComponents,
    shutdown_tx: broadcast::Sender<()>,
    config: AppConfig,
    health_task: Option<JoinHandle<()>>,
    websocket_task: Option<JoinHandle<()>>,
    shutdown_initiated: Arc<AtomicBool>,
}

impl PrismRuntime {
    /// Creates a new builder for constructing a `PrismRuntime`.
    ///
    /// This is the recommended way to create a runtime instance.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let runtime = PrismRuntime::builder()
    ///     .with_config(config)
    ///     .build()
    ///     .await?;
    /// ```
    #[must_use]
    pub fn builder() -> PrismRuntimeBuilder {
        PrismRuntimeBuilder::new()
    }

    /// Creates a new runtime with initialized components and starts background tasks.
    ///
    /// Called by `PrismRuntimeBuilder` during initialization.
    pub(super) fn new(
        components: PrismComponents,
        shutdown_tx: broadcast::Sender<()>,
        config: AppConfig,
        enable_health_checker: bool,
        enable_websocket_subscriptions: bool,
    ) -> Self {
        let shutdown_initiated = Arc::new(AtomicBool::new(false));
        let health_task = if enable_health_checker {
            if let Some(health_checker) = components.health_checker() {
                let handle = health_checker.start_with_shutdown(shutdown_tx.subscribe());
                debug!("Health checker task started");
                Some(handle)
            } else {
                None
            }
        } else {
            None
        };
        let websocket_task = if enable_websocket_subscriptions {
            let task = Self::start_websocket_subscriptions(
                components.cache_manager().clone(),
                components.upstream_manager().clone(),
                components.reorg_manager().clone(),
                shutdown_tx.subscribe(),
            );
            debug!("WebSocket subscription task started");
            Some(task)
        } else {
            None
        };

        Self { components, shutdown_tx, config, health_task, websocket_task, shutdown_initiated }
    }

    /// Returns a reference to all runtime components.
    #[must_use]
    pub fn components(&self) -> &PrismComponents {
        &self.components
    }

    /// Returns a reference to the application configuration.
    #[must_use]
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Convenience accessor for the proxy engine.
    #[must_use]
    pub fn proxy_engine(&self) -> &Arc<crate::proxy::ProxyEngine> {
        self.components.proxy_engine()
    }

    /// Convenience accessor for the cache manager.
    #[must_use]
    pub fn cache_manager(&self) -> &Arc<CacheManager> {
        self.components.cache_manager()
    }

    /// Convenience accessor for the upstream manager.
    #[must_use]
    pub fn upstream_manager(&self) -> &Arc<UpstreamManager> {
        self.components.upstream_manager()
    }

    /// Convenience accessor for the metrics collector.
    #[must_use]
    pub fn metrics_collector(&self) -> &Arc<crate::metrics::MetricsCollector> {
        self.components.metrics_collector()
    }

    /// Creates a new shutdown receiver for external shutdown coordination.
    ///
    /// Useful for listening to shutdown signals in custom background tasks.
    #[must_use]
    pub fn shutdown_receiver(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Initiates graceful shutdown of all background tasks.
    ///
    /// Broadcasts shutdown signal to all tasks, aborts health checker, and waits for
    /// WebSocket subscriptions to complete. This method is idempotent - safe to call
    /// multiple times.
    pub async fn shutdown(self) {
        if self
            .shutdown_initiated
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            warn!("Shutdown already initiated, ignoring duplicate call");
            return;
        }

        info!("Initiating Prism runtime shutdown");
        if let Err(e) = self.shutdown_tx.send(()) {
            warn!(error = %e, "Failed to send shutdown signal (no receivers)");
        }
        debug!("Shutdown signal broadcast to all tasks");
        if let Some(health_task) = self.health_task {
            health_task.abort();
            debug!("Health checker task aborted");
        }
        if let Some(websocket_task) = self.websocket_task {
            match websocket_task.await {
                Ok(()) => debug!("WebSocket subscription task completed"),
                Err(e) if e.is_cancelled() => debug!("WebSocket subscription task cancelled"),
                Err(e) => error!(error = %e, "WebSocket subscription task failed"),
            }
        }

        info!("Prism runtime shutdown complete");
    }

    /// Waits indefinitely for a shutdown signal, then performs cleanup.
    ///
    /// Useful for server implementations that need to keep the runtime alive while
    /// waiting for external shutdown signals (SIGTERM, Ctrl+C, etc.).
    pub async fn wait_for_shutdown(self) {
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let _ = shutdown_rx.recv().await;
        info!("Shutdown signal received, runtime terminating");
        self.shutdown().await;
    }

    /// Starts WebSocket subscription tasks for all upstreams with WebSocket support.
    fn start_websocket_subscriptions(
        cache_manager: Arc<CacheManager>,
        upstream_manager: Arc<UpstreamManager>,
        reorg_manager: Arc<ReorgManager>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> JoinHandle<()> {
        const MAX_RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

        tokio::spawn(async move {
            debug!("Starting chain tip subscription for all upstreams");

            let upstreams = upstream_manager.get_all_upstreams();

            let websocket_upstreams: Vec<_> = upstreams
                .iter()
                .filter(|upstream| {
                    upstream.config().supports_websocket && upstream.config().ws_url.is_some()
                })
                .cloned()
                .collect();

            debug!(
                websocket_upstreams_count = websocket_upstreams.len(),
                "Found upstreams with WebSocket support"
            );

            let mut subscription_handles = Vec::new();

            for upstream in websocket_upstreams {
                let upstream_name = upstream.config().name.clone();
                let cache_manager_clone = cache_manager.clone();
                let reorg_manager_clone = reorg_manager.clone();

                let mut task_shutdown_rx = shutdown_rx.resubscribe();
                let handle = tokio::spawn(async move {
                    let mut reconnect_delay = std::time::Duration::from_secs(1);

                    loop {
                        tokio::select! {
                            _ = task_shutdown_rx.recv() => {
                                info!(
                                    upstream_name = %upstream_name,
                                    "WebSocket subscription received shutdown signal"
                                );
                                break;
                            }
                            () = async {
                                debug!(
                                    upstream_name = %upstream_name,
                                    "Starting WebSocket subscription (with reconnection)"
                                );
                                if !upstream.should_attempt_websocket_subscription().await {
                                    debug!(
                                        upstream_name = %upstream_name,
                                        "Skipping WebSocket subscription due to failure tracker"
                                    );
                                    tokio::time::sleep(reconnect_delay).await;
                                    return;
                                }

                                match upstream
                                    .subscribe_to_new_heads(
                                        cache_manager_clone.clone(),
                                        reorg_manager_clone.clone(),
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        info!(
                                            upstream_name = %upstream_name,
                                            "WebSocket subscription completed normally"
                                        );
                                        reconnect_delay = std::time::Duration::from_secs(1);
                                    }
                                    Err(e) => {
                                        error!(
                                            upstream_name = %upstream_name,
                                            error = %e,
                                            reconnect_delay_secs = reconnect_delay.as_secs(),
                                            "WebSocket subscription failed, will retry"
                                        );

                                        upstream.record_websocket_failure().await;
                                        tokio::time::sleep(reconnect_delay).await;
                                        reconnect_delay = std::cmp::min(
                                            reconnect_delay * 2,
                                            MAX_RECONNECT_DELAY
                                        );
                                    }
                                }

                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            } => {}
                        }
                    }
                });

                subscription_handles.push(handle);
            }
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("WebSocket subscription manager received shutdown signal");
                }
                () = async {
                    for handle in subscription_handles {
                        if let Err(e) = handle.await {
                            error!(
                                error = %e,
                                "WebSocket subscription task unexpectedly terminated"
                            );
                        }
                    }
                } => {}
            }

            info!("WebSocket subscription manager shutting down");
        })
    }
}
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    let _ = assert_send::<PrismRuntime>;
    let _ = assert_sync::<PrismRuntime>;
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{UpstreamProvider, UpstreamsConfig},
        runtime::PrismRuntimeBuilder,
    };

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
    async fn test_runtime_lifecycle() {
        let config = create_test_config();

        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .build()
            .expect("Failed to build runtime");

        // Verify components accessible (they are Arc, not Option)
        let _proxy = runtime.proxy_engine();
        let _cache = runtime.cache_manager();

        // Shutdown
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn test_runtime_double_shutdown() {
        let config = create_test_config();

        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .build()
            .expect("Failed to build runtime");

        let shutdown_flag = runtime.shutdown_initiated.clone();

        // First shutdown
        runtime.shutdown().await;

        // Verify flag set
        assert!(shutdown_flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_runtime_shutdown_receiver() {
        let config = create_test_config();

        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .build()
            .expect("Failed to build runtime");

        let mut rx = runtime.shutdown_receiver();

        // Spawn task that waits for shutdown
        let task = tokio::spawn(async move {
            rx.recv().await.expect("Shutdown signal received");
        });

        // Shutdown runtime
        runtime.shutdown().await;

        // Task should complete
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("Task should complete")
            .expect("Task should not panic");
    }

    #[tokio::test]
    async fn test_runtime_with_health_checker() {
        let config = create_test_config();

        let runtime = PrismRuntimeBuilder::new()
            .with_config(config)
            .enable_health_checker()
            .build()
            .expect("Failed to build runtime");

        assert!(runtime.components().has_health_checker());

        // Shutdown
        runtime.shutdown().await;
    }
}
