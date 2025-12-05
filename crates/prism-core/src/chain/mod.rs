//! Chain state management module.
//!
//! Provides a unified [`ChainState`] component that tracks the canonical chain tip,
//! finalized block, and current head hash. This ensures all components have a
//! consistent view of the chain state.
//!
//! # Architecture: Shared Ownership Pattern
//!
//! `ChainState` uses a **shared ownership pattern** where multiple components hold
//! `Arc<ChainState>` references to the same instance. This is intentional and optimal:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                         ChainState Ownership Graph                          │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │                          ┌─────────────────┐                                │
//! │                          │   ChainState    │                                │
//! │                          │  (single inst)  │                                │
//! │                          └────────┬────────┘                                │
//! │                                   │                                         │
//! │                    Arc<ChainState> shared by:                               │
//! │          ┌────────────────┬───────┴───────┬────────────────┐                │
//! │          │                │               │                │                │
//! │          ▼                ▼               ▼                ▼                │
//! │  ┌──────────────┐ ┌─────────────┐ ┌─────────────┐ ┌──────────────┐          │
//! │  │ CacheManager │ │ReorgManager │ │ScoringEngine│ │HealthChecker │          │
//! │  │              │ │             │ │             │ │ (via Reorg)  │          │
//! │  │ reads tip,   │ │ WRITES tip, │ │ reads tip,  │ │ triggers     │          │
//! │  │ finalized    │ │ finalized,  │ │ updates tip │ │ rollback     │          │
//! │  │              │ │ head hash   │ │ on blocks   │ │ detection    │          │
//! │  └──────────────┘ └─────────────┘ └─────────────┘ └──────────────┘          │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Why This Pattern?
//!
//! 1. **Single source of truth**: All components see the same chain state
//! 2. **Wait-free reads**: Uses `ArcSwap` double-buffer pattern for ~1-2ns read latency with zero
//!    contention
//! 3. **Single writer guarantee**: Only `ReorgManager` writes to tip/hash; `ScoringEngine` only
//!    updates tip number from block observations
//! 4. **Memory efficient**: One `ChainState` instance shared across entire system
//! 5. **No hidden dependencies**: Explicit `Arc<ChainState>` at construction
//!
//! ## Thread Safety
//!
//! - **Reads** (via `current_tip()`, `finalized_block()`, etc.): Wait-free using `ArcSwap` pattern.
//!   Never block, never spin, zero contention even under extreme load.
//! - **Writes** (via `update_tip()`, `update_finalized()`): Coordinated through `ReorgManager`
//!   which holds an async mutex to serialize updates.
//!
//! ## Performance Characteristics
//!
//! | Operation | Typical Latency | Notes |
//! |-----------|-----------------|-------|
//! | `current_tip()` | ~1-2ns | Wait-free `ArcSwap` read, zero contention |
//! | `finalized_block()` | < 10ns | Single atomic load |
//! | `update_tip()` | ~1-5µs | Async lock + Arc allocation + `ArcSwap` store |
//! | `current_tip_with_hash()` | ~1-2ns | Wait-free atomic read of both values |
//!
//! ## Usage Pattern
//!
//! At startup, create one `ChainState` and share it:
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use prism_core::chain::ChainState;
//!
//! // Create single instance
//! let chain_state = Arc::new(ChainState::new());
//!
//! // Share with all components
//! let cache_manager = CacheManager::new(&config, chain_state.clone());
//! let reorg_manager = ReorgManager::new(config, chain_state.clone(), cache_manager);
//! let scoring_engine = ScoringEngine::new(config, chain_state.clone());
//! ```
//!
//! ## Anti-patterns (What NOT to Do)
//!
//! - **Don't create multiple `ChainState` instances** - components would have inconsistent views
//! - **Don't wrap in another `Arc`** - `Arc<Arc<ChainState>>` adds indirection
//! - **Don't use global singleton** - makes testing difficult
//! - **Don't pass by clone without `Arc`** - `ChainState` itself doesn't implement `Clone` (the
//!   `Arc` provides cheap cloning)

pub mod state;

pub use state::ChainState;
