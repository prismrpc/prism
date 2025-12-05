# Hedging Module

**File**: `crates/prism-core/src/upstream/hedging.rs`

## Overview

The Hedging module implements request hedging to improve tail latency (P95/P99) by automatically sending duplicate requests to backup upstreams when primary requests exceed expected response times. This prevents slow responses from degraded upstreams from impacting user experience while minimizing unnecessary duplicate work.

### Why Hedging Matters

**Tail Latency Problems Without Hedging**:

1. **Temporary Slowdowns**: A single slow upstream can significantly impact P95/P99 latency even if P50 is fast
2. **Resource Contention**: Upstreams under load may have occasional slow responses while remaining technically healthy
3. **Network Variability**: Temporary network issues can cause sporadic delays
4. **Cold Caches**: First requests to an upstream may be slower until caches warm up

**Real-World Impact**:
- **Web3 Applications**: Slow `eth_getLogs` queries can block UI rendering and frustrate users
- **Trading Bots**: High latency on `eth_call` can miss arbitrage opportunities
- **Block Explorers**: Inconsistent response times damage user trust
- **APIs**: P99 latency often appears in SLA agreements and affects customer satisfaction

### How Hedging Works

The hedging strategy operates through latency-aware request duplication:

1. **Latency Tracking**: Monitor P90/P95/P99 latencies per upstream using a sliding window (default: last 1000 requests)
2. **Primary Request**: Send request to the best upstream based on scoring
3. **Hedge Delay**: Calculate delay based on configured percentile (e.g., P95) clamped to min/max bounds
4. **Conditional Hedging**: If primary doesn't respond within the delay, send hedged requests to other upstreams
5. **First Wins**: Return the first successful response and cancel remaining requests
6. **Latency Recording**: Track all response times to improve future hedging decisions

## Core Responsibilities

- Track latency percentiles (P50/P90/P95/P99) for each upstream using efficient sliding window
- Calculate optimal hedge delays based on historical latency distribution
- Execute parallel hedged requests with intelligent timing
- Return first successful response while canceling slower requests
- Support runtime configuration updates without service restart
- Minimize overhead when hedging is disabled or insufficient latency data exists

## Architecture

### Component Structure

```
HedgeExecutor
├── config: ArcSwap<HedgeConfig>
│   └── Runtime-updatable configuration with hedging rules
├── latency_trackers: DashMap<String, LatencyTracker>
│   └── Per-upstream latency tracking for percentile calculation
├── Execution Strategies
│   ├── execute_simple() - Single upstream, no hedging
│   ├── execute_hedged() - Main entry point with hedging logic
│   ├── execute_with_hedging() - Primary + delayed hedged requests
│   ├── execute_parallel_hedged() - Race all hedged requests
│   └── execute_hedged_fallback() - Fallback when primary fails
└── Utility Methods
    ├── record_latency() - Track response times
    ├── calculate_hedge_delay() - Compute delay from percentiles
    ├── get_latency_stats() - Query P50/P95/P99/avg
    └── clear_latency_data() - Reset tracking state
```

### LatencyTracker

**Location**: Lines 95-195 in hedging.rs

```
LatencyTracker
├── samples: VecDeque<u64>
│   └── Circular buffer of latency measurements in milliseconds
├── max_samples: usize
│   └── Window size (default: 1000 samples)
├── sum: u64
│   └── Sum of all samples for average calculation
├── count: usize
│   └── Current number of samples (≤ max_samples)
└── Methods
    ├── record(latency_ms) - Add new sample, evict oldest if full
    ├── percentile(quantile) - Calculate P50/P90/P95/P99
    ├── average() - Return mean latency
    ├── sample_count() - Return number of tracked samples
    └── clear() - Reset all samples
```

## Configuration

### HedgeConfig Structure

**Location**: Lines 30-89 in hedging.rs

```rust
pub struct HedgeConfig {
    /// Enable/disable hedging globally (default: false)
    pub enabled: bool,

    /// Latency percentile to trigger hedging (default: 0.95 = P95)
    pub latency_quantile: f64,

    /// Minimum delay before hedging in milliseconds (default: 50ms)
    pub min_delay_ms: u64,

    /// Maximum delay before hedging in milliseconds (default: 2000ms)
    pub max_delay_ms: u64,

    /// Maximum parallel requests including primary (default: 2)
    pub max_parallel: usize,
}
```

### Default Configuration

