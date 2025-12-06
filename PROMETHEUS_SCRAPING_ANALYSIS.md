# Prometheus Scraping Verification Analysis

**Date**: 2025-12-06
**Analyst**: Agent Organizer
**Objective**: Verify whether Prometheus is actually scraping the Prism metrics endpoint

---

## Executive Summary

**CRITICAL FINDING**: Prometheus is likely **NOT scraping** the Prism metrics endpoint in the current devnet setup.

### Key Issues Identified

1. **No Prometheus service in devnet docker-compose**: The devnet setup at `/home/flo/workspace/personal/prism/docker/devnet/docker-compose.yml` does **not** include a Prometheus service
2. **Separate monitoring stack**: Prometheus exists in `/home/flo/workspace/personal/prism/docker/docker-compose.yml` but uses `network_mode: host` which may not reach the Prism server
3. **Configuration inconsistency**: The config file references `http://localhost:9090` but doesn't guarantee connectivity

---

## Detailed Findings

### 1. Prism Server Configuration

**File**: `/home/flo/workspace/personal/prism/docker/devnet/config.test.toml`

```toml
[admin]
enabled = true
bind_address = "127.0.0.1"
port = 3031
prometheus_url = "http://localhost:9090"  # Admin API references Prometheus

[metrics]
enabled = true
prometheus_port = 9091  # Note: Port 9091, NOT 3030!
```

**Key Points**:
- Prism metrics are enabled
- Metrics exposed on port **9091** (not on the RPC port 3030)
- Admin API expects Prometheus at `http://localhost:9090`
- **INCONSISTENCY**: The user mentioned `/metrics` endpoint on port 3030, but config shows 9091

### 2. Docker Compose Analysis

#### Devnet Setup (docker/devnet/docker-compose.yml)

**Services**:
- `geth-sealer` (block producer)
- `geth-rpc-1`, `geth-rpc-2`, `geth-rpc-3` (RPC nodes)
- `tx-spammer` (transaction generator)
- **NO PROMETHEUS SERVICE**

**Network**: `prism-devnet` (172.28.0.0/16)

**ISSUE**: The devnet setup has NO monitoring infrastructure. It only runs Geth nodes and a transaction spammer.

#### Monitoring Setup (docker/docker-compose.yml)

```yaml
services:
  prometheus:
    image: prom/prometheus:latest
    container_name: prometheus
    ports:
      - "9090:9090"
    volumes:
      - ../deploy/prometheus/prometheus.yml:/etc/prometheus/prometheus.yml
    network_mode: host  # <-- CRITICAL: Uses host networking

  grafana:
    image: grafana/grafana:latest
    container_name: grafana
    ports:
      - "3001:3000"
    network_mode: host  # <-- Uses host networking
```

**ISSUE**: These services use `network_mode: host`, which means:
- They bypass Docker networking
- They can only access services on the host's localhost
- If Prism runs in a container on the `prism-devnet` network, Prometheus **CANNOT** reach it

### 3. Prometheus Scrape Configuration

**File**: `/home/flo/workspace/personal/prism/deploy/prometheus/prometheus.yml`

```yaml
scrape_configs:
  - job_name: 'rpc-aggregator'
    static_configs:
      - targets: ['localhost:3030']  # <-- Configured to scrape port 3030
    metrics_path: '/metrics'
    scrape_interval: 5s
    scrape_timeout: 3s
    honor_labels: true
```

**ISSUES**:
1. **Wrong port**: Config scrapes `localhost:3030` but Prism metrics are on port **9091**
2. **Network isolation**: If Prometheus uses host networking and Prism is in a container, they can't communicate unless ports are properly exposed
3. **Target mismatch**: The scrape target doesn't match the actual metrics port

### 4. Production Setup (docker/docker-compose.prod.yml)

```yaml
services:
  prism:
    ports:
      - "3030:3030"  # RPC endpoint
      - "9090:9090"  # Prometheus metrics (WAIT - this contradicts config!)
    networks:
      - prism-network

  prometheus:
    profiles:
      - monitoring
    ports:
      - "9091:9090"  # Prometheus UI on host port 9091
    networks:
      - prism-network
```

**MORE CONFUSION**:
- Production setup exposes Prism metrics on port **9090** (container)
- But config.test.toml says `prometheus_port = 9091`
- Prometheus service itself runs on container port 9090, mapped to host 9091
- They're on the same network (`prism-network`) so they CAN communicate

---

## Root Cause Analysis

### The Port Confusion

There are **THREE different port numbers** being used:

1. **3030**: Prism RPC endpoint (definitely)
2. **9090**:
   - Prometheus UI (in monitoring stack)
   - Prism metrics port (in production docker-compose)
   - Referenced in config.test.toml admin section
3. **9091**:
   - Prism metrics port (in config.test.toml)
   - Prometheus UI mapped to host (in production setup)

### The Network Isolation Problem

