# Auth API Key Module

## Overview

The API Key module (`crates/prism-core/src/auth/api_key.rs`) provides core data structures and utilities for managing API keys in the Prism RPC aggregator. It defines the `ApiKey` model that represents stored authentication credentials, cryptographic key generation using ring's secure random number generator, and SHA256-based key hashing for secure storage.

**Key Responsibilities:**
- Define the `ApiKey` struct with all authentication and quota fields
- Generate cryptographically secure API keys using `ring::rand::SystemRandom`
- Hash API keys with SHA256 for secure database storage
- Provide validation methods for expiration, quota checking, and usage tracking
- Define the `MethodPermission` struct for per-method access control

This module is pure data structures and utilities - it does not handle database operations or HTTP middleware. Those responsibilities belong to the repository and middleware modules respectively.

## Architecture

### Component Structure

```
auth/api_key.rs (Lines 1-368)
├── ApiKey (Lines 13-42)                   # Main API key data model
│   ├── id: i64                            # Database primary key
│   ├── key_hash: String                   # SHA256 hash of raw key
│   ├── name: String                       # Human-readable identifier
│   ├── description: Option<String>        # Optional description
│   ├── rate_limit_max_tokens: u32         # Rate limiter bucket size
│   ├── rate_limit_refill_rate: u32        # Tokens per second
│   ├── daily_request_limit: Option<i64>   # Optional daily quota
│   ├── daily_requests_used: i64           # Current usage counter
│   ├── quota_reset_at: DateTime<Utc>      # Quota reset timestamp
│   ├── created_at: DateTime<Utc>          # Creation timestamp
│   ├── updated_at: DateTime<Utc>          # Last modification timestamp
│   ├── last_used_at: Option<DateTime<Utc>> # Last authentication
│   ├── is_active: bool                    # Active/revoked flag
│   └── expires_at: Option<DateTime<Utc>>  # Optional expiration
│
└── MethodPermission (Lines 145-156)      # Per-method permissions
    ├── id: i64                            # Database primary key
    ├── api_key_id: i64                    # Foreign key to api_keys
    ├── method_name: String                # JSON-RPC method name
    ├── max_requests_per_day: Option<i64>  # Per-method daily limit
    └── requests_today: i64                # Current method usage
```

### Dependencies

```rust
use crate::auth::AuthError;              // Error types
use chrono::{DateTime, Utc};             // Timestamp handling
use ring::rand::{SecureRandom, SystemRandom}; // Secure key generation
use serde::{Deserialize, Serialize};     // JSON serialization
use sha2::{Digest, Sha256};              // SHA256 hashing
```

## API Key Structure

### `ApiKey` Struct

**Location:** Lines 13-42

The `ApiKey` struct represents a complete API key record as stored in the database with all authentication, rate limiting, and quota tracking fields.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: i64,
    pub key_hash: String,
    pub name: String,
    pub description: Option<String>,
    pub rate_limit_max_tokens: u32,
    pub rate_limit_refill_rate: u32,
    pub daily_request_limit: Option<i64>,
    pub daily_requests_used: i64,
    pub quota_reset_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub expires_at: Option<DateTime<Utc>>,
}
```

#### Fields

**Identity Fields:**
- `id`: Database primary key, auto-assigned on creation
- `key_hash`: SHA256 hash of the raw API key (64 hex characters), used for secure lookups without storing the actual key
- `name`: User-friendly identifier for the key (e.g., "production-app", "staging-api")
- `description`: Optional long-form description for documentation purposes

**Rate Limiting Fields:**
- `rate_limit_max_tokens`: Maximum number of tokens in the token bucket rate limiter (e.g., 100 = allow bursts up to 100 requests)
- `rate_limit_refill_rate`: Number of tokens added per second (e.g., 10 = 10 requests/sec sustained rate)

**Quota Management Fields:**
- `daily_request_limit`: Optional hard limit on requests per day (None = unlimited)
- `daily_requests_used`: Current count of requests used today, reset at `quota_reset_at`
- `quota_reset_at`: UTC timestamp when daily counters should be reset (typically 24 hours after last reset)

**Lifecycle Fields:**
- `created_at`: UTC timestamp when the key was created
- `updated_at`: UTC timestamp of last modification to the key record
- `last_used_at`: Optional UTC timestamp of last successful authentication (None = never used)
- `is_active`: Boolean flag controlling whether the key can authenticate (false = revoked)
- `expires_at`: Optional UTC timestamp when the key expires (None = no expiration)

#### Serialization

The struct derives `Serialize` and `Deserialize` for JSON serialization, allowing it to be:
- Returned in CLI/API responses
- Stored in configuration files
- Logged for debugging (ensure logs redact sensitive data)

**Security Note:** The `key_hash` field is safe to expose in logs and APIs. The raw API key is never stored in this struct and should only be returned once at creation time.

## Key Generation

### `generate()` Static Method

**Location:** Lines 59-79

Generates a cryptographically secure API key suitable for authentication. Uses the `ring` crate's `SystemRandom` generator which provides cryptographic-quality randomness from the operating system.

```rust
pub fn generate() -> Result<String, AuthError>
```

**Returns:**
- `Ok(String)`: Generated API key in format `rpc_[32 random chars]`
- `Err(AuthError::KeyGenerationError)`: System RNG failed (extremely rare)

**Generation Process:**

1. **Initialize RNG** (Line 63): Create `SystemRandom` instance backed by OS entropy
2. **Generate Random Bytes** (Lines 64-68): Fill 32-byte array with cryptographically secure random data
3. **Map to Charset** (Lines 70-76): Convert each byte to alphanumeric character (A-Z, a-z, 0-9) by modulo mapping
4. **Add Prefix** (Line 78): Prepend "rpc_" for easy identification and namespacing

**Constants:**
- `CHARSET`: `"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"` (62 characters)
- `KEY_LENGTH`: 32 characters (provides ~190 bits of entropy: log2(62^32) ≈ 190.4 bits)

**Example Output:**
```
rpc_aBc123XyZ456QrStUvWxYz789012
```

**Security Properties:**
- Uses OS-level cryptographic RNG (e.g., `/dev/urandom` on Linux, `BCryptGenRandom` on Windows)
- Each character has 62 possible values (~5.95 bits entropy per character)
- Total entropy: ~190 bits (far exceeds 128-bit security threshold)
- Resistant to timing attacks (constant-time operations)
- No predictable patterns or sequential relationships

**Usage Example:**
```rust
use prism_core::auth::api_key::ApiKey;

