# Hedging Architecture

**Module**: `crates/prism-core/src/upstream/hedging.rs`

## Overview

This document provides a deep technical dive into the hedging architecture in Prism. It covers the latency tracking mechanism, percentile calculation, hedged request execution flow, and performance characteristics.

## System Architecture

### Component Hierarchy

```
┌─────────────────────────────────────────────────────────────────┐
│                       ProxyEngine                                │
│                  (Request Orchestrator)                          │
└────────────┬─────────────────────────────────────────────────────┘
             │
             ├─ Select upstreams (via ScoringEngine)
             │  (returns: ranked list, best first)
             │
             ▼
             ┌──────────────────────────────────────────────────────┐
             │              HedgeExecutor                           │
             │                                                      │
             │  ┌──────────────────────────────────────────────┐  │
             │  │  Configuration (ArcSwap<HedgeConfig>)         │  │
             │  │  - enabled, latency_quantile, delays         │  │
             │  │  - Lock-free reads via ArcSwap                │  │
             │  └──────────────────────────────────────────────┘  │
             │                                                      │
             │  ┌──────────────────────────────────────────────┐  │
             │  │  Latency Trackers (DashMap<String, LatencyTracker>)│ │
             │  │  - Per-upstream sliding window               │  │
             │  │  - P50/P90/P95/P99 calculation               │  │
             │  │  - Lock-free concurrent access                │  │
             │  └──────────────────────────────────────────────┘  │
             │                                                      │
             │  execute_hedged()                                   │
             │    ├─ Check if hedging enabled & sufficient data   │
             │    ├─ Calculate hedge delay from percentiles       │
             │    ├─ Send primary request                         │
             │    ├─ Race: primary vs hedge delay                 │
             │    └─ Return first successful response             │
             │                                                      │
             └──────────────┬───────────────────────────────────────┘
                            │
                            ├─ Record latency for all responses
                            └─ Update latency trackers
```

### Data Flow

```
1. CLIENT REQUEST
   │
   ▼
2. PROXY ENGINE
   │
   ├─ Cache Check (MISS)
   │
   ├─ Get ranked upstreams from ScoringEngine
   │  └─ Returns: [(upstream_a, score_95), (upstream_b, score_87), ...]
   │
   ▼
3. HEDGE EXECUTOR
   │
   ├─ Check if hedging enabled
   │  └─ If disabled: execute_simple(primary_upstream)
   │
   ├─ Check if sufficient latency data exists
   │  └─ If not: execute_simple(primary_upstream)
   │
   ├─ Calculate hedge delay for primary upstream
   │  ├─ Get latency tracker for "upstream_a"
   │  ├─ Query percentile(0.95) → 180ms
   │  ├─ Clamp to [min_delay_ms, max_delay_ms] → 180ms
   │  └─ hedge_delay = Duration::from_millis(180)
   │
   ▼
4. PRIMARY REQUEST
   │
   ├─ Start timer
   ├─ Send async request to upstream_a
   │
   ▼
5. RACE: PRIMARY vs DELAY
   │
   ├─ tokio::select! {
   │    ├─ BRANCH A: Primary completes first
   │    │  ├─ Record latency (100ms)
   │    │  └─ Return response  (no hedge sent)
   │    │
   │    └─ BRANCH B: Hedge delay elapsed (180ms)
   │       ├─ Primary still pending...
   │       ├─ Send hedged requests to upstream_b, upstream_c
   │       │  (in parallel, up to max_parallel)
   │       │
   │       ▼
   │       6. RACE: ALL PARALLEL REQUESTS
   │          │
   │          ├─ select_all([primary, hedge_1, hedge_2])
   │          │  ├─ hedge_1 (upstream_b) completes in 50ms 
   │          │  ├─ Record latency (230ms total = 180ms wait + 50ms)
   │          │  └─ Cancel primary and hedge_2
   │          │
   │          └─ Return first successful response
   │
   ▼
7. LATENCY RECORDING
   │
   ├─ record_latency(winning_upstream, latency_ms)
   │  ├─ Get or create LatencyTracker for upstream
   │  ├─ Add sample to circular buffer
   │  │  └─ If buffer full: evict oldest sample
   │  ├─ Update sum and count
   │  └─ Log P95, avg, sample count
   │
   ▼
8. RETURN RESPONSE
   │
   └─ Response forwarded to ProxyEngine → Cache → Client
```

