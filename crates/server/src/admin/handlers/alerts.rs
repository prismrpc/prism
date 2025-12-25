//! Alert endpoint handlers.

#![allow(clippy::missing_errors_doc)]

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use prism_core::alerts::{Alert, AlertCondition, AlertRule, AlertSeverity, AlertStatus};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use crate::admin::{audit, types::AdminApiError, AdminState};

/// Maximum length for alert rule names.
const MAX_ALERT_RULE_NAME_LENGTH: usize = 256;
/// Maximum cooldown period in seconds (24 hours).
const MAX_COOLDOWN_SECONDS: u64 = 86400;

/// Validates an alert rule's name and cooldown period.
///
/// # Errors
///
/// Returns an error if:
/// - The name is empty
/// - The name exceeds the maximum length
/// - The cooldown period exceeds the maximum allowed value
fn validate_alert_rule(name: &str, cooldown_seconds: u64) -> Result<(), String> {
    if name.is_empty() {
        return Err("Alert rule name cannot be empty".to_string());
    }
    if name.len() > MAX_ALERT_RULE_NAME_LENGTH {
        return Err(format!(
            "Alert rule name too long. Maximum length is {MAX_ALERT_RULE_NAME_LENGTH} characters."
        ));
    }
    // MIN_COOLDOWN_SECONDS is 0, so this check is always false - removed
    if cooldown_seconds > MAX_COOLDOWN_SECONDS {
        return Err(format!(
            "Cooldown too long. Maximum is {MAX_COOLDOWN_SECONDS} seconds (24 hours)."
        ));
    }
    Ok(())
}

// ========== Request/Response Types ==========

/// Query parameters for listing alerts.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ListAlertsQuery {
    /// Filter by status (optional).
    pub status: Option<String>,
}

/// Request body for creating an alert rule.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAlertRuleRequest {
    pub name: String,
    pub condition: AlertCondition,
    pub severity: AlertSeverity,
    pub enabled: bool,
    pub cooldown_seconds: u64,
}

/// Request body for updating an alert rule.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAlertRuleRequest {
    pub name: String,
    pub condition: AlertCondition,
    pub severity: AlertSeverity,
    pub enabled: bool,
    pub cooldown_seconds: u64,
}

/// Response for successful operations.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SuccessResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Response for toggle operation.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ToggleResponse {
    pub enabled: bool,
}

/// Request body for bulk alert operations.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct BulkAlertRequest {
    pub ids: Vec<String>,
}

/// Response for bulk alert operations.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BulkAlertResponse {
    pub success: bool,
    pub resolved: Option<usize>,
    pub dismissed: Option<usize>,
}

// ========== Alert Endpoints ==========

/// GET /admin/alerts
///
/// Lists all alerts with optional status filter.
#[utoipa::path(
    get,
    path = "/admin/alerts",
    tag = "Alerts",
    params(
        ("status" = Option<String>, Query, description = "Filter by status: active, resolved, acknowledged")
    ),
    responses(
        (status = 200, description = "List of alerts", body = Vec<Alert>),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn list_alerts(
    State(state): State<AdminState>,
    Query(query): Query<ListAlertsQuery>,
) -> impl IntoResponse {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    let alerts = if let Some(status_str) = query.status {
        // Parse status and filter
        match status_str.to_lowercase().as_str() {
            "active" => alert_manager.get_alerts_by_status(AlertStatus::Active),
            "resolved" => alert_manager.get_alerts_by_status(AlertStatus::Resolved),
            "acknowledged" => alert_manager.get_alerts_by_status(AlertStatus::Acknowledged),
            _ => alert_manager.get_alerts(), // Invalid status, return all
        }
    } else {
        alert_manager.get_alerts()
    };

    Json(alerts)
}

/// GET /admin/alerts/:id
///
/// Gets details of a specific alert.
#[utoipa::path(
    get,
    path = "/admin/alerts/{id}",
    tag = "Alerts",
    params(
        ("id" = String, Path, description = "Alert ID")
    ),
    responses(
        (status = 200, description = "Alert details", body = Alert),
        (status = 404, description = "Alert not found", body = crate::admin::types::AdminApiError)
    )
)]
pub async fn get_alert(
    State(state): State<AdminState>,
    Path(alert_id): Path<String>,
) -> Result<Json<Alert>, crate::admin::types::AdminApiErrorResponse> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    alert_manager
        .get_alert(&alert_id)
        .map(Json)
        .ok_or_else(|| AdminApiError::alert_not_found(&alert_id))
}

