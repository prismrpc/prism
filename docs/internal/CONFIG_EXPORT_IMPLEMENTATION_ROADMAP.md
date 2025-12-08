# Config Export/Persistence - Implementation Roadmap

## Quick Reference

**Estimated Time:** 3-4 days (core features + tests + docs)

**New Files:**
- `/crates/server/src/admin/handlers/config.rs` (~500 lines)

**Modified Files:**
- `/crates/server/src/admin/mod.rs` (add routes)
- `/crates/server/src/admin/types.rs` (add response types)
- `/crates/prism-core/src/upstream/runtime_registry.rs` (add getter)
- `/crates/server/src/main.rs` (add runtime loading on startup)

**Dependencies:** None new (uses existing `toml`, `serde_json`, `chrono`)

---

## Phase 1: Foundation (Day 1 - 4 hours)

### 1.1 Create Handler Module

**File:** `/crates/server/src/admin/handlers/config.rs`

```rust
//! Configuration export/import handlers.

use axum::{extract::{State, Query}, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::admin::AdminState;
use super::upstreams::mask_url;

// Add all type definitions from design doc
```

**Tasks:**
- [ ] Create new file
- [ ] Add module declaration in `/crates/server/src/admin/handlers/mod.rs`
- [ ] Define core types: `ExportQuery`, `ExportResponse`, `ExportableConfig`
- [ ] Add basic skeleton handlers (stub implementations)

**Testing:**
```bash
cargo check
cargo test --package server
```

### 1.2 Extend Runtime Registry

**File:** `/crates/prism-core/src/upstream/runtime_registry.rs`

**Add method:**
```rust
impl RuntimeUpstreamRegistry {
    /// Returns the storage path if configured.
    #[must_use]
    pub fn storage_path(&self) -> Option<&PathBuf> {
        self.storage_path.as_ref()
    }
}
```

**Tasks:**
- [ ] Add public getter method
- [ ] Update visibility if needed
- [ ] Run tests: `cargo test --package prism-core runtime_registry`

---

## Phase 2: Export Implementation (Day 1 - 4 hours)

### 2.1 Implement Export Handler

**Functions to implement:**
1. `export_config()` - Main handler
2. `build_exportable_config()` - Helper to gather config
3. `mask_config_secrets()` - Secret masking logic

**Copy-Paste Starting Point:**

```rust
/// GET /admin/config/export
#[utoipa::path(
    get,
    path = "/admin/config/export",
    tag = "Config",
    params(
        ("format" = Option<String>, Query, description = "Export format: 'toml' or 'json'"),
        ("include_secrets" = Option<bool>, Query, description = "Include unmasked secrets"),
        ("include_runtime_only" = Option<bool>, Query, description = "Only runtime upstreams"),
        ("include_metadata" = Option<bool>, Query, description = "Include metadata comments")
    ),
    responses(
        (status = 200, description = "Config exported", body = ExportResponse),
        (status = 500, description = "Export failed")
    )
)]
pub async fn export_config(
    State(state): State<AdminState>,
    Query(query): Query<ExportQuery>,
) -> Result<Json<ExportResponse>, (StatusCode, String)> {
    // Step 1: Build base config
    let mut exportable = build_exportable_config(&state);

    // Step 2: Add runtime upstreams
    let runtime_upstreams = state
        .proxy_engine
        .get_upstream_manager()
        .list_runtime_upstreams();

    let runtime_providers: Vec<_> = runtime_upstreams
        .iter()
        .map(|ru| convert_runtime_to_provider(ru, query.include_secrets))
        .collect();

    // Step 3: Merge or replace
    if query.include_runtime_only {
        exportable.upstreams = runtime_providers;
    } else {
        exportable.upstreams.extend(runtime_providers);
    }

    // Step 4: Mask secrets if needed
    if !query.include_secrets {
        mask_config_secrets(&mut exportable);
    } else {
        // Audit log when exporting secrets
        crate::admin::audit::log_read("config_export_with_secrets", "full", None);
    }

    // Step 5: Serialize
    let config_string = match query.format.as_deref().unwrap_or("toml") {
        "json" => serde_json::to_string_pretty(&exportable)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("JSON error: {e}")))?,
        _ => toml::to_string_pretty(&exportable)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("TOML error: {e}")))?,
    };

    // Step 6: Build response
    Ok(Json(ExportResponse {
        config: config_string,
        format: query.format.unwrap_or_else(|| "toml".to_string()),
        exported_at: chrono::Utc::now().to_rfc3339(),
        instance_name: state.config.admin.instance_name.clone(),
        version: state.version.to_string(),
        metadata: ExportMetadata {
            total_upstreams: state.config.upstreams.providers.len() + runtime_upstreams.len(),
            config_upstreams: state.config.upstreams.providers.len(),
            runtime_upstreams: runtime_upstreams.len(),
            secrets_masked: !query.include_secrets,
        },
    }))
}
```

