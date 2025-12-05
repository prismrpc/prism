# Prism RPC Aggregator Documentation

Welcome to the comprehensive documentation for **Prism**, a high-performance Ethereum RPC aggregator built in Rust. Prism is designed to provide resilient, cache-aware, and efficient access to Ethereum JSON-RPC endpoints through intelligent load balancing, advanced caching, and robust health monitoring.

## What is Prism?

Prism is a production-ready RPC proxy service that enhances Ethereum application performance and reliability by:

- **Intelligent Caching**: Multi-layer caching system for blocks, transactions, and event logs with partial-range fulfillment for `eth_getLogs`
- **Load Balancing**: Round-robin and response-time-based routing across multiple upstream RPC providers (Infura, Alchemy, QuickNode, etc.)
- **Health Monitoring**: Real-time health checks with automatic failover and circuit breaker patterns
- **Chain Reorganization Handling**: Automatic cache invalidation and reorg detection with configurable safety depth
- **API Key Authentication**: SQLite-backed authentication with quota management and method-level permissions
- **Metrics & Observability**: Comprehensive Prometheus metrics and structured logging

## Quick Start for New Developers

**Recommended Reading Order:**

1. Start with [architecture.md](architecture.md) - Understand the overall system design and data flows
2. Read [caching-system.md](caching-system.md) - Deep dive into the advanced caching implementation
3. Browse component documentation in [components/](#component-documentation) as needed

## Documentation Structure

### Core Documentation

| Document | Description | Audience |
|----------|-------------|----------|
| [architecture.md](architecture.md) | Complete system architecture, module structure, data flows, and component interactions | All developers |
| [deployment.md](deployment.md) | Deployment guide for Docker, binary, and systemd installations | DevOps & SREs |
| [caching-system.md](caching-system.md) | Advanced caching system with deduplicated storage, bitmap indexes, and reorg management | Backend engineers |

### Component Documentation

Detailed documentation for each system component is organized in the `components/` directory:

#### Cache Components
- [cache_manager.md](components/cache/cache_manager.md) - Central cache orchestrator coordinating all subcaches
- [log_cache.md](components/cache/log_cache.md) - Event log caching with roaring bitmaps and partial-range support
- [block_cache.md](components/cache/block_cache.md) - Block header and body caching with hot window ring buffer
- [transaction_cache.md](components/cache/transaction_cache.md) - Transaction and receipt caching
- [reorg_manager.md](components/cache/reorg_manager.md) - Chain reorganization detection and cache invalidation
- [converter.md](components/cache/converter.md) - JSON ↔ internal type conversion for cache efficiency

#### Proxy Components
- [proxy_engine.md](components/proxy/proxy_engine.md) - Main request orchestration and routing engine
- [proxy_handlers.md](components/proxy/proxy_handlers.md) - Method-specific handlers for cached RPC methods
- [proxy_support.md](components/proxy/proxy_support.md) - Support structures and helper functions

#### Upstream Components
- [upstream_manager.md](components/upstream/upstream_manager.md) - Upstream RPC provider lifecycle management
- [upstream_endpoint_health.md](components/upstream/upstream_endpoint_health.md) - Health checking and monitoring
- [upstream_load_balancer.md](components/upstream/upstream_load_balancer.md) - Round-robin and response-time-based selection
- [upstream_clients.md](components/upstream/upstream_clients.md) - HTTP/WebSocket client implementations
- [upstream_support.md](components/upstream/upstream_support.md) - Circuit breaker and support utilities

#### Routing & Intelligence Components
- **Consensus System** - Data integrity validation across multiple upstreams
  - [consensus/README.md](components/consensus/README.md) - Consensus system overview and use cases
  - [consensus/architecture.md](components/consensus/architecture.md) - Consensus engine design and implementation
  - [consensus/configuration.md](components/consensus/configuration.md) - Configuration reference for consensus validation
- **Hedging System** - Tail latency optimization through parallel requests
  - [hedging/README.md](components/hedging/README.md) - Request hedging overview and strategy
  - [hedging/architecture.md](components/hedging/architecture.md) - Hedge executor design and implementation
  - [hedging/configuration.md](components/hedging/configuration.md) - Configuration reference for hedging
- **Scoring System** - Multi-factor upstream selection and ranking
  - [scoring/README.md](components/scoring/README.md) - Scoring system overview and metrics
  - [scoring/architecture.md](components/scoring/architecture.md) - Scoring engine design and algorithms
  - [scoring/configuration.md](components/scoring/configuration.md) - Configuration reference for scoring weights

#### Middleware Components
- [middleware_auth.md](components/middleware/middleware_auth.md) - API key authentication and validation
- [middleware_validation.md](components/middleware/middleware_validation.md) - JSON-RPC request validation
- [middleware_rate_limiting.md](components/middleware/middleware_rate_limiting.md) - IP-based rate limiting

#### Configuration & Auth
- [config.md](components/config/config.md) - Configuration system and environment variable management
- [auth_api_key.md](components/auth/auth_api_key.md) - API key generation and hashing
- [auth_repository.md](components/auth/auth_repository.md) - SQLite repository for authentication data

#### Runtime Components
- [runtime_overview.md](components/runtime/runtime_overview.md) - Runtime module overview and quick start guide
- [runtime-api-design.md](components/runtime/runtime-api-design.md) - Design decisions and rationale
- [runtime_api_reference.md](components/runtime/runtime_api_reference.md) - Complete API documentation
- [runtime_architecture.md](components/runtime/runtime_architecture.md) - Internal design and implementation details

#### Utilities
- [utils_hex_buffer.md](components/utils/utils_hex_buffer.md) - Optimized hex formatting utilities

## Key Architectural Concepts

### Workspace Structure

Prism uses a Cargo workspace with 4 crates:

```
rpc-aggregator/
├── crates/
│   ├── prism-core/      # Core library with all business logic
│   ├── server/          # HTTP server binary
│   ├── cli/             # CLI management tool
│   └── tests/           # Integration & E2E tests
```

### Request Flow Overview

```
Client Request → Middleware (Auth, Rate Limit, Validation)
              → ProxyEngine → Method Dispatch
              → Cache Check (FULL/PARTIAL/MISS)
              → [If needed] SmartRouter → Strategy Selection:
                 - Consensus (critical methods requiring validation)
                 - Hedging (tail latency optimization)
                 - Scoring (intelligent upstream ranking)
                 - LoadBalancer (fallback round-robin)
              → Upstream Provider(s)
              → Response + Metrics
```

### Caching Strategy

- **Block Cache**: Headers + bodies with hot window ring buffer (O(1) recent block access)
- **Log Cache**: Deduplicated storage with roaring bitmap indexes for efficient filtering
- **Transaction Cache**: Transactions + receipts with log resolution from log store
- **Reorg Management**: Safe head tracking (tip - 12 blocks) with automatic invalidation

### Upstream Management

- **ChainState**: Unified chain tip and finalized block tracking shared across components (crates/prism-core/src/chain/)
- **SmartRouter**: Intelligent request routing using consensus, hedging, or scoring strategies (crates/prism-core/src/upstream/router/)
- **Consensus**: Multi-upstream validation for critical methods ensuring data integrity
- **Hedging**: Parallel request execution to optimize P99 latency
- **Scoring**: Multi-factor upstream ranking based on latency, error rates, and reputation
- **Health Checking**: Background `eth_blockNumber` polling (default: 30s intervals)
- **Load Balancer**: Round-robin or response-time-based endpoint selection (fallback strategy)
- **Circuit Breaker**: Automatic endpoint isolation on repeated failures
- **WebSocket Support**: Chain tip subscription for cache invalidation and ChainState updates

## Performance Characteristics

### Cache Performance
- **Log Query**: O(log n) with bitmap operations
- **Block Retrieval**: O(1) from hot window for recent blocks
- **Cache Hit Rate**: >95% for typical DeFi workloads
- **Memory Usage**: 30-60GB log store, 15-30GB block cache (configurable)

### Request Throughput
- **Concurrent Requests**: 1000+ with configurable limits
- **Async Runtime**: Tokio-based non-blocking request processing
- **Lock-free Reads**: Immutable Arc segments for cache access

## Development Workflow

### Essential Commands

All development workflows use `cargo-make` (see `Makefile.toml`):

```bash
# Quick development check
cargo make                       # format + clippy + test

# Full development workflow
cargo make dev                   # format + clippy + test + build

# Testing
cargo make test                  # Run all tests
cargo make test-performance      # Performance tests with output
cargo make bench                 # Run all benchmarks

# Running the service
cargo make run-server            # Start RPC aggregator
cargo make cli-config-validate   # Validate configuration
```

### Code Quality Standards

- **Clippy**: All warnings treated as errors (run `cargo make clippy`)
- **Formatting**: Rustfmt with consistent rules (run `cargo make format`)
- **Testing**: Comprehensive unit and integration tests with cargo-nextest
- **No Backwards Compatibility**: Active development, always improve and replace

## Configuration

Configuration is loaded from environment variables and TOML files. Key settings:

```bash
PRISM_BIND_PORT=3030
PRISM_MAX_CONCURRENT_REQUESTS=1000
PRISM_AUTH_ENABLED=true
PRISM_AUTH_DATABASE_URL=sqlite://./prism.db
PRISM_CACHE_RETAIN_BLOCKS=1000
PRISM_HEALTH_CHECK_INTERVAL_SECONDS=30
```

See [config.md](components/config/config.md) for complete configuration reference.

## HTTP Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | POST | Single JSON-RPC request |
| `/batch` | POST | Batch JSON-RPC requests |
| `/health` | GET | Health check with upstream status |
| `/metrics` | GET | Prometheus metrics endpoint |

## Supported RPC Methods

### Cached Methods (with specialized handlers)
- `eth_getBlockByHash` - Block cache
- `eth_getBlockByNumber` - Block cache with hot window
- `eth_getLogs` - Log cache with partial-range support
- `eth_getTransactionByHash` - Transaction cache
- `eth_getTransactionReceipt` - Transaction cache with resolved logs

### Forwarded Methods (no caching)
- `net_version`
- `eth_blockNumber`
- `eth_chainId`
- `eth_gasPrice`
- `eth_getBalance`

## Metrics & Observability

Prometheus metrics available at `/metrics`:

- `rpc_requests_total` - Total requests by method and upstream
- `rpc_cache_hits_total` - Cache hits by method
- `rpc_upstream_errors_total` - Upstream errors by provider
- `rpc_latency_seconds` - Request latency histogram

## Testing Infrastructure

- **Unit Tests**: Component-level testing with mock data
- **Integration Tests**: End-to-end scenarios in `crates/tests/`
- **Performance Tests**: Benchmark suite for critical paths
- **E2E Tests**: Real devnet with actual Geth nodes

```bash
cargo make test                  # All tests
cargo make test-core             # Core library tests
cargo make bench                 # Benchmarks
```

## Common Use Cases

### 1. DeFi Application Backend
High-read traffic with frequent `eth_getLogs` queries for events:
- Partial-range cache fulfillment reduces upstream calls by 60-80%
- Hot window provides O(1) access to recent blocks
- Automatic reorg handling ensures data consistency

### 2. Multi-Provider Resilience
Applications requiring high availability:
- Automatic failover between providers (Infura, Alchemy, etc.)
- Circuit breaker prevents cascading failures
- Health monitoring with real-time status

### 3. Rate-Limited API Access
Cost optimization for paid RPC providers:
- Cache reduces upstream API calls
- Request deduplication (future) for identical concurrent requests
- Per-provider traffic distribution

## Troubleshooting

### High Memory Usage
- Reduce chunk size in log cache config
- Lower `max_exact_results` and bitmap limits
- Decrease `retain_blocks` setting

### Low Cache Hit Rates
- Increase hot window size for block cache
- Adjust safety depth for reorg manager
- Review query patterns and filter usage

### Upstream Connection Issues
- Check health check interval configuration
- Review circuit breaker thresholds
- Verify provider URLs and authentication

## Contributing Guidelines

1. **Read the Architecture**: Understand system design before making changes
2. **Follow Code Standards**: Run `cargo make dev` before committing
3. **Write Tests**: Comprehensive tests for new features
4. **No Backwards Compatibility**: Improve and replace, don't maintain old code
5. **Document Changes**: Update relevant component documentation

## Resources

### Internal References
- Main codebase: `crates/`
- Configuration examples: `Makefile.toml`
- Test suite: `crates/tests/`

### External Resources
- [Ethereum JSON-RPC Specification](https://ethereum.org/en/developers/docs/apis/json-rpc/)
- [Rust Async Book](https://rust-lang.github.io/async-book/)
- [Tokio Runtime Documentation](https://tokio.rs/)
- [Prometheus Metrics Guide](https://prometheus.io/docs/practices/naming/)

## Getting Help

For questions or issues:
1. Check relevant component documentation in `components/`
2. Review test examples in `crates/tests/src/`
3. Examine actual implementation in `crates/prism-core/src/`
4. Consult the code review plan for performance insights

## Project Status

Prism is under active development with focus on:
- Performance optimization (see CODE_REVIEW_PLAN.md)
- E2E testing with real devnet infrastructure
- Advanced caching strategies with consensus validation
- Intelligent routing with hedging and scoring
- Production hardening

**Main Development Branch**: dev
**Active Feature Development**: Consensus, hedging, and scoring systems for intelligent upstream selection

---

**Last Updated**: November 2025
**Documentation Version**: 1.0
**Project Repository**: ``
