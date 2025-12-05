# Hedging Configuration Guide

**Module**: `crates/prism-core/src/upstream/hedging.rs`

## Overview

This guide provides comprehensive configuration documentation for Prism's hedging system. Hedging configuration controls when duplicate requests are sent to backup upstreams, how delays are calculated from latency percentiles, and how many parallel requests can be made.

## Configuration Structure

### HedgeConfig

**Location**: Lines 30-89 in hedging.rs

```rust
pub struct HedgeConfig {
    pub enabled: bool,
    pub latency_quantile: f64,
    pub min_delay_ms: u64,
    pub max_delay_ms: u64,
    pub max_parallel: usize,
}
```

## Field Reference

### `enabled: bool`

**Default**: `false`
**Location**: Line 37 in hedging.rs

Controls whether hedging is active globally.

**Values**:
- `true`: Hedging enabled for all requests
- `false`: All requests use single upstream (no hedging overhead)

**Use Cases**:
- Set to `false` for development/testing environments
- Set to `true` for production deployments requiring low tail latency
- Can be toggled at runtime via `update_config()` without service restart

**Example**:
```toml
[hedging]
enabled = true
```

**Performance Impact**:
- When `false`: Zero overhead, behaves as if hedging doesn't exist
- When `true`: Adds latency tracking overhead (~1-2μs per request) plus hedged requests for slow responses

---

### `latency_quantile: f64`

**Default**: `0.95` (P95)
**Location**: Lines 39-45 in hedging.rs
**Default Function**: Lines 63-65 in hedging.rs

The percentile of latency to use as the hedge trigger threshold.

**Values**:
- `0.50` (P50): Hedge at median latency (very aggressive, high load)
- `0.75` (P75): Hedge at 75th percentile (aggressive)
- `0.90` (P90): Hedge at 90th percentile (moderately aggressive)
- `0.95` (P95): Hedge at 95th percentile (balanced - default)
- `0.99` (P99): Hedge at 99th percentile (conservative)
- `0.999` (P99.9): Hedge only on extreme outliers (very conservative)

**How It Works**:

For each upstream, the hedging system tracks a sliding window of recent response times. The `latency_quantile` determines which percentile to use as the hedge delay:

```
Upstream latency distribution (last 1000 requests):
[50ms, 52ms, 48ms, ..., 300ms, 55ms, 2000ms, ...]

Sort ascending:
[48ms, 50ms, 52ms, 55ms, ..., 300ms, ..., 2000ms]

P95 (latency_quantile = 0.95):
Index = (1000 - 1) * 0.95 = 949
Value = sorted[949] = 180ms

Hedge delay = 180ms (clamped to [min_delay_ms, max_delay_ms])
```

**Trade-offs**:

| Quantile | Hedge Rate | Load Increase | Latency Benefit | Cost |
|----------|-----------|---------------|-----------------|------|
| P50 | ~50% | ~1.5x | Excellent | Very High |
| P75 | ~25% | ~1.25x | Very Good | High |
| P90 | ~10% | ~1.1x | Good | Medium |
| P95 | ~5% | ~1.05x | Moderate | Low |
| P99 | ~1% | ~1.01x | Slight | Very Low |

**Example**:
```toml
[hedging]
latency_quantile = 0.90  # Hedge at P90 (more aggressive)
```

**Recommendations**:
- **Latency-Critical Apps** (trading, gaming): 0.90-0.95
- **General Production**: 0.95 (balanced)
- **Cost-Sensitive**: 0.99 (minimal hedging)
- **High-Load Systems**: 0.99 (reduce duplicate requests)

---

### `min_delay_ms: u64`

**Default**: `50` (50 milliseconds)
**Location**: Lines 48-50 in hedging.rs
**Default Function**: Lines 67-69 in hedging.rs

Minimum delay in milliseconds before issuing a hedged request.

**Purpose**: Prevents hedging on very fast requests where the overhead of hedging outweighs the benefit.

**Why It Matters**:

```
Fast request (primary responds in 10ms):
├─ Without min_delay: Would hedge at calculated delay (e.g., 8ms based on P95)
│  └─ Result: Hedge sent after 8ms, primary responds at 10ms
│     └─ Overhead: Extra request for minimal benefit (2ms saved)
│
└─ With min_delay_ms = 50: Hedge only if primary exceeds 50ms
   └─ Result: Primary responds at 10ms, hedge never sent
      └─ Benefit: No overhead, optimal response time
```

