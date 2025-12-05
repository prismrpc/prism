# PrismRuntime API Design

## Design Goals

1. **Encapsulation**: All initialization logic in one place
2. **Flexibility**: Optional components based on use case
3. **Safety**: Guaranteed cleanup via RAII and Drop
4. **Ergonomics**: Builder pattern with sensible defaults
5. **Testability**: Easy to configure for different test scenarios
6. **Reusability**: Same API for server and embedded use

## Architecture

### Three-Layer Design

```
┌─────────────────────────────────────┐
│   PrismRuntimeBuilder (Builder)    │  <- Initialization & Configuration
└─────────────────────────────────────┘
                 │
                 ▼ build()
┌─────────────────────────────────────┐
│   PrismRuntime (Lifecycle)          │  <- Lifecycle Management & Shutdown
│   ├── Components                    │
│   ├── Background Tasks              │
│   └── Shutdown Coordination         │
└─────────────────────────────────────┘
                 │
                 ▼ components()
┌─────────────────────────────────────┐
│   PrismComponents (Container)       │  <- Component Access
│   ├── MetricsCollector              │
│   ├── CacheManager                  │
│   ├── ReorgManager                  │
│   ├── UpstreamManager               │
│   ├── HealthChecker (optional)      │
│   └── ProxyEngine                   │
└─────────────────────────────────────┘
```

### Layer 1: PrismRuntimeBuilder

**Purpose**: Fluent API for configuring and initializing the runtime

**Responsibilities**:
- Accept configuration via builder methods
- Validate configuration before building
- Initialize all components in correct order
- Register background tasks
- Return fully-initialized runtime

**API Surface**:
```rust
impl PrismRuntimeBuilder {
    pub fn new() -> Self;
    pub fn with_config(self, config: AppConfig) -> Self;
    pub fn enable_health_checker(self) -> Self;
    pub fn disable_health_checker(self) -> Self;
    pub fn enable_websocket_subscriptions(self) -> Self;
    pub fn disable_websocket_subscriptions(self) -> Self;
    pub fn disable_cache_cleanup(self) -> Self;
    pub fn with_shutdown_channel_capacity(self, capacity: usize) -> Self;
    pub async fn build(self) -> Result<PrismRuntime, RuntimeError>;
}
```

**Design Decisions**:
- Builder pattern for flexibility and discoverability
- Async `build()` because initialization involves I/O (upstream registration)
- Returns `Result` to propagate initialization errors
- Method chaining for ergonomic configuration
- Sensible defaults (health checker disabled by default for minimal overhead)

### Layer 2: PrismRuntime

**Purpose**: Lifecycle management and shutdown coordination

**Responsibilities**:
- Own all component instances
- Manage background task handles
- Coordinate graceful shutdown
- Prevent double-shutdown
- Expose component access

**API Surface**:
```rust
impl PrismRuntime {
    pub fn components(&self) -> &PrismComponents;
    pub fn config(&self) -> &AppConfig;

    // Convenience accessors
    pub fn proxy_engine(&self) -> &Arc<ProxyEngine>;
    pub fn cache_manager(&self) -> &Arc<CacheManager>;
    pub fn upstream_manager(&self) -> &Arc<UpstreamManager>;
    pub fn metrics_collector(&self) -> &Arc<MetricsCollector>;

    // Shutdown management
    pub fn shutdown_receiver(&self) -> broadcast::Receiver<()>;
    pub async fn shutdown(self);
    pub async fn wait_for_shutdown(self);
}
```

**Design Decisions**:
- Not Clone (owns task handles, single owner for cleanup)
- Consumes self on `shutdown()` to prevent use-after-shutdown
- Atomic flag prevents double-shutdown
- Convenience methods reduce boilerplate
- `wait_for_shutdown()` for server use case

### Layer 3: PrismComponents

**Purpose**: Thread-safe component container

**Responsibilities**:
- Hold Arc references to all components
- Provide type-safe access
- Enable efficient sharing across threads

