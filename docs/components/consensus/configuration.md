# Consensus Configuration Guide

**Module**: `crates/prism-core/src/upstream/consensus/`

## Overview

This guide provides comprehensive configuration documentation for Prism's consensus validation system. Consensus configuration controls which RPC methods require multi-upstream validation, how many upstreams to query, and how to handle edge cases like disputes and failures.

## Configuration Structure

### ConsensusConfig

**Location**: Lines 33-73 in consensus.rs

```rust
pub struct ConsensusConfig {
    pub enabled: bool,
    pub max_count: usize,
    pub min_count: usize,
    pub dispute_behavior: DisputeBehavior,
    pub failure_behavior: FailureBehavior,
    pub low_participants_behavior: LowParticipantsBehavior,
    pub methods: Vec<String>,
    pub disagreement_penalty: f64,
    pub timeout_seconds: u64,
}
```

## Field Reference

### `enabled: bool`

**Default**: `false`
**Location**: Line 39 in consensus.rs

Controls whether consensus validation is active globally.

**Values**:
- `true`: Consensus validation enabled for methods in `methods` list
- `false`: All requests use single-upstream routing (consensus bypassed)

**Use Cases**:
- Set to `false` for development/testing environments
- Set to `true` for production deployments requiring data integrity
- Can be toggled at runtime via `update_config()` without service restart

**Example**:
```toml
[consensus]
enabled = true
```

**Performance Impact**:
- When `false`: Zero overhead, behaves as if consensus module doesn't exist
- When `true`: Only affects methods in `methods` list (see below)

---

### `max_count: usize`

**Default**: `3`
**Location**: Line 43 in consensus.rs
**Default Function**: Lines 75-77 in consensus.rs

Maximum number of upstreams to query in parallel for consensus.

**Values**:
- Minimum: `1` (but renders consensus pointless)
- Practical minimum: `2` (allows basic validation)
- Recommended: `3-5` (balances validation and overhead)
- Maximum: Limited by number of available upstreams

**Trade-offs**:

| max_count | Validation Strength | Latency | Upstream Load | Cost |
|-----------|---------------------|---------|---------------|------|
| 2 | Weak (binary comparison) | Low | 2x | Low |
| 3 | Good (majority possible) | Medium | 3x | Medium |
| 5 | Excellent (strong majority) | High | 5x | High |
| 7+ | Overkill for most use cases | Very High | 7x+ | Very High |

**Example**:
```toml
[consensus]
max_count = 3  # Query 3 upstreams per consensus request
```

**Recommendations**:
- **DeFi/Financial**: 3-5 (strong validation needed)
- **General Web3**: 3 (balanced)
- **Read-heavy workloads**: 2-3 (minimize overhead)
- **Cost-sensitive**: 2 (minimum viable)

---

### `min_count: usize`

**Default**: `2`
**Location**: Line 47 in consensus.rs
**Default Function**: Lines 79-81 in consensus.rs

Minimum number of upstreams that must agree for consensus to be achieved.

**Relationship to max_count**:
- Must satisfy: `min_count <= max_count`
- Typical: `min_count = ceil(max_count / 2) + 1` (simple majority)
- Conservative: `min_count = max_count` (unanimous)
- Permissive: `min_count = 2` (any two agree)

**Common Configurations**:

| max_count | min_count | Consensus Type | Description |
|-----------|-----------|----------------|-------------|
| 3 | 2 | Majority | 2 of 3 must agree (50%+) |
| 3 | 3 | Unanimous | All 3 must agree (100%) |
| 5 | 3 | Simple Majority | 3 of 5 must agree (60%) |
| 5 | 4 | Supermajority | 4 of 5 must agree (80%) |
| 5 | 5 | Unanimous | All 5 must agree (100%) |

**Example**:
```toml
[consensus]
max_count = 3
min_count = 2  # Require 2 out of 3 to agree
```

