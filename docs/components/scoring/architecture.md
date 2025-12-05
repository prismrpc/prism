# Scoring Architecture

**Module**: `crates/prism-core/src/upstream/scoring.rs`

## Overview

This document provides a deep technical dive into the multi-factor scoring architecture in Prism. It covers metric collection, score calculation algorithms, window management, and integration with other upstream selection components.

## System Architecture

### Component Hierarchy

```
┌─────────────────────────────────────────────────────────────────┐
│                    UpstreamManager                               │
│                (Upstream Selection Logic)                        │
└────────────┬─────────────────────────────────────────────────────┘
             │
             ├─ Request routing decision
             │
             ▼
             ┌──────────────────────────────────────────────────────┐
             │              ScoringEngine                           │
             │                                                      │
             │  ┌──────────────────────────────────────────────┐  │
             │  │  Configuration (ArcSwap<ScoringConfig>)      │  │
             │  │  - weights, thresholds, window_seconds       │  │
             │  │  - Lock-free reads via ArcSwap                │  │
             │  └──────────────────────────────────────────────┘  │
             │                                                      │
             │  ┌──────────────────────────────────────────────┐  │
             │  │  Metrics (DashMap<String, Arc<UpstreamMetrics>>)│ │
             │  │  - Per-upstream performance tracking         │  │
             │  │  - Lock-free concurrent access               │  │
             │  └──────────────────────────────────────────────┘  │
             │                                                      │
             │  ┌──────────────────────────────────────────────┐  │
             │  │  Chain State (Arc<ChainState>)                │  │
             │  │  - Shared chain state for unified tip tracking│  │
             │  │  - Secondary writer for block number updates  │  │
             │  └──────────────────────────────────────────────┘  │
             │                                                      │
             │  Metric Recording                                   │
             │    ├─ record_success(name, latency_ms)             │
             │    ├─ record_error(name)                           │
             │    ├─ record_throttle(name)                        │
             │    └─ record_block_number(name, block)             │
             │                                                      │
             │  Score Calculation                                  │
             │    ├─ calculate_latency_factor()                   │
             │    ├─ calculate_error_rate_factor()                │
             │    ├─ calculate_throttle_factor()                  │
             │    ├─ calculate_block_lag_factor()                 │
             │    ├─ calculate_load_factor()                      │
             │    └─ composite = product of factors ^ weights     │
             │                                                      │
             └──────────────┬───────────────────────────────────────┘
                            │
                            └─ get_ranked_upstreams() → Sorted list
```

### Data Flow

```
1. REQUEST EXECUTION
   │
   ├─ Start timer
   ├─ upstream.send_request(request)
   └─ Record outcome:
      ├─ Success → record_success(name, latency_ms)
      ├─ Error → record_error(name)
      └─ Throttle → record_throttle(name)
   │
   ▼
2. METRIC RECORDING
   │
   ├─ Lock-free DashMap access (per-shard locking)
   ├─ Get or create UpstreamMetrics for this upstream
   ├─ Check if window expired → reset counters if needed (lock-free atomics)
   ├─ Update counters (atomics)
   │  ├─ total_requests++ (AtomicU64)
   │  ├─ success_count++ OR error_count++ OR throttle_count++ (AtomicU64)
   │  └─ latency_tracker.record(latency_ms) if success
   └─ No lock release needed (lock-free operations)
   │
   ▼
3. SCORE CALCULATION (on demand)
   │
   ├─ Lock-free DashMap read access
   ├─ Get UpstreamMetrics for requested upstream
   ├─ Check min_samples requirement
   │  └─ If insufficient: return None
   ├─ Extract raw metrics:
   │  ├─ p90, p99 from latency_tracker.percentile()
   │  ├─ error_rate = error_count / total_requests
   │  ├─ throttle_rate = throttle_count / total_requests
   │  ├─ block_lag = chain_tip - last_block_seen
   │  └─ total_requests
   │
   ▼
4. FACTOR CALCULATION
   │
   ├─ latency_factor = 1.0 - (log2(p90) / 14.0)
   │  └─ Clamped to [0.1, 1.0]
   │
   ├─ error_rate_factor = 1.0 - error_rate
   │  └─ Clamped to [0.0, 1.0]
   │
   ├─ throttle_factor = exp(-3.0 * throttle_rate)
   │  └─ Clamped to [0.0, 1.0]
   │
   ├─ block_lag_factor = 1.0 - (block_lag / max_block_lag)
   │  └─ Clamped to [0.0, 1.0]
   │
   └─ load_factor = 1.0 (placeholder)
   │
   ▼
5. COMPOSITE SCORE
   │
   └─ composite = latency_factor ^ weights.latency
                × error_rate_factor ^ weights.error_rate
                × throttle_factor ^ weights.throttle_rate
                × block_lag_factor ^ weights.block_head_lag
                × load_factor ^ weights.total_requests
                × 100
   │
   ▼
6. RETURN SCORE
   │
   └─ UpstreamScore {
       composite_score,
       individual factors,
       raw metrics,
       timestamp
     }
```

