# Runtime Upstreams Naming Analysis

**Date**: 2025-12-07
**Issue**: Naming conflict between "runtime upstreams" and the Prism runtime module
**Analyzed by**: Rust Engineer Agent

---

## Executive Summary

**Problem Identified**: The term "runtime" is overloaded in the Prism RPC Proxy codebase:
1. **Prism Runtime Module** (`crates/prism-core/src/runtime/`): Core component initialization and lifecycle management
2. **Runtime Upstreams**: Upstreams added dynamically via Admin API (vs config-file upstreams)

This creates cognitive overhead and potential confusion for developers.

**Recommended Renaming**: `runtime upstreams` → **`dynamic upstreams`**

**Impact**: 23 files requiring updates (estimated 2-3 hours of work)

---

## 1. Current "Runtime" Usage Analysis

### 1.1 Prism Runtime Module (Core Infrastructure)

**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/runtime/`

**Purpose**: Unified initialization API for all Prism core components, lifecycle management, and graceful shutdown coordination.

**Files**:
- `runtime/mod.rs` - Module documentation and exports
- `runtime/builder.rs` - `PrismRuntimeBuilder` for component initialization
- `runtime/components.rs` - `PrismComponents` container
- `runtime/lifecycle.rs` - `PrismRuntime` lifecycle management

**Key Types**:
- `PrismRuntime` - Main runtime lifecycle manager
- `PrismRuntimeBuilder` - Builder pattern for initialization
- `PrismComponents` - Container for initialized components

**Usage Example**:
```rust
let runtime = PrismRuntime::builder()
    .with_config(config)
    .enable_health_checker()
    .enable_websocket_subscriptions()
    .build()?;
```

**Documentation Context**:
> "Prism runtime initialization and lifecycle management. This module provides a unified initialization API for all Prism core components, suitable for both HTTP server deployments and embedded use cases."

### 1.2 Runtime Upstreams (Dynamic Configuration)

**Location**: `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/runtime_registry.rs`

**Purpose**: Track upstreams added via Admin API (vs config file), with JSON persistence.

**Files**:
- `upstream/runtime_registry.rs` - Registry for runtime-added upstreams
- `upstream/manager.rs` - Manager methods for runtime upstream CRUD
- `upstream/builder.rs` - Builder initialization of runtime registry

**Key Types**:
- `RuntimeUpstreamRegistry` - Registry for API-added upstreams
- `RuntimeUpstreamConfig` - Configuration for a runtime upstream
- `RuntimeUpstreamUpdate` - Update request structure
- `UpstreamSource::Runtime` - Enum variant indicating runtime origin

**Usage Example**:
```rust
let runtime_config = state
    .proxy_engine
    .get_upstream_manager()
    .add_runtime_upstream(core_request)?;
