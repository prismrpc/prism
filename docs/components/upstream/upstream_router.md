# Upstream Router Documentation

**Module**: `crates/prism-core/src/upstream/router/`

## Overview

The upstream router module provides a trait-based architecture for implementing different request routing strategies. It allows clean separation of concerns between routing logic and upstream management, enabling easy testing and evolution of routing approaches.

The router system offers two main implementations:
1. **SimpleRouter** - Basic round-robin routing with response-time awareness
2. **SmartRouter** - Intelligent routing with automatic strategy selection based on configuration

## Architecture

### RequestRouter Trait

**Location**: Lines 89-113 in router/mod.rs

```rust
#[async_trait]
pub trait RequestRouter: Send + Sync {
    async fn route(
        &self,
        request: &JsonRpcRequest,
        ctx: &RoutingContext,
    ) -> Result<JsonRpcResponse, UpstreamError>;

    fn name(&self) -> &'static str;
}
```

The `RequestRouter` trait defines the interface for all routing implementations. Each router must:
- Implement `route()` to select upstream(s) and return a response
- Provide a human-readable `name()` for logging and debugging
- Be `Send + Sync` for safe concurrent usage

### RoutingContext

**Location**: Lines 24-87 in router/mod.rs

```rust
pub struct RoutingContext {
    pub load_balancer: Arc<LoadBalancer>,
    pub scoring_engine: Arc<ScoringEngine>,
    pub consensus_engine: Arc<ConsensusEngine>,
    pub hedger: Arc<HedgeExecutor>,
    pub chain_state: Arc<ChainState>,
}
```

The `RoutingContext` aggregates all dependencies needed for routing decisions, avoiding the need to pass multiple parameters to each router method.

**Helper Methods**:

#### get_capable_upstreams()

**Location**: Lines 61-81

Returns healthy upstreams filtered by block availability if the request targets a specific block number. Falls back to all healthy upstreams if block-specific filtering returns empty (prevents spurious errors during reorgs).

```rust
let capable = ctx.get_capable_upstreams(request).await;
```

#### has_healthy_upstreams()

**Location**: Lines 84-86

Quick check for upstream availability without filtering.

```rust
if !ctx.has_healthy_upstreams().await {
    return Err(UpstreamError::NoHealthyUpstreams);
}
```

## SimpleRouter

**Location**: `crates/prism-core/src/upstream/router/simple.rs`

The `SimpleRouter` provides basic response-time-based routing. It's the fallback strategy when more sophisticated routing (consensus, scoring, hedging) is not configured.

### Architecture

```
Request → SimpleRouter
    ↓
Select upstream by response time (LoadBalancer)
    ↓
Send request with timeout
    ↓
Record metrics (success/error/throttle)
    ↓
Return response
```

### Implementation

**Key Method**: `route()` (lines 34-76 in simple.rs)

**Algorithm**:
1. Get next upstream by response time from `LoadBalancer`
2. Send request with 15-second timeout
3. Record latency and outcome to `ScoringEngine`
4. Extract and record block number if available
5. Return response or error

**Error Handling**:
- HTTP 429 errors → `record_throttle()`
- Other errors → `record_error()`
- Success → `record_success()` with latency

### When to Use

SimpleRouter is appropriate for:
- Development environments
- Simple deployments without advanced features
- Fallback when all other routing strategies are disabled
- Testing and debugging routing behavior

### Example Usage

```rust
use prism_core::upstream::router::{SimpleRouter, RoutingContext};

let router = SimpleRouter::new();
let response = router.route(&request, &ctx).await?;
```

### Performance

- **Latency**: Single upstream request + metric recording (~5-10µs overhead)
- **Throughput**: Limited by upstream capacity
- **Load Pattern**: Round-robin based on average response time

## SmartRouter

**Location**: `crates/prism-core/src/upstream/router/smart.rs`

The `SmartRouter` automatically selects the best routing strategy based on request characteristics and configuration. It implements a priority-based decision tree that maximizes data integrity while maintaining performance.

### Routing Priority Order

**Location**: Lines 256-285 in smart.rs

```
1. Consensus (if required for method)
   ↓ (not required)
2. Scoring-based selection (if enabled)
   ↓ (disabled)
3. Hedged requests (if enabled and scoring disabled)
   ↓ (disabled)
4. Simple response-time selection (fallback)
```

### Priority 1: Consensus Routing

