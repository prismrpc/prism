# Config Export/Persistence Feature Design

## Executive Summary

This design document details a comprehensive solution for exporting and persisting runtime-added upstreams in the Prism RPC Proxy. The feature enables users to:

1. Export the complete running configuration (including runtime upstreams) as portable TOML/JSON
2. Optionally persist runtime changes to disk for automatic reload on restart
3. Safely backup and restore configurations across server instances

---

## 1. Current Architecture Analysis

### 1.1 Configuration Loading Flow

```
[Startup] → [Load TOML] → [UpstreamsConfig] → [to_legacy_upstreams()] → [UpstreamManager]
                ↓
         [Environment Variables Override]
```

**Key Files:**
- `/home/flo/workspace/personal/prism/crates/prism-core/src/config/mod.rs` - Config loading/serialization
- `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/manager.rs` - Runtime upstream management
- `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/runtime_registry.rs` - Runtime persistence layer

### 1.2 Runtime Upstreams Architecture

**Current State:**
- Runtime upstreams are stored in `RuntimeUpstreamRegistry` (already implemented)
- Persistence to JSON file is supported (`runtime_upstreams.json`)
- Clear separation between config-based and runtime upstreams via `UpstreamSource` enum
- Automatic conflict detection prevents runtime upstreams from shadowing config upstreams

**Existing Data Flow:**
```
[Admin API POST /admin/upstreams]
    ↓
[UpstreamManager::add_runtime_upstream()]
    ↓
[RuntimeUpstreamRegistry::add()]
    ↓
[save_to_disk()] → runtime_upstreams.json
```

### 1.3 Configuration Serialization

**Supported Formats:**
- **TOML**: Primary configuration format (via `toml` crate + serde)
- **JSON**: Secondary format (via `serde_json`)

**Config Structure:**
```rust
pub struct AppConfig {
    environment: String,
    server: ServerConfig,
    upstreams: UpstreamsConfig,  // Vec<UpstreamProvider>
    cache: CacheConfig,
    health_check: HealthCheckConfig,
    auth: AuthConfig,
    metrics: MetricsConfig,
    logging: LoggingConfig,
    admin: AdminConfig,
    hedging: HedgeConfig,
    scoring: ScoringConfig,
    consensus: ConsensusConfig,
}
```

---

## 2. Proposed API Endpoints

### 2.1 Export Configuration

#### `GET /admin/config/export`

**Description:** Exports the complete running configuration including all runtime upstreams.

**Query Parameters:**
```typescript
{
  format?: "toml" | "json",          // Default: "toml"
  include_secrets?: boolean,          // Default: false
  include_runtime_only?: boolean,     // Default: false (include both config + runtime)
  include_metadata?: boolean          // Default: true (timestamps, source info)
}
```

**Response (200 OK):**
```typescript
{
  config: string,                    // Serialized config (TOML or JSON)
  format: "toml" | "json",
  exported_at: string,               // RFC3339 timestamp
  instance_name: string,
  version: string,
  metadata: {
    total_upstreams: number,
    config_upstreams: number,
    runtime_upstreams: number,
    secrets_masked: boolean
  }
}
```

**Example TOML Export:**
```toml
# Exported from: prism-prod-1
# Export date: 2025-12-07T10:30:00Z
# Version: 0.1.0-alpha.2

environment = "production"

[server]
bind_address = "0.0.0.0"
bind_port = 3030
max_concurrent_requests = 1000

# Config-based upstreams (from config file)
[[upstreams.providers]]
name = "infura-mainnet"
chain_id = 1
https_url = "https://mainnet.infura.io/v3/***"  # Masked
weight = 100
timeout_seconds = 30

# Runtime-added upstreams
[[upstreams.providers]]
# source: runtime
# added_at: 2025-12-07T09:15:00Z
# id: a7f3c2b1-4d8e-4f9a-9c3d-1e2f3a4b5c6d
name = "quicknode-backup"
chain_id = 1
https_url = "https://eth-mainnet.quicknode.pro/***"  # Masked
weight = 50
timeout_seconds = 30

[cache]
enabled = true
# ... rest of config ...
```

**Security Notes:**
- By default, all secrets (API keys in URLs, admin tokens) are masked with `***`
- `include_secrets=true` requires admin authentication and logs audit event
- URL masking uses existing `mask_url()` function from `upstreams.rs`

---

### 2.2 Persist Runtime Upstreams

#### `POST /admin/config/persist`

**Description:** Writes the current runtime upstreams to the configured persistence file.

**Request Body:**
```typescript
{
  backup_existing?: boolean,         // Default: true
  backup_path?: string              // Optional custom backup location
}
```

