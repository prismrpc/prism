//! Admin API authentication middleware.
//!
//! Provides token-based authentication for the admin API using constant-time comparison
//! to prevent timing attacks.

#![allow(clippy::missing_errors_doc)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use subtle::ConstantTimeEq;

/// Admin authentication middleware using constant-time comparison.
///
/// Validates requests using the `X-Admin-Token` header against the configured token.
/// Uses constant-time comparison to prevent timing attacks.
///
/// # Security
///
/// - If no token is configured (`None`), all requests are allowed (development mode).
/// - Uses `subtle::ConstantTimeEq` for timing-attack resistant comparison.
/// - Returns `401 Unauthorized` for missing or invalid tokens.
///
/// # Example
///
/// ```rust,ignore
/// use axum::{Router, middleware};
/// use std::sync::Arc;
///
/// let admin_token = Some(Arc::new("secret-token".to_string()));
/// let router = Router::new()
///     .route("/admin/status", get(handler))
///     .layer(middleware::from_fn_with_state(admin_token, admin_auth_middleware));
/// ```
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
    if provided_token.as_bytes().ct_eq(expected_token.as_bytes()).into() {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    async fn test_handler() -> &'static str {
        "success"
    }

    #[tokio::test]
    async fn test_no_auth_required() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(None, admin_auth_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_valid_token() {
        let token = Arc::new("test-token".to_string());
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(Some(token), admin_auth_middleware));

        let request = Request::builder()
            .uri("/test")
            .header("X-Admin-Token", "test-token")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_invalid_token() {
        let token = Arc::new("test-token".to_string());
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(Some(token), admin_auth_middleware));

        let request = Request::builder()
            .uri("/test")
            .header("X-Admin-Token", "wrong-token")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_missing_token() {
        let token = Arc::new("test-token".to_string());
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(Some(token), admin_auth_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
