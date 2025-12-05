use crate::{cache::reorg_manager::ReorgManager, chain::ChainState, metrics::MetricsCollector};

use super::manager::UpstreamManager;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::broadcast, time::interval};
use tracing::{error, info, warn};

/// Periodically monitors upstream endpoints and records health metrics.
///
/// Runs health checks at configured intervals, updating the `MetricsCollector`
/// with the results. Also manages WebSocket failure tracking to enable recovery
/// after upstream failures.
///
/// # `ChainState` Integration
///
/// Uses shared [`ChainState`] for direct chain tip reads (avoiding indirection through
/// [`ReorgManager`]), while mutations still go through `ReorgManager` to properly
/// handle cache invalidation on reorgs.
pub struct HealthChecker {
    upstream_manager: Arc<UpstreamManager>,
    metrics_collector: Arc<MetricsCollector>,
    reorg_manager: Option<Arc<ReorgManager>>,
    /// Direct access to chain state for reads (mutations go through `ReorgManager`)
    chain_state: Option<Arc<ChainState>>,
    check_interval: Duration,
}

impl HealthChecker {
    #[must_use]
    pub fn new(
        upstream_manager: Arc<UpstreamManager>,
        metrics_collector: Arc<MetricsCollector>,
        check_interval: Duration,
    ) -> Self {
        Self {
            upstream_manager,
            metrics_collector,
            reorg_manager: None,
            chain_state: None,
            check_interval,
        }
    }

    #[must_use]
    pub fn with_reorg_manager(mut self, reorg_manager: Arc<ReorgManager>) -> Self {
        self.reorg_manager = Some(reorg_manager);
        self
    }

    #[must_use]
    pub fn with_chain_state(mut self, chain_state: Arc<ChainState>) -> Self {
        self.chain_state = Some(chain_state);
        self
    }

