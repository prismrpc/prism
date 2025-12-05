//! End-to-end test module for Prism RPC Aggregator
//!
//! This module provides comprehensive integration tests that run against
//! a local devnet setup with multiple Anvil nodes and the Prism proxy.
//!
//! # Test Environment Setup
//!
//! Before running these tests, ensure the devnet is running:
//! ```bash
//! docker compose -f docker/devnet/docker-compose.yml up -d
//! cargo run --bin server  # In a separate terminal
//! ```
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --package tests --features e2e e2e_tests
//! ```

pub mod client;
pub mod config;
pub mod fixtures;

#[cfg(all(test, feature = "e2e"))]
pub mod basic_rpc_tests;
#[cfg(all(test, feature = "e2e"))]
pub mod batch_tests;
#[cfg(all(test, feature = "e2e"))]
pub mod cache_tests;
#[cfg(all(test, feature = "e2e"))]
pub mod failover_tests;
#[cfg(all(test, feature = "e2e"))]
pub mod load_balancing_tests;
#[cfg(all(test, feature = "e2e"))]
pub mod metrics_tests;
#[cfg(all(test, feature = "e2e"))]
pub mod reorg_tests;

pub use client::E2eClient;
pub use config::E2eConfig;
pub use fixtures::TestFixtures;
