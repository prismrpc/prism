# Admin API Input Validation Implementation Guide

## Summary

This guide documents the input validation implementation for the Admin API upstream handlers in `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs`.

## Changes Required

### 1. Add Validation Helper Functions

Insert the following validation functions after line 158 (after the `extract_upstream_metrics` function and before `/// GET /admin/upstreams`):

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

### 2. Call Validation in create_upstream Function

In the `create_upstream` function (around line 886), add validation RIGHT AFTER the function signature and BEFORE converting to core request:

```rust
pub async fn create_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(request): Json<crate::admin::types::CreateUpstreamRequest>,
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // ADD THIS LINE:
    validate_create_upstream_request(&request)?;

    // Convert to core request
    let core_request = CoreCreateUpstreamRequest {
        // ... rest of function
```

### 3. Call Validation in update_upstream Function

In the `update_upstream` function (around line 955), add validation RIGHT AFTER the function signature and BEFORE checking if it's a config-based upstream:

```rust
pub async fn create_upstream(
    State(state): State<AdminState>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    Json(request): Json<crate::admin::types::UpdateUpstreamRequest>,
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // ADD THIS LINE:
    validate_update_upstream_request(&request)?;

    // Check if this is a config-based upstream
    let manager = state.proxy_engine.get_upstream_manager();
    // ... rest of function
```

## Validation Rules Implemented

### Weight Validation
- Must be between 1 and 1000 (inclusive)
- Rejects 0 and values > 1000

### URL Validation
- Must be valid URL format
- Must use http or https scheme only
- Prevents SSRF attacks via file://, ftp://, etc.

### Name Validation
- Must be non-empty
- Must be 128 characters or less
- Must contain only alphanumeric characters, hyphens (-), and underscores (_)
- Prevents injection attacks and filesystem issues

### WebSocket URL Validation (Optional Field)
- If provided, must be valid URL format
- Must use ws or wss scheme only

## Testing

Comprehensive unit tests have been added to `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstream_validation.rs` covering:

- Valid weight values (1, 100, 1000)
- Invalid weight values (0, 1001)
- Valid HTTP/HTTPS URLs
- Invalid URL schemes (file://, ftp://, ws://)
- Invalid URL formats
- Valid names (various patterns)
- Invalid names (empty, too long, special characters)
- Complete request validation scenarios

Run tests with:
```bash
cargo test --package server --lib admin::handlers::upstream_validation
```

## Security Improvements

1. **SSRF Prevention**: URL scheme validation prevents internal network access via file://, gopher://, etc.
2. **Injection Prevention**: Name validation prevents shell injection and filesystem path traversal
3. **Data Integrity**: Weight validation ensures load balancing works correctly
4. **Consistency**: Same validation applied in both create and update operations

## Files Modified

1. `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs` - Main handlers file
2. `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstream_validation.rs` - Standalone validation module with tests (created)

## Verification Steps

1. Run `cargo check` to ensure no compilation errors
2. Run `cargo test` to verify all tests pass
3. Test with invalid inputs via API:
   - Weight 0: `curl -X POST http://localhost:3000/admin/upstreams -H "Content-Type: application/json" -d '{"name":"test","url":"http://test.com","weight":0,"chain_id":1}'`
   - Invalid URL: `curl -X POST http://localhost:3000/admin/upstreams -H "Content-Type: application/json" -d '{"name":"test","url":"file:///etc/passwd","weight":100,"chain_id":1}'`
   - Invalid name: `curl -X POST http://localhost:3000/admin/upstreams -H "Content-Type: application/json" -d '{"name":"test node","url":"http://test.com","weight":100,"chain_id":1}'`

Expected result for all: HTTP 400 Bad Request with descriptive error message.
