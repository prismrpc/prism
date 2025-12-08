# Admin API Authentication Usage Guide

## Quick Start

### Enable Authentication

Add the following to your configuration file (e.g., `config/development.toml`):

```toml
[admin]
enabled = true
admin_token = "your-secure-token-here"
```

### Make Authenticated Requests

Include the `X-Admin-Token` header in all admin API requests:

```bash
curl -H "X-Admin-Token: your-secure-token-here" \
     http://localhost:3031/admin/system/status
```

## Configuration Options

### Option 1: Configuration File

```toml
[admin]
enabled = true
admin_token = "dev-admin-token-change-in-production"
```

### Option 2: Environment Variable

```bash
export PRISM__ADMIN__ADMIN_TOKEN="production-token"
cargo run --bin server
```

### Option 3: Disable Authentication (Development Only)

```toml
[admin]
enabled = true
# admin_token = ""  # Omit or leave empty to disable auth
```

## Examples

### System Status

```bash
curl -H "X-Admin-Token: dev-admin-token-change-in-production" \
     http://localhost:3031/admin/system/status
```

### List Upstreams

```bash
curl -H "X-Admin-Token: dev-admin-token-change-in-production" \
     http://localhost:3031/admin/upstreams
```

### Get Cache Stats

```bash
curl -H "X-Admin-Token: dev-admin-token-change-in-production" \
     http://localhost:3031/admin/cache/stats
```

### Trigger Health Check

```bash
curl -X POST \
     -H "X-Admin-Token: dev-admin-token-change-in-production" \
     http://localhost:3031/admin/upstreams/{id}/health-check
```

## Error Responses

### Missing Token

```bash
curl http://localhost:3031/admin/system/status
```

Response: `401 Unauthorized`

### Invalid Token

```bash
curl -H "X-Admin-Token: wrong-token" \
     http://localhost:3031/admin/system/status
```

Response: `401 Unauthorized`

## Security Best Practices

1. **Use Strong Tokens**: Generate cryptographically random tokens
   ```bash
   openssl rand -base64 32
   ```

2. **Environment Variables**: Store tokens in environment variables, not in code
   ```bash
   export PRISM__ADMIN__ADMIN_TOKEN=$(openssl rand -base64 32)
   ```

3. **HTTPS Only**: Use HTTPS in production to prevent token interception

4. **Token Rotation**: Implement regular token rotation for production deployments

5. **Secret Management**: Use a secrets manager (e.g., HashiCorp Vault, AWS Secrets Manager)

6. **Never Commit Tokens**: Add tokens to `.gitignore` and use environment-specific configs

## Testing Authentication

### Development Mode (No Auth)

```bash
# Start server without token
cargo run --bin server

# Access admin API without authentication
curl http://localhost:3031/admin/system/status
```

### Production Mode (Auth Required)

```bash
# Start server with token
PRISM__ADMIN__ADMIN_TOKEN="test-token" cargo run --bin server

# Access admin API with authentication
curl -H "X-Admin-Token: test-token" \
     http://localhost:3031/admin/system/status

# Test invalid token (should return 401)
curl -H "X-Admin-Token: wrong-token" \
     http://localhost:3031/admin/system/status
```

## Integration with Tools

### Using with HTTPie

```bash
http GET localhost:3031/admin/system/status \
     X-Admin-Token:dev-admin-token-change-in-production
```

### Using with Postman

1. Create a new request
2. Add header: `X-Admin-Token` with value `dev-admin-token-change-in-production`
3. Send request to `http://localhost:3031/admin/system/status`

### Using with JavaScript/TypeScript

```typescript
const response = await fetch('http://localhost:3031/admin/system/status', {
  headers: {
    'X-Admin-Token': 'dev-admin-token-change-in-production'
  }
});
```

### Using with Python

```python
import requests

headers = {'X-Admin-Token': 'dev-admin-token-change-in-production'}
response = requests.get('http://localhost:3031/admin/system/status', headers=headers)
```

## Swagger UI Access

The Swagger UI is also protected by the authentication middleware:

```bash
# Access Swagger UI
curl -H "X-Admin-Token: dev-admin-token-change-in-production" \
     http://localhost:3031/admin/swagger-ui
```

Note: You may need to configure the Swagger UI client to send the authentication header.
