# Prism Runtime Module

The `runtime` module provides a unified initialization and lifecycle management API for all Prism core components. It encapsulates the complexity of component initialization, background task coordination, and graceful shutdown into a clean, builder-pattern API suitable for both HTTP server deployments and embedded use cases.

**Source:** `crates/prism-core/src/runtime/`

## Quick Start

### Basic Server Usage

```rust
use prism_core::{config::AppConfig, runtime::PrismRuntime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = AppConfig::load()?;

    let runtime = PrismRuntime::builder()
        .with_config(config)
        .enable_health_checker()
        .enable_websocket_subscriptions()
        .build()
        .await?;

    // Access components for HTTP server
    let proxy = runtime.proxy_engine();

    // ... set up HTTP routes ...

    // Wait for shutdown signal and cleanup
    runtime.wait_for_shutdown().await;
    Ok(())
}
```

### Embedded Usage (Trading Bot)

```rust
use prism_core::{config::AppConfig, runtime::PrismRuntime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = AppConfig::load()?;

    // Minimal runtime without health checker or websockets
    let runtime = PrismRuntime::builder()
        .with_config(config)
        .build()
        .await?;

    // Direct access to components for custom logic
    let cache = runtime.cache_manager();
    let upstream = runtime.upstream_manager();

    // Custom trading logic using Prism infrastructure
    // ...

    // Manual shutdown when done
    runtime.shutdown().await;
    Ok(())
}
```

## Key Features

### Centralized Initialization
- Single builder API for all component setup
- Validation before initialization
- Error propagation with typed errors
- Automatic ordering of initialization steps

### Background Task Management
- Health checking for upstream providers
- WebSocket subscriptions for chain tip updates
- Cache cleanup and maintenance tasks
- Automatic shutdown coordination

### Graceful Shutdown
- Broadcast-based shutdown signaling
- Task handle management
- Idempotent shutdown (safe to call multiple times)
- Clean resource cleanup

### Flexible Configuration
- Enable/disable components based on use case
- Configurable shutdown channel capacity
- Optional health checking
- Optional WebSocket subscriptions
- Optional cache cleanup tasks

## Module Structure

```
runtime/
├── mod.rs           # Module documentation and public API
├── builder.rs       # PrismRuntimeBuilder for initialization
├── lifecycle.rs     # PrismRuntime for lifecycle management
└── components.rs    # PrismComponents container
```

## Documentation

### Comprehensive Guides
- [Architecture](./runtime_architecture.md) - Internal design and implementation details
- [API Reference](./runtime_api_reference.md) - Detailed API documentation

### Additional Resources
- [API Design](./runtime-api-design.md) - Design decisions and rationale

## Component Overview

### PrismRuntimeBuilder
Builder for configuring and initializing the runtime with a fluent API.

**Key Methods:**
- `new()` - Create a new builder
- `with_config()` - Set application configuration
- `enable_health_checker()` - Enable upstream health checks
- `enable_websocket_subscriptions()` - Enable WebSocket chain tip subscriptions
- `disable_cache_cleanup()` - Disable automatic cache cleanup
- `build()` - Build and initialize the runtime

**Location:** `crates/prism-core/src/runtime/builder.rs`

### PrismRuntime
Main runtime container managing component lifecycles and background tasks.

**Key Methods:**
- `components()` - Access all components
- `proxy_engine()` - Convenience accessor for proxy engine
- `cache_manager()` - Convenience accessor for cache manager
- `shutdown_receiver()` - Get shutdown signal receiver
- `shutdown()` - Initiate graceful shutdown
- `wait_for_shutdown()` - Wait indefinitely for shutdown signal

**Location:** `crates/prism-core/src/runtime/lifecycle.rs`

### PrismComponents
Thread-safe container for all initialized components.

**Key Methods:**
- `metrics_collector()` - Access metrics collector
- `cache_manager()` - Access cache manager
- `reorg_manager()` - Access reorg manager
- `upstream_manager()` - Access upstream manager
- `health_checker()` - Access health checker (optional)
- `proxy_engine()` - Access proxy engine

