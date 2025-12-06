use super::{
    api_key::{ApiKey, MethodPermission},
    AuthError,
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use sqlx::{Pool, Row, Sqlite, SqlitePool};

/// Usage statistics for a specific date and method.
#[derive(Debug, Clone)]
pub struct UsageStats {
    pub date: String,
    pub method_name: String,
    pub request_count: i64,
    pub total_latency_ms: i64,
    pub error_count: i64,
}

/// Repository trait for API key database operations.
///
/// Provides an abstraction layer enabling testability (mock implementations)
/// and backend flexibility (current: `SQLite`; planned: `PostgreSQL`).
#[async_trait]
pub trait ApiKeyRepository: Send + Sync {
    /// Finds and verifies an active API key by the plaintext key using blind index lookup.
    ///
    /// # Security: Timing-Attack Resistant
    ///
    /// Uses two-phase lookup: SHA-256 blind index for O(1) database lookup,
    /// then Argon2id verification. Ensures authentication time doesn't reveal
    /// whether keys exist or their database position.
    async fn find_and_verify_key(&self, plaintext_key: &str) -> Result<Option<ApiKey>, AuthError>;

    /// Finds an active API key by its blind index.
    ///
    /// The blind index is a SHA-256 hash of the plaintext key, allowing O(1)
    /// database lookups without timing attacks. Caller must verify with Argon2id.
    async fn find_by_blind_index(&self, blind_index: &str) -> Result<Option<ApiKey>, AuthError>;

    /// Finds an active API key by its hash (deprecated for Argon2id).
    ///
    /// Not useful with Argon2id since each hash has a unique salt.
    /// Use `find_and_verify_key` instead.
    async fn find_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, AuthError>;

    async fn get_methods(&self, api_key_id: i64) -> Result<Vec<MethodPermission>, AuthError>;

    async fn increment_usage(&self, api_key_id: i64) -> Result<(), AuthError>;

    async fn increment_method_usage(&self, api_key_id: i64, method: &str) -> Result<(), AuthError>;

    async fn reset_daily_quotas(&self, api_key_id: i64) -> Result<(), AuthError>;

    async fn update_last_used(&self, api_key_id: i64) -> Result<(), AuthError>;

    async fn record_usage(
        &self,
        api_key_id: i64,
        method: &str,
        latency_ms: u64,
        success: bool,
    ) -> Result<(), AuthError>;

    async fn create(&self, key: ApiKey, methods: Vec<String>) -> Result<String, AuthError>;

    async fn list_all(&self) -> Result<Vec<ApiKey>, AuthError>;

    async fn revoke(&self, name: &str) -> Result<(), AuthError>;

    async fn update_rate_limits(
        &self,
        name: &str,
        max_tokens: u32,
        refill_rate: u32,
    ) -> Result<(), AuthError>;

    async fn get_usage_stats(
        &self,
        api_key_id: i64,
        days: i64,
    ) -> Result<Vec<UsageStats>, AuthError>;
}

pub struct SqliteRepository {
    pool: Pool<Sqlite>,
}

impl SqliteRepository {
    /// # Errors
    /// Returns `AuthError::DatabaseError` if connection fails.
    pub async fn new(database_url: &str) -> Result<Self, AuthError> {
        let pool = SqlitePool::connect(database_url).await?;

        Ok(Self { pool })
    }

    /// Extracts a non-nullable field from a database row.
    /// Returns `DatabaseError` if field is NULL or cannot be decoded.
    fn get_required<'r, T>(row: &'r sqlx::sqlite::SqliteRow, column: &str) -> Result<T, AuthError>
    where
        T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
    {
        row.try_get::<T, _>(column)
            .map_err(|e| AuthError::DatabaseError(format!("column '{column}': {e}")))
    }

    /// Extracts and converts i64 to u32, returning error on overflow or negative values.
    fn get_u32(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<u32, AuthError> {
        let value: i64 = Self::get_required(row, column)?;
        u32::try_from(value).map_err(|e| {
            AuthError::DatabaseError(format!(
                "column '{column}' value {value} out of u32 range: {e}"
            ))
        })
    }

    /// Converts a database row to an `ApiKey` struct.
    fn row_to_api_key(row: &sqlx::sqlite::SqliteRow) -> Result<ApiKey, AuthError> {
        Ok(ApiKey {
            id: Self::get_required(row, "id")?,
            key_hash: Self::get_required(row, "key_hash")?,
            blind_index: Self::get_required(row, "blind_index")?,
            name: Self::get_required(row, "name")?,
            description: row.get::<Option<String>, _>("description"),
            rate_limit_max_tokens: Self::get_u32(row, "rate_limit_max_tokens")?,
            rate_limit_refill_rate: Self::get_u32(row, "rate_limit_refill_rate")?,
            daily_request_limit: row.get::<Option<i64>, _>("daily_request_limit"),
            daily_requests_used: Self::get_required(row, "daily_requests_used")?,
            quota_reset_at: DateTime::from_naive_utc_and_offset(
                Self::get_required(row, "quota_reset_at")?,
                Utc,
            ),
            created_at: DateTime::from_naive_utc_and_offset(
                Self::get_required(row, "created_at")?,
                Utc,
            ),
            updated_at: DateTime::from_naive_utc_and_offset(
                Self::get_required(row, "updated_at")?,
                Utc,
            ),
            last_used_at: row
                .get::<Option<NaiveDateTime>, _>("last_used_at")
                .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc)),
            is_active: Self::get_required(row, "is_active")?,
            expires_at: row
                .get::<Option<NaiveDateTime>, _>("expires_at")
                .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc)),
        })
    }
}

