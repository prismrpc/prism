# Admin API Architecture Diagram

## High-Level Data Flow

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Admin API Request                           │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                        Axum HTTP Handler                            │
│  (handlers/metrics.rs, handlers/cache.rs, etc.)                     │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
                    ┌─────────────┴─────────────┐
                    │                           │
                    ▼                           ▼
    ┌───────────────────────────┐   ┌──────────────────────────┐
    │   PrometheusClient        │   │   ProxyEngine            │
    │   (Optional)              │   │   (In-Memory State)      │
    └───────────────────────────┘   └──────────────────────────┘
                    │                           │
                    │                           ▼
                    │               ┌──────────────────────────┐
                    │               │  MetricsCollector        │
                    │               │  (Live Metrics)          │
                    │               └──────────────────────────┘
                    │                           │
                    │                           ▼
                    │               ┌──────────────────────────┐
                    │               │  CacheManager            │
                    │               │  (Cache Stats)           │
                    │               └──────────────────────────┘
                    │                           │
                    │                           ▼
                    │               ┌──────────────────────────┐
                    │               │  LoadBalancer            │
                    │               │  (Upstreams List)        │
                    │               └──────────────────────────┘
                    │                           │
                    │                           ▼
                    │               ┌──────────────────────────┐
                    │               │  UpstreamEndpoint[]      │
                    │               │  (Health History)        │
                    │               └──────────────────────────┘
                    │
                    ▼
        ┌───────────────────────┐
        │  Prometheus Server    │
        │  (Historical Metrics) │
        └───────────────────────┘
                    │
                    ▼
        ┌───────────────────────┐
        │  PromQL Query Result  │
        └───────────────────────┘
                    │
                    └───────────────────────┐
                                            │
                                            ▼
                            ┌───────────────────────────────┐
                            │  Fallback Decision Logic      │
                            │  - Prometheus data available? │
                            │  - Yes: return Prometheus     │
                            │  - No: return in-memory       │
                            └───────────────────────────────┘
                                            │
                                            ▼
                            ┌───────────────────────────────┐
                            │  MetricsDataResponse<T>       │
                            │  - data: T                    │
                            │  - source: DataSource         │
                            │  - warning: Option<String>    │
                            └───────────────────────────────┘
                                            │
                                            ▼
                            ┌───────────────────────────────┐
                            │  JSON Response to Client      │
                            └───────────────────────────────┘
```

---

## Component Ownership & Thread Safety

```
┌─────────────────────────────────────────────────────────────────────┐
│                            AdminState                               │
│  ┌────────────────────────────────────────────────────────────┐    │
│  │  proxy_engine: Arc<ProxyEngine>        (Shared, Immutable) │    │
│  │  config: Arc<AppConfig>                (Shared, Immutable) │    │
│  │  prometheus_client: Option<PrometheusClient>  (Clone-able) │    │
│  └────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                ┌─────────────────┼─────────────────┐
                │                 │                 │
                ▼                 ▼                 ▼
    ┌───────────────────┐ ┌──────────────┐ ┌─────────────────┐
    │  ProxyEngine      │ │  AppConfig   │ │ PrometheusClient│
    │                   │ │              │ │                 │
    │  - metrics: Arc   │ │  - cache     │ │  - client: HTTP │
    │  - cache: Arc     │ │  - upstreams │ │  - cache: Arc   │
    │  - balancer: Arc  │ │  - server    │ │                 │
    └───────────────────┘ └──────────────┘ └─────────────────┘
                │
        ┌───────┼───────┐
        ▼       ▼       ▼
    ┌─────┐ ┌─────┐ ┌─────┐
    │Cache│ │Metr │ │Load │
    │Mgr  │ │Coll │ │Bal  │
    └─────┘ └─────┘ └─────┘
                        │
                        ▼
            ┌────────────────────────┐
            │  UpstreamEndpoint      │
            │  ┌──────────────────┐  │
            │  │ health:          │  │
            │  │   Arc<RwLock<T>> │  │
            │  ├──────────────────┤  │
            │  │ health_history:  │  │
            │  │   Arc<RwLock<    │  │
            │  │     VecDeque>>   │  │
            │  ├──────────────────┤  │
            │  │ cached_is_healthy│  │
            │  │   AtomicBool     │  │
            │  ├──────────────────┤  │
            │  │ health_cache_time│  │
            │  │   AtomicU64      │  │
            │  └──────────────────┘  │
            └────────────────────────┘
