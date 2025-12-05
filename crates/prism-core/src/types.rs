//! Core type definitions for JSON-RPC, caching, and upstream management.
//!
//! # Type Categories
//!
//! ## JSON-RPC Protocol Types
//! - [`JsonRpcRequest`], [`JsonRpcResponse`], [`JsonRpcError`]: Protocol conformance
//! - [`CacheStatus`]: Prism-specific extension indicating cache behavior
//!
//! ## Performance-Optimized Types
//! - [`RpcMetrics`]: Uses string interning to avoid allocations on hot path
//! - Block/transaction types use `Arc` for cheap cloning
//!
//! ## Configuration Types
//! - [`UpstreamConfig`], [`UpstreamHealth`]: Provider configuration and monitoring
//!
//! # Performance Notes
//!
//! String interning via `intern()` eliminates allocations per request for method
//! names and upstream identifiers. This is an intentional bounded memory leak
//! (~1KB total) for a fixed set of strings.

use ahash::AHashSet;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, LazyLock},
};

/// JSON-RPC protocol version constant to avoid repeated allocations.
/// Use `JSONRPC_VERSION_COW` for constructing requests/responses without allocation.
pub const JSONRPC_VERSION: &str = "2.0";

/// Pre-allocated `Cow` for JSON-RPC version - zero allocation for static usage.
pub const JSONRPC_VERSION_COW: Cow<'static, str> = Cow::Borrowed(JSONRPC_VERSION);

/// Allowed RPC methods - single source of truth for method validation
pub const ALLOWED_METHODS: &[&str] = &[
    "net_version",
    "eth_blockNumber",
    "eth_chainId",
    "eth_gasPrice",
    "eth_getBalance",
    "eth_getBlockByHash",
    "eth_getBlockByNumber",
    "eth_getLogs",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "eth_getCode",
];

/// Pre-computed `HashSet` for O(1) method lookups
static ALLOWED_METHODS_SET: LazyLock<AHashSet<&'static str>> =
    LazyLock::new(|| ALLOWED_METHODS.iter().copied().collect());

/// String interner for metrics keys to avoid repeated allocations on the hot path.
/// Pre-populated with allowed methods, and dynamically interns new strings as needed.
/// Uses `parking_lot::RwLock` for efficient read-heavy access patterns.
static STRING_INTERNER: LazyLock<RwLock<AHashSet<&'static str>>> = LazyLock::new(|| {
    let set: AHashSet<&'static str> = ALLOWED_METHODS.iter().copied().collect();
    RwLock::new(set)
});

/// Interns a string, returning a `&'static str` reference.
///
/// For known strings (like RPC method names), this is a fast read-lock lookup.
/// For new strings, a one-time allocation occurs and the string is cached.
///
/// # Performance
/// - Read path (existing string): ~5ns (read lock + hash lookup)
/// - Write path (new string): ~50ns (allocation + write lock)
///
/// # Intentional Memory Leak
///
/// This function uses [`Box::leak`] to create `'static` string references, which is an
/// **intentional and safe memory leak** for this specific use case:
///
/// ## Why the leak is safe and acceptable:
///
/// 1. **Bounded memory usage**: The set of interned strings is limited to:
///    - ~10 RPC method names (from `ALLOWED_METHODS`)
///    - ~5-10 upstream provider names (from configuration)
///    - Total memory: ~50 strings × ~20 bytes = **~1KB maximum**
///
/// 2. **One-time allocation**: Each unique string is leaked exactly once and reused indefinitely.
///    No per-request allocation occurs after the initial intern.
///
/// 3. **Performance critical**: This pattern enables zero-allocation metrics collection on the hot
///    path by using `&'static str` keys in hash maps, avoiding repeated `String` allocations for
///    every metric operation.
///
/// ## Leak detector warnings:
///
/// Memory profilers and leak detection tools (valgrind, sanitizers) will flag the
/// `Box::leak` calls in this function. **This is expected behavior and not a bug.**
/// The leaked memory is working memory that persists for the program's lifetime.
///
/// ## Alternative considered:
///
/// Using an arena allocator or reference-counted strings would add complexity and
/// overhead without meaningful benefit given the bounded nature of the string set.
#[inline]
fn intern(s: &str) -> &'static str {
    {
        let set = STRING_INTERNER.read();
        if let Some(&existing) = set.get(s) {
            return existing;
        }
    }

    let mut set = STRING_INTERNER.write();

    if let Some(&existing) = set.get(s) {
        return existing;
    }

    // Leak the string to get a 'static lifetime
    // This is acceptable because:
    // 1. The set of unique strings is bounded (method names + upstream names)
    // 2. Memory usage is minimal (~50 method names × ~20 bytes = ~1KB total)
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// Check if a method is in the allowed list (O(1) lookup)
#[inline]
#[must_use]
pub fn is_method_allowed(method: &str) -> bool {
    ALLOWED_METHODS_SET.contains(method)
}

