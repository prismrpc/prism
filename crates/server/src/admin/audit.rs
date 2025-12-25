//! Audit logging for admin API mutations.
//!
//! Provides structured audit logging for all write operations in the Admin API
//! to support security analysis and compliance requirements.

use serde::Serialize;
use std::net::SocketAddr;
use tracing::info;

/// Audit event for admin API mutations.
///
/// Records structured information about each mutation operation including
/// operation type, resource affected, actor information, and success status.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    /// ISO 8601 timestamp of the operation
    pub timestamp: String,
    /// Type of operation performed
    pub operation: AuditOperation,
    /// Resource type being modified
    pub resource_type: &'static str,
    /// Resource identifier (ID, name, etc.)
    pub resource_id: String,
    /// Client IP address (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
    /// Whether the operation succeeded
    pub success: bool,
    /// Optional details about the change (before/after state, parameters, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Type of mutation operation being audited.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuditOperation {
    /// Resource creation
    Create,
    /// Resource modification
    Update,
    /// Resource deletion
    Delete,
}

impl AuditEvent {
    /// Creates a new audit event for a mutation operation.
    ///
    /// # Arguments
    /// * `operation` - Type of operation (CREATE, UPDATE, DELETE)
    /// * `resource_type` - Type of resource affected (upstream, `alert_rule`, `api_key`, etc.)
    /// * `resource_id` - Identifier for the resource
    #[must_use]
    pub fn new(
        operation: AuditOperation,
        resource_type: &'static str,
        resource_id: impl Into<String>,
    ) -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339(),
            operation,
            resource_type,
            resource_id: resource_id.into(),
            client_ip: None,
            success: true,
            details: None,
        }
    }

    /// Adds client IP address to the audit event.
    #[must_use]
    pub fn with_client_ip(mut self, addr: Option<SocketAddr>) -> Self {
        self.client_ip = addr.map(|a| a.ip().to_string());
        self
    }

    /// Adds additional details about the operation.
    ///
    /// Details can include before/after state, request parameters, or any
    /// other contextual information useful for audit trails.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// Marks the operation as failed.
    #[must_use]
    pub fn failed(mut self) -> Self {
        self.success = false;
        self
    }

    /// Logs the audit event using structured tracing.
    ///
    /// Events are logged to the "audit" target at INFO level with all
    /// fields as structured data for easy parsing and analysis.
    pub fn log(self) {
        info!(
            target: "audit",
            timestamp = %self.timestamp,
            operation = ?self.operation,
            resource_type = self.resource_type,
            resource_id = %self.resource_id,
            client_ip = ?self.client_ip,
            success = self.success,
            details = ?self.details,
            "admin_api_audit"
        );
    }
}

/// Logs a successful resource creation.
///
/// # Arguments
/// * `resource_type` - Type of resource created (e.g., "upstream", "`alert_rule`")
/// * `resource_id` - Identifier of the created resource
/// * `client_ip` - Optional client socket address
pub fn log_create(
    resource_type: &'static str,
    resource_id: impl Into<String>,
    client_ip: Option<SocketAddr>,
) {
    AuditEvent::new(AuditOperation::Create, resource_type, resource_id)
        .with_client_ip(client_ip)
        .log();
}

/// Logs a successful resource update.
///
/// # Arguments
/// * `resource_type` - Type of resource updated
/// * `resource_id` - Identifier of the updated resource
/// * `client_ip` - Optional client socket address
/// * `details` - Optional before/after state or other change details
pub fn log_update(
    resource_type: &'static str,
    resource_id: impl Into<String>,
    client_ip: Option<SocketAddr>,
    details: Option<serde_json::Value>,
) {
    let mut event = AuditEvent::new(AuditOperation::Update, resource_type, resource_id)
        .with_client_ip(client_ip);
    if let Some(d) = details {
        event = event.with_details(d);
    }
    event.log();
}

/// Logs a successful resource deletion.
///
/// # Arguments
/// * `resource_type` - Type of resource deleted
/// * `resource_id` - Identifier of the deleted resource
/// * `client_ip` - Optional client socket address
pub fn log_delete(
    resource_type: &'static str,
    resource_id: impl Into<String>,
    client_ip: Option<SocketAddr>,
) {
    AuditEvent::new(AuditOperation::Delete, resource_type, resource_id)
        .with_client_ip(client_ip)
        .log();
}

/// Logs a failed operation attempt.
///
/// # Arguments
/// * `operation` - Type of operation that failed
/// * `resource_type` - Type of resource involved
/// * `resource_id` - Identifier of the resource
/// * `client_ip` - Optional client socket address
/// * `details` - Optional error details or context
pub fn log_failed(
    operation: AuditOperation,
    resource_type: &'static str,
    resource_id: impl Into<String>,
    client_ip: Option<SocketAddr>,
    details: Option<serde_json::Value>,
) {
    let mut event = AuditEvent::new(operation, resource_type, resource_id)
        .with_client_ip(client_ip)
        .failed();
    if let Some(d) = details {
        event = event.with_details(d);
    }
    event.log();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_audit_event_creation() {
        let event = AuditEvent::new(AuditOperation::Create, "upstream", "test-id");
        assert_eq!(event.resource_type, "upstream");
        assert_eq!(event.resource_id, "test-id");
        assert!(event.success);
        assert!(event.client_ip.is_none());
        assert!(event.details.is_none());
    }

    #[test]
    fn test_audit_event_with_ip() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8080);
        let event = AuditEvent::new(AuditOperation::Update, "alert_rule", "rule-123")
            .with_client_ip(Some(addr));
        assert_eq!(event.client_ip, Some("192.168.1.1".to_string()));
    }

    #[test]
    fn test_audit_event_with_details() {
        let details = serde_json::json!({"weight": {"old": 10, "new": 20}});
        let event =
            AuditEvent::new(AuditOperation::Update, "upstream", "up-1").with_details(details);
        assert!(event.details.is_some());
    }

    #[test]
    fn test_audit_event_failed() {
        let event = AuditEvent::new(AuditOperation::Delete, "api_key", "key-456").failed();
        assert!(!event.success);
    }
}