**Current Setup** (likely running):
- Prism server: Running on **host** at port 3030 (via `cargo run`)
- Prism metrics: Should be on port **9091** (per config.test.toml)
- Prometheus: Running in Docker with `network_mode: host`, trying to scrape `localhost:3030/metrics`

**Why It's NOT Working**:
1. Prometheus is configured to scrape port **3030** but metrics are on **9091**
2. Even if Prism exposes metrics on 3030, the endpoint path needs verification
3. No evidence that Prometheus is actually running

---

## Verification Steps

I've created two verification scripts to test the actual status:

### 1. Bash Script
**Location**: `/home/flo/workspace/personal/prism/verify_prometheus_scraping.sh`

Run with:
```bash
chmod +x /home/flo/workspace/personal/prism/verify_prometheus_scraping.sh
./verify_prometheus_scraping.sh
```

### 2. Python Script (Recommended)
**Location**: `/home/flo/workspace/personal/prism/verify_prometheus_scraping.py`

Run with:
```bash
python3 /home/flo/workspace/personal/prism/verify_prometheus_scraping.py
```

This script will:
1. Check if Prism `/metrics` endpoint is accessible
2. Verify Prometheus is running at `http://localhost:9090`
3. Query Prometheus targets API to see configured scrape targets
4. Query Prometheus for actual Prism metrics data
5. Provide a definitive answer on scraping status

---

## Expected Results

### If Prometheus IS Scraping:
```
✓ Prism metrics endpoint: Accessible
✓ Prometheus server: Running
✓ Target 'rpc-aggregator': UP
✓ Metrics present: rpc_requests_total, rpc_cache_hits_total, etc.
```

### If Prometheus is NOT Scraping (Most Likely):
```
✓ Prism metrics endpoint: Accessible
? Prometheus server: May not be running
✗ Target 'rpc-aggregator': Not found or DOWN
✗ No metrics data in Prometheus
```

---

## Recommended Fix

To properly enable Prometheus scraping in the devnet environment:

### Option A: Start the Monitoring Stack

1. **Start Prometheus**:
   ```bash
   docker compose -f /home/flo/workspace/personal/prism/docker/docker-compose.yml up -d prometheus
   ```

2. **Fix the scrape target** in `/home/flo/workspace/personal/prism/deploy/prometheus/prometheus.yml`:
   ```yaml
   scrape_configs:
     - job_name: 'rpc-aggregator'
       static_configs:
         - targets: ['localhost:9091']  # Change from 3030 to 9091
       metrics_path: '/metrics'
   ```

3. **Reload Prometheus config**:
   ```bash
   curl -X POST http://localhost:9090/-/reload
   ```

### Option B: Add Prometheus to Devnet

Add a Prometheus service to `/home/flo/workspace/personal/prism/docker/devnet/docker-compose.yml`:

```yaml
  prometheus:
    image: prom/prometheus:latest
    container_name: prism-devnet-prometheus
    ports:
      - "9090:9090"
    volumes:
      - ../../deploy/prometheus/prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - prometheus_data:/prometheus
    command:
      - '--config.file=/etc/prometheus/prometheus.yml'
      - '--storage.tsdb.path=/prometheus'
    networks:
      prism-devnet:
        ipv4_address: 172.28.0.30
    restart: unless-stopped

volumes:
  prometheus_data:
```

Then update the scrape config to use the host IP.

---

## Action Items

1. **RUN VERIFICATION SCRIPT** to get actual status:
   ```bash
   python3 /home/flo/workspace/personal/prism/verify_prometheus_scraping.py
   ```

2. **Confirm which port** Prism metrics are actually on:
   ```bash
   curl http://localhost:3030/metrics  # Check if metrics here
   curl http://localhost:9091/metrics  # Or check here
   ```

3. **Check if Prometheus is running**:
   ```bash
   docker ps | grep prometheus
   curl http://localhost:9090/-/healthy
   ```

4. **Review Prometheus targets**:
   ```bash
   curl http://localhost:9090/api/v1/targets | jq
   ```

5. **Fix configuration** based on findings

---

## Conclusion

Based on the configuration analysis:

**Prometheus is most likely NOT scraping Prism metrics** due to:
- Port mismatch (3030 vs 9091)
- Prometheus service may not be running
- Network isolation if services are in different Docker contexts
- No monitoring infrastructure in the devnet setup

**Next Step**: Run the verification script to get definitive proof.

---

## Files Analyzed

- `/home/flo/workspace/personal/prism/docker/devnet/docker-compose.yml`
- `/home/flo/workspace/personal/prism/docker/devnet/config.test.toml`
- `/home/flo/workspace/personal/prism/docker/docker-compose.yml`
- `/home/flo/workspace/personal/prism/docker/docker-compose.prod.yml`
- `/home/flo/workspace/personal/prism/deploy/prometheus/prometheus.yml`

## Scripts Created

- `/home/flo/workspace/personal/prism/verify_prometheus_scraping.sh` (Bash version)
- `/home/flo/workspace/personal/prism/verify_prometheus_scraping.py` (Python version - recommended)