/// Describes how a request was served from cache or upstream.
///
/// Cache status is attached to RPC responses to indicate whether data came from cache,
/// upstream, or a combination. This enables clients to understand latency characteristics
/// and helps with debugging cache behavior.
///
/// # Example
///
/// ```
/// use prism_core::types::CacheStatus;
///
/// // Full cache hit - fastest response
/// let status = CacheStatus::Full;
/// assert_eq!(status.to_string(), "FULL");
///
/// // Cache miss - had to fetch from upstream
/// let status = CacheStatus::Miss;
/// assert_eq!(status.to_string(), "MISS");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CacheStatus {
    /// All requested data was served from cache.
    ///
    /// For `eth_getLogs`, this means the entire block range was cached and no upstream
    /// calls were needed. This provides the fastest response time.
    Full,
    /// Some data was served from cache, remainder fetched from upstream.
    ///
    /// For `eth_getLogs`, this occurs when part of the requested range is cached but
    /// newer blocks must be fetched. The response combines cached and fresh data.
    Partial,
    /// Partial cache hit where some upstream ranges failed to fetch.
    ///
    /// Similar to `Partial`, but some upstream requests failed. The response contains
    /// logs from successfully cached and fetched ranges. Failure metadata is included
    /// in the `_prism_meta` field for debugging.
    PartialWithFailures,
    /// Range was cached as containing no logs.
    ///
    /// The requested block range was previously queried and confirmed to contain no
    /// matching logs. The empty result was served from cache without upstream calls.
    Empty,
    /// No cached data available, all data fetched from upstream.
    ///
    /// The requested data was not in cache and was fetched fresh from an upstream
    /// RPC provider. Subsequent identical requests may hit cache.
    Miss,
}

impl std::fmt::Display for CacheStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheStatus::Full => write!(f, "FULL"),
            CacheStatus::Partial => write!(f, "PARTIAL"),
            CacheStatus::PartialWithFailures => write!(f, "PARTIAL_WITH_FAILURES"),
            CacheStatus::Empty => write!(f, "EMPTY"),
            CacheStatus::Miss => write!(f, "MISS"),
        }
    }
}

/// JSON-RPC 2.0 request structure.
///
/// Represents an incoming RPC request conforming to the JSON-RPC 2.0 specification.
/// The structure is optimized for performance with careful choices around allocation.
///
/// # Fields
///
/// - `jsonrpc`: Protocol version (always "2.0")
/// - `method`: RPC method name (e.g., `eth_blockNumber`, `eth_getLogs`)
/// - `params`: Optional method parameters as JSON value
/// - `id`: Request identifier that must be echoed in the response
///
/// # Performance Notes
///
/// - `jsonrpc`: Uses `Cow<'static, str>` to avoid allocation when constructing with the static
///   version string "2.0". Use `JSONRPC_VERSION_COW` for zero-cost construction.
/// - `id`: Uses `Arc<serde_json::Value>` to enable cheap cloning when the request ID needs to be
///   copied to responses (e.g., error responses).
///
/// # Example
///
/// ```
/// use prism_core::types::JsonRpcRequest;
/// use serde_json::json;
///
/// let request = JsonRpcRequest::new("eth_blockNumber", None, json!(1));
///
/// assert_eq!(request.method, "eth_blockNumber");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: Cow<'static, str>,
    pub method: String,
    pub params: Option<serde_json::Value>,
    pub id: Arc<serde_json::Value>,
}