**Recommendations**:
- **High Safety**: Set `min_count = max_count` (unanimous)
- **Balanced**: Set `min_count = ceil(max_count / 2) + 1` (majority)
- **Fault Tolerant**: Set `min_count = max_count - 1` (tolerates 1 failure)

**Validation**:
```rust
assert!(min_count <= max_count, "min_count cannot exceed max_count");
assert!(min_count >= 1, "min_count must be at least 1");
```

---

### `dispute_behavior: DisputeBehavior`

**Default**: `PreferBlockHeadLeader`
**Location**: Line 52 in consensus.rs
**Enum Location**: Lines 118-128 in consensus.rs

Defines behavior when upstreams disagree and consensus cannot be reached.

#### DisputeBehavior::PreferBlockHeadLeader (DEFAULT)

**Description**: Prefer the response from the upstream with the highest block number (most synced to chain tip).

**Use Case**: Most common scenario - assume the most up-to-date node has the correct data

**Example**:
```toml
[consensus]
dispute_behavior = "PreferBlockHeadLeader"
```

**Behavior**:
```
Query 3 upstreams, all respond differently:
├─ Upstream A: block 18000000, response X
├─ Upstream B: block 17999990, response Y (10 blocks behind)
└─ Upstream C: block 17999995, response Z (5 blocks behind)

No consensus (each response unique)
→ Select Upstream A (highest block 18000000)
→ Return response X
→ Penalize B and C
```

**Advantages**:
-  Logical default (most synced node likely has correct data)
-  Handles stale nodes gracefully
-  Works well during network propagation delays

**Disadvantages**:
-  Assumes block number correlation with correctness
-  May select malicious node if it reports high block number

---

#### DisputeBehavior::ReturnError

**Description**: Return an error to the client when consensus cannot be reached.

**Use Case**: Conservative applications that require strong consensus guarantees

**Example**:
```toml
[consensus]
dispute_behavior = "ReturnError"
```

**Behavior**:
```
Query 3 upstreams, all respond differently:
├─ Upstream A: response X
├─ Upstream B: response Y
└─ Upstream C: response Z

No consensus (each response unique)
→ Return Err(UpstreamError::ConsensusFailure)
→ Client receives error
→ Penalize all upstreams (none agreed)
```

**Advantages**:
-  Maximum safety (never returns potentially incorrect data)
-  Forces client to handle consensus failure explicitly
-  Clear signal that data integrity is uncertain

**Disadvantages**:
-  Reduces availability (fails requests that could be valid)
-  May be overly conservative for minor discrepancies
-  Requires robust error handling in clients

**Recommended For**:
- Financial applications where incorrect data has severe consequences
- Critical infrastructure requiring absolute certainty
- Compliance scenarios requiring audit trails of data validation

---

#### DisputeBehavior::AcceptAnyValid

**Description**: Accept any valid response without requiring consensus.

**Use Case**: Performance-focused applications where availability is more important than consensus

**Example**:
```toml
[consensus]
dispute_behavior = "AcceptAnyValid"
```

**Behavior**:
```
Query 3 upstreams, all respond differently:
├─ Upstream A: response X (first to respond)
├─ Upstream B: response Y
└─ Upstream C: response Z

No consensus (each response unique)
→ Return first valid response (response X)
→ Still penalize disagreeing upstreams
```

**Advantages**:
-  Maximizes availability (always returns a response)
-  Lower latency (can return as soon as first response arrives)
-  Tolerates high disagreement rates

**Disadvantages**:
-  No validation benefit (essentially disables consensus)
-  May return incorrect data
-  Defeats purpose of consensus validation

**Recommended For**:
- Non-critical read operations
- Development/testing environments
- Temporary fallback during upstream issues

---

#### DisputeBehavior::PreferHighestScore

**Description**: Use the response from the upstream with the highest score according to the scoring engine.

**Use Case**: Trust the historically most reliable upstream

**Example**:
```toml
[consensus]
dispute_behavior = "PreferHighestScore"
```

