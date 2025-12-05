//! Integration and End-to-End Tests for Prism RPC Aggregator
//!
//! This crate contains various test modules:
//!
//! - `cache_cleanup_tests`: Tests for cache background cleanup and tip-triggered invalidation
//! - `hedging_tests`: Integration tests for request hedging functionality
//! - `scoring_tests`: Integration tests for multi-factor upstream scoring
//! - `consensus_tests`: Integration tests for consensus-based data validation
//! - `feature_integration_tests`: End-to-end tests with all features working together
//! - `misbehavior_tests`: Tests for upstream misbehavior tracking and penalties
//! - `websocket_tests`: Tests for WebSocket subscription functionality
//! - `logs_handler_tests`: Integration tests for the `LogsHandler` component
//! - `proxy_engine_tests`: Integration tests for the `ProxyEngine` core routing logic
//! - `adversarial_tests`: Tests for adversarial scenarios and edge cases
//! - `middleware_chain_tests`: Tests for middleware composition and ordering
//! - `mock_infrastructure`: Reusable mock types for testing (WebSocket, RPC)
//! - `e2e`: End-to-end tests against live devnet (requires `e2e` feature)
//!
//! ## Running Tests
//!
//! ### Unit/Integration Tests (no external dependencies)
//! ```bash
//! cargo test --package tests
//! ```
//!
//! ### End-to-End Tests (requires devnet AND proxy server)
//!
//! **IMPORTANT**: E2E tests require BOTH the devnet containers AND the Prism proxy server to be
//! running. Tests will fail with "Environment not ready: Timeout("proxy healthy")" if the proxy
//! isn't started.
//!
//! ```bash
//! # 1. Start the devnet
//! docker compose -f docker/devnet/docker-compose.yml up -d
//!
//! # 2. Start the Prism proxy (REQUIRED - tests will fail without this!)
//! PRISM_CONFIG=docker/devnet/config.test.toml cargo run --bin server
//!
//! # 3. In another terminal, run E2E tests
//! cargo test --package tests --features e2e e2e
//! ```
//!
//! ## E2E Test Architecture
//!
//! The E2E tests validate the complete system including:
//! - RPC proxy routing and load balancing
//! - Cache behavior (block, transaction, logs)
//! - Health checking and failover
//! - Batch request handling
//! - Metrics collection
//!
//! Tests run against a local 4-node Geth `PoA` devnet (1 sealer + 3 RPC nodes) with
//! the Prism proxy configured to aggregate requests across all nodes.

#[cfg(test)]
mod cache_cleanup_tests;

#[cfg(test)]
mod hedging_tests;

#[cfg(test)]
mod scoring_tests;

#[cfg(test)]
mod consensus_tests;

#[cfg(test)]
mod failsafe_integration_tests;

#[cfg(test)]
mod misbehavior_tests;

#[cfg(test)]
mod websocket_tests;

#[cfg(test)]
mod logs_handler_tests;

#[cfg(test)]
mod adversarial_tests;

#[cfg(test)]
mod proxy_engine_tests;

#[cfg(test)]
mod runtime_tests;

#[cfg(test)]
mod middleware_chain_tests;

/// Mock infrastructure for testing
pub mod mock_infrastructure;

/// End-to-end test module
pub mod e2e;