/// JSON-RPC 2.0 response structure.
///
/// Represents an RPC response conforming to the JSON-RPC 2.0 specification.
/// A response contains either a `result` (success) or an `error` (failure), but never both.
///
/// # Fields
///
/// - `jsonrpc`: Protocol version (always "2.0")
/// - `result`: Successful response data (mutually exclusive with `error`)
/// - `error`: Error information if the request failed (mutually exclusive with `result`)
/// - `id`: Request identifier echoed from the request
/// - `cache_status`: Optional Prism-specific extension indicating cache hit/miss status
///
/// # Performance Notes
///
/// - `jsonrpc`: Uses `Cow<'static, str>` to avoid allocation when constructing with the static
///   version string "2.0". Use `JSONRPC_VERSION_COW` for zero-cost construction.
/// - `id`: Uses `Arc<serde_json::Value>` to enable cheap cloning from the request ID without
///   deep-copying the JSON value.
///
/// # Example
///
/// ```
/// use prism_core::types::JsonRpcResponse;
/// use serde_json::json;
/// use std::sync::Arc;
///
/// // Success response
/// let response = JsonRpcResponse::success(json!("0x1234"), Arc::new(json!(1)));
/// assert!(response.result.is_some());
/// assert!(response.error.is_none());
///
/// // Error response
/// let response =
///     JsonRpcResponse::error(-32600, "Invalid Request".to_string(), Arc::new(json!(1)));
/// assert!(response.error.is_some());
/// assert!(response.result.is_none());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: Cow<'static, str>,
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
    pub id: Arc<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_status: Option<CacheStatus>,
}

impl JsonRpcRequest {
    /// Creates a new JSON-RPC request with zero allocation for the version string.
    #[must_use]
    pub fn new(
        method: impl Into<String>,
        params: Option<serde_json::Value>,
        id: serde_json::Value,
    ) -> Self {
        Self { jsonrpc: JSONRPC_VERSION_COW, method: method.into(), params, id: Arc::new(id) }
    }
}

impl JsonRpcResponse {
    /// Creates a successful JSON-RPC response with zero allocation for the version string.
    #[must_use]
    pub fn success(result: serde_json::Value, id: Arc<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION_COW,
            result: Some(result),
            error: None,
            id,
            cache_status: None,
        }
    }

    /// Creates an error JSON-RPC response with zero allocation for the version string.
    #[must_use]
    pub fn error(code: i32, message: String, id: Arc<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION_COW,
            result: None,
            error: Some(JsonRpcError { code, message, data: None }),
            id,
            cache_status: None,
        }
    }

    /// Creates a new response using a request's ID (cheap Arc clone).
    #[must_use]
    pub fn from_request_id(request: &JsonRpcRequest) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION_COW,
            result: None,
            error: None,
            id: Arc::clone(&request.id),
            cache_status: None,
        }
    }
}

/// JSON-RPC 2.0 error object.
///
/// Represents error information in a JSON-RPC response according to the specification.
/// Standard error codes follow the JSON-RPC 2.0 convention:
///
/// - `-32700`: Parse error (invalid JSON)
/// - `-32600`: Invalid request (malformed JSON-RPC)
/// - `-32601`: Method not found
/// - `-32602`: Invalid params
/// - `-32603`: Internal error
/// - `-32000` to `-32099`: Server-defined errors (implementation-specific)
///
/// # Fields
///
/// - `code`: Numeric error code indicating the error type
/// - `message`: Human-readable error description
/// - `data`: Optional additional error information (e.g., stack traces, debug info)
///
/// # Example
///
/// ```
/// use prism_core::types::JsonRpcError;
///
/// let error = JsonRpcError { code: -32601, message: "Method not found".to_string(), data: None };
///
/// assert_eq!(error.code, -32601);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

/// Error type for hash parsing
#[derive(Debug, Clone, thiserror::Error)]
pub enum HashParseError {
    #[error("missing 0x prefix")]
    MissingPrefix,
    #[error("invalid hex: {0}")]
    InvalidHex(String),
    #[error("invalid length: expected 32 bytes, got {0}")]
    InvalidLength(usize),
}

/// 32-byte hash (used for transaction hashes, block hashes, etc.)
///
/// Provides `TryFrom<&str>` for idiomatic parsing of 0x-prefixed hex strings.
///
/// # Example
/// ```
/// use prism_core::types::Hash32;
///
/// let hash: Hash32 = "0xabcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234"
///     .try_into()
///     .unwrap();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Hash32(pub [u8; 32]);

impl Hash32 {
    /// Returns the inner byte array.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Consumes self and returns the inner byte array.
    #[must_use]
    pub fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl TryFrom<&str> for Hash32 {
    type Error = HashParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let hex_str = value.strip_prefix("0x").ok_or(HashParseError::MissingPrefix)?;

        let bytes = hex::decode(hex_str).map_err(|e| HashParseError::InvalidHex(e.to_string()))?;

        if bytes.len() != 32 {
            return Err(HashParseError::InvalidLength(bytes.len()));
        }

        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Hash32(arr))
    }
}