**Behavior**:
```
Query 3 upstreams, all respond differently:
├─ Upstream A: score 95.0, response X
├─ Upstream B: score 87.5, response Y
└─ Upstream C: score 92.0, response Z

No consensus (each response unique)
→ Select Upstream A (highest score 95.0)
→ Return response X
→ Penalize B and C
```

**Advantages**:
-  Leverages historical reliability data
-  Tends to select consistently accurate upstreams
-  Adapts over time as scores change

**Disadvantages**:
-  High-scoring upstream could be compromised
-  May not reflect current state (score is historical)
-  Requires mature scoring data to be effective

**Note**: Implementation currently defaults to `AcceptAnyValid` behavior (lines 515-534 in consensus/engine.rs). Full scoring integration is planned.

---

### `failure_behavior: FailureBehavior`

**Default**: `AcceptAnyValid`
**Location**: Line 55 in consensus.rs
**Enum Location**: Lines 137-145 in consensus.rs

Defines behavior when some or all upstreams return errors instead of valid responses.

#### FailureBehavior::AcceptAnyValid (DEFAULT)

**Description**: Accept any valid response, ignoring upstream failures.

**Use Case**: Prioritize availability over perfect consensus

**Example**:
```toml
[consensus]
failure_behavior = "AcceptAnyValid"
```

**Behavior**:
```
Query 3 upstreams:
├─ Upstream A: Error (timeout)
├─ Upstream B: Valid response X
└─ Upstream C: Error (connection failed)

1 valid response available
→ Return response X
→ Mark A and C as failed (no penalty for disagreement)
```

**Advantages**:
-  Maximizes availability (tolerates upstream failures)
-  Works well when failures are common
-  Reduces error rate to clients

**Disadvantages**:
-  Single response (no validation if only one upstream succeeds)
-  May hide systemic upstream issues

---

#### FailureBehavior::ReturnError

**Description**: Return an error if any upstream fails.

**Use Case**: Conservative applications requiring all upstreams to be operational

**Example**:
```toml
[consensus]
failure_behavior = "ReturnError"
```

**Behavior**:
```
Query 3 upstreams:
├─ Upstream A: Valid response X
├─ Upstream B: Valid response X
└─ Upstream C: Error (timeout)

Consensus could be achieved (2 agree), but 1 upstream failed
→ Return Err(UpstreamError::ConsensusFailure)
→ Client receives error
```

**Advantages**:
-  Ensures all upstreams are healthy
-  Detects partial outages immediately
-  Strict validation (all upstreams must participate)

**Disadvantages**:
-  Very low availability (any single failure fails the request)
-  May be overly strict for production use
-  Upstream timeouts cause client errors

**Recommended For**:
- Testing scenarios to validate upstream health
- Debugging consensus configuration
- Monitoring and alerting systems

---

#### FailureBehavior::UseHighestScore

**Description**: When failures occur, use the response from the highest-scoring upstream that succeeded.

**Use Case**: Trust historically reliable upstreams during partial failures

**Example**:
```toml
[consensus]
failure_behavior = "UseHighestScore"
```

**Behavior**:
```
Query 3 upstreams:
├─ Upstream A: score 95.0, Valid response X
├─ Upstream B: score 87.5, Error (timeout)
└─ Upstream C: score 92.0, Valid response Y

2 valid responses available, no consensus
→ Select Upstream A (highest score 95.0)
→ Return response X
```

**Advantages**:
-  Leverages scoring data to make best decision
-  More intelligent than just accepting first valid
-  Adapts to upstream reliability over time

**Disadvantages**:
-  Still only validates against single response
-  Requires mature scoring data

---

### `low_participants_behavior: LowParticipantsBehavior`

**Default**: `AcceptAvailable`
**Location**: Line 59 in consensus.rs
**Enum Location**: Lines 154-162 in consensus.rs

Defines behavior when fewer than `min_count` upstreams are available or respond.

#### LowParticipantsBehavior::AcceptAvailable (DEFAULT)