**Location**: Lines 28-72 in smart.rs

Used when the request method requires consensus validation (e.g., `eth_getLogs`, `eth_getBlockByNumber`).

**Algorithm**:
1. Get capable upstreams for the request
2. Execute consensus across multiple upstreams
3. Wait for quorum agreement
4. Record success for all agreeing upstreams
5. Extract and record block numbers
6. Return consensus response

**When Active**:
- `consensus.enabled = true`
- Request method in `consensus.methods` list

**Example Flow**:
```
eth_getLogs request
    ↓
Check: requires_consensus("eth_getLogs") → true
    ↓
Query 3 upstreams in parallel
    ↓
Response A, B, C
    ↓
2 match (A == B) → Consensus achieved
    ↓
Return A, penalize C
```

### Priority 2: Scoring-Based Selection

**Location**: Lines 74-138 in smart.rs

Used when scoring is enabled to select the best-performing upstream based on composite scores.

**Algorithm**:
1. Get capable upstreams for the request
2. Select top-N upstreams by score
3. Pick best from top-N using `LoadBalancer`
4. Send request with 15-second timeout
5. Record metrics based on outcome
6. Return response

**When Active**:
- `scoring.enabled = true`
- Consensus not required for this method

**Scoring Factors**:
- Latency (P90)
- Error rate
- Throttle rate
- Block lag
- Total requests

**Example Flow**:
```
eth_call request
    ↓
Check: scoring.enabled → true
    ↓
Get ranked upstreams: [Alchemy: 95.2, Infura: 87.1, QuickNode: 76.5]
    ↓
Select from top 3
    ↓
Route to Alchemy
    ↓
Record latency: 120ms
```

### Priority 3: Hedged Requests

**Location**: Lines 140-189 in smart.rs

Used when hedging is enabled to improve tail latency by sending backup requests if the primary is slow.

**Algorithm**:
1. Get capable upstreams for the request
2. Execute hedged request strategy
3. Send primary request
4. If primary exceeds P95 latency, send hedge to backup
5. Return first successful response
6. Record metrics for primary upstream

**When Active**:
- `hedging.enabled = true`
- Scoring is disabled
- Consensus not required for this method

**Example Flow**:
```
eth_getBalance request
    ↓
Check: hedging.enabled → true
    ↓
Send to Infura (primary)
    ↓
Wait P95 latency (180ms)
    ↓
Primary not complete → Send hedge to Alchemy
    ↓
Alchemy responds first (150ms total)
    ↓
Cancel Infura, return Alchemy result
```

### Priority 4: Simple Response-Time Selection

**Location**: Lines 191-246 in smart.rs

Fallback strategy when all other approaches are disabled or not applicable.

**Algorithm**:
1. Get next upstream by response time
2. Send request with timeout
3. Record metrics
4. Return response

**When Active**:
- All other strategies disabled
- Always available as fallback

### Routing Decision Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                         SmartRouter                              │
└────────────┬────────────────────────────────────────────────────┘
             │
             ▼
      ┌─────────────┐
      │  Request    │
      └──────┬──────┘
             │
             ▼
    ┌────────────────┐
    │ Has healthy    │───No───► Return NoHealthyUpstreams
    │ upstreams?     │
    └────────┬───────┘
             │ Yes
             ▼
    ┌────────────────┐
    │ Requires       │───Yes──► route_with_consensus()
    │ consensus?     │
    └────────┬───────┘
             │ No
             ▼
    ┌────────────────┐
    │ Scoring        │───Yes──► route_with_scoring()
    │ enabled?       │
    └────────┬───────┘
             │ No
             ▼
    ┌────────────────┐
    │ Hedging        │───Yes──► route_with_hedge()
    │ enabled?       │
    └────────┬───────┘
             │ No
             ▼
    ┌────────────────┐
    │ Simple routing │
    │ (fallback)     │
    └────────────────┘
```

### Configuration-Driven Routing

The SmartRouter's behavior is entirely configuration-driven:

```toml
# Enable consensus for critical methods
[consensus]
enabled = true
methods = ["eth_getLogs", "eth_getBlockByNumber"]

# Enable scoring for intelligent selection
[scoring]
enabled = true
weights.latency = 8.0
weights.error_rate = 4.0

