# Prism Deployment Guide

This guide covers deploying Prism RPC Aggregator in various environments.

## Table of Contents

- [Quick Start](#quick-start)
- [Docker Deployment](#docker-deployment)
- [Binary Deployment](#binary-deployment)
- [Configuration](#configuration)
- [Monitoring](#monitoring)
- [Production Recommendations](#production-recommendations)
- [Troubleshooting](#troubleshooting)

---

## Quick Start

The fastest way to get Prism running:

```bash
# Pull the Docker image
docker pull ghcr.io/prismrpc/prism:latest

# Create a config file (edit with your RPC endpoints)
curl -o config.toml https://raw.githubusercontent.com/prismrpc/prism/main/config/docker.toml

# Run Prism
docker run -d \
  --name prism \
  -p 3030:3030 \
  -p 9090:9090 \
  -v $(pwd)/config.toml:/app/config/config.toml:ro \
  -v prism-db:/app/db \
  ghcr.io/prismrpc/prism:latest

# Test it
curl -X POST http://localhost:3030/ \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
```

---

## Docker Deployment

### Prerequisites

- Docker Engine 20.10+
- Docker Compose v2 (optional, for full stack)

### Using Docker Run

```bash
# Pull the image
docker pull ghcr.io/prismrpc/prism:0.1.0

# Create directories
mkdir -p config db

# Copy and edit configuration
cp config/docker.toml config/config.toml
# Edit config/config.toml with your RPC endpoints

# Run the container
docker run -d \
  --name prism \
  --restart unless-stopped \
  -p 3030:3030 \
  -p 9090:9090 \
  -v $(pwd)/config/config.toml:/app/config/config.toml:ro \
  -v $(pwd)/db:/app/db \
  -e RUST_LOG=info \
  ghcr.io/prismrpc/prism:0.1.0
```

### Using Docker Compose

For a complete deployment with optional monitoring:

```bash
# Clone the repository
git clone https://github.com/prismrpc/prism.git
cd prism-rpc-aggregator

# Edit configuration
cp config/docker.toml config/config.toml
# Add your RPC endpoints to config/config.toml

# Start Prism only
docker compose -f docker-compose.prod.yml up -d

# Or start with monitoring (Prometheus + Grafana)
docker compose -f docker-compose.prod.yml --profile monitoring up -d

# View logs
docker compose -f docker-compose.prod.yml logs -f prism

# Stop
docker compose -f docker-compose.prod.yml down
```

### Building from Source

```bash
# Build the Docker image locally
docker build -t prism-rpc:local .

# Run your local build
docker run -d \
  --name prism \
  -p 3030:3030 \
  -v $(pwd)/config/config.toml:/app/config/config.toml:ro \
  prism-rpc:local
```

---

## Binary Deployment

### Download Pre-built Binaries

Download from [GitHub Releases](https://github.com/prismrpc/prism/releases):

```bash
# Linux x86_64
curl -LO https://github.com/prismrpc/prism/releases/download/v0.1.0/prism-linux-x86_64.tar.gz

# Verify checksum
curl -LO https://github.com/prismrpc/prism/releases/download/v0.1.0/prism-linux-x86_64.tar.gz.sha256
sha256sum -c prism-linux-x86_64.tar.gz.sha256

# Extract
tar xzf prism-linux-x86_64.tar.gz

# Run
./prism-server
```

Available platforms:
- `prism-linux-x86_64.tar.gz` - Linux x86_64 (glibc)
- `prism-linux-x86_64-musl.tar.gz` - Linux x86_64 (static, musl)
- `prism-linux-arm64.tar.gz` - Linux ARM64
- `prism-darwin-x86_64.tar.gz` - macOS Intel
- `prism-darwin-arm64.tar.gz` - macOS Apple Silicon

### Build from Source

```bash
# Prerequisites
rustup install nightly
rustup default nightly
cargo install cargo-make

# Clone and build
git clone https://github.com/prismrpc/prism.git
cd prism-rpc-aggregator
cargo make build-release

# Binaries are in target/release/
./target/release/server
./target/release/cli
```

### Systemd Service (Linux)

Create `/etc/systemd/system/prism.service`:

```ini
[Unit]
Description=Prism RPC Aggregator
After=network.target

[Service]
Type=simple
User=prism
Group=prism
WorkingDirectory=/opt/prism
Environment="PRISM_CONFIG=/opt/prism/config/config.toml"
Environment="RUST_LOG=info,prism_core=info,server=info"
ExecStart=/opt/prism/prism-server
Restart=on-failure
RestartSec=5s

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/prism/db

[Install]
WantedBy=multi-user.target
```

Install and start:

```bash
# Create user
sudo useradd -r -s /bin/false prism

# Create directories
sudo mkdir -p /opt/prism/{config,db}
sudo cp prism-server prism-cli /opt/prism/
sudo cp config/config.toml /opt/prism/config/
sudo chown -R prism:prism /opt/prism

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable prism
sudo systemctl start prism
sudo systemctl status prism
```

---

## Configuration

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `PRISM_CONFIG` | Path to configuration file | `config/config.toml` |
| `RUST_LOG` | Logging level | `info` |
| `RUST_BACKTRACE` | Enable backtraces | `0` |

### Key Configuration Options

```toml
[server]
bind_address = "0.0.0.0"  # Use 0.0.0.0 for Docker
bind_port = 3030
max_concurrent_requests = 500

[[upstreams.providers]]
name = "primary"
chain_id = 1
https_url = "https://eth-mainnet.your-provider.com/KEY"
wss_url = "wss://eth-mainnet.your-provider.com/KEY"
weight = 2  # Higher weight = more traffic
timeout_seconds = 30
circuit_breaker_threshold = 3  # Failures before circuit opens

[cache]
enabled = true

[cache.manager_config]
retain_blocks = 2000  # Blocks to keep in cache

[auth]
enabled = true
database_url = "sqlite:///app/db/auth.db"

[metrics]
enabled = true
prometheus_port = 9090

[logging]
level = "info"
format = "json"  # Use "json" for production
```

### Validating Configuration

```bash
# Using the CLI
cargo run --bin cli -- config validate

# Or with Docker
docker run --rm \
  -v $(pwd)/config/config.toml:/app/config/config.toml:ro \
  ghcr.io/prismrpc/prism:latest \
  prism-cli config validate
```

---

## Monitoring

### Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /health` | Health status with upstream info |
| `GET /metrics` | Prometheus metrics |

### Prometheus Metrics

Key metrics to monitor:

```
# Request metrics
prism_requests_total{method,status}
prism_request_duration_seconds{method,quantile}

# Cache metrics
prism_cache_hits_total{cache_type}
prism_cache_misses_total{cache_type}

# Upstream metrics
prism_upstream_requests_total{upstream,status}
prism_upstream_response_time_seconds{upstream}
prism_upstream_health{upstream}  # 1 = healthy, 0 = unhealthy
```

### Grafana Dashboard

When using Docker Compose with monitoring profile:

1. Access Grafana at http://localhost:3001
2. Login: admin/admin (change on first login)
3. Navigate to Dashboards > Prism

---

## Production Recommendations

### 1. Multiple Upstream Providers

Configure at least 3 RPC providers for redundancy:

```toml
[[upstreams.providers]]
name = "alchemy"
https_url = "https://eth-mainnet.g.alchemy.com/v2/KEY"
weight = 3

[[upstreams.providers]]
name = "infura"
https_url = "https://mainnet.infura.io/v3/KEY"
weight = 2

[[upstreams.providers]]
name = "quicknode"
https_url = "https://your-endpoint.quiknode.pro/KEY"
weight = 1
```

### 2. Enable Authentication

Protect your endpoint with API keys:

```toml
[auth]
enabled = true
database_url = "sqlite:///app/db/auth.db"
```

Create API keys:
```bash
prism-cli auth create \
  --name "my-service" \
  --rate-limit 100 \
  --daily-limit 100000
```

### 3. Use a Reverse Proxy

Deploy behind nginx or Caddy for:
- TLS termination
- Additional rate limiting
- Request logging

Example nginx configuration:

```nginx
upstream prism {
    server 127.0.0.1:3030;
}

server {
    listen 443 ssl http2;
    server_name rpc.example.com;

    ssl_certificate /etc/ssl/certs/rpc.crt;
    ssl_certificate_key /etc/ssl/private/rpc.key;

    location / {
        proxy_pass http://prism;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 60s;
    }
}
```

### 4. Resource Limits

Set appropriate limits in Docker Compose:

```yaml
deploy:
  resources:
    limits:
      cpus: '2.0'
      memory: 2G
    reservations:
      cpus: '0.5'
      memory: 512M
```

### 5. Log Aggregation

Use JSON logging for production:

```toml
[logging]
level = "info"
format = "json"
```

Integrate with your log aggregation system (Loki, ELK, etc.).

### 6. Backup SQLite Database

If using authentication, backup the SQLite database regularly:

```bash
# Simple backup
sqlite3 /path/to/auth.db ".backup /path/to/backup/auth.db"

# Scheduled backup (cron)
0 */6 * * * sqlite3 /opt/prism/db/auth.db ".backup /backups/auth-$(date +\%Y\%m\%d-\%H\%M).db"
```

---

## Troubleshooting

### Check Health Status

```bash
curl http://localhost:3030/health | jq
```

### View Logs

```bash
# Docker
docker logs prism

# Systemd
journalctl -u prism -f

# Enable debug logging
RUST_LOG=debug ./prism-server
```

### Test Upstreams

```bash
prism-cli config test-upstreams
```

### Common Issues

**Container won't start:**
- Check configuration syntax: `prism-cli config validate`
- Ensure `bind_address = "0.0.0.0"` for Docker

**No upstream connections:**
- Verify RPC endpoint URLs
- Check firewall rules
- Test endpoints manually with curl

**High memory usage:**
- Reduce cache sizes in configuration
- Lower `retain_blocks` value

**Authentication database errors:**
- Ensure `/app/db` volume is mounted
- Check directory permissions

---

## Next Steps

- Read the [Architecture Documentation](./architecture.md)
- Learn about [Upstream Routing](./components/upstream/upstream_manager.md)
- Explore [Consensus Validation](./components/consensus/README.md)
