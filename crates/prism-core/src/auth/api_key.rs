use crate::auth::AuthError;
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, Params,
};
use chrono::{DateTime, Utc};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// Scope/role for API key access control.
///
/// Determines which endpoints an API key can access:
/// - `Rpc`: Can access JSON-RPC proxy endpoints only
/// - `Admin`: Can access admin API endpoints only
/// - `Full`: Can access both RPC and admin endpoints
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyScope {
    /// Access to JSON-RPC proxy endpoints only
    #[default]
    Rpc,
    /// Access to admin API endpoints only
    Admin,
    /// Access to both RPC and admin endpoints
    Full,
}

impl ApiKeyScope {
    /// Check if this scope allows access to RPC endpoints
    #[must_use]
    pub fn allows_rpc(&self) -> bool {
        matches!(self, Self::Rpc | Self::Full)
    }

    /// Check if this scope allows access to admin endpoints
    #[must_use]
    pub fn allows_admin(&self) -> bool {
        matches!(self, Self::Admin | Self::Full)
    }

    /// Parse scope from string (for database storage)
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "rpc" => Some(Self::Rpc),
            "admin" => Some(Self::Admin),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// Convert to string for database storage
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Admin => "admin",
            Self::Full => "full",
        }
    }
}

impl fmt::Display for ApiKeyScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Base64 encode bytes for PHC salt format (no padding, URL-safe alphabet).
fn base64_encode_salt(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((bytes.len() * 4).div_ceil(3));

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        result.push(ALPHABET[b0 >> 2] as char);
        result.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if chunk.len() > 1 {
            result.push(ALPHABET[((b1 & 0x0F) << 2) | (b2 >> 6)] as char);
        }
        if chunk.len() > 2 {
            result.push(ALPHABET[b2 & 0x3F] as char);
        }
    }

    result
}

/// Database model for API key authentication and authorization.
///
/// Stores hashed keys (never plaintext) along with rate limiting configuration,
/// daily quotas, and lifecycle metadata. Keys are validated on each request
/// and track usage for quota enforcement.
///
/// # Blind Index for Timing-Attack Resistant Lookup
///
/// The `blind_index` field stores a SHA-256 hash (hex-encoded) of the plaintext key.
/// This enables O(1) database lookups without timing attacks:
/// 1. Compute SHA-256 of incoming key (fast, constant time)
/// 2. Query database by `blind_index` (indexed, O(1))
/// 3. Verify with Argon2id against the single matching row
///
/// This eliminates the need to iterate over all keys and perform Argon2id verification
/// on each, which would leak timing information about key existence and count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// Unique database identifier
    pub id: i64,
    /// Argon2id hash of the API key in PHC string format (never store plaintext keys)
    pub key_hash: String,
    /// SHA-256 blind index for timing-attack resistant lookups (hex-encoded)
    ///
    /// Computed as: `hex(SHA256(plaintext_key))`. Used for O(1) database lookups
    /// without revealing timing information. The actual key is then verified
    /// against `key_hash` using Argon2id.
    pub blind_index: String,
    /// Human-readable name for identification
    pub name: String,
    /// Optional description for the key's purpose
    pub description: Option<String>,
    /// Maximum tokens in the rate limiter bucket
    pub rate_limit_max_tokens: u32,
    /// Token refill rate per second
    pub rate_limit_refill_rate: u32,
    /// Maximum requests allowed per day (None = unlimited)
    pub daily_request_limit: Option<i64>,
    /// Current count of requests used today
    pub daily_requests_used: i64,
    /// Timestamp when daily quota resets
    pub quota_reset_at: DateTime<Utc>,
    /// Timestamp when key was created
    pub created_at: DateTime<Utc>,
    /// Timestamp when key was last modified
    pub updated_at: DateTime<Utc>,
    /// Timestamp when key was last used for a request
    pub last_used_at: Option<DateTime<Utc>>,
    /// Whether the key is currently active (can be revoked)
    pub is_active: bool,
    /// Optional expiration timestamp (None = never expires)
    pub expires_at: Option<DateTime<Utc>>,
    /// Scope/role determining which endpoints this key can access
    pub scope: ApiKeyScope,
}