**Response (200 OK):**
```typescript
{
  success: true,
  persisted_count: number,
  backup_created: boolean,
  backup_path?: string,
  file_path: string
}
```

**Implementation:**
- Uses existing `RuntimeUpstreamRegistry::save_to_disk()`
- Creates timestamped backup: `runtime_upstreams.json.backup.2025-12-07T10-30-00Z`
- Atomic write using temp file + rename for safety

---

### 2.3 Import Configuration

#### `POST /admin/config/import`

**Description:** Imports upstreams from a configuration file.

**Request Body:**
```typescript
{
  config: string,                    // TOML or JSON config
  format: "toml" | "json",
  mode: "merge" | "replace",         // Default: "merge"
  dry_run?: boolean                  // Default: false
}
```

**Modes:**
- **merge**: Adds new runtime upstreams, skips conflicts
- **replace**: Removes all runtime upstreams and imports new ones (config upstreams untouched)

**Response (200 OK):**
```typescript
{
  success: true,
  imported: number,
  skipped: number,
  conflicts: string[],               // Names that conflicted
  dry_run: boolean
}
```

---

### 2.4 Get Export Metadata

#### `GET /admin/config/metadata`

**Description:** Returns metadata about the current configuration without exporting it.

**Response (200 OK):**
```typescript
{
  config_file_path: string,          // Original config file
  runtime_storage_path: string,      // Runtime upstreams JSON
  total_upstreams: number,
  config_upstreams: string[],        // Names from config file
  runtime_upstreams: Array<{
    id: string,
    name: string,
    created_at: string,
    updated_at: string
  }>,
  persistence_enabled: boolean
}
```

---

## 3. Data Flow Diagrams

### 3.1 Export Flow

```
┌──────────────┐
│ User Request │
│ GET /admin/  │
│ config/export│
└──────┬───────┘
       │
       ▼
┌────────────────────────────────────────────┐
│ handlers::config::export_config()          │
│                                            │
│ 1. Get config-based upstreams from         │
│    AppConfig (already loaded at startup)   │
│ 2. Get runtime upstreams from              │
│    RuntimeUpstreamRegistry::list_all()     │
│ 3. Merge both into exportable structure    │
│ 4. Mask secrets if needed                  │
│ 5. Serialize to TOML/JSON                  │
└────────────────┬───────────────────────────┘
                 │
                 ▼
         ┌──────────────┐
         │ Export Model │
         │              │
         │ - Upstreams  │
         │ - Server cfg │
         │ - Cache cfg  │
         │ - Other cfgs │
         └──────┬───────┘
                │
                ▼
    ┌─────────────────────┐
    │ Serialization Layer │
    │                     │
    │ TOML: toml::to_str()│
    │ JSON: serde_json    │
    └─────────┬───────────┘
              │
              ▼
    ┌──────────────────┐
    │ HTTP Response    │
    │ JSON with config │
    │ as string field  │
    └──────────────────┘
```

### 3.2 Persistence Flow

```
┌──────────────┐
│ User Request │
│ POST /admin/ │
│ config/      │
│ persist      │
└──────┬───────┘
       │
       ▼
┌────────────────────────────────────────────┐
│ handlers::config::persist_runtime()        │
│                                            │
│ 1. Create backup of existing file          │
│ 2. Call RuntimeUpstreamRegistry::          │
│    save_to_disk()                          │
│ 3. Verify write success                    │
└────────────────┬───────────────────────────┘
                 │
                 ▼
         ┌──────────────┐
         │ File System  │
         │              │
         │ Atomic write:│
         │ 1. tmp file  │
         │ 2. rename    │
         └──────────────┘
```

### 3.3 Import Flow

```
┌──────────────┐
│ User Request │
│ POST /admin/ │
│ config/import│
└──────┬───────┘
       │
       ▼
┌────────────────────────────────────────────┐
│ handlers::config::import_config()          │
│                                            │
│ 1. Parse TOML/JSON string                  │
│ 2. Extract upstream providers              │
│ 3. Validate each upstream                  │
│ 4. Check for conflicts                     │
│ 5. Add to RuntimeUpstreamRegistry          │
│    (uses existing add_runtime_upstream())  │
└────────────────┬───────────────────────────┘
                 │
                 ▼
         ┌──────────────┐
         │ Validation   │
         │              │
         │ - URL format │
         │ - Weight     │
         │ - Name clash │
         └──────┬───────┘
                │
                ▼
    ┌─────────────────────┐
    │ Runtime Registry    │
    │ Persistence Layer   │
    └─────────────────────┘
```

---

## 4. Implementation Approach

### 4.1 New Module Structure

**File:** `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/config.rs`