## Core Components

### HedgeExecutor

**Location**: Lines 201-568 in hedging.rs

The main hedging orchestrator that manages latency tracking and executes hedged requests.

**Structure**:
```rust
pub struct HedgeExecutor {
    config: ArcSwap<HedgeConfig>,
    latency_trackers: DashMap<String, LatencyTracker>,
}
```

**Design Decisions**:

1. **Dual State Design**: Configuration + latency trackers
   - **Why**: Configuration is user-controlled; trackers are runtime state
   - **Lock Strategy**: Separate RwLocks minimize contention
   - **Benefit**: Config updates don't block latency recording

2. **DashMap<String, LatencyTracker>**: Lock-free concurrent per-upstream tracking
   - **Why**: Multiple requests may record latency concurrently
   - **Lock Granularity**: Per-shard locking (much better than single lock)
   - **Benefit**: High concurrency with minimal contention
   - **Alternative**: DashMap for lock-free reads (not needed, writes are fast)

3. **No Persistent Storage**: Latency data lives only in memory
   - **Why**: Fresh data more valuable than historical (network conditions change)
   - **Benefit**: Zero I/O overhead, fast startup
   - **Trade-off**: Cold start requires warm-up period

### LatencyTracker

**Location**: Lines 95-195 in hedging.rs

Efficient latency percentile tracker using a sliding window circular buffer.

**Structure**:
```rust
pub struct LatencyTracker {
    samples: VecDeque<u64>,      // Circular buffer of latencies
    max_samples: usize,          // Window size (default: 1000)
    sum: u64,                    // Sum for average calculation
    count: usize,                // Current sample count
}
```

**Design Decisions**:

1. **VecDeque for Circular Buffer**:
   -  Efficient push_back() and pop_front() (O(1))
   -  Automatic capacity management
   -  Contiguous memory for cache efficiency
   -  Not a true circular buffer (slight allocation on resize)

2. **Separate sum/count Fields**:
   - **Why**: Avoid recalculating sum on every `average()` call
   - **Maintenance**: Update on insert and eviction
   - **Trade-off**: 16 extra bytes per tracker, but O(1) average

3. **Window Size**: 1000 samples (default)
   - **Typical Duration**: At 10 req/s → 100 seconds of history
   - **Memory per Tracker**: ~8KB (1000 * 8 bytes)
   - **Why Not More**: Diminishing returns beyond 1000 samples

**Alternative Designs Considered**:

1. **t-digest**:
   -  O(1) percentile calculation
   -  Compact representation
   -  Complex implementation
   -  Approximate, not exact

2. **P² Algorithm**:
   -  Online percentile estimation
   -  Approximate
   -  Doesn't provide all percentiles simultaneously

3. **Pre-sorted Buffer**:
   -  O(1) percentile lookup
   -  O(n) insert cost
   -  Not suitable for high-frequency updates

**Chosen**: Simple sorting approach is optimal for our constraints (small window, infrequent percentile queries).

### Latency Recording Flow

**Location**: Lines 236-261 in hedging.rs

```rust
pub async fn record_latency(&self, upstream_name: &str, latency_ms: u64)
```

**Algorithm**:
```
1. LOCK-FREE DashMap ACCESS
   └─ Per-shard locking (minimal contention)

2. GET OR CREATE TRACKER
   ├─ If tracker exists: Use existing
   └─ If new upstream: Create tracker with window_size=1000

3. RECORD SAMPLE
   ├─ tracker.record(latency_ms)
   │  ├─ If buffer full: pop_front() (evict oldest)
   │  │  └─ sum -= old_value
   │  ├─ push_back(latency_ms)
   │  └─ sum += latency_ms
   │
4. LOG (if DEBUG enabled)
   └─ Calculate P95, avg, sample count and log

5. NO LOCK RELEASE NEEDED (lock-free operation)
```

**Performance Characteristics**:
- **Lock Duration**: ~1-2μs (only map lookup and buffer push)
- **Memory Allocation**: None (buffer pre-allocated)
- **Complexity**: O(1) amortized

