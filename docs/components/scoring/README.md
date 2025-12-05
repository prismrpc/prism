# Scoring Module

**File**: `crates/prism-core/src/upstream/scoring.rs`

## Overview

The Scoring module implements a multi-factor scoring algorithm for ranking upstream RPC endpoints based on real-time performance metrics. It uses a composite score derived from latency percentiles, error rates, throttle rates, block lag, and total request counts to intelligently route requests to the most reliable and performant upstreams.

This approach is inspired by eRPC's scoring methodology, adapting it for Prism's architecture with enhanced metric tracking and real-time score calculation.

### Why Scoring Matters

**Problems Without Intelligent Scoring**:

1. **Round-Robin is Blind**: Treats all upstreams equally regardless of performance differences
2. **Slow Upstreams Degrade UX**: Poor upstreams get equal traffic, hurting P50/P95/P99 latency
3. **Error-Prone Nodes Waste Resources**: Failed requests consume retries and user patience
4. **Stale Nodes Return Incorrect Data**: Nodes behind the chain tip may provide outdated information
5. **Rate-Limited Upstreams Block Requests**: Throttled upstreams cause cascading failures

**Real-World Impact**:
- **DeFi Applications**: Slow `eth_call` responses delay price quotes, causing bad trades
- **NFT Marketplaces**: High error rates lead to failed mints and user frustration
- **Block Explorers**: Stale nodes show outdated transaction status
- **Trading Bots**: Throttled upstreams miss arbitrage opportunities

### How Scoring Works

The scoring engine tracks multiple performance factors per upstream and combines them into a composite score:

1. **Latency Factor** (weight: 8.0): Based on P90 latency using logarithmic scaling
2. **Error Rate Factor** (weight: 4.0): Percentage of failed requests (non-throttle errors)
3. **Throttle Rate Factor** (weight: 3.0): Percentage of HTTP 429 / rate limit responses
4. **Block Lag Factor** (weight: 2.0): Blocks behind the chain tip
5. **Load Factor** (weight: 1.0): Total requests (for load balancing)

**Composite Score Formula**:
```
composite_score = latency_factor^8.0
                × error_rate_factor^4.0
                × throttle_factor^3.0
                × block_lag_factor^2.0
                × load_factor^1.0
                × 100

Each factor ∈ [0.0, 1.0], where 1.0 is best
Composite score ∈ [0.0, 100.0], where 100.0 is perfect
```

## Core Responsibilities

- Track latency percentiles (P50/P90/P95/P99) using shared LatencyTracker from hedging module
- Monitor error rates, throttle rates, and success counts within a sliding time window
- Track block numbers to detect chain tip lag across upstreams
- Calculate individual factor scores using appropriate scaling (linear, logarithmic, exponential)
- Combine factors into weighted composite score using multiplicative formula
- Provide ranked upstream lists for request routing
- Support runtime configuration updates without service restart
- Reset metrics automatically when time window expires

## Architecture

### Component Structure

```
ScoringEngine
├── config: ArcSwap<ScoringConfig>
│   └── Runtime-updatable configuration with weights and thresholds (lock-free reads)
├── metrics: DashMap<String, Arc<UpstreamMetrics>>
│   └── Per-upstream metric tracking (lock-free concurrent access)
├── chain_state: Arc<ChainState>
│   └── Shared chain state for unified tip tracking (secondary writer)
├── Metric Recording
│   ├── record_success() - Track successful request with latency
│   ├── record_error() - Track error (non-throttle)
│   ├── record_throttle() - Track throttle response (HTTP 429)
│   └── record_block_number() - Update block height
├── Score Calculation
│   ├── calculate_score() - Compute composite score for one upstream
│   ├── calculate_latency_factor() - Logarithmic scaling
│   ├── calculate_error_rate_factor() - Linear scaling
│   ├── calculate_throttle_factor() - Exponential scaling
│   ├── calculate_block_lag_factor() - Linear penalty
│   └── calculate_load_factor() - Load balancing (placeholder)
└── Query Methods
    ├── get_ranked_upstreams() - Return all upstreams sorted by score
    ├── get_score() - Get score for specific upstream
    ├── get_metrics() - Get raw metrics for debugging
    └── chain_tip() - Get current chain tip
```

