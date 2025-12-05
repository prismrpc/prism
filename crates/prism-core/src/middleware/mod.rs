//! Request middleware pipeline for JSON-RPC validation, authentication, and rate limiting.
//!
//! This module provides the **business logic layer** for request processing middleware.
//! HTTP adapter functions (Axum extractors and responses) live in `crates/server/src/middleware`,
//! while this module contains the core validation, auth, and rate limiting logic.
//!
//! # Architecture
//!
//! The middleware pipeline processes requests in a strict order:
//!
//! ```text
//!   Incoming Request
//!        │
//!        ▼
//!   ┌─────────────────────────┐
//!   │  1. VALIDATION          │  JsonRpcRequest::validate()
//!   │     - JSON-RPC 2.0      │  - Version check
//!   │     - Method allowlist  │  - Block range limits
//!   │     - Block ranges      │  - Topic count limits
//!   │     - Topic counts      │
//!   └─────────────────────────┘
//!        │ ValidationError?
//!        ├─> JSON-RPC -32600 (Invalid Request)
//!        │
//!        ▼
//!   ┌─────────────────────────┐
//!   │  2. AUTHENTICATION      │  ApiKeyAuth::authenticate()
//!   │     - X-API-Key header  │  - Blind index lookup
//!   │     - Argon2id verify   │  - Status checks
//!   │     - Load permissions  │  - Method permissions
//!   │     - Quota checks      │  - Quota validation
//!   └─────────────────────────┘
//!        │ AuthError?
//!        ├─> JSON-RPC -32001 (Unauthorized)
//!        │
//!        ▼
//!   ┌─────────────────────────┐
//!   │  3. RATE LIMITING       │  RateLimiter::check_rate_limit()
//!   │     - Token bucket      │  - Per-key bucket
//!   │     - Token refill      │  - Configurable rates
//!   │     - Cost calculation  │  - Method-based cost
//!   └─────────────────────────┘
//!        │ RateLimitError?
//!        ├─> JSON-RPC -32005 (Rate Limit Exceeded)
//!        │
//!        ▼
//!   ┌─────────────────────────┐
//!   │  4. REQUEST HANDLER     │  RPC method execution
//!   │     - eth_getLogs       │  - Upstream query
//!   │     - eth_blockNumber   │  - Cache lookup
//!   │     - etc.              │  - Response formatting
//!   └─────────────────────────┘
//!        │
//!        ▼
//!   Response (JSON-RPC result or error)
//! ```
//!
//! # Module Organization
//!
//! - **[`validation`]**: Request format and parameter validation
//! - **[`auth`]**: API key authentication with caching
//! - **[`rate_limiting`]**: Token bucket rate limiter with per-key state
//!
//! # Validation Layer
//!
//! The validation middleware enforces request correctness and security constraints:
//!
//! ## JSON-RPC Format
//!
//! - `jsonrpc` field must be exactly `"2.0"`
//! - `method` field must contain only alphanumeric characters and underscores
//! - `method` must be in the allowlist (see `types::is_method_allowed`)
//!
//! ## Method-Specific Rules
//!
//! ### `eth_getLogs`
//!
//! - **Block range limit**: Maximum 10,000 blocks between `fromBlock` and `toBlock`
//! - **Topic limit**: Maximum 4 topics in the filter (Ethereum specification limit)
//!
//! ```json
//! {
//!   "jsonrpc": "2.0",
//!   "method": "eth_getLogs",
//!   "params": [{
//!     "fromBlock": "0x64",
//!     "toBlock": "0x2710",  // Valid: 10,000 block range
//!     "topics": ["0x1", "0x2", "0x3", "0x4"]  // Valid: 4 topics
//!   }]
//! }
//! ```
//!
//! ### `eth_getBlockByNumber`
//!
//! - Block parameter must be a valid tag (`latest`, `earliest`, `pending`, `safe`, `finalized`) or
//!   a hex-encoded number with `0x` prefix
//! - Hex values limited to 66 characters to prevent oversized inputs
//!
//! # Authentication Layer
//!
//! The auth middleware validates API keys from the `X-API-Key` HTTP header:
//!
//! ## Authentication Flow
//!
//! 1. Extract key from header
//! 2. Check in-memory cache (60-second TTL)
//! 3. If not cached: blind index lookup + Argon2id verification
//! 4. Validate key status (active, not expired, within quota)
//! 5. Load method permissions from database
//! 6. Return `AuthenticatedKey` with permissions
//!
//! ## Caching Strategy
//!
//! The `ApiKeyAuth` component caches successful authentications for 60 seconds:
//!
//! - **Cache hit**: No database query, instant authentication
//! - **Cache miss**: Full authentication flow + database queries
//! - **Invalidation**: TTL-based expiration (no manual invalidation)
//!
//! This reduces database load from ~1000 QPS to ~17 QPS for steady traffic.
//!
//! # Rate Limiting Layer
//!
//! The rate limiter implements a **token bucket algorithm** with per-key state:
//!
//! ## Token Bucket Parameters
//!
//! - `rate_limit_max_tokens`: Bucket capacity (e.g., 100 tokens)
//! - `rate_limit_refill_rate`: Tokens added per second (e.g., 10 tokens/sec)
//!
//! ## Request Cost
//!
//! Each request consumes tokens based on its computational cost:
//!
//! - `eth_blockNumber`: 1 token (cheap, cached)
//! - `eth_getLogs`: 5 tokens (expensive, query-heavy)
//! - `eth_getBlockByNumber`: 2 tokens (medium, single block)
//!
//! ## Refill Logic
//!
//! Tokens refill continuously based on elapsed time:
//!
//! ```rust,ignore
//! elapsed_seconds = (now - last_refill) / 1_000_000_000
//! tokens_to_add = elapsed_seconds * refill_rate
//! current_tokens = min(current_tokens + tokens_to_add, max_tokens)
//! ```
//!
//! This allows bursts up to `max_tokens` while enforcing a sustained rate of `refill_rate` req/sec.
//!
//! # Error Handling
//!
//! Each middleware layer returns specific error types that map to JSON-RPC error codes:
//!
//! | Error Type              | JSON-RPC Code | Description                          |
//! |-------------------------|---------------|--------------------------------------|
//! | `ValidationError`       | -32600        | Invalid request format or parameters |
//! | `AuthError::InvalidApiKey` | -32001     | API key not found or invalid         |
//! | `AuthError::ExpiredApiKey` | -32001     | API key has expired                  |
//! | `AuthError::MethodNotAllowed` | -32601  | Method not permitted for this key    |
//! | `AuthError::QuotaExceeded` | -32005     | Daily quota limit reached            |
//! | `RateLimitError`        | -32005        | Token bucket exhausted               |
//!
//! The `server` crate's middleware converts these to JSON-RPC error responses.
//!
//! # Integration with Server Crate
//!
//! The `server` crate wraps this business logic in Axum middleware:
//!
//! ```rust,ignore
//! // In crates/server/src/middleware/auth.rs
//! pub async fn auth_middleware(
//!     State(auth): State<Arc<ApiKeyAuth>>,  // From prism_core::middleware::auth
//!     TypedHeader(api_key): TypedHeader<XApiKey>,
//!     request: Request,
//!     next: Next,
//! ) -> Result<Response, JsonRpcError> {
//!     // Call business logic
//!     let authenticated = auth.authenticate(&api_key.0).await?;
//!
//!     // Store in request extensions
//!     request.extensions_mut().insert(authenticated);
//!
//!     Ok(next.run(request).await)
//! }
//! ```
//!
//! This separation enables:
//! - **Testing** business logic without HTTP machinery
//! - **Reusability** across different HTTP frameworks
//! - **Clarity** between protocol (JSON-RPC) and transport (HTTP)
//!
//! # Example: Complete Pipeline
//!
//! ```rust,no_run
//! use prism_core::middleware::{
//!     auth::ApiKeyAuth,
//!     rate_limiting::RateLimiter,
//!     ValidationError,
//! };
//! use prism_core::types::JsonRpcRequest;
//! use prism_core::auth::repository::SqliteRepository;
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Initialize middleware components
//! let repo = Arc::new(SqliteRepository::new("sqlite:auth.db").await?);
//! let auth = Arc::new(ApiKeyAuth::new(repo));
//! let rate_limiter = Arc::new(RateLimiter::new());
//!
//! // Parse request
//! let request = JsonRpcRequest::new(
//!     "eth_getLogs",
//!     Some(serde_json::json!([{"fromBlock": "0x64", "toBlock": "0x6e"}])),
//!     serde_json::json!(1),
//! );
//!
//! // Step 1: Validation
//! request.validate()?;
//!
//! // Step 2: Authentication
//! let api_key = "rpc_abc123...";
//! let authenticated = auth.authenticate(api_key).await?;
//!
//! // Step 3: Rate Limiting
//! rate_limiter.check_rate_limit(&authenticated, &request.method).await?;
//!
//! // Step 4: Process request (not shown)
//! // ...
//!
//! # Ok(())
//! # }
//! ```
//!
//! # Performance Characteristics
//!
//! - **Validation**: ~1 µs (pure computation, no I/O)
//! - **Authentication (cached)**: ~10 µs (`DashMap` lookup)
//! - **Authentication (uncached)**: ~50 ms (Argon2id verification + DB query)
//! - **Rate Limiting**: ~5 µs (atomic operations on shared state)
//!
//! Total middleware overhead: **10-20 µs** for cached requests, **~50 ms** for uncached.

pub mod auth;
pub mod rate_limiting;
pub mod validation;

pub use auth::ApiKeyAuth;
pub use rate_limiting::RateLimiter;
pub use validation::ValidationError;