**API Surface**:
```rust
impl PrismComponents {
    pub fn metrics_collector(&self) -> &Arc<MetricsCollector>;
    pub fn cache_manager(&self) -> &Arc<CacheManager>;
    pub fn reorg_manager(&self) -> &Arc<ReorgManager>;
    pub fn upstream_manager(&self) -> &Arc<UpstreamManager>;
    pub fn health_checker(&self) -> Option<&Arc<HealthChecker>>;
    pub fn proxy_engine(&self) -> &Arc<ProxyEngine>;
    pub fn has_health_checker(&self) -> bool;
}
```

**Design Decisions**:
- Clone for sharing across threads
- Arc<T> for zero-cost component sharing
- Option<Arc<T>> for optional components
- Reference return types (not owned) for consistency
- All components accessible without nested access

## Background Tasks

The runtime manages these background tasks:

### Always Running
1. **Cache Inflight Cleanup**: Removes stale fetch operations
2. **Cache Stats Updater**: Periodic statistics recalculation
3. **Cache Pruning**: Periodic old data cleanup (if enabled)

### Conditionally Running
4. **Health Checker**: Periodic upstream health checks (if enabled)
5. **WebSocket Subscriptions**: Chain tip monitoring (if enabled)

### Shutdown Sequence

```
runtime.shutdown() called
        │
        ▼
1. Set shutdown_initiated flag (atomic)
        │
        ▼
2. Broadcast shutdown signal
        │
        ├──────────────────────────────┬──────────────────────┐
        ▼                              ▼                      ▼
   Cache tasks                   Health checker        WebSocket tasks
   receive signal                gets aborted          receive signal
        │                              │                      │
        ▼                              ▼                      ▼
   Clean exit                    Task dropped           Clean exit
        │                              │                      │
        └──────────────────────────────┴──────────────────────┘
                                       │
                                       ▼
                            All tasks completed
                                       │
                                       ▼
                             Runtime dropped
```

## Error Handling

### RuntimeError Types

```rust
pub enum RuntimeError {
    MetricsInitialization(String),  // Metrics setup failed
    ConfigValidation(String),        // Invalid configuration
    NoUpstreams,                     // No upstream providers
    Initialization(String),          // Generic init failure
}
```

**Design Decisions**:
- Specific error types for common failures
- Generic variant for unexpected issues
- String details for debugging
- thiserror for Display/Error traits

### Error Propagation

```rust
// Builder validates early
let runtime = PrismRuntimeBuilder::new()
    .with_config(invalid_config)
    .build()
    .await?;  // Error here, not later

// vs manual approach (errors can happen anywhere)
let chain_state = Arc::new(ChainState::new());
let metrics = MetricsCollector::new()?;  // Error 1
let cache = CacheManager::new(&config, chain_state.clone())?;  // Error 2
let upstream = UpstreamManagerBuilder::new()
    .chain_state(chain_state.clone())
    .build()?;                            // Error 3
upstream.add_upstream(config).await;      // Error 4 (silent failure)
```

## Memory Management

### Ownership Model

```
PrismRuntime (owned)
    └── owns shutdown_tx: broadcast::Sender<()>
    └── owns components: PrismComponents (Clone)
    └── owns health_task: Option<JoinHandle<()>>
    └── owns websocket_task: Option<JoinHandle<()>>

PrismComponents (Clone)
    └── Arc<MetricsCollector>      (shared)
    └── Arc<CacheManager>           (shared)
    └── Arc<ReorgManager>           (shared)
    └── Arc<UpstreamManager>        (shared)
    └── Option<Arc<HealthChecker>>  (shared)
    └── Arc<ProxyEngine>            (shared)
```

**Key Points**:
- Runtime is not Clone (owns shutdown coordination)
- Components is Clone (just Arc references)
- Background tasks reference components via Arc
- No cyclic references (shutdown_tx is owned, not shared)

### Drop Behavior

```rust
impl Drop for PrismRuntime {
    // No explicit Drop needed!
    // - JoinHandle drop aborts tasks
    // - Arc drop decrements refcount
    // - broadcast::Sender drop closes channel
}
```

**Design Decision**: Rely on Rust's automatic cleanup rather than explicit Drop. This is safer and simpler.

## Thread Safety

### Send + Sync Guarantees

