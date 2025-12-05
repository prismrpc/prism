# Configuration Module Documentation

**Location**: `crates/prism-core/src/config/mod.rs`

## Overview

The configuration module provides a comprehensive, hierarchical configuration system for the Prism RPC aggregator. It manages all application settings including server parameters, upstream RPC providers, caching, health checks, authentication, metrics, and logging. The module supports multiple configuration sources with a clear precedence order: TOML files, environment variables, and programmatic defaults.

The configuration system is designed around the principle of sensible defaults with granular override capabilities, enabling deployment flexibility across development, testing, and production environments.

## Configuration Architecture

### Configuration Hierarchy

```
AppConfig (Root)
├── environment: String
├── server: ServerConfig
│   ├── bind_address
│   ├── bind_port
│   ├── max_concurrent_requests
│   └── request_timeout_seconds
├── upstreams: UpstreamsConfig
│   └── providers: Vec<UpstreamProvider>
│       ├── name
│       ├── chain_id
│       ├── https_url
│       ├── wss_url
│       ├── weight
│       ├── timeout_seconds
│       ├── circuit_breaker_threshold
│       └── circuit_breaker_timeout_seconds
├── cache: CacheConfig
│   ├── enabled
│   ├── cache_ttl_seconds
│   └── manager_config: CacheManagerConfig
│       ├── log_cache
│       ├── block_cache
│       ├── transaction_cache
│       ├── reorg_manager
│       ├── retain_blocks
│       ├── enable_auto_cleanup
│       ├── cleanup_interval_seconds
│       └── cleanup_every_n_blocks
├── health_check: HealthCheckConfig
│   └── interval_seconds
├── auth: AuthConfig
│   ├── enabled
│   └── database_url
├── metrics: MetricsConfig
│   ├── enabled
│   └── prometheus_port
├── logging: LoggingConfig
│   ├── level
│   └── format
├── hedging: HedgeConfig
│   ├── enabled
│   ├── latency_quantile
│   ├── min_delay_ms
│   ├── max_delay_ms
│   └── max_parallel
├── scoring: ScoringConfig
│   ├── enabled
│   ├── window_seconds
│   ├── min_samples
│   ├── max_block_lag
│   ├── top_n
│   └── weights: ScoringWeights
│       ├── latency
│       ├── error_rate
│       ├── throttle_rate
│       ├── block_head_lag
│       └── total_requests
└── consensus: ConsensusConfig
    ├── enabled
    ├── max_count
    ├── min_count
    ├── dispute_behavior
    ├── failure_behavior
    ├── low_participants_behavior
    ├── methods: Vec<String>
    ├── disagreement_penalty
    └── timeout_seconds
```

## Configuration Structs

### AppConfig

**Lines 182-214** (`crates/prism-core/src/config/mod.rs`)

The root configuration container that holds all application settings.

**Fields:**
- `environment: String` - Deployment environment identifier (default: "development")
- `server: ServerConfig` - Server binding and request settings
- `upstreams: UpstreamsConfig` - RPC provider configurations
- `cache: CacheConfig` - Caching system configuration
- `health_check: HealthCheckConfig` - Health check intervals
- `auth: AuthConfig` - Authentication settings
- `metrics: MetricsConfig` - Prometheus metrics configuration
- `logging: LoggingConfig` - Logging verbosity and format
- `hedging: HedgeConfig` - Request hedging configuration for tail latency improvement
- `scoring: ScoringConfig` - Multi-factor scoring configuration for upstream selection
- `consensus: ConsensusConfig` - Consensus and data validation configuration

**Default Implementation (Lines 299-312 in `crates/prism-core/src/config/mod.rs`):**
Creates a fully functional default configuration suitable for development environments with two example upstream providers (Infura and Alchemy).

### ServerConfig

**Lines 7-23** (`crates/prism-core/src/config/mod.rs`)

Server binding configuration and request handling parameters.

**Fields:**
- `bind_address: String` - IP address to bind the server to (default: "127.0.0.1")
- `bind_port: u16` - Port number for HTTP server (default: 3030)
- `max_concurrent_requests: usize` - Maximum simultaneous requests (default: 100)
- `request_timeout_seconds: u64` - Request timeout in seconds (default: 30)

**Default Values (Lines 220-228 in `crates/prism-core/src/config/mod.rs`):**
- `bind_address`: "127.0.0.1" (localhost only)
- `bind_port`: 3030
- `max_concurrent_requests`: 100
- `request_timeout_seconds`: 30 seconds

**Functionality:**
Controls server socket binding and request concurrency limits. The `bind_address` defaults to localhost for security, requiring explicit configuration for external access. The `max_concurrent_requests` parameter provides backpressure protection against overload.

### UpstreamProvider

**Lines 41-73** (`crates/prism-core/src/config/mod.rs`)

Configuration for a single RPC provider endpoint.

**Fields:**
- `name: String` - Human-readable provider identifier (e.g., "infura", "alchemy")
- `chain_id: u64` - Ethereum chain ID (1 for mainnet, 1337 for devnet, etc.)
- `https_url: String` - HTTP(S) endpoint URL for JSON-RPC requests
- `wss_url: Option<String>` - Optional WebSocket URL for subscriptions
- `weight: u32` - Load balancing weight, higher values receive more traffic (default: 1)
- `timeout_seconds: u64` - Request timeout for this provider (default: 30)
- `circuit_breaker_threshold: u32` - Consecutive failures before circuit breaks (default: 2)
- `circuit_breaker_timeout_seconds: u64` - Seconds to wait before retrying (default: 1)

**Default Values (Lines 75-89 in `crates/prism-core/src/config/mod.rs`):**
- `weight`: 1 (equal distribution)
- `timeout_seconds`: 30 seconds
- `circuit_breaker_threshold`: 2 consecutive failures
- `circuit_breaker_timeout_seconds`: 1 second cooldown