**Tasks:**
- [ ] Implement `export_config()` handler
- [ ] Implement `build_exportable_config()` helper
- [ ] Implement `convert_runtime_to_provider()` helper
- [ ] Implement `mask_config_secrets()` (reuse `mask_url()`)
- [ ] Add audit logging for secret exports

**Testing:**
```bash
# Start server with test config
PRISM_CONFIG=config/development.toml cargo run --bin server

# Test export
curl -H "X-Admin-Token: test" \
  "http://localhost:3031/admin/config/export?format=toml" \
  | jq '.config' -r > exported.toml

# Verify TOML is valid
cat exported.toml | toml-test  # Or just try to parse it
```

### 2.2 Add Routes

**File:** `/crates/server/src/admin/mod.rs`

**Add to router:**
```rust
// Config endpoints (add after cache endpoints)
.route("/admin/config/export", get(handlers::config::export_config))
.route("/admin/config/metadata", get(handlers::config::get_metadata))
```

**Add to OpenAPI:**
```rust
#[openapi(
    paths(
        // ... existing paths
        handlers::config::export_config,
        handlers::config::get_metadata,
    ),
    components(schemas(
        // ... existing schemas
        handlers::config::ExportResponse,
        handlers::config::ExportMetadata,
        handlers::config::ConfigMetadata,
    )),
)]
```

---

## Phase 3: Persistence Implementation (Day 2 - 3 hours)

### 3.1 Implement Persist Handler

```rust
/// POST /admin/config/persist
#[utoipa::path(
    post,
    path = "/admin/config/persist",
    tag = "Config",
    request_body = PersistRequest,
    responses(
        (status = 200, description = "Persisted", body = PersistResponse),
        (status = 400, description = "Persistence not configured"),
        (status = 500, description = "Persistence failed")
    )
)]
pub async fn persist_runtime(
    State(state): State<AdminState>,
    Json(request): Json<PersistRequest>,
) -> Result<Json<PersistResponse>, (StatusCode, String)> {
    let registry = state.proxy_engine.get_upstream_manager().get_runtime_registry();

    // Get storage path (fail if not configured)
    let storage_path = registry.storage_path()
        .ok_or((StatusCode::BAD_REQUEST, "Runtime persistence not configured".to_string()))?;

    let mut backup_path = None;
    let mut backup_created = false;

    // Create backup if requested
    if request.backup_existing && storage_path.exists() {
        use std::fs;
        use chrono::Utc;

        let timestamp = Utc::now().format("%Y-%m-%dT%H-%M-%SZ");
        let backup_file = storage_path.with_extension(format!("json.backup.{timestamp}"));

        fs::copy(&storage_path, &backup_file)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,
                         format!("Backup failed: {e}. Aborting.")))?;

        backup_path = Some(backup_file.to_string_lossy().to_string());
        backup_created = true;
    }

    // Persist (uses existing atomic write in registry)
    registry.save_to_disk()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,
                     format!("Persistence failed: {e}")))?;

    // Audit log
    crate::admin::audit::log_write("config_persist", "runtime_upstreams", None);

    Ok(Json(PersistResponse {
        success: true,
        persisted_count: registry.list_all().len(),
        backup_created,
        backup_path,
        file_path: storage_path.to_string_lossy().to_string(),
    }))
}
```

**Tasks:**
- [ ] Implement `persist_runtime()` handler
- [ ] Add backup creation logic
- [ ] Test atomic write behavior
- [ ] Add audit logging

