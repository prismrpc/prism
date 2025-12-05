# Prism Architecture Documentation

This document describes the architecture of Prism, a high-performance Ethereum RPC aggregator. It covers module structure, data flows, component interactions, and background processes.

## Table of Contents

1. [Workspace Structure](#workspace-structure)
2. [Module Overview](#module-overview)
3. [Data Flow](#data-flow)
4. [Component Interactions](#component-interactions)
5. [Background Processes](#background-processes)
6. [Key Flows](#key-flows)

---

## Workspace Structure

Prism uses a Cargo workspace with 4 crates:

```
rpc-aggregator/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── prism-core/         # Core library (all business logic)
│   ├── server/             # HTTP server binary
│   ├── cli/                # CLI management tool
│   └── tests/              # Integration & E2E tests
```

### Crate Dependencies

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   server    │     │    cli      │     │   tests     │
└──────┬──────┘     └──────┬──────┘     └──────┬──────┘
       │                   │                   │
       └───────────────────┼───────────────────┘
                           │
                           ▼
                    ┌─────────────┐
                    │ prism-core  │
                    └─────────────┘
```

---

## Module Overview

### prism-core Modules

Location: `crates/prism-core/src/`

| Module | Location | Purpose |
|--------|----------|---------|
| **auth** | `auth/` | API key authentication with SQLite backend |
| **cache** | `cache/` | Multi-layer caching system (blocks, logs, transactions) |
| **chain** | `chain/mod.rs` | ChainState - Unified chain tip and finalized block tracking |
| **config** | `config/mod.rs` | Configuration loading and validation |
| **metrics** | `metrics/mod.rs` | Prometheus metrics collection |
| **middleware** | `middleware/` | HTTP middleware (auth, rate limiting, validation) |
| **proxy** | `proxy/` | Request routing and processing engine |
| **router** | `router/mod.rs` | HTTP endpoint handlers |
| **types** | `types.rs` | Core type definitions |
| **upstream** | `upstream/` | Upstream RPC provider management with routing strategies |
| **utils** | `utils/` | Utility functions |

### Detailed Module Structure

```
prism-core/src/
├── lib.rs                          # Module exports
├── types.rs                        # Core types (JsonRpcRequest, CacheStatus, etc.)
│
├── auth/
│   ├── mod.rs                      # AuthError, AuthenticatedKey
│   ├── api_key.rs                  # API key generation/validation
│   └── repository.rs               # SQLite repository
│
├── cache/
│   ├── mod.rs                      # Cache module exports
│   ├── cache_manager.rs:81-846     # CacheManager orchestrator
│   ├── block_cache.rs              # BlockCache (headers + bodies)
│   ├── log_cache.rs                # LogCache (event logs with range tracking)
│   ├── transaction_cache.rs        # TransactionCache (txs + receipts)
│   ├── reorg_manager.rs            # Chain reorganization handling
│   ├── converter.rs                # JSON <-> internal type conversion
│   └── types.rs                    # Cache-specific types
│
├── chain/
│   └── mod.rs                      # ChainState (unified chain tip tracking)
│
├── config/
│   └── mod.rs                      # AppConfig, ServerConfig, UpstreamProviders
│
├── metrics/
│   └── mod.rs                      # MetricsCollector (Prometheus)
│
├── middleware/
│   ├── mod.rs                      # Middleware exports
│   ├── auth.rs:16-117              # ApiKeyAuth, api_key_middleware
│   ├── rate_limiting.rs            # IP-based rate limiting
│   └── validation.rs               # JSON-RPC request validation
│
├── proxy/
│   ├── mod.rs                      # Proxy module exports
│   ├── engine.rs:24-152            # ProxyEngine (main request processor)
│   ├── errors.rs                   # ProxyError enum
│   ├── utils.rs                    # Utility functions
│   └── handlers/
│       ├── mod.rs                  # Handler exports
│       ├── blocks.rs               # eth_getBlockByNumber, eth_getBlockByHash
│       ├── logs.rs:19-414          # eth_getLogs (with partial cache support)
│       └── transactions.rs         # eth_getTransactionByHash, eth_getTransactionReceipt
│
├── router/
│   └── mod.rs:1-213                # HTTP handlers (handle_rpc, handle_health, etc.)
│
├── upstream/
│   ├── mod.rs                      # Upstream module exports
│   ├── manager.rs:11-331           # UpstreamManager
│   ├── endpoint.rs:17-417          # UpstreamEndpoint
│   ├── health.rs:9-146             # HealthChecker
│   ├── http_client.rs              # HttpClient (reqwest wrapper)
│   ├── load_balancer.rs            # LoadBalancer (round-robin fallback)
│   ├── circuit_breaker.rs          # CircuitBreaker pattern
│   ├── websocket.rs                # WebSocketHandler (chain tip subscription)
│   ├── scoring.rs                  # ScoringEngine (multi-factor upstream ranking)
│   ├── hedging.rs                  # HedgeExecutor (tail latency optimization)
│   ├── router/
│   │   ├── mod.rs                  # Router trait and implementations
│   │   ├── smart.rs                # SmartRouter (consensus/hedging/scoring)
│   │   └── simple.rs               # SimpleRouter (basic load balancing)
│   ├── consensus/
│   │   ├── mod.rs                  # Consensus module exports
│   │   ├── engine.rs               # ConsensusEngine (multi-upstream validation)
│   │   ├── validator.rs            # Response validation logic
│   │   └── config.rs               # Consensus configuration
│   └── errors.rs                   # UpstreamError enum
│
└── utils/
    ├── mod.rs                      # Utility exports
    └── hex_buffer.rs               # Hex formatting utilities
```

---

## Data Flow

### Request Flow Overview

```
                            ┌─────────────────────────────────────────────────┐
                            │                   SERVER                         │
                            │                                                  │
  HTTP Request              │   ┌──────────┐    ┌─────────────┐               │
  ──────────────────────────┼──►│  Axum    │───►│ Middleware  │               │
                            │   │  Router  │    │ Stack       │               │
                            │   └──────────┘    └──────┬──────┘               │
                            │                          │                       │
                            │                          ▼                       │
                            │   ┌──────────────────────────────────────────┐  │
                            │   │              router/mod.rs               │  │
                            │   │  handle_rpc() / handle_batched_rpc()     │  │
                            │   └─────────────────────┬────────────────────┘  │
                            │                         │                        │
                            └─────────────────────────┼────────────────────────┘
                                                      │
                                                      ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                                 PRISM-CORE                                       │
│                                                                                  │
│   ┌─────────────────────────────────────────────────────────────────────────┐   │
│   │                         ProxyEngine (proxy/engine.rs)                    │   │
│   │                                                                          │   │
│   │   process_request()                                                      │   │
│   │         │                                                                │   │
│   │         ▼                                                                │   │
│   │   ┌─────────────┐                                                        │   │
│   │   │  Validate   │──► Check ALLOWED_METHODS                               │   │
│   │   │  Request    │                                                        │   │
│   │   └──────┬──────┘                                                        │   │
│   │          │                                                               │   │
│   │          ▼                                                               │   │
│   │   ┌─────────────────────────────────────────────────────────────────┐   │   │
│   │   │                      Method Dispatch                             │   │   │
│   │   │                                                                  │   │   │
│   │   │   eth_getLogs ──────────► LogsHandler                            │   │   │
│   │   │   eth_getBlockByNumber ──► BlocksHandler                         │   │   │
│   │   │   eth_getBlockByHash ────► BlocksHandler                         │   │   │
│   │   │   eth_getTransactionByHash ► TransactionsHandler                 │   │   │
│   │   │   eth_getTransactionReceipt ► TransactionsHandler                │   │   │
│   │   │   other methods ─────────► forward_to_upstream()                 │   │   │
│   │   │                                                                  │   │   │
│   │   └─────────────────────────────────────────────────────────────────┘   │   │
│   │                                                                          │   │
│   └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────┘
```

### Cache Flow

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                      CacheManager (cache/cache_manager.rs)                       │
│                                                                                  │
│   Orchestrates all sub-caches and manages:                                       │
│   - Current chain tip tracking                                                   │
│   - Inflight fetch coordination (prevents duplicate fetches)                     │
│   - Background cleanup tasks                                                     │
│                                                                                  │
│   ┌─────────────────┐   ┌─────────────────┐   ┌─────────────────────────────┐   │
│   │   BlockCache    │   │    LogCache     │   │    TransactionCache         │   │
│   │                 │   │                 │   │                             │   │
│   │ ┌─────────────┐ │   │ ┌─────────────┐ │   │ ┌─────────────────────────┐ │   │
│   │ │HeaderCache  │ │   │ │ LogStore    │ │   │ │  TransactionStore      │ │   │
│   │ │(by number)  │ │   │ │ (LogId →    │ │   │ │  (hash → tx data)      │ │   │
│   │ └─────────────┘ │   │ │  LogRecord) │ │   │ └─────────────────────────┘ │   │
│   │ ┌─────────────┐ │   │ └─────────────┘ │   │ ┌─────────────────────────┐ │   │
│   │ │BodyCache    │ │   │ ┌─────────────┐ │   │ │  ReceiptStore          │ │   │
│   │ │(by hash)    │ │   │ │BlockBitmap  │ │   │ │  (hash → receipt)      │ │   │
│   │ └─────────────┘ │   │ │(range track)│ │   │ └─────────────────────────┘ │   │
│   │ ┌─────────────┐ │   │ └─────────────┘ │   │                             │   │
│   │ │HotWindow    │ │   │ ┌─────────────┐ │   └─────────────────────────────┘   │
│   │ │(recent)     │ │   │ │ExactResult  │ │                                     │
│   │ └─────────────┘ │   │ │Cache        │ │                                     │
│   └─────────────────┘   │ └─────────────┘ │                                     │
│                         └─────────────────┘                                     │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### Upstream Flow

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                    UpstreamManager (upstream/manager.rs)                         │
│                                                                                  │
│   ┌───────────────────────────────────────────────────────────────────────────┐ │
│   │                        LoadBalancer                                        │ │
│   │                                                                            │ │
│   │   Selection Strategies:                                                    │ │
│   │   - get_next_healthy() ────────► Round-robin among healthy upstreams       │ │
│   │   - get_next_healthy_by_response_time() ► Best response time selection     │ │
│   │                                                                            │ │
│   └────────────────────────────────────┬──────────────────────────────────────┘ │
│                                        │                                        │
│                                        ▼                                        │
│   ┌────────────────────────────────────────────────────────────────────────┐   │
│   │                     UpstreamEndpoint (upstream/endpoint.rs)             │   │
│   │                                                                         │   │
│   │   ┌─────────────┐  ┌─────────────────┐  ┌─────────────────────────────┐│   │
│   │   │HealthState  │  │ CircuitBreaker  │  │   WebSocketHandler         ││   │
│   │   │             │  │                 │  │                            ││   │
│   │   │ is_healthy  │  │ Closed ─► Open  │  │ subscribe_to_new_heads()   ││   │
│   │   │ error_count │  │ on failures     │  │ (chain tip updates)        ││   │
│   │   │ response_ms │  │                 │  │                            ││   │
│   │   └─────────────┘  └─────────────────┘  └─────────────────────────────┘│   │
│   │                                                                         │   │
│   │   send_request() ──────────────────────────────────────────────────────┼───┼─►
│   │                                                                         │   │
│   └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                  │
│   ┌─────────────────────────────────────────────────────────────────────────┐   │
│   │                       HttpClient (upstream/http_client.rs)               │   │
│   │                                                                          │   │
│   │   - Connection pooling                                                   │   │
│   │   - Concurrency limiting (Semaphore)                                     │   │
│   │   - Timeout handling                                                     │   │
│   │   - TLS with rustls                                                      │   │
│   │                                                                          │   │
│   └─────────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────────┘
                                        │
                                        ▼
                              External RPC Providers
                        (Infura, Alchemy, QuickNode, etc.)
```

---

## Component Interactions

### Shared State Pattern

All major components are wrapped in `Arc<T>` for thread-safe sharing:

```rust
// From server/main.rs:79-118
let metrics_collector = Arc::new(MetricsCollector::new()?);
let chain_state = Arc::new(ChainState::new());
let cache_manager = Arc::new(CacheManager::new(&config.cache.manager_config, chain_state.clone())?);
let upstream_manager = Arc::new(
    UpstreamManagerBuilder::new()
        .chain_state(chain_state.clone())
        .concurrency_limit(max_concurrent)
        .build()?
);
let proxy_engine = Arc::new(ProxyEngine::new(
    cache_manager.clone(),
    upstream_manager.clone(),
    metrics_collector.clone(),
));
```

### Component Dependency Graph

```
                                    ┌─────────────┐
                                    │   Router    │
                                    │  Handlers   │
                                    └──────┬──────┘
                                           │
                                           ▼
                                    ┌─────────────┐
                                    │ ProxyEngine │──────────┐
                                    └──────┬──────┘          │
                                           │                 │
                    ┌──────────────────────┼────────────┐    │
                    │                      │            │    │
                    ▼                      ▼            ▼    ▼
           ┌───────────────┐      ┌───────────────┐  ┌──────────────┐
           │ CacheManager  │      │UpstreamManager│  │MetricsCollector│
           └───────┬───────┘      └───────┬───────┘  └──────────────┘
                   │                      │
        ┌──────────┼──────────┐           │
        │          │          │           ├─────────────────────────────┐
        ▼          ▼          ▼           ▼                             │
   ┌────────┐ ┌────────┐ ┌────────┐ ┌──────────────────┐               │
   │Block   │ │Log     │ │Tx      │ │   SmartRouter    │               │
   │Cache   │ │Cache   │ │Cache   │ │  (strategy sel.) │               │
   └────────┘ └────────┘ └────────┘ └────────┬─────────┘               │
                                              │                         │
                           ┌──────────────────┼──────────────────┐      │
                           │                  │                  │      │
                           ▼                  ▼                  ▼      ▼
                    ┌─────────────┐   ┌─────────────┐   ┌──────────────┐
                    │ Consensus   │   │   Hedging   │   │   Scoring    │
                    │   Engine    │   │  Executor   │   │   Engine     │
                    └──────┬──────┘   └──────┬──────┘   └──────┬───────┘
                           │                  │                  │
                           └──────────────────┼──────────────────┘
                                              │
                                              ▼
                                       ┌──────────────┐
                                       │ LoadBalancer │
                                       │  (fallback)  │
                                       └──────┬───────┘
                                              │
                                              ▼
                                       ┌─────────────────┐
                                       │UpstreamEndpoint │──► HttpClient
                                       │                 │──► CircuitBreaker
                                       │                 │──► WebSocketHandler
                                       └─────────────────┘
                                              │
                                              │ updates via WebSocket
                                              ▼
                                       ┌─────────────────┐
                                       │   ChainState    │◄───── ReorgManager
                                       │ (shared state)  │◄───── ScoringEngine
                                       └─────────────────┘
                                              │
                                       reads: CacheManager
                                              HealthChecker
                                              ConsensusEngine
```

### ChainState Ownership Pattern

`ChainState` provides a unified view of the canonical chain tip, finalized block, and head hash. Multiple components share the same `Arc<ChainState>` instance:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                       ChainState Shared Ownership                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│                         ┌─────────────────┐                                 │
│                         │   ChainState    │                                 │
│                         │ (single source  │                                 │
│                         │   of truth)     │                                 │
│                         └────────┬────────┘                                 │
│                                  │                                          │
│                   Arc<ChainState> cloned to:                                │
│          ┌───────────────┬───────┴───────┬───────────────┐                  │
│          │               │               │               │                  │
│          ▼               ▼               ▼               ▼                  │
│  ┌──────────────┐ ┌─────────────┐ ┌─────────────┐ ┌──────────────┐          │
│  │ CacheManager │ │ReorgManager │ │ScoringEngine│ │HealthChecker │          │
│  │   (READER)   │ │  (WRITER)   │ │(TIP WRITER) │ │  (READER)    │          │
│  └──────────────┘ └─────────────┘ └─────────────┘ └──────────────┘          │
│                                                                             │
│  Read Operations (lock-free, P99 < 50ns):                                   │
│    • current_tip() - Get chain tip block number                             │
│    • finalized_block() - Get finalized checkpoint                           │
│    • current_head_hash() - Get tip block hash                               │
│    • safe_head(depth) - Get tip minus safety depth                          │
│                                                                             │
│  Write Operations (coordinated):                                            │
│    • update_tip() - ReorgManager updates tip+hash                           │
│    • update_finalized() - ReorgManager updates finalized                    │
│    • update_tip_simple() - ScoringEngine updates tip number only            │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Design Points:**

| Aspect | Implementation | Rationale |
|--------|----------------|-----------|
| Ownership | Single `Arc<ChainState>` cloned to components | All see same state |
| Reads | SeqLock pattern (optimistic lock-free) | P99 < 50ns, never blocks |
| Writes | Async mutex in ReorgManager | Serializes tip/hash updates |
| Finalized | CAS loop with tip validation | Ensures finalized ≤ tip invariant |

**Why not alternatives?**

-  **Dependency injection framework**: Adds complexity without benefit for this use case
-  **Global singleton**: Makes testing difficult, hides dependencies
-  **Message passing**: Adds latency/complexity for simple state reads

See `crates/prism-core/src/chain/mod.rs` for detailed documentation.

### Handler Dependencies

Each handler (LogsHandler, BlocksHandler, TransactionsHandler) receives:

```rust
// From proxy/handlers/logs.rs:26-33
pub struct LogsHandler {
    cache_manager: Arc<CacheManager>,
    upstream_manager: Arc<UpstreamManager>,
    metrics_collector: Arc<MetricsCollector>,
}
```

### Request Routing Strategy

When a cache miss occurs and upstream providers must be queried, the SmartRouter selects the optimal routing strategy based on the RPC method and system state. The routing priority cascades through multiple strategies:

```
Request with Cache Miss
         │
         ▼
  ┌──────────────────┐
  │   SmartRouter    │
  │  Strategy Select │
  └────────┬─────────┘
           │
           ├─► 1. CONSENSUS (for critical methods)
           │   │
           │   ├─► Methods: eth_getBlockByNumber, eth_getBlockByHash
           │   ├─► Queries: 3+ upstreams in parallel
           │   ├─► Validates: Response consistency across providers
           │   ├─► Returns: Consensus result or fallback to next strategy
           │   └─► Use Case: Data integrity validation, reorg detection
           │
           ├─► 2. SCORING (intelligent upstream selection)
           │   │
           │   ├─► Factors: Latency (weight 8.0), error rate (weight 4.0), throttle rate (weight 3.0), block lag (weight 2.0), total requests (weight 1.0)
           │   ├─► Selection: Highest-scored healthy upstream
           │   ├─► Fallback: Next best on failure
           │   └─► Use Case: General-purpose intelligent routing
           │
           ├─► 3. HEDGING (for latency-sensitive operations)
           │   │
           │   ├─► Strategy: Send initial request, hedge after timeout
           │   ├─► Timeout: Configurable (default: 50-100ms)
           │   ├─► Hedges: Up to N parallel requests (default: 2-3)
           │   ├─► Returns: First successful response
           │   └─► Use Case: P99 latency optimization for time-critical queries
           │
           └─► 4. LOAD BALANCER (basic round-robin fallback)
               │
               ├─► Strategy: Round-robin among healthy upstreams
               ├─► Circuit Breaker: Removes unhealthy upstreams
               └─► Use Case: Simple distribution when advanced strategies unavailable
```

**Routing Decision Flow:**

1. **Method Classification**: Determine if method requires consensus validation (blocks, transactions)
2. **Strategy Availability**: Check if consensus/hedging/scoring engines are configured and enabled
3. **System State**: Evaluate current upstream health, scoring metrics, and request load
4. **Fallback Chain**: If selected strategy fails or is unavailable, cascade to next strategy
5. **Result**: Return first successful response with appropriate routing metadata

**Configuration Example:**

```toml
[upstream.routing]
strategy = "smart"  # Options: "smart", "simple", "scoring-only"

[upstream.consensus]
enabled = true
min_confirmations = 3
critical_methods = ["eth_getBlockByNumber", "eth_getBlockByHash"]

[upstream.hedging]
enabled = true
initial_timeout_ms = 50
max_hedge_requests = 2

[upstream.scoring]
enabled = true
weights = { latency = 0.4, error_rate = 0.3, reputation = 0.2, health = 0.1 }
```

See component-specific documentation for detailed configuration:
- [Consensus System](components/consensus/README.md)
- [Hedging System](components/hedging/README.md)
- [Scoring System](components/scoring/README.md)

---

## Background Processes

### Startup Sequence

```
server/main.rs:27-163

1. CryptoProvider::install_default()     # Install rustls crypto
2. AppConfig::load()                      # Load configuration
3. Initialize tracing/logging
4. MetricsCollector::new()                # Prometheus metrics
5. CacheManager::new()                    # Create cache orchestrator
6. cache_manager.start_all_background_tasks()  # Start cache tasks
7. ReorgManager::new()                    # Chain reorg handling
8. UpstreamManagerBuilder::new()...build()     # Upstream management
9. Add upstream providers (loop)
10. HealthChecker::new() + start_with_shutdown()  # Health monitoring
11. ProxyEngine::new()                    # Request processor
12. subscribe_to_all_upstreams()          # WebSocket subscriptions
13. create_app_with_security()            # Build Axum router
14. Start HTTP server with graceful shutdown
```

### Background Task Overview

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           Background Tasks                                       │
│                                                                                  │
│   ┌─────────────────────────────────────────────────────────────────────────┐   │
│   │                    HealthChecker (upstream/health.rs)                    │   │
│   │                                                                          │   │
│   │   - Runs at configurable interval (default: 30s)                         │   │
│   │   - Calls eth_blockNumber on each upstream                               │   │
│   │   - Updates health status and metrics                                    │   │
│   │   - Respects shutdown signal                                             │   │
│   │                                                                          │   │
│   └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                  │
│   ┌─────────────────────────────────────────────────────────────────────────┐   │
│   │              WebSocket Subscriptions (server/main.rs:249-365)            │   │
│   │                                                                          │   │
│   │   For each upstream with WebSocket support:                              │   │
│   │   - Subscribe to eth_subscribe("newHeads")                               │   │
│   │   - On new block: update cache_manager.update_tip()                      │   │
│   │   - On potential reorg: notify reorg_manager                             │   │
│   │   - Exponential backoff on connection failures                           │   │
│   │   - Respects shutdown signal                                             │   │
│   │                                                                          │   │
│   └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                  │
│   ┌─────────────────────────────────────────────────────────────────────────┐   │
│   │           CacheManager Background Tasks (cache/cache_manager.rs)         │   │
│   │                                                                          │   │
│   │   start_all_background_tasks() spawns:                                   │   │
│   │                                                                          │   │
│   │   1. Inflight Cleanup (every 60s)                                        │   │
│   │      - Removes stale fetch locks (>2min old)                             │   │
│   │      - Cleans orphaned fetch entries                                     │   │
│   │                                                                          │   │
│   │   2. Stats Updater                                                       │   │
│   │      - Periodically updates cache statistics                             │   │
│   │      - Deferred to avoid blocking hot paths                              │   │
│   │                                                                          │   │
│   │   3. Background Cleanup (configurable interval, default 5min)            │   │
│   │      - Prunes old log cache chunks                                       │   │
│   │      - Prunes block cache by safe head                                   │   │
│   │      - Prunes transaction cache to capacity limits                       │   │
│   │                                                                          │   │
│   └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                  │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### Shutdown Signal Propagation

```
                         shutdown_signal() triggered
                                   │
                                   ▼
                          shutdown_tx.send(())
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                    │
              ▼                    ▼                    ▼
     HealthChecker         WebSocket Tasks      CacheManager Tasks
     (shutdown_rx)         (shutdown_rx)        (shutdown_rx)
              │                    │                    │
              ▼                    ▼                    ▼
         Task exits          Tasks exit           Tasks exit
```

---

## Key Flows

### Single RPC Request Flow

```
1. HTTP POST /
   │
2. ├── [If auth enabled] api_key_middleware (middleware/auth.rs:89-117)
   │   ├── Extract API key from X-API-Key header or ?api_key= query
   │   ├── Validate against SQLite repository (with 60s cache)
   │   └── Check quotas, expiration, allowed methods
   │
3. ├── handle_rpc (router/mod.rs:14-55)
   │   └── Parse JsonRpcRequest
   │
4. ├── proxy_engine.process_request (proxy/engine.rs:70-81)
   │   ├── Validate request structure
   │   ├── Check if method is in ALLOWED_METHODS
   │   └── Dispatch to appropriate handler
   │
5. ├── Handler processes request (e.g., LogsHandler)
   │   ├── Check cache for data
   │   │   ├── FULL HIT: Return cached data
   │   │   ├── PARTIAL HIT: Combine cached + fetch missing
   │   │   └── MISS: Fetch from upstream, cache result
   │   │
   │   └── [If cache miss/partial]
   │       └── upstream_manager.send_request_with_response_time
   │           ├── LoadBalancer selects best upstream
   │           ├── UpstreamEndpoint.send_request
   │           │   ├── Check circuit breaker
   │           │   ├── HttpClient.send_request
   │           │   └── Update health/circuit breaker state
   │           └── Retry on failure (configurable)
   │
6. └── Return JsonRpcResponse with x-cache-status header
```

### eth_getLogs Partial Cache Flow

```
LogsHandler.handle_advanced_logs_request (proxy/handlers/logs.rs:38-78)
│
├── Parse LogFilter from request params
│
├── cache_manager.get_logs_with_ids(&filter, current_tip)
│   │
│   └── Returns (cached_logs, missing_ranges)
│
├── Decision based on results:
│   │
│   ├── cached_logs: YES, missing_ranges: NONE
│   │   └── FULL CACHE HIT → Return cached logs directly
│   │
│   ├── cached_logs: NONE, missing_ranges: NONE
│   │   └── EMPTY CACHE HIT → Return empty array
│   │
│   ├── cached_logs: YES, missing_ranges: YES
│   │   └── PARTIAL CACHE HIT
│   │       ├── Keep cached logs
│   │       ├── fetch_missing_ranges_concurrent (max 6 concurrent)
│   │       │   ├── Create sub-requests for each missing range
│   │       │   ├── Forward to upstream
│   │       │   └── Cache fetched logs
│   │       ├── Merge cached + fetched logs
│   │       └── Sort by (blockNumber, logIndex)
│   │
│   └── cached_logs: NONE, missing_ranges: YES
│       └── CACHE MISS
│           ├── Forward entire request to upstream
│           └── Cache response logs
│
└── Return response with appropriate CacheStatus
```

### Circuit Breaker State Machine

```
                           ┌────────────────────┐
                           │                    │
                           │      CLOSED        │◄──────────────────┐
                           │                    │                   │
                           └─────────┬──────────┘                   │
                                     │                              │
                           on_failure() called                on_success()
                           failure_count++                     resets state
                                     │                              │
                                     ▼                              │
                    ┌────────────────────────────────┐              │
                    │  failure_count >= threshold?   │              │
                    └────────────────┬───────────────┘              │
                                     │ YES                          │
                                     ▼                              │
                           ┌────────────────────┐                   │
                           │                    │                   │
                           │       OPEN         │───────────────────┤
                           │                    │                   │
                           └─────────┬──────────┘                   │
                                     │                              │
                           timeout elapsed                          │
                                     │                              │
                                     ▼                              │
                           ┌────────────────────┐                   │
                           │                    │                   │
                           │    HALF-OPEN       │───────────────────┘
                           │  (allow 1 request) │
                           │                    │
                           └────────────────────┘
```

### Authentication Middleware Flow

```
api_key_middleware (middleware/auth.rs:89-117)
│
├── Extract API key
│   ├── Try X-API-Key header
│   └── Fallback to ?api_key= query param
│
├── ApiKeyAuth.authenticate (middleware/auth.rs:33-82)
│   │
│   ├── Check in-memory cache (DashMap, 60s TTL)
│   │   └── If cached and fresh → return AuthenticatedKey
│   │
│   ├── Hash API key (SHA-256)
│   │
│   ├── Query SQLite repository
│   │   └── SELECT * FROM api_keys WHERE key_hash = ?
│   │
│   ├── Validate:
│   │   ├── is_active == true
│   │   ├── Not expired (expires_at > now)
│   │   ├── Within daily quota
│   │   └── Reset quota if new day
│   │
│   ├── Fetch allowed methods for key
│   │
│   ├── Build AuthenticatedKey
│   │   ├── id, name
│   │   ├── rate_limit_max_tokens, rate_limit_refill_rate
│   │   ├── daily_request_limit
│   │   └── allowed_methods, method_limits
│   │
│   └── Cache result and return
│
└── Insert AuthenticatedKey into request extensions
    └── Available to handlers for method-level auth
```

---

## Supported RPC Methods

Defined in `types.rs:5-16`:

| Method | Handler | Cacheable |
|--------|---------|-----------|
| `net_version` | Forward to upstream | No |
| `eth_blockNumber` | Forward to upstream | No |
| `eth_chainId` | Forward to upstream | No |
| `eth_gasPrice` | Forward to upstream | No |
| `eth_getBalance` | Forward to upstream | No |
| `eth_getBlockByHash` | BlocksHandler | Yes (BlockCache) |
| `eth_getBlockByNumber` | BlocksHandler | Yes (BlockCache) |
| `eth_getLogs` | LogsHandler | Yes (LogCache, partial support) |
| `eth_getTransactionByHash` | TransactionsHandler | Yes (TransactionCache) |
| `eth_getTransactionReceipt` | TransactionsHandler | Yes (TransactionCache) |

---

## Configuration

Configuration is loaded from environment variables via the `config` crate.

### Key Configuration Structs

Location: `config/mod.rs`

| Struct | Purpose |
|--------|---------|
| `AppConfig` | Root configuration |
| `ServerConfig` | Bind address, port, max concurrent requests |
| `UpstreamProviders` | List of upstream RPC endpoints |
| `AuthConfig` | Authentication settings (enabled, database_url) |
| `CacheManagerConfig` | Cache sizes, cleanup intervals, retain_blocks |
| `RateLimitingConfig` | Rate limiting parameters |

### Environment Variables (Examples)

```bash
PRISM_BIND_PORT=3030
PRISM_MAX_CONCURRENT_REQUESTS=1000
PRISM_AUTH_ENABLED=true
PRISM_AUTH_DATABASE_URL=sqlite://./prism.db
PRISM_CACHE_RETAIN_BLOCKS=1000
PRISM_HEALTH_CHECK_INTERVAL_SECONDS=30
```

---

## HTTP Endpoints

| Endpoint | Method | Handler | Description |
|----------|--------|---------|-------------|
| `/` | POST | `handle_rpc` | Single JSON-RPC request |
| `/batch` | POST | `handle_batched_rpc` | Batch JSON-RPC requests |
| `/health` | GET | `handle_health` | Health check (upstream status, cache stats) |
| `/metrics` | GET | `handle_metrics` | Prometheus metrics |

### Response Headers

- `x-cache-status`: Indicates cache status (`FULL`, `PARTIAL`, `EMPTY`, `MISS`)
- `content-type`: `application/json` for RPC, `text/plain` for metrics

---

## File Reference Quick Links

| Component | Primary File | Key Lines |
|-----------|--------------|-----------|
| Server Entry | `crates/server/src/main.rs` | 27-163 (main) |
| App Creation | `crates/server/src/main.rs` | 204-247 (create_app_with_security) |
| ProxyEngine | `crates/prism-core/src/proxy/engine.rs` | 24-152 |
| LogsHandler | `crates/prism-core/src/proxy/handlers/logs.rs` | 19-414 |
| CacheManager | `crates/prism-core/src/cache/cache_manager.rs` | 64-846 |
| UpstreamManager | `crates/prism-core/src/upstream/manager.rs` | 11-331 |
| UpstreamEndpoint | `crates/prism-core/src/upstream/endpoint.rs` | 17-299 |
| HealthChecker | `crates/prism-core/src/upstream/health.rs` | 9-115 |
| Auth Middleware | `crates/prism-core/src/middleware/auth.rs` | 16-117 |
| Router Handlers | `crates/prism-core/src/router/mod.rs` | 1-189 |
| Type Definitions | `crates/prism-core/src/types.rs` | Full file |