```rust
//! Configuration export/import handlers.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use prism_core::config::{AppConfig, UpstreamProvider};
use serde::{Deserialize, Serialize};
use std::fs;
use chrono::Utc;

use crate::admin::AdminState;

/// Export format enum
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Toml,
    Json,
}

/// Query parameters for export endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportQuery {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub include_secrets: bool,
    #[serde(default)]
    pub include_runtime_only: bool,
    #[serde(default = "default_true")]
    pub include_metadata: bool,
}

fn default_true() -> bool { true }

/// Export response
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExportResponse {
    pub config: String,
    pub format: String,
    pub exported_at: String,
    pub instance_name: String,
    pub version: String,
    pub metadata: ExportMetadata,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExportMetadata {
    pub total_upstreams: usize,
    pub config_upstreams: usize,
    pub runtime_upstreams: usize,
    pub secrets_masked: bool,
}

/// Exportable config structure (mirrors AppConfig but mutable)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportableConfig {
    pub environment: String,
    pub server: prism_core::config::ServerConfig,
    pub upstreams: Vec<UpstreamProvider>,
    pub cache: prism_core::config::CacheConfig,
    pub health_check: prism_core::config::HealthCheckConfig,
    pub auth: prism_core::config::AuthConfig,
    pub metrics: prism_core::config::MetricsConfig,
    pub logging: prism_core::config::LoggingConfig,
    pub admin: prism_core::config::AdminConfig,
    pub hedging: prism_core::upstream::HedgeConfig,
    pub scoring: prism_core::upstream::ScoringConfig,
    pub consensus: prism_core::upstream::ConsensusConfig,
}
```

### 4.2 Core Handler Implementations

```rust
/// GET /admin/config/export
#[utoipa::path(
    get,
    path = "/admin/config/export",
    tag = "Config",
    params(ExportQuery),
    responses(
        (status = 200, description = "Config exported", body = ExportResponse),
        (status = 500, description = "Export failed")
    )
)]
pub async fn export_config(
    State(state): State<AdminState>,
    Query(query): Query<ExportQuery>,
) -> Result<Json<ExportResponse>, (StatusCode, String)> {
    // 1. Build exportable config structure
    let mut exportable = build_exportable_config(&state);

    // 2. Add runtime upstreams
    let runtime_upstreams = state
        .proxy_engine
        .get_upstream_manager()
        .list_runtime_upstreams();

    let runtime_providers: Vec<UpstreamProvider> = runtime_upstreams
        .iter()
        .map(|ru| UpstreamProvider {
            name: ru.name.clone(),
            chain_id: ru.chain_id,
            https_url: if query.include_secrets {
                ru.url.clone()
            } else {
                mask_url(&ru.url)
            },
            wss_url: ru.ws_url.as_ref().map(|u| {
                if query.include_secrets { u.clone() } else { mask_url(u) }
            }),
            weight: ru.weight,
            timeout_seconds: ru.timeout_seconds,
            circuit_breaker_threshold: 5,  // Default
            circuit_breaker_timeout_seconds: 60,  // Default
        })
        .collect();

    // 3. Merge or filter based on query
    if query.include_runtime_only {
        exportable.upstreams = runtime_providers;
    } else {
        // Include both config + runtime
        exportable.upstreams.extend(runtime_providers);
    }

    // 4. Mask secrets if needed
    if !query.include_secrets {
        mask_config_secrets(&mut exportable);

        // Audit log if secrets were included
        if query.include_secrets {
            audit::log_read("config_export_with_secrets", "full", None);
        }
    }

    // 5. Serialize to requested format
    let format = query.format.as_deref().unwrap_or("toml");
    let config_string = match format {
        "json" => serde_json::to_string_pretty(&exportable)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,
                         format!("JSON serialization failed: {e}")))?,
        "toml" | _ => toml::to_string_pretty(&exportable)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,
                         format!("TOML serialization failed: {e}")))?,
    };

    // 6. Build response
    let config_upstream_count = state.config.upstreams.providers.len();
    let runtime_upstream_count = runtime_upstreams.len();

    Ok(Json(ExportResponse {
        config: config_string,
        format: format.to_string(),
        exported_at: Utc::now().to_rfc3339(),
        instance_name: state.config.admin.instance_name.clone(),
        version: state.version.to_string(),
        metadata: ExportMetadata {
            total_upstreams: config_upstream_count + runtime_upstream_count,
            config_upstreams: config_upstream_count,
            runtime_upstreams: runtime_upstream_count,
            secrets_masked: !query.include_secrets,
        },
    }))
}

/// Helper: Build exportable config from current state
fn build_exportable_config(state: &AdminState) -> ExportableConfig {
    ExportableConfig {
        environment: state.config.environment.clone(),
        server: state.config.server.clone(),
        upstreams: state.config.upstreams.providers.clone(),
        cache: state.config.cache.clone(),
        health_check: state.config.health_check.clone(),
        auth: state.config.auth.clone(),
        metrics: state.config.metrics.clone(),
        logging: state.config.logging.clone(),
        admin: state.config.admin.clone(),
        hedging: state.config.hedging.clone(),
        scoring: state.config.scoring.clone(),
        consensus: state.config.consensus.clone(),
    }
}

/// Helper: Mask secrets in config
fn mask_config_secrets(config: &mut ExportableConfig) {
    // Mask admin token
    if config.admin.admin_token.is_some() {
        config.admin.admin_token = Some("***".to_string());
    }

    // Mask auth database URL if it contains credentials
    if config.auth.database_url.contains('@') {
        config.auth.database_url = "***".to_string();
    }

    // Upstreams are already masked before this call
}

/// Helper: Reuse existing mask_url from upstreams.rs
fn mask_url(url: &str) -> String {
    super::upstreams::mask_url(url)
}
```

