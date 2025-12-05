//! # Consensus Algorithm Overview
//!
//! The consensus engine validates critical RPC responses by querying multiple
//! upstreams and requiring agreement.
//!
//! ## Algorithm Steps
//!
//! 1. **Upstream Selection**: Filter out sitting-out upstreams, select N participants
//! 2. **Parallel Query**: Send request to all selected upstreams concurrently
//! 3. **Response Grouping**: Hash each response and group by hash
//! 4. **Quorum Check**: Select group with ≥ `quorum_threshold` agreement
//! 5. **Dispute Handling**: Penalize disagreeing upstreams via scoring + misbehavior
//!
//! ## Configuration
//!
//! - `methods`: Which RPC methods require consensus (e.g., `["eth_getBlockByNumber"]`)
//! - `min_participants`: Minimum upstreams that must respond (e.g., 2)
//! - `quorum_threshold`: Fraction required for consensus (e.g., 0.67 = 2/3)
//!
//! ## Failure Modes
//!
//! - **Low participation**: Falls back based on `LowParticipantsBehavior` config
//! - **No quorum**: Returns error or most common response based on `FailureBehavior`
//! - **All disagree**: Returns error (indicates fundamental upstream issues)
//!
//! ## Penalty System
//!
//! Disagreeing upstreams receive TWO complementary penalties:
//! 1. **Scoring penalty**: `record_error()` reduces upstream score for selection
//! 2. **Misbehavior tracking**: `record_disputes()` accumulates toward sit-out
//!
//! # Module Organization
//!
//! - [`config`]: Configuration types (`ConsensusConfig`, behavior enums)
//! - [`types`]: Result types (`ConsensusResult`, `ResponseGroup`, etc.)
//! - [`engine`][]: Orchestration (`ConsensusEngine` - main entry point)
//! - [`quorum`]: Core consensus algorithm (response grouping, preference sorting)
//! - [`validation`]: Response validation and result processing

pub mod config;
pub mod engine;
pub mod quorum;
pub mod types;
pub mod validation;

#[cfg(test)]
mod tests;

// Re-export all public types to maintain backwards compatibility
pub use config::{ConsensusConfig, DisputeBehavior, FailureBehavior, LowParticipantsBehavior};
pub use engine::ConsensusEngine;
pub use types::{ConsensusMetadata, ConsensusResult, ResponseGroup, ResponseType, SelectionMethod};