**Description**: Proceed with available upstreams even if below `min_count`.

**Use Case**: Maintain availability when upstreams are degraded

**Example**:
```toml
[consensus]
low_participants_behavior = "AcceptAvailable"
```

**Behavior**:
```
Configuration: min_count = 2
Available upstreams: 1 (others are unhealthy)

Only 1 upstream available (< min_count)
→ Query the 1 available upstream
→ Return its response (no consensus validation)
→ Continue serving requests despite degraded state
```

**Advantages**:
-  Maximizes availability during outages
-  Graceful degradation (consensus → single upstream)
-  Prevents total service failure

**Disadvantages**:
-  No consensus validation when degraded
-  May hide upstream infrastructure issues
-  Data integrity not guaranteed

---

#### LowParticipantsBehavior::ReturnError

**Description**: Fail the request if insufficient upstreams are available.

**Use Case**: Conservative applications requiring minimum number of upstreams

**Example**:
```toml
[consensus]
low_participants_behavior = "ReturnError"
```

**Behavior**:
```
Configuration: min_count = 2
Available upstreams: 1 (others are unhealthy)

Only 1 upstream available (< min_count)
→ Return Err(UpstreamError::ConsensusFailure("Insufficient upstreams: 1 available, 2 required"))
→ Client receives error
```

**Advantages**:
-  Enforces minimum consensus requirements
-  Signals upstream infrastructure degradation clearly
-  Prevents accepting non-validated data

**Disadvantages**:
-  Reduces availability (fails during outages)
-  Requires robust upstream infrastructure
-  May cause cascading failures

**Recommended For**:
- Critical systems requiring guaranteed consensus
- Monitoring and alerting (detect degraded state)
- Testing minimum upstream requirements

---

#### LowParticipantsBehavior::OnlyBlockHeadLeader

**Description**: Query only the most synced upstream (highest block number) when insufficient upstreams available.

**Use Case**: Prefer most up-to-date node during degraded state

**Example**:
```toml
[consensus]
low_participants_behavior = "OnlyBlockHeadLeader"
```

**Behavior**:
```
Configuration: min_count = 2
Available upstreams: 1 healthy, 2 degraded but responsive

Healthy upstreams < min_count
→ Query scoring engine for block numbers:
  ├─ Upstream A (healthy): block 18000000
  ├─ Upstream B (degraded): block 17999995
  └─ Upstream C (degraded): block 17999990
→ Select Upstream A (highest block number)
→ Return its response
```

**Advantages**:
-  Logical fallback (most synced node likely correct)
-  Maintains availability
-  Better than random selection

**Disadvantages**:
-  Still only single-source validation
-  Assumes block number correlation with correctness

---

### `methods: Vec<String>`

**Default**: `["eth_getBlockByNumber", "eth_getBlockByHash", "eth_getTransactionByHash", "eth_getTransactionReceipt", "eth_getLogs"]`
**Location**: Line 63 in consensus.rs
**Default Function**: Lines 83-91 in consensus.rs

List of RPC methods that require consensus validation.

**Default Methods**:
1. `eth_getBlockByNumber` - Block retrieval by number
2. `eth_getBlockByHash` - Block retrieval by hash
3. `eth_getTransactionByHash` - Transaction retrieval
4. `eth_getTransactionReceipt` - Transaction receipt
5. `eth_getLogs` - Event log queries

**Selection Criteria**:
- Methods that return blockchain state that could diverge across nodes
- Methods critical for data integrity in typical Web3 applications
- Methods where incorrect data has significant downstream impact

**Methods to EXCLUDE** (never require consensus):
- **Write methods**: `eth_sendRawTransaction`, `eth_sendTransaction` (NEVER send multiple times!)
- **Chain metadata**: `eth_chainId`, `net_version` (static values)
- **Current state**: `eth_blockNumber` (changes constantly, no benefit from consensus)
- **Gas estimation**: `eth_estimateGas` (varies by node configuration)

