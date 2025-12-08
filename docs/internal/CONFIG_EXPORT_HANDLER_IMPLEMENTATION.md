# Config Export Handler Implementation

## Summary

Created a complete configuration export handler for the Prism RPC Proxy Admin API at `/admin/config/export`.

## Implementation Details

### File Created
- `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/config.rs`

### Features Implemented

1. **Export Formats**
   - TOML (default)
   - JSON
   - Content-Type headers set appropriately

2. **Query Parameters**
   - `format`: "toml" | "json" (default: "toml")
   - `include_secrets`: boolean (default: false)
   - `dynamic_only`: boolean (default: false)

3. **Security Features**
   - Secrets masked by default using `mask_url()` function
   - Logs warning when `include_secrets=true` is used
   - Masks API keys in URLs, query parameters, and long path segments

4. **Configuration Merging**
   - Reads static config from `state.config`
   - Reads dynamic upstreams from `state.proxy_engine.get_upstream_manager().get_dynamic_registry()`
   - Merges into unified `ConfigExport` structure
   - Skips disabled dynamic upstreams

5. **Error Handling**
   - Validates export format
   - Returns 400 for invalid formats
   - Returns 500 if serialization fails
   - Includes correlation ID in logs

## Key Types

### ConfigExportQuery
```rust
pub struct ConfigExportQuery {
    pub format: String,
    pub include_secrets: bool,
    pub dynamic_only: bool,
}
```

### ConfigExport
```rust
pub struct ConfigExport {
    pub environment: String,
    pub server: ServerConfig,
    pub upstreams: UpstreamsConfig,
    pub cache: CacheConfig,
    pub health_check: HealthCheckConfig,
    pub auth: AuthConfig,
    pub metrics: MetricsConfig,
    pub logging: LoggingConfig,
    pub admin: AdminConfig,
    pub hedging: HedgeConfig,
    pub scoring: ScoringConfig,
    pub consensus: ConsensusConfig,
}
```

## Route to Add

Add this route to `/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs` in the `create_admin_router` function:

```rust
// Config endpoints
.route("/admin/config/export", get(handlers::config::export_config))
```

And add the OpenAPI documentation:

```rust
#[openapi(
    paths(
        // ... existing paths ...
        handlers::config::export_config,
    ),
    components(schemas(
        // ... existing schemas ...
        crate::admin::handlers::config::ConfigExportQuery,
    ))
)]
```

## Usage Examples

### Export as TOML (default, secrets masked)
```bash
curl http://localhost:3031/admin/config/export
```

### Export as JSON with secrets
```bash
curl "http://localhost:3031/admin/config/export?format=json&include_secrets=true"
```

### Export only dynamic upstreams
```bash
curl "http://localhost:3031/admin/config/export?dynamic_only=true"
```

## Security Considerations

1. **Default Masking**: Secrets are masked by default to prevent accidental exposure
2. **Audit Logging**: Exports with `include_secrets=true` are logged with WARNING level
3. **Correlation IDs**: All exports include correlation ID for audit trail
4. **URL Masking**: Comprehensive masking of:
   - Password fields
   - Query parameters (key, apikey, api_key, token, secret, password, auth)
   - Long path segments (>20 chars, likely API keys)

## Dependencies Added

- `toml = { workspace = true }` to `crates/server/Cargo.toml`

## Files Modified

1. `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/config.rs` (created)
2. `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/mod.rs` (added module)
3. `/home/flo/workspace/personal/prism/crates/server/Cargo.toml` (added toml dependency)

## Next Steps

1. Add route to `create_admin_router()` in `/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs`
2. Add OpenAPI documentation to the ApiDoc struct
3. Test the endpoint with various query parameters
4. Consider adding persistence endpoint to save exported config to file
5. Consider adding config validation endpoint

## Technical Notes

- Used `DynamicUpstreamConfig` and `get_dynamic_registry()` (not `RuntimeUpstreamConfig`)
- Copied `mask_url()` function locally since it's private in upstreams.rs
- ConfigExport doesn't implement ToSchema (config types don't have derives)
- Returns raw string responses with appropriate Content-Type headers

## Compilation Status

✓ Compiles successfully
✓ No warnings
✓ All dependencies resolved
