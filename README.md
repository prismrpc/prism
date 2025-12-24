<div align="center">

<img src="./icon.svg" alt="Prism Logo" width="180" height="180">

# Prism


### High performance Ethereum RPC aggregator and proxy in Rust

[![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Ethereum](https://img.shields.io/badge/Ethereum-3C3C3D?style=for-the-badge&logo=Ethereum&logoColor=white)](https://ethereum.org/)

[**Homepage**](https://prismrpc.dev) · [**Docs**](https://docs.prismrpc.dev) · [**Architecture**](docs/architecture.md)
</div>

## What is Prism

**Prism** is an *Ethereum JSON RPC aggregator and proxy* written in *Rust*.

It sits between your services and one or many upstream RPC providers. Your apps speak standard JSON RPC to **Prism**. **Prism** speaks JSON RPC to your providers and takes care of the rest:

| Feature | Description |
|---------|-------------|
| **Caching** | Shared caching for blocks, transactions, receipts and logs |
| **Routing** | Intelligent routing and failover across multiple providers |
| **Consensus** | Consensus checks for critical methods across several upstreams |
| **Performance** | Tail latency reduction through hedged requests |
| **Chain View** | Unified view of chain tip and finalized blocks |
| **Auth** | Optional authentication and rate limiting |
| **Metrics** | Prometheus metrics for observability |

The goal is to move all the messy RPC logic into one place and keep your application code simple. Instead of each service building its own cache and retry logic, you point everything at **Prism** and let it handle caching, routing and failure modes.


## Getting Started

### Requirements

* Rust nightly toolchain
* `cargo make`
* Optional:
  * `cargo nextest` for tests
  * Docker and Docker Compose for the local devnet

### Build

---

#### Development build

```bash
cargo make build
```

#### Optimized build for production workloads

```bash
cargo make build-release
```

### Minimal Configuration

Prism reads configuration from a TOML file. You can set the path with the `PRISM_CONFIG` environment variable. If not set, Prism will look for `config/config.toml`.

#### Example minimal config

```toml
[server]
bind_address = "127.0.0.1"
bind_port = 3030
max_concurrent_requests = 1000

[[upstreams.providers]]
name = "primary"
chain_id = 1
https_url = "https://eth-mainnet.your-provider.com"
weight = 2
timeout_seconds = 30

[[upstreams.providers]]
name = "fallback"
chain_id = 1
https_url = "https://eth-mainnet.backup-provider.com"
weight = 1
timeout_seconds = 30

[cache]
enabled = true

[cache.manager_config]
retain_blocks = 1000
enable_auto_cleanup = true

[auth]
enabled = false

[metrics]
enabled = true
```

For a complete configuration with consensus, hedging and scoring options, see `config/example.toml`.

### Run the Server

Use `cargo make` tasks for a smooth workflow.

```bash
# start with default config
cargo make run-server

# start release server
cargo make run-server-release

# use a custom config file
PRISM_CONFIG=config/development.toml cargo make run-server
```

The server exposes the following HTTP endpoints

| Path | Description |
|---------|-------------|
| `POST /` | Single JSON RPC request |
| `POST /batch` | Batch JSON RPC request |
| `GET /health` | Simple health and status information |
| `GET /metrics` | Prometheus metrics endpoint |

#### Example request using curl

```bash
curl -X POST http://localhost:3030/ \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_blockNumber",
    "params": [],
    "id": 1
  }'
```

### Cache Status Header

Every JSON RPC response includes a cache status header:

| Header Value | Description |
|--------------|-------------|
| `X-Cache-Status: FULL` | Complete cache hit |
| `X-Cache-Status: PARTIAL` | Partial cache hit (e.g. range aware log queries) |
| `X-Cache-Status: EMPTY` | Cached known empty result |
| `X-Cache-Status: MISS` | Request served entirely from upstream |

This helps you understand how Prism behaves for your workload and whether you get the hit rate you expect.


## CLI Tool

Prism includes a separate CLI binary for configuration checks, upstream diagnostics and authentication management.

The CLI lives in the `cli` crate and is usually run through `cargo` or through `cargo make` tasks.

### Common commands

```bash
# validate configuration file
cargo make cli-config-validate

# print resolved configuration
cargo make cli-config-show

# test connectivity to all configured upstreams
cargo make cli-test-upstreams
```

When authentication is enabled you can manage API keys through the CLI.

### Create a new key

```bash
cargo run --bin cli -- auth create \
  --name "production-api" \
  --description "Production service" \
  --rate-limit 100 \
  --refill-rate 10 \
  --daily-limit 100000 \
  --expires-in-days 365 \
  --methods "eth_getLogs,eth_getBlockByNumber"
```

###  List keys

```bash
cargo run --bin cli -- auth list
```

###  Revoke a key

```bash
cargo run --bin cli -- auth revoke --name "production-api"
```

>  By default the CLI uses `sqlite://db/auth.db`. You can change this with the `--database` flag or with the `DATABASE_URL` environment variable.


## Admin API

Prism includes a built-in Admin API for monitoring and management, running on a separate HTTP server (default port: 3031).

### Features

- **System Status**: Health checks, version info, uptime
- **Upstream Management**: CRUD operations, health checks, circuit breaker control
- **Cache Management**: Statistics, memory allocation, cache clearing
- **Metrics**: KPIs, latency percentiles, request volume, error distribution
- **Alerts**: Alert management and rule configuration
- **API Keys**: Key management (when authentication is enabled)
- **Logs**: Log querying and export

### Quick Start

```bash
# Start Prism (Admin API runs on port 3031 by default)
cargo run --release

# Access Swagger UI
open http://localhost:3031/admin/swagger-ui

# Check system status
curl http://localhost:3031/admin/system/status
```

### Authentication

For production, configure an admin token in your config file or environment:

```toml
[admin]
admin_token = "your-secure-token-here"
```

Then include the token in requests:

```bash
curl -H "X-Admin-Token: your-secure-token-here" \
  http://localhost:3031/admin/system/status
```

See the [Swagger UI](http://localhost:3031/admin/swagger-ui) for complete API documentation.


## E2E Testing with Devnet

Prism ships with an end to end test environment built on a local Geth network.

The devnet contains several nodes in a private network and is used to test:

- Upstream routing and failover
- Cache behavior for blocks and logs
- Consensus validation across multiple providers
- Batch request handling

The devnet is controlled through `cargo make` tasks:

```bash
# Start devnet
cargo make devnet-start

# Start server
cargo make run-server-devnet

# Rust based end to end suites
cargo make e2e-rust
cargo make e2e-rust-cache
cargo make e2e-rust-failover
cargo make e2e-rust-batch

# Stop devnet
cargo make devnet-stop
```

This setup gives you a realistic environment to test failure modes and caching without touching mainnet infrastructure.


## License

Prism is dual licensed under the **MIT** license and the **Apache 2.0** license.

You can use either license at your option.

See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) in this repository for the full texts.

---

<div align="center">

Made with ❤️ for the Ethereum ecosystem

</div>