# Consensus Module

**Module**: `crates/prism-core/src/upstream/consensus/`

## Overview

The Consensus module provides quorum-based validation for critical RPC methods by querying multiple upstreams simultaneously and requiring agreement before accepting results. This prevents serving incorrect blockchain data due to network propagation delays, stale nodes, or malicious responses, which is particularly critical for financial applications where incorrect data can lead to monetary losses.

### Why Consensus Matters

**Data Integrity Risks Without Consensus**:

1. **Network Propagation Delays**: Different nodes may see new blocks at different times, leading to inconsistent responses
2. **Stale Nodes**: Nodes that are behind the chain tip may return incomplete or outdated data
3. **Malicious/Faulty Responses**: Compromised or misconfigured nodes may return incorrect data
4. **Chain Reorganizations**: During reorgs, different nodes may temporarily disagree on chain state

**Real-World Impact**:
- **DeFi Applications**: Incorrect `eth_getLogs` can miss transfer events, leading to accounting errors
- **NFT Platforms**: Incorrect ownership data can display the wrong owner
- **Block Explorers**: Inconsistent transaction history damages user trust
- **Price Oracles**: Incorrect `eth_call` results can trigger bad trades

### Integration with Scoring System

The consensus module is tightly integrated with Prism's scoring system:

1. **Automatic Penalty**: Upstreams that disagree with consensus receive score penalties (default 10.0 points)
2. **Future Selection Impact**: Penalized upstreams are less likely to be selected for future requests
3. **Aggregate Metrics**: Disagreement rate tracked via `ScoringEngine.record_error()`
4. **Health Correlation**: Frequent disagreements contribute to overall upstream health score

This creates a feedback loop where reliable upstreams are favored over those that frequently provide divergent data.

## Core Responsibilities

- Query multiple upstreams in parallel for consensus-required RPC methods
- Group responses by hash to identify agreement patterns
- Require minimum quorum of matching responses based on configuration
- Penalize disagreeing nodes through the scoring system
- Provide configurable dispute resolution behaviors for edge cases
- Support runtime configuration updates without service restart
- Track consensus metadata and duration for observability

## Architecture

### Component Structure

```
ConsensusEngine
├── config: Arc<RwLock<ConsensusConfig>>
│   └── Runtime-updatable configuration with consensus rules
├── misbehavior_tracker: Option<Arc<MisbehaviorTracker>>
│   └── Tracks disputes and enforces sit-out for repeatedly disagreeing upstreams
├── query_semaphore: Arc<Semaphore>
│   └── Limits concurrent consensus operations (backpressure)
├── Consensus Execution Flow
│   ├── execute_consensus() - Main entry point
│   ├── find_consensus() - Response grouping and majority detection (delegates to quorum module)
│   └── group_responses() - Hash-based response grouping (delegates to quorum module)
└── Internal Utility Methods (pub(crate))
    ├── hash_response() - Deterministic response hashing (delegates to quorum module)
    ├── responses_match() - Response equality check (delegates to quorum module)
    └── select_block_head_leader() - Block head leader selection
```

### Configuration Enums

**DisputeBehavior** (lines 118-128):
- `PreferBlockHeadLeader`: Use response from most synced upstream (default)
- `ReturnError`: Fail the request and return error to client
- `AcceptAnyValid`: Accept any valid response (no consensus requirement)
- `PreferHighestScore`: Use response from highest-scoring upstream

**FailureBehavior** (lines 137-145):
- `ReturnError`: Fail if any upstream returns an error
- `AcceptAnyValid`: Ignore failures and accept any valid response (default)
- `UseHighestScore`: Use response from highest-scoring upstream

**LowParticipantsBehavior** (lines 154-162):
- `OnlyBlockHeadLeader`: Query only the most synced upstream
- `ReturnError`: Fail if insufficient upstreams available
- `AcceptAvailable`: Proceed with available upstreams (default)

## Configuration

### ConsensusConfig Structure

**Location**: Lines 33-73 in consensus.rs