**Functionality:**
Each provider represents a distinct RPC endpoint. The `weight` field enables weighted round-robin load balancing. Circuit breaker parameters prevent cascading failures by temporarily disabling unhealthy providers. WebSocket URLs enable chain tip subscriptions for cache invalidation.

### UpstreamsConfig

**Lines 94-98** (`crates/prism-core/src/config/mod.rs`)

Container for all upstream provider configurations.

**Fields:**
- `providers: Vec<UpstreamProvider>` - List of configured RPC providers

**Default Implementation (Lines 231-258 in `crates/prism-core/src/config/mod.rs`):**
Includes two example providers:
1. Infura (mainnet) with placeholder API key
2. Alchemy (mainnet) with placeholder API key

**Functionality:**
Aggregates multiple upstream providers. The proxy engine uses this list for load balancing and failover. At least one provider must be configured; validation enforces this requirement.

### CacheConfig

**Lines 148-159** (`crates/prism-core/src/config/mod.rs`)

Wrapper for cache system configuration.

**Fields:**
- `enabled: bool` - Master switch for caching system (default: true)
- `cache_ttl_seconds: u64` - Default TTL for cached responses (default: 300)
- `manager_config: CacheManagerConfig` - Advanced cache settings (nested struct)

**Default Implementation (Lines 289-297 in `crates/prism-core/src/config/mod.rs`):**
- `enabled`: true
- `cache_ttl_seconds`: 300 (5 minutes)
- `manager_config`: Delegated to `CacheManagerConfig::default()`

**Functionality:**
Controls the entire caching system. When `enabled` is false, all caching is bypassed. The `cache_ttl_seconds` provides a default expiration time for general cached responses. The nested `manager_config` configures specialized caches (block, transaction, log) with their own parameters.

**CacheManagerConfig Reference:**
The nested `manager_config` field (from `crates/prism-core/src/cache/cache_manager.rs`) contains:
- `log_cache: LogCacheConfig` - eth_getLogs partial range fulfillment
- `block_cache: BlockCacheConfig` - Block header/body caching
- `transaction_cache: TransactionCacheConfig` - Transaction/receipt caching
- `reorg_manager: ReorgManagerConfig` - Chain reorganization handling
- `retain_blocks: u64` - Number of recent blocks to retain (default: 1000)
- `enable_auto_cleanup: bool` - Enable background cleanup tasks (default: true)
- `cleanup_interval_seconds: u64` - Cleanup interval (default: 300 seconds)
- `cleanup_every_n_blocks: u64` - Trigger cleanup every N blocks (default: 0, disabled)

**Usage Note:**
When initializing `CacheManager` with this config, you must also provide a shared `Arc<ChainState>` instance:
```rust
use prism_core::cache::{CacheManager, CacheManagerConfig};
use prism_core::chain::ChainState;
use std::sync::Arc;

let chain_state = Arc::new(ChainState::new());
let cache_manager = Arc::new(
    CacheManager::new(&config.cache.manager_config, chain_state.clone())
        .expect("Failed to initialize cache manager")
);
```
See [ChainState documentation](../chain/chain_state.md) for details on the shared ownership pattern.

### HealthCheckConfig

**Lines 103-107** (`crates/prism-core/src/config/mod.rs`)

Health monitoring configuration for upstream providers.

**Fields:**
- `interval_seconds: u64` - Seconds between health checks (default: 60)

**Default Implementation (Lines 260-263 in `crates/prism-core/src/config/mod.rs`):**
- `interval_seconds`: 60 seconds

**Functionality:**
Controls how frequently the system sends `eth_blockNumber` requests to upstream providers to verify their health. More frequent checks (lower values) detect failures faster but increase network overhead. The health check results influence load balancing decisions and circuit breaker states.

### AuthConfig

**Lines 113-120** (`crates/prism-core/src/config/mod.rs`)

API key authentication configuration.

**Fields:**
- `enabled: bool` - Enable/disable authentication middleware (default: false)
- `database_url: String` - SQLite database URL for API keys (default: "sqlite://db/auth.db")

**Default Implementation (Lines 266-274 in `crates/prism-core/src/config/mod.rs`):**
- `enabled`: false (authentication disabled by default)
- `database_url`: Constructed as "sqlite://{project_root}/db/auth.db"

**Functionality:**
When enabled, requires valid API keys for all requests. Keys are stored in a SQLite database specified by `database_url`. The default path is relative to the project root's `db/` directory. Disabling authentication allows unrestricted access (suitable for development/testing).

### MetricsConfig

**Lines 125-132** (`crates/prism-core/src/config/mod.rs`)

Prometheus metrics exporter configuration.

**Fields:**
- `enabled: bool` - Enable metrics collection and export (default: true)
- `prometheus_port: Option<u16>` - Port for /metrics endpoint (default: Some(9090))

**Default Implementation (Lines 277-280 in `crates/prism-core/src/config/mod.rs`):**
- `enabled`: true
- `prometheus_port`: Some(9090)

**Functionality:**
Controls whether the application collects and exposes Prometheus metrics. When enabled, metrics are accessible at `http://{bind_address}:{prometheus_port}/metrics`. Setting `prometheus_port` to None disables the HTTP endpoint while maintaining internal metric collection.

### LoggingConfig

**Lines 135-142** (`crates/prism-core/src/config/mod.rs`)

Logging verbosity and formatting configuration.

**Fields:**
- `level: String` - Log level: "trace", "debug", "info", "warn", "error" (default: "info")
- `format: String` - Output format: "json" or "pretty" (default: "pretty")

**Default Implementation (Lines 283-286 in `crates/prism-core/src/config/mod.rs`):**
- `level`: "info"
- `format`: "pretty"

