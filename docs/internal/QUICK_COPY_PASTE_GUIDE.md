# Quick Copy-Paste Guide for Input Validation

## Step 1: Add Validation Functions

**Location**: In `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs`

Find this line (around line 160):
```rust
/// GET /admin/upstreams
```

**Paste this code RIGHT BEFORE that line:**

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

---

## Step 2: Call Validation in create_upstream

**Location**: In the `create_upstream` function (around line 890-895)

Find this code:
```rust
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    // Convert to core request
```

**Change it to:**
```rust
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Validate request
    validate_create_upstream_request(&request)?;

    // Convert to core request
```

---

## Step 3: Call Validation in update_upstream

**Location**: In the `update_upstream` function (around line 960-965)

Find this code:
```rust
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);
    // Check if this is a config-based upstream
```

**Change it to:**
```rust
) -> Result<Json<UpstreamResponse>, (StatusCode, String)> {
    let correlation_id = crate::admin::get_correlation_id(&headers);

    // Validate request
    validate_update_upstream_request(&request)?;

    // Check if this is a config-based upstream
```

---

## Step 4: Verify

Run these commands:
```bash
cargo check
cargo test --package server
```

Both should succeed without errors.

---

## Quick Test

```bash
# Test invalid weight (should return HTTP 400)
curl -X POST http://localhost:3000/admin/upstreams \
  -H "Content-Type: application/json" \
  -d '{"name":"test","url":"http://test.com","weight":0,"chain_id":1}'

# Test invalid URL scheme (should return HTTP 400)
curl -X POST http://localhost:3000/admin/upstreams \
  -H "Content-Type: application/json" \
  -d '{"name":"test","url":"file:///etc/passwd","weight":100,"chain_id":1}'
```

Done! ✅