### UpstreamMetrics

**Location**: Lines 147-298 in scoring.rs

```
UpstreamMetrics
├── latency_tracker: RwLock<LatencyTracker>
│   └── Reuses hedging module's implementation
├── total_requests: AtomicU64
│   └── Request count in current window
├── error_count: AtomicU64
│   └── Non-throttle error count
├── throttle_count: AtomicU64
│   └── Throttle response count (HTTP 429)
├── success_count: AtomicU64
│   └── Success count
├── last_block_seen: AtomicU64
│   └── Latest block number from this upstream
├── last_block_time: RwLock<Option<Instant>>
│   └── Timestamp when last block was observed
├── window_start: RwLock<Instant>
│   └── Start of current measurement window
└── window_duration: Duration
    └── Window length (default: 1800s = 30 min)
```

## Configuration

### ScoringConfig Structure

**Location**: Lines 32-86 in scoring.rs

```rust
pub struct ScoringConfig {
    /// Enable/disable scoring-based selection (default: false)
    pub enabled: bool,

    /// Time window for metric collection in seconds (default: 1800 = 30 min)
    pub window_seconds: u64,

    /// Minimum samples required before scoring is active (default: 10)
    pub min_samples: usize,

    /// Scoring factor weights
    pub weights: ScoringWeights,

    /// Maximum block lag before heavy penalty (default: 5)
    pub max_block_lag: u64,

    /// Number of top upstreams to consider (default: 3)
    pub top_n: usize,
}
```

### ScoringWeights

**Location**: Lines 88-141 in scoring.rs

```rust
pub struct ScoringWeights {
    /// Weight for latency factor (default: 8.0)
    pub latency: f64,

    /// Weight for error rate factor (default: 4.0)
    pub error_rate: f64,

    /// Weight for throttle rate factor (default: 3.0)
    pub throttle_rate: f64,

    /// Weight for block head lag factor (default: 2.0)
    pub block_head_lag: f64,

    /// Weight for total requests factor (default: 1.0)
    pub total_requests: f64,
}
```

### Default Configuration

**Location**: Lines 75-85, 131-140 in scoring.rs

- **enabled**: `false` (opt-in feature)
- **window_seconds**: 1800 (30 minutes)
- **min_samples**: 10 (require at least 10 samples before scoring)
- **max_block_lag**: 5 (penalize upstreams more than 5 blocks behind)
- **top_n**: 3 (consider top 3 upstreams for selection)
- **weights**:
  - latency: 8.0 (most important factor)
  - error_rate: 4.0
  - throttle_rate: 3.0
  - block_head_lag: 2.0
  - total_requests: 1.0 (least important)

### TOML Configuration Examples

#### Latency-Focused (Low-Latency Apps)

```toml
[scoring]
enabled = true
window_seconds = 1800
min_samples = 10
max_block_lag = 5
top_n = 3

[scoring.weights]
latency = 10.0            # Even higher weight on latency
error_rate = 3.0          # Less weight on errors
throttle_rate = 2.0       # Less weight on throttles
block_head_lag = 1.0      # Less weight on lag
total_requests = 1.0
```

**Use Case**: Trading bots, real-time dashboards, gaming

**Trade-offs**:
-  Routes to fastest upstreams consistently
-  Minimizes P50/P95/P99 latency
- ️ May tolerate higher error rates if upstream is fast
-  Less protection against unreliable nodes

---

#### Reliability-Focused (High-Availability Apps)

```toml
[scoring]
enabled = true
window_seconds = 1800
min_samples = 10
max_block_lag = 3         # Stricter block lag tolerance
top_n = 3

[scoring.weights]
latency = 5.0             # Lower weight on latency
error_rate = 8.0          # Higher weight on errors
throttle_rate = 6.0       # Higher weight on throttles
block_head_lag = 4.0      # Higher weight on lag
total_requests = 1.0
```

