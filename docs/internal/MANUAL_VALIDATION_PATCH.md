# Manual Patch: Admin API Input Validation

## Files to Modify

File: `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs`

## Change 1: Add Validation Helper Functions

**Location**: After line 158 (after the `extract_upstream_metrics` function closes with `}`), insert these validation functions:

<details>
<summary>Click to expand validation functions code (106 lines)</summary>

```rust
/// Validates upstream weight.
///
/// # Errors
///
/// Returns error if weight is 0 or greater than 1000.
fn validate_weight(weight: u32) -> Result<(), (StatusCode, String)> {
    if weight == 0 || weight > 1000 {
        return Err((StatusCode::BAD_REQUEST, "Weight must be between 1 and 1000".to_string()));
    }
    Ok(())
}

/// Validates upstream URL.
///
/// # Errors
///
/// Returns error if URL is invalid or not using http/https scheme.
fn validate_url(url_str: &str) -> Result<(), (StatusCode, String)> {
    let url = url::Url::parse(url_str)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid URL format".to_string()))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err((StatusCode::BAD_REQUEST, "URL must use http or https scheme".to_string()));
    }

    Ok(())
}

/// Validates upstream name.
///
/// # Errors
///
/// Returns error if name is empty, too long, or contains invalid characters.
fn validate_name(name: &str) -> Result<(), (StatusCode, String)> {
    if name.is_empty() || name.len() > 128 {
        return Err((StatusCode::BAD_REQUEST, "Name must be 1-128 characters".to_string()));
    }

    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err((
            StatusCode::BAD_REQUEST,
            "Name must contain only alphanumeric characters, hyphens, and underscores".to_string(),
        ));
    }

    Ok(())
}

/// Validates create upstream request.
///
/// # Errors
///
/// Returns error if any validation fails.
fn validate_create_upstream_request(
    req: &crate::admin::types::CreateUpstreamRequest,
) -> Result<(), (StatusCode, String)> {
    validate_name(&req.name)?;
    validate_url(&req.url)?;
    validate_weight(req.weight)?;

    // Validate optional WebSocket URL
    if let Some(ws_url) = &req.ws_url {
        let url = url::Url::parse(ws_url)
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid WebSocket URL format".to_string()))?;

        if !matches!(url.scheme(), "ws" | "wss") {
            return Err((
                StatusCode::BAD_REQUEST,
                "WebSocket URL must use ws or wss scheme".to_string(),
            ));
        }
    }

    Ok(())
}

/// Validates update upstream request.
///
/// # Errors
///
/// Returns error if any validation fails.
fn validate_update_upstream_request(
    req: &crate::admin::types::UpdateUpstreamRequest,
) -> Result<(), (StatusCode, String)> {
    if let Some(name) = &req.name {
        validate_name(name)?;
    }

    if let Some(url) = &req.url {
        validate_url(url)?;
    }

    if let Some(weight) = req.weight {
        validate_weight(weight)?;
    }

    Ok(())
}
```

</details>

## Change 2: Add Validation to create_upstream Function

**Location**: In `create_upstream` function (around line 892)

**BEFORE** (current code):
```rust
pub async fn create_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(request): Json<crate::admin::types::CreateUpstreamRequest>,
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    // Convert to core request
    let core_request = CoreCreateUpstreamRequest {
```

**AFTER** (add validation):
```rust
pub async fn create_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(request): Json<crate::admin::types::CreateUpstreamRequest>,
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Validate request
    validate_create_upstream_request(&request)?;

    // Convert to core request
    let core_request = CoreCreateUpstreamRequest {
```

## Change 3: Add Validation to update_upstream Function

**Location**: In `update_upstream` function (around line 962)

**BEFORE** (current code):
```rust
pub async fn update_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    Json(request): Json<crate::admin::types::UpdateUpstreamRequest>,
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    // Check if this is a config-based upstream
    let manager = state.proxy_engine.get_upstream_manager();
```

**AFTER** (add validation):
```rust
pub async fn update_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    Json(request): Json<crate::admin::types::UpdateUpstreamRequest>,
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Validate request
    validate_update_upstream_request(&request)?;

    // Check if this is a config-based upstream
    let manager = state.proxy_engine.get_upstream_manager();
```

## Verification

After applying changes, run:

1. `cargo check` - Should compile without errors
2. `cargo test --package server` - All tests should pass
3. Test manually with invalid inputs (see VALIDATION_IMPLEMENTATION_GUIDE.md)

## Summary of Security Improvements

- **Weight**: Must be 1-1000 (prevents load balancing issues)
- **URL**: Must be http/https only (prevents SSRF via file://, ftp://, etc.)
- **Name**: Alphanumeric + hyphens/underscores only, 1-128 chars (prevents injection)
- **WebSocket URL**: If provided, must be ws/wss only

## Reference Implementation

A complete standalone validation module with unit tests is available at:
`/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstream_validation.rs`