```

**Documentation Context**:
> "Runtime upstream registry for tracking runtime-added upstreams. This module provides a registry to distinguish between upstreams loaded from config and those added via the admin API at runtime."

---

## 2. Naming Conflict Analysis

### 2.1 Semantic Confusion

The term "runtime" in both contexts means different things:

| Context | "Runtime" Meaning | Lifecycle |
|---------|-------------------|-----------|
| **Prism Runtime** | Application/component runtime environment | Entire application lifecycle |
| **Runtime Upstreams** | Added at runtime (not from config) | Dynamic (can be added/removed) |

### 2.2 Developer Cognitive Load

When reading code, developers must context-switch between:
- "Runtime" as infrastructure (PrismRuntime)
- "Runtime" as dynamically-configured (runtime upstreams)

Example confusion points:
- `runtime_registry` (upstream registry) vs `runtime` (PrismRuntime instance)
- `runtime_upstreams` (dynamic upstreams) vs `runtime.upstream_manager()` (runtime component access)
- "Runtime upstream not found" (error message) vs "Runtime initialization failed" (error message)

### 2.3 API Surface Confusion

**Admin API Responses**:
```json
{
  "id": "upstream-123",
  "source": "runtime",  // Could be confused with PrismRuntime
  ...
}
```

**Error Messages**:
```
"Runtime upstream not found. Config-based upstreams cannot be modified via API."
```

Could be misread as: "Runtime upstream not found" (suggesting PrismRuntime issue).

---

## 3. Alternative Naming Options

### 3.1 Evaluated Alternatives

| Alternative | Pros | Cons | Score |
|-------------|------|------|-------|
| **`dynamic`** | ✅ Clear distinction from static config<br>✅ Common pattern in systems<br>✅ Short and memorable | ⚠️ Could imply frequently changing | **9/10** |
| **`api-managed`** | ✅ Explicit source (Admin API)<br>✅ Clear lifecycle ownership | ❌ Verbose<br>❌ Breaks naming convention | 6/10 |
| **`managed`** | ✅ Short<br>✅ Implies lifecycle management | ⚠️ Generic, lacks specificity<br>⚠️ Doesn't convey source | 5/10 |
| **`transient`** | ✅ Implies temporary nature | ❌ Misleading - they persist to disk<br>❌ Suggests volatility | 3/10 |
| **`ephemeral`** | ✅ Poetic | ❌ Same issues as "transient"<br>❌ False implication | 2/10 |
| **`live`** | ✅ Short | ⚠️ Vague meaning<br>❌ Confusing with "runtime" | 4/10 |
| **`user-defined`** | ✅ Clear ownership | ❌ Verbose<br>❌ Doesn't capture API source | 5/10 |
| **`admin-api`** | ✅ Explicit source | ❌ Verbose<br>❌ Implementation detail leakage | 5/10 |

### 3.2 Recommended Choice: **`dynamic`**

**Rationale**:
1. **Clear semantic distinction**: "dynamic" vs "static" (config-file)
2. **Industry standard**: Widely used in infrastructure projects (Kubernetes, Consul, etc.)
3. **Concise**: Short identifier, easy to type and read
4. **Accurate**: Captures the nature of runtime-configurable resources
5. **Future-proof**: Doesn't tie to specific implementation (Admin API, CLI, etc.)

**Precedents**:
- Kubernetes: `dynamic admission controllers` vs static config
- Consul: `dynamic upstream discovery` vs static definitions
- NGINX: `dynamic upstreams` (commercial feature)
- Envoy: `dynamic cluster configuration` vs static

---

## 4. Migration Impact Assessment

### 4.1 Files Requiring Changes

#### Core Library (`prism-core`)

**High Impact** (Type/struct renames):
1. `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/runtime_registry.rs` (472 lines)
   - `RuntimeUpstreamRegistry` → `DynamicUpstreamRegistry`
   - `RuntimeUpstreamConfig` → `DynamicUpstreamConfig`
   - `RuntimeUpstreamUpdate` → `DynamicUpstreamUpdate`
   - `UpstreamSource::Runtime` → `UpstreamSource::Dynamic`
   - Module documentation updates

2. `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/manager.rs` (600+ lines)
   - `runtime_registry` field → `dynamic_registry`
   - `add_runtime_upstream()` → `add_dynamic_upstream()`
   - `update_runtime_upstream()` → `update_dynamic_upstream()`
   - `remove_runtime_upstream()` → `remove_dynamic_upstream()`
   - `get_runtime_registry()` → `get_dynamic_registry()`

3. `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/builder.rs`
   - `runtime_storage_path` → `dynamic_storage_path`
   - `RuntimeUpstreamRegistry::new()` call

4. `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/mod.rs`
   - Public re-exports

**Medium Impact** (Method calls):
5. `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/router/smart.rs`
6. `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/hedging.rs`
7. `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/scoring.rs`

#### Server (`server`)

**High Impact** (Admin API handlers):
8. `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs` (1,171 lines)
   - `create_upstream()` function - "runtime upstream" in logs/comments
   - `update_upstream()` function - "runtime upstream" in logs/comments
   - `delete_upstream()` function - "runtime upstream" in logs/comments
   - Error messages: "Runtime upstream not found" → "Dynamic upstream not found"
   - Audit log messages

9. `/home/flo/workspace/personal/prism/crates/server/src/admin/types.rs`
   - API documentation comments (minimal impact)

**Low Impact** (Imports):
10. `/home/flo/workspace/personal/prism/crates/server/src/admin/mod.rs`

#### Tests

11. `/home/flo/workspace/personal/prism/crates/tests/src/runtime_tests.rs`
12. `/home/flo/workspace/personal/prism/crates/prism-core/src/upstream/runtime_registry.rs` (test module)
    - `test_add_runtime_upstream()` → `test_add_dynamic_upstream()`

#### Documentation

**Markdown Files**:
13. `/home/flo/workspace/personal/prism/ADMIN_API_COMPLETENESS_REVIEW.md`
14. `/home/flo/workspace/personal/prism/ADMIN_API_ANALYSIS_INDEX.md`
15. `/home/flo/workspace/personal/prism/ADMIN_API_IMPLEMENTATION_SUMMARY.md`
16. `/home/flo/workspace/personal/prism/CACHE_MEMORY_ESTIMATION_PATCH.md`
17. `/home/flo/workspace/personal/prism/RUST_IMPLEMENTATION_GUIDE.md`
18. `/home/flo/workspace/personal/prism/PERFORMANCE_SUMMARY.md`
19. `/home/flo/workspace/personal/prism/ADMIN_API_PERFORMANCE_ANALYSIS.md`
20. `/home/flo/workspace/personal/prism/ARCHITECTURE_REVIEW_ADMIN_API.md`
21. `/home/flo/workspace/personal/prism/docs/admin-api/` (if exists)
22. `/home/flo/workspace/personal/prism/docs/components/upstream/upstream_manager.md`
23. `/home/flo/workspace/personal/prism/docs/components/hedging/architecture.md`

### 4.2 API Response Fields

**NO BREAKING CHANGES** - API responses don't currently expose "runtime" in field names:

```rust
// Current UpstreamResponse (unchanged)
pub struct UpstreamResponse {
    pub id: String,
    pub name: String,
    pub url: String,
    // ... no "runtime" fields
}
```

**Internal field only**:
```rust
// Internal only (not in API responses)
pub enum UpstreamSource {
    Config,
    Runtime,  // ← Would become: Dynamic
}
```

### 4.3 Persistence/Storage

**File path consideration**:
- Current: JSON persistence to configurable path (e.g., `runtime_upstreams.json`)
- Recommendation: Rename to `dynamic_upstreams.json` (or leave as-is for backward compat)
- Impact: **Optional** - existing deployments would need migration script if renamed

### 4.4 Error Messages (User-Facing)

**High visibility** changes:
```rust
// Before
"Runtime upstream not found. Config-based upstreams cannot be modified via API."