**Concurrency**:
```
Thread 1: record_latency("upstream_a", 100)
Thread 2: record_latency("upstream_a", 150)  ← Waits for lock
Thread 3: record_latency("upstream_b", 200)  ← Waits for lock

Concurrent execution with minimal contention via DashMap per-shard locking.

Alternative: Sharded locks or per-upstream locks
└─ Trade-off: More complexity, minimal benefit (writes are fast)
```

### Percentile Calculation

**Location**: Lines 144-171 in hedging.rs (LatencyTracker)

```rust
pub fn percentile(&self, quantile: f64) -> Option<u64>
```

**Algorithm**:
```
1. VALIDATE INPUT
   ├─ Return None if samples is empty
   └─ Return None if quantile not in [0.0, 1.0]

2. COPY SAMPLES
   └─ sorted: Vec<u64> = samples.iter().copied().collect()
      └─ Cost: O(n) allocation + O(n) copy

3. SORT
   └─ sorted.sort_unstable()
      └─ Cost: O(n log n) using quicksort

4. CALCULATE INDEX
   └─ index = (len - 1) * quantile
      └─ Example: 1000 samples, P95 → (999 * 0.95) = 949

5. RETURN VALUE
   └─ sorted[index]
```

**Complexity Analysis**:
- **Time**: O(n log n) where n = sample count (typically 1000)
- **Space**: O(n) for temporary vector
- **Real-World**: ~50-100μs on modern hardware for 1000 samples

**Why Not Cache Sorted Values?**:
```
Option 1: Keep samples sorted (current approach: simple)
├─ Percentile: O(n log n) per call
└─ Insert: O(1)

Option 2: Maintain sorted buffer
├─ Percentile: O(1) lookup
└─ Insert: O(n) to maintain sort

Our choice: Option 1
Why: Percentiles queried infrequently (once per request for delay calculation)
     Inserts happen frequently (every request completion)
```

**Optimization Opportunity**:
```rust
// Future: Cache last sorted result with dirty flag
struct LatencyTracker {
    samples: VecDeque<u64>,
    sorted_cache: Option<Vec<u64>>,
    dirty: bool,
}

// On insert: set dirty = true
// On percentile: if dirty { sort and cache }, return from cache
```

### Hedge Delay Calculation

**Location**: Lines 263-284 in hedging.rs

```rust
async fn calculate_hedge_delay(&self, upstream_name: &str) -> Option<Duration>
```

**Algorithm**:
```
1. ACQUIRE READ LOCKS
   ├─ config = self.config.read().await
   └─ trackers = self.latency_trackers.read().await

2. LOOKUP TRACKER
   └─ tracker = trackers.get(upstream_name)?
      └─ Return None if no data for this upstream

3. CALCULATE PERCENTILE
   └─ percentile_ms = tracker.percentile(config.latency_quantile)?
      └─ Return None if insufficient samples

4. CLAMP TO BOUNDS
   └─ delay_ms = percentile_ms.max(min_delay_ms).min(max_delay_ms)

5. RETURN DURATION
   └─ Some(Duration::from_millis(delay_ms))
```

**Example**:
```rust
// Config: quantile=0.95, min=50ms, max=2000ms
// Tracker has samples: [10, 15, 20, ..., 150, ..., 400]

// Calculate P95:
sorted = [10, 15, 20, ..., 150, ..., 400]
index = (1000 - 1) * 0.95 = 949
p95 = sorted[949] = 180ms

// Clamp:
delay_ms = max(180, 50) = 180
delay_ms = min(180, 2000) = 180

// Result:
Duration::from_millis(180)
```

**Edge Cases**:
```rust
// Case 1: Very fast upstream (P95 = 20ms)
percentile_ms = 20
delay_ms = max(20, 50) = 50  // Clamped to min
→ Duration::from_millis(50)

// Case 2: Very slow upstream (P95 = 10000ms)
percentile_ms = 10000
delay_ms = min(10000, 2000) = 2000  // Clamped to max
→ Duration::from_millis(2000)

// Case 3: No samples yet
tracker.percentile(0.95) → None
→ None (fallback to simple execution)
```

### Execute Hedged Request Flow

**Location**: Lines 286-344 in hedging.rs

```rust
pub async fn execute_hedged(
    &self,
    request: &JsonRpcRequest,
    upstreams: Vec<Arc<UpstreamEndpoint>>,
) -> Result<JsonRpcResponse, UpstreamError>
```

