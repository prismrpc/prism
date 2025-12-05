# Scoring Configuration Guide

**Module**: `crates/prism-core/src/upstream/scoring.rs`

## Overview

This guide provides comprehensive configuration documentation for Prism's multi-factor scoring system. Scoring configuration controls how upstream performance metrics are weighted, what thresholds trigger penalties, and how long metrics are tracked.

## Configuration Structure

### ScoringConfig

**Location**: Lines 32-86 in scoring.rs

```rust
pub struct ScoringConfig {
    pub enabled: bool,
    pub window_seconds: u64,
    pub min_samples: usize,
    pub weights: ScoringWeights,
    pub max_block_lag: u64,
    pub top_n: usize,
}
```

### ScoringWeights

**Location**: Lines 88-141 in scoring.rs

```rust
pub struct ScoringWeights {
    pub latency: f64,
    pub error_rate: f64,
    pub throttle_rate: f64,
    pub block_head_lag: f64,
    pub total_requests: f64,
}
```

## Field Reference

### `enabled: bool`

**Default**: `false`
**Location**: Line 38 in scoring.rs

Controls whether scoring-based upstream selection is active.

**Values**:
- `true`: Use scoring to rank upstreams, select based on composite scores
- `false`: Use simple selection (round-robin, random, or manual)

**Example**:
```toml
[scoring]
enabled = true
```

**Performance Impact**:
- When `false`: No scoring overhead
- When `true`: ~5-13μs per request for metric recording and score calculation

---

### `window_seconds: u64`

**Default**: `1800` (30 minutes)
**Location**: Line 42 in scoring.rs
**Default Function**: Lines 62-64 in scoring.rs

Time window in seconds for collecting metrics before automatic reset.

**Purpose**: Ensures scores reflect recent performance, not stale historical data.

**Value Recommendations**:

| Use Case | Window | Reasoning |
|----------|--------|-----------|
| Fast-changing conditions | 300-600s (5-10 min) | Quick adaptation to upstream changes |
| Stable infrastructure | 1800-3600s (30-60 min) | Smooth out temporary spikes |
| Development/testing | 60-300s (1-5 min) | Faster iteration |

**Example**:
```toml
[scoring]
window_seconds = 600  # 10 minute window
```

**Trade-offs**:
- **Short window**: Adapts quickly to changes, but more volatile scores
- **Long window**: Stable scores, but slow to react to degradation

---

### `min_samples: usize`

**Default**: `10`
**Location**: Line 46 in scoring.rs
**Default Function**: Lines 65-67 in scoring.rs

Minimum number of latency samples required before scoring becomes active for an upstream.

**Purpose**: Prevents unreliable scores based on insufficient data.

**Value Recommendations**:

| Traffic Level | Min Samples | Time to Score (at req/s) |
|---------------|-------------|--------------------------|
| High (100+ req/s) | 20-50 | < 1 second |
| Medium (10-100 req/s) | 10-20 | 1-2 seconds |
| Low (< 10 req/s) | 5-10 | 1-10 seconds |

**Example**:
```toml
[scoring]
min_samples = 20  # Require more samples for reliability
```

**Important**: Upstreams with fewer than `min_samples` are excluded from scoring and won't appear in `get_ranked_upstreams()` results.

---

### `weights: ScoringWeights`

**Default**: See individual weight defaults below
**Location**: Lines 49-51 in scoring.rs

Weights for each scoring factor in the composite score calculation.

**Composite Score Formula**:
```
score = (latency_factor ^ latency_weight)
      × (error_rate_factor ^ error_rate_weight)
      × (throttle_factor ^ throttle_weight)
      × (block_lag_factor ^ block_lag_weight)
      × (load_factor ^ load_weight)
      × 100
```

**Key Insight**: Higher weights make that factor more influential. Weights are exponents, so even small increases have significant impact.

#### `weights.latency: f64`

**Default**: `8.0`
**Location**: Lines 94-96 in scoring.rs
**Default Function**: Lines 115-117 in scoring.rs

Weight for latency factor based on P90 latency.

**Effect**: Controls how much latency differences affect score.

**Examples**:
```toml
# Latency-critical application
[scoring.weights]
latency = 10.0  # Very high weight

# Latency-tolerant application
[scoring.weights]
latency = 5.0   # Lower weight
```

**Latency Factor Calculation** (logarithmic scaling):
```
factor = 1.0 - (log2(p90_ms) / 14.0)
Clamped to [0.1, 1.0]

Examples:
- 100ms → 0.52
- 1000ms → 0.29
- 10000ms → 0.10
```

#### `weights.error_rate: f64`

**Default**: `4.0`
**Location**: Lines 98-100 in scoring.rs
**Default Function**: Lines 118-120 in scoring.rs

Weight for error rate factor (failed requests / total requests).

**Effect**: Controls penalty for unreliable upstreams.