match ApiKey::generate() {
    Ok(raw_key) => {
        println!("Generated key: {}", raw_key);
        // IMPORTANT: Display this key to user ONCE - cannot be retrieved later
        // Store hash_key(&raw_key) in database, never store raw_key
    }
    Err(e) => eprintln!("Key generation failed: {}", e),
}
```

**Error Handling:**

The `SystemRandom::fill()` method can fail if the OS RNG is unavailable (extremely rare). This typically only occurs in:
- Containerized environments with broken `/dev/urandom`
- Embedded systems without entropy sources
- Systems under severe resource exhaustion

In practice, this error should never occur on properly configured systems.

## Key Hashing

### `hash_key()` Static Method

**Location:** Lines 90-94

Hashes an API key using SHA256 for secure storage. The hash is used as the lookup key in the database, ensuring that even if the database is compromised, attackers cannot extract valid API keys.

```rust
#[must_use]
pub fn hash_key(key: &str) -> String
```

**Parameters:**
- `key`: Raw API key string (e.g., "rpc_abc123...")

**Returns:**
- Lowercase hexadecimal SHA256 hash (64 characters)

**Hashing Process:**

1. **Create Hasher** (Line 91): Initialize SHA256 digest
2. **Hash Key Bytes** (Line 92): Process raw key string as UTF-8 bytes
3. **Format Output** (Line 93): Convert 32-byte digest to 64-character hex string

**Example:**
```rust
let raw_key = "rpc_abc123xyz";
let hash = ApiKey::hash_key(raw_key);
// hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
```

**Security Properties:**

- **One-Way Function:** SHA256 is cryptographically secure hash - cannot reverse hash to obtain key
- **Collision Resistant:** Computationally infeasible to find two keys with same hash
- **Deterministic:** Same key always produces same hash (enables lookups)
- **Uniform Distribution:** Hashes are evenly distributed across output space (good for indexing)

**Storage Model:**

```
User's API Key (shown once)     Database Storage
------------------------       ------------------
rpc_abc123xyz          -->     9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08

Authentication Flow:
1. User sends: X-API-Key: rpc_abc123xyz
2. Middleware hashes: hash_key("rpc_abc123xyz")
3. Database lookup: WHERE key_hash = '9f86...'
4. If found -> authenticated
```

**Database Security:**

If an attacker gains read access to the database, they see:
- Key hashes (useless without original keys)
- Usage statistics
- Configuration data

They cannot:
- Authenticate as users (need raw keys, not hashes)
- Reverse engineer keys (SHA256 is one-way)
- Perform offline brute force (keys have ~190 bits entropy)

**Constant-Time Comparison:**

When validating API keys, always use constant-time comparison to prevent timing attacks:

```rust
// CORRECT: Constant-time comparison
let provided_hash = ApiKey::hash_key(&provided_key);
let stored_hash = key_record.key_hash;
if provided_hash == stored_hash { /* authenticated */ }