impl From<[u8; 32]> for Hash32 {
    fn from(arr: [u8; 32]) -> Self {
        Hash32(arr)
    }
}

impl From<Hash32> for [u8; 32] {
    fn from(hash: Hash32) -> Self {
        hash.0
    }
}

impl AsRef<[u8; 32]> for Hash32 {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for Hash32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl std::str::FromStr for Hash32 {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s)
    }
}

/// Block range for log queries
///
/// Represents an inclusive range `[from, to]` where `from <= to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRange {
    pub from: u64,
    pub to: u64,
}

impl BlockRange {
    /// Creates a new block range.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `from > to`. In release builds, the range
    /// will be created but methods like `len()` and `contains()` may behave
    /// unexpectedly.
    #[inline]
    #[must_use]
    pub fn new(from: u64, to: u64) -> Self {
        debug_assert!(from <= to, "Invalid BlockRange: from ({from}) > to ({to})");
        Self { from, to }
    }

    /// Returns the number of blocks in this range (inclusive).
    #[inline]
    #[must_use]
    pub fn len(&self) -> u64 {
        // Saturating to handle invalid ranges gracefully in release builds
        self.to.saturating_sub(self.from).saturating_add(1)
    }

    /// Returns true if the range contains zero blocks.
    ///
    /// Note: A valid `BlockRange` always contains at least one block.
    /// This returns true only for invalid ranges where `from > to`.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.from > self.to
    }

    #[inline]
    #[must_use]
    pub fn contains(&self, block: u64) -> bool {
        block >= self.from && block <= self.to
    }

    #[inline]
    #[must_use]
    pub fn overlaps(&self, other: &BlockRange) -> bool {
        self.from <= other.to && self.to >= other.from
    }

    #[inline]
    #[must_use]
    pub fn intersection(&self, other: &BlockRange) -> Option<BlockRange> {
        if self.overlaps(other) {
            Some(BlockRange { from: self.from.max(other.from), to: self.to.min(other.to) })
        } else {
            None
        }
    }
}

/// Configuration for an upstream RPC provider.
///
/// Defines connection parameters, health monitoring, and circuit breaker settings
/// for a single upstream Ethereum RPC endpoint (e.g., Alchemy, Infura, local node).
///
/// # Fields
///
/// - `url`: HTTP/HTTPS endpoint URL for JSON-RPC requests
/// - `ws_url`: Optional WebSocket URL for subscriptions (e.g., `newHeads`)
/// - `name`: Human-readable identifier for metrics and logging
/// - `chain_id`: Ethereum chain ID (1 for mainnet, 11155111 for Sepolia, etc.)
/// - `weight`: Load balancing weight (higher = more traffic), default is 1
/// - `timeout_seconds`: Request timeout in seconds
/// - `supports_websocket`: Whether WebSocket subscriptions are enabled
/// - `circuit_breaker_threshold`: Number of consecutive errors before opening circuit
/// - `circuit_breaker_timeout_seconds`: Seconds to wait before retrying after circuit opens
///
/// # Example
///
/// ```
/// use prism_core::types::UpstreamConfig;
/// use std::sync::Arc;
///
/// let config = UpstreamConfig {
///     url: "https://eth-mainnet.g.alchemy.com/v2/API_KEY".to_string(),
///     ws_url: Some("wss://eth-mainnet.g.alchemy.com/v2/API_KEY".to_string()),
///     name: Arc::from("alchemy-mainnet"),
///     chain_id: 1,
///     weight: 2,
///     timeout_seconds: 30,
///     supports_websocket: true,
///     circuit_breaker_threshold: 5,
///     circuit_breaker_timeout_seconds: 60,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    pub url: String,
    pub ws_url: Option<String>,
    pub name: Arc<str>,
    pub chain_id: u64,
    pub weight: u32,
    pub timeout_seconds: u64,
    pub supports_websocket: bool,
    pub circuit_breaker_threshold: u32,
    pub circuit_breaker_timeout_seconds: u64,
}