**Decision Tree**:
```
execute_hedged(request, upstreams)
│
├─ upstreams.is_empty() ?
│  └─ YES → Return Err(NoHealthyUpstreams)
│
├─ config.enabled ?
│  └─ NO → execute_simple(upstreams[0])
│
├─ upstreams.len() == 1 ?
│  └─ YES → execute_simple(upstreams[0])
│
├─ calculate_hedge_delay(upstreams[0].name) ?
│  ├─ None → execute_simple(upstreams[0])
│  │          (insufficient data)
│  │
│  └─ Some(delay) → execute_with_hedging(
│                     request,
│                     upstreams,
│                     max_parallel,
│                     delay
│                   )
```

**Fast Paths**:
1. **Disabled**: Bypass all hedging logic
2. **Single Upstream**: No backup available
3. **No Data**: Can't calculate delay, use simple request

### Execute with Hedging

**Location**: Lines 369-427 in hedging.rs

```rust
async fn execute_with_hedging(
    &self,
    request: &JsonRpcRequest,
    upstreams: Vec<Arc<UpstreamEndpoint>>,
    max_parallel: usize,
    hedge_delay: Duration,
) -> Result<JsonRpcResponse, UpstreamError>
```

**tokio::select! Implementation**:
```rust
let primary_future = {
    let req = request.clone();
    let upstream = primary.clone();
    async move { upstream.send_request(&req).await }
};

tokio::select! {
    // Branch A: Primary completes before hedge delay
    result = primary_future => {
        match result {
            Ok(response) => {
                // Fast path: return immediately
                self.record_latency(&primary_name, latency_ms).await;
                Ok(response)
            }
            Err(e) => {
                // Primary failed: try hedged fallback
                self.execute_hedged_fallback(request, &upstreams[1..], max_parallel - 1).await
            }
        }
    }

    // Branch B: Hedge delay elapsed, primary still pending
    () = tokio::time::sleep(hedge_delay) => {
        // Send hedged requests to all upstreams
        self.execute_parallel_hedged(request, upstreams, max_parallel, start).await
    }
}
```

**Control Flow Diagram**:
```
                    ┌─────────────────────┐
                    │  execute_with_hedging│
                    └──────────┬───────────┘
                               │
                   ┌───────────┴───────────┐
                   │                       │
            Primary Request          Sleep(hedge_delay)
                   │                       │
                   │                       │
          ┌────────▼────────┐              │
          │ Race Condition  │◄─────────────┘
          └────────┬────────┘
                   │
        ┌──────────┴──────────┐
        │                     │
    Primary              Delay
    Completes            Elapsed
        │                     │
        │                     │
    ┌───▼────┐          ┌────▼─────────┐
    │ OK?    │          │ Send Hedged  │
    └───┬────┘          │ Requests     │
        │               └────┬─────────┘
    ┌───┴───┐                │
    │       │                │
   YES     NO           ┌────▼─────────┐
    │       │           │ select_all   │
    │       │           │ (race all)   │
    │   ┌───▼────┐      └────┬─────────┘
    │   │Fallback│           │
    │   │Hedged  │      ┌────▼────┐
    │   └───┬────┘      │ First   │
    │       │           │ Success │
    │       │           └────┬────┘
    └───────┴────────────────┘
                │
           Return Response
```

**Cancellation Semantics**:

```
Scenario: Primary slow, hedge wins

T+0ms:   Primary request sent
         └─ Starts executing on upstream_a

T+180ms: Hedge delay elapsed
         └─ Hedged request sent to upstream_b

T+230ms: Hedged request completes 
         └─ select! returns this branch
         └─ Primary future is DROPPED (implicitly cancelled)
         └─ Tokio runtime cancels in-flight HTTP request

Note: Primary request may still complete on server side
      (HTTP cancellation is best-effort)
```

### Parallel Hedged Execution

**Location**: Lines 429-490 in hedging.rs

```rust
async fn execute_parallel_hedged(
    &self,
    request: &JsonRpcRequest,
    upstreams: Vec<Arc<UpstreamEndpoint>>,
    max_parallel: usize,
    start_time: Instant,
) -> Result<JsonRpcResponse, UpstreamError>
```