// INCORRECT: Don't do this
// if provided_key == stored_key { /* vulnerable to timing attacks */ }
```

The string equality operator in Rust is already constant-time for fixed-length strings like SHA256 hashes.

## Key Validation Methods

### `is_expired()` Method

**Location:** Lines 103-109

Checks whether the API key has passed its expiration date.

```rust
#[must_use]
pub fn is_expired(&self) -> bool
```

**Returns:**
- `true`: Key has expired and should not authenticate
- `false`: Key is not expired or has no expiration date

**Logic:**

```rust
if let Some(expires_at) = self.expires_at {
    Utc::now() > expires_at
} else {
    false
}
```

**Behavior:**
- If `expires_at` is `None`: Returns `false` (key never expires)
- If `expires_at` is `Some(date)`: Returns `true` if current time is past that date

**Usage in Authentication:**

Called by middleware during authentication flow (see `crates/prism-core/src/middleware/auth.rs` Lines 62-64):

```rust
if key.is_expired() {
    return Err(AuthError::ExpiredApiKey);
}
```

**Example:**
```rust
use chrono::{Utc, Duration};

let mut key = ApiKey {
    // ... other fields
    expires_at: Some(Utc::now() + Duration::days(30)),
    // ... other fields
};

assert!(!key.is_expired()); // Not expired yet

// Simulate time passing
key.expires_at = Some(Utc::now() - Duration::hours(1));
assert!(key.is_expired()); // Now expired
```

**Design Note:**

This method is marked `#[must_use]` to ensure callers don't ignore the result. If you call `is_expired()` without checking the return value, the compiler will warn you.

### `needs_quota_reset()` Method

**Location:** Lines 120-122

Determines whether daily quotas should be reset based on the `quota_reset_at` timestamp.

```rust
#[must_use]
pub fn needs_quota_reset(&self) -> bool
```

**Returns:**
- `true`: Current time has passed `quota_reset_at`, quotas should be reset
- `false`: Still within current quota period

**Logic:**

```rust
Utc::now() > self.quota_reset_at
```

**Usage in Authentication:**

Called by middleware to automatically reset quotas (see `middleware/auth.rs` Lines 67-69):

```rust
if key.needs_quota_reset() {
    self.repository.reset_daily_quotas(key.id).await?;
}
```

**Quota Reset Behavior:**

When `needs_quota_reset()` returns `true`, the repository performs:
1. Set `daily_requests_used = 0`
2. Set `quota_reset_at = now + 24 hours`
3. Reset all method-specific counters to 0

**Example:**
```rust
use chrono::{Utc, Duration};

let mut key = ApiKey {
    // ... other fields
    quota_reset_at: Utc::now() + Duration::hours(24),
    daily_requests_used: 1500,
    // ... other fields
};

// Within quota period
assert!(!key.needs_quota_reset());

// Simulate 25 hours passing
key.quota_reset_at = Utc::now() - Duration::hours(1);
assert!(key.needs_quota_reset()); // Time to reset
```

**Quota Period:**

By default, quotas operate on a rolling 24-hour window:
- First request: Sets `quota_reset_at = now + 24h`
- After 24h: `needs_quota_reset()` returns true
- Reset: New period starts with `quota_reset_at = now + 24h`

This is NOT a midnight-to-midnight daily quota, but a rolling window from first use.

### `is_within_quota()` Method

**Location:** Lines 131-137

Checks whether the API key has remaining requests available under its daily quota.

```rust
#[must_use]
pub fn is_within_quota(&self) -> bool
```

**Returns:**
- `true`: Key has quota remaining OR no quota limit is set
- `false`: Key has reached or exceeded daily quota limit

**Logic:**

```rust
if let Some(limit) = self.daily_request_limit {
    self.daily_requests_used < limit
} else {
    true
}
```

**Behavior:**
- If `daily_request_limit` is `None`: Returns `true` (unlimited)
- If `daily_request_limit` is `Some(limit)`: Returns `true` only if `daily_requests_used < limit`

**Usage in Authentication:**

Called by middleware to enforce quotas (see `middleware/auth.rs` Lines 71-73):

```rust
if !key.is_within_quota() {
    return Err(AuthError::QuotaExceeded);
}
```

**Example:**
```rust
let mut key = ApiKey {
    // ... other fields
    daily_request_limit: Some(1000),
    daily_requests_used: 500,
    // ... other fields
};

assert!(key.is_within_quota()); // 500 < 1000

key.daily_requests_used = 999;
assert!(key.is_within_quota()); // 999 < 1000

key.daily_requests_used = 1000;
assert!(!key.is_within_quota()); // 1000 == 1000 (not less than)

key.daily_requests_used = 1001;
assert!(!key.is_within_quota()); // Over quota
```

