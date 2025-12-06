//! Alert endpoint handlers.

#![allow(clippy::missing_errors_doc)]

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use prism_core::alerts::{Alert, AlertCondition, AlertRule, AlertSeverity, AlertStatus};
use serde::{Deserialize, Serialize};

use crate::admin::AdminState;

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
        (status = 404, description = "Alert not found")
    )
)]
pub async fn get_alert(
    State(state): State<AdminState>,
    Path(alert_id): Path<String>,
) -> Result<Json<Alert>, StatusCode> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    alert_manager.get_alert(&alert_id).map(Json).ok_or(StatusCode::NOT_FOUND)
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
        (status = 404, description = "Alert not found")
    )
)]
pub async fn acknowledge_alert(
    State(state): State<AdminState>,
    Path(alert_id): Path<String>,
) -> Result<Json<SuccessResponse>, StatusCode> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    if alert_manager.acknowledge_alert(&alert_id) {
        Ok(Json(SuccessResponse { success: true, message: Some("Alert acknowledged".to_string()) }))
    } else {
        Err(StatusCode::NOT_FOUND)
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
        (status = 404, description = "Alert not found")
    )
)]
pub async fn resolve_alert(
    State(state): State<AdminState>,
    Path(alert_id): Path<String>,
) -> Result<Json<SuccessResponse>, StatusCode> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    if alert_manager.resolve_alert(&alert_id) {
        Ok(Json(SuccessResponse { success: true, message: Some("Alert resolved".to_string()) }))
    } else {
        Err(StatusCode::NOT_FOUND)
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
        (status = 404, description = "Alert not found")
    )
)]
pub async fn dismiss_alert(
    State(state): State<AdminState>,
    Path(alert_id): Path<String>,
) -> Result<Json<SuccessResponse>, StatusCode> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    if alert_manager.dismiss_alert(&alert_id) {
        Ok(Json(SuccessResponse { success: true, message: Some("Alert dismissed".to_string()) }))
    } else {
        Err(StatusCode::NOT_FOUND)
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
        (status = 409, description = "Rule with same ID already exists")
    )
)]
pub async fn create_rule(
    State(state): State<AdminState>,
    Json(payload): Json<CreateAlertRuleRequest>,
) -> Result<Json<AlertRule>, StatusCode> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    // Generate a unique ID for the rule
    let rule_id = uuid::Uuid::new_v4().to_string();

    let rule = AlertRule::new(
        rule_id,
        payload.name,
        payload.condition,
        payload.severity,
        payload.enabled,
        payload.cooldown_seconds,
    );

    if alert_manager.add_rule(rule.clone()) {
        Ok(Json(rule))
    } else {
        Err(StatusCode::CONFLICT)
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
        (status = 404, description = "Alert rule not found")
    )
)]
pub async fn update_rule(
    State(state): State<AdminState>,
    Path(rule_id): Path<String>,
    Json(payload): Json<UpdateAlertRuleRequest>,
) -> Result<Json<AlertRule>, StatusCode> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    let rule = AlertRule::new(
        rule_id,
        payload.name,
        payload.condition,
        payload.severity,
        payload.enabled,
        payload.cooldown_seconds,
    );

    if alert_manager.update_rule(rule.clone()) {
        Ok(Json(rule))
    } else {
        Err(StatusCode::NOT_FOUND)
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
        (status = 404, description = "Alert rule not found")
    )
)]
pub async fn delete_rule(
    State(state): State<AdminState>,
    Path(rule_id): Path<String>,
) -> Result<Json<SuccessResponse>, StatusCode> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    if alert_manager.remove_rule(&rule_id) {
        Ok(Json(SuccessResponse { success: true, message: Some("Rule deleted".to_string()) }))
    } else {
        Err(StatusCode::NOT_FOUND)
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
        (status = 404, description = "Alert rule not found")
    )
)]
pub async fn toggle_rule(
    State(state): State<AdminState>,
    Path(rule_id): Path<String>,
) -> Result<Json<ToggleResponse>, StatusCode> {
    let alert_manager = &state.proxy_engine.get_alert_manager();

    alert_manager
        .toggle_rule(&rule_id)
        .map(|enabled| Json(ToggleResponse { enabled }))
        .ok_or(StatusCode::NOT_FOUND)
}
