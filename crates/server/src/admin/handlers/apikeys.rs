//! API key management handlers for the admin API.

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::cast_sign_loss)]

use crate::admin::{
    types::{
        ApiKeyCreatedResponse, ApiKeyResponse, ApiKeyUsageResponse, CreateApiKeyRequest,
        DailyUsage, MethodUsage, TimeRangeQuery, UpdateApiKeyRequest,
    },
    AdminState,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use prism_core::auth::{
    api_key::{ApiKey, MethodPermission},
    repository::ApiKeyRepository,
};
use std::collections::HashMap;

/// Error type for API key handler operations.
#[derive(Debug)]
pub enum ApiKeyError {
    AuthDisabled,
    NotFound,
    DatabaseError(String),
    KeyGenerationError(String),
}

impl IntoResponse for ApiKeyError {
    fn into_response(self) -> Response {
        match self {
            ApiKeyError::AuthDisabled => {
                (StatusCode::NOT_FOUND, "API key management is not available (auth disabled)")
                    .into_response()
            }
            ApiKeyError::NotFound => (StatusCode::NOT_FOUND, "API key not found").into_response(),
            ApiKeyError::DatabaseError(msg) | ApiKeyError::KeyGenerationError(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

/// Converts an `ApiKey` and its methods into an `ApiKeyResponse`.
fn to_api_key_response(key: &ApiKey, methods: &[MethodPermission]) -> ApiKeyResponse {
    // Use the blind index for the prefix since we don't have the plaintext key
    // Check bounds before slicing to prevent panics
    let key_prefix = if key.blind_index.len() >= 8 {
        format!("rpc_{}...", &key.blind_index[..8])
    } else {
        format!("rpc_{}...", &key.blind_index)
    };

    ApiKeyResponse {
        id: key.id,
        name: key.name.clone(),
        key_prefix,
        created_at: key.created_at.to_rfc3339(),
        last_used_at: key.last_used_at.map(|dt| dt.to_rfc3339()),
        revoked: !key.is_active,
        allowed_methods: methods.iter().map(|m| m.method_name.clone()).collect(),
    }
}

/// GET /admin/apikeys - List all API keys.
#[utoipa::path(
    get,
    path = "/admin/apikeys",
    tag = "API Keys",
    responses(
        (status = 200, description = "List of API keys", body = Vec<ApiKeyResponse>),
        (status = 404, description = "API key management not available (auth disabled)"),
        (status = 500, description = "Database error")
    )
)]
pub async fn list_api_keys(
    State(state): State<AdminState>,
) -> Result<Json<Vec<ApiKeyResponse>>, ApiKeyError> {
    let repo = state.api_key_repo.as_ref().ok_or(ApiKeyError::AuthDisabled)?;

    let keys = repo.list_all().await.map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    let mut responses = Vec::new();
    for key in &keys {
        let methods = repo
            .get_methods(key.id)
            .await
            .map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;
        responses.push(to_api_key_response(key, &methods));
    }

    Ok(Json(responses))
}

/// GET /admin/apikeys/:id - Get API key details by ID.
#[utoipa::path(
    get,
    path = "/admin/apikeys/{id}",
    tag = "API Keys",
    params(
        ("id" = i64, Path, description = "API key ID")
    ),
    responses(
        (status = 200, description = "API key details", body = ApiKeyResponse),
        (status = 404, description = "API key not found or auth disabled"),
        (status = 500, description = "Database error")
    )
)]
pub async fn get_api_key(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
) -> Result<Json<ApiKeyResponse>, ApiKeyError> {
    let repo = state.api_key_repo.as_ref().ok_or(ApiKeyError::AuthDisabled)?;

    // Find the key by ID
    let keys = repo.list_all().await.map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    let key = keys.iter().find(|k| k.id == id).ok_or(ApiKeyError::NotFound)?;

    let methods = repo
        .get_methods(key.id)
        .await
        .map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    Ok(Json(to_api_key_response(key, &methods)))
}

/// POST /admin/apikeys - Create a new API key.
#[utoipa::path(
    post,
    path = "/admin/apikeys",
    tag = "API Keys",
    request_body = CreateApiKeyRequest,
    responses(
        (status = 200, description = "API key created (includes plaintext key - save it!)", body = ApiKeyCreatedResponse),
        (status = 404, description = "API key management not available (auth disabled)"),
        (status = 500, description = "Database error or key generation failed")
    )
)]
pub async fn create_api_key(
    State(state): State<AdminState>,
    Json(request): Json<CreateApiKeyRequest>,
) -> Result<Json<ApiKeyCreatedResponse>, ApiKeyError> {
    let repo = state.api_key_repo.as_ref().ok_or(ApiKeyError::AuthDisabled)?;

    // Generate a new API key
    let plaintext_key =
        ApiKey::generate().map_err(|e| ApiKeyError::KeyGenerationError(e.to_string()))?;

    // Hash the key for storage
    let key_hash = ApiKey::hash_key(&plaintext_key)
        .map_err(|e| ApiKeyError::KeyGenerationError(e.to_string()))?;

    // Compute the blind index
    let blind_index = ApiKey::compute_blind_index(&plaintext_key);

    let now = Utc::now();

    // Create the API key struct
    let api_key = ApiKey {
        id: 0, // Will be assigned by database
        key_hash,
        blind_index,
        name: request.name.clone(),
        description: None,
        rate_limit_max_tokens: 100,
        rate_limit_refill_rate: 10,
        daily_request_limit: Some(10000),
        daily_requests_used: 0,
        quota_reset_at: now + chrono::Duration::days(1),
        created_at: now,
        updated_at: now,
        last_used_at: None,
        is_active: true,
        expires_at: None,
    };

    // Get the methods to allow (default to empty if not specified)
    let methods = request.allowed_methods.unwrap_or_default();

    // Store in database
    repo.create(api_key.clone(), methods.clone())
        .await
        .map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    // Fetch the created key to get its assigned ID
    let created_key = repo
        .find_and_verify_key(&plaintext_key)
        .await
        .map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?
        .ok_or_else(|| ApiKeyError::DatabaseError("Failed to retrieve created key".to_string()))?;

    Ok(Json(ApiKeyCreatedResponse {
        id: created_key.id,
        name: created_key.name,
        key: plaintext_key,
        created_at: created_key.created_at.to_rfc3339(),
    }))
}