/// POST /admin/alerts/:id/acknowledge
///
/// Acknowledges an alert.
#[utoipa::path(
    post,
    path = "/admin/alerts/{id}/acknowledge",
    tag = "Alerts",
    params(
        ("id" = String, Path, description = "Alert ID")
    ),
    responses(
        (status = 200, description = "Alert acknowledged", body = SuccessResponse),
        (status = 404, description = "Alert not found", body = crate::admin::types::AdminApiError)
    )
)]
pub async fn acknowledge_alert(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(alert_id): Path<String>,
) -> Result<Json<SuccessResponse>, crate::admin::types::AdminApiErrorResponse> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    let alert_manager = &state.proxy_engine.get_alert_manager();

    if alert_manager.acknowledge_alert(&alert_id) {
        // Audit log the acknowledgement
        audit::log_update("alert", &alert_id, Some(addr), None);

        tracing::info!(
            correlation_id = ?correlation_id,
            alert_id = %alert_id,
            "acknowledged alert"
        );

        Ok(Json(SuccessResponse { success: true, message: Some("Alert acknowledged".to_string()) }))
    } else {
        Err(AdminApiError::alert_not_found(&alert_id))
    }
}

/// POST /admin/alerts/:id/resolve
///
/// Resolves an alert.
#[utoipa::path(
    post,
    path = "/admin/alerts/{id}/resolve",
    tag = "Alerts",
    params(
        ("id" = String, Path, description = "Alert ID")
    ),
    responses(
        (status = 200, description = "Alert resolved", body = SuccessResponse),
        (status = 404, description = "Alert not found", body = crate::admin::types::AdminApiError)
    )
)]
pub async fn resolve_alert(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(alert_id): Path<String>,
) -> Result<Json<SuccessResponse>, crate::admin::types::AdminApiErrorResponse> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    let alert_manager = &state.proxy_engine.get_alert_manager();

    if alert_manager.resolve_alert(&alert_id) {
        // Audit log the resolution
        audit::log_update("alert", &alert_id, Some(addr), None);

        tracing::info!(
            correlation_id = ?correlation_id,
            alert_id = %alert_id,
            "resolved alert"
        );

        Ok(Json(SuccessResponse { success: true, message: Some("Alert resolved".to_string()) }))
    } else {
        Err(AdminApiError::alert_not_found(&alert_id))
    }
}

/// POST /admin/alerts/bulk-resolve
///
/// Resolves multiple alerts at once.
#[utoipa::path(
    post,
    path = "/admin/alerts/bulk-resolve",
    tag = "Alerts",
    request_body = BulkAlertRequest,
    responses(
        (status = 200, description = "Alerts resolved", body = BulkAlertResponse)
    )
)]
pub async fn bulk_resolve_alerts(
    State(state): State<AdminState>,
    Json(payload): Json<BulkAlertRequest>,
) -> impl IntoResponse {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    let mut resolved_count = 0;
    for alert_id in &payload.ids {
        if alert_manager.resolve_alert(alert_id) {
            resolved_count += 1;
        }
    }

    Json(BulkAlertResponse { success: true, resolved: Some(resolved_count), dismissed: None })
}

