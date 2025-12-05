# Prism Runtime API Reference

Complete API documentation for the Prism runtime module.

## Module: `prism_core::runtime`

**Location:** `crates/prism-core/src/runtime/`

### Public Exports

```rust
pub use builder::PrismRuntimeBuilder;
pub use components::PrismComponents;
pub use lifecycle::PrismRuntime;
```

## PrismRuntimeBuilder

**File:** `crates/prism-core/src/runtime/builder.rs`

Builder for constructing a `PrismRuntime` with configurable components.

### Constructor

#### `new() -> Self`

Creates a new runtime builder with default options.

**Returns:** New `PrismRuntimeBuilder` instance

**Example:**
```rust
let builder = PrismRuntimeBuilder::new();
```

**Source:** Lines 83-88 in `builder.rs`

---

### Configuration Methods

#### `with_config(self, config: AppConfig) -> Self`

Sets the application configuration. This is required before calling `build()`.

**Parameters:**
- `config: AppConfig` - Application configuration

**Returns:** Self for method chaining

**Example:**
```rust
let builder = PrismRuntimeBuilder::new()
    .with_config(config);
```

**Source:** Lines 94-97 in `builder.rs`

---

### Health Checker Methods

#### `enable_health_checker(self) -> Self`

Enables health checking for upstream providers. When enabled, a background task periodically checks upstream health using `eth_blockNumber` calls and records metrics.

**Returns:** Self for method chaining

**Example:**
```rust
let builder = PrismRuntimeBuilder::new()
    .with_config(config)
    .enable_health_checker();
```

**Source:** Lines 104-107 in `builder.rs`

#### `disable_health_checker(self) -> Self`

Disables health checking for upstream providers. Useful for embedded use cases where health checking overhead is not needed.

**Returns:** Self for method chaining

**Example:**
```rust
let builder = PrismRuntimeBuilder::new()
    .with_config(config)
    .disable_health_checker();
```

**Source:** Lines 113-116 in `builder.rs`

---

### WebSocket Methods

#### `enable_websocket_subscriptions(self) -> Self`

Enables WebSocket subscriptions to upstream providers. When enabled, subscribes to `newHeads` events for chain tip updates and automatic cache invalidation on reorgs.

**Returns:** Self for method chaining

**Example:**
```rust
let builder = PrismRuntimeBuilder::new()
    .with_config(config)
    .enable_websocket_subscriptions();
```

**Source:** Lines 123-126 in `builder.rs`

#### `disable_websocket_subscriptions(self) -> Self`

Disables WebSocket subscriptions. Useful for embedded use cases or when WebSocket support is not available.

**Returns:** Self for method chaining

**Example:**
```rust
let builder = PrismRuntimeBuilder::new()
    .with_config(config)
    .disable_websocket_subscriptions();
```

**Source:** Lines 132-135 in `builder.rs`

---

### Advanced Configuration Methods

#### `with_shutdown_channel_capacity(self, capacity: usize) -> Self`

Sets a custom shutdown channel capacity. Default is 16. Increase if you have many background tasks that need shutdown coordination.

**Parameters:**
- `capacity: usize` - Number of shutdown receivers supported

**Returns:** Self for method chaining

**Example:**
```rust
let builder = PrismRuntimeBuilder::new()
    .with_config(config)
    .with_shutdown_channel_capacity(64);
```

**Source:** Lines 142-145 in `builder.rs`

#### `disable_cache_cleanup(self) -> Self`

Disables automatic background cache cleanup tasks. By default, cache cleanup runs periodically. Disable this if you want to manually control cache cleanup.

**Returns:** Self for method chaining

**Example:**
```rust
let builder = PrismRuntimeBuilder::new()
    .with_config(config)
    .disable_cache_cleanup();
```

**Source:** Lines 152-155 in `builder.rs`

---

### Build Method

#### `async fn build(self) -> Result<PrismRuntime, RuntimeError>`

Builds the runtime, initializing all components and starting background tasks.

**Returns:** `Result<PrismRuntime, RuntimeError>`

**Errors:**

Returns `RuntimeError` if:
- No configuration was provided (`ConfigValidation`)
- Configuration validation fails (`ConfigValidation`)
- No upstream providers are configured (`NoUpstreams`)
- Component initialization fails (`MetricsInitialization`, `Initialization`)

**Example:**
```rust
let runtime = PrismRuntimeBuilder::new()
    .with_config(config)
    .build()
    .await?;
```

**Source:** Lines 163-252 in `builder.rs`

---

## PrismRuntime

**File:** `crates/prism-core/src/runtime/lifecycle.rs`

Main runtime container managing component lifecycles and background tasks.

### Component Access Methods

#### `components(&self) -> &PrismComponents`

Returns a reference to all runtime components.

**Returns:** Reference to `PrismComponents`

**Example:**
```rust
let components = runtime.components();
let cache = components.cache_manager();
```

**Source:** Lines 82-84 in `lifecycle.rs`

#### `config(&self) -> &AppConfig`

Returns a reference to the application configuration.

**Returns:** Reference to `AppConfig`