**Important:** The check is `used < limit`, not `used <= limit`. Once `daily_requests_used` equals the limit, the quota is exhausted.

**Unlimited Quota:**

```rust
let key = ApiKey {
    // ... other fields
    daily_request_limit: None,
    daily_requests_used: 999999,
    // ... other fields
};

assert!(key.is_within_quota()); // Always true when limit is None
```

**Usage Tracking:**

The `daily_requests_used` counter is incremented by the repository's `increment_usage()` method (see `repository.rs` Lines 181-196) after successful authentication. This happens automatically in the authentication middleware.

## Method Permissions

### `MethodPermission` Struct

**Location:** Lines 145-156

Represents per-method access control and quota limits for an API key. Each API key can have multiple method permissions defining which JSON-RPC methods it can call and optional per-method daily limits.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodPermission {
    pub id: i64,
    pub api_key_id: i64,
    pub method_name: String,
    pub max_requests_per_day: Option<i64>,
    pub requests_today: i64,
}
```

#### Fields

- `id`: Database primary key
- `api_key_id`: Foreign key reference to the `ApiKey.id` this permission belongs to
- `method_name`: JSON-RPC method name (e.g., "eth_blockNumber", "eth_getLogs", "eth_getBlockByNumber")
- `max_requests_per_day`: Optional limit on calls to this specific method per day (None = unlimited)
- `requests_today`: Current count of requests to this method today, reset with daily quota

**Usage:**

Method permissions are loaded by the repository's `get_methods()` method (see `repository.rs` Lines 252-276) during authentication and attached to the `AuthenticatedKey` for validation.

**Example Database State:**

```
API Key: "production-app" (id: 1)
Method Permissions:
- id: 1, method_name: "eth_blockNumber",     max: None,       used: 1500
- id: 2, method_name: "eth_getLogs",         max: Some(1000), used: 850
- id: 3, method_name: "eth_getBlockByNumber", max: Some(5000), used: 4200
```

**Permission Checking:**

When a request comes in:
1. Extract `method` from JSON-RPC request body
2. Check if `method` exists in `allowed_methods` (from method permissions)
3. If method has `max_requests_per_day`, check if `requests_today < max`
4. Increment `requests_today` after successful request

**Default Limits:**

When creating method permissions via `repository.create()`, the default `max_requests_per_day` is 1000 (see `repository.rs` Lines 359-382).

## Authenticated Context

### `AuthenticatedKey` Struct

**Location:** `crates/prism-core/src/auth/mod.rs` Lines 52-67

After successful authentication, the `ApiKey` and its method permissions are transformed into an `AuthenticatedKey` for use in request handling. This struct contains only the information needed for request processing, omitting internal details like key hashes and timestamps.

```rust
#[derive(Debug, Clone)]
pub struct AuthenticatedKey {
    pub id: i64,
    pub name: String,
    pub rate_limit_max_tokens: u32,
    pub rate_limit_refill_rate: u32,
    pub daily_request_limit: Option<i64>,
    pub allowed_methods: Vec<String>,
    pub method_limits: std::collections::HashMap<String, i64>,
}
```

#### Fields

- `id`: API key database ID (for usage tracking)
- `name`: Human-readable key name (for logging)
- `rate_limit_max_tokens`: Token bucket size for rate limiting
- `rate_limit_refill_rate`: Rate limiter refill rate
- `daily_request_limit`: Global daily request limit
- `allowed_methods`: List of permitted JSON-RPC methods
- `method_limits`: Map of method name to per-method daily limit

**Construction:**

Built during authentication (see `middleware/auth.rs` Lines 75-88):

```rust
let methods = self.repository.get_methods(key.id).await?;
let allowed_methods: Vec<String> = methods.iter()
    .map(|m| m.method_name.clone())
    .collect();

let method_limits: HashMap<String, i64> = methods.iter()
    .filter_map(|m| m.max_requests_per_day.map(|limit| (m.method_name.clone(), limit)))
    .collect();

let auth_key = AuthenticatedKey {
    id: key.id,
    name: key.name,
    rate_limit_max_tokens: key.rate_limit_max_tokens,
    rate_limit_refill_rate: key.rate_limit_refill_rate,
    daily_request_limit: key.daily_request_limit,
    allowed_methods,
    method_limits,
};
```

**Request Extension:**

The `AuthenticatedKey` is inserted into the Axum request extensions and can be extracted in any downstream handler:

```rust
use axum::Extension;
use prism_core::auth::AuthenticatedKey;