```rust
pub struct ConsensusConfig {
    /// Enable/disable consensus globally (default: false)
    pub enabled: bool,

    /// Maximum upstreams to query for consensus (default: 3)
    pub max_count: usize,

    /// Minimum agreeing responses required (default: 2)
    pub min_count: usize,

    /// Dispute resolution behavior (default: PreferBlockHeadLeader)
    pub dispute_behavior: DisputeBehavior,

    /// Failure handling behavior (default: AcceptAnyValid)
    pub failure_behavior: FailureBehavior,

    /// Low participants handling (default: AcceptAvailable)
    pub low_participants_behavior: LowParticipantsBehavior,

    /// Methods requiring consensus (default: critical read methods)
    pub methods: Vec<String>,

    /// Score penalty for disagreeing upstreams (default: 10.0)
    pub disagreement_penalty: f64,

    /// Consensus timeout in seconds (default: 10)
    pub timeout_seconds: u64,
}
```

### Default Configuration

**Location**: Lines 101-114 in consensus.rs

- **enabled**: `false` (opt-in feature)
- **max_count**: 3 (query 3 upstreams)
- **min_count**: 2 (require 2 agreeing responses = majority)
- **methods**: `["eth_getBlockByNumber", "eth_getBlockByHash", "eth_getTransactionByHash", "eth_getTransactionReceipt", "eth_getLogs"]`
- **disagreement_penalty**: 10.0 points
- **timeout_seconds**: 10 seconds

### TOML Configuration Examples

#### Conservative (High Safety)

```toml
[consensus]
enabled = true
max_count = 5          # Query 5 upstreams
min_count = 5          # Require unanimous agreement
timeout_seconds = 15
disagreement_penalty = 20.0

dispute_behavior = "ReturnError"
failure_behavior = "ReturnError"
low_participants_behavior = "ReturnError"

methods = [
    "eth_call",
    "eth_getBlockByNumber",
    "eth_getLogs"
]
```

**Use Case**: Financial applications where data accuracy is critical (DeFi, price oracles)

**Trade-offs**:
-  Maximum data integrity
-  Detects any disagreement
-  Higher latency (wait for slowest of 5)
-  5x upstream load
-  Fails frequently if upstreams have minor differences

---

#### Balanced (Recommended)

```toml
[consensus]
enabled = true
max_count = 3          # Query 3 upstreams
min_count = 2          # Majority (2 of 3)
timeout_seconds = 10
disagreement_penalty = 10.0

dispute_behavior = "PreferBlockHeadLeader"
failure_behavior = "AcceptAnyValid"
low_participants_behavior = "AcceptAvailable"

methods = [
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "eth_getLogs"
]
```

**Use Case**: General production deployments balancing safety and performance

**Trade-offs**:
-  Good data integrity (catches most errors)
-  Reasonable latency (wait for 2 of 3)
-  Tolerates one failing upstream
-  3x upstream load (moderate)
- ️ May accept incorrect data if 2 upstreams agree on wrong answer

---

#### Performance (Speed Priority)

```toml
[consensus]
enabled = true
max_count = 2          # Query only 2 upstreams
min_count = 2          # Both must agree
timeout_seconds = 5
disagreement_penalty = 5.0

dispute_behavior = "AcceptAnyValid"
failure_behavior = "AcceptAnyValid"
low_participants_behavior = "AcceptAvailable"

methods = [
    "eth_getBlockByNumber",
    "eth_getLogs"
]
```

**Use Case**: Applications prioritizing low latency over absolute data integrity

**Trade-offs**:
-  Low latency (wait for 2 upstreams)
-  Lower upstream load (2x)
- ️ Limited validation (only 2 sources)
-  No fault tolerance (both must succeed)

---

#### Method-Specific (Selective Consensus)

```toml
[consensus]
enabled = true
max_count = 3
min_count = 2
timeout_seconds = 10

# Only validate critical state-changing queries
methods = [
    "eth_call",           # Smart contract reads (price quotes, balances)
    "eth_getLogs"         # Event logs (critical for DeFi)
]

# Skip consensus for informational queries
# eth_getBlockByNumber - single source is fine for block headers
# eth_blockNumber - chain tip doesn't need consensus
```

**Use Case**: Applications with mixed workloads (some critical, some informational)

**Trade-offs**:
-  Reduced upstream load (only critical methods)
-  Faster responses for non-critical methods
-  Focused validation where it matters most

## Performance Considerations

### Latency Impact

**Without Consensus**:
```
Client Request → Select 1 upstream → Response (200ms)
Total: 200ms
```

**With Consensus (3-way)**:
```
Client Request → Query 3 upstreams in parallel
├─ Upstream A: 150ms 
├─ Upstream B: 200ms 
└─ Upstream C: 350ms 

Wait for all responses → Find consensus → Response
Total: 350ms (slowest upstream)
```