/// PUT /admin/apikeys/:id - Update an API key.
#[utoipa::path(
    put,
    path = "/admin/apikeys/{id}",
    tag = "API Keys",
    params(
        ("id" = i64, Path, description = "API key ID")
    ),
    request_body = UpdateApiKeyRequest,
    responses(
        (status = 200, description = "API key updated", body = ApiKeyResponse),
        (status = 404, description = "API key not found or auth disabled"),
        (status = 500, description = "Update not yet implemented or database error")
    )
)]
pub async fn update_api_key(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
    Json(request): Json<UpdateApiKeyRequest>,
) -> Result<Json<ApiKeyResponse>, ApiKeyError> {
    let repo = state.api_key_repo.as_ref().ok_or(ApiKeyError::AuthDisabled)?;

    // Find the key by ID
    let keys = repo.list_all().await.map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    let key = keys.iter().find(|k| k.id == id).ok_or(ApiKeyError::NotFound)?;

    // Note: The current repository doesn't have update methods for name or methods.
    // For now, we'll return an error indicating this is not yet implemented.
    // In a full implementation, we would need to add update_name and update_methods
    // methods to the ApiKeyRepository trait.

    // For the allowed_methods update, we would need to:
    // 1. Delete all existing method permissions
    // 2. Insert new method permissions
    // This requires transaction support and new repository methods.

    // Since the repository doesn't support these operations yet,
    // we'll return the current state
    let methods = repo
        .get_methods(key.id)
        .await
        .map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    // TODO: Implement actual updates when repository supports it
    if request.name.is_some() || request.allowed_methods.is_some() {
        return Err(ApiKeyError::DatabaseError(
            "Update operations not yet implemented in repository".to_string(),
        ));
    }

    Ok(Json(to_api_key_response(key, &methods)))
}