**Testing:**
```bash
# Add a runtime upstream
curl -X POST -H "X-Admin-Token: test" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "test-runtime",
    "url": "https://test.example.com",
    "weight": 100,
    "chain_id": 1,
    "timeout_seconds": 30
  }' \
  http://localhost:3031/admin/upstreams

# Persist it
curl -X POST -H "X-Admin-Token: test" \
  -H "Content-Type: application/json" \
  -d '{"backup_existing": true}' \
  http://localhost:3031/admin/config/persist

# Verify file exists
ls -la data/runtime_upstreams.json*
cat data/runtime_upstreams.json | jq
```

### 3.2 Add Startup Loading

**File:** `/crates/server/src/main.rs`

**In `init_core_services()` after upstream manager creation:**

```rust
// Around line 91-102, after creating upstream_manager

let upstream_manager = Arc::new(
    prism_core::upstream::UpstreamManagerBuilder::new()
        .chain_state(chain_state.clone())
        .concurrency_limit(config.server.max_concurrent_requests)
        .runtime_storage_path(std::path::PathBuf::from("data/runtime_upstreams.json"))  // ADD THIS
        .build()
        .map_err(|e| anyhow::anyhow!("Upstream manager initialization failed: {e}"))?,
);

// Load config-based upstreams
for upstream_config in config.to_legacy_upstreams() {
    upstream_manager.add_upstream(upstream_config);
}

// ADD THIS: Initialize config upstream names in registry
let config_names: Vec<String> = config.upstreams.providers
    .iter()
    .map(|p| p.name.clone())
    .collect();
upstream_manager.get_runtime_registry()
    .initialize_config_upstreams(config_names);

// ADD THIS: Load runtime upstreams from disk
if let Err(e) = upstream_manager.get_runtime_registry().load_from_disk() {
    tracing::warn!("Failed to load runtime upstreams: {e}");
    // Don't fail startup, just log warning
} else {
    // Add loaded runtime upstreams to manager
    for runtime_upstream in upstream_manager.list_runtime_upstreams() {
        let upstream_config = prism_core::types::UpstreamConfig {
            name: Arc::from(runtime_upstream.name.as_str()),
            url: runtime_upstream.url.clone(),
            ws_url: runtime_upstream.ws_url.clone(),
            weight: runtime_upstream.weight,
            chain_id: runtime_upstream.chain_id,
            timeout_seconds: runtime_upstream.timeout_seconds,
            supports_websocket: runtime_upstream.ws_url.is_some(),
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_seconds: 60,
        };
        upstream_manager.add_upstream(upstream_config);
    }

    let runtime_count = upstream_manager.list_runtime_upstreams().len();
    if runtime_count > 0 {
        info!(runtime_upstreams = runtime_count, "Restored runtime upstreams from disk");
    }
}

info!(endpoints_count = config.upstreams.providers.len(), "Upstream manager initialized");
```

**Tasks:**
- [ ] Add `runtime_storage_path()` to builder call
- [ ] Add config name initialization
- [ ] Add runtime upstream loading
- [ ] Add upstreams to manager after loading
- [ ] Test restart persistence

**Testing:**
```bash
# Add runtime upstream, persist, restart, verify it's still there
curl -X POST -H "X-Admin-Token: test" -H "Content-Type: application/json" \
  -d '{"name":"restart-test","url":"https://test.com","weight":100,"chain_id":1,"timeout_seconds":30}' \
  http://localhost:3031/admin/upstreams

curl -X POST -H "X-Admin-Token: test" -H "Content-Type: application/json" \
  -d '{"backup_existing":true}' http://localhost:3031/admin/config/persist

# Restart server
pkill server
cargo run --bin server

# Verify upstream still exists
curl -H "X-Admin-Token: test" http://localhost:3031/admin/upstreams | jq '.[] | select(.name=="restart-test")'
```

---

## Phase 4: Import Implementation (Day 2 - 3 hours)

### 4.1 Implement Import Handler

**Copy-Paste Starting Point:**

