# ChainState Documentation

**Module**: `crates/prism-core/src/chain/state.rs`

## Overview

`ChainState` provides unified chain tip tracking for the Prism RPC aggregator. It serves as the single source of truth for the current blockchain state, including the chain tip block number, finalized block, and head hash. This ensures all components (`CacheManager`, `ReorgManager`, `ScoringEngine`, `HealthChecker`) have a consistent view of the chain state without maintaining independent tracking.

## Architecture: Shared Ownership Pattern

`ChainState` uses a **shared ownership pattern** where multiple components hold `Arc<ChainState>` references to the same instance. This design is intentional and optimal for performance and consistency.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         ChainState Ownership Graph                          │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│                          ┌─────────────────┐                                │
│                          │   ChainState    │                                │
│                          │  (single inst)  │                                │
│                          └────────┬────────┘                                │
│                                   │                                         │
│                    Arc<ChainState> shared by:                               │
│          ┌────────────────┬───────┴───────┬────────────────┐                │
│          │                │               │                │                │
│          ▼                ▼               ▼                ▼                │
│  ┌──────────────┐ ┌─────────────┐ ┌─────────────┐ ┌──────────────┐          │
│  │ CacheManager │ │ReorgManager │ │ScoringEngine│ │HealthChecker │          │
│  │              │ │             │ │             │ │ (via Reorg)  │          │
│  │ reads tip,   │ │ WRITES tip, │ │ reads tip,  │ │ triggers     │          │
│  │ finalized    │ │ finalized,  │ │ updates tip │ │ rollback     │          │
│  │              │ │ head hash   │ │ on blocks   │ │ detection    │          │
│  └──────────────┘ └─────────────┘ └─────────────┘ └──────────────┘          │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Why This Pattern?

1. **Single source of truth**: All components see the same chain state
2. **Lock-free reads**: Uses `SeqLock` pattern for P99 read latency < 50ns
3. **Single writer guarantee**: Only `ReorgManager` writes to tip/hash; `ScoringEngine` only updates tip number from block observations
4. **Memory efficient**: One `ChainState` instance shared across entire system
5. **No hidden dependencies**: Explicit `Arc<ChainState>` at construction

## SeqLock Pattern for Optimistic Lock-Free Reads

The core innovation in `ChainState` is the `SeqLock` pattern (lines 25-98 in state.rs), which provides:
- **Lock-free reads** in the common case
- **Atomic updates** to block number and hash together
- **TOCTOU protection** via sequence number validation

### SeqLock Mechanism

```rust
struct SeqLock<T> {
    sequence: AtomicU64,  // Even = stable, Odd = write in progress
    data: parking_lot::RwLock<T>,
}
```

**Read Algorithm** (lines 51-76 in state.rs):
```
1. Read sequence (must be even = stable)
2. If odd, spin and retry
3. Read data (optimistic - no lock)
4. Read sequence again
5. If sequences match → consistent read, return
6. Otherwise → concurrent write detected, retry
```

**Write Algorithm** (lines 81-97 in state.rs):
```
1. Increment sequence (mark write start: now odd)
2. Acquire write lock
3. Modify data
4. Release write lock
5. Increment sequence (mark write complete: now even)
```

This design provides **P99 read latency < 50ns** compared to 1-10µs for async `RwLock` under contention.

## Key Types

### ChainState

**Location**: Lines 138-152 in state.rs

```rust
pub struct ChainState {
    tip: Arc<SeqLock<ChainTip>>,
    tip_write_lock: Arc<RwLock<()>>,
    finalized_block: Arc<AtomicU64>,
}
```

**Fields**:
- `tip`: Current chain tip (block number + hash) protected by `SeqLock`
- `tip_write_lock`: Async `RwLock` for coordinating async updates (writers only)
- `finalized_block`: Finalized block number (beyond which reorgs are impossible)

**Clone Behavior**: `ChainState` implements `Clone` by cloning the `Arc` references, making clones cheap (just incrementing reference counts).

### ChainTip

**Location**: Lines 20-23 in state.rs

```rust
struct ChainTip {
    block_number: u64,
    block_hash: [u8; 32],
}
```