**Examples**:
```toml
# High-reliability requirement
[scoring.weights]
error_rate = 8.0  # Heavily penalize errors

# Can tolerate some errors
[scoring.weights]
error_rate = 2.0  # Light penalty
```

**Error Rate Factor Calculation** (linear):
```
factor = 1.0 - error_rate
Clamped to [0.0, 1.0]

Examples:
- 0% errors → 1.0
- 5% errors → 0.95
- 20% errors → 0.80
```

#### `weights.throttle_rate: f64`

**Default**: `3.0`
**Location**: Lines 102-104 in scoring.rs
**Default Function**: Lines 121-123 in scoring.rs

Weight for throttle rate factor (HTTP 429 responses / total requests).

**Effect**: Controls penalty for rate-limited upstreams.

**Examples**:
```toml
# Strict throttle avoidance
[scoring.weights]
throttle_rate = 6.0  # Heavy penalty

# Can handle some throttling
[scoring.weights]
throttle_rate = 1.0  # Light penalty
```

**Throttle Factor Calculation** (exponential decay):
```
factor = exp(-3.0 * throttle_rate)
Clamped to [0.0, 1.0]

Examples:
- 0% throttles → 1.0
- 5% throttles → 0.86
- 10% throttles → 0.74
- 20% throttles → 0.55
```

**Why Exponential**: Throttling is more severe than errors (indicates quota exhaustion), so penalty increases rapidly.

#### `weights.block_head_lag: f64`

**Default**: `2.0`
**Location**: Lines 106-108 in scoring.rs
**Default Function**: Lines 124-126 in scoring.rs

Weight for block lag factor (blocks behind chain tip).

**Effect**: Controls penalty for upstreams behind the chain tip.

**Examples**:
```toml
# Must be at chain tip (DeFi)
[scoring.weights]
block_head_lag = 5.0  # High penalty for lag

# Historical queries tolerate lag
[scoring.weights]
block_head_lag = 0.5  # Minimal penalty
```

**Block Lag Factor Calculation** (linear penalty):
```
factor = 1.0 - (block_lag / max_block_lag)
Clamped to [0.0, 1.0]

Examples (max_block_lag = 5):
- 0 blocks behind → 1.0
- 2 blocks behind → 0.6
- 5 blocks behind → 0.0
- 10+ blocks behind → 0.0
```

#### `weights.total_requests: f64`

**Default**: `1.0`
**Location**: Lines 110-112 in scoring.rs
**Default Function**: Lines 127-129 in scoring.rs

Weight for load balancing factor.

**Current Implementation**: Always returns 1.0 (neutral), reserved for future use.

**Future**: Could prefer upstreams with fewer requests to spread load.

---

### `max_block_lag: u64`

**Default**: `5` blocks
**Location**: Lines 53-55 in scoring.rs
**Default Function**: Lines 68-70 in scoring.rs

Maximum acceptable block lag before upstream receives maximum penalty.

**Purpose**: Defines threshold for "too far behind" the chain tip.

**Value Recommendations**:

| Use Case | Max Lag | Reasoning |
|----------|---------|-----------|
| DeFi / Real-time | 1-3 blocks | Must see latest state |
| Block explorers | 3-5 blocks | Recent data important |
| Analytics | 10-20 blocks | Historical queries OK |
| Backfilling | No limit | Any block is fine |

**Example**:
```toml
[scoring]
max_block_lag = 3  # Strict requirement for recent blocks
```

**Calculation**:
```
chain_tip = 18,000,000 (highest block seen)
upstream_block = 17,999,995
block_lag = 18,000,000 - 17,999,995 = 5 blocks

If max_block_lag = 5:
  block_lag_factor = 1.0 - (5 / 5) = 0.0 (maximum penalty)

If max_block_lag = 10:
  block_lag_factor = 1.0 - (5 / 10) = 0.5 (moderate penalty)
```

---

### `top_n: usize`

**Default**: `3`
**Location**: Lines 58-60 in scoring.rs
**Default Function**: Lines 71-73 in scoring.rs

Number of top-scoring upstreams to consider for selection.

**Purpose**: Limits selection pool to best performers.

**Current Implementation**: Not actively used in core logic, reserved for weighted selection strategies.

**Future Use**:
```rust
// Select from top N with weighted probability
let top_upstreams = engine.get_ranked_upstreams()
    .await
    .into_iter()
    .take(config.top_n)
    .collect();

// Weighted random selection from top_upstreams
```

**Example**:
```toml
[scoring]
top_n = 5  # Consider top 5 upstreams
```

---

## Configuration Presets

### Low Latency (Speed Priority)

```toml
[scoring]
enabled = true
window_seconds = 600       # 10 min (quick adaptation)
min_samples = 20
max_block_lag = 5
top_n = 3

[scoring.weights]
latency = 10.0             # Highest priority
error_rate = 3.0
throttle_rate = 2.0
block_head_lag = 1.0
total_requests = 1.0
```