```rust
/// POST /admin/config/import
#[utoipa::path(
    post,
    path = "/admin/config/import",
    tag = "Config",
    request_body = ImportRequest,
    responses(
        (status = 200, description = "Imported", body = ImportResponse),
        (status = 400, description = "Invalid config"),
        (status = 500, description = "Import failed")
    )
)]
pub async fn import_config(
    State(state): State<AdminState>,
    Json(request): Json<ImportRequest>,
) -> Result<Json<ImportResponse>, (StatusCode, String)> {
    // 1. Parse config
    let parsed: ExportableConfig = match request.format.as_str() {
        "toml" => toml::from_str(&request.config)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("TOML parse error: {e}")))?,
        "json" => serde_json::from_str(&request.config)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("JSON parse error: {e}")))?,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid format".to_string())),
    };

    // 2. Handle replace mode
    let manager = state.proxy_engine.get_upstream_manager();
    if matches!(request.mode, ImportMode::Replace) && !request.dry_run {
        for upstream in manager.list_runtime_upstreams() {
            let _ = manager.remove_runtime_upstream(&upstream.id);
        }
    }

    // 3. Import each upstream
    let mut imported = 0;
    let mut skipped = 0;
    let mut conflicts = Vec::new();

    for provider in parsed.upstreams {
        // Skip config upstreams
        if manager.get_runtime_registry().is_config_upstream(&provider.name) {
            conflicts.push(format!("{}: conflicts with config upstream", provider.name));
            skipped += 1;
            continue;
        }

        if request.dry_run {
            imported += 1;
            continue;
        }

        // Try to add
        let create_req = prism_core::upstream::CreateUpstreamRequest {
            name: provider.name.clone(),
            url: provider.https_url,
            ws_url: provider.wss_url,
            weight: provider.weight,
            chain_id: provider.chain_id,
            timeout_seconds: provider.timeout_seconds,
        };

        match manager.add_runtime_upstream(create_req) {
            Ok(_) => imported += 1,
            Err(e) => {
                conflicts.push(format!("{}: {e}", provider.name));
                skipped += 1;
            }
        }
    }

    // Audit log
    crate::admin::audit::log_create("config_import", &format!("{imported}_upstreams"), None);

    Ok(Json(ImportResponse {
        success: true,
        imported,
        skipped,
        conflicts,
        dry_run: request.dry_run,
    }))
}
```

**Tasks:**
- [ ] Implement `import_config()` handler
- [ ] Add `ImportMode` enum and types
- [ ] Test merge mode
- [ ] Test replace mode
- [ ] Test dry-run mode
- [ ] Test conflict detection

**Testing:**
```bash
# Export current config
curl -H "X-Admin-Token: test" \
  "http://localhost:3031/admin/config/export?format=toml&include_runtime_only=true" \
  | jq -r '.config' > runtime-export.toml

# Import on clean instance (merge mode)
curl -X POST -H "X-Admin-Token: test" \
  -H "Content-Type: application/json" \
  --data @- http://localhost:3031/admin/config/import <<EOF
{
  "config": $(cat runtime-export.toml | jq -Rs .),
  "format": "toml",
  "mode": "merge",
  "dry_run": false
}
EOF
```

---

## Phase 5: Metadata & Polish (Day 3 - 2 hours)

### 5.1 Implement Metadata Handler

```rust
pub async fn get_metadata(
    State(state): State<AdminState>,
) -> Json<ConfigMetadata> {
    let registry = state.proxy_engine.get_upstream_manager().get_runtime_registry();

    let config_names: Vec<String> = state.config.upstreams.providers
        .iter()
        .map(|p| p.name.clone())
        .collect();

    let runtime_upstreams: Vec<RuntimeUpstreamMetadata> = registry.list_all()
        .into_iter()
        .map(|ru| RuntimeUpstreamMetadata {
            id: ru.id,
            name: ru.name,
            created_at: ru.created_at,
            updated_at: ru.updated_at,
        })
        .collect();

    Json(ConfigMetadata {
        config_file_path: std::env::var("PRISM_CONFIG")
            .unwrap_or_else(|_| "config/config.toml".to_string()),
        runtime_storage_path: registry.storage_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "not configured".to_string()),
        total_upstreams: config_names.len() + runtime_upstreams.len(),
        config_upstreams: config_names,
        runtime_upstreams,
        persistence_enabled: registry.storage_path().is_some(),
    })
}
```

---

## Phase 6: Testing (Day 3-4 - 6 hours)

### 6.1 Unit Tests

**File:** `/crates/server/tests/config_export_tests.rs`