**Best Case**: Same latency as fastest upstream if consensus achieved early
**Typical Case**: Latency of slowest upstream in consensus group
**Worst Case**: `timeout_seconds` if upstreams hang

### Upstream Load

| Configuration | Load Multiplier | Example (1000 req/s) |
|---------------|-----------------|----------------------|
| No consensus | 1x | 1000 req/s |
| max_count = 2 | 2x | 2000 req/s |
| max_count = 3 | 3x | 3000 req/s |
| max_count = 5 | 5x | 5000 req/s |

**Important**: Load increase is per consensus-enabled method. If only 20% of requests use consensus methods, actual impact is lower.

### Cache Interaction

Consensus **only applies to cache misses**:

```
Request Flow:
├─ Cache HIT (95% of requests) → No consensus needed
└─ Cache MISS (5% of requests) → Consensus validation → Cache result
```

**Effective Load**: If cache hit rate is 95% and consensus uses max_count=3:
- Overall load multiplier: 1 + (0.05 * 2) = 1.1x (only 10% increase)

### Optimization Strategies

1. **Enable caching for consensus methods** - Reduces consensus overhead to only first request
2. **Use selective methods list** - Only validate critical RPC methods
3. **Tune min_count** - Lower values allow faster consensus
4. **Increase timeout** - Prevents premature failures at cost of higher latency
5. **Monitor disagreement rates** - High rates indicate upstream issues

## Public API

### Constructor

#### `new(config: ConsensusConfig) -> Self`

**Location**: Lines 254-257 in consensus.rs

Creates a new consensus engine with the given configuration.

**Parameters**:
- `config`: Consensus configuration specifying quorum rules and behaviors

**Returns**: New `ConsensusEngine` instance

**Usage**:
```rust
let config = ConsensusConfig {
    enabled: true,
    max_count: 3,
    min_count: 2,
    ..Default::default()
};

let engine = ConsensusEngine::new(config);
```

### Status Methods

#### `is_enabled() -> bool`

**Location**: Lines 260-262 in consensus.rs

Returns whether consensus validation is currently enabled.

**Usage**:
```rust
if engine.is_enabled().await {
    println!("Consensus is active");
}
```

---

#### `requires_consensus(&self, method: &str) -> bool`

**Location**: Lines 268-271 in consensus.rs

Checks if a specific RPC method requires consensus validation.

**Parameters**:
- `method`: The RPC method name to check

**Returns**: `true` if method is in the consensus methods list and consensus is enabled

**Usage**:
```rust
if engine.requires_consensus("eth_getLogs").await {
    // Use consensus validation
}
```

### Configuration Management

#### `update_config(&self, config: ConsensusConfig)`

**Location**: Lines 274-277 in consensus.rs

Updates the consensus configuration at runtime without restarting the service.

**Parameters**:
- `config`: New consensus configuration

**Usage**:
```rust
let new_config = ConsensusConfig {
    enabled: true,
    max_count: 5,
    min_count: 3,
    ..Default::default()
};

engine.update_config(new_config).await;
```

**Implementation Notes**:
- Acquires write lock on config
- Logs configuration update
- Affects all subsequent requests
- Does not affect in-flight consensus operations

---

#### `get_config() -> ConsensusConfig`

**Location**: Lines 280-282 in consensus.rs

Retrieves the current consensus configuration.

**Returns**: Clone of current configuration

**Usage**:
```rust
let config = engine.get_config().await;
println!("Max count: {}, Min count: {}", config.max_count, config.min_count);
```

### Consensus Execution

#### `execute_consensus(&self, request: &JsonRpcRequest, upstreams: Vec<Arc<UpstreamEndpoint>>, scoring_engine: &Arc<ScoringEngine>) -> Result<ConsensusResult, UpstreamError>`

**Location**: Lines 299-441 in consensus.rs

**PRIMARY METHOD** - Executes a consensus query across multiple upstreams.

Queries up to `max_count` upstreams in parallel, groups responses by hash, requires agreement according to configured rules, and penalizes disagreeing nodes through the scoring system.

**Parameters**:
- `request`: JSON-RPC request to execute
- `upstreams`: List of available upstream endpoints (typically from UpstreamManager)
- `scoring_engine`: Reference to scoring engine for applying penalties

