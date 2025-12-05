//! API key authentication and authorization system.
//!
//! This module provides secure, timing-attack resistant authentication for the Prism RPC server.
//! It implements a complete auth flow from HTTP header extraction to method-level permissions,
//! with `SQLite` persistence and intelligent caching.
//!
//! # Architecture
//!
//! The authentication system uses a **Repository Pattern** for data access, enabling testability
//! and backend flexibility:
//!
//! - **[`api_key`]**: Core data models ([`ApiKey`](api_key::ApiKey),
//!   [`MethodPermission`](api_key::MethodPermission))
//! - **[`repository`]**: Database abstraction layer
//!   ([`ApiKeyRepository`](repository::ApiKeyRepository) trait)
//! - **[`AuthenticatedKey`]**: Post-authentication credentials with permissions and quotas
//!
//! # Authentication Flow
//!
//! ```text
//! 
//!   HTTP Request                    Repository Layer           Authorization
//!   ============                    ================           =============
//!
//!   X-API-Key: rpc_abc...
//!        │
//!        ├──> Format Validation
//!        │    (36 chars, rpc_ prefix)
//!        │
//!        ├──> SHA-256 Blind Index ────> Database Lookup
//!        │    (timing-safe O(1))       (indexed query)
//!        │                                   │
//!        │                                   │
//!        ├──> Argon2id Verification <────────┘
//!        │    (memory-hard, 64MB)            Found: ApiKey
//!        │
//!        ├──> Status Checks
//!        │    - is_active = true
//!        │    - expires_at > now
//!        │    - quota not exceeded
//!        │
//!        ├──> Load Permissions ──────────> api_key_methods table
//!        │
//!        └──> AuthenticatedKey ───────────> Method Validation
//!             - allowed_methods            - Rate Limiting
//!             - rate_limit_*               - Request Processing
//!             - method_limits
//! ```
//!
//! # Security Features
//!
//! ## Timing-Attack Resistance
//!
//! The system uses a **two-phase lookup** to prevent timing attacks:
//!
//! 1. **Blind Index Lookup** (SHA-256): Fast O(1) database query by hash
//! 2. **Argon2id Verification**: Constant-time password verification
//!
//! This ensures authentication time doesn't reveal:
//! - Whether a key exists in the database
//! - The number of keys stored
//! - The position of keys in storage
//!
//! ## Cryptographic Primitives
//!
//! - **Key Generation**: Ring's `SystemRandom` with rejection sampling for uniform distribution
//! - **Blind Index**: SHA-256 (acceptable for high-entropy 62^32 keyspace)
//! - **Key Storage**: Argon2id with OWASP parameters (64MB memory, 3 iterations, parallelism 4)
//!
//! # Rate Limiting Integration
//!
//! Each [`AuthenticatedKey`] contains rate limit configuration:
//!
//! - `rate_limit_max_tokens`: Token bucket capacity
//! - `rate_limit_refill_rate`: Tokens added per second
//! - `daily_request_limit`: Maximum daily requests (global)
//! - `method_limits`: Per-method daily quotas
//!
//! The middleware layer uses these fields for request throttling without additional database
//! queries.
//!
//! # Per-Key Method Permissions
//!
//! Keys can be restricted to specific RPC methods via the `api_key_methods` table:
//!
//! ```sql
//! SELECT method_name FROM api_key_methods WHERE api_key_id = ?
//! ```
//!
//! The `allowed_methods` field in [`AuthenticatedKey`] is populated during authentication
//! and checked before request dispatch. Invalid method attempts return JSON-RPC error `-32001`.
//!
//! # Repository Pattern
//!
//! The [`ApiKeyRepository`](repository::ApiKeyRepository) trait abstracts database operations:
//!
//! - **Current Backend**: `SQLite` ([`SqliteRepository`](repository::SqliteRepository))
//! - **Planned**: `PostgreSQL` for production deployments
//! - **Testing**: Mock implementations for unit tests
//!
//! ## Key Operations
//!
//! - [`find_and_verify_key`](repository::ApiKeyRepository::find_and_verify_key): Authenticate
//!   plaintext key
//! - [`get_methods`](repository::ApiKeyRepository::get_methods): Load method permissions
//! - [`increment_usage`](repository::ApiKeyRepository::increment_usage): Track request counts
//! - [`reset_daily_quotas`](repository::ApiKeyRepository::reset_daily_quotas): Reset daily counters
//!
//! # Quota Management
//!
//! Daily quotas reset automatically based on the `quota_reset_at` timestamp:
//!
//! 1. Check if `now > quota_reset_at` during authentication
//! 2. If true, call `reset_daily_quotas()` to zero counters
//! 3. Update `quota_reset_at` to `now + 24 hours`
//!
//! This ensures quotas reset without requiring background jobs or cron tasks.
//!
//! # Example Usage
//!
//! ```rust,no_run
//! use prism_core::auth::{repository::SqliteRepository, AuthenticatedKey};
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Initialize repository
//! let repo = SqliteRepository::new("sqlite:auth.db").await?;
//!
//! // Authenticate incoming request
//! let plaintext_key = "rpc_abc123..."; // From X-API-Key header
//! let maybe_key = repo.find_and_verify_key(plaintext_key).await?;
//!
//! if let Some(key) = maybe_key {
//!     // Load method permissions
//!     let methods = repo.get_methods(key.id).await?;
//!
//!     // Build authenticated context
//!     let auth = AuthenticatedKey {
//!         id: key.id,
//!         name: key.name,
//!         rate_limit_max_tokens: key.rate_limit_max_tokens,
//!         rate_limit_refill_rate: key.rate_limit_refill_rate,
//!         daily_request_limit: key.daily_request_limit,
//!         allowed_methods: methods.iter().map(|m| m.method_name.clone()).collect(),
//!         method_limits: Default::default(),
//!     };
//!
//!     // Check method authorization
//!     if auth.allowed_methods.contains(&"eth_getLogs".to_string()) {
//!         // Process request...
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Error Handling
//!
//! All operations return [`Result<T, AuthError>`](AuthError):
//!
//! - [`InvalidApiKey`](AuthError::InvalidApiKey): Key not found or verification failed
//! - [`ExpiredApiKey`](AuthError::ExpiredApiKey): Key past expiration timestamp
//! - [`InactiveApiKey`](AuthError::InactiveApiKey): Key revoked via `is_active = false`
//! - [`MethodNotAllowed`](AuthError::MethodNotAllowed): Request method not in `allowed_methods`
//! - [`RateLimitExceeded`](AuthError::RateLimitExceeded): Token bucket exhausted
//! - [`QuotaExceeded`](AuthError::QuotaExceeded): Daily request limit hit
//! - [`DatabaseError`](AuthError::DatabaseError): SQLite/database failure
//!
//! These errors map to JSON-RPC error codes in the middleware layer.