### 4.3 Persistence Handler

```rust
/// Persist request
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersistRequest {
    #[serde(default = "default_true")]
    pub backup_existing: bool,
    pub backup_path: Option<String>,
}

/// Persist response
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersistResponse {
    pub success: bool,
    pub persisted_count: usize,
    pub backup_created: bool,
    pub backup_path: Option<String>,
    pub file_path: String,
}

/// POST /admin/config/persist
#[utoipa::path(
    post,
    path = "/admin/config/persist",
    tag = "Config",
    request_body = PersistRequest,
    responses(
        (status = 200, description = "Persisted", body = PersistResponse),
        (status = 500, description = "Persistence failed")
    )
)]
pub async fn persist_runtime(
    State(state): State<AdminState>,
    Json(request): Json<PersistRequest>,
) -> Result<Json<PersistResponse>, (StatusCode, String)> {
    let registry = state.proxy_engine.get_upstream_manager().get_runtime_registry();

    // Get storage path
    let storage_path = registry.storage_path()
        .ok_or((StatusCode::BAD_REQUEST,
                "Runtime persistence not configured".to_string()))?;

    let mut backup_path = None;
    let mut backup_created = false;

    // Create backup if requested and file exists
    if request.backup_existing && storage_path.exists() {
        let timestamp = Utc::now().format("%Y-%m-%dT%H-%M-%SZ");
        let backup_file = if let Some(ref custom_path) = request.backup_path {
            PathBuf::from(custom_path)
        } else {
            storage_path.with_extension(
                format!("json.backup.{timestamp}")
            )
        };

        fs::copy(&storage_path, &backup_file)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,
                         format!("Backup creation failed: {e}")))?;

        backup_path = Some(backup_file.to_string_lossy().to_string());
        backup_created = true;
    }

    // Persist using existing registry method
    registry.save_to_disk()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,
                     format!("Persistence failed: {e}")))?;

    let runtime_upstreams = registry.list_all();

    audit::log_write("config_persist", "runtime_upstreams", None);

    Ok(Json(PersistResponse {
        success: true,
        persisted_count: runtime_upstreams.len(),
        backup_created,
        backup_path,
        file_path: storage_path.to_string_lossy().to_string(),
    }))
}
```

### 4.4 Import Handler

