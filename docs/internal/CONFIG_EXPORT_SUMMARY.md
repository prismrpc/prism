# Config Export/Persistence Feature - Executive Summary

## Overview

A production-ready solution for exporting, persisting, and importing runtime upstream configurations in the Prism RPC Proxy, enabling configuration portability and restart persistence.

---

## Key Features

### 1. Configuration Export
- **Endpoint:** `GET /admin/config/export`
- **Formats:** TOML (default) and JSON
- **Security:** Automatic secret masking (API keys, tokens, passwords)
- **Flexibility:** Export full config or runtime upstreams only

### 2. Persistence
- **Endpoint:** `POST /admin/config/persist`
- **Storage:** JSON file at `data/runtime_upstreams.json`
- **Safety:** Atomic writes with automatic backups
- **Auto-reload:** Runtime upstreams restored on server restart

### 3. Import
- **Endpoint:** `POST /admin/config/import`
- **Modes:** Merge (add new) or Replace (clear & import)
- **Validation:** Full upstream validation before import
- **Dry-run:** Test imports without applying changes

### 4. Metadata
- **Endpoint:** `GET /admin/config/metadata`
- **Info:** Config file paths, upstream counts, persistence status

---

## Use Cases

### Scenario 1: Backup Before Major Changes
```bash
# Export current config as backup
curl -H "X-Admin-Token: $TOKEN" \
  "http://localhost:3031/admin/config/export?format=toml" \
  | jq -r '.config' > backup-$(date +%Y%m%d).toml

# Make changes via Admin API
curl -X POST ... /admin/upstreams

# If issues arise, restore from backup
curl -X POST -H "X-Admin-Token: $TOKEN" \
  -d '{"config":"...","format":"toml","mode":"replace"}' \
  http://localhost:3031/admin/config/import
```

### Scenario 2: Migrate Configuration to New Server
```bash
# On source server
curl -H "X-Admin-Token: $TOKEN" \
  "http://source:3031/admin/config/export?format=toml" \
  | jq -r '.config' > config-export.toml

# On target server (merge with existing config)
curl -X POST -H "X-Admin-Token: $TOKEN" \
  -d @import-payload.json \
  http://target:3031/admin/config/import
```

### Scenario 3: Persist Runtime Changes
```bash
# Add upstreams via Admin API during operation
curl -X POST -H "X-Admin-Token: $TOKEN" \
  -d '{"name":"new-provider",...}' \
  http://localhost:3031/admin/upstreams

# Persist to survive restart
curl -X POST -H "X-Admin-Token: $TOKEN" \
  -d '{"backup_existing": true}' \
  http://localhost:3031/admin/config/persist

# Restart server - upstreams automatically restored
systemctl restart prism-server
```

---

## API Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/admin/config/export` | Export full config as TOML/JSON |
| POST | `/admin/config/persist` | Save runtime upstreams to disk |
| POST | `/admin/config/import` | Import upstreams from config string |
| GET | `/admin/config/metadata` | Get config metadata and paths |

---

## Architecture Highlights

### Data Sources Merged in Export
```
┌─────────────────┐     ┌──────────────────┐
│ Config File     │     │ Runtime Registry │
│ (TOML)          │     │ (JSON)           │
│                 │     │                  │
│ - config-based  │────►│ - runtime-added  │
│   upstreams     │     │   upstreams      │
│ - server config │     │ - metadata       │
│ - cache config  │     │ - timestamps     │
└─────────────────┘     └──────────────────┘
         │                      │
         └──────────┬───────────┘
                    ▼
            ┌──────────────┐
            │ Export API   │
            │              │
            │ Merges both  │
            │ sources into │
            │ single TOML  │
            └──────────────┘
```

### Persistence Flow (Atomic & Safe)
```
1. Check storage path configured
2. Create timestamped backup (if requested)
3. Write to temp file (.tmp)
4. Atomic rename temp → target
5. Verify write success
6. Return paths to client
```

### Startup Sequence
```
Server Start
    ├─> Load config.toml (config-based upstreams)
    ├─> Build UpstreamManager with storage path
    ├─> Initialize RuntimeUpstreamRegistry
    ├─> Load runtime_upstreams.json from disk
    ├─> Add all upstreams to manager
    └─> Start health checker for all upstreams
```

---

## Security Features

### 1. Secret Masking
**Default Behavior:** All secrets masked with `***`

