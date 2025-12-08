# Admin API Security Guide

This guide covers security best practices for deploying and operating the Prism Admin API.

## Table of Contents

- [Authentication](#authentication)
- [Rate Limiting](#rate-limiting)
- [Network Security](#network-security)
- [Development Mode](#development-mode)
- [Swagger UI Security](#swagger-ui-security)
- [Audit Logging](#audit-logging)
- [Security Checklist](#security-checklist)

## Authentication

### Token-Based Authentication

The Admin API uses a simple token-based authentication scheme with constant-time comparison to prevent timing attacks.

#### Configuration

Configure an admin token in your config file or via environment variable:

**Option 1: Configuration file (NOT RECOMMENDED for production)**
```toml
[admin]
admin_token = "your-secure-token-here"
```

**Option 2: Environment variable (RECOMMENDED)**
```bash
export PRISM_ADMIN__ADMIN_TOKEN="your-secure-token-here"
```

#### Making Authenticated Requests

Include the token in all requests using the `X-Admin-Token` header:

```bash
curl -H "X-Admin-Token: your-secure-token-here" \
  http://localhost:3031/admin/system/status
```

Example with jq for JSON formatting:
```bash
curl -s -H "X-Admin-Token: your-secure-token-here" \
  http://localhost:3031/admin/system/status | jq
```

### Security Properties

The authentication middleware provides several security features:

- **Constant-time comparison**: Token validation uses `subtle::ConstantTimeEq` to prevent timing attacks that could leak information about the expected token
- **No default token**: When `admin_token` is not set, authentication is disabled (development mode only - see warning below)
- **Header-based**: Token is sent via HTTP header, not in URL (prevents logging in proxy/web server logs)
- **Case-sensitive**: Token comparison is case-sensitive and exact - no partial matches accepted
- **No whitespace tolerance**: Leading/trailing whitespace will cause authentication to fail

### Token Generation Recommendations

Generate a cryptographically secure random token:

**Using OpenSSL (32 bytes = 256 bits):**
```bash
openssl rand -base64 32
```

**Using /dev/urandom:**
```bash
head -c 32 /dev/urandom | base64
```

**Using Python:**
```python
import secrets
print(secrets.token_urlsafe(32))
```

#### Token Requirements

Your admin token should have the following properties:

- **Minimum length**: At least 32 characters (256 bits of entropy recommended)
- **High entropy**: Use random bytes, not dictionary words or predictable patterns
- **Unique per environment**: Use different tokens for development, staging, and production
- **Stored securely**: Never commit tokens to version control
- **Rotated regularly**: Change tokens periodically (quarterly recommended)

#### What NOT to Use as Tokens

- ❌ Simple passwords like "admin123" or "password"
- ❌ Predictable patterns like "admin-token-2024"
- ❌ Short tokens (less than 20 characters)
- ❌ The same token across multiple environments
- ❌ Tokens committed to Git repositories

## Rate Limiting

The Admin API includes built-in per-IP rate limiting using a token bucket algorithm to prevent abuse and denial-of-service attacks.

### How It Works

The rate limiter uses a **token bucket algorithm**:

1. Each client IP address gets its own bucket
2. Buckets start with `max_tokens` tokens (burst capacity)
3. Tokens refill at `refill_rate` per second (sustained rate)
4. Each request consumes 1 token
5. If a bucket has less than 1 token, the request is rejected with `429 Too Many Requests`
6. Idle buckets are automatically cleaned up after 5 minutes

### Configuration

```toml
[admin]
rate_limit_max_tokens = 100    # Burst capacity: 100 requests
rate_limit_refill_rate = 10    # Sustained rate: 10 requests/sec
```

Both settings must be configured to enable rate limiting. If either is omitted, rate limiting is disabled.

### Recommended Settings

Choose settings based on your security requirements and usage patterns:

| Environment | max_tokens | refill_rate | Use Case |
|-------------|------------|-------------|----------|
| **Development** | 1000 | 100 | Local testing, no restrictions |
| **Production (Standard)** | 100 | 10 | Normal operations, moderate security |
| **Production (High Security)** | 20 | 2 | High-security environments, strict limiting |
| **CI/CD Pipeline** | 500 | 50 | Automated deployments and health checks |

### Disabling Rate Limiting

To disable rate limiting (not recommended for production):

```toml
[admin]
# Omit rate_limit_max_tokens and rate_limit_refill_rate
# or set them to null/None
```

### Understanding Rate Limit Responses

When rate limited, the API returns:

```
HTTP/1.1 429 Too Many Requests
```

The client should implement exponential backoff before retrying.

### Rate Limit Examples

**Burst capacity example (max_tokens = 100, refill_rate = 10):**
- Initial burst: 100 requests immediately
- Request 101: Rejected with 429
- After 1 second: 110 requests available (100 - 100 + 10)
- After 10 seconds: Back to full capacity (100 tokens)

**Sustained rate example:**
- With refill_rate = 10/sec, you can sustain 10 requests/second indefinitely
- Bursting to 100 requests uses your buffer, which takes 10 seconds to refill completely

### IPv4 and IPv6 Isolation

Rate limits are strictly isolated:

- IPv4 addresses have separate buckets from IPv6 addresses
- Each unique IP address gets its own bucket
- `127.0.0.1` and `::1` are treated as different clients

## Network Security

### Production Deployment Best Practices

#### 1. Bind to Internal Interface Only

**Default configuration (secure):**
```toml
[admin]
bind_address = "127.0.0.1"  # Localhost only
port = 3031
```

**For internal network access:**
```toml
[admin]
bind_address = "10.0.0.5"   # Specific internal IP
port = 3031
```

**WARNING - Never do this in production:**
```toml
[admin]
bind_address = "0.0.0.0"    # ❌ Binds to all interfaces, exposes to internet
```

#### 2. Use a Reverse Proxy

Place nginx, HAProxy, or a similar reverse proxy in front of the Admin API for:

- **TLS termination** (HTTPS)
- **Additional authentication** (mTLS, OAuth2, etc.)
- **IP whitelisting**
- **Request logging**
- **DDoS protection**

**Example nginx configuration:**

```nginx
server {
    listen 443 ssl http2;
    server_name admin.prism.internal;

    # TLS configuration
    ssl_certificate /etc/nginx/ssl/admin.prism.internal.crt;
    ssl_certificate_key /etc/nginx/ssl/admin.prism.internal.key;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers HIGH:!aNULL:!MD5;

    # IP whitelisting (optional)
    allow 10.0.0.0/8;      # Internal network
    allow 192.168.1.0/24;  # Office network
    deny all;

    # Proxy to Admin API
    location / {
        proxy_pass http://127.0.0.1:3031;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # Timeouts for long-running operations
        proxy_connect_timeout 60s;
        proxy_send_timeout 60s;
        proxy_read_timeout 60s;
    }
}
```

**Example HAProxy configuration:**

```
frontend admin_https
    bind *:443 ssl crt /etc/haproxy/certs/admin.prism.internal.pem
    mode http
    default_backend admin_api

backend admin_api
    mode http
    balance roundrobin
    option httpchk GET /admin/system/health
    server prism1 127.0.0.1:3031 check
```

#### 3. Firewall Rules

Configure your firewall to restrict access to the admin port:

**iptables example (Linux):**
```bash
# Allow from specific IP
iptables -A INPUT -p tcp --dport 3031 -s 10.0.0.0/8 -j ACCEPT

# Deny all others
iptables -A INPUT -p tcp --dport 3031 -j DROP
```

**UFW example (Ubuntu/Debian):**
```bash
# Allow from internal network only
ufw allow from 10.0.0.0/8 to any port 3031

# Or allow from specific IP
ufw allow from 10.0.1.50 to any port 3031
```

**AWS Security Group example:**
```json
{
  "IpPermissions": [
    {
      "FromPort": 3031,
      "ToPort": 3031,
      "IpProtocol": "tcp",
      "IpRanges": [
        {
          "CidrIp": "10.0.0.0/8",
          "Description": "Internal network access to Admin API"
        }
      ]
    }
  ]
}
```

#### 4. Service Mesh with mTLS

For microservices deployments, use a service mesh (Istio, Linkerd, Consul Connect) for:

- Mutual TLS (mTLS) between services
- Service-to-service authentication
- Automatic certificate rotation
- Traffic encryption

### TLS/HTTPS Configuration

The Admin API serves HTTP by default. For production, you MUST add HTTPS:

**Option 1: Reverse proxy with TLS termination (recommended)**
- See nginx/HAProxy examples above
- Simplest to manage certificates
- Can use Let's Encrypt for automated certificates

**Option 2: Service mesh with mTLS**
- Istio, Linkerd, or Consul Connect
- Automatic certificate management
- Zero-trust networking

**Option 3: Application-level TLS (future enhancement)**
- Currently not supported in Prism
- Would require adding TLS configuration to Admin API server

## Development Mode

### Understanding Development Mode

When `admin_token` is **not configured**:

- ✅ All requests are allowed without authentication
- ⚠️ A warning is logged at server startup:
  ```
  WARN: Admin API authentication is disabled (no admin_token configured)
  ```
- ❌ **NEVER use in production**

### Safe Development Practices

For local development:

```toml
[admin]
enabled = true
bind_address = "127.0.0.1"  # Localhost only
port = 3031
# admin_token not set = development mode
```

This is safe because:
- Only accessible from localhost
- No network exposure
- Convenient for local testing

### Transitioning to Production

Before deploying to production:

1. ✅ Set a strong `admin_token` via environment variable
2. ✅ Verify authentication is required (`401 Unauthorized` without token)
3. ✅ Configure rate limiting
4. ✅ Bind to internal interface or localhost
5. ✅ Set up TLS termination
6. ✅ Configure firewall rules

## Swagger UI Security

The Swagger UI is available at `/admin/swagger-ui` and provides interactive API documentation.

### Current Behavior

- **No authentication required** for Swagger UI itself
- The UI is served at `/admin/swagger-ui`
- API requests made through Swagger UI **do** require authentication

### Security Considerations

The Swagger UI endpoint exposes:
- ✅ API structure and endpoints (documentation only)
- ✅ Request/response schemas
- ❌ **NO** actual data or mutations without auth token
- ❌ **NO** ability to execute operations without providing valid `X-Admin-Token`

### Mitigation Options

#### Option 1: Accept the Risk (Recommended for Internal Networks)

If the Admin API is properly isolated to internal networks:
- Swagger UI exposure is acceptable
- Helps with internal developer onboarding
- Provides self-documenting API

#### Option 2: Restrict via Reverse Proxy

Use nginx to block Swagger UI in production:

```nginx
location /admin/swagger-ui {
    # Restrict to specific IPs
    allow 192.168.1.0/24;  # Office network
    deny all;

    proxy_pass http://127.0.0.1:3031;
}

location /admin/ {
    # All other admin endpoints
    proxy_pass http://127.0.0.1:3031;
}
```

#### Option 3: Disable in Production (Future Enhancement)

Request this feature if needed:
```toml
[admin]
enable_swagger_ui = false  # Not yet implemented
```

## Audit Logging

The Admin API logs all operations for security audit purposes.

### What Gets Logged

All mutation operations (create, update, delete) are logged with:

- **Timestamp**: When the operation occurred
- **Operation**: Type of operation (e.g., "create_upstream", "delete_api_key")
- **Resource**: What was affected (e.g., "upstream:alchemy", "apikey:key_abc123")
- **Client IP**: Source IP address (from connection info)
- **Outcome**: Success or failure
- **Error details**: If the operation failed

### Example Log Entries

```
2024-12-07T10:23:45Z INFO admin_api: upstream_created upstream=alchemy ip=10.0.1.50
2024-12-07T10:24:12Z INFO admin_api: circuit_breaker_reset upstream=infura ip=10.0.1.50
2024-12-07T10:25:33Z WARN admin_api: api_key_deletion_failed id=key_123 ip=10.0.1.50 error="Key not found"
2024-12-07T10:30:15Z WARN admin_api: authentication_failed ip=192.168.1.100 reason="Invalid token"
2024-12-07T10:31:22Z WARN admin_api: rate_limit_exceeded ip=192.168.1.100
```

### Setting Up Centralized Logging

**For production, send logs to a centralized system:**

#### Option 1: File-based logging with log shipping

```toml
[logging]
level = "info"
format = "json"  # Structured logs for parsing
```

Then use:
- **Fluentd/Fluent Bit**: Ship logs to Elasticsearch, S3, or other destinations
- **Filebeat**: ELK stack integration
- **Vector**: Modern log aggregation

#### Option 2: Direct integration (future enhancement)

Future support for direct log shipping:
- OpenTelemetry
- Datadog
- New Relic
- CloudWatch Logs

### Monitoring for Security Events

Set up alerts for suspicious activity:

1. **Failed authentication attempts**
   - Pattern: `authentication_failed`
   - Threshold: >10 failures from same IP in 5 minutes

2. **Rate limit violations**
   - Pattern: `rate_limit_exceeded`
   - Threshold: >5 violations from same IP in 1 minute

3. **Multiple failed operations**
   - Pattern: `*_failed` log entries
   - Threshold: >20 failures in 5 minutes

4. **Unauthorized access patterns**
   - Multiple 401 responses
   - Access from unexpected IP ranges
   - Requests outside business hours

### Log Retention

Recommendations:
- **Hot storage**: 30 days (for immediate investigation)
- **Warm storage**: 90 days (for compliance)
- **Cold storage**: 1-7 years (for audits, depending on regulations)

## Security Checklist

### Pre-Production Checklist

Before deploying the Admin API to production:

- [ ] **Set strong `admin_token`** (32+ characters, high entropy)
- [ ] **Store token as environment variable**, not in config file
- [ ] **Configure rate limiting** (recommended: max_tokens=100, refill_rate=10)
- [ ] **Bind to internal interface** (127.0.0.1 or specific internal IP)
- [ ] **Set up TLS termination** (nginx, HAProxy, or service mesh)
- [ ] **Configure firewall rules** (whitelist internal IPs only)
- [ ] **Test authentication works** (verify 401 without token, 200 with token)
- [ ] **Verify rate limiting works** (test 429 after burst)
- [ ] **Set up centralized logging** (Fluentd, ELK, etc.)
- [ ] **Configure log alerts** (for auth failures, rate limits)
- [ ] **Document admin token storage location** (secrets manager, vault, etc.)
- [ ] **Review Swagger UI exposure** (decide if it needs restricting)
- [ ] **Test from external network** (should be blocked)
- [ ] **Perform security scan** (OWASP ZAP, Burp Suite, etc.)

### Ongoing Security Operations

Regular security maintenance:

- [ ] **Rotate admin token quarterly** (or after personnel changes)
- [ ] **Review audit logs weekly** (check for anomalies)
- [ ] **Monitor for 401/429 responses** (failed auth, rate limits)
- [ ] **Update firewall rules as needed** (when team members change)
- [ ] **Keep Prism updated** (security patches)
- [ ] **Review access patterns monthly** (unusual usage spikes)
- [ ] **Test disaster recovery** (token rotation procedure)
- [ ] **Audit API key usage** (if API key management is enabled)
- [ ] **Review rate limit effectiveness** (adjust if legitimate traffic is blocked)
- [ ] **Validate TLS configuration** (SSL Labs scan for reverse proxy)

### Incident Response

If you suspect unauthorized access:

1. **Immediately rotate the admin token**
2. **Review audit logs** for unauthorized operations
3. **Check for created/modified resources** (upstreams, API keys, settings)
4. **Verify no malicious changes** were made to configuration
5. **Investigate source of compromise** (log analysis, forensics)
6. **Update firewall rules** if needed
7. **Document the incident** for future reference

### Security Hardening (Advanced)

For high-security environments:

- [ ] **Implement IP whitelisting** at application level (future enhancement)
- [ ] **Add request signing** in addition to token auth (future enhancement)
- [ ] **Enable mTLS** between admin clients and API
- [ ] **Use a bastion host** for admin access (jump box pattern)
- [ ] **Implement 2FA** for admin token access (secrets vault with MFA)
- [ ] **Set up honeypot endpoints** to detect scanning
- [ ] **Implement geo-blocking** if admin access is region-specific
- [ ] **Use a Web Application Firewall (WAF)** for additional protection
- [ ] **Conduct regular penetration testing**
- [ ] **Implement security headers** via reverse proxy (CSP, HSTS, etc.)

## Additional Resources

- **Admin API Overview**: `/home/flo/workspace/personal/prism/docs/admin-api/overview.md`
- **Deployment Guide**: `/home/flo/workspace/personal/prism/docs/deployment.md`
- **Configuration Reference**: `/home/flo/workspace/personal/prism/crates/prism-core/src/config/mod.rs`
- **Authentication Middleware**: `/home/flo/workspace/personal/prism/crates/server/src/admin/middleware.rs`
- **Rate Limiting Implementation**: `/home/flo/workspace/personal/prism/crates/server/src/admin/rate_limit.rs`

## Questions or Issues?

If you discover a security vulnerability, please report it responsibly:

1. **DO NOT** open a public GitHub issue
2. Email the maintainers directly (see `SECURITY.md` if available)
3. Provide details about the vulnerability
4. Allow time for patching before public disclosure

For security questions or hardening advice, open a GitHub Discussion.