```rust
/// Import request
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    pub config: String,
    pub format: String,
    #[serde(default)]
    pub mode: ImportMode,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    Merge,
    Replace,
}

impl Default for ImportMode {
    fn default() -> Self { Self::Merge }
}

/// Import response
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportResponse {
    pub success: bool,
    pub imported: usize,
    pub skipped: usize,
    pub conflicts: Vec<String>,
    pub dry_run: bool,
}

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
    // 1. Parse config string
    let parsed_config: ExportableConfig = match request.format.as_str() {
        "json" => serde_json::from_str(&request.config)
            .map_err(|e| (StatusCode::BAD_REQUEST,
                         format!("JSON parse error: {e}")))?,
        "toml" => toml::from_str(&request.config)
            .map_err(|e| (StatusCode::BAD_REQUEST,
                         format!("TOML parse error: {e}")))?,
        _ => return Err((StatusCode::BAD_REQUEST,
                        "Invalid format. Must be 'json' or 'toml'".to_string())),
    };

    // 2. Handle replace mode
    if matches!(request.mode, ImportMode::Replace) && !request.dry_run {
        // Remove all runtime upstreams
        let manager = state.proxy_engine.get_upstream_manager();
        let existing = manager.list_runtime_upstreams();
        for upstream in existing {
            let _ = manager.remove_runtime_upstream(&upstream.id);
        }
    }

    // 3. Import upstreams
    let mut imported = 0;
    let mut skipped = 0;
    let mut conflicts = Vec::new();

    for provider in parsed_config.upstreams {
        // Skip if this is a config upstream (can't add runtime with same name)
        if state.proxy_engine.get_upstream_manager()
            .get_runtime_registry()
            .is_config_upstream(&provider.name)
        {
            conflicts.push(provider.name.clone());
            skipped += 1;
            continue;
        }

        if request.dry_run {
            imported += 1;
            continue;
        }

        // Attempt to add
        let create_request = prism_core::upstream::CreateUpstreamRequest {
            name: provider.name.clone(),
            url: provider.https_url,
            ws_url: provider.wss_url,
            weight: provider.weight,
            chain_id: provider.chain_id,
            timeout_seconds: provider.timeout_seconds,
        };

        match state.proxy_engine
            .get_upstream_manager()
            .add_runtime_upstream(create_request)
        {
            Ok(_) => imported += 1,
            Err(e) => {
                conflicts.push(format!("{}: {}", provider.name, e));
                skipped += 1;
            }
        }
    }

    Ok(Json(ImportResponse {
        success: true,
        imported,
        skipped,
        conflicts,
        dry_run: request.dry_run,
    }))
}
```

### 4.5 Metadata Handler

```rust
/// GET /admin/config/metadata
#[utoipa::path(
    get,
    path = "/admin/config/metadata",
    tag = "Config",
    responses(
        (status = 200, description = "Metadata", body = ConfigMetadata)
    )
)]
pub async fn get_metadata(
    State(state): State<AdminState>,
) -> Json<ConfigMetadata> {
    let registry = state.proxy_engine.get_upstream_manager().get_runtime_registry();

    let config_upstream_names: Vec<String> = state.config
        .upstreams
        .providers
        .iter()
        .map(|p| p.name.clone())
        .collect();

    let runtime_upstreams = registry.list_all();
    let runtime_metadata: Vec<RuntimeUpstreamMetadata> = runtime_upstreams
        .iter()
        .map(|ru| RuntimeUpstreamMetadata {
            id: ru.id.clone(),
            name: ru.name.clone(),
            created_at: ru.created_at.clone(),
            updated_at: ru.updated_at.clone(),
        })
        .collect();

    Json(ConfigMetadata {
        config_file_path: std::env::var("PRISM_CONFIG")
            .unwrap_or_else(|_| "config/config.toml".to_string()),
        runtime_storage_path: registry.storage_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "not configured".to_string()),
        total_upstreams: config_upstream_names.len() + runtime_upstreams.len(),
        config_upstreams: config_upstream_names,
        runtime_upstreams: runtime_metadata,
        persistence_enabled: registry.storage_path().is_some(),
    })
}
```

---

## 5. Security Considerations

### 5.1 Secret Masking Strategy

**Sensitive Fields:**
1. **Upstream URLs**: API keys in query params, path segments, or basic auth
2. **Admin Token**: `admin.admin_token`
3. **Database URLs**: Credentials in connection strings
4. **Prometheus URLs**: May contain auth tokens

**Masking Implementation:**
```rust
// Reuse existing mask_url() from upstreams.rs
fn mask_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return "[invalid-url]".to_string();
    };

    // Mask password
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some("***"));
    }

    // Mask sensitive query parameters
    let sensitive_keys = ["key", "apikey", "api_key", "token", "secret", "password", "auth"];
    // ... (existing implementation)

    parsed.to_string()
}
```

### 5.2 Access Control

**Authentication:**
- All config endpoints require admin authentication (via existing middleware)
- `include_secrets=true` requires additional audit logging
- Import operations log source IP and affected upstreams

**Audit Events:**
```
config_export_with_secrets  # When secrets are included
config_import               # When config is imported
config_persist              # When runtime upstreams persisted
```

### 5.3 File System Security

**Persistence File Permissions:**
- Runtime upstreams file: `0600` (owner read/write only)
- Backup files: `0600`
- Parent directory: `0700`

**Atomic Writes:**
```rust
// Existing implementation in RuntimeUpstreamRegistry
pub fn save_to_disk(&self) -> Result<(), std::io::Error> {
    // Write to temp file first
    let temp_path = self.storage_path.with_extension("tmp");
    fs::write(&temp_path, contents)?;

    // Atomic rename
    fs::rename(temp_path, &self.storage_path)?;

    Ok(())
}
```

---

## 6. Data Model Changes