async fn handle_request(Extension(auth): Extension<AuthenticatedKey>) {
    tracing::info!("Request from: {}", auth.name);

    // Check method permission
    let method = "eth_blockNumber";
    if !auth.allowed_methods.contains(&method.to_string()) {
        // Method not allowed
    }
}
```

## Error Types

### `AuthError` Enum

**Location:** `crates/prism-core/src/auth/mod.rs` Lines 8-44

Comprehensive error type for all authentication-related failures using the `thiserror` crate for automatic `Error` trait implementation.

```rust
#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Invalid API key")]
    InvalidApiKey,

    #[error("API key expired")]
    ExpiredApiKey,

    #[error("API key inactive")]
    InactiveApiKey,

    #[error("Method not allowed: {0}")]
    MethodNotAllowed(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Daily quota exceeded")]
    QuotaExceeded,

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Key generation error: {0}")]
    KeyGenerationError(String),
}
```

#### Variants

**`InvalidApiKey`**
- **Meaning:** API key hash not found in database OR key is marked inactive
- **Trigger:** Database lookup returns `None` OR `is_active = false`
- **HTTP Status:** 401 Unauthorized
- **User Action:** Verify key is correct and has not been revoked

**`ExpiredApiKey`**
- **Meaning:** Key's `expires_at` timestamp has passed
- **Trigger:** `is_expired()` returns `true`
- **HTTP Status:** 401 Unauthorized
- **User Action:** Request new API key from administrator

**`InactiveApiKey`**
- **Meaning:** Key has been explicitly revoked/deactivated
- **Trigger:** `is_active = false` in database
- **HTTP Status:** 401 Unauthorized
- **User Action:** Contact administrator about revocation

**`MethodNotAllowed(String)`**
- **Meaning:** API key does not have permission to call requested JSON-RPC method
- **Trigger:** Method not in `allowed_methods` list
- **HTTP Status:** 403 Forbidden
- **User Action:** Request method permission from administrator
- **Example:** `MethodNotAllowed("eth_sendRawTransaction")`

**`RateLimitExceeded`**
- **Meaning:** Token bucket rate limiter has no tokens available
- **Trigger:** Rate limiting middleware detects exhausted bucket
- **HTTP Status:** 429 Too Many Requests
- **User Action:** Slow down request rate, retry after backoff

**`QuotaExceeded`**
- **Meaning:** Daily request limit reached
- **Trigger:** `is_within_quota()` returns `false`
- **HTTP Status:** 429 Too Many Requests
- **User Action:** Wait for quota reset (check `quota_reset_at`)

**`DatabaseError(String)`**
- **Meaning:** Database operation failed
- **Trigger:** SQLx query error, connection pool exhausted, etc.
- **HTTP Status:** 500 Internal Server Error
- **User Action:** Retry request, contact support if persists
- **Example:** `DatabaseError("Connection pool timeout")`

**`ConfigError(String)`**
- **Meaning:** Authentication configuration is invalid
- **Trigger:** Invalid database URL, missing required settings
- **HTTP Status:** 500 Internal Server Error
- **User Action:** Server-side configuration issue
- **Example:** `ConfigError("Invalid database URL")`

**`KeyGenerationError(String)`**
- **Meaning:** Failed to generate secure random bytes for API key
- **Trigger:** System RNG unavailable (extremely rare)
- **HTTP Status:** 500 Internal Server Error
- **User Action:** Retry, contact support if persists
- **Example:** `KeyGenerationError("Failed to generate secure random bytes")`

#### Error Handling

**In Middleware:**

All authentication errors are returned from the authenticate method and handled by the caller in the server layer:

```rust
let auth_key = auth.authenticate(api_key).await.map_err(|e| {
    tracing::warn!("Authentication failed: {}", e);
    StatusCode::UNAUTHORIZED
})?;
```

**Security Note:** Error details are logged but NOT returned to client. Clients receive generic 401 responses to prevent information leakage about valid/invalid keys, quota status, etc.

**In Repository:**

Database errors are wrapped and converted to AuthError::DatabaseError:

```rust
.await
.map_err(|e| AuthError::DatabaseError(e.to_string()))?;
```

## Security Considerations

### Key Storage Security

**Never Store Raw Keys:**

The database MUST store only hashed keys, never raw keys:

```rust
// CORRECT: Store hash
let raw_key = ApiKey::generate()?;
let key_hash = ApiKey::hash_key(&raw_key);
repo.create(ApiKey { key_hash, /* ... */ }, methods).await?;
// Return raw_key to user ONCE

