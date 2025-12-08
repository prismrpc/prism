# Prometheus & Prism Architecture Analysis

## Current Architecture (As Configured)

```
┌─────────────────────────────────────────────────────────────┐
│                         HOST SYSTEM                          │
│                                                              │
│  ┌────────────────────┐                                     │
│  │  Prism Server      │                                     │
│  │  (likely cargo run)│                                     │
│  │                    │                                     │
│  │  Port 3030: RPC    │◄────── User requests               │
│  │  Port 9091: Metrics│                                     │
│  └────────────────────┘                                     │
│           │                                                  │
│           │ Metrics exposed at:                             │
│           │ http://localhost:9091/metrics                   │
│           │                                                  │
│  ┌────────┼────────────────────────────────────┐           │
│  │ Docker │(network_mode: host)                │           │
│  │        ▼                                     │           │
│  │  ┌──────────────────┐                       │           │
│  │  │  Prometheus      │                       │           │
│  │  │  Port 9090       │                       │           │
│  │  │                  │                       │           │
│  │  │  CONFIGURED TO   │                       │           │
│  │  │  SCRAPE:         │                       │           │
│  │  │  localhost:3030  │ ◄──── WRONG PORT!    │           │
│  │  │     /metrics     │                       │           │
│  │  └──────────────────┘                       │           │
│  │                                              │           │
│  └──────────────────────────────────────────────┘           │
│                                                              │
│  ┌──────────────────────────────────────────────┐           │
│  │  Docker Network: prism-devnet (172.28.0.0/16)│           │
│  │                                               │           │
│  │  ┌────────────┐  ┌────────────┐  ┌────────────┐         │
│  │  │ geth-sealer│  │ geth-rpc-1 │  │ geth-rpc-2 │         │
│  │  │ :8545      │  │ :8547      │  │ :8549      │         │
│  │  └────────────┘  └────────────┘  └────────────┘         │
│  │                                               │           │
│  │  No Prometheus service in this network!      │           │
│  └──────────────────────────────────────────────┘           │
└─────────────────────────────────────────────────────────────┘
```

## The Problem

### Issue 1: Port Mismatch
```
Prometheus Config:     localhost:3030/metrics  ✗
Prism Actual Metrics:  localhost:9091/metrics  ✓
```

### Issue 2: Network Isolation
- Prometheus runs with `network_mode: host` (can access localhost)
- Prism likely runs on host (via cargo run)
- Should work IF port is correct

### Issue 3: No Monitoring in Devnet
- Devnet docker-compose doesn't include Prometheus
- Separate monitoring stack exists but may not be running
- No integration between the two setups

---

## What SHOULD Happen (Fixed Architecture)

```
┌─────────────────────────────────────────────────────────────┐
│                         HOST SYSTEM                          │
│                                                              │
│  ┌────────────────────┐                                     │
│  │  Prism Server      │                                     │
│  │                    │                                     │
│  │  Port 3030: RPC    │◄────── User requests               │
│  │  Port 9091: Metrics│                                     │
│  └────────────────────┘                                     │
│           │                                                  │
│           │ http://localhost:9091/metrics                   │
│           │                                                  │
│  ┌────────┼────────────────────────────────────┐           │
│  │ Docker │(network_mode: host)                │           │
│  │        ▼                                     │           │
│  │  ┌──────────────────┐                       │           │
│  │  │  Prometheus      │                       │           │
│  │  │  Port 9090       │                       │           │
│  │  │                  │                       │           │
│  │  │  SCRAPING:       │                       │           │
│  │  │  localhost:9091  │ ◄──── CORRECT! ✓     │           │
│  │  │     /metrics     │                       │           │
│  │  │                  │                       │           │
│  │  │  Every 5s        │───────────────────┐  │           │
│  │  └──────────────────┘                   │  │           │
│  │           │                              │  │           │
│  │           │ Stores metrics               │  │           │
│  │           ▼                              │  │           │
│  │  ┌──────────────────┐                   │  │           │
│  │  │  Grafana         │                   │  │           │
│  │  │  Port 3001       │◄──────────────────┘  │           │
│  │  │                  │                       │           │
│  │  │  Dashboards      │                       │           │
│  │  └──────────────────┘                       │           │
│  │                                              │           │
│  └──────────────────────────────────────────────┘           │
└─────────────────────────────────────────────────────────────┘
```

---

## Production Architecture (docker-compose.prod.yml)

```
┌──────────────────────────────────────────────────────────────┐
│              Docker Network: prism-network                    │
│                                                               │
│  ┌─────────────────┐        ┌──────────────────┐            │
│  │  Prism          │        │  Prometheus      │            │
│  │                 │        │                  │            │
│  │  3030:3030 (RPC)│◄───────┤  9091:9090       │            │
│  │  9090:9090 (??) │        │                  │            │
│  │                 │        │  Scrapes:        │            │
│  │  /metrics on    │        │  prism:9090      │            │
│  │  port 9090      │───────►│  /metrics        │            │
│  └─────────────────┘        └──────────────────┘            │
│                                      │                        │
│                             ┌────────┴─────────┐             │
│                             │                  │             │
│                             │  Grafana         │             │
│                             │  3001:3000       │             │
│                             │                  │             │
│                             └──────────────────┘             │
│                                                               │
└──────────────────────────────────────────────────────────────┘
```