Private struct containing the block number and hash that must be updated atomically to prevent TOCTOU races.

## Performance Characteristics

| Operation | Typical Latency | Notes |
|-----------|-----------------|-------|
| `current_tip()` | < 50ns P99 | Lock-free optimistic read |
| `finalized_block()` | < 10ns | Single atomic load |
| `update_tip()` | ~1-5µs | Async lock + `SeqLock` write |
| `current_tip_with_hash()` | < 50ns P99 | Atomic read of both values |

**Memory Footprint**:
- Per `ChainState` instance: ~80 bytes
- `SeqLock<ChainTip>`: ~56 bytes (sequence + RwLock + data)
- `AtomicU64`: 8 bytes
- `Arc` overhead: ~16 bytes

## Thread Safety

### Read Operations
- `current_tip()` (line 171)
- `finalized_block()` (line 180)
- `current_head_hash()` (line 190)
- `current_tip_with_hash()` (line 201)
- `safe_head()` (line 382)

All read operations are **lock-free** using optimistic `SeqLock` reads or simple atomic loads. They never block and automatically retry on write conflicts.

### Write Operations
- `update_tip()` (line 232)
- `force_update_tip()` (line 259)
- `force_update_tip_simple()` (line 272)
- `update_head_hash()` (line 284)
- `update_tip_simple()` (line 301)
- `update_finalized()` (line 327)

Write operations are **coordinated through async locks** to serialize updates. The `ReorgManager` holds the async mutex to ensure write ordering.

## API Reference

### new()

**Signature**: `pub fn new() -> Self`
**Location**: Line 157

Creates a new `ChainState` with all values initialized to zero/empty.

**Example**:
```rust
let chain_state = Arc::new(ChainState::new());
```

### current_tip()

**Signature**: `pub fn current_tip(&self) -> u64`
**Location**: Line 171

Returns the current chain tip block number using lock-free optimistic read.

**Example**:
```rust
let tip = chain_state.current_tip();
println!("Chain tip: {}", tip);
```

**Performance**: Typical P99 latency < 50ns. Never blocks.

### finalized_block()

**Signature**: `pub fn finalized_block(&self) -> u64`
**Location**: Line 180

Returns the finalized block number. Blocks at or below this number are finalized and cannot be reorged.

**Example**:
```rust
let finalized = chain_state.finalized_block();
if block_number <= finalized {
    // Safe from reorgs
}
```

### current_head_hash()

**Signature**: `pub fn current_head_hash(&self) -> [u8; 32]`
**Location**: Line 190

Returns the current head block hash using lock-free optimistic read.

**Example**:
```rust
let hash = chain_state.current_head_hash();
```

### current_tip_with_hash()

**Signature**: `pub fn current_tip_with_hash(&self) -> (u64, [u8; 32])`
**Location**: Line 201

Returns both the current tip block number and hash atomically. Use this when you need both values and want to ensure consistency.

**Example**:
```rust
let (tip, hash) = chain_state.current_tip_with_hash();
println!("Block {} with hash {:?}", tip, hash);
```

**Atomicity Guarantee**: The returned block number and hash are guaranteed to correspond to the same chain state update.

### update_tip()

**Signature**: `pub async fn update_tip(&self, block_number: u64, block_hash: [u8; 32]) -> bool`
**Location**: Line 232

Updates the chain tip with a new block number and hash. Only updates if the new block is newer than the current tip.

**Returns**: `true` if the tip was updated, `false` if the new block is not newer.

**Example**:
```rust
let updated = chain_state.update_tip(1000, [1u8; 32]).await;
if updated {
    println!("Tip updated to block 1000");
}
```

**Safety**: The update is atomic - both block number and hash are updated together under `SeqLock` protection.

### force_update_tip()

**Signature**: `pub async fn force_update_tip(&self, block_number: u64, block_hash: [u8; 32])`
**Location**: Line 259

Forces the chain tip to a specific block, even if lower than current. Used for rollback scenarios.

**Example**:
```rust
// Reorg detected, roll back to block 999
chain_state.force_update_tip(999, reorg_hash).await;
```

**Warning**: Only use this for verified rollback scenarios. Normal forward progression should use `update_tip()`.