**Example - Minimal**:
```toml
[consensus]
methods = [
    "eth_call"  # Only validate smart contract reads
]
```

**Example - Comprehensive**:
```toml
[consensus]
methods = [
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "eth_getLogs",
    "eth_call",
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "trace_block",
    "trace_transaction"
]
```

**Performance Impact**:
- Each method in the list incurs `max_count`x load increase
- Methods not in the list use normal single-upstream routing
- Empty list = consensus disabled for all methods (even if `enabled = true`)

**Recommendations by Use Case**:

| Use Case | Recommended Methods |
|----------|---------------------|
| DeFi | `eth_call`, `eth_getLogs`, `eth_getTransactionReceipt` |
| NFT Platform | `eth_getLogs`, `eth_getBlockByNumber`, `eth_call` |
| Block Explorer | `eth_getBlockByNumber`, `eth_getTransactionByHash`, `eth_getLogs` |
| Price Oracle | `eth_call` (critical for accurate prices) |
| General Web3 | Default list (balanced coverage) |

---

### `disagreement_penalty: f64`

**Default**: `10.0`
**Location**: Line 67 in consensus.rs
**Default Function**: Lines 93-95 in consensus.rs

Score penalty points to apply to upstreams that disagree with consensus.

**How It Works**:
1. Consensus achieved: 2 upstreams return response X, 1 returns response Y
2. Consensus selects response X (majority)
3. Call `scoring_engine.record_error(upstream_name)` for upstream that returned Y
4. Upstream's score reduced, affecting future selection probability

**Value Range**:
- **0.0**: No penalty (disagreements ignored)
- **1.0-5.0**: Light penalty (minor score reduction)
- **10.0**: Moderate penalty (default, noticeable impact)
- **20.0+**: Heavy penalty (strong disincentive)
- **100.0+**: Severe penalty (upstream effectively disabled)

**Example**:
```toml
[consensus]
disagreement_penalty = 15.0  # Stronger penalty for disagreements
```

**Recommendations**:
- **Development**: 0.0-5.0 (lenient, allows testing)
- **Production**: 10.0-15.0 (balanced feedback loop)
- **Critical Systems**: 20.0+ (low tolerance for errors)

**Integration with Scoring**:
```rust
// In consensus/engine.rs line 423
scoring_engine.record_error(upstream_name).await;
```

The actual score impact depends on the `ScoringEngine` configuration. This penalty is applied via the standard error recording mechanism.

---

### `timeout_seconds: u64`

**Default**: `10`
**Location**: Line 72 in consensus.rs
**Default Function**: Lines 97-99 in consensus.rs

Maximum time to wait for consensus responses before timing out.

**Timeout Scope**: Covers the entire consensus operation:
1. Query all upstreams in parallel
2. Wait for responses
3. Group responses by hash
4. Determine consensus

**Example**:
```toml
[consensus]
timeout_seconds = 15  # Allow 15 seconds for consensus
```

**Timeout Behavior**:
```
Start consensus → Query 3 upstreams in parallel
├─ Upstream A: responds in 200ms 
├─ Upstream B: responds in 500ms 
└─ Upstream C: hangs (no response)

After 10 seconds (timeout):
→ Return Err(UpstreamError::Timeout)
→ Upstream C marked as timed out
```

**Value Recommendations**:

| Upstream Type | Recommended Timeout | Reason |
|---------------|---------------------|--------|
| Premium (Alchemy, Infura) | 5-10s | Fast, reliable |
| Public RPC | 15-30s | Variable performance |
| Self-hosted | 5-15s | Depends on infrastructure |
| Mixed | 10-15s | Balanced |

**Trade-offs**:
- **Short timeout (5s)**: Fast failure detection, may timeout on slow upstreams
- **Medium timeout (10s)**: Balanced (default)
- **Long timeout (30s+)**: Tolerates slow upstreams, poor user experience

