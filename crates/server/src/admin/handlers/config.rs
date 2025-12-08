//! Config export handler.

#![allow(clippy::missing_errors_doc)]

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;

use crate::admin::AdminState;
use prism_core::{
    config::{
        AdminConfig, AuthConfig, CacheConfig, HealthCheckConfig, LoggingConfig, MetricsConfig,
        ServerConfig, UpstreamProvider, UpstreamsConfig,
    },
    upstream::{ConsensusConfig, HedgeConfig, ScoringConfig},
};

/// Masks sensitive parts of URLs (API keys, etc.).
///
/// This is a copy of the function from upstreams.rs since it's not public.
fn mask_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        // Parse failed - return masked placeholder for safety
        return "[invalid-url]".to_string();
    };

    // Mask password if present
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some("***"));
    }

    // Mask sensitive query parameters
    let sensitive_keys = ["key", "apikey", "api_key", "token", "secret", "password", "auth"];
    let query_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| {
            let key_lower = key.to_lowercase();
            if sensitive_keys.iter().any(|s| key_lower.contains(s)) {
                (key.to_string(), "***".to_string())
            } else {
                (key.to_string(), value.to_string())
            }
        })
        .collect();

    if !query_pairs.is_empty() {
        let new_query: String = query_pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        parsed.set_query(Some(&new_query));
    }

    // Mask long path segments (likely API keys)
    let path = parsed.path().to_string();
    let parts: Vec<&str> = path.split('/').collect();
    if let Some(last) = parts.last() {
        if last.len() > 20 {
            if last.len() >= 8 {
                let masked = format!("{}...{}", &last[..4], &last[last.len() - 4..]);
                let new_path = format!("{}/{masked}", parts[..parts.len() - 1].join("/"));
                parsed.set_path(&new_path);
            }
        }
    }

    parsed.to_string()
}

/// Query parameters for config export.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigExportQuery {
    /// Export format: "toml" or "json". Defaults to "toml".
    #[serde(default = "default_format")]
    pub format: String,

    /// Include unmasked secrets (API keys in URLs). Defaults to false.
    #[serde(default)]
    pub include_secrets: bool,

    /// Export only dynamic/runtime upstreams. Defaults to false.
    #[serde(default)]
    pub dynamic_only: bool,
}

fn default_format() -> String {
    "toml".to_string()
}

/// Full configuration export including static config and dynamic upstreams.
///
/// This structure mirrors `AppConfig` but is designed for serialization
/// and can merge dynamically-added upstreams with static config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigExport {
    /// Deployment environment.
    pub environment: String,

    /// HTTP server configuration.
    pub server: ServerConfig,

    /// Upstream RPC provider configuration (merged static + runtime).
    pub upstreams: UpstreamsConfig,

    /// Cache system configuration.
    pub cache: CacheConfig,

    /// Health check configuration.
    pub health_check: HealthCheckConfig,

    /// API key authentication configuration.
    pub auth: AuthConfig,

    /// Prometheus metrics configuration.
    pub metrics: MetricsConfig,

    /// Logging configuration.
    pub logging: LoggingConfig,

    /// Admin API configuration.
    pub admin: AdminConfig,

    /// Request hedging configuration.
    pub hedging: HedgeConfig,

    /// Multi-factor scoring configuration.
    pub scoring: ScoringConfig,

    /// Consensus and data validation configuration.
    pub consensus: ConsensusConfig,
}