### force_update_tip_simple()

**Signature**: `pub async fn force_update_tip_simple(&self, block_number: u64)`
**Location**: Line 272

Forces the tip to a specific block number without updating the hash. Preserves the existing hash.

**Example**:
```rust
// Rollback without hash
chain_state.force_update_tip_simple(999).await;
```

### update_head_hash()

**Signature**: `pub async fn update_head_hash(&self, block_hash: [u8; 32])`
**Location**: Line 284

Updates only the head hash at the same block height. Used when a reorg is detected at the current tip - the block number stays the same but the hash changes to the new canonical chain's block.

**Example**:
```rust
// Reorg at current tip
chain_state.update_head_hash(new_canonical_hash).await;
```

### update_tip_simple()

**Signature**: `pub async fn update_tip_simple(&self, block_number: u64) -> bool`
**Location**: Line 301

Updates only the chain tip block number without updating the hash. Only updates if the new block is newer than the current tip.

**Returns**: `true` if the tip was updated, `false` if the new block is not newer.

**Example**:
```rust
// Update from eth_blockNumber response (hash unknown)
let updated = chain_state.update_tip_simple(block_number).await;
```

### update_finalized()

**Signature**: `pub async fn update_finalized(&self, block: u64) -> bool`
**Location**: Line 327

Updates the finalized block number. Only updates if the new finalized block is higher than the current one AND does not exceed the current chain tip (blockchain invariant).

**Returns**: `true` if the finalized block was updated, `false` otherwise.

**Implementation**: Uses a CAS (Compare-And-Swap) loop to prevent race conditions and maintain the invariant `finalized <= tip`.

**Example**:
```rust
let updated = chain_state.update_finalized(995).await;
if updated {
    println!("Finalized block updated to 995");
}
```

**Invariant Enforcement** (lines 332-334):
```rust
if block <= current_finalized || block > current_tip {
    return false;  // Reject invalid updates
}
```

### safe_head()

**Signature**: `pub fn safe_head(&self, safety_depth: u64) -> u64`
**Location**: Line 382

Returns the safe head block number (tip minus safety depth). Blocks at or below this height are considered safe from reorgs based on the configured safety depth.

**Example**:
```rust
// With safety depth of 12 blocks
let safe = chain_state.safe_head(12);
// If tip is 1000, safe head is 988
```

**Overflow Protection**: Uses `saturating_sub()` to prevent underflow.

## Usage Pattern

### Initialization at Startup

Create one `ChainState` and share it across all components:

```rust
use std::sync::Arc;
use prism_core::chain::ChainState;

// Create single instance
let chain_state = Arc::new(ChainState::new());

// Share with all components
let cache_manager = CacheManager::new(&config, chain_state.clone());
let reorg_manager = ReorgManager::new(config, chain_state.clone(), cache_manager);
let scoring_engine = ScoringEngine::new(config, chain_state.clone());
let health_checker = HealthChecker::new(config, reorg_manager); // Gets ChainState via ReorgManager
```

### Reading Chain State

```rust
// Lock-free reads (hot path)
let tip = chain_state.current_tip();
let finalized = chain_state.finalized_block();

// Atomic read of both values
let (tip, hash) = chain_state.current_tip_with_hash();

// Calculate safe head for cache operations
let safe_head = chain_state.safe_head(config.safety_depth);
```

### Updating Chain State (ReorgManager only)

```rust
// Normal forward progression
if let Some(new_block) = fetch_latest_block().await {
    chain_state.update_tip(new_block.number, new_block.hash).await;
}

// Rollback on reorg detection
if reorg_detected {
    chain_state.force_update_tip(reorg_target_block, reorg_hash).await;
}

// Update finalized block
if let Some(finalized) = fetch_finalized_block().await {
    chain_state.update_finalized(finalized).await;
}
```

## Anti-patterns: What NOT to Do

### Don't Create Multiple Instances

```rust
//  WRONG - Components have inconsistent views
let cache_chain_state = Arc::new(ChainState::new());
let reorg_chain_state = Arc::new(ChainState::new());
```