**Returns**:
- `Ok(ConsensusResult)`: Consensus achieved or dispute resolved
- `Err(UpstreamError)`: Consensus cannot be achieved or all upstreams failed

**Error Conditions**:
- `UpstreamError::NoHealthyUpstreams`: No upstreams available
- `UpstreamError::ConsensusFailure`: Insufficient agreement and dispute behavior is `ReturnError`
- `UpstreamError::Timeout`: Consensus timeout exceeded

**Usage**:
```rust
use prism_core::{
    types::JsonRpcRequest,
    upstream::{ConsensusEngine, UpstreamManager, ScoringEngine},
};

let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getBlockByNumber".to_string(),
    params: Some(serde_json::json!(["0x1234", true])),
    id: serde_json::Value::Number(1.into()),
};

let upstreams = upstream_manager.get_healthy_upstreams().await;

match engine.execute_consensus(&request, upstreams, &scoring_engine).await {
    Ok(result) => {
        println!("Response: {:?}", result.response);
        println!("Agreement: {}/{}", result.agreement_count, result.total_queried);

        if !result.disagreeing_upstreams.is_empty() {
            println!("Disagreeing: {:?}", result.disagreeing_upstreams);
        }
    }
    Err(e) => eprintln!("Consensus failed: {}", e),
}
```

**Implementation Flow** (lines 299-441):

1. **Validate Inputs** (lines 308-310)
   - Return error if no upstreams available
   - Check empty upstream list

2. **Handle Low Participants** (lines 313-353)
   - If fewer upstreams than `min_count`:
     - `ReturnError`: Fail immediately
     - `OnlyBlockHeadLeader`: Query most synced upstream only
     - `AcceptAvailable`: Continue with available upstreams

3. **Select Upstreams** (lines 356-357)
   - Take first `max_count` upstreams from provided list
   - Expects caller to provide healthy upstreams

4. **Parallel Execution** (lines 366-386)
   - Send request to all selected upstreams concurrently
   - Apply `timeout_seconds` to entire consensus operation
   - Collect successes and failures separately

5. **Consensus Detection** (lines 419)
   - Delegate to `find_consensus()` for response grouping
   - Hash-based response comparison
   - Majority detection logic

6. **Penalty Application** (lines 422-429)
   - Call `scoring_engine.record_error()` for each disagreeing upstream
   - Log penalty application

7. **Return Result** (lines 434-440)
   - Return `ConsensusResult` with metadata
   - Include duration, agreement count, and disagreeing upstreams

**Performance Notes**:
- Uses `tokio::time::timeout` for overall operation timeout (line 382)
- All upstream queries execute in parallel via `futures_util::future::join_all` (line 382)
- Duration measured from start to finish (lines 305, 432)

### Internal Utility Methods

> **Note**: The following methods are internal (`pub(crate)`) and not part of the public API.
> They are documented here for maintainer reference only.

#### `responses_match(resp1: &JsonRpcResponse, resp2: &JsonRpcResponse) -> bool` *(internal)*

**Location**: Lines 593-595 in consensus.rs

Static method to check if two responses match (have identical content).

**Implementation Notes**:
- Compares response hashes computed by `hash_response()`
- Ignores `id` and `jsonrpc` fields, only compares `result` and `error`
- Uses deterministic hashing for consistent results
- Used internally by `SmartRouter` for consensus group matching

## Result Types

### ConsensusResult

**Location**: Lines 175-197 in consensus.rs

Contains the final response after consensus execution along with detailed metadata.

```rust
pub struct ConsensusResult {
    /// The agreed-upon response or dispute-resolved response
    pub response: JsonRpcResponse,

    /// Number of upstreams that agreed on this response
    pub agreement_count: usize,

    /// Total number of upstreams queried
    pub total_queried: usize,

    /// Whether true consensus was achieved (majority agreement)
    pub consensus_achieved: bool,

    /// Names of upstreams that disagreed with the final response
    pub disagreeing_upstreams: Vec<String>,

    /// Additional metadata about the consensus process
    pub metadata: ConsensusMetadata,
}
```

**Field Details**:

- **response**: The final response to return to the client
- **agreement_count**: How many upstreams returned this exact response
- **total_queried**: How many upstreams were actually queried
- **consensus_achieved**: `true` if `agreement_count >= min_count`, `false` if dispute resolution was used
- **disagreeing_upstreams**: List of upstream names for penalty application
- **metadata**: Additional context about selection method and timing

---