```rust
use server::admin::handlers::config::*;
use server::admin::AdminState;

#[tokio::test]
async fn test_export_toml_format() {
    let state = create_test_state().await;
    // Test TOML export
}

#[tokio::test]
async fn test_export_json_format() {
    // Test JSON export
}

#[tokio::test]
async fn test_export_masks_secrets() {
    // Verify secrets are masked by default
}

#[tokio::test]
async fn test_persist_creates_backup() {
    // Verify backup creation
}

#[tokio::test]
async fn test_import_merge_mode() {
    // Test merging upstreams
}

#[tokio::test]
async fn test_import_replace_mode() {
    // Test replacing upstreams
}

#[tokio::test]
async fn test_import_conflict_detection() {
    // Test conflict with config upstreams
}

#[tokio::test]
async fn test_import_dry_run() {
    // Test dry-run doesn't modify state
}

#[tokio::test]
async fn test_roundtrip_export_import() {
    // Export -> Import -> Verify identical
}
```

**Run:**
```bash
cargo test --package server config_export
```

### 6.2 Integration Tests

**Scenarios:**
1. Add runtime upstream → Export → Verify in output
2. Export → Import on new instance → Verify upstreams exist
3. Persist → Restart server → Verify upstreams restored
4. Import with conflicts → Verify skipped
5. Replace mode → Verify old upstreams gone

---

## Phase 7: Documentation (Day 4 - 2 hours)

### 7.1 User Guide

**File:** `/home/flo/workspace/personal/prism/docs/admin-api/config-export.md`

**Sections:**
- Overview
- Use Cases (with curl examples)
- API Reference
- Security Best Practices
- Troubleshooting

### 7.2 Update OpenAPI

**Tasks:**
- [ ] Verify all endpoints documented
- [ ] Add examples to schemas
- [ ] Test Swagger UI rendering
- [ ] Add security notes to descriptions

---

## Validation Checklist

Before considering the feature complete:

### Functionality
- [ ] Export returns valid TOML
- [ ] Export returns valid JSON
- [ ] Secrets are masked by default
- [ ] Persist creates backups
- [ ] Persist uses atomic writes
- [ ] Import validates upstreams
- [ ] Import detects conflicts
- [ ] Import dry-run works
- [ ] Metadata endpoint accurate
- [ ] Runtime upstreams survive restart

### Security
- [ ] Secrets masked by default
- [ ] Secret export requires confirmation (optional)
- [ ] Audit logs for sensitive operations
- [ ] File permissions set to 0600
- [ ] No path traversal vulnerabilities
- [ ] URL validation prevents SSRF

### Testing
- [ ] All unit tests pass
- [ ] Integration tests pass
- [ ] Manual curl testing successful
- [ ] Restart persistence verified
- [ ] Error cases handled gracefully

### Documentation
- [ ] User guide complete
- [ ] API examples working
- [ ] Security warnings present
- [ ] OpenAPI spec updated
- [ ] Architecture docs updated

---

## Quick Start Commands

```bash
# Run all tests
cargo test

# Run only config tests
cargo test --package server config_export

# Start server with dev config
PRISM_CONFIG=config/development.toml cargo run --bin server

# Test export
curl -H "X-Admin-Token: test" \
  "http://localhost:3031/admin/config/export?format=toml" | jq

# Test persist
curl -X POST -H "X-Admin-Token: test" \
  -H "Content-Type: application/json" \
  -d '{"backup_existing": true}' \
  http://localhost:3031/admin/config/persist

# View OpenAPI docs
open http://localhost:3031/admin/swagger-ui
```

---

## Rollback Plan

If issues arise:

1. **Revert main.rs changes** - Server will start without loading runtime upstreams
2. **Remove routes** - Endpoints won't be accessible
3. **Keep RuntimeUpstreamRegistry changes** - They're backward compatible

No data loss risk:
- Config file is never modified
- Runtime upstreams file is backed up before writes
- Existing upstream CRUD operations unaffected

---

## Post-Implementation Tasks

1. **Monitor logs** for errors related to config loading
2. **Document common issues** in troubleshooting guide
3. **Add metrics** for export/import operations
4. **Consider backup rotation** (keep last N backups)
5. **Add validation endpoint** for future enhancement