**Functionality:**
Controls logging verbosity and output formatting. The `level` field determines minimum log severity. The `format` field switches between human-readable "pretty" output and machine-parseable "json" output. Validation enforces that format is either "json" or "pretty" (line 468).

### HedgeConfig

**Lines 30-89** (`crates/prism-core/src/upstream/hedging.rs`)

Configuration for request hedging to improve tail latency.

**Fields:**
- `enabled: bool` - Enable hedged requests globally (default: false)
- `latency_quantile: f64` - Latency percentile to trigger hedging, 0.0-1.0 (default: 0.95 for P95)
- `min_delay_ms: u64` - Minimum delay before sending hedge request (default: 50)
- `max_delay_ms: u64` - Maximum delay before sending hedge request (default: 2000)
- `max_parallel: usize` - Maximum parallel requests including primary (default: 2)

**Default Implementation (Lines 79-88 in `crates/prism-core/src/upstream/hedging.rs`):**
- `enabled`: false (opt-in feature)
- `latency_quantile`: 0.95 (hedge after P95 latency)
- `min_delay_ms`: 50ms (don't hedge before 50ms)
- `max_delay_ms`: 2000ms (cap hedge delay at 2s)
- `max_parallel`: 2 (primary + 1 hedge request)

**Functionality:**
Controls the hedged request feature which sends parallel requests to multiple upstreams when the initial request exceeds expected response time. This dramatically improves P95 and P99 latencies.

The `latency_quantile` determines when to trigger hedging based on historical latency percentiles:
- `0.90` = P90: Hedge when request exceeds P90 latency (more aggressive)
- `0.95` = P95: Hedge when request exceeds P95 latency (balanced)
- `0.99` = P99: Hedge when request exceeds P99 latency (conservative)

The `min_delay_ms` and `max_delay_ms` provide bounds to prevent hedging too early (wasting resources) or too late (defeating the purpose).

**TOML Configuration:**
```toml
[hedging]
enabled = true
latency_quantile = 0.95
min_delay_ms = 50
max_delay_ms = 2000
max_parallel = 2
```

**Environment Variable Overrides:**
```bash
PRISM__HEDGING__ENABLED=true
PRISM__HEDGING__LATENCY_QUANTILE=0.99
PRISM__HEDGING__MIN_DELAY_MS=100
PRISM__HEDGING__MAX_DELAY_MS=1500
PRISM__HEDGING__MAX_PARALLEL=3
```

**Performance Considerations:**
- **Memory**: ~8KB per upstream for latency tracking (1000 samples × 8 bytes)
- **Throughput**: May increase upstream load by 10-30% when hedging triggers frequently
- **Latency**: Significant P95/P99 improvement, no impact on P50

**Related Documentation:**
- [Upstream Hedging](../upstream/upstream_hedging.md) - Detailed hedging implementation
- [Upstream Manager](../upstream/upstream_manager.md) - `send_request_auto()` API
- [Upstream Router](../upstream/upstream_router.md) - Router-based strategy selection

### ScoringConfig

**Lines 32-86** (`crates/prism-core/src/upstream/scoring.rs`)

Configuration for multi-factor upstream performance scoring.

**Fields:**
- `enabled: bool` - Enable scoring-based upstream selection (default: false)
- `window_seconds: u64` - Time window for metrics collection before reset (default: 1800 = 30 minutes)
- `min_samples: usize` - Minimum latency samples required for scoring (default: 10)
- `max_block_lag: u64` - Maximum acceptable block lag before maximum penalty (default: 5 blocks)
- `top_n: usize` - Number of top-scoring upstreams to consider (default: 3)
- `weights: ScoringWeights` - Weight factors for each scoring component

**ScoringWeights Fields:**
- `latency: f64` - Weight for P90 latency factor (default: 8.0)
- `error_rate: f64` - Weight for error rate factor (default: 4.0)
- `throttle_rate: f64` - Weight for throttle/rate-limit factor (default: 3.0)
- `block_head_lag: f64` - Weight for block lag factor (default: 2.0)
- `total_requests: f64` - Weight for load balancing factor (default: 1.0)

**Default Implementation** (Lines 143-174 in `crates/prism-core/src/upstream/scoring.rs`):
```rust
ScoringConfig {
    enabled: false,
    window_seconds: 1800,     // 30 minutes
    min_samples: 10,
    max_block_lag: 5,
    top_n: 3,
    weights: ScoringWeights {
        latency: 8.0,
        error_rate: 4.0,
        throttle_rate: 3.0,
        block_head_lag: 2.0,
        total_requests: 1.0,
    },
}
```

**Functionality:**
Controls the multi-factor scoring system that ranks upstreams based on performance metrics. When `enabled` is true, the system tracks latency percentiles, error rates, throttle rates, and block lag for each upstream. These factors are combined using the configured weights to produce a composite score (0-100) for each upstream.

The scoring engine uses a multiplicative scoring formula where each factor is raised to its weight exponent:
```
score = (latency_factor ^ latency_weight)
      × (error_rate_factor ^ error_rate_weight)
      × (throttle_factor ^ throttle_weight)
      × (block_lag_factor ^ block_lag_weight)
      × (load_factor ^ load_weight)
      × 100
```

Higher weights make that factor more influential. The `window_seconds` determines how long metrics are tracked before automatic reset, ensuring scores reflect recent performance. The `min_samples` prevents unreliable scores based on insufficient data.

**TOML Configuration:**
```toml
[scoring]
enabled = true
window_seconds = 1800
min_samples = 10
max_block_lag = 5
top_n = 3

[scoring.weights]
latency = 8.0
error_rate = 4.0
throttle_rate = 3.0
block_head_lag = 2.0
total_requests = 1.0
```

**Environment Variable Overrides:**
```bash
PRISM__SCORING__ENABLED=true
PRISM__SCORING__WINDOW_SECONDS=1800
PRISM__SCORING__MIN_SAMPLES=10
PRISM__SCORING__MAX_BLOCK_LAG=5
PRISM__SCORING__TOP_N=3
PRISM__SCORING__WEIGHTS__LATENCY=8.0
PRISM__SCORING__WEIGHTS__ERROR_RATE=4.0
PRISM__SCORING__WEIGHTS__THROTTLE_RATE=3.0
PRISM__SCORING__WEIGHTS__BLOCK_HEAD_LAG=2.0
PRISM__SCORING__WEIGHTS__TOTAL_REQUESTS=1.0
```

**Performance Characteristics:**
- **Memory**: ~8KB per upstream for latency tracking (1000 samples × 8 bytes)
- **CPU Overhead**: ~5-13µs per request for metric recording and score calculation
- **Latency Impact**: Minimal (scoring is async and cached)

**Use Cases:**
- **Intelligent Selection**: Route to consistently fast, reliable upstreams
- **Automatic Failover**: Low-scoring upstreams naturally receive less traffic
- **Cost Optimization**: Prefer upstreams with low throttle rates
- **Data Freshness**: Penalize upstreams with high block lag

**Related Documentation:**
- [Scoring Engine](../scoring/architecture.md) - Detailed scoring implementation
- [Scoring Configuration Guide](../scoring/configuration.md) - Comprehensive configuration reference
- [Upstream Router](../upstream/upstream_router.md) - Router integration with scoring

### ConsensusConfig

**Lines 33-73** (`crates/prism-core/src/upstream/consensus/config.rs`)

Configuration for consensus validation across multiple upstreams.

**Fields:**
- `enabled: bool` - Enable consensus validation globally (default: false)
- `max_count: usize` - Maximum upstreams to query in parallel (default: 3)
- `min_count: usize` - Minimum upstreams that must agree for consensus (default: 2)
- `dispute_behavior: DisputeBehavior` - How to handle disagreements (default: PreferBlockHeadLeader)
- `failure_behavior: FailureBehavior` - How to handle upstream failures (default: AcceptAnyValid)
- `low_participants_behavior: LowParticipantsBehavior` - Behavior when insufficient upstreams (default: AcceptAvailable)
- `methods: Vec<String>` - RPC methods requiring consensus validation
- `disagreement_penalty: f64` - Score penalty for disagreeing upstreams (default: 10.0)
- `timeout_seconds: u64` - Consensus operation timeout (default: 10)

**Default Methods List** (Lines 83-91 in `crates/prism-core/src/upstream/consensus/config.rs`):
```rust
vec![
    "eth_getBlockByNumber".to_string(),
    "eth_getBlockByHash".to_string(),
    "eth_getTransactionByHash".to_string(),
    "eth_getTransactionReceipt".to_string(),
    "eth_getLogs".to_string(),
]
```

**DisputeBehavior Enum:**
- `PreferBlockHeadLeader` - Use response from most synced upstream (default)
- `ReturnError` - Return error when consensus cannot be reached
- `AcceptAnyValid` - Accept any valid response without consensus
- `PreferHighestScore` - Use response from highest-scoring upstream

**FailureBehavior Enum:**
- `AcceptAnyValid` - Accept any valid response, ignore failures (default)
- `ReturnError` - Return error if any upstream fails
- `UseHighestScore` - Use response from highest-scoring successful upstream

**LowParticipantsBehavior Enum:**
- `AcceptAvailable` - Proceed with available upstreams (default)
- `ReturnError` - Fail if insufficient upstreams available
- `OnlyBlockHeadLeader` - Query only most synced upstream

**Default Implementation** (Lines 101-115 in `crates/prism-core/src/upstream/consensus/config.rs`):
```rust
ConsensusConfig {
    enabled: false,
    max_count: 3,
    min_count: 2,
    dispute_behavior: DisputeBehavior::PreferBlockHeadLeader,
    failure_behavior: FailureBehavior::AcceptAnyValid,
    low_participants_behavior: LowParticipantsBehavior::AcceptAvailable,
    methods: vec![
        "eth_getBlockByNumber".to_string(),
        "eth_getBlockByHash".to_string(),
        "eth_getTransactionByHash".to_string(),
        "eth_getTransactionReceipt".to_string(),
        "eth_getLogs".to_string(),
    ],
    disagreement_penalty: 10.0,
    timeout_seconds: 10,
}
```

**Functionality:**
Controls the consensus validation system that queries multiple upstreams in parallel and requires a minimum number to agree before accepting a response. This provides strong data integrity guarantees for critical RPC methods.

When `enabled` is true and a request method is in the `methods` list, the system:
1. Queries `max_count` upstreams in parallel
2. Groups responses by hash
3. Checks if at least `min_count` upstreams agree
4. Returns consensus response or handles disputes based on `dispute_behavior`
5. Penalizes disagreeing upstreams via `disagreement_penalty`

The `timeout_seconds` caps the entire consensus operation, preventing indefinite waits for slow upstreams.

**TOML Configuration:**
```toml
[consensus]
enabled = true
max_count = 3
min_count = 2
dispute_behavior = "PreferBlockHeadLeader"
failure_behavior = "AcceptAnyValid"
low_participants_behavior = "AcceptAvailable"
disagreement_penalty = 10.0
timeout_seconds = 10

methods = [
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "eth_getLogs"
]
```

**Environment Variable Overrides:**
```bash
PRISM__CONSENSUS__ENABLED=true
PRISM__CONSENSUS__MAX_COUNT=3
PRISM__CONSENSUS__MIN_COUNT=2
PRISM__CONSENSUS__DISPUTE_BEHAVIOR=PreferBlockHeadLeader
PRISM__CONSENSUS__FAILURE_BEHAVIOR=AcceptAnyValid
PRISM__CONSENSUS__LOW_PARTICIPANTS_BEHAVIOR=AcceptAvailable
PRISM__CONSENSUS__DISAGREEMENT_PENALTY=10.0
PRISM__CONSENSUS__TIMEOUT_SECONDS=10
PRISM__CONSENSUS__METHODS="eth_getLogs,eth_call,eth_getBlockByNumber"
```

**Performance Characteristics:**
- **Latency**: Adds 50-200µs overhead (parallel queries + consensus logic)
- **Throughput**: Reduced by factor of `max_count` (e.g., 3x load increase)
- **Memory**: ~1KB per concurrent consensus operation
- **Trade-off**: Higher load/latency for strong data integrity guarantees

**Use Cases:**
- **Financial Applications**: DeFi protocols requiring data integrity
- **Critical Infrastructure**: Systems where incorrect data has severe consequences
- **Data Validation**: Detecting compromised or malfunctioning upstreams
- **Compliance**: Audit trails of validated RPC responses

**Important Notes:**
- **Never use consensus for write methods** (`eth_sendRawTransaction`) - would send duplicate transactions
- **Consider load impact**: Consensus multiplies upstream load by `max_count`
- **Balance safety vs availability**: Conservative settings may reduce availability during outages

**Related Documentation:**
- [Consensus Engine](../consensus/architecture.md) - Detailed consensus implementation
- [Consensus Configuration Guide](../consensus/configuration.md) - Comprehensive configuration reference with presets
- [Upstream Router](../upstream/upstream_router.md) - Router integration with consensus

## Loading Configuration

### from_file()

**Lines 323-343** (`crates/prism-core/src/config/mod.rs`)

Loads configuration from a TOML file with environment variable overrides.

**Signature:**
```rust
pub fn from_file<P: AsRef<Path>>(config_path: P) -> Result<Self, ConfigError>
```

**Loading Order (Configuration Precedence):**
1. **Defaults** - Hardcoded defaults set via `set_default()` calls (lines 324-337)
2. **TOML File** - Values from the specified configuration file (line 338)
3. **Environment Variables** - Variables with `PRISM_` prefix (line 339)

**Environment Variable Override Pattern:**
The method uses `Environment::with_prefix("PRISM").separator("__")` which means:
- Prefix: `PRISM_`
- Separator: `__` (double underscore)
- Example: `PRISM__SERVER__BIND_PORT=8080` overrides `server.bind_port`

**Error Handling:**
Returns `ConfigError` if:
- File parsing fails (invalid TOML syntax)
- Type conversion fails (e.g., string where number expected)
- Deserialization fails (unknown fields with strict mode)

**TOML File Optional:**
The configuration file is marked as `required(false)` (line 338), meaning missing files fall back to defaults rather than erroring. This enables pure environment variable or programmatic configuration.

### load()

**Lines 353-357** (`crates/prism-core/src/config/mod.rs`)

Convenience method for loading configuration with configurable file path.

**Signature:**
```rust
pub fn load() -> Result<Self, ConfigError>
```

**Functionality:**
Checks the `PRISM_CONFIG` environment variable for a custom configuration file path, defaulting to "config/config.toml" if unset. Delegates to `from_file()` for actual loading.

**Usage:**
```rust
let config = AppConfig::load()?;
```

**Environment Variable:**
- `PRISM_CONFIG`: Specifies alternate configuration file path
- Example: `PRISM_CONFIG=config/development.toml cargo run --bin server`

## Environment Variable Overrides

### PRISM_ Prefix Convention

All configuration values can be overridden via environment variables using the `PRISM_` prefix with double underscore separators.

**Format:**
```
PRISM__{SECTION}__{FIELD}=value
```

**Examples:**
```bash
# Server configuration
PRISM__SERVER__BIND_ADDRESS=0.0.0.0
PRISM__SERVER__BIND_PORT=8080
PRISM__SERVER__MAX_CONCURRENT_REQUESTS=500
PRISM__SERVER__REQUEST_TIMEOUT_SECONDS=60

# Cache configuration
PRISM__CACHE__ENABLED=true
PRISM__CACHE__CACHE_TTL_SECONDS=600

# Health check configuration
PRISM__HEALTH_CHECK__INTERVAL_SECONDS=30

# Authentication configuration
PRISM__AUTH__ENABLED=true
PRISM__AUTH__DATABASE_URL=sqlite://custom/path/auth.db

# Metrics configuration
PRISM__METRICS__ENABLED=true
PRISM__METRICS__PROMETHEUS_PORT=9091

# Logging configuration
PRISM__LOGGING__LEVEL=debug
PRISM__LOGGING__FORMAT=json

# Environment identifier
PRISM__ENVIRONMENT=production
```

**Nested Array Configuration:**
Environment variables can override array elements using indexed notation:
```bash
PRISM__UPSTREAMS__PROVIDERS__0__NAME=custom-provider
PRISM__UPSTREAMS__PROVIDERS__0__HTTPS_URL=https://custom.rpc.com
```

**Precedence:**
Environment variables have the highest precedence and override both defaults and TOML file values.

## Validation

### validate()

**Lines 427-473** (`crates/prism-core/src/config/mod.rs`)

Validates configuration consistency and correctness.

**Signature:**
```rust
pub fn validate(&self) -> Result<(), String>
```

**Validation Checks:**

1. **Upstream Providers (Lines 428-450):**
   - At least one provider configured (empty list rejected)
   - All providers have non-empty HTTPS URLs
   - HTTPS URLs start with "http" or "https"
   - WebSocket URLs (if present) start with "ws" or "wss"

2. **Cache Configuration (Lines 452-454):**
   - `cache_ttl_seconds` must be greater than 0

3. **Health Check Configuration (Lines 456-458):**
   - `interval_seconds` must be greater than 0

4. **Server Configuration (Lines 460-466):**
   - `max_concurrent_requests` must be greater than 0
   - `bind_port` must be greater than 0

5. **Logging Configuration (Lines 468-470):**
   - `format` must be either "json" or "pretty"

**Error Messages:**
Returns descriptive error strings for each validation failure, including the provider name and invalid value for detailed debugging.

**Usage:**
```rust
let config = AppConfig::load()?;
config.validate()?;
```

## Legacy Compatibility

### to_legacy_upstreams()

**Lines 364-380** (`crates/prism-core/src/config/mod.rs`)

Converts modern `UpstreamProvider` configuration to legacy `UpstreamConfig` format.

**Signature:**
```rust
pub fn to_legacy_upstreams(&self) -> Vec<UpstreamConfig>
```

**Conversion Mapping:**
- `https_url` → `url`
- `wss_url` → `ws_url`
- `name` → `name`
- `weight` → `weight`
- `timeout_seconds` → `timeout_seconds`
- `circuit_breaker_threshold` → `circuit_breaker_threshold`
- `circuit_breaker_timeout_seconds` → `circuit_breaker_timeout_seconds`
- `chain_id` → `chain_id`
- Derives `supports_websocket` from `wss_url.is_some()`

**Purpose:**
Maintains backward compatibility with internal components that still use the older `UpstreamConfig` struct (defined in `crates/prism-core/src/types.rs:111-121`). This allows gradual migration to the new configuration system without breaking existing code.

**Deprecation Path:**
This method is marked with `#[must_use]` and should eventually be removed once all internal code migrates to using `UpstreamProvider` directly.

## Helper Methods

### socket_addr()

**Lines 391-398** (`crates/prism-core/src/config/mod.rs`)

Constructs a `SocketAddr` from bind_address and bind_port.

**Signature:**
```rust
pub fn socket_addr(&self) -> Result<std::net::SocketAddr, String>
```

**Functionality:**
Parses the string "{bind_address}:{bind_port}" into a `SocketAddr` for server binding. Returns a descriptive error if parsing fails (e.g., invalid IP address format).

**Usage:**
```rust
let addr = config.socket_addr()?;
let listener = TcpListener::bind(addr).await?;
```

### request_timeout()

**Lines 404-406** (`crates/prism-core/src/config/mod.rs`)

Converts request timeout from seconds to Duration.

**Signature:**
```rust
pub fn request_timeout(&self) -> Duration
```

**Functionality:**
Convenience method for converting the `request_timeout_seconds` field into a `std::time::Duration` for use with async timeout functions.

**Usage:**
```rust
timeout(config.request_timeout(), async_operation()).await?
```

### health_check_interval()

**Lines 412-414** (`crates/prism-core/src/config/mod.rs`)

Converts health check interval from seconds to Duration.

**Signature:**
```rust
pub fn health_check_interval(&self) -> Duration
```

**Functionality:**
Convenience method for converting `health_check.interval_seconds` into a `Duration` for use with periodic timer tasks.

**Usage:**
```rust
let mut interval = tokio::time::interval(config.health_check_interval());
```

### Legacy Getter Methods

**Lines 478-542** (`crates/prism-core/src/config/mod.rs`)

A suite of convenience methods providing direct access to nested configuration values for backward compatibility with existing code.

**Available Getters:**
- `upstreams()` - Returns `Vec<UpstreamConfig>` (calls `to_legacy_upstreams()`)
- `cache_ttl_seconds()` - Returns `u64`
- `health_check_interval_seconds()` - Returns `u64`
- `max_concurrent_requests()` - Returns `usize`
- `request_timeout_seconds()` - Returns `u64`
- `bind_address()` - Returns `&str`
- `bind_port()` - Returns `u16`
- `advanced_cache_enabled()` - Returns `bool`
- `advanced_cache_config()` - Returns `&CacheManagerConfig`
- `auth_enabled()` - Returns `bool`
- `auth_database_url()` - Returns `&str`

**Purpose:**
Maintains backward compatibility with code that accessed nested configuration via these convenience methods rather than direct field access.

## TOML Format Examples

### Minimal Configuration

```toml
# Minimal working configuration - all other values use defaults

[[upstreams.providers]]
name = "my-provider"
chain_id = 1
https_url = "https://eth-mainnet.example.com/v1"
```

### Production Configuration

```toml
# Production-ready configuration for Ethereum mainnet
environment = "production"

[server]
bind_address = "0.0.0.0"  # Listen on all interfaces
bind_port = 3030
max_concurrent_requests = 500
request_timeout_seconds = 60

[upstreams]
[[upstreams.providers]]
name = "primary-provider"
chain_id = 1
https_url = "https://eth-mainnet.primary.com/v1/YOUR_API_KEY"
wss_url = "wss://eth-mainnet.primary.com/v1/YOUR_API_KEY"
weight = 3  # Primary receives 3x traffic
timeout_seconds = 30
circuit_breaker_threshold = 3
circuit_breaker_timeout_seconds = 5

[[upstreams.providers]]
name = "secondary-provider"
chain_id = 1
https_url = "https://eth-mainnet.secondary.com/v2/YOUR_API_KEY"
wss_url = "wss://eth-mainnet.secondary.com/v2/YOUR_API_KEY"
weight = 2  # Secondary receives 2x traffic
timeout_seconds = 30
circuit_breaker_threshold = 3
circuit_breaker_timeout_seconds = 5

[[upstreams.providers]]
name = "fallback-provider"
chain_id = 1
https_url = "https://eth-mainnet.fallback.com"
weight = 1  # Fallback receives 1x traffic
timeout_seconds = 45
circuit_breaker_threshold = 5
circuit_breaker_timeout_seconds = 10

[cache]
enabled = true
cache_ttl_seconds = 300

[cache.manager_config]
retain_blocks = 2000
enable_auto_cleanup = true
cleanup_interval_seconds = 300
cleanup_every_n_blocks = 100

[cache.manager_config.log_cache]
chunk_size = 1000
max_exact_results = 10000
max_bitmap_entries = 100000
safety_depth = 12

[cache.manager_config.block_cache]
hot_window_size = 200
max_headers = 10000
max_bodies = 10000
safety_depth = 12

[cache.manager_config.transaction_cache]
max_transactions = 50000
max_receipts = 50000
safety_depth = 12

[cache.manager_config.reorg_manager]
safety_depth = 64
max_reorg_depth = 256
reorg_detection_threshold = 3

[health_check]
interval_seconds = 30  # More frequent checks in production

[auth]
enabled = true
database_url = "sqlite:///var/lib/prism/auth.db"

[metrics]
enabled = true
prometheus_port = 9090

[logging]
level = "info"
format = "json"  # JSON format for log aggregation
```

### Development Configuration

```toml
# Development configuration with verbose logging
environment = "development"

[server]
bind_address = "127.0.0.1"  # Localhost only
bind_port = 3030
max_concurrent_requests = 100
request_timeout_seconds = 30

[[upstreams.providers]]
name = "infura-dev"
chain_id = 1
https_url = "https://mainnet.infura.io/v3/YOUR_DEV_KEY"
wss_url = "wss://mainnet.infura.io/ws/v3/YOUR_DEV_KEY"

[cache]
enabled = true
cache_ttl_seconds = 60  # Shorter TTL for development

[health_check]
interval_seconds = 60

[auth]
enabled = false  # Disabled for easier development

[metrics]
enabled = true
prometheus_port = 9090

[logging]
level = "debug"  # Verbose logging
format = "pretty"  # Human-readable output
```

### Testing Configuration (Devnet)

```toml
# Configuration for local test environment with private devnet
environment = "test"

[server]
bind_address = "127.0.0.1"
bind_port = 3030
max_concurrent_requests = 500
request_timeout_seconds = 10

[upstreams]
[[upstreams.providers]]
name = "geth-sealer"
chain_id = 1337
https_url = "http://localhost:8545"
wss_url = "ws://localhost:8546"
weight = 3
timeout_seconds = 5
circuit_breaker_threshold = 3
circuit_breaker_timeout_seconds = 5

[[upstreams.providers]]
name = "geth-rpc-1"
chain_id = 1337
https_url = "http://localhost:8547"
wss_url = "ws://localhost:8548"
weight = 2
timeout_seconds = 5
circuit_breaker_threshold = 3
circuit_breaker_timeout_seconds = 5

[cache]
enabled = true
cache_ttl_seconds = 60

[cache.manager_config]
retain_blocks = 100
enable_auto_cleanup = true
cleanup_interval_seconds = 30
cleanup_every_n_blocks = 10

[cache.manager_config.log_cache]
chunk_size = 100
max_exact_results = 1000
max_bitmap_entries = 10000
safety_depth = 6

[cache.manager_config.block_cache]
hot_window_size = 50
max_headers = 1000
max_bodies = 1000
safety_depth = 6

[cache.manager_config.transaction_cache]
max_transactions = 5000
max_receipts = 5000
safety_depth = 6

[cache.manager_config.reorg_manager]
safety_depth = 12
max_reorg_depth = 64
reorg_detection_threshold = 2

[health_check]
interval_seconds = 10

[auth]
enabled = false

[metrics]
enabled = true
prometheus_port = 9091

[logging]
level = "debug"
format = "pretty"
```

## Usage Examples

### Basic Configuration Loading

```rust
use prism_core::config::AppConfig;

// Load from default location (config/config.toml)
let config = AppConfig::load()?;

// Validate configuration
config.validate()?;

// Access configuration values
println!("Server will bind to: {}", config.socket_addr()?);
println!("Request timeout: {:?}", config.request_timeout());
println!("Cache enabled: {}", config.cache.enabled);
```

### Custom Configuration File

```rust
use prism_core::config::AppConfig;

// Load from custom path
let config = AppConfig::from_file("config/production.toml")?;
config.validate()?;

// Or use environment variable
// PRISM_CONFIG=config/production.toml cargo run
let config = AppConfig::load()?;
```

### Programmatic Configuration

```rust
use prism_core::config::{AppConfig, ServerConfig, UpstreamProvider};

let mut config = AppConfig::default();

// Customize server settings
config.server.bind_address = "0.0.0.0".to_string();
config.server.bind_port = 8080;

// Add custom upstream provider
config.upstreams.providers.push(UpstreamProvider {
    name: "custom-provider".to_string(),
    chain_id: 1,
    https_url: "https://my-custom-rpc.com".to_string(),
    wss_url: Some("wss://my-custom-rpc.com".to_string()),
    weight: 2,
    timeout_seconds: 30,
    circuit_breaker_threshold: 2,
    circuit_breaker_timeout_seconds: 1,
});

// Validate before use
config.validate()?;
```

### Environment Variable Override Example

```rust
use prism_core::config::AppConfig;

// Set environment variables before loading
// In practice, these would be set in shell or deployment config
std::env::set_var("PRISM__SERVER__BIND_PORT", "8080");
std::env::set_var("PRISM__LOGGING__LEVEL", "debug");
std::env::set_var("PRISM__CACHE__ENABLED", "false");

// Load configuration (env vars override file values)
let config = AppConfig::load()?;

assert_eq!(config.server.bind_port, 8080);
assert_eq!(config.logging.level, "debug");
assert_eq!(config.cache.enabled, false);
```

### Accessing Upstream Configurations

```rust
use prism_core::config::AppConfig;

let config = AppConfig::load()?;

// Modern approach - direct access
for provider in &config.upstreams.providers {
    println!("Provider: {} at {}", provider.name, provider.https_url);
    println!("  Chain ID: {}", provider.chain_id);
    println!("  Weight: {}", provider.weight);
    if let Some(ws_url) = &provider.wss_url {
        println!("  WebSocket: {}", ws_url);
    }
}

// Legacy approach - converted format
let legacy_upstreams = config.to_legacy_upstreams();
for upstream in legacy_upstreams {
    println!("Upstream: {} supports WS: {}",
             upstream.name, upstream.supports_websocket);
}
```

### Configuration in Server Initialization

```rust
use prism_core::config::AppConfig;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load and validate configuration
    let config = AppConfig::load()?;
    config.validate()?;

    // Initialize server with configuration
    let addr = config.socket_addr()?;
    let listener = TcpListener::bind(addr).await?;

    println!("Server listening on {}", addr);
    println!("Max concurrent requests: {}",
             config.server.max_concurrent_requests);
    println!("Request timeout: {:?}", config.request_timeout());

    // Use configuration for upstream manager
    let upstreams = config.to_legacy_upstreams();
    // Initialize upstream manager with upstreams...

    Ok(())
}
```

### Conditional Feature Enabling

```rust
use prism_core::config::AppConfig;

let config = AppConfig::load()?;

// Enable features based on configuration
if config.cache.enabled {
    println!("Initializing cache system...");
    let cache_config = config.advanced_cache_config();
    // Initialize CacheManager with cache_config
}

if config.auth_enabled() {
    println!("Enabling authentication...");
    let db_url = config.auth_database_url();
    // Initialize AuthManager with db_url
}

if config.metrics.enabled {
    println!("Starting metrics exporter on port {}",
             config.metrics.prometheus_port.unwrap_or(9090));
    // Initialize MetricsCollector
}
```

## Configuration Best Practices

### Security Considerations

1. **Bind Address**: Default `127.0.0.1` restricts access to localhost. Explicitly set to `0.0.0.0` for external access.
2. **API Keys**: Never commit API keys to version control. Use environment variables or secret management.
3. **Authentication**: Enable in production with `auth.enabled = true`.
4. **Database Paths**: Use absolute paths for production to avoid ambiguity.

### Performance Tuning

1. **Weight Distribution**: Assign higher weights to more reliable/faster providers.
2. **Circuit Breaker**: Lower threshold (2-3) for faster failover, higher (5+) for transient error tolerance.
3. **Concurrent Requests**: Set based on server capacity and expected load.
4. **Cache TTL**: Balance freshness vs. cache hit rate (300-600 seconds typical).

### Development vs. Production

**Development:**
- `bind_address`: "127.0.0.1"
- `logging.level`: "debug"
- `logging.format`: "pretty"
- `auth.enabled`: false
- `request_timeout_seconds`: 30

**Production:**
- `bind_address`: "0.0.0.0" or specific interface
- `logging.level`: "info" or "warn"
- `logging.format`: "json"
- `auth.enabled`: true
- `request_timeout_seconds`: 60
- Higher `max_concurrent_requests`
- More frequent health checks

### Configuration File Organization

Recommended structure:
```
config/
├── config.toml              # Default configuration
├── development.toml         # Development overrides
├── staging.toml             # Staging environment
├── production.toml          # Production environment
└── test.toml               # Test environment
```

Use `PRISM_CONFIG` environment variable to select configuration:
```bash
PRISM_CONFIG=config/production.toml cargo run --bin server
```

## Default Values Reference

| Configuration Path | Default Value | Type |
|-------------------|---------------|------|
| `environment` | "development" | String |
| `server.bind_address` | "127.0.0.1" | String |
| `server.bind_port` | 3030 | u16 |
| `server.max_concurrent_requests` | 100 | usize |
| `server.request_timeout_seconds` | 30 | u64 |
| `upstreams.providers[].weight` | 1 | u32 |
| `upstreams.providers[].timeout_seconds` | 30 | u64 |
| `upstreams.providers[].circuit_breaker_threshold` | 2 | u32 |
| `upstreams.providers[].circuit_breaker_timeout_seconds` | 1 | u64 |
| `cache.enabled` | true | bool |
| `cache.cache_ttl_seconds` | 300 | u64 |
| `health_check.interval_seconds` | 60 | u64 |
| `auth.enabled` | false | bool |
| `auth.database_url` | "sqlite://db/auth.db" | String |
| `metrics.enabled` | true | bool |
| `metrics.prometheus_port` | Some(9090) | Option<u16> |
| `logging.level` | "info" | String |
| `logging.format` | "pretty" | String |

## Related Documentation

- **Cache Manager Configuration**: See `docs/components/cache_manager.md` for detailed cache configuration
- **Upstream Management**: See `docs/components/upstream_manager.md` for provider management
- **Authentication**: See `docs/components/middleware_auth.md` for authentication setup
- **Metrics**: See `docs/components/proxy_engine.md` for metrics collection

## Testing

The configuration module includes comprehensive tests (lines 547-617 in `crates/prism-core/src/config/mod.rs`):

**Test Coverage:**
- `test_config_defaults` (lines 553-561): Verifies default value initialization
- `test_config_validation` (lines 564-584): Tests validation error conditions
- `test_legacy_conversion` (lines 586-594): Verifies upstream config conversion
- `test_toml_deserialization` (lines 596-616): Tests TOML parsing

Run configuration tests:
```bash
cargo test -p prism-core config::tests
```