**Use Case**: DeFi protocols, financial applications, critical infrastructure

**Trade-offs**:
-  Routes to most reliable upstreams
-  Avoids error-prone and throttled nodes
-  Prefers upstreams at chain tip
- ️ May accept slightly higher latency for reliability

---

#### Balanced (Recommended Default)

```toml
[scoring]
enabled = true
window_seconds = 1800
min_samples = 10
max_block_lag = 5
top_n = 3

[scoring.weights]
latency = 8.0
error_rate = 4.0
throttle_rate = 3.0
block_head_lag = 2.0
total_requests = 1.0
```

**Use Case**: General production deployments, web applications, APIs

**Trade-offs**:
-  Good balance of latency and reliability
-  Sensible defaults based on eRPC research
-  Works well for most use cases

---

#### Custom (Application-Specific)

```toml
[scoring]
enabled = true
window_seconds = 600      # Shorter window (10 min) for faster adaptation
min_samples = 5           # Lower threshold for quicker scoring
max_block_lag = 10        # More tolerant of lag
top_n = 5                 # Consider more upstreams

[scoring.weights]
latency = 6.0
error_rate = 5.0
throttle_rate = 4.0
block_head_lag = 3.0
total_requests = 2.0      # Slightly more load balancing
```

---

## Performance Considerations

### Latency Impact

**Overhead Per Request**:
```
Scoring disabled:
└─ 0 overhead (simple round-robin or random selection)

Scoring enabled, hot path:
├─ record_success(): ~2-3μs (atomic increments + latency recording)
├─ calculate_score(): ~5-10μs (only when ranking needed)
└─ Total: ~5-13μs per request (negligible compared to network latency)
```

**Ranking Overhead**:
```
get_ranked_upstreams() with 5 upstreams:
├─ Calculate 5 scores: 5 × 10μs = 50μs
├─ Sort 5 scores: ~5μs
└─ Total: ~55μs (called once per request typically)
```

**Best Practice**: Cache ranked list for short duration (e.g., 1 second) to amortize ranking cost across multiple requests.

### Memory Usage

**Per ScoringEngine**:
```
ScoringEngine:
├─ config: ArcSwap<ScoringConfig>
│  └─ ~250 bytes (lock-free)
│
├─ metrics: DashMap<String, Arc<UpstreamMetrics>>
│  └─ Per UpstreamMetrics:
│     ├─ latency_tracker: ~8KB (1000 samples × 8 bytes)
│     ├─ Atomics: 6 × 8 bytes = 48 bytes
│     ├─ DashMap overhead: ~50 bytes per entry
│     └─ Total: ~8100 bytes per upstream
│
└─ chain_state: Arc<ChainState>
   └─ Shared reference (same instance as other components)
   └─ 8 bytes

Total for 5 upstreams: ~41KB
Total for 100 upstreams: ~820KB
```

**Conclusion**: Memory usage is minimal, not a concern even with many upstreams.

### Window Reset Performance

**Automatic Reset**:
- Occurs when `now - window_start > window_duration`
- Resets counters: O(1) (atomic stores)
- Keeps latency samples: Preserved (have own internal window)
- Overhead: ~1μs per request when reset occurs

**Note**: Window reset is per-upstream, not global, so resets are staggered.

## Public API

### Constructor

#### `new(config: ScoringConfig) -> Self`

**Location**: Lines 342-351 in scoring.rs

Creates a new scoring engine with the given configuration.

**Parameters**:
- `config`: Scoring configuration with weights and thresholds

**Returns**: New `ScoringEngine` instance

**Usage**:
```rust
let config = ScoringConfig {
    enabled: true,
    window_seconds: 1800,
    min_samples: 10,
    weights: ScoringWeights::default(),
    max_block_lag: 5,
    top_n: 3,
};

let engine = ScoringEngine::new(config);
```

