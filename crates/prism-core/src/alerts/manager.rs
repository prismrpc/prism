//! Alert management and storage.

use std::sync::Arc;

use parking_lot::RwLock;

use super::types::{Alert, AlertRule, AlertStatus};

/// Maximum number of alerts to keep in memory.
/// This prevents unbounded memory growth from accumulating historical alerts.
const MAX_ALERTS: usize = 1000;

/// Manages alerts and alert rules.
///
/// Stores active and historical alerts in memory, manages alert rules,
/// and provides methods for alert lifecycle management.
#[derive(Clone)]
pub struct AlertManager {
    /// Active and historical alerts.
    alerts: Arc<RwLock<Vec<Alert>>>,
    /// Configured alert rules.
    rules: Arc<RwLock<Vec<AlertRule>>>,
}

impl AlertManager {
    /// Creates a new alert manager.
    #[must_use]
    pub fn new() -> Self {
        Self { alerts: Arc::new(RwLock::new(Vec::new())), rules: Arc::new(RwLock::new(Vec::new())) }
    }

    // ========== Rule Management ==========

    /// Adds a new alert rule.
    ///
    /// Returns `true` if the rule was added, `false` if a rule with the same ID already exists.
    #[must_use]
    pub fn add_rule(&self, rule: AlertRule) -> bool {
        let mut rules = self.rules.write();

        // Check if rule with this ID already exists
        if rules.iter().any(|r| r.id == rule.id) {
            return false;
        }

        rules.push(rule);
        true
    }

    /// Updates an existing alert rule.
    ///
    /// Returns `true` if the rule was updated, `false` if no rule with the given ID exists.
    #[must_use]
    pub fn update_rule(&self, rule: AlertRule) -> bool {
        let mut rules = self.rules.write();

        if let Some(existing) = rules.iter_mut().find(|r| r.id == rule.id) {
            *existing = rule;
            true
        } else {
            false
        }
    }

    /// Removes an alert rule by ID.
    ///
    /// Returns `true` if the rule was removed, `false` if no rule with the given ID exists.
    #[must_use]
    pub fn remove_rule(&self, rule_id: &str) -> bool {
        let mut rules = self.rules.write();
        let initial_len = rules.len();
        rules.retain(|r| r.id != rule_id);
        rules.len() != initial_len
    }

    /// Toggles a rule's enabled status.
    ///
    /// Returns the new enabled state, or `None` if the rule doesn't exist.
    #[must_use]
    pub fn toggle_rule(&self, rule_id: &str) -> Option<bool> {
        let mut rules = self.rules.write();

        if let Some(rule) = rules.iter_mut().find(|r| r.id == rule_id) {
            rule.enabled = !rule.enabled;
            Some(rule.enabled)
        } else {
            None
        }
    }

    /// Gets all alert rules.
    #[must_use]
    pub fn get_rules(&self) -> Vec<AlertRule> {
        self.rules.read().clone()
    }

    /// Gets a specific alert rule by ID.
    #[must_use]
    pub fn get_rule(&self, rule_id: &str) -> Option<AlertRule> {
        self.rules.read().iter().find(|r| r.id == rule_id).cloned()
    }

    // ========== Alert Management ==========

    /// Creates a new alert.
    ///
    /// Enforces a maximum capacity of [`MAX_ALERTS`] to prevent unbounded memory growth.
    /// When approaching capacity:
    /// 1. At 90% capacity, removes all resolved alerts
    /// 2. If still at max capacity, removes oldest alerts (FIFO)
    ///
    /// This ensures active and recent alerts are preserved while old resolved alerts
    /// are automatically evicted.
    pub fn create_alert(&self, alert: Alert) {
        let mut alerts = self.alerts.write();

        // If at 90% capacity, clean up resolved alerts
        if alerts.len() >= MAX_ALERTS * 9 / 10 {
            alerts.retain(|a| !matches!(a.status, AlertStatus::Resolved));
        }

        // If still at max capacity, remove oldest alerts
        while alerts.len() >= MAX_ALERTS {
            alerts.remove(0);
        }

        alerts.push(alert);
    }

