//! System endpoint handlers.

#![allow(clippy::missing_errors_doc)]

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use crate::admin::{
    types::{
        Capabilities, HealthCheck, ServerSettings, SystemHealth, SystemInfo, SystemStatus,
        UpdateServerSettingsRequest, UpdateSettingsResponse,
    },
    AdminState,
};

/// GET /admin/system/status
///
/// Returns current operational status of the Prism instance.
#[utoipa::path(
    get,
    path = "/admin/system/status",
    tag = "System",
    responses(
        (status = 200, description = "System status", body = SystemStatus)
    )
)]
pub async fn get_status(State(state): State<AdminState>) -> impl IntoResponse {
    let upstream_stats = state.proxy_engine.get_upstream_stats().await;

    // Determine health based on healthy upstreams
    let health = if upstream_stats.healthy_upstreams == upstream_stats.total_upstreams &&
        upstream_stats.total_upstreams > 0
    {
        SystemHealth::Healthy
    } else if upstream_stats.healthy_upstreams > 0 {
        SystemHealth::Degraded
    } else {
        SystemHealth::Unhealthy
    };

    let chain_tip = state.chain_state.current_tip();
    let finalized = state.chain_state.finalized_block();

    let status = SystemStatus {
        instance_name: state.config.admin.instance_name.clone(),
        health,
        uptime: state.uptime_string(),
        chain_tip,
        chain_tip_age: format!("{}s ago", state.chain_state.tip_age_seconds()),
        finalized_block: finalized,
        version: format!("v{}", state.version),
    };

    Json(status)
}

/// GET /admin/system/info
///
/// Returns static system information (build info, capabilities).
#[utoipa::path(
    get,
    path = "/admin/system/info",
    tag = "System",
    responses(
        (status = 200, description = "System information", body = SystemInfo)
    )
)]
pub async fn get_info(State(state): State<AdminState>) -> impl IntoResponse {
    let info = SystemInfo {
        instance_name: state.config.admin.instance_name.clone(),
        version: format!("v{}", state.version),
        build_date: state.build_date.to_string(),
        git_commit: state.git_commit.to_string(),
        features: vec![
            "cache".to_string(),
            "hedging".to_string(),
            "scoring".to_string(),
            "circuit-breaker".to_string(),
        ],
        capabilities: Capabilities {
            supports_web_socket: true,
            supports_batch: true,
            max_batch_size: 100,
        },
        environment: state.config.environment.clone(),
    };

    Json(info)
}

/// GET /admin/system/health
///
/// Simple health check endpoint for liveness/readiness probes.
#[utoipa::path(
    get,
    path = "/admin/system/health",
    tag = "System",
    responses(
        (status = 200, description = "System is healthy", body = HealthCheck),
        (status = 503, description = "System is unhealthy", body = HealthCheck)
    )
)]
pub async fn get_health(State(state): State<AdminState>) -> impl IntoResponse {
    let upstream_stats = state.proxy_engine.get_upstream_stats().await;

    let status = if upstream_stats.healthy_upstreams > 0 {
        "healthy"
    } else {
        "unhealthy"
    };

    let response =
        HealthCheck { status: status.to_string(), timestamp: chrono::Utc::now().to_rfc3339() };

    let status_code = if upstream_stats.healthy_upstreams > 0 {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status_code, Json(response))
}

/// GET /admin/system/settings
///
/// Returns current server configuration.
#[utoipa::path(
    get,
    path = "/admin/system/settings",
    tag = "System",
    responses(
        (status = 200, description = "Server settings", body = ServerSettings)
    )
)]
pub async fn get_settings(State(state): State<AdminState>) -> impl IntoResponse {
    let settings = ServerSettings {
        bind_address: state.config.server.bind_address.clone(),
        port: state.config.server.bind_port,
        max_concurrent_requests: state.config.server.max_concurrent_requests,
        request_timeout: state.config.server.request_timeout_seconds * 1000,
        keep_alive_timeout: 5,
    };

    Json(settings)
}

/// PUT /admin/system/settings
///
/// Updates server configuration.
///
/// Note: Most server settings cannot be changed at runtime and require a restart.
/// This endpoint validates the request and returns information about what would
/// be changed on restart.
#[utoipa::path(
    put,
    path = "/admin/system/settings",
    tag = "System",
    request_body = UpdateServerSettingsRequest,
    responses(
        (status = 200, description = "Settings update processed", body = UpdateSettingsResponse),
        (status = 400, description = "Invalid settings")
    )
)]
pub async fn update_settings(
    State(_state): State<AdminState>,
    Json(request): Json<UpdateServerSettingsRequest>,
) -> Result<Json<UpdateSettingsResponse>, (StatusCode, String)> {
    // Validate the request
    if let Some(max_concurrent) = request.max_concurrent_requests {
        if max_concurrent == 0 {
            return Err((
                StatusCode::BAD_REQUEST,
                "max_concurrent_requests must be greater than 0".to_string(),
            ));
        }
    }

    if let Some(timeout) = request.request_timeout {
        if timeout == 0 {
            return Err((
                StatusCode::BAD_REQUEST,
                "request_timeout must be greater than 0".to_string(),
            ));
        }
    }

    // Server settings cannot be changed at runtime - the configuration is immutable
    // once the server starts. Changes would require modifying the config file and
    // restarting the server.
    let has_changes =
        request.max_concurrent_requests.is_some() || request.request_timeout.is_some();

    if has_changes {
        tracing::info!(
            max_concurrent_requests = ?request.max_concurrent_requests,
            request_timeout = ?request.request_timeout,
            "server settings update requested (requires restart)"
        );

        Ok(Json(UpdateSettingsResponse {
            success: true,
            message: "Server settings cannot be changed at runtime. Please update the configuration file and restart the server.".to_string(),
            requires_restart: true,
        }))
    } else {
        Ok(Json(UpdateSettingsResponse {
            success: true,
            message: "No changes requested".to_string(),
            requires_restart: false,
        }))
    }
}