**Location**: Lines 79-89 in hedging.rs

- **enabled**: `false` (opt-in feature)
- **latency_quantile**: 0.95 (P95 latency)
- **min_delay_ms**: 50 (minimum 50ms delay)
- **max_delay_ms**: 2000 (maximum 2s delay)
- **max_parallel**: 2 (primary + 1 hedged request)

### TOML Configuration Examples

#### Aggressive Hedging (Low Latency Priority)

```toml
[hedging]
enabled = true
latency_quantile = 0.90    # Hedge at P90 (earlier)
min_delay_ms = 20          # Lower minimum delay
max_delay_ms = 500         # Lower maximum delay
max_parallel = 3           # Allow 2 hedged requests
```

**Use Case**: Latency-critical applications (trading bots, real-time dashboards)

**Trade-offs**:
-  Lowest tail latency (P95/P99 significantly reduced)
-  Quick recovery from slow upstreams
-  Higher upstream load (more duplicate requests)
-  Higher cost (more RPC calls)

---

#### Balanced Hedging (Recommended)

```toml
[hedging]
enabled = true
latency_quantile = 0.95    # Hedge at P95 (default)
min_delay_ms = 50          # 50ms minimum
max_delay_ms = 2000        # 2s maximum
max_parallel = 2           # Primary + 1 hedge
```

**Use Case**: General production deployments balancing latency and cost

**Trade-offs**:
-  Good tail latency improvement
-  Moderate upstream load increase
-  Cost-effective (hedges only slow requests)
- ️ P99 improvements moderate vs aggressive

---

#### Conservative Hedging (Cost Priority)

```toml
[hedging]
enabled = true
latency_quantile = 0.99    # Hedge at P99 (only very slow requests)
min_delay_ms = 100         # 100ms minimum
max_delay_ms = 5000        # 5s maximum
max_parallel = 2           # Primary + 1 hedge
```

**Use Case**: Cost-sensitive deployments where slight latency increase is acceptable

**Trade-offs**:
-  Minimal upstream load increase
-  Low cost (hedges only pathological cases)
- ️ Modest tail latency improvement
-  Won't help P95, only P99+

---

#### Disabled (Testing/Development)

```toml
[hedging]
enabled = false
```

**Use Case**: Development environments, debugging, or when hedging overhead is unacceptable

---

## Performance Considerations

### Latency Impact

**Without Hedging**:
```
Client Request → Select upstream → Send request → Wait for response
                 1ms               0ms            200ms
Total: 200ms
```

**With Hedging (P95 hedge delay = 150ms)**:
```
Client Request → Select upstream → Send primary request
                 1ms               0ms
                                   ├─ Primary responds in 100ms → Return 
                                   └─ (Hedge never sent, no overhead)
Total: 101ms
```

**With Hedging (Slow primary)**:
```
Client Request → Select upstream → Send primary request
                 1ms               0ms
                                   ├─ Wait 150ms (P95 delay)
                                   ├─ Primary still pending...
                                   ├─ Send hedged request to backup
                                   │  └─ Hedged responds in 50ms → Return 
                                   └─ Cancel primary (was going to take 800ms)
Total: 201ms (vs 801ms without hedging)
```

**Best Case**: Same latency as without hedging when primary is fast
**Typical Case**: Slightly higher latency due to tracking overhead (~1-2ms)
**Worst Case**: Hedge delay + fastest hedged upstream (~150ms + 50ms = 200ms)

### Upstream Load

| Configuration | Load Multiplier | Example (1000 req/s) | Hedge Rate |
|---------------|-----------------|----------------------|------------|
| Disabled | 1.0x | 1000 req/s | 0% |
| P95, max=2 | 1.05x | 1050 req/s | ~5% |
| P90, max=2 | 1.10x | 1100 req/s | ~10% |
| P95, max=3 | 1.10x | 1100 req/s | ~5-10% |
| P90, max=3 | 1.20x | 1200 req/s | ~10-20% |

**Key Insight**: Actual load increase depends on upstream reliability and latency distribution. Fast, reliable upstreams rarely trigger hedges.

### Cache Interaction

Hedging complements caching:

```
Request Flow:
├─ Cache HIT (95% of requests) → No upstream call, no hedging
└─ Cache MISS (5% of requests) → Hedging applies
   ├─ Primary fast (95% of cache misses) → No hedge sent
   └─ Primary slow (5% of cache misses) → Hedge sent
```

