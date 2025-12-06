use crate::auth::{repository::ApiKeyRepository, AuthError, AuthenticatedKey};
use dashmap::DashMap;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub struct ApiKeyAuth {
    repository: Arc<dyn ApiKeyRepository>,
    cache: Arc<DashMap<String, CachedAuth>>,
}

struct CachedAuth {
    key: AuthenticatedKey,
    cached_at: Instant,
}

impl ApiKeyAuth {
    pub fn new(repository: Arc<dyn ApiKeyRepository>) -> Self {
        Self { repository, cache: Arc::new(DashMap::new()) }
    }

    /// # Errors
    /// Returns `AuthError` if the API key is invalid or database lookup fails
    pub async fn authenticate(&self, api_key: &str) -> Result<AuthenticatedKey, AuthError> {
        if let Some(cached) = self.cache.get(api_key) {
            if cached.cached_at.elapsed() < Duration::from_secs(60) {
                return Ok(cached.key.clone());
            }
        }

        let api_key_record = self
            .repository
            .find_and_verify_key(api_key)
            .await?
            .ok_or(AuthError::InvalidApiKey)?;

        if !api_key_record.is_active {
            return Err(AuthError::InactiveApiKey);
        }

        if api_key_record.is_expired() {
            return Err(AuthError::ExpiredApiKey);
        }

        if api_key_record.needs_quota_reset() {
            self.repository.reset_daily_quotas(api_key_record.id).await?;
        }

        if !api_key_record.is_within_quota() {
            return Err(AuthError::QuotaExceeded);
        }

        let methods = self.repository.get_methods(api_key_record.id).await?;

        let authenticated = AuthenticatedKey {
            id: api_key_record.id,
            name: api_key_record.name.clone(),
            rate_limit_max_tokens: api_key_record.rate_limit_max_tokens,
            rate_limit_refill_rate: api_key_record.rate_limit_refill_rate,
            daily_request_limit: api_key_record.daily_request_limit,
            allowed_methods: methods.iter().map(|m| m.method_name.clone()).collect(),
            method_limits: methods
                .iter()
                .filter_map(|m| m.max_requests_per_day.map(|limit| (m.method_name.clone(), limit)))
                .collect(),
        };

        self.cache.insert(
            api_key.to_string(),
            CachedAuth { key: authenticated.clone(), cached_at: Instant::now() },
        );

        Ok(authenticated)
    }
}