## Core Components

### ScoringEngine

**Location**: Lines 334-590 in scoring.rs

**Structure**:
```rust
pub struct ScoringEngine {
    config: ArcSwap<ScoringConfig>,
    metrics: DashMap<String, Arc<UpstreamMetrics>>,
    chain_state: Arc<ChainState>,
}
```

**Design Decisions**:

1. **ArcSwap<ScoringConfig>**: Lock-free configuration access
   - **Why**: Config reads happen on every request (hot path)
   - **Lock Strategy**: Lock-free reads via ArcSwap, atomic updates
   - **Benefit**: Zero contention on config reads

2. **DashMap<String, Arc<UpstreamMetrics>>**: Lock-free concurrent metric storage
   - **Why**: Multiple requests may record metrics concurrently
   - **Lock Strategy**: Per-shard locking (much better than single RwLock)
   - **Benefit**: High concurrency with minimal contention

3. **Arc<ChainState>**: Shared chain state for unified tip tracking
   - **Why**: Same ChainState instance used by CacheManager, ReorgManager, etc.
   - **Benefit**: All components see identical chain tip via lock-free reads

2. **AtomicU64 for Chain Tip**: Lock-free updates
   - **Why**: Frequently updated (every block number recording)
   - **Benefit**: No lock contention
   - **Trade-off**: Only supports simple operations (load, store, compare_exchange)

3. **Stateless Score Calculation**: No cached scores
   - **Why**: Scores must reflect latest metrics
   - **Alternative**: Cache scores with TTL (future optimization)

### UpstreamMetrics

**Location**: Lines 147-298 in scoring.rs

**Structure**:
```rust
pub struct UpstreamMetrics {
    latency_tracker: RwLock<LatencyTracker>,  // Shared with hedging
    total_requests: AtomicU64,
    error_count: AtomicU64,
    throttle_count: AtomicU64,
    success_count: AtomicU64,
    last_block_seen: AtomicU64,
    last_block_time: RwLock<Option<Instant>>,
    window_start: RwLock<Instant>,
    window_duration: Duration,
}
```

**Design Decisions**:

1. **Reuse LatencyTracker from Hedging**: Code sharing
   - **Benefit**: Single source of truth for latency data
   - **Consistency**: Both systems see same percentiles

2. **Atomics for Counters**: Lock-free increments
   - **Why**: High-frequency updates (every request)
   - **Performance**: Atomic increment is ~10ns vs mutex ~100ns

3. **Separate Error and Throttle Counts**: Different handling
   - **Why**: Throttles penalized differently (exponential vs linear)
   - **Benefit**: Better diagnostics and separate monitoring

4. **Window-Based Reset**: Automatic staleness prevention
   - **Why**: Ensures scores reflect recent performance
   - **Implementation**: Check on every metric recording

### Window Management

**Location**: Lines 285-297 in scoring.rs (UpstreamMetrics::maybe_reset_window)

**Algorithm**:
```rust
async fn maybe_reset_window(&self) {
    let now = Instant::now();
    let window_start = *self.window_start.read().await;

    if now.duration_since(window_start) > self.window_duration {
        // Reset counters (atomics)
        self.total_requests.store(0, Ordering::Relaxed);
        self.error_count.store(0, Ordering::Relaxed);
        self.throttle_count.store(0, Ordering::Relaxed);
        self.success_count.store(0, Ordering::Relaxed);

        // Update window start
        *self.window_start.write().await = now;

        // Note: Latency samples NOT reset (have their own window)
    }
}
```

**Key Points**:
- **Per-Upstream**: Each upstream has its own window
- **Staggered Resets**: Not all upstreams reset at once
- **Latency Preserved**: Latency tracker maintains separate 1000-sample window

**Why Not Global Reset?**:
- Avoids sudden score discontinuities
- Upstreams added at different times naturally stagger
- Simplifies concurrency (no global coordination)

## Score Calculation Algorithms

### Latency Factor (Logarithmic Scaling)

**Location**: Lines 490-506 in scoring.rs

