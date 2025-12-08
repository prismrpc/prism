# Admin API Configuration Reference

This document describes all configuration options for the Prism Admin API. The Admin API provides a separate HTTP server for monitoring and managing the Prism RPC server, running on a different port but within the same process.

## Table of Contents

- [Configuration File Location](#configuration-file-location)
- [Settings Reference](#settings-reference)
- [Environment Variables](#environment-variables)
- [Configuration Examples](#configuration-examples)
- [Security Recommendations](#security-recommendations)
- [Validation Rules](#validation-rules)

## Configuration File Location

Admin API settings are defined in the `[admin]` section of your TOML configuration file. By default, Prism loads configuration from `config/config.toml`, but you can override this using the `PRISM_CONFIG` environment variable:

```bash
PRISM_CONFIG=config/production.toml cargo run --bin server
```

## Settings Reference

### `enabled`

- **Type**: Boolean
- **Default**: `false`
- **Required**: No
- **Description**: Controls whether the Admin API server is started. When disabled, no admin endpoints are available, and no admin server is started.
- **Example**:
  ```toml
  [admin]
  enabled = true
  ```

### `bind_address`

- **Type**: String
- **Default**: `"127.0.0.1"` (localhost only)
- **Required**: No
- **Description**: IP address to bind the Admin API server to. For security, defaults to localhost only. Set to `"0.0.0.0"` to accept connections from any interface (not recommended for production without proper network security).
- **Security Note**: Binding to `0.0.0.0` exposes the admin interface to all network interfaces. Only do this if you have proper firewall rules and authentication configured.
- **Example**:
  ```toml
  bind_address = "127.0.0.1"  # Localhost only (recommended)
  # bind_address = "0.0.0.0"  # All interfaces (use with caution)
  ```

### `port`

- **Type**: Integer (u16)
- **Default**: `3031`
- **Required**: No
- **Description**: TCP port number for the Admin API server. Must be different from the main RPC server port (default 3030). Must be greater than 0.
- **Example**:
  ```toml
  port = 3031
  ```

### `instance_name`

- **Type**: String
- **Default**: System hostname (falls back to `"prism"` if hostname cannot be determined)
- **Required**: No
- **Description**: Human-readable identifier for this Prism instance. Useful in multi-instance deployments for identifying which instance you're managing. Returned in system info endpoints.
- **Example**:
  ```toml
  instance_name = "prism-prod-1"
  ```

### `admin_token`

- **Type**: String (optional)
- **Default**: `None` (no authentication)
- **Required**: No
- **Description**: Authentication token required for all Admin API requests. When set, clients must include the `X-Admin-Token: <token>` header with every request. When not set, the Admin API accepts all requests without authentication.
- **Security Warning**: If not set in production, a warning is logged at startup. Always set this in production environments.
- **Validation**:
  - Must not be empty or whitespace-only
  - Should be at least 16 characters (warning logged if shorter)
- **Example**:
  ```toml
  admin_token = "super-secret-admin-token-change-me-in-production"
  ```
- **Best Practice**: Set via environment variable rather than committing to config files:
  ```bash
  PRISM_ADMIN__ADMIN_TOKEN=your-secure-token cargo run
  ```

### `prometheus_url`

- **Type**: String (optional)
- **Default**: `None`
- **Required**: No
- **Description**: URL of a Prometheus server for querying historical metrics data. When configured, metrics endpoints can return time-series data from Prometheus. When not set, metrics endpoints return only in-memory data collected since server startup.
- **URL Format**: Must be a complete HTTP(S) URL including protocol and port
- **Example**:
  ```toml
  prometheus_url = "http://localhost:9090"
  prometheus_url = "http://prometheus:9090"  # In Docker Compose
  ```
- **Notes**:
  - The Prism server must have network access to this Prometheus instance
  - If initialization fails, a warning is logged but the server continues without Prometheus integration
  - See the [Prometheus Integration Guide](./prometheus-integration.md) for setup details

### `rate_limit_max_tokens`

- **Type**: Integer (u32, optional)
- **Default**: `Some(100)`
- **Required**: No
- **Description**: Maximum number of tokens in the rate limiter token bucket (burst capacity). Determines how many requests can be made in a short burst before rate limiting kicks in. Must be set together with `rate_limit_refill_rate`. Set to `None` or omit both rate limit settings to disable rate limiting.
- **Example**:
  ```toml
  rate_limit_max_tokens = 100  # Allow bursts of up to 100 requests
  ```
- **Notes**:
  - Uses a token bucket algorithm
  - Each request consumes 1 token
  - If set, `rate_limit_refill_rate` must also be set

### `rate_limit_refill_rate`

- **Type**: Integer (u32, optional)
- **Default**: `Some(10)`
- **Required**: No
- **Description**: Number of tokens added to the bucket per second (sustained rate limit). Controls the long-term request rate. Must be set together with `rate_limit_max_tokens`. Set to `None` or omit both rate limit settings to disable rate limiting.
- **Example**:
  ```toml
  rate_limit_refill_rate = 10  # Allow sustained rate of 10 requests/second
  ```
- **Notes**:
  - Uses a token bucket algorithm
  - Tokens refill continuously, not in discrete intervals
  - If set, `rate_limit_max_tokens` must also be set

## Environment Variables

All configuration settings can be overridden using environment variables with the `PRISM_` prefix. Use double underscores (`__`) as separators for nested fields.

### Admin API Environment Variables

| Environment Variable | Config Field | Type | Example |
|---------------------|--------------|------|---------|
| `PRISM_ADMIN__ENABLED` | `admin.enabled` | Boolean | `true` |
| `PRISM_ADMIN__BIND_ADDRESS` | `admin.bind_address` | String | `127.0.0.1` |
| `PRISM_ADMIN__PORT` | `admin.port` | Integer | `3031` |
| `PRISM_ADMIN__INSTANCE_NAME` | `admin.instance_name` | String | `prism-prod-1` |
| `PRISM_ADMIN__ADMIN_TOKEN` | `admin.admin_token` | String | `your-secure-token` |
| `PRISM_ADMIN__PROMETHEUS_URL` | `admin.prometheus_url` | String | `http://localhost:9090` |
| `PRISM_ADMIN__RATE_LIMIT_MAX_TOKENS` | `admin.rate_limit_max_tokens` | Integer | `100` |
| `PRISM_ADMIN__RATE_LIMIT_REFILL_RATE` | `admin.rate_limit_refill_rate` | Integer | `10` |

### Environment Variable Precedence

Configuration is loaded in this order (later overrides earlier):

1. **Compiled defaults**: Hardcoded default values in the application
2. **Config file**: TOML file specified by `PRISM_CONFIG` (defaults to `config/config.toml`)
3. **Environment variables**: `PRISM_*` environment variables

### Using Environment Variables

```bash
# Enable admin API and set authentication token
PRISM_ADMIN__ENABLED=true \
PRISM_ADMIN__ADMIN_TOKEN=my-secure-token \
cargo run --bin server

# Configure admin API with Prometheus and custom port
PRISM_ADMIN__ENABLED=true \
PRISM_ADMIN__PORT=3032 \
PRISM_ADMIN__PROMETHEUS_URL=http://prometheus:9090 \
PRISM_ADMIN__ADMIN_TOKEN=my-secure-token \
cargo run --bin server
```

## Configuration Examples

### Development Configuration

Minimal configuration for local development with no authentication:

```toml
[admin]
enabled = true
bind_address = "127.0.0.1"
port = 3031
# admin_token not set - no authentication (development only!)
rate_limit_max_tokens = 100
rate_limit_refill_rate = 10
```

### Production Configuration

Secure configuration for production deployment:

```toml
[admin]
enabled = true
bind_address = "127.0.0.1"  # Localhost only - use reverse proxy for external access
port = 3031
instance_name = "prism-prod-us-east-1a"
# admin_token set via PRISM_ADMIN__ADMIN_TOKEN environment variable
prometheus_url = "http://prometheus:9090"
rate_limit_max_tokens = 200
rate_limit_refill_rate = 20
```

With environment variable:
```bash
PRISM_ADMIN__ADMIN_TOKEN="$(openssl rand -base64 32)" cargo run --bin server
```

### Docker Compose Configuration

Configuration for containerized deployment:

```toml
[admin]
enabled = true
bind_address = "0.0.0.0"  # Accept connections from other containers
port = 3031
instance_name = "prism-container"
prometheus_url = "http://prometheus:9090"
rate_limit_max_tokens = 150
rate_limit_refill_rate = 15
```

Docker Compose service:
```yaml
services:
  prism:
    image: prism:latest
    environment:
      - PRISM_ADMIN__ADMIN_TOKEN=${ADMIN_TOKEN}
    ports:
      - "3031:3031"  # Expose admin API
    networks:
      - prism-network
```

### High-Traffic Production

Configuration for high-traffic environments with increased rate limits:

```toml
[admin]
enabled = true
bind_address = "127.0.0.1"
port = 3031
instance_name = "prism-prod-high-traffic"
prometheus_url = "http://prometheus:9090"
rate_limit_max_tokens = 500   # Higher burst capacity
rate_limit_refill_rate = 50   # Higher sustained rate
```

### Disabled Rate Limiting

Configuration without rate limiting (not recommended for production):

```toml
[admin]
enabled = true
bind_address = "127.0.0.1"
port = 3031
admin_token = "my-token"
# Omit rate limit settings to disable
# rate_limit_max_tokens = <not set>
# rate_limit_refill_rate = <not set>
```

## Security Recommendations

### Authentication

1. **Always set `admin_token` in production environments**
   - Generate a strong, random token (minimum 32 characters)
   - Use `openssl rand -base64 32` or similar tools
   - Never commit tokens to version control

2. **Use environment variables for secrets**
   ```bash
   # Good
   PRISM_ADMIN__ADMIN_TOKEN="$(cat /run/secrets/admin_token)"

   # Bad - don't hardcode in config files
   admin_token = "hardcoded-token-123"
   ```

3. **Rotate tokens periodically**
   - Establish a token rotation policy (e.g., every 90 days)
   - Update tokens in your secrets management system

### Network Security

1. **Bind to localhost by default**
   - Use `bind_address = "127.0.0.1"` for direct access
   - Use a reverse proxy (nginx, Caddy) for remote access
   - Never expose admin API directly to the internet

2. **Use firewall rules**
   ```bash
   # Allow admin access only from specific IPs
   iptables -A INPUT -p tcp --dport 3031 -s 10.0.0.0/8 -j ACCEPT
   iptables -A INPUT -p tcp --dport 3031 -j DROP
   ```

3. **Consider VPN or SSH tunneling**
   ```bash
   # SSH tunnel for secure remote access
   ssh -L 3031:localhost:3031 user@prism-server
   ```

### Rate Limiting

1. **Always enable rate limiting in production**
   - Prevents abuse and denial-of-service attacks
   - Default settings (100 burst, 10/sec) are reasonable for most use cases

2. **Adjust based on monitoring needs**
   - Increase limits for dashboards that poll frequently
   - Decrease limits for public-facing admin interfaces

3. **Monitor rate limit hits**
   - Track 429 (Too Many Requests) responses
   - Adjust limits if legitimate traffic is being blocked

### TLS/HTTPS

The Admin API itself doesn't implement TLS. For production deployments:

1. **Use a reverse proxy with TLS**
   ```nginx
   # nginx configuration
   server {
       listen 443 ssl;
       server_name admin.prism.example.com;

       ssl_certificate /path/to/cert.pem;
       ssl_certificate_key /path/to/key.pem;

       location / {
           proxy_pass http://localhost:3031;
           proxy_set_header X-Admin-Token $http_x_admin_token;
       }
   }
   ```

2. **Or use SSH tunneling** (see Network Security section)

### Monitoring

1. **Monitor admin API access**
   - Log all admin API requests
   - Alert on failed authentication attempts
   - Track which endpoints are being used

2. **Audit configuration changes**
   - Review settings updates via `/admin/system/settings`
   - Track upstream modifications
   - Monitor cache clear operations

## Validation Rules

The configuration is validated at startup. The server will fail to start if validation fails.

### Required Validations

1. **Port must be greater than 0**
   ```
   Error: "Bind port must be greater than 0"
   ```

2. **Admin token cannot be empty**
   ```
   Error: "Admin token cannot be empty"
   ```

### Warnings

1. **Admin API enabled without authentication (non-development)**
   ```
   WARN: SECURITY WARNING: Admin API is enabled without authentication!
         Set PRISM_ADMIN__ADMIN_TOKEN environment variable to secure the admin API.
   ```

2. **Admin token shorter than 16 characters**
   ```
   WARN: Admin token is shorter than recommended minimum of 16 characters.
         Consider using a longer, more secure token.
   ```

3. **Prometheus initialization failure**
   ```
   WARN: failed to initialize Prometheus client: <error details>
   ```
   Note: This is a warning, not an error. The server continues without Prometheus integration.

### Rate Limiting Validation

Both `rate_limit_max_tokens` and `rate_limit_refill_rate` must be set together:
- If one is set and the other is not, rate limiting is disabled
- If both are omitted or both are `None`, rate limiting is disabled
- If both are set, rate limiting is enabled with those values

## Related Documentation

- [Admin API Overview](./README.md) - General introduction to the Admin API
- [Admin API Endpoints](./endpoints.md) - Complete endpoint reference
- [Prometheus Integration](./prometheus-integration.md) - Setting up Prometheus metrics
- [Authentication Guide](./authentication.md) - Detailed authentication setup
- [Deployment Guide](../deployment.md) - Production deployment best practices

## Troubleshooting

### Admin API not accessible

1. **Check if enabled**
   ```bash
   # Verify enabled in config
   grep "enabled" config/development.toml
   ```

2. **Check port binding**
   ```bash
   # Verify port is listening
   netstat -tlnp | grep 3031
   ```

3. **Check firewall rules**
   ```bash
   # Test connection
   curl http://localhost:3031/admin/system/health
   ```

### Authentication failures

1. **Verify token is set**
   ```bash
   # Check environment variable
   echo $PRISM_ADMIN__ADMIN_TOKEN
   ```

2. **Verify header in request**
   ```bash
   # Test with correct header
   curl -H "X-Admin-Token: your-token" http://localhost:3031/admin/system/health
   ```

### Rate limiting issues

1. **Check current limits**
   ```bash
   curl -H "X-Admin-Token: your-token" http://localhost:3031/admin/system/settings
   ```

2. **Monitor rate limit headers**
   ```bash
   curl -I -H "X-Admin-Token: your-token" http://localhost:3031/admin/system/health
   # Look for X-RateLimit-* headers
   ```

### Prometheus integration not working

1. **Verify Prometheus URL is accessible**
   ```bash
   curl http://localhost:9090/api/v1/query?query=up
   ```

2. **Check server logs for initialization errors**
   ```bash
   # Look for Prometheus-related warnings
   tail -f prism.log | grep -i prometheus
   ```

3. **Verify Prometheus is scraping Prism metrics**
   ```bash
   # Check Prism appears in Prometheus targets
   curl http://localhost:9090/api/v1/targets | grep prism
   ```