# Enable hedging for tail latency
[hedging]
enabled = true
latency_quantile = 0.95
```

### Example Routing Scenarios

#### Scenario 1: All Features Enabled

```toml
[consensus]
enabled = true
methods = ["eth_getLogs"]

[scoring]
enabled = true

[hedging]
enabled = true
```

**Routing**:
- `eth_getLogs` → Consensus (priority 1)
- `eth_call` → Scoring (priority 2)
- Other methods → Scoring (priority 2)

#### Scenario 2: Scoring Only

```toml
[consensus]
enabled = false

[scoring]
enabled = true

[hedging]
enabled = false
```

**Routing**:
- All methods → Scoring (priority 2)

#### Scenario 3: Hedging Only

```toml
[consensus]
enabled = false

[scoring]
enabled = false

[hedging]
enabled = true
```

**Routing**:
- All methods → Hedging (priority 3)

#### Scenario 4: Simple Fallback

```toml
[consensus]
enabled = false

[scoring]
enabled = false

[hedging]
enabled = false
```

**Routing**:
- All methods → Simple (priority 4)

## Block Availability Filtering

### extract_requested_block()

**Location**: Lines 115-150 in router/mod.rs

Extracts the requested block number from a JSON-RPC request for block availability pre-filtering.

**Supported Methods**:
- `eth_getBlockByNumber` → First param (block number/tag)
- `eth_getBalance`, `eth_getCode` → Second param (block number/tag)
- `eth_getLogs` → `toBlock` from filter object (uses higher bound)
- Other methods → Returns `None` (no block-specific filtering)

**Example**:
```rust
// eth_getBlockByNumber request with params ["0xABCDEF", true]
let block = extract_requested_block(&request);  // Some(11259375)

// eth_getLogs with toBlock: "0x1000000"
let block = extract_requested_block(&request);  // Some(16777216)

// eth_blockNumber (no block param)
let block = extract_requested_block(&request);  // None
```

### Block Parameter Parsing

**Location**: Lines 156-161 in router/mod.rs

Parses block parameter values (hex strings or tags) into block numbers:
- Hex strings (e.g., "0x1000") → Parsed as u64
- Tags (e.g., "latest", "pending") → Returns `None` (no specific block requirement)

### extract_block_number()

**Location**: Lines 169-181 in router/mod.rs

Extracts block number from JSON-RPC responses for metric recording.

**Supported Methods**:
- `eth_blockNumber` → Response value
- `eth_getBlockByNumber` → `result.number` field
- `eth_getBlockByHash` → `result.number` field

## Error Categorization

The router uses sophisticated error categorization to determine scoring penalties:

```rust
match error {
    UpstreamError::HttpError(429, _) => {
        // HTTP 429: Rate limit
        scoring_engine.record_throttle(&upstream_name).await;
    }
    UpstreamError::RpcError(code, message) => {
        let category = RpcErrorCategory::from_code_and_message(*code, message);
        if matches!(category, RpcErrorCategory::RateLimit) {
            scoring_engine.record_throttle(&upstream_name).await;
        } else if category.should_penalize_upstream() {
            scoring_engine.record_error(&upstream_name).await;
        }
        // Client errors and execution errors don't affect scoring
    }
    _ => {
        scoring_engine.record_error(&upstream_name).await;
    }
}
```

**Error Categories**:
- **Rate Limit** → Throttle penalty (exponential)
- **Upstream Errors** → Error penalty (linear)
- **Client Errors** → No penalty (user's fault)
- **Execution Errors** → No penalty (transaction reverted, not upstream issue)

## Testing

### Router Tests

**Location**: Lines 84-92 in simple.rs, 294-301 in smart.rs

Basic tests verify router names:
```rust
#[test]
fn test_simple_router_name() {
    let router = SimpleRouter::new();
    assert_eq!(router.name(), "simple");
}

#[test]
fn test_smart_router_name() {
    let router = SmartRouter::new();
    assert_eq!(router.name(), "smart");
}
```

### Priority Tests

**Location**: Lines 312-617 in smart.rs

Comprehensive tests verify the routing priority order:

#### test_priority_1_consensus_required_takes_precedence (lines 358-377)

Verifies consensus takes priority when enabled and method is in the consensus list.

#### test_priority_2_scoring_enabled_takes_precedence_over_hedging (lines 418-440)

Verifies scoring takes priority over hedging when both are enabled.

#### test_priority_3_hedging_enabled_takes_precedence (lines 458-471)

Verifies hedging takes priority over simple routing when enabled.

#### test_priority_cascade_documentation (lines 526-563)

Documents and tests the complete priority cascade:
1. Consensus (for consensus methods)
2. Scoring (regardless of hedging)
3. Hedging (only if scoring disabled)
4. Simple (fallback)

#### test_per_method_consensus_configuration (lines 567-583)

Verifies per-method consensus configuration works correctly.

#### test_runtime_config_updates (lines 587-599)

Verifies runtime configuration updates affect routing decisions immediately.

### Running Tests

```bash
# Run all router tests
cargo test -p prism-core router::