```rust
fn calculate_latency_factor(&self, p90_ms: u64, _config: &ScoringConfig) -> f64 {
    if p90_ms == 0 {
        return 1.0;
    }

    // Logarithmic scaling: lower latency = higher factor
    // Formula: 1.0 - (log2(latency_ms) / 14.0)
    // 14.0 = log2(16384ms) as "worst case" baseline
    let normalized = (p90_ms as f64).log2() / 14.0;
    (1.0 - normalized).clamp(0.1, 1.0)
}
```

**Why Logarithmic?**:
- Diminishing returns: 100ms→200ms difference more significant than 1000ms→1100ms
- Matches human perception of latency
- Prevents extreme penalties for slow but functional upstreams

**Examples**:
```
   50ms: log2(50) / 14 = 0.40 → factor = 0.60
  100ms: log2(100) / 14 = 0.47 → factor = 0.53
  200ms: log2(200) / 14 = 0.54 → factor = 0.46
  500ms: log2(500) / 14 = 0.64 → factor = 0.36
 1000ms: log2(1000) / 14 = 0.71 → factor = 0.29
10000ms: log2(10000) / 14 = 0.95 → factor = 0.10 (clamped)
```

**Baseline Rationale** (14.0):
- 2^14 = 16384ms ≈ 16 seconds
- Upstreams slower than 16s receive minimum factor (0.1)

### Error Rate Factor (Linear Scaling)

**Location**: Lines 508-513 in scoring.rs

```rust
fn calculate_error_rate_factor(&self, error_rate: f64, _config: &ScoringConfig) -> f64 {
    // Simple: 1.0 - error_rate
    (1.0 - error_rate).clamp(0.0, 1.0)
}
```

**Why Linear?**:
- Intuitive: 10% errors = 10% score reduction
- Fair: Errors should have proportional impact

**Examples**:
```
 0% errors → 1.0 (perfect)
 5% errors → 0.95
10% errors → 0.90
20% errors → 0.80
50% errors → 0.50
```

### Throttle Factor (Exponential Scaling)

**Location**: Lines 515-521 in scoring.rs

```rust
fn calculate_throttle_factor(&self, throttle_rate: f64, _config: &ScoringConfig) -> f64 {
    // Exponential decay: throttles are worse than errors
    // Formula: exp(-3.0 * throttle_rate)
    (-3.0 * throttle_rate).exp().clamp(0.0, 1.0)
}
```

**Why Exponential?**:
- Throttling indicates quota exhaustion (severe)
- Should avoid throttled upstreams more aggressively than error-prone ones
- Exponential penalty discourages using near-quota upstreams

**Examples**:
```
 0% throttles → exp(0) = 1.00 (perfect)
 5% throttles → exp(-0.15) = 0.86
10% throttles → exp(-0.30) = 0.74
20% throttles → exp(-0.60) = 0.55
50% throttles → exp(-1.50) = 0.22
```

**Comparison with Linear**:
```
Throttle Rate | Linear | Exponential
--------------|--------|------------
      5%      |  0.95  |   0.86    (10% more penalty)
     10%      |  0.90  |   0.74    (18% more penalty)
     20%      |  0.80  |   0.55    (31% more penalty)
```

### Block Lag Factor (Linear Penalty)

**Location**: Lines 523-534 in scoring.rs

```rust
fn calculate_block_lag_factor(&self, block_lag: u64, config: &ScoringConfig) -> f64 {
    if block_lag == 0 {
        return 1.0;
    }

    // Linear penalty up to max_block_lag
    let penalty = (block_lag as f64 / config.max_block_lag as f64).min(1.0);
    (1.0 - penalty).clamp(0.0, 1.0)
}
```

**Why Linear?**:
- Each block of lag is equally bad (no diminishing returns)
- Simple threshold-based penalty

**Examples** (max_block_lag = 5):
```
0 blocks behind → 1.0 (at tip)
1 block behind  → 0.8
2 blocks behind → 0.6
3 blocks behind → 0.4
5 blocks behind → 0.0 (maximum penalty)
10 blocks behind → 0.0 (capped)
```

### Composite Score Calculation

**Location**: Lines 461-470 in scoring.rs

```rust
let composite = latency_factor.powf(config.weights.latency)
    * error_rate_factor.powf(config.weights.error_rate)
    * throttle_factor.powf(config.weights.throttle_rate)
    * block_lag_factor.powf(config.weights.block_head_lag)
    * load_factor.powf(config.weights.total_requests);

let composite_score = composite * 100.0;
```