**Value Recommendations**:

| Use Case | Recommended Min | Reasoning |
|----------|----------------|-----------|
| Fast upstreams (P50 < 50ms) | 20-30ms | Allow hedging on slow responses |
| Normal upstreams (P50 50-200ms) | 50-100ms | Standard protection |
| Slow upstreams (P50 > 200ms) | 100-200ms | Avoid excessive hedging |
| Public RPC nodes | 100ms+ | Variable performance |

**Example**:
```toml
[hedging]
min_delay_ms = 30  # Lower minimum for fast upstreams
```

**Validation**:
```rust
assert!(min_delay_ms > 0, "min_delay_ms must be positive");
assert!(min_delay_ms <= max_delay_ms, "min cannot exceed max");
```

---

### `max_delay_ms: u64`

**Default**: `2000` (2 seconds)
**Location**: Lines 53-55 in hedging.rs
**Default Function**: Lines 71-73 in hedging.rs

Maximum delay in milliseconds before issuing a hedged request.

**Purpose**: Caps the hedge delay even for very slow upstreams to ensure hedging actually improves response time.

**Why It Matters**:

```
Slow upstream (P95 = 10,000ms):
├─ Without max_delay: Would wait 10,000ms before hedging
│  └─ Result: Hedge sent after 10s, likely too late to help
│     └─ Problem: User already experienced bad latency
│
└─ With max_delay_ms = 2000: Hedge after 2s regardless of P95
   └─ Result: Hedge sent at 2s, backup may respond faster
      └─ Benefit: Cap worst-case latency at ~2s
```

**Value Recommendations**:

| Use Case | Recommended Max | Reasoning |
|----------|-----------------|-----------|
| Real-time apps | 500-1000ms | Users expect fast responses |
| General web apps | 2000-3000ms | Balanced UX |
| Background jobs | 5000-10000ms | Latency less critical |
| Batch processing | No limit needed | Hedging may not apply |

**Example**:
```toml
[hedging]
max_delay_ms = 1000  # Cap at 1 second for real-time app
```

**Validation**:
```rust
assert!(max_delay_ms > 0, "max_delay_ms must be positive");
assert!(max_delay_ms >= min_delay_ms, "max cannot be less than min");
```

---

### `max_parallel: usize`

**Default**: `2` (primary + 1 hedged request)
**Location**: Lines 58-60 in hedging.rs
**Default Function**: Lines 75-77 in hedging.rs

Maximum number of parallel requests to send, including the primary.

**Values**:
- `1`: No hedging (equivalent to `enabled = false`)
- `2`: Primary + 1 hedged request (default, minimal overhead)
- `3`: Primary + 2 hedged requests (more aggressive)
- `4+`: Primary + 3+ hedged requests (very aggressive, high load)

**How It Works**:

```
max_parallel = 2:
├─ Send primary request to upstream_1
├─ Wait hedge_delay
└─ If primary not complete: Send hedged request to upstream_2
   └─ Return first successful response

max_parallel = 3:
├─ Send primary request to upstream_1
├─ Wait hedge_delay
└─ If primary not complete: Send hedged requests to upstream_2 AND upstream_3
   └─ Return first successful response (3-way race)
```

**Load Impact**:

| max_parallel | Hedge Rate | Load Multiplier | Example (1000 req/s) |
|--------------|-----------|-----------------|----------------------|
| 1 | 0% | 1.0x | 1000 req/s |
| 2 | 5% (P95) | 1.05x | 1050 req/s |
| 3 | 5% (P95) | 1.10x | 1100 req/s (2 extra per hedge) |
| 4 | 5% (P95) | 1.15x | 1150 req/s (3 extra per hedge) |

**Trade-offs**:

| max_parallel | Latency Benefit | Load Increase | Cost | Complexity |
|--------------|----------------|---------------|------|-----------|
| 2 | Good (1 backup) | Low | Low | Simple |
| 3 | Better (2 backups) | Medium | Medium | Moderate |
| 4+ | Marginal | High | High | Complex |

**Recommendations**:
- **Most Applications**: 2 (simple and effective)
- **Critical Low-Latency**: 3 (two backup options)
- **Cost-Sensitive**: 2 (minimize load)
- **High-Availability**: 3 (more redundancy)