**Effective Load**: If cache hit rate is 95% and hedge rate is 5%:
- Cache misses: 5% need upstream calls
- Of those, 5% trigger hedges
- Load increase: 0.05 * 0.05 * 1 = 0.0025 = 0.25% overall

**Conclusion**: Hedging overhead is negligible when combined with effective caching.

## Public API

### Constructor

#### `new(config: HedgeConfig) -> Self`

**Location**: Lines 213-221 in hedging.rs

Creates a new hedge executor with the given configuration.

**Parameters**:
- `config`: Hedging configuration specifying delay rules and parallelism

**Returns**: New `HedgeExecutor` instance

**Usage**:
```rust
let config = HedgeConfig {
    enabled: true,
    latency_quantile: 0.95,
    min_delay_ms: 50,
    max_delay_ms: 2000,
    max_parallel: 2,
};

let executor = HedgeExecutor::new(config);
```

### Configuration Management

#### `update_config(&self, config: HedgeConfig)`

**Location**: Lines 224-228 in hedging.rs

Updates the hedging configuration at runtime without restarting the service.

**Parameters**:
- `config`: New hedging configuration

**Usage**:
```rust
let new_config = HedgeConfig {
    enabled: true,
    latency_quantile: 0.90,  // More aggressive hedging
    min_delay_ms: 30,
    max_delay_ms: 1000,
    max_parallel: 3,
};

executor.update_config(new_config).await;
```

**Implementation Notes**:
- Acquires write lock on config
- Logs configuration update
- Affects all subsequent requests immediately
- Does not affect in-flight hedged requests

---

#### `get_config() -> HedgeConfig`

**Location**: Lines 231-233 in hedging.rs

Retrieves the current hedging configuration.

**Returns**: Clone of current configuration

**Usage**:
```rust
let config = executor.get_config().await;
println!("Hedging enabled: {}, P{:.0} delay",
    config.enabled, config.latency_quantile * 100.0);
```

### Latency Tracking

#### `record_latency(&self, upstream_name: &str, latency_ms: u64)`

**Location**: Lines 236-261 in hedging.rs

Records a latency measurement for an upstream endpoint.

**Parameters**:
- `upstream_name`: Name of the upstream endpoint
- `latency_ms`: Response latency in milliseconds

**Usage**:
```rust
let start = Instant::now();
let response = upstream.send_request(&request).await?;
let latency_ms = start.elapsed().as_millis() as u64;

executor.record_latency("alchemy-mainnet", latency_ms).await;
```

**Implementation Details**:
- Creates new tracker if upstream not seen before (window size: 1000 samples)
- Adds sample to circular buffer, evicting oldest if full
- Logs P95, average, and sample count at DEBUG level

---

#### `get_latency_stats(&self, upstream_name: &str) -> Option<(u64, u64, u64, u64)>`

**Location**: Lines 543-560 in hedging.rs

Returns latency statistics for a specific upstream.

**Parameters**:
- `upstream_name`: Name of the upstream to query

**Returns**:
- `Some((p50, p95, p99, avg))` if latency data exists
- `None` if no samples recorded for this upstream

**Usage**:
```rust
if let Some((p50, p95, p99, avg)) = executor.get_latency_stats("infura").await {
    println!("Infura latency: P50={}ms, P95={}ms, P99={}ms, Avg={}ms",
        p50, p95, p99, avg);
}
```

---

#### `clear_latency_data(&self)`

**Location**: Lines 563-567 in hedging.rs

Clears all latency tracking data across all upstreams.

**Usage**:
```rust
// Reset tracking after upstream infrastructure changes
executor.clear_latency_data().await;
```

**Use Cases**:
- Testing and benchmarking (reset state between tests)
- After upstream configuration changes
- Manual reset when latency patterns change dramatically

### Hedged Execution

#### `execute_hedged(&self, request: &JsonRpcRequest, upstreams: Vec<Arc<UpstreamEndpoint>>) -> Result<JsonRpcResponse, UpstreamError>`

**Location**: Lines 286-344 in hedging.rs

**PRIMARY METHOD** - Executes a request with optional hedging based on configuration and latency data.

**Parameters**:
- `request`: JSON-RPC request to execute
- `upstreams`: List of upstream endpoints (first is primary, rest are backups)

**Returns**:
- `Ok(JsonRpcResponse)`: First successful response
- `Err(UpstreamError)`: All requests failed