/// DELETE /admin/alerts/:id
///
/// Dismisses (removes) an alert.
#[utoipa::path(
    delete,
    path = "/admin/alerts/{id}",
    tag = "Alerts",
    params(
        ("id" = String, Path, description = "Alert ID")
    ),
    responses(
        (status = 200, description = "Alert dismissed", body = SuccessResponse),
        (status = 404, description = "Alert not found", body = crate::admin::types::AdminApiError)
    )
)]
pub async fn dismiss_alert(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(alert_id): Path<String>,
) -> Result<Json<SuccessResponse>, crate::admin::types::AdminApiErrorResponse> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    let alert_manager = &state.proxy_engine.get_alert_manager();

    if alert_manager.dismiss_alert(&alert_id) {
        // Audit log the dismissal
        audit::log_delete("alert", &alert_id, Some(addr));

        tracing::info!(
            correlation_id = ?correlation_id,
            alert_id = %alert_id,
            "dismissed alert"
        );

        Ok(Json(SuccessResponse { success: true, message: Some("Alert dismissed".to_string()) }))
    } else {
        Err(AdminApiError::alert_not_found(&alert_id))
    }
}

/// POST /admin/alerts/bulk-dismiss
///
/// Dismisses multiple alerts at once.
#[utoipa::path(
    post,
    path = "/admin/alerts/bulk-dismiss",
    tag = "Alerts",
    request_body = BulkAlertRequest,
    responses(
        (status = 200, description = "Alerts dismissed", body = BulkAlertResponse)
    )
)]
pub async fn bulk_dismiss_alerts(
    State(state): State<AdminState>,
    Json(payload): Json<BulkAlertRequest>,
) -> impl IntoResponse {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    let mut dismissed_count = 0;
    for alert_id in &payload.ids {
        if alert_manager.dismiss_alert(alert_id) {
            dismissed_count += 1;
        }
    }

    Json(BulkAlertResponse { success: true, resolved: None, dismissed: Some(dismissed_count) })
}

// ========== Alert Rule Endpoints ==========

/// GET /admin/alerts/rules
///
/// Lists all alert rules.
#[utoipa::path(
    get,
    path = "/admin/alerts/rules",
    tag = "Alerts",
    responses(
        (status = 200, description = "List of alert rules", body = Vec<AlertRule>)
    )
)]
pub async fn list_rules(State(state): State<AdminState>) -> impl IntoResponse {
    let alert_manager = &state.proxy_engine.get_alert_manager();
    Json(alert_manager.get_rules())
}

/// POST /admin/alerts/rules
///
/// Creates a new alert rule.
#[utoipa::path(
    post,
    path = "/admin/alerts/rules",
    tag = "Alerts",
    request_body = CreateAlertRuleRequest,
    responses(
        (status = 200, description = "Alert rule created", body = AlertRule),
        (status = 409, description = "Rule with same ID already exists", body = crate::admin::types::AdminApiError)
    )
)]
pub async fn create_rule(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<CreateAlertRuleRequest>,
) -> Result<Json<AlertRule>, crate::admin::types::AdminApiErrorResponse> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    let alert_manager = &state.proxy_engine.get_alert_manager();

    // Validate the alert rule
    validate_alert_rule(&payload.name, payload.cooldown_seconds)
        .map_err(AdminApiError::validation_error)?;

    // Generate a unique ID for the rule
    let rule_id = uuid::Uuid::new_v4().to_string();

    let rule = AlertRule::new(
        rule_id.clone(),
        payload.name.clone(),
        payload.condition,
        payload.severity,
        payload.enabled,
        payload.cooldown_seconds,
    );

    if alert_manager.add_rule(rule.clone()) {
        // Audit log the rule creation
        audit::log_create("alert_rule", &rule_id, Some(addr));

        tracing::info!(
            correlation_id = ?correlation_id,
            rule_id = %rule_id,
            rule_name = %payload.name,
            "created alert rule"
        );

        Ok(Json(rule))
    } else {
        Err(AdminApiError::rule_conflict())
    }
}