// After
"Dynamic upstream not found. Config-based upstreams cannot be modified via API."
```

**Locations**:
- `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs:1066`
- `/home/flo/workspace/personal/prism/crates/server/src/admin/handlers/upstreams.rs:1146`

### 4.5 Log Messages

**Tracing/logging** updates:
```rust
// Before
tracing::info!("created runtime upstream via admin API");

// After
tracing::info!("created dynamic upstream via admin API");
```

**Structured log fields** (optional):
```rust
// Consider adding source field for clarity
tracing::info!(
    id = %response.id,
    name = %response.name,
    source = "dynamic",  // vs "config"
    "created dynamic upstream via admin API"
);
```

---

## 5. Migration Strategy

### 5.1 Phased Approach (Recommended)

**Phase 1: Core Library** (1-2 hours)
1. Rename types in `runtime_registry.rs`
2. Update method signatures in `manager.rs`
3. Fix imports in `builder.rs` and `mod.rs`
4. Run tests: `cargo test -p prism-core upstream`

**Phase 2: Server** (30-60 minutes)
1. Update handler functions in `upstreams.rs`
2. Update error messages
3. Update log messages
4. Run tests: `cargo test -p server admin`

**Phase 3: Documentation** (30 minutes)
1. Update markdown files
2. Update code comments
3. Update OpenAPI/utoipa docs (if any mention "runtime upstream")

**Phase 4: Validation** (30 minutes)
1. Full test suite: `cargo test --all`
2. Clippy check: `cargo clippy --all-targets`
3. Manual verification of error messages
4. Check logs in development environment

### 5.2 Git Strategy

**Commit structure**:
```
1. "refactor(upstream): rename RuntimeUpstream* to DynamicUpstream*"
   - Core type renames in prism-core

2. "refactor(admin): update runtime upstream terminology to dynamic"
   - Server-side handler updates

3. "docs: update runtime upstream references to dynamic upstreams"
   - Documentation updates

4. "chore: update error and log messages for dynamic upstreams"
   - User-facing message updates
