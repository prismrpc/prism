# Prism k6 Load Testing

Load testing infrastructure for Prism RPC Aggregator using [k6](https://k6.io/).

Based on patterns from [erpc's k6 load testing](https://github.com/erpc/erpc) adapted for Prism's architecture.

## Prerequisites

Install k6:

```bash
# macOS
brew install k6

# Linux (Debian/Ubuntu)
sudo gpg -k
sudo gpg --no-default-keyring --keyring /usr/share/keyrings/k6-archive-keyring.gpg \
  --keyserver hkp://keyserver.ubuntu.com:80 --recv-keys C5AD17C747E3415A3642D57D77C6C491D6AC1D69
echo "deb [signed-by=/usr/share/keyrings/k6-archive-keyring.gpg] https://dl.k6.io/deb stable main" | \
  sudo tee /etc/apt/sources.list.d/k6.list
sudo apt-get update && sudo apt-get install k6

# Windows
choco install k6

# Or via Docker
docker pull grafana/k6
```

## Quick Start

```bash
# Start devnet
cargo make devnet-start

# Wait for devnet to be ready (check health)
cargo make devnet-health

# Start Prism server with devnet config
PRISM_CONFIG=docker/devnet/config.test.toml cargo run --bin server &

# Run smoke test
cargo make k6-smoke

# Run full load test
cargo make k6-load
```

## Test Scenarios

### 1. Smoke Test (`smoke.js`)

Quick validation that Prism is responding correctly.

- **Duration:** 1 minute
- **Load:** 10 RPS
- **Use case:** CI checks, pre-deployment validation

```bash
cargo make k6-smoke
```

### 2. Historical Load Test (`historical-load.js`)

Simulates backfilling/indexer access patterns.

- **Duration:** 10 minutes
- **Load:** 200 RPS
- **Traffic patterns:**
  - 50% `eth_getLogs` (Prism's strength!)
  - 25% `eth_getBlockByNumber`
  - 15% `eth_getTransactionReceipt`
  - 10% `eth_getBlockByHash`
- **Expected cache hit rate:** >50% (historical data is immutable)

```bash
cargo make k6-historical
```

### 3. Tip-of-Chain Test (`tip-of-chain.js`)

Simulates real-time wallet/explorer access patterns.

- **Duration:** 10 minutes
- **Load:** 400 RPS (2x historical)
- **Traffic patterns:**
  - 25% Recent blocks
  - 25% Latest logs
  - 25% Transaction receipts
  - 15% Block number polling
  - 10% Account balances
- **Expected cache hit rate:** >20% (tip data changes frequently)

```bash
cargo make k6-tip
```

### 4. Stress Test (`stress.js`)

Push Prism to its limits to find breaking points.

- **Duration:** 9 minutes
- **Load:** Ramps from 100 to 1500 RPS
- **Mixed traffic patterns:** Historical + Tip-of-chain
- **Purpose:** Find capacity limits, test circuit breakers

```bash
cargo make k6-stress
```

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `PRISM_URL` | `http://localhost:3030` | Prism server URL |
| `EXECUTOR` | `load` | Executor preset: `smoke`, `load`, `stress`, `spike` |
| `RANDOM_SEED` | (none) | Fixed seed for reproducibility |

### Executor Presets

Defined in `lib/config.js`:

| Preset | RPS | Duration | VUs |
|--------|-----|----------|-----|
| `smoke` | 10 | 1m | 10-50 |
| `load` | 200 | 10m | 100-500 |
| `stress` | 500 | 5m | 200-1000 |
| `spike` | 50-500 | 5m | 200-1000 |

Override with environment variable:

```bash
EXECUTOR=stress k6 run scenarios/historical-load.js
```

## Understanding Results

### Cache Metrics

Prism returns `X-Cache-Status` header:

- `FULL` - Complete cache hit
- `PARTIAL` - Partial cache hit (some data from cache, some from upstream)
- `EMPTY` - Cached empty result (e.g., no logs in range)
- `MISS` - Cache miss, fetched from upstream

### Expected Performance

| Scenario | Cache Hit Rate | P95 Latency | Error Rate |
|----------|---------------|-------------|------------|
| Historical | >50% | <500ms | <1% |
| Tip-of-chain | >20% | <300ms | <1% |
| Stress | Variable | <2000ms | <10% |

### Method-Specific Latencies

The tests track latency per RPC method:

```
=== Method Latencies (p95) ===
  eth_blockNumber: avg=5.2ms, p95=15.3ms
  eth_getBlockByNumber: avg=45.1ms, p95=120.5ms
  eth_getLogs: avg=85.3ms, p95=250.2ms
  eth_getTransactionReceipt: avg=25.4ms, p95=80.1ms
```

## File Structure

```
k6/
├── scenarios/              # Test scripts
│   ├── smoke.js           # Quick validation
│   ├── historical-load.js # Backfilling pattern
│   ├── tip-of-chain.js    # Real-time pattern
│   └── stress.js          # Stress testing
├── lib/                   # Shared modules
│   ├── config.js          # Configuration
│   ├── payloads.js        # RPC request builders
│   ├── metrics.js         # Custom metrics
│   └── cache.js           # Block/tx caching
├── results/               # Test output (gitignored)
└── README.md
```

## Comparison with erpc

This k6 setup is based on erpc's load testing patterns:

| Feature | erpc | Prism |
|---------|------|-------|
| Scenarios | 2 (historical, tip) | 4 (+ smoke, stress) |
| Chains | Multiple (Arbitrum, Monad, etc.) | Devnet (chain 1337) |
| Traces | Yes (`debug_traceTransaction`) | No (not supported) |
| Cache tracking | No | Yes (`X-Cache-Status`) |
| Weighted patterns | Yes | Yes |
| Constant-arrival-rate | Yes | Yes |
| Transaction caching | Yes | Yes |

## Troubleshooting

### k6 not found

Install k6 using the instructions above.

### Connection refused

Ensure Prism server is running:

```bash
PRISM_CONFIG=docker/devnet/config.test.toml cargo run --bin server
```

### Low cache hit rate

1. Run more iterations to warm the cache
2. Check if devnet has enough blocks (run tx-spammer)
3. Verify cache is enabled in config

### High error rate

1. Check devnet health: `cargo make devnet-health`
2. Check Prism logs for errors
3. Reduce RPS: `EXECUTOR=smoke k6 run ...`

## CI Integration

Add to GitHub Actions:

```yaml
- name: Install k6
  run: |
    sudo gpg -k
    sudo gpg --no-default-keyring --keyring /usr/share/keyrings/k6-archive-keyring.gpg \
      --keyserver hkp://keyserver.ubuntu.com:80 \
      --recv-keys C5AD17C747E3415A3642D57D77C6C491D6AC1D69
    echo "deb [signed-by=/usr/share/keyrings/k6-archive-keyring.gpg] https://dl.k6.io/deb stable main" | \
      sudo tee /etc/apt/sources.list.d/k6.list
    sudo apt-get update && sudo apt-get install k6

- name: Run k6 smoke test
  run: cargo make k6-smoke
```