    /// Resolves an alert by ID.
    ///
    /// Returns `true` if the alert was resolved, `false` if no alert with the given ID exists.
    #[must_use]
    pub fn resolve_alert(&self, alert_id: &str) -> bool {
        let mut alerts = self.alerts.write();

        if let Some(alert) = alerts.iter_mut().find(|a| a.id == alert_id) {
            alert.resolve();
            true
        } else {
            false
        }
    }

    /// Acknowledges an alert by ID.
    ///
    /// Returns `true` if the alert was acknowledged, `false` if no alert with the given ID exists.
    #[must_use]
    pub fn acknowledge_alert(&self, alert_id: &str) -> bool {
        let mut alerts = self.alerts.write();

        if let Some(alert) = alerts.iter_mut().find(|a| a.id == alert_id) {
            alert.acknowledge();
            true
        } else {
            false
        }
    }

    /// Dismisses (removes) an alert by ID.
    ///
    /// Returns `true` if the alert was removed, `false` if no alert with the given ID exists.
    #[must_use]
    pub fn dismiss_alert(&self, alert_id: &str) -> bool {
        let mut alerts = self.alerts.write();
        let initial_len = alerts.len();
        alerts.retain(|a| a.id != alert_id);
        alerts.len() != initial_len
    }

    /// Gets all alerts.
    #[must_use]
    pub fn get_alerts(&self) -> Vec<Alert> {
        self.alerts.read().clone()
    }

    /// Gets alerts filtered by status.
    #[must_use]
    pub fn get_alerts_by_status(&self, status: AlertStatus) -> Vec<Alert> {
        self.alerts.read().iter().filter(|a| a.status == status).cloned().collect()
    }

    /// Gets a specific alert by ID.
    #[must_use]
    pub fn get_alert(&self, alert_id: &str) -> Option<Alert> {
        self.alerts.read().iter().find(|a| a.id == alert_id).cloned()
    }

    /// Gets the count of active alerts.
    #[must_use]
    pub fn active_alert_count(&self) -> usize {
        self.alerts.read().iter().filter(|a| a.status == AlertStatus::Active).count()
    }

    /// Gets the count of alerts by severity.
    #[must_use]
    pub fn alert_count_by_severity(&self, severity: super::types::AlertSeverity) -> usize {
        self.alerts
            .read()
            .iter()
            .filter(|a| a.severity == severity && a.status == AlertStatus::Active)
            .count()
    }
}