```

**Branch**: Create from `feat/admin-api` (current branch)

### 5.3 Backward Compatibility

**Storage file migration** (optional):
```rust
// If renaming JSON file from runtime_upstreams.json -> dynamic_upstreams.json
pub fn migrate_storage_path(old_path: &Path, new_path: &Path) -> Result<(), std::io::Error> {
    if old_path.exists() && !new_path.exists() {
        std::fs::rename(old_path, new_path)?;
        tracing::info!(
            old = %old_path.display(),
            new = %new_path.display(),
            "migrated upstream storage file"
        );
    }
    Ok(())
}
```

**Recommendation**: Keep storage filename as-is to avoid migration complexity.

---

## 6. Detailed Rename Mappings

### 6.1 Type Renames

| Current | New | Usage Count (est.) |
|---------|-----|--------------------|
| `RuntimeUpstreamRegistry` | `DynamicUpstreamRegistry` | 15-20 |
| `RuntimeUpstreamConfig` | `DynamicUpstreamConfig` | 30-40 |
| `RuntimeUpstreamUpdate` | `DynamicUpstreamUpdate` | 5-10 |
| `UpstreamSource::Runtime` | `UpstreamSource::Dynamic` | 10-15 |

### 6.2 Method Renames

| Current | New | Visibility |
|---------|-----|------------|
| `add_runtime_upstream()` | `add_dynamic_upstream()` | Public API |
| `update_runtime_upstream()` | `update_dynamic_upstream()` | Public API |
| `remove_runtime_upstream()` | `remove_dynamic_upstream()` | Public API |
| `get_runtime_registry()` | `get_dynamic_registry()` | Public API |

### 6.3 Field Renames

| Current | New | Context |
|---------|-----|---------|
| `runtime_upstreams: RwLock<HashMap<...>>` | `dynamic_upstreams: RwLock<HashMap<...>>` | `DynamicUpstreamRegistry` |
| `runtime_registry: Arc<...>` | `dynamic_registry: Arc<...>` | `UpstreamManager` |
| `runtime_storage_path: Option<PathBuf>` | `dynamic_storage_path: Option<PathBuf>` | Builder |

### 6.4 Variable Renames

| Context | Current | New |
|---------|---------|-----|
| Function locals | `runtime_config` | `dynamic_config` |
| Function locals | `runtime_registry` | `dynamic_registry` |
| Closures | `upstreams.runtime_upstreams` | `upstreams.dynamic_upstreams` |

---

## 7. Risk Assessment

### 7.1 Breaking Changes

**Public API**: ✅ **NO BREAKING CHANGES**
- HTTP Admin API endpoints unchanged
- JSON response structures unchanged
- Only internal Rust API changes

**Internal API**: ⚠️ **BREAKING CHANGES** (if used externally)
- Public methods in `UpstreamManager` renamed
- Public types in `prism_core::upstream` module renamed
- Impact: Any external crates importing these types will need updates

**Mitigation**: Since `prism-core` is not published to crates.io (monorepo), impact limited to this codebase.

### 7.2 Testing Impact

**Required test updates**:
- Test function names (cosmetic)
- Test assertions (minimal)
- Mock/fixture data (if hardcoded "runtime" strings)

**Test coverage**: Existing tests remain valid; only identifiers change.

### 7.3 Deployment Impact

**Zero downtime**: ✅ Yes
- No database schema changes
- No API contract changes
- JSON storage format unchanged (only internal field names)

**Rollback complexity**: Low
- Git revert of renaming commits
- No data migration required (if storage filename unchanged)

---

## 8. Recommended Implementation

### 8.1 Step-by-Step Checklist

**Preparation**:
- [ ] Create feature branch from `feat/admin-api`
- [ ] Run full test suite to establish baseline
- [ ] Search codebase for all "runtime" references: `rg -i "runtime.*upstream|upstream.*runtime"`

**Phase 1: Core Types** (`prism-core`):
- [ ] Rename `RuntimeUpstreamRegistry` → `DynamicUpstreamRegistry`
- [ ] Rename `RuntimeUpstreamConfig` → `DynamicUpstreamConfig`
- [ ] Rename `RuntimeUpstreamUpdate` → `DynamicUpstreamUpdate`
- [ ] Update `UpstreamSource::Runtime` → `UpstreamSource::Dynamic`
- [ ] Update module docs in `runtime_registry.rs`
- [ ] Rename file: `runtime_registry.rs` → `dynamic_registry.rs` (optional)
- [ ] Run: `cargo test -p prism-core --lib upstream`

**Phase 2: Manager Methods** (`prism-core`):
- [ ] Update `UpstreamManager` field: `runtime_registry` → `dynamic_registry`
- [ ] Rename: `add_runtime_upstream()` → `add_dynamic_upstream()`
- [ ] Rename: `update_runtime_upstream()` → `update_dynamic_upstream()`
- [ ] Rename: `remove_runtime_upstream()` → `remove_dynamic_upstream()`
- [ ] Rename: `get_runtime_registry()` → `get_dynamic_registry()`
- [ ] Update method documentation
- [ ] Run: `cargo test -p prism-core --lib upstream::manager`

**Phase 3: Builder** (`prism-core`):
- [ ] Update builder field: `runtime_storage_path` → `dynamic_storage_path`
- [ ] Update `DynamicUpstreamRegistry::new()` call
- [ ] Run: `cargo test -p prism-core --lib upstream::builder`

**Phase 4: Public Exports** (`prism-core`):
- [ ] Update `upstream/mod.rs` re-exports
- [ ] Run: `cargo check -p prism-core`

**Phase 5: Server Handlers** (`server`):
- [ ] Update `upstreams.rs` function calls
- [ ] Update error messages (2 locations)
- [ ] Update log messages (tracing::info! calls)
- [ ] Update audit log messages
- [ ] Update handler documentation/comments
- [ ] Run: `cargo test -p server --lib admin::handlers::upstreams`

**Phase 6: Documentation**:
- [ ] Update markdown files (grep for "runtime upstream")
- [ ] Update code comments
- [ ] Update examples (if any)

**Phase 7: Validation**:
- [ ] Run: `cargo test --all`
- [ ] Run: `cargo clippy --all-targets -- -D warnings`
- [ ] Run: `cargo doc --no-deps` (check doc generation)
- [ ] Manual test: Create/Update/Delete dynamic upstream via Admin API
- [ ] Manual test: Verify error messages
- [ ] Manual test: Check JSON persistence file

**Phase 8: Cleanup**:
- [ ] Search for remaining "runtime upstream" references
- [ ] Update CHANGELOG (if maintained)
- [ ] Create pull request with clear description

### 8.2 Testing Commands

```bash
# Phase-by-phase testing
cargo test -p prism-core upstream::runtime_registry  # (becomes dynamic_registry)
cargo test -p prism-core upstream::manager
cargo test -p server admin::handlers::upstreams

