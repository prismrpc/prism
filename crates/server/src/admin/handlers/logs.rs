//! Log endpoint handlers.
//!
//! Provides endpoints for querying system logs, listing log services/targets,
//! and exporting log data.

#![allow(clippy::missing_errors_doc)]

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};

use crate::admin::{
    types::{LogQueryParams, LogQueryResponse, LogServicesResponse},
    AdminState,
};

/// GET /admin/logs
///
/// Query logs with filtering and pagination.
#[utoipa::path(
    get,
    path = "/admin/logs",
    tag = "Logs",
    params(
        ("level" = Option<String>, Query, description = "Filter by log level (error, warn, info, debug)"),
        ("target" = Option<String>, Query, description = "Filter by target/module name"),
        ("search" = Option<String>, Query, description = "Search in log message text"),
        ("from" = Option<String>, Query, description = "Start timestamp (ISO 8601)"),
        ("to" = Option<String>, Query, description = "End timestamp (ISO 8601)"),
        ("limit" = Option<usize>, Query, description = "Maximum entries to return (default: 100)"),
        ("offset" = Option<usize>, Query, description = "Pagination offset (default: 0)")
    ),
    responses(
        (status = 200, description = "Log query results", body = LogQueryResponse),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn query_logs(
    State(state): State<AdminState>,
    Query(params): Query<LogQueryParams>,
) -> impl IntoResponse {
    let response = state.log_buffer.query(&params);
    Json(response)
}

/// GET /admin/logs/services
///
/// Returns all unique log targets/services that have logged messages.
#[utoipa::path(
    get,
    path = "/admin/logs/services",
    tag = "Logs",
    responses(
        (status = 200, description = "List of log services/targets", body = LogServicesResponse),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn list_log_services(State(state): State<AdminState>) -> impl IntoResponse {
    let services = state.log_buffer.get_targets();

    Json(LogServicesResponse { services })
}

/// GET /admin/logs/export
///
/// Export logs as JSON with optional filtering.
/// Returns the same data as `query_logs` but with `Content-Disposition` header
/// to prompt file download.
#[utoipa::path(
    get,
    path = "/admin/logs/export",
    tag = "Logs",
    params(
        ("level" = Option<String>, Query, description = "Filter by log level (error, warn, info, debug)"),
        ("target" = Option<String>, Query, description = "Filter by target/module name"),
        ("search" = Option<String>, Query, description = "Search in log message text"),
        ("from" = Option<String>, Query, description = "Start timestamp (ISO 8601)"),
        ("to" = Option<String>, Query, description = "End timestamp (ISO 8601)"),
        ("limit" = Option<usize>, Query, description = "Maximum entries to return (default: 100)"),
        ("offset" = Option<usize>, Query, description = "Pagination offset (default: 0)")
    ),
    responses(
        (status = 200, description = "Log export file", body = LogQueryResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn export_logs(
    State(state): State<AdminState>,
    Query(params): Query<LogQueryParams>,
) -> impl IntoResponse {
    let response = state.log_buffer.query(&params);

    // Generate filename with timestamp
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("prism_logs_{timestamp}.json");
    let content_disposition = format!("attachment; filename=\"{filename}\"");

    // Return with Content-Disposition header for download
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition),
        ],
        Json(response),
    )
}
