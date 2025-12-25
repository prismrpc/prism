//! Alert type definitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Severity level of an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    /// Critical alert requiring immediate attention.
    Critical,
    /// Warning alert indicating potential issues.
    Warning,
    /// Informational alert for awareness.
    Info,
}

/// Current status of an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    /// Alert is currently active.
    Active,
    /// Alert has been resolved.
    Resolved,
    /// Alert has been acknowledged by an operator.
    Acknowledged,
}

/// Conditions that can trigger an alert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertCondition {
    /// An upstream provider is unhealthy.
    UpstreamUnhealthy {
        /// ID of the unhealthy upstream.
        upstream_id: String,
    },
    /// Error rate exceeds threshold.
    HighErrorRate {
        /// Error rate threshold (0.0-1.0).
        threshold: f64,
    },
    /// Average latency exceeds threshold.
    HighLatency {
        /// Latency threshold in milliseconds.
        threshold_ms: u64,
    },
    /// Cache hit rate is below threshold.
    CacheHitRateLow {
        /// Hit rate threshold (0.0-1.0).
        threshold: f64,
    },
    /// All upstream providers are down.
    AllUpstreamsDown,
}

/// A rule defining when to create alerts.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlertRule {
    /// Unique identifier for the rule.
    pub id: String,
    /// Human-readable name for the rule.
    pub name: String,
    /// Condition that triggers the alert.
    pub condition: AlertCondition,
    /// Severity level for alerts created by this rule.
    pub severity: AlertSeverity,
    /// Whether this rule is currently enabled.
    pub enabled: bool,
    /// Minimum seconds between consecutive alerts from this rule.
    pub cooldown_seconds: u64,
}

impl AlertRule {
    /// Creates a new alert rule.
    #[must_use]
    pub fn new(
        id: String,
        name: String,
        condition: AlertCondition,
        severity: AlertSeverity,
        enabled: bool,
        cooldown_seconds: u64,
    ) -> Self {
        Self { id, name, condition, severity, enabled, cooldown_seconds }
    }
}

/// An active or historical alert instance.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Alert {
    /// Unique identifier for the alert.
    pub id: String,
    /// ID of the rule that created this alert.
    pub rule_id: String,
    /// Current status of the alert.
    pub status: AlertStatus,
    /// Severity level of the alert.
    pub severity: AlertSeverity,
    /// Descriptive message about the alert.
    pub message: String,
    /// Timestamp when the alert was created.
    pub created_at: DateTime<Utc>,
    /// Timestamp when the alert was resolved, if applicable.
    pub resolved_at: Option<DateTime<Utc>>,
    /// Timestamp when the alert was acknowledged, if applicable.
    pub acknowledged_at: Option<DateTime<Utc>>,
}

impl Alert {
    /// Creates a new active alert.
    #[must_use]
    pub fn new(id: String, rule_id: String, severity: AlertSeverity, message: String) -> Self {
        Self {
            id,
            rule_id,
            status: AlertStatus::Active,
            severity,
            message,
            created_at: Utc::now(),
            resolved_at: None,
            acknowledged_at: None,
        }
    }

    /// Marks the alert as acknowledged.
    pub fn acknowledge(&mut self) {
        if self.status == AlertStatus::Active {
            self.status = AlertStatus::Acknowledged;
            self.acknowledged_at = Some(Utc::now());
        }
    }

    /// Marks the alert as resolved.
    pub fn resolve(&mut self) {
        self.status = AlertStatus::Resolved;
        self.resolved_at = Some(Utc::now());
    }
}
