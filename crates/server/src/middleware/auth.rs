use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use prism_core::middleware::ApiKeyAuth;
use std::sync::Arc;
use url::form_urlencoded;

/// Axum middleware function that validates API keys from request headers or query parameters.
///
/// Extracts the API key from either the `X-API-Key` header or the `api_key` query parameter
/// (header takes precedence), then validates it against the authentication system. On success,
/// inserts the `AuthenticatedKey` into request extensions for downstream handlers.
///
/// # Errors
///
/// Returns `StatusCode::UNAUTHORIZED` if the API key is missing or authentication fails.
pub async fn api_key_middleware(
    State(auth): State<Arc<ApiKeyAuth>>,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let api_key_from_header = request.headers().get("X-API-Key").and_then(|v| v.to_str().ok());

    let api_key_from_query: Option<String> = if api_key_from_header.is_none() {
        request.uri().query().and_then(|q| {
            form_urlencoded::parse(q.as_bytes())
                .find(|(k, _)| k == "api_key")
                .map(|(_, v)| v.to_string())
        })
    } else {
        None
    };

    let api_key = api_key_from_header
        .or(api_key_from_query.as_deref())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let auth_key = auth.authenticate(api_key).await.map_err(|e| {
        tracing::warn!(error = %e, "authentication failed");
        StatusCode::UNAUTHORIZED
    })?;

    request.extensions_mut().insert(auth_key);

    Ok(next.run(request).await)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use prism_core::auth::{
        api_key::{ApiKey, MethodPermission},
        repository::ApiKeyRepository,
        AuthError, AuthenticatedKey,
    };
    use std::sync::Arc;
    use tower::ServiceExt;

    struct TestApiKeyRepository {
        valid_key_hash: String,
    }

    impl TestApiKeyRepository {
        fn new() -> Self {
            let valid_key = "valid_key";
            let valid_key_hash =
                ApiKey::hash_key(valid_key).expect("Test hash generation should succeed");
            Self { valid_key_hash }
        }
    }

    #[async_trait]
    impl ApiKeyRepository for TestApiKeyRepository {
        async fn find_and_verify_key(
            &self,
            plaintext_key: &str,
        ) -> Result<Option<ApiKey>, AuthError> {
            if ApiKey::verify_key(plaintext_key, &self.valid_key_hash) {
                Ok(Some(ApiKey {
                    id: 1,
                    name: "test_key".to_string(),
                    key_hash: self.valid_key_hash.clone(),
                    blind_index: ApiKey::compute_blind_index(plaintext_key),
                    description: Some("Test API key".to_string()),
                    is_active: true,
                    rate_limit_max_tokens: 10,
                    rate_limit_refill_rate: 1,
                    daily_request_limit: Some(1000),
                    daily_requests_used: 0,
                    quota_reset_at: chrono::Utc::now(),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    last_used_at: None,
                    expires_at: None,
                }))
            } else {
                Ok(None)
            }
        }

        async fn find_by_blind_index(
            &self,
            blind_index: &str,
        ) -> Result<Option<ApiKey>, AuthError> {
            let valid_key = "valid_key";
            let computed_blind_index = ApiKey::compute_blind_index(valid_key);
            if blind_index == computed_blind_index {
                Ok(Some(ApiKey {
                    id: 1,
                    name: "test_key".to_string(),
                    key_hash: self.valid_key_hash.clone(),
                    blind_index: computed_blind_index,
                    description: Some("Test API key".to_string()),
                    is_active: true,
                    rate_limit_max_tokens: 10,
                    rate_limit_refill_rate: 1,
                    daily_request_limit: Some(1000),
                    daily_requests_used: 0,
                    quota_reset_at: chrono::Utc::now(),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    last_used_at: None,
                    expires_at: None,
                }))
            } else {
                Ok(None)
            }
        }

        async fn find_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, AuthError> {
            if key_hash == self.valid_key_hash {
                let valid_key = "valid_key";
                Ok(Some(ApiKey {
                    id: 1,
                    name: "test_key".to_string(),
                    key_hash: key_hash.to_string(),
                    blind_index: ApiKey::compute_blind_index(valid_key),
                    description: Some("Test API key".to_string()),
                    is_active: true,
                    rate_limit_max_tokens: 10,
                    rate_limit_refill_rate: 1,
                    daily_request_limit: Some(1000),
                    daily_requests_used: 0,
                    quota_reset_at: chrono::Utc::now(),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    last_used_at: None,
                    expires_at: None,
                }))
            } else {
                Ok(None)
            }
        }

        async fn get_methods(&self, _api_key_id: i64) -> Result<Vec<MethodPermission>, AuthError> {
            Ok(vec![])
        }

        async fn increment_usage(&self, _api_key_id: i64) -> Result<(), AuthError> {
            Ok(())
        }

        async fn increment_method_usage(
            &self,
            _api_key_id: i64,
            _method: &str,
        ) -> Result<(), AuthError> {
            Ok(())
        }

        async fn reset_daily_quotas(&self, _api_key_id: i64) -> Result<(), AuthError> {
            Ok(())
        }

        async fn update_last_used(&self, _api_key_id: i64) -> Result<(), AuthError> {
            Ok(())
        }

        async fn record_usage(
            &self,
            _api_key_id: i64,
            _method: &str,
            _latency_ms: u64,
            _success: bool,
        ) -> Result<(), AuthError> {
            Ok(())
        }

        async fn create(&self, _key: ApiKey, _methods: Vec<String>) -> Result<String, AuthError> {
            Ok("test_hash".to_string())
        }

        async fn list_all(&self) -> Result<Vec<ApiKey>, AuthError> {
            Ok(vec![])
        }

        async fn revoke(&self, _name: &str) -> Result<(), AuthError> {
            Ok(())
        }

        async fn update_rate_limits(
            &self,
            _name: &str,
            _max_tokens: u32,
            _refill_rate: u32,
        ) -> Result<(), AuthError> {
            Ok(())
        }
    }

    async fn test_handler(
        axum::extract::Extension(auth_key): axum::extract::Extension<AuthenticatedKey>,
    ) -> String {
        format!("authenticated: {}", auth_key.name)
    }

    fn create_test_auth() -> Arc<ApiKeyAuth> {
        let repo = TestApiKeyRepository::new();
        Arc::new(ApiKeyAuth::new(Arc::new(repo)))
    }

    #[tokio::test]
    async fn test_header_auth_success() {
        let auth = create_test_auth();
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(auth.clone(), api_key_middleware))
            .with_state(auth);

        let request = Request::builder()
            .uri("/test")
            .header("X-API-Key", "valid_key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_query_param_auth_success() {
        let auth = create_test_auth();
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(auth.clone(), api_key_middleware))
            .with_state(auth);

        let request =
            Request::builder().uri("/test?api_key=valid_key").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_header_takes_precedence_over_query() {
        let auth = create_test_auth();
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(auth.clone(), api_key_middleware))
            .with_state(auth);

        let request = Request::builder()
            .uri("/test?api_key=invalid_key")
            .header("X-API-Key", "valid_key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_missing_key_returns_unauthorized() {
        let auth = create_test_auth();
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(auth.clone(), api_key_middleware))
            .with_state(auth);

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalid_key_returns_unauthorized() {
        let auth = create_test_auth();
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(auth.clone(), api_key_middleware))
            .with_state(auth);

        let request = Request::builder()
            .uri("/test")
            .header("X-API-Key", "invalid_key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_inserts_authenticated_key_into_extensions() {
        let auth = create_test_auth();
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn_with_state(auth.clone(), api_key_middleware))
            .with_state(auth);

        let request = Request::builder()
            .uri("/test")
            .header("X-API-Key", "valid_key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("authenticated: test_key"));
    }
}