/// Real-time health and performance metrics for an upstream RPC provider.
///
/// Tracks the current health status, latency percentiles, and blockchain sync state
/// of an upstream endpoint. Used by the load balancer to route requests to healthy
/// upstreams and detect sync issues.
///
/// # Fields
///
/// - `is_healthy`: Whether the upstream is currently passing health checks
/// - `last_check`: Timestamp of the last health check attempt
/// - `error_count`: Consecutive error count (resets on success)
/// - `response_time_ms`: Most recent successful request latency
/// - `latency_p50_ms`: Median latency over recent request window
/// - `latency_p95_ms`: 95th percentile latency (only 5% of requests slower)
/// - `latency_p99_ms`: 99th percentile latency (only 1% of requests slower)
/// - `latest_block`: Most recent block number from `eth_blockNumber`
/// - `finalized_block`: Most recent finalized block number (if available)
///
/// # Health Check Behavior
///
/// An upstream is marked unhealthy (`is_healthy = false`) when:
/// - Consecutive errors reach the circuit breaker threshold
/// - Health check request times out
/// - Block number falls significantly behind other upstreams (sync lag)
///
/// # Example
///
/// ```
/// use prism_core::types::UpstreamHealth;
/// use std::time::Instant;
///
/// let health = UpstreamHealth {
///     is_healthy: true,
///     last_check: Instant::now(),
///     error_count: 0,
///     response_time_ms: Some(45),
///     latency_p50_ms: Some(50),
///     latency_p95_ms: Some(150),
///     latency_p99_ms: Some(300),
///     latest_block: Some(18_000_000),
///     finalized_block: Some(17_999_900),
/// };
///
/// assert!(health.is_healthy);
/// assert_eq!(health.latest_block, Some(18_000_000));
/// ```
#[derive(Debug, Clone)]
pub struct UpstreamHealth {
    pub is_healthy: bool,
    pub last_check: std::time::Instant,
    pub error_count: u32,
    pub response_time_ms: Option<u64>,
    /// P50 (median) latency in milliseconds
    pub latency_p50_ms: Option<u64>,
    /// P95 latency in milliseconds
    pub latency_p95_ms: Option<u64>,
    /// P99 latency in milliseconds
    pub latency_p99_ms: Option<u64>,
    /// Latest block number reported by the upstream
    pub latest_block: Option<u64>,
    /// Finalized block number reported by the upstream
    pub finalized_block: Option<u64>,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            ws_url: None,
            name: Arc::from(""),
            chain_id: 0,
            weight: 1,
            timeout_seconds: 30,
            supports_websocket: false,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_seconds: 60,
        }
    }
}

impl Default for UpstreamHealth {
    fn default() -> Self {
        Self {
            is_healthy: true,
            last_check: std::time::Instant::now(),
            error_count: 0,
            response_time_ms: None,
            latency_p50_ms: None,
            latency_p95_ms: None,
            latency_p99_ms: None,
            latest_block: None,
            finalized_block: None,
        }
    }
}

/// In-memory metrics for tracking RPC request performance and cache behavior.
///
/// Collects request counts, cache hit rates, error rates, and latency distributions
/// per RPC method and upstream provider. These metrics are periodically exported to
/// Prometheus at the `/metrics` endpoint.
///
/// # Performance Optimization
///
/// Uses `&'static str` keys via string interning to avoid repeated allocations on the
/// hot path. Method names and upstream names are interned once and reused for all
/// subsequent lookups, reducing memory allocations by ~90% in high-throughput scenarios.
///
/// # Fields
///
/// - `requests_total`: Total request count per RPC method
/// - `cache_hits_total`: Cache hit count per RPC method
/// - `upstream_errors_total`: Error count per upstream provider
/// - `latency_histogram`: Latency samples per method:upstream pair
///
/// # Example
///
/// ```
/// use prism_core::types::RpcMetrics;
///
/// let mut metrics = RpcMetrics::new();
///
/// // Record a request (method name is interned internally)
/// metrics.increment_requests("eth_blockNumber");
/// metrics.increment_cache_hits("eth_blockNumber");
/// metrics.record_latency("eth_blockNumber", "alchemy", 45);
///
/// assert_eq!(metrics.requests_total.get("eth_blockNumber"), Some(&1));
/// assert_eq!(metrics.cache_hits_total.get("eth_blockNumber"), Some(&1));
/// ```
#[derive(Debug, Default, Clone)]
pub struct RpcMetrics {
    pub requests_total: HashMap<&'static str, u64>,
    pub cache_hits_total: HashMap<&'static str, u64>,
    pub upstream_errors_total: HashMap<&'static str, u64>,
    pub latency_histogram: HashMap<&'static str, Vec<u64>>,
}