# Full validation
cargo test --all
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check

# Documentation check
cargo doc --no-deps --open

# Search for remaining references
rg -i "runtime.*upstream|upstream.*runtime" --type rust
rg -i "runtimeupstream" --type rust
```

---

## 9. Communication Plan

### 9.1 Team Notification

**Before renaming**:
```
Subject: Upcoming Terminology Change: "Runtime Upstreams" → "Dynamic Upstreams"

Team,

We're renaming "runtime upstreams" to "dynamic upstreams" to avoid confusion
with the PrismRuntime module. This change:

- Affects internal Rust APIs only (no HTTP API changes)
- Requires code review updates if you have pending PRs
- Will be merged into feat/admin-api branch this week

Details: See RUNTIME_UPSTREAMS_NAMING_ANALYSIS.md
```

**After renaming**:
```
Subject: Completed: "Dynamic Upstreams" Terminology Migration

The "runtime upstreams" → "dynamic upstreams" migration is complete:

- Old: add_runtime_upstream() → New: add_dynamic_upstream()
- Old: RuntimeUpstreamConfig → New: DynamicUpstreamConfig
- Storage file: No changes needed

If you have merge conflicts, consult the migration guide.
```

### 9.2 Documentation Updates

**README** (if applicable):
```markdown
## Upstream Configuration

Prism supports two types of upstream configuration:

- **Static upstreams**: Defined in `config.toml`, loaded at startup
- **Dynamic upstreams**: Added via Admin API at runtime, persisted to JSON
```

**Admin API docs**:
```markdown
### Creating Dynamic Upstreams

Dynamic upstreams can be added via the Admin API without restarting Prism:

POST /admin/upstreams
{
  "name": "my-dynamic-upstream",
  "url": "https://eth.example.com",
  ...
}