**Example:**
```rust
let config = runtime.config();
println!("Port: {}", config.server.bind_port);
```

**Source:** Lines 87-90 in `lifecycle.rs`

---

### Convenience Accessors

#### `proxy_engine(&self) -> &Arc<ProxyEngine>`

Convenience method to get the proxy engine directly. Equivalent to `runtime.components().proxy_engine()`.

**Returns:** Reference to `Arc<ProxyEngine>`

**Example:**
```rust
let proxy = runtime.proxy_engine();
let response = proxy.process_request(request).await;
```

**Source:** Lines 93-96 in `lifecycle.rs`

#### `cache_manager(&self) -> &Arc<CacheManager>`

Convenience method to get the cache manager directly. Equivalent to `runtime.components().cache_manager()`.

**Returns:** Reference to `Arc<CacheManager>`

**Example:**
```rust
let cache = runtime.cache_manager();
let stats = cache.get_stats().await;
```

**Source:** Lines 99-102 in `lifecycle.rs`

#### `upstream_manager(&self) -> &Arc<UpstreamManager>`

Convenience method to get the upstream manager directly. Equivalent to `runtime.components().upstream_manager()`.

**Returns:** Reference to `Arc<UpstreamManager>`

**Example:**
```rust
let upstream = runtime.upstream_manager();
let upstreams = upstream.get_all_upstreams().await;
```

**Source:** Lines 105-108 in `lifecycle.rs`

#### `metrics_collector(&self) -> &Arc<MetricsCollector>`

Convenience method to get the metrics collector directly. Equivalent to `runtime.components().metrics_collector()`.

**Returns:** Reference to `Arc<MetricsCollector>`

**Example:**
```rust
let metrics = runtime.metrics_collector();
metrics.record_request_received("eth_getLogs");
```

**Source:** Lines 111-114 in `lifecycle.rs`

---

### Shutdown Methods

#### `shutdown_receiver(&self) -> broadcast::Receiver<()>`

Creates a new shutdown receiver for external shutdown coordination. Useful when you need to listen for shutdown signals in custom tasks.

**Returns:** `broadcast::Receiver<()>` for receiving shutdown signals

**Example:**
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

**Source:** Lines 120-122 in `lifecycle.rs`

#### `async fn shutdown(self)`

Initiates graceful shutdown of all background tasks. This method is idempotent - calling it multiple times is safe.

**Shutdown Sequence:**
1. Broadcasts shutdown signal to all tasks
2. Aborts health checker task (if running)
3. Waits for WebSocket subscription task to complete (if running)
4. Waits for health checker task cleanup

**Example:**
```rust
// Graceful shutdown
runtime.shutdown().await;
```

**Source:** Lines 129-157 in `lifecycle.rs`

#### `async fn wait_for_shutdown(self)`

Waits indefinitely for a shutdown signal without consuming the runtime. This is useful for server implementations that need to keep the runtime alive while waiting for external shutdown signals (SIGTERM, Ctrl+C, etc.).

**Example:**
```rust
// Server is running...
runtime.wait_for_shutdown().await;
// Shutdown signal received, cleanup happens automatically
```

**Source:** Lines 163-168 in `lifecycle.rs`

---

## PrismComponents

**File:** `crates/prism-core/src/runtime/components.rs`

Container for all initialized Prism core components. All components are wrapped in `Arc` for efficient sharing across threads and tasks.

### Constructor

#### `new(...) -> Self`

Creates a new components container. This is typically called by `PrismRuntimeBuilder` during initialization.

**Parameters:**
- `metrics_collector: Arc<MetricsCollector>`
- `cache_manager: Arc<CacheManager>`
- `reorg_manager: Arc<ReorgManager>`
- `upstream_manager: Arc<UpstreamManager>`
- `health_checker: Option<Arc<HealthChecker>>`
- `proxy_engine: Arc<ProxyEngine>`

**Returns:** New `PrismComponents` instance

**Source:** Lines 30-45 in `components.rs`

---

### Component Accessor Methods

#### `metrics_collector(&self) -> &Arc<MetricsCollector>`

Returns a reference to the metrics collector.

**Returns:** Reference to `Arc<MetricsCollector>`

**Example:**
```rust
let metrics = components.metrics_collector();
metrics.record_request_received("eth_getLogs");
```

**Source:** Lines 49-52 in `components.rs`

#### `cache_manager(&self) -> &Arc<CacheManager>`

Returns a reference to the cache manager.

**Returns:** Reference to `Arc<CacheManager>`

**Example:**
```rust
let cache = components.cache_manager();
let stats = cache.get_stats().await;
println!("Log entries: {}", stats.log_store_size);
```

**Source:** Lines 55-58 in `components.rs`

#### `reorg_manager(&self) -> &Arc<ReorgManager>`

Returns a reference to the reorg manager.

**Returns:** Reference to `Arc<ReorgManager>`

**Example:**
```rust
let reorg = components.reorg_manager();
let stats = reorg.get_reorg_stats();
println!("Safe head: {}", stats.safe_head);
```

**Source:** Lines 61-64 in `components.rs`