**Implementation**:
```rust
// Line 382 in consensus.rs
let timeout = std::time::Duration::from_secs(config.timeout_seconds);
let results = tokio::time::timeout(timeout, futures_util::future::join_all(futures))
    .await
    .map_err(|_| UpstreamError::Timeout)?;
```

## Configuration Presets

### Production (Balanced)

```toml
[consensus]
enabled = true
max_count = 3
min_count = 2
dispute_behavior = "PreferBlockHeadLeader"
failure_behavior = "AcceptAnyValid"
low_participants_behavior = "AcceptAvailable"
disagreement_penalty = 10.0
timeout_seconds = 10

methods = [
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "eth_getLogs"
]
```

**Profile**: General production use, balanced safety and performance

---

### High Assurance (Maximum Safety)

```toml
[consensus]
enabled = true
max_count = 5
min_count = 5  # Unanimous
dispute_behavior = "ReturnError"
failure_behavior = "ReturnError"
low_participants_behavior = "ReturnError"
disagreement_penalty = 25.0
timeout_seconds = 15

methods = [
    "eth_call",
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "eth_getLogs",
    "eth_getBalance",
    "eth_getStorageAt"
]
```

**Profile**: Financial applications, DeFi protocols, critical infrastructure

---

### Development (Permissive)

```toml
[consensus]
enabled = true
max_count = 2
min_count = 2
dispute_behavior = "AcceptAnyValid"
failure_behavior = "AcceptAnyValid"
low_participants_behavior = "AcceptAvailable"
disagreement_penalty = 2.0
timeout_seconds = 30

methods = [
    "eth_getLogs"
]
```

**Profile**: Development environments, testing, non-critical applications

---

### Performance Optimized (Minimal Overhead)

```toml
[consensus]
enabled = true
max_count = 2
min_count = 2
dispute_behavior = "PreferBlockHeadLeader"
failure_behavior = "AcceptAnyValid"
low_participants_behavior = "AcceptAvailable"
disagreement_penalty = 5.0
timeout_seconds = 5

methods = [
    "eth_call"  # Only critical contract reads
]
```

**Profile**: High-throughput applications, cost-sensitive deployments

## Environment Variable Overrides

While the TOML configuration is the primary method, environment variables can override settings:

```bash
# Enable/disable consensus
export PRISM_CONSENSUS_ENABLED=true

# Upstream counts
export PRISM_CONSENSUS_MAX_COUNT=3
export PRISM_CONSENSUS_MIN_COUNT=2

# Timeout
export PRISM_CONSENSUS_TIMEOUT_SECONDS=10

# Penalty
export PRISM_CONSENSUS_DISAGREEMENT_PENALTY=10.0

# Behaviors (use enum variant names)
export PRISM_CONSENSUS_DISPUTE_BEHAVIOR=PreferBlockHeadLeader
export PRISM_CONSENSUS_FAILURE_BEHAVIOR=AcceptAnyValid
export PRISM_CONSENSUS_LOW_PARTICIPANTS_BEHAVIOR=AcceptAvailable

# Methods (comma-separated)
export PRISM_CONSENSUS_METHODS="eth_getLogs,eth_call,eth_getBlockByNumber"
```

**Note**: Exact environment variable names depend on the config loading implementation. Check `crates/prism-core/src/config/` for specifics.

## Runtime Configuration Updates

Configuration can be updated without restarting the service:

```rust
use prism_core::upstream::{ConsensusEngine, ConsensusConfig, DisputeBehavior};

// Get current configuration
let current_config = engine.get_config().await;
println!("Current max_count: {}", current_config.max_count);

// Create updated configuration
let new_config = ConsensusConfig {
    max_count: 5,  // Increase to 5 upstreams
    min_count: 3,  // Require 3 agreeing
    dispute_behavior: DisputeBehavior::ReturnError,  // Stricter
    ..current_config  // Keep other settings
};

// Apply update
engine.update_config(new_config).await;

// Verify
let updated_config = engine.get_config().await;
println!("New max_count: {}", updated_config.max_count);
```