### Configuration Management

#### `update_config(&self, config: ScoringConfig)`

**Location**: Lines 353-357 in scoring.rs

Updates the scoring configuration at runtime.

**Parameters**:
- `config`: New scoring configuration

**Usage**:
```rust
let new_config = ScoringConfig {
    weights: ScoringWeights {
        latency: 10.0,  // Increase latency importance
        ..Default::default()
    },
    ..engine.get_config().await
};

engine.update_config(new_config).await;
```

---

#### `get_config() -> ScoringConfig`

**Location**: Lines 359-362 in scoring.rs

Returns the current configuration.

**Usage**:
```rust
let config = engine.get_config().await;
println!("Window: {}s, Min samples: {}", config.window_seconds, config.min_samples);
```

---

#### `is_enabled() -> bool`

**Location**: Lines 364-367 in scoring.rs

Checks if scoring is currently enabled.

**Usage**:
```rust
if engine.is_enabled().await {
    let ranked = engine.get_ranked_upstreams().await;
    // Use scoring-based selection
} else {
    // Use round-robin or random
}
```

### Metric Recording

#### `record_success(&self, upstream_name: &str, latency_ms: u64)`

**Location**: Lines 369-379 in scoring.rs

Records a successful request with its latency.

**Parameters**:
- `upstream_name`: Name of the upstream endpoint
- `latency_ms`: Request latency in milliseconds

**Usage**:
```rust
let start = Instant::now();
let response = upstream.send_request(&request).await?;
let latency_ms = start.elapsed().as_millis() as u64;

scoring_engine.record_success("alchemy-mainnet", latency_ms).await;
```

**Implementation**:
- Increments success_count and total_requests
- Records latency in LatencyTracker
- Automatically resets window if expired

---

#### `record_error(&self, upstream_name: &str)`

**Location**: Lines 381-391 in scoring.rs

Records a non-throttle error (connection failure, timeout, invalid response, etc.).

**Parameters**:
- `upstream_name`: Name of the upstream endpoint

**Usage**:
```rust
match upstream.send_request(&request).await {
    Ok(response) => {
        scoring_engine.record_success("infura", latency_ms).await;
    }
    Err(UpstreamError::Timeout) => {
        scoring_engine.record_error("infura").await;
    }
    Err(UpstreamError::RateLimited) => {
        scoring_engine.record_throttle("infura").await;  // Use record_throttle for 429s
    }
    Err(e) => {
        scoring_engine.record_error("infura").await;
    }
}
```

**Implementation**:
- Increments error_count and total_requests
- Does NOT affect latency tracking
- Lowers error_rate_factor in score calculation

---

#### `record_throttle(&self, upstream_name: &str)`

**Location**: Lines 393-403 in scoring.rs

Records a throttle response (HTTP 429 or rate limit error).

**Parameters**:
- `upstream_name`: Name of the upstream endpoint

**Usage**:
```rust
if response.status() == StatusCode::TOO_MANY_REQUESTS {
    scoring_engine.record_throttle("quicknode").await;
}
```

**Why Separate from Errors?**:
- Throttles are handled differently in scoring (exponential penalty)
- Helps distinguish between reliability issues (errors) and quota issues (throttles)
- Allows separate monitoring and alerting

---

#### `record_block_number(&self, upstream_name: &str, block: u64)`

**Location**: Lines 405-424 in scoring.rs

Records a block number seen from an upstream and updates the chain tip.

**Parameters**:
- `upstream_name`: Name of the upstream endpoint
- `block`: Block number

**Usage**:
```rust
// Extract block number from response
if let Some(block_number) = extract_block_number(&response) {
    scoring_engine.record_block_number("alchemy", block_number).await;
}
```

**Implementation**:
- Updates upstream's last_block_seen (only if higher)
- Updates global chain_tip (only if higher)
- Timestamp recorded for staleness detection

---

### Score Calculation