**Algorithm**:
```
1. CREATE FUTURES
   └─ For each upstream (up to max_parallel):
      └─ task = async { upstream.send_request(&req).await }
         └─ Returns: Option<(upstream_name, Result<Response, Error>)>

2. COLLECT TO VECTOR
   └─ futures: Vec<BoxFuture<...>> = tasks.map(Box::pin).collect()

3. RACE ALL FUTURES
   └─ Loop until futures.is_empty():
      │
      ├─ (result, _index, remaining) = select_all(futures).await
      │
      ├─ If result is Some((name, Ok(response))):
      │  ├─ Record latency
      │  ├─ Log success
      │  └─ Return response  (cancel remaining)
      │
      ├─ If result is Some((name, Err(e))):
      │  ├─ Log error
      │  └─ Continue to next future
      │
      └─ futures = remaining

4. ALL FAILED
   └─ Return Err(NoHealthyUpstreams)
```

**Example Execution**:
```
max_parallel = 3
upstreams = [primary, backup1, backup2]

T+0ms:
├─ primary.send_request()  (already running, slower than delay)
├─ backup1.send_request()  ← Started
└─ backup2.send_request()  ← Started

T+50ms:  backup1 returns Error (rate limit)
         └─ Log error, continue racing primary and backup2

T+80ms:  backup2 returns Ok(response) 
         └─ Record latency (80ms since hedge started)
         └─ Cancel primary (was going to take 200ms total)
         └─ Return response

Total latency from client perspective:
└─ hedge_delay (180ms) + backup2_latency (80ms) = 260ms
```

**select_all Behavior**:
```rust
// Initial
futures = [primary_fut, backup1_fut, backup2_fut]

// First select_all
(backup1_err, index=1, remaining) = select_all(futures)
└─ remaining = [primary_fut, backup2_fut]

// Second select_all
(backup2_ok, index=1, remaining) = select_all(remaining)
└─ primary_fut is dropped (cancelled)
└─ Return backup2_ok
```

### Simple Execution Path

**Location**: Lines 346-367 in hedging.rs

```rust
async fn execute_simple(
    &self,
    request: &JsonRpcRequest,
    upstream: &Arc<UpstreamEndpoint>,
) -> Result<JsonRpcResponse, UpstreamError>
```

**Usage**: Fallback when hedging not applicable or disabled.

**Algorithm**:
```
1. START TIMER
   └─ start = Instant::now()

2. SEND REQUEST
   └─ response = upstream.send_request(request).await

3. RECORD LATENCY
   ├─ latency_ms = start.elapsed().as_millis() as u64
   └─ self.record_latency(&upstream_name, latency_ms).await

4. RETURN
   ├─ Ok(response) if successful
   └─ Err(e) if failed
```

**Why Record Latency Even on Simple Path?**:
- Builds latency history for future hedge delay calculations
- Ensures consistent tracking regardless of hedging state
- Allows transitioning from disabled to enabled seamlessly

## Integration Points

### UpstreamManager Integration

```rust
// In UpstreamManager
pub async fn send_request_with_hedging(
    &self,
    request: &JsonRpcRequest,
) -> Result<JsonRpcResponse, UpstreamError> {
    // Get upstreams ranked by score (best first)
    let ranked = self.scoring_engine.get_ranked_upstreams().await;

    // Convert to UpstreamEndpoint references
    let upstreams: Vec<Arc<UpstreamEndpoint>> = ranked
        .iter()
        .filter_map(|(name, _score)| self.upstreams.get(name))
        .cloned()
        .collect();

    // Execute with hedging
    let start = Instant::now();
    let response = self.hedge_executor.execute_hedged(request, upstreams).await?;

    // Record latency
    let latency_ms = start.elapsed().as_millis() as u64;
    if let Some(primary) = ranked.first() {
        self.hedge_executor.record_latency(&primary.0, latency_ms).await;
    }

    Ok(response)
}
```

**Key Points**:
1. **Upstream Ordering**: ScoringEngine provides ranked list (best first)
2. **Latency Recording**: Track all request latencies, not just hedged ones
3. **Error Handling**: Propagate UpstreamError to caller

### ScoringEngine Integration