impl ConfigExport {
    /// Creates a config export from the application config and dynamic upstreams.
    ///
    /// # Arguments
    ///
    /// * `app_config` - Reference to the application configuration
    /// * `dynamic_upstreams` - List of dynamically-added upstreams
    /// * `include_secrets` - Whether to include unmasked URLs
    /// * `dynamic_only` - Whether to export only dynamic upstreams
    fn from_app_config(
        app_config: &prism_core::config::AppConfig,
        dynamic_upstreams: Vec<prism_core::upstream::DynamicUpstreamConfig>,
        include_secrets: bool,
        dynamic_only: bool,
    ) -> Self {
        // Build upstream providers list
        let mut providers = if dynamic_only {
            // Only include dynamic upstreams
            Vec::new()
        } else {
            // Start with config-based upstreams
            app_config.upstreams.providers.clone()
        };

        // Add dynamic upstreams, converting from DynamicUpstreamConfig to UpstreamProvider
        for dynamic_upstream in dynamic_upstreams {
            if !dynamic_upstream.enabled {
                continue; // Skip disabled dynamic upstreams
            }

            providers.push(UpstreamProvider {
                name: dynamic_upstream.name,
                chain_id: dynamic_upstream.chain_id,
                https_url: dynamic_upstream.url,
                wss_url: dynamic_upstream.ws_url,
                weight: dynamic_upstream.weight,
                timeout_seconds: dynamic_upstream.timeout_seconds,
                circuit_breaker_threshold: 2, // Default from config
                circuit_breaker_timeout_seconds: 1, // Default from config
            });
        }

        // Mask URLs if secrets should not be included
        if !include_secrets {
            for provider in &mut providers {
                provider.https_url = mask_url(&provider.https_url);
                if let Some(ref ws_url) = provider.wss_url {
                    provider.wss_url = Some(mask_url(ws_url));
                }
            }
        }

        Self {
            environment: app_config.environment.clone(),
            server: app_config.server.clone(),
            upstreams: UpstreamsConfig { providers },
            cache: app_config.cache.clone(),
            health_check: app_config.health_check.clone(),
            auth: app_config.auth.clone(),
            metrics: app_config.metrics.clone(),
            logging: app_config.logging.clone(),
            admin: app_config.admin.clone(),
            hedging: app_config.hedging.clone(),
            scoring: app_config.scoring.clone(),
            consensus: app_config.consensus.clone(),
        }
    }
}

/// GET /admin/config/export
///
/// Exports the full configuration including both static config and dynamic upstreams.
/// Supports TOML and JSON formats with optional secret masking.
#[utoipa::path(
    get,
    path = "/admin/config/export",
    tag = "Config",
    params(
        ("format" = Option<String>, Query, description = "Export format: 'toml' or 'json' (default: toml)"),
        ("include_secrets" = Option<bool>, Query, description = "Include unmasked secrets (default: false)"),
        ("dynamic_only" = Option<bool>, Query, description = "Export only dynamic/runtime upstreams (default: false)")
    ),
    responses(
        (status = 200, description = "Configuration exported successfully"),
        (status = 400, description = "Invalid export format"),
        (status = 500, description = "Configuration serialization failed")
    )
)]
pub async fn export_config(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<ConfigExportQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Validate format
    if query.format != "toml" && query.format != "json" {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid format '{}'. Supported formats: 'toml', 'json'", query.format),
        ));
    }

    // Security audit: Log if secrets are being exported
    if query.include_secrets {
        tracing::warn!(
            correlation_id = ?correlation_id,
            format = %query.format,
            dynamic_only = query.dynamic_only,
            "config export with unmasked secrets requested"
        );
    } else {
        tracing::info!(
            correlation_id = ?correlation_id,
            format = %query.format,
            dynamic_only = query.dynamic_only,
            "config export requested (secrets masked)"
        );
    }

    // Get dynamic upstreams from the registry
    let dynamic_registry = state.proxy_engine.get_upstream_manager().get_dynamic_registry();
    let dynamic_upstreams = dynamic_registry.list_all();

    // Build the merged config
    let config_export = ConfigExport::from_app_config(
        &state.config,
        dynamic_upstreams,
        query.include_secrets,
        query.dynamic_only,
    );

    // Serialize to requested format
    match query.format.as_str() {
        "toml" => {
            let toml_string = toml::to_string_pretty(&config_export).map_err(|e| {
                tracing::error!(
                    correlation_id = ?correlation_id,
                    error = %e,
                    "failed to serialize config to TOML"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to serialize config to TOML: {e}"),
                )
            })?;

            Ok((StatusCode::OK, [("Content-Type", "application/toml")], toml_string))
        }
        "json" => {
            let json_string = serde_json::to_string_pretty(&config_export).map_err(|e| {
                tracing::error!(
                    correlation_id = ?correlation_id,
                    error = %e,
                    "failed to serialize config to JSON"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to serialize config to JSON: {e}"),
                )
            })?;

            Ok((StatusCode::OK, [("Content-Type", "application/json")], json_string))
        }
        _ => unreachable!(), // Already validated above
    }
}