```

---

## Concurrency Patterns

### 1. Health Check Singleflight Pattern

```
Thread 1                Thread 2                Thread 3
   │                       │                       │
   ├─ health_check() ──────┼───────────────────────┤
   │                       │                       │
   ├─ CAS(false→true)      │                       │
   │   SUCCESS ✓           │                       │
   │                       ├─ CAS(false→true)      │
   │                       │   FAILURE ✗           │
   │                       │                       ├─ CAS(false→true)
   │                       │                       │   FAILURE ✗
   │                       │                       │
   ├─ do_health_check() ───┤                       │
   │   (actual work)       │                       │
   │                       ├─ wait on channel ─────┤
   │                       │                       ├─ wait on channel
   │                       │                       │
   ├─ broadcast result ────┼──────────────────────>│
   │                       │                       │
   ├─ return (is_healthy)  │                       │
   │                       ├─ return (same result) │
   │                       │                       ├─ return (same result)
   ▼                       ▼                       ▼
```

### 2. Lock-Free Health Cache Pattern

```
Request Path:
   │
   ├─ is_healthy()
   │     │
   │     ├─ now_ms ← SystemTime::now() (no lock)
   │     ├─ cache_time ← AtomicU64::load(Relaxed) (no lock)
   │     │
   │     ├─ if (now_ms - cache_time < 100ms)
   │     │     │
   │     │     └─ return AtomicBool::load(Relaxed) (no lock) ✓ FAST PATH
   │     │
   │     └─ else (cache expired)
   │           │
   │           ├─ health ← RwLock::read() (lock acquired)
   │           ├─ cb_allows ← CircuitBreaker::can_execute()
   │           ├─ is_healthy ← health.is_healthy && cb_allows
   │           │
   │           ├─ AtomicBool::store(is_healthy, Relaxed)
   │           ├─ AtomicU64::store(now_ms, Relaxed)
   │           │
   │           └─ return is_healthy (lock released)
   │
   ▼
```

### 3. ArcSwap Lock-Free Upstream List

```
LoadBalancer: ArcSwap<Vec<Arc<UpstreamEndpoint>>>

Read Path (hot):
   ├─ get_next_healthy()
   │     ├─ upstreams ← ArcSwap::load() (no lock!) ✓
   │     ├─ index ← AtomicUsize::fetch_add(1)
   │     └─ return upstreams[index]

Write Path (cold):
   ├─ add_upstream(config)
   │     └─ ArcSwap::rcu(|current| {
   │           let mut new = current.clone();
   │           new.push(upstream);
   │           new
   │       })