**Note**: Production setup has different port mapping:
- Prism exposes metrics on container port 9090
- Prometheus can reach it via `prism:9090` (Docker DNS)
- Both on same network, so communication works

---

## Configuration Comparison

| Component | Devnet Config | Production Config | Monitoring Stack |
|-----------|---------------|-------------------|------------------|
| Prism Metrics Port | 9091 (config.test.toml) | 9090 (container) | N/A |
| Prometheus Scrape Target | localhost:3030 | prism:9090 | localhost:3030 |
| Prometheus UI Port | 9090 | 9091 (host) | 9090 |
| Network | host + prism-devnet | prism-network | host |
| Services Together? | NO | YES | Maybe |

---

## Scrape Flow (When Working)

```
1. Prism exposes metrics
   └─> GET http://localhost:9091/metrics
       └─> Returns:
           rpc_requests_total{method="eth_getBlockByNumber"} 12345
           rpc_cache_hits_total{method="eth_getBlockByNumber"} 468866
           ...

2. Prometheus scrapes (every 5s)
   └─> GET http://localhost:9091/metrics
       └─> Parses Prometheus format
       └─> Stores time series data

3. Prometheus serves queries
   └─> GET http://localhost:9090/api/v1/query?query=rpc_requests_total
       └─> Returns:
           {
             "data": {
               "result": [
                 {"metric": {"method": "eth_getBlockByNumber"}, "value": [timestamp, "12345"]}
               ]
             }
           }

4. Grafana visualizes
   └─> Queries Prometheus datasource
   └─> Renders dashboards
```

---

## Current State Detection

### Test 1: Is Prism metrics endpoint accessible?
```bash
curl http://localhost:3030/metrics  # Check port 3030
curl http://localhost:9091/metrics  # Check port 9091
```

Expected: One of these should return Prometheus metrics format

### Test 2: Is Prometheus running?
```bash
docker ps | grep prometheus
curl http://localhost:9090/-/healthy
```

Expected: Returns `Prometheus Server is Healthy.`

### Test 3: What is Prometheus trying to scrape?
```bash
curl http://localhost:9090/api/v1/targets | jq
```

Expected: Shows target `rpc-aggregator` with scrapeUrl and health status

### Test 4: Does Prometheus have any Prism metrics?
```bash
curl 'http://localhost:9090/api/v1/query?query=rpc_requests_total' | jq '.data.result | length'
```

Expected:
- `> 0` = Prometheus HAS data (is scraping)
- `0` = Prometheus has NO data (not scraping)

---

## Verdict

Based on configuration analysis:

```
Prism Metrics Port:         9091 ✓
Prometheus Scrape Target:   3030 ✗
Result:                     NOT SCRAPING ✗
```

**Confidence**: 85%
**Reason**: Port mismatch in configuration files

**To confirm**: Run verification script to test actual runtime state.

---

## Fix Required

**File**: `/home/flo/workspace/personal/prism/deploy/prometheus/prometheus.yml`

**Change**:
```yaml
scrape_configs:
  - job_name: 'rpc-aggregator'
    static_configs:
      - targets: ['localhost:9091']  # ← Change this line
```

**Then**:
```bash
# If Prometheus is running in Docker
docker restart prometheus

# Or reload config
curl -X POST http://localhost:9090/-/reload
```

---

## Verification After Fix

```bash
# 1. Check target health
curl http://localhost:9090/api/v1/targets | jq '.data.activeTargets[] | select(.labels.job == "rpc-aggregator") | .health'

# Should return: "up"

# 2. Check metrics exist
curl 'http://localhost:9090/api/v1/query?query=rpc_requests_total' | jq '.data.result | length'

# Should return: > 0

# 3. View actual metrics
curl 'http://localhost:9090/api/v1/query?query=rpc_requests_total' | jq '.data.result[0]'
```

---

## Additional Notes

### About Port 3030
The user mentioned seeing metrics at `http://localhost:3030/metrics`, which suggests:
1. Either Prism exposes metrics on BOTH ports (3030 and 9091)
2. Or the config.test.toml setting is ignored
3. Or there's conditional logic based on whether a separate metrics port is set

**Recommendation**: Test both endpoints to see which one actually serves metrics.

### About the Admin API
The config shows:
```toml
[admin]
prometheus_url = "http://localhost:9090"
```

This suggests the admin API needs to query Prometheus. If Prometheus isn't running or accessible, admin API features that depend on Prometheus won't work.