**Expected Score Distribution**:
- Fast upstream (100ms P90): 85-95
- Medium upstream (200ms P90): 70-80
- Slow upstream (500ms P90): 40-60

---

### High Reliability (Error Avoidance)

```toml
[scoring]
enabled = true
window_seconds = 1800      # 30 min (stable scores)
min_samples = 10
max_block_lag = 3          # Strict lag tolerance
top_n = 3

[scoring.weights]
latency = 5.0
error_rate = 8.0           # Highest priority
throttle_rate = 6.0
block_head_lag = 4.0
total_requests = 1.0
```

**Expected Score Distribution**:
- Reliable upstream (1% errors): 80-90
- Some errors (5% errors): 60-75
- Unreliable (10% errors): 30-50

---

### Balanced (Recommended)

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

**Expected Score Distribution**:
- Excellent upstream: 80-95
- Good upstream: 65-80
- Marginal upstream: 45-65
- Poor upstream: 20-45

---

## Runtime Configuration Updates

```rust
use prism_core::upstream::{ScoringEngine, ScoringConfig, ScoringWeights};

// Get current config
let current = engine.get_config().await;

// Modify weights
let new_config = ScoringConfig {
    weights: ScoringWeights {
        latency: 10.0,  // Increase latency importance
        ..current.weights
    },
    ..current
};

// Apply
engine.update_config(new_config).await;
```

**Effects**:
-  Takes effect immediately for new score calculations
-  Does not reset existing metrics
-  Logged for audit trail
-  Not persisted to file

---

## Common Mistakes

### Mistake 1: Extreme Weights

```toml
#  PROBLEMATIC
[scoring.weights]
latency = 100.0  # Too extreme
```

**Why**: Weights are exponents. A factor of 0.9 raised to power 100 is nearly 0, making scores overly sensitive.

**Fix**: Keep weights in range 0.5 - 10.0

---

### Mistake 2: Zero Weight on Critical Factor

```toml
#  INEFFECTIVE
[scoring.weights]
error_rate = 0.0  # Ignores errors completely
```

**Why**: Upstreams with high error rates will score well if other factors are good.

**Fix**: Use at least 1.0 for all factors you care about

---

### Mistake 3: Window Too Short

```toml
#  UNSTABLE
window_seconds = 30  # Only 30 seconds
```

**Why**: Scores will fluctuate wildly, causing upstream thrashing.

**Fix**: Use at least 300s (5 min), preferably 1800s (30 min)

---

### Mistake 4: min_samples Too Low

```toml
#  UNRELIABLE
min_samples = 1  # Score after just 1 request
```

**Why**: Single requests don't represent true upstream performance.

**Fix**: Use at least 10 samples, preferably 20+

---

## Troubleshooting

### All Upstreams Have Similar Scores

**Symptom**: Ranked upstreams show scores within 5 points of each other

**Possible Causes**:
1. All upstreams actually perform similarly
2. Weights too balanced (all equal)
3. Window too long, averaging out differences

**Solutions**:
1. Increase weight on most important factor
2. Shorten window to 600-900s
3. Check raw metrics to confirm similarities

---

### Scores Change Too Frequently

**Symptom**: Rankings flip every few seconds

**Possible Causes**:
1. Window too short
2. min_samples too low
3. High variance in upstream performance

**Solutions**:
1. Increase window_seconds to 1800-3600
2. Increase min_samples to 20-50
3. Use weighted selection with top_n to smooth transitions

---

### Scoring Shows No Effect

**Symptom**: Selection seems random despite enabled scoring

**Possible Causes**:
1. `enabled = false`
2. Insufficient samples (< min_samples)
3. Selection logic not using ranked results

**Solutions**:
1. Verify `enabled = true`
2. Check `get_metrics()` to see sample counts
3. Ensure UpstreamManager calls `get_ranked_upstreams()`

---

## Monitoring Best Practices

```rust
// Log score distribution periodically
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(300));

    loop {
        interval.tick().await;

        let ranked = engine.get_ranked_upstreams().await;

        info!("=== Upstream Scores ===");
        for (name, score) in &ranked {
            if let Some(details) = engine.get_score(name).await {
                info!(
                    upstream = %name,
                    score = %format!("{:.2}", score),
                    latency_p90 = details.latency_p90_ms.unwrap_or(0),
                    error_rate = %format!("{:.1}%", details.error_rate * 100.0),
                    throttle_rate = %format!("{:.1}%", details.throttle_rate * 100.0),
                    block_lag = details.block_head_lag,
                    "upstream score"
                );
            }
        }

        // Alert on low scores
        if let Some((_, lowest_score)) = ranked.last() {
            if *lowest_score < 40.0 {
                warn!(score = lowest_score, "lowest upstream score below threshold");
            }
        }
    }
});
```

---

**Last Updated**: November 2025
**Configuration Version**: 1.0