/// PUT /admin/alerts/rules/:id
///
/// Updates an existing alert rule.
#[utoipa::path(
    put,
    path = "/admin/alerts/rules/{id}",
    tag = "Alerts",
    params(
        ("id" = String, Path, description = "Alert rule ID")
    ),
    request_body = UpdateAlertRuleRequest,
    responses(
        (status = 200, description = "Alert rule updated", body = AlertRule),
        (status = 404, description = "Alert rule not found", body = crate::admin::types::AdminApiError)
    )
)]
pub async fn update_rule(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(rule_id): Path<String>,
    Json(payload): Json<UpdateAlertRuleRequest>,
) -> Result<Json<AlertRule>, crate::admin::types::AdminApiErrorResponse> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    let alert_manager = &state.proxy_engine.get_alert_manager();

    // Validate the alert rule
    validate_alert_rule(&payload.name, payload.cooldown_seconds)
        .map_err(AdminApiError::validation_error)?;

    let rule = AlertRule::new(
        rule_id.clone(),
        payload.name.clone(),
        payload.condition,
        payload.severity,
        payload.enabled,
        payload.cooldown_seconds,
    );

    if alert_manager.update_rule(rule.clone()) {
        // Audit log the rule update
        audit::log_update("alert_rule", &rule_id, Some(addr), None);

        tracing::info!(
            correlation_id = ?correlation_id,
            rule_id = %rule_id,
            rule_name = %payload.name,
            "updated alert rule"
        );

        Ok(Json(rule))
    } else {
        Err(AdminApiError::rule_not_found(&rule_id))
    }
}

/// DELETE /admin/alerts/rules/:id
///
/// Deletes an alert rule.
#[utoipa::path(
    delete,
    path = "/admin/alerts/rules/{id}",
    tag = "Alerts",
    params(
        ("id" = String, Path, description = "Alert rule ID")
    ),
    responses(
        (status = 200, description = "Alert rule deleted", body = SuccessResponse),
        (status = 404, description = "Alert rule not found", body = crate::admin::types::AdminApiError)
    )
)]
pub async fn delete_rule(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(rule_id): Path<String>,
) -> Result<Json<SuccessResponse>, crate::admin::types::AdminApiErrorResponse> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    let alert_manager = &state.proxy_engine.get_alert_manager();

    if alert_manager.remove_rule(&rule_id) {
        // Audit log the rule deletion
        audit::log_delete("alert_rule", &rule_id, Some(addr));

        tracing::info!(
            correlation_id = ?correlation_id,
            rule_id = %rule_id,
            "deleted alert rule"
        );

        Ok(Json(SuccessResponse { success: true, message: Some("Rule deleted".to_string()) }))
    } else {
        Err(AdminApiError::rule_not_found(&rule_id))
    }
}

/// PUT /admin/alerts/rules/:id/toggle
///
/// Toggles an alert rule's enabled status.
#[utoipa::path(
    put,
    path = "/admin/alerts/rules/{id}/toggle",
    tag = "Alerts",
    params(
        ("id" = String, Path, description = "Alert rule ID")
    ),
    responses(
        (status = 200, description = "Alert rule toggled", body = ToggleResponse),
        (status = 404, description = "Alert rule not found", body = crate::admin::types::AdminApiError)
    )
)]
pub async fn toggle_rule(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(rule_id): Path<String>,
) -> Result<Json<ToggleResponse>, crate::admin::types::AdminApiErrorResponse> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    alert_manager
        .toggle_rule(&rule_id)
        .map(|enabled| {
            // Audit log the rule toggle
            let details = serde_json::json!({"enabled": enabled});
            audit::log_update("alert_rule", &rule_id, Some(addr), Some(details));
            Json(ToggleResponse { enabled })
        })
        .ok_or_else(|| AdminApiError::rule_not_found(&rule_id))
}