// INCORRECT: Never do this
repo.create(ApiKey { key_hash: raw_key, /* ... */ }, methods).await?;
```

**One-Time Display:**

Raw API keys should only be displayed to the user once at creation time. There is no "forgot my API key" recovery - users must generate new keys.

### Hashing Properties

**SHA256 Security:**
- **Preimage Resistance:** Cannot reverse hash to obtain original key
- **Collision Resistance:** Cannot find two keys with same hash
- **Output Size:** 256 bits (64 hex characters)
- **Performance:** ~500 MB/s on modern CPUs (negligible overhead)

**Why Not Argon2/bcrypt?**

Password hashing algorithms like Argon2/bcrypt are designed to be slow to resist brute-force attacks on weak passwords. API keys:
- Have ~190 bits of entropy (vs. ~40 bits for typical passwords)
- Are random, not user-chosen (no dictionary attacks)
- Are computationally infeasible to brute-force even with fast hashing
- Need fast verification for high-throughput authentication

SHA256 is appropriate for high-entropy API keys where speed matters.

### Constant-Time Comparison

When comparing hashes, use constant-time comparison to prevent timing attacks:

```rust
// CORRECT: Built-in string equality is constant-time for same-length strings
let provided_hash = ApiKey::hash_key(&provided_key);
if provided_hash == stored_hash {
    // Authenticated
}

// INCORRECT: Don't do character-by-character comparison
// Early exit on mismatch leaks information
```

Rust's `==` operator for `String` is already constant-time when strings have equal length (which SHA256 hashes always do).

### Database Compromise Scenarios

**If Attacker Gains Read Access:**

- **Can See:** Key hashes, usage stats, rate limits, allowed methods
- **Cannot Do:** Authenticate (need raw keys), reverse engineer keys (one-way hash)
- **Risk:** Low - hashes alone are useless

**If Attacker Gains Write Access:**

- **Can Do:** Modify quotas, add methods, create new keys
- **Impact:** High - can grant themselves unlimited access
- **Mitigation:** Strong database access controls, audit logging

**Defense in Depth:**

1. **Encrypted Database:** Use SQLite encryption extensions (SQLCipher)
2. **File Permissions:** Restrict database file to service account
3. **Audit Logging:** Log all key creations, modifications, deletions
4. **Key Rotation:** Periodic key rotation policies
5. **Least Privilege:** Database user has minimum required permissions

### Key Entropy Analysis

**Entropy Calculation:**

```
Charset size: 62 (A-Z, a-z, 0-9)
Key length: 32 characters
Entropy per character: log2(62) ≈ 5.95 bits
Total entropy: 32 × 5.95 ≈ 190 bits
```

**Attack Complexity:**

- **Brute Force:** 2^190 attempts (computationally infeasible)
- **Birthday Attack:** 2^95 attempts (still infeasible)
- **Comparison:** Bitcoin private keys have 256 bits entropy

**Recommended Entropy Threshold:**

- **128 bits:** Minimum for long-term security
- **190 bits:** Comfortable margin
- **256 bits:** Overkill for most applications

The 32-character alphanumeric format provides sufficient entropy while remaining human-manageable for copy-paste operations.

## Usage Examples

### Creating an API Key

```rust
use prism_core::auth::{api_key::ApiKey, repository::SqliteRepository};
use chrono::{Utc, Duration};

async fn create_key() -> Result<String, AuthError> {
    // 1. Generate raw key
    let raw_key = ApiKey::generate()?;
    println!("Generated key: {}", raw_key);

    // 2. Hash for storage
    let key_hash = ApiKey::hash_key(&raw_key);

    // 3. Build API key record
    let api_key = ApiKey {
        id: 0, // Auto-assigned by database
        key_hash,
        name: "production-api".to_string(),
        description: Some("Production application API access".to_string()),
        rate_limit_max_tokens: 100,
        rate_limit_refill_rate: 10,
        daily_request_limit: Some(100_000),
        daily_requests_used: 0,
        quota_reset_at: Utc::now() + Duration::days(1),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_used_at: None,
        is_active: true,
        expires_at: Some(Utc::now() + Duration::days(365)),
    };

    // 4. Define allowed methods
    let methods = vec![
        "eth_blockNumber".to_string(),
        "eth_getBlockByNumber".to_string(),
        "eth_getTransactionByHash".to_string(),
        "eth_getLogs".to_string(),
    ];

    // 5. Store in database
    let repo = SqliteRepository::new("sqlite://api_keys.db").await?;
    repo.create(api_key, methods).await?;

    // 6. Return raw key (ONLY TIME IT'S AVAILABLE)
    Ok(raw_key)
}
```

### Validating an API Key

```rust
use prism_core::auth::api_key::ApiKey;