### ConsensusMetadata

**Location**: Lines 200-210 in consensus.rs

Metadata about how consensus was achieved and the response groups observed.

```rust
pub struct ConsensusMetadata {
    /// How the final response was selected
    pub selection_method: SelectionMethod,

    /// All response groups with their counts
    pub response_groups: Vec<ResponseGroup>,

    /// Time taken to achieve consensus in milliseconds
    pub duration_ms: u64,
}
```

**Field Details**:

- **selection_method**: Indicates whether true consensus was reached or dispute resolution was used
- **response_groups**: All unique response groups for debugging and analysis
- **duration_ms**: Total time from consensus start to result (useful for performance monitoring)

---

### SelectionMethod

**Location**: Lines 213-225 in consensus.rs

Enum indicating how the final response was chosen.

```rust
pub enum SelectionMethod {
    Consensus,          // True consensus: majority agreed
    BlockHeadLeader,    // Dispute resolved by block head leader
    HighestScore,       // Dispute resolved by highest score
    FirstValid,         // First valid response accepted
    FallbackError,      // Fallback due to errors
}
```

**Values**:

- **Consensus**: Normal case - `agreement_count >= min_count`
- **BlockHeadLeader**: Dispute behavior selected most synced upstream
- **HighestScore**: Dispute behavior selected highest-scoring upstream
- **FirstValid**: `AcceptAnyValid` behavior selected first valid response
- **FallbackError**: Error handling behavior applied

---

### ResponseGroup

**Location**: Lines 228-241 in consensus.rs

Group of identical responses from multiple upstreams.

```rust
pub struct ResponseGroup {
    /// Hash of the response for grouping
    pub response_hash: u64,

    /// Upstreams that returned this response
    pub upstreams: Vec<String>,

    /// Number of upstreams in this group
    pub count: usize,

    /// The actual response
    pub response: JsonRpcResponse,
}
```

**Usage in Analysis**:
```rust
for group in result.metadata.response_groups {
    println!("Hash {}: {} upstreams agreed", group.response_hash, group.count);
    println!("  Upstreams: {:?}", group.upstreams);
}
```

## Internal Implementation

### Response Hashing

**Location**: Lines 565-587 in consensus.rs

The consensus system uses deterministic hashing to group identical responses.

**Method**: `hash_response(response: &JsonRpcResponse) -> u64`

