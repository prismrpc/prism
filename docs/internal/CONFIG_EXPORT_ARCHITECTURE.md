# Config Export/Persistence - Architecture Diagrams

## System Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         PRISM RPC PROXY SERVER                           │
│                                                                           │
│  ┌─────────────────────┐                  ┌──────────────────────────┐  │
│  │   Config Module     │                  │   Admin API Module       │  │
│  │  (prism-core)       │                  │    (server)              │  │
│  │                     │                  │                          │  │
│  │ ┌─────────────────┐ │                  │  ┌──────────────────┐   │  │
│  │ │   AppConfig     │ │   Read-only      │  │ config::export() │   │  │
│  │ │  (TOML file)    │ │◄─────────────────┤  │                  │   │  │
│  │ │                 │ │                  │  │ - Merge sources  │   │  │
│  │ │ - Server cfg    │ │                  │  │ - Mask secrets   │   │  │
│  │ │ - Cache cfg     │ │                  │  │ - Serialize      │   │  │
│  │ │ - Config        │ │                  │  └──────────────────┘   │  │
│  │ │   upstreams     │ │                  │                          │  │
│  │ └─────────────────┘ │                  │  ┌──────────────────┐   │  │
│  └─────────────────────┘                  │  │ config::persist()│   │  │
│                                            │  │                  │   │  │
│  ┌─────────────────────┐                  │  │ - Backup file    │   │  │
│  │ Runtime Registry    │   Read/Write     │  │ - Atomic write   │   │  │
│  │  (prism-core)       │◄────────────────►│  │ - Verify         │   │  │
│  │                     │                  │  └──────────────────┘   │  │
│  │ ┌─────────────────┐ │                  │                          │  │
│  │ │RuntimeUpstream  │ │                  │  ┌──────────────────┐   │  │
│  │ │   Registry      │ │                  │  │ config::import() │   │  │
│  │ │  (JSON file)    │ │                  │  │                  │   │  │
│  │ │                 │ │                  │  │ - Parse config   │   │  │
│  │ │ - ID mapping    │ │   Auto-load      │  │ - Validate       │   │  │
│  │ │ - Metadata      │ │──on startup─────►│  │ - Add upstreams  │   │  │
│  │ │ - Timestamps    │ │                  │  └──────────────────┘   │  │
│  │ └─────────────────┘ │                  │                          │  │
│  └─────────────────────┘                  │  ┌──────────────────┐   │  │
│           │                                │  │ config::         │   │  │
│           │                                │  │ get_metadata()   │   │  │
│  ┌────────▼────────────┐                  │  └──────────────────┘   │  │
│  │ UpstreamManager     │                  │                          │  │
│  │                     │                  └──────────────────────────┘  │
│  │ - Config upstreams  │                                                │
│  │ - Runtime upstreams │                                                │
│  │ - LoadBalancer      │                                                │
│  └─────────────────────┘                                                │
└─────────────────────────────────────────────────────────────────────────┘

                                   │
                                   │  File System
                                   ▼
                    ┌──────────────────────────────┐
                    │  Disk Persistence            │
                    │                              │
                    │  config/development.toml     │
                    │  data/runtime_upstreams.json │
                    │  data/backups/*.backup       │
                    └──────────────────────────────┘
```

## Data Flow: Export Configuration

```
┌──────────┐
│  Client  │
│  Request │
│          │
│ GET /    │
│ admin/   │
│ config/  │
│ export   │
└────┬─────┘
     │
     │  Query params:
     │  - format: toml/json
     │  - include_secrets: bool
     │  - include_runtime_only: bool
     │
     ▼
┌──────────────────────────────────────────────────┐
│ handlers::config::export_config()                │
│                                                  │
│ Step 1: Gather Config Sources                   │
│ ┌──────────────────────────────────────────┐    │
│ │ Source A: AppConfig (loaded at startup)  │    │
│ │  - server, cache, auth, metrics, etc.    │    │
│ │  - config-based upstreams                │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ ┌──────────────────────────────────────────┐    │
│ │ Source B: RuntimeUpstreamRegistry        │    │
│ │  registry.list_all()                     │    │
│ │  - runtime-added upstreams               │    │
│ │  - metadata (id, created_at, etc.)       │    │
│ └──────────────────────────────────────────┘    │
│                                                  │
│ Step 2: Build Exportable Structure              │
│ ┌──────────────────────────────────────────┐    │
│ │ ExportableConfig {                       │    │
│ │   environment,                           │    │
│ │   server,                                │    │
│ │   upstreams: [                           │    │
│ │     ...config_upstreams,                 │    │
│ │     ...runtime_upstreams ←MERGED         │    │
│ │   ],                                     │    │
│ │   cache,                                 │    │
│ │   ...                                    │    │
│ │ }                                        │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 3: Mask Secrets (if include_secrets=false) │
│ ┌──────────────────────────────────────────┐    │
│ │ For each upstream.url:                   │    │
│ │   mask_url() -> query params masked      │    │
│ │ For admin_token:                         │    │
│ │   -> "***"                               │    │
│ │ For database_url:                        │    │
│ │   -> check for credentials, mask if found│    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 4: Serialize                                │
│ ┌──────────────────────────────────────────┐    │
│ │ match format {                           │    │
│ │   "toml" => toml::to_string_pretty(),    │    │
│ │   "json" => serde_json::to_string_pretty│    │
│ │ }                                        │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 5: Build Response                          │
│ ┌──────────────────────────────────────────┐    │
│ │ ExportResponse {                         │    │
│ │   config: String,  // Serialized TOML    │    │
│ │   format: "toml",                        │    │
│ │   exported_at: "2025-12-07T...",         │    │
│ │   metadata: {                            │    │
│ │     total_upstreams: 15,                 │    │
│ │     config_upstreams: 10,                │    │
│ │     runtime_upstreams: 5,                │    │
│ │     secrets_masked: true                 │    │
│ │   }                                      │    │
│ │ }                                        │    │
│ └──────────────────────────────────────────┘    │
└────────────────────────┬─────────────────────────┘
                         │
                         ▼
                  ┌────────────┐
                  │  Client    │
                  │  Response  │
                  │  (JSON)    │
                  └────────────┘
```

## Data Flow: Persist Runtime Upstreams

```
┌──────────┐
│  Client  │
│  Request │
│          │
│ POST /   │
│ admin/   │
│ config/  │
│ persist  │
└────┬─────┘
     │
     │  Body: { backup_existing: true }
     │
     ▼
┌──────────────────────────────────────────────────┐
│ handlers::config::persist_runtime()              │
│                                                  │
│ Step 1: Get Storage Path                        │
│ ┌──────────────────────────────────────────┐    │
│ │ registry = upstream_manager              │    │
│ │   .get_runtime_registry()                │    │
│ │                                          │    │
│ │ storage_path = registry.storage_path()   │    │
│ │   Some("/data/runtime_upstreams.json")   │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 2: Create Backup (if requested)            │
│ ┌──────────────────────────────────────────┐    │
│ │ if backup_existing &&                    │    │
│ │    storage_path.exists():                │    │
│ │                                          │    │
│ │   timestamp = now().format(...)          │    │
│ │   backup_path = storage_path             │    │
│ │     .with_extension("json.backup.{ts}")  │    │
│ │                                          │    │
│ │   fs::copy(storage_path, backup_path)    │    │
│ │                                          │    │
│ │   ✓ Backup created                       │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 3: Persist (Atomic Write)                  │
│ ┌──────────────────────────────────────────┐    │
│ │ registry.save_to_disk()                  │    │
│ │                                          │    │
│ │ Internal flow:                           │    │
│ │ 1. Serialize to JSON                     │    │
│ │ 2. Write to temp file (.tmp)             │    │
│ │ 3. Atomic rename temp -> target          │    │
│ │                                          │    │
│ │ Ensures no partial writes!               │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 4: Verify & Respond                        │
│ ┌──────────────────────────────────────────┐    │
│ │ runtime_upstreams =                      │    │
│ │   registry.list_all()                    │    │
│ │                                          │    │
│ │ PersistResponse {                        │    │
│ │   success: true,                         │    │
│ │   persisted_count: 5,                    │    │
│ │   backup_created: true,                  │    │
│ │   backup_path: Some("..."),              │    │
│ │   file_path: "/data/runtime_..."         │    │
│ │ }                                        │    │
│ └──────────────────────────────────────────┘    │
└────────────────────────┬─────────────────────────┘
                         │
                         ▼
                  ┌────────────┐
                  │ File System│
                  │            │
                  │ ✓ runtime_ │
                  │   upstreams│
                  │   .json    │
                  │            │
                  │ ✓ .backup  │
                  │   file     │
                  └────────────┘
```

## Data Flow: Import Configuration

```
┌──────────┐
│  Client  │
│  Request │
│          │
│ POST /   │
│ admin/   │
│ config/  │
│ import   │
└────┬─────┘
     │
     │  Body: {
     │    config: "...",  // TOML/JSON string
     │    format: "toml",
     │    mode: "merge",
     │    dry_run: false
     │  }
     │
     ▼
┌──────────────────────────────────────────────────┐
│ handlers::config::import_config()                │
│                                                  │
│ Step 1: Parse Config String                     │
│ ┌──────────────────────────────────────────┐    │
│ │ parsed_config: ExportableConfig =        │    │
│ │   match format {                         │    │
│ │     "toml" => toml::from_str(config),    │    │
│ │     "json" => serde_json::from_str(...)  │    │
│ │   }                                      │    │
│ │                                          │    │
│ │ ✓ Syntax validated                       │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 2: Handle Mode                             │
│ ┌──────────────────────────────────────────┐    │
│ │ if mode == "replace" && !dry_run:        │    │
│ │   // Clear all runtime upstreams         │    │
│ │   for upstream in registry.list_all():   │    │
│ │     manager.remove_runtime_upstream(id)  │    │
│ │                                          │    │
│ │ if mode == "merge":                      │    │
│ │   // Keep existing, add new              │    │
│ │   (default behavior)                     │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 3: Import Each Upstream                    │
│ ┌──────────────────────────────────────────┐    │
│ │ for provider in parsed_config.upstreams: │    │
│ │                                          │    │
│ │   ┌────────────────────────────────┐     │    │
│ │   │ Conflict Check                 │     │    │
│ │   │ if registry.is_config_upstream │     │    │
│ │   │    (provider.name):            │     │    │
│ │   │   conflicts.push(name)         │     │    │
│ │   │   skipped += 1                 │     │    │
│ │   │   continue                     │     │    │
│ │   └────────────────────────────────┘     │    │
│ │            │                             │    │
│ │            ▼                             │    │
│ │   ┌────────────────────────────────┐     │    │
│ │   │ Validation                     │     │    │
│ │   │ validate_upstream_request():   │     │    │
│ │   │   - URL format                 │     │    │
│ │   │   - Weight range (1-1000)      │     │    │
│ │   │   - Name format (alphanumeric) │     │    │
│ │   └────────────────────────────────┘     │    │
│ │            │                             │    │
│ │            ▼                             │    │
│ │   ┌────────────────────────────────┐     │    │
│ │   │ Add to Registry                │     │    │
│ │   │ if !dry_run:                   │     │    │
│ │   │   manager.add_runtime_upstream │     │    │
│ │   │     (create_request)           │     │    │
│ │   │   imported += 1                │     │    │
│ │   └────────────────────────────────┘     │    │
│ │                                          │    │
│ └──────────────────────────────────────────┘    │
│           │                                      │
│           ▼                                      │
│ Step 4: Build Response                          │
│ ┌──────────────────────────────────────────┐    │
│ │ ImportResponse {                         │    │
│ │   success: true,                         │    │
│ │   imported: 8,                           │    │
│ │   skipped: 2,                            │    │
│ │   conflicts: [                           │    │
│ │     "infura-mainnet: conflicts with..." │    │
│ │   ],                                     │    │
│ │   dry_run: false                         │    │
│ │ }                                        │    │
│ └──────────────────────────────────────────┘    │
└────────────────────────┬─────────────────────────┘
                         │
                         ▼
                  ┌────────────┐
                  │ Runtime    │
                  │ Registry   │
                  │            │
                  │ ✓ New      │
                  │   upstreams│
                  │   added    │
                  └────────────┘
```

## Component Interaction Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Admin API Handler Layer                     │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  export_config()      persist_runtime()       import_config()      │
│       │                      │                       │             │
│       │                      │                       │             │
└───────┼──────────────────────┼───────────────────────┼─────────────┘
        │                      │                       │
        │                      │                       │
        ▼                      ▼                       ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      Core Business Logic Layer                      │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  ┌──────────────┐      ┌──────────────┐      ┌──────────────┐     │
│  │  AppConfig   │      │  Runtime     │      │  Upstream    │     │
│  │              │      │  Upstream    │      │  Manager     │     │
│  │ - Load from  │      │  Registry    │      │              │     │
│  │   TOML       │      │              │      │ - add_       │     │
│  │ - Validate   │◄─────┤ - add()      │◄─────┤   runtime_   │     │
│  │ - Convert    │      │ - update()   │      │   upstream() │     │
│  │              │      │ - remove()   │      │              │     │
│  │ - upstreams()│      │ - list_all() │      │ - remove_    │     │
│  │   getter     │      │              │      │   runtime_   │     │
│  │              │      │ - save_to_   │      │   upstream() │     │
│  │              │      │   disk()     │      │              │     │
│  │              │      │              │      │ - list_      │     │
│  │              │      │ - load_from_ │      │   runtime_   │     │
│  │              │      │   disk()     │      │   upstreams()│     │
│  └──────────────┘      └──────────────┘      └──────────────┘     │
│         │                      │                       │           │
│         │                      │                       │           │
└─────────┼──────────────────────┼───────────────────────┼───────────┘
          │                      │                       │
          │                      │                       │
          ▼                      ▼                       ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      Serialization Layer                            │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  ┌──────────────┐                         ┌──────────────┐         │
│  │  TOML        │                         │  JSON        │         │
│  │  (toml crate)│                         │  (serde_json)│         │
│  │              │                         │              │         │
│  │ - Serialize  │                         │ - Serialize  │         │
│  │ - Deserialize│                         │ - Deserialize│         │
│  └──────────────┘                         └──────────────┘         │
│         │                                        │                 │
│         │                                        │                 │
└─────────┼────────────────────────────────────────┼─────────────────┘
          │                                        │
          │                                        │
          ▼                                        ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         Storage Layer                               │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  config/development.toml          data/runtime_upstreams.json       │
│  (Read-only after startup)        (Read/Write, auto-reloaded)      │
│                                                                     │
│  data/runtime_upstreams.json.backup.2025-12-07T10-30-00Z           │
│  (Timestamped backups)                                              │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

## Startup Sequence with Runtime Upstreams

```
┌────────────┐
│ main()     │
│ starts     │
└─────┬──────┘
      │
      ▼
┌────────────────────────────────────────┐
│ 1. Load Config                         │
│                                        │
│ AppConfig::load()                      │
│   └─> Read config/development.toml     │
│   └─> Parse TOML                       │
│   └─> Validate                         │
│   └─> Store config_upstreams           │
└─────┬──────────────────────────────────┘
      │
      ▼
┌────────────────────────────────────────┐
│ 2. Build UpstreamManager               │
│                                        │
│ UpstreamManagerBuilder::new()          │
│   .runtime_storage_path(               │
│      "data/runtime_upstreams.json"     │
│   )                                    │
│   .build()                             │
│                                        │
│ ✓ Creates RuntimeUpstreamRegistry      │
│   with storage path                    │
└─────┬──────────────────────────────────┘
      │
      ▼
┌────────────────────────────────────────┐
│ 3. Load Config-Based Upstreams         │
│                                        │
│ for upstream in config.upstreams():    │
│   manager.add_upstream(upstream)       │
│                                        │
│ ✓ Config upstreams registered          │
│   in UpstreamManager                   │
│                                        │
│ registry.initialize_config_upstreams(  │
│   config_names                         │
│ )                                      │
│                                        │
│ ✓ Registry knows which names are       │
│   from config (prevents conflicts)     │
└─────┬──────────────────────────────────┘
      │
      ▼
┌────────────────────────────────────────┐
│ 4. Load Runtime Upstreams from Disk    │
│                                        │
│ registry.load_from_disk()              │
│                                        │
│ if file exists:                        │
│   ├─> Read JSON file                   │
│   ├─> Deserialize                      │
│   ├─> Populate registry hashmap        │
│   └─> Add to UpstreamManager           │
│                                        │
│ ✓ Runtime upstreams restored           │
└─────┬──────────────────────────────────┘
      │
      ▼
┌────────────────────────────────────────┐
│ 5. Start Health Checker                │
│                                        │
│ HealthChecker monitors ALL upstreams:  │
│   - Config-based                       │
│   - Runtime-added                      │
│                                        │
│ ✓ System ready                         │
└─────┬──────────────────────────────────┘
      │
      ▼
┌────────────────────────────────────────┐
│ Server Running                         │
│                                        │
│ - Total upstreams: 15                  │
│   - Config: 10                         │
│   - Runtime: 5 (restored from disk)    │
└────────────────────────────────────────┘
```

## File Structure

```
prism/
├── config/
│   ├── development.toml              # Main config (read-only after startup)
│   ├── production.toml
│   └── example.toml
│
├── data/                             # Runtime data directory
│   ├── runtime_upstreams.json        # Persisted runtime upstreams
│   │                                 # Auto-loaded on startup
│   │                                 # Updated via Admin API
│   │
│   └── backups/                      # Timestamped backups
│       ├── runtime_upstreams.json.backup.2025-12-07T10-30-00Z
│       ├── runtime_upstreams.json.backup.2025-12-07T14-15-30Z
│       └── runtime_upstreams.json.backup.2025-12-07T18-45-12Z
│
├── crates/
│   ├── prism-core/
│   │   └── src/
│   │       ├── config/
│   │       │   └── mod.rs            # AppConfig, serialization
│   │       │
│   │       └── upstream/
│   │           ├── manager.rs        # UpstreamManager
│   │           │                     # - add_runtime_upstream()
│   │           │                     # - update_runtime_upstream()
│   │           │                     # - remove_runtime_upstream()
│   │           │                     # - list_runtime_upstreams()
│   │           │
│   │           ├── runtime_registry.rs  # RuntimeUpstreamRegistry
│   │           │                        # - save_to_disk()
│   │           │                        # - load_from_disk()
│   │           │                        # - add() / update() / remove()
│   │           │
│   │           └── builder.rs        # UpstreamManagerBuilder
│   │                                 # - runtime_storage_path()
│   │
│   └── server/
│       └── src/
│           └── admin/
│               ├── mod.rs            # Router setup
│               │
│               └── handlers/
│                   ├── config.rs     # NEW: Export/Import/Persist
│                   │                 # - export_config()
│                   │                 # - persist_runtime()
│                   │                 # - import_config()
│                   │                 # - get_metadata()
│                   │
│                   └── upstreams.rs  # Existing upstream CRUD
│                                     # - create_upstream()
│                                     # - update_upstream()
│                                     # - delete_upstream()
│
└── docs/
    └── admin-api/
        ├── config-export.md          # User guide
        └── admin-api-specs.md        # API reference (updated)
```

## Security Boundary Diagram

```
┌────────────────────────────────────────────────────────────────────┐
│                        PUBLIC NETWORK                              │
│                                                                    │
│  ┌──────────────┐                                                 │
│  │   Client     │                                                 │
│  │   (curl/UI)  │                                                 │
│  └──────┬───────┘                                                 │
│         │                                                         │
└─────────┼─────────────────────────────────────────────────────────┘
          │  HTTPS (TLS 1.3)
          │
┌─────────┼─────────────────────────────────────────────────────────┐
│         │           ADMIN API SERVER (Port 3031)                  │
│         │                                                         │
│  ┌──────▼──────────────────────────────────────────────┐          │
│  │ Authentication Middleware                           │          │
│  │ - Verify X-Admin-Token header                       │          │
│  │ - Reject if missing/invalid                         │          │
│  │ - Log auth attempts                                 │          │
│  └──────┬──────────────────────────────────────────────┘          │
│         │                                                         │
│  ┌──────▼──────────────────────────────────────────────┐          │
│  │ Rate Limiting Middleware                            │          │
│  │ - Token bucket (100 req burst, 10 req/sec)          │          │
│  │ - Per-IP limiting                                   │          │
│  └──────┬──────────────────────────────────────────────┘          │
│         │                                                         │
│  ┌──────▼──────────────────────────────────────────────┐          │
│  │ Export Handler                                      │          │
│  │                                                     │          │
│  │ IF include_secrets=true:                            │          │
│  │   ┌────────────────────────────────────┐            │          │
│  │   │ SECURITY GATE                      │            │          │
│  │   │                                    │            │          │
│  │   │ - Require X-Admin-Confirm-Secrets │            │          │
│  │   │   header = "true"                  │            │          │
│  │   │                                    │            │          │
│  │   │ - Log audit event:                 │            │          │
│  │   │   * IP address                     │            │          │
│  │   │   * Timestamp                      │            │          │
│  │   │   * Action: "export_with_secrets" │            │          │
│  │   └────────────────────────────────────┘            │          │
│  │                                                     │          │
│  │ ELSE (default):                                     │          │
│  │   ┌────────────────────────────────────┐            │          │
│  │   │ Secret Masking                     │            │          │
│  │   │                                    │            │          │
│  │   │ - mask_url() for upstream URLs     │            │          │
│  │   │ - admin_token -> "***"            │            │          │
│  │   │ - database_url credentials masked │            │          │
│  │   └────────────────────────────────────┘            │          │
│  │                                                     │          │
│  └─────────────────────────────────────────────────────┘          │
│                                                                   │
│  ┌─────────────────────────────────────────────────────┐          │
│  │ File System Access Control                          │          │
│  │                                                     │          │
│  │ data/runtime_upstreams.json      (0600)             │          │
│  │ data/backups/*.backup            (0600)             │          │
│  │                                                     │          │
│  │ Only owner (prism process) can read/write           │          │
│  └─────────────────────────────────────────────────────┘          │
└───────────────────────────────────────────────────────────────────┘
```

## Recommendations Summary

### Critical Implementation Points

1. **Secret Masking**: Always default to masking, require explicit confirmation for unmasked export
2. **Atomic Writes**: Use temp file + rename for persistence to prevent corruption
3. **Backup Before Write**: Always create timestamped backups before modifying files
4. **Validation**: Reuse existing validation logic from `add_runtime_upstream()`
5. **Audit Logging**: Log all config exports, imports, and persistence operations

### Performance Considerations

1. **Export**: O(n) where n = total upstreams, typically < 100ms for 100 upstreams
2. **Persist**: File I/O is async, uses atomic write pattern (< 50ms)
3. **Import**: Sequential validation + add, ~10ms per upstream

### Scalability Limits

- **Max upstreams**: ~1000 (limited by TOML parser memory, not storage)
- **Max config size**: ~10MB (practical limit for TOML/JSON parsing)
- **Backup retention**: Recommend keeping last 10 backups, rotate automatically