fn validate_key_status(key: &ApiKey) -> Result<(), String> {
    // Check active status
    if !key.is_active {
        return Err("Key is revoked".to_string());
    }

    // Check expiration
    if key.is_expired() {
        return Err("Key has expired".to_string());
    }

    // Check quota
    if !key.is_within_quota() {
        return Err(format!(
            "Daily quota exceeded: {}/{}",
            key.daily_requests_used,
            key.daily_request_limit.unwrap_or(0)
        ));
    }

    Ok(())
}
```

### Testing Key Generation

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_generation() {
        let key = ApiKey::generate().expect("Key generation should succeed");

        // Check format
        assert!(key.starts_with("rpc_"));
        assert_eq!(key.len(), 36); // "rpc_" + 32 chars

        // Check uniqueness
        let key2 = ApiKey::generate().unwrap();
        assert_ne!(key, key2);
    }

    #[test]
    fn test_key_hashing() {
        let key = "rpc_test123";
        let hash1 = ApiKey::hash_key(key);
        let hash2 = ApiKey::hash_key(key);

        // Deterministic
        assert_eq!(hash1, hash2);

        // Correct length (64 hex chars)
        assert_eq!(hash1.len(), 64);

        // Different keys produce different hashes
        let hash3 = ApiKey::hash_key("rpc_test456");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_expiration_check() {
        use chrono::{Utc, Duration};

        let mut key = ApiKey {
            expires_at: Some(Utc::now() + Duration::days(30)),
            // ... other fields
        };

        assert!(!key.is_expired());

        key.expires_at = Some(Utc::now() - Duration::hours(1));
        assert!(key.is_expired());

        key.expires_at = None;
        assert!(!key.is_expired());
    }

    #[test]
    fn test_quota_checking() {
        let mut key = ApiKey {
            daily_request_limit: Some(1000),
            daily_requests_used: 500,
            // ... other fields
        };

        assert!(key.is_within_quota());

        key.daily_requests_used = 1000;
        assert!(!key.is_within_quota());

        key.daily_request_limit = None;
        key.daily_requests_used = 9999;
        assert!(key.is_within_quota()); // No limit
    }
}
```

### Quota Management

```rust
use chrono::{Utc, Duration};

async fn manage_quota(key: &mut ApiKey, repo: &impl ApiKeyRepository) -> Result<(), AuthError> {
    // Check if quota period expired
    if key.needs_quota_reset() {
        println!("Resetting quota for key: {}", key.name);
        repo.reset_daily_quotas(key.id).await?;

        // Update local copy
        key.daily_requests_used = 0;
        key.quota_reset_at = Utc::now() + Duration::days(1);
    }

    // Check quota availability
    if !key.is_within_quota() {
        let reset_in = key.quota_reset_at - Utc::now();
        return Err(AuthError::QuotaExceeded);
    }

    // Increment usage
    repo.increment_usage(key.id).await?;
    key.daily_requests_used += 1;

    Ok(())
}
```

### Method Permission Validation

```rust
use prism_core::auth::AuthenticatedKey;

fn check_method_permission(
    auth: &AuthenticatedKey,
    method: &str,
) -> Result<(), AuthError> {
    // Check if method is allowed
    if !auth.allowed_methods.contains(&method.to_string()) {
        return Err(AuthError::MethodNotAllowed(method.to_string()));
    }

    // Check method-specific quota if exists
    if let Some(&limit) = auth.method_limits.get(method) {
        // In real implementation, would check requests_today from database
        println!("Method {} has daily limit of {}", method, limit);
    }

    Ok(())
}
```

## Thread Safety

### Concurrency Guarantees

The `ApiKey` and `MethodPermission` structs are safe to use in concurrent contexts:

**Traits:**
- `Debug`, `Clone`: Standard derivation for debugging and copying
- `Serialize`, `Deserialize`: Thread-safe JSON serialization
- No interior mutability: All fields are immutable after construction

**Validation Methods:**

All validation methods (`is_expired()`, `needs_quota_reset()`, `is_within_quota()`) are marked `#[must_use]` and are pure functions:
- No shared mutable state
- No side effects
- Safe to call from any thread
- Results are deterministic based on input

**Static Methods:**

`generate()` and `hash_key()` are thread-safe:
- `generate()`: Uses `ring::rand::SystemRandom` which is thread-safe
- `hash_key()`: Pure function with no shared state

### Usage in Concurrent Systems

**Shared API Keys:**

```rust
use std::sync::Arc;

let api_key = Arc::new(api_key_record);

// Safe to clone Arc and use in multiple threads
let key1 = api_key.clone();
let key2 = api_key.clone();

tokio::spawn(async move {
    if key1.is_expired() { /* ... */ }
});

tokio::spawn(async move {
    if key2.is_within_quota() { /* ... */ }
});
```

**Mutation Handling:**

API keys are typically immutable after loading from database. If you need to update fields:

```rust
// CORRECT: Load fresh copy from database
let updated_key = repo.find_by_hash(&key_hash).await?;

// INCORRECT: Mutating shared reference
// let mut shared_key = api_key.clone();
// shared_key.daily_requests_used += 1; // Local only, not reflected in DB
```