**Hashing Strategy**:
1. Hash the `result` field by serializing to JSON string (lines 573-577)
2. Hash the `error` field by hashing code and message (lines 581-584)
3. Ignore `id` and `jsonrpc` fields (they vary per request but don't affect data)
4. Use `DefaultHasher` for consistent hash values (line 570)

**Why JSON String Hashing**:
- Ensures identical data produces identical hash regardless of internal representation
- Handles nested objects and arrays correctly
- Works with any JSON-RPC response structure

**Example**:
```rust
let resp1 = JsonRpcResponse {
    result: Some(json!({"number": "0x123"})),
    id: json!(1),  // Different IDs
    ..
};

let resp2 = JsonRpcResponse {
    result: Some(json!({"number": "0x123"})),
    id: json!(2),  // Different IDs
    ..
};

// Same hash because result is identical
assert_eq!(hash_response(&resp1), hash_response(&resp2));
```

---

### Response Grouping

**Location**: Lines 539-563 in consensus.rs

**Method**: `group_responses(responses: Vec<(String, JsonRpcResponse)>) -> Vec<ResponseGroup>`

Converts a list of upstream responses into groups of identical responses.

**Algorithm**:
1. Hash each response using `hash_response()` (line 546)
2. Use HashMap with hash as key to group responses (line 543)
3. For each hash, accumulate upstream names and increment count (lines 548-553)
4. Convert HashMap to Vec of ResponseGroup (line 562)

**Example**:
```rust
let responses = vec![
    ("upstream_a".to_string(), response_1),  // hash: 0xabcd
    ("upstream_b".to_string(), response_1),  // hash: 0xabcd (same)
    ("upstream_c".to_string(), response_2),  // hash: 0x1234 (different)
];

let groups = group_responses(responses);

// groups.len() == 2
// groups[0]: { hash: 0xabcd, upstreams: ["upstream_a", "upstream_b"], count: 2 }
// groups[1]: { hash: 0x1234, upstreams: ["upstream_c"], count: 1 }
```

---

### Consensus Detection

**Location**: Lines 447-537 in consensus.rs

**Method**: `find_consensus(responses: Vec<(String, JsonRpcResponse)>) -> Result<ConsensusResult>`

Core logic for determining if consensus exists and applying dispute resolution.

**Flow**:

1. **Group Responses** (line 458)
   - Call `group_responses()` to create response groups by hash

2. **Sort by Count** (lines 461-462)
   - Sort groups descending by count (largest group first)
   - Largest group represents majority (if it exists)

3. **Check Consensus** (line 468)
   - If `largest_group.count >= min_count` → consensus achieved
   - Otherwise → apply dispute behavior

4. **Consensus Achieved Path** (lines 470-489)
   - Return largest group response
   - Mark all other groups as disagreeing
   - Set `consensus_achieved = true`
   - Set `selection_method = Consensus`

5. **Dispute Resolution Path** (lines 491-536)
   - Apply `dispute_behavior`:
     - `ReturnError`: Return error to client (lines 493-496)
     - `AcceptAnyValid`: Accept largest group (lines 497-514)
     - `PreferBlockHeadLeader` / `PreferHighestScore`: Accept largest group (lines 515-534)
       - *Note*: Full implementation pending (requires additional context)

**Example - Consensus Achieved**:
```rust
// Input: 3 responses, 2 identical
responses = [
    ("A", response_x),  // hash: 0xaaaa
    ("B", response_x),  // hash: 0xaaaa
    ("C", response_y),  // hash: 0xbbbb
]

// Grouping produces:
groups = [
    { hash: 0xaaaa, count: 2, upstreams: ["A", "B"] },
    { hash: 0xbbbb, count: 1, upstreams: ["C"] }
]

// After sorting: largest group has count=2
// If min_count=2: 2 >= 2 → Consensus achieved!

// Result:
ConsensusResult {
    response: response_x,
    agreement_count: 2,
    total_queried: 3,
    consensus_achieved: true,
    disagreeing_upstreams: ["C"],
    metadata: {
        selection_method: SelectionMethod::Consensus,
        ..
    }
}
```

**Example - Dispute Resolution**:
```rust
// Input: 3 responses, all different
responses = [
    ("A", response_x),  // hash: 0xaaaa
    ("B", response_y),  // hash: 0xbbbb
    ("C", response_z),  // hash: 0xcccc
]

// Grouping produces:
groups = [
    { hash: 0xaaaa, count: 1, upstreams: ["A"] },
    { hash: 0xbbbb, count: 1, upstreams: ["B"] },
    { hash: 0xcccc, count: 1, upstreams: ["C"] }
]

// After sorting: largest group has count=1
// If min_count=2: 1 < 2 → No consensus

// Apply dispute_behavior:
// - ReturnError → Err(ConsensusFailure)
// - AcceptAnyValid → Return first response with consensus_achieved=false
```

---

### Block Head Leader Selection

**Location**: Lines 597-621 in consensus.rs

**Method**: `select_block_head_leader(upstreams: &[Arc<UpstreamEndpoint>], scoring_engine: &Arc<ScoringEngine>) -> Option<Arc<UpstreamEndpoint>>`

Selects the upstream with the highest block number (most synced to chain tip).

**Algorithm**:
1. Iterate through all upstreams (line 608)
2. Query scoring engine for block number metrics (lines 610-612)
3. Track upstream with highest block number (lines 613-616)
4. Return best upstream, or first upstream if no metrics available (line 620)

**Usage Context**:
- Called when `low_participants_behavior = OnlyBlockHeadLeader` (line 324)
- Used to select most reliable upstream during low participation

## Integration with UpstreamManager

The consensus module is designed to be called by `UpstreamManager` or `ProxyEngine`:

```rust
// In UpstreamManager or ProxyEngine
pub async fn send_request_with_consensus(
    &self,
    request: &JsonRpcRequest,
) -> Result<JsonRpcResponse, UpstreamError> {
    // Check if consensus is required
    if !self.consensus_engine.requires_consensus(&request.method).await {
        // Use normal single-upstream routing
        return self.send_request(request).await;
    }

    // Get healthy upstreams
    let upstreams = self.get_healthy_upstreams().await;

    // Execute consensus
    let consensus_result = self
        .consensus_engine
        .execute_consensus(request, upstreams, &self.scoring_engine)
        .await?;

    // Return consensus-validated response
    Ok(consensus_result.response)
}
```

**Integration Points**:
1. **Method Check**: Use `requires_consensus()` to determine if consensus needed
2. **Upstream Selection**: Provide healthy upstreams from UpstreamManager
3. **Scoring Integration**: Pass ScoringEngine reference for penalty application
4. **Error Handling**: Handle `UpstreamError` variants appropriately

## Usage Examples

### Basic Consensus Execution

```rust
use prism_core::{
    upstream::{ConsensusEngine, ConsensusConfig},
    types::JsonRpcRequest,
};

// Create consensus engine
let config = ConsensusConfig {
    enabled: true,
    max_count: 3,
    min_count: 2,
    methods: vec!["eth_getLogs".to_string()],
    ..Default::default()
};
let engine = ConsensusEngine::new(config);

// Prepare request
let request = JsonRpcRequest {
    jsonrpc: "2.0".to_string(),
    method: "eth_getLogs".to_string(),
    params: Some(serde_json::json!([{
        "fromBlock": "0x1000",
        "toBlock": "0x2000",
        "address": "0x..."
    }])),
    id: serde_json::Value::Number(1.into()),
};

// Get upstreams and scoring engine
let upstreams = upstream_manager.get_healthy_upstreams().await;
let scoring_engine = upstream_manager.get_scoring_engine();

// Execute consensus
match engine.execute_consensus(&request, upstreams, &scoring_engine).await {
    Ok(result) => {
        println!("Consensus achieved: {}", result.consensus_achieved);
        println!("Agreement: {}/{}", result.agreement_count, result.total_queried);
        println!("Response: {:?}", result.response);
    }
    Err(e) => eprintln!("Consensus failed: {}", e),
}
```

### Handling Consensus Results

```rust
let result = engine.execute_consensus(&request, upstreams, &scoring_engine).await?;

// Check if true consensus was achieved
if result.consensus_achieved {
    println!(" Consensus: {}/{} upstreams agreed",
        result.agreement_count, result.total_queried);
} else {
    println!(" Dispute resolution used: {:?}", result.metadata.selection_method);
}

// Log disagreeing upstreams
if !result.disagreeing_upstreams.is_empty() {
    println!(" Disagreeing upstreams (will be penalized):");
    for upstream_name in &result.disagreeing_upstreams {
        println!("  - {}", upstream_name);
    }
}

// Check response groups
println!("Response groups:");
for (i, group) in result.metadata.response_groups.iter().enumerate() {
    println!("  Group {}: {} upstreams", i + 1, group.count);
    println!("    Upstreams: {:?}", group.upstreams);
    println!("    Hash: 0x{:x}", group.response_hash);
}

println!("Consensus duration: {}ms", result.metadata.duration_ms);
```

### Runtime Configuration Update

```rust
// Start with conservative settings
let engine = ConsensusEngine::new(ConsensusConfig {
    enabled: true,
    max_count: 5,
    min_count: 5,  // Unanimous
    dispute_behavior: DisputeBehavior::ReturnError,
    ..Default::default()
});

// ... application runs ...

// Upstreams prove reliable, relax to majority consensus
let new_config = ConsensusConfig {
    enabled: true,
    max_count: 3,
    min_count: 2,  // Majority
    dispute_behavior: DisputeBehavior::PreferBlockHeadLeader,
    ..Default::default()
};

engine.update_config(new_config).await;
```

### Selective Consensus by Method

```rust
// Only validate critical methods
let config = ConsensusConfig {
    enabled: true,
    methods: vec![
        "eth_call".to_string(),      // Smart contract reads
        "eth_getLogs".to_string(),   // Event logs
    ],
    ..Default::default()
};

let engine = ConsensusEngine::new(config);

// Check before executing
if engine.requires_consensus("eth_getLogs").await {
    // Use consensus
    let result = engine.execute_consensus(&request, upstreams, &scoring_engine).await?;
} else {
    // Use single upstream
    let response = upstream_manager.send_request(&request).await?;
}
```

## Related Components

### ScoringEngine
**File**: `crates/prism-core/src/upstream/scoring.rs`

The consensus module integrates with the scoring system to penalize disagreeing upstreams:

```rust
// Penalty application (line 423 in consensus/engine.rs)
scoring_engine.record_error(upstream_name).await;
```

- **Penalty Impact**: Reduces upstream score, affecting future selection probability
- **Aggregation**: Multiple disagreements compound penalty effect
- **Recovery**: Upstreams can recover score through successful requests

### UpstreamManager
**File**: `crates/prism-core/src/upstream/manager.rs`
**Documentation**: [upstream_manager.md](../upstream/upstream_manager.md)

Provides upstreams for consensus execution and handles request routing:

- **get_healthy_upstreams()**: Provides list of available upstreams
- **Integration Point**: Calls `execute_consensus()` for consensus-required methods
- **Fallback**: Uses single-upstream routing when consensus not needed

### UpstreamEndpoint
**File**: `crates/prism-core/src/upstream/endpoint.rs`

Individual upstream endpoints queried during consensus:

- **send_request()**: Called for each upstream in parallel
- **Health Status**: Only healthy upstreams participate in consensus
- **Metrics**: Provides block number for block head leader selection

## Testing

### Unit Tests

**Location**: Lines 628-801 in consensus.rs

**Test Coverage**:

1. **test_consensus_config_defaults** (lines 635-642)
   - Validates default configuration values
   - Checks enabled=false, max_count=3, min_count=2
   - Verifies default methods list contains 5 critical methods

2. **test_consensus_engine_creation** (lines 644-648)
   - Tests engine creation with default config
   - Validates `is_enabled()` returns false by default

3. **test_requires_consensus** (lines 650-658)
   - Tests method matching logic
   - Validates consensus required for default methods
   - Validates consensus not required for non-default methods

4. **test_hash_response_identical** (lines 660-682)
   - Validates identical responses produce identical hashes
   - Tests that different `id` fields don't affect hash

5. **test_hash_response_different** (lines 684-706)
   - Validates different responses produce different hashes
   - Tests hash sensitivity to result differences

6. **test_responses_match** (lines 708-721)
   - Tests static `responses_match()` method
   - Validates response equality checking

7. **test_group_responses** (lines 723-757)
   - Tests response grouping by hash
   - Validates correct grouping of identical responses
   - Checks disagreeing responses form separate groups

8. **test_hash_response_with_error** (lines 759-789)
   - Tests hashing of error responses
   - Validates identical errors produce identical hashes

9. **test_config_update** (lines 791-800)
   - Tests runtime configuration updates
   - Validates `update_config()` and `get_config()` methods

### Integration Testing

**Recommended Tests** (to be implemented in `crates/tests/`):

```rust
#[tokio::test]
async fn test_consensus_with_agreeing_upstreams() {
    // Setup: 3 upstreams that return identical data
    // Execute: consensus query
    // Assert: consensus achieved, no disagreements
}

#[tokio::test]
async fn test_consensus_with_one_disagreeing_upstream() {
    // Setup: 3 upstreams, 2 agree, 1 disagrees
    // Execute: consensus query
    // Assert: consensus achieved, 1 upstream penalized
}

#[tokio::test]
async fn test_consensus_dispute_resolution() {
    // Setup: 3 upstreams, all return different data
    // Execute: consensus query with AcceptAnyValid
    // Assert: first valid response returned, no consensus achieved
}

#[tokio::test]
async fn test_consensus_low_participants() {
    // Setup: Only 1 healthy upstream, min_count=2
    // Execute: consensus query with AcceptAvailable behavior
    // Assert: accepts single response
}

#[tokio::test]
async fn test_consensus_timeout() {
    // Setup: Upstreams that respond slowly
    // Execute: consensus query with short timeout
    // Assert: timeout error returned
}
```

## Performance Monitoring

### Key Metrics to Track

1. **Consensus Success Rate**: `consensus_achieved` percentage
2. **Disagreement Rate**: Frequency of disagreeing upstreams
3. **Consensus Duration**: `duration_ms` distribution (P50, P95, P99)
4. **Dispute Resolution Usage**: How often dispute behaviors are triggered
5. **Upstream Penalties**: Track score reductions from disagreements

### Logging

The consensus module logs key events:

```rust
// Consensus execution (line 362)
debug!(method = %request.method, upstream_count = query_count, "executing consensus query");

// Query failures (line 398)
warn!(upstream = %name, error = %e, "consensus query failed");

// Configuration updates (line 276)
info!("consensus configuration updated");

// Disagreement penalties (line 427)
debug!(upstream = %upstream_name, penalty = config.disagreement_penalty, "penalizing disagreeing upstream");
```

---

**Last Updated**: November 2025
**Module Version**: 1.0
**Implementation Status**: Production Ready