**Example**:
```toml
[hedging]
max_parallel = 3  # Allow up to 2 hedged requests
```

**Validation**:
```rust
assert!(max_parallel >= 1, "max_parallel must be at least 1");
```

**Important**: Setting `max_parallel = 1` effectively disables hedging even if `enabled = true`, as only the primary request is sent.

---

## Configuration Presets

### Low Latency (Aggressive Hedging)

```toml
[hedging]
enabled = true
latency_quantile = 0.90    # Hedge at P90 (earlier)
min_delay_ms = 20          # Lower minimum
max_delay_ms = 500         # Cap at 500ms
max_parallel = 3           # Allow 2 hedged requests
```

**Profile**: Trading bots, gaming, real-time dashboards

**Characteristics**:
-  Lowest tail latency (P95/P99 significantly reduced)
-  Quick recovery from slow responses
-  Better user experience for latency-sensitive applications
-  Higher upstream load (~15-20% increase)
-  Higher cost (more RPC calls)

**Expected Results**:
- P95 latency: -30% to -50%
- P99 latency: -40% to -60%
- Hedge rate: ~10-15%
- Load increase: ~1.15x

---

### Balanced (Recommended)

```toml
[hedging]
enabled = true
latency_quantile = 0.95    # Hedge at P95 (default)
min_delay_ms = 50          # 50ms minimum
max_delay_ms = 2000        # 2s maximum
max_parallel = 2           # Primary + 1 hedge
```

**Profile**: General production deployments, web applications, APIs

**Characteristics**:
-  Good tail latency improvement
-  Moderate upstream load increase (~5%)
-  Cost-effective (hedges only slow outliers)
-  Simple configuration (sensible defaults)
- ️ P95 improvements moderate compared to aggressive

**Expected Results**:
- P95 latency: -15% to -30%
- P99 latency: -30% to -50%
- Hedge rate: ~5%
- Load increase: ~1.05x

---

### Conservative (Cost-Optimized)

```toml
[hedging]
enabled = true
latency_quantile = 0.99    # Hedge only at P99
min_delay_ms = 100         # Higher minimum
max_delay_ms = 5000        # Higher maximum
max_parallel = 2           # Primary + 1 hedge
```

**Profile**: Cost-sensitive deployments, batch processing, background jobs

**Characteristics**:
-  Minimal upstream load increase (~1%)
-  Low cost (hedges only pathological cases)
-  Still provides protection against outliers
- ️ Modest P95 improvement
-  Won't significantly help P95, mainly targets P99+

**Expected Results**:
- P95 latency: -5% to -10% (minimal change)
- P99 latency: -20% to -40%
- Hedge rate: ~1%
- Load increase: ~1.01x

---

### Disabled (No Hedging)

```toml
[hedging]
enabled = false
```

**Profile**: Development, testing, debugging, or when hedging is inappropriate

**Characteristics**:
-  Zero overhead
-  Simplest configuration
-  Predictable behavior
-  No tail latency protection

**Use Cases**:
- Development environments
- Debugging latency issues
- Testing without hedging interference
- Applications where hedging is contraindicated

---

### Custom Per-Method

For applications where different methods have different latency requirements:

```toml
# Base configuration
[hedging]
enabled = true
latency_quantile = 0.95
min_delay_ms = 50
max_delay_ms = 2000
max_parallel = 2

# Override for specific methods (pseudocode - requires custom implementation)
[hedging.overrides."eth_getLogs"]
latency_quantile = 0.90    # More aggressive for slow method
max_delay_ms = 5000        # Higher cap

[hedging.overrides."eth_call"]
latency_quantile = 0.99    # Conservative for fast method
min_delay_ms = 20          # Lower minimum
```

**Note**: Per-method overrides are not currently implemented but show how configuration could be extended.

---

## Environment Variable Overrides

While TOML configuration is primary, environment variables can override settings:

```bash
# Enable/disable hedging
export PRISM_HEDGING_ENABLED=true

# Latency percentile (0.0-1.0)
export PRISM_HEDGING_LATENCY_QUANTILE=0.95

# Delays in milliseconds
export PRISM_HEDGING_MIN_DELAY_MS=50
export PRISM_HEDGING_MAX_DELAY_MS=2000

# Maximum parallel requests
export PRISM_HEDGING_MAX_PARALLEL=2
```

**Note**: Exact environment variable names depend on the config loading implementation. Check `crates/prism-core/src/config/` for specifics.