### 6.1 Extend RuntimeUpstreamRegistry

**Add Method:**
```rust
impl RuntimeUpstreamRegistry {
    /// Returns the storage path if configured
    pub fn storage_path(&self) -> Option<&PathBuf> {
        self.storage_path.as_ref()
    }
}
```

### 6.2 Config Module Extensions

**Add to AppConfig:**
```rust
impl AppConfig {
    /// Exports config to TOML string
    pub fn export_to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self)
            .map_err(ConfigError::SerializationError)
    }

    /// Exports config to JSON string
    pub fn export_to_json(&self) -> Result<String, ConfigError> {
        serde_json::to_string_pretty(self)
            .map_err(ConfigError::SerializationError)
    }
}
```

### 6.3 New Types

**File:** `/home/flo/workspace/personal/prism/crates/server/src/admin/types.rs`

```rust
/// Config metadata response
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMetadata {
    pub config_file_path: String,
    pub runtime_storage_path: String,
    pub total_upstreams: usize,
    pub config_upstreams: Vec<String>,
    pub runtime_upstreams: Vec<RuntimeUpstreamMetadata>,
    pub persistence_enabled: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpstreamMetadata {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}
```

---

## 7. Risks and Mitigations

### 7.1 Risk: Accidental Secret Exposure

**Risk Level:** HIGH

**Scenario:** User exports config with `include_secrets=true` and shares the file.

**Mitigations:**
1. Default to masking secrets (`include_secrets=false`)
2. Log audit event when secrets are exported
3. Add prominent warning in API docs and OpenAPI spec
4. Response includes `secrets_masked: boolean` field for transparency
5. Consider adding `X-Admin-Confirm-Secrets: true` header requirement

**Implementation:**
```rust
if query.include_secrets {
    // Require explicit confirmation header
    if headers.get("X-Admin-Confirm-Secrets") != Some("true") {
        return Err((
            StatusCode::FORBIDDEN,
            "Include X-Admin-Confirm-Secrets: true header to export secrets".to_string()
        ));
    }

    audit::log_read("config_export_with_secrets", "full", Some(addr));
}
```

---

### 7.2 Risk: Config Overwrite Without Backup

**Risk Level:** MEDIUM

**Scenario:** User persists runtime upstreams, overwriting previous version without backup.

**Mitigations:**
1. Default to creating backups (`backup_existing=true`)
2. Atomic write (temp file + rename) prevents partial writes
3. Timestamped backups prevent name collisions
4. Fail-safe: If backup creation fails, abort persistence

**Implementation:**
```rust
if request.backup_existing && storage_path.exists() {
    let backup_result = fs::copy(&storage_path, &backup_file);
    if backup_result.is_err() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create backup. Aborting persistence.".to_string()
        ));
    }
}
```

---

### 7.3 Risk: Import of Malicious Config

**Risk Level:** MEDIUM

**Scenario:** User imports config with invalid URLs (SSRF), extreme weights, or DoS parameters.

**Mitigations:**
1. Reuse existing validation from `add_runtime_upstream()`
2. URL scheme validation (http/https only)
3. Weight range validation (1-1000)
4. Name format validation (alphanumeric + dash/underscore)
5. Dry-run mode for testing before commit
6. Rate limiting on import endpoint

**Validation:**
```rust
// Existing validation in manager.rs is reused
fn validate_upstream_request(request: &CreateUpstreamRequest) -> Result<(), UpstreamError> {
    // Name validation
    if !request.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err(UpstreamError::InvalidRequest("Invalid name format".to_string()));
    }

    // URL validation
    if url::Url::parse(&request.url).is_err() {
        return Err(UpstreamError::InvalidRequest("Invalid URL".to_string()));
    }

    // Weight validation
    if request.weight == 0 || request.weight > 1000 {
        return Err(UpstreamError::InvalidRequest("Weight out of range".to_string()));
    }

    Ok(())
}
```

---

### 7.4 Risk: File System Exhaustion

**Risk Level:** LOW

**Scenario:** Repeated exports/backups fill disk with backup files.

**Mitigations:**
1. Backups use timestamped names (automatic deduplication)
2. Document backup cleanup in ops guide
3. Consider implementing backup rotation (keep last N backups)
4. Monitor disk usage via existing metrics

**Future Enhancement:**
```rust
/// Rotates backups, keeping only the most recent N
fn rotate_backups(base_path: &Path, keep: usize) -> Result<(), std::io::Error> {
    let parent = base_path.parent().ok_or(...)?;
    let mut backups: Vec<_> = fs::read_dir(parent)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("runtime_upstreams.json.backup"))
        .collect();

    // Sort by modification time
    backups.sort_by_key(|e| e.metadata().ok()?.modified().ok());

    // Remove oldest
    for backup in backups.iter().take(backups.len().saturating_sub(keep)) {
        fs::remove_file(backup.path())?;
    }

    Ok(())
}
```