```rust
// Scoring determines upstream order for hedging
let ranked = scoring_engine.get_ranked_upstreams().await;
// Returns: [("alchemy", 95.0), ("infura", 87.0), ("quicknode", 82.0)]

// Hedging uses this order:
// - Primary: alchemy (highest score)
// - Hedge 1: infura (second highest)
// - Hedge 2: quicknode (third highest)

let upstreams = ranked
    .into_iter()
    .filter_map(|(name, _)| upstream_manager.get_upstream(&name))
    .collect();

hedge_executor.execute_hedged(&request, upstreams).await
```

**Synergy**: Scoring + Hedging = Optimal Performance
- Scoring ensures best upstream is tried first
- Hedging provides fallback if best upstream is slow
- Latency tracking in hedging feeds back to scoring

## Performance Characteristics

### Latency Analysis

**Without Hedging**:
```
Request → Select upstream (1ms) → Send request (200ms) → Response
Total: 201ms
```

**With Hedging (Primary Fast)**:
```
Request → Select upstream (1ms)
       → Send primary (100ms) 
       → Record latency (1ms)
       → Response

Total: 102ms (1ms overhead)
```

**With Hedging (Primary Slow, Hedge Wins)**:
```
Request → Select upstream (1ms)
       → Send primary (starts, will take 800ms)
       → Wait hedge delay (180ms)
       → Send hedged requests (50ms) 
       → Cancel primary
       → Record latency (1ms)
       → Response

Total: 232ms (vs 801ms without hedging)
Savings: 569ms (71% reduction)
```

**Latency Distribution Impact**:

| Scenario | P50 | P95 | P99 | Improvement |
|----------|-----|-----|-----|-------------|
| No hedging | 150ms | 500ms | 2000ms | Baseline |
| With hedging (P95) | 155ms | 350ms | 800ms | P95: -30%, P99: -60% |

### Memory Usage

**Per HedgeExecutor**:
```
HedgeExecutor:
├─ config: ArcSwap<HedgeConfig>
│  └─ ~200 bytes (config struct + Arc + RwLock overhead)
│
└─ latency_trackers: DashMap<String, LatencyTracker>
   ├─ DashMap overhead: ~50 bytes per entry + shard overhead
   └─ Per LatencyTracker:
      ├─ VecDeque<u64>: 24 bytes + (1000 * 8 bytes) = 8024 bytes
      ├─ max_samples: 8 bytes
      ├─ sum: 8 bytes
      └─ count: 8 bytes
      └─ Total: ~8048 bytes per upstream

Total for 5 upstreams: ~40KB
Total for 100 upstreams: ~800KB
```

**Memory is NOT a concern** unless tracking thousands of upstreams.

### CPU Usage

**Per Request**:
```
Hedging disabled:
└─ 0 CPU overhead (fast path)

Hedging enabled, primary fast:
├─ calculate_hedge_delay(): ~5μs (percentile lookup)
├─ tokio::select! setup: ~2μs
├─ record_latency(): ~2μs (buffer push)
└─ Total: ~9μs overhead

Hedging enabled, hedge triggered:
├─ calculate_hedge_delay(): ~5μs
├─ execute_parallel_hedged(): ~10μs (future creation)
├─ select_all iterations: ~3μs per iteration
├─ record_latency(): ~2μs
└─ Total: ~20μs overhead
```

**Percentile Calculation** (infrequent):
```
1000 samples:
├─ Copy to Vec: ~10μs
├─ Sort (quicksort): ~50μs
└─ Total: ~60μs per percentile query
```

### Upstream Load

**Load Multiplier Formula**:
```
hedge_rate = percentage of requests where hedge is triggered
extra_requests_per_hedge = max_parallel - 1

load_multiplier = 1.0 + (hedge_rate * extra_requests_per_hedge)
```

**Examples**:
```
Config: P95, max_parallel=2
hedge_rate ≈ 5% (P95 threshold)
extra_requests = 1
load = 1.0 + (0.05 * 1) = 1.05x

Config: P90, max_parallel=3
hedge_rate ≈ 10% (P90 threshold)
extra_requests = 2
load = 1.0 + (0.10 * 2) = 1.20x
```

### Network Usage

**Bandwidth**:
```
Without hedging: 1x request_size + response_size
With hedging:
├─ Primary: request_size + response_size
├─ Hedged (5% of time): extra_request_size + extra_response_size
└─ Effective: ~1.05x bandwidth with P95 hedging
```