#### `calculate_score(&self, upstream_name: &str) -> Option<UpstreamScore>`

**Location**: Lines 432-488 in scoring.rs

Calculates the composite score for a specific upstream.

**Parameters**:
- `upstream_name`: Name of the upstream to score

**Returns**:
- `Some(UpstreamScore)` if sufficient samples exist
- `None` if insufficient data (< min_samples)

**Usage**:
```rust
if let Some(score) = engine.calculate_score("alchemy").await {
    println!("Alchemy score: {:.2}", score.composite_score);
    println!("  Latency P90: {}ms (factor: {:.3})", score.latency_p90_ms.unwrap(), score.latency_factor);
    println!("  Error rate: {:.1}% (factor: {:.3})", score.error_rate * 100.0, score.error_rate_factor);
    println!("  Throttle rate: {:.1}% (factor: {:.3})", score.throttle_rate * 100.0, score.throttle_factor);
    println!("  Block lag: {} (factor: {:.3})", score.block_head_lag, score.block_lag_factor);
}
```

**Score Composition**:
```
composite_score = latency_factor^8.0
                × error_rate_factor^4.0
                × throttle_factor^3.0
                × block_lag_factor^2.0
                × load_factor^1.0
                × 100
```

---

#### `get_ranked_upstreams() -> Vec<(String, f64)>`

**Location**: Lines 547-561 in scoring.rs

**PRIMARY METHOD** - Returns all upstreams sorted by score (best first).

**Returns**: Vector of (upstream_name, composite_score) tuples, sorted descending

**Usage**:
```rust
let ranked = engine.get_ranked_upstreams().await;

for (name, score) in &ranked {
    println!("{}: {:.2}", name, score);
}

// Use best upstream
if let Some((best, score)) = ranked.first() {
    println!("Best upstream: {} (score: {:.2})", best, score);
}
```

**Example Output**:
```
alchemy-mainnet: 87.5
infura-mainnet: 82.3
quicknode-mainnet: 76.8
public-rpc-1: 45.2
public-rpc-2: 12.5
```

**Implementation**:
- Calculates score for all upstreams with sufficient samples
- Skips upstreams with insufficient data
- Sorts by score descending
- O(n log n) where n = number of upstreams

---

#### `get_score(&self, upstream_name: &str) -> Option<UpstreamScore>`

**Location**: Lines 563-566 in scoring.rs

Gets the score for a specific upstream (alias for calculate_score).

---

#### `get_metrics(&self, upstream_name: &str) -> Option<(f64, f64, u64, u64, Option<u64>)>`

**Location**: Lines 568-584 in scoring.rs

Returns raw metrics for debugging and monitoring.

**Returns**: Tuple of (error_rate, throttle_rate, total_requests, last_block_seen, avg_latency)

**Usage**:
```rust
if let Some((err_rate, throttle_rate, total, block, avg_lat)) = engine.get_metrics("alchemy").await {
    println!("Error rate: {:.2}%", err_rate * 100.0);
    println!("Throttle rate: {:.2}%", throttle_rate * 100.0);
    println!("Total requests: {}", total);
    println!("Last block: {}", block);
    println!("Avg latency: {}ms", avg_lat.unwrap_or(0));
}
```

---

### Chain Tip Tracking

#### `chain_tip() -> u64`

**Location**: Lines 426-430 in scoring.rs

Returns the highest block number seen across all upstreams.

**Usage**:
```rust
let tip = engine.chain_tip();
println!("Chain tip: {}", tip);

// Check if upstream is behind
let upstream_block = engine.get_metrics("infura").await.unwrap().3;
let lag = tip - upstream_block;
if lag > 5 {
    warn!("Infura is {} blocks behind!", lag);
}
```

---

### Utility Methods

#### `clear(&self)`

**Location**: Lines 586-590 in scoring.rs

Clears all metrics (useful for testing).

**Usage**:
```rust
engine.clear().await;
```

## Result Types