---

### 7.5 Risk: Inconsistent State After Partial Import

**Risk Level:** MEDIUM

**Scenario:** Import adds 5 upstreams successfully, then fails on the 6th. State is inconsistent.

**Mitigations:**
1. Import is transactional per-upstream (existing behavior)
2. Response includes `conflicts` array showing failures
3. Dry-run mode allows validation before commit
4. `mode=replace` clears state before import (atomic operation)
5. Failed imports don't corrupt existing upstreams

**Transactional Import (Future Enhancement):**
```rust
// Stage all upstreams first, commit only if all valid
let mut staged = Vec::new();
for provider in parsed_config.upstreams {
    let request = CreateUpstreamRequest { ... };
    validate_upstream_request(&request)?;  // Fail fast
    staged.push(request);
}

// All valid - now commit
for request in staged {
    manager.add_runtime_upstream(request)?;
}
```

---

## 8. Testing Strategy

### 8.1 Unit Tests

**File:** `/home/flo/workspace/personal/prism/crates/server/tests/config_export_tests.rs`

```rust
#[tokio::test]
async fn test_export_toml_format() {
    let state = create_test_admin_state().await;
    let query = ExportQuery {
        format: Some("toml".to_string()),
        include_secrets: false,
        include_runtime_only: false,
        include_metadata: true,
    };

    let result = export_config(State(state), Query(query)).await;
    assert!(result.is_ok());

    let response = result.unwrap().0;
    assert_eq!(response.format, "toml");
    assert!(toml::from_str::<ExportableConfig>(&response.config).is_ok());
}

#[tokio::test]
async fn test_export_masks_secrets() {
    let state = create_test_admin_state().await;
    let query = ExportQuery {
        format: Some("toml".to_string()),
        include_secrets: false,
        include_runtime_only: false,
        include_metadata: true,
    };

    let result = export_config(State(state), Query(query)).await;
    let response = result.unwrap().0;

    assert!(response.metadata.secrets_masked);
    assert!(!response.config.contains("actual_api_key"));
    assert!(response.config.contains("***"));
}

#[tokio::test]
async fn test_import_merge_mode() {
    let state = create_test_admin_state().await;

    let config_toml = r#"
        [[upstreams.providers]]
        name = "imported-upstream"
        chain_id = 1
        https_url = "https://imported.example.com"
        weight = 50
    "#;

    let request = ImportRequest {
        config: config_toml.to_string(),
        format: "toml".to_string(),
        mode: ImportMode::Merge,
        dry_run: false,
    };

    let result = import_config(State(state.clone()), Json(request)).await;
    assert!(result.is_ok());

    let response = result.unwrap().0;
    assert_eq!(response.imported, 1);
    assert_eq!(response.skipped, 0);

    // Verify it was added
    let upstreams = state.proxy_engine.get_upstream_manager().list_runtime_upstreams();
    assert_eq!(upstreams.len(), 1);
    assert_eq!(upstreams[0].name, "imported-upstream");
}

#[tokio::test]
async fn test_persist_creates_backup() {
    let state = create_test_admin_state_with_storage().await;

    // Add a runtime upstream first
    add_test_runtime_upstream(&state).await;

    let request = PersistRequest {
        backup_existing: true,
        backup_path: None,
    };

    let result = persist_runtime(State(state), Json(request)).await;
    assert!(result.is_ok());

    let response = result.unwrap().0;
    assert!(response.backup_created);
    assert!(response.backup_path.is_some());

    // Verify backup file exists
    let backup_path = PathBuf::from(response.backup_path.unwrap());
    assert!(backup_path.exists());
}
```

### 8.2 Integration Tests

**Scenarios:**
1. Export → Import roundtrip preserves configuration
2. Persist → Restart → Verify runtime upstreams restored
3. Import with conflicts → Verify skipped upstreams
4. Export with secrets → Verify masking
5. Replace mode → Verify old upstreams removed

---

## 9. Documentation Requirements

### 9.1 API Documentation (OpenAPI)

**Updates to `/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs`:**

```rust
#[openapi(
    paths(
        // ... existing paths
        handlers::config::export_config,
        handlers::config::persist_runtime,
        handlers::config::import_config,
        handlers::config::get_metadata,
    ),
    components(schemas(
        // ... existing schemas
        handlers::config::ExportResponse,
        handlers::config::ExportMetadata,
        handlers::config::PersistResponse,
        handlers::config::ImportResponse,
        handlers::config::ConfigMetadata,
    )),
)]
```