## Runtime Configuration Updates

Configuration can be updated without restarting the service:

```rust
use prism_core::upstream::{HedgeExecutor, HedgeConfig};

// Get current configuration
let current_config = executor.get_config().await;
println!("Current quantile: {}", current_config.latency_quantile);

// Create updated configuration
let new_config = HedgeConfig {
    latency_quantile: 0.90,  // More aggressive
    min_delay_ms: 30,
    max_delay_ms: 1000,
    max_parallel: 3,
    ..current_config
};

// Apply update
executor.update_config(new_config).await;

// Verify
let updated_config = executor.get_config().await;
println!("New quantile: {}", updated_config.latency_quantile);
```

**Effects**:
-  Takes effect immediately for new requests
-  Does not affect in-flight hedged requests
-  Logged for audit trail
-  Not persisted to file (need to update TOML for restart)

## Validation Rules

The hedging configuration should satisfy these invariants:

```rust
// Delays must be positive and ordered
assert!(config.min_delay_ms > 0);
assert!(config.max_delay_ms > 0);
assert!(config.min_delay_ms <= config.max_delay_ms);

// Quantile must be in valid range
assert!(config.latency_quantile >= 0.0);
assert!(config.latency_quantile <= 1.0);

// Parallelism must be at least 1
assert!(config.max_parallel >= 1);
```

## Common Configuration Mistakes

### Mistake 1: min_delay_ms > max_delay_ms

```toml
#  INVALID
min_delay_ms = 1000
max_delay_ms = 500  # Cannot be less than minimum!
```

**Fix**: Ensure `min_delay_ms <= max_delay_ms`

---

### Mistake 2: latency_quantile Outside [0.0, 1.0]

```toml
#  INVALID
latency_quantile = 95.0  # Should be 0.95, not 95!
```

**Fix**: Use decimal format (0.95 for P95, not 95.0)

---

### Mistake 3: max_parallel = 1 with enabled = true

```toml
#  INEFFECTIVE
enabled = true
max_parallel = 1  # No hedging possible with only 1 request!
```

**Why**: With `max_parallel = 1`, only the primary request is sent, so hedging cannot occur.

**Fix**: Set `max_parallel >= 2` or disable hedging entirely

---

### Mistake 4: Extremely Low min_delay_ms

```toml
#  PROBLEMATIC
min_delay_ms = 1  # Too aggressive, hedges almost all requests
```

**Why**: Very low minimum delays cause hedging on nearly every request, even fast ones, increasing load without benefit.

**Fix**: Set minimum to at least 20-50ms depending on typical response times

---

### Mistake 5: Very High max_parallel

```toml
#  WASTEFUL
max_parallel = 10  # Sends up to 10 parallel requests!
```

**Why**: Excessive parallelism provides diminishing returns and wastes upstream quota/cost.

**Fix**: Use `max_parallel = 2` or `3` for most use cases

---

### Mistake 6: Hedging on Write Methods

```toml
#  DANGEROUS if applied to write methods
enabled = true
```

**Important**: Hedging should only be used for **read methods**. Applying hedging to write methods like `eth_sendRawTransaction` can cause:
- Double spending attempts
- Wasted gas fees
- Nonce conflicts

**Fix**: Ensure hedging is only applied to read methods via application logic (not configurable directly in HedgeConfig, must be handled by caller)

---

## Troubleshooting

### High Hedge Rate

**Symptom**: Large percentage of requests trigger hedging (>20%)

**Possible Causes**:
1. `latency_quantile` too low (e.g., 0.50)
2. Upstreams genuinely slow/unreliable
3. `min_delay_ms` too low

**Solutions**:
1. Increase `latency_quantile` to 0.95 or 0.99
2. Investigate upstream health and performance
3. Increase `min_delay_ms` to reduce hedging on fast requests
4. Check if scoring is correctly routing to fastest upstreams

**Diagnostics**:
```rust
// Monitor hedge rate
let stats = executor.get_latency_stats("upstream-1").await;
if let Some((_p50, p95, _p99, _avg)) = stats {
    let hedge_delay = executor.calculate_hedge_delay("upstream-1").await;
    println!("P95: {}ms, Hedge delay: {:?}", p95, hedge_delay);
}
```

---

### No Hedging Occurring

**Symptom**: Hedging appears to be disabled even with `enabled = true`