**Location:** `crates/prism-core/src/runtime/components.rs`

## Background Tasks

The runtime manages these background tasks automatically:

### Always Running
1. **Cache Inflight Cleanup** - Removes stale fetch operations
2. **Cache Stats Updater** - Periodic statistics recalculation
3. **Cache Pruning** - Periodic old data cleanup (configurable)

### Conditionally Running
4. **Health Checker** - Periodic upstream health checks (optional)
5. **WebSocket Subscriptions** - Chain tip monitoring per upstream (optional)

## Shutdown Behavior

When `shutdown()` is called:

1. Shutdown signal broadcast to all background tasks
2. Health checker task aborted (if running)
3. WebSocket subscription tasks cancelled (if running)
4. Cache background tasks receive shutdown signal
5. All task handles awaited for completion

The shutdown is idempotent - calling multiple times is safe.

## Usage Patterns

### HTTP Server
Full-featured runtime with all components enabled:

```rust
let runtime = PrismRuntime::builder()
    .with_config(config)
    .enable_health_checker()
    .enable_websocket_subscriptions()
    .build()
    .await?;

// Server runs...
runtime.wait_for_shutdown().await;
```

### Trading Bot
Minimal runtime for embedded use:

```rust
let runtime = PrismRuntime::builder()
    .with_config(config)
    .disable_health_checker()
    .disable_websocket_subscriptions()
    .build()
    .await?;

// Custom logic...
runtime.shutdown().await;
```

### Testing
Fast test setup without I/O:

```rust
let runtime = PrismRuntime::builder()
    .with_config(test_config)
    .disable_health_checker()
    .disable_websocket_subscriptions()
    .build()
    .await?;

// Test logic...
runtime.shutdown().await;
```

### Custom Background Tasks
Coordinate custom tasks with runtime shutdown:

```rust
let mut shutdown_rx = runtime.shutdown_receiver();

tokio::spawn(async move {
    loop {
        tokio::select! {
            _ = do_work() => {},
            _ = shutdown_rx.recv() => {
                println!("Shutting down custom task");
                break;
            }
        }
    }
});
```

## Error Handling

### RuntimeError Types

- `MetricsInitialization(String)` - Metrics collector initialization failed
- `ConfigValidation(String)` - Configuration validation failed
- `NoUpstreams` - No upstream providers configured
- `Initialization(String)` - Generic initialization error

All errors implement `std::error::Error` and provide detailed context.

## Thread Safety

All components are `Send + Sync` and safe to share across threads:

- Components wrapped in `Arc` for efficient sharing
- Interior mutability where needed (DashMap, AtomicU64, RwLock)
- Compile-time enforcement of Send + Sync bounds

## Performance Characteristics

### Initialization Cost
- **Time:** ~10-50ms (mostly upstream registration)
- **Allocations:** ~20 Arc allocations, minimal heap
- **I/O:** One connection per upstream for validation

### Runtime Cost
- **Memory:** ~1KB overhead (Arc pointers, atomics)
- **CPU:** Background tasks run every 60s-300s
- **No allocation:** In steady state (all paths pre-allocated)

### Shutdown Cost
- **Time:** <100ms for graceful shutdown
- **Guarantees:** All tasks receive signal, no data loss

## Testing

Run runtime tests:

```bash
# All runtime tests
cargo test --lib -p prism-core runtime::

# Specific module tests
cargo test --lib -p prism-core runtime::builder::
cargo test --lib -p prism-core runtime::lifecycle::
cargo test --lib -p prism-core runtime::components::
```

## See Also

- [Architecture Documentation](./runtime_architecture.md) - Internal design details
- [API Reference](./runtime_api_reference.md) - Complete API documentation
- [API Design Document](./runtime-api-design.md) - Design rationale
- [Main Architecture](../../architecture.md) - Overall system architecture