pub mod api_key;
pub mod repository;

use thiserror::Error;

/// Error types for API key authentication and authorization operations.
#[derive(Error, Debug)]
pub enum AuthError {
    /// The provided API key hash was not found in the database
    #[error("Invalid API key")]
    InvalidApiKey,

    /// The API key has passed its expiration timestamp
    #[error("API key expired")]
    ExpiredApiKey,

    /// The API key has been deactivated or revoked
    #[error("API key inactive")]
    InactiveApiKey,

    /// The requested RPC method is not permitted for this API key
    #[error("Method not allowed: {0}")]
    MethodNotAllowed(String),

    /// The API key has exceeded its rate limit tokens
    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    /// The API key has exceeded its daily request quota
    #[error("Daily quota exceeded")]
    QuotaExceeded,

    /// Database operation failed
    #[error("Database error: {0}")]
    DatabaseError(String),

    /// Authentication system configuration is invalid
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Failed to generate a cryptographically secure API key
    #[error("Key generation error: {0}")]
    KeyGenerationError(String),
}

impl From<sqlx::Error> for AuthError {
    fn from(err: sqlx::Error) -> Self {
        AuthError::DatabaseError(err.to_string())
    }
}

/// Authenticated API key with permissions and quota information.
///
/// Returned after successful authentication via [`ApiKeyRepository::find_and_verify_key`].
/// Contains all authorization and rate limiting configuration needed to process
/// a request without additional database lookups.
///
/// # Fields
///
/// - `id`: Database identifier for tracking usage
/// - `name`: Human-readable key name for logging
/// - `rate_limit_max_tokens`: Token bucket capacity for rate limiting
/// - `rate_limit_refill_rate`: Tokens added per second to the bucket
/// - `daily_request_limit`: Maximum requests per 24-hour period (None = unlimited)
/// - `allowed_methods`: Whitelist of permitted RPC methods
/// - `method_limits`: Per-method daily quotas (remaining requests today)
///
/// # Usage Flow
///
/// 1. Client sends request with `X-API-Key` header
/// 2. Middleware validates key and constructs `AuthenticatedKey`
/// 3. Request handler checks `allowed_methods` for authorization
/// 4. Rate limiter uses `rate_limit_*` fields to enforce limits
/// 5. Daily quota tracking uses `method_limits` to prevent abuse
///
/// # Example
///
/// ```
/// use prism_core::auth::AuthenticatedKey;
/// use std::collections::HashMap;
///
/// // After successful authentication:
/// let key = AuthenticatedKey {
///     id: 1,
///     name: "my-api-key".to_string(),
///     rate_limit_max_tokens: 100,
///     rate_limit_refill_rate: 10,
///     daily_request_limit: Some(10000),
///     allowed_methods: vec!["eth_getLogs".to_string(), "eth_getBlockByNumber".to_string()],
///     method_limits: HashMap::new(),
/// };
///
/// // Check if method is allowed
/// if key.allowed_methods.contains(&"eth_getLogs".to_string()) {
///     // Process request
/// }
/// ```
#[derive(Debug, Clone)]
pub struct AuthenticatedKey {
    /// Database ID of the API key
    pub id: i64,
    /// Human-readable name for identification
    pub name: String,
    /// Maximum tokens available in the rate limiter bucket
    pub rate_limit_max_tokens: u32,
    /// Rate at which tokens refill per second
    pub rate_limit_refill_rate: u32,
    /// Maximum requests allowed per day (None = unlimited)
    pub daily_request_limit: Option<i64>,
    /// List of RPC methods this key is permitted to call
    pub allowed_methods: Vec<String>,
    /// Per-method daily request limits (method name -> remaining requests)
    pub method_limits: std::collections::HashMap<String, i64>,
}
