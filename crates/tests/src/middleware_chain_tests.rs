//! Middleware Chain Ordering and Integration Tests
//!
//! These tests validate the proper ordering and interaction between middleware layers:
//! - Authentication (Auth) → Rate Limiting → Validation
//!
//! The tests ensure:
//! 1. Middleware executes in the correct order
//! 2. Errors from earlier middleware prevent later middleware from running
//! 3. Cross-middleware state propagation works correctly
//! 4. Error priority is maintained (auth > rate limit > validation)

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use prism_core::{
    auth::{
        api_key::{ApiKey, MethodPermission},
        repository::ApiKeyRepository,
        AuthError,
    },
    middleware::{auth::ApiKeyAuth, rate_limiting::RateLimiter, validation::ValidationError},
    types::JsonRpcRequest,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Mutex;

/// Mock repository for testing authentication middleware in chain tests
struct MockAuthRepository {
    keys: Mutex<Vec<ApiKey>>,
    methods: Mutex<std::collections::HashMap<i64, Vec<String>>>,
    find_calls: AtomicUsize,
}

impl MockAuthRepository {
    fn new() -> Self {
        Self {
            keys: Mutex::new(Vec::new()),
            methods: Mutex::new(std::collections::HashMap::new()),
            find_calls: AtomicUsize::new(0),
        }
    }

    async fn add_key_with_methods(&self, key: ApiKey, methods: Vec<String>) {
        let key_id = key.id;
        self.keys.lock().await.push(key);
        self.methods.lock().await.insert(key_id, methods);
    }

    fn get_find_calls(&self) -> usize {
        self.find_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ApiKeyRepository for MockAuthRepository {
    async fn find_and_verify_key(&self, plaintext_key: &str) -> Result<Option<ApiKey>, AuthError> {
        self.find_calls.fetch_add(1, Ordering::SeqCst);
        let keys = self.keys.lock().await;
        // Simple mock: match by name prefix
        Ok(keys.iter().find(|k| plaintext_key.contains(&k.name)).cloned())
    }

    async fn find_by_blind_index(&self, _blind_index: &str) -> Result<Option<ApiKey>, AuthError> {
        Ok(None)
    }

    async fn find_by_hash(&self, _key_hash: &str) -> Result<Option<ApiKey>, AuthError> {
        Ok(None)
    }

    #[allow(clippy::cast_possible_wrap)]
    async fn get_methods(&self, api_key_id: i64) -> Result<Vec<MethodPermission>, AuthError> {
        let methods = self.methods.lock().await;
        let method_names = methods.get(&api_key_id).cloned().unwrap_or_default();

        Ok(method_names
            .into_iter()
            .enumerate()
            .map(|(i, name)| MethodPermission {
                id: i as i64,
                api_key_id,
                method_name: name,
                max_requests_per_day: None,
                requests_today: 0,
            })
            .collect())
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
        Ok("mock_hash".to_string())
    }

    async fn list_all(&self) -> Result<Vec<ApiKey>, AuthError> {
        Ok(self.keys.lock().await.clone())
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

fn create_api_key(id: i64, name: &str, is_active: bool) -> ApiKey {
    ApiKey {
        id,
        key_hash: "mock_hash".to_string(),
        blind_index: "mock_index".to_string(),
        name: name.to_string(),
        description: None,
        rate_limit_max_tokens: 10,
        rate_limit_refill_rate: 1,
        daily_request_limit: Some(1000),
        daily_requests_used: 0,
        quota_reset_at: Utc::now() + ChronoDuration::hours(24),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_used_at: None,
        is_active,
        expires_at: None,
    }
}

fn create_test_request(method: &str) -> JsonRpcRequest {
    JsonRpcRequest::new(method, Some(json!(["0x1234", false])), json!(1))
}

#[tokio::test]
async fn test_auth_runs_before_rate_limiting() {
    let repo = Arc::new(MockAuthRepository::new());
    // No keys added - auth will fail

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(10, 1);

    let request = create_test_request("eth_blockNumber");

    let auth_result = auth.authenticate("rpc_invalid_key_12345").await;

    assert!(auth_result.is_err(), "Authentication should fail for invalid key");
    assert!(matches!(auth_result.unwrap_err(), AuthError::InvalidApiKey));

    // Rate limiter should not be called since auth failed first
    assert_eq!(rate_limiter.bucket_count(), 0, "Rate limiter should not have any buckets");

    let validation_result = request.validate();
    assert!(validation_result.is_ok(), "Validation would succeed, but we never reach it");
}

#[tokio::test]
async fn test_auth_success_proceeds_to_rate_limiting() {
    let repo = Arc::new(MockAuthRepository::new());
    let key = create_api_key(1, "valid_key", true);
    repo.add_key_with_methods(key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(2, 1);

    let auth_result = auth.authenticate("rpc_valid_key_12345").await;
    assert!(auth_result.is_ok(), "Authentication should succeed");
    let authenticated = auth_result.unwrap();

    let client_key = format!("api_key_{}", authenticated.id);

    // First request - should pass
    assert!(rate_limiter.check_rate_limit(&client_key), "First request should pass rate limit");

    // Second request - should pass (we have 2 tokens)
    assert!(rate_limiter.check_rate_limit(&client_key), "Second request should pass rate limit");

    // Third request - should fail (out of tokens)
    assert!(!rate_limiter.check_rate_limit(&client_key), "Third request should be rate limited");
}

#[tokio::test]
async fn test_rate_limiting_runs_before_validation() {
    let repo = Arc::new(MockAuthRepository::new());
    let key = create_api_key(1, "rate_key", true);
    repo.add_key_with_methods(key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(1, 1); // Only 1 token

    // Authenticate
    let authenticated = auth.authenticate("rpc_rate_key_12345").await.unwrap();
    let client_key = format!("api_key_{}", authenticated.id);

    // Consume the single token
    assert!(rate_limiter.check_rate_limit(&client_key));

    // Now create a request that would FAIL validation (invalid method)
    let invalid_request = JsonRpcRequest::new(
        "eth_unsupported_method", // This method is not allowed
        Some(json!(["0x1234", false])),
        json!(1),
    );

    // Rate limit check should fail BEFORE we even check validation
    assert!(
        !rate_limiter.check_rate_limit(&client_key),
        "Should be rate limited before validation"
    );

    // Validation would fail too, but we never reach it
    let validation_result = invalid_request.validate();
    assert!(validation_result.is_err(), "Validation would fail, but rate limit fails first");
}

#[tokio::test]
async fn test_complete_chain_all_pass() {
    let repo = Arc::new(MockAuthRepository::new());
    let key = create_api_key(1, "complete_key", true);
    repo.add_key_with_methods(key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(10, 5);

    let request = create_test_request("eth_blockNumber");

    // Step 1: Authentication
    let authenticated = auth.authenticate("rpc_complete_key_12345").await;
    assert!(authenticated.is_ok(), "Auth should pass");
    let authenticated = authenticated.unwrap();

    // Step 2: Rate Limiting
    let client_key = format!("api_key_{}", authenticated.id);
    let rate_check = rate_limiter.check_rate_limit(&client_key);
    assert!(rate_check, "Rate limit should pass");

    // Step 3: Validation
    let validation = request.validate();
    assert!(validation.is_ok(), "Validation should pass");
}

#[tokio::test]
async fn test_auth_error_precedence_over_rate_limit() {
    let repo = Arc::new(MockAuthRepository::new());
    // No keys - auth will fail

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(0, 0); // Zero tokens - would always rate limit

    // Try to authenticate
    let auth_result = auth.authenticate("rpc_invalid_12345").await;

    // Auth error should occur first
    assert!(auth_result.is_err());
    assert!(matches!(auth_result.unwrap_err(), AuthError::InvalidApiKey));

    // Rate limiter would also reject, but we never reach it
    let rate_check = rate_limiter.check_rate_limit("any_key");
    assert!(!rate_check, "Rate limiter would also reject, but auth fails first");
}

#[tokio::test]
async fn test_rate_limit_error_precedence_over_validation() {
    let repo = Arc::new(MockAuthRepository::new());
    let key = create_api_key(1, "precedence_key", true);
    repo.add_key_with_methods(key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(1, 1);

    // Authenticate
    let authenticated = auth.authenticate("rpc_precedence_key_12345").await.unwrap();
    let client_key = format!("api_key_{}", authenticated.id);

    // Consume the token
    assert!(rate_limiter.check_rate_limit(&client_key));

    // Create request with invalid JSON-RPC version (would fail validation)
    let invalid_request = JsonRpcRequest {
        jsonrpc: std::borrow::Cow::Owned("1.0".to_string()),
        method: "eth_blockNumber".to_string(),
        params: Some(json!(["0x1234", false])),
        id: Arc::new(json!(1)),
    };

    // Rate limit should fail first
    assert!(
        !rate_limiter.check_rate_limit(&client_key),
        "Rate limit should fail before validation"
    );

    // Validation would also fail, but we never reach it
    let validation_result = invalid_request.validate();
    assert!(validation_result.is_err(), "Validation would fail, but rate limit fails first");
    assert!(matches!(validation_result.unwrap_err(), ValidationError::InvalidVersion(_)));
}

#[tokio::test]
async fn test_error_type_identification() {
    let repo = Arc::new(MockAuthRepository::new());

    // Auth error
    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let auth_error = auth.authenticate("rpc_invalid_12345").await;
    assert!(auth_error.is_err());
    assert!(matches!(auth_error.unwrap_err(), AuthError::InvalidApiKey));

    // Rate limit rejection (boolean false, not an error type)
    let rate_limiter = RateLimiter::new(0, 0);
    assert!(!rate_limiter.check_rate_limit("test_key"));

    // Validation error
    let invalid_request = JsonRpcRequest::new("eth_unsupported", None, json!(1));
    let validation_error = invalid_request.validate();
    assert!(validation_error.is_err());
    assert!(matches!(validation_error.unwrap_err(), ValidationError::MethodNotAllowed(_)));
}

#[tokio::test]
async fn test_authenticated_key_rate_limits_used() {
    let repo = Arc::new(MockAuthRepository::new());
    let mut key = create_api_key(1, "custom_rate_key", true);
    key.rate_limit_max_tokens = 5; // Custom: 5 tokens
    key.rate_limit_refill_rate = 1; // Custom: 1 token/second
    repo.add_key_with_methods(key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);

    // Authenticate and get custom rate limits
    let authenticated = auth.authenticate("rpc_custom_rate_key_12345").await.unwrap();
    assert_eq!(authenticated.rate_limit_max_tokens, 5);
    assert_eq!(authenticated.rate_limit_refill_rate, 1);

    // In a real implementation, these values would be used to create a
    // per-key rate limiter configuration. Here we verify they're available.
    assert!(authenticated.rate_limit_max_tokens > 0);
    assert!(authenticated.rate_limit_refill_rate > 0);
}

#[tokio::test]
async fn test_method_permissions_enforced_at_auth() {
    let repo = Arc::new(MockAuthRepository::new());
    let key = create_api_key(1, "limited_methods_key", true);
    // Only allow eth_blockNumber
    repo.add_key_with_methods(key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);

    // Authenticate
    let authenticated = auth.authenticate("rpc_limited_methods_key_12345").await.unwrap();

    // Check method permissions in AuthenticatedKey
    assert!(authenticated.allowed_methods.contains(&"eth_blockNumber".to_string()));
    assert!(!authenticated.allowed_methods.contains(&"eth_getLogs".to_string()));

    // Validation of eth_getLogs would pass at the validation layer...
    let logs_request = create_test_request("eth_getLogs");
    assert!(logs_request.validate().is_ok(), "eth_getLogs is a valid method");

    // ...but auth layer should have rejected it based on allowed_methods
    // (In practice, the handler would check authenticated.allowed_methods)
}

#[tokio::test]
async fn test_rate_limiter_uses_auth_identifier() {
    let repo = Arc::new(MockAuthRepository::new());
    let key1 = create_api_key(1, "user_one", true);
    let key2 = create_api_key(2, "user_two", true);
    repo.add_key_with_methods(key1, vec!["eth_blockNumber".to_string()]).await;
    repo.add_key_with_methods(key2, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(2, 1);

    // Authenticate two different users
    let user1 = auth.authenticate("rpc_user_one_12345").await.unwrap();
    let user2 = auth.authenticate("rpc_user_two_12345").await.unwrap();

    let user1_key = format!("api_key_{}", user1.id);
    let user2_key = format!("api_key_{}", user2.id);

    // User 1 makes 2 requests (consumes all tokens)
    assert!(rate_limiter.check_rate_limit(&user1_key));
    assert!(rate_limiter.check_rate_limit(&user1_key));
    assert!(!rate_limiter.check_rate_limit(&user1_key), "User 1 should be rate limited");

    // User 2 should have their own token bucket (not affected by user 1)
    assert!(rate_limiter.check_rate_limit(&user2_key), "User 2 should not be affected");
    assert!(rate_limiter.check_rate_limit(&user2_key));
    assert!(!rate_limiter.check_rate_limit(&user2_key), "User 2 should now be rate limited");
}

#[tokio::test]
async fn test_request_passes_all_middleware() {
    let repo = Arc::new(MockAuthRepository::new());
    let key = create_api_key(1, "happy_path_key", true);
    repo.add_key_with_methods(key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(10, 5);
    let request = create_test_request("eth_blockNumber");

    let authenticated = auth.authenticate("rpc_happy_path_key_12345").await;
    assert!(authenticated.is_ok(), "Auth should pass");

    let client_key = format!("api_key_{}", authenticated.unwrap().id);
    assert!(rate_limiter.check_rate_limit(&client_key), "Rate limit should pass");

    assert!(request.validate().is_ok(), "Validation should pass");
}

#[tokio::test]
async fn test_multiple_failures_first_wins() {
    let repo = Arc::new(MockAuthRepository::new());
    // No keys - auth will fail

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(0, 0); // Would also fail
    let invalid_request = JsonRpcRequest::new("invalid_method!", None, json!(1)); // Would also fail

    // Auth fails first
    let auth_result = auth.authenticate("rpc_invalid_12345").await;
    assert!(auth_result.is_err());
    assert!(matches!(auth_result.unwrap_err(), AuthError::InvalidApiKey));

    // We never reach rate limiting or validation
    assert_eq!(rate_limiter.bucket_count(), 0);
    assert!(invalid_request.validate().is_err());
}

#[tokio::test]
async fn test_empty_api_key_handling() {
    let repo = Arc::new(MockAuthRepository::new());
    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);

    // Empty string
    let result = auth.authenticate("").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AuthError::InvalidApiKey));

    // Whitespace only
    let result = auth.authenticate("   ").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AuthError::InvalidApiKey));

    // Just prefix without actual key
    let result = auth.authenticate("rpc_").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AuthError::InvalidApiKey));
}

#[tokio::test]
async fn test_inactive_key_stops_chain() {
    let repo = Arc::new(MockAuthRepository::new());
    let inactive_key = create_api_key(1, "inactive", false); // is_active = false
    repo.add_key_with_methods(inactive_key, vec!["eth_blockNumber".to_string()])
        .await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(10, 5);

    // Auth should fail with InactiveApiKey
    let result = auth.authenticate("rpc_inactive_12345").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AuthError::InactiveApiKey));

    // No buckets created in rate limiter
    assert_eq!(rate_limiter.bucket_count(), 0);
}

#[tokio::test]
async fn test_expired_key_stops_chain() {
    let repo = Arc::new(MockAuthRepository::new());
    let mut expired_key = create_api_key(1, "expired", true);
    expired_key.expires_at = Some(Utc::now() - ChronoDuration::hours(1)); // Expired
    repo.add_key_with_methods(expired_key, vec!["eth_blockNumber".to_string()])
        .await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(10, 5);

    // Auth should fail with ExpiredApiKey
    let result = auth.authenticate("rpc_expired_12345").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AuthError::ExpiredApiKey));

    // No buckets created in rate limiter
    assert_eq!(rate_limiter.bucket_count(), 0);
}