### 9.2 User Guide

**File:** `/home/flo/workspace/personal/prism/docs/admin-api/config-export.md`

```markdown
# Configuration Export and Persistence

## Overview

The Prism Admin API provides endpoints for exporting, persisting, and importing
upstream configurations. This allows you to:

- Back up your current configuration (including runtime upstreams)
- Transfer configurations between server instances
- Persist runtime changes for automatic reload on restart

## Use Cases

### 1. Backup Before Changes

```bash
# Export current config to file
curl -H "X-Admin-Token: $TOKEN" \
  http://localhost:3031/admin/config/export?format=toml \
  | jq -r '.config' > backup-$(date +%Y%m%d).toml
```

### 2. Migrate Configuration to New Server

```bash
# On source server
curl -H "X-Admin-Token: $TOKEN" \
  http://localhost:3031/admin/config/export?format=toml \
  | jq -r '.config' > export.toml

# On target server
curl -X POST -H "X-Admin-Token: $TOKEN" \
  -H "Content-Type: application/json" \
  -d @- http://localhost:3031/admin/config/import <<EOF
{
  "config": "$(cat export.toml | jq -Rs .)",
  "format": "toml",
  "mode": "merge"
}
EOF
```

### 3. Persist Runtime Changes

```bash
# After adding upstreams via API
curl -X POST -H "X-Admin-Token: $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"backup_existing": true}' \
  http://localhost:3031/admin/config/persist
```

## Security Notes

- **Never commit exported configs with secrets to git**
- Use `include_secrets=false` (default) for sharing configs
- Store configs with secrets in encrypted vaults (e.g., HashiCorp Vault)
- Audit logs track all exports with secrets

## API Reference

See [OpenAPI documentation](http://localhost:3031/admin/swagger-ui)
```

---

## 10. Implementation Checklist

### Phase 1: Core Export Functionality
- [ ] Create `/crates/server/src/admin/handlers/config.rs`
- [ ] Implement `export_config()` handler with TOML support
- [ ] Add JSON export support
- [ ] Implement secret masking
- [ ] Add to admin router
- [ ] Write unit tests
- [ ] Update OpenAPI spec

### Phase 2: Persistence
- [ ] Extend `RuntimeUpstreamRegistry` with `storage_path()` getter
- [ ] Implement `persist_runtime()` handler
- [ ] Add backup creation logic
- [ ] Test atomic writes
- [ ] Write integration tests

### Phase 3: Import
- [ ] Implement `import_config()` handler
- [ ] Add validation pipeline
- [ ] Test merge mode
- [ ] Test replace mode
- [ ] Test dry-run mode
- [ ] Add conflict resolution tests

### Phase 4: Metadata & Utilities
- [ ] Implement `get_metadata()` handler
- [ ] Add startup logic to load runtime upstreams from disk
- [ ] Test restart persistence
- [ ] Add backup rotation (optional)

### Phase 5: Documentation & Polish
- [ ] Write user guide
- [ ] Add API examples to OpenAPI
- [ ] Update architecture docs
- [ ] Add audit logging
- [ ] Security review
- [ ] Performance testing

---

## 11. Future Enhancements

### 11.1 Config Diffing
- `GET /admin/config/diff?base=file&compare=runtime`
- Shows differences between file config and running config
- Useful for detecting drift

### 11.2 Scheduled Backups
- Automatic periodic exports to S3/filesystem
- Configurable retention policy
- Integration with existing backup systems

### 11.3 Config Validation Service
- `POST /admin/config/validate` with config payload
- Returns validation errors without applying
- Useful in CI/CD pipelines

### 11.4 Multi-Instance Sync
- Sync runtime upstreams across cluster via etcd/consul
- Real-time propagation of changes
- Leader election for write coordination

### 11.5 Rollback Support
- Track config versions
- `POST /admin/config/rollback?version=N`
- Automatic rollback on health check failures

---

## 12. Conclusion

This design provides a comprehensive, production-ready solution for config export and persistence with:

- **Safety**: Backup-before-write, atomic operations, validation
- **Security**: Secret masking, audit logging, access control
- **Usability**: Multiple formats, dry-run mode, clear error messages
- **Maintainability**: Leverages existing code, minimal new dependencies
- **Extensibility**: Clear upgrade path for future enhancements

The implementation reuses existing components (RuntimeUpstreamRegistry, validation logic, serialization) and follows established patterns in the codebase, minimizing risk and development time.

**Estimated implementation time:** 3-4 days for core functionality + tests + docs

**Risk assessment:** LOW - builds on proven persistence layer, uses standard serialization libraries