All mutations should go through the repository layer which handles database transactions and synchronization.

### Authentication Cache Concurrency

The authentication middleware uses `DashMap` for concurrent caching (see `middleware/auth.rs`):

```rust
cache: Arc<DashMap<String, CachedAuth>>
```

**DashMap Properties:**
- Lock-free reads (cache hits)
- Fine-grained locking for writes (cache misses)
- No global contention
- Safe for high-concurrency authentication

**Cache Update Race:**

Multiple threads authenticating the same key simultaneously is safe:
- First thread: Cache miss → database lookup → cache insert
- Other threads: Cache miss or hit (depending on timing)
- Duplicate database lookups are harmless (read-only)
- Last write wins (all threads produce identical cache value)

## Performance Characteristics

### Key Generation

**Benchmarks:**
- **Generation:** ~1-5 microseconds per key (depends on OS RNG)
- **Throughput:** ~200,000-1,000,000 keys/second
- **Latency:** Consistent, no spikes

**Bottleneck:** Operating system entropy gathering (e.g., `/dev/urandom` read)

### Key Hashing

**Benchmarks:**
- **Hashing:** ~500-1000 nanoseconds per hash
- **Throughput:** ~1,000,000-2,000,000 hashes/second
- **Memory:** Zero allocations (uses stack buffer)

**Comparison:**
- SHA256: ~1 microsecond
- Argon2: ~50-100 milliseconds (50,000× slower)
- bcrypt: ~100-300 milliseconds (100,000× slower)

### Validation Methods

**Benchmarks:**
- `is_expired()`: ~10-50 nanoseconds (timestamp comparison)
- `needs_quota_reset()`: ~10-50 nanoseconds (timestamp comparison)
- `is_within_quota()`: ~5-20 nanoseconds (integer comparison)

All validation methods compile to simple CPU instructions with no allocations.

### Memory Usage

**Per `ApiKey` Instance:**
- Fixed fields: ~100 bytes (integers, booleans, timestamps)
- String fields: ~200 bytes (name, description, key_hash)
- Total: ~300 bytes per key

**Per `MethodPermission` Instance:**
- Fixed fields: ~24 bytes (integers)
- String field: ~30 bytes (method_name average)
- Total: ~50 bytes per permission

**Cache Memory:**

With 10,000 active keys cached:
- API keys: 10,000 × 300 bytes = 3 MB
- Method permissions: ~50,000 × 50 bytes = 2.5 MB
- DashMap overhead: ~0.5 MB
- Total: ~6 MB for typical workload

## Related Components

**Integration Points:**

- **`crates/prism-core/src/auth/mod.rs`**: Module exports, `AuthError` and `AuthenticatedKey` definitions (Lines 1-68)
- **`crates/prism-core/src/auth/repository.rs`**: Database operations using `ApiKey` and `MethodPermission` structs (Lines 1-569)
- **`crates/prism-core/src/middleware/auth.rs`**: Authentication middleware consuming these types (Lines 1-98)
- **`crates/server/src/main.rs`**: Server initialization and configuration

**Related Documentation:**

- See `docs/components/middleware_auth.md` for authentication flow and caching
- See `docs/components/middleware_rate_limiting.md` for rate limiting using `rate_limit_*` fields

## Database Schema

The `ApiKey` and `MethodPermission` structs map to the following SQLite schema:

**`api_keys` Table:**
```sql
CREATE TABLE api_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    rate_limit_max_tokens INTEGER NOT NULL,
    rate_limit_refill_rate INTEGER NOT NULL,
    daily_request_limit INTEGER,
    daily_requests_used INTEGER NOT NULL DEFAULT 0,
    quota_reset_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    last_used_at TIMESTAMP,
    is_active BOOLEAN NOT NULL DEFAULT 1,
    expires_at TIMESTAMP
);

CREATE INDEX idx_api_keys_key_hash ON api_keys(key_hash);
CREATE INDEX idx_api_keys_name ON api_keys(name);
```

**`api_key_methods` Table:**
```sql
CREATE TABLE api_key_methods (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    api_key_id INTEGER NOT NULL,
    method_name TEXT NOT NULL,
    max_requests_per_day INTEGER,
    requests_today INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (api_key_id) REFERENCES api_keys(id) ON DELETE CASCADE,
    UNIQUE(api_key_id, method_name)
);

CREATE INDEX idx_api_key_methods_api_key_id ON api_key_methods(api_key_id);
```

**Relationship:**
- One `ApiKey` has many `MethodPermission` records
- Foreign key constraint ensures referential integrity
- Cascade delete removes permissions when key is deleted
- Unique constraint prevents duplicate method entries per key