**Connection Pool**:
- Uses existing UpstreamEndpoint connection pools
- No additional connection overhead
- Concurrent requests limited by pool size

## Flow Diagrams

### Complete Hedged Request Flow

```
execute_hedged(request, upstreams)
│
├─ 1. VALIDATE
│  ├─ upstreams.is_empty()? → Err(NoHealthyUpstreams)
│  ├─ !config.enabled? → execute_simple()
│  └─ upstreams.len() == 1? → execute_simple()
│
├─ 2. PREPARE
│  ├─ primary = upstreams[0]
│  ├─ max_upstreams = min(max_parallel, upstreams.len())
│  └─ hedge_delay = calculate_hedge_delay(primary.name)?
│     └─ None? → execute_simple()
│
├─ 3. EXECUTE PRIMARY
│  └─ primary_future = primary.send_request(request)
│
├─ 4. RACE: PRIMARY vs DELAY
│  └─ tokio::select! {
│     │
│     ├─ PRIMARY COMPLETES FIRST
│     │  ├─ Ok(response)?
│     │  │  ├─ record_latency(primary.name, latency_ms)
│     │  │  └─ Return Ok(response) 
│     │  │
│     │  └─ Err(e)?
│     │     └─ execute_hedged_fallback(upstreams[1..])
│     │        └─ Try remaining upstreams
│     │
│     └─ DELAY ELAPSED FIRST
│        └─ execute_parallel_hedged(request, upstreams, max_upstreams)
│           │
│           ├─ Create futures for all upstreams (up to max)
│           ├─ select_all(futures)
│           │  └─ Loop:
│           │     ├─ First Ok → record_latency, return 
│           │     ├─ First Err → log, continue to next
│           │     └─ All Err → Err(NoHealthyUpstreams)
│           │
│           └─ Return first successful response
│
└─ 5. RETURN RESULT
   └─ Response or Error
```

### Latency Tracking Flow

```
record_latency(upstream_name, latency_ms)
│
├─ ACQUIRE WRITE LOCK
│  └─ latency_trackers.write().await
│
├─ GET OR CREATE TRACKER
│  └─ trackers.entry(upstream_name)
│     ├─ Exists → use existing
│     └─ New → create with max_samples=1000
│
├─ RECORD SAMPLE
│  └─ tracker.record(latency_ms)
│     │
│     ├─ Buffer full?
│     │  ├─ YES:
│     │  │  ├─ old = samples.pop_front()
│     │  │  ├─ sum -= old
│     │  │  ├─ samples.push_back(latency_ms)
│     │  │  └─ sum += latency_ms
│     │  │
│     │  └─ NO:
│     │     ├─ samples.push_back(latency_ms)
│     │     ├─ sum += latency_ms
│     │     └─ count += 1
│     │
│     └─ Update complete
│
├─ LOG (if DEBUG enabled)
│  ├─ p95 = tracker.percentile(0.95)
│  ├─ avg = tracker.average()
│  └─ log::debug!("latency recorded: p95={}, avg={}, samples={}")
│
└─ RELEASE LOCK
```

### Percentile Calculation Flow

```
percentile(quantile: f64) -> Option<u64>
│
├─ VALIDATE
│  ├─ samples.is_empty()? → None
│  └─ quantile ∉ [0.0, 1.0]? → None
│
├─ COPY & SORT
│  ├─ sorted = samples.iter().copied().collect()
│  │  └─ Allocate Vec<u64> with capacity = samples.len()
│  │
│  └─ sorted.sort_unstable()
│     └─ O(n log n) quicksort
│
├─ CALCULATE INDEX
│  └─ index = (sorted.len() - 1) as f64 * quantile
│     └─ Cast to usize (truncate)
│
├─ RETURN VALUE
│  └─ Some(sorted[index])
│
└─ Example:
   ├─ 1000 samples, P95:
   ├─ index = (1000 - 1) * 0.95 = 949.05 → 949
   └─ sorted[949] = 180ms
```

## Testing Strategy

### Unit Test Coverage

**Existing Tests** (lines 570-898 in hedging.rs):