#[cfg(test)]
#[allow(clippy::doc_markdown)]
mod tests {
    use super::*;
    use crate::auth::api_key::{ApiKey, MethodPermission};
    use async_trait::async_trait;
    use chrono::{Duration as ChronoDuration, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    struct MockRepository {
        keys: Mutex<Vec<ApiKey>>,
        methods: Mutex<std::collections::HashMap<i64, Vec<MethodPermission>>>,
        find_calls: AtomicUsize,
        reset_calls: AtomicUsize,
        delay_ms: Option<u64>,
        force_error: Mutex<Option<AuthError>>,
    }

    impl MockRepository {
        fn new() -> Self {
            Self {
                keys: Mutex::new(Vec::new()),
                methods: Mutex::new(std::collections::HashMap::new()),
                find_calls: AtomicUsize::new(0),
                reset_calls: AtomicUsize::new(0),
                delay_ms: None,
                force_error: Mutex::new(None),
            }
        }

        #[allow(dead_code)]
        fn with_delay(mut self, ms: u64) -> Self {
            self.delay_ms = Some(ms);
            self
        }

        async fn add_key(&self, key: ApiKey) {
            self.keys.lock().await.push(key);
        }

        async fn add_methods(&self, key_id: i64, methods: Vec<MethodPermission>) {
            self.methods.lock().await.insert(key_id, methods);
        }

        async fn set_force_error(&self, error: AuthError) {
            *self.force_error.lock().await = Some(error);
        }

        fn get_find_calls(&self) -> usize {
            self.find_calls.load(Ordering::SeqCst)
        }

        fn get_reset_calls(&self) -> usize {
            self.reset_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ApiKeyRepository for MockRepository {
        async fn find_and_verify_key(
            &self,
            plaintext_key: &str,
        ) -> Result<Option<ApiKey>, AuthError> {
            self.find_calls.fetch_add(1, Ordering::SeqCst);

            if let Some(err) = self.force_error.lock().await.take() {
                return Err(err);
            }

            if let Some(delay) = self.delay_ms {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }

            let keys = self.keys.lock().await;
            Ok(keys.iter().find(|k| plaintext_key.contains(&k.name)).cloned())
        }

        async fn find_by_blind_index(
            &self,
            _blind_index: &str,
        ) -> Result<Option<ApiKey>, AuthError> {
            Ok(None)
        }

        async fn find_by_hash(&self, _key_hash: &str) -> Result<Option<ApiKey>, AuthError> {
            Ok(None)
        }

        async fn get_methods(&self, api_key_id: i64) -> Result<Vec<MethodPermission>, AuthError> {
            let methods = self.methods.lock().await;
            Ok(methods.get(&api_key_id).cloned().unwrap_or_default())
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
            self.reset_calls.fetch_add(1, Ordering::SeqCst);
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

        async fn get_usage_stats(
            &self,
            _api_key_id: i64,
            _days: i64,
        ) -> Result<Vec<crate::auth::repository::UsageStats>, AuthError> {
            Ok(Vec::new())
        }
    }

    fn create_test_api_key(id: i64, name: &str) -> ApiKey {
        ApiKey {
            id,
            key_hash: "mock_hash".to_string(),
            blind_index: "mock_index".to_string(),
            name: name.to_string(),
            description: None,
            rate_limit_max_tokens: 100,
            rate_limit_refill_rate: 10,
            daily_request_limit: Some(1000),
            daily_requests_used: 0,
            quota_reset_at: Utc::now() + ChronoDuration::hours(24),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_used_at: None,
            is_active: true,
            expires_at: None,
        }
    }

    fn create_test_methods(key_id: i64) -> Vec<MethodPermission> {
        vec![
            MethodPermission {
                id: 1,
                api_key_id: key_id,
                method_name: "eth_blockNumber".to_string(),
                max_requests_per_day: None,
                requests_today: 0,
            },
            MethodPermission {
                id: 2,
                api_key_id: key_id,
                method_name: "eth_getLogs".to_string(),
                max_requests_per_day: Some(100),
                requests_today: 0,
            },
        ]
    }

    #[tokio::test]
    async fn test_authenticate_success() {
        let repo = Arc::new(MockRepository::new());
        let key = create_test_api_key(1, "test_key");
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo);
        let result = auth.authenticate("rpc_test_key_12345").await;

        assert!(result.is_ok(), "Authentication should succeed");
        let authenticated = result.unwrap();
        assert_eq!(authenticated.id, 1);
        assert_eq!(authenticated.name, "test_key");
        assert_eq!(authenticated.allowed_methods.len(), 2);
    }

    #[tokio::test]
    async fn test_authenticate_invalid_key() {
        let repo = Arc::new(MockRepository::new());

        let auth = ApiKeyAuth::new(repo);
        let result = auth.authenticate("rpc_invalid_key_12345").await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::InvalidApiKey => {}
            err => panic!("Expected InvalidApiKey error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_authenticate_inactive_key() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "inactive_key");
        key.is_active = false;
        repo.add_key(key).await;

        let auth = ApiKeyAuth::new(repo);
        let result = auth.authenticate("rpc_inactive_key_12345").await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::InactiveApiKey => {}
            err => panic!("Expected InactiveApiKey error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_authenticate_expired_key() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "expired_key");
        key.expires_at = Some(Utc::now() - ChronoDuration::hours(1));
        repo.add_key(key).await;

        let auth = ApiKeyAuth::new(repo);
        let result = auth.authenticate("rpc_expired_key_12345").await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::ExpiredApiKey => {}
            err => panic!("Expected ExpiredApiKey error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_authenticate_quota_exceeded() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "quota_key");
        key.daily_request_limit = Some(100);
        key.daily_requests_used = 100; // At limit
        repo.add_key(key).await;

        let auth = ApiKeyAuth::new(repo);
        let result = auth.authenticate("rpc_quota_key_12345").await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::QuotaExceeded => {}
            err => panic!("Expected QuotaExceeded error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_authenticate_under_quota() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "quota_key");
        key.daily_request_limit = Some(100);
        key.daily_requests_used = 99;
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo);
        let result = auth.authenticate("rpc_quota_key_12345").await;

        assert!(result.is_ok(), "Authentication should succeed when under quota");
    }

    #[tokio::test]
    async fn test_authenticate_unlimited_quota() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "unlimited_key");
        key.daily_request_limit = None;
        key.daily_requests_used = 1_000_000;
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo);
        let result = auth.authenticate("rpc_unlimited_key_12345").await;

        assert!(result.is_ok(), "Authentication should succeed with unlimited quota");
    }

    #[tokio::test]
    async fn test_authenticate_triggers_quota_reset() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "reset_key");
        key.quota_reset_at = Utc::now() - ChronoDuration::hours(1);
        key.daily_requests_used = 50;
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);
        let _result = auth.authenticate("rpc_reset_key_12345").await;

        assert_eq!(repo.get_reset_calls(), 1, "reset_daily_quotas should be called");
    }