```

---

## Memory Layout

```
┌─────────────────────────────────────────────────────────────┐
│                    Process Memory                           │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  Stack (per async task)                             │   │
│  │  - Request handlers (small, mostly pointers)        │   │
│  │  - Local variables (minimal)                        │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  Heap (Arc-managed)                                 │   │
│  │  ┌─────────────────────────────────────────────┐    │   │
│  │  │  ProxyEngine (single instance)              │    │   │
│  │  │  - Reference count: N (one per handler)     │    │   │
│  │  └─────────────────────────────────────────────┘    │   │
│  │  ┌─────────────────────────────────────────────┐    │   │
│  │  │  AppConfig (single instance)                │    │   │
│  │  │  - Reference count: N (one per handler)     │    │   │
│  │  └─────────────────────────────────────────────┘    │   │
│  │  ┌─────────────────────────────────────────────┐    │   │
│  │  │  CacheManager                               │    │   │
│  │  │  - BlockCache:      ~500MB typical         │    │   │
│  │  │  - TransactionCache: ~100MB typical        │    │   │
│  │  │  - LogCache:        ~200MB typical         │    │   │
│  │  └─────────────────────────────────────────────┘    │   │
│  │  ┌─────────────────────────────────────────────┐    │   │
│  │  │  UpstreamEndpoint[] (one per upstream)      │    │   │
│  │  │  - Each: ~10KB metadata                     │    │   │
│  │  │  - Health history: 100 entries × 100 bytes  │    │   │
│  │  └─────────────────────────────────────────────┘    │   │
│  │  ┌─────────────────────────────────────────────┐    │   │
│  │  │  PrometheusClient Cache (LRU)               │    │   │
│  │  │  - Max 100 entries                          │    │   │
│  │  │  - ~10MB max                                │    │   │
│  │  └─────────────────────────────────────────────┘    │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│  Total typical memory: ~1GB (mostly caches)                │
└─────────────────────────────────────────────────────────────┘
```

---

## Error Handling Flow

```
                        Admin API Request
                               │
                               ▼
                    ┌──────────────────────┐
                    │  Handler Function    │
                    └──────────────────────┘
                               │
                ┌──────────────┼──────────────┐
                │              │              │
                ▼              ▼              ▼
        ┌─────────────┐ ┌────────────┐ ┌──────────────┐
        │ Prometheus  │ │ ProxyEngine│ │  Validation  │
        │   Query     │ │   Access   │ │    Logic     │
        └─────────────┘ └────────────┘ └──────────────┘
                │              │              │
            Success?       Success?       Valid?
                │              │              │
        ┌───────┴───────┐      │      ┌───────┴───────┐
        │               │      │      │               │
        ▼               ▼      ▼      ▼               ▼
    ┌────────┐    ┌─────────────────────┐    ┌────────────┐
    │ Return │    │   Log Error         │    │   Return   │
    │ Prom   │    │   tracing::warn!()  │    │   400 Bad  │
    │ Data   │    └─────────────────────┘    │   Request  │
    └────────┘              │                 └────────────┘
                            ▼
                    ┌──────────────┐
                    │  Fallback to │
                    │  In-Memory   │
                    └──────────────┘
                            │
                            ▼
                    ┌──────────────────────┐
                    │ MetricsDataResponse  │
                    │ {                    │
                    │   data: T,           │
                    │   source: InMemory,  │
                    │   warning: Some(...)  │
                    │ }                    │
                    └──────────────────────┘
                            │
                            ▼
                    ┌──────────────┐
                    │ 200 OK with  │
                    │ degraded     │
                    │ indicator    │
                    └──────────────┘
```

---

## Key Design Decisions

### 1. Arc vs Rc

**Decision**: Use `Arc` everywhere
- ✓ Thread-safe
- ✓ Works across async boundaries
- ✓ Compatible with Tokio runtime
- ✗ Atomic ref counting overhead (minimal)

### 2. RwLock vs Mutex

**Decision**: RwLock for read-heavy workloads

| Component | Lock Type | Reason |
|-----------|-----------|--------|
| UpstreamHealth | RwLock | Many reads (every request), few writes (health checks) |
| HealthHistory | RwLock | Many reads (admin API), few writes (health checks) |
| PrometheusCache | Mutex | Balanced read/write (cache hits/misses) |

### 3. Lock-Free Optimizations

**Decision**: Use atomics for frequently accessed values

| Field | Type | Benefit |
|-------|------|---------|
| `cached_is_healthy` | AtomicBool | Zero-lock health checks |
| `health_cache_time` | AtomicU64 | Zero-lock cache validation |
| `current_index` | AtomicUsize | Zero-lock round-robin |

### 4. Singleflight Pattern

**Decision**: Deduplicate concurrent health checks

**Problem**: Without singleflight:
```
100 concurrent requests → 100 health checks → upstream overload
```

**Solution**: With singleflight:
```
100 concurrent requests → 1 health check → 99 wait → all receive result
```

---

## Future Optimizations

### 1. Lazy Health Checks

Current: Health checks on every `is_healthy()` call after cache expiry

Possible: Background health check thread with async notifications

```rust
// Potential improvement
struct HealthChecker {
    rx: tokio::sync::watch::Receiver<HealthStatus>,
}

// Background task updates health status every 60s
// Handlers just read the latest value (always fresh, no lock)
```

### 2. MOKA Cache for Prometheus

Current: Simple LRU with parking_lot::Mutex

Possible: [MOKA](https://github.com/moka-rs/moka) for advanced features
- TTL eviction
- Entry-level TTL
- Async cache loading

### 3. Memory Pool for Health History

Current: VecDeque allocation per entry

Possible: Pre-allocated ring buffer with object pooling

**Trade-off**: Complexity vs memory savings (likely not worth it)

---

*Architecture documented by Claude Code (Sonnet 4.5) on 2025-12-07*