Note: Dynamic upstreams persist across restarts via JSON storage.
```

---

## 10. Future Considerations

### 10.1 Terminology Consistency

**Establish naming conventions**:
- **Static** configuration: Loaded from TOML files
- **Dynamic** configuration: Added via API, persisted to JSON
- **Runtime** infrastructure: Application lifecycle (PrismRuntime)

**Apply consistently**:
- Future features should use "dynamic" for API-configurable resources
- Use "runtime" only for PrismRuntime module and lifecycle concepts

### 10.2 Potential Future Extensions

**Dynamic configuration scope**:
- Dynamic rate limits (per-API-key)
- Dynamic cache policies
- Dynamic routing rules

**Naming pattern**:
```rust
pub struct DynamicRateLimitRegistry { ... }
pub struct DynamicCachePolicyRegistry { ... }
pub struct DynamicRoutingRuleRegistry { ... }
```

### 10.3 Storage Strategy

**Future-proof JSON storage**:
```json
{
  "version": "1.0",
  "upstreams": [
    {
      "id": "...",
      "source": "dynamic",  // Explicit in storage
      ...
    }
  ]
}
```

---

## 11. Conclusion

### 11.1 Summary

The renaming from "runtime upstreams" to "dynamic upstreams" provides:

✅ **Clarity**: Eliminates confusion with PrismRuntime module
✅ **Consistency**: Aligns with industry standard terminology
✅ **Maintainability**: Clearer intent for future developers
✅ **Safety**: No breaking changes to public HTTP API

### 11.2 Effort Estimate

| Phase | Estimated Time | Risk |
|-------|---------------|------|
| Core library changes | 1-2 hours | Low |
| Server handler updates | 30-60 minutes | Low |
| Documentation updates | 30 minutes | Low |
| Testing & validation | 30 minutes | Low |
| **Total** | **2.5-4 hours** | **Low** |

### 11.3 Go/No-Go Recommendation

**RECOMMENDATION: ✅ PROCEED**

This renaming:
- Solves a real usability problem
- Has minimal risk (no API breakage)
- Requires reasonable effort (2-4 hours)
- Improves long-term codebase clarity

**Best time to execute**: During the `feat/admin-api` development phase, before merging to main.

---

## Appendix A: Complete File List

**Files requiring changes** (23 total):

### Rust Source Files (12)
1. `crates/prism-core/src/upstream/runtime_registry.rs`
2. `crates/prism-core/src/upstream/manager.rs`
3. `crates/prism-core/src/upstream/builder.rs`
4. `crates/prism-core/src/upstream/mod.rs`
5. `crates/prism-core/src/upstream/router/smart.rs`
6. `crates/prism-core/src/upstream/hedging.rs`
7. `crates/prism-core/src/upstream/scoring.rs`
8. `crates/server/src/admin/handlers/upstreams.rs`
9. `crates/server/src/admin/types.rs`
10. `crates/server/src/admin/mod.rs`
11. `crates/tests/src/runtime_tests.rs`
12. `crates/prism-core/src/upstream/consensus/tests/engine_tests.rs` (maybe)

### Documentation Files (11)
13. `ADMIN_API_COMPLETENESS_REVIEW.md`
14. `ADMIN_API_ANALYSIS_INDEX.md`
15. `ADMIN_API_IMPLEMENTATION_SUMMARY.md`
16. `CACHE_MEMORY_ESTIMATION_PATCH.md`
17. `RUST_IMPLEMENTATION_GUIDE.md`
18. `PERFORMANCE_SUMMARY.md`
19. `ADMIN_API_PERFORMANCE_ANALYSIS.md`
20. `ARCHITECTURE_REVIEW_ADMIN_API.md`
21. `docs/components/upstream/upstream_manager.md`
22. `docs/components/hedging/architecture.md`
23. `README.md` (possibly)

---

## Appendix B: Search Patterns

**Finding all references**:
```bash
# Primary search
rg -i "runtime.*upstream|upstream.*runtime" --type rust

# Type names
rg "RuntimeUpstream(Registry|Config|Update)" --type rust

# Method names
rg "(add|update|remove|get)_runtime_upstream" --type rust

# Field names
rg "runtime_(upstreams|registry|storage_path)" --type rust

# Enum variants
rg "UpstreamSource::Runtime" --type rust

# Comments and docs
rg "runtime upstream" --type rust
rg "runtime-added" --type rust

# Markdown documentation
rg -i "runtime upstream" --type md
```

---

**End of Analysis**