**Why Multiplicative?**:
- **Captures Interactions**: A great upstream in one dimension can't fully compensate for terrible performance in another
- **Weighted Importance**: Exponents make high-weighted factors more influential
- **Intuitive**: 0.9^8 = 0.43 (latency weight 8) vs 0.9^1 = 0.90 (load weight 1)

**Example Calculation**:
```
Factors:
- latency_factor = 0.7 (mediocre, 500ms P90)
- error_rate_factor = 0.95 (good, 5% errors)
- throttle_factor = 0.95 (good, 5% throttles)
- block_lag_factor = 0.8 (OK, 1 block behind)
- load_factor = 1.0 (neutral)

Weights:
- latency: 8.0
- error_rate: 4.0
- throttle_rate: 3.0
- block_head_lag: 2.0
- total_requests: 1.0

Composite:
= 0.7^8.0 × 0.95^4.0 × 0.95^3.0 × 0.8^2.0 × 1.0^1.0
= 0.0576 × 0.8145 × 0.8574 × 0.64 × 1.0
= 0.0257

Score:
= 0.0257 × 100 = 2.57

Interpretation: Very low score due to poor latency (factor 0.7 raised to 8th power)
```

**Impact of Weights**:
```
If latency_factor = 0.9:
- weight 1.0: 0.9^1 = 0.90
- weight 4.0: 0.9^4 = 0.66
- weight 8.0: 0.9^8 = 0.43

A 10% deficiency (0.9 factor) becomes:
- 10% penalty with weight 1
- 34% penalty with weight 4
- 57% penalty with weight 8
```

## Metric Recording Flow

### record_success()

**Location**: Lines 369-379 in scoring.rs

```
record_success(upstream_name, latency_ms)
│
├─ Acquire config read lock
├─ Acquire metrics write lock
│
├─ Get or create UpstreamMetrics
│  └─ New upstream → create with window_duration=config.window_seconds
│
├─ metrics.record_success(latency_ms)
│  ├─ maybe_reset_window()
│  │  └─ If window expired: reset counters
│  │
│  ├─ total_requests.fetch_add(1)
│  ├─ success_count.fetch_add(1)
│  └─ latency_tracker.write().record(latency_ms)
│
└─ Release locks
```

**Concurrency**:
- Multiple threads can record to different upstreams concurrently
- Recording to same upstream uses DashMap per-shard locking (minimal contention)
- Atomic operations within metrics allow lock-free reading

### record_block_number()

**Location**: Lines 405-424 in scoring.rs

```
record_block_number(upstream_name, block)
│
├─ Update global chain_tip (atomic)
│  ├─ current_tip = self.chain_tip.load()
│  └─ if block > current_tip:
│     └─ self.chain_tip.store(block)
│
├─ Acquire config read lock
├─ Acquire metrics write lock
│
├─ Get or create UpstreamMetrics
│
├─ metrics.record_block_number(block)
│  ├─ current = last_block_seen.load()
│  └─ if block > current:
│     ├─ last_block_seen.store(block)
│     └─ last_block_time = Some(now)
│
└─ Release locks
```

**Lock-Free Chain Tip**:
- Multiple threads can update concurrently
- No lock contention even under high load
- Eventual consistency is acceptable (block numbers only increase)

## Integration Points

### HedgeExecutor Integration

**Shared LatencyTracker**:
```rust
// Both systems use same latency data
use super::hedging::LatencyTracker;

// In UpstreamMetrics
latency_tracker: RwLock<LatencyTracker>
```

**Benefit**: Consistent latency percentiles across hedging and scoring.

**Flow**:
```
Request → Execute (hedging) → Record latency
                                    ↓
                              ScoringEngine.record_success()
                                    ↓
                              LatencyTracker.record()
                                    ↓
                              Both hedging and scoring see updated percentiles
```

### UpstreamManager Integration

**Selection Pattern**:
```rust
// In UpstreamManager
pub async fn select_upstream(&self) -> Arc<UpstreamEndpoint> {
    if !self.scoring_engine.is_enabled().await {
        return self.round_robin_select();
    }

    let ranked = self.scoring_engine.get_ranked_upstreams().await;

    if let Some((best_name, _score)) = ranked.first() {
        if let Some(upstream) = self.upstreams.get(best_name) {
            return upstream.clone();
        }
    }

    // Fallback
    self.round_robin_select()
}
```