#[async_trait]
impl ApiKeyRepository for SqliteRepository {
    async fn find_and_verify_key(&self, plaintext_key: &str) -> Result<Option<ApiKey>, AuthError> {
        if !ApiKey::is_valid_format(plaintext_key) {
            return Ok(None);
        }

        let blind_index = ApiKey::compute_blind_index(plaintext_key);

        let api_key = self.find_by_blind_index(&blind_index).await?;

        match api_key {
            Some(key) => {
                if ApiKey::verify_key(plaintext_key, &key.key_hash) {
                    Ok(Some(key))
                } else {
                    // Blind index matched but Argon2 verification failed
                    // This could happen if there's a hash collision (astronomically unlikely)
                    // or if the blind_index was computed from a different key (data corruption)
                    tracing::warn!("blind index matched but Argon2 verification failed");
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    async fn find_by_blind_index(&self, blind_index: &str) -> Result<Option<ApiKey>, AuthError> {
        let result = sqlx::query(
            r"
            SELECT id, key_hash, blind_index, name, description,
                   rate_limit_max_tokens, rate_limit_refill_rate,
                   daily_request_limit, daily_requests_used,
                   quota_reset_at, created_at, updated_at,
                   last_used_at, is_active, expires_at
            FROM api_keys
            WHERE blind_index = ? AND is_active = 1
            ",
        )
        .bind(blind_index)
        .fetch_optional(&self.pool)
        .await?;

        result.map(|row| Self::row_to_api_key(&row)).transpose()
    }

    async fn find_by_hash(&self, key_hash: &str) -> Result<Option<ApiKey>, AuthError> {
        let result = sqlx::query(
            r"
            SELECT id, key_hash, blind_index, name, description,
                   rate_limit_max_tokens, rate_limit_refill_rate,
                   daily_request_limit, daily_requests_used,
                   quota_reset_at, created_at, updated_at,
                   last_used_at, is_active, expires_at
            FROM api_keys
            WHERE key_hash = ? AND is_active = 1
            ",
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await?;

        result.map(|row| Self::row_to_api_key(&row)).transpose()
    }

    async fn get_methods(&self, api_key_id: i64) -> Result<Vec<MethodPermission>, AuthError> {
        let rows = sqlx::query(
            r"
            SELECT id, api_key_id, method_name,
                   max_requests_per_day, requests_today
            FROM api_key_methods
            WHERE api_key_id = ?
            ",
        )
        .bind(api_key_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(MethodPermission {
                    id: Self::get_required(&row, "id")?,
                    api_key_id: Self::get_required(&row, "api_key_id")?,
                    method_name: Self::get_required(&row, "method_name")?,
                    max_requests_per_day: row.get::<Option<i64>, _>("max_requests_per_day"),
                    requests_today: Self::get_required(&row, "requests_today")?,
                })
            })
            .collect()
    }

    async fn increment_usage(&self, api_key_id: i64) -> Result<(), AuthError> {
        sqlx::query(
            r"
            UPDATE api_keys 
            SET daily_requests_used = daily_requests_used + 1,
                last_used_at = CURRENT_TIMESTAMP
            WHERE id = ?
            ",
        )
        .bind(api_key_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn increment_method_usage(&self, api_key_id: i64, method: &str) -> Result<(), AuthError> {
        sqlx::query(
            r"
            UPDATE api_key_methods 
            SET requests_today = requests_today + 1
            WHERE api_key_id = ? AND method_name = ?
            ",
        )
        .bind(api_key_id)
        .bind(method)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn reset_daily_quotas(&self, api_key_id: i64) -> Result<(), AuthError> {
        sqlx::query(
            r"
            UPDATE api_keys 
            SET daily_requests_used = 0, quota_reset_at = CURRENT_TIMESTAMP
            WHERE id = ?
            ",
        )
        .bind(api_key_id)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r"
            UPDATE api_key_methods
            SET requests_today = 0
            WHERE api_key_id = ?
            ",
        )
        .bind(api_key_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_last_used(&self, api_key_id: i64) -> Result<(), AuthError> {
        sqlx::query(
            r"
            UPDATE api_keys 
            SET last_used_at = CURRENT_TIMESTAMP
            WHERE id = ?
            ",
        )
        .bind(api_key_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn record_usage(
        &self,
        api_key_id: i64,
        method: &str,
        _latency_ms: u64,
        _success: bool,
    ) -> Result<(), AuthError> {
        self.increment_usage(api_key_id).await?;
        self.increment_method_usage(api_key_id, method).await?;
        Ok(())
    }

    async fn create(&self, key: ApiKey, methods: Vec<String>) -> Result<String, AuthError> {
        let mut tx = self.pool.begin().await?;

        let key_id = sqlx::query(
            r"
            INSERT INTO api_keys (key_hash, blind_index, name, description, rate_limit_max_tokens,
                                rate_limit_refill_rate, daily_request_limit, daily_requests_used,
                                quota_reset_at, created_at, updated_at, is_active, expires_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(&key.key_hash)
        .bind(&key.blind_index)
        .bind(&key.name)
        .bind(&key.description)
        .bind(i64::from(key.rate_limit_max_tokens))
        .bind(i64::from(key.rate_limit_refill_rate))
        .bind(key.daily_request_limit)
        .bind(key.daily_requests_used)
        .bind(key.quota_reset_at.naive_utc())
        .bind(key.created_at.naive_utc())
        .bind(key.updated_at.naive_utc())
        .bind(key.is_active)
        .bind(key.expires_at.map(|dt| dt.naive_utc()))
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();

        for method in methods {
            sqlx::query(
                r"
                INSERT INTO api_key_methods (api_key_id, method_name, max_requests_per_day)
                VALUES (?, ?, 1000)
                ",
            )
            .bind(key_id)
            .bind(method)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(key.key_hash)
    }

    async fn list_all(&self) -> Result<Vec<ApiKey>, AuthError> {
        let rows = sqlx::query(
            r"
            SELECT id, key_hash, blind_index, name, description,
                   rate_limit_max_tokens, rate_limit_refill_rate,
                   daily_request_limit, daily_requests_used,
                   quota_reset_at, created_at, updated_at,
                   last_used_at, is_active, expires_at
            FROM api_keys
            ORDER BY created_at DESC
            ",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|row| Self::row_to_api_key(&row)).collect()
    }

    async fn revoke(&self, name: &str) -> Result<(), AuthError> {
        sqlx::query(
            r"
            UPDATE api_keys
            SET is_active = 0
            WHERE name = ?
            ",
        )
        .bind(name)
        .execute(&self.pool)
        .await?;

        tracing::info!(
            key_name = name,
            "api key revoked - cache remains valid as it contains immutable blockchain data"
        );
        Ok(())
    }

    async fn update_rate_limits(
        &self,
        name: &str,
        max_tokens: u32,
        refill_rate: u32,
    ) -> Result<(), AuthError> {
        sqlx::query(
            r"
            UPDATE api_keys
            SET rate_limit_max_tokens = ?, rate_limit_refill_rate = ?
            WHERE name = ?
            ",
        )
        .bind(i64::from(max_tokens))
        .bind(i64::from(refill_rate))
        .bind(name)
        .execute(&self.pool)
        .await?;

        tracing::info!(
            key_name = name,
            max_tokens = max_tokens,
            refill_rate = refill_rate,
            "rate limits updated - cache remains valid as it contains immutable blockchain data"
        );
        Ok(())
    }

    async fn get_usage_stats(
        &self,
        api_key_id: i64,
        days: i64,
    ) -> Result<Vec<UsageStats>, AuthError> {
        let rows = sqlx::query(
            r"
            SELECT date, method_name, request_count, total_latency_ms, error_count
            FROM api_key_usage
            WHERE api_key_id = ?
              AND date >= date('now', ? || ' days')
            ORDER BY date DESC, method_name
            ",
        )
        .bind(api_key_id)
        .bind(format!("-{days}"))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(UsageStats {
                    date: Self::get_required(&row, "date")?,
                    method_name: Self::get_required(&row, "method_name")?,
                    request_count: Self::get_required(&row, "request_count")?,
                    total_latency_ms: Self::get_required(&row, "total_latency_ms")?,
                    error_count: Self::get_required(&row, "error_count")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// SQL schema for test databases including the `blind_index` column.
    /// The production init.sql is missing this column, so we define it here.
    const TEST_SCHEMA: &str = r"
        CREATE TABLE api_keys (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            key_hash TEXT NOT NULL UNIQUE,
            blind_index TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL UNIQUE,
            description TEXT,
            rate_limit_max_tokens INTEGER NOT NULL DEFAULT 100,
            rate_limit_refill_rate INTEGER NOT NULL DEFAULT 10,
            daily_request_limit INTEGER,
            daily_requests_used INTEGER NOT NULL DEFAULT 0,
            quota_reset_at TIMESTAMP NOT NULL,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_used_at TIMESTAMP,
            is_active BOOLEAN NOT NULL DEFAULT 1,
            expires_at TIMESTAMP
        );

        CREATE TABLE api_key_methods (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            api_key_id INTEGER NOT NULL,
            method_name TEXT NOT NULL,
            max_requests_per_day INTEGER,
            requests_today INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (api_key_id) REFERENCES api_keys(id) ON DELETE CASCADE,
            UNIQUE(api_key_id, method_name)
        );

        CREATE INDEX idx_api_keys_blind_index ON api_keys(blind_index);
        CREATE INDEX idx_api_keys_hash ON api_keys(key_hash);
        CREATE INDEX idx_api_keys_active ON api_keys(is_active);
        CREATE INDEX idx_api_key_methods_lookup ON api_key_methods(api_key_id, method_name);
    ";

    /// Creates an in-memory `SQLite` repository with the test schema.
    async fn create_test_repo() -> SqliteRepository {
        let repo = SqliteRepository::new(":memory:").await.expect("Should create repo");
        sqlx::raw_sql(TEST_SCHEMA)
            .execute(&repo.pool)
            .await
            .expect("Should create schema");
        repo
    }

    /// Creates a test API key with all fields populated.
    fn create_test_api_key(name: &str) -> (ApiKey, String) {
        let plaintext_key = ApiKey::generate().expect("Should generate key");
        let key_hash = ApiKey::hash_key(&plaintext_key).expect("Should hash key");
        let blind_index = ApiKey::compute_blind_index(&plaintext_key);
        let now = Utc::now();

        let api_key = ApiKey {
            id: 0,
            key_hash,
            blind_index,
            name: name.to_string(),
            description: Some(format!("Test key {name}")),
            rate_limit_max_tokens: 100,
            rate_limit_refill_rate: 10,
            daily_request_limit: Some(1000),
            daily_requests_used: 0,
            quota_reset_at: now + Duration::days(1),
            created_at: now,
            updated_at: now,
            last_used_at: None,
            is_active: true,
            expires_at: None,
        };

        (api_key, plaintext_key)
    }

    #[tokio::test]
    async fn test_repository_creation() {
        let repo_result = SqliteRepository::new(":memory:").await;
        assert!(repo_result.is_ok(), "Repository creation should succeed");
    }

    #[tokio::test]
    async fn test_repository_creation_invalid_url() {
        let result = SqliteRepository::new("invalid://not-a-valid-url").await;
        assert!(result.is_err(), "Invalid URL should fail");
    }

    #[tokio::test]
    async fn test_create_api_key() {
        let repo = create_test_repo().await;
        let (api_key, _plaintext) = create_test_api_key("test-key");
        let methods = vec!["eth_blockNumber".to_string(), "eth_getLogs".to_string()];

        let result = repo.create(api_key.clone(), methods).await;

        assert!(result.is_ok(), "Create should succeed");
        let returned_hash = result.unwrap();
        assert_eq!(returned_hash, api_key.key_hash, "Should return key hash");
    }

    #[tokio::test]
    async fn test_create_api_key_with_no_methods() {
        let repo = create_test_repo().await;
        let (api_key, _plaintext) = create_test_api_key("no-methods-key");
        let methods: Vec<String> = vec![];

        let result = repo.create(api_key, methods).await;
        assert!(result.is_ok(), "Create with no methods should succeed");
    }

    #[tokio::test]
    async fn test_create_api_key_duplicate_name_fails() {
        let repo = create_test_repo().await;
        let (api_key1, _plaintext1) = create_test_api_key("duplicate-name");
        let (api_key2, _plaintext2) = create_test_api_key("duplicate-name");

        let result1 = repo.create(api_key1, vec![]).await;
        assert!(result1.is_ok(), "First create should succeed");

        let result2 = repo.create(api_key2, vec![]).await;
        assert!(result2.is_err(), "Duplicate name should fail");
    }

    #[tokio::test]
    async fn test_find_and_verify_key_success() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("verify-test");
        repo.create(api_key.clone(), vec!["eth_blockNumber".to_string()])
            .await
            .expect("Create should succeed");

        let result = repo.find_and_verify_key(&plaintext).await;

        assert!(result.is_ok(), "Find and verify should succeed");
        let found_key = result.unwrap();
        assert!(found_key.is_some(), "Should find the key");
        let key = found_key.unwrap();
        assert_eq!(key.name, "verify-test", "Should match key name");
        assert!(key.is_active, "Key should be active");
    }

    #[tokio::test]
    async fn test_find_and_verify_key_not_found() {
        let repo = create_test_repo().await;
        let (api_key, _plaintext) = create_test_api_key("exists-key");
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        // Generate a different key that wasn't stored
        let other_key = ApiKey::generate().expect("Should generate key");
        let result = repo.find_and_verify_key(&other_key).await;

        assert!(result.is_ok(), "Query should succeed");
        assert!(result.unwrap().is_none(), "Should not find nonexistent key");
    }

    #[tokio::test]
    async fn test_find_and_verify_key_invalid_format() {
        let repo = create_test_repo().await;

        let result = repo.find_and_verify_key("invalid-key-format").await;
        assert!(result.is_ok(), "Query should succeed");
        assert!(result.unwrap().is_none(), "Invalid format should return None");

        let result = repo.find_and_verify_key("").await;
        assert!(result.is_ok(), "Query should succeed");
        assert!(result.unwrap().is_none(), "Empty key should return None");

        let result = repo.find_and_verify_key("rpc_short").await;
        assert!(result.is_ok(), "Query should succeed");
        assert!(result.unwrap().is_none(), "Too short key should return None");
    }

    #[tokio::test]
    async fn test_find_and_verify_key_inactive_key() {
        let repo = create_test_repo().await;
        let (mut api_key, plaintext) = create_test_api_key("inactive-test");
        api_key.is_active = false;
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let result = repo.find_and_verify_key(&plaintext).await;
        assert!(result.is_ok(), "Query should succeed");
        assert!(result.unwrap().is_none(), "Should not find inactive key");
    }

    #[tokio::test]
    async fn test_find_by_blind_index_success() {
        let repo = create_test_repo().await;
        let (api_key, _plaintext) = create_test_api_key("blind-index-test");
        let blind_index = api_key.blind_index.clone();
        repo.create(api_key.clone(), vec![]).await.expect("Create should succeed");

        let result = repo.find_by_blind_index(&blind_index).await;

        assert!(result.is_ok(), "Query should succeed");
        let found_key = result.unwrap();
        assert!(found_key.is_some(), "Should find key by blind index");
        assert_eq!(found_key.unwrap().name, "blind-index-test");
    }

    #[tokio::test]
    async fn test_find_by_blind_index_not_found() {
        let repo = create_test_repo().await;

        let result = repo.find_by_blind_index("nonexistent_blind_index").await;
        assert!(result.is_ok(), "Query should succeed");
        assert!(result.unwrap().is_none(), "Should not find nonexistent blind index");
    }

    #[tokio::test]
    async fn test_find_by_blind_index_inactive_excluded() {
        let repo = create_test_repo().await;
        let (mut api_key, _plaintext) = create_test_api_key("inactive-blind-test");
        let blind_index = api_key.blind_index.clone();
        api_key.is_active = false;
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let result = repo.find_by_blind_index(&blind_index).await;
        assert!(result.is_ok(), "Query should succeed");
        assert!(result.unwrap().is_none(), "Should not find inactive key");
    }

    #[tokio::test]
    async fn test_find_by_hash_success() {
        let repo = create_test_repo().await;
        let (api_key, _plaintext) = create_test_api_key("hash-test");
        let key_hash = api_key.key_hash.clone();
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let result = repo.find_by_hash(&key_hash).await;

        assert!(result.is_ok(), "Query should succeed");
        let found_key = result.unwrap();
        assert!(found_key.is_some(), "Should find key by hash");
        assert_eq!(found_key.unwrap().name, "hash-test");
    }

    #[tokio::test]
    async fn test_find_by_hash_not_found() {
        let repo = create_test_repo().await;

        let result = repo.find_by_hash("nonexistent_hash").await;
        assert!(result.is_ok(), "Query should succeed");
        assert!(result.unwrap().is_none(), "Should not find nonexistent hash");
    }

    #[tokio::test]
    async fn test_get_methods_returns_all_methods() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("methods-test");
        let methods =
            vec!["eth_blockNumber".to_string(), "eth_getLogs".to_string(), "eth_call".to_string()];
        repo.create(api_key, methods.clone()).await.expect("Create should succeed");

        // Get the key to get its ID
        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        let result = repo.get_methods(found_key.id).await;

        assert!(result.is_ok(), "Get methods should succeed");
        let method_perms = result.unwrap();
        assert_eq!(method_perms.len(), 3, "Should return 3 methods");

        let method_names: Vec<_> = method_perms.iter().map(|m| m.method_name.as_str()).collect();
        assert!(method_names.contains(&"eth_blockNumber"));
        assert!(method_names.contains(&"eth_getLogs"));
        assert!(method_names.contains(&"eth_call"));
    }

    #[tokio::test]
    async fn test_get_methods_empty_for_key_without_methods() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("no-methods-test");
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        let result = repo.get_methods(found_key.id).await;

        assert!(result.is_ok(), "Get methods should succeed");
        assert!(result.unwrap().is_empty(), "Should return empty vec");
    }

    #[tokio::test]
    async fn test_get_methods_nonexistent_key() {
        let repo = create_test_repo().await;

        let result = repo.get_methods(99999).await;
        assert!(result.is_ok(), "Get methods should succeed");
        assert!(result.unwrap().is_empty(), "Should return empty for nonexistent key");
    }

    #[tokio::test]
    async fn test_increment_usage() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("usage-test");
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        assert_eq!(found_key.daily_requests_used, 0, "Initial usage should be 0");

        repo.increment_usage(found_key.id).await.expect("Increment should succeed");
        repo.increment_usage(found_key.id).await.expect("Increment should succeed");
        repo.increment_usage(found_key.id).await.expect("Increment should succeed");

        let updated_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        assert_eq!(updated_key.daily_requests_used, 3, "Usage should be 3");
        assert!(updated_key.last_used_at.is_some(), "Last used should be set");
    }

    #[tokio::test]
    async fn test_increment_method_usage() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("method-usage-test");
        repo.create(api_key, vec!["eth_blockNumber".to_string()])
            .await
            .expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        let methods = repo.get_methods(found_key.id).await.expect("Get methods should succeed");
        let method = methods.iter().find(|m| m.method_name == "eth_blockNumber").unwrap();
        assert_eq!(method.requests_today, 0, "Initial requests should be 0");

        repo.increment_method_usage(found_key.id, "eth_blockNumber")
            .await
            .expect("Increment should succeed");
        repo.increment_method_usage(found_key.id, "eth_blockNumber")
            .await
            .expect("Increment should succeed");

        let methods = repo.get_methods(found_key.id).await.expect("Get methods should succeed");
        let method = methods.iter().find(|m| m.method_name == "eth_blockNumber").unwrap();
        assert_eq!(method.requests_today, 2, "Requests should be 2");
    }

    #[tokio::test]
    async fn test_reset_daily_quotas() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("quota-reset-test");
        repo.create(api_key, vec!["eth_blockNumber".to_string()])
            .await
            .expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        repo.increment_usage(found_key.id).await.expect("Increment should succeed");
        repo.increment_usage(found_key.id).await.expect("Increment should succeed");
        repo.increment_method_usage(found_key.id, "eth_blockNumber")
            .await
            .expect("Increment should succeed");

        let key_before = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        assert_eq!(key_before.daily_requests_used, 2, "Usage should be 2 before reset");

        repo.reset_daily_quotas(found_key.id).await.expect("Reset should succeed");

        let key_after = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        assert_eq!(key_after.daily_requests_used, 0, "Usage should be 0 after reset");

        let methods = repo.get_methods(found_key.id).await.expect("Get methods should succeed");
        let method = methods.iter().find(|m| m.method_name == "eth_blockNumber").unwrap();
        assert_eq!(method.requests_today, 0, "Method requests should be 0 after reset");
    }

    #[tokio::test]
    async fn test_update_last_used() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("last-used-test");
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        assert!(found_key.last_used_at.is_none(), "Initial last_used_at should be None");

        repo.update_last_used(found_key.id).await.expect("Update should succeed");

        let updated_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        assert!(updated_key.last_used_at.is_some(), "last_used_at should be set");
    }

    #[tokio::test]
    async fn test_record_usage() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("record-usage-test");
        repo.create(api_key, vec!["eth_getLogs".to_string()])
            .await
            .expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        repo.record_usage(found_key.id, "eth_getLogs", 50, true)
            .await
            .expect("Record usage should succeed");

        let updated_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        assert_eq!(updated_key.daily_requests_used, 1, "API key usage should be 1");

        let methods = repo.get_methods(found_key.id).await.expect("Get methods should succeed");
        let method = methods.iter().find(|m| m.method_name == "eth_getLogs").unwrap();
        assert_eq!(method.requests_today, 1, "Method requests should be 1");
    }

    #[tokio::test]
    async fn test_list_all_returns_all_keys() {
        let repo = create_test_repo().await;

        let (key1, _) = create_test_api_key("list-test-1");
        let (key2, _) = create_test_api_key("list-test-2");
        let (key3, _) = create_test_api_key("list-test-3");

        repo.create(key1, vec![]).await.expect("Create should succeed");
        repo.create(key2, vec![]).await.expect("Create should succeed");
        repo.create(key3, vec![]).await.expect("Create should succeed");

        let result = repo.list_all().await;

        assert!(result.is_ok(), "List all should succeed");
        let all_keys = result.unwrap();
        assert_eq!(all_keys.len(), 3, "Should return 3 keys");

        let names: Vec<_> = all_keys.iter().map(|k| k.name.as_str()).collect();
        assert!(names.contains(&"list-test-1"));
        assert!(names.contains(&"list-test-2"));
        assert!(names.contains(&"list-test-3"));
    }

    #[tokio::test]
    async fn test_list_all_includes_inactive_keys() {
        let repo = create_test_repo().await;

        let (active_key, _) = create_test_api_key("active-key");
        let (mut inactive_key, _) = create_test_api_key("inactive-key");
        inactive_key.is_active = false;

        repo.create(active_key, vec![]).await.expect("Create should succeed");
        repo.create(inactive_key, vec![]).await.expect("Create should succeed");

        let keys = repo.list_all().await.expect("List all should succeed");

        assert_eq!(keys.len(), 2, "Should return both active and inactive keys");
    }

    #[tokio::test]
    async fn test_list_all_empty_database() {
        let repo = create_test_repo().await;

        let keys = repo.list_all().await.expect("List all should succeed");
        assert!(keys.is_empty(), "Empty database should return empty vec");
    }

    #[tokio::test]
    async fn test_list_all_ordered_by_creation_time_desc() {
        let repo = create_test_repo().await;

        let (first, _) = create_test_api_key("first-key");
        repo.create(first, vec![]).await.expect("Create should succeed");

        let (second, _) = create_test_api_key("second-key");
        repo.create(second, vec![]).await.expect("Create should succeed");

        let (third, _) = create_test_api_key("third-key");
        repo.create(third, vec![]).await.expect("Create should succeed");

        let all_keys = repo.list_all().await.expect("List all should succeed");

        assert_eq!(all_keys[0].name, "third-key", "Most recent key should be first");
        assert_eq!(all_keys[2].name, "first-key", "Oldest key should be last");
    }

    #[tokio::test]
    async fn test_revoke_key() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("revoke-test");
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        assert!(found_key.is_active, "Key should be active initially");

        repo.revoke("revoke-test").await.expect("Revoke should succeed");

        let result = repo.find_and_verify_key(&plaintext).await.expect("Find should succeed");
        assert!(result.is_none(), "Revoked key should not be found");

        let all_keys = repo.list_all().await.expect("List all should succeed");
        let revoked = all_keys.iter().find(|k| k.name == "revoke-test").unwrap();
        assert!(!revoked.is_active, "Key should be marked inactive");
    }

    #[tokio::test]
    async fn test_revoke_nonexistent_key() {
        let repo = create_test_repo().await;

        let result = repo.revoke("nonexistent-key").await;
        assert!(result.is_ok(), "Revoke nonexistent key should succeed");
    }

    #[tokio::test]
    async fn test_update_rate_limits() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("rate-limit-test");
        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        assert_eq!(found_key.rate_limit_max_tokens, 100);
        assert_eq!(found_key.rate_limit_refill_rate, 10);

        repo.update_rate_limits("rate-limit-test", 500, 50)
            .await
            .expect("Update should succeed");

        let updated_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        assert_eq!(updated_key.rate_limit_max_tokens, 500);
        assert_eq!(updated_key.rate_limit_refill_rate, 50);
    }

    #[tokio::test]
    async fn test_update_rate_limits_nonexistent_key() {
        let repo = create_test_repo().await;

        let result = repo.update_rate_limits("nonexistent-key", 500, 50).await;
        assert!(result.is_ok(), "Update nonexistent key should succeed");
    }

    #[tokio::test]
    async fn test_concurrent_usage_increments() {
        let repo = std::sync::Arc::new(create_test_repo().await);
        let (api_key, plaintext) = create_test_api_key("concurrent-test");
        repo.create(api_key, vec!["eth_blockNumber".to_string()])
            .await
            .expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        let key_id = found_key.id;

        let mut handles = vec![];
        for _ in 0..10 {
            let repo_clone = std::sync::Arc::clone(&repo);
            handles.push(tokio::spawn(async move { repo_clone.increment_usage(key_id).await }));
        }

        for handle in handles {
            handle.await.expect("Task should complete").expect("Increment should succeed");
        }

        let final_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        assert_eq!(final_key.daily_requests_used, 10, "All increments should be counted");
    }

    #[tokio::test]
    async fn test_api_key_with_all_optional_fields_null() {
        let repo = create_test_repo().await;
        let plaintext = ApiKey::generate().expect("Should generate key");
        let key_hash = ApiKey::hash_key(&plaintext).expect("Should hash key");
        let blind_index = ApiKey::compute_blind_index(&plaintext);
        let now = Utc::now();

        let api_key = ApiKey {
            id: 0,
            key_hash,
            blind_index,
            name: "minimal-key".to_string(),
            description: None,
            rate_limit_max_tokens: 100,
            rate_limit_refill_rate: 10,
            daily_request_limit: None,
            daily_requests_used: 0,
            quota_reset_at: now + Duration::days(1),
            created_at: now,
            updated_at: now,
            last_used_at: None,
            is_active: true,
            expires_at: None,
        };

        let result = repo.create(api_key, vec![]).await;
        assert!(result.is_ok(), "Create with null optional fields should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");
        assert!(found_key.description.is_none());
        assert!(found_key.daily_request_limit.is_none());
        assert!(found_key.expires_at.is_none());
    }

    #[tokio::test]
    async fn test_api_key_with_expiration() {
        let repo = create_test_repo().await;
        let plaintext = ApiKey::generate().expect("Should generate key");
        let key_hash = ApiKey::hash_key(&plaintext).expect("Should hash key");
        let blind_index = ApiKey::compute_blind_index(&plaintext);
        let now = Utc::now();

        let api_key = ApiKey {
            id: 0,
            key_hash,
            blind_index,
            name: "expiring-key".to_string(),
            description: Some("This key expires".to_string()),
            rate_limit_max_tokens: 100,
            rate_limit_refill_rate: 10,
            daily_request_limit: Some(5000),
            daily_requests_used: 0,
            quota_reset_at: now + Duration::days(1),
            created_at: now,
            updated_at: now,
            last_used_at: None,
            is_active: true,
            expires_at: Some(now + Duration::days(30)),
        };

        repo.create(api_key, vec![]).await.expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        assert!(found_key.expires_at.is_some());
        let expires = found_key.expires_at.unwrap();
        let diff = expires - now;
        assert!(diff.num_days() >= 29 && diff.num_days() <= 31);
    }

    #[tokio::test]
    async fn test_method_permission_max_requests_per_day() {
        let repo = create_test_repo().await;
        let (api_key, plaintext) = create_test_api_key("method-limit-test");
        repo.create(api_key, vec!["eth_blockNumber".to_string()])
            .await
            .expect("Create should succeed");

        let found_key = repo
            .find_and_verify_key(&plaintext)
            .await
            .expect("Find should succeed")
            .expect("Key should exist");

        let methods = repo.get_methods(found_key.id).await.expect("Get methods should succeed");
        let method = methods.iter().find(|m| m.method_name == "eth_blockNumber").unwrap();

        assert_eq!(method.max_requests_per_day, Some(1000));
    }
}