**Error Conditions**:
- `UpstreamError::NoHealthyUpstreams`: Empty upstream list
- `UpstreamError::*`: All upstreams returned errors

**Usage**:
```rust
use prism_core::{
    types::JsonRpcRequest,
    upstream::{HedgeExecutor, HedgeConfig},
};

let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getLogs".to_string(),
    params: Some(serde_json::json!([{
        "fromBlock": "0x1000",
        "toBlock": "0x2000"
    }])),
    id: serde_json::json!(1),
};

let upstreams = vec![
    alchemy_upstream.clone(),
    infura_upstream.clone(),
    quicknode_upstream.clone(),
];

match executor.execute_hedged(&request, upstreams).await {
    Ok(response) => println!("Response: {:?}", response),
    Err(e) => eprintln!("All requests failed: {}", e),
}
```

**Implementation Flow** (lines 301-344):

1. **Validate Inputs** (lines 306-308)
   - Return error if no upstreams available

2. **Check Hedging Conditions** (lines 312-315)
   - If hedging disabled or only one upstream: use simple execution
   - Avoids overhead when hedging isn't beneficial

3. **Calculate Hedge Delay** (lines 323-331)
   - Query latency tracker for primary upstream
   - Compute delay based on `latency_quantile` percentile
   - Fall back to simple execution if insufficient data

4. **Execute with Hedging** (line 343)
   - Delegate to `execute_with_hedging()` for the full hedging logic

**Performance Notes**:
- Fast path when hedging disabled: ~0 overhead
- With hedging: adds latency tracking (~1-2μs per request)
- Hedge decision: O(1) percentile lookup from pre-sorted samples

## Result Types

### Latency Statistics

The `get_latency_stats()` method returns a tuple of percentiles:

```rust
(p50, p95, p99, avg)
```

**Field Details**:
- **p50**: Median latency - 50% of requests complete in this time or less
- **p95**: 95th percentile - only 5% of requests take longer
- **p99**: 99th percentile - only 1% of requests take longer
- **avg**: Average latency across all samples

**Example Analysis**:
```rust
let (p50, p95, p99, avg) = executor.get_latency_stats("alchemy").await.unwrap();

// p50 = 100ms, p95 = 200ms, p99 = 500ms, avg = 120ms
// Interpretation:
// - Most requests (50%) complete in 100ms
// - 95% complete in 200ms or less → hedge delay would be ~200ms with P95 config
// - 1% of requests take 500ms+ (tail latency)
// - Average slightly above median indicates positive skew (some slow outliers)
```

## Internal Implementation

### Hedge Delay Calculation

**Location**: Lines 263-284 in hedging.rs

```rust
async fn calculate_hedge_delay(&self, upstream_name: &str) -> Option<Duration> {
    let config = self.config.read().await;
    let trackers = self.latency_trackers.read().await;

    let tracker = trackers.get(upstream_name)?;
    let percentile_ms = tracker.percentile(config.latency_quantile)?;

    // Clamp between min and max delay
    let delay_ms = percentile_ms.max(config.min_delay_ms).min(config.max_delay_ms);

    Some(Duration::from_millis(delay_ms))
}
```

**Algorithm**:
1. Read current configuration
2. Look up latency tracker for the upstream
3. Calculate the configured percentile (e.g., P95)
4. Clamp result between `min_delay_ms` and `max_delay_ms`
5. Convert to Duration

**Clamping Examples**:

```rust
// Fast upstream (P95 = 30ms)
percentile_ms = 30
delay_ms = max(30, 50) = 50  // Clamped to minimum
Duration::from_millis(50)

// Normal upstream (P95 = 200ms)
percentile_ms = 200
delay_ms = max(200, 50).min(2000) = 200  // No clamping needed
Duration::from_millis(200)

// Slow upstream (P95 = 5000ms)
percentile_ms = 5000
delay_ms = min(5000, 2000) = 2000  // Clamped to maximum
Duration::from_millis(2000)
```

**Why Clamp?**:
- **min_delay_ms**: Prevents hedging on very fast requests where network overhead dominates
- **max_delay_ms**: Prevents excessively long delays that defeat the purpose of hedging

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

**Algorithm**:

```
1. SEND PRIMARY REQUEST
   └─ Start async request to upstreams[0]

2. RACE TWO FUTURES
   ├─ A. Primary completes
   │  ├─ If success: Record latency and return response
   │  └─ If error: Execute fallback with remaining upstreams
   │
   └─ B. Hedge delay elapsed
      └─ Send parallel hedged requests to all upstreams
         └─ Return first successful response
```

**tokio::select! Usage** (lines 392-426):
```rust
tokio::select! {
    // Branch A: Primary completes before hedge delay
    result = primary_future => {
        match result {
            Ok(response) => {
                self.record_latency(&primary_name, latency_ms).await;
                Ok(response)  // Fast path: return immediately
            }
            Err(e) => {
                // Primary failed, try hedged fallback
                self.execute_hedged_fallback(request, &upstreams[1..], max_parallel - 1).await
            }
        }
    }

    // Branch B: Hedge delay elapsed, primary still pending
    () = tokio::time::sleep(hedge_delay) => {
        // Issue hedged requests to all upstreams in parallel
        self.execute_parallel_hedged(request, upstreams, max_parallel, start).await
    }
}
```

**Key Insight**: The `select!` macro races the two branches. Whichever completes first wins, and the other is cancelled.

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
1. CREATE FUTURES for up to max_parallel upstreams
   └─ futures = [upstream_1.send(), upstream_2.send(), upstream_3.send()]

2. RACE ALL FUTURES using select_all
   └─ Loop:
      ├─ select_all(futures) → (result, _index, remaining_futures)
      ├─ If result is Ok: Record latency, return response 
      ├─ If result is Err: Log error, continue to next
      └─ futures = remaining_futures

3. IF ALL FAIL
   └─ Return Err(NoHealthyUpstreams)
```

**Why select_all?**:
- Returns first completed future (success or error)
- Provides remaining futures to continue racing if first fails
- Cancels all remaining futures when one succeeds

**Example Execution**:
```
Start: 3 hedged requests sent in parallel

T+50ms:  upstream_2 returns Error (timeout)
         → Log error, continue racing upstream_1 and upstream_3

T+80ms:  upstream_3 returns Ok(response)
         → Record latency, return response
         → Cancel upstream_1 (was going to take 200ms)

Total latency: 80ms (vs 200ms if we waited for all)
```

### Percentile Calculation

**Location**: Lines 144-171 in hedging.rs (LatencyTracker)

```rust
pub fn percentile(&self, quantile: f64) -> Option<u64> {
    if self.samples.is_empty() || !(0.0..=1.0).contains(&quantile) {
        return None;
    }

    let mut sorted: Vec<u64> = self.samples.iter().copied().collect();
    sorted.sort_unstable();

    let index = ((sorted.len() as f64 - 1.0) * quantile) as usize;
    Some(sorted[index])
}
```

**Algorithm**:
1. Validate quantile is in [0.0, 1.0]
2. Copy samples to temporary vector
3. Sort samples ascending
4. Calculate index: `(n - 1) * quantile`
5. Return value at index

**Complexity**:
- Time: O(n log n) for sorting where n = sample count (typically 1000)
- Space: O(n) for temporary vector
- Per request: ~50-100μs on modern hardware

**Why Simple Sorting?**:
- Sample count is small (1000 max)
- Percentile queries are infrequent (only when calculating hedge delay)
- More sophisticated algorithms (t-digest, P²) add complexity for minimal benefit

**Alternative Algorithms** (not implemented):
- **t-digest**: O(1) percentile with O(log n) insert, but more complex
- **P² algorithm**: Online streaming percentiles, but approximate
- **Pre-sorted buffer**: Faster percentiles but O(n) inserts

## Integration with UpstreamManager

The hedging executor is designed to be called by `UpstreamManager` or `ProxyEngine`:

```rust
// In UpstreamManager or ProxyEngine
pub async fn send_request_with_hedging(
    &self,
    request: &JsonRpcRequest,
) -> Result<JsonRpcResponse, UpstreamError> {
    // Get ranked upstreams (best first)
    let upstreams = self.scoring_engine.get_ranked_upstreams().await;

    // Convert names to UpstreamEndpoint references
    let upstream_refs: Vec<Arc<UpstreamEndpoint>> = upstreams
        .iter()
        .filter_map(|name| self.upstreams.get(name))
        .cloned()
        .collect();

    // Execute with hedging
    let start = Instant::now();
    let response = self.hedge_executor.execute_hedged(request, upstream_refs).await?;

    // Record latency for primary upstream (first in list)
    let latency_ms = start.elapsed().as_millis() as u64;
    self.hedge_executor.record_latency(upstreams[0].0, latency_ms).await;

    Ok(response)
}
```

**Integration Points**:
1. **Upstream Selection**: Provide ordered list of upstreams (best first)
2. **Latency Recording**: Track response times for all requests
3. **Error Handling**: Handle `UpstreamError` variants appropriately
4. **Configuration**: Update hedge config based on application needs

## Usage Examples

### Basic Hedging Execution

```rust
use prism_core::{
    upstream::{HedgeExecutor, HedgeConfig},
    types::JsonRpcRequest,
};