```rust
// Compile-time enforcement
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    let _ = assert_send::<PrismRuntime>;
    let _ = assert_sync::<PrismRuntime>;
};
```

All components are Send + Sync:
- `MetricsCollector`: Uses internal synchronization
- `CacheManager`: DashMap, AtomicU64 for lock-free operations
- `ReorgManager`: RwLock, DashMap, Atomics
- `UpstreamManager`: DashMap for thread-safe storage
- `HealthChecker`: All Arc references
- `ProxyEngine`: All Arc references

## Configuration Patterns

### Development vs Production

```rust
// Development: Full logging and monitoring
let dev_runtime = PrismRuntimeBuilder::new()
    .with_config(config)
    .enable_health_checker()
    .enable_websocket_subscriptions()
    .build()
    .await?;

// Production: Same, with metrics
let prod_runtime = PrismRuntimeBuilder::new()
    .with_config(config)
    .enable_health_checker()
    .enable_websocket_subscriptions()
    .with_shutdown_channel_capacity(64)  // More tasks
    .build()
    .await?;
```

### Testing

```rust
// Fast test setup
let test_runtime = PrismRuntimeBuilder::new()
    .with_config(test_config)
    .disable_health_checker()       // No I/O overhead
    .disable_websocket_subscriptions()  // No network
    .build()
    .await?;
```

### Embedded (Trading Bot)

```rust
// Minimal overhead
let bot_runtime = PrismRuntimeBuilder::new()
    .with_config(config)
    .disable_health_checker()
    .disable_websocket_subscriptions()
    .disable_cache_cleanup()        // Manual control
    .build()
    .await?;
```

## Extension Points

### Custom Background Tasks

```rust
let mut shutdown_rx = runtime.shutdown_receiver();

tokio::spawn(async move {
    loop {
        tokio::select! {
            _ = custom_work() => {},
            _ = shutdown_rx.recv() => break,
        }
    }
});
```

### Dynamic Component Configuration

```rust
let runtime = PrismRuntimeBuilder::new()
    .with_config(config)
    .build()
    .await?;

// Add upstream dynamically
runtime.upstream_manager()
    .add_upstream(new_upstream_config)
    .await;
```

### Custom Metrics

```rust
let metrics = runtime.metrics_collector();

// Record custom application metrics
metrics.record_custom("trading_signal_detected", 1.0);
```

## Performance Characteristics

### Initialization Cost
- **Time**: ~10-50ms (mostly upstream registration)
- **Allocations**: ~20 Arc allocations, minimal heap
- **I/O**: One connection per upstream for validation

### Runtime Cost
- **Memory**: ~1KB overhead (Arc pointers, atomics)
- **CPU**: Background tasks run every 60s-300s
- **No allocation**: In steady state (all paths pre-allocated)

### Shutdown Cost
- **Time**: <100ms for graceful shutdown
- **Guarantees**: All tasks receive signal, no data loss

## Comparison to Alternatives

### vs Manual Initialization
| Aspect | Manual | PrismRuntime |
|--------|--------|--------------|
| Lines of code | ~100 | ~10 |
| Error handling | Scattered | Centralized |
| Shutdown safety | Manual | Automatic |
| Testability | Hard | Easy |
| Embedded use | Duplicate code | Same API |

### vs Dependency Injection
| Aspect | DI Framework | PrismRuntime |
|--------|--------------|--------------|
| Complexity | High | Low |
| Compile time | Slow | Fast |
| Type safety | Runtime | Compile-time |
| Explicit | No | Yes |

### vs Service Container
| Aspect | Container | PrismRuntime |
|--------|-----------|--------------|
| Lookup | String keys | Type-safe methods |
| Registration | Runtime | Compile-time |
| Lifecycle | Manual | Automatic |

## Future Extensions

Potential additions without breaking changes:

1. **Hot Reload**: `runtime.reload_config(new_config).await`
2. **Observability**: `runtime.trace_request(request_id)`
3. **Plugins**: `runtime.register_plugin(plugin)`
4. **Multi-Chain**: `runtime.add_chain(chain_config).await`
5. **Metrics Export**: `runtime.export_metrics_snapshot()`

All additive - no breaking changes to existing API.