    #[tokio::test]
    async fn test_cache_hit_avoids_repository() {
        let repo = Arc::new(MockRepository::new());
        let key = create_test_api_key(1, "cached_key");
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);

        let _result1 = auth.authenticate("rpc_cached_key_12345").await;
        assert_eq!(repo.get_find_calls(), 1, "First call should hit repository");

        let _result2 = auth.authenticate("rpc_cached_key_12345").await;
        assert_eq!(repo.get_find_calls(), 1, "Second call should use cache, not repository");
    }

    #[tokio::test]
    async fn test_cache_independent_keys() {
        let repo = Arc::new(MockRepository::new());
        let key1 = create_test_api_key(1, "key_one");
        let key2 = create_test_api_key(2, "key_two");
        repo.add_key(key1).await;
        repo.add_key(key2).await;
        repo.add_methods(1, create_test_methods(1)).await;
        repo.add_methods(2, create_test_methods(2)).await;

        let auth = ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>);

        let result1 = auth.authenticate("rpc_key_one_12345").await;
        assert!(result1.is_ok());
        assert_eq!(repo.get_find_calls(), 1);

        let result2 = auth.authenticate("rpc_key_two_12345").await;
        assert!(result2.is_ok());
        assert_eq!(repo.get_find_calls(), 2, "Second key should hit repository");

        let _result3 = auth.authenticate("rpc_key_one_12345").await;
        assert_eq!(repo.get_find_calls(), 2, "First key should still be cached");
    }

    #[tokio::test]
    async fn test_method_permissions_loaded() {
        let repo = Arc::new(MockRepository::new());
        let key = create_test_api_key(1, "methods_key");
        repo.add_key(key).await;

        let methods = vec![
            MethodPermission {
                id: 1,
                api_key_id: 1,
                method_name: "eth_blockNumber".to_string(),
                max_requests_per_day: None,
                requests_today: 0,
            },
            MethodPermission {
                id: 2,
                api_key_id: 1,
                method_name: "eth_getLogs".to_string(),
                max_requests_per_day: Some(500),
                requests_today: 0,
            },
            MethodPermission {
                id: 3,
                api_key_id: 1,
                method_name: "eth_getBlockByNumber".to_string(),
                max_requests_per_day: Some(1000),
                requests_today: 0,
            },
        ];
        repo.add_methods(1, methods).await;

        let auth = ApiKeyAuth::new(repo as Arc<dyn ApiKeyRepository>);
        let result = auth.authenticate("rpc_methods_key_12345").await;

        assert!(result.is_ok());
        let authenticated = result.unwrap();

        // Check allowed methods
        assert_eq!(authenticated.allowed_methods.len(), 3);
        assert!(authenticated.allowed_methods.contains(&"eth_blockNumber".to_string()));
        assert!(authenticated.allowed_methods.contains(&"eth_getLogs".to_string()));
        assert!(authenticated.allowed_methods.contains(&"eth_getBlockByNumber".to_string()));

        assert_eq!(authenticated.method_limits.len(), 2);
        assert_eq!(authenticated.method_limits.get("eth_getLogs"), Some(&500));
        assert_eq!(authenticated.method_limits.get("eth_getBlockByNumber"), Some(&1000));
        assert_eq!(authenticated.method_limits.get("eth_blockNumber"), None);
    }

    #[tokio::test]
    async fn test_authentication_no_methods() {
        let repo = Arc::new(MockRepository::new());
        let key = create_test_api_key(1, "no_methods_key");
        repo.add_key(key).await;

        let auth = ApiKeyAuth::new(repo as Arc<dyn ApiKeyRepository>);
        let result = auth.authenticate("rpc_no_methods_key_12345").await;

        assert!(result.is_ok());
        let authenticated = result.unwrap();
        assert!(authenticated.allowed_methods.is_empty());
        assert!(authenticated.method_limits.is_empty());
    }

    #[tokio::test]
    async fn test_database_error_propagation() {
        let repo = Arc::new(MockRepository::new());
        repo.set_force_error(AuthError::DatabaseError("Connection failed".to_string()))
            .await;

        let auth = ApiKeyAuth::new(repo as Arc<dyn ApiKeyRepository>);
        let result = auth.authenticate("rpc_any_key_12345").await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::DatabaseError(msg) => {
                assert!(msg.contains("Connection failed"));
            }
            err => panic!("Expected DatabaseError, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_rate_limit_config() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "rate_key");
        key.rate_limit_max_tokens = 500;
        key.rate_limit_refill_rate = 50;
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo as Arc<dyn ApiKeyRepository>);
        let result = auth.authenticate("rpc_rate_key_12345").await;

        assert!(result.is_ok());
        let authenticated = result.unwrap();
        assert_eq!(authenticated.rate_limit_max_tokens, 500);
        assert_eq!(authenticated.rate_limit_refill_rate, 50);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_same_key() {
        let repo = Arc::new(MockRepository::new());
        let key = create_test_api_key(1, "concurrent_key");
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = Arc::new(ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let auth_clone = Arc::clone(&auth);
            handles.push(tokio::spawn(async move {
                auth_clone.authenticate("rpc_concurrent_key_12345").await
            }));
        }

        let mut successes = 0;
        for handle in handles {
            if handle.await.expect("Task should not panic").is_ok() {
                successes += 1;
            }
        }

        assert_eq!(successes, 10, "All concurrent requests should succeed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_different_keys() {
        let repo = Arc::new(MockRepository::new());

        for i in 0..5 {
            let key = create_test_api_key(i, &format!("multi_key_{i}"));
            repo.add_key(key).await;
            repo.add_methods(i, create_test_methods(i)).await;
        }

        let auth = Arc::new(ApiKeyAuth::new(repo.clone() as Arc<dyn ApiKeyRepository>));

        let mut handles = Vec::new();
        for i in 0..5 {
            let auth_clone = Arc::clone(&auth);
            handles.push(tokio::spawn(async move {
                auth_clone.authenticate(&format!("rpc_multi_key_{i}_12345")).await
            }));
        }

        let mut successes = 0;
        for handle in handles {
            if handle.await.expect("Task should not panic").is_ok() {
                successes += 1;
            }
        }

        assert_eq!(successes, 5, "All different key requests should succeed");
        assert_eq!(repo.get_find_calls(), 5);
    }

    #[tokio::test]
    async fn test_key_expires_in_future() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "future_key");
        key.expires_at = Some(Utc::now() + ChronoDuration::days(30));
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo as Arc<dyn ApiKeyRepository>);
        let result = auth.authenticate("rpc_future_key_12345").await;

        assert!(result.is_ok(), "Key with future expiration should work");
    }

    #[tokio::test]
    async fn test_key_never_expires() {
        let repo = Arc::new(MockRepository::new());
        let mut key = create_test_api_key(1, "permanent_key");
        key.expires_at = None;
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo as Arc<dyn ApiKeyRepository>);
        let result = auth.authenticate("rpc_permanent_key_12345").await;

        assert!(result.is_ok(), "Key with no expiration should work");
    }

    #[tokio::test]
    async fn test_authenticated_key_id_matches() {
        let repo = Arc::new(MockRepository::new());
        let key = create_test_api_key(42, "id_test_key");
        repo.add_key(key).await;
        repo.add_methods(42, create_test_methods(42)).await;

        let auth = ApiKeyAuth::new(repo as Arc<dyn ApiKeyRepository>);
        let result = auth.authenticate("rpc_id_test_key_12345").await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, 42);
    }

    #[tokio::test]
    async fn test_key_name_preserved() {
        let repo = Arc::new(MockRepository::new());
        let key = create_test_api_key(1, "my_special_key_name");
        repo.add_key(key).await;
        repo.add_methods(1, create_test_methods(1)).await;

        let auth = ApiKeyAuth::new(repo as Arc<dyn ApiKeyRepository>);
        let result = auth.authenticate("rpc_my_special_key_name_12345").await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, "my_special_key_name");
    }
}