```rust
//  CORRECT - Share single instance
let chain_state = Arc::new(ChainState::new());
let cache_manager = CacheManager::new(&config, chain_state.clone());
let reorg_manager = ReorgManager::new(config, chain_state.clone(), cache_manager);
```

### Don't Wrap in Another Arc

```rust
//  WRONG - Adds unnecessary indirection
let chain_state = Arc::new(ChainState::new());
let double_arc = Arc::new(chain_state);
```

```rust
//  CORRECT - Clone the Arc directly
let chain_state = Arc::new(ChainState::new());
let cache_manager = CacheManager::new(&config, chain_state.clone());
```

### Don't Use Global Singleton

```rust
//  WRONG - Makes testing difficult
static CHAIN_STATE: OnceCell<Arc<ChainState>> = OnceCell::new();
```

```rust
//  CORRECT - Explicit dependency injection
fn new_cache_manager(chain_state: Arc<ChainState>) -> CacheManager {
    CacheManager::new(&config, chain_state)
}
```

### Don't Pass by Value Without Arc

```rust
//  WRONG - ChainState doesn't implement Clone (the struct itself)
fn process(chain_state: ChainState) { }
```

```rust
//  CORRECT - Pass Arc by clone (cheap reference count increment)
fn process(chain_state: Arc<ChainState>) { }
```

## Shared Ownership by Component

### CacheManager

**Reads**:
- `current_tip()` - For determining cache validity windows
- `finalized_block()` - For safe eviction boundaries
- `safe_head()` - For calculating reorg-safe cache boundaries

**Writes**: None

### ReorgManager

**Reads**:
- `current_tip_with_hash()` - For detecting chain state changes

**Writes**:
- `update_tip()` - Normal forward block progression
- `force_update_tip()` - Rollback on reorg detection
- `update_head_hash()` - Reorg at current tip
- `update_finalized()` - Finality updates from chain

### ScoringEngine

**Reads**:
- `current_tip()` - For calculating block lag penalties

**Writes**:
- `update_tip_simple()` - Updates tip from observed block numbers (does not update hash)

### HealthChecker (via ReorgManager)

**Reads**:
- `current_tip()` - For comparing against upstream block numbers

**Writes**: None (triggers ReorgManager to write)

## Concurrency Tests

The implementation includes comprehensive property-based concurrency tests (lines 549-755 in state.rs):

### test_property_concurrent_updates_preserve_consistency (lines 562-638)

**Property**: Concurrent updates never produce inconsistent (tip, hash) pairs.

Spawns 50 concurrent writers and 10 readers, verifying that readers never observe a tip from one update paired with a hash from a different update.

### test_property_finalized_never_exceeds_tip (lines 644-661)

**Property**: Finalized block is always <= current tip.

Verifies the invariant holds regardless of update order.

### test_property_finalized_monotonically_increases (lines 665-685)

**Property**: Finalized can only increase, never decrease.

### test_property_force_update_maintains_consistency (lines 691-712)

**Property**: `force_update_tip()` maintains consistency even during rollbacks.

### test_property_concurrent_force_updates_preserve_consistency (lines 717-754)

**Property**: Concurrent force updates (including rollbacks) maintain consistency.

Tests 10 iterations with 30 concurrent tasks alternating between high and low block numbers.

## Related Components

- **ReorgManager** (`docs/components/reorg/reorg_manager.md`) - Primary writer to ChainState
- **CacheManager** (`docs/components/cache/cache_manager.md`) - Uses ChainState for cache invalidation
- **ScoringEngine** (`docs/components/scoring/scoring_engine.md`) - Uses ChainState for block lag calculation
- **HealthChecker** (`docs/components/upstream/upstream_health.md`) - Monitors chain progression

## Testing

Run ChainState tests:
```bash
cargo test -p prism-core chain::state::tests
```

Run concurrency tests specifically:
```bash
cargo test -p prism-core chain::state::tests::test_property
```

## References

- **Module Documentation**: `crates/prism-core/src/chain/mod.rs` (lines 1-90)
- **Implementation**: `crates/prism-core/src/chain/state.rs` (lines 1-756)
- **SeqLock Pattern**: Lines 25-98 in state.rs
- **Concurrency Tests**: Lines 549-755 in state.rs