// Create hedge executor
let config = HedgeConfig {
    enabled: true,
    latency_quantile: 0.95,  // P95
    min_delay_ms: 50,
    max_delay_ms: 2000,
    max_parallel: 2,
};
let executor = HedgeExecutor::new(config);

// Prepare request
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_call".to_string(),
    params: Some(serde_json::json!([{
        "to": "0x...",
        "data": "0x..."
    }, "latest"])),
    id: serde_json::json!(1),
};

// Get upstreams (from UpstreamManager)
let upstreams = vec![
    primary_upstream.clone(),
    backup_upstream.clone(),
];

// Execute with hedging
match executor.execute_hedged(&request, upstreams).await {
    Ok(response) => println!("Response: {:?}", response),
    Err(e) => eprintln!("Request failed: {}", e),
}
```

### Monitoring Latency Statistics

```rust
// Query latency stats periodically
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));

    loop {
        interval.tick().await;

        for upstream_name in &["alchemy", "infura", "quicknode"] {
            if let Some((p50, p95, p99, avg)) = executor.get_latency_stats(upstream_name).await {
                info!(
                    upstream = upstream_name,
                    p50 = p50,
                    p95 = p95,
                    p99 = p99,
                    avg = avg,
                    "latency statistics"
                );

                // Alert if P99 is too high
                if p99 > 5000 {
                    warn!(upstream = upstream_name, p99 = p99, "high tail latency detected");
                }
            }
        }
    }
});
```

### Dynamic Configuration Adjustment

```rust
// Adjust hedging based on observed performance
let stats = executor.get_latency_stats("primary-upstream").await;

if let Some((_p50, p95, p99, _avg)) = stats {
    let new_config = if p99 > 3000 {
        // High tail latency: aggressive hedging
        HedgeConfig {
            enabled: true,
            latency_quantile: 0.90,  // Hedge earlier
            min_delay_ms: 30,
            max_delay_ms: 1000,
            max_parallel: 3,  // More parallel requests
        }
    } else if p95 < 200 {
        // Low latency: conservative hedging
        HedgeConfig {
            enabled: true,
            latency_quantile: 0.99,  // Hedge only outliers
            min_delay_ms: 100,
            max_delay_ms: 2000,
            max_parallel: 2,
        }
    } else {
        // Normal latency: balanced hedging
        HedgeConfig::default()
    };

    executor.update_config(new_config).await;
}
```

### Hedging with Error Handling

```rust
let upstreams = vec![primary, backup1, backup2];

match executor.execute_hedged(&request, upstreams).await {
    Ok(response) => {
        info!("request succeeded");
        Ok(response)
    }
    Err(UpstreamError::NoHealthyUpstreams) => {
        error!("all upstreams failed");
        // Fallback logic or return error to client
        Err(UpstreamError::NoHealthyUpstreams)
    }
    Err(e) => {
        error!(error = %e, "hedged request error");
        Err(e)
    }
}
```

## Related Components

### ScoringEngine
**File**: `crates/prism-core/src/upstream/scoring.rs`
**Documentation**: [scoring/README.md](../scoring/)

The scoring system determines upstream ordering for hedging:

```rust
// Get ranked upstreams (best first)
let ranked = scoring_engine.get_ranked_upstreams().await;

// Use for hedging (primary = highest score)
let upstreams: Vec<Arc<UpstreamEndpoint>> = ranked
    .iter()
    .filter_map(|(name, _score)| upstream_manager.get_upstream(name))
    .collect();