/// DELETE /admin/apikeys/:id - Revoke an API key.
#[utoipa::path(
    delete,
    path = "/admin/apikeys/{id}",
    tag = "API Keys",
    params(
        ("id" = i64, Path, description = "API key ID")
    ),
    responses(
        (status = 204, description = "API key revoked"),
        (status = 404, description = "API key not found or auth disabled"),
        (status = 500, description = "Database error")
    )
)]
pub async fn delete_api_key(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiKeyError> {
    let repo = state.api_key_repo.as_ref().ok_or(ApiKeyError::AuthDisabled)?;

    // Find the key by ID to get its name
    let keys = repo.list_all().await.map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    let key = keys.iter().find(|k| k.id == id).ok_or(ApiKeyError::NotFound)?;

    // Revoke the key by name
    repo.revoke(&key.name)
        .await
        .map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /admin/apikeys/:id/revoke - Alternative endpoint to revoke an API key.
#[utoipa::path(
    post,
    path = "/admin/apikeys/{id}/revoke",
    tag = "API Keys",
    params(
        ("id" = i64, Path, description = "API key ID")
    ),
    responses(
        (status = 204, description = "API key revoked"),
        (status = 404, description = "API key not found or auth disabled"),
        (status = 500, description = "Database error")
    )
)]
pub async fn revoke_api_key(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiKeyError> {
    // Delegate to delete_api_key
    delete_api_key(State(state), Path(id)).await
}

/// GET /admin/apikeys/:id/usage - Get usage statistics for an API key.
#[utoipa::path(
    get,
    path = "/admin/apikeys/{id}/usage",
    tag = "API Keys",
    params(
        ("id" = i64, Path, description = "API key ID"),
        ("timeRange" = Option<String>, Query, description = "Time range: 24h, 7d, 30d, 90d (default: 7d)")
    ),
    responses(
        (status = 200, description = "API key usage statistics", body = ApiKeyUsageResponse),
        (status = 404, description = "API key not found or auth disabled"),
        (status = 500, description = "Database error")
    )
)]
pub async fn get_api_key_usage(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<ApiKeyUsageResponse>, ApiKeyError> {
    let repo = state.api_key_repo.as_ref().ok_or(ApiKeyError::AuthDisabled)?;

    // Find the key by ID to ensure it exists
    let keys = repo.list_all().await.map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    let key = keys.iter().find(|k| k.id == id).ok_or(ApiKeyError::NotFound)?;

    // Parse time range (default to 7 days)
    let days = match query.time_range.as_str() {
        "24h" => 1,
        "30d" => 30,
        "90d" => 90,
        _ => 7, // default (covers "7d" and unknown values)
    };

    // Get usage stats from the database
    let usage_stats = repo
        .get_usage_stats(key.id, days)
        .await
        .map_err(|e| ApiKeyError::DatabaseError(e.to_string()))?;

    // Calculate total requests across all time in the usage stats
    let total_requests: u64 = usage_stats.iter().map(|stat| stat.request_count as u64).sum();

    // Get current day's requests from the ApiKey struct
    let requests_today = key.daily_requests_used as u64;

    // Build usage by method
    let mut method_totals: HashMap<String, u64> = HashMap::new();
    for stat in &usage_stats {
        *method_totals.entry(stat.method_name.clone()).or_insert(0) += stat.request_count as u64;
    }

    let total_for_percentage = if total_requests > 0 {
        total_requests
    } else {
        1
    };
    let mut usage_by_method: Vec<MethodUsage> = method_totals
        .into_iter()
        .map(|(method, count)| {
            #[allow(clippy::cast_precision_loss)]
            let percentage = (count as f64 / total_for_percentage as f64) * 100.0;
            MethodUsage { method, count, percentage }
        })
        .collect();
    usage_by_method.sort_by(|a, b| b.count.cmp(&a.count));

    // Build daily usage aggregates
    let mut daily_totals: HashMap<String, u64> = HashMap::new();
    for stat in &usage_stats {
        *daily_totals.entry(stat.date.clone()).or_insert(0) += stat.request_count as u64;
    }

    let mut daily_usage: Vec<DailyUsage> = daily_totals
        .into_iter()
        .map(|(date, requests)| DailyUsage { date, requests })
        .collect();
    daily_usage.sort_by(|a, b| b.date.cmp(&a.date));

    Ok(Json(ApiKeyUsageResponse {
        key_id: key.id,
        total_requests,
        requests_today,
        last_used: key.last_used_at.map(|dt| dt.to_rfc3339()),
        usage_by_method,
        daily_usage,
    }))
}
