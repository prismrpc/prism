# Prometheus Scraping Status - Quick Summary

## Status: LIKELY NOT SCRAPING

---

## Quick Test

Run this command to verify:

```bash
python3 /home/flo/workspace/personal/prism/verify_prometheus_scraping.py
```

Or manually check:

```bash
# 1. Check Prism metrics
curl http://localhost:3030/metrics | head -20
curl http://localhost:9091/metrics | head -20

# 2. Check Prometheus
curl http://localhost:9090/-/healthy

# 3. Check Prometheus targets
curl http://localhost:9090/api/v1/targets | jq '.data.activeTargets[] | {job: .labels.job, health: .health, url: .scrapeUrl}'

# 4. Check if metrics exist in Prometheus
curl 'http://localhost:9090/api/v1/query?query=rpc_requests_total' | jq '.data.result | length'
```

---

## Key Issues Found

### 1. Port Confusion
- **Config says**: Metrics on port `9091` (config.test.toml)
- **Prometheus scrapes**: Port `3030` (prometheus.yml)
- **User mentioned**: Port `3030` with `/metrics`

### 2. No Prometheus in Devnet
- Devnet docker-compose has NO Prometheus service
- Monitoring stack is separate (`docker/docker-compose.yml`)
- Uses `network_mode: host` which may not reach containerized Prism

### 3. Network Isolation
- If Prism runs in devnet network (172.28.0.0/16)
- And Prometheus uses host networking
- They cannot communicate

---

## Configuration Files

### Prism Config
**File**: `/home/flo/workspace/personal/prism/docker/devnet/config.test.toml`
- `prometheus_port = 9091` (line 121)
- `prometheus_url = "http://localhost:9090"` (line 19)

### Prometheus Config
**File**: `/home/flo/workspace/personal/prism/deploy/prometheus/prometheus.yml`
- Scrapes `localhost:3030/metrics`
- Should probably be `localhost:9091/metrics`

### Docker Compose
**Devnet**: `/home/flo/workspace/personal/prism/docker/devnet/docker-compose.yml`
- NO Prometheus service

**Monitoring**: `/home/flo/workspace/personal/prism/docker/docker-compose.yml`
- Has Prometheus on port 9090
- Uses `network_mode: host`

---

## Quick Fix

### Option 1: Fix Port Mismatch

Edit `/home/flo/workspace/personal/prism/deploy/prometheus/prometheus.yml`:

```yaml
scrape_configs:
  - job_name: 'rpc-aggregator'
    static_configs:
      - targets: ['localhost:9091']  # Change from 3030 to 9091
```

Then reload:
```bash
curl -X POST http://localhost:9090/-/reload
```

### Option 2: Start Monitoring Stack

```bash
docker compose -f /home/flo/workspace/personal/prism/docker/docker-compose.yml up -d prometheus
```

Then apply Option 1 fix above.

---

## Verification Commands

After applying fixes:

```bash
# Wait 10 seconds for scrape
sleep 10

# Check if metrics are in Prometheus
curl 'http://localhost:9090/api/v1/query?query=rpc_requests_total' | jq '.data.result | length'

# If returns > 0, Prometheus IS scraping
# If returns 0, Prometheus is NOT scraping
```

---

## Expected Behavior When Working

### Prometheus Targets API
```bash
$ curl http://localhost:9090/api/v1/targets
{
  "data": {
    "activeTargets": [
      {
        "labels": {"job": "rpc-aggregator"},
        "health": "up",
        "lastScrape": "2025-12-06T...",
        "scrapeUrl": "http://localhost:9091/metrics"
      }
    ]
  }
}
```

### Metrics Query
```bash
$ curl 'http://localhost:9090/api/v1/query?query=rpc_requests_total'
{
  "data": {
    "result": [
      {
        "metric": {"method": "eth_getBlockByNumber"},
        "value": [timestamp, "12345"]
      }
    ]
  }
}
```

---

## Answer to Original Question

**Is Prometheus scraping Prism metrics?**

**LIKELY NO**, because:

1. Prometheus scrape config points to wrong port (3030 instead of 9091)
2. Prometheus service may not be running
3. Network isolation between services
4. No monitoring in devnet docker-compose

**To confirm**: Run the verification script:
```bash
python3 /home/flo/workspace/personal/prism/verify_prometheus_scraping.py
```

---

## Related Files

- Analysis: `/home/flo/workspace/personal/prism/PROMETHEUS_SCRAPING_ANALYSIS.md`
- Bash script: `/home/flo/workspace/personal/prism/verify_prometheus_scraping.sh`
- Python script: `/home/flo/workspace/personal/prism/verify_prometheus_scraping.py`