impl ApiKey {
    /// Generates a cryptographically secure API key with `rpc_` prefix.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::KeyGenerationError`] if the system random number
    /// generator fails to produce secure random bytes.
    ///
    /// # Security
    ///
    /// Keys must be hashed with [`hash_key`](Self::hash_key) before storage.
    /// Never store the plaintext key in the database.
    ///
    /// Uses rejection sampling to ensure uniform distribution across all 62
    /// alphanumeric characters. Without this, characters at indices 0-7 would
    /// be ~1.6% more likely due to 256 % 62 = 8 extra mappings.
    pub fn generate() -> Result<String, AuthError> {
        const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        const CHARSET_LEN: usize = 62;
        const KEY_LENGTH: usize = 32;
        #[allow(clippy::cast_possible_truncation)]
        const MAX_UNBIASED: u8 = (256 / CHARSET_LEN * CHARSET_LEN - 1) as u8;

        let rng = SystemRandom::new();
        let mut key = String::with_capacity(KEY_LENGTH);

        for _ in 0..KEY_LENGTH {
            loop {
                let mut byte = [0u8; 1];
                rng.fill(&mut byte).map_err(|_| {
                    AuthError::KeyGenerationError(
                        "Failed to generate secure random bytes".to_string(),
                    )
                })?;

                if byte[0] <= MAX_UNBIASED {
                    let idx = (byte[0] as usize) % CHARSET_LEN;
                    key.push(CHARSET[idx] as char);
                    break;
                }
            }
        }

        Ok(format!("rpc_{key}"))
    }

    /// Validates that an API key has the correct format.
    ///
    /// # Security
    ///
    /// Always validate format before performing expensive operations like
    /// blind index computation or Argon2id verification. This prevents
    /// timing attacks and `DoS` via malformed keys.
    #[must_use]
    pub fn is_valid_format(key: &str) -> bool {
        const PREFIX: &str = "rpc_";
        const TOTAL_LENGTH: usize = 36;

        if key.len() != TOTAL_LENGTH {
            return false;
        }

        if !key.starts_with(PREFIX) {
            return false;
        }

        key[PREFIX.len()..].chars().all(|c| c.is_ascii_alphanumeric())
    }

    /// Computes the blind index for an API key (SHA-256 hash, hex-encoded).
    ///
    /// The blind index enables O(1) database lookups without timing attacks.
    ///
    /// # Security Properties
    ///
    /// While SHA-256 is not suitable for password hashing (too fast for brute force),
    /// it's appropriate here because:
    /// 1. API keys are high-entropy random strings (256+ bits), not human passwords
    /// 2. Brute forcing 62^32 possibilities is computationally infeasible
    /// 3. The actual security comes from Argon2id verification after lookup
    #[must_use]
    pub fn compute_blind_index(key: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        let result = hasher.finalize();
        hex::encode(result)
    }