#### `upstream_manager(&self) -> &Arc<UpstreamManager>`

Returns a reference to the upstream manager.

**Returns:** Reference to `Arc<UpstreamManager>`

**Example:**
```rust
let upstream = components.upstream_manager();
let upstreams = upstream.get_all_upstreams().await;
println!("Configured upstreams: {}", upstreams.len());
```

**Source:** Lines 67-70 in `components.rs`

#### `health_checker(&self) -> Option<&Arc<HealthChecker>>`

Returns a reference to the health checker, if enabled. Returns `None` if health checking was disabled during runtime initialization.

**Returns:** `Option<&Arc<HealthChecker>>`

**Example:**
```rust
if let Some(health) = components.health_checker() {
    let result = health.check_upstream("infura").await;
    println!("Health check result: {:?}", result);
}
```

**Source:** Lines 76-78 in `components.rs`

#### `proxy_engine(&self) -> &Arc<ProxyEngine>`

Returns a reference to the proxy engine. This is the main entry point for processing RPC requests.

**Returns:** Reference to `Arc<ProxyEngine>`

**Example:**
```rust
let proxy = components.proxy_engine();
let response = proxy.process_request(request).await;
```

**Source:** Lines 81-84 in `components.rs`

#### `has_health_checker(&self) -> bool`

Returns whether health checking is enabled.

**Returns:** `true` if health checker is present, `false` otherwise

**Example:**
```rust
if components.has_health_checker() {
    println!("Health checking enabled");
}
```

**Source:** Lines 87-90 in `components.rs`

---

## RuntimeError

**File:** `crates/prism-core/src/runtime/builder.rs`

Errors that can occur during runtime initialization.

### Variants

#### `MetricsInitialization(String)`

Metrics collector initialization failed. Contains error details.

**Example:**
```rust
Err(RuntimeError::MetricsInitialization("Failed to create registry".to_string()))
```

**Source:** Lines 21-22 in `builder.rs`

#### `ConfigValidation(String)`

Configuration validation failed. Contains validation error details.

**Example:**
```rust
Err(RuntimeError::ConfigValidation("No configuration provided".to_string()))
```

**Source:** Lines 25-26 in `builder.rs`

#### `NoUpstreams`

No upstream providers configured. At least one upstream is required.

**Example:**
```rust
Err(RuntimeError::NoUpstreams)
```

**Source:** Lines 29-30 in `builder.rs`

#### `Initialization(String)`

Generic initialization error. Contains error details.

**Example:**
```rust
Err(RuntimeError::Initialization("Component setup failed".to_string()))
```

**Source:** Lines 33-34 in `builder.rs`

---

## Usage Patterns

### HTTP Server Pattern

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

    let proxy = runtime.proxy_engine();
    // ... set up HTTP server with proxy ...

    runtime.wait_for_shutdown().await;
    Ok(())
}
```

### Embedded Application Pattern

```rust
use prism_core::{config::AppConfig, runtime::PrismRuntime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = AppConfig::load()?;

    let runtime = PrismRuntime::builder()
        .with_config(config)
        .disable_health_checker()
        .disable_websocket_subscriptions()
        .build()
        .await?;

    let proxy = runtime.proxy_engine();
    // ... custom application logic ...

    runtime.shutdown().await;
    Ok(())
}
```

### Testing Pattern

```rust
use prism_core::runtime::PrismRuntimeBuilder;

#[tokio::test]
async fn test_with_runtime() {
    let config = create_test_config();

    let runtime = PrismRuntimeBuilder::new()
        .with_config(config)
        .disable_health_checker()
        .disable_websocket_subscriptions()
        .build()
        .await
        .expect("Failed to build runtime");

    // Test logic...

    runtime.shutdown().await;
}
```

### Custom Background Task Pattern

```rust
let mut shutdown_rx = runtime.shutdown_receiver();

tokio::spawn(async move {
    loop {
        tokio::select! {
            result = custom_work() => {
                // Handle result
            }
            _ = shutdown_rx.recv() => {
                println!("Shutting down custom task");
                break;
            }
        }
    }
});
```

## Thread Safety

All types are Send + Sync:
- `PrismRuntimeBuilder` - Send + Sync (for passing between threads during setup)
- `PrismRuntime` - Send + Sync (can be used from multiple threads)
- `PrismComponents` - Clone + Send + Sync (safe to share via Arc)

## Performance Notes

### Initialization Cost
- Time: ~10-50ms depending on upstream count
- Allocations: ~20 Arc allocations
- I/O: One connection per upstream for validation

### Runtime Overhead
- Memory: ~1KB for runtime structures
- CPU: Background tasks run every 30-300s
- No allocation in steady state

### Shutdown Cost
- Time: <100ms for graceful shutdown
- All tasks notified simultaneously
- Clean resource cleanup guaranteed

## See Also

- [Runtime Overview](./runtime_overview.md) - Quick start and overview
- [Architecture Documentation](./runtime_architecture.md) - Internal design details
- [API Design](./runtime-api-design.md) - Design decisions and rationale
- [Main Architecture](../../architecture.md) - Overall system architecture