**Effects**:
-  Takes effect immediately for new requests
-  Does not affect in-flight consensus operations
-  Logged for audit trail
-  Not persisted to file (need to update TOML for restart)

## Validation Rules

The consensus configuration should satisfy these invariants:

```rust
// min_count must not exceed max_count
assert!(config.min_count <= config.max_count);

// Both counts must be at least 1
assert!(config.min_count >= 1);
assert!(config.max_count >= 1);

// Timeout must be positive
assert!(config.timeout_seconds > 0);

// Penalty should be non-negative
assert!(config.disagreement_penalty >= 0.0);
```

## Common Configuration Mistakes

### Mistake 1: max_count < min_count

```toml
#  INVALID
max_count = 2
min_count = 3  # Cannot require more agreements than upstreams queried!
```

**Fix**: Ensure `min_count <= max_count`

---

### Mistake 2: Including Write Methods

```toml
#  DANGEROUS
methods = [
    "eth_sendRawTransaction"  # NEVER use consensus for writes!
]
```

**Why**: Sending the same transaction to multiple upstreams can cause:
- Double spending attempts
- Wasted gas fees
- Nonce conflicts

**Fix**: Only include read methods

---

### Mistake 3: Too Strict for Available Upstreams

```toml
#  WILL FAIL
max_count = 5
min_count = 5  # Unanimous

# But only 3 upstreams configured!
```

**Fix**: Ensure enough upstreams are configured to meet `max_count`

---

### Mistake 4: Consensus on Chain Tip Methods

```toml
#  INEFFECTIVE
methods = [
    "eth_blockNumber"  # Changes every block, no consensus benefit
]
```

**Why**: Chain tip methods like `eth_blockNumber` change constantly and nodes may be slightly out of sync due to network propagation. Consensus provides little value.

**Fix**: Only use consensus for historical/stable data

---

### Mistake 5: Overly Conservative in Production

```toml
#  TOO STRICT FOR PRODUCTION
failure_behavior = "ReturnError"
low_participants_behavior = "ReturnError"
dispute_behavior = "ReturnError"
```

**Why**: Any single failure fails the entire request, leading to poor availability

**Fix**: Use more permissive behaviors like `AcceptAnyValid` or `PreferBlockHeadLeader`

## Troubleshooting

### High Consensus Failure Rate

**Symptom**: Many requests return `ConsensusFailure` errors

**Possible Causes**:
1. `min_count` too high for available upstreams
2. `dispute_behavior = "ReturnError"` too strict
3. Upstreams genuinely disagreeing (data integrity issue!)

**Solutions**:
1. Lower `min_count` or increase upstream count
2. Use `PreferBlockHeadLeader` dispute behavior
3. Investigate upstream health and synchronization

---

### Consensus Timeouts

**Symptom**: Frequent `Timeout` errors during consensus

**Possible Causes**:
1. `timeout_seconds` too low for slow upstreams
2. Upstreams hanging or very slow
3. Network issues

**Solutions**:
1. Increase `timeout_seconds` to 15-30s
2. Remove slow upstreams from pool
3. Check network connectivity

---

### No Disagreement Penalties Applied

**Symptom**: Upstreams disagree but scores don't change

**Possible Causes**:
1. `disagreement_penalty = 0.0`
2. Scoring engine not configured
3. Disagreeing upstreams not properly identified

**Solutions**:
1. Set `disagreement_penalty > 0.0` (default 10.0)
2. Verify `ScoringEngine` is initialized
3. Check logs for penalty application

---

### Consensus Overhead Too High

**Symptom**: High latency and upstream load

**Possible Causes**:
1. Too many methods in `methods` list
2. `max_count` too high (5+)
3. Not enough caching

**Solutions**:
1. Reduce `methods` list to critical methods only
2. Lower `max_count` to 2-3
3. Increase cache hit rate to reduce consensus operations

---

**Last Updated**: November 2025
**Configuration Version**: 1.0