executor.execute_hedged(&request, upstreams).await
```

### UpstreamManager
**File**: `crates/prism-core/src/upstream/manager.rs`
**Documentation**: [../upstream/upstream_manager.md](../upstream/upstream_manager.md)

Provides upstreams for hedging and tracks request outcomes:

- **get_healthy_upstreams()**: Filters to healthy upstreams only
- **get_upstream()**: Retrieves specific upstream by name
- **Integration**: Calls `execute_hedged()` for requests

### ConsensusEngine
**Module**: `crates/prism-core/src/upstream/consensus/`
**Documentation**: [../consensus/README.md](../consensus/)

Hedging and consensus can work together:

```rust
// First: Try consensus (if required for method)
if consensus_engine.requires_consensus(&request.method).await {
    return consensus_engine.execute_consensus(&request, upstreams, &scoring_engine).await;
}

// Otherwise: Use hedging for tail latency
executor.execute_hedged(&request, upstreams).await
```

## Testing

### Unit Tests

**Location**: Lines 570-898 in hedging.rs

**Test Coverage**:

1. **test_latency_tracker_basic** (lines 576-586)
   - Validates basic latency recording and averaging
   - Ensures sample count increments correctly

2. **test_latency_tracker_percentiles** (lines 588-601)
   - Tests P50/P95/P99 calculation with known distribution
   - Records 100 samples (1-100ms) and verifies percentiles

3. **test_latency_tracker_sliding_window** (lines 603-620)
   - Validates circular buffer behavior
   - Ensures old samples evicted when window fills

4. **test_hedge_config_defaults** (lines 637-646)
   - Verifies default configuration values

5. **test_hedge_executor_latency_recording** (lines 658-672)
   - Tests latency recording across multiple calls
   - Validates P50 and average calculations

6. **test_calculate_hedge_delay_basic** (lines 705-722)
   - Tests hedge delay calculation with clamping
   - Ensures delay respects min/max bounds

7. **test_hedge_timeout_configuration** (lines 831-867)
   - Tests delay clamping with very fast/slow upstreams
   - Validates min_delay_ms and max_delay_ms enforcement

### Integration Testing

**Recommended Integration Tests**:

```rust
#[tokio::test]
async fn test_hedging_with_slow_primary() {
    // Setup: Primary upstream slow, backup fast
    // Execute: Hedged request with P95 delay = 100ms
    // Assert: Backup response returned, latency < primary response time
}

#[tokio::test]
async fn test_hedging_with_fast_primary() {
    // Setup: Primary upstream fast
    // Execute: Hedged request
    // Assert: Primary response returned, hedge never sent
}

#[tokio::test]
async fn test_hedging_all_upstreams_fail() {
    // Setup: All upstreams return errors
    // Execute: Hedged request
    // Assert: NoHealthyUpstreams error returned
}

#[tokio::test]
async fn test_hedging_percentile_accuracy() {
    // Setup: Record known latency distribution
    // Execute: Calculate hedge delay
    // Assert: Delay matches expected percentile
}
```

## Performance Monitoring

### Key Metrics to Track

1. **Hedge Rate**: Percentage of requests where hedging was triggered
2. **Hedge Success Rate**: How often hedged request was faster than primary
3. **Latency Reduction**: P95/P99 before vs after hedging
4. **Upstream Load**: Increase in requests per upstream
5. **Hedge Delay Accuracy**: How well delays match actual latency

### Logging

The hedging module logs key events:

```rust
// Hedge delay calculation (line 252)
debug!(
    upstream = %upstream_name,
    latency_ms = latency_ms,
    p95 = p95,
    avg = tracker.average().unwrap_or(0),
    samples = tracker.sample_count(),
    "recorded upstream latency"
);

// Insufficient latency data (line 327)
debug!(
    upstream = %primary_name,
    "insufficient latency data for hedging, using simple execution"
);

// Hedging execution (line 335)
debug!(
    primary = %primary_name,
    hedge_delay_ms = delay.as_millis(),
    max_parallel = max_upstreams,
    "executing hedged request"
);

// Primary success before hedge (line 399)
debug!(
    upstream = %primary_name,
    latency_ms = latency_ms,
    "primary request succeeded before hedge"
);

// Hedge delay elapsed (line 417)
debug!(
    upstream = %primary_name,
    delay_ms = hedge_delay.as_millis(),
    "hedge delay elapsed, issuing hedged requests"
);

// Hedged request success (line 473)
info!(
    upstream = %upstream_name,
    latency_ms = latency_ms,
    hedged = true,
    "hedged request succeeded"
);
```

---

**Last Updated**: November 2025
**Module Version**: 1.0
**Implementation Status**: Production Ready