### UpstreamScore

**Location**: Lines 304-332 in scoring.rs

```rust
pub struct UpstreamScore {
    /// Name of the upstream
    pub upstream_name: String,

    /// Final composite score (0.0 = worst, 100.0 = best)
    pub composite_score: f64,

    /// Individual factor scores (0.0 - 1.0 each)
    pub latency_factor: f64,
    pub error_rate_factor: f64,
    pub throttle_factor: f64,
    pub block_lag_factor: f64,
    pub load_factor: f64,

    /// Raw metrics for debugging
    pub latency_p90_ms: Option<u64>,
    pub latency_p99_ms: Option<u64>,
    pub error_rate: f64,
    pub throttle_rate: f64,
    pub block_head_lag: u64,
    pub total_requests: u64,

    /// When this score was calculated
    pub last_updated: Instant,
}
```

**Usage**:
```rust
let score = engine.calculate_score("alchemy").await.unwrap();

// Decision making based on score
if score.composite_score > 80.0 {
    println!("Excellent upstream");
} else if score.composite_score > 60.0 {
    println!("Good upstream");
} else if score.composite_score > 40.0 {
    println!("Marginal upstream");
} else {
    println!("Poor upstream - consider removing");
}

// Debugging specific factors
if score.latency_factor < 0.5 {
    println!("High latency detected: P90={}ms", score.latency_p90_ms.unwrap());
}

if score.throttle_factor < 0.8 {
    println!("Throttling detected: {:.1}%", score.throttle_rate * 100.0);
}
```

## Integration with Other Components

### HedgeExecutor Integration

```rust
// Scoring determines upstream order for hedging
let ranked = scoring_engine.get_ranked_upstreams().await;

// Convert to UpstreamEndpoint references (best first)
let upstreams: Vec<Arc<UpstreamEndpoint>> = ranked
    .iter()
    .filter_map(|(name, _score)| upstream_manager.get_upstream(name))
    .collect();

// Hedging uses this order:
// - Primary: Best scored upstream
// - Hedge 1: Second best
// - Hedge 2: Third best
hedge_executor.execute_hedged(&request, upstreams).await
```

### ConsensusEngine Integration

```rust
// Scoring can help with consensus dispute resolution
let ranked = scoring_engine.get_ranked_upstreams().await;

// When consensus fails, prefer highest-scored upstream
if dispute_behavior == DisputeBehavior::PreferHighestScore {
    let best_upstream = ranked.first().unwrap().0;
    // Use response from best_upstream
}

// Also, disagreeing upstreams are penalized via record_error()
for disagreeing_upstream in &consensus_result.disagreeing_upstreams {
    scoring_engine.record_error(disagreeing_upstream).await;
}
```

### UpstreamManager Integration

```rust
// In UpstreamManager
pub async fn select_upstream(&self) -> Arc<UpstreamEndpoint> {
    if self.scoring_engine.is_enabled().await {
        // Use scoring-based selection
        let ranked = self.scoring_engine.get_ranked_upstreams().await;

        if let Some((name, _score)) = ranked.first() {
            return self.upstreams.get(name).unwrap().clone();
        }
    }

    // Fallback to round-robin
    self.round_robin_select()
}
```

## Usage Examples

### Basic Score Calculation

```rust
use prism_core::upstream::{ScoringEngine, ScoringConfig};

// Create scoring engine
let config = ScoringConfig {
    enabled: true,
    window_seconds: 1800,
    min_samples: 10,
    ..Default::default()
};
let engine = ScoringEngine::new(config);

// Record some requests
for _ in 0..20 {
    engine.record_success("alchemy", 100).await;
    engine.record_block_number("alchemy", 18000000).await;
}

// Calculate score
if let Some(score) = engine.calculate_score("alchemy").await {
    println!("Alchemy composite score: {:.2}", score.composite_score);
}
```

### Comparing Upstreams