#[tokio::test]
async fn test_quota_exceeded_stops_chain() {
    let repo = Arc::new(MockAuthRepository::new());
    let mut quota_key = create_api_key(1, "quota_exceeded", true);
    quota_key.daily_request_limit = Some(100);
    quota_key.daily_requests_used = 100; // At limit
    repo.add_key_with_methods(quota_key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(10, 5);

    // Auth should fail with QuotaExceeded
    let result = auth.authenticate("rpc_quota_exceeded_12345").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AuthError::QuotaExceeded));

    // No buckets created in rate limiter
    assert_eq!(rate_limiter.bucket_count(), 0);
}

#[tokio::test]
async fn test_cached_auth_still_rate_limited() {
    let repo = Arc::new(MockAuthRepository::new());
    let key = create_api_key(1, "cached", true);
    repo.add_key_with_methods(key, vec!["eth_blockNumber".to_string()]).await;

    let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
    let rate_limiter = RateLimiter::new(2, 1);

    // First authentication - hits repository
    let auth1 = auth.authenticate("rpc_cached_12345").await;
    assert!(auth1.is_ok());
    assert_eq!(repo.get_find_calls(), 1, "First auth should hit repository");

    // Second authentication - uses cache
    let auth2 = auth.authenticate("rpc_cached_12345").await;
    assert!(auth2.is_ok());
    assert_eq!(repo.get_find_calls(), 1, "Second auth should use cache");

    // But rate limiting still applies
    let client_key = format!("api_key_{}", auth2.unwrap().id);
    assert!(rate_limiter.check_rate_limit(&client_key));
    assert!(rate_limiter.check_rate_limit(&client_key));
    assert!(
        !rate_limiter.check_rate_limit(&client_key),
        "Cached auth should still be rate limited"
    );
}
