# Admin API Authentication Implementation

## Summary

Added token-based authentication to the admin API using a simple admin token from configuration (Option B). The implementation uses constant-time comparison to prevent timing attacks and supports both authenticated and development modes.

## Changes Made

### 1. Configuration (`/home/flo/workspace/personal/prism/crates/prism-core/src/config/mod.rs`)

Added `admin_token` field to `AdminConfig`:

```rust
pub struct AdminConfig {
    // ... existing fields ...

    /// Optional admin authentication token.
    /// If `None`, the admin API does not require authentication (development mode).
    /// If `Some(token)`, all admin API requests must include the `X-Admin-Token` header
    /// with a matching token value.
    #[serde(default)]
    pub admin_token: Option<String>,
}
```

### 2. Middleware (`/home/flo/workspace/personal/prism/crates/server/src/admin/middleware.rs`)

Created new middleware module with authentication logic:

```rust
pub async fn admin_auth_middleware(
    State(admin_token): State<Option<Arc<String>>>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // If no token configured, allow all requests (dev mode)
    let Some(expected_token) = admin_token else {
        return Ok(next.run(request).await);
    };

    // Extract the token from the request header
    let provided_token = request
        .headers()
        .get("X-Admin-Token")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // Constant-time comparison to prevent timing attacks
    use subtle::ConstantTimeEq;
    if provided_token.as_bytes().ct_eq(expected_token.as_bytes()).into() {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
```

**Security Features:**
- Constant-time comparison using `subtle::ConstantTimeEq` to prevent timing attacks
- Returns `401 Unauthorized` for missing or invalid tokens
- Supports development mode when no token is configured

### 3. Router Integration (`/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs`)

Integrated middleware into the admin router:

```rust
pub fn create_admin_router(state: AdminState) -> Router {
    // Extract admin token for authentication middleware
    let admin_token = state.config.admin.admin_token.clone().map(Arc::new);

    let router = Router::new()
        // ... all admin routes ...

    // Apply authentication middleware to all admin endpoints
    router
        .layer(axum_middleware::from_fn_with_state(
            admin_token,
            middleware::admin_auth_middleware,
        ))
        .merge(SwaggerUi::new("/admin/swagger-ui")
            .url("/admin/api-docs/openapi.json", ApiDoc::openapi()))
        .with_state(state)
}
```

### 4. Dependencies (`/home/flo/workspace/personal/prism/crates/server/Cargo.toml`)

Added `subtle` crate for constant-time comparison:

```toml
subtle = "2.5"
```

### 5. Configuration File (`/home/flo/workspace/personal/prism/config/development.toml`)

Updated development configuration with admin token:

```toml
[admin]
enabled = true
bind_address = "127.0.0.1"
port = 3031
prometheus_url = "http://localhost:9090"
admin_token = "dev-admin-token-change-in-production"  # Set to enable admin authentication
```

## Usage

### Development Mode (No Authentication)

Set `admin_token = None` or omit the field:

```toml
[admin]
enabled = true
admin_token = ""  # Empty string or omit entirely
```

All admin API requests will be allowed without authentication.

### Production Mode (Authentication Required)

Set a strong token in configuration:

```toml
[admin]
enabled = true
admin_token = "your-secure-random-token-here"
```

All admin API requests must include the `X-Admin-Token` header:

```bash
curl -H "X-Admin-Token: your-secure-random-token-here" \
     http://localhost:3031/admin/system/status
```

### Using Environment Variables

Override the token via environment variable:

```bash
export PRISM__ADMIN__ADMIN_TOKEN="production-token"
```

## Security Considerations

1. **Constant-Time Comparison**: Uses `subtle::ConstantTimeEq` to prevent timing attacks
2. **Token Storage**: Store tokens securely (environment variables, secrets management)
3. **Token Strength**: Use cryptographically random tokens in production
4. **HTTPS**: Use HTTPS in production to prevent token interception
5. **Token Rotation**: Implement token rotation for long-running deployments

## Testing

Comprehensive test suite with 4 test cases:

```bash
cargo test -p server admin::middleware::tests
```

Tests cover:
- No authentication required (development mode)
- Valid token authentication
- Invalid token rejection
- Missing token rejection

All tests pass successfully.

## Files Modified

1. `/home/flo/workspace/personal/prism/crates/prism-core/src/config/mod.rs`
2. `/home/flo/workspace/personal/prism/crates/server/src/admin/middleware.rs` (new)
3. `/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs`
4. `/home/flo/workspace/personal/prism/crates/server/Cargo.toml`
5. `/home/flo/workspace/personal/prism/config/development.toml`

## Verification

```bash
# Compile check
cargo check -p server

# Run tests
cargo test -p server admin::middleware::tests

# Full build
cargo build -p server
```

All checks pass successfully.