**Possible Causes**:
1. `max_parallel = 1`
2. Insufficient latency data (< 1 sample)
3. Only one upstream available
4. All upstreams very fast (primary always responds within delay)

**Solutions**:
1. Set `max_parallel >= 2`
2. Ensure requests are being tracked (`record_latency()` called)
3. Add more upstreams to upstream pool
4. Lower `min_delay_ms` if appropriate

**Diagnostics**:
```rust
// Check latency tracking
let stats = executor.get_latency_stats("upstream-1").await;
if stats.is_none() {
    println!("No latency data for upstream-1");
} else {
    println!("Latency stats: {:?}", stats);
}
```

---

### Excessive Upstream Load

**Symptom**: Upstream quota exhausted or cost very high

**Possible Causes**:
1. `max_parallel` too high
2. `latency_quantile` too low (hedging too often)
3. Cache hit rate low (more requests going to upstreams)

**Solutions**:
1. Reduce `max_parallel` to 2
2. Increase `latency_quantile` to 0.99
3. Improve caching to reduce upstream requests
4. Consider disabling hedging during peak load

**Expected Load**:
```
Base load: 1000 req/s
Cache hit rate: 95% → 50 req/s to upstreams
Hedge rate at P95: 5% → 2.5 hedge requests/s
Total upstream load: 50 + 2.5 = 52.5 req/s (~5% increase)
```

---

### Hedge Delays Too Long

**Symptom**: Hedged requests sent too late to help

**Possible Causes**:
1. `max_delay_ms` too high
2. P95 latency very high, needs to be capped
3. Slow upstreams skewing latency distribution

**Solutions**:
1. Lower `max_delay_ms` to 1000-2000ms
2. Use `latency_quantile = 0.90` instead of 0.95
3. Remove slow upstreams from pool or improve scoring

---

### Hedge Delays Too Short

**Symptom**: Hedging triggered immediately, high hedge rate

**Possible Causes**:
1. `min_delay_ms` too low
2. Primary upstream very fast, calculated delay < minimum
3. Insufficient latency samples (defaults to min)

**Solutions**:
1. Increase `min_delay_ms` to 50-100ms
2. Allow more samples to accumulate before hedging
3. Check that latency tracking is working correctly

---

## Performance Tuning

### Latency-Optimized

Goal: Minimize P95/P99 latency, cost is secondary

```toml
[hedging]
enabled = true
latency_quantile = 0.90
min_delay_ms = 20
max_delay_ms = 1000
max_parallel = 3
```

**Expected Impact**:
- P95: -40% to -60%
- P99: -50% to -70%
- Load: +15% to +20%

---

### Cost-Optimized

Goal: Minimize upstream costs, accept higher tail latency

```toml
[hedging]
enabled = true
latency_quantile = 0.99
min_delay_ms = 100
max_delay_ms = 5000
max_parallel = 2
```

**Expected Impact**:
- P95: -5% to -10%
- P99: -20% to -30%
- Load: +1% to +2%

---

### Balanced

Goal: Good latency improvement with reasonable cost

```toml
[hedging]
enabled = true
latency_quantile = 0.95
min_delay_ms = 50
max_delay_ms = 2000
max_parallel = 2
```

**Expected Impact**:
- P95: -20% to -30%
- P99: -30% to -50%
- Load: +5% to +10%

---

## Monitoring Best Practices

### Key Metrics

```rust
// Collect hedging metrics periodically
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));

    loop {
        interval.tick().await;

        for upstream in &upstreams {
            if let Some((p50, p95, p99, avg)) = executor.get_latency_stats(upstream).await {
                // Log statistics
                info!(
                    upstream = upstream,
                    p50 = p50,
                    p95 = p95,
                    p99 = p99,
                    avg = avg,
                    "latency statistics"
                );

                // Calculate hedge delay
                if let Some(delay) = executor.calculate_hedge_delay(upstream).await {
                    info!(upstream = upstream, delay_ms = delay.as_millis(), "hedge delay");
                }
            }
        }
    }
});
```

### Alerts

```rust
// Alert on anomalies
if let Some((_p50, _p95, p99, _avg)) = executor.get_latency_stats(upstream).await {
    if p99 > 5000 {
        warn!(upstream = upstream, p99 = p99, "very high tail latency");
    }
}
```

---

**Last Updated**: November 2025
**Configuration Version**: 1.0