    #[must_use]
    pub fn start_with_shutdown(
        &self,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        let upstream_manager = self.upstream_manager.clone();
        let metrics_collector = self.metrics_collector.clone();
        let check_interval = self.check_interval;

        let reorg_manager_clone = self.reorg_manager.clone();
        let chain_state_clone = self.chain_state.clone();

        tokio::spawn(async move {
            let mut interval = interval(check_interval);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = Self::check_all_upstreams(
                            &upstream_manager,
                            &metrics_collector,
                            reorg_manager_clone.as_ref(),
                            chain_state_clone.as_ref(),
                        ).await {
                            error!(error = %e, "health check failed");
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        tracing::info!("health checker shutting down");
                        break;
                    }
                }
            }
        })
    }

    /// Performs health checks on all registered upstream endpoints.
    ///
    /// For each upstream, records the health status in metrics and resets WebSocket
    /// failure tracking on successful checks to enable reconnection after recovery.
    /// Also tracks latency percentiles for hedging decisions and block numbers for
    /// scoring-based upstream selection.
    ///
    /// **Reorg Detection**: Also monitors block numbers from health checks to detect
    /// potential reorgs. If block numbers go backwards or we see different block
    /// numbers at the same height, it triggers reorg handling in the `ReorgManager`.
    ///
    /// # `ChainState` Usage
    ///
    /// When `chain_state` is provided, reads chain tip directly for efficiency.
    /// Falls back to `ReorgManager.get_current_tip()` if `chain_state` is not available.
    /// Mutations always go through `ReorgManager` to ensure proper cache invalidation.
    ///
    /// # Complexity Note
    ///
    /// This function exceeds the line limit due to coordinating multiple subsystems
    /// (metrics, health, reorg detection, finality tracking) in a single pass over
    /// upstreams. Splitting into smaller functions would require passing many
    /// intermediate values between them, reducing rather than improving clarity.
    /// The linear structure with clear section comments makes the flow easy to follow.
    #[allow(clippy::too_many_lines)]
    async fn check_all_upstreams(
        upstream_manager: &UpstreamManager,
        metrics_collector: &MetricsCollector,
        reorg_manager: Option<&Arc<ReorgManager>>,
        chain_state: Option<&Arc<ChainState>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let upstreams = upstream_manager.get_all_upstreams();
        let mut healthy_count = 0usize;
        let mut max_finalized_block = 0u64;
        let mut block_numbers: HashMap<String, u64> = HashMap::new();

        for upstream in upstreams.iter() {
            let upstream_name = Arc::clone(&upstream.config().name);
            let health = upstream.get_health().await;
            let response_time_ms = health.response_time_ms.unwrap_or(0);

            let (is_healthy, block_number) = upstream.health_check_with_block_number().await;

            if is_healthy {
                healthy_count += 1;
                metrics_collector.record_upstream_health(&upstream_name, true);
                metrics_collector.record_health_check(&upstream_name, true, response_time_ms);

                upstream.record_websocket_success().await;

                if response_time_ms > 0 {
                    let hedge_executor = upstream_manager.get_hedge_executor();
                    hedge_executor.record_latency(&upstream_name, response_time_ms);

                    upstream_manager.record_scoring_success(&upstream_name, response_time_ms);
                }

                if let Some(block) = block_number {
                    upstream_manager.record_block_number(&upstream_name, block).await;
                    block_numbers.insert(upstream_name.to_string(), block);
                }

                if let Some(finalized_block) = health.finalized_block {
                    max_finalized_block = max_finalized_block.max(finalized_block);
                }

                if let Some((p50, p95, p99, avg)) =
                    upstream_manager.get_upstream_latency_stats(&upstream_name)
                {
                    metrics_collector.record_upstream_latency_percentiles(
                        &upstream_name,
                        p50,
                        p95,
                        p99,
                        avg,
                    );
                }

                let cb_state = upstream.get_circuit_breaker_state().await;
                let cb_failures = upstream.get_circuit_breaker_failure_count().await;
                metrics_collector.record_circuit_breaker_state(
                    &upstream_name,
                    cb_state,
                    cb_failures,
                );

                if let Some(score) = upstream_manager.get_upstream_score(&upstream_name) {
                    metrics_collector.record_upstream_score(
                        &upstream_name,
                        score.composite_score,
                        score.latency_factor,
                        score.error_rate_factor,
                        score.throttle_factor,
                        score.block_lag_factor,
                    );
                    metrics_collector.record_upstream_error_rate(&upstream_name, score.error_rate);
                    metrics_collector
                        .record_upstream_throttle_rate(&upstream_name, score.throttle_rate);
                    metrics_collector
                        .record_upstream_block_lag(&upstream_name, score.block_head_lag);
                }

                let chain_tip = upstream_manager.get_chain_tip();
                if chain_tip > 0 {
                    metrics_collector.record_chain_tip(chain_tip);
                }

                if let Some(rt) = health.response_time_ms {
                    if let Some(block) = block_number {
                        info!(
                            upstream = %upstream_name,
                            response_time_ms = rt,
                            block_number = block,
                            "health check passed for upstream"
                        );
                    } else {
                        info!(
                            upstream = %upstream_name,
                            response_time_ms = rt,
                            "health check passed for upstream"
                        );
                    }
                } else {
                    info!(upstream = %upstream_name, "health check passed for upstream");
                }
            } else {
                metrics_collector.record_upstream_health(&upstream_name, false);
                metrics_collector.record_health_check(&upstream_name, false, response_time_ms);

                upstream_manager.record_scoring_error(&upstream_name);

                let cb_state = upstream.get_circuit_breaker_state().await;
                let cb_failures = upstream.get_circuit_breaker_failure_count().await;
                metrics_collector.record_circuit_breaker_state(
                    &upstream_name,
                    cb_state,
                    cb_failures,
                );

                if let Some(rt) = health.response_time_ms {
                    warn!(
                        upstream = %upstream_name,
                        response_time_ms = rt,
                        "health check failed for upstream"
                    );
                } else {
                    warn!(upstream = %upstream_name, "health check failed for upstream");
                }
            }
        }

        metrics_collector.record_healthy_upstream_count(healthy_count);

        if max_finalized_block > 0 {
            if let Some(reorg_mgr) = reorg_manager {
                reorg_mgr.update_finalized_block(max_finalized_block).await;
            }
        }

        if let Some(reorg_mgr) = reorg_manager {
            if let Some(max_block) = block_numbers.values().max().copied() {
                let current_tip = match chain_state {
                    Some(cs) => cs.current_tip(),
                    None => reorg_mgr.get_current_tip(),
                };

                if max_block < current_tip && current_tip > 0 {
                    let rollback_depth = current_tip - max_block;
                    if rollback_depth > 0 {
                        warn!(
                            current_tip = current_tip,
                            max_health_check_block = max_block,
                            rollback_depth = rollback_depth,
                            "health check detected chain rollback, triggering reorg handling"
                        );
                        reorg_mgr.update_tip_number_only(max_block).await;
                    }
                } else if max_block > current_tip {
                    info!(
                        current_tip = current_tip,
                        new_tip = max_block,
                        "health check updating chain tip (WebSocket backup)"
                    );
                    reorg_mgr.update_tip_number_only(max_block).await;
                }
            }
        }

        Ok(())
    }

    pub async fn check_upstream(&self, name: &str) -> Option<bool> {
        let upstreams = self.upstream_manager.get_all_upstreams();

        for upstream in upstreams.iter() {
            if upstream.config().name.as_ref() == name {
                let result = upstream.health_check().await;

                let health = upstream.get_health().await;
                let status = if result { "PASSED" } else { "FAILED" };

                if let Some(rt) = health.response_time_ms {
                    info!(
                        upstream = %name,
                        status = %status,
                        response_time_ms = rt,
                        "manual health check"
                    );
                } else {
                    info!(upstream = %name, status = %status, "manual health check");
                }

                return Some(result);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UpstreamConfig;

    fn create_test_upstream_manager() -> (Arc<UpstreamManager>, Arc<ChainState>) {
        let chain_state = Arc::new(ChainState::new());
        let upstream_manager = Arc::new(
            crate::upstream::UpstreamManagerBuilder::new()
                .chain_state(Arc::clone(&chain_state))
                .build()
                .unwrap(),
        );
        (upstream_manager, chain_state)
    }

    fn test_config(name: &str, url: &str) -> UpstreamConfig {
        UpstreamConfig {
            url: url.to_string(),
            name: Arc::from(name),
            weight: 100,
            timeout_seconds: 30,
            supports_websocket: false,
            ws_url: None,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_seconds: 60,
            chain_id: 1,
        }
    }

    #[tokio::test]
    async fn test_health_checker_creation() {
        let chain_state = Arc::new(crate::chain::ChainState::new());
        let upstream_manager = Arc::new(
            crate::upstream::UpstreamManagerBuilder::new()
                .chain_state(chain_state)
                .build()
                .unwrap(),
        );
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());
        let config = UpstreamConfig {
            url: "https://example.com".to_string(),
            name: Arc::from("test"),
            weight: 1,
            timeout_seconds: 30,
            supports_websocket: false,
            ws_url: None,
            circuit_breaker_threshold: 2,
            circuit_breaker_timeout_seconds: 1,
            chain_id: 0,
        };

        upstream_manager.add_upstream(config);

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(60));

        assert!(health_checker.check_upstream("test").await.is_some());
    }

    #[tokio::test]
    async fn test_health_checker_new_basic() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(30));

        assert!(health_checker.reorg_manager.is_none());
        assert!(health_checker.chain_state.is_none());
        assert_eq!(health_checker.check_interval, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn test_health_checker_with_chain_state() {
        let (upstream_manager, chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(30))
                .with_chain_state(chain_state);

        assert!(health_checker.chain_state.is_some());
    }

    #[tokio::test]
    async fn test_health_checker_builder_chain() {
        let (upstream_manager, chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(30))
                .with_chain_state(chain_state);

        assert!(health_checker.chain_state.is_some());
        assert!(health_checker.reorg_manager.is_none());
    }

    #[tokio::test]
    async fn test_check_upstream_not_found() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        upstream_manager.add_upstream(test_config("upstream1", "https://example.com"));

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(30));

        let result = health_checker.check_upstream("nonexistent").await;
        assert!(result.is_none(), "Non-existent upstream should return None");
    }

    #[tokio::test]
    async fn test_check_upstream_found() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        upstream_manager.add_upstream(test_config("test-upstream", "https://example.com"));

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(30));

        let result = health_checker.check_upstream("test-upstream").await;
        assert!(result.is_some(), "Existing upstream should return Some");
    }

    #[tokio::test]
    async fn test_check_upstream_multiple_upstreams() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        upstream_manager.add_upstream(test_config("upstream1", "https://example1.com"));
        upstream_manager.add_upstream(test_config("upstream2", "https://example2.com"));
        upstream_manager.add_upstream(test_config("upstream3", "https://example3.com"));

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(30));

        assert!(health_checker.check_upstream("upstream1").await.is_some());
        assert!(health_checker.check_upstream("upstream2").await.is_some());
        assert!(health_checker.check_upstream("upstream3").await.is_some());
        assert!(health_checker.check_upstream("upstream4").await.is_none());
    }

    #[tokio::test]
    async fn test_check_upstream_empty_name() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        upstream_manager.add_upstream(test_config("upstream1", "https://example.com"));

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(30));

        let result = health_checker.check_upstream("").await;
        assert!(result.is_none(), "Empty name should not match");
    }

    #[tokio::test]
    async fn test_start_with_shutdown_immediate() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(3600));

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

        let handle = health_checker.start_with_shutdown(shutdown_rx);

        shutdown_tx.send(()).expect("Send should succeed");

        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(result.is_ok(), "Health checker should shut down promptly");
    }

    #[tokio::test]
    async fn test_start_with_shutdown_delayed() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(3600));

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

        let handle = health_checker.start_with_shutdown(shutdown_rx);

        tokio::time::sleep(Duration::from_millis(50)).await;

        shutdown_tx.send(()).expect("Send should succeed");

        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(result.is_ok(), "Health checker should shut down");
    }

    #[tokio::test]
    async fn test_check_all_upstreams_empty() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let result =
            HealthChecker::check_all_upstreams(&upstream_manager, &metrics_collector, None, None)
                .await;

        assert!(result.is_ok(), "Empty check should succeed");
    }

    #[tokio::test]
    async fn test_check_all_upstreams_with_upstreams() {
        let (upstream_manager, chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        upstream_manager.add_upstream(test_config("upstream1", "https://example1.com"));
        upstream_manager.add_upstream(test_config("upstream2", "https://example2.com"));

        let result = HealthChecker::check_all_upstreams(
            &upstream_manager,
            &metrics_collector,
            None,
            Some(&chain_state),
        )
        .await;

        assert!(result.is_ok(), "Check with upstreams should succeed");
    }

    #[tokio::test]
    async fn test_check_all_upstreams_with_chain_state() {
        let (upstream_manager, chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        upstream_manager.add_upstream(test_config("upstream1", "https://example1.com"));

        let result = HealthChecker::check_all_upstreams(
            &upstream_manager,
            &metrics_collector,
            None,
            Some(&chain_state),
        )
        .await;

        assert!(result.is_ok(), "Check with chain state should succeed");
    }

    #[test]
    fn test_check_interval_various_durations() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let hc1 = HealthChecker::new(
            Arc::clone(&upstream_manager),
            Arc::clone(&metrics_collector),
            Duration::from_secs(1),
        );
        assert_eq!(hc1.check_interval, Duration::from_secs(1));

        let hc2 = HealthChecker::new(
            Arc::clone(&upstream_manager),
            Arc::clone(&metrics_collector),
            Duration::from_secs(300),
        );
        assert_eq!(hc2.check_interval, Duration::from_secs(300));

        let hc3 =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_millis(500));
        assert_eq!(hc3.check_interval, Duration::from_millis(500));
    }

    #[tokio::test]
    async fn test_health_checker_full_setup() {
        let (upstream_manager, chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let config1 = UpstreamConfig {
            circuit_breaker_threshold: 3,
            ..test_config("primary", "https://primary.com")
        };
        let config2 = UpstreamConfig {
            circuit_breaker_threshold: 5,
            ..test_config("secondary", "https://secondary.com")
        };

        upstream_manager.add_upstream(config1);
        upstream_manager.add_upstream(config2);

        let health_checker =
            HealthChecker::new(upstream_manager, metrics_collector, Duration::from_secs(30))
                .with_chain_state(chain_state);

        assert!(health_checker.chain_state.is_some());
        assert_eq!(health_checker.check_interval, Duration::from_secs(30));

        assert!(health_checker.check_upstream("primary").await.is_some());
        assert!(health_checker.check_upstream("secondary").await.is_some());
    }

    #[tokio::test]
    async fn test_health_checker_with_unhealthy_upstream() {
        let (upstream_manager, _chain_state) = create_test_upstream_manager();
        let metrics_collector = Arc::new(MetricsCollector::new().unwrap());

        let config = UpstreamConfig {
            circuit_breaker_threshold: 2,
            ..test_config("fragile", "https://fragile.com")
        };
        upstream_manager.add_upstream(config);

        let health_checker = HealthChecker::new(
            upstream_manager.clone(),
            metrics_collector,
            Duration::from_secs(30),
        );

        let upstreams = upstream_manager.get_all_upstreams();
        let upstream = upstreams.iter().find(|u| u.config().name.as_ref() == "fragile").unwrap();
        upstream.circuit_breaker().on_failure().await;
        upstream.circuit_breaker().on_failure().await;

        let result = health_checker.check_upstream("fragile").await;
        assert!(result.is_some(), "Should return Some even for unhealthy");
        assert!(!result.unwrap(), "Tripped circuit breaker should be unhealthy");
    }
}