    /// Hashes an API key using Argon2id for secure storage.
    ///
    /// Uses memory-hard hashing with recommended OWASP parameters:
    /// - Memory: 64 MB (m=65536)
    /// - Iterations: 3 (t=3)
    /// - Parallelism: 4 (p=4)
    /// - Output: 32 bytes
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::KeyGenerationError`] if hashing fails.
    pub fn hash_key(key: &str) -> Result<String, AuthError> {
        let params = Params::new(65536, 3, 4, Some(32)).map_err(|e| {
            AuthError::KeyGenerationError(format!("Failed to create Argon2 params: {e}"))
        })?;

        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

        let rng = SystemRandom::new();
        let mut salt_bytes = [0u8; 16];
        rng.fill(&mut salt_bytes)
            .map_err(|_| AuthError::KeyGenerationError("Failed to generate salt".to_string()))?;

        let salt_b64 = base64_encode_salt(&salt_bytes);
        let salt = SaltString::from_b64(&salt_b64).map_err(|e| {
            AuthError::KeyGenerationError(format!("Failed to create salt string: {e}"))
        })?;

        let hash = argon2
            .hash_password(key.as_bytes(), &salt)
            .map_err(|e| AuthError::KeyGenerationError(format!("Failed to hash API key: {e}")))?;

        Ok(hash.to_string())
    }

    /// Verifies an API key against a stored Argon2id hash.
    #[must_use]
    pub fn verify_key(key: &str, hash: &str) -> bool {
        let Ok(parsed_hash) = PasswordHash::new(hash) else {
            return false;
        };

        Argon2::default().verify_password(key.as_bytes(), &parsed_hash).is_ok()
    }

    #[must_use]
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            Utc::now() > expires_at
        } else {
            false
        }
    }

    #[must_use]
    pub fn needs_quota_reset(&self) -> bool {
        Utc::now() > self.quota_reset_at
    }

    #[must_use]
    pub fn is_within_quota(&self) -> bool {
        if let Some(limit) = self.daily_request_limit {
            self.daily_requests_used < limit
        } else {
            true
        }
    }
}

/// Per-method permission and quota tracking for an API key.
///
/// Allows fine-grained control over which RPC methods a key can access
/// and enforces per-method daily request limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodPermission {
    /// Unique database identifier
    pub id: i64,
    /// Foreign key reference to the associated [`ApiKey`]
    pub api_key_id: i64,
    /// RPC method name (e.g., `eth_blockNumber`, `eth_getLogs`)
    pub method_name: String,
    /// Maximum requests allowed per day for this method (None = unlimited)
    pub max_requests_per_day: Option<i64>,
    /// Current count of requests made today for this method
    pub requests_today: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::collections::HashSet;

    #[test]
    fn test_generate_key_format() {
        let key = ApiKey::generate().expect("Key generation should succeed");

        assert!(key.starts_with("rpc_"), "Key should start with 'rpc_' prefix");
        assert_eq!(key.len(), 36, "Key should be 36 characters total");

        let random_part = &key[4..];
        assert!(
            random_part.chars().all(|c| c.is_ascii_alphanumeric()),
            "Random portion should only contain alphanumeric characters"
        );
    }

    #[test]
    fn test_generate_key_uniqueness() {
        let mut keys = HashSet::new();
        let num_keys = 100;

        for _ in 0..num_keys {
            let key = ApiKey::generate().expect("Key generation should succeed");
            keys.insert(key);
        }

        assert_eq!(keys.len(), num_keys, "All generated keys should be unique");
    }

    #[test]
    fn test_hash_key_verifiable() {
        let key = "rpc_testkey12345678901234567890ab";

        let hash = ApiKey::hash_key(key).expect("Hashing should succeed");

        assert!(ApiKey::verify_key(key, &hash), "Key should verify against its hash");
    }

    #[test]
    fn test_hash_key_different_inputs() {
        let key1 = "rpc_testkey12345678901234567890ab";
        let key2 = "rpc_testkey12345678901234567890cd";

        let hash1 = ApiKey::hash_key(key1).expect("Hashing should succeed");
        let hash2 = ApiKey::hash_key(key2).expect("Hashing should succeed");

        assert!(!ApiKey::verify_key(key1, &hash2), "Key1 should not verify against key2's hash");
        assert!(!ApiKey::verify_key(key2, &hash1), "Key2 should not verify against key1's hash");
    }

    #[test]
    fn test_hash_key_format() {
        let key = "rpc_testkey12345678901234567890ab";
        let hash = ApiKey::hash_key(key).expect("Hashing should succeed");

        assert!(hash.starts_with("$argon2id$"), "Hash should be in Argon2id PHC format");
        assert!(hash.contains("m=65536"), "Hash should use 64MB memory (m=65536)");
        assert!(hash.contains("t=3"), "Hash should use 3 iterations (t=3)");
        assert!(hash.contains("p=4"), "Hash should use parallelism 4 (p=4)");
    }

    #[test]
    fn test_hash_key_unique_salts() {
        let key = "rpc_testkey12345678901234567890ab";

        let hash1 = ApiKey::hash_key(key).expect("Hashing should succeed");
        let hash2 = ApiKey::hash_key(key).expect("Hashing should succeed");

        assert_ne!(hash1, hash2, "Same key should produce different hashes due to unique salts");

        assert!(ApiKey::verify_key(key, &hash1), "Key should verify against first hash");
        assert!(ApiKey::verify_key(key, &hash2), "Key should verify against second hash");
    }

    #[test]
    fn test_verify_key_invalid_hash() {
        let key = "rpc_testkey12345678901234567890ab";

        assert!(!ApiKey::verify_key(key, "invalid_hash"));
        assert!(!ApiKey::verify_key(key, ""));
        assert!(!ApiKey::verify_key(key, "$argon2id$invalid"));
    }

    #[test]
    fn test_is_expired() {
        let now = Utc::now();

        let key_no_expiry = ApiKey {
            id: 1,
            key_hash: "hash".to_string(),
            blind_index: "blind".to_string(),
            name: "test".to_string(),
            description: None,
            rate_limit_max_tokens: 100,
            rate_limit_refill_rate: 10,
            daily_request_limit: None,
            daily_requests_used: 0,
            quota_reset_at: now,
            created_at: now,
            updated_at: now,
            last_used_at: None,
            is_active: true,
            expires_at: None,
            scope: Default::default(),
        };
        assert!(!key_no_expiry.is_expired(), "Key without expiry should not be expired");

        let key_future_expiry =
            ApiKey { expires_at: Some(now + Duration::hours(1)), ..key_no_expiry.clone() };
        assert!(!key_future_expiry.is_expired(), "Key with future expiry should not be expired");

        let key_past_expiry =
            ApiKey { expires_at: Some(now - Duration::hours(1)), ..key_no_expiry.clone() };
        assert!(key_past_expiry.is_expired(), "Key with past expiry should be expired");
    }

    #[test]
    fn test_needs_quota_reset() {
        let now = Utc::now();

        let key_needs_reset = ApiKey {
            id: 1,
            key_hash: "hash".to_string(),
            blind_index: "blind".to_string(),
            name: "test".to_string(),
            description: None,
            rate_limit_max_tokens: 100,
            rate_limit_refill_rate: 10,
            daily_request_limit: Some(1000),
            daily_requests_used: 500,
            quota_reset_at: now - Duration::hours(1),
            created_at: now,
            updated_at: now,
            last_used_at: None,
            is_active: true,
            expires_at: None,
            scope: Default::default(),
        };
        assert!(key_needs_reset.needs_quota_reset(), "Key with past reset time should need reset");

        let key_no_reset =
            ApiKey { quota_reset_at: now + Duration::hours(1), ..key_needs_reset.clone() };
        assert!(
            !key_no_reset.needs_quota_reset(),
            "Key with future reset time should not need reset"
        );
    }

    #[test]
    fn test_is_within_quota() {
        let now = Utc::now();

        let key_no_limit = ApiKey {
            id: 1,
            key_hash: "hash".to_string(),
            blind_index: "blind".to_string(),
            name: "test".to_string(),
            description: None,
            rate_limit_max_tokens: 100,
            rate_limit_refill_rate: 10,
            daily_request_limit: None,
            daily_requests_used: 1_000_000,
            quota_reset_at: now + Duration::hours(24),
            created_at: now,
            updated_at: now,
            last_used_at: None,
            is_active: true,
            expires_at: None,
            scope: Default::default(),
        };
        assert!(key_no_limit.is_within_quota(), "Key without limit should always be within quota");

        let key_under_quota = ApiKey {
            daily_request_limit: Some(1000),
            daily_requests_used: 500,
            ..key_no_limit.clone()
        };
        assert!(key_under_quota.is_within_quota(), "Key under limit should be within quota");

        let key_at_limit = ApiKey {
            daily_request_limit: Some(1000),
            daily_requests_used: 1000,
            ..key_no_limit.clone()
        };
        assert!(!key_at_limit.is_within_quota(), "Key at limit should not be within quota");

        let key_over_quota = ApiKey {
            daily_request_limit: Some(1000),
            daily_requests_used: 1500,
            ..key_no_limit.clone()
        };
        assert!(!key_over_quota.is_within_quota(), "Key over limit should not be within quota");
    }

    #[test]
    fn test_key_generation_and_hashing_roundtrip() {
        let key = ApiKey::generate().expect("Key generation should succeed");
        let hash = ApiKey::hash_key(&key).expect("Hashing should succeed");

        assert!(ApiKey::verify_key(&key, &hash), "Generated key should verify against its hash");
        assert!(hash.starts_with("$argon2id$"), "Hash should be in Argon2id PHC format");
    }

    #[test]
    fn test_is_valid_format_valid_keys() {
        let key = ApiKey::generate().expect("Key generation should succeed");
        assert!(ApiKey::is_valid_format(&key), "Generated key should have valid format");

        let valid_key = "rpc_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef";
        assert_eq!(valid_key.len(), 36);
        assert!(ApiKey::is_valid_format(valid_key), "Valid key should pass format check");

        let lowercase = "rpc_abcdefghijklmnopqrstuvwxyz012345";
        assert_eq!(lowercase.len(), 36);
        assert!(ApiKey::is_valid_format(lowercase), "Lowercase alphanumeric should be valid");

        let uppercase = "rpc_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";
        assert_eq!(uppercase.len(), 36);
        assert!(ApiKey::is_valid_format(uppercase), "Uppercase alphanumeric should be valid");

        let mixed = "rpc_aB1cD2eF3gH4iJ5kL6mN7oP8qR9sTuVw";
        assert_eq!(mixed.len(), 36);
        assert!(ApiKey::is_valid_format(mixed), "Mixed alphanumeric should be valid");
    }

    #[test]
    fn test_is_valid_format_invalid_keys() {
        assert!(!ApiKey::is_valid_format("abc_testkey12345678901234567890ab"));
        assert!(!ApiKey::is_valid_format("RPC_testkey12345678901234567890ab"));
        assert!(!ApiKey::is_valid_format("testkey12345678901234567890abcdef"));

        assert!(!ApiKey::is_valid_format("rpc_short"));
        assert!(!ApiKey::is_valid_format("rpc_"));
        assert!(!ApiKey::is_valid_format(""));

        assert!(!ApiKey::is_valid_format("rpc_testkey12345678901234567890abcdef"));

        assert!(!ApiKey::is_valid_format("rpc_testkey123456789012345678!@#$"));
        assert!(!ApiKey::is_valid_format("rpc_testkey12345678901234567890_-"));
        assert!(!ApiKey::is_valid_format("rpc_testkey1234567890123456789 ab"));

        assert!(!ApiKey::is_valid_format("rpc_testkey12345678901234567890αβ"));
    }
}