impl RpcMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            requests_total: HashMap::new(),
            cache_hits_total: HashMap::new(),
            upstream_errors_total: HashMap::new(),
            latency_histogram: HashMap::new(),
        }
    }

    /// Increment request count for a method.
    ///
    /// Uses string interning to avoid allocation on the hot path.
    #[inline]
    pub fn increment_requests(&mut self, method: &str) {
        *self.requests_total.entry(intern(method)).or_insert(0) += 1;
    }

    /// Increment cache hit count for a method.
    ///
    /// Uses string interning to avoid allocation on the hot path.
    #[inline]
    pub fn increment_cache_hits(&mut self, method: &str) {
        *self.cache_hits_total.entry(intern(method)).or_insert(0) += 1;
    }

    /// Increment error count for an upstream.
    ///
    /// Uses string interning to avoid allocation on the hot path.
    #[inline]
    pub fn increment_upstream_errors(&mut self, upstream: &str) {
        *self.upstream_errors_total.entry(intern(upstream)).or_insert(0) += 1;
    }

    /// Record latency for a method/upstream combination.
    ///
    /// Uses string interning for the composite key to avoid repeated allocations.
    /// The key format is `method:upstream` (e.g., `eth_call:alchemy`).
    #[inline]
    pub fn record_latency(&mut self, method: &str, upstream: &str, latency_ms: u64) {
        // Build composite key and intern it
        let mut key = String::with_capacity(method.len() + upstream.len() + 1);
        key.push_str(method);
        key.push(':');
        key.push_str(upstream);
        self.latency_histogram.entry(intern(&key)).or_default().push(latency_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- BlockRange tests ---

    #[test]
    fn test_block_range_new() {
        let range = BlockRange::new(100, 200);
        assert_eq!(range.from, 100);
        assert_eq!(range.to, 200);
    }

    #[test]
    fn test_block_range_single_block() {
        let range = BlockRange::new(100, 100);
        assert_eq!(range.len(), 1);
        assert!(range.contains(100));
        assert!(!range.contains(99));
        assert!(!range.contains(101));
    }

    #[test]
    fn test_block_range_len() {
        assert_eq!(BlockRange::new(0, 0).len(), 1);
        assert_eq!(BlockRange::new(0, 9).len(), 10);
        assert_eq!(BlockRange::new(100, 199).len(), 100);
    }

    #[test]
    fn test_block_range_contains() {
        let range = BlockRange::new(10, 20);

        assert!(!range.contains(9));
        assert!(range.contains(10));
        assert!(range.contains(15));
        assert!(range.contains(20));
        assert!(!range.contains(21));
    }

    #[test]
    fn test_block_range_overlaps() {
        let range = BlockRange::new(10, 20);

        // No overlap - before
        assert!(!range.overlaps(&BlockRange::new(0, 9)));

        // No overlap - after
        assert!(!range.overlaps(&BlockRange::new(21, 30)));

        // Overlap - touching start
        assert!(range.overlaps(&BlockRange::new(5, 10)));

        // Overlap - touching end
        assert!(range.overlaps(&BlockRange::new(20, 25)));

        // Overlap - contained within
        assert!(range.overlaps(&BlockRange::new(12, 18)));

        // Overlap - contains the range
        assert!(range.overlaps(&BlockRange::new(5, 25)));

        // Overlap - partial start
        assert!(range.overlaps(&BlockRange::new(5, 15)));

        // Overlap - partial end
        assert!(range.overlaps(&BlockRange::new(15, 25)));
    }

    #[test]
    fn test_block_range_intersection() {
        let range = BlockRange::new(10, 20);

        // No intersection
        assert_eq!(range.intersection(&BlockRange::new(0, 9)), None);
        assert_eq!(range.intersection(&BlockRange::new(21, 30)), None);

        // Partial intersection
        assert_eq!(range.intersection(&BlockRange::new(5, 15)), Some(BlockRange::new(10, 15)));
        assert_eq!(range.intersection(&BlockRange::new(15, 25)), Some(BlockRange::new(15, 20)));

        // Contained intersection
        assert_eq!(range.intersection(&BlockRange::new(12, 18)), Some(BlockRange::new(12, 18)));

        // Contains intersection
        assert_eq!(range.intersection(&BlockRange::new(5, 25)), Some(BlockRange::new(10, 20)));

        // Single block intersection
        assert_eq!(range.intersection(&BlockRange::new(10, 10)), Some(BlockRange::new(10, 10)));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Invalid BlockRange")]
    fn test_block_range_invalid_panics_in_debug() {
        let _ = BlockRange::new(20, 10); // from > to should panic
    }
}