# Run priority tests specifically
cargo test -p prism-core router::smart::priority_tests

# Run with output
cargo test -p prism-core router:: -- --nocapture
```

## Usage Examples

### Basic SimpleRouter Usage

```rust
use prism_core::upstream::router::{SimpleRouter, RoutingContext};

let router = SimpleRouter::new();
let ctx = RoutingContext::new(
    load_balancer,
    scoring_engine,
    consensus_engine,
    hedger,
    chain_state,
);

let response = router.route(&request, &ctx).await?;
```

### SmartRouter with Full Configuration

```rust
use prism_core::upstream::router::{SmartRouter, RoutingContext};

// SmartRouter automatically selects best strategy
let router = SmartRouter::new();

// Route request (strategy selected based on config)
let response = router.route(&request, &ctx).await?;
```

### Custom Router Implementation

```rust
use async_trait::async_trait;
use prism_core::upstream::router::{RequestRouter, RoutingContext};

struct CustomRouter;

#[async_trait]
impl RequestRouter for CustomRouter {
    async fn route(
        &self,
        request: &JsonRpcRequest,
        ctx: &RoutingContext,
    ) -> Result<JsonRpcResponse, UpstreamError> {
        // Custom routing logic
        let upstream = ctx.load_balancer.get_next_healthy_by_response_time().await?;
        upstream.send_request(request).await
    }

    fn name(&self) -> &'static str {
        "custom"
    }
}
```

## Performance Considerations

### SimpleRouter

- **Overhead**: ~5-10µs per request (metric recording)
- **Latency**: Single upstream request
- **Throughput**: Limited by upstream capacity
- **Memory**: Minimal (~16 bytes for router state)

### SmartRouter - Consensus Mode

- **Overhead**: ~50-200µs (parallel requests + consensus logic)
- **Latency**: Slowest of quorum upstreams
- **Throughput**: Reduced by factor of `max_count`
- **Memory**: ~1KB per concurrent consensus operation

### SmartRouter - Scoring Mode

- **Overhead**: ~10-20µs (score lookup + metric recording)
- **Latency**: Single upstream request
- **Throughput**: Optimal (routes to fastest upstreams)
- **Memory**: ~8KB per upstream (latency tracking)

### SmartRouter - Hedging Mode

- **Overhead**: ~5-15µs (latency tracking)
- **Latency**: Best of primary + hedge (typically P95 → P50)
- **Throughput**: Increased load by ~5-10% (hedged requests)
- **Memory**: ~8KB per upstream (latency tracking)

## Related Components

- **LoadBalancer** (`docs/components/upstream/load_balancer.md`) - Upstream selection
- **ConsensusEngine** (`docs/components/consensus/consensus_engine.md`) - Quorum validation
- **ScoringEngine** (`docs/components/scoring/scoring_engine.md`) - Performance tracking
- **HedgeExecutor** (`docs/components/hedging/hedge_executor.md`) - Tail latency reduction
- **Consensus Configuration** (`docs/components/consensus/configuration.md`) - Consensus settings
- **Scoring Configuration** (`docs/components/scoring/configuration.md`) - Scoring settings
- **Hedging Configuration** (`docs/components/hedging/configuration.md`) - Hedging settings

## References

- **Router Trait**: `crates/prism-core/src/upstream/router/mod.rs` (lines 89-113)
- **RoutingContext**: `crates/prism-core/src/upstream/router/mod.rs` (lines 24-87)
- **SimpleRouter**: `crates/prism-core/src/upstream/router/simple.rs` (lines 1-93)
- **SmartRouter**: `crates/prism-core/src/upstream/router/smart.rs` (lines 1-618)
- **Priority Tests**: `crates/prism-core/src/upstream/router/smart.rs` (lines 312-617)
