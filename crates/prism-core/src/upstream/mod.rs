//! Upstream RPC provider management and load balancing.
//!
//! This module handles communication with upstream Ethereum RPC providers, including:
//! - HTTP and WebSocket client implementations
//! - Circuit breaker pattern for fault tolerance
//! - Health checking and endpoint management
//! - Load balancing across multiple upstreams
//! - Connection pooling and concurrency control
//! - Multi-factor scoring for optimal upstream selection
//! - Hedged requests for improved tail latency
//! - Consensus and data validation for critical RPC methods
//!
//! # Routing Strategy Priority
//!
//! When routing requests to upstreams, the [`SmartRouter`] applies strategies in the
//! following priority order (highest to lowest):
//!
//! 1. **Consensus** - If the RPC method requires quorum validation (configured via
//!    [`ConsensusConfig::methods`]), the request is sent to multiple upstreams and responses are
//!    compared. Disagreeing upstreams accumulate disputes and may be temporarily excluded
//!    (sit-out). Use for critical methods like `eth_getBlockByNumber`.
//!
//! 2. **Scoring** - If scoring is enabled ([`ScoringConfig`]), upstreams are ranked by a
//!    multi-factor score combining latency (P90), error rate, throttle rate, and block lag. The
//!    top-N scoring upstreams are selected for the request.
//!
//! 3. **Hedging** - If hedging is enabled ([`HedgeConfig`]), a primary request is sent immediately,
//!    and after a configurable delay, backup requests are sent to additional upstreams. The first
//!    successful response wins, improving tail latency.
//!
//! 4. **Response-time** - Fallback strategy selecting the upstream with the best recent response
//!    time from the [`LoadBalancer`].
//!
//! ## Example Flow
//!
//! ```text
//! Request → [Consensus required?]
//!              │
//!              ├─ Yes → ConsensusEngine (quorum validation)
//!              │
//!              └─ No → [Scoring enabled?]
//!                        │
//!                        ├─ Yes → Score-based selection (top-N upstreams)
//!                        │
//!                        └─ No → [Hedging enabled?]
//!                                  │
//!                                  ├─ Yes → HedgeExecutor (parallel with delay)
//!                                  │
//!                                  └─ No → LoadBalancer (response-time selection)
//! ```
//!
//! See [`SmartRouter`] implementation for details.

pub mod builder;
pub mod circuit_breaker;
pub mod consensus;
pub mod dynamic_registry;
pub mod endpoint;
pub mod errors;
pub mod health;
pub mod hedging;
pub mod http_client;
pub mod latency_tracker;
pub mod load_balancer;
pub mod manager;
pub mod misbehavior;
pub mod router;
pub mod scoring;
pub mod websocket;

pub use builder::{BuilderError, UpstreamManagerBuilder};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerState};
pub use consensus::{
    ConsensusConfig, ConsensusEngine, ConsensusMetadata, ConsensusResult, DisputeBehavior,
    FailureBehavior, LowParticipantsBehavior, ResponseGroup, SelectionMethod,
};
pub use dynamic_registry::{
    DynamicUpstreamConfig, DynamicUpstreamRegistry, DynamicUpstreamUpdate, UpstreamSource,
};
pub use endpoint::UpstreamEndpoint;
pub use errors::{RpcErrorCategory, UpstreamError};
pub use hedging::{HedgeConfig, HedgeExecutor};
pub use http_client::{HttpClient, HttpClientConfig};
pub use latency_tracker::LatencyTracker;
pub use load_balancer::{LoadBalancer, LoadBalancerConfig, LoadBalancerStats};
pub use manager::{
    CreateUpstreamRequest, UnhealthyBehaviorConfig, UpdateUpstreamRequest, UpstreamManager,
    UpstreamManagerConfig,
};
pub use misbehavior::{MisbehaviorConfig, MisbehaviorStats, MisbehaviorTracker};
pub use router::{RoutingContext, SmartRouter};
pub use scoring::{ScoringConfig, ScoringEngine, ScoringWeights, UpstreamMetrics, UpstreamScore};
pub use websocket::{WebSocketFailureTracker, WebSocketHandler};
