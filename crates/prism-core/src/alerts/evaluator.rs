//! Alert rule evaluation and background monitoring.
//!
//! Provides automatic evaluation of alert rules based on system metrics and
//! creates alerts when conditions are met.

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::Utc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::{
    manager::AlertManager,
    types::{Alert, AlertCondition, AlertSeverity},
};
use crate::{
    metrics::MetricsCollector,
    upstream::{circuit_breaker::CircuitBreakerState, manager::UpstreamManager},
};

/// Evaluates alert rules in the background and creates alerts when conditions are met.
///
/// The evaluator runs on a periodic interval, checking all enabled rules against
/// current system metrics. It respects cooldown periods to prevent alert spam and
/// handles errors gracefully to avoid crashing the server.
pub struct AlertEvaluator {
    alert_manager: Arc<AlertManager>,
    metrics_collector: Arc<MetricsCollector>,
    upstream_manager: Arc<UpstreamManager>,
    evaluation_interval: Duration,
    /// Tracks the last time each rule fired an alert
    last_alert_times: Arc<tokio::sync::RwLock<HashMap<String, chrono::DateTime<Utc>>>>,
}

impl AlertEvaluator {
    /// Creates a new alert evaluator.
    ///
    /// # Arguments
    ///
    /// * `alert_manager` - Manager for storing and retrieving alerts
    /// * `metrics_collector` - Collector for system metrics
    /// * `upstream_manager` - Manager for upstream endpoints
    /// * `evaluation_interval` - How often to evaluate rules (e.g., 30 seconds)
    #[must_use]
    pub fn new(
        alert_manager: Arc<AlertManager>,
        metrics_collector: Arc<MetricsCollector>,
        upstream_manager: Arc<UpstreamManager>,
        evaluation_interval: Duration,
    ) -> Self {
        Self {
            alert_manager,
            metrics_collector,
            upstream_manager,
            evaluation_interval,
            last_alert_times: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Starts the background evaluation task.
    ///
    /// Spawns a tokio task that periodically evaluates all enabled alert rules.
    /// The task runs indefinitely until the returned `JoinHandle` is aborted or
    /// the process terminates.
    ///
    /// The task uses a panic-safe wrapper to catch panics and recover gracefully,
    /// preventing a single evaluation failure from crashing the entire server.
    #[must_use]
    pub fn start(self: Arc<Self>) -> JoinHandle<()> {
        let interval = self.evaluation_interval;

        tokio::spawn(async move {
            info!(
                interval_seconds = interval.as_secs(),
                "Starting alert evaluator background task"
            );

            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                ticker.tick().await;

                // Use tokio::spawn to isolate each evaluation run
                // This allows panics to be caught at the task boundary
                let evaluator = Arc::clone(&self);
                let eval_handle = tokio::spawn(async move {
                    evaluator.evaluate_rules().await;
                });

                // Wait for the evaluation to complete, catching any panics
                match eval_handle.await {
                    Ok(()) => {
                        // Evaluation completed successfully
                    }
                    Err(e) => {
                        if e.is_panic() {
                            error!(
                                error = ?e,
                                "Alert evaluation panicked - recovering"
                            );
                        } else if e.is_cancelled() {
                            warn!("Alert evaluation task was cancelled");
                        }
                    }
                }
            }
        })
    }

    /// Evaluates all enabled rules and creates alerts as needed.
    ///
    /// This method is called periodically by the background task. It:
    /// 1. Retrieves all enabled alert rules
    /// 2. Evaluates each rule's condition
    /// 3. Checks cooldown periods
    /// 4. Creates alerts for triggered rules
    async fn evaluate_rules(&self) {
        debug!("Evaluating alert rules");

        let rules = self.alert_manager.get_rules();
        let enabled_rules: Vec<_> = rules.into_iter().filter(|rule| rule.enabled).collect();

        if enabled_rules.is_empty() {
            debug!("No enabled alert rules to evaluate");
            return;
        }

        debug!(count = enabled_rules.len(), "Evaluating enabled rules");

        for rule in enabled_rules {
            // Check if we're within cooldown period
            if !self.is_cooldown_expired(&rule.id, rule.cooldown_seconds).await {
                debug!(
                    rule_id = %rule.id,
                    rule_name = %rule.name,
                    "Rule is in cooldown period, skipping"
                );
                continue;
            }

            // Evaluate the rule's condition
            match self.evaluate_condition(&rule.condition).await {
                Ok(triggered) => {
                    if triggered {
                        self.handle_triggered_rule(
                            &rule.id,
                            &rule.name,
                            rule.severity,
                            &rule.condition,
                        )
                        .await;
                    } else {
                        debug!(
                            rule_id = %rule.id,
                            rule_name = %rule.name,
                            "Rule condition not met"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        rule_id = %rule.id,
                        rule_name = %rule.name,
                        error = %e,
                        "Failed to evaluate rule condition"
                    );
                }
            }
        }
    }

    /// Evaluates a single rule condition against current metrics.
    ///
    /// # Errors
    ///
    /// Returns an error if metric retrieval fails or the condition cannot be evaluated.
    async fn evaluate_condition(
        &self,
        condition: &AlertCondition,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        match condition {
            AlertCondition::HighErrorRate { threshold } => {
                let summary = self.metrics_collector.get_metrics_summary().await;
                let total_requests = summary.total_requests;

                if total_requests == 0 {
                    return Ok(false);
                }

                #[allow(clippy::cast_precision_loss)]
                let error_rate = summary.total_upstream_errors as f64 / total_requests as f64;

                Ok(error_rate > *threshold)
            }

            AlertCondition::HighLatency { threshold_ms } => {
                let summary = self.metrics_collector.get_metrics_summary().await;

                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let current_latency_ms = summary.average_latency_ms as u64;

                Ok(current_latency_ms > *threshold_ms)
            }

            AlertCondition::CacheHitRateLow { threshold } => {
                let summary = self.metrics_collector.get_metrics_summary().await;
                Ok(summary.cache_hit_rate < *threshold)
            }

            AlertCondition::AllUpstreamsDown => {
                let upstreams = self.upstream_manager.get_all_upstreams();

                // Check if all upstreams have open circuit breakers
                let mut all_down = true;
                for upstream in upstreams.iter() {
                    let state = upstream.get_circuit_breaker_state().await;
                    if state != CircuitBreakerState::Open {
                        all_down = false;
                        break;
                    }
                }

                Ok(all_down && !upstreams.is_empty())
            }

            AlertCondition::UpstreamUnhealthy { upstream_id } => {
                let upstreams = self.upstream_manager.get_all_upstreams();

                for upstream in upstreams.iter() {
                    if upstream.config().name.as_ref() == upstream_id.as_str() {
                        let state = upstream.get_circuit_breaker_state().await;
                        return Ok(state == CircuitBreakerState::Open);
                    }
                }

                // Upstream not found or couldn't get state
                Ok(false)
            }
        }
    }

    /// Checks if the cooldown period has expired for a given rule.
    async fn is_cooldown_expired(&self, rule_id: &str, cooldown_seconds: u64) -> bool {
        let last_alert_times = self.last_alert_times.read().await;

        if let Some(last_time) = last_alert_times.get(rule_id) {
            let elapsed = Utc::now().signed_duration_since(*last_time).num_seconds();

            #[allow(clippy::cast_sign_loss)]
            let elapsed_u64 = elapsed.max(0) as u64;

            elapsed_u64 >= cooldown_seconds
        } else {
            // No previous alert, cooldown is expired
            true
        }
    }

    /// Handles a triggered rule by creating an alert and updating cooldown tracking.
    async fn handle_triggered_rule(
        &self,
        rule_id: &str,
        rule_name: &str,
        severity: AlertSeverity,
        condition: &AlertCondition,
    ) {
        let alert_id = Uuid::new_v4().to_string();
        let message = self.generate_alert_message(condition).await;

        let alert = Alert::new(alert_id.clone(), rule_id.to_string(), severity, message.clone());

        self.alert_manager.create_alert(alert);

        // Update last alert time
        let mut last_alert_times = self.last_alert_times.write().await;
        last_alert_times.insert(rule_id.to_string(), Utc::now());

        info!(
            alert_id = %alert_id,
            rule_id = %rule_id,
            rule_name = %rule_name,
            severity = ?severity,
            message = %message,
            "Alert created"
        );
    }

    /// Generates a descriptive message for an alert based on its condition.
    async fn generate_alert_message(&self, condition: &AlertCondition) -> String {
        match condition {
            AlertCondition::HighErrorRate { threshold } => {
                let summary = self.metrics_collector.get_metrics_summary().await;
                let total_requests = summary.total_requests;

                #[allow(clippy::cast_precision_loss)]
                let error_rate = if total_requests > 0 {
                    summary.total_upstream_errors as f64 / total_requests as f64
                } else {
                    0.0
                };

                format!(
                    "High error rate detected: {:.2}% (threshold: {:.2}%)",
                    error_rate * 100.0,
                    threshold * 100.0
                )
            }

            AlertCondition::HighLatency { threshold_ms } => {
                let summary = self.metrics_collector.get_metrics_summary().await;
                format!(
                    "High latency detected: {:.0}ms (threshold: {}ms)",
                    summary.average_latency_ms, threshold_ms
                )
            }

            AlertCondition::CacheHitRateLow { threshold } => {
                let summary = self.metrics_collector.get_metrics_summary().await;
                format!(
                    "Low cache hit rate: {:.2}% (threshold: {:.2}%)",
                    summary.cache_hit_rate * 100.0,
                    threshold * 100.0
                )
            }

            AlertCondition::AllUpstreamsDown => {
                let upstreams = self.upstream_manager.get_all_upstreams();
                format!("All {} upstream endpoints are unhealthy", upstreams.len())
            }

            AlertCondition::UpstreamUnhealthy { upstream_id } => {
                format!("Upstream '{upstream_id}' is unhealthy (circuit breaker open)")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{alerts::types::AlertRule, chain::ChainState, upstream::UpstreamManagerBuilder};

    fn create_test_evaluator() -> Arc<AlertEvaluator> {
        let alert_manager = Arc::new(AlertManager::new());
        let metrics_collector = Arc::new(MetricsCollector::new().expect("metrics init"));
        let chain_state = Arc::new(ChainState::new());
        let upstream_manager = Arc::new(
            UpstreamManagerBuilder::new()
                .chain_state(chain_state)
                .concurrency_limit(100)
                .build()
                .expect("upstream manager init"),
        );

        Arc::new(AlertEvaluator::new(
            alert_manager,
            metrics_collector,
            upstream_manager,
            Duration::from_secs(30),
        ))
    }

    #[tokio::test]
    async fn test_evaluator_creation() {
        let evaluator = create_test_evaluator();
        assert_eq!(evaluator.evaluation_interval, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn test_cooldown_expired() {
        let evaluator = create_test_evaluator();

        // No previous alert - cooldown should be expired
        assert!(evaluator.is_cooldown_expired("rule1", 60).await);

        // Add a recent alert
        {
            let mut last_times = evaluator.last_alert_times.write().await;
            last_times.insert("rule1".to_string(), Utc::now());
        }

        // Cooldown should not be expired yet
        assert!(!evaluator.is_cooldown_expired("rule1", 60).await);

        // Add an old alert
        {
            let mut last_times = evaluator.last_alert_times.write().await;
            let old_time = Utc::now() - chrono::Duration::seconds(120);
            last_times.insert("rule2".to_string(), old_time);
        }

        // Cooldown should be expired
        assert!(evaluator.is_cooldown_expired("rule2", 60).await);
    }

    #[tokio::test]
    async fn test_evaluate_high_error_rate() {
        let evaluator = create_test_evaluator();

        // Record some errors
        evaluator
            .metrics_collector
            .record_request("eth_blockNumber", "test", true, 100)
            .await;
        evaluator
            .metrics_collector
            .record_request("eth_blockNumber", "test", false, 100)
            .await;
        evaluator.metrics_collector.record_upstream_error("test", "timeout").await;

        let condition = AlertCondition::HighErrorRate { threshold: 0.3 };
        let result = evaluator.evaluate_condition(&condition).await.unwrap();

        // 1 error out of 2 requests = 50% > 30%
        assert!(result);
    }

    #[tokio::test]
    async fn test_evaluate_high_latency() {
        let evaluator = create_test_evaluator();

        // Record high latency request
        evaluator
            .metrics_collector
            .record_request("eth_blockNumber", "test", true, 2000)
            .await;

        let condition = AlertCondition::HighLatency { threshold_ms: 1000 };
        let result = evaluator.evaluate_condition(&condition).await.unwrap();

        assert!(result);
    }

    #[tokio::test]
    async fn test_evaluate_low_cache_hit_rate() {
        let evaluator = create_test_evaluator();

        // Record requests with low cache hit rate
        evaluator
            .metrics_collector
            .record_request("eth_blockNumber", "test", true, 100)
            .await;
        evaluator
            .metrics_collector
            .record_request("eth_blockNumber", "test", true, 100)
            .await;
        evaluator.metrics_collector.record_cache_hit("eth_blockNumber");

        let condition = AlertCondition::CacheHitRateLow { threshold: 0.75 };
        let result = evaluator.evaluate_condition(&condition).await.unwrap();

        // 1 hit / 2 requests = 50% < 75%
        assert!(result);
    }

    #[tokio::test]
    async fn test_generate_alert_message() {
        let evaluator = create_test_evaluator();

        let condition = AlertCondition::HighErrorRate { threshold: 0.5 };
        let message = evaluator.generate_alert_message(&condition).await;
        assert!(message.contains("High error rate"));

        let condition = AlertCondition::AllUpstreamsDown;
        let message = evaluator.generate_alert_message(&condition).await;
        assert!(message.contains("upstream"));
    }

    #[tokio::test]
    async fn test_evaluate_rules_with_disabled_rule() {
        let evaluator = create_test_evaluator();

        // Add a disabled rule
        let rule = AlertRule::new(
            "test-rule".to_string(),
            "Test Rule".to_string(),
            AlertCondition::HighErrorRate { threshold: 0.1 },
            AlertSeverity::Warning,
            false, // disabled
            300,
        );
        let _ = evaluator.alert_manager.add_rule(rule);

        // Record high error rate
        evaluator.metrics_collector.record_upstream_error("test", "timeout").await;

        evaluator.evaluate_rules().await;

        // No alerts should be created because rule is disabled
        let alerts = evaluator.alert_manager.get_alerts();
        assert!(alerts.is_empty());
    }
}