**Masked Fields:**
- Upstream URLs: API keys in query params, path segments, basic auth
- Admin token: `admin.admin_token`
- Database URLs: Credentials in connection strings
- Prometheus URLs: Auth tokens

**Example:**
```toml
# Original
https_url = "https://mainnet.infura.io/v3/abc123def456"

# Masked export
https_url = "https://mainnet.infura.io/v3/***"
```

### 2. Access Control
- All endpoints require admin authentication
- Exporting with secrets requires explicit confirmation header (optional enhancement)
- Audit logging for all sensitive operations

### 3. File System Security
- Persistence file: `0600` permissions (owner read/write only)
- Backup files: `0600` permissions
- Atomic writes prevent partial/corrupted files

### 4. Validation
- URL scheme validation (http/https only, prevents SSRF)
- Weight range validation (1-1000)
- Name format validation (alphanumeric + dash/underscore)
- Conflict detection (runtime names can't shadow config names)

---

## Implementation Details

### New Files
- `/crates/server/src/admin/handlers/config.rs` (~500 lines)

### Modified Files
- `/crates/server/src/admin/mod.rs` (add 4 routes)
- `/crates/server/src/admin/types.rs` (add 8 types)
- `/crates/prism-core/src/upstream/runtime_registry.rs` (add getter)
- `/crates/server/src/main.rs` (add startup loading ~30 lines)

### Dependencies
**None new** - Uses existing:
- `toml` (serialization)
- `serde_json` (JSON support)
- `chrono` (timestamps)
- Existing `mask_url()` from upstreams handler

### Estimated Effort
- **Core implementation:** 2 days
- **Testing:** 1 day
- **Documentation:** 0.5 day
- **Total:** 3-4 days

---

## Risk Assessment

### Low Risk Areas
- **Existing Infrastructure:** Builds on proven `RuntimeUpstreamRegistry` persistence layer
- **Serialization:** Uses battle-tested `toml` and `serde_json` crates
- **Validation:** Reuses existing upstream validation logic
- **No Breaking Changes:** All changes are additive

### Medium Risk Areas
- **Secret Exposure:** Mitigated by default masking + audit logging
- **Config Overwrite:** Mitigated by automatic backups + atomic writes
- **Import Validation:** Mitigated by reusing existing validation + dry-run mode

### Mitigation Strategies
1. **Default to Safe:** Secrets masked by default, backups created by default
2. **Atomic Operations:** Temp file + rename pattern for all writes
3. **Validation First:** All imports validated before applying
4. **Audit Trail:** All sensitive operations logged
5. **Fail-Safe:** Errors during persistence don't corrupt existing files

---

## Testing Strategy

### Unit Tests (~15 tests)
- Export TOML format validation
- Export JSON format validation
- Secret masking verification
- Persist backup creation
- Import merge mode
- Import replace mode
- Import conflict detection
- Import dry-run mode
- Roundtrip export → import

### Integration Tests (~5 scenarios)
1. Add runtime upstream → Export → Verify in output
2. Export → Import on new instance → Verify identical
3. Persist → Restart server → Verify upstreams restored
4. Import with conflicts → Verify skipped correctly
5. Replace mode → Verify old upstreams removed

### Manual Testing
- Curl commands for all endpoints
- Swagger UI verification
- Restart persistence testing
- Backup rotation testing

---

## Documentation Deliverables

1. **User Guide:** `/docs/admin-api/config-export.md`
   - Overview and use cases
   - Curl examples for each endpoint
   - Security best practices
   - Troubleshooting guide

2. **API Reference:** Updated OpenAPI spec
   - All endpoints documented
   - Request/response examples
   - Security warnings

3. **Architecture Docs:** Updated architecture diagrams
   - Data flow diagrams
   - Component interaction
   - Startup sequence

---

## Performance Characteristics

### Export
- **Complexity:** O(n) where n = total upstreams
- **Typical Time:** < 100ms for 100 upstreams
- **Memory:** ~10MB for large configs (1000 upstreams)

### Persist
- **File I/O:** Async, atomic write pattern
- **Typical Time:** < 50ms
- **Disk Space:** ~10KB per 100 upstreams + backups

### Import
- **Complexity:** O(n) sequential validation + add
- **Typical Time:** ~10ms per upstream
- **Throughput:** ~100 upstreams/second

### Startup
- **Additional Time:** +50-100ms for loading runtime upstreams
- **Memory:** Negligible (upstreams already in memory)

---

## Future Enhancements

### Phase 2 (Optional)
1. **Config Diffing:** Compare file config vs runtime config
2. **Scheduled Backups:** Automatic periodic exports to S3/filesystem
3. **Config Validation:** Validate config without applying
4. **Rollback Support:** Track versions and rollback capability

### Phase 3 (Advanced)
1. **Multi-Instance Sync:** Sync runtime upstreams via etcd/consul
2. **Backup Rotation:** Auto-delete old backups (keep last N)
3. **Import Validation Service:** CI/CD integration
4. **Config Templates:** Predefined upstream sets

---

## Success Criteria

### Functional
✅ Export returns valid, parseable TOML/JSON
✅ Secrets masked by default
✅ Persist creates backups before writing
✅ Import validates all upstreams before applying
✅ Runtime upstreams survive server restarts

### Non-Functional
✅ Export completes in < 200ms for 100 upstreams
✅ Persist uses atomic writes (no corruption risk)
✅ Import handles conflicts gracefully
✅ All operations logged for audit
✅ File permissions set to 0600 for security

### Documentation
✅ User guide with working examples
✅ OpenAPI spec complete
✅ Architecture diagrams updated
✅ Security best practices documented

---

## Deployment Checklist

### Before Deployment
- [ ] All tests passing
- [ ] Security review completed
- [ ] Documentation reviewed
- [ ] Swagger UI tested
- [ ] Manual testing completed

### Deployment Steps
1. **Create data directory:** `mkdir -p data && chmod 700 data`
2. **Update config:** Add storage path to builder (already in roadmap)
3. **Deploy binary:** No schema changes, safe to deploy
4. **Verify endpoints:** Test export/persist/import
5. **Monitor logs:** Check for loading errors

### Rollback Plan
- Remove routes from admin router
- Revert main.rs startup changes
- Server functions normally without runtime persistence

---

## Support & Troubleshooting

### Common Issues

**Issue:** "Runtime persistence not configured"
**Solution:** Ensure `runtime_storage_path()` is set in UpstreamManagerBuilder

**Issue:** "Backup creation failed"
**Solution:** Check disk space and directory permissions (0700 for data/)

**Issue:** "Import conflicts detected"
**Solution:** Use dry-run mode first, check conflicts array in response

**Issue:** "Runtime upstreams not restored after restart"
**Solution:** Check logs for loading errors, verify JSON file exists and is valid

### Monitoring

**Metrics to Monitor:**
- Config export count (success/failure)
- Persist operation latency
- Import operation count and success rate
- Backup file count and total size

**Log Messages:**
- `INFO: Restored N runtime upstreams from disk`
- `WARN: Failed to load runtime upstreams: {error}`
- `INFO: Persisted N runtime upstreams`

---

## Conclusion

This design provides a **production-ready, secure, and user-friendly** solution for config export and persistence with:

✅ **Safety:** Automatic backups, atomic writes, validation
✅ **Security:** Secret masking, audit logging, access control
✅ **Usability:** Multiple formats, dry-run mode, clear errors
✅ **Reliability:** Leverages proven persistence layer
✅ **Performance:** Minimal overhead, fast operations
✅ **Maintainability:** Reuses existing code, follows patterns

**Ready for implementation** with clear roadmap, minimal risk, and comprehensive documentation.

---

## Quick Reference

### Files to Read
1. `CONFIG_EXPORT_DESIGN.md` - Full design document
2. `CONFIG_EXPORT_ARCHITECTURE.md` - Detailed diagrams
3. `CONFIG_EXPORT_IMPLEMENTATION_ROADMAP.md` - Step-by-step guide

### Key Code Locations
- Upstream manager: `/crates/prism-core/src/upstream/manager.rs`
- Runtime registry: `/crates/prism-core/src/upstream/runtime_registry.rs`
- Admin handlers: `/crates/server/src/admin/handlers/`
- Config module: `/crates/prism-core/src/config/mod.rs`

### Test Commands
```bash
# Run all tests
cargo test

# Start server
PRISM_CONFIG=config/development.toml cargo run --bin server

# Test export
curl -H "X-Admin-Token: test" http://localhost:3031/admin/config/export | jq

# View API docs
open http://localhost:3031/admin/swagger-ui
```