/// Request body for config persist endpoint.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersistConfigRequest {
    /// Path to persist the dynamic upstreams JSON file.
    /// Defaults to "data/dynamic_upstreams.json".
    #[serde(default = "default_persist_path")]
    pub path: String,
}

fn default_persist_path() -> String {
    "data/dynamic_upstreams.json".to_string()
}

/// Response for config persist operation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersistConfigResponse {
    pub success: bool,
    pub upstreams_persisted: usize,
    pub path: String,
    pub backup_created: bool,
}

/// POST /admin/config/persist
///
/// Persists dynamic upstreams to a JSON file for persistence across restarts.
/// Creates a backup of the existing file if it exists.
#[utoipa::path(
    post,
    path = "/admin/config/persist",
    tag = "Config",
    request_body(content = Option<PersistConfigRequest>, description = "Optional persist configuration"),
    responses(
        (status = 200, description = "Dynamic upstreams persisted successfully", body = PersistConfigResponse),
        (status = 500, description = "Failed to persist configuration")
    )
)]
pub async fn persist_config(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<Option<PersistConfigRequest>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Use default path if not provided
    let persist_path =
        request.as_ref().map(|r| r.path.clone()).unwrap_or_else(default_persist_path);

    tracing::info!(
        correlation_id = ?correlation_id,
        path = %persist_path,
        "config persist requested"
    );

    // Get dynamic upstreams from the registry
    let dynamic_registry = state.proxy_engine.get_upstream_manager().get_dynamic_registry();
    let dynamic_upstreams = dynamic_registry.list_all();

    let upstreams_count = dynamic_upstreams.len();

    // Serialize to JSON
    let json_data = serde_json::to_string_pretty(&dynamic_upstreams).map_err(|e| {
        tracing::error!(
            correlation_id = ?correlation_id,
            error = %e,
            "failed to serialize dynamic upstreams to JSON"
        );
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to serialize dynamic upstreams to JSON: {e}"),
        )
    })?;

    // Ensure the directory exists
    let path = PathBuf::from(&persist_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            tracing::error!(
                correlation_id = ?correlation_id,
                error = %e,
                directory = ?parent,
                "failed to create data directory"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create data directory: {e}"))
        })?;
    }

    // Create backup if file exists
    let backup_created = if path.exists() {
        let backup_path = path.with_extension("json.bak");
        std::fs::rename(&path, &backup_path).map_err(|e| {
            tracing::error!(
                correlation_id = ?correlation_id,
                error = %e,
                from = ?path,
                to = ?backup_path,
                "failed to create backup file"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create backup file: {e}"))
        })?;
        tracing::info!(
            correlation_id = ?correlation_id,
            backup_path = ?backup_path,
            "created backup of existing file"
        );
        true
    } else {
        false
    };

    // Write to temp file then rename for atomic operation
    let temp_path = path.with_extension("json.tmp");
    std::fs::write(&temp_path, &json_data).map_err(|e| {
        tracing::error!(
            correlation_id = ?correlation_id,
            error = %e,
            temp_path = ?temp_path,
            "failed to write temporary file"
        );
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write temporary file: {e}"))
    })?;

    // Atomic rename
    std::fs::rename(&temp_path, &path).map_err(|e| {
        tracing::error!(
            correlation_id = ?correlation_id,
            error = %e,
            from = ?temp_path,
            to = ?path,
            "failed to rename temporary file to final path"
        );
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to rename temporary file: {e}"))
    })?;

    tracing::info!(
        correlation_id = ?correlation_id,
        path = %persist_path,
        upstreams_count = upstreams_count,
        backup_created = backup_created,
        "dynamic upstreams persisted successfully"
    );

    // Audit log the operation
    if let Some(_api_key_repo) = &state.api_key_repo {
        // Note: In a real implementation, we'd extract the admin user/key from auth middleware
        // For now, we log it as an admin operation
        tracing::info!(
            correlation_id = ?correlation_id,
            action = "config.persist",
            upstreams_count = upstreams_count,
            path = %persist_path,
            "audit: config persist operation"
        );
    }

    Ok((
        StatusCode::OK,
        Json(PersistConfigResponse {
            success: true,
            upstreams_persisted: upstreams_count,
            path: persist_path,
            backup_created,
        }),
    ))
}