impl Default for AlertManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::types::{AlertCondition, AlertSeverity};

    #[test]
    fn test_add_rule() {
        let manager = AlertManager::new();
        let rule = AlertRule::new(
            "rule1".to_string(),
            "Test Rule".to_string(),
            AlertCondition::AllUpstreamsDown,
            AlertSeverity::Critical,
            true,
            300,
        );

        assert!(manager.add_rule(rule.clone()));
        assert!(!manager.add_rule(rule)); // Duplicate should fail
    }

    #[test]
    fn test_remove_rule() {
        let manager = AlertManager::new();
        let rule = AlertRule::new(
            "rule1".to_string(),
            "Test Rule".to_string(),
            AlertCondition::AllUpstreamsDown,
            AlertSeverity::Critical,
            true,
            300,
        );

        let _ = manager.add_rule(rule);
        assert!(manager.remove_rule("rule1"));
        assert!(!manager.remove_rule("rule1")); // Already removed
    }

    #[test]
    fn test_toggle_rule() {
        let manager = AlertManager::new();
        let rule = AlertRule::new(
            "rule1".to_string(),
            "Test Rule".to_string(),
            AlertCondition::AllUpstreamsDown,
            AlertSeverity::Critical,
            true,
            300,
        );

        let _ = manager.add_rule(rule);
        assert_eq!(manager.toggle_rule("rule1"), Some(false));
        assert_eq!(manager.toggle_rule("rule1"), Some(true));
        assert_eq!(manager.toggle_rule("nonexistent"), None);
    }

    #[test]
    fn test_create_and_get_alert() {
        let manager = AlertManager::new();
        let alert = Alert::new(
            "alert1".to_string(),
            "rule1".to_string(),
            AlertSeverity::Warning,
            "Test alert".to_string(),
        );

        manager.create_alert(alert);
        assert!(manager.get_alert("alert1").is_some());
        assert_eq!(manager.get_alerts().len(), 1);
    }

    #[test]
    fn test_acknowledge_alert() {
        let manager = AlertManager::new();
        let alert = Alert::new(
            "alert1".to_string(),
            "rule1".to_string(),
            AlertSeverity::Warning,
            "Test alert".to_string(),
        );

        manager.create_alert(alert);
        assert!(manager.acknowledge_alert("alert1"));

        let acknowledged = manager.get_alert("alert1").unwrap();
        assert_eq!(acknowledged.status, AlertStatus::Acknowledged);
        assert!(acknowledged.acknowledged_at.is_some());
    }

    #[test]
    fn test_resolve_alert() {
        let manager = AlertManager::new();
        let alert = Alert::new(
            "alert1".to_string(),
            "rule1".to_string(),
            AlertSeverity::Warning,
            "Test alert".to_string(),
        );

        manager.create_alert(alert);
        assert!(manager.resolve_alert("alert1"));

        let resolved = manager.get_alert("alert1").unwrap();
        assert_eq!(resolved.status, AlertStatus::Resolved);
        assert!(resolved.resolved_at.is_some());
    }

    #[test]
    fn test_dismiss_alert() {
        let manager = AlertManager::new();
        let alert = Alert::new(
            "alert1".to_string(),
            "rule1".to_string(),
            AlertSeverity::Warning,
            "Test alert".to_string(),
        );

        manager.create_alert(alert);
        assert!(manager.dismiss_alert("alert1"));
        assert!(manager.get_alert("alert1").is_none());
    }

    #[test]
    fn test_get_alerts_by_status() {
        let manager = AlertManager::new();

        let alert1 = Alert::new(
            "alert1".to_string(),
            "rule1".to_string(),
            AlertSeverity::Warning,
            "Active alert".to_string(),
        );

        let mut alert2 = Alert::new(
            "alert2".to_string(),
            "rule1".to_string(),
            AlertSeverity::Critical,
            "Resolved alert".to_string(),
        );
        alert2.resolve();

        manager.create_alert(alert1);
        manager.create_alert(alert2);

        let active = manager.get_alerts_by_status(AlertStatus::Active);
        assert_eq!(active.len(), 1);

        let resolved = manager.get_alerts_by_status(AlertStatus::Resolved);
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn test_active_alert_count() {
        let manager = AlertManager::new();

        let alert1 = Alert::new(
            "alert1".to_string(),
            "rule1".to_string(),
            AlertSeverity::Warning,
            "Active alert".to_string(),
        );

        let mut alert2 = Alert::new(
            "alert2".to_string(),
            "rule1".to_string(),
            AlertSeverity::Critical,
            "Resolved alert".to_string(),
        );
        alert2.resolve();

        manager.create_alert(alert1);
        manager.create_alert(alert2);

        assert_eq!(manager.active_alert_count(), 1);
    }

    #[test]
    fn test_alert_capacity_bounds() {
        let manager = AlertManager::new();

        // Create alerts up to just under 90% capacity (899 alerts)
        for i in 0..899 {
            let alert = Alert::new(
                format!("alert{i}"),
                "rule1".to_string(),
                AlertSeverity::Info,
                format!("Alert {i}"),
            );
            manager.create_alert(alert);
        }

        assert_eq!(manager.get_alerts().len(), 899);

        // Add one more to reach 900 (90% threshold) - cleanup triggers on next insert
        let alert_899 = Alert::new(
            "alert899".to_string(),
            "rule1".to_string(),
            AlertSeverity::Info,
            "Alert 899".to_string(),
        );
        manager.create_alert(alert_899);
        assert_eq!(manager.get_alerts().len(), 900);

        // Adding one more alert triggers cleanup (removes resolved, but none exist)
        // Since all are active and we're at 900 (>= 90%), cleanup runs but removes nothing
        // Then the new alert is added = 901
        let new_alert = Alert::new(
            "alert900".to_string(),
            "rule1".to_string(),
            AlertSeverity::Warning,
            "Alert 900".to_string(),
        );
        manager.create_alert(new_alert);

        // Should have 901 alerts (900 active + 1 new, no resolved to remove)
        let alerts = manager.get_alerts();
        assert_eq!(alerts.len(), 901);

        // Verify all alerts are active
        let active_count = alerts.iter().filter(|a| a.status == AlertStatus::Active).count();
        assert_eq!(active_count, 901);
    }

    #[test]
    fn test_alert_capacity_eviction_when_no_resolved() {
        let manager = AlertManager::new();

        // Fill to max capacity with all active alerts
        for i in 0..1000 {
            let alert = Alert::new(
                format!("alert{i}"),
                "rule1".to_string(),
                AlertSeverity::Warning,
                format!("Alert {i}"),
            );
            manager.create_alert(alert);
        }

        assert_eq!(manager.get_alerts().len(), 1000);

        // Add another alert - should evict oldest
        let new_alert = Alert::new(
            "alert1000".to_string(),
            "rule1".to_string(),
            AlertSeverity::Critical,
            "Alert 1000".to_string(),
        );
        manager.create_alert(new_alert);

        // Should still be at max capacity
        let alerts = manager.get_alerts();
        assert_eq!(alerts.len(), 1000);

        // The oldest alert (alert0) should be gone
        assert!(manager.get_alert("alert0").is_none());

        // The newest alert should be present
        assert!(manager.get_alert("alert1000").is_some());

        // alert1 should still be present (second oldest)
        assert!(manager.get_alert("alert1").is_some());
    }

    #[test]
    fn test_alert_capacity_prefers_resolved_eviction() {
        let manager = AlertManager::new();

        // Create 850 active alerts (under 90% threshold)
        for i in 0..850 {
            let alert = Alert::new(
                format!("active{i}"),
                "rule1".to_string(),
                AlertSeverity::Warning,
                format!("Active alert {i}"),
            );
            manager.create_alert(alert);
        }

        assert_eq!(manager.get_alerts().len(), 850);

        // Create 50 resolved alerts (now at 900 = 90% threshold)
        for i in 0..50 {
            let mut alert = Alert::new(
                format!("resolved{i}"),
                "rule1".to_string(),
                AlertSeverity::Info,
                format!("Resolved alert {i}"),
            );
            alert.resolve();
            manager.create_alert(alert);
        }

        // Note: cleanup triggers at 900, so some resolved alerts get cleaned during insertion
        // After adding all 50 resolved, we have 850 active (resolved were cleaned as we added them)
        // Actually, let's verify what we have
        let alerts_before = manager.get_alerts();
        let active_before =
            alerts_before.iter().filter(|a| matches!(a.status, AlertStatus::Active)).count();

        // At 900+ alerts, cleanup removes resolved on each insert
        // So we should have 850 active and any resolved that survived
        assert_eq!(active_before, 850);

        // Add one more alert
        let new_alert = Alert::new(
            "new_alert".to_string(),
            "rule1".to_string(),
            AlertSeverity::Critical,
            "New alert".to_string(),
        );
        manager.create_alert(new_alert);

        let alerts = manager.get_alerts();

        // All original active alerts should still be present
        for i in 0..850 {
            assert!(manager.get_alert(&format!("active{i}")).is_some());
        }

        // New alert should be present
        assert!(manager.get_alert("new_alert").is_some());

        // Should have 851 active alerts total
        let active_count =
            alerts.iter().filter(|a| matches!(a.status, AlertStatus::Active)).count();
        assert_eq!(active_count, 851);
    }
}