```rust
let upstreams = vec!["alchemy", "infura", "quicknode"];

for name in &upstreams {
    if let Some(score) = engine.calculate_score(name).await {
        println!("{:12} | Score: {:5.2} | P90: {:4}ms | Errors: {:4.1}% | Throttles: {:4.1}%",
            name,
            score.composite_score,
            score.latency_p90_ms.unwrap_or(0),
            score.error_rate * 100.0,
            score.throttle_rate * 100.0
        );
    }
}
```

**Output**:
```
alchemy      | Score: 87.50 | P90:  120ms | Errors:  1.2% | Throttles:  0.5%
infura       | Score: 82.30 | P90:  150ms | Errors:  2.1% | Throttles:  1.2%
quicknode    | Score: 76.80 | P90:  180ms | Errors:  3.5% | Throttles:  2.8%
```

### Weighted Selection

```rust
// Select upstream with weighted probability based on scores
let ranked = engine.get_ranked_upstreams().await;
let total_score: f64 = ranked.iter().map(|(_, score)| score).sum();

let mut rng = rand::thread_rng();
let mut target = rng.gen::<f64>() * total_score;

for (name, score) in &ranked {
    target -= score;
    if target <= 0.0 {
        println!("Selected: {}", name);
        break;
    }
}
```

### Monitoring Dashboard

```rust
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));

    loop {
        interval.tick().await;

        let ranked = engine.get_ranked_upstreams().await;

        println!("\n=== Upstream Scores ===");
        for (i, (name, score)) in ranked.iter().enumerate() {
            let details = engine.get_score(name).await.unwrap();

            println!("{}. {} ({:.2})", i + 1, name, score);
            println!("   L:{:.2} E:{:.2} T:{:.2} B:{:.2}",
                details.latency_factor,
                details.error_rate_factor,
                details.throttle_factor,
                details.block_lag_factor
            );
        }
    }
});
```

## Related Components

### LatencyTracker (Hedging Module)
**File**: `crates/prism-core/src/upstream/hedging.rs`
**Lines**: 95-195

The scoring module reuses the `LatencyTracker` from the hedging module for efficient percentile calculation:

```rust
use super::hedging::LatencyTracker;

pub struct UpstreamMetrics {
    latency_tracker: RwLock<LatencyTracker>,
    // ... other fields
}
```

This sharing reduces code duplication and ensures consistent latency tracking across both systems.

## Testing

### Unit Tests

**Location**: Lines 596-1149 in scoring.rs

**Test Coverage**:

1. **test_scoring_config_defaults** (lines 601-612)
2. **test_upstream_metrics_basic** (lines 624-636)
3. **test_upstream_metrics_error_rate** (lines 638-650)
4. **test_upstream_metrics_throttle_rate** (lines 652-665)
5. **test_scoring_engine_calculate_score** (lines 721-739)
6. **test_scoring_engine_ranked_upstreams** (lines 755-778)
7. **test_latency_factor_calculation** (lines 854-876)
8. **test_throttle_factor_calculation** (lines 878-896)
9. **test_composite_score_calculation** (lines 1114-1148)

## Performance Monitoring

### Key Metrics

```rust
// Periodic score monitoring
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(300)); // Every 5 min

    loop {
        interval.tick().await;

        for upstream in &upstreams {
            if let Some(score) = engine.calculate_score(upstream).await {
                // Alert if score drops significantly
                if score.composite_score < 50.0 {
                    warn!(
                        upstream = upstream,
                        score = score.composite_score,
                        "low upstream score detected"
                    );
                }

                // Track score trends
                metrics::gauge!("upstream_score", score.composite_score, "upstream" => upstream.clone());
                metrics::gauge!("upstream_latency_p90", score.latency_p90_ms.unwrap_or(0) as f64, "upstream" => upstream.clone());
                metrics::gauge!("upstream_error_rate", score.error_rate, "upstream" => upstream.clone());
            }
        }
    }
});
```

---

**Last Updated**: November 2025
**Module Version**: 1.0
**Implementation Status**: Production Ready