**Caching Strategy** (recommended):
```rust
// Cache ranked list for 1 second
pub struct UpstreamManager {
    scoring_cache: Arc<RwLock<Option<(Instant, Vec<(String, f64)>)>>>,
}

pub async fn get_ranked_upstreams_cached(&self) -> Vec<(String, f64)> {
    let mut cache = self.scoring_cache.write().await;

    if let Some((timestamp, ranked)) = &*cache {
        if timestamp.elapsed() < Duration::from_secs(1) {
            return ranked.clone();
        }
    }

    // Cache miss or expired
    let ranked = self.scoring_engine.get_ranked_upstreams().await;
    *cache = Some((Instant::now(), ranked.clone()));
    ranked
}
```

**Benefit**: Amortizes ranking cost (~55μs for 5 upstreams) across many requests.

## Performance Characteristics

### Metric Recording

**Per Request**:
```
record_success() with hot cache:
├─ DashMap lookup: ~10ns (lock-free on hot path)
├─ Window check: ~5ns
├─ Atomic increment (×2): ~20ns
├─ Latency recording: ~2μs
└─ Total: ~2-3μs

record_error():
├─ DashMap lookup: ~10ns (lock-free on hot path)
├─ Window check: ~5ns
├─ Atomic increment (×2): ~20ns
└─ Total: ~35-50ns

record_block_number():
├─ Atomic chain_tip update: ~10ns
├─ DashMap lookup: ~10ns (lock-free on hot path)
├─ Atomic last_block update: ~10ns
└─ Total: ~30-40ns
```

### Score Calculation

**Single Upstream**:
```
calculate_score():
├─ Get metrics: ~20ns (DashMap lookup)
├─ Check min_samples: ~5ns
├─ Latency percentile: ~60μs (sorts 1000 samples)
├─ Calculate factors (×5): ~50ns
├─ Composite calculation: ~30ns
└─ Total: ~60-65μs
```

**Ranked List** (5 upstreams):
```
get_ranked_upstreams():
├─ Calculate 5 scores: 5 × 65μs = 325μs
├─ Sort 5 scores: ~5μs
└─ Total: ~330μs

Amortized over 1000 requests (1s cache):
330μs / 1000 = 0.33μs per request
```

### Memory Footprint

**Per Upstream**:
```
UpstreamMetrics:
├─ LatencyTracker: 8KB (1000 × u64)
├─ Atomics (×6): 48 bytes
├─ RwLock overhead: ~100 bytes
└─ Total: ~8.2KB

5 upstreams: ~41KB
100 upstreams: ~820KB
```

**Conclusion**: Negligible memory usage.

## Testing Strategy

**Unit Tests**: Lines 596-1149 in scoring.rs

**Coverage**:
1.  Configuration defaults
2.  Metric recording (success, error, throttle, block)
3.  Rate calculations
4.  Factor calculations (latency, error, throttle, block lag)
5.  Composite score calculation
6.  Ranking logic
7.  Window reset behavior
8.  Concurrent access

**Missing**:
1.  Score stability over time
2.  Cache hit rate impact on effective overhead
3.  Integration with UpstreamManager

## Future Enhancements

### 1. Load Factor Implementation

**Current**: Always returns 1.0
**Future**: Prefer upstreams with fewer requests

```rust
fn calculate_load_factor(&self, total_requests: u64, all_metrics: &[&UpstreamMetrics]) -> f64 {
    // Calculate average load across all upstreams
    let avg_load: u64 = all_metrics.iter()
        .map(|m| m.total_requests())
        .sum::<u64>() / all_metrics.len() as u64;

    // Penalize upstreams above average
    if total_requests > avg_load {
        let excess = total_requests - avg_load;
        let penalty = (excess as f64 / avg_load as f64).min(0.5);
        1.0 - penalty
    } else {
        1.0
    }
}
```

### 2. Score Caching with TTL

**Current**: Calculate on every call
**Future**: Cache scores for short duration

```rust
struct CachedScore {
    score: UpstreamScore,
    calculated_at: Instant,
    ttl: Duration,
}
```

### 3. Adaptive Weights

**Current**: Static weights from config
**Future**: Learn optimal weights from outcomes

```rust
// Increase weight for factors with highest variance
if latency_variance > error_variance {
    weights.latency += 0.1;
    weights.error_rate -= 0.1;
}
```

### 4. Historical Score Tracking

**Current**: Single window, no history
**Future**: Track score over time for trending

```rust
struct ScoreHistory {
    scores: VecDeque<(Instant, f64)>,
    max_history: usize,
}
```

---

**Last Updated**: November 2025
**Architecture Version**: 1.0
**Implementation Status**: Production Ready