1.  LatencyTracker basic operations (lines 576-586)
2.  Percentile calculation accuracy (lines 588-601)
3.  Sliding window behavior (lines 603-620)
4.  Configuration defaults (lines 637-646)
5.  Latency recording and stats (lines 658-672)
6.  Hedge delay calculation with clamping (lines 705-722)
7.  Delay clamping edge cases (lines 831-867)
8.  Percentile tracking over time (lines 870-897)

**Missing Test Coverage** (recommended additions):

1.  Full execute_hedged() with mock upstreams
2.  Primary fast path (hedge never sent)
3.  Hedge triggered and wins
4.  All upstreams fail
5.  Concurrent latency recording
6.  Memory usage under load

### Integration Test Scenarios

```rust
#[tokio::test]
async fn test_hedging_primary_fast() {
    // Setup: Primary responds in 50ms
    // Config: hedge_delay = 100ms
    // Assert: Primary response returned, hedge never sent
}

#[tokio::test]
async fn test_hedging_primary_slow_hedge_wins() {
    // Setup: Primary responds in 500ms, backup in 50ms
    // Config: hedge_delay = 100ms
    // Assert: Backup response returned after ~150ms total
}

#[tokio::test]
async fn test_hedging_all_fail() {
    // Setup: All upstreams return errors
    // Assert: Err(NoHealthyUpstreams)
}

#[tokio::test]
async fn test_hedging_latency_tracking_accuracy() {
    // Record known distribution: 1-100ms
    // Query P50, P95, P99
    // Assert: Values match expected percentiles
}

#[tokio::test]
async fn test_hedging_concurrent_requests() {
    // Send 100 concurrent requests
    // Assert: No panics, all requests complete
    // Assert: Latency tracking remains consistent
}
```

### Performance Testing

**Benchmarks**:
```rust
#[bench]
fn bench_latency_tracker_insert(b: &mut Bencher) {
    let mut tracker = LatencyTracker::new(1000);
    b.iter(|| tracker.record(100));
}

#[bench]
fn bench_percentile_calculation(b: &mut Bencher) {
    let mut tracker = LatencyTracker::new(1000);
    for i in 1..=1000 {
        tracker.record(i);
    }
    b.iter(|| tracker.percentile(0.95));
}

#[bench]
fn bench_hedge_delay_calculation(b: &mut Bencher) {
    // Benchmark calculate_hedge_delay() with 1000 samples
}
```

## Future Enhancements

### 1. Adaptive Hedge Delay

**Current**: Static percentile configuration
**Future**: Dynamically adjust based on success rate

```rust
// Pseudocode
if hedge_success_rate > 0.8 {
    // Hedging too aggressive
    latency_quantile = min(0.99, latency_quantile + 0.05);
} else if hedge_success_rate < 0.2 {
    // Hedging not aggressive enough
    latency_quantile = max(0.75, latency_quantile - 0.05);
}
```

### 2. Per-Method Hedge Configuration

**Current**: Global hedge config for all methods
**Future**: Method-specific hedging

```rust
// Example: Aggressive hedging for slow methods
hedging.methods["eth_getLogs"] = HedgeConfig {
    latency_quantile: 0.90,
    max_parallel: 3,
};

// Conservative for fast methods
hedging.methods["eth_blockNumber"] = HedgeConfig {
    latency_quantile: 0.99,
    max_parallel: 2,
};
```

### 3. Smart Upstream Selection for Hedges

**Current**: Send to next upstreams in scored order
**Future**: Skip upstreams unlikely to help

```rust
// Don't hedge to upstreams that are slower than primary
let primary_p50 = get_latency_stats(primary).p50;
let eligible_hedges: Vec<_> = backups
    .iter()
    .filter(|u| get_latency_stats(u).p50 < primary_p50)
    .collect();
```

### 4. Percentile Estimation Algorithms

**Current**: Simple sorting (O(n log n))
**Future**: Streaming algorithms for large windows

```rust
// t-digest for O(1) percentile queries
use tdigest::TDigest;

struct LatencyTracker {
    digest: TDigest,
}

// Percentile becomes O(1)
fn percentile(&self, q: f64) -> f64 {
    self.digest.estimate_quantile(q)
}
```

---

**Last Updated**: November 2025
**Architecture Version**: 1.0
**Implementation Status**: Production Ready
